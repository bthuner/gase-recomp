//! What the Build button does, without the window: check the tools,
//! recompile the ROM, fetch gase, run its Android project, collect the APK.
//!
//! ```text
//!  ROM ──► gase-recomp (this process) ──► <out>/ (a Cargo workspace, mobile/ inside)
//!                                              │
//!  gase @ GASE_REV (git, cached) ──► mobile/android/gradlew assembleRelease
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
    /// `sdkmanager` asks to accept the Android SDK licence, on its
    /// standard input. The window says so on the button, and running the
    /// fix answers "y": clicking is accepting.
    pub accept_licences: bool,
}

/// What the Android SDK's licence prompt refers to.
pub const SDK_LICENCE: &str = "https://developer.android.com/studio/terms";

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

/// SDL2's development files, which gase's window links against.
fn sdl2_check() -> Check {
    let found = if cfg!(windows) {
        Some("see gase's README for SDL2 on Windows (vcpkg)".to_string())
    } else {
        version(Path::new("pkg-config"), "--modversion")
            .and(
                Command::new("pkg-config")
                    .args(["--modversion", "sdl2"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| format!("SDL2 {}", String::from_utf8_lossy(&o.stdout).trim())),
            )
            .or_else(|| version(Path::new("sdl2-config"), "--version").map(|v| format!("SDL2 {v}")))
    };
    Check {
        name: "SDL2 (window, sound)",
        ok: found.is_some(),
        detail: found.unwrap_or_else(|| {
            "apt install libsdl2-dev · brew install sdl2 · dnf install SDL2-devel".into()
        }),
        fix: None,
    }
}

/// Everything a build for `target` needs (for these ABIs, on Android).
#[must_use]
pub fn check_tools(target: Target, abis: &[String]) -> Vec<Check> {
    if target == Target::Desktop {
        let cargo = version(Path::new("cargo"), "--version");
        return vec![
            Check {
                name: "Rust (cargo)",
                ok: cargo.is_some(),
                detail: cargo.unwrap_or_else(|| "install from https://rustup.rs".into()),
                fix: None,
            },
            sdl2_check(),
        ];
    }
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
                accept_licences: false,
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
                    accept_licences: true,
                }),
            });
        }
    }
    checks
}

// ---- the build ----------------------------------------------------------------------

/// What to build: an Android app, or the game for this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Android,
    /// gase plus the game's code, built here, played in gase's window.
    Desktop,
}

impl Target {
    /// The stages the build goes through, in order.
    #[must_use]
    pub fn stages(self) -> &'static [Stage] {
        match self {
            Self::Android => &[Stage::Recompile, Stage::FetchGase, Stage::BuildApk],
            Self::Desktop => &[Stage::Recompile, Stage::BuildDesktop],
        }
    }
}

/// What to build.
#[derive(Clone, Debug)]
pub struct Job {
    pub target: Target,
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
    BuildDesktop,
}

impl Stage {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Recompile => "Recompile the ROM to Rust",
            Self::FetchGase => "Fetch gase (the emulator around the game)",
            Self::BuildApk => "Build the APK (Rust for Android, SDL2, Gradle)",
            Self::BuildDesktop => "Compile the game for this computer (cargo build --release)",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Stage(Stage),
    Log(String),
    /// The APK or the program, or what went wrong.
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
    if job.target == Target::Android {
        check_app_id(&job.app_id)?;
        if job.abis.is_empty() {
            return Err("choose at least one processor family (ABI)".into());
        }
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
    let id = identifier(rom.title());
    if job.target == Target::Desktop {
        return build_desktop(job, &id, events);
    }
    let package = format!("{id}-mobile");

    // 2. gase, at the revision the generated code is written against.
    let _ = events.send(Event::Stage(Stage::FetchGase));
    fetch_gase(&job.gase_dir, events)?;

    // 3. gase's Android project, pointed at the game's crate.
    let _ = events.send(Event::Stage(Stage::BuildApk));
    let sdk = android_sdk().ok_or("no Android SDK: install it and set ANDROID_HOME")?;
    log("each ABI compiles the whole game: expect a few minutes".into());
    // gase's Gradle wrapper: it downloads the Gradle version gase's
    // Android project is written for (once, into ~/.gradle), so whichever
    // Gradle this computer has, if any, does not matter.
    let gradlew = job.gase_dir.join("mobile/android/gradlew");
    if !gradlew.exists() {
        return Err(format!(
            "{} is missing: gase at {} predates its Gradle wrapper",
            gradlew.display(),
            &GASE_REV[..7]
        ));
    }
    let mut gradle = Command::new(&gradlew);
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
    let apk = job.out.join(format!("{id}.apk"));
    std::fs::copy(&built, &apk).map_err(|e| format!("{}: {e}", built.display()))?;
    log(format!("APK: {}", apk.display()));
    Ok(apk)
}

/// The game for this computer: the generated crate is gase plus the
/// game's code, built like any Cargo project.
fn build_desktop(job: &Job, id: &str, events: &Sender<Event>) -> Result<PathBuf, String> {
    let _ = events.send(Event::Stage(Stage::BuildDesktop));
    let _ = events.send(Event::Log(
        "the first build compiles gase too; a big game takes a few minutes".into(),
    ));
    let mut cargo = Command::new("cargo");
    cargo
        .args(["build", "--release", "--manifest-path"])
        .arg(job.out.join("Cargo.toml"));
    stream(cargo, events)?;
    let exe = job
        .out
        .join("target/release")
        .join(format!("{id}{}", std::env::consts::EXE_SUFFIX));
    if !exe.exists() {
        return Err(format!("no program at {}", exe.display()));
    }
    let _ = events.send(Event::Log(format!("program: {}", exe.display())));
    Ok(exe)
}

/// Start a recompiled game built by [`Target::Desktop`]. It runs in its
/// project folder, where it finds `rom.bin` and writes `gase_missing.txt`
/// (the next build picks that up).
pub fn play(exe: &Path) -> Result<(), String> {
    let project = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("not a built game")?;
    Command::new(exe)
        .current_dir(project)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", exe.display()))
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
fn stream(command: Command, events: &Sender<Event>) -> Result<(), String> {
    stream_answering(command, events, false)
}

/// [`stream`], answering "y" to every question the command asks on its
/// standard input when `yes` (sdkmanager's licence prompt), and giving it
/// no input otherwise, so nothing waits for a keyboard it does not have.
fn stream_answering(mut command: Command, events: &Sender<Event>, yes: bool) -> Result<(), String> {
    let shown = format!("{command:?}").replace('"', "");
    let _ = events.send(Event::Log(format!("$ {shown}")));
    let mut child = command
        .stdin(if yes { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{shown}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            use std::io::Write;
            // Until the command stops reading (its end of the pipe closes).
            while stdin.write_all(b"y\n").is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    }
    let pipes: Vec<Box<dyn std::io::Read + Send>> = vec![
        Box::new(child.stdout.take().expect("piped")),
        Box::new(child.stderr.take().expect("piped")),
    ];
    let readers: Vec<_> = pipes
        .into_iter()
        .map(|pipe| {
            let events = events.clone();
            std::thread::spawn(move || {
                let mut kept = Vec::new();
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    kept.push(line.clone());
                    let _ = events.send(Event::Log(line));
                }
                kept
            })
        })
        .collect();
    let status = child.wait().map_err(|e| e.to_string())?;
    let mut output = Vec::new();
    for r in readers {
        output.extend(r.join().unwrap_or_default());
    }
    if status.success() {
        Ok(())
    } else {
        let program = command.get_program().to_string_lossy().into_owned();
        Err(match failure_reason(&output) {
            Some(reason) => format!("{program} failed ({status}):\n{reason}"),
            None => format!("{shown} failed ({status})"),
        })
    }
}

/// Why a command failed, from its output: the end of a long log is
/// usually advice ("Get more help at…"), not the cause.
///
/// * Gradle explains itself between `* What went wrong:` and `* Try:`.
/// * Cargo, rustc, CMake and shell scripts print `error…` lines.
fn failure_reason(output: &[String]) -> Option<String> {
    if let Some(start) = output.iter().position(|l| l.trim() == "* What went wrong:") {
        let block: Vec<&str> = output[start + 1..]
            .iter()
            .map(|l| l.trim_end())
            .take_while(|l| !l.starts_with("* Try:") && !l.starts_with("* Exception is:"))
            .filter(|l| !l.trim().is_empty())
            .collect();
        if !block.is_empty() {
            return Some(block.join("\n"));
        }
    }
    let errors: Vec<&str> = output
        .iter()
        .map(|l| l.trim())
        .filter(|l| {
            let lower = l.to_lowercase();
            lower.starts_with("error") || lower.contains("error:") || lower.starts_with("fatal")
        })
        .collect();
    // The first ones: later errors are often consequences of the first.
    (!errors.is_empty()).then(|| {
        errors
            .iter()
            .take(6)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Run a [`Fix`], or `adb install`, the same way.
pub fn run_command(program: &Path, args: &[String], events: &Sender<Event>) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    stream(c, events)
}

/// Run a [`Fix`], accepting licences if it says so.
pub fn run_fix(fix: &Fix, events: &Sender<Event>) -> Result<(), String> {
    let mut c = Command::new(&fix.program);
    c.args(&fix.args);
    stream_answering(c, events, fix.accept_licences)
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
            accept_licences: true,
        };
        assert_eq!(fix.display(), "sdkmanager --install \"ndk;27.2.12479018\"");
    }

    #[test]
    fn failures_say_why() {
        let lines = |s: &str| s.lines().map(String::from).collect::<Vec<_>>();
        let gradle = lines(
            "> Task :app:buildNative FAILED\n\nFAILURE: Build failed with an exception.\n\n\
             * What went wrong:\nExecution failed for task ':app:buildNative'.\n\
             > Process 'command 'sh'' finished with non-zero exit value 101\n\n\
             * Try:\n> Run with --stacktrace option to get the stack trace.\n\
             > Get more help at https://help.gradle.org.\n\nBUILD FAILED in 58s",
        );
        assert_eq!(
            failure_reason(&gradle).as_deref(),
            Some(
                "Execution failed for task ':app:buildNative'.\n\
                 > Process 'command 'sh'' finished with non-zero exit value 101"
            )
        );
        let cargo = lines(
            "   Compiling foo v0.1.0\nerror[E0425]: cannot find value `x`\n  --> src/lib.rs:1:1\nwarning: unused",
        );
        assert_eq!(
            failure_reason(&cargo).as_deref(),
            Some("error[E0425]: cannot find value `x`")
        );
        assert_eq!(failure_reason(&lines("all good\nbye")), None);
    }

    #[test]
    fn licence_prompts_are_answered_only_when_accepted() {
        // A stand-in for sdkmanager: asks, then installs only on "y".
        let ask = "printf 'Accept? (y/N): '; read a; [ \"$a\" = y ] && echo installed";
        let fix = |accept_licences| Fix {
            program: "sh".into(),
            args: vec!["-c".into(), ask.into()],
            accept_licences,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(run_fix(&fix(true), &tx).is_ok());
        assert!(
            run_fix(&fix(false), &tx).is_err(),
            "no keyboard: no answer, no install"
        );
        drop(tx);
        let installed = rx
            .iter()
            .filter(
                |e| matches!(e, Event::Log(l) if l.ends_with("installed") && !l.starts_with("$ ")),
            )
            .count();
        assert_eq!(installed, 1);
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
