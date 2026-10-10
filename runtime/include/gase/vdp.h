/*
 * vdp.h — Sega Genesis / Mega Drive Video Display Processor (YM7101).
 *
 * The VDP is the graphics chip. The 68000 talks to it through two ports:
 *   - the DATA port    ($C00000 / $C00002) : read/write VRAM, CRAM, VSRAM
 *   - the CONTROL port ($C00004 / $C00006) : set registers, set the address,
 *                                            start DMA, read the status word
 * The runtime renders one scanline at a time (gase_vdp_render_line) so that
 * mid-frame tricks (horizontal interrupts, raster effects) keep working.
 */
#ifndef GASE_VDP_H
#define GASE_VDP_H

#include <stdint.h>
#include <stdbool.h>

#define GASE_FB_WIDTH  320   /* widest mode (H40). H32 uses 256 pixels. */
#define GASE_FB_HEIGHT 240   /* tallest mode (V30, PAL). NTSC uses 224.  */

typedef struct GaseVdp {
    uint8_t  vram[0x10000];   /* 64 KiB of video RAM: tiles + maps + sprites */
    uint16_t cram[64];        /* colour RAM: 4 palettes x 16 colours (9-bit) */
    uint16_t vsram[40];       /* vertical scroll RAM                          */
    uint8_t  reg[24];         /* the 24 VDP registers                         */

    /* Control-port state machine (2-word commands) */
    bool     pending;         /* first half of a command was written         */
    uint8_t  code;            /* CD5..CD0: target (VRAM/CRAM/VSRAM) + DMA     */
    uint16_t addr;            /* current access address                       */
    uint16_t read_buffer;     /* prefetched word for data-port reads          */
    bool     dma_fill_pending;/* VRAM fill waits for the next data write      */

    /* Timing / interrupts */
    int      line;            /* current scanline (0..261 on NTSC)            */
    int      hint_counter;    /* counts down to the next horizontal interrupt */
    bool     vint_pending;    /* level 6 interrupt requested                  */
    bool     hint_pending;    /* level 4 interrupt requested                  */
    bool     in_vblank;
    bool     odd_frame;
    bool     sprite_overflow;
    bool     sprite_collision;

    /* Output: 0x00RRGGBB pixels. Only width x height are meaningful. */
    uint32_t framebuffer[GASE_FB_WIDTH * GASE_FB_HEIGHT];
    int      width;           /* 256 or 320 */
    int      height;          /* 224 or 240 */
} GaseVdp;

extern GaseVdp gase_vdp;

void     gase_vdp_reset(void);

/* Port accesses coming from the 68000 bus (addresses already decoded). */
void     gase_vdp_write_data(uint16_t value);
uint16_t gase_vdp_read_data(void);
void     gase_vdp_write_ctrl(uint16_t value);
uint16_t gase_vdp_read_status(void);
uint16_t gase_vdp_read_hv(int cycles_into_line);

/* Called by the scheduler once per scanline (0..lines_per_frame-1).
 * Renders the line if it is visible, updates the H/V interrupt state. */
void     gase_vdp_run_line(int line);

/* Interrupt acknowledge, called when the 68000 takes level 4 or 6. */
void     gase_vdp_ack_irq(int level);

/* Highest interrupt level the VDP currently asserts (0, 4 or 6),
 * already masked by the enable bits in registers 0 and 1. */
int      gase_vdp_irq_level(void);

/* Visible lines for the current mode (224 or 240). */
int      gase_vdp_active_lines(void);

#endif
