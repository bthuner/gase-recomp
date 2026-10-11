"""
analysis.py — find the code inside the ROM.

A ROM is a mix of code and data (graphics, music, level maps, tables) with
nothing telling them apart. We use *recursive descent*: start from the
addresses we KNOW are code — the exception vectors — and follow every
branch and call. Whatever is reached that way is code; the rest is left
alone.

    function  = an entry point + every instruction reachable from it
                through branches (BRA, Bcc, DBcc, JMP to a static address)
    call      = BSR / JSR: its target becomes the entry of another function
    resume    = the address right after a call: where the function can be
                re-entered when the dispatcher resumes it (see gase.h)

What static analysis cannot see — `jmp (a0)`, `jsr (a1,d0.w)` — is the job
of the *coverage feedback loop*: the runtime logs unknown addresses to
gase_missing.txt, and we feed them back with `--entries`.

Two classic jump-table idioms are recognised here to reduce that work:

    jmp   table(pc,d0.w)        ; table = a list of BRA.W instructions
    move.w table(pc,d0.w),d0    ; table = a list of 16-bit offsets
    jmp   table(pc,d0.w)        ;   relative to the table itself
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .m68k import DecodeError, Instruction, decode
from .m68k.decoder import (FLOW_BRANCH, FLOW_CALL, FLOW_COND, FLOW_JUMP,
                           FLOW_NEXT, FLOW_RETURN, FLOW_STOP, FLOW_TRAP)
from .rom import VECTOR_NAMES, Rom

MAX_FUNCTION_INSTRUCTIONS = 20000
MAX_JUMP_TABLE = 256


@dataclass
class Function:
    entry: int
    name: str
    instructions: dict[int, Instruction] = field(default_factory=dict)
    resume_points: set[int] = field(default_factory=set)
    # Indirect jumps whose possible targets were recovered from a table.
    jump_tables: dict[int, list[int]] = field(default_factory=dict)
    bad_targets: set[int] = field(default_factory=set)   # failed to decode

    @property
    def size(self) -> int:
        return sum(i.length for i in self.instructions.values())


@dataclass
class Program:
    rom: Rom
    functions: dict[int, Function] = field(default_factory=dict)
    entry_names: dict[int, str] = field(default_factory=dict)
    rejected_entries: dict[int, str] = field(default_factory=dict)
    ram_vectors: dict[str, int] = field(default_factory=dict)

    @property
    def code_bytes(self) -> int:
        seen = {}
        for f in self.functions.values():
            for a, ins in f.instructions.items():
                seen[a] = ins.length
        return sum(seen.values())


class Analyzer:
    def __init__(self, rom: Rom):
        self.rom = rom
        self._cache: dict[int, Instruction | DecodeError] = {}

    # -- decoding with a cache (functions overlap, decode once) -----------
    def read16(self, addr: int) -> int:
        addr &= 0xFFFFFF
        if not self.rom.contains(addr, 2):
            raise IndexError(addr)
        return self.rom.u16(addr)

    def decode_at(self, addr: int) -> Instruction:
        hit = self._cache.get(addr)
        if hit is None:
            try:
                hit = decode(self.read16, addr)
            except DecodeError as e:
                hit = e
            self._cache[addr] = hit
        if isinstance(hit, DecodeError):
            raise hit
        return hit

    def in_rom(self, addr: int) -> bool:
        return addr % 2 == 0 and 0 < addr < len(self.rom.data)

    # -- entry points ---------------------------------------------------
    def vector_entries(self) -> tuple[dict[int, str], dict[str, int]]:
        """Code addresses named by the exception vector table ($000-$0FF)."""
        entries: dict[int, str] = {}
        ram: dict[str, int] = {}
        for num, target in enumerate(self.rom.vectors):
            if num == 0:                       # initial SSP, not code
                continue
            name = VECTOR_NAMES.get(num, f"vector{num}")
            target &= 0xFFFFFF
            if self.in_rom(target) and target >= 0x100:
                entries.setdefault(target, name)
            elif target >= 0xE00000:
                # Some games point a vector into RAM, where they copy a
                # `jmp xxx.l` at boot. The runtime follows it (see cpu.c).
                ram[name] = target
        return entries, ram

    # -- the main loop --------------------------------------------------
    def run(self, extra_entries: dict[int, str] | None = None) -> Program:
        program = Program(rom=self.rom)
        entries, program.ram_vectors = self.vector_entries()
        for addr, name in (extra_entries or {}).items():
            entries.setdefault(addr & 0xFFFFFF, name)
        worklist = list(entries.items())
        while worklist:
            addr, name = worklist.pop()
            if addr in program.functions or addr in program.rejected_entries:
                continue
            if not self.in_rom(addr):
                program.rejected_entries[addr] = "outside ROM"
                continue
            try:
                self.decode_at(addr)
            except (DecodeError, IndexError) as e:
                program.rejected_entries[addr] = str(e)
                continue
            fn = self.explore(addr, name)
            program.functions[addr] = fn
            program.entry_names[addr] = name
            for ins in fn.instructions.values():
                if ins.flow == FLOW_CALL and ins.target is not None:
                    t = ins.target & 0xFFFFFF
                    if t not in program.functions:
                        worklist.append((t, f"sub_{t:06X}"))
        return program

    def explore(self, entry: int, name: str) -> Function:
        """Collect every instruction reachable from `entry` without calls."""
        fn = Function(entry=entry, name=name)
        todo = [entry]
        while todo:
            addr = todo.pop()
            if addr in fn.instructions or addr in fn.bad_targets:
                continue
            if len(fn.instructions) >= MAX_FUNCTION_INSTRUCTIONS:
                break
            try:
                ins = self.decode_at(addr)
            except (DecodeError, IndexError):
                fn.bad_targets.add(addr)
                continue
            fn.instructions[addr] = ins
            flow = ins.flow
            if flow in (FLOW_NEXT, FLOW_TRAP, FLOW_CALL, FLOW_COND):
                todo.append(ins.next_addr)
            if flow == FLOW_CALL:
                fn.resume_points.add(ins.next_addr)
            if flow in (FLOW_BRANCH, FLOW_COND) and ins.target is not None:
                todo.append(ins.target & 0xFFFFFF)
            if flow == FLOW_JUMP:
                targets = self.jump_table(fn, ins)
                if targets:
                    fn.jump_tables[addr] = targets
                    todo.extend(targets)
            # FLOW_RETURN / FLOW_STOP: this path ends here.
        return fn

    # -- jump-table heuristics -------------------------------------------
    def jump_table(self, fn: Function, ins: Instruction) -> list[int]:
        op = ins.dst
        if op is None or op.mode != "pcindex":
            return []
        base = op.addr
        prev = fn.instructions.get(ins.addr - 4)
        if (prev is not None and prev.mnemonic == "move" and prev.size == 2
                and prev.src is not None and prev.src.mode == "pcindex"
                and prev.src.addr == base and prev.dst is not None
                and prev.dst.mode == "dn" and prev.dst.reg == op.xreg):
            return self._offset_table(base)
        return self._bra_table(base)

    def _bra_table(self, base: int) -> list[int]:
        """`jmp table(pc,dN)` followed by BRA.W instructions (4 bytes each)."""
        targets = []
        addr = base
        for _ in range(MAX_JUMP_TABLE):
            try:
                ins = self.decode_at(addr)
            except (DecodeError, IndexError):
                break
            if ins.mnemonic not in ("bra", "jmp") or ins.flow != FLOW_BRANCH:
                break
            targets.append(addr)
            addr += ins.length
        return targets

    def _offset_table(self, base: int) -> list[int]:
        """Signed 16-bit offsets from `base`. The table ends where code begins."""
        targets: list[int] = []
        end = base + 2 * MAX_JUMP_TABLE
        pos = base
        while pos < end and self.rom.contains(pos, 2):
            off = self.rom.u16(pos)
            off = off - 0x10000 if off & 0x8000 else off
            t = base + off
            if not self.in_rom(t) or off == 0:
                break
            try:
                self.decode_at(t)
            except (DecodeError, IndexError):
                break
            targets.append(t)
            if base < t < end:
                end = t                         # code starts here: stop
            pos += 2
        return sorted(set(targets))


def analyze(rom: Rom, extra_entries: dict[int, str] | None = None) -> Program:
    return Analyzer(rom).run(extra_entries)


__all__ = ["Analyzer", "Function", "Program", "analyze",
           "FLOW_RETURN", "FLOW_STOP"]
