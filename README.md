# gase-recomp

**ROM in, native game out.** gase-recomp is a *pedagogic* static recompiler for
Sega Genesis / Mega Drive games. Bring a ROM you own, choose a platform
(desktop, Android, iOS), and it translates the game's 68000 machine code
into portable C that builds natively for that platform.

The goal is just as much to **teach how a decomp/recomp project is built**
as to run games: every module is small, commented, and maps to one step of
the pipeline. The making-of explains it chapter by chapter:
**[bthuner.github.io/gase-recomp](https://bthuner.github.io/gase-recomp/)**
(sources in [`docs/`](docs/), English and [French](docs/fr/)).

🇫🇷 [Lire en français](README.fr.md)

```
 game.md ──► rom.py ──► m68k/decoder.py ──► analysis.py ──► codegen.py ──► project.py
  (ROM)     header,     68000 machine       functions,      one C block    CMake + SDL2
            vectors     code → objects      branches, calls per instruction desktop/android/ios
                                                                 │
                                    runtime/ (C): CPU glue, memory map, VDP, PSG, pads
```

## Quick start

Requirements: Python 3.10+, a C compiler, CMake, SDL2 (`apt install libsdl2-dev`,
`brew install sdl2`, vcpkg on Windows).

```sh
python -m gase info    game.md                 # header, checksum, vectors
python -m gase disasm  game.md | less          # what the analysis found
python -m gase recomp  game.md -o build/game   # asks which platform
cmake -S build/game -B build/game/out && cmake --build build/game/out
./build/game/out/<name>                        # keys: arrows, Z X C, Enter
```

`--platform desktop|android|ios|headless` skips the question. Each output
folder has a `BUILD.md` with the platform-specific steps (Android Studio,
Xcode).

### The coverage feedback loop

Static analysis can't know where `jmp (a0)` goes. When the game reaches
code that wasn't recompiled, the runtime writes the address to
`gase_missing.txt` and stops. Put that file in the output folder, run the
same `recomp` command again, rebuild: the address is now a function.
Repeat until the game plays through. The making-of explains why this loop
is central to static recompilation.

> No game data is included or distributed. Use ROMs you dumped or own.

## Repository tour

| path | what you learn there |
|------|----------------------|
| [`gase/rom.py`](gase/rom.py) | ROM layout: vector table, Sega header, checksum, SMD interleave |
| [`gase/m68k/`](gase/m68k/) | decoding a CISC instruction set: opcode lines, addressing modes, extension words |
| [`gase/analysis.py`](gase/analysis.py) | recursive-descent disassembly, functions, jump-table heuristics |
| [`gase/codegen.py`](gase/codegen.py) | translating instructions to C, labels/goto, calls and returns |
| [`runtime/include/gase/gase.h`](runtime/include/gase/gase.h) | the contract between generated code and the runtime |
| [`runtime/include/gase/m68k_ops.h`](runtime/include/gase/m68k_ops.h) | 68000 flags (X N Z V C), shifts, BCD, multiply/divide |
| [`runtime/src/cpu.c`](runtime/src/cpu.c) | dispatch, exceptions, interrupts |
| [`runtime/src/system.c`](runtime/src/system.c) | the scanline scheduler: how ahead-of-time code still gets interrupts |
| [`runtime/src/vdp.c`](runtime/src/vdp.c) | the Genesis video chip: planes, sprites, scrolling, DMA |
| [`runtime/src/psg.c`](runtime/src/psg.c) | the SN76489 sound chip |
| [`runtime/src/platform_sdl.c`](runtime/src/platform_sdl.c) | one SDL2 shell for desktop, Android and iOS (touch controls) |
| [`tests/difftest/`](tests/difftest/) | differential testing against the Musashi 68000 emulator |
| [`tests/roms/make_test_rom.py`](tests/roms/make_test_rom.py) | a Genesis ROM written by hand, for end-to-end tests |

## Testing

```sh
pip install pytest machine68k
python -m pytest                                        # everything
python tests/difftest/difftest.py --cases 2000 --seed 7 # more random cases
```

The differential test runs thousands of random instruction sequences on
both the recompiled C and [Musashi](https://github.com/kstenerud/Musashi)
(through `machine68k`), and compares registers, flags, exception frames and
RAM. The end-to-end test assembles a ROM, recompiles it, builds it, checks
that the feedback loop finds the indirect call, then checks RAM and pixels.

## Status and roadmap

Works today: full 68000 instruction set (decoder checked against Musashi on
all 65,536 opcodes, semantics checked by differential tests), VDP (planes,
window, sprites, scrolling, DMA), PSG, 3-button pads, VBlank/HBlank
interrupts, desktop builds. Android and iOS project generation is
implemented, but it is not yet tested on devices.

Next steps, each a good `feature/*` branch:

- [ ] Z80 CPU + YM2612 FM synthesis (most game music runs on the Z80)
- [ ] battery-backed SRAM saves, 6-button pads
- [ ] VDP shadow/highlight and interlace modes
- [ ] an interpreter fallback for code in RAM and self-modifying code
- [ ] a better cycle model (the current one is an approximation)
- [ ] ROMs above 4 MiB (SSF2 mapper)

## Contributing

`main` holds releases, `develop` is the integration branch, and all work
happens in `feature/*` or `fix/*` branches merged into `develop` through
pull requests. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT, © 2026 Théo Hunerblaes. Sega, Genesis and Mega Drive are trademarks of
Sega; this project is not affiliated with Sega.
