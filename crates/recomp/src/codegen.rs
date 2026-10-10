//! Translate analysed 68000 functions into Rust.
//!
//! Rust has no `goto`, so each function becomes a state machine over its
//! **blocks** (runs of instructions with no label inside):
//!
//! ```text
//! pub fn f_000200(cx: &mut Cx) {
//!     let mut pc = cx.pc;
//!     loop {
//!         match pc {
//!             0x00027E => {
//!                 // 00027E  4A39 00FF 0000    tst.b ($ff0000).l
//!                 cx.cycles += 16;
//!                 let ea_d = 0xFF0000;
//!                 let v = cx.r8(ea_d);
//!                 cx.logic(v, 1);
//!                 // 000284  67F8              beq $27e
//!                 cx.cycles += 10;
//!                 if cx.cond(7) { if !cx.poll(0x27E) { return; } pc = 0x27E; continue; }
//!                 pc = 0x000286;
//!             }
//!             ...
//!             _ => { cx.pc = pc; return; }
//!         }
//!     }
//! }
//! ```
//!
//! A branch is `pc = TARGET; continue`, which LLVM compiles back into a
//! direct jump. Any address that is not one of the function's blocks
//! leaves through the `_` arm, back to the dispatcher. The blocks are also
//! where the dispatcher can *enter* a function, which is what makes
//! frame-by-frame execution possible (see the runtime's `Cx::run_frame`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::analysis::{Function, Program};
use crate::m68k::{Flow, Instruction, Mode, Operand};

#[must_use]
pub fn fn_name(addr: u32) -> String {
    format!("f_{addr:06X}")
}

fn hex(v: u32) -> String {
    format!("0x{v:X}")
}

fn mask(sz: u8) -> &'static str {
    match sz {
        1 => "0xFF",
        2 => "0xFFFF",
        _ => "0xFFFF_FFFF",
    }
}

/// `base + disp`, written readably.
fn plus(base: &str, disp: i32) -> String {
    match disp {
        0 => base.to_string(),
        d if d > 0 => format!("{base}.wrapping_add({d})"),
        d => format!("{base}.wrapping_sub({})", d.unsigned_abs()),
    }
}

/// `cx.cond(7 /* eq */)`: the condition number, named for the reader.
fn cond(cc: Option<u8>) -> String {
    let cc = cc.unwrap();
    format!(
        "cx.cond({cc} /* {} */)",
        crate::m68k::CONDITIONS[usize::from(cc)]
    )
}

fn index_reg(op: &Operand) -> String {
    let reg = format!("cx.{}[{}]", if op.xa { 'a' } else { 'd' }, op.xreg);
    if op.xlong {
        reg
    } else {
        format!("sext16({reg})")
    }
}

/// An operand being accessed: `setup` computes its address once (applying
/// `(An)+` / `-(An)` side effects), then `read` / `write` use it.
struct Ea {
    op: Operand,
    size: u8,
    var: &'static str,
}

impl Ea {
    fn new(op: Operand, size: u8, var: &'static str) -> Self {
        Self { op, size, var }
    }

    /// A byte push/pop on A7 moves it by 2, to keep the stack word-aligned.
    fn step(&self) -> u8 {
        if self.size == 1 && self.op.reg == 7 {
            2
        } else {
            self.size
        }
    }

    /// The address of a memory operand, without side effects.
    fn address(&self) -> String {
        let op = &self.op;
        match op.mode {
            Mode::Ind => format!("cx.a[{}]", op.reg),
            Mode::Disp => plus(&format!("cx.a[{}]", op.reg), op.disp),
            Mode::Index => plus(
                &format!("cx.a[{}].wrapping_add({})", op.reg, index_reg(op)),
                op.disp,
            ),
            Mode::AbsW | Mode::AbsL | Mode::PcDisp => hex(op.addr), // address registers keep all 32 bits
            Mode::PcIndex => format!("{}u32.wrapping_add({})", hex(op.addr), index_reg(op)),
            m => panic!("no address for {m:?}"),
        }
    }

    fn setup(&self, out: &mut Vec<String>) {
        let (r, v) = (self.op.reg, self.var);
        match self.op.mode {
            Mode::PostInc => {
                out.push(format!("let {v} = cx.a[{r}];"));
                out.push(format!(
                    "cx.a[{r}] = cx.a[{r}].wrapping_add({});",
                    self.step()
                ));
            }
            Mode::PreDec => {
                out.push(format!(
                    "cx.a[{r}] = cx.a[{r}].wrapping_sub({});",
                    self.step()
                ));
                out.push(format!("let {v} = cx.a[{r}];"));
            }
            Mode::Dn | Mode::An | Mode::Imm | Mode::Sr | Mode::Ccr | Mode::Usp => {}
            _ => out.push(format!("let {v} = {};", self.address())),
        }
    }

    fn read(&self) -> String {
        let (op, sz) = (&self.op, self.size);
        match op.mode {
            Mode::Dn if sz == 4 => format!("cx.d[{}]", op.reg),
            Mode::Dn => format!("(cx.d[{}] & {})", op.reg, mask(sz)),
            Mode::An if sz == 4 => format!("cx.a[{}]", op.reg),
            Mode::An => format!("(cx.a[{}] & {})", op.reg, mask(sz)),
            Mode::Imm => hex(match sz {
                1 => op.imm & 0xFF,
                2 => op.imm & 0xFFFF,
                _ => op.imm,
            }),
            _ => format!("cx.r{}({})", u32::from(sz) * 8, self.var),
        }
    }

    fn write(&self, value: &str) -> String {
        let (op, sz) = (&self.op, self.size);
        match op.mode {
            Mode::Dn => match sz {
                1 => format!("cx.set_d8({}, {value});", op.reg),
                2 => format!("cx.set_d16({}, {value});", op.reg),
                _ => format!("cx.d[{}] = {value};", op.reg),
            },
            Mode::An => format!("cx.a[{}] = {value};", op.reg),
            Mode::Imm | Mode::PcDisp | Mode::PcIndex => panic!("write to a read-only operand"),
            _ => format!("cx.w{}({}, {value});", u32::from(sz) * 8, self.var),
        }
    }
}

const ALLOW: &str = "#[allow(non_snake_case, unused_mut, unused_variables, unused_assignments, unused_parens, unreachable_code, clippy::all)]\n";

struct Emitter<'a> {
    program: &'a Program,
    f: &'a Function,
    blocks: BTreeSet<u32>,
    opts: &'a Options,
}

impl<'a> Emitter<'a> {
    fn in_fn(&self, a: u32) -> bool {
        self.f.instructions.contains_key(&a)
    }

    /// Every address where a block must start: entry, branch targets,
    /// resume points after calls and exceptions, poll resume points...
    fn find_blocks(&mut self) {
        let f = self.f;
        let order: Vec<u32> = f.instructions.keys().copied().collect();
        let mut b = BTreeSet::from([f.entry]);
        for (i, &addr) in order.iter().enumerate() {
            let ins = &f.instructions[&addr];
            let next = order.get(i + 1).copied();
            if let (Flow::Branch | Flow::Cond, Some(t)) = (ins.flow, ins.target) {
                b.insert(t & 0xFF_FFFF);
            }
            b.extend(f.jump_tables.get(&addr).into_iter().flatten());
            let resumes = matches!(ins.flow, Flow::Call | Flow::Trap)
                || matches!(
                    ins.mnemonic,
                    "andi_sr" | "ori_sr" | "eori_sr" | "move_to_sr" | "stop"
                );
            if resumes {
                b.insert(ins.next_addr());
            }
            if matches!(ins.mnemonic, "stop" | "illegal" | "linea" | "linef") {
                b.insert(addr);
            }
            let falls = matches!(ins.flow, Flow::Next | Flow::Cond | Flow::Call | Flow::Trap);
            if (falls && next != Some(ins.next_addr())) || (!falls && next.is_some()) {
                // a discontinuity: whatever comes next starts a block
                b.extend(next);
                b.insert(ins.next_addr());
            }
        }
        // Cut long straight-line runs: LLVM's memory analyses get slow on
        // huge blocks full of loads and stores (see the making-of).
        let mut run = 0;
        for &addr in &order {
            run = if b.contains(&addr) { 1 } else { run + 1 };
            if run > self.opts.max_block {
                b.insert(addr);
                run = 1;
            }
        }
        self.blocks = b.into_iter().filter(|a| self.in_fn(*a)).collect();
    }

    /// Jump to a static target, polling on loops.
    fn goto(&self, target: u32, here: u32) -> String {
        let t = target & 0xFF_FFFF;
        if !self.in_fn(t) {
            return format!("cx.pc = {}; return;", hex(t));
        }
        if t <= here {
            format!("if !cx.poll({0}) {{ return; }} pc = {0}; continue;", hex(t))
        } else {
            format!("pc = {}; continue;", hex(t))
        }
    }

    fn call(&self, target: u32, ret: u32) -> String {
        let t = target & 0xFF_FFFF;
        if self.program.functions.contains_key(&t) {
            format!(
                "if !cx.call({}, {}, {}) {{ return; }}",
                fn_name(t),
                hex(t),
                hex(ret)
            )
        } else {
            format!(
                "if !cx.call_indirect({}, {}) {{ return; }}",
                hex(t),
                hex(ret)
            )
        }
    }

    /// The `match` arms: one per block, with its number of instructions.
    fn arms(&self) -> Vec<(u32, String, usize)> {
        let f = self.f;
        let order: Vec<u32> = f.instructions.keys().copied().collect();
        let mut arms: Vec<(u32, String, usize)> = Vec::new();
        for (i, &addr) in order.iter().enumerate() {
            let ins = &f.instructions[&addr];
            if self.blocks.contains(&addr) || arms.is_empty() {
                arms.push((addr, String::new(), 0));
            }
            let (_, o, count) = arms.last_mut().unwrap();
            *count += 1;
            let raw: Vec<String> = (0..ins.length / 2)
                .filter_map(|k| self.program.rom.u16(addr + 2 * k))
                .map(|w| format!("{w:04X}"))
                .collect();
            let _ = writeln!(
                o,
                "                // {addr:06X}  {:<20} {}",
                raw.join(" "),
                ins.text()
            );
            let _ = writeln!(o, "                cx.cycles += {};", ins.cycles);
            for line in self.instruction(ins) {
                let _ = writeln!(o, "                {line}");
            }
            // Leaving the block by falling through into the next one.
            let falls = matches!(ins.flow, Flow::Next | Flow::Cond | Flow::Call | Flow::Trap)
                || matches!(ins.mnemonic, "stop");
            let next = order.get(i + 1).copied();
            if falls
                && (next.is_none_or(|n| self.blocks.contains(&n)) || next != Some(ins.next_addr()))
            {
                let n = ins.next_addr();
                if self.in_fn(n) {
                    let _ = writeln!(o, "                pc = {};", hex(n));
                } else {
                    let _ = writeln!(
                        o,
                        "                cx.pc = {}; return; // undecodable",
                        hex(n)
                    );
                }
            }
        }
        arms
    }

    fn emit(mut self) -> String {
        self.find_blocks();
        let f = self.f;
        let name = fn_name(f.entry);
        let arms = self.arms();
        let mut o = String::new();
        let _ = writeln!(
            o,
            "/// `{}`: {} instructions.",
            f.name,
            f.instructions.len()
        );
        o.push_str(ALLOW);
        let _ = writeln!(o, "pub fn {name}(cx: &mut Cx) {{");
        o.push_str("    let mut pc = cx.pc;\n    loop {\n");

        // Group consecutive blocks into chunks of about CHUNK instructions.
        let mut chunks: Vec<Vec<&(u32, String, usize)>> = vec![Vec::new()];
        let mut size = 0;
        for arm in &arms {
            if size + arm.2 > self.opts.chunk && size > 0 {
                chunks.push(Vec::new());
                size = 0;
            }
            chunks.last_mut().unwrap().push(arm);
            size += arm.2;
        }

        if chunks.len() == 1 {
            // Small function: the blocks are the arms of a single `match`.
            o.push_str("        match pc {\n");
            for (addr, body, _) in &arms {
                let _ = write!(
                    o,
                    "            {} => {{\n{body}            }}\n",
                    hex(*addr)
                );
            }
            o.push_str("            _ => { cx.pc = pc; return; }\n        }\n    }\n}\n");
            return o;
        }

        // Big function: each chunk is its own Rust function (LLVM compiles
        // many small functions much faster than one huge one). A chunk runs
        // until control leaves it and returns the next address, or LEAVE when
        // the 68000 function itself returns.
        o.push_str("        pc = match pc {\n");
        for (k, chunk) in chunks.iter().enumerate() {
            let pats: Vec<String> = chunk.iter().map(|a| hex(a.0)).collect();
            let _ = writeln!(o, "            {} => {name}_{k}(cx, pc),", pats.join(" | "));
        }
        o.push_str("            _ => { cx.pc = pc; return; }\n        };\n");
        o.push_str("        if pc == LEAVE {\n            return;\n        }\n    }\n}\n");
        for (k, chunk) in chunks.iter().enumerate() {
            o.push_str(ALLOW);
            o.push_str("#[inline(never)]\n");
            let _ = writeln!(o, "fn {name}_{k}(cx: &mut Cx, mut pc: u32) -> u32 {{");
            o.push_str("    loop {\n        match pc {\n");
            for (addr, body, _) in chunk {
                let body = body.replace("return;", "return LEAVE;");
                let _ = write!(
                    o,
                    "            {} => {{\n{body}            }}\n",
                    hex(*addr)
                );
            }
            o.push_str("            _ => return pc,\n        }\n    }\n}\n");
        }
        o
    }

    // =====================================================================
    // One method per instruction family. Each returns Rust statements.
    // =====================================================================

    fn instruction(&self, ins: &Instruction) -> Vec<String> {
        let mut o = Vec::new();
        let sz = ins.size;
        let src = |var| Ea::new(ins.src.unwrap(), sz, var);
        let dst = |var| Ea::new(ins.dst.unwrap(), sz, var);
        let next = hex(ins.next_addr());
        match ins.mnemonic {
            // -- data movement --------------------------------------------
            "move" => {
                let (s, d) = (src("ea_s"), dst("ea_d"));
                s.setup(&mut o);
                o.push(format!("let v = {};", s.read()));
                d.setup(&mut o);
                o.push(d.write("v"));
                o.push(format!("cx.logic(v, {sz});"));
            }
            "movea" => {
                let s = src("ea_s");
                s.setup(&mut o);
                let v = if sz == 4 {
                    s.read()
                } else {
                    format!("sext16({})", s.read())
                };
                o.push(format!("let v = {v};"));
                o.push(format!("cx.a[{}] = v;", ins.dst.unwrap().reg));
            }
            "moveq" => {
                let r = ins.dst.unwrap().reg;
                o.push(format!("cx.d[{r}] = {};", hex(ins.src.unwrap().imm)));
                o.push(format!("cx.logic({}, 4);", hex(ins.src.unwrap().imm)));
            }
            "lea" => o.push(format!(
                "cx.a[{}] = {};",
                ins.dst.unwrap().reg,
                src("ea_s").address()
            )),
            "pea" => {
                o.push(format!("let v = {};", src("ea_s").address()));
                o.push("cx.push32(v);".into());
            }
            "clr" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(d.write("0"));
                o.push(format!("cx.logic(0, {sz});"));
            }
            "exg" => {
                let reg = |op: Operand| {
                    format!(
                        "cx.{}[{}]",
                        if op.mode == Mode::Dn { 'd' } else { 'a' },
                        op.reg
                    )
                };
                let (a, b) = (reg(ins.src.unwrap()), reg(ins.dst.unwrap()));
                o.push(format!("let t = {a};"));
                o.push(format!("{a} = {b};"));
                o.push(format!("{b} = t;"));
            }
            "swap" => {
                let r = ins.dst.unwrap().reg;
                o.push(format!("cx.d[{r}] = cx.d[{r}].rotate_left(16);"));
                o.push(format!("let v = cx.d[{r}];"));
                o.push("cx.logic(v, 4);".into());
            }
            "ext" => {
                let r = ins.dst.unwrap().reg;
                if sz == 2 {
                    o.push(format!("let v = sext8(cx.d[{r}]);"));
                    o.push(format!("cx.set_d16({r}, v);"));
                    o.push("cx.logic(v, 2);".into());
                } else {
                    o.push(format!("cx.d[{r}] = sext16(cx.d[{r}]);"));
                    o.push(format!("let v = cx.d[{r}];"));
                    o.push("cx.logic(v, 4);".into());
                }
            }
            "movem" => self.movem(ins, &mut o),
            "movep" => self.movep(ins, &mut o),

            // -- arithmetic and logic --------------------------------------
            "add" | "addi" => self.binary(ins, "add", true, &mut o),
            "sub" | "subi" => self.binary(ins, "sub", true, &mut o),
            "addx" | "subx" => self.binary(ins, ins.mnemonic, true, &mut o),
            "cmp" | "cmpi" | "cmpm" => self.binary(ins, "cmp", false, &mut o),
            "and" | "andi" => self.logic(ins, "&", &mut o),
            "or" | "ori" => self.logic(ins, "|", &mut o),
            "eor" | "eori" => self.logic(ins, "^", &mut o),
            "addq" | "subq" => {
                let d = ins.dst.unwrap();
                if d.mode == Mode::An {
                    // ADDQ/SUBQ to An: whole register, no flags.
                    let f = if ins.mnemonic == "addq" {
                        "wrapping_add"
                    } else {
                        "wrapping_sub"
                    };
                    o.push(format!(
                        "cx.a[{0}] = cx.a[{0}].{f}({1});",
                        d.reg,
                        ins.src.unwrap().imm
                    ));
                } else {
                    self.binary(
                        ins,
                        if ins.mnemonic == "addq" { "add" } else { "sub" },
                        true,
                        &mut o,
                    );
                }
            }
            "adda" | "suba" | "cmpa" => {
                let s = src("ea_s");
                s.setup(&mut o);
                let v = if sz == 4 {
                    s.read()
                } else {
                    format!("sext16({})", s.read())
                };
                o.push(format!("let s = {v};"));
                let r = ins.dst.unwrap().reg;
                match ins.mnemonic {
                    "adda" => o.push(format!("cx.a[{r}] = cx.a[{r}].wrapping_add(s);")),
                    "suba" => o.push(format!("cx.a[{r}] = cx.a[{r}].wrapping_sub(s);")),
                    _ => o.push(format!("let d = cx.a[{r}]; cx.cmp(d, s, 4);")),
                }
            }
            "neg" | "negx" | "not" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let d = {};", d.read()));
                match ins.mnemonic {
                    "neg" => o.push(format!("let r = cx.sub(0, d, {sz});")),
                    "negx" => o.push(format!("let r = cx.subx(0, d, {sz});")),
                    _ => o.push(format!("let r = !d & {};", mask(sz))),
                }
                o.push(d.write("r"));
                if ins.mnemonic == "not" {
                    o.push(format!("cx.logic(r, {sz});"));
                }
            }
            "tst" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let v = {};", d.read()));
                o.push(format!("cx.logic(v, {sz});"));
            }
            "tas" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let v = {};", d.read()));
                o.push("cx.logic(v, 1);".into());
                if d.op.mode == Mode::Dn {
                    o.push(d.write("v | 0x80"));
                } else {
                    o.push("cx.w8_tas(ea_d, v | 0x80);".into());
                }
            }
            "scc" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!(
                    "let v = if {} {{ 0xFF }} else {{ 0 }};",
                    cond(ins.cond)
                ));
                o.push(d.write("v"));
            }
            "mulu" | "muls" | "divu" | "divs" => {
                let s = Ea::new(ins.src.unwrap(), 2, "ea_s");
                s.setup(&mut o);
                o.push(format!("let s = {};", s.read()));
                let r = ins.dst.unwrap().reg;
                let op = ins.mnemonic;
                if op.starts_with("div") {
                    // Division by zero clears N, Z, V and C, then traps.
                    o.push(format!("if s == 0 {{ cx.n = false; cx.z = false; cx.v = false; cx.c = false; if !cx.exception(5, {next}) {{ return; }} }}"));
                    o.push(format!(
                        "else {{ let d = cx.d[{r}]; cx.d[{r}] = cx.{op}(d, s); }}"
                    ));
                } else {
                    o.push(format!("let d = cx.d[{r}]; cx.d[{r}] = cx.{op}(d, s);"));
                }
            }
            "abcd" | "sbcd" => {
                let (s, d) = (src("ea_s"), dst("ea_d"));
                s.setup(&mut o);
                o.push(format!("let s = {};", s.read()));
                d.setup(&mut o);
                o.push(format!("let d = {};", d.read()));
                o.push(format!("let r = cx.{}(d, s);", ins.mnemonic));
                o.push(d.write("r"));
            }
            "nbcd" => {
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let d = {};", d.read()));
                o.push("let r = cx.nbcd(d);".into());
                o.push(d.write("r"));
            }
            "chk" => {
                let s = Ea::new(ins.src.unwrap(), 2, "ea_s");
                s.setup(&mut o);
                let r = ins.dst.unwrap().reg;
                o.push(format!("let bound = {} as u16 as i16;", s.read()));
                o.push(format!("let v = cx.d[{r}] as u16 as i16;"));
                o.push("cx.n = v < 0; cx.z = v == 0; cx.v = false; cx.c = false;".into());
                o.push(format!(
                    "if !(0..=bound).contains(&v) && !cx.exception(6, {next}) {{ return; }}"
                ));
            }

            // -- shifts and rotates -----------------------------------------
            "asl" | "asr" | "lsl" | "lsr" | "rol" | "ror" | "roxl" | "roxr" => {
                let s = ins.src.unwrap();
                let count = if s.mode == Mode::Dn {
                    format!("cx.d[{}] & 63", s.reg)
                } else {
                    s.imm.to_string()
                };
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let d = {};", d.read()));
                o.push(format!("let r = cx.{}(d, {count}, {sz});", ins.mnemonic));
                o.push(d.write("r"));
            }

            // -- single-bit operations ----------------------------------------
            "btst" | "bchg" | "bclr" | "bset" => {
                let s = ins.src.unwrap();
                let modulo = if ins.dst.unwrap().mode == Mode::Dn {
                    31
                } else {
                    7
                };
                let bit = if s.mode == Mode::Dn {
                    format!("cx.d[{}] & {modulo}", s.reg)
                } else {
                    (s.imm & modulo).to_string()
                };
                let d = dst("ea_d");
                d.setup(&mut o);
                o.push(format!("let b = 1u32 << ({bit});"));
                o.push(format!("let v = {};", d.read()));
                o.push("cx.z = v & b == 0;".into());
                match ins.mnemonic {
                    "bchg" => o.push(d.write("v ^ b")),
                    "bclr" => o.push(d.write("v & !b")),
                    "bset" => o.push(d.write("v | b")),
                    _ => {}
                }
            }

            // -- status register -------------------------------------------------
            "andi_ccr" => o.push(format!(
                "let v = u32::from(cx.ccr()) & {}; cx.set_ccr(v);",
                hex(ins.src.unwrap().imm)
            )),
            "ori_ccr" => o.push(format!(
                "let v = u32::from(cx.ccr()) | {}; cx.set_ccr(v);",
                hex(ins.src.unwrap().imm)
            )),
            "eori_ccr" => o.push(format!(
                "let v = u32::from(cx.ccr()) ^ {}; cx.set_ccr(v);",
                hex(ins.src.unwrap().imm)
            )),
            "andi_sr" | "ori_sr" | "eori_sr" => {
                let op = match ins.mnemonic {
                    "andi_sr" => '&',
                    "ori_sr" => '|',
                    _ => '^',
                };
                o.push(format!(
                    "let v = u32::from(cx.sr()) {op} {}; cx.set_sr(v);",
                    hex(ins.src.unwrap().imm)
                ));
                // Lowering the mask can let a pending interrupt in *now*.
                o.push(format!("if !cx.check_irq({next}) {{ return; }}"));
            }
            "move_to_sr" => {
                let s = Ea::new(ins.src.unwrap(), 2, "ea_s");
                s.setup(&mut o);
                o.push(format!("let v = {}; cx.set_sr(v);", s.read()));
                o.push(format!("if !cx.check_irq({next}) {{ return; }}"));
            }
            "move_to_ccr" => {
                let s = Ea::new(ins.src.unwrap(), 2, "ea_s");
                s.setup(&mut o);
                o.push(format!("let v = {}; cx.set_ccr(v);", s.read()));
            }
            "move_from_sr" => {
                let d = Ea::new(ins.dst.unwrap(), 2, "ea_d");
                d.setup(&mut o);
                o.push("let v = u32::from(cx.sr());".into());
                o.push(d.write("v"));
            }
            "move_usp" => {
                if ins.src.unwrap().mode == Mode::Usp {
                    o.push(format!("cx.a[{}] = cx.usp();", ins.dst.unwrap().reg));
                } else {
                    o.push(format!(
                        "let v = cx.a[{}]; cx.set_usp(v);",
                        ins.src.unwrap().reg
                    ));
                }
            }

            // -- stack frames ------------------------------------------------------
            "link" => {
                let r = ins.src.unwrap().reg;
                o.push(format!("let v = cx.a[{r}]; cx.push32(v);"));
                o.push(format!("cx.a[{r}] = cx.a[7];"));
                o.push(format!(
                    "cx.a[7] = cx.a[7].wrapping_add({});",
                    hex(ins.dst.unwrap().imm)
                ));
            }
            "unlk" => {
                let r = ins.dst.unwrap().reg;
                o.push(format!("cx.a[7] = cx.a[{r}];"));
                o.push(format!("let v = cx.pop32(); cx.a[{r}] = v;"));
            }

            // -- control flow ---------------------------------------------------------
            "bra" => o.push(self.goto(ins.target.unwrap(), ins.addr)),
            "bcc" => o.push(format!(
                "if {} {{ {} }}",
                cond(ins.cond),
                self.goto(ins.target.unwrap(), ins.addr)
            )),
            "dbcc" => {
                let r = ins.dst.unwrap().reg;
                o.push(format!("if !{} {{", cond(ins.cond)));
                o.push(format!("    let c = cx.d[{r}].wrapping_sub(1) & 0xFFFF;"));
                o.push(format!("    cx.set_d16({r}, c);"));
                o.push(format!(
                    "    if c != 0xFFFF {{ {} }}",
                    self.goto(ins.target.unwrap(), ins.addr)
                ));
                o.push("}".into());
            }
            "bsr" => o.push(self.call(ins.target.unwrap(), ins.next_addr())),
            "jsr" => match ins.target {
                Some(t) => o.push(self.call(t, ins.next_addr())),
                None => {
                    o.push(format!(
                        "let t = {};",
                        Ea::new(ins.dst.unwrap(), 4, "").address()
                    ));
                    o.push(format!("if !cx.call_indirect(t, {next}) {{ return; }}"));
                }
            },
            "jmp" => match ins.target {
                Some(t) => o.push(self.goto(t, ins.addr)),
                None => {
                    // Known jump-table targets are blocks of this function: the
                    // `match` handles them; anything else leaves through `_`.
                    o.push(format!(
                        "let t = {} & 0xFF_FFFF;",
                        Ea::new(ins.dst.unwrap(), 4, "").address()
                    ));
                    o.push("if !cx.poll(t) { return; } pc = t; continue;".into());
                }
            },
            "rts" => o.push("let r = cx.pop32(); cx.pc = r & 0xFF_FFFF; return;".into()),
            "rtr" => {
                o.push("let ccr = cx.pop16(); cx.set_ccr(ccr);".into());
                o.push("let r = cx.pop32(); cx.pc = r & 0xFF_FFFF; return;".into());
            }
            "rte" => {
                // Pop both words from the supervisor stack *before* restoring SR,
                // which may switch to the user stack.
                o.push("let sr = cx.pop16(); let r = cx.pop32();".into());
                o.push("cx.set_sr(sr); cx.pc = r & 0xFF_FFFF; return;".into());
            }
            "trap" => o.push(format!(
                "if !cx.exception({}, {next}) {{ return; }}",
                32 + ins.data.unwrap()
            )),
            "trapv" => o.push(format!("if cx.v && !cx.exception(7, {next}) {{ return; }}")),
            "illegal" | "linea" | "linef" => {
                let vec = match ins.mnemonic {
                    "illegal" => 4,
                    "linea" => 10,
                    _ => 11,
                };
                o.push(format!(
                    "if !cx.exception({vec}, {0}) {{ return; }} pc = {0}; continue;",
                    hex(ins.addr)
                ));
            }
            "stop" => o.push(format!(
                "if !cx.stop({}, {}) {{ return; }}",
                hex(ins.data.unwrap()),
                hex(ins.addr)
            )),
            "nop" => {}
            "reset" => o.push("cx.reset_devices();".into()),
            m => panic!("no code generator for {m}"),
        }
        o
    }

    fn binary(&self, ins: &Instruction, func: &str, store: bool, o: &mut Vec<String>) {
        let s = Ea::new(ins.src.unwrap(), ins.size, "ea_s");
        let d = Ea::new(ins.dst.unwrap(), ins.size, "ea_d");
        s.setup(o);
        o.push(format!("let s = {};", s.read()));
        d.setup(o);
        o.push(format!("let d = {};", d.read()));
        if store {
            o.push(format!("let r = cx.{func}(d, s, {});", ins.size));
            o.push(d.write("r"));
        } else {
            o.push(format!("cx.{func}(d, s, {});", ins.size));
        }
    }

    fn logic(&self, ins: &Instruction, op: &str, o: &mut Vec<String>) {
        let s = Ea::new(ins.src.unwrap(), ins.size, "ea_s");
        let d = Ea::new(ins.dst.unwrap(), ins.size, "ea_d");
        s.setup(o);
        o.push(format!("let s = {};", s.read()));
        d.setup(o);
        o.push(format!(
            "let r = ({} {op} s) & {};",
            d.read(),
            mask(ins.size)
        ));
        o.push(d.write("r"));
        o.push(format!("cx.logic(r, {});", ins.size));
    }

    fn movem(&self, ins: &Instruction, o: &mut Vec<String>) {
        let (sz, mask) = (u32::from(ins.size), ins.data.unwrap());
        let regs: Vec<String> = (0..8)
            .map(|i| format!("cx.d[{i}]"))
            .chain((0..8).map(|i| format!("cx.a[{i}]")))
            .collect();
        if let Some(d) = ins.dst {
            // registers -> memory
            if d.mode == Mode::PreDec {
                // The mask is reversed (bit 0 = A7), stored downwards. A register
                // in the list is stored with its value *before* the instruction.
                o.push(format!("let mut a = cx.a[{}];", d.reg));
                for bit in 0..16 {
                    if mask >> bit & 1 != 0 {
                        o.push(format!(
                            "a = a.wrapping_sub({sz}); let v = {}; cx.w{}(a, v);",
                            regs[15 - bit],
                            sz * 8
                        ));
                    }
                }
                o.push(format!("cx.a[{}] = a;", d.reg));
            } else {
                o.push(format!(
                    "let mut a = {};",
                    Ea::new(d, ins.size, "").address()
                ));
                for (bit, reg) in regs.iter().enumerate() {
                    if mask >> bit & 1 != 0 {
                        o.push(format!(
                            "let v = {reg}; cx.w{}(a, v); a = a.wrapping_add({sz});",
                            sz * 8
                        ));
                    }
                }
            }
            return;
        }
        // memory -> registers (words are sign-extended to 32 bits)
        let s = ins.src.unwrap();
        if s.mode == Mode::PostInc {
            o.push(format!("let mut a = cx.a[{}];", s.reg));
        } else {
            o.push(format!(
                "let mut a = {};",
                Ea::new(s, ins.size, "").address()
            ));
        }
        for (bit, reg) in regs.iter().enumerate() {
            if mask >> bit & 1 != 0 {
                let val = if sz == 4 {
                    "cx.r32(a)".to_string()
                } else {
                    "sext16(cx.r16(a))".to_string()
                };
                o.push(format!(
                    "let v = {val}; {reg} = v; a = a.wrapping_add({sz});"
                ));
            }
        }
        if s.mode == Mode::PostInc {
            o.push(format!("cx.a[{}] = a;", s.reg));
        }
    }

    /// MOVEP moves a register to every other byte (8-bit peripherals).
    fn movep(&self, ins: &Instruction, o: &mut Vec<String>) {
        let n = u32::from(ins.size);
        let (s, d) = (ins.src.unwrap(), ins.dst.unwrap());
        if s.mode == Mode::Dn {
            o.push(format!(
                "let a = {};",
                plus(&format!("cx.a[{}]", d.reg), d.disp)
            ));
            for k in 0..n {
                o.push(format!(
                    "let v = cx.d[{}] >> {}; cx.w8(a.wrapping_add({}), v);",
                    s.reg,
                    8 * (n - 1 - k),
                    2 * k
                ));
            }
        } else {
            o.push(format!(
                "let a = {};",
                plus(&format!("cx.a[{}]", s.reg), s.disp)
            ));
            o.push("let mut v = 0u32;".into());
            for k in 0..n {
                o.push(format!("v = v << 8 | cx.r8(a.wrapping_add({}));", 2 * k));
            }
            o.push(if n == 4 {
                format!("cx.d[{}] = v;", d.reg)
            } else {
                format!("cx.set_d16({}, v);", d.reg)
            });
        }
    }
}

/// All the blocks of the program: address -> function that runs it.
fn block_table(program: &Program, opts: &Options) -> BTreeMap<u32, u32> {
    let mut table = BTreeMap::new();
    for f in program.functions.values() {
        let mut e = Emitter {
            program,
            f,
            blocks: BTreeSet::new(),
            opts,
        };
        e.find_blocks();
        for b in e.blocks {
            table.entry(b).or_insert(f.entry);
        }
    }
    // A function's entry always runs in that function.
    for &entry in program.functions.keys() {
        table.insert(entry, entry);
    }
    table
}

/// Options for [`generate`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Functions per source file (smaller files compile in parallel).
    pub functions_per_file: usize,
    /// Longest run of instructions in one block.
    pub max_block: usize,
    /// Instructions per chunk function, when a 68000 function is big.
    pub chunk: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            functions_per_file: 200,
            max_block: 24,
            chunk: 48,
        }
    }
}

/// Generate the `recomp` module: `{file name: Rust source}`. The module
/// expects the crate root to define `type Machine` (the console it runs on).
#[must_use]
pub fn generate(program: &Program, opts: &Options) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let entries: Vec<u32> = program.functions.keys().copied().collect();
    let title = program.rom.title().replace('"', "'");
    let mut root = format!(
        "//! Generated by gase-recomp from \"{title}\". Do not edit by hand.\n\
         //! Each function below is a 68000 routine translated to Rust.\n\n\
         #[allow(unused_imports)]\n\
         pub(crate) use gase_recomp_runtime::{{sext8, sext16}};\n\
         pub(crate) type Cx = gase_recomp_runtime::Cx<crate::Machine>;\n\
         /// What a chunk returns when its 68000 function returns.\n\
         pub(crate) const LEAVE: u32 = u32::MAX;\n\n\
         /// CRC-32 and size of the ROM this code was recompiled from.\n\
         pub const ROM_CRC32: u32 = 0x{:08X};\n\
         pub const ROM_SIZE: usize = {};\n\
         pub const TITLE: &str = \"{title}\";\n\n",
        program.rom.crc32(),
        program.rom.data.len()
    );
    for (k, chunk) in entries.chunks(opts.functions_per_file.max(1)).enumerate() {
        let name = format!("part_{k:03}");
        let mut src = String::from(
            "// Generated by gase-recomp. Do not edit by hand.\n#![allow(unused_imports)]\nuse super::*;\n\n",
        );
        for a in chunk {
            src.push_str(
                &Emitter {
                    program,
                    f: &program.functions[a],
                    blocks: BTreeSet::new(),
                    opts,
                }
                .emit(),
            );
            src.push('\n');
        }
        let _ = writeln!(root, "mod {name};\npub use {name}::*;");
        files.insert(format!("{name}.rs"), src);
    }

    // The dispatcher: every block of every function.
    let table = block_table(program, opts);
    let mut by_fn: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (b, f) in &table {
        by_fn.entry(*f).or_default().push(*b);
    }
    root.push_str("\n/// Run the recompiled code at `cx.pc`. `false` if there is none.\n");
    root.push_str("pub fn dispatch(cx: &mut Cx) -> bool {\n    match cx.pc {\n");
    for (f, blocks) in &by_fn {
        let pats: Vec<String> = blocks.iter().map(|b| hex(*b)).collect();
        let _ = writeln!(root, "        {} => {}(cx),", pats.join(" | "), fn_name(*f));
    }
    root.push_str("        _ => return false,\n    }\n    true\n}\n\n");
    root.push_str("/// Is `addr` the start of a recompiled block?\npub fn is_block(addr: u32) -> bool {\n    matches!(addr, ");
    let all: Vec<String> = table.keys().map(|b| hex(*b)).collect();
    root.push_str(&if all.is_empty() {
        "0xFFFF_FFFF".to_string()
    } else {
        all.join(" | ")
    });
    root.push_str(")\n}\n\n/// What the runtime needs: the dispatcher and the block test.\n");
    root.push_str("pub fn code() -> gase_recomp_runtime::Code<crate::Machine> {\n");
    root.push_str("    gase_recomp_runtime::Code { dispatch, is_block }\n}\n");
    files.insert("mod.rs".into(), root);
    files
}

/// The same, as one file (for `include!`, used by tests).
#[must_use]
pub fn generate_single(program: &Program, opts: &Options) -> String {
    let files = generate(
        program,
        &Options {
            functions_per_file: usize::MAX,
            ..opts.clone()
        },
    );
    let mut out = files["mod.rs"].replace("mod part_000;\npub use part_000::*;\n", "");
    out = out.replace("//! ", "// ");
    if let Some(part) = files.get("part_000.rs") {
        out.push_str(&part.replace("#![allow(unused_imports)]\nuse super::*;\n", ""));
    }
    out
}
