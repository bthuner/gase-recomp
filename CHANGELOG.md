# Changelog

All notable changes to gase-recomp are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/).

## [Unreleased]

## [0.2.1] - 2026-10-11

The studio, after its first builds on other computers.

### Fixed
- **A failed build says why.** The studio's error used to be the end of
  the log ("Get more help at…"); it now carries Gradle's *What went wrong*
  block, or the first error lines of cargo, CMake or the build scripts.
- **Installing SDK packages from the studio** (the NDK, the Android
  platform) hung on `sdkmanager`'s licence question, which a window cannot
  answer. The button now reads *Accept the licence and install*, links the
  Android SDK licence, and answers it.
- **Gradle 9 is reported as missing Gradle 8.** gase's Android project
  needs Gradle 8, and the check passed any Gradle.

## [0.2.0] - 2026-10-11

The Rust edition: the first published release. Recompiled games run on the
chips of [gase](https://github.com/bthuner/gase) 0.3.0 and use its menus,
save states and frontends, on desktops and on phones. The C + Python
edition (0.1.0) stays in the repository as a teaching reference.

### Added
- **The Rust recompiler** (`crates/recomp`, the `gase-recomp` program):
  ROM loader, 68000 decoder, control-flow analysis and Rust code
  generation (blocks as a state machine: Rust has no `goto`). `info`,
  `disasm` and `recomp`; a generated game is its own crate, built with
  `cargo`, for the desktop, Android, iOS or headless.
- **A runtime that plugs into gase** (`crates/runtime`): only the 68000 is
  replaced, as gase's `ExternalCpu`; the VDP, sound chips and Z80 stay
  gase's. Code the analysis missed runs on gase's interpreter and is
  logged, so the next recompilation includes it.
- **Exact 68000 timings**, measured on gase's interpreter for every
  instruction form, and hardware waits charged where they happen: the
  difftest compares cycles too. `GASE_RECOMP_INTERPRET` forces address
  ranges onto the interpreter to bisect a glitch.
- **Real games**: hashbug, Sonic 1 and Aladdin play recompiled from start to
  finish of the tested levels.
- **Phones**: a generated game has a `mobile/` crate with its own
  `SDL_main` and the ROM built in, so gase's Android and iOS projects build
  it as an app of its own.
- **gase-recomp studio** (`gase-recomp-studio`): pick a ROM, press Build.
  *This computer* recompiles and compiles the game, then **Play**; *Android
  app* builds an APK with gase's Android project and installs it over
  USB. It lists the missing tools and installs what one command can.
  `--cli` does the same without a window, `--desktop ROM` is what gase's
  *Recompile to native…* opens.
- **A JIT** (`crates/jit`): 68000 code to x86-64 or AArch64 machine code
  while the game runs, for any ROM, through a small IR, with gase's
  interpreter as the fallback (10.6× the interpreter on a register loop).
  `gase-jit` is gase with the JIT for every ROM; "gase JIT" is the same as
  an Android app.
- **Phone builds** (`crates/remote`): in gase JIT's pause menu, *Make an
  app of this game…* finds the studio on the local network, sends the ROM
  and installs the app the computer builds once someone clicks Accept.
- **Differential testing** against gase's hardware-validated interpreter:
  random instruction snippets (registers, flags, memory and cycles), and an
  end-to-end test that plays a hand-made ROM recompiled, frame for frame.
- **The making-of** (English and French): every step, from the ROM header
  to the JIT, with the bugs found on the way.
- Release files: the programs for Linux and macOS (Apple silicon), and the
  gase JIT APK.

[Unreleased]: https://github.com/bthuner/gase-recomp/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/bthuner/gase-recomp/releases/tag/v0.2.1
[0.2.0]: https://github.com/bthuner/gase-recomp/releases/tag/v0.2.0
