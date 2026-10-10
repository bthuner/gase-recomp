"""
project.py — write a ready-to-build project for the chosen platform.

    <out>/
      CMakeLists.txt        one build script for every platform
      runtime/              the portable C runtime (copied from this repo)
      generated/            the C code produced by codegen.py
      assets/rom.bin        your ROM (loaded at startup, never redistributed)
      scripts/              helpers (fetch SDL2 sources for mobile builds)
      android/              Gradle project            (--platform android)
      ios/                  Xcode generation script   (--platform ios)
      BUILD.md              how to build this output

The generated code depends on the exact ROM it came from, so the runtime
checks the ROM's CRC32 at startup.
"""

from __future__ import annotations

import shutil
from pathlib import Path

from .analysis import Program
from .codegen import generate

REPO_ROOT = Path(__file__).resolve().parent.parent
RUNTIME_DIR = REPO_ROOT / "runtime"
TEMPLATES_DIR = Path(__file__).resolve().parent / "templates"

PLATFORMS = {
    "desktop": "Linux / macOS / Windows executable (CMake + SDL2)",
    "android": "Android app (Gradle + NDK + SDL2)",
    "ios": "iOS app (CMake -> Xcode + SDL2)",
    "headless": "no window, renders frames to files (tests, CI)",
}


def _render(template: str, values: dict[str, str]) -> str:
    for key, val in values.items():
        template = template.replace("@" + key + "@", val)
    return template


def _copy_template_dir(src: Path, dst: Path, values: dict[str, str]) -> None:
    for path in sorted(src.rglob("*")):
        rel = path.relative_to(src)
        target = dst / Path(_render(str(rel), values))
        if path.is_dir():
            target.mkdir(parents=True, exist_ok=True)
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(_render(path.read_text(), values))
        if path.suffix == ".sh":
            target.chmod(0o755)


def identifier(title: str) -> str:
    """A lowercase identifier for package / bundle names."""
    out = "".join(c.lower() if c.isalnum() else "_" for c in title).strip("_")
    while "__" in out:
        out = out.replace("__", "_")
    if not out or not out[0].isalpha():
        out = "game_" + out
    return out[:40]


def write_project(program: Program, out: Path, platform: str) -> list[Path]:
    if platform not in PLATFORMS:
        raise ValueError(f"unknown platform {platform!r}; choose from {', '.join(PLATFORMS)}")
    out.mkdir(parents=True, exist_ok=True)
    rom = program.rom
    values = {
        "TITLE": rom.title,
        "ID": identifier(rom.title),
        "PLATFORM": platform,
        "CRC": f"{rom.crc32:08X}",
    }

    # 1. the C runtime (fresh copy each time, so runtime fixes propagate)
    if (out / "runtime").exists():
        shutil.rmtree(out / "runtime")
    shutil.copytree(RUNTIME_DIR, out / "runtime",
                    ignore=shutil.ignore_patterns("tests", "*.o", "build*"))

    # 2. generated C code
    gen = out / "generated"
    if gen.exists():
        shutil.rmtree(gen)
    gen.mkdir()
    written = []
    for name, text in generate(program).items():
        (gen / name).write_text(text)
        written.append(gen / name)

    # 3. the ROM, as an asset loaded at runtime
    (out / "assets").mkdir(exist_ok=True)
    (out / "assets" / "rom.bin").write_bytes(rom.data)

    # 4. build files: common + per platform
    _copy_template_dir(TEMPLATES_DIR / "common", out, values)
    if (TEMPLATES_DIR / platform).exists():
        _copy_template_dir(TEMPLATES_DIR / platform, out, values)
    return written
