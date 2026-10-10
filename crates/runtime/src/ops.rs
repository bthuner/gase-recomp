//! 68000 arithmetic and condition codes, as small inline methods.
//!
//! Every helper takes the operand size in bytes (`sz` = 1, 2 or 4). The
//! recompiler always passes a constant, so after inlining LLVM folds every
//! size test away, and a flag that is overwritten before anyone reads it
//! is simply never computed.
//!
//! The condition codes (low byte of SR):
//!
//! | flag | name     | meaning                                          |
//! |------|----------|--------------------------------------------------|
//! | X    | extend   | a copy of C kept for multi-precision arithmetic  |
//! | N    | negative | most significant bit of the result               |
//! | Z    | zero     | the result is zero                               |
//! | V    | overflow | signed overflow                                  |
//! | C    | carry    | unsigned carry / borrow                          |
//!
//! Where the manual calls a flag "undefined" (BCD, DIV, CHK) we do what the
//! real chip does, as measured by the gase interpreter's test vectors. Our
//! differential test caught every place where we first got it wrong.

use crate::cpu::Cx;
use crate::machine::Machine;

#[inline(always)]
const fn mask(sz: u32) -> u32 {
    match sz {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[inline(always)]
const fn msb(sz: u32) -> u32 {
    match sz {
        1 => 0x80,
        2 => 0x8000,
        _ => 0x8000_0000,
    }
}

/// Sign-extend a byte.
#[inline(always)]
#[must_use]
pub const fn sext8(v: u32) -> u32 {
    v as u8 as i8 as i32 as u32
}

/// Sign-extend a word.
#[inline(always)]
#[must_use]
pub const fn sext16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

impl<M: Machine> Cx<M> {
    #[inline(always)]
    fn nz(&mut self, r: u32, sz: u32) {
        self.n = r & msb(sz) != 0;
        self.z = r & mask(sz) == 0;
    }

    /// N and Z from the result, V and C cleared (MOVE, AND, OR, TST...).
    #[inline(always)]
    pub fn logic(&mut self, r: u32, sz: u32) {
        self.nz(r, sz);
        self.v = false;
        self.c = false;
    }

    #[inline(always)]
    pub fn add(&mut self, d: u32, s: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (d, s) = (d & m, s & m);
        let full = u64::from(d) + u64::from(s);
        let r = full as u32 & m;
        self.c = full >> (sz * 8) & 1 != 0;
        self.x = self.c;
        self.v = !(d ^ s) & (d ^ r) & top != 0; // same-sign inputs, sign flipped
        self.nz(r, sz);
        r
    }

    #[inline(always)]
    pub fn sub(&mut self, d: u32, s: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (d, s) = (d & m, s & m);
        let r = d.wrapping_sub(s) & m;
        self.c = s > d; // borrow
        self.x = self.c;
        self.v = (d ^ s) & (d ^ r) & top != 0;
        self.nz(r, sz);
        r
    }

    /// CMP is a SUB that only keeps the flags, and leaves X alone.
    #[inline(always)]
    pub fn cmp(&mut self, d: u32, s: u32, sz: u32) {
        let x = self.x;
        self.sub(d, s, sz);
        self.x = x;
    }

    /// ADDX: adds X too, and Z is only ever *cleared* (multi-word results).
    #[inline(always)]
    pub fn addx(&mut self, d: u32, s: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (d, s) = (d & m, s & m);
        let full = u64::from(d) + u64::from(s) + u64::from(self.x);
        let r = full as u32 & m;
        self.c = full >> (sz * 8) & 1 != 0;
        self.x = self.c;
        self.v = !(d ^ s) & (d ^ r) & top != 0;
        self.n = r & top != 0;
        if r != 0 {
            self.z = false;
        }
        r
    }

    #[inline(always)]
    pub fn subx(&mut self, d: u32, s: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (d, s) = (d & m, s & m);
        let r = d.wrapping_sub(s).wrapping_sub(u32::from(self.x)) & m;
        self.c = u64::from(s) + u64::from(self.x) > u64::from(d);
        self.x = self.c;
        self.v = (d ^ s) & (d ^ r) & top != 0;
        self.n = r & top != 0;
        if r != 0 {
            self.z = false;
        }
        r
    }

    // ---- shifts and rotates (cnt = 0..63) ------------------------------------

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn asl(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let r;
        if cnt == 0 {
            r = d;
            (self.c, self.v) = (false, false);
        } else if cnt < bits {
            r = (d << cnt) & m;
            self.c = (d >> (bits - cnt)) & 1 != 0;
            self.x = self.c;
            // V: did the sign bit change at any point? The bits travelling
            // through the MSB must all be equal.
            let top = (((1u64 << (cnt + 1)) - 1) << (bits - 1 - cnt)) as u32 & m;
            self.v = d & top != 0 && d & top != top;
        } else {
            r = 0;
            self.c = cnt == bits && d & 1 != 0;
            self.x = self.c;
            self.v = d != 0;
        }
        self.nz(r, sz);
        r
    }

    pub fn asr(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let neg = d & msb(sz) != 0;
        let r;
        if cnt == 0 {
            r = d;
            self.c = false;
        } else if cnt < bits {
            r = (d >> cnt) | if neg { m & !(m >> cnt) } else { 0 }; // replicate the sign
            self.c = (d >> (cnt - 1)) & 1 != 0;
            self.x = self.c;
        } else {
            r = if neg { m } else { 0 };
            self.c = neg;
            self.x = neg;
        }
        self.v = false;
        self.nz(r, sz);
        r
    }

    pub fn lsl(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let r;
        if cnt == 0 {
            r = d;
            self.c = false;
        } else if cnt < bits {
            r = (d << cnt) & m;
            self.c = (d >> (bits - cnt)) & 1 != 0;
            self.x = self.c;
        } else {
            r = 0;
            self.c = cnt == bits && d & 1 != 0;
            self.x = self.c;
        }
        self.v = false;
        self.nz(r, sz);
        r
    }

    pub fn lsr(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let r;
        if cnt == 0 {
            r = d;
            self.c = false;
        } else if cnt < bits {
            r = d >> cnt;
            self.c = (d >> (cnt - 1)) & 1 != 0;
            self.x = self.c;
        } else {
            r = 0;
            self.c = cnt == bits && d & msb(sz) != 0;
            self.x = self.c;
        }
        self.v = false;
        self.nz(r, sz);
        r
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn rol(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let k = cnt % bits;
        let r = if k == 0 {
            d
        } else {
            ((d << k) | (d >> (bits - k))) & m
        };
        self.c = cnt != 0 && r & 1 != 0; // the last bit rotated out
        self.v = false;
        self.nz(r, sz);
        r
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn ror(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, bits) = (mask(sz), sz * 8);
        let d = d & m;
        let k = cnt % bits;
        let r = if k == 0 {
            d
        } else {
            ((d >> k) | (d << (bits - k))) & m
        };
        self.c = cnt != 0 && r & msb(sz) != 0;
        self.v = false;
        self.nz(r, sz);
        r
    }

    /// ROXL/ROXR rotate through X: a (bits + 1)-wide rotation.
    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn roxl(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (mut r, mut x) = (d & m, self.x);
        for _ in 0..cnt % (sz * 8 + 1) {
            let out = r & top != 0;
            r = ((r << 1) | u32::from(x)) & m;
            x = out;
        }
        self.x = x;
        self.c = x;
        self.v = false;
        self.nz(r, sz);
        r
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn roxr(&mut self, d: u32, cnt: u32, sz: u32) -> u32 {
        let (m, top) = (mask(sz), msb(sz));
        let (mut r, mut x) = (d & m, self.x);
        for _ in 0..cnt % (sz * 8 + 1) {
            let out = r & 1 != 0;
            r = (r >> 1) | if x { top } else { 0 };
            x = out;
        }
        self.x = x;
        self.c = x;
        self.v = false;
        self.nz(r, sz);
        r
    }

    // ---- multiply / divide (16x16 -> 32, 32/16 -> 16:16) --------------------

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn mulu(&mut self, d: u32, s: u32) -> u32 {
        let r = (d & 0xFFFF) * (s & 0xFFFF);
        self.logic(r, 4);
        r
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn muls(&mut self, d: u32, s: u32) -> u32 {
        let r = (i32::from(d as u16 as i16) * i32::from(s as u16 as i16)) as u32;
        self.logic(r, 4);
        r
    }

    /// The divisor must not be zero (the caller raises the exception). On
    /// overflow the destination is unchanged and V is set.
    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn divu(&mut self, d: u32, s: u32) -> u32 {
        let s = s & 0xFFFF;
        let (q, rem) = (d / s, d % s);
        if q > 0xFFFF {
            self.v = true;
            self.n = true;
            self.z = false;
            self.c = false;
            return d;
        }
        self.logic(q, 2);
        rem << 16 | q
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn divs(&mut self, d: u32, s: u32) -> u32 {
        let (sd, ss) = (d as i32, i32::from(s as u16 as i16));
        let q = i64::from(sd) / i64::from(ss); // truncates toward zero, like the 68000
        let rem = i64::from(sd) % i64::from(ss);
        if !(-32768..=32767).contains(&q) {
            self.v = true;
            self.n = true;
            self.z = false;
            self.c = false;
            return d;
        }
        self.logic(q as u32, 2);
        (rem as u32 & 0xFFFF) << 16 | (q as u32 & 0xFFFF)
    }

    // ---- binary-coded decimal (two digits per byte) ----------------------------
    //
    // How the real chip corrects a binary sum into decimal, including what
    // it does with invalid digits (A-F) and the "undefined" V flag. This is
    // the gase interpreter's model, measured against hardware test vectors;
    // it differs from older emulators such as Musashi.

    fn set_bcd_flags(&mut self, (result, carry, overflow): (u32, bool, bool)) -> u32 {
        self.c = carry;
        self.x = carry;
        self.v = overflow;
        self.n = result & 0x80 != 0;
        if result != 0 {
            self.z = false;
        }
        result
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn abcd(&mut self, d: u32, s: u32) -> u32 {
        let (d, s) = (d & 0xFF, s & 0xFF);
        let binary = d + s + u32::from(self.x);
        let carries = ((s & d) | (!binary & s) | (!binary & d)) & 0x88;
        let too_big = (((binary + 0x66) ^ binary) & 0x110) >> 1;
        let correction = (carries | too_big) - ((carries | too_big) >> 2);
        let result = binary + correction;
        self.set_bcd_flags((
            result & 0xFF,
            (carries | (binary & !result)) & 0x80 != 0,
            !binary & result & 0x80 != 0,
        ))
    }

    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn sbcd(&mut self, d: u32, s: u32) -> u32 {
        let (d, s) = (d & 0xFF, s & 0xFF);
        let binary = d.wrapping_sub(s).wrapping_sub(u32::from(self.x));
        let borrows = ((!d & s) | (binary & !d) | (binary & s)) & 0x88;
        let correction = borrows - (borrows >> 2);
        let result = binary.wrapping_sub(correction);
        self.set_bcd_flags((
            result & 0xFF,
            (borrows | (!binary & result)) & 0x80 != 0,
            binary & !result & 0x80 != 0,
        ))
    }

    /// NBCD: 0 - d - X, in decimal.
    #[inline(never)] // rare and heavy: keep it out of every call site
    pub fn nbcd(&mut self, d: u32) -> u32 {
        self.sbcd(0, d)
    }

    // ---- conditions (Bcc, DBcc, Scc) ---------------------------------------------

    #[inline(always)]
    #[must_use]
    pub fn cond(&self, cc: u8) -> bool {
        match cc {
            0 => true,
            1 => false,
            2 => !self.c && !self.z,
            3 => self.c || self.z,
            4 => !self.c,
            5 => self.c,
            6 => !self.z,
            7 => self.z,
            8 => !self.v,
            9 => self.v,
            10 => !self.n,
            11 => self.n,
            12 => self.n == self.v,
            13 => self.n != self.v,
            14 => !self.z && self.n == self.v,
            _ => self.z || self.n != self.v,
        }
    }
}
