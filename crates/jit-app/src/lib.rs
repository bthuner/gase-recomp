//! gase, with gase-recomp's JIT as the 68000 of every game.
//!
//! A statically recompiled game registers its code for *its* ROM. The JIT
//! needs nothing in advance, so it registers for *any* ROM
//! (`external::register_any`): every game opened in gase then runs on
//! machine code written while it plays. Everything else (menus, save
//! states, rewind, touch controls) is gase's, as always.
//!
//! * desktop: the `gase-jit` program (`src/main.rs`);
//! * Android: this library's `SDL_main`, built into an app by gase's own
//!   Android project (`-PgaseCrate=…/crates/jit-app/Cargo.toml:gase-recomp-jit-app`).
//!
//! Not iOS: Apple does not let apps write machine code, so gase stays with
//! its interpreter there.

use gase_core::system::external;
use gase_recomp_jit::Jit;

/// Use the JIT for every ROM opened from now on.
pub fn register() {
    external::register_any(Some(|| Box::new(Jit::new())));
}

/// The Android app's entry point, called by SDL (see gase's
/// mobile/README.md): register the JIT, then run gase's phone app as usual.
///
/// # Safety
///
/// SDL passes C's `argc` and `argv`.
#[cfg(target_os = "android")]
#[allow(unsafe_code, non_snake_case)] // the name is SDL's
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_main(
    argc: std::ffi::c_int,
    argv: *mut *mut std::ffi::c_char,
) -> std::ffi::c_int {
    register();
    // SAFETY: as promised by our caller.
    let args = unsafe { gase_mobile::args(argc, argv) };
    gase_mobile::run(&args)
}
