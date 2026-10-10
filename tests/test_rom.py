"""ROM loading: header, checksum, SMD de-interleaving."""

import make_test_rom

from gase.rom import load_rom_bytes


def interleave_smd(flat: bytes) -> bytes:
    """The inverse of the loader: produce a .smd image from a flat ROM."""
    header = bytearray(0x200)
    header[8], header[9] = 0xAA, 0xBB
    body = bytearray()
    padded = flat + b"\xff" * (-len(flat) % 0x4000)
    for block in range(0, len(padded), 0x4000):
        chunk = padded[block:block + 0x4000]
        body += chunk[1::2] + chunk[0::2]          # odd bytes first, then even
    return bytes(header + body)


def test_header_and_checksum():
    rom = load_rom_bytes(make_test_rom.build())
    assert rom.header.console == "SEGA MEGA DRIVE"
    assert rom.title == "GASE TEST ROM"
    assert rom.checksum_ok
    assert rom.vectors[1] == 0x200                 # reset vector


def test_smd_is_deinterleaved():
    flat = make_test_rom.build()
    rom = load_rom_bytes(interleave_smd(flat))
    assert rom.source_format == "smd"
    assert rom.data[:len(flat)] == flat
