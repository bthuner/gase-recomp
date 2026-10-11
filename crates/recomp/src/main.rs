//! The `gase-recomp` command line.
//!
//! ```text
//! gase-recomp info   ROM
//! gase-recomp disasm ROM [--function HEX] [--entries FILE]
//! gase-recomp recomp ROM -o DIR [--platform desktop|android|ios|headless] [--entries FILE] [--runtime PATH]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gase_recomp::analysis::analyze;
use gase_recomp::codegen::Options;
use gase_recomp::project::{RuntimeSource, parse_entries, write_project};
use gase_recomp::rom::{Rom, vector_name};

const USAGE: &str = "\
gase-recomp: statically recompile a Sega Mega Drive / Genesis ROM to Rust

usage:
  gase-recomp info   ROM
  gase-recomp disasm ROM [--function HEX] [--entries FILE]
  gase-recomp recomp ROM -o DIR [--platform desktop|android|ios|headless] [--entries FILE] [--runtime PATH]
                     [--chunk N] [--max-block N]   code shape (see the making-of)
";

fn run(args: &[String]) -> Result<(), String> {
    let (cmd, rest) = args.split_first().ok_or(USAGE)?;
    let mut rom_path = None;
    let (mut out, mut platform, mut function, mut runtime) =
        (None, "desktop".to_string(), None, None);
    let mut entries = BTreeMap::new();
    let mut opts = Options::default();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "-o" | "--output" => out = Some(PathBuf::from(value()?)),
            "-p" | "--platform" => platform = value()?,
            "--function" => {
                function = Some(
                    u32::from_str_radix(value()?.trim_start_matches("0x"), 16)
                        .map_err(|e| e.to_string())?,
                )
            }
            "--entries" => parse_entries(Path::new(&value()?), &mut entries)?,
            "--runtime" => runtime = Some(PathBuf::from(value()?)),
            // Code shape knobs (compile time vs. jumps between functions).
            "--chunk" => opts.chunk = value()?.parse().map_err(|e| format!("--chunk: {e}"))?,
            "--max-block" => {
                opts.max_block = value()?.parse().map_err(|e| format!("--max-block: {e}"))?
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ => rom_path = Some(a.clone()),
        }
    }
    let rom_path = rom_path.ok_or(USAGE)?;
    let raw = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let rom = Rom::from_bytes(&raw).map_err(|e| e.to_string())?;

    match cmd.as_str() {
        "info" => {
            let h = &rom.header;
            println!(
                "file       {rom_path} ({}, {} bytes)",
                if rom.smd { "smd" } else { "bin" },
                rom.data.len()
            );
            println!("console    {}", h.console);
            println!("title      {} / {}", h.title_overseas, h.title_domestic);
            println!("serial     {}", h.serial);
            println!("region     {}", h.region);
            let ok = if rom.computed_checksum() == h.checksum {
                "OK".to_string()
            } else {
                format!("MISMATCH (computed ${:04X})", rom.computed_checksum())
            };
            println!("checksum   ${:04X} {ok}", h.checksum);
            println!("crc32      {:08X}", rom.crc32());
            for n in [0, 1, 2, 3, 4, 28, 30, 32] {
                println!(
                    "  vector {n:2} {:<20} ${:08X}",
                    vector_name(n),
                    rom.vectors[n]
                );
            }
        }
        "disasm" => {
            let program = analyze(&rom, &entries);
            for (entry, f) in &program.functions {
                if function.is_some_and(|x| x != *entry) {
                    continue;
                }
                println!("\n; ---- {} (${entry:06X}) ----", f.name);
                for (addr, ins) in &f.instructions {
                    println!("{addr:06X}  {}", ins.text());
                }
            }
        }
        "recomp" => {
            let out = out.ok_or("recomp needs -o DIR")?;
            let missing = out.join("gase_missing.txt");
            if missing.exists() {
                println!(
                    "[1/4] using addresses discovered at run time: {}",
                    missing.display()
                );
                parse_entries(&missing, &mut entries)?;
            }
            println!(
                "[1/4] ROM       \"{}\", {} KiB, checksum {}",
                rom.title(),
                rom.data.len() / 1024,
                if rom.computed_checksum() == rom.header.checksum {
                    "OK"
                } else {
                    "mismatch"
                }
            );
            let program = analyze(&rom, &entries);
            println!(
                "[2/4] analysis  {} functions, {} instructions",
                program.functions.len(),
                program.instruction_count()
            );
            for (a, why) in &program.rejected_entries {
                println!("      ! entry ${a:06X} rejected: {why}");
            }
            // Every project builds for all of them; the platform picks the
            // desktop default (window or not) and what we print at the end.
            let desktop = match platform.as_str() {
                "desktop" | "android" | "ios" => true,
                "headless" => false,
                p => {
                    return Err(format!(
                        "unknown platform {p:?} (desktop, android, ios or headless)"
                    ));
                }
            };
            let runtime = runtime.map_or_else(RuntimeSource::default_for_this_build, |p| {
                RuntimeSource::Path(p.canonicalize().unwrap_or(p))
            });
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            let n = write_project(&program, &out, desktop, &runtime, &opts)
                .map_err(|e| e.to_string())?;
            println!(
                "[3/4] codegen   {n} Rust files in {}",
                out.join("src/recomp").display()
            );
            println!(
                "[4/4] project   crate written to {}: cd there and `cargo run --release`",
                out.display()
            );
            if matches!(platform.as_str(), "android" | "ios") {
                println!(
                    "      {platform}: see \"On a phone\" in {}",
                    out.join("BUILD.md").display()
                );
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
