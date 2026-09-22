//! Unoccupied cells and destination-owned collision reservations.

use super::{Animation, AnimationKind, Direction, Position, Transition};
use crate::game::WorldView;

/// Space or a temporary collision marker owned by a neighboring movement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Empty {
    /// Truly unoccupied and available to every actor.
    #[default]
    Space,
    /// Occupied until the movement owner releases the corresponding marker.
    Reserved(Reservation),
}

/// The finite set of non-actor occupancy markers used by original movements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reservation {
    /// Old cell of a falling object or Murphy transfer.
    Vacating {
        /// Movement direction used to find the owning destination.
        direction: Direction,
        /// Source lifetime used for presentation, never independently advanced.
        duration: SourceDuration,
    },
    /// Old cell of a Snik Snak, released by its seventh movement callback.
    SnikSnakSource(Direction),
    /// Old cell of an Electron, released by its seventh movement callback.
    ElectronSource(Direction),
    /// Empty endpoint held by a Murphy push or port traversal.
    MurphyDestination,
    /// Side cell held while a rounded actor prepares a roll.
    RoundedSide,
    /// Cell below a rolling rock or falling Orange Disk.
    RoundedDestination,
}

/// The only lifetimes used by a source marker in the original movement rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceDuration {
    /// Ordinary eight-picture actor transfer.
    Eight,
    /// Murphy's nine-picture rightward Red Disk step.
    Nine,
}

impl SourceDuration {
    /// Returns the fixed presentation length without accepting arbitrary counts.
    pub const fn frames(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Nine => 9,
        }
    }
}

impl Empty {
    /// Produces the marker's presentation without giving it autonomous timing.
    pub(super) fn animation(self) -> Animation {
        let (kind, count) = match self {
            Self::Space => return Animation::idle(),
            Self::Reserved(Reservation::Vacating {
                direction,
                duration,
            }) => (AnimationKind::Vacating(direction), duration.frames()),
            Self::Reserved(Reservation::SnikSnakSource(direction)) => {
                (AnimationKind::SnikSnakVacating(direction), 1)
            }
            Self::Reserved(Reservation::ElectronSource(direction)) => {
                (AnimationKind::ElectronVacating(direction), 1)
            }
            Self::Reserved(Reservation::MurphyDestination) => (AnimationKind::MurphyDestination, 1),
            Self::Reserved(Reservation::RoundedSide) => (AnimationKind::RoundedSide, 1),
            Self::Reserved(Reservation::RoundedDestination) => {
                (AnimationKind::RoundedDestination, 1)
            }
        };
        Animation::view(kind, 0, count)
    }

    /// Leaves markers unchanged; only their owning movement releases them.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
