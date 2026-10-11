/*
 * platform_headless.c — run the game with no window, for tests and CI.
 *
 *   game_headless rom.bin [frames] [out.ppm] [ram.bin]
 *
 * Runs `frames` frames as fast as possible, then writes the last picture
 * as a PPM image and (optionally) dumps the 64 KiB of work RAM.
 * Set GASE_PAD=<hex button mask> to hold buttons down.
 */
#include <stdio.h>
#include <stdlib.h>

#include "gase/gase.h"
#include "gase/system.h"
#include "gase/vdp.h"

static long frames_left = 60;
static const char *ppm_path = "frame.ppm";
static const char *ram_path;

static void write_outputs(const uint32_t *pixels, int width, int height) {
    FILE *f = fopen(ppm_path, "wb");
    if (f) {
        fprintf(f, "P6\n%d %d\n255\n", width, height);
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++) {
                uint32_t p = pixels[y * GASE_FB_WIDTH + x];
                uint8_t rgb[3] = {(uint8_t)(p >> 16), (uint8_t)(p >> 8), (uint8_t)p};
                fwrite(rgb, 1, 3, f);
            }
        fclose(f);
    }
    if (ram_path && (f = fopen(ram_path, "wb"))) {
        fwrite(gase_ram, 1, sizeof gase_ram, f);
        fclose(f);
    }
}

bool gase_platform_frame(const uint32_t *pixels, int width, int height) {
    if (--frames_left > 0) return true;
    write_outputs(pixels, width, height);
    printf("[gase] done: %lld cycles, frame %dx%d written to %s\n",
           (long long)gase_cpu.cycles, width, height, ppm_path);
    return false;
}

void gase_platform_audio(const int16_t *samples, int count) {
    (void)samples; (void)count;
}

void gase_platform_fatal(const char *message) {
    fprintf(stderr, "[gase] fatal: %s\n", message);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s rom.bin [frames] [out.ppm] [ram.bin]\n", argv[0]);
        return 1;
    }
    if (argc > 2) frames_left = atol(argv[2]);
    if (argc > 3) ppm_path = argv[3];
    if (argc > 4) ram_path = argv[4];
    const char *pad = getenv("GASE_PAD");
    if (pad) gase_pad[0] = (uint16_t)strtoul(pad, NULL, 16);

    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 1; }
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *rom = malloc((size_t)size);
    if (!rom || fread(rom, 1, (size_t)size, f) != (size_t)size) { fclose(f); return 1; }
    fclose(f);
    if (!gase_system_init(rom, (uint32_t)size)) return 1;
    gase_run();
    return 0;
}
