//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every cell stores one complete [`Actor`]. Concrete actors own their legal
//! phases and bounded progress values. Rendering consumes those same typed phases
//! directly; there is no parallel animation-kind enum or assignable view.
//!
//! This module only dispatches by actor identity. Each actor's exhaustive match
//! owns timing and completion behavior. Shared rounded physics and enemy turn
//! mapping accept only the concrete families whose rules they implement.
//!
//! Actor callbacks inspect an immutable world and return owned replacement
//! values. The renderer compares previous/current cell states independently.
//! The game commits each transition with exclusive access before
//! the next callback, preserving Murphy-first, row-major update semantics.

#![warn(missing_docs)]

pub mod enemy;
mod frame;
mod geometry;
pub mod rounded;
mod state;
mod transition;

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

    /// Reports the original idle collision state without consulting sprite metadata.
    pub fn is_idle(&self) -> bool {
        match self {
            Self::Empty(empty) => matches!(empty, Empty::Space),
            Self::Zonk(actor) => matches!(actor.phase(), rounded::RoundedPhase::Resting),
            Self::Infotron(actor) => matches!(actor.phase(), rounded::RoundedPhase::Resting),
            Self::Base(actor) => matches!(actor, Base::Resting),
            Self::Murphy(actor) => matches!(
                actor.phase(),
                murphy::MurphyPhase::Ready | murphy::MurphyPhase::PreparingPush { .. }
            ),
            Self::OrangeDisk(actor) => matches!(actor.phase(), orange_disk::OrangePhase::Resting),
            Self::YellowDisk(actor) => matches!(actor, YellowDisk::Resting),
            Self::RedDisk(actor) => matches!(actor, RedDisk::Collectible),
            Self::RamChip(_)
            | Self::Hardware(_)
            | Self::Exit(_)
            | Self::Port(_)
            | Self::InvisibleWall(_) => true,
            Self::SnikSnak(_)
            | Self::Electron(_)
            | Self::Bug(_)
            | Self::Terminal(_)
            | Self::Explosion(_) => false,
        }
    }

    /// Reports targets whose autonomous behavior is suspended by a Murphy action.
    pub fn is_held(&self) -> bool {
        match self {
            Self::Zonk(actor) => matches!(actor.phase(), rounded::RoundedPhase::Held),
            Self::Infotron(actor) => matches!(actor.phase(), rounded::RoundedPhase::Held),
            Self::OrangeDisk(actor) => matches!(actor.phase(), orange_disk::OrangePhase::Held),
            Self::Base(actor) => matches!(actor, Base::Held),
            Self::YellowDisk(actor) => matches!(actor, YellowDisk::Held),
            Self::RedDisk(actor) => matches!(actor, RedDisk::Held),
            Self::Bug(actor) => matches!(actor, Bug::Held),
            Self::Empty(_)
            | Self::Murphy(_)
            | Self::RamChip(_)
            | Self::Hardware(_)
            | Self::Exit(_)
            | Self::Port(_)
            | Self::SnikSnak(_)
            | Self::Terminal(_)
            | Self::Electron(_)
            | Self::InvisibleWall(_)
            | Self::Explosion(_) => false,
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
