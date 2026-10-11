/*
 * bus.c — the slow path of the 68000 memory map: everything that is not
 * plain ROM or work RAM (those are inlined in gase.h).
 */
#include "gase/gase.h"
#include "gase/psg.h"
#include "gase/system.h"
#include "gase/vdp.h"

uint8_t *gase_rom;
uint32_t gase_rom_size;
uint8_t  gase_ram[0x10000];
uint8_t  gase_z80_ram[0x2000];

/* The Z80 is the sound CPU. We don't run it yet (see the roadmap), but the
 * 68000 still talks to it: it uploads the sound driver into Z80 RAM and
 * "requests the bus" before touching it. We answer like real hardware so
 * that games don't wait forever. */
static bool z80_busreq;
static bool z80_reset = true;

void gase_bus_reset(void) {
    for (size_t i = 0; i < sizeof gase_ram; i++) gase_ram[i] = 0;
    for (size_t i = 0; i < sizeof gase_z80_ram; i++) gase_z80_ram[i] = 0;
    z80_busreq = false;
    z80_reset = true;
}

uint8_t gase_bus_read8(uint32_t addr) {
    addr &= 0xFFFFFF;
    if (addr < 0x400000)                          /* ROM (or unmapped part) */
        return addr < gase_rom_size ? gase_rom[addr] : 0xFF;
    if (addr >= 0xE00000)
        return gase_ram[addr & 0xFFFF];
    if (addr >= 0xA00000 && addr < 0xA10000) {   /* Z80 space */
        if ((addr & 0x7FFF) < 0x4000) return gase_z80_ram[addr & 0x1FFF];
        return 0;                                 /* YM2612 status: never busy */
    }
    if (addr >= 0xA10000 && addr < 0xA10020)
        return gase_io_read(addr);
    if ((addr & ~1u) == 0xA11100)                 /* bit 0 = 0: bus granted */
        return (addr & 1) ? 0 : (z80_busreq && !z80_reset ? 0x00 : 0x01);
    if (addr >= 0xC00000 && addr < 0xC00020) {
        uint16_t w = gase_bus_read16(addr & ~1u);
        return (addr & 1) ? (uint8_t)w : (uint8_t)(w >> 8);
    }
    return 0;
}

uint16_t gase_bus_read16(uint32_t addr) {
    addr &= 0xFFFFFE;
    if (addr >= 0xC00000 && addr < 0xC00020) {
        switch (addr & 0x1F) {
        case 0x00: case 0x02: return gase_vdp_read_data();
        case 0x04: case 0x06: return gase_vdp_read_status();
        case 0x08: case 0x0A: case 0x0C: case 0x0E:
            return gase_vdp_read_hv((int)(GASE_CYCLES_PER_LINE -
                                          (gase_cpu.next_event - gase_cpu.cycles)));
        default: return 0;
        }
    }
    if (addr >= 0xA00000 && addr < 0xA10000) {   /* Z80 space is 8-bit wide */
        uint8_t b = gase_bus_read8(addr);
        return (uint16_t)(b << 8 | b);
    }
    return (uint16_t)(gase_bus_read8(addr) << 8 | gase_bus_read8(addr + 1));
}

uint32_t gase_bus_read32(uint32_t addr) {
    return (uint32_t)gase_bus_read16(addr) << 16 | gase_bus_read16(addr + 2);
}

void gase_bus_write8(uint32_t addr, uint8_t value) {
    addr &= 0xFFFFFF;
    if (addr >= 0xE00000) { gase_ram[addr & 0xFFFF] = value; return; }
    if (addr >= 0xA00000 && addr < 0xA10000) {
        if ((addr & 0x7FFF) < 0x4000) gase_z80_ram[addr & 0x1FFF] = value;
        return;                                   /* YM2612 writes: TODO */
    }
    if (addr >= 0xA10000 && addr < 0xA10020) { gase_io_write(addr, value); return; }
    if (addr == 0xA11100) { z80_busreq = value & 1; return; }
    if (addr == 0xA11200) { z80_reset = !(value & 1); return; }
    if (addr >= 0xC00000 && addr < 0xC00020) {
        if ((addr & 0x19) == 0x11) { gase_psg_write(value); return; }
        /* A byte write to the VDP puts the same byte on both halves. */
        gase_bus_write16(addr & ~1u, (uint16_t)(value << 8 | value));
        return;
    }
    /* ROM, TMSS ($A14000), SRAM control ($A130F1)...: ignored for now. */
}

void gase_bus_write16(uint32_t addr, uint16_t value) {
    addr &= 0xFFFFFE;
    if (addr >= 0xC00000 && addr < 0xC00020) {
        switch (addr & 0x1F) {
        case 0x00: case 0x02: gase_vdp_write_data(value); return;
        case 0x04: case 0x06: gase_vdp_write_ctrl(value); return;
        case 0x10: case 0x12: case 0x14: case 0x16:
            gase_psg_write((uint8_t)value); return;
        default: return;
        }
    }
    if (addr >= 0xE00000) {
        gase_ram[addr & 0xFFFF] = (uint8_t)(value >> 8);
        gase_ram[(addr + 1) & 0xFFFF] = (uint8_t)value;
        return;
    }
    if ((addr >= 0xA00000 && addr < 0xA10000) || addr == 0xA11100 || addr == 0xA11200) {
        gase_bus_write8(addr, (uint8_t)(value >> 8));   /* 8-bit devices: high byte */
        return;
    }
    gase_bus_write8(addr, (uint8_t)(value >> 8));
    gase_bus_write8(addr + 1, (uint8_t)value);
}

void gase_bus_write32(uint32_t addr, uint32_t value) {
    gase_bus_write16(addr, (uint16_t)(value >> 16));
    gase_bus_write16(addr + 2, (uint16_t)value);
}
