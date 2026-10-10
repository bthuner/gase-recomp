package org.gaserecomp.@ID@;

import org.libsdl.app.SDLActivity;

/**
 * SDLActivity does all the work: it creates the surface, forwards input
 * and calls our C main() (renamed SDL_main by SDL.h) from libmain.so.
 * SDL2 is linked statically into libmain.so, so that's the only library.
 */
public class GameActivity extends SDLActivity {
    @Override
    protected String[] getLibraries() {
        return new String[] { "main" };
    }
}
