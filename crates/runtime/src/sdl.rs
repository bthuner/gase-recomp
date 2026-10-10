//! A minimal SDL2 window for desktop play (feature `sdl`).
//!
//! Keys: arrows, Z/X/C = A/B/C, A/S/D = X/Y/Z, Enter = Start, Esc = quit.
//! The long-term plan is to run recompiled games inside gase's own
//! frontends (menus, save states, gamepads, touch, web); this window is the
//! spike's way to see and hear one.

use gase_core::Buttons;
use sdl2::audio::AudioSpecDesired;
use sdl2::event::Event;
use sdl2::keyboard::Scancode;
use sdl2::pixels::PixelFormatEnum;

use crate::game::{Game, GameInfo};

pub fn main(info: &GameInfo, args: &[String]) -> Result<(), String> {
    let rom_path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "rom.bin".into());
    let rom = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let mut game = Game::new(info, &rom, 48_000)?;

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    let window = video
        .window(info.title, 320 * 3, 224 * 3)
        .position_centered()
        .resizable()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|e| e.to_string())?;
    let creator = canvas.texture_creator();
    let mut texture = creator
        .create_texture_streaming(
            PixelFormatEnum::ARGB8888,
            gase_vdp::MAX_WIDTH as u32,
            gase_vdp::MAX_HEIGHT as u32,
        )
        .map_err(|e| e.to_string())?;
    let audio = sdl.audio()?;
    let queue = audio.open_queue::<i16, _>(
        None,
        &AudioSpecDesired {
            freq: Some(48_000),
            channels: Some(2),
            samples: Some(1024),
        },
    )?;
    queue.resume();
    let mut events = sdl.event_pump()?;
    let mut samples = Vec::new();

    'running: loop {
        for e in events.poll_iter() {
            if matches!(
                e,
                Event::Quit { .. }
                    | Event::KeyDown {
                        scancode: Some(Scancode::Escape),
                        ..
                    }
            ) {
                break 'running;
            }
        }
        let keys = events.keyboard_state();
        let mut b = Buttons(0);
        for (code, button) in [
            (Scancode::Up, Buttons::UP),
            (Scancode::Down, Buttons::DOWN),
            (Scancode::Left, Buttons::LEFT),
            (Scancode::Right, Buttons::RIGHT),
            (Scancode::Z, Buttons::A),
            (Scancode::X, Buttons::B),
            (Scancode::C, Buttons::C),
            (Scancode::A, Buttons::X),
            (Scancode::S, Buttons::Y),
            (Scancode::D, Buttons::Z),
            (Scancode::Return, Buttons::START),
        ] {
            b.set(button, keys.is_scancode_pressed(code));
        }
        game.set_buttons(0, b);
        game.run_frame();

        let (pixels, w, h, stride) = game.frame();
        texture
            .with_lock(None, |buf, pitch| {
                for y in 0..h {
                    for (x, &p) in pixels[y * stride..y * stride + w].iter().enumerate() {
                        buf[y * pitch + 4 * x..y * pitch + 4 * x + 4]
                            .copy_from_slice(&p.to_le_bytes());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        canvas.clear();
        canvas.copy(
            &texture,
            sdl2::rect::Rect::new(0, 0, w as u32, h as u32),
            None,
        )?;
        canvas.present();

        samples.clear();
        game.drain_audio(&mut samples);
        if queue.size() < 48_000 / 5 * 4 {
            queue.queue_audio(&samples)?;
        }
    }
    Ok(())
}
