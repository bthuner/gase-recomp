//! gase-recomp's JIT: 68000 code translated to native code *while the
//! game runs*, for any ROM, with no compiler on the machine.
//!
//! The static recompiler translates a whole game before it runs: it can
//! use rustc and LLVM, it sees all the code at once, and the result is a
//! program per game. A JIT (a dynamic recompiler) translates one *block*
//! the first time the CPU gets there, writes the machine code into memory,
//! and jumps to it; the next time, the block is already there. Nothing
//! needs to be built in advance, so it works for any ROM, on a phone too
//! (Android lets an app write and run machine code; iOS does not).
//!
//! ```text
//!  pc ──► cache? ──yes──► run the block (native code) ──► next pc
//!           │no
//!           ▼
//!  ir::block: decode (gase-recomp's decoder), measure the timings
//!           │       moveq, add Dn,Dn, Bcc…   →  native operations
//!           │       everything else         →  "interpret this one"
//!           ▼
//!  backend (x64.rs): write machine code with dynasm, add it to the cache
//! ```
//!
//! Between blocks the engine does what the runtime's poll points do for
//! the static recompiler: let the console catch up (scanlines, VBlank),
//! take interrupts, end the frame. Instructions the backend does not
//! translate yet run on gase's own interpreter, one at a time, with the
//! registers handed over and back: the JIT is correct for every game from
//! the start, and the difftest (tests/difftest.rs) checks it against the
//! interpreter instruction for instruction, cycles included.

mod ir;
#[cfg(target_arch = "x86_64")]
mod x64;

use std::collections::HashMap;
use std::fmt;

use gase_core::system::external::{External, ExternalCpu};
use gase_m68k::{Bus, M68k};
use gase_recomp_runtime::Machine;

pub use ir::{Alu, Base, Ea, Op, Size};

/// The 68000 registers the machine code works on. `repr(C)`: the backend
/// addresses the fields by offset.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub d: [u32; 8],
    /// `a[7]` is the active stack pointer.
    pub a: [u32; 8],
    pub pc: u32,
    /// The condition codes, one byte each, always 0 or 1.
    pub x: u8,
    pub n: u8,
    pub z: u8,
    pub v: u8,
    pub c: u8,
    pad: [u8; 3],
    /// 68000 cycles since power-on.
    pub cycles: u64,
    /// Compiled code may keep running (jumping between the blocks of a
    /// region) while `cycles` is below this: the next console event.
    pub limit: u64,
}

impl State {
    fn ccr(&self) -> u8 {
        self.x << 4 | self.n << 3 | self.z << 2 | self.v << 1 | self.c
    }

    fn set_ccr(&mut self, ccr: u8) {
        self.x = (ccr >> 4) & 1;
        self.n = (ccr >> 3) & 1;
        self.z = (ccr >> 2) & 1;
        self.v = (ccr >> 1) & 1;
        self.c = ccr & 1;
    }
}

/// gase's 68000 sees the console through this.
struct DynBus<'b>(&'b mut dyn Machine);

impl Bus for DynBus<'_> {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.0.read_byte(addr)
    }
    fn read_word(&mut self, addr: u32) -> u16 {
        self.0.read_word(addr)
    }
    fn write_byte(&mut self, addr: u32, value: u8) {
        self.0.write_byte(addr, value);
    }
    fn write_word(&mut self, addr: u32, value: u16) {
        self.0.write_word(addr, value);
    }
    fn interrupt_acknowledge(&mut self, level: u8) -> Option<u8> {
        self.0.interrupt_acknowledge(level)
    }
    fn reset_devices(&mut self) {
        self.0.reset_devices();
    }
    fn tas_write_byte(&mut self, addr: u32, value: u8) {
        self.0.tas_write_byte(addr, value);
    }
}

/// What a block runs with: the registers, and (for the interpreter
/// fallback) the console and gase's 68000. `repr(C)` with the state first:
/// the machine code gets a pointer to this.
#[repr(C)]
pub struct Ctx<'a> {
    pub st: State,
    /// gase's interpreter: the state between frames, and the fallback.
    /// It also keeps what native code never touches: the supervisor bit,
    /// the interrupt mask, the inactive stack pointer.
    cpu: &'a mut M68k,
    m: &'a mut dyn Machine,
    next_event: u64,
}

impl fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx")
            .field("st", &self.st)
            .finish_non_exhaustive()
    }
}

impl<'a> Ctx<'a> {
    fn new(cpu: &'a mut M68k, m: &'a mut dyn Machine) -> Self {
        let cycles = m.cycles();
        let next_event = m.catch_up(cycles);
        let mut ctx = Self {
            st: State {
                cycles,
                ..State::default()
            },
            cpu,
            m,
            next_event,
        };
        ctx.take_back();
        ctx
    }

    /// Hand the registers to the interpreter.
    fn hand_over(&mut self) {
        self.cpu.d = self.st.d;
        self.cpu.a = self.st.a;
        self.cpu.set_ccr(self.st.ccr());
        self.cpu.set_pc(&mut DynBus(&mut *self.m), self.st.pc);
    }

    /// Take them back.
    fn take_back(&mut self) {
        self.st.d = self.cpu.d;
        self.st.a = self.cpu.a;
        self.st.set_ccr(self.cpu.ccr());
        self.st.pc = self.cpu.pc() & 0xFF_FFFF;
    }

    /// Run one instruction (or take a pending interrupt) on gase's
    /// interpreter.
    pub fn interpret_one(&mut self) {
        self.hand_over();
        self.cpu.set_interrupt_level(self.m.irq_level());
        self.m.set_clock(self.st.cycles);
        let cycles = self.cpu.step(&mut DynBus(&mut *self.m));
        self.st.cycles += u64::from(cycles);
        // Waits the hardware imposed (DMA, VDP FIFO, Z80 bus), at once.
        self.st.cycles = self.m.stalled_clock(self.st.cycles);
        self.take_back();
    }

    // ---- memory, for native code ------------------------------------------
    //
    // As in the static recompiler's runtime: tell the console the time
    // before the access, and after an access to the Z80 area, I/O or the
    // VDP, count the waits the hardware imposed at once. Word accesses
    // ignore address bit 0 (gase's interpreter with address errors off).

    fn waited(&mut self, addr: u32) {
        if (0xA0_0000..0xE0_0000).contains(&(addr & 0xFF_FFFF)) {
            self.st.cycles = self.m.stalled_clock(self.st.cycles);
        }
    }

    pub fn read8(&mut self, addr: u32) -> u32 {
        self.m.set_clock(self.st.cycles);
        let v = u32::from(self.m.read_byte(addr & 0xFF_FFFF));
        self.waited(addr);
        v
    }

    pub fn read16(&mut self, addr: u32) -> u32 {
        self.m.set_clock(self.st.cycles);
        let v = u32::from(self.m.read_word(addr & 0xFF_FFFE));
        self.waited(addr);
        v
    }

    /// A long access is two word accesses, high word first.
    pub fn read32(&mut self, addr: u32) -> u32 {
        let hi = self.read16(addr);
        hi << 16 | self.read16(addr.wrapping_add(2))
    }

    pub fn write8(&mut self, addr: u32, v: u32) {
        self.m.set_clock(self.st.cycles);
        self.m.write_byte(addr & 0xFF_FFFF, v as u8);
        self.waited(addr);
    }

    pub fn write16(&mut self, addr: u32, v: u32) {
        self.m.set_clock(self.st.cycles);
        self.m.write_word(addr & 0xFF_FFFE, v as u16);
        self.waited(addr);
    }

    pub fn write32(&mut self, addr: u32, v: u32) {
        self.write16(addr, v >> 16);
        self.write16(addr.wrapping_add(2), v);
    }

    fn interrupt_pending(&mut self) -> bool {
        let level = self.m.irq_level();
        let mask = ((self.cpu.sr() >> 8) & 7) as u8;
        level == 7 || level > mask
    }
}

/// Where compiled blocks live.
#[derive(Debug)]
enum Backend {
    #[cfg(target_arch = "x86_64")]
    X64(Box<x64::X64>),
    /// No native backend for this processor yet: everything is
    /// interpreted (still correct, just not faster).
    #[cfg_attr(target_arch = "x86_64", allow(dead_code))]
    None,
}

/// Counters, to see how much of a game runs natively.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Regions compiled, and entries into compiled code.
    pub blocks_compiled: u64,
    pub blocks_run: u64,
    /// Instructions run outside compiled blocks (RAM code, interrupts).
    pub interpreted: u64,
}

/// The JIT, as a CPU gase can use instead of its interpreter.
#[derive(Debug)]
pub struct Jit {
    backend: Backend,
    /// pc → compiled block. ROM never changes, so nothing is invalidated.
    cache: HashMap<u32, Block>,
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug)]
struct Block(#[allow(dead_code)] usize);

/// Only code in ROM is compiled: code in RAM may be rewritten by the game.
const ROM_END: u32 = 0x40_0000;
/// Compiled code comes back to the engine at least this often (in 68000
/// cycles), even with no console event due: a frame is 128 000.
const SLICE: u64 = 20_000;

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}

impl Jit {
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(target_arch = "x86_64")]
            backend: Backend::X64(Box::new(x64::X64::new())),
            #[cfg(not(target_arch = "x86_64"))]
            backend: Backend::None,
            cache: HashMap::new(),
            stats: Stats::default(),
        }
    }

    /// The name of the native backend, if any.
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::X64(_) => "x86-64",
            Backend::None => "none (interpreter only)",
        }
    }

    /// Run until the console says the frame is over, or `stop` says so.
    /// Returns the final registers (also stored back into `cpu`).
    pub fn run(
        &mut self,
        cpu: &mut M68k,
        m: &mut dyn Machine,
        mut stop: impl FnMut(&State) -> bool,
    ) -> State {
        let mut ctx = Ctx::new(cpu, m);
        loop {
            if stop(&ctx.st) || ctx.cpu.is_halted() {
                break;
            }
            // Between blocks: the console's events, then interrupts.
            if ctx.st.cycles >= ctx.next_event {
                ctx.st.cycles = ctx.m.stalled_clock(ctx.st.cycles);
                ctx.next_event = ctx.m.catch_up(ctx.st.cycles);
                if ctx.m.frame_done() {
                    break;
                }
            }
            if ctx.cpu.is_stopped() || ctx.interrupt_pending() {
                let stopped = ctx.cpu.is_stopped();
                ctx.interpret_one();
                self.stats.interpreted += 1;
                if stopped && ctx.cpu.is_stopped() {
                    // Asleep until the next event (STOP).
                    ctx.st.cycles = ctx.st.cycles.max(ctx.next_event);
                }
                continue;
            }
            let pc = ctx.st.pc;
            if pc >= ROM_END {
                ctx.interpret_one();
                self.stats.interpreted += 1;
                continue;
            }
            self.run_block(&mut ctx, pc);
        }
        ctx.hand_over();
        ctx.st
    }

    fn run_block(&mut self, ctx: &mut Ctx<'_>, pc: u32) {
        match &mut self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::X64(x64) => {
                let block = match self.cache.get(&pc) {
                    Some(b) => *b,
                    None => {
                        let m = &mut *ctx.m;
                        let region = ir::region(|a| Some(m.read_word(a & 0xFF_FFFE)), pc, ROM_END);
                        let b = Block(x64.compile(&region));
                        self.cache.insert(pc, b);
                        self.stats.blocks_compiled += 1;
                        b
                    }
                };
                self.stats.blocks_run += 1;
                ctx.st.limit = ctx.next_event.min(ctx.st.cycles + SLICE);
                x64.run(block.0, ctx);
            }
            Backend::None => {
                ctx.interpret_one();
                self.stats.interpreted += 1;
            }
        }
    }
}

impl ExternalCpu for Jit {
    fn run_frame(&mut self, cpu: &mut M68k, console: &mut External<'_>) {
        self.run(cpu, console, |_| false);
    }

    fn clone_box(&self) -> Box<dyn ExternalCpu> {
        // The cache is only a cache: a copy starts empty.
        Box::new(Self::new())
    }

    fn describe(&self) -> String {
        format!("JIT ({})", self.backend_name())
    }
}
