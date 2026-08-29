//! SDL2 entry point for the original front end and fixed-step Supaplex play.

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sdl2::{
    event::Event,
    keyboard::{Scancode, TextInputUtil},
    mouse::MouseButton,
};
use supaplex_clone::platform as sdl2;
use supaplex_clone::{
    actor::Direction,
    assets,
    audio::AudioPlayer,
    cli::{FIRST_STEP_RATE, LAST_STEP_RATE, Options},
    demo::Demo,
    frontend::{
        ControlsTarget, MainMenuTarget, MenuSelection, ORIGINAL_FADE_DURATION, controls_target_at,
        fade_in_opacity, fade_out_opacity, main_menu_player_row_at, main_menu_target_at,
        splash_frame,
    },
    game::{Game, GameStatus, Input},
    level::{Level, LevelSet},
    profiles::{LevelResult, MAX_PLAYER_NAME_LENGTH, MAX_PLAYERS, PlayerBook, ProfileError},
    render::{LOGICAL_HEIGHT, LOGICAL_WIDTH, MenuDisplay, MenuLevelLine, MenuLevelStyle, Renderer},
};

/// Maximum display rate used when vsync is unavailable or ignored.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
const RENDER_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
const RENDER_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 35);

/// Optional exact player-save path used by portable installs and test sessions.
const PLAYER_PROFILE_PATH_ENVIRONMENT_VARIABLE: &str = "SUPAPLEX_PROFILE_PATH";

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
    /// Return to the menu with selection and optional successful-play duration.
    Menu {
        /// One-based level initially highlighted after the gameplay transition.
        next_selection: usize,
        /// Whole successful-session seconds to add to the current player.
        completed_seconds: Option<u64>,
    },
    /// Close the application without revisiting the menu.
    Quit,
}

/// Terminal result of one original attract-mode demonstration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DemoOutcome {
    /// Return to the menu and report the simulation state reached by the stream.
    Menu(GameStatus),
    /// Close the application without revisiting the menu.
    Quit,
}

/// Menu operation awaiting text entry or confirmation through the OK button.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingMenuAction {
    /// Name currently being entered for a new player profile.
    NewPlayer(String),
    /// Deletion of the selected player awaiting OK or Enter.
    DeletePlayer,
    /// Skip of the selected level awaiting OK or Enter.
    SkipLevel,
}

/// Original full-screen page displayed temporarily outside the main menu.
enum AuxiliaryPage<'content> {
    /// Illustrated actor and hardware GFX tutorial.
    GfxTutor,
    /// Information background plus owned text and original coordinates.
    Information(&'content [(String, i32, i32)]),
}

/// Persistent and decoded resources shared throughout one main-menu visit.
struct MainMenuResources<'resource> {
    /// Borrowed validated level collection used to decode visible titles.
    level_set: LevelSet<'resource>,
    /// Number of playable records used to clamp selection state.
    level_count: usize,
    /// Ten validated original demonstrations in F1 through F10 order.
    demos: &'resource [Demo],
    /// Validated fixed simulation frequency used during demo playback.
    steps_per_second: u32,
    /// Mutable player list updated by menu actions and gameplay progression.
    players: &'resource mut PlayerBook,
    /// Writable platform preference file receiving immediate player updates.
    profile_path: &'resource Path,
    /// SDL text-input controller activated only while entering a new name.
    text_input: &'resource TextInputUtil,
}

/// Borrowed dynamic state required to repaint one main-menu snapshot.
struct MainMenuFrame<'frame> {
    /// Three retained level titles surrounding the current selection.
    rows: &'frame MenuLevelRows,
    /// Current persistent player list and ranking source.
    players: &'frame PlayerBook,
    /// Center-field status, prompt, or confirmation message.
    message: &'frame str,
    /// Zero-based first row in the visible five-entry ranking window.
    ranking_offset: usize,
    /// Button under the mouse for exact-region hover feedback.
    hovered: Option<MainMenuTarget>,
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

/// Emits PocketGo-only startup milestones for the diagnostic launcher log.
fn startup_trace(message: &str) {
    #[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
    eprintln!("supaplex_startup={message}");

    #[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
    let _ = message;
}

/// Parses the command line, loads one record, and runs its SDL2 session.
fn main() -> ExitCode {
    startup_trace("process_entered");

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
    startup_trace("bundled_levels_validated");

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

/// Initializes SDL2 and owns the front end plus one fixed-rate play loop.
fn run(level_bytes: &[u8], initial_level: usize, steps_per_second: u32) -> Result<(), String> {
    startup_trace(if cfg!(debug_assertions) {
        "build_debug"
    } else {
        "build_release"
    });

    // Nearest-neighbor scaling preserves the hard pixel edges of the original
    // 16×16 artwork after its 2× atlas repack and logical-window scaling.
    sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");

    // SDL 2.30's PulseAudio backend can leave both its device and hotplug
    // threads asleep in `pa_threaded_mainloop_wait` after an output failure.
    // `SDL_CloseAudioDevice` then waits forever to join the device thread, as
    // observed from both Escape and window-close paths. Modern Linux desktops
    // already expose the same graph through native PipeWire, whose SDL backend
    // does not share that shutdown deadlock. The comma-separated value asks SDL
    // to try PipeWire first while retaining PulseAudio for older installations.
    // `SDL_SetHint` uses normal priority, so an explicit `SDL_AUDIODRIVER`
    // environment override still wins for users who require another backend.
    #[cfg(all(
        target_os = "linux",
        not(any(feature = "pocketgo", target_env = "uclibc"))
    ))]
    sdl2::hint::set("SDL_AUDIODRIVER", "pipewire,pulseaudio");

    let sdl = sdl2::init().map_err(|error| format!("initialize SDL2: {error}"))?;
    startup_trace("platform_initialized");
    let video = sdl
        .video()
        .map_err(|error| format!("initialize SDL2 video: {error}"))?;
    startup_trace("video_initialized");
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
    startup_trace("framebuffer_canvas_ready");

    // The creator outlives `Renderer`, satisfying SDL texture lifetime rules
    // without leaking either the canvas or an atlas texture.
    let texture_creator = canvas.texture_creator();
    let mut renderer = Renderer::new(&texture_creator).map_err(|error| error.to_string())?;
    startup_trace("graphics_decoded");
    let mut event_pump = sdl
        .event_pump()
        .map_err(|error| format!("create SDL2 event pump: {error}"))?;
    startup_trace("input_opened");
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
    startup_trace(if audio.is_some() {
        "audio_opened"
    } else {
        "audio_disabled"
    });
    startup_trace("splash_entered");
    if !show_splash(&mut canvas, &mut renderer, &mut event_pump)? {
        // Closing the window during startup is an ordinary successful exit,
        // exactly like closing it from gameplay rather than a loading failure.
        shutdown_audio(&mut audio);
        return Ok(());
    }
    startup_trace("splash_completed");
    let level_set = LevelSet::new(level_bytes);
    let level_count = level_set
        .level_count()
        .map_err(|error| format!("validate level collection: {error}"))?;
    let demo_assets = assets::load_demos().map_err(|error| format!("load demos: {error}"))?;
    let demos: Vec<Demo> = demo_assets
        .demos
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            Demo::decode(bytes.as_ref(), level_count)
                .map_err(|error| format!("decode demo {}: {error}", index + 1))
        })
        .collect::<Result<_, _>>()?;
    let profile_path = player_profile_path()?;
    let mut players = PlayerBook::load(&profile_path, level_count)
        .map_err(|error| format!("load player profiles: {error}"))?;
    startup_trace("profiles_loaded");
    let text_input = video.text_input();
    let mut menu_level = initial_level;
    startup_trace("main_menu_entered");
    loop {
        let level_number = match show_main_menu(
            &mut canvas,
            &mut renderer,
            &mut event_pump,
            &mut audio,
            menu_level,
            MainMenuResources {
                level_set,
                level_count,
                demos: &demos,
                steps_per_second,
                players: &mut players,
                profile_path: &profile_path,
                text_input: &text_input,
            },
        )? {
            MenuOutcome::Play(level_number) => level_number,
            MenuOutcome::Quit => {
                // Stop the callback before video and SDL begin dropping. Some
                // audio backends otherwise wait indefinitely during device close.
                shutdown_audio(&mut audio);
                return Ok(());
            }
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
            GameOutcome::Menu {
                next_selection,
                completed_seconds,
            } => {
                // Progress is committed only after the terminal animation and
                // fade have completed, so an interrupted session cannot unlock.
                if let Some(elapsed_seconds) = completed_seconds {
                    players
                        .complete_current(level_number, elapsed_seconds)
                        .map_err(|error| format!("record completed level: {error}"))?;
                    save_player_book(&players, &profile_path)?;
                }
                menu_level = next_selection;
            }
            GameOutcome::Quit => {
                // Window-manager exits share the same deterministic teardown as
                // menu Escape instead of relying on implicit local drop order.
                shutdown_audio(&mut audio);
                return Ok(());
            }
        }
    }
}

/// Removes and synchronously shuts down the optional audio player.
fn shutdown_audio(audio: &mut Option<AudioPlayer>) {
    // Taking the value first prevents a later scope exit from attempting a
    // second close. Headless sessions deliberately have nothing to stop.
    if let Some(audio) = audio.take() {
        audio.shutdown();
    }
}

/// Resolves the cross-platform writable file used for persistent player state.
fn player_profile_path() -> Result<PathBuf, String> {
    // An explicit file supports portable installations and isolated automated
    // sessions without tying profile storage to the asset-root feature.
    if let Some(path) = env::var_os(PLAYER_PROFILE_PATH_ENVIRONMENT_VARIABLE) {
        return Ok(PathBuf::from(path));
    }

    // SDL otherwise chooses the operating system's per-user preference
    // directory, keeping saves outside both bundled and unbundled asset trees.
    let directory = sdl2::filesystem::pref_path("eloraiby", "supaplex-clone")
        .map_err(|error| format!("resolve player profile directory: {error}"))?;
    Ok(PathBuf::from(directory).join("players.dat"))
}

/// Persists a mutated player book with one consistent front-end diagnostic.
fn save_player_book(players: &PlayerBook, path: &Path) -> Result<(), String> {
    // Menu actions call this immediately after mutation, minimizing progress
    // loss if the process later closes through the window manager.
    players
        .save(path)
        .map_err(|error| format!("save player profiles: {error}"))
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
    let mut completion_tick = None;

    loop {
        // The frame start serves both the fixed-step accumulator and the
        // renderer's fallback deadline, keeping their clocks consistent.
        let frame_started = Instant::now();
        accumulator += frame_started
            .duration_since(previous)
            .min(Duration::from_millis(250));
        previous = frame_started;
        // Escape requests the same in-world death as a lethal actor rather
        // than bypassing the explosion animation with an immediate menu fade.
        let mut destroy_murphy = false;

        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(GameOutcome::Quit),
                Event::KeyDown {
                    scancode: Some(Scancode::Escape | Scancode::LCtrl),
                    repeat: false,
                    ..
                } => destroy_murphy = true,
                Event::KeyDown {
                    scancode: Some(Scancode::R | Scancode::LShift),
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
                    completion_tick = None;
                    accumulator = Duration::ZERO;
                    previous = frame_started;
                }
                Event::KeyDown {
                    scancode: Some(Scancode::M | Scancode::Tab),
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
                    scancode: Some(Scancode::S | Scancode::Backspace),
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

        if destroy_murphy {
            // The simulation owns blast construction and terminal timing. A
            // repeated Escape during the death sequence is an intentional no-op.
            game.destroy_murphy();
        }

        let mut processed_steps = 0;
        while accumulator >= step && processed_steps < MAX_STEPS_PER_FRAME {
            let keyboard = event_pump.keyboard_state();
            let input = Input {
                direction: keyboard_direction(&keyboard),
                action: keyboard.is_scancode_pressed(Scancode::Space)
                    || keyboard.is_scancode_pressed(Scancode::LAlt),
            };
            game.tick(input);
            if game.status() == GameStatus::Completed && completion_tick.is_none() {
                // Capture the instant completion first appears so the original
                // terminal delay does not inflate persistent player time.
                completion_tick = Some(game.tick_count());
            }
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
            return Ok(GameOutcome::Menu {
                next_selection,
                completed_seconds: completion_tick
                    .map(|ticks| ticks / u64::from(session.steps_per_second)),
            });
        }

        // Hardware vsync normally consumes most of this interval. The explicit
        // remainder covers software, dummy, remote, and misconfigured backends.
        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Replays one original input stream without accepting live gameplay commands.
fn play_demo(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    audio: &mut Option<AudioPlayer>,
    demo: &Demo,
    session: LevelSession<'_>,
) -> Result<DemoOutcome, String> {
    // Legacy standalone demos do not embed a SpeedFix seed, so the original
    // zero-initialized demo seed table supplies zero for deterministic Bugs.
    let mut game = Game::with_random_seed(session.level, 0).map_err(|error| error.to_string())?;
    if let Some(audio) = audio.as_mut() {
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
        return Ok(DemoOutcome::Quit);
    }

    let step = simulation_step(session.steps_per_second);
    const MAX_STEPS_PER_FRAME: usize = 6;
    let mut playback = demo.playback();
    let mut previous = Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut stream_finished = false;
    let mut window_title = String::new();

    loop {
        let frame_started = Instant::now();
        accumulator += frame_started
            .duration_since(previous)
            .min(Duration::from_millis(250));
        previous = frame_started;
        let mut interrupted = false;
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(DemoOutcome::Quit),
                Event::KeyDown { repeat: false, .. }
                | Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left | MouseButton::Right,
                    ..
                } => interrupted = true,
                _ => {}
            }
        }

        let mut processed_steps = 0;
        while accumulator >= step && processed_steps < MAX_STEPS_PER_FRAME && !stream_finished {
            if let Some(input) = playback.next() {
                game.tick(input);
                if let Some(audio) = audio.as_mut() {
                    // Demo effects pass through the same one-channel priority
                    // gate as live play while recorded input remains immutable.
                    for effect in game.take_sound_effects() {
                        audio.play(effect);
                    }
                } else {
                    game.take_sound_effects();
                }
                accumulator -= step;
                processed_steps += 1;
            } else {
                stream_finished = true;
            }
        }
        if processed_steps == MAX_STEPS_PER_FRAME {
            // Discard excess lag exactly like live play so demo input does not
            // continue long after the display becomes responsive again.
            accumulator = Duration::ZERO;
        }

        let title = format!(
            "Supaplex - Demo - Level {:03}: {} - {} steps remaining",
            session.level_number,
            game.title(),
            playback.len()
        );
        if title != window_title {
            canvas
                .window_mut()
                .set_title(&title)
                .map_err(|error| format!("update demo window title: {error}"))?;
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

        if interrupted || stream_finished || game.terminal_transition_ready() {
            let status = game.status();
            if !fade_game(
                canvas,
                renderer,
                event_pump,
                &game,
                session,
                FadeDirection::Out,
            )? {
                return Ok(DemoOutcome::Quit);
            }
            if let Some(audio) = audio.as_mut() {
                audio.restart_level();
            }
            return Ok(DemoOutcome::Menu(status));
        }

        if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

/// Maps the original F1 through F10 shortcuts to zero-based demo indices.
fn demo_index_for_scancode(scancode: Scancode) -> Option<usize> {
    // An explicit mapping avoids relying on SDL enum discriminant contiguity.
    match scancode {
        Scancode::F1 => Some(0),
        Scancode::F2 => Some(1),
        Scancode::F3 => Some(2),
        Scancode::F4 => Some(3),
        Scancode::F5 => Some(4),
        Scancode::F6 => Some(5),
        Scancode::F7 => Some(6),
        Scancode::F8 => Some(7),
        Scancode::F9 => Some(8),
        Scancode::F10 => Some(9),
        _ => None,
    }
}

/// Selects one of ten attract demos from the current wall-clock instant.
fn random_demo_index() -> usize {
    // The original seeds its random selection from the clock. Nanoseconds give
    // repeated clicks useful variation while a pre-epoch failure falls back to 0.
    let ticks = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    usize::try_from(ticks % 10).expect("demo modulo always fits usize")
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
    // board state on entry and the final terminal state on departure.
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

/// Runs the interactive original main menu until play or quit is selected.
fn show_main_menu(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    audio: &mut Option<AudioPlayer>,
    initial_level: usize,
    resources: MainMenuResources<'_>,
) -> Result<MenuOutcome, String> {
    // Destructure once so action branches remain readable while the grouping
    // keeps this event-loop interface below the clippy complexity threshold.
    let MainMenuResources {
        level_set,
        level_count,
        demos,
        steps_per_second,
        players,
        profile_path,
        text_input,
    } = resources;
    // Construction can fail only for an empty validated collection. Keeping the
    // guard here makes the menu safe for custom LEVELS.DAT distributions too.
    let mut selection = MenuSelection::new(initial_level, level_count)
        .ok_or_else(|| "level collection contains no playable records".to_owned())?;
    let mut rows = load_menu_level_rows(level_set, selection)?;
    let mut reveal_started = Instant::now();
    let mut window_title = String::new();
    let mut message = "  WELCOME TO SUPAPLEX  ".to_owned();
    let mut pending = None;
    let mut hovered = None;
    let mut ranking_offset = 0usize;

    loop {
        let frame_started = Instant::now();
        let selection_before_events = selection;
        let mut requested_action = None;
        let mut requested_demo = None;
        let mut clicked_player_row = None;
        for event in event_pump.poll_iter() {
            // Text entry is a modal menu state: ordinary level shortcuts cannot
            // trigger while an unfinished name is receiving SDL text events.
            if let Some(PendingMenuAction::NewPlayer(name)) = pending.as_mut() {
                match event {
                    Event::Quit { .. } => {
                        text_input.stop();
                        return Ok(MenuOutcome::Quit);
                    }
                    Event::KeyDown {
                        scancode: Some(Scancode::Escape | Scancode::LCtrl),
                        repeat: false,
                        ..
                    } => {
                        text_input.stop();
                        pending = None;
                        message = "  WELCOME TO SUPAPLEX  ".to_owned();
                    }
                    Event::KeyDown {
                        scancode: Some(Scancode::Backspace),
                        repeat: false,
                        ..
                    } => {
                        name.pop();
                        message = new_player_prompt(name);
                    }
                    Event::KeyDown {
                        scancode: Some(Scancode::Return | Scancode::KpEnter),
                        repeat: false,
                        ..
                    } => {
                        let entered_name = name.clone();
                        match players.add(&entered_name) {
                            Ok(()) => {
                                save_player_book(players, profile_path)?;
                                message = "     PLAYER CREATED    ".to_owned();
                                pending = None;
                                text_input.stop();
                            }
                            Err(error) => message = profile_menu_message(&error),
                        }
                    }
                    Event::TextInput { text, .. } => {
                        // SDL may deliver several Unicode characters together.
                        // Retain only original ASCII name glyphs and the first
                        // eight bytes that fit the fixed menu field.
                        for character in text.chars() {
                            let character = character.to_ascii_uppercase();
                            if name.len() < MAX_PLAYER_NAME_LENGTH
                                && (character.is_ascii_uppercase()
                                    || character.is_ascii_digit()
                                    || character == ' '
                                    || character == '-')
                            {
                                name.push(character);
                            }
                        }
                        message = new_player_prompt(name);
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    scancode: Some(Scancode::Escape | Scancode::LCtrl),
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
                    scancode: Some(Scancode::PageUp | Scancode::Tab),
                    ..
                } => selection.move_by(-10),
                Event::KeyDown {
                    scancode: Some(Scancode::PageDown | Scancode::Backspace),
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
                    scancode:
                        Some(Scancode::Return | Scancode::KpEnter | Scancode::Space | Scancode::LAlt),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::Ok),
                Event::KeyDown {
                    scancode: Some(Scancode::N),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::NewPlayer),
                Event::KeyDown {
                    scancode: Some(Scancode::Delete),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::DeletePlayer),
                Event::KeyDown {
                    scancode: Some(Scancode::K),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::SkipLevel),
                Event::KeyDown {
                    scancode: Some(Scancode::T),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::Statistics),
                Event::KeyDown {
                    scancode: Some(Scancode::G),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::GfxTutor),
                Event::KeyDown {
                    scancode: Some(Scancode::D),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::Demo),
                Event::KeyDown {
                    scancode: Some(Scancode::C),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::Controls),
                Event::KeyDown {
                    scancode: Some(Scancode::LShift),
                    repeat: false,
                    ..
                } => requested_action = Some(MainMenuTarget::Controls),
                Event::KeyDown {
                    scancode: Some(scancode),
                    repeat: false,
                    ..
                } if demo_index_for_scancode(scancode).is_some() => {
                    requested_demo = demo_index_for_scancode(scancode);
                    requested_action = Some(MainMenuTarget::Demo);
                }
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
                Event::MouseMotion { x, y, .. } => hovered = main_menu_target_at(x, y),
                Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    x,
                    y,
                    ..
                } => {
                    requested_action = main_menu_target_at(x, y);
                    clicked_player_row = main_menu_player_row_at(x, y);
                }
                _ => {}
            }
        }

        if selection != selection_before_events {
            // Decode only after the selection changes; stable menu frames reuse
            // their three owned title strings without touching LEVELS.DAT.
            rows = load_menu_level_rows(level_set, selection)?;
        }
        let mut start_level = false;
        if let Some(action) = requested_action {
            // Any non-OK action cancels an outstanding confirmation before the
            // new action runs, matching the original click-away behavior.
            if action != MainMenuTarget::Ok
                && matches!(
                    pending,
                    Some(PendingMenuAction::DeletePlayer | PendingMenuAction::SkipLevel)
                )
            {
                pending = None;
            }
            match action {
                MainMenuTarget::NewPlayer => {
                    if players.len() >= MAX_PLAYERS {
                        message = "PLAYER LIST FULL       ".to_owned();
                    } else {
                        pending = Some(PendingMenuAction::NewPlayer(String::new()));
                        message = new_player_prompt("");
                        text_input.start();
                    }
                }
                MainMenuTarget::DeletePlayer => {
                    if let Some(player) = players.current() {
                        message = format!("DELETE '{:<8}' ???", player.name());
                        pending = Some(PendingMenuAction::DeletePlayer);
                    } else {
                        message = "NO PLAYER SELECTED     ".to_owned();
                    }
                }
                MainMenuTarget::SkipLevel => {
                    if players.current().is_some() {
                        message = format!("SKIP LEVEL {:03} ???    ", selection.selected_level());
                        pending = Some(PendingMenuAction::SkipLevel);
                    } else {
                        message = "NO PLAYER SELECTED     ".to_owned();
                    }
                }
                MainMenuTarget::Statistics => {
                    if let Some(player) = players.current() {
                        let lines = statistics_lines(player);
                        if !show_auxiliary_page(
                            canvas,
                            renderer,
                            event_pump,
                            AuxiliaryPage::Information(&lines),
                        )? {
                            return Ok(MenuOutcome::Quit);
                        }
                        reveal_started = Instant::now();
                    } else {
                        message = "NO PLAYER SELECTED     ".to_owned();
                    }
                }
                MainMenuTarget::GfxTutor => {
                    if !show_auxiliary_page(canvas, renderer, event_pump, AuxiliaryPage::GfxTutor)?
                    {
                        return Ok(MenuOutcome::Quit);
                    }
                    reveal_started = Instant::now();
                }
                MainMenuTarget::Demo => {
                    let demo_index = requested_demo.unwrap_or_else(random_demo_index);
                    let demo = demos
                        .get(demo_index)
                        .ok_or_else(|| format!("demo {} is unavailable", demo_index + 1))?;
                    if !fade_menu_to_black(
                        canvas,
                        renderer,
                        event_pump,
                        MainMenuFrame {
                            rows: &rows,
                            players,
                            message: &message,
                            ranking_offset,
                            hovered,
                        },
                    )? {
                        return Ok(MenuOutcome::Quit);
                    }
                    let level = level_set.load(demo.level_number()).map_err(|error| {
                        format!(
                            "load demo {} level {}: {error}",
                            demo_index + 1,
                            demo.level_number()
                        )
                    })?;
                    match play_demo(
                        canvas,
                        renderer,
                        event_pump,
                        audio,
                        demo,
                        LevelSession {
                            level: &level,
                            level_number: demo.level_number(),
                            level_count,
                            steps_per_second,
                        },
                    )? {
                        DemoOutcome::Menu(GameStatus::Completed) => {
                            message = "    DEMO SUCCESSFUL    ".to_owned();
                        }
                        DemoOutcome::Menu(GameStatus::Playing | GameStatus::Dead) => {
                            message = "      DEMO FAILED      ".to_owned();
                        }
                        DemoOutcome::Quit => return Ok(MenuOutcome::Quit),
                    }
                    hovered = None;
                    reveal_started = Instant::now();
                }
                MainMenuTarget::Controls => {
                    if !show_controls_screen(canvas, renderer, event_pump, audio)? {
                        return Ok(MenuOutcome::Quit);
                    }
                    reveal_started = Instant::now();
                }
                MainMenuTarget::RankingUp => ranking_offset = ranking_offset.saturating_sub(1),
                MainMenuTarget::RankingDown => {
                    ranking_offset = ranking_offset
                        .saturating_add(1)
                        .min(players.len().saturating_sub(5));
                }
                MainMenuTarget::Ok => match pending.take() {
                    Some(PendingMenuAction::DeletePlayer) => {
                        if players.delete_current().is_some() {
                            save_player_book(players, profile_path)?;
                            message = "     PLAYER DELETED    ".to_owned();
                        } else {
                            message = "NO PLAYER SELECTED     ".to_owned();
                        }
                    }
                    Some(PendingMenuAction::SkipLevel) => {
                        match players.skip_current(selection.selected_level()) {
                            Ok(()) => {
                                save_player_book(players, profile_path)?;
                                message = "     LEVEL SKIPPED     ".to_owned();
                            }
                            Err(error) => message = profile_menu_message(&error),
                        }
                    }
                    Some(PendingMenuAction::NewPlayer(_)) => {
                        // Name entry consumes Enter earlier and cannot reach this
                        // branch; retaining it makes the enum match exhaustive.
                    }
                    None if players.can_play(selection.selected_level()) => start_level = true,
                    None if players.current().is_none() => {
                        message = "NO PLAYER SELECTED     ".to_owned();
                    }
                    None => message = "LEVEL NOT AVAILABLE    ".to_owned(),
                },
                MainMenuTarget::LevelSet => {
                    message = " ORIGINAL DISK ACTIVE  ".to_owned();
                }
                MainMenuTarget::PlayerUp => {
                    players.select_previous();
                    save_player_book(players, profile_path)?;
                    select_player_next_level(&mut selection, players);
                }
                MainMenuTarget::PlayerDown => {
                    players.select_next();
                    save_player_book(players, profile_path)?;
                    select_player_next_level(&mut selection, players);
                }
                MainMenuTarget::PlayerList => match clicked_player_row {
                    Some(-1) => {
                        players.select_previous();
                        save_player_book(players, profile_path)?;
                        select_player_next_level(&mut selection, players);
                    }
                    Some(1) => {
                        players.select_next();
                        save_player_book(players, profile_path)?;
                        select_player_next_level(&mut selection, players);
                    }
                    _ => {}
                },
                MainMenuTarget::LevelUp => selection.move_by(-1),
                MainMenuTarget::LevelDown => selection.move_by(1),
                MainMenuTarget::Credits => {
                    let lines = credits_lines();
                    if !show_auxiliary_page(
                        canvas,
                        renderer,
                        event_pump,
                        AuxiliaryPage::Information(&lines),
                    )? {
                        return Ok(MenuOutcome::Quit);
                    }
                    reveal_started = Instant::now();
                }
            }
        }

        if selection != selection_before_events {
            // Mouse actions are processed after polling, so refresh rows again
            // when their arrows changed the selection outside the first check.
            rows = load_menu_level_rows(level_set, selection)?;
        }
        if start_level {
            // Fade the unchanged selected menu to black before constructing the
            // level, hiding the state swap between two opaque terminal frames.
            if !fade_menu_to_black(
                canvas,
                renderer,
                event_pump,
                MainMenuFrame {
                    rows: &rows,
                    players,
                    message: &message,
                    ranking_offset,
                    hovered,
                },
            )? {
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

        draw_menu_rows(
            canvas,
            renderer,
            MainMenuFrame {
                rows: &rows,
                players,
                message: &message,
                ranking_offset,
                hovered,
            },
        )?;
        renderer
            .draw_black_overlay(canvas, fade_in_opacity(reveal_started.elapsed()))
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
    frame: MainMenuFrame<'_>,
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
                        scancode: Some(Scancode::Escape | Scancode::LCtrl),
                        repeat: false,
                        ..
                    }
            )
        }) {
            return Ok(false);
        }

        let elapsed = started.elapsed();
        draw_menu_rows(
            canvas,
            renderer,
            MainMenuFrame {
                rows: frame.rows,
                players: frame.players,
                message: frame.message,
                ranking_offset: frame.ranking_offset,
                hovered: frame.hovered,
            },
        )?;
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
    frame: MainMenuFrame<'_>,
) -> Result<(), String> {
    // Format rankings into owned rows before lending both those strings and the
    // profile/level fields to the renderer for this single immediate frame.
    let ordered = frame.players.rankings();
    let rankings: Vec<String> = ordered
        .iter()
        .skip(frame.ranking_offset)
        .take(5)
        .map(|player| format_ranking_row(player))
        .collect();
    let hall_of_fame: Vec<String> = ordered
        .iter()
        .take(10)
        .map(|player| format_hall_of_fame_row(player))
        .collect();
    let player_names = frame.players.visible_names();
    let current = frame.players.current();
    let levels = [
        frame.rows.previous.as_ref().map(|row| MenuLevelLine {
            number: row.number,
            title: row.title.as_str(),
            style: menu_level_style(frame.players, row.number),
        }),
        Some(MenuLevelLine {
            number: frame.rows.current.number,
            title: frame.rows.current.title.as_str(),
            style: menu_level_style(frame.players, frame.rows.current.number),
        }),
        frame.rows.next.as_ref().map(|row| MenuLevelLine {
            number: row.number,
            title: row.title.as_str(),
            style: menu_level_style(frame.players, row.number),
        }),
    ];
    let display = MenuDisplay {
        levels,
        players: player_names,
        player_seconds: current.map_or(0, |player| player.total_seconds()),
        next_level: current.and_then(|player| player.next_level_to_play()),
        message: frame.message,
        rankings: &rankings,
        ranking_position: frame.ranking_offset.saturating_add(1),
        hall_of_fame: &hall_of_fame,
        hovered: frame.hovered,
    };
    renderer
        .draw_menu(canvas, &display)
        .map_err(|error| error.to_string())
}

/// Maps one player's level result and lock boundary to an original row color.
fn menu_level_style(players: &PlayerBook, level_number: usize) -> MenuLevelStyle {
    // With no player every row is locked. Otherwise completed/skipped states
    // take precedence, and only the first unfinished row becomes available.
    let Some(player) = players.current() else {
        return MenuLevelStyle::Locked;
    };
    match player.level_result(level_number) {
        Some(LevelResult::Completed) => MenuLevelStyle::Completed,
        Some(LevelResult::Skipped) => MenuLevelStyle::Skipped,
        Some(LevelResult::Unfinished) if player.can_play(level_number) => MenuLevelStyle::Available,
        Some(LevelResult::Unfinished) | None => MenuLevelStyle::Locked,
    }
}

/// Formats one original-width ranking entry from a persistent player.
fn format_ranking_row(player: &supaplex_clone::profiles::PlayerProfile) -> String {
    // Rankings compare progress first and show the next level beside a padded
    // name and the original three-digit hour clock.
    let level = player.next_level_to_play().unwrap_or(999);
    let seconds = player.total_seconds();
    format!(
        "{level:03} {:<8} {:03}:{:02}:{:02}",
        player.name(),
        (seconds / 3_600).min(999),
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Formats one compact upper-right hall-of-fame entry.
fn format_hall_of_fame_row(player: &supaplex_clone::profiles::PlayerProfile) -> String {
    // The hall field omits progress and fits an eight-character name plus time.
    let seconds = player.total_seconds();
    format!(
        "{:<8} {:03}:{:02}:{:02}",
        player.name(),
        (seconds / 3_600).min(999),
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Formats the live eight-character New Player entry prompt.
fn new_player_prompt(name: &str) -> String {
    // Padding overwrites characters from longer previous frames because MENU.DAT
    // itself is repainted before every text pass.
    format!("YOUR NAME: {:<8}    ", name)
}

/// Converts a profile-domain failure into the original menu's message vocabulary.
fn profile_menu_message(error: &ProfileError) -> String {
    // Known user-policy errors receive specific source-style messages; storage
    // and structural errors remain terminal elsewhere and do not enter here.
    match error {
        ProfileError::InvalidName(_) => "INVALID NAME           ".to_owned(),
        ProfileError::PlayerExists(_) => "PLAYER EXISTS          ".to_owned(),
        ProfileError::PlayerListFull => "PLAYER LIST FULL       ".to_owned(),
        ProfileError::NoPlayerSelected => "NO PLAYER SELECTED     ".to_owned(),
        ProfileError::SkipNotPossible { .. } => "SKIP NOT POSSIBLE      ".to_owned(),
        _ => "PROFILE ERROR          ".to_owned(),
    }
}

/// Jumps the menu selection to the selected player's first unfinished level.
fn select_player_next_level(selection: &mut MenuSelection, players: &PlayerBook) {
    // Fully completed or absent players retain the current row; ordinary
    // profiles reproduce the original autoselection of their next level.
    if let Some(level) = players
        .current()
        .and_then(|player| player.next_level_to_play())
    {
        selection.select(level);
    }
}

/// Builds the original Statistics page plus current clone progression totals.
fn statistics_lines(player: &supaplex_clone::profiles::PlayerProfile) -> Vec<(String, i32, i32)> {
    // Whole-second storage maps back to the original hour/minute/second fields;
    // the average deliberately counts solved levels rather than skipped rows.
    let seconds = player.total_seconds();
    let completed = player.completed_levels();
    let average_minutes = if completed == 0 {
        0
    } else {
        seconds / completed as u64 / 60
    };
    vec![
        ("SUPAPLEX  BY DREAM FACTORY".to_owned(), 80, 20),
        ("(C) DIGITAL INTEGRATION LTD 1991".to_owned(), 64, 50),
        (
            "________________________________________________".to_owned(),
            16,
            60,
        ),
        ("SUPAPLEX PLAYER STATISTICS".to_owned(), 80, 80),
        (format!("CURRENT PLAYER :  {:>8}", player.name()), 80, 100),
        (
            format!(
                "CURRENT LEVEL  :       {:03}",
                player.next_level_to_play().unwrap_or(999)
            ),
            80,
            110,
        ),
        (
            format!(
                "TOTAL TIME USED: {:03}:{:02}:{:02}",
                (seconds / 3_600).min(999),
                seconds / 60 % 60,
                seconds % 60
            ),
            80,
            120,
        ),
        (
            format!("LEVELS COMPLETED: {:3}", player.completed_levels()),
            80,
            130,
        ),
        (
            format!("LEVELS SKIPPED  : {:3}", player.skipped_levels()),
            80,
            140,
        ),
        (
            format!("AVERAGE TIME USED PER LEVEL  {average_minutes:3} MINUTES"),
            32,
            155,
        ),
        ("PRESS ANY KEY OR MOUSE BUTTON".to_owned(), 72, 180),
    ]
}

/// Builds the level-design credits exactly as printed by the original menu.
fn credits_lines() -> Vec<(String, i32, i32)> {
    // Keeping these as data makes the shared information-page loop responsible
    // for transitions and dismissal while preserving every original coordinate.
    vec![
        ("SUPAPLEX  BY DREAM FACTORY".to_owned(), 80, 10),
        ("ORIGINAL DESIGN BY PHILIP JESPERSEN".to_owned(), 56, 40),
        ("AND MICHAEL STOPP".to_owned(), 88, 50),
        ("NEARLY ALL LEVELS BY MICHEAL STOPP".to_owned(), 56, 90),
        ("A FEW LEVELS BY PHILIP JESPERSEN".to_owned(), 64, 100),
        ("HARDLY ANY LEVELS BY BARBARA STOPP".to_owned(), 56, 110),
        ("PRESS ANY KEY OR MOUSE BUTTON".to_owned(), 72, 170),
        ("(C) DIGITAL INTEGRATION LTD 1991".to_owned(), 64, 190),
    ]
}

/// Displays one dismissible auxiliary page with palette-style fades.
fn show_auxiliary_page(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    page: AuxiliaryPage<'_>,
) -> Result<bool, String> {
    // Fade in from black before accepting dismissal so the opening mouse-down
    // cannot immediately close the page it requested.
    let reveal_started = Instant::now();
    loop {
        let frame_started = Instant::now();
        if event_pump
            .poll_iter()
            .any(|event| matches!(event, Event::Quit { .. }))
        {
            return Ok(false);
        }
        draw_auxiliary_page(canvas, renderer, &page)?;
        renderer
            .draw_black_overlay(canvas, fade_in_opacity(reveal_started.elapsed()))
            .map_err(|error| error.to_string())?;
        canvas.present();
        if reveal_started.elapsed() >= ORIGINAL_FADE_DURATION {
            break;
        }
        limit_frontend_frame(frame_started);
    }

    loop {
        let frame_started = Instant::now();
        let mut dismissed = false;
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(false),
                Event::KeyDown { repeat: false, .. }
                | Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left | MouseButton::Right,
                    ..
                } => dismissed = true,
                _ => {}
            }
        }
        draw_auxiliary_page(canvas, renderer, &page)?;
        canvas.present();
        if dismissed {
            break;
        }
        limit_frontend_frame(frame_started);
    }

    // End fully black so the main menu can reveal its restored palette without
    // a one-frame mixture of the two unrelated screen palettes.
    let hide_started = Instant::now();
    loop {
        let frame_started = Instant::now();
        if event_pump
            .poll_iter()
            .any(|event| matches!(event, Event::Quit { .. }))
        {
            return Ok(false);
        }
        draw_auxiliary_page(canvas, renderer, &page)?;
        renderer
            .draw_black_overlay(canvas, fade_out_opacity(hide_started.elapsed()))
            .map_err(|error| error.to_string())?;
        canvas.present();
        if hide_started.elapsed() >= ORIGINAL_FADE_DURATION {
            return Ok(true);
        }
        limit_frontend_frame(frame_started);
    }
}

/// Draws one auxiliary-page variant without owning its event or fade policy.
fn draw_auxiliary_page(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    page: &AuxiliaryPage<'_>,
) -> Result<(), String> {
    // Information strings are converted into borrowed triples for the immediate
    // renderer call; the page retains ownership for the entire modal loop.
    match page {
        AuxiliaryPage::GfxTutor => renderer
            .draw_gfx_tutor(canvas)
            .map_err(|error| error.to_string()),
        AuxiliaryPage::Information(lines) => {
            let borrowed: Vec<_> = lines
                .iter()
                .map(|(text, x, y)| (text.as_str(), *x, *y))
                .collect();
            renderer
                .draw_information(canvas, &borrowed)
                .map_err(|error| error.to_string())
        }
    }
}

/// Runs the clickable original controls screen and updates supported audio state.
fn show_controls_screen(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    audio: &mut Option<AudioPlayer>,
) -> Result<bool, String> {
    // Retain flags locally so rendering never needs to lock the callback merely
    // to repaint an unchanged menu frame.
    let (mut music_enabled, mut effects_enabled) = match audio.as_mut() {
        Some(audio) => (audio.music_enabled(), audio.effects_enabled()),
        None => (false, false),
    };
    let mut hovered = None;
    let mut status = "CLICK MUSIC, FX, DEVICE, OR EXIT".to_owned();
    let reveal_started = Instant::now();

    loop {
        let frame_started = Instant::now();
        let mut leave = false;
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return Ok(false),
                Event::KeyDown {
                    scancode: Some(Scancode::Escape | Scancode::LCtrl),
                    repeat: false,
                    ..
                } => leave = true,
                Event::KeyDown {
                    scancode: Some(Scancode::M),
                    repeat: false,
                    ..
                } => {
                    if let Some(audio) = audio.as_mut() {
                        music_enabled = audio.toggle_music();
                        status = audio_status_message(music_enabled, effects_enabled);
                    }
                }
                Event::KeyDown {
                    scancode: Some(Scancode::S),
                    repeat: false,
                    ..
                } => {
                    if let Some(audio) = audio.as_mut() {
                        effects_enabled = audio.toggle_effects();
                        status = audio_status_message(music_enabled, effects_enabled);
                    }
                }
                Event::MouseMotion { x, y, .. } => hovered = controls_target_at(x, y),
                Event::MouseButtonDown {
                    mouse_btn: MouseButton::Left,
                    x,
                    y,
                    ..
                } => {
                    if let Some(target) = controls_target_at(x, y) {
                        if target == ControlsTarget::Exit {
                            leave = true;
                        } else {
                            let result = apply_controls_target(
                                target,
                                audio,
                                music_enabled,
                                effects_enabled,
                            );
                            music_enabled = result.0;
                            effects_enabled = result.1;
                            status = result.2;
                        }
                    }
                }
                _ => {}
            }
        }

        let title = format!("Supaplex - Controls - {status}");
        canvas
            .window_mut()
            .set_title(&title)
            .map_err(|error| format!("update controls window title: {error}"))?;
        renderer
            .draw_controls(canvas)
            .and_then(|()| {
                renderer.draw_controls_state(canvas, music_enabled, effects_enabled, hovered)
            })
            .map_err(|error| error.to_string())?;
        renderer
            .draw_black_overlay(canvas, fade_in_opacity(reveal_started.elapsed()))
            .map_err(|error| error.to_string())?;
        canvas.present();
        if leave {
            return fade_controls_to_black(
                canvas,
                renderer,
                event_pump,
                music_enabled,
                effects_enabled,
                hovered,
            );
        }
        limit_frontend_frame(frame_started);
    }
}

/// Applies one supported controls-screen target and returns flags plus feedback.
fn apply_controls_target(
    target: ControlsTarget,
    audio: &mut Option<AudioPlayer>,
    current_music: bool,
    current_effects: bool,
) -> (bool, bool, String) {
    // Hardware choices map onto the two independent voices this SDL port owns;
    // unsupported joystick selection is acknowledged without lying about input.
    let (music, effects, message) = match target {
        ControlsTarget::Music => (
            !current_music,
            current_effects,
            if current_music {
                "MUSIC OFF"
            } else {
                "MUSIC ON"
            },
        ),
        ControlsTarget::Effects => (
            current_music,
            !current_effects,
            if current_effects { "FX OFF" } else { "FX ON" },
        ),
        ControlsTarget::Adlib | ControlsTarget::Roland => (true, false, "MUSIC DEVICE SELECTED"),
        ControlsTarget::SoundBlaster
        | ControlsTarget::Internal
        | ControlsTarget::Standard
        | ControlsTarget::Samples => (false, true, "EFFECT DEVICE SELECTED"),
        ControlsTarget::Combined => (true, true, "MUSIC AND FX SELECTED"),
        ControlsTarget::Keyboard => (current_music, current_effects, "KEYBOARD SELECTED"),
        ControlsTarget::Joystick => (current_music, current_effects, "JOYSTICK UNAVAILABLE"),
        ControlsTarget::Exit => (current_music, current_effects, "EXIT"),
    };
    if let Some(audio) = audio.as_mut() {
        audio.set_music_enabled(music);
        audio.set_effects_enabled(effects);
        (music, effects, message.to_owned())
    } else {
        (false, false, "AUDIO DEVICE UNAVAILABLE".to_owned())
    }
}

/// Formats concise controls-screen audio feedback from two independent voices.
fn audio_status_message(music_enabled: bool, effects_enabled: bool) -> String {
    // A fixed four-state vocabulary keeps the native window title stable.
    format!(
        "MUSIC {} / FX {}",
        if music_enabled { "ON" } else { "OFF" },
        if effects_enabled { "ON" } else { "OFF" }
    )
}

/// Fades the current controls snapshot to black before restoring the main menu.
fn fade_controls_to_black(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    renderer: &mut Renderer<'_>,
    event_pump: &mut sdl2::EventPump,
    music_enabled: bool,
    effects_enabled: bool,
    hovered: Option<ControlsTarget>,
) -> Result<bool, String> {
    // Audio state remains visually fixed while only black opacity advances.
    let started = Instant::now();
    loop {
        let frame_started = Instant::now();
        if event_pump
            .poll_iter()
            .any(|event| matches!(event, Event::Quit { .. }))
        {
            return Ok(false);
        }
        renderer
            .draw_controls(canvas)
            .and_then(|()| {
                renderer.draw_controls_state(canvas, music_enabled, effects_enabled, hovered)
            })
            .map_err(|error| error.to_string())?;
        renderer
            .draw_black_overlay(canvas, fade_out_opacity(started.elapsed()))
            .map_err(|error| error.to_string())?;
        canvas.present();
        if started.elapsed() >= ORIGINAL_FADE_DURATION {
            return Ok(true);
        }
        limit_frontend_frame(frame_started);
    }
}

/// Sleeps for the unused portion of one non-gameplay display frame.
fn limit_frontend_frame(frame_started: Instant) {
    // Hardware vsync usually consumes this budget; dummy and software backends
    // receive an explicit cap without affecting wall-clock fade calculations.
    if let Some(remaining) = RENDER_INTERVAL.checked_sub(frame_started.elapsed()) {
        std::thread::sleep(remaining);
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
