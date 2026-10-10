/*
 * bus.h — the 68000 memory map of the Genesis / Mega Drive.
 *
 *   $000000-$3FFFFF  cartridge ROM (up to 4 MiB)
 *   $A00000-$A0FFFF  Z80 address space (8 KiB sound RAM, YM2612, ...)
 *   $A10000-$A1001F  I/O: version register, controller ports
 *   $A11100          Z80 bus request      $A11200  Z80 reset
 *   $A14000          TMSS lock            $C00000-$C0001F  VDP + PSG
 *   $FF0000-$FFFFFF  64 KiB of work RAM (mirrored from $E00000)
 *
 * Every memory access performed by recompiled code goes through these
 * functions. ROM and RAM have inlined fast paths (see gase.h); anything
 * else lands in the slow "I/O" path in bus.c.
 */
#ifndef GASE_BUS_H
#define GASE_BUS_H

#include <stdint.h>
#include <stddef.h>

extern uint8_t *gase_rom;        /* ROM bytes, big-endian as on cartridge */
extern uint32_t gase_rom_size;
extern uint8_t  gase_ram[0x10000];
extern uint8_t  gase_z80_ram[0x2000];

void     gase_bus_reset(void);

uint8_t  gase_bus_read8(uint32_t addr);
uint16_t gase_bus_read16(uint32_t addr);
uint32_t gase_bus_read32(uint32_t addr);
void     gase_bus_write8(uint32_t addr, uint8_t value);
void     gase_bus_write16(uint32_t addr, uint16_t value);
void     gase_bus_write32(uint32_t addr, uint32_t value);

#endif
