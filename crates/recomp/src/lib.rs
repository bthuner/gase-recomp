//! gase-recomp: statically recompile a Sega Mega Drive / Genesis ROM into a
//! Rust crate that runs on the chips of the [gase] emulator.
//!
//! The pipeline, one module per step (read them in this order):
//!
//! | module       | step                                                  |
//! |--------------|-------------------------------------------------------|
//! | [`rom`]      | load the ROM, parse the header and the vector table   |
//! | [`m68k`]     | decode 68000 machine code into [`m68k::Instruction`]s |
//! | [`analysis`] | find the code: functions, branches, calls             |
//! | [`codegen`]  | turn each function into Rust source                   |
//! | [`project`]  | write a buildable crate for the chosen platform       |
//!
//! [gase]: https://github.com/bthuner/gase

pub mod analysis;
pub mod codegen;
pub mod m68k;
pub mod project;
pub mod rom;
