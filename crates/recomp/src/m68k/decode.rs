//! Turn 68000 machine code into [`Instruction`] values.
//!
//! The 68000 has 16-bit opcodes. The top 4 bits split the instruction set
//! into 16 "lines", which is also how Motorola's manual is organised:
//!
//! | line | instructions               | line | instructions                 |
//! |------|----------------------------|------|------------------------------|
//! | 0    | bit ops, MOVEP, immediates | 8    | OR, DIV, SBCD                |
//! | 1    | MOVE.B                     | 9    | SUB, SUBA, SUBX              |
//! | 2    | MOVE.L                     | A    | line-A trap (unused here)    |
//! | 3    | MOVE.W                     | B    | CMP, CMPA, CMPM, EOR         |
//! | 4    | miscellaneous              | C    | AND, MUL, ABCD, EXG          |
//! | 5    | ADDQ, SUBQ, Scc, DBcc      | D    | ADD, ADDA, ADDX              |
//! | 6    | Bcc, BRA, BSR              | E    | shifts and rotates           |
//! | 7    | MOVEQ                      | F    | line-F trap (coprocessor)    |
//!
//! After the opcode come 0-4 extension words: first the source operand's,
//! then the destination's. [`decode`] reads them through a closure so it
//! works on a ROM, a slice, or anything else.
//!
//! The decoder is *strict*: an encoding the 68000 would reject raises
//! [`DecodeError`]. That strictness is our first line of defence against
//! decoding graphics or music as if they were code.

use std::fmt;

use super::operand::{
    ALL, ALTERABLE, CONTROL, CONTROL_ALT, DATA, DATA_ALT, MEM_ALT, Mode, Modes, Operand,
};

/// Condition codes of Bcc / DBcc / Scc, in encoding order.
pub const CONDITIONS: [&str; 16] = [
    "t", "f", "hi", "ls", "cc", "cs", "ne", "eq", "vc", "vs", "pl", "mi", "ge", "lt", "gt", "le",
];

/// How an instruction affects control flow; the analysis relies on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Continue with the next instruction.
    Next,
    /// Unconditional jump to a static target (BRA, JMP abs).
    Branch,
    /// Maybe jump to the target, else fall through (Bcc, DBcc).
    Cond,
    /// Call a subroutine (static target or not), then come back.
    Call,
    /// Jump to an address only known at run time (`jmp (a0)`).
    Jump,
    /// RTS, RTE, RTR.
    Return,
    /// Raises an exception, then comes back (TRAP, CHK, DIV by zero...).
    Trap,
    /// The path ends here (ILLEGAL).
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub &'static str);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for DecodeError {}

type Result<T> = std::result::Result<T, DecodeError>;

/// One decoded 68000 instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub addr: u32,
    pub opcode: u16,
    /// Length in bytes, opcode and extension words included.
    pub length: u32,
    /// Lowercase name; a few have a suffix to tell forms apart
    /// (`move_to_sr`, `andi_ccr`...).
    pub mnemonic: &'static str,
    /// Operand size in bytes: 1, 2 or 4 (0 when it does not apply).
    pub size: u8,
    pub src: Option<Operand>,
    pub dst: Option<Operand>,
    /// Condition of Bcc / DBcc / Scc.
    pub cond: Option<u8>,
    /// Static branch or call target.
    pub target: Option<u32>,
    /// MOVEM register mask, TRAP number, STOP immediate.
    pub data: Option<u32>,
    pub flow: Flow,
    /// Rough 68000 cycle count.
    pub cycles: u32,
}

impl Instruction {
    fn new(addr: u32, opcode: u16) -> Self {
        Self {
            addr,
            opcode,
            length: 2,
            mnemonic: "?",
            size: 0,
            src: None,
            dst: None,
            cond: None,
            target: None,
            data: None,
            flow: Flow::Next,
            cycles: 4,
        }
    }

    #[must_use]
    pub fn next_addr(&self) -> u32 {
        self.addr.wrapping_add(self.length)
    }

    /// Motorola-style disassembly.
    #[must_use]
    pub fn text(&self) -> String {
        let mut name = self.mnemonic.split('_').next().unwrap_or("?").to_string();
        if let Some(c) = self.cond {
            name = name.replace("cc", CONDITIONS[usize::from(c)]);
            if name == "dbf" {
                name = "dbra".into();
            }
        }
        if !NO_SUFFIX.contains(&self.mnemonic) {
            name += match self.size {
                1 => ".b",
                2 => ".w",
                4 => ".l",
                _ => "",
            };
        }
        let mut ops: Vec<String> = Vec::new();
        if self.mnemonic == "movem" {
            let mask = self.data.unwrap_or(0) as u16;
            if let Some(d) = self.dst {
                ops.push(reglist(mask, d.mode == Mode::PreDec));
                ops.push(d.to_string());
            } else if let Some(s) = self.src {
                ops.push(s.to_string());
                ops.push(reglist(mask, false));
            }
        } else {
            ops.extend(self.src.iter().map(ToString::to_string));
            ops.extend(self.dst.iter().map(ToString::to_string));
            if let Some(t) = self.target {
                ops.push(format!("${t:x}"));
            }
            match (self.mnemonic, self.data) {
                ("trap", Some(n)) => ops.push(format!("#{n}")),
                ("stop", Some(n)) => ops.push(format!("#${n:04x}")),
                _ => {}
            }
        }
        if ops.is_empty() {
            name
        } else {
            format!("{name} {}", ops.join(","))
        }
    }
}

const NO_SUFFIX: &[&str] = &[
    "bra",
    "bsr",
    "bcc",
    "dbcc",
    "scc",
    "jmp",
    "jsr",
    "lea",
    "pea",
    "rts",
    "rte",
    "rtr",
    "nop",
    "reset",
    "stop",
    "trap",
    "trapv",
    "illegal",
    "link",
    "unlk",
    "exg",
    "swap",
    "linea",
    "linef",
    "move_usp",
    "andi_ccr",
    "ori_ccr",
    "eori_ccr",
    "andi_sr",
    "ori_sr",
    "eori_sr",
    "move_to_ccr",
    "move_to_sr",
    "move_from_sr",
    "moveq",
];

/// MOVEM register list: bit 0 = D0 .. bit 15 = A7 (reversed for `-(An)`).
fn reglist(mask: u16, reversed: bool) -> String {
    let mask = if reversed { mask.reverse_bits() } else { mask };
    let names: Vec<String> = (0..16)
        .filter(|i| mask >> i & 1 != 0)
        .map(|i| {
            if i < 8 {
                format!("d{i}")
            } else {
                format!("a{}", i - 8)
            }
        })
        .collect();
    if names.is_empty() {
        "0".into()
    } else {
        names.join("/")
    }
}

/// Sequential reader of the words following the opcode.
struct Reader<F> {
    read16: F,
    pos: u32,
}

impl<F: FnMut(u32) -> Option<u16>> Reader<F> {
    fn word(&mut self) -> Result<u16> {
        let w = (self.read16)(self.pos)
            .ok_or(DecodeError("instruction runs past the end of memory"))?;
        self.pos = self.pos.wrapping_add(2);
        Ok(w)
    }
    fn long(&mut self) -> Result<u32> {
        let hi = self.word()?;
        Ok(u32::from(hi) << 16 | u32::from(self.word()?))
    }

    /// Decode one effective address, consuming its extension words.
    fn ea(&mut self, mode: u16, reg: u16, size: u8, allowed: Modes) -> Result<Operand> {
        let r = reg as u8;
        let op = match mode {
            0 => Operand::dreg(r),
            1 => Operand::areg(r),
            2 => Operand::new(Mode::Ind, r),
            3 => Operand::new(Mode::PostInc, r),
            4 => Operand::new(Mode::PreDec, r),
            5 => {
                let mut o = Operand::new(Mode::Disp, r);
                o.disp = i32::from(self.word()? as i16);
                o
            }
            6 => self.brief(Operand::new(Mode::Index, r))?,
            _ => match reg {
                0 => {
                    let mut o = Operand::new(Mode::AbsW, 0);
                    o.addr = i32::from(self.word()? as i16) as u32;
                    o
                }
                1 => {
                    let mut o = Operand::new(Mode::AbsL, 0);
                    o.addr = self.long()?;
                    o
                }
                2 => {
                    let base = self.pos;
                    let mut o = Operand::new(Mode::PcDisp, 0);
                    o.addr = base.wrapping_add(i32::from(self.word()? as i16) as u32);
                    o
                }
                3 => {
                    let base = self.pos;
                    let mut o = self.brief(Operand::new(Mode::PcIndex, 0))?;
                    o.addr = base.wrapping_add(o.disp as u32);
                    o
                }
                4 => {
                    let v = match size {
                        1 => u32::from(self.word()? & 0xFF),
                        2 => u32::from(self.word()?),
                        4 => self.long()?,
                        _ => return Err(DecodeError("immediate operand without a size")),
                    };
                    Operand::imm(v)
                }
                _ => return Err(DecodeError("invalid addressing mode 7/5-7")),
            },
        };
        if !allowed.contains(op.mode) {
            return Err(DecodeError("addressing mode not allowed here"));
        }
        if op.mode == Mode::An && size == 1 {
            return Err(DecodeError("byte access to an address register"));
        }
        Ok(op)
    }

    fn brief(&mut self, mut op: Operand) -> Result<Operand> {
        let ext = self.word()?;
        if ext & 0x0700 != 0 {
            return Err(DecodeError("full extension word (68020+)"));
        }
        op.disp = i32::from(ext as u8 as i8);
        op.xreg = ((ext >> 12) & 7) as u8;
        op.xa = ext & 0x8000 != 0;
        op.xlong = ext & 0x0800 != 0;
        Ok(op)
    }
}

/// Standard size field at bits 7-6: 00 = .B, 01 = .W, 10 = .L.
fn size_bits(bits: u16) -> Result<u8> {
    match bits {
        0 => Ok(1),
        1 => Ok(2),
        2 => Ok(4),
        _ => Err(DecodeError("invalid size field")),
    }
}

/// Decode the instruction at `addr`. `read16` returns `None` past the end
/// of memory.
pub fn decode(read16: impl FnMut(u32) -> Option<u16>, addr: u32) -> Result<Instruction> {
    if addr & 1 != 0 {
        return Err(DecodeError("odd instruction address"));
    }
    let mut r = Reader { read16, pos: addr };
    let op = r.word()?;
    let mut ins = Instruction::new(addr, op);
    match op >> 12 {
        0x0 => line0(&mut ins, op, &mut r)?,
        0x1..=0x3 => line_move(&mut ins, op, &mut r)?,
        0x4 => line4(&mut ins, op, &mut r)?,
        0x5 => line5(&mut ins, op, &mut r)?,
        0x6 => line6(&mut ins, op, &mut r)?,
        0x7 => line7(&mut ins, op)?,
        0x8 => line8(&mut ins, op, &mut r)?,
        0x9 => add_sub(&mut ins, op, &mut r, "sub", "suba", "subx")?,
        0xB => line_b(&mut ins, op, &mut r)?,
        0xC => line_c(&mut ins, op, &mut r)?,
        0xD => add_sub(&mut ins, op, &mut r, "add", "adda", "addx")?,
        0xE => line_e(&mut ins, op, &mut r)?,
        0xA => (ins.mnemonic, ins.flow) = ("linea", Flow::Trap),
        _ => (ins.mnemonic, ins.flow) = ("linef", Flow::Trap),
    }
    ins.length = r.pos.wrapping_sub(addr);
    ins.cycles = estimate_cycles(&ins);
    Ok(ins)
}

fn ea_bits(op: u16) -> (u16, u16) {
    ((op >> 3) & 7, op & 7)
}

fn dn(op: u16) -> Operand {
    Operand::dreg(((op >> 9) & 7) as u8)
}

const BIT_OPS: [&str; 4] = ["btst", "bchg", "bclr", "bset"];

// --- line 0: immediate ops, bit ops, MOVEP ---------------------------------

fn line0<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    if op & 0x0100 != 0 {
        if mode == 1 {
            // MOVEP: register <-> every other byte of memory
            let opmode = (op >> 6) & 7;
            ins.mnemonic = "movep";
            ins.size = if opmode & 1 != 0 { 4 } else { 2 };
            let mut mem = Operand::new(Mode::Disp, reg as u8);
            mem.disp = i32::from(r.word()? as i16);
            if opmode & 2 != 0 {
                (ins.src, ins.dst) = (Some(dn(op)), Some(mem));
            } else {
                (ins.src, ins.dst) = (Some(mem), Some(dn(op)));
            }
            return Ok(());
        }
        let kind = usize::from((op >> 6) & 3); // BTST/BCHG/BCLR/BSET Dn,<ea>
        ins.mnemonic = BIT_OPS[kind];
        ins.src = Some(dn(op));
        let d = r.ea(mode, reg, 1, if kind == 0 { DATA } else { DATA_ALT })?;
        ins.size = if d.mode == Mode::Dn { 4 } else { 1 };
        ins.dst = Some(d);
        return Ok(());
    }
    let kind = (op >> 9) & 7;
    if kind == 4 {
        // BTST/BCHG/BCLR/BSET #n,<ea>; the CPU ignores the upper byte of n.
        let bkind = usize::from((op >> 6) & 3);
        ins.mnemonic = BIT_OPS[bkind];
        let bit = r.word()?;
        ins.src = Some(Operand::imm(u32::from(bit & 0xFF)));
        let allowed = if bkind == 0 {
            DATA.without(Mode::Imm)
        } else {
            DATA_ALT
        };
        let d = r.ea(mode, reg, 1, allowed)?;
        ins.size = if d.mode == Mode::Dn { 4 } else { 1 };
        ins.dst = Some(d);
        return Ok(());
    }
    let name = match kind {
        0 => "ori",
        1 => "andi",
        2 => "subi",
        3 => "addi",
        5 => "eori",
        6 => "cmpi",
        _ => return Err(DecodeError("unknown line-0 instruction")),
    };
    if op & 0x3F == 0x3C && matches!(name, "ori" | "andi" | "eori") {
        // ORI/ANDI/EORI to CCR (byte) or to SR (word, privileged)
        match (op >> 6) & 3 {
            0 => {
                ins.mnemonic = match name {
                    "ori" => "ori_ccr",
                    "andi" => "andi_ccr",
                    _ => "eori_ccr",
                };
                ins.size = 1;
                ins.src = Some(Operand::imm(u32::from(r.word()? & 0xFF)));
                ins.dst = Some(Operand::new(Mode::Ccr, 0));
            }
            1 => {
                ins.mnemonic = match name {
                    "ori" => "ori_sr",
                    "andi" => "andi_sr",
                    _ => "eori_sr",
                };
                ins.size = 2;
                ins.src = Some(Operand::imm(u32::from(r.word()?)));
                ins.dst = Some(Operand::new(Mode::Sr, 0));
            }
            _ => return Err(DecodeError("invalid size for immediate to CCR/SR")),
        }
        return Ok(());
    }
    ins.mnemonic = name;
    ins.size = size_bits((op >> 6) & 3)?;
    ins.src = Some(r.ea(7, 4, ins.size, ALL)?); // the immediate comes first
    ins.dst = Some(r.ea(mode, reg, ins.size, DATA_ALT)?);
    Ok(())
}

// --- lines 1-3: MOVE / MOVEA -------------------------------------------------

fn line_move<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    ins.size = match op >> 12 {
        1 => 1,
        3 => 2,
        _ => 4,
    };
    let (smode, sreg) = ea_bits(op);
    let (dreg, dmode) = ((op >> 9) & 7, (op >> 6) & 7);
    ins.src = Some(r.ea(smode, sreg, ins.size, ALL)?);
    if dmode == 1 {
        if ins.size == 1 {
            return Err(DecodeError("MOVEA.B does not exist"));
        }
        ins.mnemonic = "movea";
        ins.dst = Some(Operand::areg(dreg as u8));
    } else {
        ins.mnemonic = "move";
        ins.dst = Some(r.ea(dmode, dreg, ins.size, DATA_ALT)?);
    }
    Ok(())
}

// --- line 4: miscellaneous -----------------------------------------------------

fn line4<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let simple = match op {
        0x4AFC => Some(("illegal", Flow::Stop)),
        0x4E70 => Some(("reset", Flow::Next)),
        0x4E71 => Some(("nop", Flow::Next)),
        0x4E73 => Some(("rte", Flow::Return)),
        0x4E75 => Some(("rts", Flow::Return)),
        0x4E76 => Some(("trapv", Flow::Trap)),
        0x4E77 => Some(("rtr", Flow::Return)),
        _ => None,
    };
    if let Some((name, flow)) = simple {
        (ins.mnemonic, ins.flow) = (name, flow);
        return Ok(());
    }
    if op == 0x4E72 {
        ins.mnemonic = "stop";
        ins.data = Some(u32::from(r.word()?));
        return Ok(());
    }
    if op & 0xFFF0 == 0x4E40 {
        (ins.mnemonic, ins.flow, ins.data) = ("trap", Flow::Trap, Some(u32::from(op & 15)));
        return Ok(());
    }
    if op & 0xFFF8 == 0x4E50 {
        ins.mnemonic = "link";
        ins.size = 2;
        ins.src = Some(Operand::areg(reg as u8));
        ins.dst = Some(Operand::imm(i32::from(r.word()? as i16) as u32));
        return Ok(());
    }
    if op & 0xFFF8 == 0x4E58 {
        (ins.mnemonic, ins.dst) = ("unlk", Some(Operand::areg(reg as u8)));
        return Ok(());
    }
    if op & 0xFFF0 == 0x4E60 {
        ins.mnemonic = "move_usp";
        ins.size = 4;
        let usp = Operand::new(Mode::Usp, 0);
        if op & 8 != 0 {
            (ins.src, ins.dst) = (Some(usp), Some(Operand::areg(reg as u8)));
        } else {
            (ins.src, ins.dst) = (Some(Operand::areg(reg as u8)), Some(usp));
        }
        return Ok(());
    }
    if matches!(op & 0xFFC0, 0x4E80 | 0x4EC0) {
        // JSR / JMP
        let is_jsr = op & 0xFFC0 == 0x4E80;
        ins.mnemonic = if is_jsr { "jsr" } else { "jmp" };
        let d = r.ea(mode, reg, 4, CONTROL)?;
        ins.target = d.static_address();
        ins.flow = match (is_jsr, ins.target) {
            (true, _) => Flow::Call,
            (false, Some(_)) => Flow::Branch,
            (false, None) => Flow::Jump,
        };
        if ins.target.is_none() {
            ins.dst = Some(d);
        }
        return Ok(());
    }
    if op & 0xFFB8 == 0x4880 {
        // EXT.W / EXT.L
        ins.mnemonic = "ext";
        ins.size = if op & 0x40 != 0 { 4 } else { 2 };
        ins.dst = Some(Operand::dreg(reg as u8));
        return Ok(());
    }
    if op & 0xFB80 == 0x4880 {
        // MOVEM
        ins.mnemonic = "movem";
        ins.size = if op & 0x40 != 0 { 4 } else { 2 };
        ins.data = Some(u32::from(r.word()?));
        if op & 0x0400 != 0 {
            ins.src = Some(r.ea(mode, reg, ins.size, CONTROL.with(Mode::PostInc))?);
        } else {
            ins.dst = Some(r.ea(mode, reg, ins.size, CONTROL_ALT.with(Mode::PreDec))?);
        }
        return Ok(());
    }
    if op & 0xF1C0 == 0x41C0 {
        ins.mnemonic = "lea";
        ins.size = 4;
        ins.src = Some(r.ea(mode, reg, 4, CONTROL)?);
        ins.dst = Some(Operand::areg(((op >> 9) & 7) as u8));
        return Ok(());
    }
    if op & 0xF1C0 == 0x4180 {
        (ins.mnemonic, ins.size, ins.flow) = ("chk", 2, Flow::Trap);
        ins.src = Some(r.ea(mode, reg, 2, DATA)?);
        ins.dst = Some(dn(op));
        return Ok(());
    }
    match op & 0xFFC0 {
        0x40C0 => {
            (ins.mnemonic, ins.size) = ("move_from_sr", 2);
            ins.src = Some(Operand::new(Mode::Sr, 0));
            ins.dst = Some(r.ea(mode, reg, 2, DATA_ALT)?);
            return Ok(());
        }
        0x44C0 => {
            (ins.mnemonic, ins.size) = ("move_to_ccr", 2);
            ins.src = Some(r.ea(mode, reg, 2, DATA)?);
            ins.dst = Some(Operand::new(Mode::Ccr, 0));
            return Ok(());
        }
        0x46C0 => {
            (ins.mnemonic, ins.size) = ("move_to_sr", 2);
            ins.src = Some(r.ea(mode, reg, 2, DATA)?);
            ins.dst = Some(Operand::new(Mode::Sr, 0));
            return Ok(());
        }
        0x4800 => {
            (ins.mnemonic, ins.size) = ("nbcd", 1);
            ins.dst = Some(r.ea(mode, reg, 1, DATA_ALT)?);
            return Ok(());
        }
        0x4840 => {
            if mode == 0 {
                (ins.mnemonic, ins.size, ins.dst) = ("swap", 4, Some(Operand::dreg(reg as u8)));
            } else {
                (ins.mnemonic, ins.size) = ("pea", 4);
                ins.src = Some(r.ea(mode, reg, 4, CONTROL)?);
            }
            return Ok(());
        }
        0x4AC0 => {
            (ins.mnemonic, ins.size) = ("tas", 1);
            ins.dst = Some(r.ea(mode, reg, 1, DATA_ALT)?);
            return Ok(());
        }
        _ => {}
    }
    let name = match op & 0xFF00 {
        0x4000 => "negx",
        0x4200 => "clr",
        0x4400 => "neg",
        0x4600 => "not",
        0x4A00 => "tst",
        _ => return Err(DecodeError("unknown line-4 instruction")),
    };
    ins.mnemonic = name;
    ins.size = size_bits((op >> 6) & 3)?;
    ins.dst = Some(r.ea(mode, reg, ins.size, DATA_ALT)?);
    Ok(())
}

// --- line 5: ADDQ / SUBQ / Scc / DBcc --------------------------------------

fn line5<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    if (op >> 6) & 3 == 3 {
        ins.cond = Some(((op >> 8) & 15) as u8);
        if mode == 1 {
            (ins.mnemonic, ins.size, ins.flow) = ("dbcc", 2, Flow::Cond);
            ins.dst = Some(Operand::dreg(reg as u8));
            let base = r.pos;
            ins.target = Some(base.wrapping_add(i32::from(r.word()? as i16) as u32));
        } else {
            (ins.mnemonic, ins.size) = ("scc", 1);
            ins.dst = Some(r.ea(mode, reg, 1, DATA_ALT)?);
        }
        return Ok(());
    }
    ins.mnemonic = if op & 0x100 != 0 { "subq" } else { "addq" };
    ins.size = size_bits((op >> 6) & 3)?;
    let data = (op >> 9) & 7;
    ins.src = Some(Operand::imm(if data == 0 { 8 } else { u32::from(data) }));
    ins.dst = Some(r.ea(mode, reg, ins.size, ALTERABLE)?);
    Ok(())
}

// --- line 6: Bcc / BRA / BSR -------------------------------------------------

fn line6<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let cond = ((op >> 8) & 15) as u8;
    let base = r.pos;
    // A zero byte means "16-bit displacement follows". ($FF announces a
    // 32-bit one only on the 68020+; on a 68000 it is simply -1, a branch
    // to an odd address that faults when taken.)
    let disp = match op & 0xFF {
        0 => i32::from(r.word()? as i16),
        d => i32::from(d as u8 as i8),
    };
    ins.target = Some(base.wrapping_add(disp as u32));
    match cond {
        0 => (ins.mnemonic, ins.flow) = ("bra", Flow::Branch),
        1 => (ins.mnemonic, ins.flow) = ("bsr", Flow::Call),
        _ => (ins.mnemonic, ins.cond, ins.flow) = ("bcc", Some(cond), Flow::Cond),
    }
    Ok(())
}

fn line7(ins: &mut Instruction, op: u16) -> Result<()> {
    if op & 0x100 != 0 {
        return Err(DecodeError("MOVEQ with bit 8 set"));
    }
    (ins.mnemonic, ins.size) = ("moveq", 4);
    ins.src = Some(Operand::imm(i32::from(op as u8 as i8) as u32));
    ins.dst = Some(dn(op));
    Ok(())
}

// --- lines 8, 9, B, C, D: two-operand arithmetic and logic -----------------

/// ADDX/SUBX/ABCD/SBCD: `Dy,Dx` or `-(Ay),-(Ax)`.
fn x_form(ins: &mut Instruction, op: u16, name: &'static str) {
    ins.mnemonic = name;
    let (rx, ry) = (((op >> 9) & 7) as u8, (op & 7) as u8);
    if op & 8 != 0 {
        ins.src = Some(Operand::new(Mode::PreDec, ry));
        ins.dst = Some(Operand::new(Mode::PreDec, rx));
    } else {
        ins.src = Some(Operand::dreg(ry));
        ins.dst = Some(Operand::dreg(rx));
    }
}

/// Opmode 0-2: `<ea>,Dn`; opmode 4-6: `Dn,<ea>`.
fn arith<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
    name: &'static str,
    src_allowed: Modes,
    dst_allowed: Modes,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let opmode = (op >> 6) & 7;
    ins.mnemonic = name;
    ins.size = size_bits(opmode & 3)?;
    if opmode < 4 {
        ins.src = Some(r.ea(mode, reg, ins.size, src_allowed)?);
        ins.dst = Some(dn(op));
    } else {
        ins.src = Some(dn(op));
        ins.dst = Some(r.ea(mode, reg, ins.size, dst_allowed)?);
    }
    Ok(())
}

fn line8<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        ins.mnemonic = if opmode == 3 { "divu" } else { "divs" };
        (ins.size, ins.flow) = (2, Flow::Trap); // may raise "divide by zero"
        ins.src = Some(r.ea(mode, reg, 2, DATA)?);
        ins.dst = Some(dn(op));
        return Ok(());
    }
    if op & 0x1F0 == 0x100 {
        ins.size = 1;
        x_form(ins, op, "sbcd");
        return Ok(());
    }
    arith(ins, op, r, "or", DATA, MEM_ALT)
}

fn add_sub<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
    name: &'static str,
    name_a: &'static str,
    name_x: &'static str,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        ins.mnemonic = name_a;
        ins.size = if opmode == 3 { 2 } else { 4 };
        ins.src = Some(r.ea(mode, reg, ins.size, ALL)?);
        ins.dst = Some(Operand::areg(((op >> 9) & 7) as u8));
        return Ok(());
    }
    if opmode >= 4 && mode <= 1 {
        ins.size = size_bits(opmode & 3)?;
        x_form(ins, op, name_x);
        return Ok(());
    }
    arith(ins, op, r, name, ALL, MEM_ALT)
}

fn line_b<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        ins.mnemonic = "cmpa";
        ins.size = if opmode == 3 { 2 } else { 4 };
        ins.src = Some(r.ea(mode, reg, ins.size, ALL)?);
        ins.dst = Some(Operand::areg(((op >> 9) & 7) as u8));
        return Ok(());
    }
    if opmode < 3 {
        return arith(ins, op, r, "cmp", ALL, ALL);
    }
    ins.size = size_bits(opmode & 3)?;
    if mode == 1 {
        ins.mnemonic = "cmpm";
        ins.src = Some(Operand::new(Mode::PostInc, reg as u8));
        ins.dst = Some(Operand::new(Mode::PostInc, ((op >> 9) & 7) as u8));
        return Ok(());
    }
    ins.mnemonic = "eor";
    ins.src = Some(dn(op));
    ins.dst = Some(r.ea(mode, reg, ins.size, DATA_ALT)?);
    Ok(())
}

fn line_c<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let (mode, reg) = ea_bits(op);
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        ins.mnemonic = if opmode == 3 { "mulu" } else { "muls" };
        ins.size = 2;
        ins.src = Some(r.ea(mode, reg, 2, DATA)?);
        ins.dst = Some(dn(op));
        return Ok(());
    }
    if op & 0x1F0 == 0x100 {
        ins.size = 1;
        x_form(ins, op, "abcd");
        return Ok(());
    }
    let (rx, ry) = (((op >> 9) & 7) as u8, reg as u8);
    let pair = match op & 0x1F8 {
        0x140 => Some((Operand::dreg(rx), Operand::dreg(ry))),
        0x148 => Some((Operand::areg(rx), Operand::areg(ry))),
        0x188 => Some((Operand::dreg(rx), Operand::areg(ry))),
        _ => None,
    };
    if let Some((a, b)) = pair {
        (ins.mnemonic, ins.size, ins.src, ins.dst) = ("exg", 4, Some(a), Some(b));
        return Ok(());
    }
    arith(ins, op, r, "and", DATA, MEM_ALT)
}

// --- line E: shifts and rotates ---------------------------------------------------

const SHIFTS: [[&str; 2]; 4] = [
    ["asr", "asl"],
    ["lsr", "lsl"],
    ["roxr", "roxl"],
    ["ror", "rol"],
];

fn line_e<F: FnMut(u32) -> Option<u16>>(
    ins: &mut Instruction,
    op: u16,
    r: &mut Reader<F>,
) -> Result<()> {
    let left = usize::from(op & 0x100 != 0);
    if (op >> 6) & 3 == 3 {
        // memory form: shift one word by one bit
        if op & 0x0800 != 0 {
            return Err(DecodeError("bit-field instructions (68020+)"));
        }
        let (mode, reg) = ea_bits(op);
        ins.mnemonic = SHIFTS[usize::from((op >> 9) & 3)][left];
        ins.size = 2;
        ins.src = Some(Operand::imm(1));
        ins.dst = Some(r.ea(mode, reg, 2, MEM_ALT)?);
        return Ok(());
    }
    ins.mnemonic = SHIFTS[usize::from((op >> 3) & 3)][left];
    ins.size = size_bits((op >> 6) & 3)?;
    let count = (op >> 9) & 7;
    ins.src = Some(if op & 0x20 != 0 {
        Operand::dreg(count as u8)
    } else {
        Operand::imm(if count == 0 { 8 } else { u32::from(count) })
    });
    ins.dst = Some(Operand::dreg((op & 7) as u8));
    Ok(())
}

// --- cycle estimate --------------------------------------------------------------
//
// Real 68000 timings depend on the addressing mode and on wait states. We
// only need "about right" so that the number of instructions executed per
// scanline, and so per frame, is plausible.

fn estimate_cycles(ins: &Instruction) -> u32 {
    let fixed = match ins.mnemonic {
        "mulu" | "muls" => 54,
        "divu" => 108,
        "divs" => 122,
        "rts" => 16,
        "rte" | "rtr" => 20,
        "jsr" | "bsr" => 18,
        "trap" => 34,
        "bra" | "bcc" | "dbcc" | "jmp" => 10,
        "link" => 16,
        "unlk" => 12,
        "stop" => 4,
        _ => 0,
    };
    if fixed != 0 {
        return fixed;
    }
    let mut cycles = 2 * ins.length;
    for opnd in [ins.src, ins.dst].into_iter().flatten() {
        if opnd.is_memory() {
            cycles += if ins.size == 4 { 8 } else { 4 };
        }
    }
    if ins.mnemonic == "movem" {
        cycles += ins.data.unwrap_or(0).count_ones() * if ins.size == 4 { 8 } else { 4 };
    }
    if matches!(&ins.mnemonic[..2], "as" | "ls" | "ro") {
        cycles += 6;
    }
    (cycles + 2).max(4)
}
