//! Runtime for Mega Drive games statically recompiled by `gase-recomp`.
//!
//! The recompiler turns each 68000 routine of a ROM into a Rust function
//! taking a [`Cx`]: the CPU's registers and flags, plus the console around
//! it, a [`Machine`]. The real machine, [`GenesisMachine`], is made of the
//! chips of the [gase](https://github.com/bthuner/gase) emulator: only the
//! 68000 is replaced by recompiled code, and gase's 68000 interpreter
//! stays on standby for the code the recompiler did not find.
//!
//! | module      | what you learn there                                     |
//! |-------------|----------------------------------------------------------|
//! | [`cpu`]     | blocks as a state machine, calls, poll points, IRQs      |
//! | [`ops`]     | the 68000's flags: X N Z V C, shifts, BCD, mul/div       |
//! | [`machine`] | the interface between the CPU and the rest of the console |
//! | [`genesis`] | an inverted scheduler on top of gase's chips             |

pub mod cpu;
pub mod game;
pub mod genesis;
pub mod headless;
pub mod machine;
pub mod ops;
#[cfg(feature = "sdl")]
pub mod sdl;

pub use game::{Game, GameInfo};
/// The chips, re-exported so a generated crate needs a single dependency.
pub use gase_core;

pub use cpu::{Code, Cx};
pub use genesis::GenesisMachine;
pub use machine::{FlatMachine, Machine};
pub use ops::{sext8, sext16};

/// The CRC-32 used by zip and PNG: a game checks it at start-up, to be sure
/// it runs with the ROM it was recompiled from.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// The `main` of a generated game: `--headless` runs without a window.
pub fn main(info: &GameInfo) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = cfg!(not(feature = "sdl")) || args.iter().any(|a| a == "--headless");
    #[cfg(feature = "sdl")]
    let result = if headless {
        headless::main(info, &args)
    } else {
        sdl::main(info, &args)
    };
    #[cfg(not(feature = "sdl"))]
    let result = {
        let _ = headless;
        headless::main(info, &args)
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
