//! Write a ready-to-build Rust crate for a recompiled ROM.
//!
//! ```text
//! <out>/
//!   Cargo.toml       gase-recomp-runtime + gase (the emulator's frontends)
//!   rom.bin          your ROM: graphics, sound and data are read from it
//!   src/main.rs      registers the recompiled code, then runs gase
//!   src/recomp/      the generated code (mod.rs + part_NNN.rs)
//!   mobile/          the same game as an Android/iOS app (its SDL_main)
//!   BUILD.md
//! ```
//!
//! The game *is* gase with one more registered CPU: same window, menus,
//! save states, rewind and controls. The crate is a workspace of its own,
//! so it can live anywhere, even inside this repository.
//!
//! On a phone there is no `main`: the system starts the app, and SDL calls
//! a C function, `SDL_main`. `mobile/` is a second crate in the same
//! workspace that exports one: it registers the same `src/recomp/` code,
//! embeds the ROM, and hands over to gase's phone shell (`gase-mobile`).
//! gase's own Android and iOS projects build it into an app.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::analysis::Program;
use crate::codegen::{Options, generate};

/// The gase revision every generated crate uses. It must be the one the
/// runtime is built against (the root `Cargo.toml`), so that Cargo builds
/// a single gase-core and the game's registration reaches gase's consoles.
pub const GASE_REV: &str = "7a41ae06b2089d4ad4fc202ae4129b1684d94c24";

/// Where the generated crate finds the runtime.
#[derive(Clone, Debug)]
pub enum RuntimeSource {
    Path(PathBuf),
    Git { url: String, rev: String },
}

impl RuntimeSource {
    /// The runtime next to this generator when run from a checkout,
    /// otherwise the project's Git repository.
    #[must_use]
    pub fn default_for_this_build() -> Self {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime");
        match here.canonicalize() {
            Ok(p) if p.join("Cargo.toml").exists() => Self::Path(p),
            _ => Self::Git {
                url: "https://github.com/bthuner/gase-recomp".into(),
                rev: "develop".into(),
            },
        }
    }

    fn toml(&self) -> String {
        match self {
            Self::Path(p) => format!("{{ path = {:?} }}", p.display().to_string()),
            Self::Git { url, rev } => format!("{{ git = {url:?}, branch = {rev:?} }}"),
        }
    }
}

/// Entry file: one hex address per line, optional name, `#` comments.
pub fn parse_entries(path: &Path, into: &mut BTreeMap<u32, String>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut parts = line.split_whitespace();
        let Some(addr) = parts.next() else { continue };
        let a = u32::from_str_radix(addr.trim_start_matches("0x").trim_start_matches('$'), 16)
            .map_err(|e| format!("{}: bad address {addr:?}: {e}", path.display()))?
            & 0xFF_FFFF;
        into.insert(
            a,
            parts
                .next()
                .map_or_else(|| format!("sub_{a:06X}"), str::to_string),
        );
    }
    Ok(())
}

/// A crate name from the game's title.
#[must_use]
pub fn identifier(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_alphabetic()) {
        format!("game_{out}")
    } else {
        out
    }
}

/// Write the crate; returns the number of generated source files.
pub fn write_project(
    program: &Program,
    out: &Path,
    desktop: bool,
    runtime: &RuntimeSource,
    opts: &Options,
) -> io::Result<usize> {
    let id = identifier(program.rom.title());
    let title = program.rom.title();
    let gen_dir = out.join("src/recomp");
    if gen_dir.exists() {
        fs::remove_dir_all(&gen_dir)?;
    }
    fs::create_dir_all(&gen_dir)?;
    let files = generate(program, opts);
    for (name, src) in &files {
        fs::write(gen_dir.join(name), src)?;
    }
    fs::write(out.join("rom.bin"), &program.rom.data)?;
    let default = if desktop { "[\"window\"]" } else { "[]" };
    let gase_git = format!("git = \"https://github.com/bthuner/gase\", rev = \"{GASE_REV}\"");
    fs::write(
        out.join("Cargo.toml"),
        format!(
            "# \"{title}\", recompiled by gase-recomp. Regenerate rather than edit.\n\
             [package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
             # A workspace of its own: the crate can live anywhere. `mobile` is\n\
             # the same game as a phone app (see BUILD.md).\n[workspace]\nmembers = [\"mobile\"]\n\n\
             [dependencies]\ngase-recomp-runtime = {}\n\
             # gase's frontends, at the revision the runtime uses (one gase-core).\n\
             gase = {{ {gase_git}, default-features = false }}\n\n\
             [features]\ndefault = {default}\n\
             # gase's window, sound and gamepads (needs the SDL2 development package).\n\
             window = [\"gase/sdl\"]\n\n\
             # Generated code is big: optimise dependencies, keep our own build fast.\n\
             [profile.dev]\nopt-level = 1\n\n[profile.dev.package.\"*\"]\nopt-level = 3\n\n\
             [profile.release]\ncodegen-units = 16\nlto = \"off\"\n",
            runtime.toml()
        ),
    )?;
    fs::create_dir_all(out.join("src"))?;
    fs::write(out.join("src/main.rs"), MAIN.replace("@TITLE@", title))?;
    fs::create_dir_all(out.join("mobile"))?;
    fs::write(
        out.join("mobile/Cargo.toml"),
        format!(
            "# \"{title}\" as an Android/iOS app. Regenerate rather than edit.\n\
             [package]\nname = \"{id}-mobile\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
             # gase's phone build turns this into libmain.so (Android) or a static\n\
             # library (iOS) with `cargo rustc --crate-type …`.\n\
             [lib]\npath = \"lib.rs\"\n\n\
             [dependencies]\ngase-recomp-runtime = {}\n\
             # gase's phone shell, without its own SDL_main: ours is in lib.rs.\n\
             gase-mobile = {{ {gase_git}, default-features = false }}\n",
            runtime.toml()
        ),
    )?;
    fs::write(out.join("mobile/lib.rs"), MOBILE.replace("@TITLE@", title))?;
    fs::write(
        out.join("BUILD.md"),
        BUILD
            .replace("@TITLE@", title)
            .replace("@ID@", &id)
            // iOS bundle identifiers allow letters, digits, '-' and '.'.
            .replace("@IOS_ID@", &id.replace('_', "-"))
            .replace("@GASE_REV@", GASE_REV),
    )?;
    Ok(files.len())
}

const MAIN: &str = r#"//! "@TITLE@", statically recompiled by gase-recomp.
//!
//! `src/recomp/` holds the game's 68000 code translated to Rust. Everything
//! else is the gase emulator: this program registers the recompiled code
//! for the game's ROM, then runs gase, which uses it for that ROM.

mod recomp;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    gase_recomp_runtime::register(recomp::ROM_CRC32, recomp::cpu);
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // A word that is neither an option, an option's value nor a file is
    // a typo, usually `cargo run release` for `cargo run --release`.
    if let Some(word) = stray_word(&args) {
        eprintln!("error: `{word}` is not a ROM file");
        if word == "release" {
            eprintln!("hint: build in release mode with `cargo run --release`");
        }
        return ExitCode::FAILURE;
    }
    // No ROM on the command line: open the one shipped next to the game.
    if !args.iter().any(|a| Path::new(a).is_file()) {
        if let Some(rom) = default_rom() {
            args.push(rom.to_string_lossy().into_owned());
        }
    }
    gase::run_command_line(args)
}

/// The first argument that does not start with `-`, does not follow an
/// option (`--frames 600`) and is not an existing file.
fn stray_word(args: &[String]) -> Option<&str> {
    args.iter().enumerate().find_map(|(i, a)| {
        let after_option = i > 0 && args[i - 1].starts_with('-');
        let stray = !a.starts_with('-') && !after_option && !Path::new(a).exists();
        stray.then_some(a.as_str())
    })
}

/// `rom.bin` in the current folder, or next to the program.
fn default_rom() -> Option<PathBuf> {
    let here = PathBuf::from("rom.bin");
    if here.is_file() {
        return Some(here);
    }
    let exe = std::env::current_exe().ok()?;
    let next_to_exe = exe.parent()?.join("rom.bin");
    next_to_exe.is_file().then_some(next_to_exe)
}
"#;

const MOBILE: &str = r#"//! "@TITLE@" as an Android or iOS app.
//!
//! A phone starts an app through its own framework, and SDL then calls the
//! C function `SDL_main` (see gase's mobile/README.md). This one registers
//! the recompiled code, exactly like `src/main.rs` does on a desktop, and
//! runs gase's phone shell with the ROM built into the app.

#[allow(clippy::all, unused_parens, unused_imports, dead_code)]
#[path = "../src/recomp/mod.rs"]
mod recomp;

use std::ffi::{c_char, c_int};

/// The app's entry point, called by SDL.
///
/// # Safety
///
/// SDL passes C's `argc` and `argv`.
#[allow(non_snake_case)] // the name is SDL's
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    gase_recomp_runtime::register(recomp::ROM_CRC32, recomp::cpu);
    // SAFETY: as promised by our caller.
    let args = unsafe { gase_mobile::args(argc, argv) };
    gase_mobile::run_game(&args, recomp::TITLE, include_bytes!("../rom.bin"))
}
"#;

const BUILD: &str = r#"# @TITLE@

Generated by **gase-recomp**: the game's 68000 code, translated to Rust and
run by the [gase](https://github.com/bthuner/gase) emulator.

```sh
cargo run --release                     # gase's window, menus, save states, gamepads
cargo run --release -- --headless --frames 600 --screenshot shot.png
cargo run --release --no-default-features -- --headless --bench   # no SDL2 needed
```

`rom.bin` (here or next to the program) is opened at start. Any ROM opened
from the menus works too: only this one runs recompiled code (matched by its
CRC-32), the others run on gase's interpreter as usual. All of gase's options
apply (`--help`).

## Code the analysis missed

When the game reaches code the static analysis did not find (a jump table,
code copied to RAM...), gase's 68000 interpreter runs it, so the game keeps
going, and the address is appended to `gase_missing.txt`. Run
`gase-recomp recomp <rom> -o <this folder>` again: the file is picked up and
those addresses become recompiled functions. This is the *coverage feedback
loop* described in the making-of.

## On a phone

`mobile/` is the same game as an app: it exports `SDL_main`, registers the
same recompiled code and carries `rom.bin` inside. gase's own Android and
iOS projects build it.

The easy way for Android is gase-recomp's studio: pick the ROM, press
Build, get the APK (`cargo run --release -p gase-recomp-studio` in a
gase-recomp checkout). By hand, get gase at the revision this game uses:

```sh
git clone https://github.com/bthuner/gase && git -C gase checkout @GASE_REV@
```

**Android** (Android SDK + NDK 27, `rustup target add aarch64-linux-android`):

```sh
gradle -p gase/mobile/android assembleRelease -PgaseAbis=arm64-v8a \
    -PgaseCrate="$PWD/mobile/Cargo.toml:@ID@-mobile" \
    -PgaseAppId=io.github.gase_recomp.@ID@ -PgaseAppName="@TITLE@"
adb install gase/mobile/android/app/build/outputs/apk/release/app-release.apk
```

**iOS** (a Mac with Xcode and XcodeGen, `rustup target add aarch64-apple-ios
aarch64-apple-ios-sim`):

```sh
GASE_MOBILE_MANIFEST="$PWD/mobile/Cargo.toml" GASE_MOBILE_PACKAGE=@ID@-mobile \
GASE_APP_NAME="@TITLE@" GASE_APP_ID=io.github.gase-recomp.@IOS_ID@ \
    gase/mobile/ios/build.sh
```

The app installs next to gase (its own identifier), opens the game at
start, and has gase's touch controls, menus and save states. Each
processor family (ABI) compiles the game again, so build only the one your
phone needs (`arm64-v8a` for nearly all of them). See gase's
`mobile/README.md` for signing, devices and troubleshooting.

## A bug the emulator does not have

If something goes wrong here but not when gase's interpreter runs the same
ROM, a routine was translated wrong. Find it by bisection: run address
ranges on the interpreter anyway, and see whether the bug goes away.

```sh
GASE_RECOMP_INTERPRET=all cargo run --release          # everything: bug gone?
GASE_RECOMP_INTERPRET=0-0xFFFFF cargo run --release    # first half of the ROM
GASE_RECOMP_INTERPRET=0x100000-0x17FFFF cargo run --release
```

Keep halving the range that makes the bug disappear. A save state made just
before the bug makes each try quick. Please report the final range!
"#;

#[cfg(test)]
mod tests {
    /// The runtime and the generated crates must use the same gase.
    #[test]
    fn gase_revision_matches_the_workspace() {
        let root = include_str!("../../../Cargo.toml");
        assert!(
            root.contains(super::GASE_REV),
            "update GASE_REV or the root Cargo.toml"
        );
    }
}
