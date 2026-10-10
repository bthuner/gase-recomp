/*
 * system.c — boot, the scanline scheduler and the main loop.
 *
 * The recompiled code runs "for real" on the host CPU, much faster than a
 * 68000. To keep the game in sync with the video hardware we count the
 * (estimated) cycles of every instruction. When a scanline's worth of
 * cycles has elapsed, the next POLL point calls gase_events(), which runs
 * gase_system_tick() to draw the line and maybe raise an interrupt.
 */
#include <stdio.h>
#include <stdlib.h>

#include "gase/gase.h"
#include "gase/psg.h"
#include "gase/system.h"
#include "gase/vdp.h"

static int line;
static int16_t audio[GASE_AUDIO_RATE / 50];

uint32_t gase_crc32(const uint8_t *data, size_t size) {
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < size; i++) {
        crc ^= data[i];
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1)));
    }
    return ~crc;
}

bool gase_system_init(uint8_t *rom, uint32_t size) {
    if (size != gase_rom_expected_size || gase_crc32(rom, size) != gase_rom_crc32) {
        fprintf(stderr, "[gase] this ROM is not the one the code was recompiled from "
                        "(expected %u bytes, CRC32 %08X)\n",
                (unsigned)gase_rom_expected_size, (unsigned)gase_rom_crc32);
        return false;
    }
    gase_rom = rom;
    gase_rom_size = size;
    gase_bus_reset();
    gase_io_reset();
    gase_vdp_reset();
    gase_psg_reset(GASE_AUDIO_RATE);

    /* 68000 reset: supervisor mode, interrupts masked, SSP and PC read from
     * the first two vectors of the ROM. */
    gase_cpu = (GaseCpu){0};
    gase_cpu.s = 1;
    gase_cpu.imask = 7;
    A(7) = R32(0);
    gase_cpu.pc = R32(4);
    gase_cpu.next_event = GASE_CYCLES_PER_LINE;
    line = 0;
    return true;
}

void gase_system_tick(void) {
    gase_vdp_run_line(line);
    if (line == gase_vdp_active_lines()) {
        /* The beam just left the visible area: the frame is ready. */
        int samples = GASE_AUDIO_RATE / 60;
        gase_psg_render(audio, samples);
        gase_platform_audio(audio, samples);
        if (!gase_platform_frame(gase_vdp.framebuffer, gase_vdp.width, gase_vdp.height))
            exit(0);
    }
    if (++line == GASE_LINES_PER_FRAME) line = 0;
    gase_cpu.next_event += GASE_CYCLES_PER_LINE;
}

int gase_system_irq_level(void) {
    return gase_vdp_irq_level();
}

void gase_system_ack_irq(int level) {
    gase_vdp_ack_irq(level);
}

void gase_run(void) {
    for (;;) gase_dispatch();
}
