/*
 * m68k_ops.h — 68000 arithmetic and condition codes, as inline helpers.
 *
 * Every helper takes the operand size in bytes (sz = 1, 2 or 4). The
 * recompiler always passes a constant, so after inlining the compiler
 * folds all the size tests away: op_add(x, y, 2) becomes a handful of
 * instructions.
 *
 * Condition codes (the low byte of SR):
 *   X  extend   — a copy of C kept for multi-precision arithmetic (ADDX..)
 *   N  negative — most significant bit of the result
 *   Z  zero     — result is zero
 *   V  overflow — signed overflow
 *   C  carry    — unsigned carry / borrow
 *
 * For the few "undefined" flags (BCD ops, DIV overflow) we reproduce the
 * behaviour of the Musashi emulator, our reference in differential tests.
 */
#ifndef GASE_M68K_OPS_H
#define GASE_M68K_OPS_H

#define OP_MASK(sz) ((sz) == 1 ? 0xFFu : (sz) == 2 ? 0xFFFFu : 0xFFFFFFFFu)
#define OP_MSB(sz)  ((sz) == 1 ? 0x80u : (sz) == 2 ? 0x8000u : 0x80000000u)
#define OP_BITS(sz) ((sz) * 8)

#define F gase_cpu

/* N and Z from a result; V and C cleared. Used by MOVE, AND, OR, TST... */
static inline void op_logic(uint32_t r, int sz) {
    r &= OP_MASK(sz);
    F.n = (r & OP_MSB(sz)) != 0;
    F.z = r == 0;
    F.v = 0;
    F.c = 0;
}

static inline uint32_t op_add(uint32_t d, uint32_t s, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    d &= m; s &= m;
    uint64_t full = (uint64_t)d + s;
    uint32_t r = (uint32_t)full & m;
    F.c = F.x = (uint8_t)((full >> OP_BITS(sz)) & 1);
    F.v = ((~(d ^ s) & (d ^ r)) & msb) != 0;  /* same-sign inputs, sign flipped */
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_sub(uint32_t d, uint32_t s, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    d &= m; s &= m;
    uint32_t r = (d - s) & m;
    F.c = F.x = s > d;                         /* borrow */
    F.v = (((d ^ s) & (d ^ r)) & msb) != 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

/* CMP is a SUB that only keeps the flags — and leaves X alone. */
static inline void op_cmp(uint32_t d, uint32_t s, int sz) {
    uint8_t x = F.x;
    op_sub(d, s, sz);
    F.x = x;
}

/* ADDX/SUBX: add the X flag; Z is only *cleared* (multi-word results). */
static inline uint32_t op_addx(uint32_t d, uint32_t s, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    d &= m; s &= m;
    uint64_t full = (uint64_t)d + s + F.x;
    uint32_t r = (uint32_t)full & m;
    F.c = F.x = (uint8_t)((full >> OP_BITS(sz)) & 1);
    F.v = ((~(d ^ s) & (d ^ r)) & msb) != 0;
    F.n = (r & msb) != 0;
    if (r) F.z = 0;
    return r;
}

static inline uint32_t op_subx(uint32_t d, uint32_t s, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    d &= m; s &= m;
    uint32_t r = (d - s - F.x) & m;
    F.c = F.x = ((uint64_t)s + F.x) > d;
    F.v = (((d ^ s) & (d ^ r)) & msb) != 0;
    F.n = (r & msb) != 0;
    if (r) F.z = 0;
    return r;
}

/* ---- shifts and rotates (cnt = 0..63) --------------------------------- */

static inline uint32_t op_asl(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz);
    uint32_t r;
    d &= m;
    if (cnt == 0) {
        r = d; F.c = 0; F.v = 0;
    } else if (cnt < bits) {
        r = (d << cnt) & m;
        F.c = F.x = (d >> (bits - cnt)) & 1;
        /* V: did the sign bit change at any point during the shift? The
         * bits that travel through the MSB must all be equal. */
        uint32_t top = (uint32_t)((((uint64_t)1 << (cnt + 1)) - 1) << (bits - 1 - cnt)) & m;
        F.v = (d & top) != 0 && (d & top) != top;
    } else {
        r = 0;
        F.c = F.x = (cnt == bits) ? (d & 1) : 0;
        F.v = d != 0;
    }
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_asr(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz);
    uint32_t r;
    d &= m;
    int neg = (d & msb) != 0;
    if (cnt == 0) {
        r = d; F.c = 0;
    } else if (cnt < bits) {
        r = d >> cnt;
        if (neg) r |= m & ~(m >> cnt);      /* replicate the sign bit */
        F.c = F.x = (d >> (cnt - 1)) & 1;
    } else {
        r = neg ? m : 0;
        F.c = F.x = (uint8_t)neg;
    }
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_lsl(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz);
    uint32_t r;
    d &= m;
    if (cnt == 0) {
        r = d; F.c = 0;
    } else if (cnt < bits) {
        r = (d << cnt) & m;
        F.c = F.x = (d >> (bits - cnt)) & 1;
    } else {
        r = 0;
        F.c = F.x = (cnt == bits) ? (d & 1) : 0;
    }
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_lsr(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz);
    uint32_t r;
    d &= m;
    if (cnt == 0) {
        r = d; F.c = 0;
    } else if (cnt < bits) {
        r = d >> cnt;
        F.c = F.x = (d >> (cnt - 1)) & 1;
    } else {
        r = 0;
        F.c = F.x = (cnt == bits) ? ((d & msb) != 0) : 0;
    }
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_rol(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz), n = cnt % bits;
    uint32_t r;
    d &= m;
    r = n ? ((d << n) | (d >> (bits - n))) & m : d;
    F.c = cnt ? (r & 1) : 0;                 /* last bit rotated out */
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_ror(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    unsigned bits = OP_BITS(sz), n = cnt % bits;
    uint32_t r;
    d &= m;
    r = n ? ((d >> n) | (d << (bits - n))) & m : d;
    F.c = cnt ? (r & msb) != 0 : 0;
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

/* ROXL/ROXR rotate through X: a (bits+1)-wide rotation. Small loop, rare op. */
static inline uint32_t op_roxl(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    uint32_t r = d & m, x = F.x;
    for (uint32_t i = 0; i < cnt % (OP_BITS(sz) + 1); i++) {
        uint32_t out = (r & msb) != 0;
        r = ((r << 1) | x) & m;
        x = out;
    }
    F.x = F.c = (uint8_t)x;
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

static inline uint32_t op_roxr(uint32_t d, uint32_t cnt, int sz) {
    uint32_t m = OP_MASK(sz), msb = OP_MSB(sz);
    uint32_t r = d & m, x = F.x;
    for (uint32_t i = 0; i < cnt % (OP_BITS(sz) + 1); i++) {
        uint32_t out = r & 1;
        r = (r >> 1) | (x ? msb : 0);
        x = out;
    }
    F.x = F.c = (uint8_t)x;
    F.v = 0;
    F.n = (r & msb) != 0;
    F.z = r == 0;
    return r;
}

/* ---- multiply / divide (16x16 -> 32, 32/16 -> 16:16) ------------------ */

static inline uint32_t op_mulu(uint32_t d, uint32_t s) {
    uint32_t r = (d & 0xFFFF) * (s & 0xFFFF);
    op_logic(r, 4);
    return r;
}

static inline uint32_t op_muls(uint32_t d, uint32_t s) {
    uint32_t r = (uint32_t)((int32_t)(int16_t)d * (int32_t)(int16_t)s);
    op_logic(r, 4);
    return r;
}

/* Divisor must be non-zero (the caller raises the zero-divide exception).
 * On overflow the destination is left untouched, V is set and C cleared
 * (Motorola manual); N and Z are undefined, we leave them alone. */
static inline uint32_t op_divu(uint32_t d, uint32_t s) {
    s &= 0xFFFF;
    uint32_t q = d / s, rem = d % s;
    if (q > 0xFFFF) { F.v = 1; F.c = 0; return d; }
    op_logic(q, 2);
    return (rem << 16) | q;
}

static inline uint32_t op_divs(uint32_t d, uint32_t s) {
    int32_t sd = (int32_t)d, ss = (int16_t)s;
    if (sd == INT32_MIN && ss == -1) { F.v = 1; F.c = 0; return d; }
    int32_t q = sd / ss, rem = sd % ss;      /* C truncates toward zero, like the 68000 */
    if (q < -32768 || q > 32767) { F.v = 1; F.c = 0; return d; }
    op_logic((uint32_t)q, 2);
    return ((uint32_t)rem & 0xFFFF) << 16 | ((uint32_t)q & 0xFFFF);
}

/* ---- binary-coded decimal (2 digits per byte) ------------------------- */

static inline uint32_t op_abcd(uint32_t d, uint32_t s) {
    uint32_t r = (s & 0x0F) + (d & 0x0F) + F.x;
    uint32_t v = ~r;
    if (r > 9) r += 6;
    r += (s & 0xF0) + (d & 0xF0);
    F.x = F.c = r > 0x99;
    if (F.c) r -= 0xA0;
    F.v = (v & r & 0x80) != 0;
    F.n = (r & 0x80) != 0;
    r &= 0xFF;
    if (r) F.z = 0;
    return r;
}

static inline uint32_t op_sbcd(uint32_t d, uint32_t s) {
    uint32_t r = (d & 0x0F) - (s & 0x0F) - F.x;
    uint32_t v = ~r;
    if (r > 9) r -= 6;
    r += (d & 0xF0) - (s & 0xF0);
    F.x = F.c = r > 0x99;
    if (F.c) r += 0xA0;
    r &= 0xFF;
    F.v = (v & r & 0x80) != 0;
    F.n = (r & 0x80) != 0;
    if (r) F.z = 0;
    return r;
}

/* NBCD: 0 - d - X in decimal. Returns the value to store. */
static inline uint32_t op_nbcd(uint32_t d) {
    uint32_t r = (0x9A - d - F.x) & 0xFF;
    if (r != 0x9A) {
        uint32_t v = ~r;
        if ((r & 0x0F) == 0x0A) r = (r & 0xF0) + 0x10;
        r &= 0xFF;
        F.v = (v & r & 0x80) != 0;
        if (r) F.z = 0;
        F.c = F.x = 1;
    } else {
        F.v = 0; F.c = F.x = 0;
        r = d & 0xFF;
    }
    F.n = (r & 0x80) != 0;
    return r;
}

#undef F

/* ---- conditions (Bcc, DBcc, Scc) -------------------------------------- */

#define COND_T  1
#define COND_F  0
#define COND_HI (!gase_cpu.c && !gase_cpu.z)
#define COND_LS (gase_cpu.c || gase_cpu.z)
#define COND_CC (!gase_cpu.c)
#define COND_CS (gase_cpu.c)
#define COND_NE (!gase_cpu.z)
#define COND_EQ (gase_cpu.z)
#define COND_VC (!gase_cpu.v)
#define COND_VS (gase_cpu.v)
#define COND_PL (!gase_cpu.n)
#define COND_MI (gase_cpu.n)
#define COND_GE (gase_cpu.n == gase_cpu.v)
#define COND_LT (gase_cpu.n != gase_cpu.v)
#define COND_GT (!gase_cpu.z && gase_cpu.n == gase_cpu.v)
#define COND_LE (gase_cpu.z || gase_cpu.n != gase_cpu.v)

#endif
