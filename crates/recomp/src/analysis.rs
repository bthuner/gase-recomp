//! Find the code inside the ROM.
//!
//! A ROM mixes code and data (graphics, music, maps, tables) with nothing
//! telling them apart. We use **recursive descent**: start from addresses
//! we *know* are code, the exception vectors, and follow every branch and
//! call. Whatever is reached that way is code; the rest is left alone.
//!
//! * a **function** is an entry point plus every instruction reachable from
//!   it through branches (BRA, Bcc, DBcc, JMP to a static address);
//! * a **call** (BSR / JSR) makes its target the entry of another function;
//! * a **resume point** is the address right after a call, where the
//!   dispatcher may re-enter the function (see the runtime's `call`).
//!
//! What static analysis cannot see (`jmp (a0)`, `jsr (a1,d0.w)`) is left to
//! run time: the runtime's interpreter fallback runs that code, logs the
//! address to `gase_missing.txt`, and the next recompilation turns it into
//! a function. That is the *coverage feedback loop*.
//!
//! Two classic jump-table idioms are recognised to reduce that work:
//!
//! ```text
//!     jmp    table(pc,d0.w)       ; table = BRA.W instructions
//!
//!     move.w table(pc,d0.w),d0    ; table = 16-bit offsets
//!     jmp    table(pc,d0.w)       ;   relative to the table
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::m68k::{DecodeError, Flow, Instruction, Mode, decode};
use crate::rom::{Rom, vector_name};

const MAX_FUNCTION_INSTRUCTIONS: usize = 20_000;
const MAX_JUMP_TABLE: u32 = 256;

#[derive(Clone, Debug)]
pub struct Function {
    pub entry: u32,
    pub name: String,
    pub instructions: BTreeMap<u32, Instruction>,
    pub resume_points: BTreeSet<u32>,
    /// Indirect jumps whose possible targets were recovered from a table.
    pub jump_tables: BTreeMap<u32, Vec<u32>>,
    /// Branch targets that failed to decode.
    pub bad_targets: BTreeSet<u32>,
}

#[derive(Debug)]
pub struct Program {
    pub rom: Rom,
    pub functions: BTreeMap<u32, Function>,
    pub rejected_entries: BTreeMap<u32, String>,
}

impl Program {
    #[must_use]
    pub fn instruction_count(&self) -> usize {
        self.functions.values().map(|f| f.instructions.len()).sum()
    }
}

pub struct Analyzer<'a> {
    rom: &'a Rom,
    cache: HashMap<u32, Result<Instruction, DecodeError>>,
}

impl std::fmt::Debug for Analyzer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyzer")
            .field("cached", &self.cache.len())
            .finish()
    }
}

impl<'a> Analyzer<'a> {
    #[must_use]
    pub fn new(rom: &'a Rom) -> Self {
        Self {
            rom,
            cache: HashMap::new(),
        }
    }

    /// Decode with a cache: functions overlap, decode each address once.
    fn decode_at(&mut self, addr: u32) -> Result<Instruction, DecodeError> {
        let rom = self.rom;
        self.cache
            .entry(addr)
            .or_insert_with(|| decode(|a| rom.u16(a & 0xFF_FFFF), addr))
            .clone()
    }

    fn in_rom(&self, addr: u32) -> bool {
        addr % 2 == 0 && addr > 0 && (addr as usize) < self.rom.data.len()
    }

    /// Code addresses named by the exception vector table.
    fn vector_entries(&self) -> BTreeMap<u32, String> {
        let mut entries = BTreeMap::new();
        for (n, &target) in self.rom.vectors.iter().enumerate().skip(1) {
            let target = target & 0xFF_FFFF;
            if self.in_rom(target) && target >= 0x100 {
                entries.entry(target).or_insert_with(|| vector_name(n));
            }
        }
        entries
    }

    pub fn run(mut self, extra: &BTreeMap<u32, String>) -> Program {
        let mut entries = self.vector_entries();
        for (&a, name) in extra {
            entries.entry(a & 0xFF_FFFF).or_insert_with(|| name.clone());
        }
        let mut functions = BTreeMap::new();
        let mut rejected = BTreeMap::new();
        let mut worklist: Vec<(u32, String)> = entries.into_iter().collect();
        while let Some((addr, name)) = worklist.pop() {
            if functions.contains_key(&addr) || rejected.contains_key(&addr) {
                continue;
            }
            if !self.in_rom(addr) {
                rejected.insert(addr, "outside ROM".to_string());
                continue;
            }
            if let Err(e) = self.decode_at(addr) {
                rejected.insert(addr, e.to_string());
                continue;
            }
            let f = self.explore(addr, name);
            for ins in f.instructions.values() {
                if let (Flow::Call, Some(t)) = (ins.flow, ins.target) {
                    let t = t & 0xFF_FFFF;
                    if !functions.contains_key(&t) {
                        worklist.push((t, format!("sub_{t:06X}")));
                    }
                }
            }
            functions.insert(addr, f);
        }
        Program {
            rom: self.rom.clone(),
            functions,
            rejected_entries: rejected,
        }
    }

    /// Collect every instruction reachable from `entry` without calls.
    fn explore(&mut self, entry: u32, name: String) -> Function {
        let mut f = Function {
            entry,
            name,
            instructions: BTreeMap::new(),
            resume_points: BTreeSet::new(),
            jump_tables: BTreeMap::new(),
            bad_targets: BTreeSet::new(),
        };
        let mut todo = vec![entry];
        while let Some(addr) = todo.pop() {
            if f.instructions.contains_key(&addr) || f.bad_targets.contains(&addr) {
                continue;
            }
            if f.instructions.len() >= MAX_FUNCTION_INSTRUCTIONS {
                break;
            }
            let ins = match self.decode_at(addr) {
                Ok(i) if self.in_rom(addr) => i,
                _ => {
                    f.bad_targets.insert(addr);
                    continue;
                }
            };
            if matches!(ins.flow, Flow::Next | Flow::Trap | Flow::Call | Flow::Cond) {
                todo.push(ins.next_addr());
            }
            if ins.flow == Flow::Call {
                f.resume_points.insert(ins.next_addr());
            }
            if let (Flow::Branch | Flow::Cond, Some(t)) = (ins.flow, ins.target) {
                todo.push(t & 0xFF_FFFF);
            }
            if ins.flow == Flow::Jump {
                let targets = self.jump_table(&f, &ins);
                if !targets.is_empty() {
                    todo.extend(&targets);
                    f.jump_tables.insert(addr, targets);
                }
            }
            f.instructions.insert(addr, ins);
        }
        f
    }

    fn jump_table(&mut self, f: &Function, ins: &Instruction) -> Vec<u32> {
        let Some(op) = ins.dst.filter(|o| o.mode == Mode::PcIndex) else {
            return Vec::new();
        };
        let base = op.addr;
        let offsets_idiom = f
            .instructions
            .get(&ins.addr.wrapping_sub(4))
            .is_some_and(|p| {
                p.mnemonic == "move"
                    && p.size == 2
                    && p.src
                        .is_some_and(|s| s.mode == Mode::PcIndex && s.addr == base)
                    && p.dst
                        .is_some_and(|d| d.mode == Mode::Dn && d.reg == op.xreg)
            });
        if offsets_idiom {
            self.offset_table(base)
        } else {
            self.bra_table(base)
        }
    }

    /// `jmp table(pc,dN)` followed by BRA.W / JMP instructions.
    fn bra_table(&mut self, base: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut addr = base;
        for _ in 0..MAX_JUMP_TABLE {
            match self.decode_at(addr) {
                Ok(i) if matches!(i.mnemonic, "bra" | "jmp") && i.flow == Flow::Branch => {
                    out.push(addr);
                    addr += i.length;
                }
                _ => break,
            }
        }
        out
    }

    /// Signed 16-bit offsets from `base`; the table ends where code begins.
    fn offset_table(&mut self, base: u32) -> Vec<u32> {
        let mut targets = BTreeSet::new();
        let mut end = base + 2 * MAX_JUMP_TABLE;
        let mut pos = base;
        while pos < end {
            let Some(off) = self.rom.u16(pos) else { break };
            let t = base.wrapping_add(i32::from(off as i16) as u32);
            if off == 0 || !self.in_rom(t) || self.decode_at(t).is_err() {
                break;
            }
            targets.insert(t);
            if base < t && t < end {
                end = t;
            }
            pos += 2;
        }
        targets.into_iter().collect()
    }
}

/// Analyse `rom`, with optional extra entry points (address -> name).
#[must_use]
pub fn analyze(rom: &Rom, extra: &BTreeMap<u32, String>) -> Program {
    Analyzer::new(rom).run(extra)
}
