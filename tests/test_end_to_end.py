"""ROM -> recomp -> build -> run, including the coverage feedback loop."""

import shutil
import subprocess
import sys

import make_test_rom
import pytest

from gase.cli import main as gase_main

pytestmark = pytest.mark.skipif(not shutil.which("cmake"), reason="cmake not installed")


def build(out):
    subprocess.run(["cmake", "-S", str(out), "-B", str(out / "build"), "-DGASE_HEADLESS=ON"],
                   check=True, capture_output=True)
    subprocess.run(["cmake", "--build", str(out / "build")], check=True, capture_output=True)
    return out / "build" / "game_headless"


def run(exe, out, frames):
    return subprocess.run([str(exe), "assets/rom.bin", str(frames), "frame.ppm", "ram.bin"],
                          cwd=out, capture_output=True, text=True)


def test_recompile_build_and_run(tmp_path):
    rom = tmp_path / "test.bin"
    rom.write_bytes(make_test_rom.build())
    out = tmp_path / "out"

    # Pass 1: `jsr (a0)` targets code the analysis could not see.
    assert gase_main(["recomp", str(rom), "-p", "headless", "-o", str(out)]) == 0
    result = run(build(out), out, 60)
    assert result.returncode == 2, result.stderr
    assert (out / "gase_missing.txt").read_text().split() == ["0x0002EE"]

    # Pass 2: the address found at runtime becomes a function.
    assert gase_main(["recomp", str(rom), "-p", "headless", "-o", str(out)]) == 0
    result = run(build(out), out, 120)
    assert result.returncode == 0, result.stderr

    ram = (out / "ram.bin").read_bytes()
    frames = int.from_bytes(ram[2:4], "big")
    vblanks = int.from_bytes(ram[0x10:0x14], "big")
    assert 117 <= vblanks <= 120
    assert frames == vblanks                          # main loop ran once per VBlank
    assert sum(ram[4:8]) == frames                    # jump table: one case per frame
    assert min(ram[4:8]) >= frames // 4 - 1          # ...and every case was reached
    assert ram[8] == frames & 0xFF                    # indirect call
    assert ram[9] == frames & 0xFF                    # static call

    ppm = (out / "frame.ppm").read_bytes()
    _, size, _, pixels = ppm.split(b"\n", 3)
    width, height = map(int, size.split())
    assert (width, height) == (320, 224)

    def pixel(x, y):
        i = (y * width + x) * 3
        return tuple(pixels[i:i + 3])

    assert pixel(10, 82) == (255, 0, 0)               # row 10: solid red tiles
    assert {pixel(10, 98), pixel(11, 98)} == {(0, 255, 0), (0, 0, 255)}   # checkerboard


def test_every_platform_project_is_generated(tmp_path):
    rom = tmp_path / "test.bin"
    rom.write_bytes(make_test_rom.build())
    for platform, marker in [("desktop", "CMakeLists.txt"), ("android", "android/app/build.gradle"),
                             ("ios", "ios/generate_xcode.sh"), ("headless", "BUILD.md")]:
        out = tmp_path / platform
        assert gase_main(["recomp", str(rom), "-p", platform, "-o", str(out)]) == 0
        assert (out / marker).exists()
        assert (out / "assets" / "rom.bin").read_bytes() == rom.read_bytes()
    java = list((tmp_path / "android").rglob("GameActivity.java"))
    assert java and "package org.gaserecomp.gase_test_rom;" in java[0].read_text()


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
