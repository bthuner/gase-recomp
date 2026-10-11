//! What the Build button does, without the window: check the tools,
//! recompile the ROM, fetch gase, run its Android project, collect the APK.
//!
//! ```text
//!  ROM ──► gase-recomp (this process) ──► <out>/ (a Cargo workspace, mobile/ inside)
//!                                              │
//!  gase @ GASE_REV (git, cached) ──► gradle -p mobile/android assembleRelease
//!                                    -PgaseCrate=<out>/mobile/Cargo.toml:<id>-mobile
//!                                              │
//!                                     <out>/<id>.apk  ──► adb install (optional)
//! ```
//!
//! Nothing here is specific to the window: [`run`] reports progress as
//! [`Event`]s on a channel, and the command-line mode (`--cli`) prints the
//! same events. CI builds the test ROM's APK through it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use gase_recomp::analysis::analyze;
use gase_recomp::codegen::Options;
use gase_recomp::project::{GASE_REV, RuntimeSource, identifier, parse_entries, write_project};
use gase_recomp::rom::Rom;

/// The NDK gase's Android project is built with (its `ndkVersion`).
pub const NDK_VERSION: &str = "27.2.12479018";
/// The Android platform it compiles against (its `compileSdk`).
pub const PLATFORM: &str = "android-35";
pub const GASE_GIT: &str = "https://github.com/bthuner/gase";

/// Android's names for processor families, with Rust's.
pub const ABIS: [(&str, &str); 3] = [
    ("arm64-v8a", "aarch64-linux-android"),
    ("armeabi-v7a", "armv7-linux-androideabi"),
    ("x86_64", "x86_64-linux-android"),
];

// ---- what a ROM is called ---------------------------------------------------

/// What the window shows once a ROM is picked.
#[derive(Clone, Debug)]
pub struct RomInfo {
    pub path: PathBuf,
    pub title: String,
    pub size: usize,
    pub crc32: u32,
    pub checksum_ok: bool,
    /// `aladdin`: the crate name, also the end of the app id.
    pub id: String,
}

impl RomInfo {
    pub fn read(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let rom = Rom::from_bytes(&raw).map_err(|e| e.to_string())?;
        Ok(Self {
            path: path.to_path_buf(),
            title: rom.title().to_string(),
            size: rom.data.len(),
            crc32: rom.crc32(),
            checksum_ok: rom.computed_checksum() == rom.header.checksum,
            id: identifier(rom.title()),
        })
    }

    /// `ALADDIN` → `Aladdin`: header titles are often in capitals.
    #[must_use]
    pub fn display_name(&self) -> String {
        if self.title.chars().any(char::is_lowercase) {
            return self.title.clone();
        }
        self.title
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next().map_or_else(String::new, |f| {
                    f.to_uppercase()
                        .chain(c.flat_map(char::to_lowercase))
                        .collect()
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[must_use]
    pub fn default_app_id(&self) -> String {
        format!("io.github.gase_recomp.{}", self.id)
    }

    /// Where the project and the APK go: a folder next to the ROM.
    #[must_use]
    pub fn default_out(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{}-recomp", self.id))
    }
}

/// Android application ids: two or more dot-separated parts, each a letter
/// then letters, digits or `_`.
pub fn check_app_id(id: &str) -> Result<(), String> {
    let parts: Vec<&str> = id.split('.').collect();
    let part_ok = |p: &&str| {
        p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if parts.len() >= 2 && parts.iter().all(part_ok) {
        Ok(())
    } else {
        Err(format!(
            "\"{id}\" is not an Android application id (like io.github.me.mygame)"
        ))
    }
}

// ---- the tools ------------------------------------------------------------------

/// A command that fixes a missing piece.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Fix {
    #[must_use]
    pub fn display(&self) -> String {
        let mut s = self.program.display().to_string();
        for a in &self.args {
            s.push(' ');
            if a.contains(' ') || a.contains(';') {
                s.push_str(&format!("\"{a}\""));
            } else {
                s.push_str(a);
            }
        }
        s
    }
}

/// One line of the "what you need" list.
#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    /// Version found, or how to get it.
    pub detail: String,
    /// A command the window can run for you.
    pub fix: Option<Fix>,
}

/// The line where a program says its version (`--version`, `-version`),
/// if it runs. Not simply the first line: Java may first print
/// "Picked up JAVA_TOOL_OPTIONS…", Gradle a row of dashes.
fn version(program: &Path, arg: &str) -> Option<String> {
    let out = Command::new(program).arg(arg).output().ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Picked up") && !l.starts_with("---"))
        .collect();
    lines
        .iter()
        .find(|l| l.to_lowercase().contains("version") || l.starts_with("Gradle "))
        .or_else(|| lines.first())
        .map(|l| (*l).to_string())
}

/// The Android SDK: `ANDROID_HOME`, `ANDROID_SDK_ROOT`, or where Android
/// Studio installs it.
#[must_use]
pub fn android_sdk() -> Option<PathBuf> {
    let env = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let usual = [
        home.as_ref().map(|h| h.join("Android/Sdk")),
        home.as_ref().map(|h| h.join("Library/Android/sdk")),
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Android/Sdk")),
    ];
    env.chain(usual.into_iter().flatten()).find(|p| p.is_dir())
}

fn sdkmanager(sdk: &Path) -> PathBuf {
    sdk.join("cmdline-tools/latest/bin/sdkmanager")
}

/// `adb`, from the SDK or the PATH.
#[must_use]
pub fn adb() -> Option<PathBuf> {
    let from_sdk = android_sdk().map(|s| s.join("platform-tools/adb"));
    from_sdk
        .filter(|p| p.exists())
        .or_else(|| version(Path::new("adb"), "version").map(|_| PathBuf::from("adb")))
}

/// Everything an APK build needs, for these ABIs.
#[must_use]
pub fn check_tools(abis: &[String]) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut tool = |name, program: &str, arg, hint: &str| {
        let v = version(Path::new(program), arg);
        checks.push(Check {
            name,
            ok: v.is_some(),
            detail: v.unwrap_or_else(|| hint.to_string()),
            fix: None,
        });
    };
    tool(
        "Rust (cargo)",
        "cargo",
        "--version",
        "install from https://rustup.rs",
    );
    tool("git", "git", "--version", "install git");
    tool(
        "Java 17+",
        "java",
        "-version",
        "install a JDK 17, e.g. Temurin (or the one in Android Studio)",
    );
    tool(
        "Gradle 8",
        "gradle",
        "--version",
        "install Gradle 8 (https://gradle.org/install), or `brew install gradle`",
    );
    tool(
        "CMake",
        "cmake",
        "--version",
        "install CMake (it builds SDL2 for Android)",
    );
    if cfg!(windows) {
        checks.push(Check {
            name: "Unix shell",
            ok: false,
            detail: "gase's Android build is a shell script: run the studio in WSL".into(),
            fix: None,
        });
    }

    // One Rust target per ABI.
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    for (abi, triple) in ABIS {
        if !abis.iter().any(|a| a == abi) {
            continue;
        }
        let ok = installed
            .as_deref()
            .is_some_and(|t| t.lines().any(|l| l.trim() == triple));
        checks.push(Check {
            name: "Rust for Android",
            ok,
            detail: if ok {
                triple.to_string()
            } else {
                format!("rustup target add {triple}")
            },
            fix: (!ok && installed.is_some()).then(|| Fix {
                program: "rustup".into(),
                args: vec!["target".into(), "add".into(), triple.into()],
            }),
        });
    }

    // The SDK, then what gase's project needs from it.
    let sdk = android_sdk();
    checks.push(Check {
        name: "Android SDK",
        ok: sdk.is_some(),
        detail: sdk.as_ref().map_or_else(
            || "install Android Studio, or the command-line tools; set ANDROID_HOME".into(),
            |s| s.display().to_string(),
        ),
        fix: None,
    });
    if let Some(sdk) = &sdk {
        let manager = sdkmanager(sdk);
        let has_manager = manager.exists();
        for (name, dir, package) in [
            (
                "Android NDK",
                format!("ndk/{NDK_VERSION}"),
                format!("ndk;{NDK_VERSION}"),
            ),
            (
                "Android platform",
                format!("platforms/{PLATFORM}"),
                format!("platforms;{PLATFORM}"),
            ),
        ] {
            let ok = sdk.join(&dir).is_dir();
            checks.push(Check {
                name,
                ok,
                detail: if ok {
                    dir
                } else {
                    format!("sdkmanager \"{package}\"")
                },
                fix: (!ok && has_manager).then(|| Fix {
                    program: manager.clone(),
                    args: vec!["--install".into(), package],
                }),
            });
        }
    }
    checks
}

// ---- the build ----------------------------------------------------------------------

/// What to build.
#[derive(Clone, Debug)]
pub struct Job {
    pub rom: PathBuf,
    /// The generated project; the APK is copied here too.
    pub out: PathBuf,
    pub app_name: String,
    pub app_id: String,
    pub abis: Vec<String>,
    /// Where gase is checked out (shared by every game).
    pub gase_dir: PathBuf,
}

/// The stages, in order (the window shows them as a list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Recompile,
    FetchGase,
    BuildApk,
}

impl Stage {
    pub const ALL: [Self; 3] = [Self::Recompile, Self::FetchGase, Self::BuildApk];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Recompile => "Recompile the ROM to Rust",
            Self::FetchGase => "Fetch gase (the emulator around the game)",
            Self::BuildApk => "Build the APK (Rust for Android, SDL2, Gradle)",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Stage(Stage),
    Log(String),
    /// The APK, or what went wrong.
    Done(Result<PathBuf, String>),
}

/// Where gase is kept between builds.
#[must_use]
pub fn default_gase_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("gase-recomp").join("gase")
}

/// Run the whole build; the last event is always [`Event::Done`].
pub fn run(job: &Job, events: &Sender<Event>) {
    let result = build(job, events);
    let _ = events.send(Event::Done(result));
}

fn build(job: &Job, events: &Sender<Event>) -> Result<PathBuf, String> {
    // Gradle runs in gase's checkout: every path it gets must be absolute.
    let job = &Job {
        out: std::path::absolute(&job.out).map_err(|e| format!("{}: {e}", job.out.display()))?,
        ..job.clone()
    };
    let log = |s: String| {
        let _ = events.send(Event::Log(s));
    };
    check_app_id(&job.app_id)?;
    if job.abis.is_empty() {
        return Err("choose at least one processor family (ABI)".into());
    }

    // 1. The ROM becomes a Rust workspace (with its phone crate).
    let _ = events.send(Event::Stage(Stage::Recompile));
    let raw = std::fs::read(&job.rom).map_err(|e| format!("{}: {e}", job.rom.display()))?;
    let rom = Rom::from_bytes(&raw).map_err(|e| e.to_string())?;
    let mut entries = BTreeMap::new();
    let missing = job.out.join("gase_missing.txt");
    if missing.exists() {
        log(format!(
            "using the addresses found at run time in {}",
            missing.display()
        ));
        parse_entries(&missing, &mut entries)?;
    }
    let program = analyze(&rom, &entries);
    log(format!(
        "\"{}\": {} functions, {} instructions",
        rom.title(),
        program.functions.len(),
        program.instruction_count()
    ));
    std::fs::create_dir_all(&job.out).map_err(|e| e.to_string())?;
    let files = write_project(
        &program,
        &job.out,
        true,
        &RuntimeSource::default_for_this_build(),
        &Options::default(),
    )
    .map_err(|e| e.to_string())?;
    log(format!("{files} Rust files in {}", job.out.display()));
    let package = format!("{}-mobile", identifier(rom.title()));

    // 2. gase, at the revision the generated code is written against.
    let _ = events.send(Event::Stage(Stage::FetchGase));
    fetch_gase(&job.gase_dir, events)?;

    // 3. gase's Android project, pointed at the game's crate.
    let _ = events.send(Event::Stage(Stage::BuildApk));
    let sdk = android_sdk().ok_or("no Android SDK: install it and set ANDROID_HOME")?;
    log("each ABI compiles the whole game: expect a few minutes".into());
    let mut gradle = Command::new("gradle");
    gradle
        .arg("-p")
        .arg(job.gase_dir.join("mobile/android"))
        .arg("assembleRelease")
        .arg(format!("-PgaseAbis={}", job.abis.join(",")))
        .arg(format!(
            "-PgaseCrate={}:{package}",
            job.out.join("mobile/Cargo.toml").display()
        ))
        .arg(format!("-PgaseAppId={}", job.app_id))
        .arg(format!("-PgaseAppName={}", job.app_name))
        .env("ANDROID_HOME", &sdk);
    stream(gradle, events)?;
    let built = job
        .gase_dir
        .join("mobile/android/app/build/outputs/apk/release/app-release.apk");
    let apk = job.out.join(format!("{}.apk", identifier(rom.title())));
    std::fs::copy(&built, &apk).map_err(|e| format!("{}: {e}", built.display()))?;
    log(format!("APK: {}", apk.display()));
    Ok(apk)
}

/// Clone gase once, then make sure the pinned revision is checked out.
fn fetch_gase(dir: &Path, events: &Sender<Event>) -> Result<(), String> {
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir).args(args);
        c
    };
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(dir.parent().unwrap_or(dir)).map_err(|e| e.to_string())?;
        let mut clone = Command::new("git");
        clone.args(["clone", GASE_GIT]).arg(dir);
        stream(clone, events)?;
    }
    let have = git(&["cat-file", "-e", &format!("{GASE_REV}^{{commit}}")])
        .status()
        .is_ok_and(|s| s.success());
    if !have {
        stream(git(&["fetch", "origin"]), events)?;
    }
    stream(git(&["checkout", "--quiet", "--detach", GASE_REV]), events)?;
    let _ = events.send(Event::Log(format!(
        "gase {} in {}",
        &GASE_REV[..7],
        dir.display()
    )));
    Ok(())
}

/// Run a command, sending each line it prints as a log event.
fn stream(mut command: Command, events: &Sender<Event>) -> Result<(), String> {
    let shown = format!("{command:?}").replace('"', "");
    let _ = events.send(Event::Log(format!("$ {shown}")));
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{shown}: {e}"))?;
    let pipes: Vec<Box<dyn std::io::Read + Send>> = vec![
        Box::new(child.stdout.take().expect("piped")),
        Box::new(child.stderr.take().expect("piped")),
    ];
    let readers: Vec<_> = pipes
        .into_iter()
        .map(|pipe| {
            let events = events.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = events.send(Event::Log(line));
                }
            })
        })
        .collect();
    let status = child.wait().map_err(|e| e.to_string())?;
    for r in readers {
        let _ = r.join();
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} failed ({status})"))
    }
}

/// Run a [`Fix`], or `adb install`, the same way.
pub fn run_command(program: &Path, args: &[String], events: &Sender<Event>) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    stream(c, events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids() {
        assert!(check_app_id("io.github.gase_recomp.aladdin").is_ok());
        assert!(check_app_id("aladdin").is_err(), "one part");
        assert!(check_app_id("io.1game").is_err(), "starts with a digit");
        assert!(check_app_id("io.my-game").is_err(), "dash");
    }

    #[test]
    fn display_names() {
        let info = |title: &str| RomInfo {
            path: PathBuf::from("/roms/game.md"),
            title: title.into(),
            size: 0,
            crc32: 0,
            checksum_ok: true,
            id: identifier(title),
        };
        assert_eq!(info("ALADDIN").display_name(), "Aladdin");
        assert_eq!(
            info("SONIC THE      HEDGEHOG").display_name(),
            "Sonic The Hedgehog"
        );
        assert_eq!(info("hashbug").display_name(), "hashbug");
        assert_eq!(
            info("ALADDIN").default_app_id(),
            "io.github.gase_recomp.aladdin"
        );
        assert_eq!(
            info("ALADDIN").default_out(),
            PathBuf::from("/roms/aladdin-recomp")
        );
    }

    #[test]
    fn fixes_are_shown_as_commands() {
        let fix = Fix {
            program: "sdkmanager".into(),
            args: vec!["--install".into(), "ndk;27.2.12479018".into()],
        };
        assert_eq!(fix.display(), "sdkmanager --install \"ndk;27.2.12479018\"");
    }

    #[test]
    fn streamed_commands_report_their_output_and_failure() {
        let (tx, rx) = std::sync::mpsc::channel();
        let ok = run_command(
            Path::new("sh"),
            &["-c".into(), "echo out; echo err >&2".into()],
            &tx,
        );
        assert!(ok.is_ok());
        let fail = run_command(Path::new("sh"), &["-c".into(), "exit 3".into()], &tx);
        assert!(fail.is_err());
        drop(tx);
        let lines: Vec<String> = rx
            .iter()
            .filter_map(|e| match e {
                Event::Log(l) => Some(l),
                _ => None,
            })
            .collect();
        assert!(lines.contains(&"out".to_string()));
        assert!(lines.contains(&"err".to_string()));
    }
}
