//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every level cell contains one [`State`].  An [`Actor`] is an enum whose
//! variants wrap actor-specific structs, while [`Animation`] records the visual
//! phase and the semantic transition that follows its final frame.
//!
//! Concrete submodules own actor data and local decisions. This coordinator
//! owns enum dispatch and completion actions that can touch several actor kinds.
//! Private support modules own geometry, enemy turn mapping, validated cell and
//! animation values, and atomic transitions. Their public value types are
//! re-exported here; internal builders stay within the actor family.
//!
//! Actor callbacks inspect an immutable world and return owned replacement
//! values. They do not mutate the board through shared references: the game
//! applies each transition with exclusive access before the next callback.
//! Keeping that boundary preserves Murphy-first, row-major update semantics.

#![warn(missing_docs)]

mod animation;
mod enemy;
mod frame;
mod geometry;
mod state;
mod transition;

pub use animation::{Animation, AnimationKind};
use animation::{AnimationAdvance, AnimationNext};
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

pub use base::Base;
pub use bug::Bug;
pub use electron::Electron;
pub use empty::Empty;
pub use exit::Exit;
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

pub(crate) use explosion::explode_at;
use murphy::{actor_for_push_target, murphy_is_protected_from_falling_actor, pushed_actor_matches};

use crate::game::{SoundEffect, WorldView};

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
    /// Delegates behavior to the concrete struct wrapped by this enum variant.
    pub(crate) fn transition(
        &self,
        state: &State,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        // A planting Murphy owns a hold-sensitive counter. Generic animation
        // advancement would make Space release unable to cancel the operation,
        // so this phase delegates directly to the actor state machine.
        if let Self::Murphy(murphy) = self
            && murphy.is_planting_red_disk()
        {
            return murphy.transition(position, world);
        }

        // Snik Snak turn cycles use two different global modulo-four phases,
        // while their penultimate transfer update also owns source cleanup.
        // Intercept both before generic per-tick animation advancement.
        if let Self::SnikSnak(snik_snak) = self {
            if world.freeze_enemies() {
                return None;
            }
            match state.animation().kind() {
                AnimationKind::SnikSnakTurn(_) => {
                    return snik_snak.transition(state, position, world);
                }
                AnimationKind::SnikSnakMove(direction) if state.animation().frame() == 6 => {
                    return Some(
                        snik_snak.advance_penultimate_movement(position, direction, state, world),
                    );
                }
                AnimationKind::SnikSnakMove(_) => {}
                _ => {}
            }
        }

        // Electron turns and source cleanup obey the same split scheduling as
        // Snik Snaks, while retaining Electron-specific collision and residue.
        if let Self::Electron(electron) = self {
            if world.freeze_enemies() {
                return None;
            }
            match state.animation().kind() {
                AnimationKind::ElectronTurn(_) => {
                    return electron.transition(state, position, world);
                }
                AnimationKind::ElectronMove(direction) if state.animation().frame() == 6 => {
                    return Some(
                        electron.advance_penultimate_movement(position, direction, state, world),
                    );
                }
                AnimationKind::ElectronMove(_) => {}
                _ => {}
            }
        }

        // Original falling state 0x15 increments to 0x16 and clears the prior
        // cell before the final two pictures play. The temporary Space marker
        // has no updater of its own, so the destination must own this write.
        if matches!(self, Self::Zonk(_) | Self::Infotron(_))
            && let AnimationKind::Moving(direction) = state.animation().kind()
            && state.animation().frame() == 5
        {
            return Some(self.advance_falling_source(position, direction, world));
        }

        // Reserved push/snap targets retain their actor identity for rendering
        // and blast interactions but must not fall, roll, or otherwise update.
        if state.animation().kind() == AnimationKind::MurphyPushTarget {
            return None;
        }

        // Freeze pauses both stable and pre-fall Zonks. A transfer already
        // represented by synchronized destination/source animations must
        // finish, or its Vacating source would release out of phase with a
        // permanently paused destination.
        if matches!(self, Self::Zonk(_)) && world.freeze_zonks() && !state.animation().is_movement()
        {
            return None;
        }

        // The game session owns a planted fuse even while Murphy covers its
        // cell. It updates the State frame after the linear actor pass, preventing the
        // visible actor and concealed timer from advancing independently.
        if state.animation().kind() == AnimationKind::RedDiskFuse
            && world.is_active_red_disk(position)
        {
            return None;
        }

        // Bug state changes happen only on the global four-tick cadence. Murphy
        // has already interacted before this row-major actor callback, which
        // preserves the active→safe and safe→active collision boundaries.
        if matches!(
            state.animation().kind(),
            AnimationKind::Bug
                | AnimationKind::BugDormant
                | AnimationKind::Explosion
                | AnimationKind::ElectronExplosion
        ) && !world.tick_count().is_multiple_of(4)
        {
            return None;
        }

        // Animations advance before actor decisions. Their terminal action may
        // settle, explode, or explicitly hand control back to the actor.
        match state.animation().advance() {
            AnimationAdvance::Frame(animation) => {
                // The DOS Bug updater checks all eight neighbors after each
                // active quarter-tick frame is selected. Dormant frames and
                // non-Bug animations pass through without an audio request.
                let events = if matches!(self, Self::Bug(_))
                    && animation.kind() == AnimationKind::Bug
                    && world.has_neighboring_murphy(position)
                {
                    vec![GameEvent::PlaySound(SoundEffect::Bug)]
                } else {
                    Vec::new()
                };
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::animated(self.clone(), animation),
                    )],
                    events,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Settle) => {
                return Some(self.settle(position, state, world));
            }
            AnimationAdvance::Finished(AnimationNext::BeginZonkFall) => {
                let Self::Zonk(zonk) = self else {
                    // `Animation::zonk_pre_fall` is the only constructor for
                    // this promise. Recover malformed internal state as idle
                    // instead of allowing an unrelated actor to fall.
                    debug_assert!(false, "only a Zonk may finish pre-fall");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return zonk.begin_fall(position, world);
            }
            AnimationAdvance::Finished(AnimationNext::BeginInfotronFall) => {
                let Self::Infotron(infotron) = self else {
                    debug_assert!(false, "only an Infotron may finish Infotron pre-fall");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return infotron.begin_fall(position, world);
            }
            AnimationAdvance::Finished(AnimationNext::BeginRoundedSlide { direction }) => {
                let side = world.offset(position, direction);
                let diagonal = side.and_then(|cell| world.offset(cell, Direction::Down));
                let side_reserved = side
                    .and_then(|cell| world.state(cell))
                    .is_some_and(|state| {
                        matches!(state.actor(), Actor::Empty(_))
                            && state.animation().kind() == AnimationKind::RoundedSide
                    });

                if side_reserved && diagonal.is_some_and(|cell| world.is_empty(cell)) {
                    let side = side.expect("a validated side reservation has a position");
                    let diagonal = diagonal.expect("a validated diagonal has a position");
                    let falling_actor = match self {
                        Self::Zonk(_) => Self::Zonk(Zonk::falling()),
                        Self::Infotron(_) => Self::Infotron(Infotron::falling()),
                        _ => {
                            debug_assert!(false, "only rounded actors may begin a side slide");
                            return Some(Transition::replace(position, State::new(self.clone())));
                        }
                    };
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                side,
                                State::animated(falling_actor, Animation::rolling(direction)),
                            ),
                            CellWrite::new(diagonal, State::rounded_destination()),
                        ],
                        Vec::new(),
                    ));
                }

                if side_reserved {
                    // State 0x51 is deliberately sticky: another actor may
                    // vacate the diagonal later while the side stays reserved.
                    return Some(Transition::replace(
                        position,
                        State::animated(self.clone(), Animation::rounded_wait(direction)),
                    ));
                }

                // A blast can consume the reservation. Recover the rounded
                // actor as stable without overwriting the new side occupant.
                return Some(Transition::replace(position, State::new(self.clone())));
            }
            AnimationAdvance::Finished(AnimationNext::BeginRoundedFall { direction: _ }) => {
                debug_assert!(matches!(self, Self::Zonk(_) | Self::Infotron(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation().kind() == AnimationKind::RoundedDestination
                }) {
                    return Some(Transition::replace(position, State::new(self.clone())));
                }
                return Some(Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(
                            destination,
                            State::animated(self.clone(), Animation::moving(Direction::Down)),
                        ),
                    ],
                    Vec::new(),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BeginOrangeFall) => {
                debug_assert!(matches!(self, Self::OrangeDisk(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation().kind() == AnimationKind::RoundedDestination
                }) {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::OrangeDisk(OrangeDisk::resting())),
                    ));
                }
                return Some(Transition::replace(
                    position,
                    State::animated(self.clone(), Animation::orange_falling()),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishOrangeFall) => {
                debug_assert!(matches!(self, Self::OrangeDisk(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::OrangeDisk(OrangeDisk::resting())),
                    ));
                };
                let destination_reserved = world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation().kind() == AnimationKind::RoundedDestination
                });
                if !destination_reserved {
                    return Some(explode_at(world, position, false));
                }

                let landing_cell = world.offset(destination, Direction::Down);
                if landing_cell.is_some_and(|cell| world.is_empty(cell)) {
                    let landing_cell = landing_cell.expect("validated landing cell exists");
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                destination,
                                State::animated(
                                    Self::OrangeDisk(OrangeDisk::falling()),
                                    Animation::orange_falling(),
                                ),
                            ),
                            CellWrite::new(landing_cell, State::rounded_destination()),
                        ],
                        Vec::new(),
                    ));
                }

                if landing_cell
                    .and_then(|cell| world.state(cell))
                    .is_some_and(|state| matches!(state.actor(), Actor::Explosion(_)))
                {
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                destination,
                                State::new(Self::OrangeDisk(OrangeDisk::resting())),
                            ),
                        ],
                        Vec::new(),
                    ));
                }

                let mut explosion = explode_at(world, destination, false);
                // The DOS routine clears the old falling source before it
                // detonates the newly occupied destination. Our immutable
                // WorldView still exposes the Orange Disk at that source, so
                // discard only the spurious delayed timer it would otherwise
                // receive as a reactive neighbor. The immediate blast write at
                // the source remains: the new 3x3 wave legitimately covers it.
                explosion.events.retain(|event| {
                    !matches!(
                        event,
                        GameEvent::ScheduleExplosion {
                            position: scheduled,
                            ..
                        } if *scheduled == position
                    )
                });
                explosion
                    .writes
                    .insert(0, CellWrite::new(position, State::empty()));
                return Some(explosion);
            }
            AnimationAdvance::Finished(AnimationNext::RandomizeBug) => {
                // The shared session RNG must be consumed at application time
                // so Bugs ending together draw distinct values in row order.
                debug_assert!(matches!(self, Self::Bug(_)));
                return Some(Transition::new(
                    Vec::new(),
                    vec![GameEvent::RandomizeBug(position)],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::ActivateBug) => {
                debug_assert!(matches!(self, Self::Bug(_)));
                // Reaching active frame zero also performs the proximity check;
                // waiting until frame one would omit one original Bug chirp.
                let events = world
                    .has_neighboring_murphy(position)
                    .then_some(GameEvent::PlaySound(SoundEffect::Bug))
                    .into_iter()
                    .collect();
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::animated(Self::Bug(Bug), Animation::bug_active()),
                    )],
                    events,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishSnikSnakMove { direction }) => {
                let Self::SnikSnak(snik_snak) = self else {
                    // Only `Animation::snik_snak_move` constructs this promise.
                    // Recover malformed state without mutating neighboring cells.
                    debug_assert!(false, "only a Snik Snak may finish this movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return Some(snik_snak.finish_movement(position, direction, world));
            }
            AnimationAdvance::Finished(AnimationNext::FinishElectronMove { direction }) => {
                let Self::Electron(electron) = self else {
                    // Only `Animation::electron_move` constructs this promise;
                    // avoid neighbor writes if internal state is corrupted.
                    debug_assert!(false, "only an Electron may finish this movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return Some(electron.finish_movement(position, direction, world));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeEmpty) => {
                return Some(Transition::new(
                    vec![CellWrite::new(position, State::empty())],
                    vec![GameEvent::ExplosionFinished],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeInfotron) => {
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::new(Actor::Infotron(Infotron::resting())),
                    )],
                    vec![GameEvent::ExplosionFinished],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Explode(residue)) => {
                // Only disk fuses use this animation promise. Secondary actor
                // chains are represented by the game's independent timer array.
                return Some(explode_at(
                    world,
                    position,
                    residue == ExplosionResidue::Infotron,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::ResumeMurphy) => {
                // A movement's final pose intentionally survives one complete
                // update after its source reservation is released. Only now
                // may fresh input begin another move, matching the original
                // separation between movement completion and direction input.
                let Self::Murphy(murphy) = self else {
                    // `Animation::murphy_ready` is the sole constructor for
                    // this promise, so a different actor would be an internal
                    // state-construction bug. Recover as a stable actor in
                    // release builds instead of leaving an immortal animation.
                    debug_assert!(false, "only Murphy may resume after movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };

                return murphy.transition(position, world).or_else(|| {
                    // No usable input leaves Murphy genuinely idle. This is
                    // not an extra movement frame: the final moving pose was
                    // already retained for the preceding simulation update.
                    Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
                    ))
                });
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphySnap { direction, target }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a snap action");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let mut writes = vec![CellWrite::new(position, State::new(Self::Murphy(*murphy)))];
                if let Some(target_position) = world.offset(position, direction)
                    && world.state(target_position).is_some_and(|target_state| {
                        target_state.animation().kind() == AnimationKind::MurphyPushTarget
                    })
                {
                    writes.push(CellWrite::new(target_position, State::empty()));
                }
                let events = match target {
                    MurphySnapTarget::Base => Vec::new(),
                    MurphySnapTarget::Infotron => vec![GameEvent::CollectInfotron],
                    MurphySnapTarget::RedDisk => vec![GameEvent::CollectRedDisk],
                };
                return Some(Transition::new(writes, events));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyPush { direction, target }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a push action");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(target_position) = world.offset(position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(destination) = world.offset(target_position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let reservations_intact = world.state(target_position).is_some_and(|state| {
                    state.animation().kind() == AnimationKind::MurphyPushTarget
                        && pushed_actor_matches(state.actor(), target)
                }) && world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation().kind() == AnimationKind::MurphyDestination
                });
                if !reservations_intact {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
                    ));
                }
                let pushed_state = if target == MurphyPushTarget::OrangeDisk
                    && direction == Direction::Right
                    && world
                        .offset(destination, Direction::Down)
                        .is_some_and(|below| world.is_empty(below))
                {
                    State::animated(
                        Self::OrangeDisk(OrangeDisk::falling()),
                        Animation::orange_pre_fall(),
                    )
                } else {
                    State::new(actor_for_push_target(target))
                };
                let mut writes = vec![
                    CellWrite::new(position, State::empty()),
                    CellWrite::new(target_position, State::new(Self::Murphy(*murphy))),
                    CellWrite::new(destination, pushed_state),
                ];
                if target == MurphyPushTarget::OrangeDisk
                    && direction == Direction::Right
                    && let Some(below) = world.offset(destination, Direction::Down)
                    && world.is_empty(below)
                {
                    // The original right-push completion immediately installs
                    // Orange state 0x20 and its destination reservation. The
                    // corresponding left-push path intentionally does not.
                    writes.push(CellWrite::new(below, State::rounded_destination()));
                }
                return Some(Transition::new(writes, Vec::new()));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyPort { direction }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a port traversal");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(port_position) = world.offset(position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(destination) = world.offset(port_position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation().kind() == AnimationKind::MurphyDestination
                }) {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
                    ));
                }
                let events = world
                    .state(port_position)
                    .and_then(|state| match state.actor() {
                        Actor::Port(port) if port.is_special() => world
                            .special_port(port_position)
                            .copied()
                            .map(GameEvent::ApplySpecialPort),
                        _ => None,
                    })
                    .into_iter()
                    .collect();
                return Some(Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(destination, State::new(Self::Murphy(*murphy))),
                    ],
                    events,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyExit) => {
                debug_assert!(matches!(self, Self::Murphy(_)));
                // Success was recorded when Murphy entered the Exit. Finishing
                // its artwork now only removes his disappearing sprite.
                return Some(Transition::replace(position, State::empty()));
            }
            AnimationAdvance::Finished(AnimationNext::Act) | AnimationAdvance::Ready => {}
        }

        match self {
            Self::Empty(actor) => actor.transition(position, world),
            Self::Zonk(actor) => actor.transition(position, world),
            Self::Base(actor) => actor.transition(position, world),
            Self::Murphy(actor) => actor.transition(position, world),
            Self::Infotron(actor) => actor.transition(position, world),
            Self::RamChip(actor) => actor.transition(position, world),
            Self::Hardware(actor) => actor.transition(position, world),
            Self::Exit(actor) => actor.transition(position, world),
            Self::OrangeDisk(actor) => actor.transition(position, world),
            Self::Port(actor) => actor.transition(position, world),
            Self::SnikSnak(actor) => actor.transition(state, position, world),
            Self::YellowDisk(actor) => actor.transition(position, world),
            Self::Terminal(actor) => actor.transition(position, world),
            Self::RedDisk(actor) => actor.transition(position, world),
            Self::Electron(actor) => actor.transition(state, position, world),
            Self::Bug(actor) => actor.transition(position, world),
            Self::InvisibleWall(actor) => actor.transition(position, world),
            Self::Explosion(actor) => actor.transition(position, world),
        }
    }

    /// Returns the validated starting/resting animation for this actor type.
    fn default_animation(&self) -> Animation {
        match self {
            Self::SnikSnak(actor) => Animation::snik_snak_turn(
                EnemyTurn::Left,
                EnemyTurn::Left.initial_frame(actor.heading()),
            ),
            Self::Electron(actor) => Animation::electron_turn(
                EnemyTurn::Left,
                EnemyTurn::Left.initial_frame(actor.heading()),
            ),
            Self::Bug(_) => Animation::bug_active(),
            Self::Terminal(terminal) => Animation::terminal(terminal.screen_frame()),
            Self::Explosion(explosion) => Animation::explosion(explosion.residue()),
            _ => Animation::idle(),
        }
    }

    /// Advances a fall to state `0x16` while releasing its temporary old cell.
    fn advance_falling_source(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Transition {
        debug_assert!(matches!(self, Self::Zonk(_) | Self::Infotron(_)));
        debug_assert_eq!(direction, Direction::Down);
        let mut writes = vec![CellWrite::new(
            position,
            State::animated(self.clone(), Animation::moving_at(direction, 6)),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                matches!(source_state.actor(), Actor::Empty(_))
                    && source_state.animation().kind() == AnimationKind::Vacating(direction)
            })
        {
            // Preserve an actor or explosion that already consumed the source;
            // only the still-matching temporary marker may become true Space.
            writes.push(CellWrite::new(source, State::empty()));
        }

        Transition::new(writes, Vec::new())
    }

    /// Resolves movement completion using the latest neighboring cell states.
    fn settle(&self, position: Position, state: &State, world: &WorldView<'_>) -> Transition {
        match self {
            Self::Murphy(_) if state.animation().is_movement() => {
                // Completion keeps the last interpolated pose, but changes its
                // promised action to input resumption. The synchronized source
                // reservation releases in this same tick. Murphy is processed
                // before the later row-major actor pass, so trailing hazards
                // observe the updated reservation rather than stale occupancy.
                let (direction, source_distance, target) = match state.animation().kind() {
                    AnimationKind::Murphy(MurphyAnimation::Move {
                        direction, target, ..
                    }) => (direction, 1, target),
                    // Murphy never owns Rolling, but retaining a total fallback
                    // makes malformed internal states settle without erasing a
                    // potentially unrelated neighboring cell.
                    _ => {
                        return Transition::replace(
                            position,
                            State::animated(
                                self.clone(),
                                Animation::murphy_ready(state.animation().kind()),
                            ),
                        );
                    }
                };
                let mut source = Some(position);
                for _ in 0..source_distance {
                    source = source.and_then(|cell| world.offset(cell, direction.opposite()));
                }

                let mut writes = vec![CellWrite::new(
                    position,
                    State::animated(
                        self.clone(),
                        Animation::murphy_ready(state.animation().kind()),
                    ),
                )];
                if let Some(source) = source
                    && world.state(source).is_some_and(|source_state| {
                        matches!(source_state.actor(), Actor::Empty(_))
                            && source_state.animation().kind() == AnimationKind::Vacating(direction)
                    })
                {
                    // Never erase an explosion or actor that replaced the
                    // reservation while Murphy was in flight.
                    writes.push(CellWrite::new(source, State::empty()));
                }
                let events = match target {
                    MurphyMoveTarget::Empty | MurphyMoveTarget::Base => Vec::new(),
                    MurphyMoveTarget::Infotron => vec![GameEvent::CollectInfotron],
                    MurphyMoveTarget::RedDisk => vec![GameEvent::CollectRedDisk],
                    MurphyMoveTarget::PlantedRedDisk => Vec::new(),
                };
                return Transition::new(writes, events);
            }
            Self::Zonk(_) if world.freeze_zonks() && state.animation().is_movement() => {
                // The in-flight transfer reaches its destination, then loses
                // falling momentum without inspecting the next cell. This is
                // where an original in-flight Zonk first observes freeze.
                return Transition::replace(position, State::new(Self::Zonk(Zonk::resting())));
            }
            Self::Zonk(zonk) if zonk.is_falling() => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        match target.actor() {
                            Actor::Murphy(_) if murphy_is_protected_from_falling_actor(target) => {
                                // Horizontal push states 0x0e/0x0f/0x25/
                                // 0x26/0x28/0x29 are explicit original crush
                                // exceptions. The DOS routine returns before
                                // its later Fall-sound call on this path.
                                return Transition::replace(
                                    position,
                                    State::new(Self::Zonk(Zonk::resting())),
                                );
                            }
                            Actor::Murphy(_) => {
                                // Murphy has already taken his player-first
                                // update this tick. Remaining here therefore
                                // means the falling Zonk genuinely crushes him.
                                return explode_at(world, below, false);
                            }
                            Actor::SnikSnak(_) | Actor::Electron(_) => {
                                return explode_at(world, below, false);
                            }
                            Actor::OrangeDisk(_) if target.is_idle() => {
                                // A Zonk arms an otherwise stable Orange Disk
                                // after a short delay while itself comes to rest.
                                let orange = State::animated(
                                    Actor::OrangeDisk(OrangeDisk::resting()),
                                    Animation::orange_disk_fuse(),
                                );
                                return Transition::new(
                                    vec![
                                        CellWrite::new(
                                            position,
                                            State::new(Self::Zonk(Zonk::resting())),
                                        ),
                                        CellWrite::new(below, orange),
                                    ],
                                    Vec::new(),
                                );
                            }
                            _ => {}
                        }
                    }
                    if world.is_empty(below) {
                        // Retained momentum begins the next cell transfer on
                        // this completion callback. Only the first unsupported
                        // resting state uses `ZonkPreFall`; inserting an idle
                        // update here would make a long fall visibly stutter.
                        return Transition::move_actor(
                            position,
                            below,
                            Self::Zonk(Zonk::falling()),
                            Direction::Down,
                        );
                    }
                    // Landing on a non-reactive occupant is the one safe Zonk
                    // terminal path that selects the original Fall effect.
                    return Transition::new(
                        vec![CellWrite::new(
                            position,
                            State::new(Self::Zonk(Zonk::resting())),
                        )],
                        vec![GameEvent::PlaySound(SoundEffect::Fall)],
                    );
                }
            }
            Self::Infotron(infotron) if infotron.is_falling() => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        if matches!(target.actor(), Actor::Murphy(_)) {
                            if murphy_is_protected_from_falling_actor(target) {
                                // Protected push states use the same silent
                                // early return as the Zonk landing routine.
                                return Transition::replace(
                                    position,
                                    State::new(Self::Infotron(Infotron::resting())),
                                );
                            }
                            // Sequential player-first mutation has already
                            // decided whether Murphy escaped before this hit.
                            return explode_at(world, below, false);
                        }
                        let hits_living_actor =
                            matches!(target.actor(), Actor::SnikSnak(_) | Actor::Electron(_));
                        let hits_idle_disk = target.is_idle()
                            && matches!(
                                target.actor(),
                                Actor::RedDisk(_) | Actor::YellowDisk(_) | Actor::OrangeDisk(_)
                            );
                        let hits_active_red_disk = matches!(target.actor(), Actor::RedDisk(_))
                            && world.is_active_red_disk(below);
                        if hits_living_actor || hits_idle_disk || hits_active_red_disk {
                            return explode_at(world, below, false);
                        }
                    }
                    let still_falling = world.is_empty(below);
                    // A continued vertical transfer remains silent. Only the
                    // first obstructed settle matches `playFallSound`.
                    let events = (!still_falling)
                        .then_some(GameEvent::PlaySound(SoundEffect::Fall))
                        .into_iter()
                        .collect();
                    return Transition::new(
                        vec![CellWrite::new(
                            position,
                            State::new(Self::Infotron(if still_falling {
                                Infotron::falling()
                            } else {
                                Infotron::resting()
                            })),
                        )],
                        events,
                    );
                }
            }
            Self::OrangeDisk(disk) if disk.is_falling() => {
                if let Some(below) = world.offset(position, Direction::Down)
                    && !world.is_empty(below)
                {
                    return explode_at(world, position, false);
                }
                return Transition::replace(position, State::new(Self::OrangeDisk(*disk)));
            }
            _ => {}
        }

        // Actors without a special landing rule simply retain their persistent
        // fields and return to their type-specific idle or cyclic animation.
        Transition::replace(position, State::new(self.clone()))
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
