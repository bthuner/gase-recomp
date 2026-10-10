//! The Mega Drive around the recompiled 68000, built from gase's chips.
//!
//! gase's own scheduler (`Genesis::run_frame`) drives the 68000: it steps
//! the interpreter one instruction at a time and handles the scanline
//! events in between. Here the roles are reversed: the recompiled code
//! runs freely, and at each poll point it asks the machine to *catch up*
//! with the time it has used. This module is that inverted scheduler; the
//! chips themselves (VDP, YM2612, PSG, Z80, I/O, cartridge) are gase's.
//!
//! Time is counted in master clock ticks: the 68000 runs at master / 7,
//! the Z80 at master / 15, and a scanline lasts 3420 ticks.

use gase_core::bus::{Hardware, Z80Bus};
use gase_core::{Buttons, Cartridge, Device, Region};
use gase_m68k::Bus;
use gase_sound::{DcBlocker, Resampler};
use gase_vdp::{HBLANK_START_CYCLE, MASTER_CYCLES_PER_LINE, VINT_CYCLE, VideoStandard};
use gase_z80::Z80;

use crate::machine::Machine;

/// Which scanline event comes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The first blank line: the VBlank interrupt fires 770 ticks in.
    Vint,
    /// The horizontal blank starts 2650 ticks into the line.
    Hblank,
    /// The line ends after 3420 ticks.
    End,
}

/// A Mega Drive minus its 68000.
pub struct GenesisMachine {
    pub hw: Hardware,
    pub z80: Z80,
    z80_clock: u64,
    /// The 68000's clock (master ticks) at its last memory access.
    cpu_clock: u64,
    line: u16,
    stage: Stage,
    frame_done: bool,
    pub frame_count: u64,
    resampler: Resampler,
    dc_blocker: DcBlocker,
    audio_out: Vec<i16>,
}

impl std::fmt::Debug for GenesisMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GenesisMachine {{ line: {}, frame: {} }}",
            self.line, self.frame_count
        )
    }
}

impl GenesisMachine {
    /// Power on a console with `cart` inserted, audio at `sample_rate`.
    #[must_use]
    pub fn new(cart: Cartridge, sample_rate: u32) -> Self {
        let r = cart.header.regions;
        let region = if r.americas || !(r.japan || r.europe) {
            Region::Americas
        } else if r.japan {
            Region::Japan
        } else {
            Region::Europe
        };
        let standard = if region.is_pal() {
            VideoStandard::Pal
        } else {
            VideoStandard::Ntsc
        };
        let rate = gase_core::audio::native_rate(standard.master_clock());
        let mut hw = Hardware {
            cart,
            ram: vec![0; 0x1_0000],
            zram: vec![0; 0x2000],
            vdp: gase_vdp::Vdp::new(standard),
            ym: gase_sound::Ym2612::new(),
            psg: gase_sound::Psg::new(),
            io: gase_core::io::Io::new(region),
            audio: gase_core::audio::AudioClock::default(),
            z80_busreq: false,
            z80_reset: true,
            z80_bank: 0,
            z80_irq: false,
            reset_requested: false,
            now: 0,
            line_start: 0,
            m68k_wait: 0,
            vdp_stall: 0,
        };
        hw.io.ports[0].device = Device::SixButton;
        hw.io.ports[1].device = Device::SixButton;
        hw.vdp.begin_line(0);
        let mut resampler = Resampler::new(rate, f64::from(sample_rate));
        resampler.set_gain(0.5);
        Self {
            hw,
            z80: Z80::new(),
            z80_clock: 0,
            cpu_clock: 0,
            line: 0,
            stage: Stage::Hblank,
            frame_done: false,
            frame_count: 0,
            resampler,
            dc_blocker: DcBlocker::new(10.0, rate),
            audio_out: Vec::new(),
        }
    }

    pub fn set_buttons(&mut self, port: usize, buttons: Buttons) {
        if let Some(p) = self.hw.io.ports.get_mut(port) {
            p.buttons = buttons;
        }
    }

    /// The last complete picture: `0x00RRGGBB` pixels, width, height, stride.
    #[must_use]
    pub fn frame(&self) -> (&[u32], usize, usize, usize) {
        let (w, h) = self.hw.vdp.frame_size();
        (self.hw.vdp.frame(), w, h, gase_vdp::MAX_WIDTH)
    }

    /// Move the audio produced so far (interleaved stereo) to `out`.
    pub fn drain_audio(&mut self, out: &mut Vec<i16>) {
        out.append(&mut self.audio_out);
    }

    fn line_start(&self) -> u64 {
        self.hw.line_start
    }

    fn next_event_tick(&self) -> u64 {
        self.line_start()
            + u64::from(match self.stage {
                Stage::Vint => VINT_CYCLE,
                Stage::Hblank => HBLANK_START_CYCLE,
                Stage::End => MASTER_CYCLES_PER_LINE,
            })
    }

    /// Advance the scanline state machine and the Z80 to master tick `now`.
    fn run_to(&mut self, now: u64) {
        loop {
            let event = self.next_event_tick();
            if now < event {
                self.run_z80(now);
                return;
            }
            self.run_z80(event);
            self.hw.now = event;
            match self.stage {
                Stage::Vint => {
                    self.hw.vdp.trigger_vint();
                    self.hw.z80_irq = true; // the Z80 gets VBlank too, for a line
                    self.stage = Stage::Hblank;
                }
                Stage::Hblank => {
                    self.hw.vdp.hblank();
                    self.stage = Stage::End;
                }
                Stage::End => self.next_line(event),
            }
            if self.frame_done {
                return;
            }
        }
    }

    fn next_line(&mut self, start: u64) {
        self.line += 1;
        if self.line == self.hw.vdp.total_lines() {
            self.line = 0;
            self.end_frame();
        }
        self.hw.line_start = start;
        self.hw.vdp.begin_line(self.line);
        let active = self.hw.vdp.active_lines();
        if self.line == active + 1 {
            self.hw.z80_irq = false;
        }
        self.stage = if self.line == active {
            Stage::Vint
        } else {
            Stage::Hblank
        };
    }

    fn end_frame(&mut self) {
        self.hw
            .audio
            .run_until(self.hw.now, &mut self.hw.ym, &mut self.hw.psg);
        for (l, r) in self.hw.audio.samples.drain(..) {
            let (l, r) = self.dc_blocker.process(l, r);
            self.resampler.push(l, r);
        }
        self.resampler.drain(&mut self.audio_out);
        self.frame_count += 1;
        self.frame_done = true;
    }

    fn run_z80(&mut self, target: u64) {
        if self.hw.z80_reset || self.hw.z80_busreq {
            if self.hw.z80_reset {
                self.z80.reset();
            }
            self.z80_clock = self.z80_clock.max(target);
            return;
        }
        while self.z80_clock < target {
            self.hw.now = self.z80_clock;
            self.z80.set_irq(self.hw.z80_irq);
            let cycles = self.z80.step(&mut Z80Bus(&mut self.hw));
            self.z80_clock += u64::from(cycles) * 15;
        }
    }

    /// A 68000-to-VDP DMA freezes the 68000 until the transfer is done.
    fn dma_freeze(&mut self) {
        while self.hw.vdp.dma_68k_active() && !self.frame_done {
            let event = self.next_event_tick();
            self.hw.vdp.advance((event - self.line_start()) as u32);
            if self.hw.vdp.dma_68k_active() {
                self.cpu_clock = self.cpu_clock.max(event);
            } else {
                let done = self.line_start() + u64::from(self.hw.vdp.dma_68k_done_at());
                self.cpu_clock = self.cpu_clock.max(done);
            }
            self.run_to(self.cpu_clock);
        }
    }

    /// Before an access to anything but ROM and RAM: bring the console up
    /// to the 68000's time, so the VDP and the sound chips see it in order.
    fn sync(&mut self) {
        self.run_to(self.cpu_clock);
        self.hw.now = self.cpu_clock;
    }

    /// After such an access: the time the hardware made the 68000 wait.
    fn charge_waits(&mut self) {
        self.cpu_clock += u64::from(std::mem::take(&mut self.hw.vdp_stall))
            + 7 * u64::from(std::mem::take(&mut self.hw.m68k_wait));
    }
}

impl Bus for GenesisMachine {
    #[inline]
    fn read_byte(&mut self, addr: u32) -> u8 {
        if addr >= 0xE0_0000 {
            return self.hw.ram[(addr & 0xFFFF) as usize];
        }
        if addr < 0x40_0000 {
            return self.hw.read_byte(addr);
        }
        self.sync();
        let v = self.hw.read_byte(addr);
        self.charge_waits();
        v
    }

    #[inline]
    fn read_word(&mut self, addr: u32) -> u16 {
        if addr >= 0xE0_0000 {
            let i = (addr & 0xFFFF) as usize;
            return u16::from_be_bytes([self.hw.ram[i], self.hw.ram[i + 1]]);
        }
        if addr < 0x40_0000 {
            return self.hw.read_word(addr);
        }
        self.sync();
        let v = self.hw.read_word(addr);
        self.charge_waits();
        v
    }

    #[inline]
    fn write_byte(&mut self, addr: u32, value: u8) {
        if addr >= 0xE0_0000 {
            self.hw.ram[(addr & 0xFFFF) as usize] = value;
            return;
        }
        self.sync();
        self.hw.write_byte(addr, value);
        self.charge_waits();
        self.dma_freeze();
    }

    #[inline]
    fn write_word(&mut self, addr: u32, value: u16) {
        if addr >= 0xE0_0000 {
            let i = (addr & 0xFFFF) as usize;
            self.hw.ram[i..i + 2].copy_from_slice(&value.to_be_bytes());
            return;
        }
        self.sync();
        self.hw.write_word(addr, value);
        self.charge_waits();
        self.dma_freeze();
    }

    fn interrupt_acknowledge(&mut self, level: u8) -> Option<u8> {
        self.hw.interrupt_acknowledge(level)
    }

    fn reset_devices(&mut self) {
        self.hw.reset_devices();
    }

    fn tas_write_byte(&mut self, addr: u32, value: u8) {
        // The Mega Drive's bus arbiter drops the write half of TAS on work RAM.
        if addr < 0xE0_0000 {
            self.write_byte(addr, value);
        }
    }
}

impl Machine for GenesisMachine {
    #[inline]
    fn set_clock(&mut self, cycles: u64) {
        self.cpu_clock = self.cpu_clock.max(cycles * 7);
    }

    fn catch_up(&mut self, cycles: u64) -> u64 {
        self.cpu_clock = self.cpu_clock.max(cycles * 7);
        self.run_to(self.cpu_clock);
        self.next_event_tick().div_ceil(7)
    }

    fn stalled_clock(&mut self, cycles: u64) -> u64 {
        cycles.max(self.cpu_clock.div_ceil(7))
    }

    fn take_frame_done(&mut self) -> bool {
        std::mem::take(&mut self.frame_done)
    }

    fn irq_level(&mut self) -> u8 {
        self.hw.vdp.interrupt_level()
    }
}
