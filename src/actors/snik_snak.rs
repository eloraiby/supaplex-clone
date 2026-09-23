//! Snik Snak turn cadence, movement reservations, and contact rules.

use super::enemy::EnemyPhase;
use super::murphy::murphy_is_crossing_port;
use super::{
    Actor, CellWrite, Direction, EnemyTurn, Frame, Position, State, Transition, explode_at,
};
use crate::game::WorldView;

/// Scissor-like enemy that follows walls and explodes on contact with Murphy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnikSnak {
    /// Direction used as the basis of the next left-hand wall-following choice.
    heading: Direction,
    /// Complete turn or movement state; no unrelated animation is representable.
    phase: EnemyPhase,
}

impl SnikSnak {
    /// Returns the complete enemy-specific phase.
    pub const fn phase(self) -> EnemyPhase {
        self.phase
    }

    /// Constructs a complete cell and synchronizes heading when a transfer starts.
    pub(super) fn in_phase(mut self, phase: EnemyPhase) -> State {
        if let EnemyPhase::Moving { direction, .. } = phase {
            self.heading = direction;
        }
        self.phase = phase;
        State::new(Actor::SnikSnak(self))
    }

    /// Advances only this enemy's phases, honoring global freeze and source cleanup.
    pub(super) fn update(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        if world.freeze_enemies() {
            return None;
        }
        match self.phase {
            EnemyPhase::Turning { turn, frame } => self.transition(turn, frame, position, world),
            EnemyPhase::Moving { direction, frame } => match frame.index() {
                6 => Some(self.advance_penultimate_movement(position, direction, world)),
                _ => Some(match frame.next() {
                    Some(frame) => Transition::paint(
                        position,
                        self.in_phase(EnemyPhase::Moving { direction, frame }),
                    ),
                    None => self.finish_movement(position, direction, world),
                }),
            },
        }
    }

    /// Creates a Snik Snak whose first left-turn candidate follows `heading`.
    pub const fn new(heading: Direction) -> Self {
        Self {
            heading,
            phase: EnemyPhase::Turning {
                turn: EnemyTurn::Left,
                frame: Frame::new(EnemyTurn::Left.initial_frame(heading)).unwrap(),
            },
        }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Advances or evaluates the current globally phased turn animation.
    pub(super) fn transition(
        &self,
        turn: EnemyTurn,
        frame: Frame<8>,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        if world.freeze_enemies() {
            // Original enemy freeze returns before changing either the state
            // byte or framebuffer, so retaining the complete State is required.
            return None;
        }

        if world.tick_count().is_multiple_of(4) {
            // The original draws the current turn picture and then increments
            // its low three state bits, wrapping within the selected cycle.
            let next_frame = (frame.index() + 1) & 7;
            return Some(Transition::paint(
                position,
                self.in_phase(EnemyPhase::Turning {
                    turn,
                    frame: Frame::new(next_frame).unwrap(),
                }),
            ));
        }

        if world.tick_count() % 4 != 3 {
            return None;
        }

        let direction = turn.direction_at_frame(frame.index())?;
        let destination = world.offset(position, direction)?;
        if world.is_empty(destination) {
            return Some(Transition::move_snik_snak(
                position,
                destination,
                Self::new(direction),
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
        world: &WorldView<'_>,
    ) -> Transition {
        let mut writes = vec![CellWrite::new(
            position,
            self.in_phase(EnemyPhase::Moving {
                direction,
                frame: Frame::last(),
            }),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                source_state.reservation()
                    == Some(super::empty::Reservation::SnikSnakSource(direction))
            })
        {
            // A blast may already have replaced the reservation. As in the DOS
            // routine, never erase an Explosion encountered at the old source.
            writes.push(CellWrite::new(source, State::empty()));
        }

        Transition::new(writes, Vec::new()).with_drawing(
            position,
            self.in_phase(EnemyPhase::Moving {
                direction,
                frame: Frame::last(),
            })
            .actor()
            .clone(),
        )
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
                return Transition::move_snik_snak(position, forward, *self, direction);
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
        Transition::paint(
            position,
            self.in_phase(EnemyPhase::Turning {
                turn,
                frame: Frame::new(turn.preceding_frame(candidate)).unwrap(),
            }),
        )
    }
}
