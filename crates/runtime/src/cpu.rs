//! The recompiled 68000: registers, memory access and control flow.
//!
//! Every recompiled function has the same shape:
//!
//! ```ignore
//! pub fn f_000200(cx: &mut Cx<'_>) {
//!     let mut pc = cx.pc;                 // which block to start with
//!     loop {
//!         match pc {
//!             0x000200 => { /* straight-line code */ pc = 0x00022C; }
//!             0x00022C => { /* ... */ if !cx.poll(0x22C) { return; } pc = 0x22C; continue; }
//!             _ => { cx.pc = pc; return; }  // not ours: back to the dispatcher
//!         }
//!     }
//! }
//! ```
//!
//! Rust has no `goto`, so a function is a small state machine whose state
//! is the 68000 address of the next *block* (a run of instructions with no
//! label inside). LLVM turns `pc = CONSTANT; continue` back into a direct
//! jump. The bonus: a function can be entered at **any** block, so the
//! native call stack is only a cache of the 68000's own stack. That lets
//! the runtime unwind everything at the end of a frame and pick up again at
//! the next one (see [`Cx::run_frame`]), which is how frontends drive the
//! game one frame at a time.
//!
//! A `Cx` lives for one frame. It borrows the console (a [`Machine`]) and
//! gase's 68000 interpreter, whose state is the CPU's state between frames:
//! [`Cx::new`] loads the registers from it, [`Cx::finish`] stores them back.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write as _;

use gase_m68k::{Bus, M68k};

use crate::machine::Machine;

/// What the generated crate provides to the runtime.
#[derive(Clone, Copy)]
pub struct Code {
    /// Run the recompiled code at `cx.pc`; `false` if there is none.
    pub dispatch: for<'a> fn(&mut Cx<'a>) -> bool,
    /// Is `addr` the start of a recompiled block?
    pub is_block: fn(u32) -> bool,
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code")
    }
}

/// The machine as a [`Bus`] for gase's interpreter (which wants a sized type).
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

/// The CPU context: the 68000's state plus the machine around it.
pub struct Cx<'a> {
    /// Data registers D0-D7.
    pub d: [u32; 8],
    /// Address registers A0-A7; `a[7]` is the active stack pointer.
    pub a: [u32; 8],
    /// The stack pointer A7 does not name (USP in supervisor mode).
    pub inactive_sp: u32,
    /// Between functions: the next address to execute.
    pub pc: u32,
    // The status register, "exploded": one field per flag is much cheaper
    // to update after every instruction than a packed word.
    pub x: bool,
    pub n: bool,
    pub z: bool,
    pub v: bool,
    pub c: bool,
    pub s: bool,
    pub t: bool,
    pub imask: u8,
    pub stopped: bool,
    /// 68000 cycles since power-on (an estimate, see the decoder).
    pub cycles: u64,
    /// When the machine next needs attention.
    pub next_event: u64,
    /// Set at the end of a frame: every function returns, up to run_frame.
    pub unwinding: bool,

    /// The console around the CPU (memory map, scheduler, interrupts).
    pub m: &'a mut dyn Machine,
    /// gase's interpreter: the CPU state between frames, and the fallback
    /// for code that was not recompiled.
    cpu: &'a mut M68k,
    code: Code,
    /// Addresses the interpreter had to run (the coverage feedback loop).
    pub missing: BTreeSet<u32>,
    /// Where to append newly found addresses (None = don't write a file).
    pub missing_log: Option<String>,
}

impl fmt::Debug for Cx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Cx {{ pc: {:06X}, d: {:08X?}, a: {:08X?}, sr: {:04X} }}",
            self.pc,
            self.d,
            self.a,
            self.sr()
        )
    }
}

impl<'a> Cx<'a> {
    /// Start a frame: load the CPU state from `cpu`.
    pub fn new(cpu: &'a mut M68k, m: &'a mut dyn Machine, code: Code) -> Self {
        let cycles = m.cycles();
        let next_event = m.catch_up(cycles);
        let mut cx = Self {
            d: cpu.d,
            a: cpu.a,
            inactive_sp: 0,
            pc: cpu.pc() & 0xFF_FFFF,
            x: false,
            n: false,
            z: false,
            v: false,
            c: false,
            s: true,
            t: false,
            imask: 7,
            stopped: cpu.is_stopped(),
            cycles,
            next_event,
            unwinding: false,
            m,
            cpu,
            code,
            missing: BTreeSet::new(),
            missing_log: None,
        };
        cx.load_registers();
        cx
    }

    /// End the frame: store the CPU state back into the interpreter.
    pub fn finish(mut self) -> BTreeSet<u32> {
        self.store_registers();
        let pc = self.pc;
        self.cpu.set_pc(&mut DynBus(&mut *self.m), pc);
        self.missing
    }

    // ---- status register --------------------------------------------------
    //
    //   T . S . . I2 I1 I0 . . . X N Z V C
    //  15   13      10  9  8      4 3 2 1 0

    #[must_use]
    pub fn ccr(&self) -> u8 {
        u8::from(self.x) << 4
            | u8::from(self.n) << 3
            | u8::from(self.z) << 2
            | u8::from(self.v) << 1
            | u8::from(self.c)
    }

    pub fn set_ccr(&mut self, ccr: u32) {
        self.x = ccr & 0x10 != 0;
        self.n = ccr & 0x08 != 0;
        self.z = ccr & 0x04 != 0;
        self.v = ccr & 0x02 != 0;
        self.c = ccr & 0x01 != 0;
    }

    #[must_use]
    pub fn sr(&self) -> u16 {
        u16::from(self.t) << 15
            | u16::from(self.s) << 13
            | u16::from(self.imask) << 8
            | u16::from(self.ccr())
    }

    pub fn set_sr(&mut self, sr: u32) {
        let s = sr & 0x2000 != 0;
        if s != self.s {
            // Two stack pointers exist; A7 is whichever one the mode selects.
            std::mem::swap(&mut self.a[7], &mut self.inactive_sp);
            self.s = s;
        }
        self.t = sr & 0x8000 != 0;
        self.imask = ((sr >> 8) & 7) as u8;
        self.set_ccr(sr);
    }

    #[must_use]
    pub fn usp(&self) -> u32 {
        if self.s { self.inactive_sp } else { self.a[7] }
    }

    pub fn set_usp(&mut self, v: u32) {
        if self.s {
            self.inactive_sp = v
        } else {
            self.a[7] = v
        }
    }

    // ---- memory -------------------------------------------------------------
    //
    // The 68000 has a 24-bit address bus. Word accesses at odd addresses
    // would raise an address error on a real CPU; like gase with address
    // errors disabled, we simply ignore address bit 0.

    #[inline(always)]
    pub fn r8(&mut self, a: u32) -> u32 {
        self.m.set_clock(self.cycles);
        u32::from(self.m.read_byte(a & 0xFF_FFFF))
    }
    #[inline(always)]
    pub fn r16(&mut self, a: u32) -> u32 {
        self.m.set_clock(self.cycles);
        u32::from(self.m.read_word(a & 0xFF_FFFE))
    }
    /// A long access is two word accesses, high word first.
    #[inline(always)]
    pub fn r32(&mut self, a: u32) -> u32 {
        let hi = self.r16(a);
        hi << 16 | self.r16(a.wrapping_add(2))
    }
    #[inline(always)]
    pub fn w8(&mut self, a: u32, v: u32) {
        self.m.set_clock(self.cycles);
        self.m.write_byte(a & 0xFF_FFFF, v as u8);
    }
    #[inline(always)]
    pub fn w16(&mut self, a: u32, v: u32) {
        self.m.set_clock(self.cycles);
        self.m.write_word(a & 0xFF_FFFE, v as u16);
    }
    #[inline(always)]
    pub fn w32(&mut self, a: u32, v: u32) {
        self.w16(a, v >> 16);
        self.w16(a.wrapping_add(2), v);
    }
    /// The write half of TAS. On the Mega Drive it is lost for work RAM:
    /// the bus arbiter does not support read-modify-write cycles.
    pub fn w8_tas(&mut self, a: u32, v: u32) {
        self.m.set_clock(self.cycles);
        self.m.tas_write_byte(a & 0xFF_FFFF, v as u8);
    }
    pub fn reset_devices(&mut self) {
        self.m.reset_devices();
    }

    pub fn push16(&mut self, v: u32) {
        self.a[7] = self.a[7].wrapping_sub(2);
        self.w16(self.a[7], v);
    }
    pub fn push32(&mut self, v: u32) {
        self.a[7] = self.a[7].wrapping_sub(4);
        self.w32(self.a[7], v);
    }
    pub fn pop16(&mut self) -> u32 {
        let v = self.r16(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        v
    }
    pub fn pop32(&mut self) -> u32 {
        let v = self.r32(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        v
    }

    // ---- registers -----------------------------------------------------------

    /// Write the low byte of Dn, leaving the upper bits alone.
    #[inline(always)]
    pub fn set_d8(&mut self, n: usize, v: u32) {
        self.d[n] = (self.d[n] & 0xFFFF_FF00) | (v & 0xFF);
    }
    #[inline(always)]
    pub fn set_d16(&mut self, n: usize, v: u32) {
        self.d[n] = (self.d[n] & 0xFFFF_0000) | (v & 0xFFFF);
    }

    // ---- control flow -----------------------------------------------------------
    //
    // A call pushes the return address on the *emulated* stack, sets `pc`
    // to the callee and calls it. RTS pops the return address into `pc` and
    // returns. Back in the caller, `returned` checks that we came back where
    // expected: games sometimes play with the stack, and then the
    // dispatcher keeps running code until the expected return shows up.

    /// Call a recompiled function. Returns false if the caller must return
    /// too (frame end, or the callee never came back).
    #[inline(always)]
    pub fn call(&mut self, f: fn(&mut Self), target: u32, ret: u32) -> bool {
        let sp = self.a[7];
        self.push32(ret);
        self.pc = target;
        f(self);
        self.returned(ret, sp)
    }

    /// Call through a register (`jsr (a0)`): only known at run time.
    pub fn call_indirect(&mut self, target: u32, ret: u32) -> bool {
        let sp = self.a[7];
        self.push32(ret);
        self.pc = target & 0xFF_FFFF;
        self.dispatch();
        self.returned(ret, sp)
    }

    #[inline(always)]
    fn returned(&mut self, ret: u32, sp: u32) -> bool {
        if self.pc == ret && !self.unwinding {
            return true;
        }
        self.resume(ret, sp)
    }

    /// Keep running code until it returns to `ret` with the stack back at
    /// `sp`. False when the stack unwinds *past* that frame (the game
    /// abandoned the call, e.g. a soft reset) or at the end of a frame.
    fn resume(&mut self, ret: u32, sp: u32) -> bool {
        loop {
            if self.unwinding {
                return false;
            }
            if self.pc == ret && self.a[7] == sp {
                return true;
            }
            if self.a[7] > sp {
                return false;
            }
            self.dispatch();
        }
    }

    /// Run the code at `pc`: recompiled if we have it, interpreted if not.
    pub fn dispatch(&mut self) {
        self.pc &= 0xFF_FFFF;
        if !(self.code.dispatch)(self) {
            self.interpret();
        }
    }

    /// Poll point, placed on backward branches (loops): the only place
    /// where time visibly passes for the rest of the console. Returns
    /// false if the function must return (see [`Cx::call`]).
    #[inline(always)]
    pub fn poll(&mut self, resume: u32) -> bool {
        self.cycles < self.next_event || self.events(resume)
    }

    #[cold]
    fn events(&mut self, resume: u32) -> bool {
        self.cycles = self.m.stalled_clock(self.cycles);
        self.next_event = self.m.catch_up(self.cycles);
        if self.m.frame_done() {
            self.pc = resume;
            self.unwinding = true;
            return false;
        }
        self.check_irq(resume)
    }

    /// Take the highest pending interrupt if the mask allows it (level 7
    /// is not maskable). The Mega Drive uses 6 = VBlank, 4 = HBlank, 2 = pad.
    pub fn check_irq(&mut self, resume: u32) -> bool {
        loop {
            let level = self.m.irq_level();
            if level == 0 || (level <= self.imask && level != 7) {
                return true;
            }
            self.m.interrupt_acknowledge(level);
            self.stopped = false;
            if !self.enter_exception(24 + u32::from(level), resume, Some(level)) {
                return false;
            }
        }
    }

    /// TRAP, CHK, TRAPV, division by zero...: run the handler, come back.
    pub fn exception(&mut self, vector: u32, resume: u32) -> bool {
        self.enter_exception(vector, resume, None)
    }

    fn enter_exception(&mut self, vector: u32, resume: u32, level: Option<u8>) -> bool {
        let sr = u32::from(self.sr());
        let sp = self.a[7]; // where RTE will bring A7 back
        self.set_sr((sr | 0x2000) & 0x7FFF); // supervisor, no trace
        if let Some(l) = level {
            self.imask = l;
        }
        self.push32(resume);
        self.push16(sr);
        self.cycles += 34;
        self.pc = self.r32(vector * 4) & 0xFF_FFFF;
        self.dispatch();
        self.returned(resume, sp)
    }

    /// STOP: load SR, then sleep until an interrupt arrives. `here` is the
    /// STOP's own address: if the frame ends meanwhile, we resume by
    /// executing it again.
    pub fn stop(&mut self, sr: u32, here: u32) -> bool {
        self.set_sr(sr);
        self.stopped = true;
        while self.stopped {
            self.cycles = self.cycles.max(self.next_event);
            if !self.events(here) {
                return false;
            }
        }
        true
    }

    /// Run until the end of the current frame.
    pub fn run_frame(&mut self) {
        self.unwinding = false;
        while !self.unwinding {
            self.dispatch();
        }
        self.unwinding = false;
    }

    // ---- the interpreter fallback -------------------------------------------
    //
    // Code the analysis did not find (jump tables it could not decode, code
    // copied to RAM...) runs on gase's own 68000 interpreter, until it
    // reaches a recompiled block again. The address is logged so that the
    // next recompilation includes it.

    #[cold]
    fn interpret(&mut self) {
        let start = self.pc;
        if self.missing.insert(start) && start < 0xE0_0000 {
            if let Some(path) = &self.missing_log {
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "0x{start:06X}");
                }
            }
        }
        self.store_registers();
        self.m.set_clock(self.cycles);
        self.cpu.set_pc(&mut DynBus(&mut *self.m), start);
        loop {
            self.cpu.set_interrupt_level(self.m.irq_level());
            self.m.set_clock(self.cycles);
            self.cycles += u64::from(self.cpu.step(&mut DynBus(&mut *self.m)));
            if self.cycles >= self.next_event {
                self.cycles = self.m.stalled_clock(self.cycles);
                self.next_event = self.m.catch_up(self.cycles);
                if self.m.frame_done() {
                    self.unwinding = true;
                    break;
                }
            }
            assert!(
                !self.cpu.is_halted(),
                "68000 halted (double fault) at ${:06X}",
                self.cpu.pc()
            );
            if (self.code.is_block)(self.cpu.pc() & 0xFF_FFFF) {
                break;
            }
        }
        self.load_registers();
    }

    /// Copy our registers into the interpreter.
    fn store_registers(&mut self) {
        self.cpu.set_sr(self.sr());
        self.cpu.d = self.d;
        self.cpu.a = self.a;
        if self.s {
            self.cpu.set_usp(self.inactive_sp);
        } else {
            self.cpu.set_ssp(self.inactive_sp);
        }
    }

    /// Copy the interpreter's registers into ours.
    fn load_registers(&mut self) {
        let sr = self.cpu.sr();
        self.s = sr & 0x2000 != 0;
        self.set_sr(u32::from(sr));
        self.d = self.cpu.d;
        self.a = self.cpu.a;
        self.inactive_sp = if self.s {
            self.cpu.usp()
        } else {
            self.cpu.ssp()
        };
        self.stopped = self.cpu.is_stopped();
        self.pc = self.cpu.pc() & 0xFF_FFFF;
    }
}
