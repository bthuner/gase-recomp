//! The x86-64 backend: [`Op`]s → machine code, with dynasm.
//!
//! A block is a function `extern "sysv64" fn(*mut Ctx)`. It keeps the
//! context pointer in `rbx` (callee-saved, so it survives calls into
//! Rust), works on the 68000 registers *in memory* (`[rbx + offset]`), and
//! returns with `State::pc` set to where the 68000 continues.
//!
//! The 68000's flags map well onto x86's: after `add`, `sub` or `cmp` of
//! the same width, x86's carry, overflow, sign and zero flags are the
//! 68000's C, V, N and Z, and `setc`/`seto`/`sets`/`setz` store them as
//! bytes. (X is C again after add and sub.)
//!
//! This is the only module of the crate that uses `unsafe`: calling code
//! we just wrote, and the helper that receives a raw context pointer.
#![allow(unsafe_code)]

use std::mem::offset_of;

use dynasmrt::x64::Assembler;
use dynasmrt::{AssemblyOffset, DynamicLabel, DynasmApi, DynasmLabelApi, dynasm};

use crate::ir::{Alu, Base, Ea, Op, Size};
use crate::{Ctx, State};

/// A block, as the machine sees it.
type BlockFn = extern "sysv64" fn(*mut Ctx<'_>);

pub struct X64 {
    ops: Assembler,
}

impl std::fmt::Debug for X64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X64 {{ {} bytes of code }}", self.ops.offset().0)
    }
}

// Where things are in `State` (and so in `Ctx`, which starts with it).
const fn d(r: u8) -> i32 {
    (offset_of!(State, d) + 4 * r as usize) as i32
}
const fn a(r: u8) -> i32 {
    (offset_of!(State, a) + 4 * r as usize) as i32
}
const PC: i32 = offset_of!(State, pc) as i32;
const X: i32 = offset_of!(State, x) as i32;
const N: i32 = offset_of!(State, n) as i32;
const Z: i32 = offset_of!(State, z) as i32;
const V: i32 = offset_of!(State, v) as i32;
const C: i32 = offset_of!(State, c) as i32;
const CYCLES: i32 = offset_of!(State, cycles) as i32;
const LIMIT: i32 = offset_of!(State, limit) as i32;

/// Called by a block for [`Op::Interpret`]: returns 1 if the 68000
/// continued at `next`.
extern "sysv64" fn interpret(ctx: *mut Ctx<'_>, next: u32) -> u32 {
    // SAFETY: blocks are only called by `X64::run`, with a pointer to a
    // live, exclusively borrowed `Ctx`, and pass it on unchanged.
    let ctx = unsafe { &mut *ctx };
    ctx.interpret_one();
    u32::from(ctx.st.pc == next)
}

// Memory, through the console (see `Ctx::read8` and the rest).
macro_rules! memory_helpers {
    ($($read:ident $write:ident),*) => {$(
        extern "sysv64" fn $read(ctx: *mut Ctx<'_>, addr: u32) -> u32 {
            // SAFETY: as for `interpret`.
            unsafe { &mut *ctx }.$read(addr)
        }
        extern "sysv64" fn $write(ctx: *mut Ctx<'_>, addr: u32, v: u32) {
            // SAFETY: as for `interpret`.
            unsafe { &mut *ctx }.$write(addr, v);
        }
    )*};
}
memory_helpers!(read8 write8, read16 write16, read32 write32);

fn read_helper(size: Size) -> i64 {
    match size {
        Size::Byte => read8 as *const () as i64,
        Size::Word => read16 as *const () as i64,
        Size::Long => read32 as *const () as i64,
    }
}

fn write_helper(size: Size) -> i64 {
    match size {
        Size::Byte => write8 as *const () as i64,
        Size::Word => write16 as *const () as i64,
        Size::Long => write32 as *const () as i64,
    }
}

impl X64 {
    pub fn new() -> Self {
        Self {
            ops: Assembler::new().expect("executable memory"),
        }
    }

    /// Run the block compiled at `at`.
    pub fn run(&mut self, at: usize, ctx: &mut Ctx<'_>) {
        let reader = self.ops.reader();
        let code = reader.lock();
        // SAFETY: `at` is the start of a block written by `compile` and
        // committed; blocks follow the sysv64 convention with this
        // signature, and only touch the `Ctx` they are given.
        let block: BlockFn = unsafe { std::mem::transmute(code.ptr(AssemblyOffset(at))) };
        block(ctx);
    }

    /// Write the machine code for a region (its first block is the
    /// entry); returns where it starts.
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
        // rbx = the context; r12 holds a value across calls. Three pushes'
        // worth (with the return address) keep calls 16-byte aligned.
        dynasm!(ops
            ; .arch x64
            ; push rbx
            ; push r12
            ; sub rsp, 8
            ; mov rbx, rdi
        );
        for ((_, block), (_, label)) in region.iter().zip(&labels) {
            dynasm!(ops ; .arch x64 ; =>*label);
            for op in block {
                emit(ops, *op, &jumps);
            }
        }
        dynasm!(ops
            ; .arch x64
            ; =>leave
            ; add rsp, 8
            ; pop r12
            ; pop rbx
            ; ret
        );
        ops.commit().expect("executable memory");
        start.0
    }
}

/// Where a block can jump: its region's blocks, or out.
struct Jumps<'a> {
    leave: DynamicLabel,
    labels: &'a [(u32, DynamicLabel)],
}

/// N and Z from x86's flags; V and C cleared (moves and logic).
fn logic_flags(ops: &mut Assembler) {
    dynasm!(ops
        ; .arch x64
        ; sets BYTE [rbx + N]
        ; setz BYTE [rbx + Z]
        ; mov BYTE [rbx + V], 0
        ; mov BYTE [rbx + C], 0
    );
}

/// How far `(An)+` and `-(An)` move An: the size, except that the stack
/// pointer stays even for bytes.
fn step(size: Size, r: u8) -> i32 {
    match size {
        Size::Byte if r == 7 => 2,
        Size::Byte => 1,
        Size::Word => 2,
        Size::Long => 4,
    }
}

/// The address of a memory operand into esi (and An moved, for `(An)+`
/// and `-(An)`).
fn address(ops: &mut Assembler, ea: Ea, size: Size) {
    match ea {
        Ea::Ind(r) => dynasm!(ops ; .arch x64 ; mov esi, DWORD [rbx + a(r)]),
        Ea::PostInc(r) => dynasm!(ops
            ; .arch x64
            ; mov esi, DWORD [rbx + a(r)]
            ; add DWORD [rbx + a(r)], step(size, r)
        ),
        Ea::PreDec(r) => dynasm!(ops
            ; .arch x64
            ; sub DWORD [rbx + a(r)], step(size, r)
            ; mov esi, DWORD [rbx + a(r)]
        ),
        Ea::Disp(r, disp) => dynasm!(ops
            ; .arch x64
            ; mov esi, DWORD [rbx + a(r)]
            ; add esi, disp
        ),
        Ea::Index {
            base,
            disp,
            x,
            xa,
            xlong,
        } => {
            match base {
                Base::A(r) => dynasm!(ops ; .arch x64 ; mov esi, DWORD [rbx + a(r)]),
                Base::Pc(addr) => dynasm!(ops ; .arch x64 ; mov esi, addr as i32),
            }
            let xoff = if xa { a(x) } else { d(x) };
            dynasm!(ops ; .arch x64 ; mov ecx, DWORD [rbx + xoff]);
            if !xlong {
                dynasm!(ops ; .arch x64 ; movsx ecx, cx);
            }
            dynasm!(ops ; .arch x64 ; add esi, ecx ; add esi, disp);
        }
        Ea::Abs(addr) => dynasm!(ops ; .arch x64 ; mov esi, addr as i32),
        Ea::D(_) | Ea::A(_) | Ea::Imm(_) => unreachable!("not in memory"),
    }
}

/// Call a helper with the context (and esi, edx as set up).
fn call(ops: &mut Assembler, helper: i64) {
    dynasm!(ops
        ; .arch x64
        ; mov rdi, rbx
        ; mov rax, QWORD helper
        ; call rax
    );
}

/// The value of an operand into eax.
fn load(ops: &mut Assembler, src: Ea, size: Size) {
    match src {
        Ea::D(r) => dynasm!(ops ; .arch x64 ; mov eax, DWORD [rbx + d(r)]),
        Ea::A(r) => dynasm!(ops ; .arch x64 ; mov eax, DWORD [rbx + a(r)]),
        Ea::Imm(v) => dynasm!(ops ; .arch x64 ; mov eax, v as i32),
        _ => {
            address(ops, src, size);
            call(ops, read_helper(size));
        }
    }
}

/// Store r12d into an operand (MOVE's destination).
fn store_r12(ops: &mut Assembler, dst: Ea, size: Size) {
    match dst {
        Ea::D(r) => match size {
            Size::Byte => dynasm!(ops ; .arch x64 ; mov BYTE [rbx + d(r)], r12b),
            Size::Word => dynasm!(ops ; .arch x64 ; mov WORD [rbx + d(r)], r12w),
            Size::Long => dynasm!(ops ; .arch x64 ; mov DWORD [rbx + d(r)], r12d),
        },
        // MOVEA: all 32 bits, a word sign-extended.
        Ea::A(r) => match size {
            Size::Word => dynasm!(ops
                ; .arch x64
                ; movsx eax, r12w
                ; mov DWORD [rbx + a(r)], eax
            ),
            _ => dynasm!(ops ; .arch x64 ; mov DWORD [rbx + a(r)], r12d),
        },
        _ => {
            address(ops, dst, size);
            dynasm!(ops ; .arch x64 ; mov edx, r12d);
            call(ops, write_helper(size));
        }
    }
}

/// Test eax at `size` (sets x86's sign and zero flags).
fn test(ops: &mut Assembler, size: Size) {
    match size {
        Size::Byte => dynasm!(ops ; .arch x64 ; test al, al),
        Size::Word => dynasm!(ops ; .arch x64 ; test ax, ax),
        Size::Long => dynasm!(ops ; .arch x64 ; test eax, eax),
    }
}

/// Store eax at `size` into Dn (the upper bits of Dn stay).
fn store(ops: &mut Assembler, size: Size, r: u8) {
    match size {
        Size::Byte => dynasm!(ops ; .arch x64 ; mov BYTE [rbx + d(r)], al),
        Size::Word => dynasm!(ops ; .arch x64 ; mov WORD [rbx + d(r)], ax),
        Size::Long => dynasm!(ops ; .arch x64 ; mov DWORD [rbx + d(r)], eax),
    }
}

/// `Dn <op>= eax` at `size`.
fn alu(ops: &mut Assembler, op: Alu, size: Size, r: u8) {
    macro_rules! sized {
        ($ins:ident) => {
            match size {
                Size::Byte => dynasm!(ops ; .arch x64 ; $ins BYTE [rbx + d(r)], al),
                Size::Word => dynasm!(ops ; .arch x64 ; $ins WORD [rbx + d(r)], ax),
                Size::Long => dynasm!(ops ; .arch x64 ; $ins DWORD [rbx + d(r)], eax),
            }
        };
    }
    match op {
        Alu::Add => sized!(add),
        Alu::Sub => sized!(sub),
        Alu::And => sized!(and),
        Alu::Or => sized!(or),
        Alu::Eor => sized!(xor),
        Alu::Cmp => sized!(cmp),
    }
    match op {
        Alu::Add | Alu::Sub => dynasm!(ops
            ; .arch x64
            ; setc BYTE [rbx + C]
            ; setc BYTE [rbx + X]
            ; seto BYTE [rbx + V]
            ; sets BYTE [rbx + N]
            ; setz BYTE [rbx + Z]
        ),
        Alu::Cmp => dynasm!(ops
            ; .arch x64
            ; setc BYTE [rbx + C]
            ; seto BYTE [rbx + V]
            ; sets BYTE [rbx + N]
            ; setz BYTE [rbx + Z]
        ),
        Alu::And | Alu::Or | Alu::Eor => logic_flags(ops),
    }
}

/// al = 1 if 68000 condition `cc` holds (the flag bytes are 0 or 1).
fn condition(ops: &mut Assembler, cc: u8) {
    match cc {
        0 => dynasm!(ops ; .arch x64 ; mov al, 1),
        1 => dynasm!(ops ; .arch x64 ; mov al, 0),
        2 => dynasm!(ops ; .arch x64 ; mov al, [rbx + C] ; or al, [rbx + Z] ; xor al, 1), // hi
        3 => dynasm!(ops ; .arch x64 ; mov al, [rbx + C] ; or al, [rbx + Z]),             // ls
        4 => dynasm!(ops ; .arch x64 ; mov al, [rbx + C] ; xor al, 1),                    // cc
        5 => dynasm!(ops ; .arch x64 ; mov al, [rbx + C]),                                // cs
        6 => dynasm!(ops ; .arch x64 ; mov al, [rbx + Z] ; xor al, 1),                    // ne
        7 => dynasm!(ops ; .arch x64 ; mov al, [rbx + Z]),                                // eq
        8 => dynasm!(ops ; .arch x64 ; mov al, [rbx + V] ; xor al, 1),                    // vc
        9 => dynasm!(ops ; .arch x64 ; mov al, [rbx + V]),                                // vs
        10 => dynasm!(ops ; .arch x64 ; mov al, [rbx + N] ; xor al, 1),                   // pl
        11 => dynasm!(ops ; .arch x64 ; mov al, [rbx + N]),                               // mi
        12 => dynasm!(ops ; .arch x64 ; mov al, [rbx + N] ; xor al, [rbx + V] ; xor al, 1), // ge
        13 => dynasm!(ops ; .arch x64 ; mov al, [rbx + N] ; xor al, [rbx + V]),           // lt
        14 => dynasm!(ops
            ; .arch x64 ; mov al, [rbx + N] ; xor al, [rbx + V] ; or al, [rbx + Z] ; xor al, 1), // gt
        _ => dynasm!(ops
            ; .arch x64 ; mov al, [rbx + N] ; xor al, [rbx + V] ; or al, [rbx + Z]), // le
    }
}

fn cycles(ops: &mut Assembler, n: u32) {
    dynasm!(ops ; .arch x64 ; add QWORD [rbx + CYCLES], n as i32);
}

/// Continue at `pc`: a direct jump if it is in the region and there is
/// time left, else back to the engine.
fn goto(ops: &mut Assembler, pc: u32, jumps: &Jumps<'_>) {
    dynasm!(ops ; .arch x64 ; mov DWORD [rbx + PC], pc as i32);
    match jumps.labels.iter().find(|(at, _)| *at == pc) {
        Some((_, label)) => dynasm!(ops
            ; .arch x64
            ; mov rax, QWORD [rbx + CYCLES]
            ; cmp rax, QWORD [rbx + LIMIT]
            ; jb =>*label
            ; jmp =>jumps.leave
        ),
        None => dynasm!(ops ; .arch x64 ; jmp =>jumps.leave),
    }
}

fn emit(ops: &mut Assembler, op: Op, jumps: &Jumps<'_>) {
    let leave = jumps.leave;
    match op {
        Op::Cycles(n) => cycles(ops, n),
        Op::Moveq { dst, value } => {
            // The flags are known now.
            let (n, z) = (i8::from(value >> 31 != 0), i8::from(value == 0));
            dynasm!(ops
                ; .arch x64
                ; mov DWORD [rbx + d(dst)], value as i32
                ; mov BYTE [rbx + N], n
                ; mov BYTE [rbx + Z], z
                ; mov BYTE [rbx + V], 0
                ; mov BYTE [rbx + C], 0
            );
        }
        Op::Move { size, src, dst } => {
            load(ops, src, size);
            dynasm!(ops ; .arch x64 ; mov r12d, eax);
            store_r12(ops, dst, size);
            if !matches!(dst, Ea::A(_)) {
                dynasm!(ops ; .arch x64 ; mov eax, r12d);
                test(ops, size);
                logic_flags(ops);
            }
        }
        Op::Lea { src, dst } => {
            address(ops, src, Size::Long);
            dynasm!(ops ; .arch x64 ; mov DWORD [rbx + a(dst)], esi);
        }
        Op::Alu { op, size, src, dst } => {
            load(ops, src, size);
            alu(ops, op, size, dst);
        }
        Op::AluA { op, size, src, dst } => {
            load(ops, src, size);
            if size == Size::Word {
                dynasm!(ops ; .arch x64 ; movsx eax, ax);
            }
            match op {
                Alu::Add => dynasm!(ops ; .arch x64 ; add DWORD [rbx + a(dst)], eax),
                Alu::Sub => dynasm!(ops ; .arch x64 ; sub DWORD [rbx + a(dst)], eax),
                _ => dynasm!(ops
                    ; .arch x64
                    ; cmp DWORD [rbx + a(dst)], eax
                    ; setc BYTE [rbx + C]
                    ; seto BYTE [rbx + V]
                    ; sets BYTE [rbx + N]
                    ; setz BYTE [rbx + Z]
                ),
            }
        }
        Op::Tst { size, src } => {
            load(ops, src, size);
            test(ops, size);
            logic_flags(ops);
        }
        Op::Clr { size, reg } => {
            dynasm!(ops ; .arch x64 ; xor eax, eax);
            store(ops, size, reg);
            dynasm!(ops
                ; .arch x64
                ; mov BYTE [rbx + N], 0
                ; mov BYTE [rbx + Z], 1
                ; mov BYTE [rbx + V], 0
                ; mov BYTE [rbx + C], 0
            );
        }
        Op::Interpret { pc, next } => {
            dynasm!(ops
                ; .arch x64
                ; mov DWORD [rbx + PC], pc as i32
                ; mov rdi, rbx
                ; mov esi, next as i32
                ; mov rax, QWORD interpret as *const () as i64
                ; call rax
                ; test eax, eax
                ; jz =>leave
            );
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
                dynasm!(ops ; .arch x64 ; test al, al ; jz =>not);
                cycles(ops, cycles_taken);
                goto(ops, target, jumps);
                dynasm!(ops ; .arch x64 ; =>not);
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
            dynasm!(ops ; .arch x64 ; test al, al ; jz =>decrement);
            cycles(ops, cycles_true);
            goto(ops, fall, jumps);
            // Dn.w - 1; the 68000 stops at -1, i.e. when it was 0 (carry).
            dynasm!(ops
                ; .arch x64
                ; =>decrement
                ; sub WORD [rbx + d(reg)], 1
                ; jc =>exhausted
            );
            cycles(ops, cycles_loop);
            goto(ops, target, jumps);
            dynasm!(ops ; .arch x64 ; =>exhausted);
            cycles(ops, cycles_exhausted);
            goto(ops, fall, jumps);
        }
        Op::Exit { pc } => goto(ops, pc, jumps),
        Op::Leave => dynasm!(ops ; .arch x64 ; jmp =>leave),
    }
}
