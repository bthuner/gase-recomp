//! A phone and a studio on this machine, over real sockets.

use std::time::Duration;

use gase_recomp_remote::Progress;
use gase_recomp_remote::client::{self, Studio};
use gase_recomp_remote::server::Server;

/// A UDP port of its own per test (tests run in parallel).
fn studio(udp: u16) -> (Server, Studio) {
    let server = Server::start("test-pc", 0, Some(udp)).expect("server");
    let found = client::discover_on(udp, Duration::from_millis(500)).expect("discover");
    let studio = found
        .into_iter()
        .find(|s| s.addr.port() == server.port())
        .expect("the studio answers discovery");
    assert_eq!(studio.name, "test-pc");
    (server, studio)
}

#[test]
fn a_phone_gets_its_apk() {
    let (server, studio) = studio(38_901);
    let rom = vec![0x4E; 4096];
    // The computer: accept the request, build, hand over the APK.
    let builder = std::thread::spawn(move || {
        let request = loop {
            if let Some(r) = server.waiting().pop() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(request.title, "ALADDIN & co");
        // Someone takes a moment to click Accept.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(server.rom(request.id).unwrap().len(), 4096);
        server.set_progress(
            request.id,
            Progress::Running {
                stage: "Build the APK".into(),
                log: "Compiling".into(),
            },
        );
        std::thread::sleep(Duration::from_millis(100));
        server.finish(request.id, b"PK\x03\x04 an apk".to_vec());
        // Keep serving until the phone has its APK.
        std::thread::sleep(Duration::from_millis(500));
    });
    let mut seen = Vec::new();
    let apk = client::build(
        &studio,
        "ALADDIN & co",
        &rom,
        Duration::from_millis(20),
        |p| {
            seen.push(p.clone());
        },
    )
    .expect("the APK");
    assert_eq!(apk, b"PK\x03\x04 an apk");
    assert!(seen.contains(&Progress::Waiting));
    assert!(seen.iter().any(|p| matches!(p, Progress::Running { .. })));
    assert!(matches!(seen.last(), Some(Progress::Done(n)) if *n == apk.len()));
    builder.join().unwrap();
}

#[test]
fn a_refusal_is_reported() {
    let (server, studio) = studio(38_902);
    // The phone waits on its own thread; the computer clicks Refuse.
    let phone = std::thread::spawn(move || {
        client::build(
            &studio,
            "GAME",
            &[1, 2, 3],
            Duration::from_millis(10),
            |_| {},
        )
    });
    let request = loop {
        if let Some(r) = server.waiting().pop() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    server.set_progress(request.id, Progress::Refused);
    assert_eq!(
        phone.join().unwrap(),
        Err("refused on the computer".to_string())
    );
}

#[test]
fn empty_and_unknown_requests_are_errors() {
    let (_server, studio) = studio(38_903);
    assert!(client::submit(&studio, "GAME", &[]).is_err());
    assert!(client::progress(&studio, 99).is_err());
    assert!(client::apk(&studio, 99).is_err());
}
