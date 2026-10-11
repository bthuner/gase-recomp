//! The Motorola 68000: operand model and instruction decoder.

mod decode;
mod operand;
pub mod timing;

pub use decode::{CONDITIONS, DecodeError, Flow, Instruction, decode};
pub use operand::{Mode, Operand};
