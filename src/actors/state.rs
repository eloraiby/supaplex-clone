//! Complete board-cell values and constructors for movement reservations.
//!
//! A cell stores only a complete Actor. Presentation is derived on demand, so
//! there is no independent animation that could contradict its identity.

use super::{
    Actor, Animation, AnimationKind, Bug, Direction, Electron, Empty, EnemyTurn, RedDisk, SnikSnak,
};

/// Complete content of one board cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    /// Actor-specific identity and persistent behavior data.
    actor: Actor,
}

impl State {
    /// Wraps a complete actor whose own type determines its legal phases.
    pub fn new(actor: Actor) -> Self {
        Self { actor }
    }

    /// Creates ordinary traversable Space.
    pub fn empty() -> Self {
        Self::new(Actor::Empty(Empty::Space))
    }

    /// Creates a destination-owned collision reservation.
    pub(super) fn reserved(reservation: super::empty::Reservation) -> Self {
        Self::new(Actor::Empty(Empty::Reserved(reservation)))
    }

    /// Creates the eight-update source marker for a rounded object's fall.
    pub(super) fn vacating(direction: Direction) -> Self {
        Self::vacating_for(direction, 8)
    }

    /// Creates a source marker with the owning Murphy movement's exact duration.
    pub(super) fn vacating_for(direction: Direction, duration: u8) -> Self {
        Self::reserved(super::empty::Reservation::Vacating {
            direction,
            duration: std::num::NonZeroU8::new(duration).expect("movement duration is positive"),
        })
    }

    /// Reserves an empty Murphy push or port destination.
    pub(super) fn murphy_destination() -> Self {
        Self::reserved(super::empty::Reservation::MurphyDestination)
    }

    /// Reserves the side cell of a rounded object's pending roll.
    pub(super) fn rounded_side() -> Self {
        Self::reserved(super::empty::Reservation::RoundedSide)
    }

    /// Reserves the next downward cell of a rolling rock or Orange Disk.
    pub(super) fn rounded_destination() -> Self {
        Self::reserved(super::empty::Reservation::RoundedDestination)
    }

    /// Returns the complete actor, including its actor-specific phase.
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Derives a read-only rendering description from this actor's current phase.
    pub fn animation(&self) -> Animation {
        self.actor.animation()
    }

    /// Restores the session-owned planted fuse from its serialized countdown.
    pub(crate) fn planted_red_disk(frame: u8) -> Self {
        Self::new(Actor::RedDisk(RedDisk::planted(frame)))
    }

    /// Starts one safe Bug interval selected by the session RNG.
    pub(crate) fn dormant_bug(delay: u8) -> Self {
        Self::new(Actor::Bug(Bug::dormant(delay)))
    }

    /// Loads a Snik Snak turn picture using the original low-three-bit decoding.
    pub(crate) fn loaded_snik_snak_turn(frame: u8) -> Self {
        SnikSnak::new(Direction::Right).in_phase(super::enemy::EnemyPhase::Turning {
            turn: EnemyTurn::Left,
            frame: super::Frame::new(frame & 7).unwrap(),
        })
    }

    /// Loads the destination half of an original Snik Snak transfer.
    pub(crate) fn loaded_snik_snak_move(direction: Direction) -> Self {
        SnikSnak::new(direction).in_phase(super::enemy::EnemyPhase::Moving {
            direction,
            frame: super::Frame::first(),
        })
    }

    /// Loads the corresponding destination-owned Snik Snak source marker.
    pub(crate) fn loaded_snik_snak_source(direction: Direction) -> Self {
        Self::reserved(super::empty::Reservation::SnikSnakSource(direction))
    }

    /// Loads an Electron turn picture using the original low-three-bit decoding.
    pub(crate) fn loaded_electron_turn(frame: u8) -> Self {
        Electron::new(Direction::Right).in_phase(super::enemy::EnemyPhase::Turning {
            turn: EnemyTurn::Left,
            frame: super::Frame::new(frame & 7).unwrap(),
        })
    }

    /// Loads the destination half of an original Electron transfer.
    pub(crate) fn loaded_electron_move(direction: Direction) -> Self {
        Electron::new(direction).in_phase(super::enemy::EnemyPhase::Moving {
            direction,
            frame: super::Frame::first(),
        })
    }

    /// Loads the corresponding destination-owned Electron source marker.
    pub(crate) fn loaded_electron_source(direction: Direction) -> Self {
        Self::reserved(super::empty::Reservation::ElectronSource(direction))
    }

    /// Reports whether the cell has no actor and no movement reservation.
    pub fn is_empty(&self) -> bool {
        matches!(self.actor, Actor::Empty(Empty::Space))
    }

    /// Reports whether the actor currently presents its stable, unlocked pose.
    pub fn is_idle(&self) -> bool {
        self.animation().kind() == AnimationKind::Idle
    }
}
