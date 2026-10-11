/*
 * platform_sdl.c — the "shell" around the recompiled game, built on SDL2.
 * The same file serves desktop (Linux, macOS, Windows), Android and iOS:
 * SDL hides window creation, input, audio and the app life cycle.
 *
 * Controls
 *   keyboard   arrows = D-pad, Z/X/C = A/B/C, Enter = Start, A/S/D = X/Y/Z
 *   gamepad    any controller SDL knows (Xbox / PlayStation layouts)
 *   touch      on-screen D-pad and buttons, drawn over the game
 */
#include <SDL.h>
#include <stdio.h>
#include <stdlib.h>

#include "gase/gase.h"
#include "gase/system.h"
#include "gase/vdp.h"

static SDL_Window *window;
static SDL_Renderer *renderer;
static SDL_Texture *texture;
static SDL_AudioDeviceID audio_dev;
static SDL_GameController *pad;
static Uint64 next_frame_time;
static bool touch_ui;

/* ---- on-screen controls (touch) ---------------------------------------- */

typedef struct { float x, y, w, h; uint16_t buttons; } TouchZone;

/* Normalised to the window (0..1). Diagonals come for free: the D-pad
 * zones overlap at the corners. */
static const TouchZone zones[] = {
    {0.02f, 0.45f, 0.10f, 0.18f, GASE_BTN_LEFT},
    {0.16f, 0.45f, 0.10f, 0.18f, GASE_BTN_RIGHT},
    {0.08f, 0.30f, 0.12f, 0.17f, GASE_BTN_UP},
    {0.08f, 0.62f, 0.12f, 0.17f, GASE_BTN_DOWN},
    {0.70f, 0.58f, 0.09f, 0.16f, GASE_BTN_A},
    {0.80f, 0.50f, 0.09f, 0.16f, GASE_BTN_B},
    {0.90f, 0.42f, 0.09f, 0.16f, GASE_BTN_C},
    {0.44f, 0.86f, 0.12f, 0.10f, GASE_BTN_START},
};

static uint16_t read_touch(int win_w, int win_h) {
    uint16_t b = 0;
    for (int d = 0; d < SDL_GetNumTouchDevices(); d++) {
        SDL_TouchID id = SDL_GetTouchDevice(d);
        for (int f = 0; f < SDL_GetNumTouchFingers(id); f++) {
            SDL_Finger *finger = SDL_GetTouchFinger(id, f);
            if (!finger) continue;
            touch_ui = true;
            for (size_t z = 0; z < SDL_arraysize(zones); z++) {
                const TouchZone *t = &zones[z];
                if (finger->x >= t->x && finger->x <= t->x + t->w &&
                    finger->y >= t->y && finger->y <= t->y + t->h)
                    b |= t->buttons;
            }
        }
    }
    (void)win_w; (void)win_h;
    return b;
}

static void draw_touch_ui(int win_w, int win_h, uint16_t pressed) {
    SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
    for (size_t z = 0; z < SDL_arraysize(zones); z++) {
        const TouchZone *t = &zones[z];
        SDL_Rect r = {(int)(t->x * win_w), (int)(t->y * win_h),
                      (int)(t->w * win_w), (int)(t->h * win_h)};
        Uint8 alpha = (pressed & t->buttons) ? 140 : 60;
        SDL_SetRenderDrawColor(renderer, 255, 255, 255, alpha);
        SDL_RenderFillRect(renderer, &r);
    }
}

/* ---- input --------------------------------------------------------------- */

static uint16_t read_keyboard(void) {
    const Uint8 *k = SDL_GetKeyboardState(NULL);
    uint16_t b = 0;
    if (k[SDL_SCANCODE_UP])     b |= GASE_BTN_UP;
    if (k[SDL_SCANCODE_DOWN])   b |= GASE_BTN_DOWN;
    if (k[SDL_SCANCODE_LEFT])   b |= GASE_BTN_LEFT;
    if (k[SDL_SCANCODE_RIGHT])  b |= GASE_BTN_RIGHT;
    if (k[SDL_SCANCODE_Z])      b |= GASE_BTN_A;
    if (k[SDL_SCANCODE_X])      b |= GASE_BTN_B;
    if (k[SDL_SCANCODE_C])      b |= GASE_BTN_C;
    if (k[SDL_SCANCODE_RETURN]) b |= GASE_BTN_START;
    if (k[SDL_SCANCODE_A])      b |= GASE_BTN_X;
    if (k[SDL_SCANCODE_S])      b |= GASE_BTN_Y;
    if (k[SDL_SCANCODE_D])      b |= GASE_BTN_Z;
    return b;
}

static uint16_t read_gamepad(void) {
    if (!pad) return 0;
    uint16_t b = 0;
    Sint16 lx = SDL_GameControllerGetAxis(pad, SDL_CONTROLLER_AXIS_LEFTX);
    Sint16 ly = SDL_GameControllerGetAxis(pad, SDL_CONTROLLER_AXIS_LEFTY);
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_DPAD_UP) || ly < -16000)    b |= GASE_BTN_UP;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_DPAD_DOWN) || ly > 16000)   b |= GASE_BTN_DOWN;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_DPAD_LEFT) || lx < -16000)  b |= GASE_BTN_LEFT;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_DPAD_RIGHT) || lx > 16000)  b |= GASE_BTN_RIGHT;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_X)) b |= GASE_BTN_A;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_A)) b |= GASE_BTN_B;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_B)) b |= GASE_BTN_C;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_START)) b |= GASE_BTN_START;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_LEFTSHOULDER)) b |= GASE_BTN_X;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_Y)) b |= GASE_BTN_Y;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_RIGHTSHOULDER)) b |= GASE_BTN_Z;
    if (SDL_GameControllerGetButton(pad, SDL_CONTROLLER_BUTTON_BACK)) b |= GASE_BTN_MODE;
    return b;
}

/* ---- the platform interface ----------------------------------------------- */

bool gase_platform_frame(const uint32_t *pixels, int width, int height) {
    SDL_Event e;
    while (SDL_PollEvent(&e)) {
        switch (e.type) {
        case SDL_QUIT:
            return false;
        case SDL_KEYDOWN:
            if (e.key.keysym.scancode == SDL_SCANCODE_ESCAPE) return false;
            break;
        case SDL_CONTROLLERDEVICEADDED:
            if (!pad) pad = SDL_GameControllerOpen(e.cdevice.which);
            break;
        case SDL_CONTROLLERDEVICEREMOVED:
            if (pad && e.cdevice.which == SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(pad))) {
                SDL_GameControllerClose(pad);
                pad = NULL;
            }
            break;
        default:
            break;
        }
    }
    int win_w, win_h;
    SDL_GetRendererOutputSize(renderer, &win_w, &win_h);
    uint16_t touch = read_touch(win_w, win_h);
    gase_pad[0] = read_keyboard() | read_gamepad() | touch;

    /* Upload the frame and scale it to the window, keeping a 4:3 aspect. */
    SDL_Rect src = {0, 0, width, height};
    SDL_UpdateTexture(texture, NULL, pixels, GASE_FB_WIDTH * 4);
    float scale_w = (float)win_w / (4.0f / 3.0f * height), scale_h = (float)win_h / height;
    float scale = scale_w < scale_h ? scale_w : scale_h;
    int dst_w = (int)(4.0f / 3.0f * height * scale), dst_h = (int)(height * scale);
    SDL_Rect dst = {(win_w - dst_w) / 2, (win_h - dst_h) / 2, dst_w, dst_h};
    SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
    SDL_RenderClear(renderer);
    SDL_RenderCopy(renderer, texture, &src, &dst);
    if (touch_ui) draw_touch_ui(win_w, win_h, touch);
    SDL_RenderPresent(renderer);

    /* Pace to 60 frames per second. */
    Uint64 freq = SDL_GetPerformanceFrequency();
    Uint64 now = SDL_GetPerformanceCounter();
    if (next_frame_time == 0 || now > next_frame_time + freq / 10) next_frame_time = now;
    next_frame_time += freq / 60;
    while ((now = SDL_GetPerformanceCounter()) < next_frame_time) {
        Uint64 left_ms = (next_frame_time - now) * 1000 / freq;
        if (left_ms > 1) SDL_Delay((Uint32)(left_ms - 1));
    }
    return true;
}

void gase_platform_audio(const int16_t *samples, int count) {
    if (!audio_dev) return;
    /* Don't let latency grow if we run slower than real time. */
    if (SDL_GetQueuedAudioSize(audio_dev) > (Uint32)(GASE_AUDIO_RATE / 10 * 2)) return;
    SDL_QueueAudio(audio_dev, samples, (Uint32)count * 2);
}

void gase_platform_fatal(const char *message) {
    SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "gase-recomp", message, window);
}

/* ---- boot -------------------------------------------------------------------- */

static uint8_t *load_rom(const char *path, uint32_t *size) {
    /* On Android SDL_RWFromFile also reads from the APK's assets folder. */
    SDL_RWops *rw = SDL_RWFromFile(path, "rb");
    if (!rw) return NULL;
    Sint64 n = SDL_RWsize(rw);
    uint8_t *data = n > 0 ? malloc((size_t)n) : NULL;
    if (data && SDL_RWread(rw, data, 1, (size_t)n) != (size_t)n) { free(data); data = NULL; }
    SDL_RWclose(rw);
    *size = (uint32_t)n;
    return data;
}

int main(int argc, char **argv) {
    if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO | SDL_INIT_GAMECONTROLLER) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return 1;
    }
    uint32_t size = 0;
    uint8_t *rom = NULL;
    if (argc > 1) rom = load_rom(argv[1], &size);
    if (!rom) rom = load_rom("rom.bin", &size);               /* cwd / Android assets */
    if (!rom) {
        char *base = SDL_GetBasePath();                       /* next to the exe / iOS bundle */
        if (base) {
            char path[1024];
            SDL_snprintf(path, sizeof path, "%srom.bin", base);
            rom = load_rom(path, &size);
            SDL_free(base);
        }
    }
    if (!rom) {
        SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "gase-recomp",
                                 "rom.bin not found. Put your ROM next to the executable.", NULL);
        return 1;
    }
    if (!gase_system_init(rom, size)) {
        SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "gase-recomp",
                                 "This ROM does not match the recompiled code.", NULL);
        return 1;
    }

    SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "0");          /* crisp pixels */
    SDL_SetHint(SDL_HINT_ORIENTATIONS, "LandscapeLeft LandscapeRight");
    window = SDL_CreateWindow(gase_game_title, SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
                              320 * 3, 224 * 3,
                              SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI);
    renderer = SDL_CreateRenderer(window, -1, 0);   /* best available, software as fallback */
    texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_ARGB8888, SDL_TEXTUREACCESS_STREAMING,
                                GASE_FB_WIDTH, GASE_FB_HEIGHT);
    if (!window || !renderer || !texture) {
        fprintf(stderr, "SDL video: %s\n", SDL_GetError());
        return 1;
    }
    SDL_AudioSpec want = {0}, have;
    want.freq = GASE_AUDIO_RATE;
    want.format = AUDIO_S16SYS;
    want.channels = 1;
    want.samples = 1024;
    audio_dev = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
    if (audio_dev) SDL_PauseAudioDevice(audio_dev, 0);
#if defined(__ANDROID__) || defined(__IPHONEOS__)
    touch_ui = true;
#endif

    gase_run();                                               /* never returns */
    return 0;
}
