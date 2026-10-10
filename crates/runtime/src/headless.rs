//! Run a recompiled game without a window: for tests, CI and benchmarks.
//!
//! ```text
//! game --headless ROM [--frames N] [--ppm out.ppm] [--ram ram.bin] [--pad HEX]
//! ```

use std::time::Instant;

use gase_core::Buttons;

use crate::game::{Game, GameInfo};

pub fn main(info: &GameInfo, args: &[String]) -> Result<(), String> {
    let mut rom_path = None;
    let (mut frames, mut ppm, mut ram, mut pad) = (60u64, None, None, 0u16);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--headless" => {}
            "--frames" => frames = value()?.parse().map_err(|e| format!("--frames: {e}"))?,
            "--ppm" => ppm = Some(value()?),
            "--ram" => ram = Some(value()?),
            "--pad" => {
                pad = u16::from_str_radix(&value()?, 16).map_err(|e| format!("--pad: {e}"))?
            }
            _ => rom_path = Some(a.clone()),
        }
    }
    let rom_path = rom_path.unwrap_or_else(|| "rom.bin".into());
    let rom = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let mut game = Game::new(info, &rom, 44_100)?;
    game.set_buttons(0, Buttons(pad));
    let start = Instant::now();
    for _ in 0..frames {
        game.run_frame();
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "[gase-recomp] {frames} frames in {secs:.3} s ({:.0} fps), {} cycles",
        frames as f64 / secs.max(1e-9),
        game.cx.cycles
    );
    if !game.cx.missing.is_empty() {
        println!(
            "[gase-recomp] interpreted (not recompiled) entry points: {:06X?}",
            game.cx.missing
        );
    }
    if let Some(path) = ppm {
        let (pixels, w, h, stride) = game.frame();
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for y in 0..h {
            for &p in &pixels[y * stride..y * stride + w] {
                out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        std::fs::write(&path, out).map_err(|e| format!("{path}: {e}"))?;
    }
    if let Some(path) = ram {
        std::fs::write(&path, game.ram()).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}
