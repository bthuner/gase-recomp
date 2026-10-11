/*
 * psg.h — Texas Instruments SN76489 Programmable Sound Generator.
 *
 * 3 square-wave tone channels + 1 noise channel, written by the 68000
 * (or the Z80) through the byte port at $C00011.
 */
#ifndef GASE_PSG_H
#define GASE_PSG_H

#include <stdint.h>

void gase_psg_reset(int sample_rate);
void gase_psg_write(uint8_t value);
/* Produce `count` signed 16-bit mono samples into `out`. */
void gase_psg_render(int16_t *out, int count);

#endif
