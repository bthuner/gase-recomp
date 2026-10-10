"""
rom.py — load a Genesis / Mega Drive ROM and read its header.

A cartridge ROM is just the bytes the 68000 sees at $000000-$3FFFFF.
Its first 512 bytes are special:

    $000-$0FF  exception vector table: 64 big-endian 32-bit addresses
               $000 = initial stack pointer, $004 = reset entry point,
               $070 = level 4 (HBlank), $078 = level 6 (VBlank), ...
    $100-$1FF  the Sega header: console name, title, serial, checksum,
               supported I/O devices, ROM/RAM ranges, region codes.

Dumps found in the wild come in two layouts:

    .bin / .gen / .md   "flat": byte N of the file is byte N of the ROM.
    .smd                "Super Magic Drive" copier format: a 512-byte
                        header, then 16 KiB blocks where the first 8 KiB
                        hold the ODD bytes and the next 8 KiB the EVEN
                        bytes. We de-interleave it back to a flat image.
"""

from __future__ import annotations

import struct
import zlib
from dataclasses import dataclass, field
from pathlib import Path

ROM_MAX_SIZE = 0x400000          # 4 MiB of cartridge address space
HEADER_START = 0x100
CHECKSUM_START = 0x200           # the checksum covers $200..end of ROM

# Names of the 64 exception vectors (only the useful ones are named).
VECTOR_NAMES = {
    0: "initial_ssp", 1: "reset", 2: "bus_error", 3: "address_error",
    4: "illegal_instruction", 5: "zero_divide", 6: "chk", 7: "trapv",
    8: "privilege_violation", 9: "trace", 10: "line_a", 11: "line_f",
    24: "spurious_irq", 25: "irq1", 26: "irq2_ext", 27: "irq3",
    28: "irq4_hblank", 29: "irq5", 30: "irq6_vblank", 31: "irq7",
    **{32 + n: f"trap{n}" for n in range(16)},
}


class RomError(Exception):
    pass


@dataclass
class RomHeader:
    console: str
    copyright: str
    title_domestic: str
    title_overseas: str
    serial: str
    checksum: int
    devices: str
    rom_start: int
    rom_end: int
    ram_start: int
    ram_end: int
    region: str


@dataclass
class Rom:
    data: bytes                              # flat, big-endian ROM image
    header: RomHeader
    vectors: list[int] = field(default_factory=list)
    source_format: str = "bin"

    # -- convenience readers (big-endian, like the 68000) ---------------
    def u8(self, addr: int) -> int:
        return self.data[addr]

    def u16(self, addr: int) -> int:
        return (self.data[addr] << 8) | self.data[addr + 1]

    def u32(self, addr: int) -> int:
        return (self.u16(addr) << 16) | self.u16(addr + 2)

    def contains(self, addr: int, length: int = 2) -> bool:
        return 0 <= addr and addr + length <= len(self.data)

    @property
    def crc32(self) -> int:
        return zlib.crc32(self.data) & 0xFFFFFFFF

    def computed_checksum(self) -> int:
        """Sum of all big-endian words from $200 to the end, modulo 2^16."""
        body = self.data[CHECKSUM_START:]
        if len(body) % 2:
            body += b"\x00"
        words = struct.unpack(f">{len(body) // 2}H", body)
        return sum(words) & 0xFFFF

    @property
    def checksum_ok(self) -> bool:
        return self.computed_checksum() == self.header.checksum

    @property
    def title(self) -> str:
        return self.header.title_overseas or self.header.title_domestic or "UNTITLED"


def _ascii(raw: bytes) -> str:
    return raw.decode("ascii", errors="replace").strip(" \x00")


def looks_like_smd(raw: bytes) -> bool:
    """SMD files have a 512-byte copier header; byte 8/9 are $AA $BB."""
    if len(raw) < 0x200 + 0x4000 or (len(raw) - 0x200) % 0x4000:
        return False
    if raw[8] == 0xAA and raw[9] == 0xBB:
        return True
    # Fallback: a flat ROM carries "SEGA" at $100, an interleaved one doesn't.
    return raw[0x100:0x104] != b"SEGA" and _deinterleave(raw)[0x100:0x104] == b"SEGA"


def _deinterleave(raw: bytes) -> bytes:
    """Undo the SMD block interleave (odd bytes first, then even bytes)."""
    body = raw[0x200:]
    out = bytearray(len(body))
    for block in range(0, len(body), 0x4000):
        chunk = body[block:block + 0x4000]
        odd, even = chunk[:0x2000], chunk[0x2000:]
        out[block + 1:block + 0x4000:2] = odd
        out[block + 0:block + 0x4000:2] = even
    return bytes(out)


def parse_header(data: bytes) -> RomHeader:
    h = data[HEADER_START:HEADER_START + 0x100]
    return RomHeader(
        console=_ascii(h[0x00:0x10]),
        copyright=_ascii(h[0x10:0x20]),
        title_domestic=_ascii(h[0x20:0x50]),
        title_overseas=_ascii(h[0x50:0x80]),
        serial=_ascii(h[0x80:0x8E]),
        checksum=struct.unpack(">H", h[0x8E:0x90])[0],
        devices=_ascii(h[0x90:0xA0]),
        rom_start=struct.unpack(">I", h[0xA0:0xA4])[0],
        rom_end=struct.unpack(">I", h[0xA4:0xA8])[0],
        ram_start=struct.unpack(">I", h[0xA8:0xAC])[0],
        ram_end=struct.unpack(">I", h[0xAC:0xB0])[0],
        region=_ascii(h[0xF0:0xF3]),
    )


def load_rom_bytes(raw: bytes) -> Rom:
    fmt = "bin"
    if looks_like_smd(raw):
        raw, fmt = _deinterleave(raw), "smd"
    if len(raw) < 0x200:
        raise RomError("file too small to be a Genesis ROM (no header)")
    if len(raw) > ROM_MAX_SIZE:
        raise RomError(f"ROM is {len(raw)} bytes; mappers above 4 MiB are not supported yet")
    if len(raw) % 2:
        raw += b"\xff"
    vectors = list(struct.unpack(">64I", raw[:0x100]))
    return Rom(data=raw, header=parse_header(raw), vectors=vectors, source_format=fmt)


def load_rom(path: str | Path) -> Rom:
    return load_rom_bytes(Path(path).read_bytes())
