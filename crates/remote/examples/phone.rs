//! What the phone does, from a desktop: find a studio, send a ROM, follow
//! the build, save the APK. Handy to try a studio's phone builds without a
//! phone.
//!
//! ```sh
//! cargo run -p gase-recomp-remote --example phone -- game.md
//! ```

use std::time::Duration;

use gase_recomp_remote::{Progress, client};

fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).ok_or("usage: phone ROM")?;
    let rom = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let studios = client::discover(Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let studio = studios.first().ok_or("no studio on this network")?;
    println!("found {} at {}", studio.name, studio.addr);
    let title = std::path::Path::new(&path)
        .file_stem()
        .map_or("game".into(), |s| s.to_string_lossy().into_owned());
    let apk = client::build(studio, &title, &rom, Duration::from_secs(1), |p| match p {
        Progress::Waiting => println!("waiting for Accept on {}…", studio.name),
        Progress::Running { stage, log } => println!("{stage}: {log}"),
        Progress::Done(n) => println!("done, {n} bytes"),
        Progress::Failed(e) => println!("failed: {e}"),
        Progress::Refused => println!("refused"),
    })?;
    std::fs::write("phone-build.apk", &apk).map_err(|e| e.to_string())?;
    println!("saved phone-build.apk");
    Ok(())
}
