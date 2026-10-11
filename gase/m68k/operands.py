"""
operands.py — the 68000 "effective address" (EA) modes.

Almost every 68000 instruction encodes its operands as a 6-bit field:
3 bits of MODE and 3 bits of REGISTER. Mode 7 uses the register field
as a sub-mode. Some modes need extra 16-bit "extension words" that
follow the opcode in memory.

    mode reg  syntax          meaning                          ext words
    0    n    Dn              data register                    0
    1    n    An              address register                 0
    2    n    (An)            memory at An                     0
    3    n    (An)+           memory at An, then An += size    0
    4    n    -(An)           An -= size, then memory at An    0
    5    n    d16(An)         memory at An + d16               1
    6    n    d8(An,Xn)       memory at An + Xn + d8           1 (brief)
    7    0    $xxxx.w         absolute short (sign-extended)   1
    7    1    $xxxxxxxx.l     absolute long                    2
    7    2    d16(PC)         PC-relative                      1
    7    3    d8(PC,Xn)       PC-relative with index           1 (brief)
    7    4    #imm            immediate                        1 or 2

The "brief extension word" used by the indexed modes is:
    bit 15     index is An (1) or Dn (0)
    bits 14-12 index register number
    bit 11     index is .L (1) or sign-extended .W (0)
    bits 7-0   signed 8-bit displacement
"""

from __future__ import annotations

from dataclasses import dataclass

# Addressing-mode categories, as named in the Motorola manual. Each
# instruction only accepts some of them; enforcing this is what keeps the
# decoder from happily "decoding" random data as code.
ALL = frozenset({"dn", "an", "ind", "postinc", "predec", "disp", "index",
                 "absw", "absl", "pcdisp", "pcindex", "imm"})
DATA = ALL - {"an"}
MEMORY = ALL - {"dn", "an"}
CONTROL = frozenset({"ind", "disp", "index", "absw", "absl", "pcdisp", "pcindex"})
ALTERABLE = ALL - {"pcdisp", "pcindex", "imm"}
DATA_ALT = ALTERABLE - {"an"}
MEM_ALT = ALTERABLE - {"dn", "an"}
CONTROL_ALT = CONTROL - {"pcdisp", "pcindex"}


def sext8(v: int) -> int:
    return v - 0x100 if v & 0x80 else v


def sext16(v: int) -> int:
    return v - 0x10000 if v & 0x8000 else v


@dataclass(frozen=True)
class Operand:
    mode: str
    reg: int = 0
    # disp: d16 / d8 displacement.  For "absw"/"absl" and the PC-relative
    # modes we pre-compute the final static address into `addr`.
    disp: int = 0
    addr: int = 0
    imm: int = 0
    xreg: int = 0
    xa: bool = False
    xlong: bool = False

    @property
    def is_memory(self) -> bool:
        return self.mode not in ("dn", "an", "imm")

    def static_address(self) -> int | None:
        """Address known at recompile time (absolute and d16(PC) modes)."""
        if self.mode in ("absw", "absl", "pcdisp"):
            return self.addr
        return None

    def __str__(self) -> str:
        x = f"{'a' if self.xa else 'd'}{self.xreg}.{'l' if self.xlong else 'w'}"
        m = self.mode
        if m == "dn":
            return f"d{self.reg}"
        if m == "an":
            return "sp" if self.reg == 7 else f"a{self.reg}"
        an = "sp" if self.reg == 7 else f"a{self.reg}"
        if m == "ind":
            return f"({an})"
        if m == "postinc":
            return f"({an})+"
        if m == "predec":
            return f"-({an})"
        if m == "disp":
            return f"{self.disp}({an})"
        if m == "index":
            return f"{self.disp}({an},{x})"
        if m == "absw":
            return f"(${self.addr & 0xFFFFFF:x}).w"
        if m == "absl":
            return f"(${self.addr:x}).l"
        if m == "pcdisp":
            return f"${self.addr:x}(pc)"
        if m == "pcindex":
            return f"${self.addr:x}(pc,{x})"
        if m == "imm":
            return f"#${self.imm:x}"
        return m                                # "sr", "ccr", "usp"


def dreg(n: int) -> Operand:
    return Operand("dn", n)


def areg(n: int) -> Operand:
    return Operand("an", n)


def imm(v: int) -> Operand:
    return Operand("imm", imm=v)
