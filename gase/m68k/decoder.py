"""
decoder.py — turn 68000 machine code into Instruction objects.

The 68000 has 16-bit opcodes. The top 4 bits split the instruction set
into 16 "lines" (that is how Motorola's manual organises it too):

    0  bit ops / MOVEP / immediate ops     8  OR / DIV / SBCD
    1  MOVE.B                              9  SUB / SUBA / SUBX
    2  MOVE.L                              A  (line-A trap, unused on Genesis)
    3  MOVE.W                              B  CMP / CMPA / CMPM / EOR
    4  miscellaneous                       C  AND / MUL / ABCD / EXG
    5  ADDQ / SUBQ / Scc / DBcc            D  ADD / ADDA / ADDX
    6  Bcc / BRA / BSR                     E  shifts and rotates
    7  MOVEQ                               F  (line-F trap, coprocessor)

After the opcode come 0-4 extension words (immediates, displacements,
absolute addresses), first for the source operand then for the
destination. `decode(read16, addr)` reads them through `read16` so the
decoder works on a ROM, a bytes object, or anything else.

The decoder is strict: an encoding that the 68000 would reject (e.g.
MOVE to an immediate, or a 68020-only instruction) raises DecodeError.
That strictness is our first line of defence against decoding data as
if it were code.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable

from .operands import (ALL, ALTERABLE, CONTROL, CONTROL_ALT, DATA, DATA_ALT,
                       MEM_ALT, MEMORY, Operand, areg, dreg, imm, sext8, sext16)

CONDITIONS = ["t", "f", "hi", "ls", "cc", "cs", "ne", "eq",
              "vc", "vs", "pl", "mi", "ge", "lt", "gt", "le"]
SIZE_SUFFIX = {0: "", 1: ".b", 2: ".w", 4: ".l"}

# How each instruction affects control flow. The analyser relies on this.
FLOW_NEXT = "next"        # continue with the following instruction
FLOW_BRANCH = "branch"    # unconditional jump to a static target (BRA, JMP abs)
FLOW_COND = "cond"        # maybe jump to target, else fall through (Bcc, DBcc)
FLOW_CALL = "call"        # call target (static or not), then come back
FLOW_JUMP = "jump"        # jump to a target only known at runtime
FLOW_RETURN = "return"    # RTS / RTE / RTR
FLOW_TRAP = "trap"        # raises an exception, then comes back (TRAP, CHK...)
FLOW_STOP = "stop"        # code ends here (ILLEGAL...)


class DecodeError(Exception):
    pass


@dataclass
class Instruction:
    addr: int
    opcode: int
    length: int = 2
    mnemonic: str = "?"
    size: int = 0                  # operand size in bytes: 1, 2, 4 (0 = n/a)
    src: Operand | None = None
    dst: Operand | None = None
    cond: int | None = None        # condition code for Bcc / DBcc / Scc
    target: int | None = None      # static branch / call target
    data: int | None = None        # MOVEM mask, TRAP number, STOP value...
    flow: str = FLOW_NEXT
    cycles: int = 4                # rough 68000 cycle estimate

    @property
    def next_addr(self) -> int:
        return self.addr + self.length

    def text(self) -> str:
        """Motorola-style disassembly."""
        name = self.mnemonic.split("_")[0]      # move_to_sr -> move, andi_ccr -> andi
        if self.cond is not None:
            name = name.replace("cc", CONDITIONS[self.cond])
            if name == "dbf":
                name = "dbra"                   # the usual spelling
        if self.mnemonic not in _NO_SUFFIX:
            name += SIZE_SUFFIX.get(self.size, "")
        ops = []
        if self.mnemonic == "movem":
            regs = _reglist(self.data, self.dst is not None and self.dst.mode == "predec")
            ops = [regs, str(self.dst)] if self.dst is not None else [str(self.src), regs]
        else:
            if self.src is not None:
                ops.append(str(self.src))
            if self.dst is not None:
                ops.append(str(self.dst))
            if self.target is not None:
                ops.append(f"${self.target:x}")
            if self.mnemonic in ("trap", "stop") and self.data is not None:
                ops.append(f"#{self.data}" if self.mnemonic == "trap" else f"#${self.data:04x}")
        return f"{name} {','.join(ops)}".strip()


_NO_SUFFIX = {"bra", "bsr", "bcc", "dbcc", "scc", "jmp", "jsr", "lea", "pea",
              "rts", "rte", "rtr", "nop", "reset", "stop", "trap", "trapv",
              "illegal", "link", "unlk", "exg", "swap", "linea", "linef",
              "move_usp", "andi_ccr", "ori_ccr", "eori_ccr", "andi_sr", "ori_sr",
              "eori_sr", "move_to_ccr", "move_to_sr", "move_from_sr", "moveq"}


def _reglist(mask: int, reversed_mask: bool) -> str:
    """MOVEM register mask: bit 0 = D0 .. bit 15 = A7 (reversed for -(An))."""
    if reversed_mask:
        mask = int(f"{mask:016b}"[::-1], 2)
    names = [f"d{i}" for i in range(8)] + [f"a{i}" for i in range(8)]
    return "/".join(n for i, n in enumerate(names) if mask >> i & 1) or "0"


class _Reader:
    """Sequential reader of extension words following the opcode."""

    def __init__(self, read16: Callable[[int], int], addr: int):
        self.read16 = read16
        self.pos = addr

    def word(self) -> int:
        try:
            w = self.read16(self.pos)
        except (IndexError, ValueError) as e:
            raise DecodeError("instruction runs past the end of memory") from e
        self.pos += 2
        return w

    def long(self) -> int:
        hi = self.word()
        return (hi << 16) | self.word()


def _ea(r: _Reader, mode: int, reg: int, size: int, allowed: frozenset) -> Operand:
    """Decode one effective address, consuming its extension words."""
    if mode == 0:
        op = dreg(reg)
    elif mode == 1:
        op = areg(reg)
    elif mode == 2:
        op = Operand("ind", reg)
    elif mode == 3:
        op = Operand("postinc", reg)
    elif mode == 4:
        op = Operand("predec", reg)
    elif mode == 5:
        op = Operand("disp", reg, disp=sext16(r.word()))
    elif mode == 6:
        op = _brief(r, Operand("index", reg))
    elif reg == 0:
        op = Operand("absw", addr=sext16(r.word()) & 0xFFFFFFFF)
    elif reg == 1:
        op = Operand("absl", addr=r.long())
    elif reg == 2:
        base = r.pos
        op = Operand("pcdisp", addr=(base + sext16(r.word())) & 0xFFFFFFFF)
    elif reg == 3:
        base = r.pos
        op = _brief(r, Operand("pcindex"))
        op = Operand("pcindex", addr=(base + op.disp) & 0xFFFFFFFF, disp=op.disp,
                     xreg=op.xreg, xa=op.xa, xlong=op.xlong)
    elif reg == 4:
        if size == 0:
            raise DecodeError("immediate operand without a size")
        v = r.long() if size == 4 else r.word()
        op = imm(v & 0xFF if size == 1 else v)
    else:
        raise DecodeError(f"invalid addressing mode 7/{reg}")
    if op.mode not in allowed:
        raise DecodeError(f"addressing mode {op.mode} not allowed here")
    if op.mode == "an" and size == 1:
        raise DecodeError("byte access to an address register")
    return op


def _brief(r: _Reader, op: Operand) -> Operand:
    ext = r.word()
    if ext & 0x0700:
        raise DecodeError("full extension word (68020+) not supported on 68000")
    return Operand(op.mode, op.reg, disp=sext8(ext & 0xFF),
                   xreg=(ext >> 12) & 7, xa=bool(ext & 0x8000), xlong=bool(ext & 0x0800))


def _size_bits(bits: int) -> int:
    """Standard size field at bits 7-6: 00=.B 01=.W 10=.L 11=invalid."""
    if bits == 3:
        raise DecodeError("invalid size field")
    return (1, 2, 4)[bits]


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

def decode(read16: Callable[[int], int], addr: int) -> Instruction:
    if addr & 1:
        raise DecodeError("odd instruction address")
    r = _Reader(read16, addr)
    op = r.word()
    ins = Instruction(addr=addr, opcode=op)
    _LINES[op >> 12](ins, op, r)
    ins.length = r.pos - addr
    ins.cycles = _estimate_cycles(ins)
    return ins


def _ea_bits(op: int) -> tuple[int, int]:
    return (op >> 3) & 7, op & 7


# --- line 0: immediate ops, bit ops, MOVEP --------------------------------

def _line0(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    if op & 0x0100:
        if mode == 1:                           # MOVEP
            opmode = (op >> 6) & 7
            ins.mnemonic = "movep"
            ins.size = 4 if opmode & 1 else 2
            mem = Operand("disp", reg, disp=sext16(r.word()))
            if opmode & 2:
                ins.src, ins.dst = dreg((op >> 9) & 7), mem
            else:
                ins.src, ins.dst = mem, dreg((op >> 9) & 7)
            return
        kind = (op >> 6) & 3                    # BTST/BCHG/BCLR/BSET Dn,<ea>
        ins.mnemonic = ("btst", "bchg", "bclr", "bset")[kind]
        allowed = DATA if kind == 0 else DATA_ALT
        ins.src = dreg((op >> 9) & 7)
        ins.dst = _ea(r, mode, reg, 1, allowed)
        ins.size = 4 if ins.dst.mode == "dn" else 1
        return

    kind = (op >> 9) & 7
    if kind == 4:                               # BTST/BCHG/BCLR/BSET #n,<ea>
        bkind = (op >> 6) & 3
        ins.mnemonic = ("btst", "bchg", "bclr", "bset")[bkind]
        bit = r.word()                          # upper byte ignored by the CPU
        ins.src = imm(bit & 0xFF)
        allowed = (DATA - {"imm"}) if bkind == 0 else DATA_ALT
        ins.dst = _ea(r, mode, reg, 1, allowed)
        ins.size = 4 if ins.dst.mode == "dn" else 1
        return
    names = {0: "ori", 1: "andi", 2: "subi", 3: "addi", 5: "eori", 6: "cmpi"}
    if kind not in names:
        raise DecodeError("unknown line-0 instruction")
    name = names[kind]
    if (op & 0x3F) == 0x3C and name in ("ori", "andi", "eori"):
        # ORI/ANDI/EORI to CCR (byte) or to SR (word, privileged)
        sbits = (op >> 6) & 3
        if sbits == 0:
            ins.mnemonic, ins.size = f"{name}_ccr", 1
            ins.src = imm(r.word() & 0xFF)
            ins.dst = Operand("ccr")
        elif sbits == 1:
            ins.mnemonic, ins.size = f"{name}_sr", 2
            ins.src = imm(r.word())
            ins.dst = Operand("sr")
        else:
            raise DecodeError("invalid size for immediate to CCR/SR")
        return
    ins.mnemonic = name
    ins.size = _size_bits((op >> 6) & 3)
    ins.src = _ea(r, 7, 4, ins.size, ALL)       # the immediate comes first
    ins.dst = _ea(r, mode, reg, ins.size, DATA_ALT)


# --- lines 1-3: MOVE / MOVEA ------------------------------------------------

def _move(ins: Instruction, op: int, r: _Reader) -> None:
    ins.size = {1: 1, 3: 2, 2: 4}[op >> 12]
    smode, sreg = _ea_bits(op)
    dreg_, dmode = (op >> 9) & 7, (op >> 6) & 7
    ins.src = _ea(r, smode, sreg, ins.size, ALL)
    if dmode == 1:
        if ins.size == 1:
            raise DecodeError("MOVEA.B does not exist")
        ins.mnemonic = "movea"
        ins.dst = areg(dreg_)
    else:
        ins.mnemonic = "move"
        ins.dst = _ea(r, dmode, dreg_, ins.size, DATA_ALT)


# --- line 4: miscellaneous ---------------------------------------------------

_SIMPLE4 = {
    0x4AFC: ("illegal", FLOW_STOP), 0x4E70: ("reset", FLOW_NEXT),
    0x4E71: ("nop", FLOW_NEXT), 0x4E73: ("rte", FLOW_RETURN),
    0x4E75: ("rts", FLOW_RETURN), 0x4E76: ("trapv", FLOW_TRAP),
    0x4E77: ("rtr", FLOW_RETURN),
}


def _line4(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    if op in _SIMPLE4:
        ins.mnemonic, ins.flow = _SIMPLE4[op]
        return
    if op == 0x4E72:
        ins.mnemonic, ins.data, ins.flow = "stop", r.word(), FLOW_NEXT
        return
    if op & 0xFFF0 == 0x4E40:
        ins.mnemonic, ins.data, ins.flow = "trap", op & 15, FLOW_TRAP
        return
    if op & 0xFFF8 == 0x4E50:
        ins.mnemonic, ins.size = "link", 2
        ins.src, ins.dst = areg(reg), imm(sext16(r.word()) & 0xFFFFFFFF)
        return
    if op & 0xFFF8 == 0x4E58:
        ins.mnemonic, ins.dst = "unlk", areg(reg)
        return
    if op & 0xFFF0 == 0x4E60:
        ins.mnemonic, ins.size = "move_usp", 4
        if op & 8:
            ins.src, ins.dst = Operand("usp"), areg(reg)
        else:
            ins.src, ins.dst = areg(reg), Operand("usp")
        return
    if op & 0xFFC0 in (0x4E80, 0x4EC0):         # JSR / JMP
        is_jsr = op & 0xFFC0 == 0x4E80
        ins.mnemonic = "jsr" if is_jsr else "jmp"
        ins.dst = _ea(r, mode, reg, 4, CONTROL)
        static = ins.dst.static_address()
        ins.target = static
        if is_jsr:
            ins.flow = FLOW_CALL
        else:
            ins.flow = FLOW_BRANCH if static is not None else FLOW_JUMP
        if static is not None:
            ins.dst = None
        return
    if op & 0xFFB8 == 0x4880:                   # EXT.W / EXT.L
        ins.mnemonic, ins.size = "ext", 4 if op & 0x40 else 2
        ins.dst = dreg(reg)
        return
    if op & 0xFB80 == 0x4880:                   # MOVEM
        ins.mnemonic = "movem"
        ins.size = 4 if op & 0x40 else 2
        ins.data = r.word()
        if op & 0x0400:                         # memory -> registers
            ins.src = _ea(r, mode, reg, ins.size, CONTROL | {"postinc"})
        else:                                   # registers -> memory
            ins.dst = _ea(r, mode, reg, ins.size, CONTROL_ALT | {"predec"})
        return
    if op & 0xF1C0 == 0x41C0:
        ins.mnemonic, ins.size = "lea", 4
        ins.src, ins.dst = _ea(r, mode, reg, 4, CONTROL), areg((op >> 9) & 7)
        return
    if op & 0xF1C0 == 0x4180:
        ins.mnemonic, ins.size, ins.flow = "chk", 2, FLOW_TRAP
        ins.src, ins.dst = _ea(r, mode, reg, 2, DATA), dreg((op >> 9) & 7)
        return
    top = op & 0xFFC0
    if top == 0x40C0:
        ins.mnemonic, ins.size = "move_from_sr", 2
        ins.src, ins.dst = Operand("sr"), _ea(r, mode, reg, 2, DATA_ALT)
        return
    if top == 0x44C0:
        ins.mnemonic, ins.size = "move_to_ccr", 2
        ins.src, ins.dst = _ea(r, mode, reg, 2, DATA), Operand("ccr")
        return
    if top == 0x46C0:
        ins.mnemonic, ins.size = "move_to_sr", 2
        ins.src, ins.dst = _ea(r, mode, reg, 2, DATA), Operand("sr")
        return
    if top == 0x4800:
        ins.mnemonic, ins.size = "nbcd", 1
        ins.dst = _ea(r, mode, reg, 1, DATA_ALT)
        return
    if top == 0x4840:
        if mode == 0:
            ins.mnemonic, ins.size, ins.dst = "swap", 4, dreg(reg)
        else:
            ins.mnemonic, ins.size = "pea", 4
            ins.src = _ea(r, mode, reg, 4, CONTROL)
        return
    if top == 0x4AC0:
        ins.mnemonic, ins.size = "tas", 1
        ins.dst = _ea(r, mode, reg, 1, DATA_ALT)
        return
    group = op & 0xFF00
    unary = {0x4000: "negx", 0x4200: "clr", 0x4400: "neg", 0x4600: "not", 0x4A00: "tst"}
    if group in unary:
        ins.mnemonic = unary[group]
        ins.size = _size_bits((op >> 6) & 3)
        ins.dst = _ea(r, mode, reg, ins.size, DATA_ALT)
        return
    raise DecodeError("unknown line-4 instruction")


# --- line 5: ADDQ / SUBQ / Scc / DBcc ------------------------------------

def _line5(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    if (op >> 6) & 3 == 3:
        ins.cond = (op >> 8) & 15
        if mode == 1:
            ins.mnemonic, ins.size, ins.flow = "dbcc", 2, FLOW_COND
            ins.dst = dreg(reg)
            base = r.pos
            ins.target = (base + sext16(r.word())) & 0xFFFFFFFF
        else:
            ins.mnemonic, ins.size = "scc", 1
            ins.dst = _ea(r, mode, reg, 1, DATA_ALT)
        return
    ins.mnemonic = "subq" if op & 0x100 else "addq"
    ins.size = _size_bits((op >> 6) & 3)
    ins.src = imm(((op >> 9) & 7) or 8)
    ins.dst = _ea(r, mode, reg, ins.size, ALTERABLE)


# --- line 6: Bcc / BRA / BSR ---------------------------------------------

def _line6(ins: Instruction, op: int, r: _Reader) -> None:
    cond = (op >> 8) & 15
    base = r.pos
    # A zero byte means "a 16-bit displacement follows". ($FF announces a
    # 32-bit one only on the 68020+: on a 68000 it is simply -1.)
    disp = op & 0xFF
    if disp == 0:
        disp = sext16(r.word())
    else:
        disp = sext8(disp)
    ins.target = (base + disp) & 0xFFFFFFFF
    if cond == 0:
        ins.mnemonic, ins.flow = "bra", FLOW_BRANCH
    elif cond == 1:
        ins.mnemonic, ins.flow = "bsr", FLOW_CALL
    else:
        ins.mnemonic, ins.cond, ins.flow = "bcc", cond, FLOW_COND


def _line7(ins: Instruction, op: int, r: _Reader) -> None:
    if op & 0x100:
        raise DecodeError("MOVEQ with bit 8 set")
    ins.mnemonic, ins.size = "moveq", 4
    ins.src = imm(sext8(op & 0xFF) & 0xFFFFFFFF)
    ins.dst = dreg((op >> 9) & 7)


# --- lines 8, 9, B, C, D: the two-operand arithmetic/logic family ---------

def _x_form(ins: Instruction, op: int, name: str) -> None:
    """ADDX/SUBX/ABCD/SBCD: Dy,Dx or -(Ay),-(Ax)."""
    ins.mnemonic = name
    rx, ry = (op >> 9) & 7, op & 7
    if op & 8:
        ins.src, ins.dst = Operand("predec", ry), Operand("predec", rx)
    else:
        ins.src, ins.dst = dreg(ry), dreg(rx)


def _arith(ins: Instruction, op: int, r: _Reader, name: str,
           src_allowed: frozenset, dst_allowed: frozenset) -> None:
    """Common shape: opmode 0-2 = <ea>,Dn ; opmode 4-6 = Dn,<ea>."""
    mode, reg = _ea_bits(op)
    opmode = (op >> 6) & 7
    ins.mnemonic = name
    ins.size = _size_bits(opmode & 3)
    dn = dreg((op >> 9) & 7)
    if opmode < 4:
        ins.src, ins.dst = _ea(r, mode, reg, ins.size, src_allowed), dn
    else:
        ins.src, ins.dst = dn, _ea(r, mode, reg, ins.size, dst_allowed)


def _line8(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    opmode = (op >> 6) & 7
    if opmode in (3, 7):
        ins.mnemonic = "divu" if opmode == 3 else "divs"
        ins.size, ins.flow = 2, FLOW_TRAP       # may raise "divide by zero"
        ins.src, ins.dst = _ea(r, mode, reg, 2, DATA), dreg((op >> 9) & 7)
        return
    if op & 0x1F0 == 0x100:
        ins.size = 1
        return _x_form(ins, op, "sbcd")
    _arith(ins, op, r, "or", DATA, MEM_ALT)


def _add_sub(ins: Instruction, op: int, r: _Reader, name: str) -> None:
    mode, reg = _ea_bits(op)
    opmode = (op >> 6) & 7
    if opmode in (3, 7):
        ins.mnemonic = name + "a"
        ins.size = 2 if opmode == 3 else 4
        ins.src, ins.dst = _ea(r, mode, reg, ins.size, ALL), areg((op >> 9) & 7)
        return
    if opmode >= 4 and mode in (0, 1):
        ins.size = _size_bits(opmode & 3)
        return _x_form(ins, op, name + "x")
    _arith(ins, op, r, name, ALL, MEM_ALT)


def _line9(ins: Instruction, op: int, r: _Reader) -> None:
    _add_sub(ins, op, r, "sub")


def _lineD(ins: Instruction, op: int, r: _Reader) -> None:
    _add_sub(ins, op, r, "add")


def _lineB(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    opmode = (op >> 6) & 7
    if opmode in (3, 7):
        ins.mnemonic = "cmpa"
        ins.size = 2 if opmode == 3 else 4
        ins.src, ins.dst = _ea(r, mode, reg, ins.size, ALL), areg((op >> 9) & 7)
        return
    if opmode < 3:
        return _arith(ins, op, r, "cmp", ALL, ALL)
    ins.size = _size_bits(opmode & 3)
    if mode == 1:
        ins.mnemonic = "cmpm"
        ins.src, ins.dst = Operand("postinc", reg), Operand("postinc", (op >> 9) & 7)
        return
    ins.mnemonic = "eor"
    ins.src, ins.dst = dreg((op >> 9) & 7), _ea(r, mode, reg, ins.size, DATA_ALT)


def _lineC(ins: Instruction, op: int, r: _Reader) -> None:
    mode, reg = _ea_bits(op)
    opmode = (op >> 6) & 7
    if opmode in (3, 7):
        ins.mnemonic, ins.size = ("mulu" if opmode == 3 else "muls"), 2
        ins.src, ins.dst = _ea(r, mode, reg, 2, DATA), dreg((op >> 9) & 7)
        return
    if op & 0x1F0 == 0x100:
        ins.size = 1
        return _x_form(ins, op, "abcd")
    exg = op & 0x1F8
    if exg in (0x140, 0x148, 0x188):
        ins.mnemonic, ins.size = "exg", 4
        rx, ry = (op >> 9) & 7, reg
        if exg == 0x140:
            ins.src, ins.dst = dreg(rx), dreg(ry)
        elif exg == 0x148:
            ins.src, ins.dst = areg(rx), areg(ry)
        else:
            ins.src, ins.dst = dreg(rx), areg(ry)
        return
    _arith(ins, op, r, "and", DATA, MEM_ALT)


# --- line E: shifts and rotates -------------------------------------------

_SHIFT_NAMES = ("as", "ls", "rox", "ro")


def _lineE(ins: Instruction, op: int, r: _Reader) -> None:
    direction = "l" if op & 0x100 else "r"
    if (op >> 6) & 3 == 3:                      # memory form: shift by one
        if op & 0x0800:
            raise DecodeError("bit-field instructions are 68020+")
        mode, reg = _ea_bits(op)
        ins.mnemonic = _SHIFT_NAMES[(op >> 9) & 3] + direction
        ins.size = 2
        ins.src = imm(1)
        ins.dst = _ea(r, mode, reg, 2, MEM_ALT)
        return
    ins.mnemonic = _SHIFT_NAMES[(op >> 3) & 3] + direction
    ins.size = _size_bits((op >> 6) & 3)
    count = (op >> 9) & 7
    ins.src = dreg(count) if op & 0x20 else imm(count or 8)
    ins.dst = dreg(op & 7)


def _lineA(ins: Instruction, op: int, r: _Reader) -> None:
    ins.mnemonic, ins.flow = "linea", FLOW_TRAP


def _lineF(ins: Instruction, op: int, r: _Reader) -> None:
    ins.mnemonic, ins.flow = "linef", FLOW_TRAP


_LINES = [_line0, _move, _move, _move, _line4, _line5, _line6, _line7,
          _line8, _line9, _lineA, _lineB, _lineC, _lineD, _lineE, _lineF]


# ---------------------------------------------------------------------------
# Rough cycle estimate. Real 68000 timings depend on the addressing mode and
# on wait states; we only need "about right" so that the number of
# instructions executed per scanline (and thus per frame) is plausible.
# ---------------------------------------------------------------------------

_FIXED_CYCLES = {"mulu": 54, "muls": 54, "divu": 108, "divs": 122, "rts": 16,
                 "rte": 20, "rtr": 20, "jsr": 18, "bsr": 18, "trap": 34,
                 "bra": 10, "bcc": 10, "dbcc": 10, "jmp": 10, "link": 16,
                 "unlk": 12, "stop": 4}


def _estimate_cycles(ins: Instruction) -> int:
    if ins.mnemonic in _FIXED_CYCLES:
        return _FIXED_CYCLES[ins.mnemonic]
    cycles = 2 * ins.length                     # 4 cycles per extra word, approx.
    for opnd in (ins.src, ins.dst):
        if opnd is not None and opnd.is_memory:
            cycles += 8 if ins.size == 4 else 4
    if ins.mnemonic == "movem":
        cycles += bin(ins.data or 0).count("1") * (8 if ins.size == 4 else 4)
    if ins.mnemonic[:2] in ("as", "ls", "ro"):
        cycles += 6
    return max(4, cycles + 2)
