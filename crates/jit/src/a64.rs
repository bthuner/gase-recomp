//! The AArch64 backend (phones, Apple silicon, Raspberry Pi): [`Op`]s →
//! machine code, with dynasm. Same design as `x64.rs`; read that first.
//!
//! A block is `extern "C" fn(*mut Ctx)`: the context pointer stays in
//! `x19`, a value that must survive a call in `x20` (both callee-saved),
//! and the 68000's registers live in memory at `[x19, #offset]`.
//!
//! Two differences from x86 shape the code:
//!
//! * **No 8- and 16-bit arithmetic.** AArch64 only computes flags on 32
//!   or 64 bits. For a byte operation, both operands are shifted left by
//!   24 so that the byte sits at the top of the register: the carry,
//!   overflow, sign and zero of the 32-bit operation are then exactly
//!   those of the byte operation. (16 for a word.)
//! * **The carry of a subtraction is inverted.** ARM sets C when there is
//!   *no* borrow; the 68000 (and x86) when there is. After `subs`/`cmp`,
//!   the 68000's C is ARM's "carry clear" (`cc`).
//!
//! As in `x64.rs`, this is the only place with `unsafe`.
#![allow(unsafe_code)]

use std::mem::offset_of;

use dynasmrt::aarch64::Assembler;
use dynasmrt::{AssemblyOffset, DynamicLabel, DynasmApi, DynasmLabelApi, dynasm};

use crate::ir::{Alu, Base, Ea, Op, Size};
use crate::{Ctx, State};

type BlockFn = extern "C" fn(*mut Ctx<'_>);

pub struct A64 {
    ops: Assembler,
}

impl std::fmt::Debug for A64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A64 {{ {} bytes of code }}", self.ops.offset().0)
    }
}

const fn d(r: u8) -> u32 {
    (offset_of!(State, d) + 4 * r as usize) as u32
}
const fn a(r: u8) -> u32 {
    (offset_of!(State, a) + 4 * r as usize) as u32
}
const PC: u32 = offset_of!(State, pc) as u32;
const FLAG_X: u32 = offset_of!(State, x) as u32;
const FLAG_N: u32 = offset_of!(State, n) as u32;
const FLAG_Z: u32 = offset_of!(State, z) as u32;
const FLAG_V: u32 = offset_of!(State, v) as u32;
const FLAG_C: u32 = offset_of!(State, c) as u32;
const CYCLES: u32 = offset_of!(State, cycles) as u32;
const LIMIT: u32 = offset_of!(State, limit) as u32;

extern "C" fn interpret(ctx: *mut Ctx<'_>, next: u32) -> u32 {
    // SAFETY: blocks are only called by `A64::run`, with a pointer to a
    // live, exclusively borrowed `Ctx`, and pass it on unchanged.
    let ctx = unsafe { &mut *ctx };
    ctx.interpret_one();
    u32::from(ctx.st.pc == next)
}

macro_rules! memory_helpers {
    ($($read:ident $write:ident),*) => {$(
        extern "C" fn $read(ctx: *mut Ctx<'_>, addr: u32) -> u32 {
            // SAFETY: as for `interpret`.
            unsafe { &mut *ctx }.$read(addr)
        }
        extern "C" fn $write(ctx: *mut Ctx<'_>, addr: u32, v: u32) {
            // SAFETY: as for `interpret`.
            unsafe { &mut *ctx }.$write(addr, v);
        }
    )*};
}
memory_helpers!(read8 write8, read16 write16, read32 write32);

fn read_helper(size: Size) -> u64 {
    match size {
        Size::Byte => read8 as *const () as u64,
        Size::Word => read16 as *const () as u64,
        Size::Long => read32 as *const () as u64,
    }
}

fn write_helper(size: Size) -> u64 {
    match size {
        Size::Byte => write8 as *const () as u64,
        Size::Word => write16 as *const () as u64,
        Size::Long => write32 as *const () as u64,
    }
}

/// How far left a value of `size` is shifted to compute its flags.
fn shift(size: Size) -> u32 {
    match size {
        Size::Byte => 24,
        Size::Word => 16,
        Size::Long => 0,
    }
}

/// Where a block can jump: its region's blocks, or out.
struct Jumps<'a> {
    leave: DynamicLabel,
    labels: &'a [(u32, DynamicLabel)],
}

impl A64 {
    pub fn new() -> Self {
        Self {
            ops: Assembler::new().expect("executable memory"),
        }
    }

    pub fn run(&mut self, at: usize, ctx: &mut Ctx<'_>) {
        let reader = self.ops.reader();
        let code = reader.lock();
        // SAFETY: `at` is the start of a region written by `compile` and
        // committed (dynasmrt flushes the instruction cache); regions
        // follow the C calling convention with this signature and only
        // touch the `Ctx` they are given.
        let block: BlockFn = unsafe { std::mem::transmute(code.ptr(AssemblyOffset(at))) };
        block(ctx);
    }

    pub fn compile(&mut self, region: &[(u32, Vec<Op>)]) -> usize {
        let ops = &mut self.ops;
        let start = ops.offset();
        let leave = ops.new_dynamic_label();
        let labels: Vec<(u32, DynamicLabel)> = region
            .iter()
            .map(|(pc, _)| (*pc, ops.new_dynamic_label()))
            .collect();
        let jumps = Jumps {
            leave,
            labels: &labels,
        };
        // Save the frame pointer, the return address and x19/x20.
        dynasm!(ops
            ; .arch aarch64
            ; stp x29, x30, [sp, #-32]!
            ; stp x19, x20, [sp, #16]
            ; mov x19, x0
        );
        for ((_, block), (_, label)) in region.iter().zip(&labels) {
            dynasm!(ops ; .arch aarch64 ; =>*label);
            for op in block {
                emit(ops, *op, &jumps);
            }
        }
        dynasm!(ops
            ; .arch aarch64
            ; =>leave
            ; ldp x19, x20, [sp, #16]
            ; ldp x29, x30, [sp], #32
            ; ret
        );
        ops.commit().expect("executable memory");
        start.0
    }
}

/// w`r` = `v` (any 32-bit constant).
fn mov32(ops: &mut Assembler, r: u32, v: u32) {
    dynasm!(ops ; .arch aarch64 ; movz W(r), #v & 0xFFFF);
    if v >> 16 != 0 {
        dynasm!(ops ; .arch aarch64 ; movk W(r), #v >> 16, lsl #16);
    }
}

/// Call a Rust helper (x0 = the context; w1, w2 as set up).
fn call(ops: &mut Assembler, helper: u64) {
    dynasm!(ops
        ; .arch aarch64
        ; mov x0, x19
        ; movz x16, #(helper & 0xFFFF) as u32
        ; movk x16, #((helper >> 16) & 0xFFFF) as u32, lsl #16
        ; movk x16, #((helper >> 32) & 0xFFFF) as u32, lsl #32
        ; movk x16, #((helper >> 48) & 0xFFFF) as u32, lsl #48
        ; blr x16
    );
}

fn step(size: Size, r: u8) -> u32 {
    match size {
        Size::Byte if r == 7 => 2,
        Size::Byte => 1,
        Size::Word => 2,
        Size::Long => 4,
    }
}

/// The address of a memory operand into w1 (An moved for `(An)+`/`-(An)`).
fn address(ops: &mut Assembler, ea: Ea, size: Size) {
    match ea {
        Ea::Ind(r) => dynasm!(ops ; .arch aarch64 ; ldr w1, [x19, a(r)]),
        Ea::PostInc(r) => dynasm!(ops
            ; .arch aarch64
            ; ldr w1, [x19, a(r)]
            ; add w2, w1, step(size, r)
            ; str w2, [x19, a(r)]
        ),
        Ea::PreDec(r) => dynasm!(ops
            ; .arch aarch64
            ; ldr w1, [x19, a(r)]
            ; sub w1, w1, step(size, r)
            ; str w1, [x19, a(r)]
        ),
        Ea::Disp(r, disp) => {
            dynasm!(ops ; .arch aarch64 ; ldr w1, [x19, a(r)]);
            add_const(ops, disp);
        }
        Ea::Index {
            base,
            disp,
            x,
            xa,
            xlong,
        } => {
            match base {
                Base::A(r) => dynasm!(ops ; .arch aarch64 ; ldr w1, [x19, a(r)]),
                Base::Pc(addr) => mov32(ops, 1, addr),
            }
            let xoff = if xa { a(x) } else { d(x) };
            dynasm!(ops ; .arch aarch64 ; ldr w2, [x19, xoff]);
            if !xlong {
                dynasm!(ops ; .arch aarch64 ; sxth w2, w2);
            }
            dynasm!(ops ; .arch aarch64 ; add w1, w1, w2);
            add_const(ops, disp);
        }
        Ea::Abs(addr) => mov32(ops, 1, addr),
        Ea::D(_) | Ea::A(_) | Ea::Imm(_) => unreachable!("not in memory"),
    }
}

/// w1 += `disp` (through w3).
fn add_const(ops: &mut Assembler, disp: i32) {
    if disp != 0 {
        mov32(ops, 3, disp as u32);
        dynasm!(ops ; .arch aarch64 ; add w1, w1, w3);
    }
}

/// The value of an operand into w0.
fn load(ops: &mut Assembler, src: Ea, size: Size) {
    match src {
        Ea::D(r) => dynasm!(ops ; .arch aarch64 ; ldr w0, [x19, d(r)]),
        Ea::A(r) => dynasm!(ops ; .arch aarch64 ; ldr w0, [x19, a(r)]),
        Ea::Imm(v) => mov32(ops, 0, v),
        _ => {
            address(ops, src, size);
            call(ops, read_helper(size));
        }
    }
}

/// Store the low `size` of w`r` into Dn.
fn store_d(ops: &mut Assembler, size: Size, r: u32, dn: u8) {
    match size {
        Size::Byte => dynasm!(ops ; .arch aarch64 ; strb W(r), [x19, d(dn)]),
        Size::Word => dynasm!(ops ; .arch aarch64 ; strh W(r), [x19, d(dn)]),
        Size::Long => dynasm!(ops ; .arch aarch64 ; str W(r), [x19, d(dn)]),
    }
}

/// Store w20 into MOVE's destination.
fn store_w20(ops: &mut Assembler, dst: Ea, size: Size) {
    match dst {
        Ea::D(r) => store_d(ops, size, 20, r),
        Ea::A(r) => match size {
            Size::Word => dynasm!(ops
                ; .arch aarch64
                ; sxth w0, w20
                ; str w0, [x19, a(r)]
            ),
            _ => dynasm!(ops ; .arch aarch64 ; str w20, [x19, a(r)]),
        },
        _ => {
            address(ops, dst, size);
            dynasm!(ops ; .arch aarch64 ; mov w2, w20);
            call(ops, write_helper(size));
        }
    }
}

/// N and Z of w`r` at `size` (through w0); V and C cleared.
fn logic_flags(ops: &mut Assembler, size: Size, r: u32) {
    let sh = shift(size);
    dynasm!(ops
        ; .arch aarch64
        ; lsl w0, W(r), sh
        ; tst w0, w0
        ; cset w0, mi
        ; strb w0, [x19, FLAG_N]
        ; cset w0, eq
        ; strb w0, [x19, FLAG_Z]
        ; strb wzr, [x19, FLAG_V]
        ; strb wzr, [x19, FLAG_C]
    );
}

/// Dn <op>= w0 at `size`, with 68000 flags.
fn alu(ops: &mut Assembler, op: Alu, size: Size, dn: u8) {
    let sh = shift(size);
    // w4 = Dn, w5 = source, both shifted so the operand is at the top.
    dynasm!(ops
        ; .arch aarch64
        ; ldr w4, [x19, d(dn)]
        ; lsl w4, w4, sh
        ; lsl w5, w0, sh
    );
    match op {
        Alu::Add => dynasm!(ops ; .arch aarch64 ; adds w2, w4, w5),
        Alu::Sub => dynasm!(ops ; .arch aarch64 ; subs w2, w4, w5),
        Alu::Cmp => dynasm!(ops ; .arch aarch64 ; cmp w4, w5),
        Alu::And => dynasm!(ops ; .arch aarch64 ; ands w2, w4, w5),
        Alu::Or => dynasm!(ops ; .arch aarch64 ; orr w2, w4, w5 ; tst w2, w2),
        Alu::Eor => dynasm!(ops ; .arch aarch64 ; eor w2, w4, w5 ; tst w2, w2),
    }
    match op {
        Alu::Add => dynasm!(ops
            ; .arch aarch64
            ; cset w0, cs
            ; strb w0, [x19, FLAG_C]
            ; strb w0, [x19, FLAG_X]
            ; cset w0, vs
            ; strb w0, [x19, FLAG_V]
            ; cset w0, mi
            ; strb w0, [x19, FLAG_N]
            ; cset w0, eq
            ; strb w0, [x19, FLAG_Z]
        ),
        // The 68000's borrow is ARM's carry *clear*.
        Alu::Sub => dynasm!(ops
            ; .arch aarch64
            ; cset w0, cc
            ; strb w0, [x19, FLAG_C]
            ; strb w0, [x19, FLAG_X]
            ; cset w0, vs
            ; strb w0, [x19, FLAG_V]
            ; cset w0, mi
            ; strb w0, [x19, FLAG_N]
            ; cset w0, eq
            ; strb w0, [x19, FLAG_Z]
        ),
        Alu::Cmp => dynasm!(ops
            ; .arch aarch64
            ; cset w0, cc
            ; strb w0, [x19, FLAG_C]
            ; cset w0, vs
            ; strb w0, [x19, FLAG_V]
            ; cset w0, mi
            ; strb w0, [x19, FLAG_N]
            ; cset w0, eq
            ; strb w0, [x19, FLAG_Z]
        ),
        Alu::And | Alu::Or | Alu::Eor => dynasm!(ops
            ; .arch aarch64
            ; cset w0, mi
            ; strb w0, [x19, FLAG_N]
            ; cset w0, eq
            ; strb w0, [x19, FLAG_Z]
            ; strb wzr, [x19, FLAG_V]
            ; strb wzr, [x19, FLAG_C]
        ),
    }
    if op != Alu::Cmp {
        if sh != 0 {
            dynasm!(ops ; .arch aarch64 ; lsr w2, w2, sh);
        }
        store_d(ops, size, 2, dn);
    }
}

/// w0 = 1 if 68000 condition `cc` holds (the flag bytes are 0 or 1).
fn condition(ops: &mut Assembler, cc: u8) {
    macro_rules! flag {
        ($f:expr) => {
            dynasm!(ops ; .arch aarch64 ; ldrb w0, [x19, $f])
        };
    }
    macro_rules! or {
        ($f:expr) => {
            dynasm!(ops ; .arch aarch64 ; ldrb w1, [x19, $f] ; orr w0, w0, w1)
        };
    }
    macro_rules! xor {
        ($f:expr) => {
            dynasm!(ops ; .arch aarch64 ; ldrb w1, [x19, $f] ; eor w0, w0, w1)
        };
    }
    macro_rules! not {
        () => {
            dynasm!(ops ; .arch aarch64 ; eor w0, w0, 1)
        };
    }
    match cc {
        0 => mov32(ops, 0, 1),
        1 => mov32(ops, 0, 0),
        2 => {
            flag!(FLAG_C);
            or!(FLAG_Z);
            not!();
        } // hi
        3 => {
            flag!(FLAG_C);
            or!(FLAG_Z);
        } // ls
        4 => {
            flag!(FLAG_C);
            not!();
        } // cc
        5 => flag!(FLAG_C), // cs
        6 => {
            flag!(FLAG_Z);
            not!();
        } // ne
        7 => flag!(FLAG_Z), // eq
        8 => {
            flag!(FLAG_V);
            not!();
        } // vc
        9 => flag!(FLAG_V), // vs
        10 => {
            flag!(FLAG_N);
            not!();
        } // pl
        11 => flag!(FLAG_N), // mi
        12 => {
            flag!(FLAG_N);
            xor!(FLAG_V);
            not!();
        } // ge
        13 => {
            flag!(FLAG_N);
            xor!(FLAG_V);
        } // lt
        14 => {
            flag!(FLAG_N);
            xor!(FLAG_V);
            or!(FLAG_Z);
            not!();
        } // gt
        _ => {
            flag!(FLAG_N);
            xor!(FLAG_V);
            or!(FLAG_Z);
        } // le
    }
}

fn cycles(ops: &mut Assembler, n: u32) {
    dynasm!(ops ; .arch aarch64 ; ldr x0, [x19, CYCLES]);
    if n < 4096 {
        dynasm!(ops ; .arch aarch64 ; add x0, x0, n);
    } else {
        mov32(ops, 1, n);
        dynasm!(ops ; .arch aarch64 ; add x0, x0, x1);
    }
    dynasm!(ops ; .arch aarch64 ; str x0, [x19, CYCLES]);
}

fn goto(ops: &mut Assembler, pc: u32, jumps: &Jumps<'_>) {
    mov32(ops, 0, pc);
    dynasm!(ops ; .arch aarch64 ; str w0, [x19, PC]);
    match jumps.labels.iter().find(|(at, _)| *at == pc) {
        Some((_, label)) => dynasm!(ops
            ; .arch aarch64
            ; ldr x0, [x19, CYCLES]
            ; ldr x1, [x19, LIMIT]
            ; cmp x0, x1
            ; b.lo =>*label
            ; b =>jumps.leave
        ),
        None => dynasm!(ops ; .arch aarch64 ; b =>jumps.leave),
    }
}

fn emit(ops: &mut Assembler, op: Op, jumps: &Jumps<'_>) {
    let leave = jumps.leave;
    match op {
        Op::Cycles(n) => cycles(ops, n),
        Op::Moveq { dst, value } => {
            mov32(ops, 0, value);
            dynasm!(ops ; .arch aarch64 ; str w0, [x19, d(dst)]);
            mov32(ops, 0, value >> 31);
            dynasm!(ops ; .arch aarch64 ; strb w0, [x19, FLAG_N]);
            mov32(ops, 0, u32::from(value == 0));
            dynasm!(ops
                ; .arch aarch64
                ; strb w0, [x19, FLAG_Z]
                ; strb wzr, [x19, FLAG_V]
                ; strb wzr, [x19, FLAG_C]
            );
        }
        Op::Move { size, src, dst } => {
            load(ops, src, size);
            dynasm!(ops ; .arch aarch64 ; mov w20, w0);
            store_w20(ops, dst, size);
            if !matches!(dst, Ea::A(_)) {
                logic_flags(ops, size, 20);
            }
        }
        Op::Lea { src, dst } => {
            address(ops, src, Size::Long);
            dynasm!(ops ; .arch aarch64 ; str w1, [x19, a(dst)]);
        }
        Op::Alu { op, size, src, dst } => {
            load(ops, src, size);
            alu(ops, op, size, dst);
        }
        Op::AluA { op, size, src, dst } => {
            load(ops, src, size);
            if size == Size::Word {
                dynasm!(ops ; .arch aarch64 ; sxth w0, w0);
            }
            dynasm!(ops ; .arch aarch64 ; ldr w1, [x19, a(dst)]);
            match op {
                Alu::Add => dynasm!(ops ; .arch aarch64 ; add w1, w1, w0 ; str w1, [x19, a(dst)]),
                Alu::Sub => dynasm!(ops ; .arch aarch64 ; sub w1, w1, w0 ; str w1, [x19, a(dst)]),
                _ => dynasm!(ops
                    ; .arch aarch64
                    ; cmp w1, w0
                    ; cset w0, cc
                    ; strb w0, [x19, FLAG_C]
                    ; cset w0, vs
                    ; strb w0, [x19, FLAG_V]
                    ; cset w0, mi
                    ; strb w0, [x19, FLAG_N]
                    ; cset w0, eq
                    ; strb w0, [x19, FLAG_Z]
                ),
            }
        }
        Op::Tst { size, src } => {
            load(ops, src, size);
            logic_flags(ops, size, 0);
        }
        Op::Clr { size, reg } => {
            match size {
                Size::Byte => dynasm!(ops ; .arch aarch64 ; strb wzr, [x19, d(reg)]),
                Size::Word => dynasm!(ops ; .arch aarch64 ; strh wzr, [x19, d(reg)]),
                Size::Long => dynasm!(ops ; .arch aarch64 ; str wzr, [x19, d(reg)]),
            }
            mov32(ops, 0, 1);
            dynasm!(ops
                ; .arch aarch64
                ; strb wzr, [x19, FLAG_N]
                ; strb w0, [x19, FLAG_Z]
                ; strb wzr, [x19, FLAG_V]
                ; strb wzr, [x19, FLAG_C]
            );
        }
        Op::Interpret { pc, next } => {
            mov32(ops, 0, pc);
            dynasm!(ops ; .arch aarch64 ; str w0, [x19, PC]);
            mov32(ops, 1, next);
            call(ops, interpret as *const () as u64);
            dynasm!(ops ; .arch aarch64 ; cbz w0, =>leave);
        }
        Op::Branch {
            cond,
            target,
            fall,
            cycles_taken,
            cycles_not,
        } => match cond {
            None => {
                cycles(ops, cycles_taken);
                goto(ops, target, jumps);
            }
            Some(cc) => {
                let not = ops.new_dynamic_label();
                condition(ops, cc);
                dynasm!(ops ; .arch aarch64 ; cbz w0, =>not);
                cycles(ops, cycles_taken);
                goto(ops, target, jumps);
                dynasm!(ops ; .arch aarch64 ; =>not);
                cycles(ops, cycles_not);
                goto(ops, fall, jumps);
            }
        },
        Op::Dbcc {
            cond,
            reg,
            target,
            fall,
            cycles_true,
            cycles_loop,
            cycles_exhausted,
        } => {
            let decrement = ops.new_dynamic_label();
            let exhausted = ops.new_dynamic_label();
            condition(ops, cond);
            dynasm!(ops ; .arch aarch64 ; cbz w0, =>decrement);
            cycles(ops, cycles_true);
            goto(ops, fall, jumps);
            // Dn.w - 1; the loop ends when Dn.w was 0 (now -1).
            dynasm!(ops
                ; .arch aarch64
                ; =>decrement
                ; ldrh w1, [x19, d(reg)]
                ; sub w2, w1, 1
                ; strh w2, [x19, d(reg)]
                ; cbz w1, =>exhausted
            );
            cycles(ops, cycles_loop);
            goto(ops, target, jumps);
            dynasm!(ops ; .arch aarch64 ; =>exhausted);
            cycles(ops, cycles_exhausted);
            goto(ops, fall, jumps);
        }
        Op::Exit { pc } => goto(ops, pc, jumps),
        Op::Leave => dynasm!(ops ; .arch aarch64 ; b =>leave),
    }
}
