//! The JIT against gase's interpreter: random snippets run on both from
//! the same state; registers, flags, stack, memory and cycles must match.
//!
//! Three kinds of snippets: any valid instruction (mostly interpreted by
//! the JIT, so this checks the hand-over), register-only instructions
//! (what the backend compiles natively), and little branches and loops.
//!
//! ```sh
//! cargo test -p gase-recomp-jit
//! GASE_JIT_SEED=7 GASE_JIT_CASES=20000 cargo test -p gase-recomp-jit
//! ```

use gase_m68k::{Bus, M68k};
use gase_recomp::m68k::{Flow, Mode, decode};
use gase_recomp_jit::Jit;
use gase_recomp_runtime::FlatMachine;

/// Every exception vector points here: a lone RTE.
const HANDLER: u32 = 0x800;
/// Snippets start here (in "ROM", where the JIT compiles).
const ENTRY: u32 = 0x1000;
/// Return address pushed before each snippet: reaching it ends the case.
/// In RAM, above the stack: the JIT only compiles ROM, so it never runs
/// past it (compiled code may loop without coming back to the engine).
const SENTINEL: u32 = 0xFF_FFF0;
/// Work RAM, where the address registers point.
const RAM: u32 = 0xFF_0000;

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9) | 1)
    }
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// Instructions left out: they change the CPU mode under our feet or
/// never come back.
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
    "move_usp",
];

/// Any valid straight-line instruction, avoiding absolute addresses in the
/// code area.
fn any_instruction(rng: &mut Rng) -> Vec<u16> {
    loop {
        let mut words = [0u16; 5];
        for w in &mut words {
            *w = rng.next() as u16;
        }
        for w in &mut words[1..4] {
            if rng.next() & 1 == 0 {
                *w &= 0xF8FF; // valid 68000 brief extension words
            }
        }
        let Ok(ins) = decode(|a| words.get((a / 2) as usize).copied(), 0) else {
            continue;
        };
        if !matches!(ins.flow, Flow::Next | Flow::Trap) || EXCLUDED.contains(&ins.mnemonic) {
            continue;
        }
        let low_abs = [ins.src, ins.dst]
            .into_iter()
            .flatten()
            .any(|o| matches!(o.mode, Mode::AbsW | Mode::AbsL) && o.addr & 0xFF_FFFF < RAM);
        if !low_abs {
            return words[..(ins.length / 2) as usize].to_vec();
        }
    }
}

/// A register-only instruction of the kinds the backend compiles.
fn register_instruction(rng: &mut Rng) -> Vec<u16> {
    let (rx, ry) = (rng.below(8) as u16, rng.below(8) as u16);
    let size = rng.below(3) as u16; // 0 byte, 1 word, 2 long
    let imm = |rng: &mut Rng| -> Vec<u16> {
        match size {
            0 => vec![rng.next() as u16 & 0xFF],
            1 => vec![rng.next() as u16],
            _ => vec![(rng.next() >> 16) as u16, rng.next() as u16],
        }
    };
    let move_size = [1u16, 3, 2][size as usize]; // MOVE's own size field
    match rng.below(10) {
        0 => vec![0x7000 | rx << 9 | (rng.next() as u16 & 0xFF)], // moveq
        1 => vec![move_size << 12 | rx << 9 | ry],                // move Dy,Dx
        2 => [vec![move_size << 12 | rx << 9 | 0x3C], imm(rng)].concat(), // move #,Dx
        3 => {
            // add/sub/and/or/cmp Dy,Dx; eor Dx,Dy
            let line = [0xD000u16, 0x9000, 0xC000, 0x8000, 0xB000, 0xB100][rng.below(6) as usize];
            // (EOR's direction bit is already in its line value.)
            vec![line | rx << 9 | size << 6 | ry]
        }
        4 => {
            vec![0x5000 | (rng.below(8) as u16) << 9 | (rng.below(2) as u16) << 8 | size << 6 | ry]
        } // addq/subq
        5 => {
            // ori/andi/subi/addi/eori/cmpi #,Dy
            let base = [0x0000u16, 0x0200, 0x0400, 0x0600, 0x0A00, 0x0C00][rng.below(6) as usize];
            [vec![base | size << 6 | ry], imm(rng)].concat()
        }
        6 => vec![0x4A00 | size << 6 | ry],           // tst
        7 => vec![0x4200 | size << 6 | ry],           // clr
        8 => vec![0x7000 | rx << 9],                  // moveq #0
        _ => vec![0xB000 | rx << 9 | size << 6 | ry], // cmp
    }
}

/// A forward Bcc over one moveq, or a short DBcc loop.
fn branch(rng: &mut Rng) -> Vec<u16> {
    let cc = rng.below(16) as u16;
    if rng.next() & 1 == 0 {
        let cc = if cc == 1 { 0 } else { cc }; // not BSR
        vec![0x6002 | cc << 8, 0x7000 | (rng.below(8) as u16) << 9 | 0x55]
    } else {
        // moveq #n,d7 ; loop: addq.l #1,d0 ; dbcc d7,loop
        let n = rng.below(6) as u16;
        vec![0x7E00 | n, 0x5280, 0x50C8 | cc << 8 | 7, 0xFFFC]
    }
}

fn snippet(rng: &mut Rng) -> Vec<u16> {
    let mut code = Vec::new();
    for _ in 0..1 + rng.below(6) {
        code.extend(match rng.below(4) {
            0 => any_instruction(rng),
            1 => branch(rng),
            _ => register_instruction(rng),
        });
    }
    code.push(0x4E75); // rts
    code
}

/// A random starting point, as in the static recompiler's difftest.
fn initial(rng: &mut Rng) -> ([u32; 8], [u32; 8], u16) {
    let mut d = [0; 8];
    for v in &mut d {
        *v = match rng.below(10) {
            0..=2 => rng.next(),
            3..=5 => rng.below(40),
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
            ][rng.below(11) as usize],
            _ => rng.next() & 0xFFFF,
        };
    }
    let mut a = [0; 8];
    for v in &mut a[..7] {
        *v = RAM + ((0x1000 + rng.below(0xE000)) & !1);
    }
    a[7] = 0xFF_FF00;
    (d, a, 0x2700 | rng.below(32) as u16)
}

fn machine(code: &[u16], ram: &[u8]) -> FlatMachine {
    let mut m = FlatMachine::default();
    for v in 0..64u32 {
        m.mem[(v * 4) as usize..(v * 4 + 4) as usize].copy_from_slice(&HANDLER.to_be_bytes());
    }
    m.write_word(HANDLER, 0x4E73); // rte
    for (i, w) in code.iter().enumerate() {
        m.write_word(ENTRY + 2 * i as u32, *w);
    }
    m.mem[RAM as usize..].copy_from_slice(ram);
    m
}

fn cpu(m: &mut FlatMachine, (d, a, sr): ([u32; 8], [u32; 8], u16)) -> M68k {
    let mut cpu = M68k::new();
    cpu.set_address_errors(false);
    cpu.set_sr(0x2700);
    cpu.d = d;
    cpu.a = a;
    cpu.set_usp(RAM + 0x8000);
    cpu.set_sr(sr);
    cpu.a[7] -= 4;
    m.write_word(cpu.a[7], (SENTINEL >> 16) as u16);
    m.write_word(cpu.a[7] + 2, SENTINEL as u16);
    cpu.set_pc(m, ENTRY);
    cpu
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    d: [u32; 8],
    a: [u32; 8],
    sr: u16,
    usp: u32,
    pc: u32,
    cycles: u64,
}

fn outcome(cpu: &M68k, cycles: u64) -> Outcome {
    Outcome {
        d: cpu.d,
        a: cpu.a,
        sr: cpu.sr(),
        usp: cpu.usp(),
        pc: cpu.pc(),
        cycles,
    }
}

#[test]
fn the_jit_matches_the_interpreter() {
    let seed = std::env::var("GASE_JIT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let cases: u32 = std::env::var("GASE_JIT_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let mut rng = Rng::new(seed);
    let ram: Vec<u8> = (0..0x1_0000).map(|_| rng.next() as u8).collect();
    let (mut compared, mut failures) = (0, Vec::new());
    let mut jit_stats = gase_recomp_jit::Stats::default();

    for case in 0..cases {
        let code = snippet(&mut rng);
        let start = initial(&mut rng);

        // The reference: gase's interpreter, one instruction at a time.
        let mut m = machine(&code, &ram);
        let mut reference = cpu(&mut m, start);
        let mut cycles = 0u64;
        let mut steps = 0;
        while reference.pc() != SENTINEL && steps < 5000 && !reference.is_halted() {
            cycles += u64::from(reference.step(&mut m));
            steps += 1;
        }
        if reference.pc() != SENTINEL {
            continue; // wandered off (a clobbered return address)
        }
        let expected = (outcome(&reference, cycles), m.mem[RAM as usize..].to_vec());

        // The JIT, a fresh one per case (its cache is per ROM).
        let mut m = machine(&code, &ram);
        let mut jitted = cpu(&mut m, start);
        let mut jit = Jit::new();
        let mut rounds = 0;
        let end = jit.run(&mut jitted, &mut m, |st| {
            rounds += 1;
            st.pc == SENTINEL || rounds > 10_000
        });
        let got = (outcome(&jitted, end.cycles), m.mem[RAM as usize..].to_vec());
        jit_stats.blocks_compiled += jit.stats.blocks_compiled;
        jit_stats.blocks_run += jit.stats.blocks_run;
        compared += 1;
        if got != expected {
            let ram_diff = (0..got.1.len()).find(|&i| got.1[i] != expected.1[i]);
            failures.push(format!(
                "case {case}: code {code:04X?} start {start:08X?}\n    interpreter {:08X?}\n    jit         {:08X?}\n    first RAM difference: {ram_diff:X?}",
                expected.0, got.0
            ));
        }
    }
    println!("{compared} cases, JIT {jit_stats:?}");
    assert!(compared > cases / 2, "only {compared} cases compared");
    assert!(
        failures.is_empty(),
        "{} of {compared} cases differ:\n{}",
        failures.len(),
        failures[..failures.len().min(10)].join("\n")
    );
}
