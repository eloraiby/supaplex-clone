//! Electron turn cadence, movement reservations, and contact explosions.

use super::{
    Actor, Animation, AnimationKind, CellWrite, Direction, EnemyTurn, Position, State, Transition,
    explode_at,
};
use crate::game::WorldView;

/// Spark enemy that follows walls and produces Infotrons when destroyed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Electron {
    /// Direction used as the basis of the next right-hand wall-following choice.
    heading: Direction,
}

impl Electron {
    /// Creates an Electron whose first left-turn candidate follows `heading`.
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
            // Freezing must preserve both the exact turn picture and logical
            // state byte; restarting the Electron at frame zero changes paths.
            return None;
        }

        let AnimationKind::ElectronTurn(turn) = state.animation.kind else {
            // A valid transfer is consumed by the finite-animation path before
            // this method runs. Recover only malformed idle state here.
            debug_assert!(
                matches!(state.animation.kind, AnimationKind::Idle),
                "Electron decisions require a turn animation"
            );
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::Electron(*self),
                    Animation::electron_turn(
                        EnemyTurn::Left,
                        EnemyTurn::Left.initial_frame(self.heading),
                    ),
                ),
            ));
        };

        if world.tick_count().is_multiple_of(4) {
            // As with the original state byte, retain the selected cycle's high
            // group while its low three bits wrap from seven back to zero.
            let next_frame = (state.animation.frame + 1) & 7;
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::Electron(*self),
                    Animation::electron_turn(turn, next_frame),
                ),
            ));
        }

        if world.tick_count() % 4 != 3 {
            return None;
        }

        let direction = turn.direction_at_frame(state.animation.frame)?;
        let destination = world.offset(position, direction)?;
        if world.is_empty(destination) {
            return Some(Transition::move_electron(
                position,
                destination,
                Actor::Electron(Self { heading: direction }),
                direction,
            ));
        }

        // Unlike a Snik Snak, an Electron has no exception for Murphy's four
        // port states: any targeted Murphy state detonates an Infotron wave.
        world
            .state(destination)
            .is_some_and(|target| matches!(target.actor(), Actor::Murphy(_)))
            .then(|| explode_at(world, position, true))
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
                Actor::Electron(*self),
                Animation::electron_move_at(direction, 7),
            ),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                matches!(source_state.actor(), Actor::Empty(_))
                    && source_state.animation.kind == AnimationKind::ElectronVacating(direction)
            })
        {
            // An explosion that reached the old cell wins over movement cleanup
            // and must never be replaced by Space.
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
                return Transition::move_electron(
                    position,
                    forward,
                    Actor::Electron(*self),
                    direction,
                );
            }
            if world
                .state(forward)
                .is_some_and(|state| matches!(state.actor(), Actor::Murphy(_)))
            {
                return explode_at(world, position, true);
            }
        }

        let right = direction.right();
        if self.is_empty_or_murphy(position, right, world) {
            return self.begin_turn(position, EnemyTurn::Right, right);
        }

        // A fully blocked Electron starts a left-cycle U-turn, preserving the
        // chance to take a side cell that opens while the cycle is in progress.
        self.begin_turn(position, EnemyTurn::Left, left)
    }

    /// Reports whether a side cell requests a turn without attacking yet.
    fn is_empty_or_murphy(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> bool {
        // Side Murphy contact is intentionally deferred to the matching turn
        // state; only forward contact at movement completion explodes at once.
        world
            .offset(position, direction)
            .and_then(|target| world.state(target))
            .is_some_and(|state| state.is_empty() || matches!(state.actor(), Actor::Murphy(_)))
    }

    /// Builds the odd intermediate frame preceding one side candidate.
    fn begin_turn(&self, position: Position, turn: EnemyTurn, candidate: Direction) -> Transition {
        let animation = Animation::electron_turn(turn, turn.preceding_frame(candidate));
        Transition::replace(position, State::animated(Actor::Electron(*self), animation))
    }
}
