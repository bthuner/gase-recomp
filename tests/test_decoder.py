"""The 68000 decoder, on known encodings and against Musashi's disassembler."""

import random

import pytest

from gase.m68k import DecodeError, decode


def dis(*words):
    return decode(lambda a: words[a // 2], 0)


@pytest.mark.parametrize("words,text,length", [
    ((0x3080,), "move.w d0,(a0)", 2),
    ((0x7005,), "moveq #$5,d0", 2),
    ((0x4E75,), "rts", 2),
    ((0x51C9, 0xFFFE), "dbra d1,$0", 4),
    ((0x4EFB, 0x0002), "jmp $4(pc,d0.w)", 4),
    ((0x48E7, 0xC0C0), "movem.l d0/d1/a0/a1,-(sp)", 4),
    ((0x23FC, 0x5345, 0x4741, 0x00A1, 0x4000), "move.l #$53454741,($a14000).l", 10),
    ((0xE348,), "lsl.w #$1,d0", 2),
])
def test_known_encodings(words, text, length):
    ins = dis(*words)
    assert ins.text() == text
    assert ins.length == length


@pytest.mark.parametrize("words", [(0x4AFB,), (0x60FF,), (0xE8C0,), (0x0E00,)])
def test_invalid_encodings_are_rejected(words):
    with pytest.raises(DecodeError):
        dis(*words, 0, 0, 0)


def test_every_opcode_matches_musashi():
    """Validity and length of all 65536 opcodes agree with Musashi."""
    m68k = pytest.importorskip("machine68k")
    mach = m68k.Machine(m68k.CPUType.M68000, 64)
    rng = random.Random(1)
    bad = []
    for op in range(0x10000):
        if op >> 12 in (0xA, 0xF):               # line A/F: we decode them as traps
            continue
        ext = [rng.randrange(0x10000) & 0xF8FF for _ in range(4)]   # 68000 brief ext words
        words = [op] + ext
        for k, w in enumerate(words):
            mach.mem.w16(0x1000 + 2 * k, w)
        length, text = mach.cpu.disassemble(0x1000)
        theirs = None if text.startswith("dc.w") and op != 0x4AFC else length   # $4AFC = ILLEGAL, a real instruction
        try:
            mine = decode(lambda a: words[(a - 0x1000) // 2], 0x1000).length
        except DecodeError:
            mine = None
        if mine != theirs:
            bad.append((hex(op), text, mine))
    assert not bad, bad[:20]
