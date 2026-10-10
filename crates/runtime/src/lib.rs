//! Runtime for Mega Drive games statically recompiled by `gase-recomp`.
//!
//! The recompiler turns each 68000 routine of a ROM into a Rust function
//! taking a [`Cx`]: the CPU's registers and flags, plus the console around
//! it, a [`Machine`]. The console is [gase](https://github.com/bthuner/gase)'s:
//! a recompiled game registers itself ([`register`]) and gase drives it
//! frame by frame through [`Recompiled`], an external CPU. Only the 68000
//! is replaced; gase's interpreter stays on standby for the code the
//! recompiler did not find, and gase's frontends provide the window,
//! menus, save states and controls on every platform.
//!
//! | module         | what you learn there                                    |
//! |----------------|---------------------------------------------------------|
//! | [`cpu`]        | blocks as a state machine, calls, poll points, IRQs     |
//! | [`ops`]        | the 68000's flags: X N Z V C, shifts, BCD, mul/div      |
//! | [`machine`]    | the interface between the CPU and the rest of the console |
//! | [`recompiled`] | how a recompiled game plugs into gase                   |

pub mod cpu;
pub mod machine;
pub mod ops;
pub mod recompiled;

pub use cpu::{Code, Cx};
/// gase's console, re-exported for generated crates and tests.
pub use gase_core;
pub use gase_m68k;
pub use machine::{FlatMachine, Machine};
pub use ops::{sext8, sext16};
pub use recompiled::{Recompiled, register};
