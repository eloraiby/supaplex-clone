//! Explicit pictures, atomic board writes, and session events emitted by actors.
//!
//! Actors inspect an immutable world and return replacement values. The game
//! records pictures and commits writes and events before the next scheduled actor.
//! A picture can describe the final animation frame while its writes complete the
//! action. Cell changes alone never imply drawing; reservation releases are silent.

use super::{Actor, Direction, MurphyMoveTarget, Position, State};
use crate::{game::SoundEffect, level::SpecialPort};

/// An actor picture emitted during a simulation callback, independent of cell storage.
///
/// The actor owns its bounded phase, including a final picture which need not
/// remain on the board after completion. Positions use original board cells;
/// atlas coordinates and pixel copies belong exclusively to the renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Drawing {
    /// Anchor used by this picture, even if the actor transfers before tick end.
    pub position: Position,
    /// Complete typed picture; Space explicitly paints an empty cell.
    pub actor: Actor,
}

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
    /// Pictures emitted before completion writes and their immediate events.
    pub(crate) drawings: Vec<Drawing>,
    /// Side effects applied after all cell replacements in this transition.
    pub(crate) events: Vec<GameEvent>,
}

impl Transition {
    /// Creates a silent board transition; its owner adds pictures explicitly.
    pub(super) fn new(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self {
            writes,
            drawings: Vec::new(),
            events,
        }
    }

    /// Commits a blast's separately constructed cell writes and ordered pictures.
    pub(super) fn blast(
        writes: Vec<CellWrite>,
        events: Vec<GameEvent>,
        drawings: Vec<Drawing>,
    ) -> Self {
        // The wave builder knows which cells explode and which are reservations
        // being released. No consumer needs to infer that distinction afterward.
        Self {
            writes,
            drawings,
            events,
        }
    }

    /// Replaces one cell without implying any change to the saved level pixels.
    pub(super) fn replace(position: Position, state: State) -> Self {
        Self::new(vec![CellWrite::new(position, state)], Vec::new())
    }

    /// Replaces a cell and deliberately paints its new actor on this callback.
    pub(super) fn paint(position: Position, state: State) -> Self {
        let actor = state.actor().clone();
        Self::replace(position, state).with_drawing(position, actor)
    }

    /// Adds an explicit picture without requiring that picture to survive in a cell.
    pub(super) fn with_drawing(mut self, position: Position, actor: Actor) -> Self {
        self.drawings.push(Drawing { position, actor });
        self
    }

    /// Paints a terminal action picture before any completion or blast drawings.
    pub(super) fn after_drawing(mut self, position: Position, actor: Actor) -> Self {
        self.drawings.insert(0, Drawing { position, actor });
        self
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
        let picture = state.actor().clone();
        Self::new(
            vec![
                CellWrite::new(source, State::loaded_snik_snak_source(direction)),
                CellWrite::new(destination, state),
            ],
            Vec::new(),
        )
        .with_drawing(destination, picture)
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
        let picture = state.actor().clone();
        Self::new(
            vec![
                CellWrite::new(source, State::loaded_electron_source(direction)),
                CellWrite::new(destination, state),
            ],
            Vec::new(),
        )
        .with_drawing(destination, picture)
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
        let picture = destination_state.actor().clone();
        Self::new(
            vec![
                CellWrite::new(source, State::vacating_for(direction, duration)),
                CellWrite::new(destination, destination_state),
            ],
            events,
        )
        .with_drawing(destination, picture)
    }
}
