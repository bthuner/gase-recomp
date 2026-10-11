//! The front end: 68000 code at an address → a block of [`Op`]s.
//!
//! A *block* is a straight run of instructions that ends at the first
//! branch (or after [`MAX_BLOCK`] instructions). Each instruction becomes
//! either a native operation the backend writes as machine code, or
//! [`Op::Interpret`]: "run this one instruction on gase's interpreter".
//! Everything is correct from day one; native coverage grows one
//! instruction family at a time, and the difftest checks each.
//!
//! The decoder and the timings are the static recompiler's
//! ([`gase_recomp::m68k`]): the same instruction is translated the same way
//! whether it is translated before the game runs or while it runs.

use gase_recomp::m68k::{Flow, Instruction, Mode, Operand, decode, timing};

/// At most this many instructions per block.
pub const MAX_BLOCK: usize = 32;
/// At most this many instructions per region (see [`region`]).
pub const MAX_REGION: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    fn of(bytes: u8) -> Option<Self> {
        match bytes {
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::Long),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add,
    Sub,
    And,
    Or,
    Eor,
    /// Like `Sub`, but only the flags are kept (and X is untouched).
    Cmp,
}

/// Where an operand is: the 68000's addressing modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ea {
    D(u8),
    A(u8),
    /// `(An)`
    Ind(u8),
    /// `(An)+`
    PostInc(u8),
    /// `-(An)`
    PreDec(u8),
    /// `d16(An)`
    Disp(u8, i32),
    /// `d8(An,Xn)` or `d8(PC,Xn)`.
    Index {
        base: Base,
        disp: i32,
        x: u8,
        /// Xn is an address register.
        xa: bool,
        /// Xn.l rather than Xn.w (sign-extended).
        xlong: bool,
    },
    /// An address known when compiling: absolute, or PC-relative.
    Abs(u32),
    Imm(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    A(u8),
    /// `PC + d8`, already added up.
    Pc(u32),
}

impl Ea {
    fn of(o: Operand) -> Option<Self> {
        Some(match o.mode {
            Mode::Dn => Self::D(o.reg),
            Mode::An => Self::A(o.reg),
            Mode::Ind => Self::Ind(o.reg),
            Mode::PostInc => Self::PostInc(o.reg),
            Mode::PreDec => Self::PreDec(o.reg),
            Mode::Disp => Self::Disp(o.reg, o.disp),
            Mode::Index | Mode::PcIndex => Self::Index {
                base: if o.mode == Mode::Index {
                    Base::A(o.reg)
                } else {
                    Base::Pc(o.addr)
                },
                disp: if o.mode == Mode::Index { o.disp } else { 0 },
                x: o.xreg,
                xa: o.xa,
                xlong: o.xlong,
            },
            // All 32 bits: LEA keeps them (memory accesses use 24).
            Mode::AbsW | Mode::AbsL | Mode::PcDisp => Self::Abs(o.addr),
            Mode::Imm => Self::Imm(o.imm),
            Mode::Sr | Mode::Ccr | Mode::Usp => return None,
        })
    }

    /// In memory (needs an address)?
    #[must_use]
    pub fn is_memory(self) -> bool {
        !matches!(self, Self::D(_) | Self::A(_) | Self::Imm(_))
    }
}

/// One step of a compiled block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Count this many 68000 cycles (measured on gase's interpreter).
    Cycles(u32),
    /// `moveq #value,Dn`: the flags are known when compiling.
    Moveq { dst: u8, value: u32 },
    /// `move.<size> src,dst` (N and Z from the value, V and C cleared), or
    /// `movea` when `dst` is an address register (no flags; a word is
    /// sign-extended).
    Move { size: Size, src: Ea, dst: Ea },
    /// `lea src,An`.
    Lea { src: Ea, dst: u8 },
    /// `<op>.<size> src,Dn` with 68000 flags.
    Alu {
        op: Alu,
        size: Size,
        src: Ea,
        dst: u8,
    },
    /// `adda`/`suba` (no flags) and `cmpa` (flags, on 32 bits): a word
    /// source is sign-extended. Also `addq`/`subq` to An.
    AluA {
        op: Alu,
        size: Size,
        src: Ea,
        dst: u8,
    },
    /// `tst.<size> src`.
    Tst { size: Size, src: Ea },
    /// `clr.<size> Dn`.
    Clr { size: Size, reg: u8 },
    /// Run the instruction at `pc` on the interpreter (it counts its own
    /// cycles). If it did not continue at `next` (an exception, a branch),
    /// leave the block.
    Interpret { pc: u32, next: u32 },
    /// `Bcc` (`cond` = 2..15) or `BRA` (`None`): set `pc`, leave.
    Branch {
        cond: Option<u8>,
        target: u32,
        fall: u32,
        cycles_taken: u32,
        cycles_not: u32,
    },
    /// `DBcc Dn,target`: set `pc`, leave.
    Dbcc {
        cond: u8,
        reg: u8,
        target: u32,
        fall: u32,
        /// Condition true: no decrement.
        cycles_true: u32,
        cycles_loop: u32,
        cycles_exhausted: u32,
    },
    /// Continue at `pc` (the block is full).
    Exit { pc: u32 },
    /// Leave; `pc` is already set (after an interpreted branch).
    Leave,
}

/// Instructions after which the block must end even though the
/// interpreter continued at the next address: they change the interrupt
/// mask or stop the CPU, and the scheduler must look at interrupts now.
const ENDS_BLOCK: &[&str] = &[
    "move_to_sr",
    "andi_sr",
    "ori_sr",
    "eori_sr",
    "stop",
    "reset",
];

/// A data register operand.
fn dreg(op: Option<Operand>) -> Option<u8> {
    op.filter(|o| o.mode == Mode::Dn).map(|o| o.reg)
}

/// An address register operand.
fn areg(op: Option<Operand>) -> Option<u8> {
    op.filter(|o| o.mode == Mode::An).map(|o| o.reg)
}

fn ea(op: Option<Operand>) -> Option<Ea> {
    Ea::of(op?)
}

/// The native form of one instruction, if the backend has one.
fn native(ins: &Instruction) -> Option<Op> {
    let size = Size::of(ins.size);
    let even = |t: u32| t & 1 == 0;
    Some(match ins.mnemonic {
        "moveq" => Op::Moveq {
            dst: dreg(ins.dst)?,
            value: ins.src?.imm,
        },
        "move" | "movea" => Op::Move {
            size: size?,
            src: ea(ins.src)?,
            dst: ea(ins.dst)?,
        },
        "lea" => Op::Lea {
            src: ea(ins.src)?,
            dst: areg(ins.dst)?,
        },
        "adda" | "suba" | "cmpa" => Op::AluA {
            op: match ins.mnemonic {
                "adda" => Alu::Add,
                "suba" => Alu::Sub,
                _ => Alu::Cmp,
            },
            size: size?,
            src: ea(ins.src)?,
            dst: areg(ins.dst)?,
        },
        "addq" | "subq" if areg(ins.dst).is_some() => Op::AluA {
            op: if ins.mnemonic == "addq" {
                Alu::Add
            } else {
                Alu::Sub
            },
            size: Size::Long,
            src: ea(ins.src)?,
            dst: areg(ins.dst)?,
        },
        "add" | "addq" | "addi" | "sub" | "subq" | "subi" | "and" | "andi" | "or" | "ori"
        | "eor" | "eori" | "cmp" | "cmpi" => Op::Alu {
            op: match &ins.mnemonic[..2] {
                "ad" => Alu::Add,
                "su" => Alu::Sub,
                "an" => Alu::And,
                "or" => Alu::Or,
                "eo" => Alu::Eor,
                _ => Alu::Cmp,
            },
            size: size?,
            src: ea(ins.src)?,
            dst: dreg(ins.dst)?,
        },
        "tst" => Op::Tst {
            size: size?,
            src: ea(ins.dst.or(ins.src))?,
        },
        "clr" => Op::Clr {
            size: size?,
            reg: dreg(ins.dst.or(ins.src))?,
        },
        "bra" | "bcc" if ins.target.is_some_and(even) => Op::Branch {
            cond: ins.cond,
            target: ins.target? & 0xFF_FFFF,
            fall: ins.next_addr(),
            // BRA has one outcome, measured in `cycles`; Bcc two.
            cycles_taken: if ins.cond.is_some() {
                ins.cycles_alt[0]
            } else {
                ins.cycles
            },
            cycles_not: ins.cycles,
        },
        "dbcc" if ins.target.is_some_and(even) => Op::Dbcc {
            cond: ins.cond?,
            reg: dreg(ins.dst.or(ins.src))?,
            target: ins.target? & 0xFF_FFFF,
            fall: ins.next_addr(),
            cycles_true: ins.cycles,
            cycles_loop: ins.cycles_alt[0],
            cycles_exhausted: ins.cycles_alt[1],
        },
        _ => return None,
    })
}

/// Compile the block at `pc`. `read16` reads the program (ROM).
pub fn block(mut read16: impl FnMut(u32) -> Option<u16>, pc: u32) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut at = pc;
    for _ in 0..MAX_BLOCK {
        let Ok(mut ins) = decode(&mut read16, at) else {
            // Not an instruction we know: the interpreter raises the right
            // exception.
            ops.push(Op::Interpret {
                pc: at,
                next: at.wrapping_add(2),
            });
            ops.push(Op::Leave);
            return ops;
        };
        timing::measure(&mut ins, &mut read16);
        let next = ins.next_addr();
        match native(&ins) {
            Some(op @ (Op::Branch { .. } | Op::Dbcc { .. })) => {
                ops.push(op);
                return ops;
            }
            Some(op) => {
                ops.push(Op::Cycles(ins.cycles));
                ops.push(op);
            }
            None => {
                ops.push(Op::Interpret { pc: at, next });
                if ins.flow != Flow::Next || ENDS_BLOCK.contains(&ins.mnemonic) {
                    ops.push(Op::Leave);
                    return ops;
                }
            }
        }
        at = next;
    }
    ops.push(Op::Exit { pc: at });
    ops
}

/// A *region*: the block at `pc` and the blocks its branches reach, as long
/// as they are in ROM and the region stays small. Branches between blocks
/// of a region become direct jumps in the machine code, so a loop runs
/// without coming back to the engine until the console needs attention.
pub fn region(
    mut read16: impl FnMut(u32) -> Option<u16>,
    pc: u32,
    rom_end: u32,
) -> Vec<(u32, Vec<Op>)> {
    let mut blocks: Vec<(u32, Vec<Op>)> = Vec::new();
    let mut todo = vec![pc];
    let mut size = 0;
    while let Some(at) = todo.pop() {
        if blocks.iter().any(|(b, _)| *b == at) || size >= MAX_REGION {
            continue;
        }
        let ops = block(&mut read16, at);
        size += ops.iter().filter(|o| !matches!(o, Op::Cycles(_))).count();
        for exit in exits(&ops) {
            if exit < rom_end && exit & 1 == 0 {
                todo.push(exit);
            }
        }
        blocks.push((at, ops));
    }
    blocks
}

/// Where a block may continue (statically known).
fn exits(ops: &[Op]) -> Vec<u32> {
    match ops.last() {
        Some(Op::Branch {
            cond: None, target, ..
        }) => vec![*target],
        Some(Op::Branch { target, fall, .. } | Op::Dbcc { target, fall, .. }) => {
            vec![*fall, *target]
        }
        Some(Op::Exit { pc }) => vec![*pc],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(words: &[u16]) -> Vec<Op> {
        block(|a| words.get((a / 2) as usize).copied(), 0)
    }

    #[test]
    fn register_code_is_native_and_branches_end_blocks() {
        let ops = compile(&[
            0x7005, // moveq #5,d0
            0xD280, // add.l d0,d1
            0x4A41, // tst.w d1
            0x66F8, // bne.s -8 (to 0)
        ]);
        assert_eq!(
            ops,
            [
                Op::Cycles(4),
                Op::Moveq { dst: 0, value: 5 },
                Op::Cycles(8),
                Op::Alu {
                    op: Alu::Add,
                    size: Size::Long,
                    src: Ea::D(0),
                    dst: 1
                },
                Op::Cycles(4),
                Op::Tst {
                    size: Size::Word,
                    src: Ea::D(1)
                },
                Op::Branch {
                    cond: Some(6),
                    target: 0,
                    fall: 8,
                    cycles_taken: 10,
                    cycles_not: 8
                },
            ]
        );
    }

    #[test]
    fn a_loop_nest_is_one_region() {
        let words = [
            0x7E3F, 0x7CFF, 0xD081, 0x6302, 0x5284, 0x51CE, 0xFFF8, 0x51CF, 0xFFF2, 0x4E75,
        ];
        let r = region(|a| words.get((a / 2) as usize).copied(), 0, 0x40_0000);
        let mut starts: Vec<u32> = r.iter().map(|(pc, _)| *pc).collect();
        starts.sort_unstable();
        // entry, outer, inner, addq, dbf d6 (bls target), dbf d7, rts.
        assert_eq!(starts, [0x0, 0x2, 0x4, 0x8, 0xA, 0xE, 0x12]);
    }

    #[test]
    fn the_rest_is_interpreted() {
        let ops = compile(&[
            0xC2C0, // mulu d0,d1: not native yet
            0x4E75, // rts: control flow
        ]);
        assert_eq!(
            ops,
            [
                Op::Interpret { pc: 0, next: 2 },
                Op::Interpret { pc: 2, next: 4 },
                Op::Leave
            ]
        );
        // Memory operands are native.
        assert_eq!(
            compile(&[0x3080])[1], // move.w d0,(a0)
            Op::Move {
                size: Size::Word,
                src: Ea::D(0),
                dst: Ea::Ind(0)
            }
        );
    }
}
