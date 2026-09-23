//! Zonk gravity, delayed falls, and left-first rounded-support rolls.

use super::murphy::murphy_is_protected_from_falling_actor;
use super::rounded::{RoundedActor, RoundedPhase};
use super::{
    Actor, CellWrite, Direction, Frame, GameEvent, OrangeDisk, Position, State, Transition,
    explode_at,
};
use crate::game::{SoundEffect, WorldView};

/// A rounded rock that falls, rolls, can be pushed, and can crush actors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Zonk {
    /// The complete legal physical phase; momentum is derived from this value.
    phase: RoundedPhase,
}

impl Zonk {
    /// Creates this actor from a phase belonging to rounded-object physics.
    ///
    /// A player phase cannot be assigned to a rock.
    /// ```compile_fail
    /// use supaplex_clone::actors::{Zonk, murphy::MurphyPhase};
    /// let rock = Zonk::from_phase(MurphyPhase::Ready);
    /// ```
    pub const fn from_phase(phase: RoundedPhase) -> Self {
        Self { phase }
    }

    /// Returns the physical state for typed dispatch and collision queries.
    pub const fn phase(self) -> RoundedPhase {
        self.phase
    }

    /// Replaces only this concrete actor's physical phase.
    pub(super) fn in_phase(mut self, phase: RoundedPhase) -> State {
        self.phase = phase;
        State::new(Actor::Zonk(self))
    }

    /// Creates a stationary Zonk as read from a level record.
    pub const fn resting() -> Self {
        Self {
            phase: RoundedPhase::Resting,
        }
    }

    /// Reports whether this Zonk currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.phase.is_falling()
    }

    /// Chooses a fall or roll from the full states of neighboring cells.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        // Frozen Zonks keep both their position and their exact animation state.
        if world.freeze_zonks() {
            return None;
        }

        let below = world.offset(position, Direction::Down)?;
        if matches!(self.phase, RoundedPhase::Momentum) {
            let continuation = world.state(below).and_then(State::reservation)
                == Some(super::empty::Reservation::RoundedContinuation);
            return (world.is_empty(below) || continuation)
                .then(|| RoundedActor::Zonk(*self).resume_fall(position, below));
        }
        if world.is_empty(below) {
            // The initiating callback consumes the first arming update (40 ->
            // 41). The following callback transfers without drawing picture zero.
            return Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::AwaitingFall),
            ));
        }
        RoundedActor::Zonk(*self).start_roll(position, world)
    }

    /// Starts the armed fall when its destination survived the intervening tick.
    pub(super) fn begin_fall(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if !world.is_empty(below) {
            // OpenSupaplex holds state `0x41` while the destination is blocked.
            // In particular, it does not reconsider a diagonal roll until the
            // pending vertical fall either succeeds or the Zonk is replaced.
            return None;
        }

        Some(Transition::move_actor(
            position,
            below,
            RoundedActor::Zonk(Self {
                phase: RoundedPhase::Momentum,
            }),
        ))
    }
}

impl Zonk {
    /// Resolves crushes, landing sounds, and retained momentum after a full fall.
    pub(super) fn land(&self, position: Position, world: &WorldView<'_>) -> Transition {
        if world.freeze_zonks() {
            return Transition::replace(position, State::new(Actor::Zonk(Self::resting())));
        }
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
                            State::new(Actor::Zonk(Zonk::resting())),
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
                        let orange = OrangeDisk::resting()
                            .in_phase(super::orange_disk::OrangePhase::Fuse(Frame::first()));
                        return Transition::new(
                            vec![
                                CellWrite::new(position, State::new(Actor::Zonk(Zonk::resting()))),
                                CellWrite::new(below, orange),
                            ],
                            Vec::new(),
                        );
                    }
                    _ => {}
                }
            }
            if world.is_empty(below) {
                return RoundedActor::Zonk(*self).continue_fall(position, below);
            }
            // Landing can reserve a new roll, but its first picture belongs to
            // the following callback. The current callback still draws fall 7.
            let mut transition = RoundedActor::Zonk(*self)
                .reserve_roll(position, world)
                .unwrap_or_else(|| {
                    Transition::replace(position, State::new(Actor::Zonk(Self::resting())))
                });
            transition
                .events
                .push(GameEvent::PlaySound(SoundEffect::Fall));
            return transition;
        }
        Transition::replace(position, self.in_phase(RoundedPhase::Resting))
    }
}
