//! How long each instruction takes, measured on gase's 68000.
//!
//! Recompiled code has no clock of its own: every instruction adds its
//! cycle count to `cx.cycles`, and that count decides when the rest of the
//! console (scanlines, VBlank, DMA) catches up. A rough estimate is enough
//! to *run* a game, not to run it *like the console does*: Aladdin streams
//! its sprite graphics during VBlank, and with timings 10 % off its uploads
//! landed at the wrong moment (wrong tiles in the HUD, garbage columns).
//!
//! So we don't model timings, we ask the reference. Each instruction is
//! executed once, with its real extension words, on gase's interpreter
//! (validated against hardware test vectors) in a small sandbox, and we
//! keep the number of cycles it reports. A few timings depend on values
//! only known at run time; we measure the fixed part and the generated
//! code adds the rest:
//!
//! | instruction          | measured here                 | added at run time      |
//! |----------------------|-------------------------------|------------------------|
//! | Bcc, Scc, DBcc       | each outcome separately       | (picks one)            |
//! | shift/rotate by Dn   | with a count of 0             | 2 per bit shifted      |
//! | MULU / MULS          | with a multiplier of 0        | 2 per bit (pattern)    |
//! | DIVU / DIVS          | the operand fetch only        | the division itself    |
//! | CHK                  | in bounds                     | out: the trap          |
//! | TRAP, TRAPV, ...     | without the exception         | the exception (runtime)|

use std::collections::HashMap;

use gase_m68k::{Bus, M68k};

use super::{Instruction, Mode};

/// Where the sandbox puts the address registers: even, apart, in RAM.
const AREG_BASE: u32 = 0x00FF_8000;
/// Cycles the runtime charges itself when it takes an exception.
pub const EXCEPTION_CYCLES: u32 = 34;

/// Memory that reads as zero except for the instruction under test.
#[derive(Default)]
struct Sandbox {
    mem: HashMap<u32, u8>,
}

impl Bus for Sandbox {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.mem.get(&(addr & 0xFF_FFFF)).copied().unwrap_or(0)
    }
    fn read_word(&mut self, addr: u32) -> u16 {
        u16::from_be_bytes([self.read_byte(addr), self.read_byte(addr | 1)])
    }
    fn write_byte(&mut self, addr: u32, value: u8) {
        self.mem.insert(addr & 0xFF_FFFF, value);
    }
    fn write_word(&mut self, addr: u32, value: u16) {
        let [hi, lo] = value.to_be_bytes();
        self.write_byte(addr & !1, hi);
        self.write_byte(addr | 1, lo);
    }
}

/// Does condition `cc` hold for these flags (CCR bits: X N Z V C)?
fn holds(cc: u8, ccr: u8) -> bool {
    let (n, z, v, c) = (ccr & 8 != 0, ccr & 4 != 0, ccr & 2 != 0, ccr & 1 != 0);
    match cc {
        0 => true,
        1 => false,
        2 => !c && !z,
        3 => c || z,
        4 => !c,
        5 => c,
        6 => !z,
        7 => z,
        8 => !v,
        9 => v,
        10 => !n,
        11 => n,
        12 => n == v,
        13 => n != v,
        14 => !z && n == v,
        _ => z || n != v,
    }
}

/// Flags under which condition `cc` is `wanted` (None: never, e.g. F).
fn ccr_for(cc: u8, wanted: bool) -> Option<u8> {
    (0..16).find(|&ccr| holds(cc, ccr) == wanted)
}

/// Run `words` (the opcode at `addr`, then its extension words) once and
/// return the cycles taken. All data registers hold `d`, except that
/// `counter` sets one of them (DBcc).
fn run(addr: u32, words: &[u16], ccr: u8, counter: Option<(u8, u32)>) -> u32 {
    let mut bus = Sandbox::default();
    for (i, w) in words.iter().enumerate() {
        bus.write_word(addr + 2 * i as u32, *w);
    }
    let mut cpu = M68k::new();
    cpu.set_address_errors(false);
    cpu.set_sr(0x2700 | u16::from(ccr));
    for i in 0..8 {
        cpu.d[i] = 0;
        cpu.a[i] = AREG_BASE + 0x100 * i as u32;
    }
    cpu.set_ssp(AREG_BASE + 0x800);
    if let Some((r, v)) = counter {
        cpu.d[usize::from(r)] = v;
    }
    cpu.set_pc(&mut bus, addr);
    cpu.step(&mut bus)
}

/// The operand fetch of DIVU/DIVS: MULU/MULS with the same operand take
/// that, 38 cycles and their multiplier's part.
fn division_operand_cycles(addr: u32, words: &[u16], signed: bool, imm: u32) -> u32 {
    let mut twin = words.to_vec();
    twin[0] = 0xC000 | (words[0] & 0x0FFF); // DIVx <ea>,Dn -> MULx <ea>,Dn
    run(addr, &twin, 0, None).saturating_sub(38 + multiplier_cycles(signed, imm))
}

/// The run-time part MULU/MULS add for a multiplier (see the runtime).
fn multiplier_cycles(signed: bool, m: u32) -> u32 {
    let m = m & 0xFFFF;
    if signed {
        2 * (((m << 1) ^ (m << 2)) & 0x1_FFFE).count_ones()
    } else {
        2 * m.count_ones()
    }
}

/// Fill in `ins.cycles` (and `ins.cycles_alt` for the instructions with
/// several outcomes). `read16` reads the program, as for the decoder.
pub fn measure(ins: &mut Instruction, mut read16: impl FnMut(u32) -> Option<u16>) {
    let words: Vec<u16> = (0..ins.length / 2)
        .map(|k| read16(ins.addr + 2 * k).unwrap_or(0))
        .collect();
    let at = |ccr| run(ins.addr, &words, ccr, None);
    match ins.mnemonic {
        "bcc" | "scc" => {
            let cc = ins.cond.unwrap_or(0);
            // [condition false, condition true]; T and F only have one.
            let f = ccr_for(cc, false).map(at);
            let t = ccr_for(cc, true).map(at);
            ins.cycles = f.or(t).unwrap_or(4);
            ins.cycles_alt[0] = t.or(f).unwrap_or(4);
        }
        "dbcc" => {
            let cc = ins.cond.unwrap_or(0);
            let r = ins.dst.map_or(0, |d| d.reg);
            // Condition true: no decrement, fall through.
            ins.cycles = ccr_for(cc, true).map_or(12, at);
            let ccr = ccr_for(cc, false).unwrap_or(0);
            // Counter not exhausted: loop. Exhausted (0 -> -1): fall through.
            ins.cycles_alt[0] = run(ins.addr, &words, ccr, Some((r, 5)));
            ins.cycles_alt[1] = run(ins.addr, &words, ccr, Some((r, 0)));
        }
        "divu" | "divs" | "mulu" | "muls" => {
            // Registers and memory hold 0 in the sandbox; only an immediate
            // operand does not.
            let imm = ins.src.filter(|s| s.mode == Mode::Imm).map_or(0, |s| s.imm);
            let signed = ins.mnemonic.ends_with('s');
            ins.cycles = if ins.mnemonic.starts_with("div") {
                division_operand_cycles(ins.addr, &words, signed, imm)
            } else {
                at(0) - multiplier_cycles(signed, imm)
            };
        }
        "chk" => {
            // In bounds: CMP.W with the same operand, plus 6. (Measured
            // through the twin, because a negative immediate bound would
            // make every run trap.)
            let mut twin = words.clone();
            twin[0] = 0xB040 | (words[0] & 0x0E3F); // CHK <ea>,Dn -> CMP.W <ea>,Dn
            ins.cycles = run(ins.addr, &twin, 0, None) + 6;
        }
        "trap" | "illegal" | "linea" | "linef" => {
            ins.cycles = at(0).saturating_sub(EXCEPTION_CYCLES);
        }
        // Shifts by a register run with a count of 0, bit operations on bit
        // 0: the base cost. TRAPV runs without its exception (V = 0).
        _ => ins.cycles = at(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m68k::decode;

    fn timed(words: &[u16]) -> Instruction {
        let read = |a: u32| words.get((a / 2) as usize).copied();
        let mut ins = decode(read, 0).unwrap();
        measure(&mut ins, read);
        ins
    }

    /// Values from the 68000 user's manual, section 8.
    #[test]
    fn manual_timings() {
        assert_eq!(timed(&[0x7005]).cycles, 4); // moveq
        assert_eq!(timed(&[0x4E71]).cycles, 4); // nop
        assert_eq!(timed(&[0x3080]).cycles, 8); // move.w d0,(a0)
        assert_eq!(timed(&[0x5280]).cycles, 8); // addq.l #1,d0
        assert_eq!(timed(&[0x41F9, 0x00FF, 0x0000]).cycles, 12); // lea abs.l
        assert_eq!(timed(&[0xE348]).cycles, 8); // lsl.w #1,d0
        assert_eq!(timed(&[0xE368]).cycles, 6); // lsl.w d1,d0: + 2 per bit
        assert_eq!(timed(&[0xC0C1]).cycles, 38); // mulu d1,d0: + 2 per bit
        assert_eq!(timed(&[0x4DBC, 0xD2B3]).cycles, 14); // chk.w #-11597,d6, in bounds
        let bne = timed(&[0x6602]); // bne.s
        assert_eq!((bne.cycles, bne.cycles_alt[0]), (8, 10));
        let bne_w = timed(&[0x6600, 0x0002]); // bne.w
        assert_eq!((bne_w.cycles, bne_w.cycles_alt[0]), (12, 10));
        let dbra = timed(&[0x51C9, 0xFFFE]);
        assert_eq!(dbra.cycles_alt, [10, 14]);
        assert_eq!(dbra.cycles, 12); // never taken for F, but defined
    }
}
