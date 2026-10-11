//! A recompiled game as gase sees it: an [`ExternalCpu`].
//!
//! gase runs a console frame by frame. For a recompiled game, each frame
//! hands the 68000's state (in gase's interpreter) and the console to
//! [`Recompiled::run_frame`], which builds a [`Cx`], runs the recompiled
//! code until the frame ends, and stores the state back. Everything else
//! (window, menus, save states, rewind, controls, sound) is gase's.
//!
//! ## Finding a miscompiled routine
//!
//! When a recompiled game misbehaves where gase's interpreter does not,
//! some routine was translated wrong. `GASE_RECOMP_INTERPRET` runs chosen
//! address ranges on the interpreter anyway, so you can bisect:
//!
//! ```sh
//! GASE_RECOMP_INTERPRET=all cargo run --release           # everything: bug gone?
//! GASE_RECOMP_INTERPRET=0-0xFFFFF cargo run --release     # first half of the ROM
//! GASE_RECOMP_INTERPRET=0x80000-0xBFFFF,0x1234 cargo run --release
//! ```
//!
//! If the bug disappears, the culprit is in the ranges; halve them and try
//! again. A save state made just before the bug makes each try quick.

use std::collections::BTreeSet;

use gase_core::system::external::{self, External, ExternalCpu};
use gase_m68k::M68k;

use crate::cpu::{Code, Cx};

/// The code of a recompiled game, ready to drive a gase console.
#[derive(Clone, Debug)]
pub struct Recompiled {
    code: Code,
    title: &'static str,
    /// Entry points the interpreter had to run, over the whole session.
    missing: BTreeSet<u32>,
    /// Where newly found ones are appended (the coverage feedback loop).
    missing_log: Option<String>,
    /// Ranges run on the interpreter anyway (`GASE_RECOMP_INTERPRET`).
    forced: Vec<(u32, u32)>,
}

impl Recompiled {
    #[must_use]
    pub fn new(code: Code, title: &'static str) -> Self {
        let forced = std::env::var("GASE_RECOMP_INTERPRET")
            .map(|v| parse_ranges(&v))
            .unwrap_or_default();
        if !forced.is_empty() {
            eprintln!("gase-recomp: interpreting {forced:06X?} (GASE_RECOMP_INTERPRET)");
        }
        Self {
            code,
            title,
            missing: BTreeSet::new(),
            missing_log: Some("gase_missing.txt".into()),
            forced,
        }
    }

    /// Run these address ranges (inclusive) on the interpreter.
    #[must_use]
    pub fn interpret(mut self, ranges: Vec<(u32, u32)>) -> Self {
        self.forced = ranges;
        self
    }

    /// Append the addresses found at run time to `path` instead of
    /// `gase_missing.txt` (`None`: don't write them anywhere).
    #[must_use]
    pub fn log_to(mut self, path: Option<String>) -> Self {
        self.missing_log = path;
        self
    }

    /// Entry points that ran on the interpreter so far.
    #[must_use]
    pub fn missing(&self) -> &BTreeSet<u32> {
        &self.missing
    }
}

impl ExternalCpu for Recompiled {
    fn run_frame(&mut self, cpu: &mut M68k, console: &mut External<'_>) {
        let mut cx = Cx::new(cpu, console, self.code);
        cx.missing = std::mem::take(&mut self.missing);
        cx.missing_log = self.missing_log.clone();
        cx.forced = &self.forced;
        cx.run_frame();
        self.missing = cx.finish();
    }

    fn clone_box(&self) -> Box<dyn ExternalCpu> {
        Box::new(self.clone())
    }

    fn describe(&self) -> String {
        format!("\"{}\" recompiled by gase-recomp", self.title)
    }
}

/// `all`, or ranges like `0x1000-0x1FFF,0x5A2C` (hex with or without
/// `0x`, or decimal).
#[must_use]
pub fn parse_ranges(text: &str) -> Vec<(u32, u32)> {
    let number = |s: &str| {
        let s = s.trim();
        match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => s.parse().ok(),
        }
    };
    text.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.eq_ignore_ascii_case("all") {
                return Some((0, 0xFF_FFFF));
            }
            match part.split_once('-') {
                Some((lo, hi)) => Some((number(lo)?, number(hi)?)),
                None => number(part).map(|a| (a, a)),
            }
        })
        .collect()
}

/// Make every gase console created for the ROM with this CRC-32 run the
/// recompiled code. A generated game calls it first thing in `main`.
pub fn register(rom_crc32: u32, factory: external::Factory) {
    external::register(rom_crc32, factory);
}

#[cfg(test)]
mod tests {
    #[test]
    fn ranges() {
        assert_eq!(super::parse_ranges("all"), [(0, 0xFF_FFFF)]);
        assert_eq!(
            super::parse_ranges("0x1000-0x1FFF, 0x5A2C,16-32"),
            [(0x1000, 0x1FFF), (0x5A2C, 0x5A2C), (16, 32)]
        );
        assert!(super::parse_ranges("").is_empty());
    }
}
