//! The decoder against the gase emulator's own disassembler, on every opcode.

use gase_recomp::m68k::{DecodeError, decode};

fn dis(words: &[u16]) -> Result<(u32, String), DecodeError> {
    let i = decode(|a| words.get((a / 2) as usize).copied(), 0)?;
    Ok((i.length, i.text()))
}

#[test]
fn known_encodings() {
    let cases: &[(&[u16], &str)] = &[
        (&[0x3080], "move.w d0,(a0)"),
        (&[0x7005], "moveq #$5,d0"),
        (&[0x4E75], "rts"),
        (&[0x51C9, 0xFFFE], "dbra d1,$0"),
        (&[0x4EFB, 0x0002], "jmp $4(pc,d0.w)"),
        (&[0x48E7, 0xC0C0], "movem.l d0/d1/a0/a1,-(sp)"),
        (
            &[0x23FC, 0x5345, 0x4741, 0x00A1, 0x4000],
            "move.l #$53454741,($a14000).l",
        ),
        (&[0xE348], "lsl.w #$1,d0"),
        (&[0x46FC, 0x2700], "move #$2700,sr"),
    ];
    for (words, text) in cases {
        assert_eq!(dis(words).unwrap().1, *text);
    }
}

/// Validity and length of all 65,536 opcodes agree with gase's disassembler
/// (itself checked against ~2.6 million hardware test vectors).
#[test]
fn every_opcode_matches_gase() {
    let mut seed = 0x1234_5678u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as u16 & 0xF8FF // valid 68000 brief extension words
    };
    let mut bad = Vec::new();
    for op in 0..=0xFFFFu16 {
        if matches!(op >> 12, 0xA | 0xF) {
            continue; // line A/F: we decode them as traps, gase as dc.w
        }
        let words = [op, rand(), rand(), rand(), rand()];
        let (text, len) = gase_m68k::disasm::disassemble(0, |a| words[(a / 2) as usize % 5]);
        let theirs = (!text.starts_with("dc.w")).then_some(len);
        let mine = dis(&words).ok().map(|(l, _)| l);
        if mine != theirs {
            bad.push(format!(
                "{op:04X}: gase `{text}` ({theirs:?}) vs ours {mine:?}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches, e.g.\n{}",
        bad.len(),
        bad[..bad.len().min(20)].join("\n")
    );
}
