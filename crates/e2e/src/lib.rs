//! End-to-end test: assemble a ROM by hand, recompile it, run it on gase's
//! chips, and check what the game wrote to RAM and drew on screen.

#![allow(dead_code)]

include!("test_rom.rs");

/// The console the generated code runs on.
pub type Machine = gase_recomp_runtime::GenesisMachine;

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
    use gase_recomp_runtime::{Game, GameInfo};

    use super::test_rom::*;

    const ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rom.bin"));

    fn play(code: gase_recomp_runtime::Code<super::Machine>, frames: u32) -> Game {
        let info = GameInfo {
            title: "test",
            rom_crc32: gase_recomp_runtime::crc32(ROM),
            rom_size: ROM.len(),
            code,
        };
        let mut game = Game::new(&info, ROM, 44_100).unwrap();
        game.cx.missing_log = None;
        for _ in 0..frames {
            game.run_frame();
        }
        game
    }

    fn check(game: &Game, frames: u32) {
        let ram = game.ram();
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

        let (pixels, w, h, stride) = game.frame();
        assert_eq!((w, h), (320, 224));
        assert_eq!(
            pixels[82 * stride + 10] & 0xFF_FFFF,
            0xFF_0000,
            "row 10: red tiles"
        );
        let checker = [
            pixels[98 * stride + 10] & 0xFF_FFFF,
            pixels[98 * stride + 11] & 0xFF_FFFF,
        ];
        assert!(
            checker.contains(&0x00_FF00) && checker.contains(&0x00_00FF),
            "row 12: green/blue checkerboard, got {checker:06X?}"
        );
    }

    /// First pass: `jsr (a0)` leads to code the analysis never saw. The
    /// interpreter runs it, the game keeps going, the address is recorded.
    #[test]
    fn unknown_code_runs_on_the_interpreter_and_is_recorded() {
        let game = play(super::first_pass::code(), 120);
        check(&game, 120);
        assert_eq!(
            game.cx.missing.iter().copied().collect::<Vec<_>>(),
            [INDIRECT_SUB]
        );
    }

    /// Second pass, after the coverage feedback loop: everything recompiled.
    #[test]
    fn after_the_feedback_loop_everything_is_recompiled() {
        let game = play(super::second_pass::code(), 120);
        check(&game, 120);
        assert!(
            game.cx.missing.is_empty(),
            "still interpreted: {:06X?}",
            game.cx.missing
        );
    }
}
