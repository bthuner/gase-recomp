/*
 * harness.c — runs recompiled test snippets for the differential test.
 *
 * Built with -DGASE_FLAT_MEMORY: memory is 16 MiB of plain RAM, exactly
 * like the reference emulator (Musashi) on the Python side. For every test
 * case we load the same initial state, run the recompiled code and print
 * the final state; tests/difftest/difftest.py compares it with Musashi.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "gase/gase.h"
#include "gase/system.h"

uint8_t gase_flat[0x1000000];
static uint8_t low_image[0x10000], ram_image[0x10000];

/* The scheduler, platform and ROM identity are not needed here. */
void gase_system_tick(void) { gase_cpu.next_event += 1000000; }
int  gase_system_irq_level(void) { return 0; }
void gase_system_ack_irq(int level) { (void)level; }
void gase_platform_fatal(const char *message) { fprintf(stderr, "fatal: %s\n", message); }

static uint32_t crc32(const uint8_t *p, size_t n) {
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; i++) {
        crc ^= p[i];
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1)));
    }
    return ~crc;
}

static void load(const char *path, uint8_t *dst, size_t n) {
    FILE *f = fopen(path, "rb");
    if (!f || fread(dst, 1, n, f) != n) { perror(path); exit(1); }
    fclose(f);
}

int main(int argc, char **argv) {
    if (argc != 5) { fprintf(stderr, "usage: harness low.bin ram.bin cases.bin out.txt\n"); return 1; }
    load(argv[1], low_image, sizeof low_image);
    load(argv[2], ram_image, sizeof ram_image);
    FILE *in = fopen(argv[3], "rb"), *out = fopen(argv[4], "w");
    if (!in || !out) { perror("cases"); return 1; }
    uint32_t c[19];   /* entry, d0-d7, a0-a7, usp, sr */
    while (fread(c, 4, 19, in) == 19) {
        memcpy(gase_flat, low_image, sizeof low_image);
        memcpy(gase_flat + 0xFF0000, ram_image, sizeof ram_image);
        memset(&gase_cpu, 0, sizeof gase_cpu);
        gase_cpu.s = 1;
        gase_cpu.next_event = 1000000;
        for (int i = 0; i < 8; i++) { D(i) = c[1 + i]; A(i) = c[9 + i]; }
        gase_cpu.usp = c[17];
        gase_set_sr((uint16_t)c[18]);
        PUSH32(0xF00);                        /* sentinel return address */
        gase_cpu.pc = c[0];
        gase_dispatch();
        fprintf(out, "%08X", gase_cpu.pc);
        for (int i = 0; i < 8; i++) fprintf(out, " %08X", D(i));
        for (int i = 0; i < 8; i++) fprintf(out, " %08X", A(i));
        fprintf(out, " %08X %04X %08X\n", gase_cpu.usp, gase_get_sr(),
                crc32(gase_flat + 0xFF0000, 0x10000));
    }
    fclose(in);
    fclose(out);
    return 0;
}
