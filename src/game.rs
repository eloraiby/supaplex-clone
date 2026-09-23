//! Row-major board storage and deterministic application of actor transitions.

use std::{
    error::Error,
    fmt,
    ops::Range,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::actors::{empty::Reservation, murphy::MurphyPhase};

use crate::{
    actors::{
        Actor, Base, Bug, CHAIN_REACTION_FRAMES, Direction, Drawing, Electron, Empty, Exit,
        GameEvent, Hardware, Infotron, InvisibleWall, Murphy, OrangeDisk, Port, PortDirections,
        Position, RED_DISK_DETONATION_COUNTDOWN, RamChip, RamChipShape, RedDisk, SnikSnak, State,
        Terminal, Transition, YellowDisk, Zonk,
    },
    level::{LEVEL_HEIGHT, LEVEL_WIDTH, Level, SpecialPort},
};

/// Player commands sampled for one fixed simulation step.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Input {
    /// Orthogonal direction requested by the arrow keys, if any.
    pub direction: Option<Direction>,
    /// Whether Space is held to snap an adjacent collectible without moving.
    pub action: bool,
}

/// Contiguous two-dimensional storage using only `width * y + x` indexing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Board {
    /// Number of columns represented by each contiguous row.
    width: usize,
    /// Number of rows represented by the cell vector.
    height: usize,
    /// Complete cell states in row-major order.
    cells: Vec<State>,
}

impl Board {
    /// Builds a board after validating dimensions and the exact vector length.
    pub fn new(width: usize, height: usize, cells: Vec<State>) -> Result<Self, BoardError> {
        // Checked multiplication turns unreasonable dimensions into a clear
        // error rather than allowing integer wraparound during index checks.
        let expected = width
            .checked_mul(height)
            .ok_or(BoardError::DimensionsOverflow { width, height })?;
        if cells.len() != expected {
            return Err(BoardError::WrongCellCount {
                expected,
                actual: cells.len(),
            });
        }

        Ok(Self {
            width,
            height,
            cells,
        })
    }

    /// Converts an already validated level's raw tile codes into actor states.
    pub fn from_level(level: &Level) -> Result<Self, BoardError> {
        // Tile conversion deliberately retains RAM-chip and Hardware appearance
        // variants while normalizing their shared collision behavior.
        let cells = level
            .tiles()
            .iter()
            .copied()
            .map(state_from_tile)
            .collect::<Result<Vec<_>, _>>()?;
        let mut board = Self::new(LEVEL_WIDTH, LEVEL_HEIGHT, cells)?;
        board.initialize_loaded_enemies(level.tiles());
        Ok(board)
    }

    /// Applies the original pre-play conversion for serialized enemy tiles.
    fn initialize_loaded_enemies(&mut self, serialized_tiles: &[u8]) {
        // `convertToEasyTiles` scans original tile positions in row-major order
        // and consults the board already changed by earlier enemies. A free cell
        // on the left selects turn state one. Otherwise a free cell above, then
        // a free cell on the right, starts a transfer immediately and replaces
        // the serialized source with an unscheduled `0xffff` reservation.
        for (index, tile) in serialized_tiles.iter().copied().enumerate() {
            if !matches!(tile, 17 | 24) {
                continue;
            }

            let position = self
                .position(index)
                .expect("serialized level tile indices fit the constructed board");
            let left = position
                .x
                .checked_sub(1)
                .map(|x| Position::new(x, position.y));
            let above = position
                .y
                .checked_sub(1)
                .map(|y| Position::new(position.x, y));
            let right = position
                .x
                .checked_add(1)
                .filter(|x| *x < self.width)
                .map(|x| Position::new(x, position.y));

            if left
                .and_then(|neighbor| self.state(neighbor))
                .is_some_and(State::is_empty)
            {
                let state = if tile == 17 {
                    State::loaded_snik_snak_turn(1)
                } else {
                    State::loaded_electron_turn(1)
                };
                self.set(position, state)
                    .expect("serialized enemy position remains in bounds");
                continue;
            }

            let movement = [(above, Direction::Up), (right, Direction::Right)]
                .into_iter()
                .find_map(|(neighbor, direction)| {
                    neighbor
                        .filter(|candidate| self.state(*candidate).is_some_and(State::is_empty))
                        .map(|destination| (destination, direction))
                });

            if let Some((destination, direction)) = movement {
                let (source_state, destination_state) = if tile == 17 {
                    (
                        State::loaded_snik_snak_source(direction),
                        State::loaded_snik_snak_move(direction),
                    )
                } else {
                    (
                        State::loaded_electron_source(direction),
                        State::loaded_electron_move(direction),
                    )
                };
                self.set(position, source_state)
                    .expect("serialized enemy source remains in bounds");
                self.set(destination, destination_state)
                    .expect("validated enemy destination remains in bounds");
            } else {
                let state = if tile == 17 {
                    State::loaded_snik_snak_turn(0)
                } else {
                    State::loaded_electron_turn(0)
                };
                self.set(position, state)
                    .expect("blocked serialized enemy position remains in bounds");
            }
        }
    }

    /// Returns the number of columns in this board.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Returns the number of rows in this board.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Returns every cell in canonical row-major order.
    pub fn cells(&self) -> &[State] {
        &self.cells
    }

    /// Returns the exact linear interval scanned by the DOS moving-object pass.
    fn moving_object_scan_range(&self) -> Range<usize> {
        // The original loop does not skip each row's side cells separately. It
        // starts at linear index `width + 1` and stops before
        // `cell_count - width - 1`. On a conventional bordered level that is
        // effectively the playfield, but malformed edge actors expose the two
        // asymmetric endpoints and several original demos depend on them.
        let start = self.width.saturating_add(1).min(self.cells.len());
        let end = self
            .cells
            .len()
            .saturating_sub(self.width.saturating_add(1));
        start.min(end)..end
    }

    /// Converts a bounded coordinate using the required `width * y + x` rule.
    pub fn index(&self, position: Position) -> Option<usize> {
        // Both axes are checked separately so an oversized `x` cannot alias a
        // valid cell in a later row.
        if position.x >= self.width || position.y >= self.height {
            return None;
        }

        Some(self.width * position.y + position.x)
    }

    /// Converts a valid row-major index back into its `(x, y)` coordinate.
    pub fn position(&self, index: usize) -> Option<Position> {
        if index >= self.cells.len() {
            return None;
        }

        Some(Position::new(index % self.width, index / self.width))
    }

    /// Returns one complete actor/animation state when the coordinate is valid.
    pub fn state(&self, position: Position) -> Option<&State> {
        self.index(position).and_then(|index| self.cells.get(index))
    }

    /// Replaces one cell after converting its coordinate through the sole index path.
    fn set(&mut self, position: Position, state: State) -> Result<(), BoardError> {
        let index = self
            .index(position)
            .ok_or(BoardError::OutOfBounds(position))?;
        self.cells[index] = state;
        Ok(())
    }
}

/// Describes an invalid board shape, coordinate, or serialized tile code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoardError {
    /// `width * height` could not be represented by `usize`.
    DimensionsOverflow {
        /// Requested number of columns.
        width: usize,
        /// Requested number of rows.
        height: usize,
    },
    /// The supplied contiguous vector did not exactly fill the dimensions.
    WrongCellCount {
        /// Required number of states.
        expected: usize,
        /// Supplied number of states.
        actual: usize,
    },
    /// A coordinate used for an internal or public access was outside the board.
    OutOfBounds(Position),
    /// A tile code had no actor mapping.
    UnknownTile(u8),
}

impl fmt::Display for BoardError {
    /// Formats board failures with their dimensions, coordinate, or tile code.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionsOverflow { width, height } => {
                write!(
                    formatter,
                    "board dimensions {width}x{height} overflow usize"
                )
            }
            Self::WrongCellCount { expected, actual } => write!(
                formatter,
                "board requires {expected} cells but received {actual}"
            ),
            Self::OutOfBounds(position) => {
                write!(
                    formatter,
                    "board coordinate ({}, {}) is out of bounds",
                    position.x, position.y
                )
            }
            Self::UnknownTile(tile) => write!(formatter, "tile code {tile} has no actor mapping"),
        }
    }
}

impl Error for BoardError {}

/// Overall outcome of the current play session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameStatus {
    /// Murphy can still move and the simulation continues to tick.
    Playing,
    /// Murphy entered an unlocked Exit.
    Completed,
    /// Murphy was caught in an explosion.
    Dead,
}

/// Original gameplay sound selected by an actor transition.
///
/// Keeping semantic requests in the platform-independent simulation lets tests
/// verify the exact trigger tick without opening an SDL audio device. The SDL
/// front end drains these requests after each batch of fixed simulation steps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundEffect {
    /// A normal or Electron 3x3 explosion wave has begun.
    Explosion,
    /// Murphy has started moving through or snapping up an Infotron.
    Infotron,
    /// A held push has begun, or a planted Red Disk has been committed.
    Push,
    /// A falling Zonk or Infotron has landed on a non-explosive obstacle.
    Fall,
    /// An active Bug has advanced while Murphy occupies a neighboring cell.
    Bug,
    /// Murphy has started eating Base or a currently safe Bug.
    Base,
    /// Murphy has selected an unlocked Exit.
    Exit,
}

/// Concealed portion of the single Red Disk fuse planted beneath Murphy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlantedRedDisk {
    /// Board cell where Murphy initiated the planting action.
    position: Position,
    /// Original countdown byte, beginning at two after placement completes.
    countdown: u8,
}

/// Derives the ordinary-play RNG seed from the current process wall clock.
fn clock_random_seed() -> u16 {
    // Supaplex seeds normal play from the clock and preserves that stream over
    // restarts. Fold both seconds and subsecond time into the original 16 bits;
    // an unavailable pre-epoch duration has a deterministic zero fallback.
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (duration.as_secs() as u16) ^ (duration.subsec_nanos() as u16)
}

/// Complete mutable play session around one row-major board.
#[derive(Clone, Debug)]
pub struct Game {
    /// Current live board mutated by the linear actor pass.
    board: Board,
    /// Scoped output of explicit pictures; ordinary headless ticks retain none.
    drawings: Option<Vec<Drawing>>,
    /// Level title used by the SDL HUD and window title.
    title: String,
    /// Required Infotrons not yet collected.
    remaining_infotrons: u16,
    /// Red Disks currently held by Murphy.
    red_disks: u16,
    /// One position-owned fuse, including while visible or covered by Murphy.
    planted_red_disk: Option<PlantedRedDisk>,
    /// Current player-gravity toggle.
    gravity: bool,
    /// Current falling-Zonk freeze toggle.
    freeze_zonks: bool,
    /// Current enemy freeze toggle.
    freeze_enemies: bool,
    /// Metadata records consulted by special ports.
    special_ports: Vec<SpecialPort>,
    /// Current terminal session outcome.
    status: GameStatus,
    /// Number of fixed simulation steps processed for this session.
    tick: u64,
    /// Shared wrapping 16-bit random stream used for per-Bug cooldowns.
    random_seed: u16,
    /// Mask applied to randomized Terminal delays before and after activation.
    terminal_delay_mask: u8,
    /// Signed secondary-wave timers stored independently from explosion graphics.
    explosion_timers: Vec<i8>,
    /// Original global flag controlling one shared random camera-shake draw.
    explosion_started: bool,
    /// Remaining updates in the original post-death or post-completion sequence.
    quit_countdown: u8,
    /// Sound requests waiting for the SDL front end to drain and play them.
    pending_sound_effects: Vec<SoundEffect>,
}

impl Game {
    /// Creates a play session from a decoded original level.
    pub fn new(level: &Level) -> Result<Self, GameError> {
        Self::with_random_seed(level, clock_random_seed())
    }

    /// Creates a play session with an explicit stream seed for tests/replays.
    pub fn with_random_seed(level: &Level, random_seed: u16) -> Result<Self, GameError> {
        // Replays must supply the seed recorded or implied by their format so
        // randomized Bug wakeups remain deterministic across every playback.
        let board = Board::from_level(level)?;
        let murphy_count = board
            .cells()
            .iter()
            .filter(|state| matches!(state.actor(), Actor::Murphy(_)))
            .count();
        if murphy_count != 1 {
            return Err(GameError::InvalidMurphyCount(murphy_count));
        }

        let available_infotrons = board
            .cells()
            .iter()
            .filter(|state| matches!(state.actor(), Actor::Infotron(_)))
            .count();
        let available_infotrons = u16::try_from(available_infotrons)
            .expect("a 60x24 board's Infotron count always fits in u16");

        // A stored zero is the format's sentinel for "all Infotrons present".
        let remaining_infotrons = if level.required_infotrons() == 0 {
            available_infotrons
        } else {
            u16::from(level.required_infotrons())
        };

        let explosion_timers = vec![0; board.cells().len()];

        Ok(Self {
            board,
            drawings: None,
            title: level.title().to_owned(),
            remaining_infotrons,
            red_disks: 0,
            planted_red_disk: None,
            gravity: level.gravity(),
            freeze_zonks: level.freeze_zonks(),
            freeze_enemies: false,
            special_ports: level.special_ports().to_vec(),
            status: GameStatus::Playing,
            tick: 0,
            random_seed,
            terminal_delay_mask: 0x7f,
            explosion_timers,
            explosion_started: false,
            quit_countdown: 0,
            pending_sound_effects: Vec::new(),
        })
    }

    /// Restarts a level while retaining the process-wide random stream state.
    pub fn restart(&mut self, level: &Level) -> Result<(), GameError> {
        let random_seed = self.random_seed;
        *self = Self::with_random_seed(level, random_seed)?;
        Ok(())
    }

    /// Returns an immutable reference to the current live row-major board.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Returns the level title embedded in its original record.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the number of additional Infotrons required to unlock the Exit.
    pub fn remaining_infotrons(&self) -> u16 {
        self.remaining_infotrons
    }

    /// Returns the number of collected Red Disks available to drop.
    pub fn red_disks(&self) -> u16 {
        self.red_disks
    }

    /// Returns the current gravity setting, including special-port changes.
    pub fn gravity(&self) -> bool {
        self.gravity
    }

    /// Returns the current Zonk-freeze setting, including special-port changes.
    pub fn freeze_zonks(&self) -> bool {
        self.freeze_zonks
    }

    /// Returns the current enemy-freeze setting, including special-port changes.
    pub fn freeze_enemies(&self) -> bool {
        self.freeze_enemies
    }

    /// Returns whether the session is active, completed, or dead.
    pub fn status(&self) -> GameStatus {
        self.status
    }

    /// Reports whether a terminal level has completed its original exit delay.
    pub fn terminal_transition_ready(&self) -> bool {
        // Completed and dead levels continue non-player simulation for 0x40
        // updates. The front end may leave only after that shared countdown is
        // exhausted, so exit and explosion animations are never cut short.
        self.status != GameStatus::Playing && self.quit_countdown == 0
    }

    /// Returns the number of fixed simulation steps processed.
    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Removes and returns every sound requested since the preceding drain.
    pub fn take_sound_effects(&mut self) -> Vec<SoundEffect> {
        // Taking the vector guarantees that an effect is presented exactly
        // once even when rendering runs faster than the fixed simulation.
        std::mem::take(&mut self.pending_sound_effects)
    }

    /// Finds Murphy's current logical destination cell, including during movement.
    pub fn murphy_position(&self) -> Option<Position> {
        self.board
            .cells()
            .iter()
            .enumerate()
            .find_map(|(index, state)| {
                matches!(state.actor(), Actor::Murphy(_))
                    .then(|| self.board.position(index))
                    .flatten()
            })
    }

    /// Destroys a living Murphy through the ordinary gameplay explosion path.
    pub fn destroy_murphy(&mut self) {
        // Escape is a play-session command, not a shortcut around terminal
        // states. Ignore repeated requests after death and requests made while
        // the successful Exit sequence is already controlling the session.
        if self.status != GameStatus::Playing {
            return;
        }

        // Murphy can be logically stored at a movement destination while his
        // sprite is still interpolating toward it. The original kill request
        // likewise explodes that current logical location on the next update.
        let Some(position) = self.murphy_position() else {
            return;
        };

        // Reuse the normal bounded 3x3 wave so destructible neighbors, chain
        // timers, the explosion sound, death status, and exit delay all follow
        // exactly the same rules as an enemy or falling-object collision.
        self.detonate_position(position);
    }

    /// Advances one tick and returns explicit pictures in callback execution order.
    pub fn tick_with_drawings(&mut self, input: Input) -> Vec<Drawing> {
        // Execute exactly the ordinary simulation, retaining its drawing effects
        // for the caller instead of inferring them from changed board cells.
        self.record_drawings(|game| game.tick(input))
    }

    /// Records an explicit death request's immediate blast pictures without a tick.
    pub fn destroy_murphy_with_drawings(&mut self) -> Vec<Drawing> {
        self.record_drawings(Self::destroy_murphy)
    }

    /// Scopes presentation output so unused histories cannot grow between ticks.
    fn record_drawings(&mut self, operation: impl FnOnce(&mut Self)) -> Vec<Drawing> {
        debug_assert!(self.drawings.is_none(), "recording scopes cannot nest");
        self.drawings = Some(Vec::new());
        operation(self);
        self.drawings
            .take()
            .expect("recording scope remains active")
    }

    /// Appends a session-owned picture at the exact point its event occurs.
    fn draw_actor(&mut self, position: Position, actor: Actor) {
        if let Some(drawings) = &mut self.drawings {
            drawings.push(Drawing { position, actor });
        }
    }

    /// Applies one Murphy-first, row-major simulation step to the live board.
    pub fn tick(&mut self, input: Input) {
        // A terminal status becomes final only after the original countdown.
        // Until then, every non-Murphy updater, fuse, and timer remains live.
        if self.status != GameStatus::Playing && self.quit_countdown == 0 {
            return;
        }

        let playing = self.status == GameStatus::Playing;
        let simulating = playing || self.quit_countdown > 0;
        let murphy_source = self.murphy_position();
        let exit_is_disappearing = self.status == GameStatus::Completed
            && murphy_source
                .and_then(|position| self.board.state(position))
                .is_some_and(|state| {
                    matches!(
                        state.actor(),
                        Actor::Murphy(actor) if matches!(actor.phase(), MurphyPhase::Exiting(_))
                    )
                });

        // Supaplex always updates Murphy before constructing the linear moving-
        // object schedule. Every later actor therefore sees his completed move,
        // turn, source reservation, or interaction from this same tick. Once
        // Exit contact records success, only that terminal animation continues;
        // ordinary player input must not start another action.
        if (playing || exit_is_disappearing)
            && let Some(position) = murphy_source
            && let Some(transition) =
                self.transition_at(position, if playing { input } else { Input::default() })
        {
            self.apply_transition(transition);
        }

        // Capture updater identity once from the live post-Murphy board. This
        // deliberately includes actors Murphy pushed and explosions Murphy
        // created: the original linear scan sees those new tile kinds too. The
        // sole synthetic cell omitted is Murphy's newly Vacating source because
        // original Space tiles do not receive moving-object callbacks. Preserve
        // the DOS loop's asymmetric linear bounds rather than normalizing them
        // to a geometric inner rectangle; edge-case demos rely on that detail.
        let scan_range = self.board.moving_object_scan_range();
        let schedule = self.board.cells()[scan_range.clone()]
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                let index = scan_range.start + index;
                let position = self
                    .board
                    .position(index)
                    .expect("enumerated board indices are always valid");
                let is_new_murphy_source = Some(position) == murphy_source
                    && matches!(state.reservation(), Some(Reservation::Vacating { .. }));
                (!is_new_murphy_source && !matches!(state.actor(), Actor::Murphy(_)) && simulating)
                    .then(|| (position, std::mem::discriminant(state.actor())))
            })
            .collect::<Vec<_>>();

        for (position, expected_actor) in schedule {
            let Some(state) = self.board.state(position) else {
                continue;
            };
            if std::mem::discriminant(state.actor()) != expected_actor {
                continue;
            }
            if let Some(transition) = self.transition_at(position, Input::default()) {
                self.apply_transition(transition);
            }
        }

        if simulating {
            self.advance_planted_red_disk();
            self.advance_explosion_timers();
            self.consume_explosion_random_value();
        }
        if self.quit_countdown > 0 {
            self.quit_countdown -= 1;
        }
        self.tick = self.tick.saturating_add(1);
    }

    /// Evaluates one scheduled actor against the board left by earlier actors.
    fn transition_at(&self, position: Position, input: Input) -> Option<Transition> {
        let state = self.board.state(position)?.clone();
        let world = WorldView::new(
            &self.board,
            input,
            self.remaining_infotrons,
            self.red_disks,
            self.planted_red_disk.map(|disk| disk.position),
            self.gravity,
            self.freeze_zonks,
            self.freeze_enemies,
            self.tick,
            &self.special_ports,
        );
        state.actor().transition(position, &world)
    }

    /// Commits every write, then applies events before the next scheduled cell.
    fn apply_transition(&mut self, transition: Transition) {
        // Actor helpers construct only bounded coordinates. Keeping writes in
        // one value ensures no other actor can observe a partially moved actor.
        // An action can draw its last picture and complete in this callback.
        // Retain that effect before its writes replace the stored actor phase.
        if let Some(drawings) = &mut self.drawings {
            drawings.extend(transition.drawings);
        }
        for write in transition.writes {
            self.board
                .set(write.position, write.state)
                .expect("actor transition emitted an in-bounds position");
        }
        for event in transition.events {
            self.apply_event(event);
        }
    }

    /// Applies a gameplay side effect emitted by an immediate transition.
    fn apply_event(&mut self, event: GameEvent) {
        match event {
            GameEvent::CollectInfotron => {
                self.remaining_infotrons = self.remaining_infotrons.saturating_sub(1);
            }
            GameEvent::CollectRedDisk => {
                self.red_disks = self.red_disks.saturating_add(1);
            }
            GameEvent::BeginPlantRedDisk(position) => {
                // Countdown one is the cancellable, unspent placement state.
                // The actor retains it only while Space remains held.
                if self.red_disks > 0 && self.planted_red_disk.is_none() {
                    self.planted_red_disk = Some(PlantedRedDisk {
                        position,
                        countdown: 1,
                    });
                }
            }
            GameEvent::CancelPlantRedDisk => {
                // A completed fuse can no longer be cancelled by player input.
                if self
                    .planted_red_disk
                    .is_some_and(|disk| disk.countdown <= 1)
                {
                    self.planted_red_disk = None;
                }
            }
            GameEvent::FinishPlantRedDisk => {
                if let Some(disk) = self.planted_red_disk.as_mut()
                    && disk.countdown <= 1
                    && self.red_disks > 0
                {
                    self.red_disks -= 1;
                    disk.countdown = 2;
                    self.pending_sound_effects.push(SoundEffect::Push);
                }
            }
            GameEvent::Completed => {
                self.status = GameStatus::Completed;
                if self.quit_countdown == 0 {
                    self.quit_countdown = 0x40;
                }
            }
            GameEvent::Died => {
                self.status = GameStatus::Dead;
                if self.quit_countdown == 0 {
                    self.quit_countdown = 0x40;
                }
            }
            GameEvent::ApplySpecialPort(port) => {
                self.gravity = port.gravity;
                self.freeze_zonks = port.freeze_zonks;
                self.freeze_enemies = port.freeze_enemies;
            }
            GameEvent::ActivateTerminal => self.detonate_yellow_disks(),
            GameEvent::RandomizeBug(position) => {
                // Same-tick active cycles consume consecutive values because
                // events are applied immediately in row-major actor order.
                let delay = 32 + ((self.next_random() as u8) & 0x3f);
                if self
                    .board
                    .state(position)
                    .is_some_and(|state| matches!(state.actor(), Actor::Bug(_)))
                {
                    let state = State::dormant_bug(delay);
                    self.draw_actor(position, state.actor().clone());
                    self.board
                        .set(position, state)
                        .expect("scheduled Bug position remains in bounds");
                }
            }
            GameEvent::RandomizeTerminal(position) => {
                // Terminal scrolling shares the same generator as Bugs and all
                // other original random effects.  The low byte is masked, then
                // stored as a negative signed delay so subsequent updates count
                // it back toward zero without consuming more random values.
                let delay = -i8::try_from((self.next_random() as u8) & self.terminal_delay_mask)
                    .expect("Terminal delay masks never exceed signed-byte range");
                let Some(state) = self.board.state(position) else {
                    return;
                };
                let Actor::Terminal(terminal) = state.actor() else {
                    return;
                };
                let actor = Actor::Terminal(terminal.after_scroll(delay));
                self.draw_actor(position, actor.clone());
                self.board
                    .set(position, State::new(actor))
                    .expect("scheduled Terminal position remains in bounds");
            }
            GameEvent::ScheduleExplosion { position, electron } => {
                // Timer sign carries the future wave type exactly as the DOS
                // byte array did: positive is regular, negative is Electron.
                if let Some(index) = self.board.index(position) {
                    let delay = CHAIN_REACTION_FRAMES as i8;
                    self.explosion_timers[index] = if electron { -delay } else { delay };
                }
            }
            GameEvent::ExplosionStarted => self.explosion_started = true,
            GameEvent::ExplosionFinished => self.explosion_started = false,
            GameEvent::PlaySound(effect) => self.pending_sound_effects.push(effect),
        }
    }

    /// Advances the original wrapping 16-bit generator and returns `seed / 2`.
    fn next_random(&mut self) -> u16 {
        self.random_seed = self.random_seed.wrapping_mul(1509).wrapping_add(49);
        self.random_seed / 2
    }

    /// Expands every idle Yellow Disk into a normal 3×3 explosion immediately.
    fn detonate_yellow_disks(&mut self) {
        // Activating any panel shortens every panel's future randomized wait
        // from a 0x7f mask to a 0x07 mask.  This global timing change is part of
        // the original effect and also changes when the shared RNG is consumed.
        self.terminal_delay_mask = 7;

        // The detonation latch is level-wide.  Preserve each panel's independent
        // signed counter and screen frame while marking every panel as consumed.
        let terminal_positions = self
            .board
            .cells()
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                matches!(state.actor(), Actor::Terminal(_))
                    .then(|| self.board.position(index))
                    .flatten()
            })
            .collect::<Vec<_>>();
        for position in terminal_positions {
            let terminal = match self.board.state(position).map(State::actor) {
                Some(Actor::Terminal(terminal)) => terminal.activate(),
                _ => continue,
            };
            self.board
                .set(position, State::new(Actor::Terminal(terminal)))
                .expect("enumerated Terminal position must remain in bounds");
        }

        // Scan the live board once in row-major order. An earlier Yellow blast
        // may turn a later adjacent Yellow Disk into a delayed chain before its
        // index is reached; that later cell must no longer detonate directly.
        for index in 0..self.board.cells().len() {
            let position = self
                .board
                .position(index)
                .expect("linear Yellow Disk scan remains in bounds");
            let should_detonate = self.board.state(position).is_some_and(|state| {
                state.is_idle() && matches!(state.actor(), Actor::YellowDisk(_))
            });
            if should_detonate {
                self.detonate_position(position);
            }
        }
    }

    /// Advances the position-owned fuse and reflects its visible/concealed state.
    fn advance_planted_red_disk(&mut self) {
        let Some(mut planted) = self.planted_red_disk.take() else {
            return;
        };
        // Values zero and one are the unplanted/arming states. A completely
        // placed disk starts at two and increments once per gameplay iteration.
        if planted.countdown <= 1 {
            self.planted_red_disk = Some(planted);
            return;
        }
        planted.countdown = planted.countdown.saturating_add(1);

        // The concealed fuse keeps running under Murphy. Reaching 0x28 detonates
        // the stored position regardless of which actor currently covers it.
        if planted.countdown >= RED_DISK_DETONATION_COUNTDOWN {
            self.detonate_position(planted.position);
            return;
        }

        let Some(state) = self.board.state(planted.position) else {
            return;
        };
        match state.actor() {
            Actor::Murphy(_) => self.planted_red_disk = Some(planted),
            Actor::Empty(Empty::Reserved(Reservation::Vacating { .. })) => {
                // Murphy's just-vacated source remains collision-reserved until
                // his movement finishes, so the concealed fuse stays hidden.
                self.planted_red_disk = Some(planted);
            }
            Actor::Empty(Empty::Space) | Actor::RedDisk(RedDisk::Planted(_)) => {
                // A visible State exposes the current animation frame, while
                // this retained record lets Murphy cross without losing time.
                let state = State::planted_red_disk(planted.countdown);
                self.draw_actor(planted.position, state.actor().clone());
                self.board
                    .set(planted.position, state)
                    .expect("stored planted-disk position must remain in bounds");
                self.planted_red_disk = Some(planted);
            }
            // Another actor or blast already consumed the concealed disk.
            _ => {}
        }
    }

    /// Advances every independent signed chain timer in row-major order.
    fn advance_explosion_timers(&mut self) {
        // A wave emitted by an earlier index can install a timer at a later
        // index, which is then decremented in this same pass.  Iterating the live
        // vector directly preserves that subtle original ordering behavior.
        for index in 0..self.explosion_timers.len() {
            let timer = self.explosion_timers[index];
            if timer == 0 {
                continue;
            }

            let next = if timer < 0 { timer + 1 } else { timer - 1 };
            self.explosion_timers[index] = next;
            if next != 0 {
                continue;
            }

            let position = self
                .board
                .position(index)
                .expect("explosion timer indices share the board dimensions");
            let transition = self.explosion_transition(position, timer < 0);
            self.apply_transition(transition);
        }
    }

    /// Consumes the original shared random draw while explosion shake is active.
    fn consume_explosion_random_value(&mut self) {
        // The generated value is presentation-only, but advancing this exact
        // stream is gameplay-significant because Terminals and Bugs use it too.
        if self.explosion_started {
            let _ = self.next_random();
        }
    }

    /// Detonates one position against the current live board immediately.
    fn detonate_position(&mut self, position: Position) {
        let transition = self.explosion_transition(position, false);
        self.apply_transition(transition);
    }

    /// Builds one blast from the state left by all earlier linear transitions.
    fn explosion_transition(&self, position: Position, electron: bool) -> Transition {
        let world = WorldView::new(
            &self.board,
            Input::default(),
            self.remaining_infotrons,
            self.red_disks,
            self.planted_red_disk.map(|disk| disk.position),
            self.gravity,
            self.freeze_zonks,
            self.freeze_enemies,
            self.tick,
            &self.special_ports,
        );
        crate::actors::explode_at(&world, position, electron)
    }
}

/// Describes why a decoded level could not start a valid play session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GameError {
    /// Board construction failed because of malformed dimensions or a tile code.
    Board(BoardError),
    /// Play requires exactly one Murphy start tile.
    InvalidMurphyCount(usize),
}

impl fmt::Display for GameError {
    /// Formats the underlying board issue or invalid Murphy count.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(error) => write!(formatter, "could not create board: {error}"),
            Self::InvalidMurphyCount(count) => {
                write!(
                    formatter,
                    "level contains {count} Murphy actors; expected exactly one"
                )
            }
        }
    }
}

impl Error for GameError {}

impl From<BoardError> for GameError {
    /// Preserves board-construction detail while converting to a game error.
    fn from(error: BoardError) -> Self {
        Self::Board(error)
    }
}

/// Immutable view of the live board exposed during one actor's linear update.
pub(crate) struct WorldView<'board> {
    /// Board state left by Murphy and all earlier scheduled actors this tick.
    board: &'board Board,
    /// Player command sampled for this simulation step.
    input: Input,
    /// Collectible requirement after all earlier events in this linear step.
    remaining_infotrons: u16,
    /// Red Disk inventory after all earlier events in this linear step.
    red_disks: u16,
    /// Position owning the single concealed or visible planted Red Disk fuse.
    active_red_disk_position: Option<Position>,
    /// Current player-gravity toggle after earlier events in this tick.
    gravity: bool,
    /// Current Zonk-freeze toggle after earlier events in this tick.
    freeze_zonks: bool,
    /// Current enemy-freeze toggle after earlier special-port events.
    freeze_enemies: bool,
    /// Global fixed-step counter used by cadence-gated actors such as Bugs.
    tick_count: u64,
    /// Special-port metadata attached to this level.
    special_ports: &'board [SpecialPort],
}

impl<'board> WorldView<'board> {
    /// Creates one immutable context shared by all actor transition methods.
    #[allow(clippy::too_many_arguments)]
    fn new(
        board: &'board Board,
        input: Input,
        remaining_infotrons: u16,
        red_disks: u16,
        active_red_disk_position: Option<Position>,
        gravity: bool,
        freeze_zonks: bool,
        freeze_enemies: bool,
        tick_count: u64,
        special_ports: &'board [SpecialPort],
    ) -> Self {
        Self {
            board,
            input,
            remaining_infotrons,
            red_disks,
            active_red_disk_position,
            gravity,
            freeze_zonks,
            freeze_enemies,
            tick_count,
            special_ports,
        }
    }

    /// Returns the complete state at a bounded position.
    pub(crate) fn state(&self, position: Position) -> Option<&State> {
        self.board.state(position)
    }

    /// Returns the current player command.
    pub(crate) fn input(&self) -> Input {
        self.input
    }

    /// Returns the collectible requirement visible to actors this step.
    pub(crate) fn remaining_infotrons(&self) -> u16 {
        self.remaining_infotrons
    }

    /// Returns the Red Disk inventory visible to actors this step.
    pub(crate) fn red_disks(&self) -> u16 {
        self.red_disks
    }

    /// Reports whether the level already has its one permitted planted fuse.
    pub(crate) fn has_active_red_disk(&self) -> bool {
        self.active_red_disk_position.is_some()
    }

    /// Reports whether one cell owns the active fuse and may conceal it again.
    pub(crate) fn is_active_red_disk(&self, position: Position) -> bool {
        self.active_red_disk_position == Some(position)
    }

    /// Returns whether Murphy gravity is currently enabled.
    pub(crate) fn gravity(&self) -> bool {
        self.gravity
    }

    /// Returns whether falling Zonks are currently frozen.
    pub(crate) fn freeze_zonks(&self) -> bool {
        self.freeze_zonks
    }

    /// Returns whether wall-following enemies are currently frozen.
    pub(crate) fn freeze_enemies(&self) -> bool {
        self.freeze_enemies
    }

    /// Returns the global fixed-step counter before this tick is completed.
    pub(crate) fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Returns whether one bounded position contains stable empty space.
    pub(crate) fn is_empty(&self, position: Position) -> bool {
        self.state(position).is_some_and(State::is_empty)
    }

    /// Offsets a position by one orthogonal direction with checked boundaries.
    pub(crate) fn offset(&self, position: Position, direction: Direction) -> Option<Position> {
        let (delta_x, delta_y) = match direction {
            Direction::Up => (0, -1),
            Direction::Right => (1, 0),
            Direction::Down => (0, 1),
            Direction::Left => (-1, 0),
        };
        self.offset_xy(position, delta_x, delta_y)
    }

    /// Offsets a position by signed deltas and rejects underflow or board escape.
    pub(crate) fn offset_xy(
        &self,
        position: Position,
        delta_x: isize,
        delta_y: isize,
    ) -> Option<Position> {
        let x = position.x.checked_add_signed(delta_x)?;
        let y = position.y.checked_add_signed(delta_y)?;
        let result = Position::new(x, y);
        self.board.index(result).map(|_| result)
    }

    /// Reports whether a stable rounded actor supports a possible side roll.
    pub(crate) fn is_rounded_stable_support(&self, position: Position) -> bool {
        self.state(position).is_some_and(|state| {
            state.is_idle()
                && matches!(
                    state.actor(),
                    Actor::Zonk(_) | Actor::Infotron(_) | Actor::RamChip(_)
                )
        })
    }

    /// Reports whether a Bug's current animation frame is dangerous to Murphy.
    pub(crate) fn is_bug_active(&self, position: Position) -> bool {
        self.state(position)
            .is_some_and(|state| matches!(state.actor(), Actor::Bug(Bug::Active(_))))
    }

    /// Reports whether any of the eight surrounding cells currently holds Murphy.
    pub(crate) fn has_neighboring_murphy(&self, position: Position) -> bool {
        // Bug sound uses the original 3x3 neighborhood rather than only the
        // four directions Murphy can enter from, so diagonal proximity counts.
        (-1_isize..=1).any(|delta_y| {
            (-1_isize..=1).any(|delta_x| {
                (delta_x != 0 || delta_y != 0)
                    && self
                        .offset_xy(position, delta_x, delta_y)
                        .and_then(|neighbor| self.state(neighbor))
                        .is_some_and(|state| matches!(state.actor(), Actor::Murphy(_)))
            })
        })
    }

    /// Finds metadata for the special port occupying one exact board coordinate.
    pub(crate) fn special_port(&self, position: Position) -> Option<&SpecialPort> {
        self.special_ports
            .iter()
            .find(|port| port.x == position.x && port.y == position.y)
    }
}

/// Maps one serialized level byte to a concrete actor in its initial legal phase.
fn state_from_tile(tile: u8) -> Result<State, BoardError> {
    let actor = match tile {
        0 => Actor::Empty(Empty::Space),
        1 => Actor::Zonk(Zonk::resting()),
        2 => Actor::Base(Base::Resting),
        3 => Actor::Murphy(Murphy::new()),
        4 => Actor::Infotron(Infotron::resting()),
        5 => Actor::RamChip(RamChip::new(RamChipShape::Center)),
        6 => Actor::Hardware(Hardware::new(0)),
        7 => Actor::Exit(Exit),
        8 => Actor::OrangeDisk(OrangeDisk::resting()),
        9 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Right), false)),
        10 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Down), false)),
        11 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Left), false)),
        12 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Up), false)),
        13 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Right), true)),
        14 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Down), true)),
        15 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Left), true)),
        16 => Actor::Port(Port::new(PortDirections::OneWay(Direction::Up), true)),
        // Heading Right makes the left-hand wall-following preference test Up
        // first, matching the original serialized state-zero Snik Snak.
        17 => Actor::SnikSnak(SnikSnak::new(Direction::Right)),
        18 => Actor::YellowDisk(YellowDisk::Resting),
        19 => Actor::Terminal(Terminal::new()),
        20 => Actor::RedDisk(RedDisk::Collectible),
        21 => Actor::Port(Port::new(PortDirections::Vertical, false)),
        22 => Actor::Port(Port::new(PortDirections::Horizontal, false)),
        23 => Actor::Port(Port::new(PortDirections::Any, false)),
        // Electrons begin in the same original state-zero left-turn cycle as
        // Snik Snaks; a rightward prior heading makes its first candidate Up.
        24 => Actor::Electron(Electron::new(Direction::Right)),
        25 => Actor::Bug(Bug::new()),
        26 => Actor::RamChip(RamChip::new(RamChipShape::Left)),
        27 => Actor::RamChip(RamChip::new(RamChipShape::Right)),
        28..=37 => Actor::Hardware(Hardware::new(tile - 27)),
        38 => Actor::RamChip(RamChip::new(RamChipShape::Top)),
        39 => Actor::RamChip(RamChip::new(RamChipShape::Bottom)),
        40 => Actor::InvisibleWall(InvisibleWall),
        _ => return Err(BoardError::UnknownTile(tile)),
    };

    Ok(State::new(actor))
}

#[cfg(test)]
mod tests {
    //! Focused simulations proving indexing, animation occupancy, and mechanics.

    mod snapshots {
        use crate::actors as model;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/legacy_snapshot.rs"
        ));
    }
    use snapshots::SnapshotExt;

    use super::{Board, Game, GameStatus, Input, PlantedRedDisk, SoundEffect};
    use crate::actors::{
        Actor, Base, Bug, CHAIN_REACTION_FRAMES, Direction, Electron, Empty, EnemyTurn, Exit,
        Explosion, ExplosionResidue, Hardware, Infotron, InvisibleWall, Murphy, MurphyAnimation,
        OrangeDisk, Port, PortDirections, Position, RedDisk, SnikSnak, State, Terminal, YellowDisk,
        Zonk,
    };
    use crate::level::{LEVEL_RECORD_SIZE, LEVEL_WIDTH, LevelSet, SpecialPort};

    /// Original level-set bytes used for end-to-end initialization checks.
    const ORIGINAL_LEVELS: &[u8] = include_bytes!("../assets/data/levels.dat");

    /// First legacy attract-mode stream, verified as successful by OpenSupaplex.
    const ORIGINAL_DEMO_ZERO: &[u8] = include_bytes!("../assets/data/demo0.bin");

    /// Creates a compact bordered game with mutable private fields for tests.
    fn game_with(placements: &[(Position, State)], required_infotrons: u16) -> Game {
        let width = 7;
        let height = 6;
        let mut cells = vec![State::empty(); width * height];

        // An indestructible border mirrors the invariant of all bundled levels.
        for y in 0..height {
            for x in 0..width {
                if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                    cells[width * y + x] = State::new(Actor::Hardware(Hardware::new(0)));
                }
            }
        }
        for (position, state) in placements {
            cells[width * position.y + position.x] = state.clone();
        }

        Game {
            board: Board::new(width, height, cells).expect("fixture dimensions should match"),
            drawings: None,
            title: "TEST".to_owned(),
            remaining_infotrons: required_infotrons,
            red_disks: 0,
            planted_red_disk: None,
            gravity: false,
            freeze_zonks: false,
            freeze_enemies: false,
            special_ports: Vec::new(),
            status: GameStatus::Playing,
            tick: 0,
            random_seed: 0,
            terminal_delay_mask: 0x7f,
            explosion_timers: vec![0; width * height],
            explosion_started: false,
            quit_countdown: 0,
            pending_sound_effects: Vec::new(),
        }
    }

    /// Board writes are silent; immediate events explicitly emit their pictures.
    #[test]
    fn drawing_scopes_record_effects_without_interpreting_cell_mutations() {
        let position = Position::new(2, 2);
        let mut game = game_with(&[(position, State::new(Actor::Bug(Bug::new())))], 0);
        let drawings = game.record_drawings(|game| {
            game.board.set(position, State::dormant_bug(1)).unwrap();
            game.apply_event(crate::actors::GameEvent::RandomizeBug(position));
        });
        assert_eq!(drawings.len(), 1);
        assert_eq!(drawings[0].position, position);
        assert_eq!(
            &drawings[0].actor,
            game.board.state(position).unwrap().actor()
        );
        assert!(game.drawings.is_none());
        assert!(game.record_drawings(|_| {}).is_empty());
        game.tick(Input::default());
        assert!(game.drawings.is_none());
    }

    /// Murphy's callback precedes even an actor at a smaller linear board index.
    #[test]
    fn drawings_preserve_callback_order_even_across_board_indices() {
        let rock = Position::new(2, 2);
        let murphy = Position::new(4, 3);
        let mut game = game_with(
            &[
                (
                    rock,
                    State::new(Actor::Zonk(Zonk::from_phase(
                        crate::actors::rounded::RoundedPhase::Falling(crate::actors::Frame::first()),
                    ))),
                ),
                (murphy, State::new(Actor::Murphy(Murphy::new()))),
            ],
            0,
        );
        let drawings = game.tick_with_drawings(Input {
            direction: Some(Direction::Right),
            action: false,
        });
        let player_picture = drawings
            .iter()
            .position(|drawing| matches!(drawing.actor, Actor::Murphy(_)))
            .unwrap();
        let rock_picture = drawings
            .iter()
            .position(|drawing| matches!(drawing.actor, Actor::Zonk(_)))
            .unwrap();
        assert!(player_picture < rock_picture);
        assert!(
            game.board.index(drawings[player_picture].position)
                > game.board.index(drawings[rock_picture].position)
        );
    }

    /// Escape's immediate blast is observable even when no fixed tick is due.
    #[test]
    fn explicit_death_records_all_blast_cells_without_advancing_time() {
        let mut game = game_with(
            &[(
                Position::new(3, 3),
                State::new(Actor::Murphy(Murphy::new())),
            )],
            0,
        );
        let drawings = game.destroy_murphy_with_drawings();
        assert_eq!(game.tick_count(), 0);
        assert_eq!(game.status(), GameStatus::Dead);
        assert_eq!(drawings.len(), 9);
        assert!(
            drawings
                .iter()
                .all(|drawing| matches!(drawing.actor, Actor::Explosion(_)))
        );
        assert!(game.destroy_murphy_with_drawings().is_empty());
        assert!(game.drawings.is_none());
    }

    /// A blast clears a rolling source outside its wave without clearing its own blast cells.
    #[test]
    fn blasts_release_both_rounded_roll_reservations_with_explicit_cleanup() {
        use crate::actors::{Frame, Horizontal, empty::Reservation, rounded::RoundedPhase};
        for direction in [Horizontal::Left, Horizontal::Right] {
            for (frame, reservation) in [
                (2, Reservation::RollingSource(direction)),
                (4, Reservation::RoundedCorner(direction)),
            ] {
                for infotron in [false, true] {
                    let phase = RoundedPhase::Rolling {
                        direction,
                        frame: Frame::new(frame).unwrap(),
                    };
                    let actor = if infotron {
                        Actor::Infotron(Infotron::from_phase(phase))
                    } else {
                        Actor::Zonk(Zonk::from_phase(phase))
                    };
                    let (source, center) = match direction {
                        Horizontal::Left => (Position::new(4, 3), Position::new(2, 3)),
                        Horizontal::Right => (Position::new(2, 3), Position::new(4, 3)),
                    };
                    let destination = Position::new(3, 4);
                    let mut game = game_with(
                        &[
                            (
                                Position::new(1, 1),
                                State::new(Actor::Murphy(Murphy::new())),
                            ),
                            (Position::new(3, 3), State::new(actor)),
                            (
                                source,
                                State::new(Actor::Empty(Empty::Reserved(reservation))),
                            ),
                            (
                                destination,
                                State::new(Actor::Empty(Empty::Reserved(
                                    Reservation::RoundedDestination,
                                ))),
                            ),
                        ],
                        0,
                    );
                    let drawings = game.record_drawings(|game| game.detonate_position(center));
                    assert!(game.board().state(source).unwrap().is_empty());
                    assert!(drawings.iter().any(|drawing| drawing.position == source
                        && matches!(drawing.actor, Actor::Empty(Empty::Space))));
                    assert!(matches!(
                        game.board().state(destination).unwrap().actor(),
                        Actor::Explosion(_)
                    ));
                    assert!(
                        !drawings
                            .iter()
                            .any(|drawing| drawing.position == destination
                                && matches!(drawing.actor, Actor::Empty(_)))
                    );
                }
            }
        }
    }

    /// Returns the actor at a required fixture coordinate.
    fn actor_at(game: &Game, x: usize, y: usize) -> &Actor {
        game.board()
            .state(Position::new(x, y))
            .expect("fixture coordinate should be in bounds")
            .actor()
    }

    /// Builds one original-size board around explicitly supplied raw tile IDs.
    fn loaded_board_with_tiles(placements: &[(Position, u8)]) -> Board {
        let mut record = [0_u8; LEVEL_RECORD_SIZE];

        // Hardware provides a non-empty default around each fixture. Metadata
        // remains zeroed so no unrelated port or gravity rule affects loading.
        record[..crate::level::TILE_COUNT].fill(6);
        for (position, tile) in placements {
            let index = LEVEL_WIDTH * position.y + position.x;
            record[index] = *tile;
        }

        let level = LevelSet::new(&record)
            .load(1)
            .expect("the synthetic original-size level record should parse");
        Board::from_level(&level).expect("all synthetic tile identifiers should be supported")
    }

    /// Decodes one legacy demo command's low nibble into sampled player input.
    fn original_demo_input(command: u8) -> Input {
        // Commands one through four follow Up, Left, Down, Right ordering.
        // Adding four combines the same direction with Space, while nine is
        // Space alone. Values ten through fifteen never occur in valid demos.
        let (direction, action) = match command {
            0 => (None, false),
            1 => (Some(Direction::Up), false),
            2 => (Some(Direction::Left), false),
            3 => (Some(Direction::Down), false),
            4 => (Some(Direction::Right), false),
            5 => (Some(Direction::Up), true),
            6 => (Some(Direction::Left), true),
            7 => (Some(Direction::Down), true),
            8 => (Some(Direction::Right), true),
            9 => (None, true),
            _ => panic!("invalid original demo command {command:#x}"),
        };
        Input { direction, action }
    }

    /// Confirms all indexing routes implement exactly `width * y + x`.
    #[test]
    fn board_is_a_single_row_major_vector() {
        let cells = (0..6)
            .map(|_| State::new(Actor::Base(Base::Resting)))
            .collect::<Vec<_>>();
        let board = Board::new(3, 2, cells).expect("shape should be valid");

        let position = Position::new(2, 1);
        assert_eq!(
            board.index(position),
            Some(board.width() * position.y + position.x)
        );
        assert_eq!(board.position(5), Some(Position::new(2, 1)));
        assert_eq!(board.index(Position::new(3, 0)), None);
    }

    /// Confirms the moving-object scan retains the DOS loop's linear endpoints.
    #[test]
    fn moving_object_scan_uses_asymmetric_linear_border_bounds() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 4),
                    State::new(Actor::SnikSnak(SnikSnak::new(Direction::Left))),
                ),
                (Position::new(3, 5), State::empty()),
            ],
            0,
        );

        // A 7×6 board scans indices 8 through 33. The source at index 31 is
        // eligible, but the destination on the bottom row at index 38 is not.
        assert_eq!(game.board.moving_object_scan_range(), 8..34);
        game.tick = 3;
        game.tick(Input::default());
        let bottom_enemy = game
            .board()
            .state(Position::new(3, 5))
            .expect("bottom-edge destination should exist");
        assert_eq!(
            bottom_enemy.snapshot().label(),
            format!("SnikSnakMove({:?})", Direction::Down)
        );
        assert_eq!(bottom_enemy.snapshot().frame(), 0);

        for _ in 0..4 {
            game.tick(Input::default());
        }
        assert_eq!(
            game.board()
                .state(Position::new(3, 5))
                .expect("excluded bottom-edge enemy should remain present")
                .snapshot()
                .frame(),
            0
        );
    }

    /// Confirms destination occupancy and source reservation move in lockstep.
    #[test]
    fn murphy_move_occupies_destination_during_animation() {
        let mut game = game_with(
            &[(
                Position::new(2, 2),
                State::new(Actor::Murphy(Murphy::new())),
            )],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        let source = game
            .board()
            .state(Position::new(2, 2))
            .expect("source should exist");
        assert!(matches!(source.actor(), Actor::Empty(_)));
        assert_eq!(
            source.snapshot().label(),
            format!("Vacating({:?})", Direction::Right)
        );
        assert!(!source.is_empty());
        let destination = game
            .board()
            .state(Position::new(3, 2))
            .expect("destination should exist");
        assert!(matches!(destination.actor(), Actor::Murphy(_)));
        assert_eq!(
            destination.snapshot().label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Right,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );

        // The initiating update already consumes frame zero. Seven following
        // updates release the source while retaining final frame seven until
        // Murphy processes the next input update.
        for _ in 0..7 {
            game.tick(Input::default());
        }
        let completed = game
            .board()
            .state(Position::new(3, 2))
            .expect("destination should exist")
            .snapshot();
        assert_eq!(
            completed.label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Right,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(completed.frame(), 7);
        assert!(
            game.board()
                .state(Position::new(2, 2))
                .expect("released source should exist")
                .is_empty()
        );

        // With no new command, input resumption settles Murphy to true Idle.
        game.tick(Input::default());
        assert_eq!(
            game.board()
                .state(Position::new(3, 2))
                .expect("destination should remain occupied")
                .snapshot()
                .label(),
            "Idle"
        );
    }

    /// Confirms collection occurs when Murphy's dedicated strip completes.
    #[test]
    fn murphy_collects_infotron_and_decrements_requirement() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
            ],
            1,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        assert_eq!(game.remaining_infotrons(), 1);
        for _ in 0..7 {
            game.tick(Input::default());
        }
        assert_eq!(game.remaining_infotrons(), 0);
    }

    /// Confirms Murphy's material sounds begin with input, not strip completion.
    #[test]
    fn murphy_requests_base_and_infotron_sounds_on_the_action_tick() {
        let murphy = (
            Position::new(2, 2),
            State::new(Actor::Murphy(Murphy::new())),
        );
        let input = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };

        // Moving into Base selects the Base effect before any of its eight
        // visual frames can complete.
        let mut base = game_with(
            &[
                murphy.clone(),
                (Position::new(3, 2), State::new(Actor::Base(Base::Resting))),
            ],
            0,
        );
        base.tick(input);
        assert_eq!(base.take_sound_effects(), vec![SoundEffect::Base]);
        assert!(base.take_sound_effects().is_empty());

        // Infotron movement follows the same timing even though collection is
        // deliberately deferred until the final animation frame.
        let mut infotron = game_with(
            &[
                murphy,
                (
                    Position::new(3, 2),
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
            ],
            1,
        );
        infotron.tick(input);
        assert_eq!(infotron.take_sound_effects(), vec![SoundEffect::Infotron]);
        assert_eq!(infotron.remaining_infotrons(), 1);
    }

    /// Confirms the push effect waits for the original hold counter to expire.
    #[test]
    fn murphy_requests_push_sound_only_when_the_push_strip_begins() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );
        let held_right = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };

        // The initiating update plus seven countdown updates remain silent.
        for _ in 0..8 {
            game.tick(held_right);
            assert!(game.take_sound_effects().is_empty());
        }
        game.tick(held_right);
        assert_eq!(game.take_sound_effects(), vec![SoundEffect::Push]);
    }

    /// Confirms active Bugs chirp only when Murphy is in their 3x3 neighborhood.
    #[test]
    fn bug_sound_requires_active_animation_and_neighboring_murphy() {
        let mut adjacent = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::new(Actor::Bug(Bug::new()))),
            ],
            0,
        );
        adjacent.tick(Input::default());
        assert_eq!(adjacent.take_sound_effects(), vec![SoundEffect::Bug]);

        // A Bug outside the eight-cell neighborhood advances identically but
        // makes no request, matching the proximity test in the DOS updater.
        let mut distant = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(4, 1), State::new(Actor::Bug(Bug::new()))),
            ],
            0,
        );
        distant.tick(Input::default());
        assert!(distant.take_sound_effects().is_empty());
    }

    /// Confirms a stable Zonk can be pushed only with free space behind it.
    #[test]
    fn murphy_pushes_one_idle_zonk_horizontally() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        for _ in 0..15 {
            game.tick(Input {
                direction: Some(Direction::Right),
                ..Input::default()
            });
        }

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        let pushed = game
            .board()
            .state(Position::new(4, 2))
            .expect("pushed Zonk destination should exist");
        assert!(matches!(pushed.actor(), Actor::Zonk(_)));
        assert_eq!(pushed.snapshot().label(), "ZonkPreFall");
    }

    /// Confirms releasing a prepared push restores both reserved board cells.
    #[test]
    fn murphy_cancels_push_when_direction_is_released() {
        let murphy_position = Position::new(2, 2);
        let zonk_position = Position::new(3, 2);
        let destination = Position::new(4, 2);
        let mut game = game_with(
            &[
                (murphy_position, State::new(Actor::Murphy(Murphy::new()))),
                (zonk_position, State::new(Actor::Zonk(Zonk::resting()))),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert!(
            !game
                .board()
                .state(destination)
                .expect("push destination should be reserved")
                .is_empty()
        );

        game.tick(Input::default());
        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));
        assert!(
            game.board()
                .state(destination)
                .expect("cancelled destination should exist")
                .is_empty()
        );
    }

    /// Confirms snapping retains its target and defers collection to frame seven.
    #[test]
    fn murphy_snap_collection_finishes_after_its_target_specific_strip() {
        let target = Position::new(3, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (target, State::new(Actor::Infotron(Infotron::resting()))),
            ],
            1,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            action: true,
        });
        assert_eq!(game.remaining_infotrons(), 1);
        assert!(matches!(actor_at(&game, 3, 2), Actor::Infotron(_)));
        assert_eq!(
            game.board()
                .state(Position::new(2, 2))
                .expect("Murphy should remain in place")
                .snapshot()
                .label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Snap {
                    direction: Direction::Right,
                    target: crate::actors::MurphySnapTarget::Infotron,
                }
            )
        );

        for _ in 0..6 {
            game.tick(Input::default());
        }
        assert_eq!(game.remaining_infotrons(), 0);
        assert!(
            game.board()
                .state(target)
                .expect("snapped target cell should exist")
                .is_empty()
        );
    }

    /// Confirms Murphy-first input cannot consume a moving Infotron from the
    /// three directions where the original requires target state zero.
    #[test]
    fn murphy_refuses_moving_infotrons_before_their_linear_update() {
        let murphy_position = Position::new(3, 3);
        let cases = [
            (Direction::Up, Position::new(3, 2), Position::new(3, 1)),
            (Direction::Left, Position::new(2, 3), Position::new(2, 2)),
            (Direction::Right, Position::new(4, 3), Position::new(4, 2)),
        ];

        for action in [false, true] {
            for (direction, target, source) in cases {
                let mut placements = vec![
                    (murphy_position, State::new(Actor::Murphy(Murphy::new()))),
                    (source, State::new(Actor::Infotron(Infotron::resting()))),
                ];
                if direction.is_horizontal() {
                    placements.push((
                        Position::new(target.x, target.y + 1),
                        State::new(Actor::Hardware(Hardware::new(0))),
                    ));
                }
                let mut game = game_with(&placements, 1);

                // Arm the Infotron, then transfer it into Murphy's neighboring
                // cell. Its destination owns the still-solid source marker.
                game.tick(Input::default());
                game.tick(Input::default());
                let moving = game
                    .board()
                    .state(target)
                    .expect("the falling Infotron should occupy its destination");
                assert_eq!(
                    moving.snapshot().label(),
                    format!("Moving({:?})", Direction::Down)
                );
                assert_eq!(moving.snapshot().frame(), 0);

                game.tick(Input {
                    direction: Some(direction),
                    action,
                });

                // Murphy is evaluated first and must refuse the interaction.
                // The retained Infotron is then present in the captured linear
                // schedule and advances once during this same tick.
                let murphy = game
                    .board()
                    .state(murphy_position)
                    .expect("Murphy should retain his cell");
                assert!(
                    matches!(murphy.actor(), Actor::Murphy(_)),
                    "direction {direction:?}, action {action}"
                );
                assert_eq!(
                    murphy.snapshot().label(),
                    "Idle",
                    "direction {direction:?}, action {action}"
                );
                assert_eq!(game.remaining_infotrons(), 1);
                let moving = game
                    .board()
                    .state(target)
                    .expect("the refused Infotron should remain scheduled");
                assert!(matches!(moving.actor(), Actor::Infotron(_)));
                assert_eq!(
                    moving.snapshot().label(),
                    format!("Moving({:?})", Direction::Down)
                );
                assert_eq!(moving.snapshot().frame(), 1);

                // Destination-owned cleanup must still run at original state
                // 0x16 now that Murphy no longer removes its updater.
                for _ in 0..5 {
                    game.tick(Input::default());
                }
                assert!(
                    game.board()
                        .state(source)
                        .expect("the released source remains addressable")
                        .is_empty(),
                    "direction {direction:?}, action {action}"
                );
            }
        }
    }

    /// Confirms ordinary downward movement retains the original tile-only
    /// Infotron check rather than normalizing all four directions to state zero.
    #[test]
    fn murphy_downward_move_accepts_a_non_idle_infotron() {
        let murphy_position = Position::new(3, 1);
        let target = Position::new(3, 2);
        let mut game = game_with(
            &[
                (murphy_position, State::new(Actor::Murphy(Murphy::new()))),
                (target, State::new(Actor::Infotron(Infotron::resting()))),
                (
                    Position::new(3, 3),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 4),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            1,
        );

        game.tick(Input::default());
        assert_eq!(
            game.board()
                .state(target)
                .expect("the rounded Infotron should retain its cell")
                .snapshot()
                .label(),
            format!("RoundedPreRoll({:?})", Direction::Left)
        );

        game.tick(Input {
            direction: Some(Direction::Down),
            ..Input::default()
        });
        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        assert_eq!(
            game.board()
                .state(target)
                .expect("Murphy should begin the downward eating strip")
                .snapshot()
                .label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Down,
                    target: crate::actors::MurphyMoveTarget::Infotron,
                    looking_left: false,
                }
            )
        );
    }

    /// Confirms a Zonk arms for one update before one captured fall begins.
    #[test]
    fn zonk_arms_then_falls_once_without_a_double_update() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        // The first callback records the original `0x41` pre-fall phase in the
        // Zonk's own cell without reserving or entering the destination yet.
        game.tick(Input::default());
        let armed = game
            .board()
            .state(Position::new(3, 1))
            .expect("armed Zonk cell should exist");
        assert!(matches!(armed.actor(), Actor::Zonk(zonk) if !zonk.is_falling()));
        assert_eq!(armed.snapshot().label(), "ZonkPreFall");
        assert!(matches!(actor_at(&game, 3, 2), Actor::Empty(_)));

        // The next captured callback starts exactly one transfer. The new
        // destination is not scheduled again later in this same linear pass.
        game.tick(Input::default());

        assert!(matches!(actor_at(&game, 3, 1), Actor::Empty(_)));
        let falling = game
            .board()
            .state(Position::new(3, 2))
            .expect("fall destination should exist");
        assert!(matches!(falling.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            falling.snapshot().label(),
            format!("Moving({:?})", Direction::Down)
        );
        assert_eq!(falling.snapshot().frame(), 0);
        assert!(matches!(actor_at(&game, 3, 3), Actor::Empty(_)));
    }

    /// Confirms a resting Infotron uses the same pre-fall boundary as a Zonk.
    #[test]
    fn infotron_arms_before_its_first_fall() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
            ],
            0,
        );

        game.tick(Input::default());
        let armed = game
            .board()
            .state(Position::new(3, 1))
            .expect("armed Infotron should remain in its source cell");
        assert_eq!(armed.snapshot().label(), "InfotronPreFall");

        game.tick(Input::default());
        let falling = game
            .board()
            .state(Position::new(3, 2))
            .expect("Infotron fall destination should exist");
        assert!(matches!(falling.actor(), Actor::Infotron(infotron) if infotron.is_falling()));
        assert_eq!(
            falling.snapshot().label(),
            format!("Moving({:?})", Direction::Down)
        );
    }

    /// Confirms falling destinations clear their sources at original state 0x16.
    #[test]
    fn zonk_and_infotron_release_sources_before_their_final_two_frames() {
        let zonk_source = Position::new(2, 1);
        let infotron_source = Position::new(4, 1);
        let zonk_destination = Position::new(2, 2);
        let infotron_destination = Position::new(4, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (zonk_source, State::new(Actor::Zonk(Zonk::resting()))),
                (
                    infotron_source,
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
            ],
            0,
        );

        // Arm on the first callback, begin movement on the second, then reach
        // original destination state 0x15 after five movement callbacks.
        for _ in 0..7 {
            game.tick(Input::default());
        }
        assert_eq!(
            game.board()
                .state(zonk_destination)
                .expect("moving Zonk should occupy its destination")
                .snapshot()
                .frame(),
            5
        );
        assert_eq!(
            game.board()
                .state(infotron_destination)
                .expect("moving Infotron should occupy its destination")
                .snapshot()
                .frame(),
            5
        );
        assert!(
            !game
                .board()
                .state(zonk_source)
                .expect("Zonk source reservation should still exist")
                .is_empty()
        );
        assert!(
            !game
                .board()
                .state(infotron_source)
                .expect("Infotron source reservation should still exist")
                .is_empty()
        );

        game.tick(Input::default());
        assert_eq!(
            game.board()
                .state(zonk_destination)
                .expect("Zonk should retain its destination")
                .snapshot()
                .frame(),
            6
        );
        assert_eq!(
            game.board()
                .state(infotron_destination)
                .expect("Infotron should retain its destination")
                .snapshot()
                .frame(),
            6
        );
        assert!(
            game.board()
                .state(zonk_source)
                .expect("released Zonk source should remain addressable")
                .is_empty()
        );
        assert!(
            game.board()
                .state(infotron_source)
                .expect("released Infotron source should remain addressable")
                .is_empty()
        );
    }

    /// Confirms retained downward momentum does not repeat the resting delay.
    #[test]
    fn falling_zonk_continues_into_the_next_cell_without_rearming() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        // The initial pre-fall update, transfer start, and eight animation
        // updates complete the first cell while preserving falling momentum.
        for _ in 0..10 {
            game.tick(Input::default());
        }
        // The final picture reserves the next destination as original state
        // 70/99. The next callback transfers and consumes falling picture zero.
        assert_eq!(
            game.board()
                .state(Position::new(3, 3))
                .unwrap()
                .reservation(),
            Some(crate::actors::empty::Reservation::RoundedContinuation)
        );
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(zonk) if zonk.is_falling()));
        game.tick(Input::default());
        let continued = game
            .board()
            .state(Position::new(3, 3))
            .expect("continued fall destination should exist");
        assert!(matches!(continued.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            continued.snapshot().label(),
            format!("Moving({:?})", Direction::Down)
        );
        assert_eq!(continued.snapshot().frame(), 1);
        let prior_cell = game
            .board()
            .state(Position::new(3, 2))
            .expect("continued fall source should remain reserved");
        assert_eq!(
            prior_cell.snapshot().label(),
            format!("Vacating({:?})", Direction::Down)
        );
        assert!(!prior_cell.is_empty());
    }

    /// Confirms the Fall effect marks a completed safe landing, not fall start.
    #[test]
    fn falling_zonk_requests_fall_sound_when_it_reaches_support() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        // One pre-fall update and all eight transfer pictures are silent. The
        // following settle callback observes Hardware and requests Fall once.
        for _ in 0..9 {
            game.tick(Input::default());
            assert!(game.take_sound_effects().is_empty());
        }
        game.tick(Input::default());
        assert_eq!(game.take_sound_effects(), vec![SoundEffect::Fall]);
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));
    }

    /// Confirms protected push states bypass the falling-object sound branch.
    #[test]
    fn falling_actor_lands_silently_on_push_protected_murphy() {
        for falling_actor in [
            Actor::Zonk(Zonk::resting()),
            Actor::Infotron(Infotron::resting()),
        ] {
            let mut game = game_with(
                &[
                    (Position::new(3, 1), State::new(falling_actor)),
                    (
                        Position::new(3, 3),
                        State::new(Actor::Murphy(Murphy::new())),
                    ),
                    (
                        Position::new(4, 3),
                        State::new(Actor::Zonk(Zonk::resting())),
                    ),
                ],
                0,
            );
            let held_right = Input {
                direction: Some(Direction::Right),
                ..Input::default()
            };

            // Both falling actors reach their final transfer picture while
            // Murphy's eight-tick hold begins the protected push animation.
            // The tenth update settles them onto Murphy without adding Fall.
            for _ in 0..10 {
                game.tick(held_right);
            }
            assert_eq!(game.take_sound_effects(), vec![SoundEffect::Push]);
            match actor_at(&game, 3, 2) {
                Actor::Zonk(zonk) => assert!(!zonk.is_falling()),
                Actor::Infotron(infotron) => assert!(!infotron.is_falling()),
                actor => panic!("falling actor settled as unexpected {actor:?}"),
            }
        }
    }

    /// Confirms freeze lets an in-flight fall finish without desynchronizing it.
    #[test]
    fn freezing_an_in_flight_zonk_finishes_its_source_reservation() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        // Let the distinct pre-fall callback and transfer callback both run
        // before enabling freeze, leaving a real in-flight movement at frame 0.
        game.tick(Input::default());
        game.tick(Input::default());
        game.freeze_zonks = true;

        // The destination keeps advancing even though freeze became active.
        // Its source marker has no autonomous frame; destination state 0x16
        // releases that marker before the last two pictures are shown.
        for expected_frame in 1..=7 {
            game.tick(Input::default());
            let destination = game
                .board()
                .state(Position::new(3, 2))
                .expect("fall destination should exist")
                .snapshot();
            assert_eq!(destination.frame(), expected_frame);
            let source = game
                .board()
                .state(Position::new(3, 1))
                .expect("fall source should remain addressable");
            if expected_frame < 6 {
                assert_eq!(
                    source.snapshot().label(),
                    format!("Vacating({:?})", Direction::Down)
                );
                assert!(!source.is_empty());
            } else {
                assert!(source.is_empty());
            }
        }

        game.tick(Input::default());
        assert!(
            game.board()
                .state(Position::new(3, 1))
                .expect("released fall source should exist")
                .is_empty()
        );
        assert!(matches!(
            actor_at(&game, 3, 2),
            Actor::Zonk(zonk) if !zonk.is_falling()
        ));

        // A subsequent frozen tick cannot begin the next downward transfer.
        game.tick(Input::default());
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));
        assert!(matches!(actor_at(&game, 3, 3), Actor::Empty(_)));
    }

    /// Confirms gravity overrides even otherwise valid unsupported side input.
    #[test]
    fn gravity_forces_murphy_down_instead_of_sideways() {
        let mut game = game_with(
            &[(
                (Position::new(2, 2)),
                State::new(Actor::Murphy(Murphy::new())),
            )],
            0,
        );
        game.gravity = true;

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 2, 3), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Empty(_)));
    }

    /// Confirms eating Base remains the original exception to unsupported gravity.
    #[test]
    fn gravity_allows_a_plain_sideways_base_eat() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(3, 2), State::new(Actor::Base(Base::Resting))),
            ],
            0,
        );
        game.gravity = true;

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 2, 3), Actor::Empty(_)));
    }

    /// Confirms Murphy can occupy the destination of an already armed Zonk.
    #[test]
    fn murphy_moves_before_an_armed_zonk_can_enter_the_same_destination() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        // Arm the Zonk while its lower cell is still empty. Its actor and
        // position remain unchanged until a later scheduled callback.
        game.tick(Input::default());
        let armed = game
            .board()
            .state(Position::new(3, 1))
            .expect("armed Zonk cell should exist");
        assert_eq!(armed.snapshot().label(), "ZonkPreFall");

        // Murphy is processed first and occupies the lower cell. The Zonk then
        // observes that live write and retains its pre-fall promise in place.
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        let still_armed = game
            .board()
            .state(Position::new(3, 1))
            .expect("blocked pre-fall Zonk should remain in place");
        assert!(matches!(still_armed.actor(), Actor::Zonk(_)));
        assert_eq!(still_armed.snapshot().label(), "ZonkPreFall");
        assert!(
            !game
                .board()
                .state(Position::new(2, 2))
                .expect("Murphy source should remain reserved")
                .is_empty()
        );
    }

    /// Confirms Murphy moves before a Zonk resolves its final landing frame.
    #[test]
    fn murphy_escapes_as_a_zonk_finishes_falling_above() {
        let mut game = game_with(
            &[
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
            ],
            0,
        );

        // One pre-fall step plus eight transfer steps leave the Zonk on its final
        // interpolated frame immediately above an otherwise idle Murphy.
        for _ in 0..9 {
            game.tick(Input::default());
        }
        let falling = game
            .board()
            .state(Position::new(3, 2))
            .expect("falling Zonk destination should exist");
        assert!(matches!(falling.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(falling.snapshot().frame(), 7);

        // Murphy's earlier Right move changes the would-be crush into
        // a stable landing above his collision-reserved source cell.
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Playing);
        let murphy = game
            .board()
            .state(Position::new(4, 3))
            .expect("Murphy's escaped destination should exist");
        assert!(matches!(murphy.actor(), Actor::Murphy(_)));
        assert_eq!(
            murphy.snapshot().label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Right,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(murphy.snapshot().frame(), 0);
        let source = game
            .board()
            .state(Position::new(3, 3))
            .expect("Murphy's reserved source should exist");
        assert_eq!(
            source.snapshot().label(),
            format!("Vacating({:?})", Direction::Right)
        );
        assert!(!source.is_empty());
        assert!(matches!(
            actor_at(&game, 3, 2),
            Actor::Zonk(zonk) if !zonk.is_falling()
        ));
    }

    /// Confirms held Down lets Murphy move before a trailing Zonk transfers.
    #[test]
    fn murphy_continues_down_before_a_trailing_zonk_transfers() {
        let mut game = game_with(
            &[
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
            ],
            0,
        );
        let down = Input {
            direction: Some(Direction::Down),
            ..Input::default()
        };

        // Movement completion releases the old collision reservation during
        // Murphy's player-first update but keeps his final Down pose. The later
        // Zonk arms in its original cell without entering the released source.
        for _ in 0..8 {
            game.tick(down);
        }
        let ready = game
            .board()
            .state(Position::new(3, 3))
            .expect("completed Murphy destination should exist");
        assert!(matches!(ready.actor(), Actor::Murphy(_)));
        assert_eq!(
            ready.snapshot().label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Down,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(ready.snapshot().frame(), 7);
        assert!(
            game.board()
                .state(Position::new(3, 2))
                .expect("released Murphy source should exist")
                .is_empty()
        );
        let trailing = game
            .board()
            .state(Position::new(3, 1))
            .expect("pre-fall trailing Zonk should remain in place");
        assert!(matches!(trailing.actor(), Actor::Zonk(zonk) if !zonk.is_falling()));
        assert_eq!(trailing.snapshot().label(), "ZonkPreFall");
        assert_eq!(trailing.snapshot().frame(), 0);

        // The following update resumes held input first. Only afterward does
        // the armed Zonk begin entering the older, now-safe source cell.
        game.tick(down);

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 3, 4), Actor::Murphy(_)));
        let moving = game
            .board()
            .state(Position::new(3, 4))
            .expect("Murphy destination should exist")
            .snapshot();
        assert_eq!(
            moving.label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Down,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(moving.frame(), 0);
        let trailing = game
            .board()
            .state(Position::new(3, 2))
            .expect("trailing Zonk should enter the older source");
        assert!(matches!(trailing.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            trailing.snapshot().label(),
            format!("Moving({:?})", Direction::Down)
        );
        assert_eq!(trailing.snapshot().frame(), 0);
    }

    /// Confirms a turn happens before a trailing pre-fall Zonk transfers.
    #[test]
    fn murphy_turns_sideways_before_a_trailing_zonk_transfers() {
        let mut game = game_with(
            &[
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Down),
            ..Input::default()
        });
        let turn = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };
        // Seven more steps finish the original Down animation without applying
        // the newly held Right direction during the completion update.
        for _ in 0..7 {
            game.tick(turn);
        }
        let ready = game
            .board()
            .state(Position::new(3, 3))
            .expect("completed Murphy destination should exist");
        assert!(matches!(ready.actor(), Actor::Murphy(_)));
        assert_eq!(
            ready.snapshot().label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Down,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(ready.snapshot().frame(), 7);
        assert!(
            game.board()
                .state(Position::new(3, 2))
                .expect("released Murphy source should exist")
                .is_empty()
        );
        let trailing = game
            .board()
            .state(Position::new(3, 1))
            .expect("pre-fall trailing Zonk should remain in place");
        assert!(matches!(trailing.actor(), Actor::Zonk(zonk) if !zonk.is_falling()));
        assert_eq!(trailing.snapshot().label(), "ZonkPreFall");
        assert_eq!(trailing.snapshot().frame(), 0);

        // The next update consumes Right and reserves Murphy's current source
        // before the armed Zonk transfers into the older released source.
        game.tick(turn);

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 4, 3), Actor::Murphy(_)));
        let trailing = game
            .board()
            .state(Position::new(3, 2))
            .expect("trailing Zonk should enter the older source");
        assert!(matches!(trailing.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            trailing.snapshot().label(),
            format!("Moving({:?})", Direction::Down)
        );
        assert_eq!(trailing.snapshot().frame(), 0);
        assert!(
            !game
                .board()
                .state(Position::new(3, 3))
                .expect("turn source should exist")
                .is_empty()
        );
        let moving = game
            .board()
            .state(Position::new(4, 3))
            .expect("turned Murphy destination should exist")
            .snapshot();
        assert_eq!(
            moving.label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Right,
                    target: crate::actors::MurphyMoveTarget::Empty,
                    looking_left: false,
                }
            )
        );
        assert_eq!(moving.frame(), 0);
    }

    /// Confirms actor identity alone cannot bypass an animated source reserve.
    #[test]
    fn murphy_cannot_enter_a_vacating_hazard_source() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        // A separate first callback arms the fall; the second starts movement
        // and leaves an invisible but collision-occupied source reservation.
        game.tick(Input::default());
        game.tick(Input::default());
        let source = game
            .board()
            .state(Position::new(3, 1))
            .expect("fall source should exist");
        assert!(matches!(source.actor(), Actor::Empty(_)));
        assert_eq!(
            source.snapshot().label(),
            format!("Vacating({:?})", Direction::Down)
        );
        assert!(!source.is_empty());

        // Murphy must remain in place until the source animation releases it.
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert!(matches!(actor_at(&game, 2, 1), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 3, 1), Actor::Empty(_)));
        assert!(
            !game
                .board()
                .state(Position::new(3, 1))
                .expect("reserved source should remain present")
                .is_empty()
        );
    }

    /// Confirms Murphy can push an idle Orange Disk horizontally into free space.
    #[test]
    fn murphy_pushes_an_orange_disk_horizontally() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        for _ in 0..15 {
            game.tick(Input {
                direction: Some(Direction::Right),
                ..Input::default()
            });
        }

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        let orange = game
            .board()
            .state(Position::new(4, 2))
            .expect("pushed Orange Disk should remain at its source during pre-fall");
        assert!(matches!(orange.actor(), Actor::OrangeDisk(_)));
        assert_eq!(orange.snapshot().label(), "OrangePreFall");
        assert!(
            !game
                .board()
                .state(Position::new(4, 3))
                .expect("Orange fall destination should be reserved")
                .is_empty()
        );
    }

    /// Confirms a rounded roll uses pre-roll, horizontal, then vertical phases.
    #[test]
    fn rounded_rolls_use_a_diagonal_animation_kind() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        game.tick(Input::default());
        let preparing = game
            .board()
            .state(Position::new(3, 1))
            .expect("rounded actor source should exist");
        assert_eq!(
            preparing.snapshot().label(),
            format!("RoundedPreRoll({:?})", Direction::Left)
        );

        game.tick(Input::default());

        let rolled = game
            .board()
            .state(Position::new(2, 1))
            .expect("horizontal slide cell should exist");
        assert!(matches!(rolled.actor(), Actor::Zonk(_)));
        assert_eq!(
            rolled.snapshot().label(),
            format!("Rolling({:?})", Direction::Left)
        );
    }

    /// Confirms freeze lets both in-flight rounded movement phases complete.
    #[test]
    fn freezing_an_in_flight_roll_keeps_both_animation_cells_synchronized() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        for _ in 0..2 {
            game.tick(Input::default());
        }
        game.freeze_zonks = true;
        // Pictures zero and one already ran during preparation.
        for expected_frame in 3..=7 {
            game.tick(Input::default());
            let sliding = game
                .board()
                .state(Position::new(2, 1))
                .expect("horizontal slide cell should exist")
                .snapshot();
            assert_eq!(sliding.frame(), expected_frame);
            assert!(
                !game
                    .board()
                    .state(Position::new(2, 2))
                    .expect("diagonal destination reservation should exist")
                    .is_empty()
            );
        }

        game.tick(Input::default());
        assert!(matches!(
            actor_at(&game, 2, 2),
            Actor::Zonk(zonk) if zonk.is_falling()
        ));
        assert_eq!(
            game.board()
                .state(Position::new(2, 2))
                .expect("vertical roll destination should exist")
                .snapshot()
                .label(),
            format!("Moving({:?})", Direction::Down)
        );

        for _ in 0..8 {
            game.tick(Input::default());
        }
        assert!(matches!(
            actor_at(&game, 2, 2),
            Actor::Zonk(zonk) if !zonk.is_falling()
        ));
        assert!(matches!(actor_at(&game, 2, 3), Actor::Empty(_)));
    }

    /// Confirms Murphy blocks the first roll candidate before Zonk updates.
    #[test]
    fn rolling_zonk_chooses_its_other_side_after_murphy_moves_first() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        let preparing = game
            .board()
            .state(Position::new(3, 1))
            .expect("right-hand pre-roll source should exist");
        assert!(matches!(preparing.actor(), Actor::Zonk(_)));
        assert_eq!(
            preparing.snapshot().label(),
            format!("RoundedPreRoll({:?})", Direction::Right)
        );
        assert!(
            !game
                .board()
                .state(Position::new(4, 1))
                .expect("right side should be reserved")
                .is_empty()
        );
    }

    /// Confirms a port traversal occupies its exit before a later Zonk update.
    #[test]
    fn murphy_crosses_a_port_before_a_zonk_can_enter_its_exit() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::Port(Port::new(PortDirections::Horizontal, false))),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Playing);
        let murphy = game
            .board()
            .state(Position::new(1, 2))
            .expect("port entrance should retain Murphy during traversal");
        assert!(matches!(murphy.actor(), Actor::Murphy(_)));
        assert_eq!(
            murphy.snapshot().label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Port {
                    direction: Direction::Right,
                }
            )
        );
        assert!(
            !game
                .board()
                .state(Position::new(3, 2))
                .expect("port exit reservation should exist")
                .is_empty()
        );
        assert!(matches!(actor_at(&game, 3, 1), Actor::Zonk(_)));
    }

    /// Confirms Murphy's push reserves its destination before later physics.
    #[test]
    fn murphy_pushes_before_a_zonk_can_enter_the_disk_destination() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(2, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 1, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 2, 2), Actor::Zonk(_)));
        assert!(
            !game
                .board()
                .state(Position::new(3, 2))
                .expect("push destination reservation should exist")
                .is_empty()
        );
        assert!(matches!(actor_at(&game, 3, 1), Actor::Zonk(_)));
    }

    /// Confirms one planted Red Disk uses Murphy's vacated source and blocks another.
    #[test]
    fn planted_red_disk_uses_murphys_cell_and_enforces_one_fuse() {
        let origin = Position::new(2, 2);
        let mut game = game_with(&[(origin, State::new(Actor::Murphy(Murphy::new())))], 0);
        game.red_disks = 2;

        // A completely input-free update arms the original placement latch.
        game.tick(Input::default());
        for _ in 0..65 {
            game.tick(Input {
                action: true,
                ..Input::default()
            });
        }
        assert_eq!(game.red_disks(), 1);
        assert_eq!(
            game.planted_red_disk.map(|disk| disk.position),
            Some(origin)
        );
        assert!(matches!(actor_at(&game, 3, 2), Actor::Empty(_)));

        // Vacate the stored source cell after the completed hold-to-place pose.
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert_eq!(
            game.board()
                .state(origin)
                .expect("reserved planted position should exist")
                .snapshot()
                .label(),
            format!("Vacating({:?})", Direction::Right)
        );

        // The disk materializes only after Murphy's collision reservation has
        // released the source; its independent fuse has continued throughout.
        for _ in 0..7 {
            game.tick(Input::default());
        }

        let planted = game
            .board()
            .state(origin)
            .expect("planted position should exist");
        assert!(matches!(planted.actor(), Actor::RedDisk(_)));
        assert_eq!(planted.snapshot().label(), "RedDiskFuse");
        assert_eq!(
            game.planted_red_disk.map(|disk| disk.position),
            Some(origin)
        );

        // A second plant command is ignored while the retained fuse is active.
        game.tick(Input::default());
        game.tick(Input {
            action: true,
            ..Input::default()
        });
        assert_eq!(game.red_disks(), 1);
        assert!(game.planted_red_disk.is_some());

        // Crossing the visible disk conceals it without collecting it or
        // cancelling the timer stored independently of board occupancy.
        game.tick(Input {
            direction: Some(Direction::Left),
            ..Input::default()
        });
        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        assert_eq!(
            game.board()
                .state(origin)
                .expect("Murphy should animate over the planted disk")
                .snapshot()
                .label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Move {
                    direction: Direction::Left,
                    target: crate::actors::MurphyMoveTarget::PlantedRedDisk,
                    looking_left: true,
                }
            )
        );
        assert_eq!(game.red_disks(), 1);
        assert!(game.planted_red_disk.is_some());
    }

    /// Confirms releasing Space cancels planting without spending inventory.
    #[test]
    fn incomplete_red_disk_plant_is_cancellable() {
        let origin = Position::new(2, 2);
        let mut game = game_with(&[(origin, State::new(Actor::Murphy(Murphy::new())))], 0);
        game.red_disks = 1;

        game.tick(Input::default());
        game.tick(Input {
            action: true,
            ..Input::default()
        });
        assert_eq!(game.red_disks(), 1);
        assert_eq!(game.planted_red_disk.map(|disk| disk.countdown), Some(1));

        game.tick(Input::default());
        assert_eq!(game.red_disks(), 1);
        assert!(game.planted_red_disk.is_none());
        assert_eq!(
            game.board()
                .state(origin)
                .expect("Murphy's cell should remain present")
                .snapshot()
                .label(),
            "Idle"
        );
    }

    /// Confirms a Zonk strike arms the Orange Disk's delayed animation promise.
    #[test]
    fn zonk_arms_an_idle_orange_disk_after_landing() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
                (
                    Position::new(3, 4),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        for _ in 0..10 {
            game.tick(Input::default());
        }

        let orange = game
            .board()
            .state(Position::new(3, 3))
            .expect("Orange Disk cell should exist");
        assert_eq!(orange.snapshot().label(), "OrangeDiskFuse");
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));

        for _ in 0..6 {
            game.tick(Input::default());
        }
        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
    }

    /// Confirms an Orange landing blast does not re-arm its vacated source.
    #[test]
    fn orange_landing_explosion_schedules_no_phantom_source_wave() {
        let source = Position::new(3, 1);
        let destination = Position::new(3, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (source, State::new(Actor::OrangeDisk(OrangeDisk::resting()))),
                (
                    Position::new(3, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        // One initiating update, two pre-fall states, and eight falling states
        // place the armed disk at its blocked destination and emit its blast.
        for _ in 0..11 {
            game.tick(Input::default());
        }

        assert!(matches!(actor_at(&game, 3, 1), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Explosion(_)));
        let source_index = game
            .board()
            .index(source)
            .expect("the Orange source should remain in bounds");
        assert_eq!(game.explosion_timers[source_index], 0);
        let destination_index = game
            .board()
            .index(destination)
            .expect("the Orange destination should remain in bounds");
        assert_eq!(game.explosion_timers[destination_index], 0);
    }

    /// Confirms a blast that consumes a falling owner also releases the
    /// destination-owned source marker just outside the visible 3x3 wave.
    #[test]
    fn explosions_release_falling_zonk_and_infotron_sources() {
        for actor in [
            Actor::Zonk(Zonk::resting()),
            Actor::Infotron(Infotron::resting()),
        ] {
            let source = Position::new(4, 2);
            let destination = Position::new(4, 3);
            let mut game = game_with(
                &[
                    (
                        Position::new(1, 4),
                        State::new(Actor::Murphy(Murphy::new())),
                    ),
                    (source, State::new(actor.clone())),
                    (
                        Position::new(4, 4),
                        State::new(Actor::Hardware(Hardware::new(0))),
                    ),
                ],
                0,
            );

            game.tick(Input::default());
            game.tick(Input::default());
            assert_eq!(
                game.board()
                    .state(destination)
                    .expect("the falling owner should occupy its destination")
                    .snapshot()
                    .label(),
                format!("Moving({:?})", Direction::Down)
            );
            assert_eq!(
                game.board()
                    .state(source)
                    .expect("the falling source should remain reserved")
                    .snapshot()
                    .label(),
                format!("Vacating({:?})", Direction::Down)
            );

            // This wave reaches the destination at its upper-left corner, but
            // its old source one row higher is outside the blast footprint.
            game.detonate_position(Position::new(5, 4));
            assert!(matches!(actor_at(&game, 4, 3), Actor::Explosion(_)));
            assert!(
                game.board()
                    .state(source)
                    .expect("the released source remains addressable")
                    .is_empty(),
                "blast should release the source owned by {actor:?}"
            );
        }
    }

    /// Confirms both rounded phases release the side or diagonal marker when a
    /// blast reaches the owner but not that reservation.
    #[test]
    fn explosions_release_rounded_zonk_and_infotron_reservations() {
        for actor in [
            Actor::Zonk(Zonk::resting()),
            Actor::Infotron(Infotron::resting()),
        ] {
            for direction in [Direction::Left, Direction::Right] {
                for rolling in [false, true] {
                    let origin = Position::new(3, 2);
                    let mut placements = vec![
                        (
                            Position::new(1, 4),
                            State::new(Actor::Murphy(Murphy::new())),
                        ),
                        (origin, State::new(actor.clone())),
                        (
                            Position::new(3, 3),
                            State::new(Actor::Zonk(Zonk::resting())),
                        ),
                        (
                            Position::new(3, 4),
                            State::new(Actor::Hardware(Hardware::new(0))),
                        ),
                    ];
                    if direction == Direction::Right {
                        placements.push((
                            Position::new(2, 2),
                            State::new(Actor::Hardware(Hardware::new(0))),
                        ));
                    }
                    let mut game = game_with(&placements, 0);

                    game.tick(Input::default());
                    assert_eq!(
                        game.board()
                            .state(origin)
                            .expect("the rounded owner should retain its source")
                            .snapshot()
                            .label(),
                        format!("RoundedPreRoll({:?})", direction)
                    );

                    let (owner, reservation, blast_center, expected_kind) = if rolling {
                        game.tick(Input::default());
                        match direction {
                            Direction::Left => (
                                Position::new(2, 2),
                                Position::new(2, 3),
                                Position::new(1, 1),
                                format!("Rolling({:?})", Direction::Left),
                            ),
                            Direction::Right => (
                                Position::new(4, 2),
                                Position::new(4, 3),
                                Position::new(5, 1),
                                format!("Rolling({:?})", Direction::Right),
                            ),
                            _ => unreachable!("the fixture uses horizontal rolls only"),
                        }
                    } else {
                        match direction {
                            Direction::Left => (
                                origin,
                                Position::new(2, 2),
                                Position::new(4, 1),
                                format!("RoundedPreRoll({:?})", Direction::Left),
                            ),
                            Direction::Right => (
                                origin,
                                Position::new(4, 2),
                                Position::new(2, 1),
                                format!("RoundedPreRoll({:?})", Direction::Right),
                            ),
                            _ => unreachable!("the fixture uses horizontal rolls only"),
                        }
                    };
                    assert_eq!(
                        game.board()
                            .state(owner)
                            .expect("the rounded owner should occupy its phase cell")
                            .snapshot()
                            .label(),
                        expected_kind
                    );
                    assert!(
                        !game
                            .board()
                            .state(reservation)
                            .expect("the rounded reservation should exist")
                            .is_empty()
                    );

                    game.detonate_position(blast_center);
                    assert!(matches!(
                        actor_at(&game, owner.x, owner.y),
                        Actor::Explosion(_)
                    ));
                    assert!(
                        game.board()
                            .state(reservation)
                            .expect("the released reservation remains addressable")
                            .is_empty(),
                        "actor {actor:?}, direction {direction:?}, rolling {rolling}"
                    );
                }
            }
        }
    }

    /// Confirms reservation cleanup yields to explosion writes, including a
    /// marker already consumed by an earlier live-board wave.
    #[test]
    fn rounded_cleanup_never_erases_explosion_cells() {
        let source = Position::new(3, 2);
        let destination = Position::new(3, 3);
        let mut falling = game_with(
            &[
                (
                    Position::new(5, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (source, State::new(Actor::Infotron(Infotron::resting()))),
                (
                    Position::new(3, 4),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );
        falling.tick(Input::default());
        falling.tick(Input::default());
        falling.detonate_position(destination);
        assert!(matches!(actor_at(&falling, 3, 2), Actor::Explosion(_)));

        let owner = Position::new(3, 2);
        let side = Position::new(2, 2);
        let mut rounded = game_with(
            &[
                (
                    Position::new(5, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (owner, State::new(Actor::Infotron(Infotron::resting()))),
                (
                    Position::new(3, 3),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 4),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );
        rounded.tick(Input::default());
        rounded.detonate_position(Position::new(1, 3));
        assert!(matches!(
            actor_at(&rounded, side.x, side.y),
            Actor::Explosion(_)
        ));
        assert!(matches!(actor_at(&rounded, 3, 2), Actor::Infotron(_)));

        rounded.detonate_position(Position::new(4, 1));
        assert!(matches!(
            actor_at(&rounded, side.x, side.y),
            Actor::Explosion(_)
        ));
    }

    /// Confirms an Infotron landing on any idle disk detonates immediately.
    #[test]
    fn falling_infotron_detonates_an_idle_red_disk() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::RedDisk(RedDisk::Collectible)),
                ),
            ],
            0,
        );

        for _ in 0..10 {
            game.tick(Input::default());
        }

        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Explosion(_)));
    }

    /// Confirms a falling Infotron also detonates a visible planted fuse.
    #[test]
    fn falling_infotron_detonates_an_active_red_disk() {
        let disk_position = Position::new(3, 3);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Infotron(Infotron::resting())),
                ),
                (disk_position, State::planted_red_disk(5)),
            ],
            0,
        );
        game.planted_red_disk = Some(PlantedRedDisk {
            position: disk_position,
            countdown: 5,
        });

        for _ in 0..10 {
            game.tick(Input::default());
        }

        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
        assert!(game.planted_red_disk.is_none());
    }

    /// Confirms an Electron crushed at the seed leaves Infotron residue.
    #[test]
    fn crushed_electron_upgrades_the_center_blast_residue() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 1),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 1),
                    State::new(Actor::Zonk(Zonk::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::Electron(Electron::new(Direction::Left))),
                ),
            ],
            0,
        );
        game.freeze_enemies = true;

        for _ in 0..10 {
            game.tick(Input::default());
        }

        let Actor::Explosion(explosion) = actor_at(&game, 3, 3) else {
            panic!("crushed Electron should become an explosion");
        };
        assert_eq!(explosion.residue(), ExplosionResidue::Infotron);
    }

    /// Confirms a later wave overwrites an earlier visual residue in scan order.
    #[test]
    fn overlapping_later_blast_replaces_electron_residue() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::Electron(Electron::new(Direction::Left))),
                ),
            ],
            0,
        );

        game.detonate_position(Position::new(2, 2));
        game.detonate_position(Position::new(4, 2));

        let Actor::Explosion(explosion) = actor_at(&game, 3, 2) else {
            panic!("overlap should remain an explosion");
        };
        assert_eq!(explosion.residue(), ExplosionResidue::Empty);
    }

    /// Confirms a touched chain actor waits for its independent timer to expire.
    #[test]
    fn chain_reaction_waits_for_its_independent_timer() {
        let mut game = game_with(
            &[
                (
                    Position::new(4, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
            ],
            0,
        );

        game.detonate_position(Position::new(2, 2));
        assert_eq!(game.status(), GameStatus::Playing);
        let chain_index = game
            .board
            .index(Position::new(3, 2))
            .expect("Orange Disk chain center should be in bounds");
        assert_eq!(game.explosion_timers[chain_index], 13);

        for _ in 0..CHAIN_REACTION_FRAMES - 1 {
            game.tick(Input::default());
        }
        assert_eq!(game.status(), GameStatus::Playing);

        game.tick(Input::default());
        assert_eq!(game.status(), GameStatus::Dead);
        assert_eq!(game.explosion_timers[chain_index], 0);
        assert!(matches!(actor_at(&game, 4, 2), Actor::Explosion(_)));
    }

    /// Confirms a completed Red Disk placement uses countdown values two through forty.
    #[test]
    fn planted_red_disk_detonates_at_original_countdown_value() {
        let disk_position = Position::new(3, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (disk_position, State::planted_red_disk(2)),
            ],
            0,
        );
        game.planted_red_disk = Some(PlantedRedDisk {
            position: disk_position,
            countdown: 2,
        });

        // Thirty-seven updates leave countdown 39 and the disk intact.
        for _ in 0..37 {
            game.tick(Input::default());
        }
        assert!(matches!(actor_at(&game, 3, 2), Actor::RedDisk(_)));
        assert_eq!(
            game.planted_red_disk
                .expect("fuse should remain active before value forty")
                .countdown,
            39
        );

        game.tick(Input::default());
        assert!(game.planted_red_disk.is_none());
        assert!(matches!(actor_at(&game, 3, 2), Actor::Explosion(_)));
    }

    /// Confirms adjacent delayed centers retain independent timer-array entries.
    #[test]
    fn same_deadline_chain_centers_expire_independently_of_visual_frames() {
        let mut game = game_with(
            &[
                (
                    Position::new(5, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
                (
                    Position::new(2, 3),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
            ],
            0,
        );

        game.detonate_position(Position::new(2, 2));
        let first_index = game
            .board
            .index(Position::new(3, 2))
            .expect("first chain center should be in bounds");
        let second_index = game
            .board
            .index(Position::new(2, 3))
            .expect("second chain center should be in bounds");
        assert_eq!(game.explosion_timers[first_index], 13);
        assert_eq!(game.explosion_timers[second_index], 13);

        for _ in 0..CHAIN_REACTION_FRAMES {
            game.tick(Input::default());
        }
        assert_eq!(game.explosion_timers[first_index], 0);
        assert_eq!(game.explosion_timers[second_index], 0);

        // The timer expiry emitted both waves even though their independently
        // quarter-paced visual cells are still in progress.
        assert!(matches!(actor_at(&game, 3, 2), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 2, 3), Actor::Explosion(_)));
    }

    /// Confirms sequential overlapping blasts retain both outer edges.
    #[test]
    fn sequential_orange_disk_blasts_retain_their_full_extents() {
        let mut game = game_with(
            &[
                (
                    Position::new(3, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 1),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
                (
                    Position::new(4, 1),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
                (
                    Position::new(2, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
                (
                    Position::new(4, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        for _ in 0..11 {
            game.tick(Input::default());
        }

        assert!(matches!(actor_at(&game, 1, 2), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 5, 2), Actor::Explosion(_)));
    }

    /// Confirms an Exit is gated by the remaining collectible count.
    #[test]
    fn exit_completes_only_after_requirement_is_zero() {
        let placements = [
            (
                Position::new(2, 2),
                State::new(Actor::Murphy(Murphy::new())),
            ),
            (Position::new(3, 2), State::new(Actor::Exit(Exit))),
        ];
        let mut locked = game_with(&placements, 1);
        let mut open = game_with(&placements, 0);
        let input = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };

        locked.tick(input);
        open.tick(input);

        assert_eq!(locked.status(), GameStatus::Playing);
        assert!(!locked.terminal_transition_ready());
        assert!(locked.take_sound_effects().is_empty());
        assert_eq!(open.status(), GameStatus::Completed);
        assert!(!open.terminal_transition_ready());
        assert_eq!(open.take_sound_effects(), vec![SoundEffect::Exit]);
        assert_eq!(
            open.board()
                .state(Position::new(2, 2))
                .expect("Exit animation should remain at Murphy's source")
                .snapshot()
                .label(),
            format!("Murphy({:?})", MurphyAnimation::Exit)
        );
        // The terminal status does not freeze the disappearance sequence.
        for _ in 0..39 {
            open.tick(Input::default());
        }
        assert_eq!(open.status(), GameStatus::Completed);
        assert!(open.murphy_position().is_none());
        for _ in 0..24 {
            open.tick(Input::default());
        }
        assert!(open.terminal_transition_ready());
    }

    /// Confirms a player-requested death uses the complete normal explosion lifecycle.
    #[test]
    fn destroying_murphy_uses_the_collision_explosion_path() {
        let murphy_position = Position::new(3, 2);
        let neighbor_position = Position::new(4, 2);
        let mut game = game_with(
            &[
                (murphy_position, State::new(Actor::Murphy(Murphy::new()))),
                (neighbor_position, State::new(Actor::Base(Base::Resting))),
            ],
            0,
        );

        game.destroy_murphy();

        // Direct destruction creates the blast immediately but leaves the
        // complete sixty-four-update terminal delay for subsequent ticks.
        assert_eq!(game.status(), GameStatus::Dead);
        assert_eq!(game.quit_countdown, 0x40);
        assert_eq!(game.take_sound_effects(), vec![SoundEffect::Explosion]);
        assert!(matches!(
            actor_at(&game, murphy_position.x, murphy_position.y),
            Actor::Explosion(_)
        ));
        assert!(matches!(
            actor_at(&game, neighbor_position.x, neighbor_position.y),
            Actor::Explosion(_)
        ));

        // Repeated Escape requests during the death sequence must neither
        // restart its countdown nor enqueue duplicate explosion sounds.
        game.destroy_murphy();
        assert_eq!(game.quit_countdown, 0x40);
        assert!(game.take_sound_effects().is_empty());

        game.tick(Input::default());
        assert_eq!(game.quit_countdown, 0x3f);
    }

    /// Confirms Murphy cannot enter an enemy cell without being destroyed.
    #[test]
    fn touching_a_snik_snak_immediately_destroys_murphy() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::SnikSnak(SnikSnak::new(Direction::Left))),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert_eq!(game.status(), GameStatus::Dead);
        assert_eq!(game.take_sound_effects(), vec![SoundEffect::Explosion]);
        assert_eq!(game.quit_countdown, 0x3f);
        assert!(matches!(actor_at(&game, 2, 2), Actor::Explosion(_)));

        let first_frame = game
            .board()
            .state(Position::new(2, 2))
            .expect("blast cell should exist")
            .snapshot()
            .frame();
        // Explosion pictures advance only on global ticks divisible by four.
        game.tick(Input::default());
        let second_frame = game
            .board()
            .state(Position::new(2, 2))
            .expect("blast cell should exist")
            .snapshot()
            .frame();
        assert_eq!(second_frame, first_frame);
        for _ in 0..3 {
            game.tick(Input::default());
        }
        let quarter_frame = game
            .board()
            .state(Position::new(2, 2))
            .expect("blast cell should survive through its next quarter tick")
            .snapshot()
            .frame();
        assert!(quarter_frame > second_frame);
    }

    /// Confirms only the late half of a regular explosion is traversable.
    #[test]
    fn murphy_enters_mature_regular_explosions_but_not_lethal_blasts() {
        let murphy = Position::new(3, 3);
        let target = Position::new(3, 2);
        let movement = Input {
            direction: Some(Direction::Up),
            ..Input::default()
        };
        let fixture = |residue| {
            game_with(
                &[
                    (murphy, State::new(Actor::Murphy(Murphy::new()))),
                    (
                        target,
                        State::new(Actor::Explosion(Explosion::new(residue))),
                    ),
                ],
                0,
            )
        };

        let mut young_regular = fixture(ExplosionResidue::Empty);
        young_regular.tick(movement);
        assert_eq!(young_regular.status(), GameStatus::Dead);

        let mut mature_regular = fixture(ExplosionResidue::Empty);
        // Explosion state advances on ticks 0, 4, 8, and 12. Raw state four is
        // the first value the original collision helper erases as harmless.
        for _ in 0..13 {
            mature_regular.tick(Input::default());
        }
        assert_eq!(
            mature_regular
                .board()
                .state(target)
                .expect("the mature explosion should remain present")
                .snapshot()
                .frame(),
            4
        );
        mature_regular.tick(movement);
        assert_eq!(mature_regular.status(), GameStatus::Playing);
        assert_eq!(mature_regular.murphy_position(), Some(target));

        let mut electron_blast = fixture(ExplosionResidue::Infotron);
        // Electron explosions retain the high bit in every visible state and
        // therefore remain lethal even after their fourth animation frame.
        for _ in 0..13 {
            electron_blast.tick(Input::default());
        }
        electron_blast.tick(movement);
        assert_eq!(electron_blast.status(), GameStatus::Dead);
    }

    /// Confirms accidental tile 40 blocks Murphy without gaining a reveal state.
    #[test]
    fn invisible_wall_remains_invisible_and_solid_after_contact() {
        let wall = Position::new(3, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (wall, State::new(Actor::InvisibleWall(InvisibleWall))),
            ],
            0,
        );
        let press_wall = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };

        game.tick(press_wall);
        game.tick(press_wall);

        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::InvisibleWall(_)));
        assert_eq!(
            game.board()
                .state(wall)
                .expect("invisible wall should remain addressable")
                .snapshot()
                .label(),
            "Idle"
        );
    }

    /// Confirms a Snik Snak rotates on quarter ticks before reserving a step.
    #[test]
    fn snik_snak_turns_before_moving_and_releases_its_source_on_frame_seven() {
        let source = Position::new(3, 3);
        let left_destination = Position::new(2, 3);
        let mut game = game_with(
            &[
                (
                    Position::new(5, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    source,
                    State::new(Actor::SnikSnak(SnikSnak::new(Direction::Right))),
                ),
            ],
            0,
        );

        // State zero advances on tick zero; the first actionable even state is
        // reached on tick seven. No instant wall-following choice is allowed.
        for _ in 0..7 {
            game.tick(Input::default());
        }
        assert!(matches!(
            actor_at(&game, source.x, source.y),
            Actor::SnikSnak(_)
        ));
        game.tick(Input::default());
        assert!(matches!(
            actor_at(&game, left_destination.x, left_destination.y),
            Actor::SnikSnak(_)
        ));
        assert_eq!(
            game.board()
                .state(left_destination)
                .expect("moving Snik Snak destination should exist")
                .snapshot()
                .label(),
            format!("SnikSnakMove({:?})", Direction::Left)
        );

        for _ in 0..6 {
            game.tick(Input::default());
        }
        assert!(
            !game
                .board()
                .state(source)
                .expect("source reservation should exist through frame six")
                .is_empty()
        );
        game.tick(Input::default());
        assert!(
            game.board()
                .state(source)
                .expect("source cell should remain addressable")
                .is_empty()
        );
    }

    /// Confirms side contact waits until the matching Snik Snak turn state.
    #[test]
    fn snik_snak_attacks_only_the_direction_of_its_current_turn_frame() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 3),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::SnikSnak(SnikSnak::new(Direction::Right))),
                ),
            ],
            0,
        );

        // Murphy is adjacent from the beginning, but frame zero's intervening
        // rotation must complete before frame two tests the left-hand cell.
        for _ in 0..7 {
            game.tick(Input::default());
            assert_eq!(game.status(), GameStatus::Playing);
        }
        game.tick(Input::default());
        assert_eq!(game.status(), GameStatus::Dead);
        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
    }

    /// Confirms Electrons use the original left-hand turn and transfer phases.
    #[test]
    fn electron_uses_left_hand_turning_and_destination_owned_source_cleanup() {
        let source = Position::new(3, 3);
        let left_destination = Position::new(2, 3);
        let mut game = game_with(
            &[
                (
                    Position::new(5, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    source,
                    State::new(Actor::Electron(Electron::new(Direction::Right))),
                ),
            ],
            0,
        );

        for _ in 0..7 {
            game.tick(Input::default());
        }
        assert!(matches!(
            actor_at(&game, source.x, source.y),
            Actor::Electron(_)
        ));
        game.tick(Input::default());
        assert_eq!(
            game.board()
                .state(left_destination)
                .expect("moving Electron destination should exist")
                .snapshot()
                .label(),
            format!("ElectronMove({:?})", Direction::Left)
        );

        for _ in 0..6 {
            game.tick(Input::default());
        }
        assert_eq!(
            game.board()
                .state(source)
                .expect("Electron source reservation should exist")
                .snapshot()
                .label(),
            format!("ElectronVacating({:?})", Direction::Left)
        );
        game.tick(Input::default());
        assert!(
            game.board()
                .state(source)
                .expect("released Electron source should remain addressable")
                .is_empty()
        );
    }

    /// Confirms only Snik Snaks exempt Murphy's original port movement states.
    #[test]
    fn electron_turn_contact_detonates_during_murphy_port_traversal() {
        let placements = |enemy| {
            [
                (
                    Position::new(3, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(4, 2),
                    State::new(Actor::Port(Port::new(
                        PortDirections::OneWay(Direction::Right),
                        false,
                    ))),
                ),
                (Position::new(3, 3), State::new(enemy)),
            ]
        };
        let mut snik_snak = game_with(
            &placements(Actor::SnikSnak(SnikSnak::new(Direction::Right))),
            0,
        );
        let mut electron = game_with(
            &placements(Actor::Electron(Electron::new(Direction::Right))),
            0,
        );
        snik_snak.tick = 3;
        electron.tick = 3;
        let enter_port = Input {
            direction: Some(Direction::Right),
            ..Input::default()
        };

        snik_snak.tick(enter_port);
        electron.tick(enter_port);

        assert_eq!(snik_snak.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&snik_snak, 3, 3), Actor::SnikSnak(_)));
        assert_eq!(electron.status(), GameStatus::Dead);
        let Actor::Explosion(center) = actor_at(&electron, 3, 3) else {
            panic!("Electron contact should replace its center with an explosion");
        };
        assert_eq!(center.residue(), ExplosionResidue::Infotron);
    }

    /// Confirms special-port metadata is applied by the completed traversal.
    #[test]
    fn crossing_a_special_port_updates_global_toggles() {
        let port_position = Position::new(3, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    port_position,
                    State::new(Actor::Port(Port::new(
                        PortDirections::OneWay(Direction::Right),
                        true,
                    ))),
                ),
            ],
            0,
        );
        game.special_ports.push(SpecialPort {
            x: port_position.x,
            y: port_position.y,
            gravity: true,
            freeze_zonks: true,
            freeze_enemies: true,
        });

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        assert_eq!(
            game.board()
                .state(Position::new(2, 2))
                .expect("port source should retain Murphy")
                .snapshot()
                .label(),
            format!(
                "Murphy({:?})",
                MurphyAnimation::Port {
                    direction: Direction::Right,
                }
            )
        );
        assert!(!game.gravity());
        assert!(!game.freeze_zonks());
        assert!(!game.freeze_enemies());

        for _ in 0..7 {
            game.tick(Input::default());
        }
        assert!(matches!(actor_at(&game, 4, 2), Actor::Murphy(_)));
        assert!(game.gravity());
        assert!(game.freeze_zonks());
        assert!(game.freeze_enemies());
    }

    /// Confirms a Terminal expands every Yellow Disk into a 3×3 blast.
    #[test]
    fn terminal_detonates_yellow_disks() {
        let mut game = game_with(
            &[
                (
                    Position::new(2, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
                (
                    Position::new(4, 3),
                    State::new(Actor::YellowDisk(YellowDisk::Resting)),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 4, 3), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
    }

    /// Confirms Terminal scrolling waits on a signed counter between RNG draws.
    #[test]
    fn terminal_scrolls_with_original_randomized_signed_delay() {
        let terminal_position = Position::new(2, 2);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    terminal_position,
                    State::new(Actor::Terminal(Terminal::new())),
                ),
            ],
            0,
        );

        // Seed zero advances to 49 and returns 24.  With the initial 0x7f
        // mask, the Terminal therefore stores -24 after its first scroll.
        game.tick(Input::default());
        let Actor::Terminal(terminal) = actor_at(&game, 2, 2) else {
            panic!("terminal should remain in place after scrolling");
        };
        assert_eq!(terminal.screen_frame(), 1);
        assert_eq!(game.random_seed, 49);

        // Twenty-four increments reach zero without consuming the generator.
        for _ in 0..24 {
            game.tick(Input::default());
        }
        assert_eq!(game.random_seed, 49);

        // The following update increments zero to one, scrolls, and draws the
        // next value from the same stream that later Bugs will consume.
        game.tick(Input::default());
        let Actor::Terminal(terminal) = actor_at(&game, 2, 2) else {
            panic!("terminal should remain in place after its second scroll");
        };
        assert_eq!(terminal.screen_frame(), 2);
        assert_ne!(game.random_seed, 49);
    }

    /// Confirms activation shortens future Terminal waits without resetting them.
    #[test]
    fn terminal_activation_uses_the_post_detonation_delay_mask() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert_eq!(game.terminal_delay_mask, 7);

        // The first generated value is 24; masking it with seven produces zero,
        // so the activated panel is immediately eligible again next update.
        let Actor::Terminal(first) = actor_at(&game, 2, 2) else {
            panic!("activated terminal should remain in place");
        };
        assert_eq!(first.screen_frame(), 1);
        game.tick(Input::default());
        let Actor::Terminal(second) = actor_at(&game, 2, 2) else {
            panic!("activated terminal should continue scrolling");
        };
        assert_eq!(second.screen_frame(), 2);
    }

    /// Confirms a touched explosive actor contributes its own 3×3 chain wave.
    #[test]
    fn terminal_blast_chains_through_an_adjacent_orange_disk() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 4),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::YellowDisk(YellowDisk::Resting)),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                ),
                (
                    Position::new(3, 3),
                    State::new(Actor::Hardware(Hardware::new(0))),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        // The Yellow Disk's own wave ends at x=3. The touched Orange Disk
        // promises a delayed wave rather than recursively expanding at once.
        assert!(matches!(actor_at(&game, 4, 2), Actor::Empty(_)));
        for _ in 0..CHAIN_REACTION_FRAMES {
            game.tick(Input::default());
        }
        assert!(matches!(actor_at(&game, 4, 2), Actor::Explosion(_)));
    }

    /// Confirms a Terminal records activation and cannot detonate a later disk.
    #[test]
    fn terminal_activation_is_one_shot() {
        let terminal_position = Position::new(2, 2);
        let yellow_position = Position::new(5, 3);
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    terminal_position,
                    State::new(Actor::Terminal(Terminal::new())),
                ),
                (
                    yellow_position,
                    State::new(Actor::YellowDisk(YellowDisk::Resting)),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        let Actor::Terminal(terminal) = actor_at(&game, 2, 2) else {
            panic!("terminal should survive its remote detonation");
        };
        assert!(terminal.is_activated());

        // Let Murphy settle and the first remote wave disappear, then place a
        // fresh idle disk where a repeated activation would visibly destroy it.
        for _ in 0..8 {
            game.tick(Input::default());
        }
        game.board
            .set(
                yellow_position,
                State::new(Actor::YellowDisk(YellowDisk::Resting)),
            )
            .expect("fixture position should remain valid");
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        assert!(matches!(actor_at(&game, 5, 3), Actor::YellowDisk(_)));
    }

    /// Confirms touching one of several Terminals consumes the level-wide latch.
    #[test]
    fn one_terminal_activates_every_terminal_in_the_level() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
                (
                    Position::new(4, 3),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });

        for position in [Position::new(2, 2), Position::new(4, 3)] {
            let Actor::Terminal(terminal) = game
                .board()
                .state(position)
                .expect("terminal fixture should exist")
                .actor()
            else {
                panic!("terminal should survive an empty detonation event");
            };
            assert!(terminal.is_activated());
        }
    }

    /// Confirms a Yellow blast defers a later adjacent Yellow in scan order.
    #[test]
    fn terminal_scans_yellow_disks_against_each_prior_live_wave() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (
                    Position::new(2, 4),
                    State::new(Actor::Terminal(Terminal::new())),
                ),
                (
                    Position::new(2, 2),
                    State::new(Actor::YellowDisk(YellowDisk::Resting)),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::YellowDisk(YellowDisk::Resting)),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        let pending_position = Position::new(3, 2);
        let pending_index = game
            .board
            .index(pending_position)
            .expect("later Yellow center should be in bounds");

        // The first primary wave converted the later Yellow before the live
        // row-major Yellow scan reached it. Its independent timer was installed
        // at thirteen and decremented once by the same iteration's timer pass.
        assert!(matches!(actor_at(&game, 2, 2), Actor::Explosion(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Explosion(_)));
        assert_eq!(game.explosion_timers[pending_index], 12);

        for _ in 0..11 {
            game.tick(Input::default());
        }
        assert_eq!(game.explosion_timers[pending_index], 1);
        game.tick(Input::default());
        assert_eq!(game.explosion_timers[pending_index], 0);
        let emitted = game
            .board()
            .state(pending_position)
            .expect("secondary center should exist");
        assert!(matches!(emitted.actor(), Actor::Explosion(_)));
        assert_eq!(emitted.snapshot().frame_count(), 8);
    }

    /// Confirms the engine's explicit empty actor is available for fixtures.
    #[test]
    fn empty_actor_has_no_self_transition() {
        let game = game_with(
            &[(
                Position::new(1, 1),
                State::new(Actor::Murphy(Murphy::new())),
            )],
            0,
        );
        assert!(matches!(Actor::Empty(Empty::Space), Actor::Empty(_)));
        assert_eq!(game.tick_count(), 0);
    }

    /// Confirms every Bug begins active and advances only every fourth tick.
    #[test]
    fn bugs_begin_synchronized_on_the_global_quarter_tick_cadence() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::new(Actor::Bug(Bug::new()))),
                (Position::new(4, 2), State::new(Actor::Bug(Bug::new()))),
            ],
            0,
        );

        for position in [Position::new(2, 2), Position::new(4, 2)] {
            let state = game.board().state(position).expect("Bug should exist");
            assert_eq!(state.snapshot().label(), "Bug");
            assert_eq!(state.snapshot().frame(), 0);
        }

        // Global frame zero is eligible, while frames one through three hold.
        game.tick(Input::default());
        for _ in 0..3 {
            game.tick(Input::default());
        }
        for position in [Position::new(2, 2), Position::new(4, 2)] {
            assert_eq!(
                game.board()
                    .state(position)
                    .expect("Bug should remain present")
                    .snapshot()
                    .frame(),
                1
            );
        }

        game.tick(Input::default());
        for position in [Position::new(2, 2), Position::new(4, 2)] {
            assert_eq!(
                game.board()
                    .state(position)
                    .expect("Bug should remain present")
                    .snapshot()
                    .frame(),
                2
            );
        }
    }

    /// Confirms synchronized Bugs draw distinct row-major cooldown durations.
    #[test]
    fn bugs_randomize_independent_delays_after_the_shared_first_cycle() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::new(Actor::Bug(Bug::new()))),
                (Position::new(4, 2), State::new(Actor::Bug(Bug::new()))),
            ],
            0,
        );
        game.random_seed = 0;

        // Eligible frames 0,4,...,48 reach active frame 13. Frame 52 then
        // consumes consecutive values from the one shared 16-bit RNG stream.
        for _ in 0..53 {
            game.tick(Input::default());
        }

        let first = game
            .board()
            .state(Position::new(2, 2))
            .expect("first Bug should exist")
            .snapshot();
        let second = game
            .board()
            .state(Position::new(4, 2))
            .expect("second Bug should exist")
            .snapshot();
        assert_eq!(first.label(), "BugDormant");
        assert_eq!(second.label(), "BugDormant");
        assert_eq!(first.frame_count(), 56);
        assert_eq!(second.frame_count(), 35);
    }

    /// Confirms active frame thirteen remains lethal before cooldown scheduling.
    #[test]
    fn bug_final_active_frame_is_still_lethal_to_murphy() {
        let mut game = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::new(Actor::Bug(Bug::new()))),
            ],
            0,
        );

        for _ in 0..52 {
            game.tick(Input::default());
        }
        let bug = game
            .board()
            .state(Position::new(2, 2))
            .expect("Bug should exist before cooldown");
        assert_eq!(bug.snapshot().label(), "Bug");
        assert_eq!(bug.snapshot().frame(), 13);

        // Murphy updates first on the due tick and therefore still collides
        // with active frame 13 before the Bug randomizes its safe interval.
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert_eq!(game.status(), GameStatus::Dead);
    }

    /// Confirms a dormant Bug is safe and reactivates only after Murphy updates.
    #[test]
    fn dormant_bug_is_safe_through_its_activation_boundary() {
        let mut escapable = game_with(
            &[
                (
                    Position::new(1, 2),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::dormant_bug(1)),
            ],
            0,
        );
        escapable.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert_eq!(escapable.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&escapable, 2, 2), Actor::Murphy(_)));

        let mut reactivating = game_with(
            &[
                (
                    Position::new(1, 4),
                    State::new(Actor::Murphy(Murphy::new())),
                ),
                (Position::new(2, 2), State::dormant_bug(1)),
            ],
            0,
        );
        reactivating.tick(Input::default());
        let active = reactivating
            .board()
            .state(Position::new(2, 2))
            .expect("Bug should reactivate in place");
        assert_eq!(active.snapshot().label(), "Bug");
        assert_eq!(active.snapshot().frame(), 0);
    }

    /// Confirms every supplied tile code maps to a typed actor in all 111 levels.
    #[test]
    fn every_bundled_level_initializes_as_a_game() {
        let levels = LevelSet::new(ORIGINAL_LEVELS);

        for level_number in 1..=111 {
            let level = levels
                .load(level_number)
                .expect("bundled record should parse");
            let game = Game::new(&level).expect("bundled tiles should map to actor states");
            assert_eq!(game.board().cells().len(), 60 * 24);
            assert!(game.murphy_position().is_some());
        }
    }

    /// Confirms the first original level's documented title and requirement.
    #[test]
    fn bundled_level_one_keeps_its_original_metadata() {
        let level = LevelSet::new(ORIGINAL_LEVELS)
            .load(1)
            .expect("first bundled record should parse");
        let game = Game::new(&level).expect("first bundled level should initialize");

        assert_eq!(game.title(), "------- WARM UP -------");
        assert_eq!(game.remaining_infotrons(), 19);
        assert_eq!(game.murphy_position(), Some(Position::new(30, 20)));
    }

    /// Reproduces every branch of the original enemy conversion pass.
    #[test]
    fn loaded_enemies_derive_their_initial_state_from_live_neighbors() {
        let snik_turn = Position::new(3, 3);
        let electron_blocked = Position::new(8, 3);
        let snik_source = Position::new(13, 4);
        let snik_destination = Position::new(13, 3);
        let electron_source = Position::new(18, 4);
        let electron_destination = Position::new(19, 4);
        let board = loaded_board_with_tiles(&[
            // A stable Space on the left has priority over every other route
            // and selects raw turn state one without moving the enemy.
            (snik_turn, 17),
            (Position::new(2, 3), 0),
            // With no free neighbor, an Electron retains raw turn state zero.
            (electron_blocked, 24),
            // A free cell above has priority over a free cell on the right and
            // starts the Up transfer before the first simulation callback.
            (snik_source, 17),
            (snik_destination, 0),
            (Position::new(14, 4), 0),
            // When left and above are blocked, a free right cell starts the
            // corresponding pre-play Electron transfer.
            (electron_source, 24),
            (electron_destination, 0),
        ]);

        let snik_turn_state = board
            .state(snik_turn)
            .expect("the stationary Snik Snak should remain at its source");
        assert!(matches!(snik_turn_state.actor(), Actor::SnikSnak(_)));
        assert_eq!(
            snik_turn_state.snapshot().label(),
            format!("SnikSnakTurn({:?})", EnemyTurn::Left)
        );
        assert_eq!(snik_turn_state.snapshot().frame(), 1);

        let blocked_state = board
            .state(electron_blocked)
            .expect("the blocked Electron should remain at its source");
        assert!(matches!(blocked_state.actor(), Actor::Electron(_)));
        assert_eq!(
            blocked_state.snapshot().label(),
            format!("ElectronTurn({:?})", EnemyTurn::Left)
        );
        assert_eq!(blocked_state.snapshot().frame(), 0);

        let snik_source_state = board
            .state(snik_source)
            .expect("the Snik Snak source reservation should remain in bounds");
        assert!(matches!(snik_source_state.actor(), Actor::Empty(_)));
        assert_eq!(
            snik_source_state.snapshot().label(),
            format!("SnikSnakVacating({:?})", Direction::Up)
        );
        let snik_destination_state = board
            .state(snik_destination)
            .expect("the Snik Snak destination should remain in bounds");
        assert!(matches!(
            snik_destination_state.actor(),
            Actor::SnikSnak(enemy) if enemy.heading() == Direction::Up
        ));
        assert_eq!(
            snik_destination_state.snapshot().label(),
            format!("SnikSnakMove({:?})", Direction::Up)
        );
        assert_eq!(snik_destination_state.snapshot().frame(), 0);

        let electron_source_state = board
            .state(electron_source)
            .expect("the Electron source reservation should remain in bounds");
        assert!(matches!(electron_source_state.actor(), Actor::Empty(_)));
        assert_eq!(
            electron_source_state.snapshot().label(),
            format!("ElectronVacating({:?})", Direction::Right)
        );
        let electron_destination_state = board
            .state(electron_destination)
            .expect("the Electron destination should remain in bounds");
        assert!(matches!(
            electron_destination_state.actor(),
            Actor::Electron(enemy) if enemy.heading() == Direction::Right
        ));
        assert_eq!(
            electron_destination_state.snapshot().label(),
            format!("ElectronMove({:?})", Direction::Right)
        );
        assert_eq!(electron_destination_state.snapshot().frame(), 0);
    }

    /// Replays the original successful level-one attract demo to its Exit.
    #[test]
    fn original_demo_zero_remains_a_successful_solution() {
        let level_number = usize::from(ORIGINAL_DEMO_ZERO[0]);
        let level = LevelSet::new(ORIGINAL_LEVELS)
            .load(level_number)
            .expect("the original demo's level should remain available");
        // Legacy demos predate the embedded SpeedFix seed and consequently use
        // the zero-initialized entry in the original demo seed table.
        let mut game =
            Game::with_random_seed(&level, 0).expect("the original demo's level should initialize");

        for encoded in ORIGINAL_DEMO_ZERO[1..]
            .iter()
            .copied()
            .take_while(|byte| *byte != 0xff)
        {
            // The high nibble stores repeat count minus one. Feed every decoded
            // sample through the public simulation boundary so this remains an
            // end-to-end scheduler, actor, interaction, and animation check.
            let input = original_demo_input(encoded & 0x0f);
            for _ in 0..=encoded >> 4 {
                game.tick(input);
            }
        }

        assert_eq!(game.status(), GameStatus::Completed);
        assert_eq!(game.remaining_infotrons(), 0);
    }
}
