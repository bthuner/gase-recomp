"""
cli.py — the command line.

    python -m gase info    game.md
    python -m gase disasm  game.md [--function 0x200]
    python -m gase recomp  game.md --platform desktop -o build/game
                           [--entries gase_missing.txt]
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import __version__
from .analysis import analyze
from .project import PLATFORMS, write_project
from .rom import VECTOR_NAMES, RomError, load_rom


def parse_entries(path: Path) -> dict[int, str]:
    """Entry file: one hex address per line, optional name, '#' comments."""
    entries: dict[int, str] = {}
    for line in path.read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        addr = int(parts[0].lstrip("$"), 16) & 0xFFFFFF
        entries[addr] = parts[1] if len(parts) > 1 else f"sub_{addr:06X}"
    return entries


def cmd_info(args) -> int:
    rom = load_rom(args.rom)
    h = rom.header
    print(f"file          {args.rom} ({rom.source_format}, {len(rom.data)} bytes)")
    print(f"console       {h.console}")
    print(f"title         {h.title_overseas}  /  {h.title_domestic}")
    print(f"serial        {h.serial}")
    print(f"copyright     {h.copyright}")
    print(f"region        {h.region}")
    print(f"devices       {h.devices}")
    print(f"rom range     ${h.rom_start:06X}-${h.rom_end:06X}")
    status = "OK" if rom.checksum_ok else f"MISMATCH (computed ${rom.computed_checksum():04X})"
    print(f"checksum      ${h.checksum:04X} {status}")
    print(f"crc32         {rom.crc32:08X}")
    print("vectors")
    for num in (0, 1, 2, 3, 4, 28, 30, 32):
        print(f"  {num:2d} {VECTOR_NAMES.get(num, ''):<20} ${rom.vectors[num]:08X}")
    return 0


def _load_entries(args) -> dict[int, str]:
    entries: dict[int, str] = {}
    for path in args.entries or []:
        entries.update(parse_entries(Path(path)))
    return entries


def cmd_disasm(args) -> int:
    rom = load_rom(args.rom)
    program = analyze(rom, _load_entries(args))
    only = int(args.function, 16) if args.function else None
    for entry in sorted(program.functions):
        if only is not None and entry != only:
            continue
        fn = program.functions[entry]
        print(f"\n; ---- {fn.name} (${entry:06X}) ----")
        for addr in sorted(fn.instructions):
            ins = fn.instructions[addr]
            print(f"{addr:06X}  {ins.text()}")
    return 0


def choose_platform() -> str:
    print("Which platform do you want to build for?")
    names = list(PLATFORMS)
    for i, name in enumerate(names, 1):
        print(f"  {i}. {name:<9} {PLATFORMS[name]}")
    while True:
        answer = input("> ").strip().lower()
        if answer in names:
            return answer
        if answer.isdigit() and 1 <= int(answer) <= len(names):
            return names[int(answer) - 1]


def cmd_recomp(args) -> int:
    rom = load_rom(args.rom)
    out = Path(args.output)
    entries = _load_entries(args)
    missing = out / "gase_missing.txt"
    if missing.exists():
        print(f"[1/4] using addresses discovered at runtime: {missing}")
        entries.update(parse_entries(missing))
    platform = args.platform or (choose_platform() if sys.stdin.isatty() else "desktop")

    print(f"[1/4] ROM      {rom.title!r}, {len(rom.data) // 1024} KiB, "
          f"checksum {'OK' if rom.checksum_ok else 'mismatch'}")
    program = analyze(rom, entries)
    ninstr = sum(len(f.instructions) for f in program.functions.values())
    print(f"[2/4] analysis {len(program.functions)} functions, {ninstr} instructions, "
          f"{program.code_bytes} bytes of code")
    for addr, why in sorted(program.rejected_entries.items()):
        print(f"      ! entry ${addr:06X} rejected: {why}")
    files = write_project(program, out, platform)
    print(f"[3/4] codegen  {len(files)} C files in {out / 'generated'}")
    print(f"[4/4] project  {platform} project written to {out}  (see {out / 'BUILD.md'})")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="gase", description=(
        "gase-recomp: statically recompile a Sega Genesis / Mega Drive ROM to C."))
    parser.add_argument("--version", action="version", version=f"gase-recomp {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("info", help="show the ROM header and vectors")
    p.add_argument("rom")
    p.set_defaults(func=cmd_info)

    p = sub.add_parser("disasm", help="disassemble every function found")
    p.add_argument("rom")
    p.add_argument("--function", help="only this entry point (hex)")
    p.add_argument("--entries", action="append", help="file of extra entry points")
    p.set_defaults(func=cmd_disasm)

    p = sub.add_parser("recomp", help="recompile the ROM into a buildable project")
    p.add_argument("rom")
    p.add_argument("-o", "--output", required=True, help="output directory")
    p.add_argument("-p", "--platform", choices=list(PLATFORMS),
                   help="target platform (asked interactively if omitted)")
    p.add_argument("--entries", action="append", help="file of extra entry points")
    p.set_defaults(func=cmd_recomp)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except (RomError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
