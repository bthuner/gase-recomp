//! How much faster is native code? Two 68000 loops, run by gase's
//! interpreter and by the JIT from the same start: one on registers only,
//! one moving memory around like a game does.
//!
//! ```sh
//! cargo run --release -p gase-recomp-jit --example bench
//! ```

use std::time::Instant;

use gase_m68k::{Bus, M68k};
use gase_recomp_jit::Jit;
use gase_recomp_runtime::FlatMachine;

const ENTRY: u32 = 0x1000;
const DONE: u32 = 0x2000;

/// d7 outer iterations of 65 536 inner ones: arithmetic, logic, compares
/// and branches on registers only.
const REGISTERS: &[u16] = &[
    0x7E3F, // moveq #63,d7
    // outer:
    0x7CFF, //   moveq #-1,d6        (dbf runs 65 536 times)
    // inner:
    0xD081, //   add.l d1,d0
    0xB183, //   eor.l d0,d3
    0x5382, //   subq.l #1,d2
    0xB480, //   cmp.l d0,d2
    0x6302, //   bls.s +2
    0x5284, //   addq.l #1,d4
    0x51CE, 0xFFF2, // dbf d6,inner
    0x51CF, 0xFFEC, // dbf d7,outer
    0x4EF9, 0x0000, 0x2000, // jmp DONE
];

/// A copy-and-checksum loop over 8 KiB of work RAM, 256 times: loads,
/// stores, post-increments, address arithmetic.
const MEMORY: &[u16] = &[
    0x7EFF, // moveq #-1,d7 ... then
    0x3E3C, 0x00FF, // move.w #255,d7
    // outer:
    0x41F9, 0x00FF, 0x0000, // lea $FF0000,a0
    0x43F9, 0x00FF, 0x4000, // lea $FF4000,a1
    0x3C3C, 0x07FF, //  move.w #2047,d6
    // inner:
    0x2018, //   move.l (a0)+,d0
    0xD280, //   add.l d0,d1
    0x22C0, //   move.l d0,(a1)+
    0x3429, 0xFFFE, // move.w -2(a1),d2
    0xB440, //   cmp.w d0,d2
    0x51CE, 0xFFF2, // dbf d6,inner
    0x51CF, 0xFFDE, // dbf d7,outer
    0x4EF9, 0x0000, 0x2000, // jmp DONE
];

fn machine(program: &[u16]) -> FlatMachine {
    let mut m = FlatMachine::default();
    for (i, w) in program.iter().enumerate() {
        m.write_word(ENTRY + 2 * i as u32, *w);
    }
    m.write_word(DONE, 0x60FE); // bra.s *
    m
}

fn cpu(m: &mut FlatMachine) -> M68k {
    let mut cpu = M68k::new();
    cpu.set_sr(0x2700);
    cpu.d = [0, 3, 0x7FFF_0000, 0, 0, 0, 0, 0];
    cpu.a[7] = 0xFF_FF00;
    cpu.set_pc(m, ENTRY);
    cpu
}

fn bench(name: &str, program: &[u16]) {
    let mut m = machine(program);
    let mut interp = cpu(&mut m);
    let t = Instant::now();
    let mut cycles = 0u64;
    let mut instructions = 0u64;
    while interp.pc() != DONE {
        cycles += u64::from(interp.step(&mut m));
        instructions += 1;
    }
    let t_interp = t.elapsed().as_secs_f64();
    let ram = m.mem[0xFF_0000..].to_vec();

    let mut m = machine(program);
    let mut jitted = cpu(&mut m);
    let mut jit = Jit::new();
    let t = Instant::now();
    let end = jit.run(&mut jitted, &mut m, |st| st.pc == DONE);
    let t_jit = t.elapsed().as_secs_f64();

    assert_eq!(jitted.d, interp.d, "same registers");
    assert_eq!(jitted.a, interp.a, "same registers");
    assert_eq!(end.cycles, cycles, "same cycles");
    assert!(m.mem[0xFF_0000..] == ram[..], "same memory");
    let mhz = |secs: f64| cycles as f64 / secs / 1e6;
    println!(
        "{name}: {:.0} M instructions, {:.0} M cycles",
        instructions as f64 / 1e6,
        cycles as f64 / 1e6
    );
    println!(
        "  interpreter    {:>7.1} ms  {:>6.0} MHz of 68000",
        t_interp * 1e3,
        mhz(t_interp)
    );
    println!(
        "  JIT ({:6})   {:>7.1} ms  {:>6.0} MHz of 68000   {:.1}x",
        jit.backend_name(),
        t_jit * 1e3,
        mhz(t_jit),
        t_interp / t_jit
    );
}

fn main() {
    bench("registers", REGISTERS);
    bench("memory", MEMORY);
    println!("(a real Mega Drive: 7.67 MHz)");
}
