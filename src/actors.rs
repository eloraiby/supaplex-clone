//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every cell stores one complete [`Actor`]. Concrete actors own their legal
//! phases and bounded progress values; [`Animation`] is a read-only presentation
//! computed from those phases and cannot be assigned back to simulation state.
//!
//! This module only dispatches by actor identity. Each actor's exhaustive match
//! owns timing and completion behavior. Shared rounded physics and enemy turn
//! mapping accept only the concrete families whose rules they implement.
//!
//! Actor callbacks inspect an immutable world and return owned replacement
//! values. The game applies each transition with exclusive board access before
//! the next callback, preserving Murphy-first, row-major update semantics.

#![warn(missing_docs)]

mod animation;
pub mod enemy;
mod frame;
mod geometry;
pub mod rounded;
mod state;
mod transition;

pub use animation::{Animation, AnimationKind};
pub use enemy::EnemyTurn;
pub use frame::Frame;
pub use geometry::{Direction, Horizontal, Position};
pub use state::State;
pub(crate) use transition::{CellWrite, GameEvent, Transition};

pub mod base;
pub mod bug;
pub mod electron;
pub mod empty;
pub mod exit;
pub mod explosion;
pub mod hardware;
pub mod infotron;
pub mod invisible_wall;
pub mod murphy;
pub mod orange_disk;
pub mod port;
pub mod ram_chip;
pub mod red_disk;
pub mod snik_snak;
pub mod terminal;
pub mod yellow_disk;
pub mod zonk;

use crate::game::WorldView;
pub use base::Base;
pub use bug::Bug;
pub use electron::Electron;
pub use empty::Empty;
pub use exit::Exit;
pub(crate) use explosion::explode_at;
pub use explosion::{Explosion, ExplosionResidue};
pub use hardware::Hardware;
pub use infotron::Infotron;
pub use invisible_wall::InvisibleWall;
pub use murphy::{Murphy, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget, MurphySnapTarget};
pub use orange_disk::OrangeDisk;
pub use port::{Port, PortDirections};
pub use ram_chip::{RamChip, RamChipShape};
pub use red_disk::RedDisk;
pub use snik_snak::SnikSnak;
pub use terminal::Terminal;
pub use yellow_disk::YellowDisk;
pub use zonk::Zonk;

/// Delay before an actor touched by one blast emits its own secondary wave.
pub(crate) const CHAIN_REACTION_FRAMES: u8 = 13;

/// Countdown value at which a completely planted Red Disk detonates.
pub(crate) const RED_DISK_DETONATION_COUNTDOWN: u8 = 0x28;

/// Runtime identity for every original tile and temporary explosion cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Actor {
    /// Unoccupied cell.
    Empty(Empty),
    /// Falling and rolling rock.
    Zonk(Zonk),
    /// Diggable circuit-board material.
    Base(Base),
    /// Player character.
    Murphy(Murphy),
    /// Falling collectible.
    Infotron(Infotron),
    /// Destructible RAM-chip wall.
    RamChip(RamChip),
    /// Indestructible decorative wall.
    Hardware(Hardware),
    /// Locked or open goal.
    Exit(Exit),
    /// Falling explosive disk.
    OrangeDisk(OrangeDisk),
    /// Directional pass-through tile.
    Port(Port),
    /// Scissor-like wall-following enemy.
    SnikSnak(SnikSnak),
    /// Terminal-detonated pushable disk.
    YellowDisk(YellowDisk),
    /// Yellow-Disk detonation terminal.
    Terminal(Terminal),
    /// Collectible and droppable timed explosive.
    RedDisk(RedDisk),
    /// Infotron-producing wall-following enemy.
    Electron(Electron),
    /// Periodically dangerous Base tile.
    Bug(Bug),
    /// Hidden indestructible wall from extended classic level files.
    InvisibleWall(InvisibleWall),
    /// Runtime-only animated blast cell.
    Explosion(Explosion),
}

impl Actor {
    /// Dispatches directly to the concrete actor's exhaustive phase machine.
    pub(crate) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        match self {
            Self::Empty(actor) => actor.transition(position, world),
            Self::Zonk(actor) => rounded::RoundedActor::Zonk(*actor).update(position, world),
            Self::Base(actor) => actor.transition(position, world),
            Self::Murphy(actor) => actor.transition(position, world),
            Self::Infotron(actor) => {
                rounded::RoundedActor::Infotron(*actor).update(position, world)
            }
            Self::RamChip(actor) => actor.transition(position, world),
            Self::Hardware(actor) => actor.transition(position, world),
            Self::Exit(actor) => actor.transition(position, world),
            Self::OrangeDisk(actor) => actor.transition(position, world),
            Self::Port(actor) => actor.transition(position, world),
            Self::SnikSnak(actor) => actor.update(position, world),
            Self::YellowDisk(actor) => actor.transition(position, world),
            Self::Terminal(actor) => actor.transition(position, world),
            Self::RedDisk(actor) => actor.transition(position, world),
            Self::Electron(actor) => actor.update(position, world),
            Self::Bug(actor) => actor.transition(position, world),
            Self::InvisibleWall(actor) => actor.transition(position, world),
            Self::Explosion(actor) => actor.transition(position, world),
        }
    }

    /// Computes presentation; the returned value has no simulation write-back API.
    pub fn animation(&self) -> Animation {
        match self {
            Self::Empty(actor) => actor.animation(),
            Self::Zonk(actor) => actor.animation(),
            Self::Base(actor) => actor.animation(),
            Self::Murphy(actor) => actor.animation(),
            Self::Infotron(actor) => actor.animation(),
            Self::RamChip(_) => Animation::idle(),
            Self::Hardware(_) => Animation::idle(),
            Self::Exit(_) => Animation::idle(),
            Self::OrangeDisk(actor) => actor.animation(),
            Self::Port(_) => Animation::idle(),
            Self::SnikSnak(actor) => actor.animation(),
            Self::YellowDisk(actor) => actor.animation(),
            Self::Terminal(actor) => actor.animation(),
            Self::RedDisk(actor) => actor.animation(),
            Self::Electron(actor) => actor.animation(),
            Self::Bug(actor) => actor.animation(),
            Self::InvisibleWall(_) => Animation::idle(),
            Self::Explosion(actor) => actor.animation(),
        }
    }

    /// Returns the serialized tile code used for this actor's static sprite.
    pub fn tile_code(&self) -> u8 {
        match self {
            Self::Empty(_) => 0,
            Self::Zonk(_) => 1,
            Self::Base(_) => 2,
            Self::Murphy(_) => 3,
            Self::Infotron(_) => 4,
            Self::RamChip(chip) => match chip.shape() {
                RamChipShape::Center => 5,
                RamChipShape::Left => 26,
                RamChipShape::Right => 27,
                RamChipShape::Top => 38,
                RamChipShape::Bottom => 39,
            },
            Self::Hardware(hardware) => {
                if hardware.variant() == 0 {
                    6
                } else {
                    27 + hardware.variant().min(10)
                }
            }
            Self::Exit(_) => 7,
            Self::OrangeDisk(_) => 8,
            Self::Port(port) => port.tile_code(),
            Self::SnikSnak(_) => 17,
            Self::YellowDisk(_) => 18,
            Self::Terminal(_) => 19,
            Self::RedDisk(_) => 20,
            Self::Electron(_) => 24,
            Self::Bug(_) => 25,
            Self::InvisibleWall(_) => 40,
            Self::Explosion(_) => 0,
        }
    }
}
