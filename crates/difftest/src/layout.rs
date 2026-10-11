// Shared by build.rs and the tests: where things live in the test image.

/// The snippets: one every SLOT bytes from CODE_BASE.
const CODE_BASE: u32 = 0x1000;
const SLOT: u32 = 0x30;
const MAX_CASES: u32 = (0x1_0000 - CODE_BASE) / SLOT;
/// Up to this many instructions per snippet, then RTS.
const MAX_LEN: u32 = 4;
/// Every exception vector points here: a lone RTE.
const HANDLER: u32 = 0x800;
/// Work RAM, where the address registers point.
const RAM: u32 = 0xFF_0000;

/// xorshift32: tiny, deterministic, shared by both sides.
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
}
