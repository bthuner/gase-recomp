/*
 * io.c — the I/O chip at $A10000: version register and controller ports.
 *
 * A 3-button pad has more buttons than wires, so it is multiplexed by the
 * TH line, which the game toggles through the data port:
 *
 *   TH = 1  ->  bits: 0 1 C B Right Left Down Up      (0 = pressed)
 *   TH = 0  ->  bits: 0 0 Start A 0 0 Down Up
 *
 * TODO (feature branch welcome): 6-button pads use a third TH pulse to
 * expose X, Y, Z and Mode.
 */
#include "gase/system.h"

uint16_t gase_pad[2];

static uint8_t data_latch[3];   /* value written by the 68000 per port */
static uint8_t ctrl[3];         /* 1 bits = pin is an output (driven by CPU) */

void gase_io_reset(void) {
    for (int i = 0; i < 3; i++) { data_latch[i] = 0x40; ctrl[i] = 0; }
}

static uint8_t pad_lines(int port) {
    if (port > 1) return 0x7F;                  /* nothing plugged in */
    uint16_t b = gase_pad[port];
    bool th = (data_latch[port] & 0x40) != 0;
    uint8_t v;
    if (th) {
        v = 0x40;
        if (!(b & GASE_BTN_UP))    v |= 0x01;
        if (!(b & GASE_BTN_DOWN))  v |= 0x02;
        if (!(b & GASE_BTN_LEFT))  v |= 0x04;
        if (!(b & GASE_BTN_RIGHT)) v |= 0x08;
        if (!(b & GASE_BTN_B))     v |= 0x10;
        if (!(b & GASE_BTN_C))     v |= 0x20;
    } else {
        v = 0x00;
        if (!(b & GASE_BTN_UP))    v |= 0x01;
        if (!(b & GASE_BTN_DOWN))  v |= 0x02;
        if (!(b & GASE_BTN_A))     v |= 0x10;
        if (!(b & GASE_BTN_START)) v |= 0x20;
    }
    return v;
}

uint8_t gase_io_read(uint32_t addr) {
    switch (addr & 0x1F) {
    case 0x01:
        /* Version: overseas (bit 7), NTSC (bit 6 = 0), no Mega-CD (bit 5). */
        return 0xA0;
    case 0x03: case 0x05: case 0x07: {
        int port = (int)((addr & 0x1F) - 3) / 2;
        uint8_t in = pad_lines(port);
        /* Output pins read back what the CPU wrote, inputs come from the pad. */
        return (uint8_t)((data_latch[port] & ctrl[port]) | (in & ~ctrl[port] & 0x7F));
    }
    case 0x09: case 0x0B: case 0x0D:
        return ctrl[((addr & 0x1F) - 9) / 2];
    default:
        return 0;
    }
}

void gase_io_write(uint32_t addr, uint8_t value) {
    switch (addr & 0x1F) {
    case 0x03: case 0x05: case 0x07:
        data_latch[((addr & 0x1F) - 3) / 2] = value;
        break;
    case 0x09: case 0x0B: case 0x0D:
        ctrl[((addr & 0x1F) - 9) / 2] = value;
        break;
    default:
        break;
    }
}
