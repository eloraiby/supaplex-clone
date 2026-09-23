//! Atomic board writes and session events emitted by actor state machines.
//!
//! Actors inspect an immutable world and return replacement values. The game
//! commits each transition before invoking the next scheduled actor. Rendering
//! compares complete previous/current frames and does not participate here.

use super::{Direction, MurphyMoveTarget, Position, State};
use crate::{game::SoundEffect, level::SpecialPort};

/// One atomic write included in an actor's immediate board transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CellWrite {
    /// Cell replaced as part of its actor's indivisible transition.
    pub(crate) position: Position,
    /// Complete actor and animation state written to that cell.
    pub(crate) state: State,
}

impl CellWrite {
    /// Creates one write to be committed before the next actor is called.
    pub(crate) fn new(position: Position, state: State) -> Self {
        Self { position, state }
    }
}

/// Gameplay side effect emitted after its transition is applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameEvent {
    /// Decrease the number of Infotrons still required.
    CollectInfotron,
    /// Add one Red Disk to Murphy's inventory.
    CollectRedDisk,
    /// Reserve the one planted-disk slot at countdown one while Space is held.
    BeginPlantRedDisk(Position),
    /// Cancel an incomplete placement without spending a Red Disk.
    CancelPlantRedDisk,
    /// Spend one disk and arm the completed placement at countdown two.
    FinishPlantRedDisk,
    /// Mark the current level as successfully completed.
    Completed,
    /// Mark Murphy as destroyed.
    Died,
    /// Replace global toggles with a special port's metadata.
    ApplySpecialPort(SpecialPort),
    /// Detonate all idle Yellow Disks currently present on the board.
    ActivateTerminal,
    /// Consume the shared RNG stream and schedule one Bug's safe interval.
    RandomizeBug(Position),
    /// Consume the shared RNG stream and schedule one Terminal screen scroll.
    RandomizeTerminal(Position),
    /// Start an independent signed thirteen-tick secondary explosion timer.
    ScheduleExplosion {
        /// Center whose delayed wave will be emitted when the timer reaches zero.
        position: Position,
        /// Whether the delayed wave uses Electron graphics and Infotron residue.
        electron: bool,
    },
    /// Mark the global explosion effect active for deterministic RNG consumption.
    ExplosionStarted,
    /// Clear the original global explosion flag when one visual cell completes.
    ExplosionFinished,
    /// Forward one actor-selected effect to the platform playback queue.
    PlaySound(SoundEffect),
}

/// Atomic multi-cell change applied immediately during the linear update pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Transition {
    /// Cell replacements committed together before the next actor is updated.
    pub(crate) writes: Vec<CellWrite>,
    /// Side effects applied after all cell replacements in this transition.
    pub(crate) events: Vec<GameEvent>,
}

impl Transition {
    /// Groups cell replacements with their immediate session effects.
    pub(super) fn new(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self { writes, events }
    }

    /// Replaces one cell as a complete, actor-owned state value.
    pub(super) fn replace(position: Position, state: State) -> Self {
        Self::new(vec![CellWrite::new(position, state)], Vec::new())
    }

    /// Transfers a rounded actor without consuming its first falling picture.
    pub(super) fn move_actor(
        source: Position,
        destination: Position,
        actor: super::rounded::RoundedActor,
    ) -> Self {
        Self::new(
            vec![
                CellWrite::new(source, State::vacating(Direction::Down)),
                CellWrite::new(
                    destination,
                    actor.in_phase(super::rounded::RoundedPhase::Falling(super::Frame::first())),
                ),
            ],
            Vec::new(),
        )
    }

    /// Begins a Snik Snak transfer with a destination-owned source marker.
    pub(super) fn move_snik_snak(
        source: Position,
        destination: Position,
        actor: super::SnikSnak,
        direction: Direction,
    ) -> Self {
        let state = actor.in_phase(super::enemy::EnemyPhase::Moving {
            direction,
            frame: super::Frame::first(),
        });
        Self::new(
            vec![
                CellWrite::new(source, State::loaded_snik_snak_source(direction)),
                CellWrite::new(destination, state),
            ],
            Vec::new(),
        )
    }

    /// Begins an Electron transfer with its own source marker family.
    pub(super) fn move_electron(
        source: Position,
        destination: Position,
        actor: super::Electron,
        direction: Direction,
    ) -> Self {
        let state = actor.in_phase(super::enemy::EnemyPhase::Moving {
            direction,
            frame: super::Frame::first(),
        });
        Self::new(
            vec![
                CellWrite::new(source, State::loaded_electron_source(direction)),
                CellWrite::new(destination, state),
            ],
            Vec::new(),
        )
    }

    /// Starts a material-specific Murphy step without session side effects.
    pub(super) fn move_murphy(
        source: Position,
        destination: Position,
        actor: super::Murphy,
        direction: Direction,
        target: MurphyMoveTarget,
    ) -> Self {
        Self::move_murphy_with_events(source, destination, actor, direction, target, Vec::new())
    }

    /// Starts a typed Murphy step and emits its sound after both cell writes.
    pub(super) fn move_murphy_with_events(
        source: Position,
        destination: Position,
        actor: super::Murphy,
        direction: Direction,
        target: MurphyMoveTarget,
        events: Vec<GameEvent>,
    ) -> Self {
        let (destination_state, duration) = actor.moving(direction, target);
        Self::new(
            vec![
                CellWrite::new(source, State::vacating_for(direction, duration)),
                CellWrite::new(destination, destination_state),
            ],
            events,
        )
    }
}
