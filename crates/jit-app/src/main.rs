//! `gase-jit`: gase, with the JIT as the 68000 of every game.
//!
//! ```sh
//! cargo run --release -p gase-recomp-jit-app -- game.md
//! cargo run --release -p gase-recomp-jit-app -- --headless --bench game.md
//! ```
//!
//! All of gase's options apply (`--help`).

use std::process::ExitCode;

fn main() -> ExitCode {
    gase_recomp_jit_app::register();
    let args: Vec<String> = std::env::args().skip(1).collect();
    gase::run_command_line(args)
}
