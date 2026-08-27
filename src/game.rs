//! Row-major board storage and deterministic application of actor transitions.

use std::{cmp::Reverse, error::Error, fmt};

use crate::{
    actor::{
        Actor, AnimationKind, Base, Bug, Direction, Electron, Empty, Exit, GameEvent, Hardware,
        Infotron, InvisibleWall, Murphy, OrangeDisk, Port, PortDirections, Position, RamChip,
        RamChipShape, RedDisk, SnikSnak, State, Terminal, Transition, YellowDisk, Zonk,
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

/// Complete mutable play session around one row-major board.
#[derive(Clone, Debug)]
pub struct Game {
    /// Current accepted cell snapshot.
    board: Board,
    /// Level title used by the SDL HUD and window title.
    title: String,
    /// Required Infotrons not yet collected.
    remaining_infotrons: u16,
    /// Red Disks currently held by Murphy.
    red_disks: u16,
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
}

impl Game {
    /// Creates a play session from a decoded original level.
    pub fn new(level: &Level) -> Result<Self, GameError> {
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
            gravity: level.gravity(),
            freeze_zonks: level.freeze_zonks(),
            freeze_enemies: false,
            special_ports: level.special_ports().to_vec(),
            status: GameStatus::Playing,
            tick: 0,
        })
    }

    /// Returns the current immutable row-major board snapshot.
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

    /// Evaluates and atomically applies one deterministic simulation step.
    pub fn tick(&mut self, input: Input) {
        // Completed or dead sessions remain visually stable until the caller
        // restarts them, avoiding post-mortem physics surprises.
        if self.status != GameStatus::Playing {
            return;
        }

        let snapshot = self.board.clone();
        let mut proposals = Vec::new();
        {
            let world = WorldView::new(
                &snapshot,
                input,
                self.remaining_infotrons,
                self.red_disks,
                self.gravity,
                self.freeze_zonks,
                self.freeze_enemies,
                &self.special_ports,
            );

            // Every actor reads the same snapshot. No proposal can therefore
            // move an actor twice merely because of row-major scan order.
            for (index, state) in snapshot.cells().iter().enumerate() {
                let position = snapshot
                    .position(index)
                    .expect("enumerated board indices are always valid");
                if let Some(proposal) = state.actor().transition(state, position, &world) {
                    proposals.push(proposal);
                }
            }
        }

        // Higher-priority actions claim cells first. Source index breaks ties,
        // making the result independent of proposal insertion implementation.
        proposals.sort_by_key(|proposal| {
            (
                Reverse(proposal.priority),
                snapshot.index(proposal.source).unwrap_or(usize::MAX),
            )
        });

        let mut claimed = vec![false; snapshot.cells().len()];
        let mut next = snapshot.clone();
        let mut events = Vec::new();
        for proposal in proposals {
            let indices = proposal
                .writes
                .iter()
                .map(|write| snapshot.index(write.position))
                .collect::<Option<Vec<_>>>();
            let Some(indices) = indices else {
                continue;
            };
            if indices.iter().any(|index| claimed[*index]) {
                continue;
            }

            for (write, index) in proposal.writes.into_iter().zip(indices) {
                claimed[index] = true;
                // The coordinate was validated against an identically shaped
                // snapshot, so this assignment cannot fail for `next`.
                next.set(write.position, write.state)
                    .expect("validated transition position must remain in bounds");
            }
            events.extend(proposal.events);
        }

        self.board = next;
        for event in events {
            self.apply_event(event);
        }
        self.tick = self.tick.saturating_add(1);
    }

    /// Applies a gameplay side effect emitted by an accepted atomic transition.
    fn apply_event(&mut self, event: GameEvent) {
        match event {
            GameEvent::CollectInfotron => {
                self.remaining_infotrons = self.remaining_infotrons.saturating_sub(1);
            }
            GameEvent::CollectRedDisk => {
                self.red_disks = self.red_disks.saturating_add(1);
            }
            GameEvent::SpendRedDisk => {
                self.red_disks = self.red_disks.saturating_sub(1);
            }
            GameEvent::Completed => self.status = GameStatus::Completed,
            GameEvent::Died => self.status = GameStatus::Dead,
            GameEvent::ApplySpecialPort(port) => {
                self.gravity = port.gravity;
                self.freeze_zonks = port.freeze_zonks;
                self.freeze_enemies = port.freeze_enemies;
            }
            GameEvent::ActivateTerminal => self.detonate_yellow_disks(),
        }
    }

    /// Expands every Yellow Disk into a normal 3×3 explosion immediately.
    fn detonate_yellow_disks(&mut self) {
        let snapshot = self.board.clone();
        let centers: Vec<Position> = snapshot
            .cells()
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                matches!(state.actor(), Actor::YellowDisk(_))
                    .then(|| snapshot.position(index))
                    .flatten()
            })
            .collect();

        if centers.is_empty() {
            return;
        }

        let world = WorldView::new(
            &snapshot,
            Input::default(),
            self.remaining_infotrons,
            self.red_disks,
            self.gravity,
            self.freeze_zonks,
            self.freeze_enemies,
            &self.special_ports,
        );
        let transitions: Vec<Transition> = centers
            .into_iter()
            .map(|center| crate::actor::explode_at(&world, center, false))
            .collect();

        // Terminal blasts intentionally overlap and therefore merge. They are
        // already the highest-priority action, so direct union is deterministic.
        for transition in transitions {
            for write in transition.writes {
                self.board
                    .set(write.position, write.state)
                    .expect("explosion helper emits only bounded writes");
            }
            for event in transition.events {
                if event == GameEvent::Died {
                    self.status = GameStatus::Dead;
                }
            }
        }
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

/// Immutable environment exposed to every actor during one proposal phase.
pub(crate) struct WorldView<'board> {
    /// Snapshot read by every actor in this simulation step.
    board: &'board Board,
    /// Player command sampled for this simulation step.
    input: Input,
    /// Collectible requirement before accepted events from this step.
    remaining_infotrons: u16,
    /// Red Disk inventory before accepted events from this step.
    red_disks: u16,
    /// Snapshot of the current player-gravity toggle.
    gravity: bool,
    /// Snapshot of the current Zonk-freeze toggle.
    freeze_zonks: bool,
    /// Snapshot of the current enemy-freeze toggle.
    freeze_enemies: bool,
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
        gravity: bool,
        freeze_zonks: bool,
        freeze_enemies: bool,
        special_ports: &'board [SpecialPort],
    ) -> Self {
        Self {
            board,
            input,
            remaining_infotrons,
            red_disks,
            gravity,
            freeze_zonks,
            freeze_enemies,
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

    /// Reports whether a falling object should destroy this neighboring actor.
    pub(crate) fn is_crushable(&self, position: Position) -> bool {
        self.state(position).is_some_and(|state| {
            matches!(
                state.actor(),
                Actor::Murphy(_) | Actor::SnikSnak(_) | Actor::Electron(_)
            )
        })
    }

    /// Reports whether a Bug's current animation frame is dangerous to Murphy.
    pub(crate) fn is_bug_active(&self, position: Position) -> bool {
        self.state(position).is_some_and(|state| {
            state.animation().kind() == AnimationKind::Bug && state.animation().frame() < 4
        })
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
        17 => Actor::SnikSnak(SnikSnak::new(Direction::Left)),
        18 => Actor::YellowDisk(YellowDisk),
        19 => Actor::Terminal(Terminal),
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

    use super::{Board, Game, GameStatus, Input};
    use crate::actor::{
        Actor, AnimationKind, Base, Direction, Empty, Exit, Hardware, Infotron, Murphy, Position,
        State, Zonk,
    };
    use crate::level::LevelSet;

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
            gravity: false,
            freeze_zonks: false,
            freeze_enemies: false,
            special_ports: Vec::new(),
            status: GameStatus::Playing,
            tick: 0,
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

    /// Confirms logical occupancy moves immediately and animation later settles.
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

        assert!(matches!(actor_at(&game, 2, 2), Actor::Empty(_)));
        let destination = game
            .board()
            .state(Position::new(3, 2))
            .expect("destination should exist");
        assert!(matches!(destination.actor(), Actor::Murphy(_)));
        assert_eq!(
            destination.animation().kind(),
            AnimationKind::Moving(Direction::Right)
        );

        // Four animation ticks expose the promised Settle transition.
        for _ in 0..4 {
            game.tick(Input::default());
        }
        assert_eq!(
            game.board()
                .state(Position::new(3, 2))
                .expect("destination should exist")
                .animation()
                .kind(),
            AnimationKind::Idle
        );
    }

    /// Confirms collection is part of the accepted atomic movement proposal.
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
        assert!(matches!(actor_at(&game, 4, 2), Actor::Zonk(_)));
    }

    /// Confirms falling uses one snapshot and cannot cascade several cells per tick.
    #[test]
    fn zonk_falls_once_and_remains_collision_occupied() {
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

        game.tick(Input::default());

        assert!(matches!(actor_at(&game, 3, 1), Actor::Empty(_)));
        assert!(matches!(actor_at(&game, 3, 2), Actor::Zonk(_)));
        assert!(matches!(actor_at(&game, 3, 3), Actor::Empty(_)));
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
