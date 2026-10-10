/*
 * vdp.c — Sega Genesis / Mega Drive Video Display Processor (YM7101).
 *
 * This file is meant to be read. It implements the VDP the way a game sees
 * it: two I/O ports, 24 registers, three memories and a line-based renderer.
 *
 *   VRAM  (64 KiB)  tiles (8x8 pixels, 4 bits per pixel = 32 bytes each),
 *                   name tables for planes A/B/window, the sprite table and
 *                   the horizontal scroll table. Their location is set by
 *                   registers, so VRAM layout is entirely up to the game.
 *   CRAM  (64 x 9 bits)  four palettes of 16 colours. Colour 0 of every
 *                   palette is "transparent" for planes and sprites.
 *   VSRAM (40 x 10 bits) vertical scroll values (one pair per 16 px column).
 *
 * Accuracy target: "good enough for typical games", not cycle exactness.
 * DMA completes instantly and the screen is drawn one whole line at a time.
 */
#include "gase/vdp.h"
#include "gase/bus.h"

#include <string.h>

GaseVdp gase_vdp;

/* ------------------------------------------------------------------------ */
/* Register helpers                                                         */
/* ------------------------------------------------------------------------ */
/*
 * The registers that matter for this implementation:
 *   0  bit4 IE1 (H-interrupt enable), bit5 blank leftmost 8 pixels
 *   1  bit6 DISP (display on), bit5 IE0 (V-interrupt enable),
 *      bit4 M1 (DMA enable), bit3 M2 (V30 = 240 lines)
 *   2  plane A name table  (bits 5-3 -> address bits 15-13)
 *   3  window name table   (bits 5-1 -> address bits 15-11)
 *   4  plane B name table  (bits 2-0 -> address bits 15-13)
 *   5  sprite table        (bits 6-0 -> address bits 15-9)
 *   7  backdrop colour     (bits 5-4 palette, bits 3-0 colour)
 *  10  H-interrupt counter reload value
 *  11  bit2 vertical scroll mode, bits 1-0 horizontal scroll mode
 *  12  bit0 (and bit7) H40 = 320 pixels wide, bit3 shadow/highlight
 *  13  horizontal scroll table (bits 5-0 -> address bits 15-10)
 *  15  auto-increment added to the address after each data port access
 *  16  plane size: bits 5-4 height, bits 1-0 width (0=32, 1=64, 3=128 cells)
 *  17  window H position, 18 window V position
 *  19,20  DMA length (low, high) in words
 *  21,22,23  DMA source (low, mid, high); bits 7-6 of reg 23 = DMA mode
 */
#define REG(n) (gase_vdp.reg[(n)])

static bool h40(void)            { return (REG(12) & 0x01) != 0; }
static bool display_enabled(void){ return (REG(1) & 0x40) != 0; }
static bool dma_enabled(void)    { return (REG(1) & 0x10) != 0; }

int gase_vdp_active_lines(void)
{
    return (REG(1) & 0x08) ? 240 : 224;
}

/* ------------------------------------------------------------------------ */
/* Colour conversion                                                        */
/* ------------------------------------------------------------------------ */
/*
 * A CRAM entry is 0000 BBB0 GGG0 RRR0: three bits per component. The DAC
 * is not linear; this table is the commonly used measurement of real
 * hardware output levels (normal, non-shadowed mode).
 */
static const uint8_t level_8bit[8] = { 0, 52, 87, 116, 144, 172, 206, 255 };

static uint32_t cram_to_rgb(uint16_t c)
{
    uint32_t r = level_8bit[(c >> 1) & 7];
    uint32_t g = level_8bit[(c >> 5) & 7];
    uint32_t b = level_8bit[(c >> 9) & 7];
    return (r << 16) | (g << 8) | b;
}

/* ------------------------------------------------------------------------ */
/* Memory access through the data port                                      */
/* ------------------------------------------------------------------------ */
/*
 * The low 4 bits of the code register (CD3..CD0) choose the target:
 *   0001 VRAM write   0000 VRAM read
 *   0011 CRAM write   1000 CRAM read
 *   0101 VSRAM write  0100 VSRAM read
 * CD5 requests DMA; CD4 is only used by VRAM copy (it reads back as set).
 */
enum {
    CODE_VRAM_READ   = 0x0, CODE_VRAM_WRITE  = 0x1,
    CODE_CRAM_WRITE  = 0x3,
    CODE_VSRAM_READ  = 0x4, CODE_VSRAM_WRITE = 0x5,
    CODE_CRAM_READ   = 0x8,
    CODE_DMA         = 0x20
};

/*
 * VRAM is byte addressed but the port is 16 bits wide. A word written to
 * address A puts the high byte at A and the low byte at A^1. For an even
 * address that is the obvious big-endian layout; for an odd address the
 * two bytes come out swapped. Tiles and tables are stored big-endian.
 */
static void vram_write_word(uint16_t a, uint16_t value)
{
    gase_vdp.vram[a]             = (uint8_t)(value >> 8);
    gase_vdp.vram[(uint16_t)(a ^ 1)] = (uint8_t)value;
}

static uint16_t vram_read_word(uint16_t a)
{
    a &= 0xFFFE;
    return (uint16_t)((gase_vdp.vram[a] << 8) | gase_vdp.vram[a + 1]);
}

/* Store one word at the current address in whatever memory `code` selects. */
static void write_target(uint16_t value)
{
    uint16_t a = gase_vdp.addr;
    switch (gase_vdp.code & 0x0F) {
    case CODE_VRAM_WRITE:
        vram_write_word(a, value);
        break;
    case CODE_CRAM_WRITE:          /* only the 9 colour bits are stored */
        gase_vdp.cram[(a >> 1) & 0x3F] = value & 0x0EEE;
        break;
    case CODE_VSRAM_WRITE:         /* 10-bit scroll values, 40 entries   */
        if (((a >> 1) & 0x3F) < 40)
            gase_vdp.vsram[(a >> 1) & 0x3F] = value & 0x07FF;
        break;
    default:                       /* writes with a read code are ignored */
        break;
    }
}

/* Fetch the word at the current address for a read command. */
static uint16_t read_target(void)
{
    uint16_t a = gase_vdp.addr;
    switch (gase_vdp.code & 0x0F) {
    case CODE_VRAM_READ:  return vram_read_word(a);
    case CODE_CRAM_READ:  return gase_vdp.cram[(a >> 1) & 0x3F];
    case CODE_VSRAM_READ: {
        unsigned i = (a >> 1) & 0x3F;
        return i < 40 ? gase_vdp.vsram[i] : gase_vdp.vsram[0];
    }
    default:              return 0;
    }
}

static void advance_addr(void)
{
    gase_vdp.addr = (uint16_t)(gase_vdp.addr + REG(15));
}

/* ------------------------------------------------------------------------ */
/* DMA                                                                      */
/* ------------------------------------------------------------------------ */
/*
 * Three DMA modes, selected by register 23 bits 7-6:
 *   0x  68k -> VDP : copy words from the 68000 bus (ROM/RAM) into VRAM,
 *                    CRAM or VSRAM. Source = regs 23:22:21 shifted left 1
 *                    (bit 6 of reg 23 is source bit 23, hence "0x").
 *   10  VRAM fill  : armed by the command, runs on the next data write;
 *                    the high byte of that data is repeated over VRAM.
 *   11  VRAM copy  : byte copy inside VRAM, source = regs 22:21.
 * Length = regs 20:19 (words for 68k transfers, bytes for fill/copy);
 * a length of 0 means 0x10000. Afterwards the length registers read 0 and
 * the source registers point past the last byte moved, like hardware.
 * Real DMA takes time and stalls the 68000; here it is instantaneous.
 */
static uint32_t dma_length(void)
{
    uint32_t len = (uint32_t)REG(19) | ((uint32_t)REG(20) << 8);
    return len ? len : 0x10000;
}

/* Store the updated source (in words) back into regs 21/22 and zero length. */
static void dma_finish(uint32_t src_words)
{
    REG(21) = (uint8_t)src_words;
    REG(22) = (uint8_t)(src_words >> 8);
    REG(19) = 0;
    REG(20) = 0;
    gase_vdp.code &= (uint8_t)~CODE_DMA;   /* DMA is over */
}

static void dma_68k_to_vdp(void)
{
    uint32_t len = dma_length();
    /* Source counter is in words; only its low 16 bits increment, so a
     * transfer wraps inside a 128 KiB block (a well-known hardware quirk). */
    uint32_t hi  = (uint32_t)(REG(23) & 0x7F) << 16;
    uint32_t lo  = (uint32_t)REG(21) | ((uint32_t)REG(22) << 8);

    while (len--) {
        uint32_t src = (hi | lo) << 1;
        write_target(gase_bus_read16(src));
        lo = (lo + 1) & 0xFFFF;
        advance_addr();
    }
    dma_finish(lo);
}

static void dma_vram_fill(uint16_t value)
{
    uint32_t len = dma_length();
    uint32_t src = (uint32_t)REG(21) | ((uint32_t)REG(22) << 8);

    /* The data write itself happens normally first ... */
    write_target(value);
    /* ... then the fill repeats the high byte, landing on addr^1. For
     * CRAM/VSRAM targets the whole word is repeated instead. */
    while (len--) {
        if ((gase_vdp.code & 0x0F) == CODE_VRAM_WRITE)
            gase_vdp.vram[(uint16_t)(gase_vdp.addr ^ 1)] = (uint8_t)(value >> 8);
        else
            write_target(value);
        advance_addr();
        src = (src + 1) & 0xFFFF;
    }
    gase_vdp.dma_fill_pending = false;
    dma_finish(src);
}

static void dma_vram_copy(void)
{
    uint32_t len = dma_length();
    uint32_t src = (uint32_t)REG(21) | ((uint32_t)REG(22) << 8);

    while (len--) {
        gase_vdp.vram[gase_vdp.addr] = gase_vdp.vram[src];
        src = (src + 1) & 0xFFFF;
        advance_addr();
    }
    dma_finish(src);
}

static void dma_start(void)
{
    switch (REG(23) >> 6) {
    case 0: case 1: dma_68k_to_vdp();                  break;
    case 2:         gase_vdp.dma_fill_pending = true;  break;
    case 3:         dma_vram_copy();                   break;
    }
}

/* ------------------------------------------------------------------------ */
/* Ports                                                                    */
/* ------------------------------------------------------------------------ */

void gase_vdp_reset(void)
{
    memset(&gase_vdp, 0, sizeof gase_vdp);
    gase_vdp.width  = 256;
    gase_vdp.height = 224;
}

/*
 * Control port. Two kinds of words arrive here:
 *
 *   Register write (one word, only when no command is half-written):
 *       100R RRRR VVVV VVVV      register R := V
 *
 *   Address/command (two words):
 *       first : CD1 CD0 A13 A12 A11 A10 A9 A8 A7 A6 A5 A4 A3 A2 A1 A0
 *       second:  0   0   0   0   0   0   0  0 CD5 CD4 CD3 CD2 0 0 A15 A14
 *
 * The first word already updates the low address/code bits (games rely on
 * this), the second word completes the command and may start a DMA.
 */
void gase_vdp_write_ctrl(uint16_t value)
{
    if (!gase_vdp.pending) {
        if ((value & 0xC000) == 0x8000) {
            unsigned r = (value >> 8) & 0x1F;
            if (r < 24)
                gase_vdp.reg[r] = (uint8_t)value;
            /* A real VDP also clears CD1-CD0 here; nothing in this model
             * depends on it, so the code register is left untouched. */
            return;
        }
        gase_vdp.code = (uint8_t)((gase_vdp.code & 0x3C) | (value >> 14));
        gase_vdp.addr = (uint16_t)((gase_vdp.addr & 0xC000) | (value & 0x3FFF));
        gase_vdp.pending = true;
        return;
    }

    gase_vdp.pending = false;
    gase_vdp.code = (uint8_t)((gase_vdp.code & 0x03) | ((value >> 2) & 0x3C));
    gase_vdp.addr = (uint16_t)((gase_vdp.addr & 0x3FFF) | ((value & 3) << 14));

    if ((gase_vdp.code & CODE_DMA) && dma_enabled())
        dma_start();
    else
        gase_vdp.code &= (uint8_t)~CODE_DMA; /* DMA bit ignored if disabled */

    /* A read command prefetches the first word right away. */
    if ((gase_vdp.code & 0x01) == 0)
        gase_vdp.read_buffer = read_target();
}

void gase_vdp_write_data(uint16_t value)
{
    gase_vdp.pending = false;
    if (gase_vdp.dma_fill_pending) {
        dma_vram_fill(value);
        return;
    }
    write_target(value);
    advance_addr();
}

uint16_t gase_vdp_read_data(void)
{
    uint16_t v = gase_vdp.read_buffer;
    gase_vdp.pending = false;
    advance_addr();
    gase_vdp.read_buffer = read_target();   /* prefetch the next word */
    return v;
}

/*
 * Status register:
 *   bit 9 FIFO empty (always: writes complete instantly)   bit 8 FIFO full
 *   bit 7 F  V-interrupt pending   bit 6 sprite overflow   bit 5 collision
 *   bit 4 odd frame (interlace)    bit 3 VBLANK  bit 2 HBLANK
 *   bit 1 DMA busy (never: DMA is instant)     bit 0 PAL (0 = NTSC)
 * The top 6 bits are not driven by the VDP; on hardware they read as the
 * next 68000 opcode word. 0x34xx is the value most often seen.
 * Reading status also aborts a half-written command, which is how games
 * "reset" the control port state machine.
 */
uint16_t gase_vdp_read_status(void)
{
    uint16_t s = 0x3400 | 0x0200;
    if (gase_vdp.vint_pending)              s |= 0x0080;
    if (gase_vdp.sprite_overflow)           s |= 0x0040;
    if (gase_vdp.sprite_collision)          s |= 0x0020;
    if (gase_vdp.odd_frame)                 s |= 0x0010;
    if (gase_vdp.in_vblank || !display_enabled())
                                            s |= 0x0008;   /* blanked = vblank */

    gase_vdp.pending = false;
    gase_vdp.sprite_overflow  = false;   /* both flags clear when read */
    gase_vdp.sprite_collision = false;
    return s;
}

/*
 * HV counter: high byte = V counter, low byte = H counter.
 *
 * V counter: the line number, but it is only 8 bits wide and NTSC has 262
 * lines, so in 224-line mode it counts 0x00..0xEA then jumps back to 0xE5
 * and continues to 0xFF (lines 0xEB.. read as line-6). In other modes we
 * simply report the low 8 bits.
 *
 * H counter: counts in units of 2 pixels and is non-linear too: in H40 it
 * goes 0x00..0xB6 then jumps to 0xE4..0xFF; in H32 0x00..0x93 then
 * 0xE9..0xFF. We map the 68000 cycle position (488 per line) linearly onto
 * that sequence.
 */
uint16_t gase_vdp_read_hv(int cycles_into_line)
{
    int line = gase_vdp.line;
    int v = line;
    if (gase_vdp_active_lines() == 224 && line > 0xEA)
        v = line - 6;
    v &= 0xFF;

    int first_end  = h40() ? 0xB6 : 0x93;  /* last value before the jump */
    int jump_start = h40() ? 0xE4 : 0xE9;
    int steps = (first_end + 1) + (0x100 - jump_start);
    if (cycles_into_line < 0)   cycles_into_line = 0;
    if (cycles_into_line > 487) cycles_into_line = 487;
    int n = cycles_into_line * steps / 488;
    int h = (n <= first_end) ? n : jump_start + (n - first_end - 1);

    return (uint16_t)((v << 8) | (h & 0xFF));
}

/* ------------------------------------------------------------------------ */
/* Interrupts                                                               */
/* ------------------------------------------------------------------------ */
/*
 * V-interrupt (level 6): raised at the first line of vertical blank.
 * H-interrupt (level 4): a down-counter loaded from register 10. It is
 * decremented on every active line; when it underflows the interrupt fires
 * and the counter is reloaded. During vertical blank it is reloaded on
 * every line, so each frame starts counting afresh.
 */
int gase_vdp_irq_level(void)
{
    if (gase_vdp.vint_pending && (REG(1) & 0x20)) return 6;
    if (gase_vdp.hint_pending && (REG(0) & 0x10)) return 4;
    return 0;
}

void gase_vdp_ack_irq(int level)
{
    if (level == 6)      gase_vdp.vint_pending = false;
    else if (level == 4) gase_vdp.hint_pending = false;
}

/* ------------------------------------------------------------------------ */
/* Rendering                                                                */
/* ------------------------------------------------------------------------ */
/*
 * Each layer is first drawn into a line buffer of bytes:
 *     bit 7      priority
 *     bits 5-0   CRAM index (palette * 16 + colour)
 * A pixel is transparent when its colour (bits 3-0) is 0.
 * The three buffers are then merged with the hardware priority order:
 *
 *   back  backdrop < B low < A low < sprite low
 *              < B high < A high < sprite high  front
 *
 * (plane A here also contains the window, which replaces A where active.)
 */
#define PRIO     0x80
#define OPAQUE(p) (((p) & 0x0F) != 0)

/* One pixel of a tile. Tiles are 8x8, 4 bits per pixel, rows of 4 bytes,
 * left pixel in the high nibble: 32 bytes per tile. */
static uint8_t tile_pixel(unsigned tile, unsigned px, unsigned py)
{
    uint16_t a = (uint16_t)(tile * 32 + py * 4 + (px >> 1));
    uint8_t  b = gase_vdp.vram[a];
    return (px & 1) ? (b & 0x0F) : (b >> 4);
}

/*
 * A name table entry (one 16-bit word per 8x8 cell) is:
 *   P CC V H TTTTTTTTTTT
 *   P priority, CC palette, V/H flips, T tile index (0..2047)
 * Returns the pixel at (x, y) inside the cell in the line-buffer format.
 */
static uint8_t cell_pixel(uint16_t entry, unsigned x, unsigned y)
{
    if (entry & 0x0800) x = 7 - x;          /* horizontal flip */
    if (entry & 0x1000) y = 7 - y;          /* vertical flip   */
    uint8_t c = tile_pixel(entry & 0x07FF, x, y);
    if (c == 0) return 0;
    return (uint8_t)(((entry >> 9) & 0x30) | c | ((entry & 0x8000) ? PRIO : 0));
}

/* Register 16 size code -> cells: 0=32, 1=64, 3=128 (2 is invalid). */
static unsigned plane_cells(unsigned code)
{
    static const unsigned cells[4] = { 32, 64, 32, 128 };
    return cells[code & 3];
}

/*
 * Horizontal scroll table (base = reg 13 << 10). Each entry is a pair of
 * words: plane A then plane B. Register 11 bits 1-0 pick the granularity:
 *   00 one value for the whole screen   01 (invalid) first 8 lines repeat
 *   10 one value per 8-line cell row    11 one value per line
 */
static uint16_t hscroll_for(int line, int plane_b)
{
    uint16_t base = (uint16_t)((REG(13) & 0x3F) << 10);
    unsigned row;
    switch (REG(11) & 3) {
    case 0:  row = 0;                      break;
    case 1:  row = (unsigned)line & 7;     break;
    case 2:  row = (unsigned)line & ~7u;   break;
    default: row = (unsigned)line;         break;
    }
    return vram_read_word((uint16_t)(base + row * 4 + (plane_b ? 2 : 0))) & 0x3FF;
}

/*
 * Vertical scroll comes from VSRAM: entries alternate plane A, plane B.
 * Register 11 bit 2 = 0: entries 0/1 scroll the whole screen;
 *                   = 1: each 16-pixel screen column has its own pair.
 */
static uint16_t vscroll_for(int x, int plane_b)
{
    unsigned i = (REG(11) & 0x04) ? (unsigned)(x >> 4) * 2 : 0;
    if (i > 38) i = 38;
    return gase_vdp.vsram[i + (plane_b ? 1 : 0)] & 0x3FF;
}

/* Draw a scrolling plane (A or B) into `out`. */
static void draw_plane(uint8_t *out, int line, int width, uint16_t nt_base,
                       int plane_b)
{
    unsigned w_cells = plane_cells(REG(16));
    unsigned h_cells = plane_cells(REG(16) >> 4);
    unsigned wmask = w_cells * 8 - 1, hmask = h_cells * 8 - 1;
    unsigned hs = hscroll_for(line, plane_b);

    for (int x = 0; x < width; x++) {
        /* Scrolling right by hs means screen x shows plane column x - hs. */
        unsigned px = ((unsigned)x - hs) & wmask;
        unsigned py = ((unsigned)line + vscroll_for(x, plane_b)) & hmask;
        uint16_t e  = vram_read_word((uint16_t)(nt_base +
                        ((py >> 3) * w_cells + (px >> 3)) * 2));
        out[x] = cell_pixel(e, px & 7, py & 7);
    }
}

/*
 * The window is a non-scrolling plane that replaces plane A in a region:
 *   reg 18: bit7 DOWN, bits 4-0 VP  -> lines >= VP*8 (DOWN) or < VP*8
 *   reg 17: bit7 RIGT, bits 4-0 HP  -> x >= HP*16 (RIGT) or x < HP*16
 * If the line is inside the vertical window range, the whole line is
 * window; otherwise only the horizontal range is. Its name table is 64
 * cells wide in H40 and 32 in H32.
 */
static void draw_window(uint8_t *out, int line, int width)
{
    unsigned vp = (unsigned)(REG(18) & 0x1F) * 8;
    unsigned hp = (unsigned)(REG(17) & 0x1F) * 16;
    bool whole_line = (REG(18) & 0x80) ? (unsigned)line >= vp
                                       : (unsigned)line <  vp;
    uint16_t base = (uint16_t)((REG(3) & (h40() ? 0x3C : 0x3E)) << 10);
    unsigned w_cells = h40() ? 64 : 32;

    for (int x = 0; x < width; x++) {
        bool in = whole_line || ((REG(17) & 0x80) ? (unsigned)x >= hp
                                                  : (unsigned)x <  hp);
        if (!in) continue;
        uint16_t e = vram_read_word((uint16_t)(base +
                        (((unsigned)line >> 3) * w_cells + ((unsigned)x >> 3)) * 2));
        out[x] = cell_pixel(e, (unsigned)x & 7, (unsigned)line & 7);
    }
}

/*
 * Sprites. The sprite attribute table (base = reg 5 << 9) holds 8-byte
 * entries:
 *   word 0: ------YY YYYYYYYY  Y position + 128
 *   word 1: ----HHVV -LLLLLLL  size (cells-1) H and V, link to next sprite
 *   word 2: P CC V H TTTTTTTTTTT  same format as a name table entry
 *   word 3: -------X XXXXXXXX  X position + 128
 * Sprites are not drawn in table order but by following the links from
 * entry 0 until a link of 0. A sprite's tiles are arranged column-major:
 * the tile at cell (cx, cy) is tile + cx * height + cy.
 *
 * Hardware limits (H40 / H32): 80 / 64 sprites in the table, 20 / 16
 * sprites and 320 / 256 sprite pixels per line. Excess sets the overflow
 * flag. Earlier sprites in the list are in front of later ones; two opaque
 * sprite pixels on the same spot set the collision flag.
 */
static void draw_sprites(uint8_t *out, int line, int width)
{
    unsigned max_total = h40() ? 80 : 64;
    unsigned max_line  = h40() ? 20 : 16;
    int      max_dots  = h40() ? 320 : 256;
    uint16_t base = (uint16_t)((REG(5) & (h40() ? 0x7E : 0x7F)) << 9);

    /* Phase 1: walk the linked list, collect sprites on this line. */
    uint16_t found[20];
    unsigned nfound = 0, idx = 0;
    for (unsigned n = 0; n < max_total; n++) {
        uint16_t e  = (uint16_t)(base + idx * 8);
        int y       = (int)(vram_read_word(e) & 0x1FF) - 128;
        uint16_t w1 = vram_read_word((uint16_t)(e + 2));
        int h       = (int)((w1 >> 8) & 3) + 1;

        if (line >= y && line < y + h * 8) {
            if (nfound == max_line) { gase_vdp.sprite_overflow = true; break; }
            found[nfound++] = e;
        }
        idx = w1 & 0x7F;
        if (idx == 0 || idx >= max_total) break;
    }

    /* Phase 2: draw them front to back; first opaque pixel wins. */
    int dots = 0;
    for (unsigned i = 0; i < nfound; i++) {
        uint16_t e    = found[i];
        int y         = (int)(vram_read_word(e) & 0x1FF) - 128;
        uint16_t w1   = vram_read_word((uint16_t)(e + 2));
        uint16_t attr = vram_read_word((uint16_t)(e + 4));
        int rawx      = vram_read_word((uint16_t)(e + 6)) & 0x1FF;
        int x0        = rawx - 128;
        unsigned wc   = ((w1 >> 10) & 3) + 1;
        unsigned hc   = ((w1 >> 8) & 3) + 1;

        /* Sprite masking (simplified): a sprite at X=0 hides all later
         * sprites on this line, provided it is not the first one. */
        if (rawx == 0 && i > 0) break;

        unsigned sy = (unsigned)(line - y);              /* row in sprite */
        if (attr & 0x1000) sy = hc * 8 - 1 - sy;         /* V flip        */

        for (unsigned sx = 0; sx < wc * 8; sx++) {
            if (dots++ >= max_dots) { gase_vdp.sprite_overflow = true; return; }
            int x = x0 + (int)sx;
            if (x < 0 || x >= width) continue;
            unsigned fx = (attr & 0x0800) ? wc * 8 - 1 - sx : sx;  /* H flip */
            unsigned tile = (attr & 0x07FF) + (fx >> 3) * hc + (sy >> 3);
            uint8_t c = tile_pixel(tile & 0x07FF, fx & 7, sy & 7);
            if (c == 0) continue;
            if (OPAQUE(out[x])) { gase_vdp.sprite_collision = true; continue; }
            out[x] = (uint8_t)(((attr >> 9) & 0x30) | c |
                               ((attr & 0x8000) ? PRIO : 0));
        }
    }
}

/*
 * TODO (not implemented): shadow/highlight mode (reg 12 bit 3), which
 * darkens or brightens pixels based on priority and special sprite colours,
 * and the interlace modes (reg 12 bits 2-1), which double vertical
 * resolution in mode 2 using 8x16 tiles.
 */
static void render_line(int line)
{
    int width = gase_vdp.width;
    uint32_t *row = &gase_vdp.framebuffer[line * GASE_FB_WIDTH];
    uint32_t backdrop = cram_to_rgb(gase_vdp.cram[REG(7) & 0x3F]);

    if (!display_enabled()) {        /* blanked screen shows the backdrop */
        for (int x = 0; x < width; x++) row[x] = backdrop;
        return;
    }

    uint8_t b[GASE_FB_WIDTH], a[GASE_FB_WIDTH], s[GASE_FB_WIDTH];
    memset(s, 0, sizeof s);
    draw_plane(b, line, width, (uint16_t)((REG(4) & 0x07) << 13), 1);
    draw_plane(a, line, width, (uint16_t)((REG(2) & 0x38) << 10), 0);
    draw_window(a, line, width);
    draw_sprites(s, line, width);

    for (int x = 0; x < width; x++) {
        /* Front-to-back candidate list; the first opaque one is shown. */
        const uint8_t order[6] = {
            (s[x] & PRIO) ? s[x] : 0, (a[x] & PRIO) ? a[x] : 0,
            (b[x] & PRIO) ? b[x] : 0, (s[x] & PRIO) ? 0 : s[x],
            (a[x] & PRIO) ? 0 : a[x], (b[x] & PRIO) ? 0 : b[x],
        };
        uint32_t rgb = backdrop;
        for (int i = 0; i < 6; i++) {
            if (OPAQUE(order[i])) {
                rgb = cram_to_rgb(gase_vdp.cram[order[i] & 0x3F]);
                break;
            }
        }
        row[x] = rgb;
    }

    /* Register 0 bit 5 blanks the leftmost 8 pixels (hides scroll seams). */
    if (REG(0) & 0x20)
        for (int x = 0; x < 8; x++) row[x] = backdrop;
}

/* ------------------------------------------------------------------------ */
/* Per-line scheduling                                                      */
/* ------------------------------------------------------------------------ */
/*
 * NTSC frame = 262 lines: 224 (or 240) active lines, then vertical blank.
 */
void gase_vdp_run_line(int line)
{
    int active = gase_vdp_active_lines();
    gase_vdp.line = line;

    if (line == 0) {
        gase_vdp.in_vblank = false;
        gase_vdp.odd_frame = !gase_vdp.odd_frame;
        /* The output size is latched once per frame. */
        gase_vdp.width  = h40() ? 320 : 256;
        gase_vdp.height = active;
    }

    if (line < active) {
        if (line < GASE_FB_HEIGHT)
            render_line(line);
        if (gase_vdp.hint_counter-- == 0) {
            gase_vdp.hint_pending = true;
            gase_vdp.hint_counter = REG(10);
        }
    } else {
        gase_vdp.hint_counter = REG(10);
    }

    if (line == active) {
        gase_vdp.in_vblank    = true;
        gase_vdp.vint_pending = true;
    }
}
