//! Row-major board storage and deterministic application of actor transitions.

use std::{
    error::Error,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    actor::{
        Actor, AnimationKind, Base, Bug, Direction, Electron, Empty, Exit, GameEvent, Hardware,
        Infotron, InvisibleWall, Murphy, OrangeDisk, Port, PortDirections, Position,
        RED_DISK_FUSE_FRAMES, RamChip, RamChipShape, RedDisk, SnikSnak, State, Terminal,
        Transition, YellowDisk, Zonk,
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
    /// Whether the Red Disk drop key was pressed during this step.
    pub drop_disk: bool,
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
        Self::new(LEVEL_WIDTH, LEVEL_HEIGHT, cells)
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

/// Concealed portion of the single Red Disk fuse planted beneath Murphy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlantedRedDisk {
    /// Board cell where Murphy initiated the planting action.
    position: Position,
    /// Fuse frames already consumed while Murphy still covers the disk.
    elapsed_frames: u8,
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
}

impl Game {
    /// Creates a play session from a decoded original level.
    pub fn new(level: &Level) -> Result<Self, GameError> {
        Self::with_random_seed(level, clock_random_seed())
    }

    /// Creates a play session with an explicit stream seed for tests/replays.
    fn with_random_seed(level: &Level, random_seed: u16) -> Result<Self, GameError> {
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

        Ok(Self {
            board,
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

    /// Returns the number of fixed simulation steps processed.
    pub fn tick_count(&self) -> u64 {
        self.tick
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

    /// Applies one Murphy-first, row-major simulation step to the live board.
    pub fn tick(&mut self, input: Input) {
        // Completion is final. Death still advances only existing Explosion
        // actors so the visible blast can finish without post-mortem physics.
        if self.status == GameStatus::Completed {
            return;
        }

        let playing = self.status == GameStatus::Playing;
        let murphy_source = playing.then(|| self.murphy_position()).flatten();

        // Supaplex always updates Murphy before constructing the linear moving-
        // object schedule. Every later actor therefore sees his completed move,
        // turn, source reservation, or interaction from this same tick.
        if playing
            && let Some(position) = murphy_source
            && let Some(transition) = self.transition_at(position, input)
        {
            self.apply_transition(transition);
        }

        // Capture updater identity once from the live post-Murphy board. This
        // deliberately includes actors Murphy pushed and explosions Murphy
        // created: the original linear scan sees those new tile kinds too. The
        // sole synthetic cell omitted is Murphy's newly Vacating source because
        // original Space tiles do not receive moving-object callbacks.
        let schedule = self
            .board
            .cells()
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                let position = self
                    .board
                    .position(index)
                    .expect("enumerated board indices are always valid");
                let is_new_murphy_source = Some(position) == murphy_source
                    && matches!(state.actor(), Actor::Empty(_))
                    && matches!(state.animation().kind(), AnimationKind::Vacating(_));
                (!is_new_murphy_source
                    && !matches!(state.actor(), Actor::Murphy(_))
                    && (playing || matches!(state.actor(), Actor::Explosion(_))))
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

        if self.status == GameStatus::Playing {
            self.advance_planted_red_disk();
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
        state.actor().transition(&state, position, &world)
    }

    /// Commits every write, then applies events before the next scheduled cell.
    fn apply_transition(&mut self, transition: Transition) {
        // Actor helpers construct only bounded coordinates. Keeping writes in
        // one value ensures no other actor can observe a partially moved actor.
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
            GameEvent::PlantRedDisk(position) => {
                // The transition checks both conditions against the live board;
                // repeat the guards to keep event application total and robust.
                if self.red_disks > 0 && self.planted_red_disk.is_none() {
                    self.red_disks -= 1;
                    self.planted_red_disk = Some(PlantedRedDisk {
                        position,
                        elapsed_frames: 0,
                    });
                }
            }
            GameEvent::Completed => self.status = GameStatus::Completed,
            GameEvent::Died => self.status = GameStatus::Dead,
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
                    self.board
                        .set(position, State::dormant_bug(delay))
                        .expect("scheduled Bug position remains in bounds");
                }
            }
        }
    }

    /// Advances the original wrapping 16-bit generator and returns `seed / 2`.
    fn next_random(&mut self) -> u16 {
        self.random_seed = self.random_seed.wrapping_mul(1509).wrapping_add(49);
        self.random_seed / 2
    }

    /// Expands every idle Yellow Disk into a normal 3×3 explosion immediately.
    fn detonate_yellow_disks(&mut self) {
        // The original latch is level-wide: touching any panel consumes every
        // other panel as well. Persist that fact in each Terminal actor so a
        // later interaction cannot detonate disks skipped while they moved.
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
            self.board
                .set(position, State::new(Actor::Terminal(Terminal::activated())))
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
        planted.elapsed_frames = planted.elapsed_frames.saturating_add(1);

        // The concealed fuse keeps running under Murphy. Reaching its terminal
        // frame detonates the stored position whether or not Murphy escaped.
        if planted.elapsed_frames >= RED_DISK_FUSE_FRAMES {
            self.detonate_position(planted.position);
            return;
        }

        let Some(state) = self.board.state(planted.position) else {
            return;
        };
        match state.actor() {
            Actor::Murphy(_) => self.planted_red_disk = Some(planted),
            Actor::Empty(_) if matches!(state.animation().kind(), AnimationKind::Vacating(_)) => {
                // Murphy's just-vacated source remains collision-reserved until
                // his movement finishes, so the concealed fuse stays hidden.
                self.planted_red_disk = Some(planted);
            }
            Actor::Empty(_) | Actor::RedDisk(_)
                if state.is_empty() || state.animation().kind() == AnimationKind::RedDiskFuse =>
            {
                // A visible State exposes the current animation frame, while
                // this retained record lets Murphy cross without losing time.
                self.board
                    .set(
                        planted.position,
                        State::planted_red_disk(planted.elapsed_frames),
                    )
                    .expect("stored planted-disk position must remain in bounds");
                self.planted_red_disk = Some(planted);
            }
            // Another actor or blast already consumed the concealed disk.
            _ => {}
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
        crate::actor::explode_at(&world, position, electron)
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
            .is_some_and(|state| state.animation().kind() == AnimationKind::Bug)
    }

    /// Finds metadata for the special port occupying one exact board coordinate.
    pub(crate) fn special_port(&self, position: Position) -> Option<&SpecialPort> {
        self.special_ports
            .iter()
            .find(|port| port.x == position.x && port.y == position.y)
    }
}

/// Maps one serialized level byte to a typed actor and default animation.
fn state_from_tile(tile: u8) -> Result<State, BoardError> {
    let actor = match tile {
        0 => Actor::Empty(Empty),
        1 => Actor::Zonk(Zonk::resting()),
        2 => Actor::Base(Base),
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
        18 => Actor::YellowDisk(YellowDisk),
        19 => Actor::Terminal(Terminal::new()),
        20 => Actor::RedDisk(RedDisk),
        21 => Actor::Port(Port::new(PortDirections::Vertical, false)),
        22 => Actor::Port(Port::new(PortDirections::Horizontal, false)),
        23 => Actor::Port(Port::new(PortDirections::Any, false)),
        24 => Actor::Electron(Electron::new(Direction::Left)),
        25 => Actor::Bug(Bug),
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

    use super::{Board, Game, GameStatus, Input, PlantedRedDisk};
    use crate::actor::{
        Actor, AnimationKind, Base, Bug, CHAIN_REACTION_FRAMES, Direction, Electron, Empty, Exit,
        ExplosionResidue, Hardware, Infotron, Murphy, OrangeDisk, Port, PortDirections, Position,
        RedDisk, SnikSnak, State, Terminal, YellowDisk, Zonk,
    };
    use crate::level::{LevelSet, SpecialPort};

    /// Original level-set bytes used for end-to-end initialization checks.
    const ORIGINAL_LEVELS: &[u8] = include_bytes!("../data/levels.dat");

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
        }
    }

    /// Returns the actor at a required fixture coordinate.
    fn actor_at(game: &Game, x: usize, y: usize) -> &Actor {
        game.board()
            .state(Position::new(x, y))
            .expect("fixture coordinate should be in bounds")
            .actor()
    }

    /// Confirms all indexing routes implement exactly `width * y + x`.
    #[test]
    fn board_is_a_single_row_major_vector() {
        let cells = (0..6)
            .map(|_| State::new(Actor::Base(Base)))
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
            source.animation().kind(),
            AnimationKind::Vacating(Direction::Right)
        );
        assert!(!source.is_empty());
        let destination = game
            .board()
            .state(Position::new(3, 2))
            .expect("destination should exist");
        assert!(matches!(destination.actor(), Actor::Murphy(_)));
        assert_eq!(
            destination.animation().kind(),
            AnimationKind::Moving(Direction::Right)
        );

        // Four animation ticks release the synchronized source and retain the
        // final moving pose until Murphy's next input-processing update.
        for _ in 0..4 {
            game.tick(Input::default());
        }
        let completed = game
            .board()
            .state(Position::new(3, 2))
            .expect("destination should exist")
            .animation();
        assert_eq!(completed.kind(), AnimationKind::Moving(Direction::Right));
        assert_eq!(completed.frame(), 3);
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
                .animation()
                .kind(),
            AnimationKind::Idle
        );
    }

    /// Confirms collection is part of Murphy's immediate atomic movement.
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

        assert_eq!(game.remaining_infotrons(), 0);
        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
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

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        let pushed = game
            .board()
            .state(Position::new(4, 2))
            .expect("pushed Zonk destination should exist");
        assert!(matches!(pushed.actor(), Actor::Zonk(_)));
        assert_eq!(
            pushed.animation().kind(),
            AnimationKind::Moving(Direction::Right)
        );
        // Murphy performs the push before schedule capture, so the pushed
        // Zonk receives its normal row-major callback on that same tick.
        assert_eq!(pushed.animation().frame(), 1);
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
        assert_eq!(armed.animation().kind(), AnimationKind::ZonkPreFall);
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
            falling.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(falling.animation().frame(), 0);
        assert!(matches!(actor_at(&game, 3, 3), Actor::Empty(_)));
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

        // The initial pre-fall update, transfer start, and four animation
        // updates complete the first cell while preserving falling momentum.
        for _ in 0..6 {
            game.tick(Input::default());
        }
        let continued = game
            .board()
            .state(Position::new(3, 3))
            .expect("continued fall destination should exist");
        assert!(matches!(continued.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            continued.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(continued.animation().frame(), 0);
        let prior_cell = game
            .board()
            .state(Position::new(3, 2))
            .expect("continued fall source should remain reserved");
        assert_eq!(
            prior_cell.animation().kind(),
            AnimationKind::Vacating(Direction::Down)
        );
        assert!(!prior_cell.is_empty());
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

        // Destination and invisible source must advance as one transfer even
        // though freeze became active after the move began.
        for expected_frame in 1..=3 {
            game.tick(Input::default());
            let source = game
                .board()
                .state(Position::new(3, 1))
                .expect("fall source should exist")
                .animation();
            let destination = game
                .board()
                .state(Position::new(3, 2))
                .expect("fall destination should exist")
                .animation();
            assert_eq!(source.frame(), expected_frame);
            assert_eq!(destination.frame(), expected_frame);
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
                (Position::new(3, 2), State::new(Actor::Base(Base))),
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
        assert_eq!(armed.animation().kind(), AnimationKind::ZonkPreFall);

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
        assert_eq!(still_armed.animation().kind(), AnimationKind::ZonkPreFall);
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

        // One pre-fall step plus four transfer steps leave the Zonk on its final
        // interpolated frame immediately above an otherwise idle Murphy.
        for _ in 0..5 {
            game.tick(Input::default());
        }
        let falling = game
            .board()
            .state(Position::new(3, 2))
            .expect("falling Zonk destination should exist");
        assert!(matches!(falling.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(falling.animation().frame(), 3);

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
            murphy.animation().kind(),
            AnimationKind::Moving(Direction::Right)
        );
        assert_eq!(murphy.animation().frame(), 0);
        let source = game
            .board()
            .state(Position::new(3, 3))
            .expect("Murphy's reserved source should exist");
        assert_eq!(
            source.animation().kind(),
            AnimationKind::Vacating(Direction::Right)
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
        for _ in 0..5 {
            game.tick(down);
        }
        let ready = game
            .board()
            .state(Position::new(3, 3))
            .expect("completed Murphy destination should exist");
        assert!(matches!(ready.actor(), Actor::Murphy(_)));
        assert_eq!(
            ready.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(ready.animation().frame(), 3);
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
        assert_eq!(trailing.animation().kind(), AnimationKind::ZonkPreFall);
        assert_eq!(trailing.animation().frame(), 0);

        // The following update resumes held input first. Only afterward does
        // the armed Zonk begin entering the older, now-safe source cell.
        game.tick(down);

        assert_eq!(game.status(), GameStatus::Playing);
        assert!(matches!(actor_at(&game, 3, 4), Actor::Murphy(_)));
        let moving = game
            .board()
            .state(Position::new(3, 4))
            .expect("Murphy destination should exist")
            .animation();
        assert_eq!(moving.kind(), AnimationKind::Moving(Direction::Down));
        assert_eq!(moving.frame(), 0);
        let trailing = game
            .board()
            .state(Position::new(3, 2))
            .expect("trailing Zonk should enter the older source");
        assert!(matches!(trailing.actor(), Actor::Zonk(zonk) if zonk.is_falling()));
        assert_eq!(
            trailing.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(trailing.animation().frame(), 0);
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
        // Four more steps finish the original Down animation without applying
        // the newly held Right direction during the completion update.
        for _ in 0..4 {
            game.tick(turn);
        }
        let ready = game
            .board()
            .state(Position::new(3, 3))
            .expect("completed Murphy destination should exist");
        assert!(matches!(ready.actor(), Actor::Murphy(_)));
        assert_eq!(
            ready.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(ready.animation().frame(), 3);
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
        assert_eq!(trailing.animation().kind(), AnimationKind::ZonkPreFall);
        assert_eq!(trailing.animation().frame(), 0);

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
            trailing.animation().kind(),
            AnimationKind::Moving(Direction::Down)
        );
        assert_eq!(trailing.animation().frame(), 0);
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
            .animation();
        assert_eq!(moving.kind(), AnimationKind::Moving(Direction::Right));
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
            source.animation().kind(),
            AnimationKind::Vacating(Direction::Down)
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

        assert!(matches!(actor_at(&game, 3, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 4, 2), Actor::OrangeDisk(_)));
    }

    /// Confirms diagonal rolls expose their two-axis animation displacement.
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

        let rolled = game
            .board()
            .state(Position::new(2, 2))
            .expect("diagonal destination should exist");
        assert!(matches!(rolled.actor(), Actor::Zonk(_)));
        assert_eq!(
            rolled.animation().kind(),
            AnimationKind::Rolling(Direction::Left)
        );
    }

    /// Confirms freeze also completes a diagonal roll and its paired source.
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

        game.tick(Input::default());
        game.freeze_zonks = true;
        for expected_frame in 1..=3 {
            game.tick(Input::default());
            let source = game
                .board()
                .state(Position::new(3, 1))
                .expect("roll source should exist")
                .animation();
            let destination = game
                .board()
                .state(Position::new(2, 2))
                .expect("roll destination should exist")
                .animation();
            assert_eq!(source.frame(), expected_frame);
            assert_eq!(destination.frame(), expected_frame);
        }

        game.tick(Input::default());
        assert!(
            game.board()
                .state(Position::new(3, 1))
                .expect("released roll source should exist")
                .is_empty()
        );
        assert!(matches!(actor_at(&game, 2, 2), Actor::Zonk(_)));

        game.tick(Input::default());
        assert!(matches!(actor_at(&game, 2, 2), Actor::Zonk(_)));
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
        let rolled = game
            .board()
            .state(Position::new(4, 2))
            .expect("right-hand roll destination should exist");
        assert!(matches!(rolled.actor(), Actor::Zonk(_)));
        assert_eq!(
            rolled.animation().kind(),
            AnimationKind::Rolling(Direction::Right)
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
            .state(Position::new(3, 2))
            .expect("port exit should exist");
        assert!(matches!(murphy.actor(), Actor::Murphy(_)));
        assert_eq!(
            murphy.animation().kind(),
            AnimationKind::PortTraversal(Direction::Right)
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
        assert!(matches!(actor_at(&game, 2, 2), Actor::Murphy(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));
        assert!(matches!(actor_at(&game, 3, 1), Actor::Zonk(_)));
    }

    /// Confirms one planted Red Disk uses Murphy's vacated source and blocks another.
    #[test]
    fn planted_red_disk_uses_murphys_cell_and_enforces_one_fuse() {
        let origin = Position::new(2, 2);
        let mut game = game_with(&[(origin, State::new(Actor::Murphy(Murphy::new())))], 0);
        game.red_disks = 2;

        game.tick(Input {
            drop_disk: true,
            ..Input::default()
        });
        assert_eq!(game.red_disks(), 1);
        assert_eq!(
            game.planted_red_disk.map(|disk| disk.position),
            Some(origin)
        );
        assert!(matches!(actor_at(&game, 3, 2), Actor::Empty(_)));

        // Finish Murphy's planting pose, then vacate the stored source cell.
        for _ in 0..4 {
            game.tick(Input::default());
        }
        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        assert_eq!(
            game.board()
                .state(origin)
                .expect("reserved planted position should exist")
                .animation()
                .kind(),
            AnimationKind::Vacating(Direction::Right)
        );

        // The disk materializes only after Murphy's collision reservation has
        // released the source; its independent fuse has continued throughout.
        for _ in 0..4 {
            game.tick(Input::default());
        }

        let planted = game
            .board()
            .state(origin)
            .expect("planted position should exist");
        assert!(matches!(planted.actor(), Actor::RedDisk(_)));
        assert_eq!(planted.animation().kind(), AnimationKind::RedDiskFuse);
        assert_eq!(
            game.planted_red_disk.map(|disk| disk.position),
            Some(origin)
        );

        // A second plant command is ignored while the retained fuse is active.
        game.tick(Input {
            drop_disk: true,
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
        assert_eq!(game.red_disks(), 1);
        assert!(game.planted_red_disk.is_some());
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

        for _ in 0..6 {
            game.tick(Input::default());
        }

        let orange = game
            .board()
            .state(Position::new(3, 3))
            .expect("Orange Disk cell should exist");
        assert_eq!(orange.animation().kind(), AnimationKind::OrangeDiskFuse);
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));

        for _ in 0..6 {
            game.tick(Input::default());
        }
        assert!(matches!(actor_at(&game, 3, 3), Actor::Explosion(_)));
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
                (Position::new(3, 3), State::new(Actor::RedDisk(RedDisk))),
            ],
            0,
        );

        for _ in 0..5 {
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
            elapsed_frames: 5,
        });

        for _ in 0..5 {
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

        for _ in 0..6 {
            game.tick(Input::default());
        }

        let Actor::Explosion(explosion) = actor_at(&game, 3, 3) else {
            panic!("crushed Electron should become an explosion");
        };
        assert_eq!(explosion.residue(), ExplosionResidue::Infotron);
    }

    /// Confirms a later normal wave cannot erase active Infotron residue.
    #[test]
    fn overlapping_later_blast_preserves_electron_residue() {
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
        assert_eq!(explosion.residue(), ExplosionResidue::Infotron);
    }

    /// Confirms a touched chain actor waits for its promised terminal wave.
    #[test]
    fn chain_reaction_waits_for_its_animation_to_finish() {
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

        for _ in 0..CHAIN_REACTION_FRAMES - 1 {
            game.tick(Input::default());
        }
        assert_eq!(game.status(), GameStatus::Playing);

        game.tick(Input::default());
        assert_eq!(game.status(), GameStatus::Dead);
        assert!(matches!(actor_at(&game, 4, 2), Actor::Explosion(_)));
    }

    /// Confirms adjacent delayed centers consume each other's stale promises.
    #[test]
    fn same_deadline_chain_centers_preserve_and_consume_their_own_timers() {
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
        for _ in 0..CHAIN_REACTION_FRAMES {
            game.tick(Input::default());
        }
        for _ in 0..8 {
            game.tick(Input::default());
        }

        // Both secondary waves have completed their normal cleanup. If either
        // stale promise survived their union, these cells would still be live.
        assert!(matches!(actor_at(&game, 3, 2), Actor::Empty(_)));
        assert!(matches!(actor_at(&game, 2, 3), Actor::Empty(_)));
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

        for _ in 0..5 {
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
        assert_eq!(open.status(), GameStatus::Completed);
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
        assert!(matches!(actor_at(&game, 2, 2), Actor::Explosion(_)));

        let first_frame = game
            .board()
            .state(Position::new(2, 2))
            .expect("blast cell should exist")
            .animation()
            .frame();
        game.tick(Input::default());
        let second_frame = game
            .board()
            .state(Position::new(2, 2))
            .expect("blast cell should exist")
            .animation()
            .frame();
        assert!(
            second_frame > first_frame,
            "death blast should keep animating"
        );
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

        assert!(matches!(actor_at(&game, 4, 2), Actor::Murphy(_)));
        assert_eq!(
            game.board()
                .state(Position::new(4, 2))
                .expect("port destination should exist")
                .animation()
                .kind(),
            AnimationKind::PortTraversal(Direction::Right)
        );
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
                    State::new(Actor::YellowDisk(YellowDisk)),
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
                    State::new(Actor::YellowDisk(YellowDisk)),
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
                (yellow_position, State::new(Actor::YellowDisk(YellowDisk))),
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
            .set(yellow_position, State::new(Actor::YellowDisk(YellowDisk)))
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
                    State::new(Actor::YellowDisk(YellowDisk)),
                ),
                (
                    Position::new(3, 2),
                    State::new(Actor::YellowDisk(YellowDisk)),
                ),
            ],
            0,
        );

        game.tick(Input {
            direction: Some(Direction::Right),
            ..Input::default()
        });
        for _ in 0..8 {
            game.tick(Input::default());
        }

        // The first primary wave has cleaned up. Its adjacent later Yellow was
        // already a pending Explosion when the row-major scan reached it. Both
        // new Explosion cells received their first callback on the activation
        // tick because Murphy created them before schedule capture.
        assert!(matches!(actor_at(&game, 2, 2), Actor::Empty(_)));
        let pending = game
            .board()
            .state(Position::new(3, 2))
            .expect("later Yellow center should exist");
        assert!(matches!(pending.actor(), Actor::Explosion(_)));
        assert_eq!(pending.animation().frame(), 9);
        assert_eq!(pending.animation().frame_count(), CHAIN_REACTION_FRAMES);

        for _ in 9..CHAIN_REACTION_FRAMES {
            game.tick(Input::default());
        }
        let emitted = game
            .board()
            .state(Position::new(3, 2))
            .expect("secondary center should exist");
        assert!(matches!(emitted.actor(), Actor::Explosion(_)));
        assert_eq!(emitted.animation().frame(), 0);
        assert_eq!(emitted.animation().frame_count(), 8);
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
        assert!(matches!(Actor::Empty(Empty), Actor::Empty(_)));
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
                (Position::new(2, 2), State::new(Actor::Bug(Bug))),
                (Position::new(4, 2), State::new(Actor::Bug(Bug))),
            ],
            0,
        );

        for position in [Position::new(2, 2), Position::new(4, 2)] {
            let state = game.board().state(position).expect("Bug should exist");
            assert_eq!(state.animation().kind(), AnimationKind::Bug);
            assert_eq!(state.animation().frame(), 0);
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
                    .animation()
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
                    .animation()
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
                (Position::new(2, 2), State::new(Actor::Bug(Bug))),
                (Position::new(4, 2), State::new(Actor::Bug(Bug))),
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
            .animation();
        let second = game
            .board()
            .state(Position::new(4, 2))
            .expect("second Bug should exist")
            .animation();
        assert_eq!(first.kind(), AnimationKind::BugDormant);
        assert_eq!(second.kind(), AnimationKind::BugDormant);
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
                (Position::new(2, 2), State::new(Actor::Bug(Bug))),
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
        assert_eq!(bug.animation().kind(), AnimationKind::Bug);
        assert_eq!(bug.animation().frame(), 13);

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
        assert_eq!(active.animation().kind(), AnimationKind::Bug);
        assert_eq!(active.animation().frame(), 0);
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
}
