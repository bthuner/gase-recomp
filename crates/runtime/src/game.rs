//! A recompiled game, ready to be driven one frame at a time.
//!
//! This is the shape every frontend (headless, SDL desktop, and later
//! gase's mobile and web shells) expects from a console: run a frame, show
//! the picture, play the sound, read the pads.

use gase_core::{Buttons, Cartridge};

use crate::{Code, Cx, GenesisMachine};

/// What the generated crate tells the frontend about itself.
#[derive(Clone, Copy, Debug)]
pub struct GameInfo {
    pub title: &'static str,
    pub rom_crc32: u32,
    pub rom_size: usize,
    pub code: Code<GenesisMachine>,
}

/// A recompiled game running on gase's chips.
#[derive(Debug)]
pub struct Game {
    pub cx: Cx<GenesisMachine>,
}

impl Game {
    /// Boot `rom`, which must be the ROM the code was recompiled from: the
    /// code reads its graphics, sound and data from it.
    pub fn new(info: &GameInfo, rom: &[u8], sample_rate: u32) -> Result<Self, String> {
        let crc = crate::crc32(rom);
        if rom.len() != info.rom_size || crc != info.rom_crc32 {
            return Err(format!(
                "this ROM (CRC32 {crc:08X}, {} bytes) is not the one \"{}\" was recompiled from (CRC32 {:08X}, {} bytes)",
                rom.len(),
                info.title,
                info.rom_crc32,
                info.rom_size
            ));
        }
        let cart = Cartridge::from_bytes(rom).map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            cx: Cx::new(GenesisMachine::new(cart, sample_rate), info.code),
        })
    }

    pub fn run_frame(&mut self) {
        self.cx.run_frame();
    }

    pub fn set_buttons(&mut self, port: usize, buttons: Buttons) {
        self.cx.m.set_buttons(port, buttons);
    }

    /// `0x00RRGGBB` pixels, width, height, stride.
    #[must_use]
    pub fn frame(&self) -> (&[u32], usize, usize, usize) {
        self.cx.m.frame()
    }

    pub fn drain_audio(&mut self, out: &mut Vec<i16>) {
        self.cx.m.drain_audio(out);
    }

    /// The 64 KiB of work RAM (handy for tests and tools).
    #[must_use]
    pub fn ram(&self) -> &[u8] {
        &self.cx.m.hw.ram
    }
}
