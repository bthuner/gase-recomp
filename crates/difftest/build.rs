//! Build the test program: random, valid 68000 snippets packed into a fake
//! ROM, then recompiled to Rust by the gase-recomp library itself.
//!
//! `GASE_DIFFTEST_SEED` and `GASE_DIFFTEST_CASES` change the program.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use gase_recomp::analysis::analyze;
use gase_recomp::codegen::{Options, generate_single};
use gase_recomp::m68k::{Flow, Mode, decode};
use gase_recomp::rom::Rom;

include!("src/layout.rs");

/// Instructions we leave out: they change the CPU mode (SR writes would
/// switch stacks under our feet) or never come back.
const EXCLUDED: &[&str] = &[
    "stop",
    "reset",
    "illegal",
    "linea",
    "linef",
    "move_to_sr",
    "andi_sr",
    "ori_sr",
    "eori_sr",
];

fn random_instruction(rng: &mut Rng) -> Vec<u16> {
    loop {
        let mut words = [0u16; 5];
        for w in &mut words {
            *w = rng.next() as u16;
        }
        // brief extension words must have bits 8-10 clear on a 68000
        for w in &mut words[1..4] {
            if rng.next() & 1 == 0 {
                *w &= 0xF8FF;
            }
        }
        let Ok(ins) = decode(|a| words.get((a / 2) as usize).copied(), 0) else {
            continue;
        };
        if !matches!(ins.flow, Flow::Next | Flow::Trap) || EXCLUDED.contains(&ins.mnemonic) {
            continue;
        }
        // keep absolute accesses away from the code
        let low_abs = [ins.src, ins.dst]
            .into_iter()
            .flatten()
            .any(|o| matches!(o.mode, Mode::AbsW | Mode::AbsL) && o.addr & 0xFF_FFFF < RAM);
        if low_abs {
            continue;
        }
        return words[..(ins.length / 2) as usize].to_vec();
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=GASE_DIFFTEST_SEED");
    println!("cargo:rerun-if-env-changed=GASE_DIFFTEST_CASES");
    println!("cargo:rerun-if-changed=src/layout.rs");
    let seed: u32 = env::var("GASE_DIFFTEST_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let cases: u32 = env::var("GASE_DIFFTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500);
    let mut rng = Rng::new(seed);

    let mut low = vec![0u8; 0x1_0000];
    low[0..4].copy_from_slice(&0x00FF_FF00u32.to_be_bytes());
    for v in 1..64 {
        low[v * 4..v * 4 + 4].copy_from_slice(&HANDLER.to_be_bytes());
    }
    low[HANDLER as usize..HANDLER as usize + 2].copy_from_slice(&0x4E73u16.to_be_bytes()); // RTE
    low[0x100..0x110].copy_from_slice(b"SEGA DIFFTEST   ");
    let mut entries = BTreeMap::new();
    for i in 0..cases.min(MAX_CASES) {
        let addr = CODE_BASE + i * SLOT;
        let mut pos = addr as usize;
        let count = 1 + rng.next() % MAX_LEN;
        for _ in 0..count {
            for w in random_instruction(&mut rng) {
                low[pos..pos + 2].copy_from_slice(&w.to_be_bytes());
                pos += 2;
            }
        }
        low[pos..pos + 2].copy_from_slice(&0x4E75u16.to_be_bytes()); // RTS
        entries.insert(addr, format!("case_{i}"));
    }
    let rom = Rom::from_bytes(&low).expect("test image");
    let program = analyze(&rom, &entries);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("low.bin"), &low).unwrap();
    // Tiny blocks and chunks, so that every snippet also exercises the
    // chunked form used for big functions.
    let opts = Options {
        max_block: 1,
        chunk: 2,
        ..Options::default()
    };
    fs::write(out.join("recomp.rs"), generate_single(&program, &opts)).unwrap();
    let list: Vec<String> = entries.keys().map(|a| format!("0x{a:X}")).collect();
    fs::write(
        out.join("cases.rs"),
        format!(
            "pub const CASES: &[u32] = &[{}];\npub const SEED: u32 = {seed};\n",
            list.join(", ")
        ),
    )
    .unwrap();
}
