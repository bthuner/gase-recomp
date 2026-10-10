/*
 * system.h — the machine around the CPU: scheduler, interrupts, platform.
 *
 * Time is measured in 68000 cycles. An NTSC Genesis draws 262 scanlines
 * per frame, 60 frames per second, and the 68000 runs at 7.67 MHz:
 * 7670453 / 60 / 262 ~= 488 cycles per scanline.
 */
#ifndef GASE_SYSTEM_H
#define GASE_SYSTEM_H

#include <stdbool.h>
#include <stdint.h>

#define GASE_CYCLES_PER_LINE  488
#define GASE_LINES_PER_FRAME  262
#define GASE_AUDIO_RATE       44100

/* Called by cpu.c */
void gase_system_tick(void);         /* advance one scanline               */
int  gase_system_irq_level(void);    /* highest pending interrupt level    */
void gase_system_ack_irq(int level);

/* ---- controllers (io.c) ------------------------------------------------ */

/* Buttons, 1 = pressed. Set by the platform layer for pads 0 and 1. */
enum {
    GASE_BTN_UP = 1 << 0, GASE_BTN_DOWN = 1 << 1, GASE_BTN_LEFT = 1 << 2,
    GASE_BTN_RIGHT = 1 << 3, GASE_BTN_A = 1 << 4, GASE_BTN_B = 1 << 5,
    GASE_BTN_C = 1 << 6, GASE_BTN_START = 1 << 7,
    GASE_BTN_X = 1 << 8, GASE_BTN_Y = 1 << 9, GASE_BTN_Z = 1 << 10,
    GASE_BTN_MODE = 1 << 11
};
extern uint16_t gase_pad[2];

void    gase_io_reset(void);
uint8_t gase_io_read(uint32_t addr);
void    gase_io_write(uint32_t addr, uint8_t value);

/* ---- implemented by each platform (platform_sdl.c, platform_headless.c) */

/* A frame is complete: show it, read the inputs, wait for the next 1/60 s.
 * Return false to quit. */
bool gase_platform_frame(const uint32_t *pixels, int width, int height);
void gase_platform_audio(const int16_t *samples, int count);
void gase_platform_fatal(const char *message);

#endif
