//! "Make an app of this game…" on a phone: ask gase-recomp's studio on a
//! computer of the same network to build the app, then install it.
//!
//! ```text
//!  pause menu ─► make_app(rom path)          (gase's main loop: return at once)
//!                   └─ a thread:
//!                      discover the studio    UDP broadcast, 3 s
//!                      send the ROM           the computer shows "Accept?"
//!                      follow the build       one toast per stage
//!                      download the APK
//!                      gase_mobile::install_apk ─► Android's installer
//! ```
//!
//! The protocol is in `crates/remote`; the toasts go through gase's
//! [`Notices`], which any thread may post to.

use std::io;
use std::path::Path;
use std::time::Duration;

use gase_mobile::Notices;
use gase_recomp_remote::{Progress, client};

/// The hook gase calls from its pause menu (see `gase_mobile::Hooks`).
pub fn make_app(rom: String, notices: Notices) {
    std::thread::spawn(move || {
        let say = |text: String| notices.push(text);
        let result = run(
            Path::new(&rom),
            || client::discover(Duration::from_secs(3)),
            Duration::from_secs(1),
            &say,
            gase_mobile::install_apk,
        );
        if let Err(e) = result {
            say(format!("No app: {e}"));
        }
    });
}

/// The whole conversation, with what touches the world passed in (so the
/// tests can run it on a desktop against a studio on `localhost`).
pub fn run(
    rom: &Path,
    discover: impl FnOnce() -> io::Result<Vec<client::Studio>>,
    poll: Duration,
    say: &dyn Fn(String),
    install: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let bytes = std::fs::read(rom).map_err(|e| format!("cannot read the ROM: {e}"))?;
    let title = rom
        .file_stem()
        .map_or_else(|| "game".into(), |s| s.to_string_lossy().into_owned());

    say("Looking for gase-recomp studio on this network…".into());
    let studios = discover().map_err(|e| format!("no network: {e}"))?;
    let Some(studio) = studios.into_iter().next() else {
        return Err("no studio found. On a computer of this network, open \
                    gase-recomp-studio and turn on Phone builds"
            .into());
    };

    let mut stage = String::new();
    let apk = client::build(&studio, &title, &bytes, poll, |p| match p {
        Progress::Waiting => say(format!("Click Accept on {}", studio.name)),
        // One toast per stage: the log changes too fast to read.
        Progress::Running { stage: now, .. } if *now != stage => {
            stage.clone_from(now);
            say(format!("{}: {now}", studio.name));
        }
        Progress::Done(n) => say(format!("Downloading the app ({} MB)…", n >> 20)),
        _ => {}
    })?;
    install(&apk)?;
    say(format!("Installing {title}…"));
    Ok(())
}
