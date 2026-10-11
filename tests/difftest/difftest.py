"""
difftest.py — differential testing of the recompiler against Musashi.

    python tests/difftest/difftest.py [--cases 2000] [--seed 1] [--length 3]

1. Generate random, valid 68000 instructions with our own decoder.
2. Pack them into small snippets ending with RTS, inside a fake "ROM".
3. Recompile the snippets to C and build them with a tiny harness.
4. Run every snippet on BOTH the recompiled C and the Musashi reference
   emulator, from the same random initial state.
5. Compare registers, flags, stack and RAM. Any difference is a bug in
   the decoder, the code generator or the runtime helpers.

Exceptions are part of the test: TRAP, CHK, TRAPV and divide-by-zero
vector to a handler that just does RTE, so exception stack frames are
compared too.
"""

from __future__ import annotations

import argparse
import random
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from gase.analysis import analyze                    # noqa: E402
from gase.codegen import generate                    # noqa: E402
from gase.m68k import DecodeError, decode            # noqa: E402
from gase.rom import load_rom_bytes                  # noqa: E402

CODE_BASE = 0x1000
SLOT = 0x40
HANDLER = 0x800
SENTINEL = 0xF00
RAM = 0xFF0000

EXCLUDED = {"stop", "reset", "illegal", "linea", "linef", "move_to_sr",
            "andi_sr", "ori_sr", "eori_sr"}


def random_instruction(rng: random.Random) -> list[int]:
    while True:
        words = [rng.randrange(0x10000)] + [rng.randrange(0x10000) for _ in range(4)]
        # brief extension words must have bits 8-10 clear on a 68000
        for k in (1, 2, 3):
            if rng.random() < 0.5:
                words[k] &= 0xF8FF
        try:
            ins = decode(lambda a: words[a // 2], 0)
        except (DecodeError, IndexError):
            continue
        if ins.flow not in ("next", "trap") or ins.mnemonic in EXCLUDED:
            continue
        if any(op is not None and op.mode in ("absw", "absl") and (op.addr & 0xFFFFFF) < RAM
               for op in (ins.src, ins.dst)):
            continue                          # keep writes away from the code
        return words[:ins.length // 2]


def edge_value(rng: random.Random) -> int:
    kind = rng.random()
    if kind < 0.3:
        return rng.randrange(1 << 32)
    if kind < 0.6:
        return rng.randrange(0, 40)
    if kind < 0.8:
        return rng.choice([0, 1, 0x7F, 0x80, 0xFF, 0x7FFF, 0x8000, 0xFFFF,
                           0x7FFFFFFF, 0x80000000, 0xFFFFFFFF, 0x99, 0x09])
    return rng.randrange(1 << 16)


def build_image(rng: random.Random, ncases: int, length: int):
    low = bytearray(0x10000)
    struct.pack_into(">I", low, 0, 0xFFFF00)
    for v in range(1, 64):
        struct.pack_into(">I", low, 4 * v, HANDLER)
    struct.pack_into(">H", low, HANDLER, 0x4E73)          # RTE
    low[0x100:0x110] = b"SEGA DIFFTEST   "
    entries = []
    for i in range(ncases):
        addr = CODE_BASE + i * SLOT
        pos = addr
        for _ in range(rng.randint(1, length)):
            for w in random_instruction(rng):
                struct.pack_into(">H", low, pos, w)
                pos += 2
        struct.pack_into(">H", low, pos, 0x4E75)          # RTS
        entries.append(addr)
    assert CODE_BASE + ncases * SLOT <= 0x10000, "too many cases for the image"
    ram = bytes(rng.randrange(256) for _ in range(0x10000))
    return bytes(low), ram, entries


def make_states(rng: random.Random, entries: list[int]) -> list[list[int]]:
    states = []
    for e in entries:
        d = [edge_value(rng) for _ in range(8)]
        a = [RAM + (rng.randrange(0x1000, 0xF000) & ~1) for _ in range(7)] + [0xFFFF00]
        usp = RAM + 0x8000
        sr = 0x2700 | rng.randrange(32)
        states.append([e, *d, *a, usp, sr])
    return states


def build_harness(low: bytes, entries: list[int], workdir: Path) -> Path:
    program = analyze(load_rom_bytes(low), {e: f"case_{e:04X}" for e in entries})
    gen = workdir / "generated"
    gen.mkdir()
    for name, text in generate(program).items():
        (gen / name).write_text(text)
    exe = workdir / "harness"
    cmd = ["cc", "-std=c99", "-O1", "-DGASE_FLAT_MEMORY", "-Wall", "-Wno-unused-label",
           "-Werror", f"-I{ROOT / 'runtime/include'}", f"-I{gen}",
           str(ROOT / "tests/difftest/harness.c"), str(ROOT / "runtime/src/cpu.c"),
           *map(str, sorted(gen.glob("*.c"))), "-o", str(exe)]
    subprocess.run(cmd, check=True)
    return exe


def run_musashi(low: bytes, ram: bytes, states: list[list[int]]) -> list[list[int]]:
    import machine68k as m
    mach = m.Machine(m.CPUType.M68000, 16 * 1024)
    cpu, mem = mach.cpu, mach.mem
    R = m.Register
    results = []
    for st in states:
        mem.w_block(0, low)
        mem.w_block(RAM, ram)
        cpu.w_sr(0x2700)
        cpu.w_reg(R.SP, st[16])
        for i in range(8):
            cpu.w_reg(getattr(R, f"D{i}"), st[1 + i])
        for i in range(7):
            cpu.w_reg(getattr(R, f"A{i}"), st[9 + i])
        cpu.w_usp(st[17])
        cpu.w_sr(st[18])
        sp = cpu.r_reg(R.SP) - 4
        mem.w32(sp, SENTINEL)
        cpu.w_reg(R.SP, sp)
        cpu.w_pc(st[0])
        valid = True
        for _ in range(64):
            pc = cpu.r_pc()
            # Snippets that clobber their own return address wander off
            # into random code: such cases are meaningless, skip them.
            if not (st[0] <= pc < st[0] + SLOT or pc == HANDLER):
                valid = False
                break
            cpu.execute(1)
            if cpu.r_pc() == SENTINEL:
                break
        regs = [cpu.r_pc() if valid else 0xDEAD]
        regs += [cpu.r_reg(getattr(R, f"D{i}")) for i in range(8)]
        regs += [cpu.r_reg(getattr(R, f"A{i}")) for i in range(7)] + [cpu.r_reg(R.SP)]
        regs += [cpu.r_usp(), cpu.r_sr() & 0xA71F,
                 zlib.crc32(bytes(mem.r_block(RAM, 0x10000)))]
        results.append(regs)
    return results


NAMES = ["pc"] + [f"d{i}" for i in range(8)] + [f"a{i}" for i in range(8)] + ["usp", "sr", "ram"]


def run(ncases: int, seed: int, length: int, verbose: bool = True) -> list[str]:
    rng = random.Random(seed)
    low, ram, entries = build_image(rng, ncases, length)
    states = make_states(rng, entries)
    expected = run_musashi(low, ram, states)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        exe = build_harness(low, entries, work)
        (work / "low.bin").write_bytes(low)
        (work / "ram.bin").write_bytes(ram)
        with open(work / "cases.bin", "wb") as f:
            for st in states:
                f.write(struct.pack("<19I", *st))
        subprocess.run([str(exe), str(work / "low.bin"), str(work / "ram.bin"),
                        str(work / "cases.bin"), str(work / "out.txt")], check=True)
        got = [[int(x, 16) for x in line.split()] for line in (work / "out.txt").read_text().splitlines()]

    rom = load_rom_bytes(low)
    failures = []
    for st, exp, mine in zip(states, expected, got):
        if exp[0] != SENTINEL:
            continue                       # Musashi didn't come back (rare)
        listing, pos = [], st[0]
        while True:
            ins = decode(rom.u16, pos)
            listing.append(ins.text())
            if ins.mnemonic == "rts":
                break
            pos = ins.next_addr
        if any(t.startswith("div") for t in listing):
            # On DIV overflow the 68000 clears C; Musashi leaves it alone.
            exp[18] &= ~1
            mine[18] &= ~1
        diffs = [f"{n}: musashi={e:08X} gase={g:08X}" for n, e, g in zip(NAMES, exp, mine) if e != g]
        if diffs:
            failures.append(f"case ${st[0]:04X} [{'; '.join(listing)}] "
                            f"in: d={[hex(x) for x in st[1:9]]} sr={st[18]:04X}\n    " + "\n    ".join(diffs))
    if verbose:
        print(f"{len(got)} cases, {len(failures)} mismatches")
        for f in failures[:40]:
            print(f)
    return failures


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--cases", type=int, default=900)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--length", type=int, default=3, help="max instructions per snippet")
    a = p.parse_args()
    return 1 if run(a.cases, a.seed, a.length) else 0


if __name__ == "__main__":
    sys.exit(main())
