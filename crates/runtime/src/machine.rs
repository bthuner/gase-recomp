//! What the recompiled CPU needs from the rest of the console.
//!
//! Recompiled code replaces the 68000, nothing else. Everything around it
//! (memory map, video, sound, Z80, pads) is a [`Machine`]: on a real game,
//! gase's own console seen through
//! [`External`](gase_core::system::external::External), the inverted
//! scheduler gase provides for external CPUs; in tests, [`FlatMachine`],
//! 16 MiB of RAM.
//!
//! The machine also sees the 68000 through [`gase_m68k::Bus`], exactly
//! like gase's interpreter does, so both CPUs can share it: that is how
//! the interpreter fallback works.

use gase_core::system::external::External;
use gase_m68k::Bus;

/// The console around the recompiled 68000.
pub trait Machine: Bus {
    /// The 68000's clock, in CPU cycles since power-on.
    fn cycles(&self) -> u64 {
        0
    }

    /// The 68000's clock before a memory access: the VDP's FIFO and the
    /// sound chips need to know *when* an access happens.
    fn set_clock(&mut self, cycles: u64);

    /// Run every other chip up to `cycles` (scanlines, Z80, sound) and
    /// return when the next event is due (end of line, VBlank...).
    fn catch_up(&mut self, cycles: u64) -> u64;

    /// The CPU clock once the waits imposed by the hardware are counted
    /// (VDP FIFO full, Z80 bus arbitration, a 68000-to-VDP DMA freeze).
    fn stalled_clock(&self, cycles: u64) -> u64 {
        cycles
    }

    /// Is the frame over? Then the CPU returns to its frontend.
    fn frame_done(&self) -> bool {
        false
    }

    /// Highest interrupt level currently requested (0 = none).
    fn irq_level(&self) -> u8 {
        0
    }
}

/// gase's console, as seen by an external CPU during one frame.
impl Machine for External<'_> {
    fn cycles(&self) -> u64 {
        External::cycles(self)
    }
    #[inline]
    fn set_clock(&mut self, cycles: u64) {
        External::set_clock(self, cycles);
    }
    fn catch_up(&mut self, cycles: u64) -> u64 {
        External::catch_up(self, cycles)
    }
    fn stalled_clock(&self, cycles: u64) -> u64 {
        External::stalled_clock(self, cycles)
    }
    fn frame_done(&self) -> bool {
        External::frame_done(self)
    }
    fn irq_level(&self) -> u8 {
        self.interrupt_level()
    }
}

/// 16 MiB of flat RAM and nothing else: the machine of the differential
/// tests, where the recompiled code and the gase interpreter must agree
/// instruction for instruction.
#[derive(Clone)]
pub struct FlatMachine {
    pub mem: Vec<u8>,
}

impl std::fmt::Debug for FlatMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FlatMachine")
    }
}

impl Default for FlatMachine {
    fn default() -> Self {
        Self {
            mem: vec![0; 0x100_0000],
        }
    }
}

impl Bus for FlatMachine {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.mem[(addr & 0xFF_FFFF) as usize]
    }
    fn read_word(&mut self, addr: u32) -> u16 {
        let a = (addr & 0xFF_FFFE) as usize;
        u16::from_be_bytes([self.mem[a], self.mem[a + 1]])
    }
    fn write_byte(&mut self, addr: u32, value: u8) {
        self.mem[(addr & 0xFF_FFFF) as usize] = value;
    }
    fn write_word(&mut self, addr: u32, value: u16) {
        let a = (addr & 0xFF_FFFE) as usize;
        self.mem[a..a + 2].copy_from_slice(&value.to_be_bytes());
    }
}

impl Machine for FlatMachine {
    fn set_clock(&mut self, _cycles: u64) {}
    fn catch_up(&mut self, _cycles: u64) -> u64 {
        u64::MAX
    }
}
