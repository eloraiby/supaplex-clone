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
    frontend::{
        MenuSelection, ORIGINAL_FADE_DURATION, fade_in_opacity, fade_out_opacity, splash_frame,
    },
    game::{Game, GameStatus, Input},
    level::{Level, LevelSet},
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

/// Terminal result of one selected gameplay session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GameOutcome {
    /// Return to the menu with the contained level initially highlighted.
    Menu(usize),
    /// Close the application without revisiting the menu.
    Quit,
}

/// Immutable metadata shared by one game loop and its two palette fades.
#[derive(Clone, Copy, Debug)]
struct LevelSession<'level> {
    /// Decoded source record used to construct and restart the game.
    level: &'level Level,
    /// One-based level number shown in panel, window title, and next selection.
    level_number: usize,
    /// Total playable records used to clamp automatic completion advancement.
    level_count: usize,
    /// Validated fixed simulation frequency selected on the command line.
    steps_per_second: u32,
}

/// Direction of a full-screen black palette-style transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FadeDirection {
    /// Reveal a new screen by reducing black opacity from 255 to zero.
    In,
    /// Hide the current screen by increasing black opacity from zero to 255.
    Out,
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
    let mut menu_level = initial_level;
    loop {
        let level_number = match show_main_menu(
            &mut canvas,
            &mut renderer,
            &mut event_pump,
            &mut audio,
            level_set,
            menu_level,
            level_count,
        )? {
            MenuOutcome::Play(level_number) => level_number,
            MenuOutcome::Quit => return Ok(()),
        };
        let level = level_set
            .load(level_number)
            .map_err(|error| format!("load selected level {level_number}: {error}"))?;
        match play_level(
            &mut canvas,
            &mut renderer,
            &mut event_pump,
            &mut audio,
            LevelSession {
                level: &level,
                level_number,
                level_count,
                steps_per_second,
            },
        )? {
            GameOutcome::Menu(next_selection) => menu_level = next_selection,
            GameOutcome::Quit => return Ok(()),
        }
    }
}

/// Plays one selected level through its entry and exit transitions.
fn play_level(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    audio: &mut Option<AudioPlayer>,
    session: LevelSession<'_>,
) -> Result<GameOutcome, String> {
    // Constructing after the menu fade guarantees no simulation time elapses
    // while the selected record is still hidden behind the old screen.
    let mut game = Game::new(session.level).map_err(|error| error.to_string())?;
    if let Some(audio) = audio.as_mut() {
        // A new level clears any protected terminal effect and resumes music
        // only when the user has not muted it.
        audio.restart_level();
    }
    if !fade_game(
        canvas,
        renderer,
        event_pump,
        &game,
        session,
        FadeDirection::In,
    )? {
        return Ok(GameOutcome::Quit);
    }

    // Convert the validated CLI frequency once per session. Integer nanoseconds
    // lose less than one nanosecond per update instead of rounding milliseconds.
    let step = simulation_step(session.steps_per_second);
    const MAX_STEPS_PER_FRAME: usize = 6;
    let mut previous = Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut window_title = String::new();

    loop {
        // The frame start serves both the fixed-step accumulator and the
        // renderer's fallback deadline, keeping their clocks consistent.
        let frame_started = Instant::now();
        accumulator += frame_started
            .duration_since(previous)
            .min(Duration::from_millis(250));
        previous = frame_started;
        let mut return_to_menu = false;

        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(GameOutcome::Quit),
                Event::KeyDown {
                    scancode: Some(Scancode::Escape),
                    repeat: false,
                    ..
                } => return_to_menu = true,
                Event::KeyDown {
                    scancode: Some(Scancode::R),
                    repeat: false,
                    ..
                } => {
                    // Restarting reconstructs actors and level toggles while
                    // retaining the process RNG stream, as original play does.
                    game.restart(session.level)
                        .map_err(|error| error.to_string())?;
                    if let Some(audio) = audio.as_mut() {
                        audio.restart_level();
                    }
                    accumulator = Duration::ZERO;
                    previous = frame_started;
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

        if return_to_menu {
            if !fade_game(
                canvas,
                renderer,
                event_pump,
                &game,
                session,
                FadeDirection::Out,
            )? {
                return Ok(GameOutcome::Quit);
            }
            if let Some(audio) = audio.as_mut() {
                // Clear any partially playing effect before menu navigation and
                // resume soundtrack playback if it remains user-enabled.
                audio.restart_level();
            }
            return Ok(GameOutcome::Menu(session.level_number));
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
            // Discard excessive lag rather than replaying delayed input long
            // after the application has become responsive again.
            accumulator = Duration::ZERO;
        }

        let outcome = match game.status() {
            GameStatus::Playing => "PLAYING",
            GameStatus::Completed => "COMPLETE",
            GameStatus::Dead => "DESTROYED",
        };
        let title = format!(
            "Supaplex - Level {:03}: {} - {} Infotrons - {outcome}",
            session.level_number,
            game.title(),
            game.remaining_infotrons()
        );
        if title != window_title {
            // Updating native window chrome can be expensive, so cross that
            // platform boundary only when a visible value has changed.
            canvas
                .window_mut()
                .set_title(&title)
                .map_err(|error| format!("update window title: {error}"))?;
            window_title = title;
        }
        renderer
            .draw(
                canvas,
                &game,
                session.level_number,
                session.steps_per_second,
            )
            .map_err(|error| error.to_string())?;
        canvas.present();

        if game.terminal_transition_ready() {
            // Completion highlights the next record when one exists; death
            // returns to the same level so Enter offers an immediate retry.
            let next_selection = if game.status() == GameStatus::Completed {
                session
                    .level_number
                    .saturating_add(1)
                    .min(session.level_count)
            } else {
                session.level_number
            };
            if !fade_game(
                canvas,
                renderer,
                event_pump,
                &game,
                session,
                FadeDirection::Out,
            )? {
                return Ok(GameOutcome::Quit);
            }
            if let Some(audio) = audio.as_mut() {
                audio.restart_level();
            }
            return Ok(GameOutcome::Menu(next_selection));
        }

        // Hardware vsync normally consumes most of this interval. The explicit
        // remainder covers software, dummy, remote, and misconfigured backends.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Fades a stationary gameplay snapshot between black and the game palette.
fn fade_game(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    game: &Game,
    session: LevelSession<'_>,
    direction: FadeDirection,
) -> Result<bool, String> {
    // Gameplay remains paused during both directions. This preserves the first
    // board state on entry and the final terminal or Escape state on departure.
    let started = Instant::now();
    loop {
        let frame_started = Instant::now();
        if event_pump
            .poll_iter()
            .any(|event| matches!(event, Event::Quit { .. }))
        {
            return Ok(false);
        }

        let elapsed = started.elapsed();
        let opacity = match direction {
            FadeDirection::In => fade_in_opacity(elapsed),
            FadeDirection::Out => fade_out_opacity(elapsed),
        };
        renderer
            .draw(canvas, game, session.level_number, session.steps_per_second)
            .map_err(|error| error.to_string())?;
        renderer
            .draw_black_overlay(canvas, opacity)
            .map_err(|error| error.to_string())?;
        canvas.present();
        if elapsed >= ORIGINAL_FADE_DURATION {
            return Ok(true);
        }

        // A stable frame cap keeps the duration wall-clock based and prevents a
        // non-vsync renderer from consuming a core during the palette effect.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
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
        let mut start_level = false;
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
                } => start_level = true,
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
        if start_level {
            // Fade the unchanged selected menu to black before constructing the
            // level, hiding the state swap between two opaque terminal frames.
            if !fade_menu_to_black(canvas, renderer, event_pump, &rows)? {
                return Ok(MenuOutcome::Quit);
            }
            return Ok(MenuOutcome::Play(selection.selected_level()));
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

        draw_menu_rows(canvas, renderer, &rows)?;
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

/// Fades the selected main-menu snapshot to opaque black before level loading.
fn fade_menu_to_black(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    rows: &MenuLevelRows,
) -> Result<bool, String> {
    // Redrawing MENU.DAT on every sample keeps the transition independent from
    // back-buffer retention and makes window exposes safe during the fade.
    let started = Instant::now();
    loop {
        let frame_started = Instant::now();
        if event_pump.poll_iter().any(|event| {
            matches!(
                event,
                Event::Quit { .. }
                    | Event::KeyDown {
                        scancode: Some(Scancode::Escape),
                        repeat: false,
                        ..
                    }
            )
        }) {
            return Ok(false);
        }

        let elapsed = started.elapsed();
        draw_menu_rows(canvas, renderer, rows)?;
        renderer
            .draw_black_overlay(canvas, fade_out_opacity(elapsed))
            .map_err(|error| error.to_string())?;
        canvas.present();
        if elapsed >= ORIGINAL_FADE_DURATION {
            return Ok(true);
        }

        // Transition sampling follows the shared display cap; opacity itself is
        // derived from elapsed time and therefore does not depend on refresh rate.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Draws one retained menu-row set through the renderer's borrowed string view.
fn draw_menu_rows(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    rows: &MenuLevelRows,
) -> Result<(), String> {
    // Converting owned rows to short-lived tuples at this boundary avoids
    // storing references into movable `String` fields inside the menu state.
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
        .map_err(|error| error.to_string())
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
