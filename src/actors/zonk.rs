//! Zonk gravity, delayed falls, and left-first rounded-support rolls.

use super::{Actor, Animation, Direction, Position, State, Transition};
use crate::game::WorldView;

/// A rounded rock that falls, rolls, can be pushed, and can crush actors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Zonk {
    /// Whether this Zonk has downward momentum from an earlier fall.
    falling: bool,
}

impl Zonk {
    /// Creates the momentum-bearing state used by completed falls and pushes.
    pub(super) const fn falling() -> Self {
        Self { falling: true }
    }

    /// Creates a stationary Zonk as read from a level record.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether this Zonk currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.falling
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
        if world.is_empty(below) {
            if self.falling {
                // Momentum from a completed fall continues directly into the
                // next cell. The one-update arming delay belongs only to a
                // stable Zonk beginning a new fall from rest.
                return Some(Transition::move_actor(
                    position,
                    below,
                    Actor::Zonk(*self),
                    Direction::Down,
                ));
            }

            // The original engine changes a resting Zonk to pre-fall state
            // `0x41` on this callback and transfers it only on the following
            // callback. That distinction lets Murphy move first when his old
            // source opens directly below a trailing Zonk.
            return Some(Transition::replace(
                position,
                State::animated(Actor::Zonk(*self), Animation::zonk_pre_fall()),
            ));
        }

        // Only a stable rounded support permits a diagonal roll. Inspecting the
        // support animation prevents rolling from a Zonk that is itself moving.
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
                    Actor::Zonk(*self),
                    direction,
                ));
            }
        }

        None
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
            Actor::Zonk(Self { falling: true }),
            Direction::Down,
        ))
    }
}
