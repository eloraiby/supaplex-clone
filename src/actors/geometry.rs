//! Board coordinates and cardinal directions shared by actor state machines.

/// A zero-based location on the row-major board.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Position {
    /// Horizontal cell coordinate, increasing to the right.
    pub x: usize,
    /// Vertical cell coordinate, increasing downward.
    pub y: usize,
}

impl Position {
    /// Creates a board position without assuming any particular dimensions.
    pub const fn new(x: usize, y: usize) -> Self {
        Self { x, y }
    }
}

/// One of the four orthogonal movement and facing directions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// One row toward the top of the board.
    Up,
    /// One column toward the right edge of the board.
    Right,
    /// One row toward the bottom of the board.
    Down,
    /// One column toward the left edge of the board.
    Left,
}

impl Direction {
    /// Stable iteration order used by neighborhood and enemy checks.
    pub const ALL: [Self; 4] = [Self::Up, Self::Right, Self::Down, Self::Left];

    /// Returns the direction obtained by turning counter-clockwise.
    pub const fn left(self) -> Self {
        match self {
            Self::Up => Self::Left,
            Self::Right => Self::Up,
            Self::Down => Self::Right,
            Self::Left => Self::Down,
        }
    }

    /// Returns the direction obtained by turning clockwise.
    pub const fn right(self) -> Self {
        match self {
            Self::Up => Self::Right,
            Self::Right => Self::Down,
            Self::Down => Self::Left,
            Self::Left => Self::Up,
        }
    }

    /// Returns the direction facing back toward the current cell.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Right => Self::Left,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
        }
    }

    /// Reports whether this direction can be used for a horizontal push.
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// A lateral direction accepted by rolling rocks and horizontal pushes.
///
/// Unlike [`Direction`], this type cannot represent an upward or downward roll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Horizontal {
    /// One column toward the left edge.
    Left,
    /// One column toward the right edge.
    Right,
}

impl Horizontal {
    /// Converts a restricted direction for board-neighbor lookup.
    pub const fn direction(self) -> Direction {
        match self {
            Self::Left => Direction::Left,
            Self::Right => Direction::Right,
        }
    }
}
