"""
codegen.py — translate analysed 68000 functions into C.

Each 68000 instruction becomes a small, self-contained C block:

    /* 000204  3280          move.w d0,(a0) */
    CYC(8);
    {
        uint32_t s = D(0) & 0xFFFF;
        W16(A(0), s);
        op_logic(s, 2);
    }

The C compiler then does the heavy lifting: register allocation, constant
folding, dead flag elimination (a flag written and overwritten before
anyone reads it costs nothing after optimisation).

Control flow inside a function uses labels and `goto`; calls and
returns go through the macros of runtime/include/gase/gase.h.
"""

from __future__ import annotations

from dataclasses import dataclass

from .analysis import Function, Program
from .m68k import CONDITIONS, Instruction, Operand

MASK = {1: "0xFFu", 2: "0xFFFFu", 4: "0xFFFFFFFFu"}


def fn_name(addr: int) -> str:
    return f"f_{addr:06X}"


def label(addr: int) -> str:
    return f"L_{addr:06X}"


def hexa(v: int) -> str:
    return f"0x{v & 0xFFFFFFFF:X}u"


def cond(cc: int) -> str:
    return f"COND_{CONDITIONS[cc].upper()}"


class CodegenError(Exception):
    pass


# ---------------------------------------------------------------------------
# Effective addresses: how to read / write an operand in C
# ---------------------------------------------------------------------------

@dataclass
class EA:
    """An operand being accessed. `setup()` must run before read/write: it
    computes the address once (and applies (An)+ / -(An) side effects)."""
    op: Operand
    size: int
    tag: str                      # unique-ish C variable suffix: "s" or "d"

    def _step(self) -> int:
        # A byte push/pop on A7 moves by 2 to keep the stack word-aligned.
        if self.size == 1 and self.op.reg == 7:
            return 2
        return self.size

    def address(self) -> str:
        """Address expression for memory modes (no side effects)."""
        op = self.op
        m = op.mode
        if m == "ind":
            return f"A({op.reg})"
        if m == "disp":
            return _plus(f"A({op.reg})", op.disp)
        if m == "index":
            return _plus(f"(A({op.reg}) + {_index(op)})", op.disp)
        if m in ("absw", "absl", "pcdisp"):
            return hexa(op.addr)
        if m == "pcindex":
            return f"({hexa(op.addr)} + {_index(op)})"
        raise CodegenError(f"no address for mode {m}")

    def setup(self) -> list[str]:
        op, var = self.op, f"ea_{self.tag}"
        m = op.mode
        if m == "postinc":
            return [f"uint32_t {var} = A({op.reg});", f"A({op.reg}) += {self._step()};"]
        if m == "predec":
            return [f"A({op.reg}) -= {self._step()};", f"uint32_t {var} = A({op.reg});"]
        if m in ("ind", "disp", "index", "absw", "absl", "pcdisp", "pcindex"):
            return [f"uint32_t {var} = {self.address()};"]
        return []

    def read(self) -> str:
        op, sz = self.op, self.size
        if op.mode == "dn":
            return f"D({op.reg})" if sz == 4 else f"(D({op.reg}) & {MASK[sz]})"
        if op.mode == "an":
            return f"A({op.reg})" if sz == 4 else f"(A({op.reg}) & {MASK[sz]})"
        if op.mode == "imm":
            return hexa(op.imm & 0xFFFFFFFF if sz == 4 else op.imm & int(MASK[sz][:-1], 16))
        return f"R{sz * 8}(ea_{self.tag})"

    def write(self, value: str) -> str:
        op, sz = self.op, self.size
        if op.mode == "dn":
            return {1: f"SETD8({op.reg}, {value});", 2: f"SETD16({op.reg}, {value});",
                    4: f"D({op.reg}) = {value};"}[sz]
        if op.mode == "an":
            return f"A({op.reg}) = {value};"
        if op.mode in ("imm", "pcdisp", "pcindex"):
            raise CodegenError("write to a read-only operand")
        return f"W{sz * 8}(ea_{self.tag}, {value});"


def _index(op: Operand) -> str:
    reg = f"{'A' if op.xa else 'D'}({op.xreg})"
    return reg if op.xlong else f"SEXT16({reg})"


def _plus(base: str, disp: int) -> str:
    if disp == 0:
        return base
    return f"({base} + {disp}u)" if disp > 0 else f"({base} - {-disp}u)"


# ---------------------------------------------------------------------------
# Function emitter
# ---------------------------------------------------------------------------

class FunctionEmitter:
    def __init__(self, program: Program, fn: Function):
        self.program = program
        self.fn = fn
        self.order = sorted(fn.instructions)
        self.labels: set[int] = set()
        self.out: list[str] = []

    # -- helpers --------------------------------------------------------
    def in_fn(self, addr: int) -> bool:
        return addr in self.fn.instructions

    def goto(self, target: int, here: int) -> list[str]:
        """Jump to a static target: a goto when we can, the dispatcher if not."""
        target &= 0xFFFFFF
        if not self.in_fn(target):
            return [f"JUMP({hexa(target)});"]
        lines = []
        if target <= here:
            lines.append(f"POLL({hexa(target)});")      # a loop: let time pass
        lines.append(f"goto {label(target)};")
        return lines

    def call(self, target: int, ret: int) -> str:
        target &= 0xFFFFFF
        if target in self.program.functions:
            return f"CALL({fn_name(target)}, {hexa(target)}, {hexa(ret)});"
        return f"CALL_INDIRECT({hexa(target)}, {hexa(ret)});"

    # -- the two passes --------------------------------------------------
    def collect_labels(self) -> None:
        f = self.fn
        self.labels.add(f.entry)
        self.labels |= {r for r in f.resume_points if self.in_fn(r)}
        for i, addr in enumerate(self.order):
            ins = f.instructions[addr]
            if ins.target is not None and ins.flow in ("branch", "cond"):
                if self.in_fn(ins.target & 0xFFFFFF):
                    self.labels.add(ins.target & 0xFFFFFF)
            for t in f.jump_tables.get(addr, []):
                if self.in_fn(t):
                    self.labels.add(t)
            if ins.flow in ("next", "cond", "call", "trap"):
                nxt = self.order[i + 1] if i + 1 < len(self.order) else None
                if nxt != ins.next_addr and self.in_fn(ins.next_addr):
                    self.labels.add(ins.next_addr)

    def emit(self) -> str:
        self.collect_labels()
        f = self.fn
        o = self.out
        o.append(f"/* {f.name}: {len(f.instructions)} instructions, {f.size} bytes */")
        o.append(f"void {fn_name(f.entry)}(void)")
        o.append("{")
        resumes = sorted(r for r in f.resume_points if self.in_fn(r))
        if resumes:
            # Re-entry after a call, when the dispatcher resumes this function.
            o.append(f"    if (gase_cpu.pc != {hexa(f.entry)}) {{")
            o.append("        switch (gase_cpu.pc) {")
            for r in resumes:
                o.append(f"        case {hexa(r)}: goto {label(r)};")
            o.append(f'        default: gase_unresolved(gase_cpu.pc, "bad re-entry into {fn_name(f.entry)}"); return;')
            o.append("        }")
            o.append("    }")
        if self.order[0] != f.entry:
            # Some code of this function sits *below* its entry point.
            o.append(f"    goto {label(f.entry)};")
        for i, addr in enumerate(self.order):
            ins = f.instructions[addr]
            self.emit_instruction(ins)
            if ins.flow in ("next", "cond", "call", "trap"):
                nxt = self.order[i + 1] if i + 1 < len(self.order) else None
                if nxt != ins.next_addr:
                    for line in self.goto_fallthrough(ins.next_addr):
                        o.append("    " + line)
        o.append("}")
        return "\n".join(o)

    def goto_fallthrough(self, addr: int) -> list[str]:
        if self.in_fn(addr):
            return [f"goto {label(addr)};"]
        return [f"JUMP({hexa(addr)});   /* falls into undecodable code */"]

    def emit_instruction(self, ins: Instruction) -> None:
        raw = " ".join(f"{self.program.rom.u16(a):04X}"
                       for a in range(ins.addr, ins.addr + ins.length, 2)
                       if self.program.rom.contains(a))
        if ins.addr in self.labels:
            self.out.append(f"{label(ins.addr)}:")
        self.out.append(f"    /* {ins.addr:06X}  {raw:<20} {ins.text()} */")
        handler = getattr(self, "op_" + ins.mnemonic, None)
        if handler is None:
            raise CodegenError(f"no code generator for {ins.mnemonic}")
        body = handler(ins)
        self.out.append(f"    CYC({ins.cycles});")
        if len(body) == 1 and not body[0].startswith("uint"):
            self.out.append("    " + body[0])
        else:
            self.out.append("    {")
            self.out.extend("        " + line for line in body)
            self.out.append("    }")

    # =====================================================================
    # Instruction generators. Each returns a list of C statements.
    # =====================================================================

    def _src_dst(self, ins: Instruction):
        s = EA(ins.src, ins.size, "s")
        d = EA(ins.dst, ins.size, "d")
        return s, d

    # -- data movement -----------------------------------------------------
    def op_move(self, ins):
        s, d = self._src_dst(ins)
        return [*s.setup(), f"uint32_t v = {s.read()};", *d.setup(), d.write("v"),
                f"op_logic(v, {ins.size});"]

    def op_movea(self, ins):
        s = EA(ins.src, ins.size, "s")
        v = s.read() if ins.size == 4 else f"SEXT16({s.read()})"
        return [*s.setup(), f"A({ins.dst.reg}) = {v};"]

    def op_moveq(self, ins):
        return [f"D({ins.dst.reg}) = {hexa(ins.src.imm)};", f"op_logic({hexa(ins.src.imm)}, 4);"]

    def op_lea(self, ins):
        return [f"A({ins.dst.reg}) = {EA(ins.src, 4, 's').address()};"]

    def op_pea(self, ins):
        return [f"uint32_t v = {EA(ins.src, 4, 's').address()};", "PUSH32(v);"]

    def op_clr(self, ins):
        d = EA(ins.dst, ins.size, "d")
        return [*d.setup(), d.write("0"), f"op_logic(0, {ins.size});"]

    def op_exg(self, ins):
        a = f"{'D' if ins.src.mode == 'dn' else 'A'}({ins.src.reg})"
        b = f"{'D' if ins.dst.mode == 'dn' else 'A'}({ins.dst.reg})"
        return [f"uint32_t t = {a};", f"{a} = {b};", f"{b} = t;"]

    def op_swap(self, ins):
        r = ins.dst.reg
        return [f"D({r}) = (D({r}) >> 16) | (D({r}) << 16);", f"op_logic(D({r}), 4);"]

    def op_ext(self, ins):
        r = ins.dst.reg
        if ins.size == 2:
            return [f"uint32_t v = SEXT8(D({r}));", f"SETD16({r}, v);", "op_logic(v, 2);"]
        return [f"D({r}) = SEXT16(D({r}));", f"op_logic(D({r}), 4);"]

    def op_movem(self, ins):
        sz, mask = ins.size, ins.data
        regs = [f"D({i})" for i in range(8)] + [f"A({i})" for i in range(8)]
        lines = []
        if ins.dst is not None:                 # registers -> memory
            op = ins.dst
            if op.mode == "predec":
                # mask is reversed: bit 0 = A7 ... bit 15 = D0; stored downward.
                lines.append(f"uint32_t a = A({op.reg});")
                for bit in range(16):
                    if mask >> bit & 1:
                        lines.append(f"a -= {sz}; W{sz * 8}(a, {regs[15 - bit]});")
                lines.append(f"A({op.reg}) = a;")
            else:
                lines.append(f"uint32_t a = {EA(op, sz, 'd').address()};")
                for bit in range(16):
                    if mask >> bit & 1:
                        lines.append(f"W{sz * 8}(a, {regs[bit]}); a += {sz};")
            return lines
        op = ins.src                            # memory -> registers
        if op.mode == "postinc":
            lines.append(f"uint32_t a = A({op.reg});")
        else:
            lines.append(f"uint32_t a = {EA(op, sz, 's').address()};")
        for bit in range(16):
            if mask >> bit & 1:
                val = f"R32(a)" if sz == 4 else "SEXT16(R16(a))"
                lines.append(f"{regs[bit]} = {val}; a += {sz};")
        if op.mode == "postinc":
            lines.append(f"A({op.reg}) = a;")
        return lines

    def op_movep(self, ins):
        # MOVEP moves a register to every other byte (8-bit peripherals).
        n = ins.size
        if ins.src.mode == "dn":
            r, mem = ins.src.reg, ins.dst
            lines = [f"uint32_t a = {_plus(f'A({mem.reg})', mem.disp)};"]
            for k in range(n):
                shift = 8 * (n - 1 - k)
                lines.append(f"W8(a + {2 * k}u, D({r}) >> {shift});")
            return lines
        r, mem = ins.dst.reg, ins.src
        lines = [f"uint32_t a = {_plus(f'A({mem.reg})', mem.disp)};", "uint32_t v = 0;"]
        for k in range(n):
            lines.append(f"v = (v << 8) | R8(a + {2 * k}u);")
        lines.append(f"D({r}) = v;" if n == 4 else f"SETD16({r}, v);")
        return lines

    # -- arithmetic and logic ---------------------------------------------
    def _binary(self, ins, func: str, store: bool = True):
        s, d = self._src_dst(ins)
        lines = [*s.setup(), f"uint32_t s = {s.read()};", *d.setup(),
                 f"uint32_t d = {d.read()};"]
        if store:
            lines += [f"uint32_t r = {func}(d, s, {ins.size});", d.write("r")]
        else:
            lines.append(f"{func}(d, s, {ins.size});")
        return lines

    def _logic(self, ins, c_op: str):
        s, d = self._src_dst(ins)
        return [*s.setup(), f"uint32_t s = {s.read()};", *d.setup(),
                f"uint32_t r = ({d.read()} {c_op} s) & {MASK[ins.size]};", d.write("r"),
                f"op_logic(r, {ins.size});"]

    def op_add(self, ins):  return self._binary(ins, "op_add")
    def op_addi(self, ins): return self._binary(ins, "op_add")
    def op_sub(self, ins):  return self._binary(ins, "op_sub")
    def op_subi(self, ins): return self._binary(ins, "op_sub")
    def op_cmp(self, ins):  return self._binary(ins, "op_cmp", store=False)
    def op_cmpi(self, ins): return self._binary(ins, "op_cmp", store=False)
    def op_cmpm(self, ins): return self._binary(ins, "op_cmp", store=False)
    def op_addx(self, ins): return self._binary(ins, "op_addx")
    def op_subx(self, ins): return self._binary(ins, "op_subx")
    def op_and(self, ins):  return self._logic(ins, "&")
    def op_andi(self, ins): return self._logic(ins, "&")
    def op_or(self, ins):   return self._logic(ins, "|")
    def op_ori(self, ins):  return self._logic(ins, "|")
    def op_eor(self, ins):  return self._logic(ins, "^")
    def op_eori(self, ins): return self._logic(ins, "^")

    def _quick(self, ins, func: str, c_op: str):
        if ins.dst.mode == "an":                # ADDQ/SUBQ to An: no flags, 32-bit
            return [f"A({ins.dst.reg}) {c_op}= {ins.src.imm}u;"]
        return self._binary(ins, func)

    def op_addq(self, ins): return self._quick(ins, "op_add", "+")
    def op_subq(self, ins): return self._quick(ins, "op_sub", "-")

    def _addr_arith(self, ins, c_op: str):
        s = EA(ins.src, ins.size, "s")
        v = s.read() if ins.size == 4 else f"SEXT16({s.read()})"
        return [*s.setup(), f"A({ins.dst.reg}) {c_op}= {v};"]

    def op_adda(self, ins): return self._addr_arith(ins, "+")
    def op_suba(self, ins): return self._addr_arith(ins, "-")

    def op_cmpa(self, ins):
        s = EA(ins.src, ins.size, "s")
        v = s.read() if ins.size == 4 else f"SEXT16({s.read()})"
        return [*s.setup(), f"op_cmp(A({ins.dst.reg}), {v}, 4);"]

    def _unary(self, ins, expr: str, flags: str | None):
        d = EA(ins.dst, ins.size, "d")
        lines = [*d.setup(), f"uint32_t d = {d.read()};", f"uint32_t r = {expr};", d.write("r")]
        if flags:
            lines.append(flags)
        return lines

    def op_neg(self, ins):  return self._unary(ins, f"op_sub(0, d, {ins.size})", None)
    def op_negx(self, ins): return self._unary(ins, f"op_subx(0, d, {ins.size})", None)
    def op_not(self, ins):
        return self._unary(ins, f"~d & {MASK[ins.size]}", f"op_logic(r, {ins.size});")

    def op_tst(self, ins):
        d = EA(ins.dst, ins.size, "d")
        return [*d.setup(), f"op_logic({d.read()}, {ins.size});"]

    def op_tas(self, ins):
        d = EA(ins.dst, 1, "d")
        return [*d.setup(), f"uint32_t v = {d.read()};", "op_logic(v, 1);", d.write("v | 0x80u")]

    def op_scc(self, ins):
        d = EA(ins.dst, 1, "d")
        return [*d.setup(), d.write(f"{cond(ins.cond)} ? 0xFFu : 0u")]

    def _muldiv(self, ins, func: str, may_trap: bool):
        s = EA(ins.src, 2, "s")
        r = ins.dst.reg
        lines = [*s.setup(), f"uint32_t s = {s.read()};"]
        if may_trap:
            lines += ["if (s == 0) {",
                      f"    EXCEPTION(5, {hexa(ins.next_addr)});   /* divide by zero */",
                      "} else {",
                      f"    D({r}) = {func}(D({r}), s);",
                      "}"]
        else:
            lines.append(f"D({r}) = {func}(D({r}), s);")
        return lines

    def op_mulu(self, ins): return self._muldiv(ins, "op_mulu", False)
    def op_muls(self, ins): return self._muldiv(ins, "op_muls", False)
    def op_divu(self, ins): return self._muldiv(ins, "op_divu", True)
    def op_divs(self, ins): return self._muldiv(ins, "op_divs", True)

    def op_abcd(self, ins): return self._binary_bcd(ins, "op_abcd")
    def op_sbcd(self, ins): return self._binary_bcd(ins, "op_sbcd")

    def _binary_bcd(self, ins, func):
        s, d = self._src_dst(ins)
        return [*s.setup(), f"uint32_t s = {s.read()};", *d.setup(),
                f"uint32_t r = {func}({d.read()}, s);", d.write("r")]

    def op_nbcd(self, ins):
        d = EA(ins.dst, 1, "d")
        return [*d.setup(), f"uint32_t r = op_nbcd({d.read()});", d.write("r")]

    def op_chk(self, ins):
        s = EA(ins.src, 2, "s")
        r = ins.dst.reg
        nxt = hexa(ins.next_addr)
        return [*s.setup(), f"int16_t bound = (int16_t){s.read()};",
                f"int16_t v = (int16_t)D({r});",
                "gase_cpu.z = v == 0; gase_cpu.v = 0; gase_cpu.c = 0;",
                f"if (v < 0) {{ gase_cpu.n = 1; EXCEPTION(6, {nxt}); }}",
                f"else if (v > bound) {{ gase_cpu.n = 0; EXCEPTION(6, {nxt}); }}"]

    # -- shifts and rotates -----------------------------------------------
    def _shift(self, ins, func: str):
        count = f"(D({ins.src.reg}) & 63)" if ins.src.mode == "dn" else f"{ins.src.imm}u"
        d = EA(ins.dst, ins.size, "d")
        return [*d.setup(), f"uint32_t r = {func}({d.read()}, {count}, {ins.size});", d.write("r")]

    def op_asl(self, ins):  return self._shift(ins, "op_asl")
    def op_asr(self, ins):  return self._shift(ins, "op_asr")
    def op_lsl(self, ins):  return self._shift(ins, "op_lsl")
    def op_lsr(self, ins):  return self._shift(ins, "op_lsr")
    def op_rol(self, ins):  return self._shift(ins, "op_rol")
    def op_ror(self, ins):  return self._shift(ins, "op_ror")
    def op_roxl(self, ins): return self._shift(ins, "op_roxl")
    def op_roxr(self, ins): return self._shift(ins, "op_roxr")

    # -- single-bit operations ------------------------------------------------
    def _bit(self, ins, update: str | None):
        modulo = 31 if ins.dst.mode == "dn" else 7
        bit = (f"(D({ins.src.reg}) & {modulo})" if ins.src.mode == "dn"
               else f"{ins.src.imm & modulo}")
        d = EA(ins.dst, ins.size, "d")
        lines = [*d.setup(), f"uint32_t b = 1u << {bit};", f"uint32_t v = {d.read()};",
                 "gase_cpu.z = (v & b) == 0;"]
        if update:
            lines.append(d.write(update))
        return lines

    def op_btst(self, ins): return self._bit(ins, None)
    def op_bchg(self, ins): return self._bit(ins, "v ^ b")
    def op_bclr(self, ins): return self._bit(ins, "v & ~b")
    def op_bset(self, ins): return self._bit(ins, "v | b")

    # -- status register --------------------------------------------------
    def op_andi_ccr(self, ins): return [f"gase_set_ccr(gase_get_ccr() & {hexa(ins.src.imm)});"]
    def op_ori_ccr(self, ins):  return [f"gase_set_ccr(gase_get_ccr() | {hexa(ins.src.imm)});"]
    def op_eori_ccr(self, ins): return [f"gase_set_ccr(gase_get_ccr() ^ {hexa(ins.src.imm)});"]

    def _sr_changed(self, ins, stmt: str):
        # Lowering the interrupt mask can unmask a pending interrupt *now*.
        return [stmt, f"if (!gase_check_irq({hexa(ins.next_addr)})) return;"]

    def op_andi_sr(self, ins): return self._sr_changed(ins, f"gase_set_sr(gase_get_sr() & {hexa(ins.src.imm)});")
    def op_ori_sr(self, ins):  return self._sr_changed(ins, f"gase_set_sr(gase_get_sr() | {hexa(ins.src.imm)});")
    def op_eori_sr(self, ins): return self._sr_changed(ins, f"gase_set_sr(gase_get_sr() ^ {hexa(ins.src.imm)});")

    def op_move_to_sr(self, ins):
        s = EA(ins.src, 2, "s")
        return [*s.setup(), *self._sr_changed(ins, f"gase_set_sr({s.read()});")]

    def op_move_to_ccr(self, ins):
        s = EA(ins.src, 2, "s")
        return [*s.setup(), f"gase_set_ccr({s.read()} & 0xFFu);"]

    def op_move_from_sr(self, ins):
        d = EA(ins.dst, 2, "d")
        return [*d.setup(), d.write("gase_get_sr()")]

    def op_move_usp(self, ins):
        if ins.src.mode == "usp":
            return [f"A({ins.dst.reg}) = gase_cpu.usp;"]
        return [f"gase_cpu.usp = A({ins.src.reg});"]

    # -- stack frames ---------------------------------------------------------
    def op_link(self, ins):
        r = ins.src.reg
        return [f"PUSH32(A({r}));", f"A({r}) = A(7);", f"A(7) += {hexa(ins.dst.imm)};"]

    def op_unlk(self, ins):
        r = ins.dst.reg
        return ["A(7) = A({0});".format(r), f"A({r}) = POP32();"]

    # -- control flow ------------------------------------------------------
    def op_bra(self, ins):
        return self.goto(ins.target, ins.addr)

    def op_bcc(self, ins):
        return [f"if ({cond(ins.cond)}) {{", *("    " + x for x in self.goto(ins.target, ins.addr)), "}"]

    def op_dbcc(self, ins):
        r = ins.dst.reg
        return [f"if (!{cond(ins.cond)}) {{",
                f"    SETD16({r}, D({r}) - 1);",
                f"    if ((D({r}) & 0xFFFFu) != 0xFFFFu) {{",
                *("        " + x for x in self.goto(ins.target, ins.addr)),
                "    }", "}"]

    def op_bsr(self, ins):
        return [self.call(ins.target, ins.next_addr)]

    def op_jsr(self, ins):
        if ins.target is not None:
            return [self.call(ins.target, ins.next_addr)]
        d = EA(ins.dst, 4, "d")
        return [f"uint32_t t = {d.address()};", f"CALL_INDIRECT(t, {hexa(ins.next_addr)});"]

    def op_jmp(self, ins):
        if ins.target is not None:
            return self.goto(ins.target, ins.addr)
        d = EA(ins.dst, 4, "d")
        lines = [f"uint32_t t = {d.address()} & 0xFFFFFFu;"]
        table = [t for t in self.fn.jump_tables.get(ins.addr, []) if self.in_fn(t)]
        if table:
            # A recovered jump table: stay inside this C function.
            lines.append("switch (t) {")
            lines += [f"case {hexa(t)}: goto {label(t)};" for t in table]
            lines.append("default: break;")
            lines.append("}")
        lines.append("JUMP(t);")
        return lines

    def op_rts(self, ins):
        return ["gase_cpu.pc = POP32();", "return;"]

    def op_rtr(self, ins):
        return ["gase_set_ccr(POP16() & 0xFFu);", "gase_cpu.pc = POP32();", "return;"]

    def op_rte(self, ins):
        # Pop both words from the supervisor stack *before* restoring SR,
        # which may switch to the user stack.
        return ["uint32_t sr = POP16();", "gase_cpu.pc = POP32();", "gase_set_sr(sr);", "return;"]

    def op_trap(self, ins):
        return [f"EXCEPTION({32 + ins.data}, {hexa(ins.next_addr)});"]

    def op_trapv(self, ins):
        return [f"if (gase_cpu.v) EXCEPTION(7, {hexa(ins.next_addr)});"]

    def op_illegal(self, ins):
        return [f"gase_exception(4, {hexa(ins.addr)});", "return;"]

    def op_linea(self, ins):
        return [f"gase_exception(10, {hexa(ins.addr)});", "return;"]

    def op_linef(self, ins):
        return [f"gase_exception(11, {hexa(ins.addr)});", "return;"]

    def op_stop(self, ins):
        return [f"gase_set_sr({hexa(ins.data)});",
                f"if (!gase_stop({hexa(ins.next_addr)})) return;"]

    def op_nop(self, ins):
        return [";"]

    def op_reset(self, ins):
        return ["/* RESET pulses the reset line of external chips: nothing to do */"]


# ---------------------------------------------------------------------------
# Whole-program output
# ---------------------------------------------------------------------------

FILE_HEADER = """\
/*
 * Generated by gase-recomp from "{title}" — do not edit by hand.
 * Each function below is a 68000 routine translated to C.
 */
#include "gase/gase.h"
#include "recomp.h"
"""


def generate(program: Program, functions_per_file: int = 300) -> dict[str, str]:
    """Return {filename: C source} for the whole program."""
    title = program.rom.title.replace("*/", "")
    files: dict[str, str] = {}
    entries = sorted(program.functions)

    proto = ["/* Generated by gase-recomp — prototypes of all recompiled functions. */",
             "#ifndef RECOMP_H", "#define RECOMP_H", ""]
    proto += [f"void {fn_name(a)}(void);" for a in entries]
    proto += ["", "#endif", ""]
    files["recomp.h"] = "\n".join(proto)

    for chunk_no in range(0, max(1, len(entries)), functions_per_file):
        chunk = entries[chunk_no:chunk_no + functions_per_file]
        parts = [FILE_HEADER.format(title=title)]
        for a in chunk:
            parts.append(FunctionEmitter(program, program.functions[a]).emit())
            parts.append("")
        files[f"recomp_{chunk_no // functions_per_file:03d}.c"] = "\n".join(parts)

    # Dispatch table: every address where a C function can start running.
    rows: dict[int, str] = {}
    for a in entries:
        rows[a] = fn_name(a)
    for a in entries:
        for r in sorted(program.functions[a].resume_points):
            if r in program.functions[a].instructions:
                rows.setdefault(r, fn_name(a))
    table = ["/* Generated by gase-recomp — address -> C function, sorted. */",
             '#include "gase/gase.h"', '#include "recomp.h"', "",
             "const GaseEntry gase_entries[] = {"]
    table += [f"    {{ {hexa(a)}, {f} }}," for a, f in sorted(rows.items())]
    table += ["};", "const size_t gase_entry_count = sizeof gase_entries / sizeof gase_entries[0];", ""]
    files["recomp_table.c"] = "\n".join(table)

    safe = title.replace("\\", "\\\\").replace('"', '\\"')
    files["recomp_config.c"] = "\n".join([
        "/* Generated by gase-recomp — identity of the ROM this code was built from. */",
        '#include "gase/gase.h"', "",
        f'const char     gase_game_title[]       = "{safe}";',
        f"const uint32_t gase_rom_crc32          = {hexa(program.rom.crc32)};",
        f"const uint32_t gase_rom_expected_size  = {hexa(len(program.rom.data))};", ""])
    return files
