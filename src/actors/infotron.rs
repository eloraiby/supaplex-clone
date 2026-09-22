//! Collectible Infotron gravity and rounded-support rolls.

use super::{Actor, Animation, Direction, Position, State, Transition};
use crate::game::WorldView;

/// A collectible that falls and rolls with Zonk-like physics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Infotron {
    /// Whether this Infotron has downward momentum from an earlier fall.
    falling: bool,
}

impl Infotron {
    /// Creates the momentum-bearing state used by completed falls and pushes.
    pub(super) const fn falling() -> Self {
        Self { falling: true }
    }

    /// Creates a stationary collectible as read from a level record.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether this Infotron currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    /// Chooses a fall or rounded-support roll from neighboring states.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if world.is_empty(below) {
            if self.falling {
                return Some(Transition::move_actor(
                    position,
                    below,
                    Actor::Infotron(*self),
                    Direction::Down,
                ));
            }

            return Some(Transition::replace(
                position,
                State::animated(Actor::Infotron(*self), Animation::infotron_pre_fall()),
            ));
        }

        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Direction::Left, Direction::Right] {
            let side = world.offset(position, direction)?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::prepare_rounded_roll(
                    position,
                    side,
                    Actor::Infotron(*self),
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
            Actor::Infotron(Self { falling: true }),
            Direction::Down,
        ))
    }
}
