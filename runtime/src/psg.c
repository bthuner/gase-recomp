/*
 * psg.c — Texas Instruments SN76489 PSG, the Sega variant built into the
 * Genesis VDP.
 *
 * The chip has 8 internal registers, written one byte at a time:
 *
 *   Latch byte : 1 CC T DDDD   CC = channel (0-2 tone, 3 noise)
 *                               T  = 0 tone/noise register, 1 attenuation
 *                               DDDD = low 4 bits of the data
 *   Data byte  : 0 - DDDDDD    upper 6 bits of a tone period (or the full
 *                               value for attenuation/noise) for the
 *                               register latched last
 *
 * Tone channels: a 10-bit counter is decremented at clock/16; when it hits
 * zero it reloads with the period and the square wave flips. So the output
 * frequency is clock / (32 * period).
 *
 * Noise channel: register = -- F RR. RR picks the shift rate (clock/16
 * divided by 16, 32, 64, or "same as tone 2" when RR = 3). F = 1 selects
 * white noise, 0 "periodic" noise (a 1/16 duty pulse). Writing the noise
 * register resets the 16-bit shift register.
 *
 * Attenuation: 4 bits, 2 dB per step, 15 = silent.
 */
#include "gase/psg.h"

#include <string.h>

#define PSG_CLOCK      3579545u        /* NTSC master clock / 15 */
#define PSG_TICK_RATE  (PSG_CLOCK / 16) /* counters advance at this rate */

/* Amplitude per attenuation step: 4000 * 10^(-2dB*n/20). The loudest
 * possible mix (4 channels at +4000) stays well inside int16. */
static const int16_t volume_table[16] = {
    4000, 3177, 2524, 2005, 1592, 1265, 1005, 798,
     634,  504,  400,  318,  252,  200,  159,    0
};

static struct {
    uint16_t period[3];    /* tone periods (10 bits)                     */
    uint8_t  noise_ctrl;   /* noise register (3 bits)                    */
    uint8_t  atten[4];     /* attenuation per channel (4 bits)           */
    uint8_t  latched;      /* last latched register: channel*2 + type    */

    uint16_t counter[4];   /* down-counters                              */
    uint8_t  output[4];    /* current square wave output (0/1)           */
    uint16_t lfsr;         /* noise shift register                       */

    uint32_t step;         /* PSG ticks per output sample, 16.16 fixed   */
    uint32_t frac;         /* fractional tick accumulator, 16.16         */
} psg;

void gase_psg_reset(int sample_rate)
{
    memset(&psg, 0, sizeof psg);
    for (int i = 0; i < 4; i++) psg.atten[i] = 15;   /* silent at power on */
    psg.lfsr = 0x8000;
    if (sample_rate <= 0) sample_rate = 44100;
    psg.step = (uint32_t)(((uint64_t)PSG_TICK_RATE << 16) / (uint32_t)sample_rate);
}

void gase_psg_write(uint8_t value)
{
    if (value & 0x80) {                         /* latch byte */
        psg.latched = (value >> 4) & 7;
        unsigned ch = psg.latched >> 1;
        if (psg.latched & 1) {
            psg.atten[ch] = value & 0x0F;
        } else if (ch < 3) {
            psg.period[ch] = (uint16_t)((psg.period[ch] & 0x3F0) | (value & 0x0F));
        } else {
            psg.noise_ctrl = value & 7;
            psg.lfsr = 0x8000;
        }
    } else {                                    /* data byte */
        unsigned ch = psg.latched >> 1;
        if (psg.latched & 1) {
            psg.atten[ch] = value & 0x0F;
        } else if (ch < 3) {
            psg.period[ch] = (uint16_t)((psg.period[ch] & 0x00F) |
                                        ((value & 0x3F) << 4));
        } else {
            psg.noise_ctrl = value & 7;
            psg.lfsr = 0x8000;
        }
    }
}

/* Shift the noise LFSR once. The Sega variant is 16 bits wide and, for
 * white noise, feeds back the XOR of bits 0 and 3 (tap mask 0x0009). */
static void noise_shift(void)
{
    unsigned fb;
    if (psg.noise_ctrl & 4) {
        unsigned t = psg.lfsr & 0x0009;
        fb = (t ^ (t >> 3)) & 1;
    } else {
        fb = psg.lfsr & 1;                      /* periodic: just rotate */
    }
    psg.lfsr = (uint16_t)((psg.lfsr >> 1) | (fb << 15));
}

/* Advance the chip by one tick (clock / 16) and return the mixed level. */
static int tick(void)
{
    int mix = 0;

    for (int ch = 0; ch < 3; ch++) {
        uint16_t p = psg.period[ch];
        if (p <= 1) {
            /* Period 0 or 1 holds the output high: games use this with
             * the volume register to play sampled sound. */
            psg.output[ch] = 1;
        } else if (--psg.counter[ch] == 0 || psg.counter[ch] > p) {
            psg.counter[ch] = p;
            psg.output[ch] ^= 1;
        }
        int v = volume_table[psg.atten[ch]];
        mix += psg.output[ch] ? v : -v;
    }

    /* Noise: the counter flips a divider, and the LFSR shifts on each
     * rising edge of it (so effectively at half the counter rate). */
    unsigned rate = psg.noise_ctrl & 3;
    uint16_t np = rate == 3 ? psg.period[2] : (uint16_t)(0x10 << rate);
    if (np == 0) np = 1;
    if (--psg.counter[3] == 0 || psg.counter[3] > np) {
        psg.counter[3] = np;
        psg.output[3] ^= 1;
        if (psg.output[3]) noise_shift();
    }
    int nv = volume_table[psg.atten[3]];
    mix += (psg.lfsr & 1) ? nv : -nv;

    return mix;
}

/*
 * The chip runs at ~223.7 kHz; the host wants e.g. 44.1 kHz. For each
 * output sample we run the number of chip ticks it spans (tracked in 16.16
 * fixed point so no time is lost) and average them: a cheap box filter
 * that keeps very high tones from aliasing badly.
 */
void gase_psg_render(int16_t *out, int count)
{
    for (int i = 0; i < count; i++) {
        psg.frac += psg.step;
        unsigned ticks = psg.frac >> 16;
        psg.frac &= 0xFFFF;

        int32_t sum = 0;
        for (unsigned t = 0; t < ticks; t++)
            sum += tick();
        out[i] = (int16_t)(ticks ? sum / (int32_t)ticks : 0);
    }
}
