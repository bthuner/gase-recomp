//! The phone's half: find a studio, send the ROM, follow the build, take
//! the APK.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::http::request;
use crate::{ANNOUNCE, DISCOVER, PORT, Progress, encode_query};

/// A studio that answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Studio {
    pub addr: SocketAddr,
    /// The computer's name, to show.
    pub name: String,
}

/// Ask the local network (and this machine) who runs a studio, waiting
/// `timeout` for answers.
pub fn discover(timeout: Duration) -> io::Result<Vec<Studio>> {
    discover_on(PORT, timeout)
}

/// [`discover`] on another UDP port (tests).
pub fn discover_on(port: u16, timeout: Duration) -> io::Result<Vec<Studio>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    // The whole network, and this machine (a studio on the same computer).
    let _ = socket.send_to(DISCOVER.as_bytes(), (Ipv4Addr::BROADCAST, port));
    let _ = socket.send_to(DISCOVER.as_bytes(), (Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + timeout;
    let mut found: Vec<Studio> = Vec::new();
    let mut buf = [0u8; 256];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            break;
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        let Some(rest) = text.strip_prefix(ANNOUNCE) else {
            continue;
        };
        let mut words = rest.trim().splitn(2, ' ');
        let Some(tcp) = words.next().and_then(|p| p.parse::<u16>().ok()) else {
            continue;
        };
        let studio = Studio {
            addr: SocketAddr::new(from.ip(), tcp),
            name: words.next().unwrap_or("studio").to_string(),
        };
        if !found
            .iter()
            .any(|s| s.name == studio.name && s.addr.port() == tcp)
        {
            found.push(studio);
        }
    }
    Ok(found)
}

const TIMEOUT: Duration = Duration::from_secs(30);

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).trim().to_string()
}

/// Send a ROM; returns the build's number.
pub fn submit(studio: &Studio, title: &str, rom: &[u8]) -> Result<u32, String> {
    let target = format!("/builds?title={}", encode_query(title));
    let (status, body) =
        request(studio.addr, "POST", &target, rom, TIMEOUT).map_err(|e| e.to_string())?;
    match status {
        201 => text(&body).parse().map_err(|_| "bad answer".to_string()),
        _ => Err(format!("{status}: {}", text(&body))),
    }
}

/// Where build `id` is.
pub fn progress(studio: &Studio, id: u32) -> Result<Progress, String> {
    let (status, body) = request(studio.addr, "GET", &format!("/builds/{id}"), &[], TIMEOUT)
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("{status}: {}", text(&body)));
    }
    Progress::decode(&String::from_utf8_lossy(&body)).ok_or_else(|| "bad answer".into())
}

/// The APK of a finished build.
pub fn apk(studio: &Studio, id: u32) -> Result<Vec<u8>, String> {
    let (status, body) = request(
        studio.addr,
        "GET",
        &format!("/builds/{id}/apk"),
        &[],
        Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    match status {
        200 => Ok(body),
        _ => Err(format!("{status}: {}", text(&body))),
    }
}

/// The whole thing: send, follow (calling `report` on each change), fetch.
pub fn build(
    studio: &Studio,
    title: &str,
    rom: &[u8],
    poll: Duration,
    mut report: impl FnMut(&Progress),
) -> Result<Vec<u8>, String> {
    let id = submit(studio, title, rom)?;
    let mut last = None;
    loop {
        let p = progress(studio, id)?;
        if last.as_ref() != Some(&p) {
            report(&p);
            last = Some(p.clone());
        }
        match p {
            Progress::Done(_) => return apk(studio, id),
            Progress::Failed(e) => return Err(e),
            Progress::Refused => return Err("refused on the computer".into()),
            Progress::Waiting | Progress::Running { .. } => std::thread::sleep(poll),
        }
    }
}
