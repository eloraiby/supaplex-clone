//! Collectible Infotron gravity and rounded-support rolls.

use super::murphy::murphy_is_protected_from_falling_actor;
use super::rounded::{RoundedActor, RoundedPhase};
use super::{Actor, Direction, GameEvent, Position, State, Transition, explode_at};
use crate::game::{SoundEffect, WorldView};

/// A collectible that falls and rolls with Zonk-like physics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Infotron {
    /// The complete legal physical phase; momentum is derived from this value.
    phase: RoundedPhase,
}

impl Infotron {
    /// Creates this actor from a phase belonging to rounded-object physics.
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
        State::new(Actor::Infotron(self))
    }

    /// Creates a stationary collectible as read from a level record.
    pub const fn resting() -> Self {
        Self {
            phase: RoundedPhase::Resting,
        }
    }

    /// Reports whether this Infotron currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.phase.is_falling()
    }

    /// Chooses a fall or rounded-support roll from neighboring states.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if matches!(self.phase, RoundedPhase::Momentum) {
            let continuation = world.state(below).and_then(State::reservation)
                == Some(super::empty::Reservation::RoundedContinuation);
            return (world.is_empty(below) || continuation)
                .then(|| RoundedActor::Infotron(*self).resume_fall(position, below));
        }
        if world.is_empty(below) {
            // The initiating callback consumes the first arming update (40 ->
            // 41). The following callback transfers without drawing picture zero.
            return Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::AwaitingFall),
            ));
        }
        RoundedActor::Infotron(*self).start_roll(position, world)
    }

    /// Starts a resting Infotron fall after its destination survives one update.
    pub(super) fn begin_fall(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if !world.is_empty(below) {
            return None;
        }

        Some(Transition::move_actor(
            position,
            below,
            RoundedActor::Infotron(Self {
                phase: RoundedPhase::Momentum,
            }),
        ))
    }
}

impl Infotron {
    /// Resolves crushes, landing sounds, and retained momentum after a full fall.
    pub(super) fn land(&self, position: Position, world: &WorldView<'_>) -> Transition {
        if let Some(below) = world.offset(position, Direction::Down) {
            if let Some(target) = world.state(below) {
                if matches!(target.actor(), Actor::Murphy(_)) {
                    if murphy_is_protected_from_falling_actor(target) {
                        // Protected push states use the same silent
                        // early return as the Zonk landing routine.
                        return Transition::replace(
                            position,
                            State::new(Actor::Infotron(Infotron::resting())),
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
                let hits_active_red_disk =
                    matches!(target.actor(), Actor::RedDisk(_)) && world.is_active_red_disk(below);
                if hits_living_actor || hits_idle_disk || hits_active_red_disk {
                    return explode_at(world, below, false);
                }
            }
            if world.is_empty(below) {
                return RoundedActor::Infotron(*self).continue_fall(position, below);
            }
            let mut transition = RoundedActor::Infotron(*self)
                .reserve_roll(position, world)
                .unwrap_or_else(|| {
                    Transition::replace(position, State::new(Actor::Infotron(Self::resting())))
                });
            transition
                .events
                .push(GameEvent::PlaySound(SoundEffect::Fall));
            return transition;
        }
        Transition::replace(position, self.in_phase(RoundedPhase::Resting))
    }
}
