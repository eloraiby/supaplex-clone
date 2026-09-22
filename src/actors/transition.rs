//! Owned atomic board writes and session events emitted by actor behavior.
//!
//! Actors inspect an immutable world and return replacement values. The game
//! commits writes and events before invoking the next scheduled actor, keeping
//! mutable board ownership outside actor logic and preserving linear ordering.

use super::{Actor, Animation, Direction, Empty, MurphyMoveTarget, Position, State};
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
    /// Creates one fully specified immediate board transition.
    pub(super) fn new(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self { writes, events }
    }

    /// Creates an explosion transition for immediate sequential application.
    pub(super) fn blast(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self::new(writes, events)
    }

    /// Replaces only the currently updating actor's cell.
    pub(super) fn replace(position: Position, state: State) -> Self {
        Self::new(vec![CellWrite::new(position, state)], Vec::new())
    }

    /// Moves an actor atomically without producing a gameplay event.
    pub(super) fn move_actor(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        Self::move_actor_with_events(source, destination, actor, direction, Vec::new())
    }

    /// Moves an actor atomically and emits side effects after its cell writes.
    pub(super) fn move_actor_with_events(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        events: Vec<GameEvent>,
    ) -> Self {
        let destination_state = State::animated(actor, Animation::moving(direction));
        Self::new(
            vec![
                CellWrite::new(source, State::vacating(direction)),
                CellWrite::new(destination, destination_state),
            ],
            events,
        )
    }

    /// Starts a Snik Snak transfer with a destination-owned release schedule.
    pub(super) fn move_snik_snak(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        // A generic Vacating animation releases itself after eight callbacks.
        // The DOS enemy instead clears its source from movement frame seven,
        // so this stable reservation is explicitly owned by the destination.
        let destination_state = State::animated(actor, Animation::snik_snak_move(direction));
        let source_state = State::animated(
            Actor::Empty(Empty),
            Animation::snik_snak_vacating(direction),
        );
        Self::new(
            vec![
                CellWrite::new(source, source_state),
                CellWrite::new(destination, destination_state),
            ],
            Vec::new(),
        )
    }

    /// Starts an Electron transfer with destination-owned source cleanup.
    pub(super) fn move_electron(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        // Separate animation kinds keep Electron reservations distinguishable
        // from a Snik Snak or generic actor crossing the same cells later.
        let destination_state = State::animated(actor, Animation::electron_move(direction));
        let source_state =
            State::animated(Actor::Empty(Empty), Animation::electron_vacating(direction));
        Self::new(
            vec![
                CellWrite::new(source, source_state),
                CellWrite::new(destination, destination_state),
            ],
            Vec::new(),
        )
    }

    /// Moves Murphy with a target-specific eight- or nine-frame descriptor.
    pub(super) fn move_murphy(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        target: MurphyMoveTarget,
        looking_left: bool,
    ) -> Self {
        Self::move_murphy_with_events(
            source,
            destination,
            actor,
            direction,
            target,
            looking_left,
            Vec::new(),
        )
    }

    /// Moves Murphy while emitting action-start side effects after both writes.
    pub(super) fn move_murphy_with_events(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        target: MurphyMoveTarget,
        looking_left: bool,
        events: Vec<GameEvent>,
    ) -> Self {
        // The destination owns animation progress from the initiating update;
        // the sound belongs to that same atomic start, never to completion.
        let animation = Animation::murphy_move(direction, target, looking_left);
        let frame_count = animation.frame_count();
        Self::new(
            vec![
                CellWrite::new(source, State::vacating_for(direction, frame_count)),
                CellWrite::new(destination, State::animated(actor, animation)),
            ],
            events,
        )
    }

    /// Begins the two-update side delay while reserving only the adjacent cell.
    pub(super) fn prepare_rounded_roll(
        source: Position,
        side: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        Self::new(
            vec![
                CellWrite::new(
                    source,
                    State::animated(actor, Animation::rounded_pre_roll(direction)),
                ),
                CellWrite::new(side, State::rounded_side()),
            ],
            Vec::new(),
        )
    }
}
