//! Build a synthetic ROM with a lot of random (valid) 68000 code, to
//! measure how long rustc takes on game-sized generated code.
//!
//! ```sh
//! cargo run --release -p gase-recomp --example synth_rom -- big.bin 500 300
//! ```
//! (500 functions of ~300 instructions: ~150k instructions, the code size
//! of a large Mega Drive game.)

use gase_recomp::m68k::{Flow, Mode, decode};

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

fn random_instruction(rng: &mut Rng) -> Vec<u16> {
    loop {
        let mut w = [0u16; 5];
        for x in &mut w {
            *x = rng.next() as u16;
        }
        for x in &mut w[1..4] {
            *x &= 0xF8FF;
        }
        let Ok(i) = decode(|a| w.get((a / 2) as usize).copied(), 0) else {
            continue;
        };
        let abs_low = [i.src, i.dst]
            .into_iter()
            .flatten()
            .any(|o| matches!(o.mode, Mode::AbsW | Mode::AbsL));
        if i.flow == Flow::Next
            && !abs_low
            && !matches!(
                i.mnemonic,
                "stop" | "reset" | "move_to_sr" | "andi_sr" | "ori_sr" | "eori_sr"
            )
        {
            return w[..(i.length / 2) as usize].to_vec();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().cloned().unwrap_or_else(|| "synth.bin".into());
    let nfuncs: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500);
    let len: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let mut rng = Rng(0x1234_5678);
    let mut code: Vec<u16> = Vec::new();
    let base = 0x200u32;
    // reset: jsr every function, then loop forever
    let call_area = nfuncs * 3 + 2;
    let mut func_addrs = Vec::new();
    let mut body: Vec<u16> = Vec::new();
    let mut addr = base + 2 * call_area as u32;
    for _ in 0..nfuncs {
        func_addrs.push(addr);
        let mut f: Vec<u16> = Vec::new();
        for k in 0..len {
            if k % 25 == 24 {
                f.extend([0x6602, 0x4E71]); // bne.s +2 ; nop  (a forward branch)
            } else {
                f.extend(random_instruction(&mut rng));
            }
        }
        f.push(0x4E75); // rts
        addr += 2 * f.len() as u32;
        body.extend(f);
    }
    for a in &func_addrs {
        code.extend([0x4EB9, (a >> 16) as u16, *a as u16]); // jsr abs.l
    }
    code.extend([0x60FE, 0x4E71]); // bra.s * ; nop
    code.extend(body);
    let mut rom = vec![0u8; base as usize];
    rom[0..4].copy_from_slice(&0x00FF_FE00u32.to_be_bytes());
    rom[4..8].copy_from_slice(&base.to_be_bytes());
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x150..0x160].copy_from_slice(b"SYNTHETIC BENCH ");
    for w in code {
        rom.extend(w.to_be_bytes());
    }
    std::fs::write(&out, &rom).unwrap();
    println!(
        "{out}: {} bytes, {} functions x ~{len} instructions",
        rom.len(),
        nfuncs
    );
}
