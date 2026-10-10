"""
make_test_rom.py — build a tiny Genesis ROM by hand, for end-to-end tests.

No game is shipped with gase-recomp, so we write our own: a few hundred
bytes of 68000 machine code, assembled here word by word. It is also a
nice illustration of what a minimal Genesis program must do:

    1. vector table + Sega header (the console checks "SEGA" at $100)
    2. unlock TMSS, set up the VDP registers
    3. upload a palette, two tiles and a tilemap
    4. enable the display and the VBlank interrupt
    5. main loop: wait for VBlank, then do some work

The work deliberately exercises the hard parts of static recompilation:
    - a jump table `jmp table(pc,d0.w)` made of BRA.W (static heuristic)
    - an indirect call `jsr (a0)` that analysis can't see (feedback loop)
    - a VBlank interrupt handler driving the main loop

RAM layout (results checked by tests/test_end_to_end.py):
    $FF0000.b  vblank flag          $FF0004-7.b  jump-table counters
    $FF0002.w  frame counter        $FF0008.b    indirect-call counter
    $FF0010.l  vblank counter       $FF0009.b    static-call counter
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path


class Asm:
    """A two-pass mini assembler: emit words, labels resolved at the end."""

    def __init__(self, origin: int):
        self.origin = origin
        self.items: list = []          # ints (words) or (kind, label, base_fn)
        self.labels: dict[str, int] = {}
        self.pc = origin

    def label(self, name: str) -> None:
        self.labels[name] = self.pc

    def w(self, *words: int) -> None:
        for word in words:
            self.items.append(word & 0xFFFF)
            self.pc += 2

    def l(self, value: int) -> None:
        self.w(value >> 16, value)

    def rel16(self, name: str) -> None:
        """16-bit displacement to `name`, relative to this extension word."""
        self.items.append(("rel16", name, self.pc))
        self.pc += 2

    def rel8_in_opcode(self, opcode: int, name: str) -> None:
        """Short branch: displacement in the low byte, relative to pc+2."""
        self.items.append(("rel8", name, self.pc, opcode))
        self.pc += 2

    def pcindex(self, name: str, reg: int = 0) -> None:
        """Brief extension word for d8(pc,dN.w)."""
        self.items.append(("idx8", name, self.pc, reg))
        self.pc += 2

    def assemble(self) -> bytes:
        out = bytearray()
        for it in self.items:
            if isinstance(it, int):
                word = it
            elif it[0] == "rel16":
                word = (self.labels[it[1]] - it[2]) & 0xFFFF
            elif it[0] == "rel8":
                disp = self.labels[it[1]] - (it[2] + 2)
                assert -128 <= disp <= 127 and disp != 0, it
                word = it[3] | (disp & 0xFF)
            else:
                disp = self.labels[it[1]] - it[2]
                assert -128 <= disp <= 127, it
                word = (it[3] << 12) | (disp & 0xFF)
            out += struct.pack(">H", word)
        return bytes(out)


VDP_DATA, VDP_CTRL = 0xC00000, 0xC00004
FLAG, FRAMES, JT, ICALL, SCALL, VBLS = 0xFF0000, 0xFF0002, 0xFF0004, 0xFF0008, 0xFF0009, 0xFF0010


def vram_write(addr: int) -> int:
    """The 32-bit VDP command "write VRAM at addr"."""
    return 0x40000000 | ((addr & 0x3FFF) << 16) | (addr >> 14)


def build() -> bytes:
    a = Asm(0x200)

    a.label("reset")
    a.w(0x46FC, 0x2700)                         # move.w #$2700,sr
    a.w(0x1039); a.l(0xA10001)                  # move.b $A10001,d0
    a.w(0x0200, 0x000F)                         # andi.b #$0F,d0
    a.rel8_in_opcode(0x6700, "no_tmss")         # beq.s no_tmss
    a.w(0x23FC); a.l(0x53454741); a.l(0xA14000) # move.l #'SEGA',$A14000
    a.label("no_tmss")
    a.w(0x43F9); a.l(VDP_CTRL)                  # lea $C00004,a1
    a.w(0x45F9); a.l(VDP_DATA)                  # lea $C00000,a2

    a.w(0x41FA); a.rel16("vdp_regs")            # lea vdp_regs(pc),a0
    a.w(0x7200 | 18)                            # moveq #18,d1
    a.label("reg_loop")
    a.w(0x3298)                                 # move.w (a0)+,(a1)
    a.w(0x51C9); a.rel16("reg_loop")            # dbra d1,reg_loop

    a.w(0x22BC); a.l(0xC0000000)                # move.l #CRAM write 0,(a1)
    a.w(0x41FA); a.rel16("palette")             # lea palette(pc),a0
    a.w(0x7200 | 15)                            # moveq #15,d1
    a.label("pal_loop")
    a.w(0x3498)                                 # move.w (a0)+,(a2)
    a.w(0x51C9); a.rel16("pal_loop")

    a.w(0x22BC); a.l(vram_write(0x0020))        # tiles 1 and 2
    a.w(0x41FA); a.rel16("tiles")
    a.w(0x7200 | 15)                            # 16 longs
    a.label("tile_loop")
    a.w(0x2498)                                 # move.l (a0)+,(a2)
    a.w(0x51C9); a.rel16("tile_loop")

    for row, tile in ((10, 1), (12, 2)):        # two rows of plane A
        a.w(0x22BC); a.l(vram_write(0xC000 + row * 128))
        a.w(0x7400 | tile)                      # moveq #tile,d2
        a.w(0x7200 | 39)                        # moveq #39,d1
        a.label(f"map_loop{row}")
        a.w(0x3482)                             # move.w d2,(a2)
        a.w(0x51C9); a.rel16(f"map_loop{row}")

    a.w(0x32BC, 0x8174)                         # display on, VBlank IRQ on
    a.w(0x46FC, 0x2000)                         # move.w #$2000,sr

    a.label("main")
    a.w(0x4A39); a.l(FLAG)                      # tst.b flag
    a.rel8_in_opcode(0x6700, "main")            # beq.s main
    a.w(0x4239); a.l(FLAG)                      # clr.b flag
    a.w(0x5279); a.l(FRAMES)                    # addq.w #1,frames
    a.w(0x6100); a.rel16("static_sub")          # bsr.w static_sub
    a.w(0x41FA); a.rel16("indirect_sub")        # lea indirect_sub(pc),a0
    a.w(0x4E90)                                 # jsr (a0)
    a.w(0x3039); a.l(FRAMES)                    # move.w frames,d0
    a.w(0x0240, 0x0003)                         # andi.w #3,d0
    a.w(0xD040)                                 # add.w d0,d0
    a.w(0xD040)                                 # add.w d0,d0
    a.w(0x4EFB); a.pcindex("table")             # jmp table(pc,d0.w)
    a.label("table")
    for n in range(4):
        a.w(0x6000); a.rel16(f"case{n}")        # bra.w caseN
    for n in range(4):
        a.label(f"case{n}")
        a.w(0x5239); a.l(JT + n)                # addq.b #1,counter[n]
        a.w(0x6000); a.rel16("main")            # bra.w main

    a.label("static_sub")
    a.w(0x5239); a.l(SCALL)                     # addq.b #1,$FF0009
    a.w(0x4E75)                                 # rts

    a.label("indirect_sub")                     # only reachable via jsr (a0)
    a.w(0x3439); a.l(FRAMES)                    # move.w frames,d2
    a.w(0x0242, 0x000F)                         # andi.w #$F,d2
    a.w(0x0042, 0x8700)                         # ori.w #$8700,d2 (backdrop)
    a.w(0x3282)                                 # move.w d2,(a1)
    a.w(0x5239); a.l(ICALL)                     # addq.b #1,$FF0008
    a.w(0x4E75)                                 # rts

    a.label("vblank")
    a.w(0x13FC, 0x0001); a.l(FLAG)              # move.b #1,flag
    a.w(0x52B9); a.l(VBLS)                      # addq.l #1,vblanks
    a.w(0x4E73)                                 # rte

    a.label("error")                            # crash handler: spin forever
    a.w(0x4E71)                                 # nop
    a.rel8_in_opcode(0x6000, "error")           # bra.s error

    a.label("vdp_regs")
    a.w(0x8004, 0x8134, 0x8230, 0x8328, 0x8407, 0x855C, 0x8600, 0x8700,
        0x8800, 0x8900, 0x8AFF, 0x8B00, 0x8C81, 0x8D2E, 0x8E00, 0x8F02,
        0x9001, 0x9100, 0x9200)
    a.label("palette")
    a.w(0x0000, 0x000E, 0x00E0, 0x0E00, *[0x0EEE] * 12)
    a.label("tiles")
    for _ in range(8):
        a.l(0x11111111)                         # tile 1: solid colour 1 (red)
    for row in range(8):
        a.l(0x23232323 if row % 2 == 0 else 0x32323232)  # tile 2: green/blue

    code = a.assemble()
    rom = bytearray(0x200) + code
    rom += b"\xff" * (-len(rom) % 0x400)        # pad to 1 KiB

    struct.pack_into(">I", rom, 0, 0x00FFFE00)  # initial SSP
    for v in range(1, 64):
        struct.pack_into(">I", rom, v * 4, a.labels["error"])
    struct.pack_into(">I", rom, 4, a.labels["reset"])
    struct.pack_into(">I", rom, 30 * 4, a.labels["vblank"])
    header = {0x100: b"SEGA MEGA DRIVE ", 0x110: b"(C)GASE 2026.OCT",
              0x120: b"GASE TEST ROM".ljust(48), 0x150: b"GASE TEST ROM".ljust(48),
              0x180: b"GM 00000000-00", 0x190: b"J".ljust(16), 0x1F0: b"JUE"}
    for off, text in header.items():
        rom[off:off + len(text)] = text
    struct.pack_into(">II", rom, 0x1A0, 0, len(rom) - 1)
    struct.pack_into(">II", rom, 0x1A8, 0xFF0000, 0xFFFFFF)
    checksum = sum(struct.unpack(f">{(len(rom) - 0x200) // 2}H", rom[0x200:])) & 0xFFFF
    struct.pack_into(">H", rom, 0x18E, checksum)
    return bytes(rom)


if __name__ == "__main__":
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "gase_test.bin")
    out.write_bytes(build())
    print(f"wrote {out} ({out.stat().st_size} bytes)")
