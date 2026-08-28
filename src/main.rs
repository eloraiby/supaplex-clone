//! SDL2 entry point for the original front end and fixed-step Supaplex play.

use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use sdl2::{event::Event, keyboard::Scancode};
use supaplex_clone::{
    actor::Direction,
    assets,
    audio::AudioPlayer,
    cli::{FIRST_STEP_RATE, LAST_STEP_RATE, Options},
    frontend::{MenuSelection, fade_in_opacity, splash_frame},
    game::{Game, GameStatus, Input},
    level::LevelSet,
    render::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Renderer},
};

/// Maximum display rate used when vsync is unavailable or ignored.
const RENDER_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// One decoded level-list row retained between main-menu frames.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MenuLevelRow {
    /// One-based number rendered before the level title.
    number: usize,
    /// Trimmed original level title owned independently of its decoded record.
    title: String,
}

/// Previous, selected, and next rows shown in the original level-list frame.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MenuLevelRows {
    /// Row above the selection, absent when level one is selected.
    previous: Option<MenuLevelRow>,
    /// Always-present highlighted row chosen by [`MenuSelection`].
    current: MenuLevelRow,
    /// Row below the selection, absent when the final level is selected.
    next: Option<MenuLevelRow>,
}

/// Terminal result of the blocking main-menu event loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuOutcome {
    /// Start the contained one-based level number.
    Play(usize),
    /// Close the application without starting a level.
    Quit,
}

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

    // Validate the initial menu record before SDL startup so malformed level
    // data remains distinguishable from a later platform initialization error.
    if let Err(error) = LevelSet::new(level_bytes.as_ref()).load(options.level_number()) {
        eprintln!("could not load level {}: {error}", options.level_number());
        return ExitCode::FAILURE;
    }

    match run(
        level_bytes.as_ref(),
        options.level_number(),
        options.steps_per_second(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not run level {}: {error}", options.level_number());
            ExitCode::FAILURE
        }
    }
}

/// Initializes SDL2 and owns the front-end plus one selected fixed-rate play loop.
fn run(level_bytes: &[u8], initial_level: usize, steps_per_second: u32) -> Result<(), String> {
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
    let mut audio = match sdl
        .audio()
        .map_err(|error| format!("initialize SDL2 audio: {error}"))
        .and_then(|subsystem| AudioPlayer::new(&subsystem))
    {
        Ok(audio) => Some(audio),
        Err(error) => {
            // Missing devices are common over remote sessions. The complete
            // visual front end and gameplay remain available without audio.
            eprintln!("audio disabled: {error}");
            None
        }
    };
    if !show_splash(&mut canvas, &mut renderer, &mut event_pump)? {
        // Closing the window during startup is an ordinary successful exit,
        // exactly like closing it from gameplay rather than a loading failure.
        return Ok(());
    }
    let level_set = LevelSet::new(level_bytes);
    let level_count = level_set
        .level_count()
        .map_err(|error| format!("validate level collection: {error}"))?;
    let level_number = match show_main_menu(
        &mut canvas,
        &mut renderer,
        &mut event_pump,
        &mut audio,
        level_set,
        initial_level,
        level_count,
    )? {
        MenuOutcome::Play(level_number) => level_number,
        MenuOutcome::Quit => return Ok(()),
    };
    let level = level_set
        .load(level_number)
        .map_err(|error| format!("load selected level {level_number}: {error}"))?;
    let mut game = Game::new(&level).map_err(|error| error.to_string())?;

    // Convert the validated CLI frequency once. Integer nanoseconds lose less
    // than one nanosecond per update instead of rounding through milliseconds;
    // rendering remains independently capped at sixty frames per second below.
    let step = simulation_step(steps_per_second);
    const MAX_STEPS_PER_FRAME: usize = 6;
    // Vsync is only a request and is ignored by some SDL render backends. An
    // independent deadline prevents those backends from rendering hundreds of
    // redundant frames per second and consuming an entire CPU core.
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
                    game.restart(&level).map_err(|error| error.to_string())?;
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
        while accumulator >= step && processed_steps < MAX_STEPS_PER_FRAME {
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
            accumulator -= step;
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
            .draw(&mut canvas, &game, level_number, steps_per_second)
            .map_err(|error| error.to_string())?;
        canvas.present();

        // Hardware vsync normally consumes most or all of this interval. The
        // explicit remainder is still required for software, dummy, remote,
        // and misconfigured drivers that return from presentation immediately.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    Ok(())
}

/// Runs the keyboard-driven original main menu until play or quit is selected.
fn show_main_menu(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    audio: &mut Option<AudioPlayer>,
    level_set: LevelSet<'_>,
    initial_level: usize,
    level_count: usize,
) -> Result<MenuOutcome, String> {
    // Construction can fail only for an empty validated collection. Keeping the
    // guard here makes the menu safe for custom LEVELS.DAT distributions too.
    let mut selection = MenuSelection::new(initial_level, level_count)
        .ok_or_else(|| "level collection contains no playable records".to_owned())?;
    let mut rows = load_menu_level_rows(level_set, selection)?;
    let started = Instant::now();
    let mut window_title = String::new();

    loop {
        let frame_started = Instant::now();
        let selection_before_events = selection;
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    scancode: Some(Scancode::Escape),
                    repeat: false,
                    ..
                } => return Ok(MenuOutcome::Quit),
                Event::KeyDown {
                    scancode: Some(Scancode::Up | Scancode::Left),
                    ..
                } => selection.move_by(-1),
                Event::KeyDown {
                    scancode: Some(Scancode::Down | Scancode::Right),
                    ..
                } => selection.move_by(1),
                Event::KeyDown {
                    scancode: Some(Scancode::PageUp),
                    ..
                } => selection.move_by(-10),
                Event::KeyDown {
                    scancode: Some(Scancode::PageDown),
                    ..
                } => selection.move_by(10),
                Event::KeyDown {
                    scancode: Some(Scancode::Home),
                    repeat: false,
                    ..
                } => selection.select_first(),
                Event::KeyDown {
                    scancode: Some(Scancode::End),
                    repeat: false,
                    ..
                } => selection.select_last(),
                Event::KeyDown {
                    scancode: Some(Scancode::Return | Scancode::KpEnter | Scancode::Space),
                    repeat: false,
                    ..
                } => return Ok(MenuOutcome::Play(selection.selected_level())),
                Event::KeyDown {
                    scancode: Some(Scancode::M),
                    repeat: false,
                    ..
                } => {
                    // Music remains independently controllable before a level
                    // starts, matching the same key available during gameplay.
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
                    // Effect muting is retained across the selected level because
                    // the same player instance owns both front-end and gameplay.
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

        if selection != selection_before_events {
            // Decode only after the selection changes; stable menu frames reuse
            // their three owned title strings without touching LEVELS.DAT.
            rows = load_menu_level_rows(level_set, selection)?;
        }
        let title = format!(
            "Supaplex - Main Menu - Level {:03}: {}",
            rows.current.number, rows.current.title
        );
        if title != window_title {
            canvas
                .window_mut()
                .set_title(&title)
                .map_err(|error| format!("update main-menu window title: {error}"))?;
            window_title = title;
        }

        renderer
            .draw_menu(
                canvas,
                rows.previous
                    .as_ref()
                    .map(|row| (row.number, row.title.as_str())),
                (rows.current.number, rows.current.title.as_str()),
                rows.next
                    .as_ref()
                    .map(|row| (row.number, row.title.as_str())),
            )
            .map_err(|error| error.to_string())?;
        renderer
            .draw_black_overlay(canvas, fade_in_opacity(started.elapsed()))
            .map_err(|error| error.to_string())?;
        canvas.present();

        // Menu animation is limited separately from simulation because no game
        // updates should accumulate while the user browses the level list.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Decodes the at-most-three original titles visible around one menu selection.
fn load_menu_level_rows(
    level_set: LevelSet<'_>,
    selection: MenuSelection,
) -> Result<MenuLevelRows, String> {
    // Optional neighbor numbers map directly to optional rows, while the current
    // selection is guaranteed valid by `MenuSelection` and must always decode.
    let previous = selection
        .previous_level()
        .map(|number| load_menu_level_row(level_set, number))
        .transpose()?;
    let current = load_menu_level_row(level_set, selection.selected_level())?;
    let next = selection
        .next_level()
        .map(|number| load_menu_level_row(level_set, number))
        .transpose()?;
    Ok(MenuLevelRows {
        previous,
        current,
        next,
    })
}

/// Decodes one level record into the owned subset required by the menu renderer.
fn load_menu_level_row(level_set: LevelSet<'_>, number: usize) -> Result<MenuLevelRow, String> {
    // Dropping tiles and metadata immediately keeps the long-lived menu state
    // small while preserving the parser's validation for every displayed title.
    let level = level_set
        .load(number)
        .map_err(|error| format!("load menu level {number}: {error}"))?;
    Ok(MenuLevelRow {
        number,
        title: level.title().to_owned(),
    })
}

/// Displays the timed original title sequence and reports whether startup continues.
fn show_splash(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
) -> Result<bool, String> {
    // The original title fades at a 70 Hz-derived cadence, but rendering is
    // sampled independently so 60 Hz and high-refresh displays see equal timing.
    let started = Instant::now();
    loop {
        let frame_started = Instant::now();
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(false),
                Event::KeyDown { repeat: false, .. } => return Ok(true),
                _ => {}
            }
        }

        let frame = splash_frame(started.elapsed());
        renderer
            .draw_splash(canvas)
            .map_err(|error| error.to_string())?;
        renderer
            .draw_black_overlay(canvas, frame.black_opacity)
            .map_err(|error| error.to_string())?;
        canvas.present();
        if frame.finished {
            return Ok(true);
        }

        // Keep the startup loop responsive without busy-spinning when a render
        // backend accepts the vsync request but does not actually block on it.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Converts a validated updates-per-second rate to one fixed-step duration.
fn simulation_step(steps_per_second: u32) -> Duration {
    // `Options` enforces this range before SDL startup. Keep the assertion near
    // the division so a future non-CLI caller cannot silently violate it in a
    // debug build; the positive minimum also makes division by zero impossible.
    debug_assert!((FIRST_STEP_RATE..=LAST_STEP_RATE).contains(&steps_per_second));
    Duration::from_nanos(1_000_000_000 / u64::from(steps_per_second))
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

#[cfg(test)]
mod tests {
    //! Fixed-step conversion checks that do not initialize SDL.

    use std::time::Duration;

    use super::simulation_step;
    use supaplex_clone::cli::{DEFAULT_STEP_RATE, FIRST_STEP_RATE, LAST_STEP_RATE};

    /// Confirms the omitted-option default retains the historical 35-Hz duration.
    #[test]
    fn default_step_rate_preserves_original_timing() {
        let duration = simulation_step(DEFAULT_STEP_RATE);

        assert_eq!(duration, Duration::from_nanos(1_000_000_000 / 35));
    }

    /// Confirms both validated custom endpoints use the requested rate divisor.
    #[test]
    fn custom_step_rate_boundaries_convert_to_nanoseconds() {
        assert_eq!(
            simulation_step(FIRST_STEP_RATE),
            Duration::from_nanos(1_000_000_000 / u64::from(FIRST_STEP_RATE))
        );
        assert_eq!(
            simulation_step(LAST_STEP_RATE),
            Duration::from_nanos(1_000_000_000 / u64::from(LAST_STEP_RATE))
        );
    }
}
