//! Shared direction mapping for the original eight-frame enemy turn cycles.

use super::Direction;

/// Rotation family used by the original eight-state enemy turn cycles.
///
/// Snik Snaks and Electrons do not choose a new direction in one update. Their
/// state byte advances around one of these cycles on global quarter ticks, and
/// only even-numbered frames test the direction represented by that picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnemyTurn {
    /// Counter-clockwise cycle whose candidates are Up, Left, Down, and Right.
    Left,
    /// Clockwise cycle whose candidates are Up, Right, Down, and Left.
    Right,
}

impl EnemyTurn {
    /// Returns the direction tested by an even turn frame.
    pub(super) const fn direction_at_frame(self, frame: u8) -> Option<Direction> {
        // Odd frames are visual intermediates and deliberately perform no
        // collision test on the original frame-counter phase.
        match (self, frame & 7) {
            (Self::Left, 0) | (Self::Right, 0) => Some(Direction::Up),
            (Self::Left, 2) | (Self::Right, 6) => Some(Direction::Left),
            (Self::Left, 4) | (Self::Right, 4) => Some(Direction::Down),
            (Self::Left, 6) | (Self::Right, 2) => Some(Direction::Right),
            _ => None,
        }
    }

    /// Finds the even frame at which this cycle tests `direction`.
    const fn candidate_frame(self, direction: Direction) -> u8 {
        match (self, direction) {
            (Self::Left, Direction::Up) | (Self::Right, Direction::Up) => 0,
            (Self::Left, Direction::Left) | (Self::Right, Direction::Right) => 2,
            (Self::Left, Direction::Down) | (Self::Right, Direction::Down) => 4,
            (Self::Left, Direction::Right) | (Self::Right, Direction::Left) => 6,
        }
    }

    /// Selects the serialized starting frame for an enemy facing `heading`.
    pub(super) const fn initial_frame(self, heading: Direction) -> u8 {
        // A new enemy begins on the candidate immediately to its preferred
        // side. In particular, a right-facing state-zero enemy tests Up.
        let first_candidate = match self {
            Self::Left => heading.left(),
            Self::Right => heading.right(),
        };
        self.candidate_frame(first_candidate)
    }

    /// Returns the intermediate frame just before `direction` is tested.
    pub(super) const fn preceding_frame(self, direction: Direction) -> u8 {
        // Wrapping seven positions backwards converts candidate frame zero to
        // frame seven while every other even candidate becomes its prior odd
        // animation frame.
        self.candidate_frame(direction).wrapping_add(7) & 7
    }
}

/// Legal wall-following phases shared by Snik Snaks and Electrons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnemyPhase {
    /// Cadence-gated eight-picture candidate scan.
    Turning {
        /// Clockwise or counter-clockwise candidate ordering.
        turn: EnemyTurn,
        /// Current original low-three-bit picture.
        frame: super::Frame<8>,
    },
    /// Eight-update transfer with a destination-owned source reservation.
    Moving {
        /// Cardinal direction of the transfer.
        direction: Direction,
        /// Current picture, including the source-release boundary at six.
        frame: super::Frame<8>,
    },
}
