//! Shared physical phases and reservation handling for Zonks and Infotrons.
//!
//! Only these two concrete actors enter this helper. Their landing rules remain
//! in their own modules; the shared enum admits no Murphy, enemy, or fuse state.

use super::{
    Actor, Animation, AnimationKind, CellWrite, Direction, Frame, Horizontal, Infotron, Position,
    State, Transition, Zonk,
};
use crate::game::WorldView;

/// Legal physical phases shared by rounded falling objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoundedPhase {
    /// Stable object without downward momentum.
    Resting,
    /// Original idle picture with retained momentum, resumed on the next callback.
    Momentum,
    /// One-callback delay before the first downward transfer.
    AwaitingFall,
    /// Side reservation installed; the diagonal must remain available.
    PreparingRoll(Horizontal),
    /// Horizontal slide toward a reserved diagonal destination.
    Rolling {
        /// Only left or right is representable.
        direction: Horizontal,
        /// Eight-picture slide progress.
        frame: Frame<8>,
    },
    /// Downward transfer with a source reservation released at picture six.
    Falling(Frame<8>),
    /// Temporarily retained while Murphy pushes or collects this object.
    Held,
}

/// A restricted dispatcher for the two actors that share rounded physics.
#[derive(Clone, Copy)]
pub(super) enum RoundedActor {
    /// A rock, whose landing can arm an Orange Disk.
    Zonk(Zonk),
    /// A collectible, whose landing can detonate any loose disk.
    Infotron(Infotron),
}

impl RoundedActor {
    /// Returns the actor's sole authoritative physical phase.
    fn phase(self) -> RoundedPhase {
        match self {
            Self::Zonk(a) => a.phase(),
            Self::Infotron(a) => a.phase(),
        }
    }

    /// Constructs a complete cell without exposing arbitrary actor/phase pairing.
    pub(super) fn in_phase(self, phase: RoundedPhase) -> State {
        match self {
            Self::Zonk(a) => a.in_phase(phase),
            Self::Infotron(a) => a.in_phase(phase),
        }
    }

    /// Dispatches a resting or momentum-bearing decision to its concrete actor.
    fn decide(self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        match self {
            Self::Zonk(a) => a.transition(position, world),
            Self::Infotron(a) => a.transition(position, world),
        }
    }

    /// Advances exactly one legal rounded phase in the original update order.
    pub(super) fn update(self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        // Freeze pauses arming and roll preparation, but an in-flight rock must
        // finish its reserved transfer before losing momentum at landing.
        if matches!(self, Self::Zonk(_))
            && world.freeze_zonks()
            && !matches!(
                self.phase(),
                RoundedPhase::Falling(_) | RoundedPhase::Rolling { .. }
            )
        {
            return None;
        }
        match self.phase() {
            RoundedPhase::Resting | RoundedPhase::Momentum => self.decide(position, world),
            RoundedPhase::Held => None,
            RoundedPhase::AwaitingFall => match self {
                Self::Zonk(a) => a.begin_fall(position, world),
                Self::Infotron(a) => a.begin_fall(position, world),
            },
            RoundedPhase::PreparingRoll(direction) => self.begin_slide(position, direction, world),
            RoundedPhase::Rolling { direction, frame } => match frame.next() {
                Some(frame) => Some(Transition::replace(
                    position,
                    self.in_phase(RoundedPhase::Rolling { direction, frame }),
                )),
                None => self.begin_drop(position, world),
            },
            RoundedPhase::Falling(frame) => match frame.next() {
                Some(next) => {
                    let mut writes = vec![CellWrite::new(
                        position,
                        self.in_phase(RoundedPhase::Falling(next)),
                    )];
                    // State 0x16 opens the source before the final two pictures.
                    if frame.index() == 5
                        && let Some(source) = world.offset(position, Direction::Up)
                        && world.state(source).is_some_and(|state| {
                            matches!(
                                state.actor(),
                                Actor::Empty(super::Empty::Reserved(
                                    super::empty::Reservation::Vacating {
                                        direction: Direction::Down,
                                        ..
                                    }
                                ))
                            )
                        })
                    {
                        writes.push(CellWrite::new(source, State::empty()));
                    }
                    Some(Transition::new(writes, Vec::new()))
                }
                None => Some(match self {
                    Self::Zonk(a) => a.land(position, world),
                    Self::Infotron(a) => a.land(position, world),
                }),
            },
        }
    }

    /// Begins a lateral transfer only while both destination reservations survive.
    fn begin_slide(
        self,
        position: Position,
        direction: Horizontal,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let side = world.offset(position, direction.direction());
        let diagonal = side.and_then(|cell| world.offset(cell, Direction::Down));
        let reserved = side
            .and_then(|cell| world.state(cell))
            .is_some_and(|state| {
                matches!(
                    state.actor(),
                    Actor::Empty(super::Empty::Reserved(
                        super::empty::Reservation::RoundedSide
                    ))
                )
            });
        match (reserved, side, diagonal) {
            (true, Some(side), Some(diagonal)) if world.is_empty(diagonal) => {
                Some(Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(
                            side,
                            self.in_phase(RoundedPhase::Rolling {
                                direction,
                                frame: Frame::first(),
                            }),
                        ),
                        CellWrite::new(diagonal, State::rounded_destination()),
                    ],
                    Vec::new(),
                ))
            }
            // A blocked diagonal leaves original state 0x51 sticky.
            (true, _, _) => None,
            // A blast can consume the side marker; do not overwrite its replacement.
            (false, _, _) => Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::Resting),
            )),
        }
    }

    /// Transfers the completed slide downward only into its own surviving marker.
    fn begin_drop(self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        if let Some(destination) = world.offset(position, Direction::Down)
            && world.state(destination).is_some_and(|state| {
                matches!(
                    state.actor(),
                    Actor::Empty(super::Empty::Reserved(
                        super::empty::Reservation::RoundedDestination
                    ))
                )
            })
        {
            return Some(Transition::new(
                vec![
                    CellWrite::new(position, State::empty()),
                    CellWrite::new(
                        destination,
                        self.in_phase(RoundedPhase::Falling(Frame::first())),
                    ),
                ],
                Vec::new(),
            ));
        }
        Some(Transition::replace(
            position,
            self.in_phase(RoundedPhase::Momentum),
        ))
    }
}

impl RoundedPhase {
    /// Derives a sprite description; arming artwork differs between the two actors.
    pub(super) fn animation(self, pre_fall: AnimationKind) -> Animation {
        match self {
            Self::Resting | Self::Momentum => Animation::idle(),
            Self::AwaitingFall => Animation::view(pre_fall, 0, 1),
            Self::PreparingRoll(direction) => {
                Animation::view(AnimationKind::RoundedPreRoll(direction.direction()), 0, 1)
            }
            Self::Rolling { direction, frame } => Animation::view(
                AnimationKind::Rolling(direction.direction()),
                frame.index(),
                8,
            ),
            Self::Falling(frame) => {
                Animation::view(AnimationKind::Moving(Direction::Down), frame.index(), 8)
            }
            Self::Held => Animation::view(AnimationKind::MurphyPushTarget, 0, 1),
        }
    }

    /// Reports momentum without storing a second, potentially conflicting flag.
    pub(super) const fn is_falling(self) -> bool {
        matches!(
            self,
            Self::Momentum | Self::Rolling { .. } | Self::Falling(_)
        )
    }
}
