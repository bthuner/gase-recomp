//! End-to-end test: assemble a ROM by hand, recompile it, run it on gase's
//! chips, and check what the game wrote to RAM and drew on screen.

#![allow(dead_code)]

include!("test_rom.rs");

#[allow(clippy::all, unused_parens, unused_imports)]
mod first_pass {
    include!(concat!(env!("OUT_DIR"), "/first_pass.rs"));
}

#[allow(clippy::all, unused_parens, unused_imports)]
mod second_pass {
    include!(concat!(env!("OUT_DIR"), "/second_pass.rs"));
}

#[cfg(test)]
mod tests {
    use gase_core::{Cartridge, Config, Genesis};
    use gase_recomp_runtime::{Code, Recompiled};

    use super::test_rom::*;

    const ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rom.bin"));

    /// Play `frames` frames on a gase console driven by the recompiled
    /// code, the way every gase frontend does; return the console and the
    /// addresses the interpreter had to run (the `gase_missing.txt` lines).
    fn play(code: Code, frames: u32, name: &str) -> (Genesis, Vec<String>) {
        let log =
            std::env::temp_dir().join(format!("gase-recomp-e2e-{name}-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let cpu = Recompiled::new(code, "test").log_to(Some(log.to_string_lossy().into_owned()));
        let mut console = Genesis::new(Cartridge::from_bytes(ROM).unwrap(), &Config::default());
        console.set_external_cpu(Some(Box::new(cpu)));
        for _ in 0..frames {
            console.run_frame();
        }
        let missing = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        (console, missing.lines().map(str::to_string).collect())
    }

    fn check(console: &Genesis, frames: u32) {
        let ram = &console.hw.ram;
        let r = |a: u32| usize::try_from(a & 0xFFFF).unwrap();
        let count = u16::from_be_bytes([ram[r(FRAMES)], ram[r(FRAMES) + 1]]);
        let vblanks = u32::from_be_bytes(ram[r(VBLS)..r(VBLS) + 4].try_into().unwrap());
        assert!(
            (frames - 2..=frames).contains(&vblanks),
            "{vblanks} VBlanks in {frames} frames"
        );
        assert_eq!(
            u32::from(count),
            vblanks,
            "the main loop runs once per VBlank"
        );
        let jt: u32 = ram[r(JT)..r(JT) + 4].iter().map(|&b| u32::from(b)).sum();
        assert_eq!(jt, vblanks, "one jump-table case per frame");
        assert!(
            ram[r(JT)..r(JT) + 4]
                .iter()
                .all(|&b| u32::from(b) >= vblanks / 4 - 1),
            "every case reached"
        );
        assert_eq!(ram[r(ICALL)], count as u8, "indirect call");
        assert_eq!(ram[r(SCALL)], count as u8, "static call");

        let frame = console.frame();
        assert_eq!((frame.width, frame.height), (320, 224));
        let px = |x: usize, y: usize| frame.pixels[y * frame.stride + x] & 0xFF_FFFF;
        assert_eq!(px(10, 82), 0xFF_0000, "row 10: red tiles");
        let checker = [px(10, 98), px(11, 98)];
        assert!(
            checker.contains(&0x00_FF00) && checker.contains(&0x00_00FF),
            "row 12: green/blue checkerboard, got {checker:06X?}"
        );
    }

    /// First pass: `jsr (a0)` leads to code the analysis never saw. The
    /// interpreter runs it, the game keeps going, the address is recorded.
    #[test]
    fn unknown_code_runs_on_the_interpreter_and_is_recorded() {
        let (console, missing) = play(super::first_pass::code(), 120, "first");
        check(&console, 120);
        assert_eq!(missing, [format!("0x{INDIRECT_SUB:06X}")]);
    }

    /// Second pass, after the coverage feedback loop: everything recompiled.
    #[test]
    fn after_the_feedback_loop_everything_is_recompiled() {
        let (console, missing) = play(super::second_pass::code(), 120, "second");
        check(&console, 120);
        assert!(missing.is_empty(), "still interpreted: {missing:?}");
    }

    /// Same pictures and RAM as gase's own interpreter running the ROM.
    #[test]
    fn matches_the_interpreter_frame_for_frame() {
        let (recompiled, _) = play(super::second_pass::code(), 60, "versus");
        let mut interpreted = Genesis::new(Cartridge::from_bytes(ROM).unwrap(), &Config::default());
        for _ in 0..60 {
            interpreted.run_frame();
        }
        assert_eq!(
            recompiled.frame().pixels,
            interpreted.frame().pixels,
            "picture"
        );
        let r = |g: &Genesis| g.hw.ram[..0x20].to_vec();
        assert_eq!(r(&recompiled), r(&interpreted), "game state in RAM");
    }

    /// Save states made while running recompiled code load back and agree.
    #[test]
    fn save_states_work_with_recompiled_code() {
        let (mut a, _) = play(super::second_pass::code(), 30, "state");
        let state = a.save_state();
        let (mut b, _) = play(super::second_pass::code(), 1, "state2");
        b.load_state(&state).unwrap();
        for _ in 0..30 {
            a.run_frame();
            b.run_frame();
        }
        assert_eq!(a.hw.ram, b.hw.ram);
        assert_eq!(a.frame().pixels, b.frame().pixels);
    }
}
