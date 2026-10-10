//! A recompiled game as gase sees it: an [`ExternalCpu`].
//!
//! gase runs a console frame by frame. For a recompiled game, each frame
//! hands the 68000's state (in gase's interpreter) and the console to
//! [`Recompiled::run_frame`], which builds a [`Cx`], runs the recompiled
//! code until the frame ends, and stores the state back. Everything else
//! (window, menus, save states, rewind, controls, sound) is gase's.

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
}

impl Recompiled {
    #[must_use]
    pub fn new(code: Code, title: &'static str) -> Self {
        Self {
            code,
            title,
            missing: BTreeSet::new(),
            missing_log: Some("gase_missing.txt".into()),
        }
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

/// Make every gase console created for the ROM with this CRC-32 run the
/// recompiled code. A generated game calls it first thing in `main`.
pub fn register(rom_crc32: u32, factory: external::Factory) {
    external::register(rom_crc32, factory);
}
