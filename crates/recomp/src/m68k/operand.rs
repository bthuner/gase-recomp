//! The 68000 "effective address" (EA) modes.
//!
//! Almost every 68000 instruction encodes an operand as a 6-bit field:
//! 3 bits of MODE and 3 bits of REGISTER. Mode 7 uses the register field as
//! a sub-mode. Some modes need *extension words* after the opcode.
//!
//! | mode | reg | syntax      | meaning                         | ext words |
//! |------|-----|-------------|---------------------------------|-----------|
//! | 0    | n   | `Dn`        | data register                   | 0         |
//! | 1    | n   | `An`        | address register                | 0         |
//! | 2    | n   | `(An)`      | memory at An                    | 0         |
//! | 3    | n   | `(An)+`     | memory at An, then An += size   | 0         |
//! | 4    | n   | `-(An)`     | An -= size, then memory at An   | 0         |
//! | 5    | n   | `d16(An)`   | memory at An + d16              | 1         |
//! | 6    | n   | `d8(An,Xn)` | memory at An + Xn + d8          | 1 (brief) |
//! | 7    | 0   | `$xxxx.w`   | absolute short (sign-extended)  | 1         |
//! | 7    | 1   | `$xxxxxxxx` | absolute long                   | 2         |
//! | 7    | 2   | `d16(PC)`   | PC-relative                     | 1         |
//! | 7    | 3   | `d8(PC,Xn)` | PC-relative with index          | 1 (brief) |
//! | 7    | 4   | `#imm`      | immediate                       | 1 or 2    |
//!
//! The *brief extension word* of the indexed modes: bit 15 = index is An,
//! bits 14-12 = index register, bit 11 = index is `.L` (else sign-extended
//! `.W`), bits 7-0 = signed 8-bit displacement.

use std::fmt;

/// Addressing modes, plus three pseudo-operands (`sr`, `ccr`, `usp`) used by
/// the instructions that move the status register or the user stack pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Dn,
    An,
    Ind,
    PostInc,
    PreDec,
    Disp,
    Index,
    AbsW,
    AbsL,
    PcDisp,
    PcIndex,
    Imm,
    Sr,
    Ccr,
    Usp,
}

/// A set of allowed modes, as a bit mask (the Motorola manual's categories).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes(u16);

impl Modes {
    const fn of(modes: &[Mode]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < modes.len() {
            bits |= 1 << modes[i] as u16;
            i += 1;
        }
        Self(bits)
    }
    #[must_use]
    pub const fn contains(self, m: Mode) -> bool {
        self.0 & (1 << m as u16) != 0
    }
    #[must_use]
    pub const fn with(self, m: Mode) -> Self {
        Self(self.0 | 1 << m as u16)
    }
    #[must_use]
    pub const fn without(self, m: Mode) -> Self {
        Self(self.0 & !(1 << m as u16))
    }
}

use Mode::*;

/// Every addressing mode.
pub const ALL: Modes = Modes::of(&[
    Dn, An, Ind, PostInc, PreDec, Disp, Index, AbsW, AbsL, PcDisp, PcIndex, Imm,
]);
/// Everything but `An`.
pub const DATA: Modes = ALL.without(An);
/// Modes that can be written to.
pub const ALTERABLE: Modes = ALL.without(PcDisp).without(PcIndex).without(Imm);
pub const DATA_ALT: Modes = ALTERABLE.without(An);
pub const MEM_ALT: Modes = DATA_ALT.without(Dn);
/// Modes that name an address (for LEA, JMP, JSR...).
pub const CONTROL: Modes = Modes::of(&[Ind, Disp, Index, AbsW, AbsL, PcDisp, PcIndex]);
pub const CONTROL_ALT: Modes = CONTROL.without(PcDisp).without(PcIndex);

/// One decoded operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operand {
    pub mode: Mode,
    pub reg: u8,
    /// d16 / d8 displacement.
    pub disp: i32,
    /// Static address, pre-computed for `AbsW`, `AbsL`, `PcDisp` and (base
    /// only) `PcIndex`.
    pub addr: u32,
    pub imm: u32,
    pub xreg: u8,
    pub xa: bool,
    pub xlong: bool,
}

impl Operand {
    #[must_use]
    pub const fn new(mode: Mode, reg: u8) -> Self {
        Self {
            mode,
            reg,
            disp: 0,
            addr: 0,
            imm: 0,
            xreg: 0,
            xa: false,
            xlong: false,
        }
    }
    #[must_use]
    pub const fn dreg(n: u8) -> Self {
        Self::new(Dn, n)
    }
    #[must_use]
    pub const fn areg(n: u8) -> Self {
        Self::new(An, n)
    }
    #[must_use]
    pub const fn imm(v: u32) -> Self {
        let mut o = Self::new(Imm, 0);
        o.imm = v;
        o
    }

    /// Does the operand live in memory?
    #[must_use]
    pub fn is_memory(&self) -> bool {
        !matches!(self.mode, Dn | An | Imm | Sr | Ccr | Usp)
    }

    /// The address, when it is known at recompile time.
    #[must_use]
    pub fn static_address(&self) -> Option<u32> {
        matches!(self.mode, AbsW | AbsL | PcDisp).then_some(self.addr)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let an = |r: u8| {
            if r == 7 {
                "sp".to_string()
            } else {
                format!("a{r}")
            }
        };
        let x = format!(
            "{}{}.{}",
            if self.xa { 'a' } else { 'd' },
            self.xreg,
            if self.xlong { 'l' } else { 'w' }
        );
        match self.mode {
            Dn => write!(f, "d{}", self.reg),
            An => write!(f, "{}", an(self.reg)),
            Ind => write!(f, "({})", an(self.reg)),
            PostInc => write!(f, "({})+", an(self.reg)),
            PreDec => write!(f, "-({})", an(self.reg)),
            Disp => write!(f, "{}({})", self.disp, an(self.reg)),
            Index => write!(f, "{}({},{x})", self.disp, an(self.reg)),
            AbsW => write!(f, "(${:x}).w", self.addr & 0xFF_FFFF),
            AbsL => write!(f, "(${:x}).l", self.addr),
            PcDisp => write!(f, "${:x}(pc)", self.addr),
            PcIndex => write!(f, "${:x}(pc,{x})", self.addr),
            Imm => write!(f, "#${:x}", self.imm),
            Sr => f.write_str("sr"),
            Ccr => f.write_str("ccr"),
            Usp => f.write_str("usp"),
        }
    }
}
