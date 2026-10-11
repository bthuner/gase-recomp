//! gase-recomp studio: pick a ROM, press Build, get an Android app.
//!
//! ```text
//! gase-recomp-studio [ROM]              the window
//! gase-recomp-studio --desktop ROM      the window, set to build for this computer
//! gase-recomp-studio --phone-builds     the window, phones of this network may ask
//! gase-recomp-studio --check [--target desktop|android]
//! gase-recomp-studio --cli ROM [--target android|desktop] [--run] [-o DIR]
//!                    [--name NAME] [--id APP_ID] [--abis arm64-v8a,...]
//! ```
//!
//! The window is a thin layer over [`pipeline`]: the same steps run with
//! `--cli`, which is what CI uses.

mod app;
mod pipeline;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::channel;

use pipeline::{Event, Job, RomInfo, Target};

const USAGE: &str = "\
gase-recomp-studio: pick a ROM, press Build, get an Android app

usage:
  gase-recomp-studio [ROM]           open the window
  gase-recomp-studio --desktop ROM   open the window, set to build for this computer
  gase-recomp-studio --phone-builds  open the window, letting phones on this network ask for apps
                                     (what gase's \"Recompile to native\" opens)
  gase-recomp-studio --check [--target T] [--abis LIST]
  gase-recomp-studio --cli ROM [--target T] [--run] [-o DIR] [--name NAME] [--id APP_ID] [--abis LIST]
                                     T: android (default) or desktop; --run plays the desktop build
                                     LIST: arm64-v8a,armeabi-v7a,x86_64 (default arm64-v8a)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--cli") => cli(&args[1..]),
        Some("--desktop") => window(args.get(1).map(PathBuf::from), Target::Desktop, false),
        Some("--phone-builds") => window(None, Target::Android, true),
        Some("--check") => check(&args[1..]),
        Some("-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        _ => window(args.first().map(PathBuf::from), Target::Android, false),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn window(rom: Option<PathBuf>, target: Target, phone_builds: bool) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("gase-recomp studio")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([760.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "gase-recomp studio",
        options,
        Box::new(move |cc| Ok(Box::new(app::Studio::new(cc, rom, target, phone_builds)))),
    )
    .map_err(|e| e.to_string())
}

/// `--target desktop|android` from the arguments (default: android).
fn target(args: &[String]) -> Result<Target, String> {
    match args
        .iter()
        .position(|a| a == "--target")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
    {
        None | Some("android") => Ok(Target::Android),
        Some("desktop") => Ok(Target::Desktop),
        Some(t) => Err(format!("unknown target {t:?} (android or desktop)")),
    }
}

/// `--abis a,b` from the arguments, or the default.
fn abis(args: &[String]) -> Vec<String> {
    args.iter()
        .position(|a| a == "--abis")
        .and_then(|i| args.get(i + 1))
        .map_or_else(
            || vec!["arm64-v8a".to_string()],
            |list| list.split(',').map(|s| s.trim().to_string()).collect(),
        )
}

fn check(args: &[String]) -> Result<(), String> {
    let checks = pipeline::check_tools(target(args)?, &abis(args));
    for c in &checks {
        println!(
            "{} {:<18} {}",
            if c.ok { "ok " } else { "-- " },
            c.name,
            c.detail
        );
        if let Some(fix) = &c.fix {
            println!("    fix: {}", fix.display());
        }
    }
    if checks.iter().all(|c| c.ok) {
        Ok(())
    } else {
        Err("some tools are missing".into())
    }
}

fn cli(args: &[String]) -> Result<(), String> {
    let mut rom = None;
    let (mut out, mut name, mut id) = (None, None, None);
    let mut run = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "-o" | "--output" => out = Some(PathBuf::from(value()?)),
            "--name" => name = Some(value()?),
            "--id" => id = Some(value()?),
            "--abis" | "--target" => {
                value()?;
            }
            "--run" => run = true,
            _ if a.starts_with('-') => return Err(format!("unknown option {a}\n{USAGE}")),
            _ => rom = Some(PathBuf::from(a)),
        }
    }
    let info = RomInfo::read(&rom.ok_or(USAGE)?)?;
    let job = Job {
        target: target(args)?,
        out: out.unwrap_or_else(|| info.default_out()),
        app_name: name.unwrap_or_else(|| info.display_name()),
        app_id: id.unwrap_or_else(|| info.default_app_id()),
        abis: abis(args),
        gase_dir: pipeline::default_gase_dir(),
        rom: info.path,
    };
    println!(
        "\"{}\" ({}) → {}",
        job.app_name,
        job.app_id,
        job.out.display()
    );
    if run && job.target != Target::Desktop {
        return Err("--run plays a desktop build: add --target desktop".into());
    }
    let (tx, rx) = channel();
    let worker = std::thread::spawn(move || pipeline::run(&job, &tx));
    let mut result = Err("the build stopped without a result".to_string());
    for event in rx {
        match event {
            Event::Stage(s) => println!("== {}", s.label()),
            Event::Log(line) => println!("   {line}"),
            Event::Done(r) => result = r,
        }
    }
    let _ = worker.join();
    let built = result?;
    println!("built: {}", built.display());
    if run {
        pipeline::play(&built)?;
    }
    Ok(())
}
