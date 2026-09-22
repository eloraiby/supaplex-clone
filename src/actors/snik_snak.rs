//! Snik Snak turn cadence, movement reservations, and contact rules.

use super::{
    Actor, Animation, AnimationKind, CellWrite, Direction, EnemyTurn, Position, State, Transition,
    explode_at, murphy_is_crossing_port,
};
use crate::game::WorldView;

/// Scissor-like enemy that follows walls and explodes on contact with Murphy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnikSnak {
    /// Direction used as the basis of the next left-hand wall-following choice.
    heading: Direction,
}

impl SnikSnak {
    /// Creates a Snik Snak whose first left-turn candidate follows `heading`.
    pub const fn new(heading: Direction) -> Self {
        Self { heading }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Advances or evaluates the current globally phased turn animation.
    pub(super) fn transition(
        &self,
        state: &State,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        if world.freeze_enemies() {
            // Original enemy freeze returns before changing either the state
            // byte or framebuffer, so retaining the complete State is required.
            return None;
        }

        let AnimationKind::SnikSnakTurn(turn) = state.animation.kind else {
            // Transfers are handled by the generic finite-animation path. This
            // fallback makes an internally malformed idle Snik Snak recover to
            // the correct left-turn cycle without inventing an instant step.
            debug_assert!(
                matches!(state.animation.kind, AnimationKind::Idle),
                "Snik Snak decisions require a turn animation"
            );
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::SnikSnak(*self),
                    Animation::snik_snak_turn(
                        EnemyTurn::Left,
                        EnemyTurn::Left.initial_frame(self.heading),
                    ),
                ),
            ));
        };

        if world.tick_count().is_multiple_of(4) {
            // The original draws the current turn picture and then increments
            // its low three state bits, wrapping within the selected cycle.
            let next_frame = (state.animation.frame + 1) & 7;
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::SnikSnak(*self),
                    Animation::snik_snak_turn(turn, next_frame),
                ),
            ));
        }

        if world.tick_count() % 4 != 3 {
            return None;
        }

        let direction = turn.direction_at_frame(state.animation.frame)?;
        let destination = world.offset(position, direction)?;
        if world.is_empty(destination) {
            return Some(Transition::move_snik_snak(
                position,
                destination,
                Actor::SnikSnak(Self { heading: direction }),
                direction,
            ));
        }

        let target_is_vulnerable_murphy = world.state(destination).is_some_and(|target| {
            matches!(target.actor(), Actor::Murphy(_)) && !murphy_is_crossing_port(target)
        });
        target_is_vulnerable_murphy.then(|| explode_at(world, position, false))
    }

    /// Releases the old source on the original seventh movement callback.
    pub(super) fn advance_penultimate_movement(
        &self,
        position: Position,
        direction: Direction,
        state: &State,
        world: &WorldView<'_>,
    ) -> Transition {
        debug_assert_eq!(state.animation.frame, 6);
        let mut writes = vec![CellWrite::new(
            position,
            State::animated(
                Actor::SnikSnak(*self),
                Animation::snik_snak_move_at(direction, 7),
            ),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                matches!(source_state.actor(), Actor::Empty(_))
                    && source_state.animation.kind == AnimationKind::SnikSnakVacating(direction)
            })
        {
            // A blast may already have replaced the reservation. As in the DOS
            // routine, never erase an Explosion encountered at the old source.
            writes.push(CellWrite::new(source, State::empty()));
        }

        Transition::new(writes, Vec::new())
    }

    /// Resolves left, forward, right, then turn-around after a completed move.
    pub(super) fn finish_movement(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Transition {
        let left = direction.left();
        if self.is_empty_or_murphy(position, left, world) {
            return self.begin_turn(position, EnemyTurn::Left, left);
        }

        if let Some(forward) = world.offset(position, direction) {
            if world.is_empty(forward) {
                return Transition::move_snik_snak(
                    position,
                    forward,
                    Actor::SnikSnak(*self),
                    direction,
                );
            }
            if world
                .state(forward)
                .is_some_and(|state| matches!(state.actor(), Actor::Murphy(_)))
            {
                // Unlike side contact, forward contact detonates immediately
                // and does not exempt Murphy while he crosses a port.
                return explode_at(world, position, false);
            }
        }

        let right = direction.right();
        if self.is_empty_or_murphy(position, right, world) {
            return self.begin_turn(position, EnemyTurn::Right, right);
        }

        // A dead end begins a counter-clockwise scan from the left candidate;
        // it does not teleport the enemy into the cell behind it.
        self.begin_turn(position, EnemyTurn::Left, left)
    }

    /// Reports whether a side cell causes a turn without immediate attack.
    fn is_empty_or_murphy(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> bool {
        // The original movement-completion routines treat Murphy exactly like
        // Space for side-choice purposes. Contact is reconsidered only after
        // the turn cycle reaches that direction on a later quarter tick.
        world
            .offset(position, direction)
            .and_then(|target| world.state(target))
            .is_some_and(|state| state.is_empty() || matches!(state.actor(), Actor::Murphy(_)))
    }

    /// Builds the odd intermediate frame preceding one side candidate.
    fn begin_turn(&self, position: Position, turn: EnemyTurn, candidate: Direction) -> Transition {
        let animation = Animation::snik_snak_turn(turn, turn.preceding_frame(candidate));
        Transition::replace(position, State::animated(Actor::SnikSnak(*self), animation))
    }
}
