//! Write a ready-to-build Rust crate for a recompiled ROM.
//!
//! ```text
//! <out>/
//!   Cargo.toml       gase-recomp-runtime + gase (the emulator's frontends)
//!   rom.bin          your ROM: graphics, sound and data are read from it
//!   src/main.rs      registers the recompiled code, then runs gase
//!   src/recomp/      the generated code (mod.rs + part_NNN.rs)
//!   BUILD.md
//! ```
//!
//! The game *is* gase with one more registered CPU: same window, menus,
//! save states, rewind and controls. The crate is a workspace of its own,
//! so it can live anywhere, even inside this repository.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::analysis::Program;
use crate::codegen::{Options, generate};

/// The gase revision every generated crate uses. It must be the one the
/// runtime is built against (the root `Cargo.toml`), so that Cargo builds
/// a single gase-core and the game's registration reaches gase's consoles.
pub const GASE_REV: &str = "79d6b1c5cfab02eb200f2077c4aa9dc2bd81ce49";

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
    fs::write(
        out.join("Cargo.toml"),
        format!(
            "# \"{title}\", recompiled by gase-recomp. Regenerate rather than edit.\n\
             [package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
             # A workspace of its own: the crate can live anywhere.\n[workspace]\n\n\
             [dependencies]\ngase-recomp-runtime = {}\n\
             # gase's frontends, at the revision the runtime uses (one gase-core).\n\
             gase = {{ git = \"https://github.com/bthuner/gase\", rev = \"{GASE_REV}\", default-features = false }}\n\n\
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
    fs::write(out.join("BUILD.md"), BUILD.replace("@TITLE@", title))?;
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
    // No ROM on the command line: open the one shipped next to the game.
    if !args.iter().any(|a| Path::new(a).is_file()) {
        if let Some(rom) = default_rom() {
            args.push(rom.to_string_lossy().into_owned());
        }
    }
    gase::run_command_line(args)
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
