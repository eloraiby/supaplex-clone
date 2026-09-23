//! OrangeDisk identity and scheduled behavior.

use super::{
    Actor, CellWrite, Direction, Frame, GameEvent, Position, State, Transition, explode_at,
};
use crate::game::WorldView;

/// Legal phases of an Orange Disk's source-retained fall or triggered fuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrangePhase {
    /// Stationary and available for a horizontal push.
    Resting,
    /// Two original updates before the first visible falling picture.
    AwaitingFall(Frame<2>),
    /// Eight-picture fall whose logical owner remains in the source cell.
    Falling(Frame<8>),
    /// Six-update delay after being struck by a falling Zonk.
    Fuse(Frame<6>),
    /// Reserved by Murphy until push completion or cancellation.
    Held,
}

/// Falling explosive disk with only its own legal phases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrangeDisk {
    /// Sole authoritative phase, including any bounded progress counter.
    phase: OrangePhase,
}

impl OrangeDisk {
    /// Constructs a disk using only Orange Disk phases.
    pub const fn from_phase(phase: OrangePhase) -> Self {
        Self { phase }
    }

    /// Creates a stationary disk loaded from a level.
    pub const fn resting() -> Self {
        Self {
            phase: OrangePhase::Resting,
        }
    }

    /// Reports whether a downward transfer has been armed or started.
    pub const fn is_falling(self) -> bool {
        matches!(
            self.phase,
            OrangePhase::AwaitingFall(_) | OrangePhase::Falling(_)
        )
    }

    /// Exposes the typed phase for collision and diagnostic inspection.
    pub const fn phase(self) -> OrangePhase {
        self.phase
    }

    /// Constructs a complete Orange Disk cell from an Orange-only phase.
    pub(super) fn in_phase(mut self, phase: OrangePhase) -> State {
        self.phase = phase;
        State::new(Actor::OrangeDisk(self))
    }

    /// Advances the disk's exhaustive state machine once in row-major order.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        match self.phase {
            OrangePhase::Resting => {
                let below = world.offset(position, Direction::Down)?;
                world.is_empty(below).then(|| {
                    Transition::new(
                        vec![
                            CellWrite::new(
                                position,
                                self.in_phase(OrangePhase::AwaitingFall(Frame::first())),
                            ),
                            CellWrite::new(below, State::rounded_destination()),
                        ],
                        Vec::new(),
                    )
                })
            }
            OrangePhase::Held => None,
            OrangePhase::AwaitingFall(frame) => match frame.next() {
                Some(frame) => Some(Transition::replace(
                    position,
                    self.in_phase(OrangePhase::AwaitingFall(frame)),
                )),
                None => self.begin_fall(position, world),
            },
            OrangePhase::Falling(frame) => match frame.next() {
                Some(frame) => Some(Transition::paint(
                    position,
                    self.in_phase(OrangePhase::Falling(frame)),
                )),
                None => self.finish_fall(position, world),
            },
            OrangePhase::Fuse(frame) => Some(match frame.next() {
                Some(frame) => {
                    Transition::replace(position, self.in_phase(OrangePhase::Fuse(frame)))
                }
                None => explode_at(world, position, false),
            }),
        }
    }

    /// Starts the visible fall only while the downward reservation survives.
    fn begin_fall(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let Some(destination) = world.offset(position, Direction::Down) else {
            return Some(Transition::replace(
                position,
                self.in_phase(OrangePhase::Resting),
            ));
        };
        if !world.state(destination).is_some_and(|state| {
            state.reservation() == Some(super::empty::Reservation::RoundedDestination)
        }) {
            return Some(Transition::replace(
                position,
                State::new(Actor::OrangeDisk(OrangeDisk::resting())),
            ));
        }
        Some(Transition::paint(
            position,
            self.in_phase(OrangePhase::Falling(Frame::first())),
        ))
    }

    /// Transfers the disk downward, continues its fall, or detonates on landing.
    fn finish_fall(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let Some(destination) = world.offset(position, Direction::Down) else {
            return Some(Transition::replace(
                position,
                State::new(Actor::OrangeDisk(OrangeDisk::resting())),
            ));
        };
        let destination_reserved = world.state(destination).is_some_and(|state| {
            state.reservation() == Some(super::empty::Reservation::RoundedDestination)
        });
        if !destination_reserved {
            return Some(explode_at(world, position, false));
        }

        let landing_cell = world.offset(destination, Direction::Down);
        if landing_cell.is_some_and(|cell| world.is_empty(cell)) {
            let landing_cell = landing_cell.expect("validated landing cell exists");
            return Some(
                Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(
                            destination,
                            self.in_phase(OrangePhase::Falling(Frame::first())),
                        ),
                        CellWrite::new(landing_cell, State::rounded_destination()),
                    ],
                    Vec::new(),
                )
                .with_drawing(
                    destination,
                    self.in_phase(OrangePhase::Falling(Frame::first()))
                        .actor()
                        .clone(),
                ),
            );
        }

        if landing_cell
            .and_then(|cell| world.state(cell))
            .is_some_and(|state| matches!(state.actor(), Actor::Explosion(_)))
        {
            return Some(
                Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(
                            destination,
                            State::new(Actor::OrangeDisk(OrangeDisk::resting())),
                        ),
                    ],
                    Vec::new(),
                )
                .with_drawing(destination, Actor::OrangeDisk(OrangeDisk::resting())),
            );
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
        Some(explosion)
    }
}
