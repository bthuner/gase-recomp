/*
 * gase.h — everything recompiled code needs.
 *
 * The recompiler (python -m gase recomp) turns every 68000 function of the
 * ROM into a C function `void f_XXXXXX(void)`. Those functions only touch
 * the machine through the macros and inline helpers declared here:
 *
 *   registers ........ D(n), A(n), SETD8/SETD16, the flags in gase_cpu
 *   memory ........... R8/R16/R32, W8/W16/W32, PUSH32/POP32
 *   arithmetic ....... op_add, op_sub, op_logic, ... (see m68k_ops.h)
 *   control flow ..... CALL, CALL_INDIRECT, JUMP, POLL, EXCEPTION
 *
 * Keeping this contract small is what lets the same generated code run on
 * desktop, Android and iOS: only the platform layer changes.
 */
#ifndef GASE_H
#define GASE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "gase/bus.h"

/* ------------------------------------------------------------------------
 * CPU state
 * ---------------------------------------------------------------------- */

typedef struct GaseCpu {
    uint32_t d[8];          /* data registers D0-D7                         */
    uint32_t a[8];          /* address registers A0-A7 (A7 = active stack)  */
    uint32_t usp;           /* the *inactive* stack pointer when in super-  */
    uint32_t ssp;           /*   visor (usp) or user (ssp) mode             */
    uint32_t pc;            /* only meaningful between functions: the next  */
                            /*   address to execute (see "dispatch" below)  */
    /* The status register is kept "exploded": one byte per flag is much
     * cheaper to update than packing bits after every instruction. */
    uint8_t  x, n, z, v, c; /* condition codes (0 or 1)                     */
    uint8_t  s;             /* supervisor mode                              */
    uint8_t  t;             /* trace mode (ignored)                         */
    uint8_t  imask;         /* interrupt mask, 0..7                         */
    bool     stopped;       /* executing a STOP instruction                 */

    int64_t  cycles;        /* 68000 cycles since power-on                  */
    int64_t  next_event;    /* when the scheduler must run (end of line)    */
} GaseCpu;

extern GaseCpu gase_cpu;

#define D(n) (gase_cpu.d[n])
#define A(n) (gase_cpu.a[n])

/* Writing a byte or word to a data register leaves the upper bits alone. */
#define SETD8(n, v)  (D(n) = (D(n) & 0xFFFFFF00u) | ((uint32_t)(v) & 0xFFu))
#define SETD16(n, v) (D(n) = (D(n) & 0xFFFF0000u) | ((uint32_t)(v) & 0xFFFFu))

#define SEXT8(v)  ((uint32_t)(int32_t)(int8_t)(uint8_t)(v))
#define SEXT16(v) ((uint32_t)(int32_t)(int16_t)(uint16_t)(v))

/* Rough cycle accounting: every recompiled instruction adds its estimate. */
#define CYC(n) (gase_cpu.cycles += (n))

uint16_t gase_get_sr(void);
void     gase_set_sr(uint16_t sr);     /* may swap stacks (S bit)        */
uint8_t  gase_get_ccr(void);
void     gase_set_ccr(uint8_t ccr);

/* ------------------------------------------------------------------------
 * Memory. The 68000 has a 24-bit address bus: the top 8 bits are ignored.
 * ROM and work RAM get inline fast paths, everything else goes to bus.c.
 * ---------------------------------------------------------------------- */

#ifdef GASE_FLAT_MEMORY
/* Test builds (differential testing against a reference CPU) replace the
 * Genesis memory map with 16 MiB of flat RAM. */
extern uint8_t gase_flat[0x1000000];
static inline uint32_t R8(uint32_t a)  { return gase_flat[a & 0xFFFFFF]; }
static inline uint32_t R16(uint32_t a) { a &= 0xFFFFFF; return (uint32_t)gase_flat[a] << 8 | gase_flat[(a + 1) & 0xFFFFFF]; }
static inline void W8(uint32_t a, uint32_t v)  { gase_flat[a & 0xFFFFFF] = (uint8_t)v; }
static inline void W16(uint32_t a, uint32_t v) { a &= 0xFFFFFF; gase_flat[a] = (uint8_t)(v >> 8); gase_flat[(a + 1) & 0xFFFFFF] = (uint8_t)v; }
#else
static inline uint32_t R8(uint32_t a) {
    a &= 0xFFFFFF;
    if (a < gase_rom_size) return gase_rom[a];
    if (a >= 0xE00000) return gase_ram[a & 0xFFFF];
    return gase_bus_read8(a);
}
static inline uint32_t R16(uint32_t a) {
    a &= 0xFFFFFF;
    if (a < gase_rom_size) return (uint32_t)gase_rom[a] << 8 | gase_rom[a + 1];
    if (a >= 0xE00000) { a &= 0xFFFF; return (uint32_t)gase_ram[a] << 8 | gase_ram[(a + 1) & 0xFFFF]; }
    return gase_bus_read16(a);
}
static inline void W8(uint32_t a, uint32_t v) {
    a &= 0xFFFFFF;
    if (a >= 0xE00000) gase_ram[a & 0xFFFF] = (uint8_t)v;
    else gase_bus_write8(a, (uint8_t)v);
}
static inline void W16(uint32_t a, uint32_t v) {
    a &= 0xFFFFFF;
    if (a >= 0xE00000) { a &= 0xFFFF; gase_ram[a] = (uint8_t)(v >> 8); gase_ram[(a + 1) & 0xFFFF] = (uint8_t)v; }
    else gase_bus_write16(a, (uint16_t)v);
}
#endif

/* A long access is two word accesses, high word first (like the 68000). */
static inline uint32_t R32(uint32_t a) { return R16(a) << 16 | R16(a + 2); }
static inline void W32(uint32_t a, uint32_t v) { W16(a, v >> 16); W16(a + 2, v); }

static inline void PUSH16(uint32_t v) { A(7) -= 2; W16(A(7), v); }
static inline void PUSH32(uint32_t v) { A(7) -= 4; W32(A(7), v); }
static inline uint32_t POP16(void) { uint32_t v = R16(A(7)); A(7) += 2; return v; }
static inline uint32_t POP32(void) { uint32_t v = R32(A(7)); A(7) += 4; return v; }

#include "gase/m68k_ops.h"

/* ------------------------------------------------------------------------
 * Control flow and dispatch
 *
 * A 68000 "function" (code reached by BSR/JSR) becomes a C function. A
 * call pushes the return address on the *emulated* stack, sets
 * gase_cpu.pc to the callee and calls it. RTS pops the return address
 * into gase_cpu.pc and returns. Back in the caller we check that pc is
 * the address we expected: games sometimes play tricks with the stack,
 * and when that happens gase_resume() takes over and keeps running code
 * through the dispatcher until the expected return address shows up.
 * ---------------------------------------------------------------------- */

typedef void (*GaseFn)(void);

typedef struct GaseEntry {
    uint32_t addr;          /* 68000 address                              */
    GaseFn   fn;            /* C function that can start running there   */
} GaseEntry;

/* Generated by the recompiler (recomp_table.c): sorted by address. */
extern const GaseEntry gase_entries[];
extern const size_t    gase_entry_count;

GaseFn gase_lookup(uint32_t addr);         /* NULL when unknown            */
void   gase_dispatch(void);                /* run the code at gase_cpu.pc  */
bool   gase_resume(uint32_t expected_pc, uint32_t expected_sp);
void   gase_unresolved(uint32_t addr, const char *why);

/* Scheduler hook: run pending scanlines / take interrupts. Returns false
 * when an interrupt handler changed where execution continues. */
bool   gase_events(uint32_t resume_pc);
bool   gase_check_irq(uint32_t resume_pc);
bool   gase_exception(int vector, uint32_t resume_pc);
bool   gase_stop(uint32_t resume_pc);

#define CALL(fn, target, ret) do {                                         \
        uint32_t sp_ = A(7);                                              \
        PUSH32(ret);                                                      \
        gase_cpu.pc = (target);                                           \
        fn();                                                             \
        if (gase_cpu.pc != (ret) && !gase_resume((ret), sp_)) return;     \
    } while (0)

#define CALL_INDIRECT(target, ret) do {                                    \
        uint32_t sp_ = A(7), t_ = (target);                               \
        PUSH32(ret);                                                      \
        gase_cpu.pc = t_;                                                 \
        gase_dispatch();                                                  \
        if (gase_cpu.pc != (ret) && !gase_resume((ret), sp_)) return;     \
    } while (0)

/* Leave this C function and let the dispatcher continue at `target`. */
#define JUMP(target) do { gase_cpu.pc = (target); return; } while (0)

/* Poll points are placed on backward branches (loops): this is where the
 * scheduler gets a chance to advance the video beam and fire interrupts. */
#define POLL(resume) do {                                                  \
        if (gase_cpu.cycles >= gase_cpu.next_event && !gase_events(resume)) \
            return;                                                       \
    } while (0)

#define EXCEPTION(vec, resume) do {                                        \
        if (!gase_exception((vec), (resume))) return;                     \
    } while (0)

/* ------------------------------------------------------------------------
 * System
 * ---------------------------------------------------------------------- */

/* Generated by the recompiler (recomp_config.c). */
extern const char     gase_game_title[];
extern const uint32_t gase_rom_crc32;
extern const uint32_t gase_rom_expected_size;

uint32_t gase_crc32(const uint8_t *data, size_t size);
bool     gase_system_init(uint8_t *rom, uint32_t size);   /* false = bad ROM */
void     gase_run(void);                                  /* never returns   */

#endif
