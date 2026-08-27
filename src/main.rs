//! SDL2 entry point for selecting and playing one original Supaplex level.

use std::{process::ExitCode, time::Duration};

use sdl2::{event::Event, keyboard::Scancode};
use supaplex_clone::{
    actor::Direction,
    cli::Options,
    game::{Game, GameStatus, Input},
    level::{Level, LevelSet},
    render::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Renderer},
};

/// Contains the original 111-level set in the executable.
///
/// Embedding the bytes makes `cargo run -- --level N` independent of the
/// caller's current directory while still parsing the supplied DOS data.
const ORIGINAL_LEVELS: &[u8] = include_bytes!("../data/levels.dat");

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

    // Decode the chosen record before initializing platform resources. This
    // gives corrupt data the same clear error path as bad CLI use.
    let level = match LevelSet::new(ORIGINAL_LEVELS).load(options.level_number()) {
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

    // Physics advances at a fixed rate independent of rendering or monitor
    // refresh. Four animation frames therefore produce a 160 ms cell movement.
    const STEP: Duration = Duration::from_millis(40);
    const MAX_STEPS_PER_FRAME: usize = 6;
    let mut previous = std::time::Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut drop_disk = false;

    'running: loop {
        let now = std::time::Instant::now();
        // Capping a long pause prevents a debugger stop or window drag from
        // causing an unbounded burst of catch-up simulation.
        accumulator += now.duration_since(previous).min(Duration::from_millis(250));
        previous = now;

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
                    // Restarting reconstructs every actor and global toggle from
                    // the immutable decoded record rather than patching state.
                    game = Game::new(level).map_err(|error| error.to_string())?;
                    accumulator = Duration::ZERO;
                }
                Event::KeyDown {
                    scancode: Some(Scancode::D),
                    repeat: false,
                    ..
                } => drop_disk = true,
                _ => {}
            }
        }

        let mut processed_steps = 0;
        while accumulator >= STEP && processed_steps < MAX_STEPS_PER_FRAME {
            let keyboard = event_pump.keyboard_state();
            let input = Input {
                direction: keyboard_direction(&keyboard),
                action: keyboard.is_scancode_pressed(Scancode::Space),
                drop_disk,
            };
            game.tick(input);
            drop_disk = false;
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
        canvas
            .window_mut()
            .set_title(&title)
            .map_err(|error| format!("update window title: {error}"))?;
        renderer
            .draw(&mut canvas, &game, level_number)
            .map_err(|error| error.to_string())?;

        // Vsync normally limits the loop; this tiny sleep also prevents a busy
        // loop on renderers that ignore the requested presentation interval.
        std::thread::sleep(Duration::from_millis(1));
    }

    Ok(())
}

/// Selects at most one held arrow key in a stable priority order.
fn keyboard_direction(keyboard: &sdl2::keyboard::KeyboardState<'_>) -> Option<Direction> {
    // A deterministic order avoids diagonal commands, which the original grid
    // does not support, when the player holds two arrows simultaneously.
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
