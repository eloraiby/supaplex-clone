//! Collectible Infotron gravity and rounded-support rolls.

use super::murphy::murphy_is_protected_from_falling_actor;
use super::rounded::{RoundedActor, RoundedPhase};
use super::{
    Actor, Animation, CellWrite, Direction, GameEvent, Horizontal, Position, State, Transition,
    explode_at,
};
use crate::game::{SoundEffect, WorldView};

/// A collectible that falls and rolls with Zonk-like physics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Infotron {
    /// The complete legal physical phase; momentum is derived from this value.
    phase: RoundedPhase,
}

impl Infotron {
    /// Returns the physical state for typed dispatch and collision queries.
    pub const fn phase(self) -> RoundedPhase {
        self.phase
    }

    /// Replaces only this concrete actor's physical phase.
    pub(super) fn in_phase(mut self, phase: RoundedPhase) -> State {
        self.phase = phase;
        State::new(Actor::Infotron(self))
    }

    /// Derives the rendering view from the single authoritative phase.
    pub(super) fn animation(self) -> Animation {
        self.phase.animation(super::AnimationKind::InfotronPreFall)
    }

    /// Creates the momentum-bearing state used by completed falls and pushes.
    pub(super) const fn falling() -> Self {
        Self {
            phase: RoundedPhase::Momentum,
        }
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
        if world.is_empty(below) {
            if self.phase.is_falling() {
                return Some(Transition::move_actor(
                    position,
                    below,
                    RoundedActor::Infotron(*self),
                ));
            }

            return Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::AwaitingFall),
            ));
        }

        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Horizontal::Left, Horizontal::Right] {
            let side = world.offset(position, direction.direction())?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::prepare_rounded_roll(
                    position,
                    side,
                    RoundedActor::Infotron(*self),
                    direction,
                ));
            }
        }

        None
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
    /// Land: resolve this actor-owned phase against the live board.
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
                    State::new(Actor::Infotron(if still_falling {
                        Infotron::falling()
                    } else {
                        Infotron::resting()
                    })),
                )],
                events,
            );
        }
        Transition::replace(position, self.in_phase(RoundedPhase::Momentum))
    }
}
