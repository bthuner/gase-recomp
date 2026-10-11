//! "Make an app of this game…", run on a desktop: the phone's side of
//! gase JIT against a studio on this machine, with a fake installer.

use std::sync::Mutex;
use std::time::Duration;

use gase_recomp_jit_app::phone;
use gase_recomp_remote::Progress;
use gase_recomp_remote::client;
use gase_recomp_remote::server::Server;

#[test]
fn the_phone_installs_what_the_studio_built() {
    let dir = std::env::temp_dir().join(format!("gase-jit-phone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("ALADDIN.md");
    std::fs::write(&rom, vec![0x4E; 4096]).unwrap();

    let server = Server::start("test-pc", 0, Some(38_911)).expect("server");
    let builder = std::thread::spawn(move || {
        let request = loop {
            if let Some(r) = server.waiting().pop() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(request.title, "ALADDIN", "the title is the ROM's name");
        std::thread::sleep(Duration::from_millis(100));
        for log in ["Compiling gase-core", "Compiling gase"] {
            server.set_progress(
                request.id,
                Progress::Running {
                    stage: "Build the APK".into(),
                    log: log.into(),
                },
            );
            std::thread::sleep(Duration::from_millis(60));
        }
        server.finish(request.id, b"PK an apk".to_vec());
        std::thread::sleep(Duration::from_millis(400));
    });

    let toasts = Mutex::new(Vec::new());
    let installed = Mutex::new(None);
    phone::run(
        &rom,
        || client::discover_on(38_911, Duration::from_millis(500)),
        Duration::from_millis(20),
        &|t| toasts.lock().unwrap().push(t),
        |apk| {
            *installed.lock().unwrap() = Some(apk.to_vec());
            Ok(())
        },
    )
    .expect("an app");
    builder.join().unwrap();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        installed.into_inner().unwrap().as_deref(),
        Some(&b"PK an apk"[..])
    );
    let toasts = toasts.into_inner().unwrap();
    assert_eq!(
        toasts,
        [
            "Looking for gase-recomp studio on this network…",
            "Click Accept on test-pc",
            "test-pc: Build the APK",
            "Downloading the app (0 MB)…",
            "Installing ALADDIN…",
        ],
        "one toast per stage, not per log line"
    );
}

#[test]
fn no_studio_says_what_to_do() {
    let dir = std::env::temp_dir().join(format!("gase-jit-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("game.bin");
    std::fs::write(&rom, [0u8; 512]).unwrap();
    let err = phone::run(
        &rom,
        || Ok(Vec::new()),
        Duration::from_millis(20),
        &|_| {},
        |_| panic!("nothing to install"),
    )
    .unwrap_err();
    std::fs::remove_dir_all(&dir).ok();
    assert!(err.contains("Phone builds"), "{err}");
}
