/*
 * cpu.c — the parts of the 68000 that are not individual instructions:
 * the status register, exceptions and interrupts, and the dispatcher that
 * glues recompiled functions together.
 */
#include <stdio.h>
#include <stdlib.h>

#include "gase/gase.h"
#include "gase/system.h"

GaseCpu gase_cpu;

/* ------------------------------------------------------------------------
 * Status register:  T . S . . I2 I1 I0 . . . X N Z V C
 *                  15   13     10  9  8       4 3 2 1 0
 * ---------------------------------------------------------------------- */

uint8_t gase_get_ccr(void) {
    return (uint8_t)(gase_cpu.x << 4 | gase_cpu.n << 3 | gase_cpu.z << 2 |
                     gase_cpu.v << 1 | gase_cpu.c);
}

void gase_set_ccr(uint8_t ccr) {
    gase_cpu.x = (ccr >> 4) & 1;
    gase_cpu.n = (ccr >> 3) & 1;
    gase_cpu.z = (ccr >> 2) & 1;
    gase_cpu.v = (ccr >> 1) & 1;
    gase_cpu.c = ccr & 1;
}

uint16_t gase_get_sr(void) {
    return (uint16_t)(gase_cpu.t << 15 | gase_cpu.s << 13 | gase_cpu.imask << 8 |
                      gase_get_ccr());
}

void gase_set_sr(uint16_t sr) {
    uint8_t s = (sr >> 13) & 1;
    /* Two stack pointers exist; A7 is whichever one the mode selects. */
    if (s != gase_cpu.s) {
        if (s) { gase_cpu.usp = A(7); A(7) = gase_cpu.ssp; }
        else   { gase_cpu.ssp = A(7); A(7) = gase_cpu.usp; }
        gase_cpu.s = s;
    }
    gase_cpu.t = (sr >> 15) & 1;
    gase_cpu.imask = (sr >> 8) & 7;
    gase_set_ccr((uint8_t)sr);
}

/* ------------------------------------------------------------------------
 * Dispatch
 * ---------------------------------------------------------------------- */

GaseFn gase_lookup(uint32_t addr) {
    size_t lo = 0, hi = gase_entry_count;
    addr &= 0xFFFFFF;
    while (lo < hi) {                         /* binary search, table sorted */
        size_t mid = (lo + hi) / 2;
        if (gase_entries[mid].addr < addr) lo = mid + 1;
        else hi = mid;
    }
    if (lo < gase_entry_count && gase_entries[lo].addr == addr)
        return gase_entries[lo].fn;
    return NULL;
}

void gase_unresolved(uint32_t addr, const char *why) {
    /* The coverage feedback loop: record the address, so that the next
     * `gase recomp --entries gase_missing.txt` turns it into a function. */
    FILE *f = fopen("gase_missing.txt", "a");
    if (f) { fprintf(f, "0x%06X\n", addr & 0xFFFFFF); fclose(f); }
    fprintf(stderr,
            "[gase] no recompiled code for $%06X (%s).\n"
            "[gase] It was appended to gase_missing.txt; re-run\n"
            "[gase]   python -m gase recomp <rom> --entries gase_missing.txt ...\n",
            addr & 0xFFFFFF, why);
    gase_platform_fatal("Unknown code address, see gase_missing.txt");
    exit(2);
}

void gase_dispatch(void) {
    for (int hops = 0; hops < 16; hops++) {
        uint32_t pc = gase_cpu.pc & 0xFFFFFF;
        GaseFn fn = gase_lookup(pc);
        if (fn) { gase_cpu.pc = pc; fn(); return; }
        /* Code in RAM can't be recompiled ahead of time, but the common
         * case is a single `jmp $xxxxxx.l` (opcode $4EF9) that the game
         * copied there at boot: we can just follow it. */
        if (pc >= 0xE00000 && R16(pc) == 0x4EF9) {
            gase_cpu.pc = R32(pc + 2);
            continue;
        }
        gase_unresolved(pc, pc >= 0xE00000 ? "code in RAM" : "indirect jump or call");
    }
    gase_unresolved(gase_cpu.pc, "too many RAM trampolines");
}

/* Keep executing until the code returns to `expected_pc` with the stack
 * back where it was (a call returning normally). Returns false when the
 * stack unwinds *past* our frame: the game abandoned this call (e.g. a
 * soft reset), so our C caller must return too. */
bool gase_resume(uint32_t expected_pc, uint32_t expected_sp) {
    for (;;) {
        if (gase_cpu.pc == expected_pc && A(7) == expected_sp) return true;
        if (A(7) > expected_sp) return false;
        gase_dispatch();
    }
}

/* ------------------------------------------------------------------------
 * Exceptions and interrupts
 * ---------------------------------------------------------------------- */

static bool enter_exception(int vector, uint32_t resume_pc, int new_mask) {
    uint16_t sr = gase_get_sr();
    uint32_t sp = A(7);                       /* where RTE will bring A7 back */
    gase_set_sr((uint16_t)((sr | 0x2000) & 0x7FFF));   /* supervisor, no trace */
    if (new_mask >= 0) gase_cpu.imask = (uint8_t)new_mask;
    PUSH32(resume_pc);
    PUSH16(sr);
    gase_cpu.pc = R32((uint32_t)vector * 4);
    gase_dispatch();
    /* The handler ends with RTE, which restores SR and pops resume_pc. */
    if (gase_cpu.pc == resume_pc && A(7) == sp) return true;
    return gase_resume(resume_pc, sp) && gase_cpu.pc == resume_pc;
}

bool gase_exception(int vector, uint32_t resume_pc) {
    return enter_exception(vector, resume_pc, -1);
}

/* Take the highest pending interrupt if the mask allows it. Level 7 is
 * non-maskable; the Genesis uses level 6 (VBlank), 4 (HBlank), 2 (pad). */
bool gase_check_irq(uint32_t resume_pc) {
    for (;;) {
        int level = gase_system_irq_level();
        if (level == 0 || (level <= gase_cpu.imask && level != 7)) return true;
        gase_system_ack_irq(level);
        gase_cpu.stopped = false;
        if (!enter_exception(24 + level, resume_pc, level)) return false;
    }
}

bool gase_events(uint32_t resume_pc) {
    while (gase_cpu.cycles >= gase_cpu.next_event)
        gase_system_tick();                    /* one scanline */
    return gase_check_irq(resume_pc);
}

/* STOP: sleep until an interrupt arrives. */
bool gase_stop(uint32_t resume_pc) {
    gase_cpu.stopped = true;
    while (gase_cpu.stopped) {
        gase_cpu.cycles = gase_cpu.next_event;
        if (!gase_events(resume_pc)) return false;
    }
    return true;
}
