//! Differential testing: every random snippet built by `build.rs` runs on
//! the recompiled code *and* on gase's 68000 interpreter (validated
//! against ~2.6 million hardware test vectors), from the same random
//! state. Registers, flags, exception frames and RAM must match exactly.
//!
//! ```sh
//! cargo test -p gase-recomp-difftest
//! GASE_DIFFTEST_SEED=7 GASE_DIFFTEST_CASES=2000 cargo test -p gase-recomp-difftest
//! ```

#![allow(dead_code)]

use gase_m68k::{Bus, M68k};
use gase_recomp_runtime::{Cx, FlatMachine};

include!("layout.rs");
include!(concat!(env!("OUT_DIR"), "/cases.rs"));

/// The machine the generated code runs on.
pub type Machine = FlatMachine;

#[allow(clippy::all, unused_parens)]
mod recomp {
    include!(concat!(env!("OUT_DIR"), "/recomp.rs"));
}

/// Return address pushed before each snippet; reaching it ends the case.
const SENTINEL: u32 = 0xF00;

/// What we compare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub pc: u32,
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub usp: u32,
    pub sr: u16,
    pub ram: Vec<u8>,
}

/// A random starting point: interesting data values, pointers into RAM.
fn initial(rng: &mut Rng) -> ([u32; 8], [u32; 8], u16) {
    let mut d = [0; 8];
    for v in &mut d {
        let k = rng.next() % 10;
        *v = match k {
            0..=2 => rng.next(),
            3..=5 => rng.next() % 40,
            6..=7 => [
                0,
                1,
                0x7F,
                0x80,
                0xFF,
                0x7FFF,
                0x8000,
                0xFFFF,
                0x7FFF_FFFF,
                0x8000_0000,
                0xFFFF_FFFF,
                0x99,
                0x09,
            ][(rng.next() % 13) as usize],
            _ => rng.next() & 0xFFFF,
        };
    }
    let mut a = [0; 8];
    for v in &mut a[..7] {
        *v = RAM + ((0x1000 + rng.next() % 0xE000) & !1);
    }
    a[7] = 0xFF_FF00;
    (d, a, 0x2700 | (rng.next() % 32) as u16)
}

fn fresh(mem: &mut FlatMachine, low: &[u8], ram: &[u8]) {
    mem.mem[..0x1_0000].copy_from_slice(low);
    mem.mem[RAM as usize..].copy_from_slice(ram);
}

fn snapshot(pc: u32, d: [u32; 8], a: [u32; 8], usp: u32, sr: u16, m: &FlatMachine) -> State {
    State {
        pc,
        d,
        a,
        usp,
        sr,
        ram: m.mem[RAM as usize..].to_vec(),
    }
}

/// Run every case on both CPUs; return the mismatches as readable text.
#[must_use]
pub fn run() -> (usize, Vec<String>) {
    let low = include_bytes!(concat!(env!("OUT_DIR"), "/low.bin"));
    let mut rng = Rng::new(SEED ^ 0xABCD);
    let ram: Vec<u8> = (0..0x1_0000).map(|_| rng.next() as u8).collect();

    let mut cx = Cx::new(FlatMachine::default(), recomp::code());
    cx.missing_log = None;
    let mut reference = FlatMachine::default();
    let mut cpu = M68k::new();
    cpu.set_address_errors(false);

    let (mut compared, mut failures) = (0, Vec::new());
    for &entry in CASES {
        let (d, a, sr) = initial(&mut rng);

        // the reference: gase's interpreter, one instruction at a time
        fresh(&mut reference, low, &ram);
        cpu.set_sr(0x2700);
        cpu.d = d;
        cpu.a = a;
        cpu.set_usp(RAM + 0x8000);
        cpu.set_sr(sr);
        cpu.a[7] -= 4;
        reference.write_word(cpu.a[7], (SENTINEL >> 16) as u16);
        reference.write_word(cpu.a[7] + 2, SENTINEL as u16);
        cpu.set_pc(&mut reference, entry);
        let mut valid = true;
        for _ in 0..64 {
            let pc = cpu.pc();
            if pc == SENTINEL {
                break;
            }
            // snippets that clobber their own return address wander off
            if !(entry..entry + SLOT).contains(&pc) && pc != HANDLER {
                valid = false;
                break;
            }
            cpu.step(&mut reference);
        }
        if !valid || cpu.pc() != SENTINEL || cpu.is_halted() {
            continue;
        }
        let expected = snapshot(cpu.pc(), cpu.d, cpu.a, cpu.usp(), cpu.sr(), &reference);

        // the recompiled code
        fresh(&mut cx.m, low, &ram);
        cx.s = true;
        cx.set_sr(0x2700);
        cx.d = d;
        cx.a = a;
        cx.set_usp(RAM + 0x8000);
        cx.set_sr(u32::from(sr));
        cx.push32(SENTINEL);
        cx.pc = entry;
        cx.dispatch();
        assert!(
            cx.missing.is_empty(),
            "case {entry:#X} fell back to the interpreter"
        );
        let got = snapshot(cx.pc, cx.d, cx.a, cx.usp(), cx.sr(), &cx.m);

        compared += 1;
        if got != expected {
            let mut diff = Vec::new();
            for i in 0..8 {
                if got.d[i] != expected.d[i] {
                    diff.push(format!(
                        "d{i}: gase={:08X} recomp={:08X}",
                        expected.d[i], got.d[i]
                    ));
                }
                if got.a[i] != expected.a[i] {
                    diff.push(format!(
                        "a{i}: gase={:08X} recomp={:08X}",
                        expected.a[i], got.a[i]
                    ));
                }
            }
            if got.sr != expected.sr {
                diff.push(format!(
                    "sr: gase={:04X} recomp={:04X}",
                    expected.sr, got.sr
                ));
            }
            if got.pc != expected.pc {
                diff.push(format!(
                    "pc: gase={:06X} recomp={:06X}",
                    expected.pc, got.pc
                ));
            }
            if got.usp != expected.usp {
                diff.push(format!(
                    "usp: gase={:08X} recomp={:08X}",
                    expected.usp, got.usp
                ));
            }
            if let Some(i) = (0..got.ram.len()).find(|&i| got.ram[i] != expected.ram[i]) {
                diff.push(format!(
                    "ram[{:06X}]: gase={:02X} recomp={:02X}",
                    RAM as usize + i,
                    expected.ram[i],
                    got.ram[i]
                ));
            }
            let mut listing = Vec::new();
            let mut pc = entry;
            loop {
                let (text, len) = gase_m68k::disasm::disassemble(pc, |x| {
                    u16::from_be_bytes([
                        low[(x & 0xFFFF) as usize],
                        low[((x + 1) & 0xFFFF) as usize],
                    ])
                });
                if text.starts_with("rts") {
                    break;
                }
                listing.push(text);
                pc += len;
            }
            failures.push(format!(
                "{entry:#06X} [{}] d={d:08X?} sr={sr:04X}\n    {}",
                listing.join("; "),
                diff.join("\n    ")
            ));
        }
    }
    (compared, failures)
}

#[cfg(test)]
mod tests {
    #[test]
    fn recompiled_code_matches_the_gase_interpreter() {
        let (compared, failures) = super::run();
        assert!(compared > 100, "only {compared} cases compared");
        assert!(
            failures.is_empty(),
            "{} of {compared} cases differ:\n{}",
            failures.len(),
            failures[..failures.len().min(25)].join("\n")
        );
    }
}
