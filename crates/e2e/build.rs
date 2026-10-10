//! Assemble the test ROM, then recompile it twice:
//! * `first_pass`: what static analysis finds on its own;
//! * `second_pass`: after the coverage feedback loop has added the address
//!   the first pass had to interpret.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gase_recomp::analysis::analyze;
use gase_recomp::codegen::generate_single;
use gase_recomp::rom::Rom;

include!("src/test_rom.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/test_rom.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let raw = test_rom::build();
    let rom = Rom::from_bytes(&raw).unwrap();
    std::fs::write(out.join("rom.bin"), &raw).unwrap();
    let first = analyze(&rom, &BTreeMap::new());
    std::fs::write(out.join("first_pass.rs"), generate_single(&first)).unwrap();
    let found = BTreeMap::from([(test_rom::INDIRECT_SUB, "found_at_run_time".to_string())]);
    let second = analyze(&rom, &found);
    std::fs::write(out.join("second_pass.rs"), generate_single(&second)).unwrap();
}
