//! Shared physical phases and reservation handling for Zonks and Infotrons.
//!
//! Only these two concrete actors enter this helper. Their landing rules remain
//! in their own modules; the shared enum admits no Murphy, enemy, or fuse state.

use super::{CellWrite, Direction, Frame, Horizontal, Infotron, Position, State, Transition, Zonk};
use crate::game::WorldView;

/// Legal physical phases shared by rounded falling objects.
///
/// A lateral roll cannot carry a vertical direction.
/// ```compile_fail
/// use supaplex_clone::actors::{Direction, Frame, rounded::RoundedPhase};
/// let roll = RoundedPhase::Rolling { direction: Direction::Up, frame: Frame::first() };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoundedPhase {
    /// Stable object without downward momentum.
    Resting,
    /// Completed fall with its next cell reserved; transfer resumes next callback.
    Momentum,
    /// One-callback delay before the first downward transfer.
    AwaitingFall,
    /// First two roll pictures, while the actor still owns its original cell.
    PreparingRoll {
        /// Only left or right can reserve a diagonal fall.
        direction: Horizontal,
        /// Next preparation picture; the second repeats while the diagonal is blocked.
        frame: Frame<2>,
    },
    /// Horizontal slide toward a reserved diagonal destination.
    Rolling {
        /// Only left or right is representable.
        direction: Horizontal,
        /// Next picture to draw, after the two preparation pictures.
        frame: Frame<8>,
    },
    /// Next falling picture; the sixth copy releases the source in the same callback.
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
            RoundedPhase::PreparingRoll { direction, frame } => {
                self.prepare_slide(position, direction, frame, world)
            }
            RoundedPhase::Rolling { direction, frame } => {
                let mut transition = match frame.next() {
                    Some(next) => Transition::replace(
                        position,
                        self.in_phase(RoundedPhase::Rolling {
                            direction,
                            frame: next,
                        }),
                    ),
                    None => self.begin_drop(position, world)?,
                };
                if let Some(source) = world.offset(position, direction.direction().opposite()) {
                    match frame.index() {
                        // The fourth picture changes FF to AA. The sixth frees
                        // the source, allowing Murphy to enter on his next update.
                        3 if world.state(source).and_then(State::reservation)
                            == Some(super::empty::Reservation::RollingSource(direction)) =>
                        {
                            transition.writes.push(CellWrite::new(
                                source,
                                State::reserved(super::empty::Reservation::RoundedCorner(
                                    direction,
                                )),
                            ));
                        }
                        5 if world
                            .state(source)
                            .and_then(State::reservation)
                            .is_some_and(|r| {
                                matches!(r, super::empty::Reservation::RollingSource(d)
                                | super::empty::Reservation::RoundedCorner(d) if d == direction)
                            }) =>
                        {
                            transition
                                .writes
                                .push(CellWrite::new(source, State::empty()));
                        }
                        _ => {}
                    }
                }
                Some(
                    transition.after_drawing(
                        position,
                        self.in_phase(RoundedPhase::Rolling { direction, frame })
                            .actor()
                            .clone(),
                    ),
                )
            }
            RoundedPhase::Falling(frame) => {
                let mut transition = match frame.next() {
                    Some(next) => {
                        Transition::replace(position, self.in_phase(RoundedPhase::Falling(next)))
                    }
                    None => match self {
                        Self::Zonk(a) => a.land(position, world),
                        Self::Infotron(a) => a.land(position, world),
                    },
                };
                if frame.index() == 5
                    && let Some(source) = world.offset(position, Direction::Up)
                    && matches!(
                        world.state(source).and_then(State::reservation),
                        Some(super::empty::Reservation::Vacating {
                            direction: Direction::Down,
                            ..
                        })
                    )
                {
                    transition
                        .writes
                        .push(CellWrite::new(source, State::empty()));
                }
                // Drawing precedes state advancement, source release, and any
                // landing blast. The last frame never requires an extra update.
                Some(transition.after_drawing(
                    position,
                    self.in_phase(RoundedPhase::Falling(frame)).actor().clone(),
                ))
            }
        }
    }

    /// Paints the first two roll pictures before transferring logical ownership.
    fn prepare_slide(
        self,
        position: Position,
        direction: Horizontal,
        frame: Frame<2>,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let picture = self
            .in_phase(RoundedPhase::PreparingRoll { direction, frame })
            .actor()
            .clone();
        let waiting = self.in_phase(RoundedPhase::PreparingRoll {
            direction,
            frame: Frame::last(),
        });
        let side = world.offset(position, direction.direction())?;
        let diagonal = world.offset(side, Direction::Down)?;
        let side_available = world.is_empty(side)
            || world.state(side).and_then(State::reservation)
                == Some(super::empty::Reservation::RoundedSide);
        let transition = match (frame.next(), side_available, world.is_empty(diagonal)) {
            // Preparation picture one repeats while either required cell is blocked.
            (Some(_), _, _) | (None, false, _) | (None, _, false) => {
                Transition::replace(position, waiting)
            }
            (None, true, true) => Transition::new(
                vec![
                    CellWrite::new(
                        position,
                        State::reserved(super::empty::Reservation::RollingSource(direction)),
                    ),
                    CellWrite::new(
                        side,
                        self.in_phase(RoundedPhase::Rolling {
                            direction,
                            frame: Frame::new(2)
                                .expect("two preparation pictures precede the slide"),
                        }),
                    ),
                    CellWrite::new(diagonal, State::rounded_destination()),
                ],
                Vec::new(),
            ),
        };
        Some(transition.after_drawing(position, picture))
    }

    /// Ends the eighth roll picture by reserving the source of the pending fall.
    fn begin_drop(self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let destination = world.offset(position, Direction::Down)?;
        if world.state(destination).and_then(State::reservation)
            != Some(super::empty::Reservation::RoundedDestination)
        {
            return Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::Resting),
            ));
        }
        // Transfer only: falling picture zero belongs to the next scheduled
        // callback, after Murphy has had his turn in that tick.
        Some(Transition::move_actor(position, destination, self))
    }

    /// Selects a left-first candidate using the original diagonal marker rules.
    fn roll_candidate(
        self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<(Horizontal, Position)> {
        let below = world.offset(position, Direction::Down)?;
        if !world.is_rounded_stable_support(below) {
            return None;
        }
        for direction in [Horizontal::Left, Horizontal::Right] {
            let side = world.offset(position, direction.direction())?;
            let diagonal = world.offset(side, Direction::Down)?;
            let diagonal_available = world.is_empty(diagonal)
                || matches!(
                    world.state(diagonal).and_then(State::reservation),
                    Some(
                        super::empty::Reservation::RoundedSide
                            | super::empty::Reservation::RoundedCorner(_)
                    )
                );
            if world.is_empty(side) && diagonal_available {
                return Some((direction, side));
            }
        }
        None
    }

    /// Reserves a side cell, leaving picture zero pending after a landing callback.
    pub(super) fn reserve_roll(
        self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let (direction, side) = self.roll_candidate(position, world)?;
        Some(self.reserve_roll_at(position, side, direction, Frame::first()))
    }

    /// Begins a roll from rest and consumes picture zero on the initiating callback.
    pub(super) fn start_roll(
        self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let (direction, side) = self.roll_candidate(position, world)?;
        let picture = self.in_phase(RoundedPhase::PreparingRoll {
            direction,
            frame: Frame::first(),
        });
        Some(
            self.reserve_roll_at(position, side, direction, Frame::last())
                .with_drawing(position, picture.actor().clone()),
        )
    }

    /// Records source-owned preparation and its side marker as one atomic change.
    fn reserve_roll_at(
        self,
        position: Position,
        side: Position,
        direction: Horizontal,
        frame: Frame<2>,
    ) -> Transition {
        Transition::new(
            vec![
                CellWrite::new(
                    position,
                    self.in_phase(RoundedPhase::PreparingRoll { direction, frame }),
                ),
                CellWrite::new(side, State::rounded_side()),
            ],
            Vec::new(),
        )
    }

    /// Reserves the next cell after a final fall picture, before the next callback.
    pub(super) fn continue_fall(self, position: Position, below: Position) -> Transition {
        Transition::new(
            vec![
                CellWrite::new(position, self.in_phase(RoundedPhase::Momentum)),
                CellWrite::new(
                    below,
                    State::reserved(super::empty::Reservation::RoundedContinuation),
                ),
            ],
            Vec::new(),
        )
    }

    /// Transfers a continuation and paints its first falling picture immediately.
    pub(super) fn resume_fall(self, position: Position, below: Position) -> Transition {
        Transition::new(
            vec![
                CellWrite::new(position, State::vacating(Direction::Down)),
                CellWrite::new(
                    below,
                    self.in_phase(RoundedPhase::Falling(
                        Frame::new(1).expect("fall has eight pictures"),
                    )),
                ),
            ],
            Vec::new(),
        )
        .with_drawing(
            below,
            self.in_phase(RoundedPhase::Falling(Frame::first()))
                .actor()
                .clone(),
        )
    }
}

impl RoundedPhase {
    /// Reports momentum without storing a second, potentially conflicting flag.
    pub(super) const fn is_falling(self) -> bool {
        matches!(
            self,
            Self::Momentum | Self::Rolling { .. } | Self::Falling(_)
        )
    }
}
