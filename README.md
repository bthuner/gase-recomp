# gase-recomp

**ROM in, native game out.** gase-recomp is a *pedagogic* static recompiler for
Sega Genesis / Mega Drive games. Bring a ROM you own, and it translates the
game's 68000 machine code into Rust that runs on the chips of
[**gase**](https://github.com/bthuner/gase), a Rust Mega Drive emulator: only
the 68000 is replaced by recompiled code, the VDP, YM2612, PSG and Z80 are
gase's.

> **Two editions live here.** The **Rust edition** (`crates/`) is the main
> path. The first **C + Python edition** (`gase/`, `runtime/`, `tests/`) stays
> as a teaching reference: same pipeline, different trade-offs. The
> making-of tells why we switched.

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

## Quick start (Rust edition)

Requirements: Rust 1.85+; for a desktop window, the SDL2 development package
(`apt install libsdl2-dev`, `brew install sdl2`).

Ready-made programs (`gase-recomp`, `gase-recomp-studio`, `gase-jit`, for
Linux and macOS) and the "gase JIT" Android app are attached to each
[release](https://github.com/bthuner/gase-recomp/releases). Recompiling a
game still compiles Rust, so Rust is needed either way.

```sh
cargo run --release -p gase-recomp -- info   game.md
cargo run --release -p gase-recomp -- disasm game.md | less
cargo run --release -p gase-recomp -- recomp game.md -o build/game
cd build/game && cargo run --release           # gase's window, menus, save states
cargo run --release -- --headless --frames 600 --screenshot shot.png
```

The generated folder is an ordinary Cargo crate: `src/recomp/` is the
game's code in Rust, `rom.bin` your ROM (graphics, sound and data are still
read from it). The program **is gase**, the emulator, with one more CPU:
it registers the recompiled code for its ROM's CRC-32, then runs gase's
own frontend, so you get gase's window, menus, save states with previews,
rewind, gamepads and settings. Code the analysis missed runs on gase's
68000 interpreter, so the game keeps playing, and its address goes to
`gase_missing.txt`; run `recomp` again and it becomes Rust too.

Desktop (Linux, macOS, Windows), headless, **Android and iOS**. Every
generated game also has a `mobile/` crate: the same game as a phone app,
with its own `SDL_main`, the ROM built in, and gase's touch controls and
menus. gase's own Android and iOS projects build it (the commands are in
the game's `BUILD.md`; CI builds the test ROM's APK). The browser is next.

`crates/jit` is a **JIT**: it translates 68000 code to x86-64 or AArch64
machine code *while* the game runs, for any ROM, with no compiler (10.6× the
interpreter on a register loop, 4.1× on a memory loop). What it does not
translate yet goes through gase's interpreter. `crates/jit-app` puts it in
gase for every ROM: `cargo run --release -p gase-recomp-jit-app -- game.md`
on a desktop, and a "gase JIT" Android app (built by CI with gase's
Android project; not iOS, which forbids apps to write machine code).

| crate | what you learn there |
|-------|----------------------|
| [`crates/recomp`](crates/recomp/src/lib.rs) | ROM, 68000 decoder, analysis, Rust code generation (no `goto`: blocks as a state machine) |
| [`crates/runtime`](crates/runtime/src/lib.rs) | the CPU context, flags, calls and poll points, the interpreter fallback, and how a recompiled game plugs into gase (an `ExternalCpu`) |
| [`crates/difftest`](crates/difftest/src/lib.rs) | differential testing against gase's hardware-validated 68000 interpreter |
| [`crates/jit`](crates/jit/src/lib.rs) | a JIT: 68000 code to x86-64 or AArch64 machine code while the game runs, for any ROM (regions, flags mapped onto x86's, an interpreter escape hatch) |
| [`crates/e2e`](crates/e2e/src/lib.rs) | a hand-assembled ROM, recompiled and played on a gase console: feedback loop, same frames as the interpreter, save states |

### The studio: pick a ROM, press Build, play

```sh
cargo run --release -p gase-recomp-studio            # the window
cargo run --release -p gase-recomp-studio -- --check # what is installed, what is missing
cargo run --release -p gase-recomp-studio -- --cli game.md   # same, without a window
cargo run --release -p gase-recomp-studio -- --cli game.md --target desktop --run
```

**This computer**: recompile, compile, then **Play** starts gase with the
game's code built in (`--desktop game.md` opens the window set to it).
**Android app**: a window around the whole Android build. Pick a ROM, check the app's name
and identifier, press **Build the APK**. It recompiles the ROM, fetches gase
at the right revision, builds with gase's Android project and puts
`<game>.apk` next to the project, with a button to install it on a phone
over USB (`adb`). The list on the right shows what this computer is missing
(Android SDK, NDK, Java, Gradle, CMake, Rust's Android target) and fixes
what one command can fix. Linux and macOS; on Windows, use WSL.

**Phone builds**: switch them on in the studio (or start it with
`--phone-builds`), and the "gase JIT" Android app on the same network
offers **Make an app of this game…** in its pause menu. The phone finds
the studio, sends the ROM, and you click **Accept** on the computer. The
studio builds the APK, and the phone downloads and installs it. Nothing is
typed on the phone, and the ROM stays on your network (`crates/remote`:
UDP discovery plus a few HTTP requests). No phone at hand? `cargo run -p
gase-recomp-remote --example phone -- game.md` plays its part.

```sh
cargo test --workspace                       # decoder, differential and end-to-end tests
GASE_DIFFTEST_SEED=7 cargo test -p gase-recomp-difftest
```

## Quick start (C edition, teaching reference)

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

First real game: **hashbug**, a 2.2 MB homebrew, recompiled in one pass and
played inside gase with graphics identical to the emulator (release build
about 5 minutes).
Second: **Aladdin**, recompiled in one pass (release build under 4 minutes)
and fully playable. A few sprite and background glitches the emulator
didn't show came from timing: estimated instruction timings (now
measured on gase's 68000 and checked cycle for cycle by the differential
test) and hardware waits counted too late (now charged at once). The
glitches no longer reproduce (making-of, chapter 10). A bisection switch,
`GASE_RECOMP_INTERPRET`, helps find a miscompiled routine in a game we
don't have.

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
