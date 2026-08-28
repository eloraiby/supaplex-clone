//! SDL2 entry point for selecting and playing one original Supaplex level.

use std::{process::ExitCode, time::Duration};

use sdl2::{event::Event, keyboard::Scancode};
use supaplex_clone::{
    actor::Direction,
    assets,
    audio::AudioPlayer,
    cli::Options,
    game::{Game, GameStatus, Input},
    level::{Level, LevelSet},
    render::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Renderer},
};

/// Parses the command line, loads one record, and runs its SDL2 session.
fn main() -> ExitCode {
    // Parse only the arguments after the executable name. `Options` owns all
    // user-facing syntax validation so the eventual SDL front end stays small.
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{}", Options::usage());
            return ExitCode::FAILURE;
        }
    };

    // Acquire the level collection before initializing platform resources. A
    // default build borrows embedded bytes, while `unbundle` reports a missing
    // external file through the same startup-error path.
    let level_bytes = match assets::load_levels() {
        Ok(level_bytes) => level_bytes,
        Err(error) => {
            eprintln!("could not load level data: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Decode the chosen record only after acquisition succeeds so corrupt data
    // remains distinguishable from a missing unbundled asset.
    let level = match LevelSet::new(level_bytes.as_ref()).load(options.level_number()) {
        Ok(level) => level,
        Err(error) => {
            eprintln!("could not load level {}: {error}", options.level_number());
            return ExitCode::FAILURE;
        }
    };

    match run(&level, options.level_number()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not run level {}: {error}", options.level_number());
            ExitCode::FAILURE
        }
    }
}

/// Initializes SDL2 and owns every resource for one windowed play loop.
fn run(level: &Level, level_number: usize) -> Result<(), String> {
    // Nearest-neighbor scaling preserves the hard pixel edges of the original
    // 16×16 artwork after its 2× atlas repack and logical-window scaling.
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");
    let sdl = sdl2::init().map_err(|error| format!("initialize SDL2: {error}"))?;
    let video = sdl
        .video()
        .map_err(|error| format!("initialize SDL2 video: {error}"))?;
    let window = video
        .window("Supaplex", LOGICAL_WIDTH, LOGICAL_HEIGHT)
        .position_centered()
        .resizable()
        .build()
        .map_err(|error| format!("create SDL2 window: {error}"))?;
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|error| format!("create SDL2 canvas: {error}"))?;
    canvas
        .set_logical_size(LOGICAL_WIDTH, LOGICAL_HEIGHT)
        .map_err(|error| format!("set logical render size: {error}"))?;

    // The creator outlives `Renderer`, satisfying SDL texture lifetime rules
    // without leaking either the canvas or an atlas texture.
    let texture_creator = canvas.texture_creator();
    let mut renderer = Renderer::new(&texture_creator).map_err(|error| error.to_string())?;
    let mut event_pump = sdl
        .event_pump()
        .map_err(|error| format!("create SDL2 event pump: {error}"))?;
    let mut game = Game::new(level).map_err(|error| error.to_string())?;
    let mut audio = match sdl
        .audio()
        .map_err(|error| format!("initialize SDL2 audio: {error}"))
        .and_then(|subsystem| AudioPlayer::new(&subsystem))
    {
        Ok(audio) => Some(audio),
        Err(error) => {
            // Missing devices are common over remote sessions. Gameplay stays
            // available, while stderr still explains why audio was disabled.
            eprintln!("audio disabled: {error}");
            None
        }
    };

    // The DOS game and the SpeedFix reference timing both advance gameplay at
    // thirty-five iterations per second.  Keep this as an integer nanosecond
    // duration so the fixed-step accumulator loses less than one nanosecond per
    // iteration instead of rounding every update to 28 or 29 milliseconds.
    // Rendering remains independently capped at sixty frames per second below.
    const STEP: Duration = Duration::from_nanos(1_000_000_000 / 35);
    const MAX_STEPS_PER_FRAME: usize = 6;
    // Vsync is only a request and is ignored by some SDL render backends. An
    // independent deadline prevents those backends from rendering hundreds of
    // redundant frames per second and consuming an entire CPU core.
    const RENDER_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
    let mut previous = std::time::Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut window_title = String::new();

    'running: loop {
        // The frame start serves both the fixed-step accumulator and the
        // renderer's fallback deadline, keeping their clocks consistent.
        let frame_started = std::time::Instant::now();
        // Capping a long pause prevents a debugger stop or window drag from
        // causing an unbounded burst of catch-up simulation.
        accumulator += frame_started
            .duration_since(previous)
            .min(Duration::from_millis(250));
        previous = frame_started;

        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    scancode: Some(Scancode::Escape),
                    ..
                } => break 'running,
                Event::KeyDown {
                    scancode: Some(Scancode::R),
                    repeat: false,
                    ..
                } => {
                    // Restarting reconstructs actors and level toggles while
                    // retaining the process RNG stream, as original play does.
                    game.restart(level).map_err(|error| error.to_string())?;
                    if let Some(audio) = audio.as_mut() {
                        audio.restart_level();
                    }
                    accumulator = Duration::ZERO;
                }
                Event::KeyDown {
                    scancode: Some(Scancode::M),
                    repeat: false,
                    ..
                } => {
                    // Music is an independent voice, so muting it never drops a
                    // currently protected gameplay effect.
                    if let Some(audio) = audio.as_mut() {
                        let enabled = audio.toggle_music();
                        eprintln!("music {}", if enabled { "enabled" } else { "muted" });
                    }
                }
                Event::KeyDown {
                    scancode: Some(Scancode::S),
                    repeat: false,
                    ..
                } => {
                    // Toggling effects leaves the independent music voice live.
                    if let Some(audio) = audio.as_mut() {
                        let enabled = audio.toggle_effects();
                        eprintln!(
                            "sound effects {}",
                            if enabled { "enabled" } else { "muted" }
                        );
                    }
                }
                _ => {}
            }
        }

        let mut processed_steps = 0;
        while accumulator >= STEP && processed_steps < MAX_STEPS_PER_FRAME {
            let keyboard = event_pump.keyboard_state();
            let input = Input {
                direction: keyboard_direction(&keyboard),
                action: keyboard.is_scancode_pressed(Scancode::Space),
            };
            game.tick(input);
            if let Some(audio) = audio.as_mut() {
                // Drain after every simulation step so catch-up frames retain
                // actor event order before the original priority gate runs.
                for effect in game.take_sound_effects() {
                    audio.play(effect);
                }
            } else {
                // A headless session must still discard requests instead of
                // letting an unused queue grow for the lifetime of the level.
                game.take_sound_effects();
            }
            accumulator -= STEP;
            processed_steps += 1;
        }
        if processed_steps == MAX_STEPS_PER_FRAME {
            // Discard excessive lag rather than letting delayed input replay for
            // seconds after the application becomes responsive again.
            accumulator = Duration::ZERO;
        }

        let outcome = match game.status() {
            GameStatus::Playing => "PLAYING",
            GameStatus::Completed => "COMPLETE",
            GameStatus::Dead => "DESTROYED",
        };
        let title = format!(
            "Supaplex - Level {level_number:03}: {} - {} Infotrons - {outcome}",
            game.title(),
            game.remaining_infotrons()
        );
        // Updating native window chrome can be surprisingly expensive on some
        // compositors, so only cross that platform boundary when data changes.
        if title != window_title {
            canvas
                .window_mut()
                .set_title(&title)
                .map_err(|error| format!("update window title: {error}"))?;
            window_title = title;
        }
        renderer
            .draw(&mut canvas, &game, level_number)
            .map_err(|error| error.to_string())?;

        // Hardware vsync normally consumes most or all of this interval. The
        // explicit remainder is still required for software, dummy, remote,
        // and misconfigured drivers that return from presentation immediately.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    Ok(())
}

/// Selects at most one held arrow key using stable directional precedence.
fn keyboard_direction(keyboard: &sdl2::keyboard::KeyboardState<'_>) -> Option<Direction> {
    // A deterministic order avoids diagonal commands, which the original grid
    // does not support when the player holds multiple arrows at once.
    if keyboard.is_scancode_pressed(Scancode::Up) {
        Some(Direction::Up)
    } else if keyboard.is_scancode_pressed(Scancode::Down) {
        Some(Direction::Down)
    } else if keyboard.is_scancode_pressed(Scancode::Left) {
        Some(Direction::Left)
    } else if keyboard.is_scancode_pressed(Scancode::Right) {
        Some(Direction::Right)
    } else {
        None
    }
}
