//! Player input, hold-sensitive actions, and Murphy animation descriptors.

use super::{
    Actor, CellWrite, Direction, Frame, GameEvent, Horizontal, OrangeDisk, Position, State,
    Transition, YellowDisk, Zonk, explode_at,
};
use crate::game::SoundEffect;
use crate::game::WorldView;

/// Material Murphy crosses or consumes during one cell-to-cell animation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MurphyMoveTarget {
    /// Ordinary empty space, including a cell reached by gravity.
    Empty,
    /// Diggable Base or a safe Bug that has already reverted to Base.
    Base,
    /// A required Infotron collected when the movement finishes.
    Infotron,
    /// A loose Red Disk collected when the movement finishes.
    RedDisk,
    /// The already planted fuse crossed without adding it to inventory.
    PlantedRedDisk,
}

/// Material Murphy removes without leaving his current cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MurphySnapTarget {
    /// Diggable Base or a safe Bug represented by the Base-eating sequence.
    Base,
    /// A required Infotron represented by the seven-frame collection sequence.
    Infotron,
    /// A loose Red Disk represented by the original Red Disk sequence.
    RedDisk,
}

/// Pushable actor represented by one of Murphy's dedicated push sequences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MurphyPushTarget {
    /// A Zonk, which can only be pushed horizontally.
    Zonk,
    /// A Yellow Disk, which can be pushed in all four directions.
    YellowDisk,
    /// An Orange Disk, which can only be pushed horizontally.
    OrangeDisk,
}

/// One original Murphy animation descriptor selected by action context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MurphyAnimation {
    /// Movement into an adjacent cell, retaining both material and side-facing.
    Move {
        /// Direction in which Murphy enters the destination cell.
        direction: Direction,
        /// Material that determines the appropriate eating/collection artwork.
        target: MurphyMoveTarget,
        /// Horizontal pose retained while moving vertically.
        looking_left: bool,
    },
    /// Stationary removal or collection of one adjacent cell.
    Snap {
        /// Direction from Murphy to the affected cell.
        direction: Direction,
        /// Material that determines the dedicated snapping strip.
        target: MurphySnapTarget,
    },
    /// Movement that transfers one pushable actor into the following cell.
    Push {
        /// Direction in which Murphy and the pushed actor travel.
        direction: Direction,
        /// Actor that determines the Zonk, Yellow, or Orange artwork.
        target: MurphyPushTarget,
    },
    /// Two-cell traversal through an intervening port.
    Port {
        /// Direction accepted by the port and used for the traversal pair.
        direction: Direction,
    },
    /// Forty-frame disappearance played after entering an unlocked Exit.
    Exit,
    /// Sixty-four-tick hold-to-place Red Disk sequence.
    PlantRedDisk,
}

impl MurphyAnimation {
    /// Returns the exact number of original updates used by this action.
    pub const fn frame_count(self) -> u8 {
        match self {
            // The original rightward Red Disk table deliberately contains a
            // duplicated ninth coordinate. Retaining it is demo-compatible.
            Self::Move {
                direction: Direction::Right,
                target: MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk,
                ..
            } => 9,
            Self::Move { .. } | Self::Push { .. } | Self::Port { .. } => 8,
            Self::Snap {
                target: MurphySnapTarget::Infotron,
                ..
            } => 7,
            Self::Snap { .. } => 8,
            Self::Exit => 40,
            // Planting is advanced by Murphy's hold-sensitive state machine,
            // its own phase match rather than a free-running strip.
            Self::PlantRedDisk => 65,
        }
    }
}

/// Push actions encode each object's permitted movement directions.
///
/// A rock cannot acquire a vertical push action.
/// ```compile_fail
/// use supaplex_clone::actors::{Direction, murphy::PushAction};
/// let push = PushAction::Zonk(Direction::Down);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushAction {
    /// Rocks permit lateral pushes only.
    Zonk(Horizontal),
    /// Orange Disks permit lateral pushes only.
    OrangeDisk(Horizontal),
    /// Yellow Disks permit all four cardinal directions.
    YellowDisk(Direction),
}

impl PushAction {
    /// Returns the board direction accepted by this typed push.
    pub const fn direction(self) -> Direction {
        match self {
            Self::Zonk(side) | Self::OrangeDisk(side) => side.direction(),
            Self::YellowDisk(direction) => direction,
        }
    }

    /// Returns the renderer's push-target discriminator.
    pub const fn target(self) -> MurphyPushTarget {
        match self {
            Self::Zonk(_) => MurphyPushTarget::Zonk,
            Self::OrangeDisk(_) => MurphyPushTarget::OrangeDisk,
            Self::YellowDisk(_) => MurphyPushTarget::YellowDisk,
        }
    }

    /// Restricts a directional input before it can enter a stored push phase.
    fn from_input(direction: Direction, target: MurphyPushTarget) -> Option<Self> {
        match target {
            MurphyPushTarget::YellowDisk => Some(Self::YellowDisk(direction)),
            MurphyPushTarget::Zonk => Horizontal::from_direction(direction).map(Self::Zonk),
            MurphyPushTarget::OrangeDisk => {
                Horizontal::from_direction(direction).map(Self::OrangeDisk)
            }
        }
    }
}

/// Materials whose ordinary movement strips all contain eight pictures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepTarget {
    /// Empty destination, including a gravity-driven step.
    Empty,
    /// Diggable Base or a dormant Bug.
    Base,
    /// Collectible Infotron.
    Infotron,
}

/// Red Disk travel encodes the original rightward ninth picture in its type.
///
/// An eight-picture frame cannot be supplied to the nine-picture rightward strip.
/// ```compile_fail
/// use supaplex_clone::actors::{Frame, murphy::DiskTravel};
/// let travel = DiskTravel::Right(Frame::<8>::first());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskTravel {
    /// Eight-picture upward collection.
    Up(Frame<8>),
    /// Eight-picture downward collection.
    Down(Frame<8>),
    /// Eight-picture leftward collection.
    Left(Frame<8>),
    /// Nine-picture rightward collection, including the duplicated coordinate.
    Right(Frame<9>),
}

/// Legal ordinary movement strips, with timing selected by their payload type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Travel {
    /// Eight-picture movement through a non-disk material.
    Step {
        /// Cardinal direction of travel.
        direction: Direction,
        /// Materials sharing the eight-picture strip contract.
        target: StepTarget,
        /// Bounded progress within the strip.
        frame: Frame<8>,
    },
    /// Direction-specific Red Disk strip with its exact duration.
    Disk {
        /// Whether the disk is a session-owned fuse rather than inventory.
        planted: bool,
        /// Direction and progress, including the nine-picture rightward case.
        travel: DiskTravel,
    },
}

/// Movement metadata retained for exactly one update after source release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalPose {
    /// Direction of the movement that just completed.
    direction: Direction,
    /// Material needed to reconstruct the last picture and its strip length.
    target: MurphyMoveTarget,
}

/// Adjacent collection strips encode their differing lengths directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Snap {
    /// Eight-picture Base removal.
    Base(Direction, Frame<8>),
    /// Seven-picture Infotron collection.
    Infotron(Direction, Frame<7>),
    /// Eight-picture Red Disk collection.
    RedDisk(Direction, Frame<8>),
}

/// Murphy's complete state: input delays and finite actions cannot overlap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MurphyPhase {
    /// Accepts a fresh input action.
    Ready,
    /// Direction must remain held while target and destination are reserved.
    PreparingPush {
        /// Push whose direction is legal for its target.
        action: PushAction,
        /// Original countdown after the initiating update, from seven to zero.
        remaining: Frame<8>,
    },
    /// Space-only placement that can still be cancelled by releasing input.
    PlantingRedDisk {
        /// Remaining hold updates, from 63 to zero; artwork is derived from it.
        remaining: Frame<64>,
    },
    /// Ordinary cell-to-cell movement with material-specific timing.
    Moving(Travel),
    /// Final moving pose held after collection and source cleanup.
    Resuming(FinalPose),
    /// Adjacent removal while Murphy remains in place.
    Snapping(Snap),
    /// Push transfer after the hold requirement succeeds.
    Pushing {
        /// Direction and target constrained as one legal action.
        action: PushAction,
        /// Bounded eight-picture transfer progress.
        frame: Frame<8>,
    },
    /// Two-cell traversal with a source-retained logical actor.
    CrossingPort {
        /// Port traversal direction accepted at action start.
        direction: Direction,
        /// Progress through the eight-picture composite strip.
        frame: Frame<8>,
    },
    /// Forty-picture disappearance after completion is recorded.
    Exiting(Frame<40>),
}

/// Advances an action that consumes its first picture on the initiating update.
fn next_action_frame<const N: u8>(frame: Frame<N>) -> Option<Frame<N>> {
    // The callback drawing the last picture also resolves gameplay. Returning
    // None at the penultimate stored picture preserves that original boundary.
    let next = frame.next()?;
    next.next().map(|_| next)
}

impl Travel {
    /// Selects the only strip type permitted by the direction and material.
    fn new(direction: Direction, target: MurphyMoveTarget) -> Self {
        match target {
            MurphyMoveTarget::Empty => Self::Step {
                direction,
                target: StepTarget::Empty,
                frame: Frame::first(),
            },
            MurphyMoveTarget::Base => Self::Step {
                direction,
                target: StepTarget::Base,
                frame: Frame::first(),
            },
            MurphyMoveTarget::Infotron => Self::Step {
                direction,
                target: StepTarget::Infotron,
                frame: Frame::first(),
            },
            MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk => Self::Disk {
                planted: target == MurphyMoveTarget::PlantedRedDisk,
                travel: match direction {
                    Direction::Up => DiskTravel::Up(Frame::first()),
                    Direction::Down => DiskTravel::Down(Frame::first()),
                    Direction::Left => DiskTravel::Left(Frame::first()),
                    Direction::Right => DiskTravel::Right(Frame::first()),
                },
            },
        }
    }

    /// Derives render metadata and bounded progress from the stored strip variant.
    fn pose(self) -> (FinalPose, u8) {
        match self {
            Self::Step {
                direction,
                target,
                frame,
            } => (
                FinalPose {
                    direction,
                    target: match target {
                        StepTarget::Empty => MurphyMoveTarget::Empty,
                        StepTarget::Base => MurphyMoveTarget::Base,
                        StepTarget::Infotron => MurphyMoveTarget::Infotron,
                    },
                },
                frame.index(),
            ),
            Self::Disk { planted, travel } => {
                let (direction, frame) = match travel {
                    DiskTravel::Up(frame) => (Direction::Up, frame.index()),
                    DiskTravel::Down(frame) => (Direction::Down, frame.index()),
                    DiskTravel::Left(frame) => (Direction::Left, frame.index()),
                    DiskTravel::Right(frame) => (Direction::Right, frame.index()),
                };
                (
                    FinalPose {
                        direction,
                        target: if planted {
                            MurphyMoveTarget::PlantedRedDisk
                        } else {
                            MurphyMoveTarget::RedDisk
                        },
                    },
                    frame,
                )
            }
        }
    }

    /// Advances only within this strip; completion cannot produce an invalid frame.
    fn next(self) -> Option<Self> {
        Some(match self {
            Self::Step {
                direction,
                target,
                frame,
            } => Self::Step {
                direction,
                target,
                frame: next_action_frame(frame)?,
            },
            Self::Disk { planted, travel } => Self::Disk {
                planted,
                travel: match travel {
                    DiskTravel::Up(frame) => DiskTravel::Up(next_action_frame(frame)?),
                    DiskTravel::Down(frame) => DiskTravel::Down(next_action_frame(frame)?),
                    DiskTravel::Left(frame) => DiskTravel::Left(next_action_frame(frame)?),
                    DiskTravel::Right(frame) => DiskTravel::Right(next_action_frame(frame)?),
                },
            },
        })
    }
}

impl Snap {
    /// Selects a target-specific strip without accepting an arbitrary duration.
    fn new(direction: Direction, target: MurphySnapTarget) -> Self {
        match target {
            MurphySnapTarget::Base => Self::Base(direction, Frame::first()),
            MurphySnapTarget::Infotron => Self::Infotron(direction, Frame::first()),
            MurphySnapTarget::RedDisk => Self::RedDisk(direction, Frame::first()),
        }
    }

    /// Derives the action descriptor and current picture from the typed variant.
    fn pose(self) -> (Direction, MurphySnapTarget, u8) {
        match self {
            Self::Base(d, f) => (d, MurphySnapTarget::Base, f.index()),
            Self::Infotron(d, f) => (d, MurphySnapTarget::Infotron, f.index()),
            Self::RedDisk(d, f) => (d, MurphySnapTarget::RedDisk, f.index()),
        }
    }

    /// Selects the bounded terminal picture emitted on the collection callback.
    fn last_picture(self) -> Self {
        // The picture outlives this action only in the drawing stream. It is
        // never installed as a new active snap, so collection is not delayed.
        match self {
            Self::Base(direction, _) => Self::Base(direction, Frame::last()),
            Self::Infotron(direction, _) => Self::Infotron(direction, Frame::last()),
            Self::RedDisk(direction, _) => Self::RedDisk(direction, Frame::last()),
        }
    }

    /// Advances the target's own strip using Murphy's immediate-first-picture rule.
    fn next(self) -> Option<Self> {
        Some(match self {
            Self::Base(d, f) => Self::Base(d, next_action_frame(f)?),
            Self::Infotron(d, f) => Self::Infotron(d, next_action_frame(f)?),
            Self::RedDisk(d, f) => Self::RedDisk(d, next_action_frame(f)?),
        })
    }
}

/// The player-controlled actor and all state retained between input updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Murphy {
    /// Original left/right look flag retained across vertical movement.
    looking_left: bool,
    /// Whether the last idle Murphy update observed no direction or Space key.
    previous_input_was_none: bool,
    /// Current hold-sensitive push or Red Disk preparation, if any.
    phase: MurphyPhase,
}

impl Murphy {
    /// Returns Murphy's complete phase for typed collision queries.
    pub const fn phase(self) -> MurphyPhase {
        self.phase
    }

    /// Constructs a complete player cell from a player-only phase.
    pub(super) fn in_phase(mut self, phase: MurphyPhase) -> State {
        self.phase = phase;
        State::new(Actor::Murphy(self))
    }

    /// Returns the same persistent input/facing data ready for a fresh action.
    fn ready(mut self) -> Self {
        self.phase = MurphyPhase::Ready;
        self
    }

    /// Starts a correctly timed movement selected from direction and material.
    pub(super) fn moving(
        self,
        direction: Direction,
        target: MurphyMoveTarget,
    ) -> (State, super::empty::SourceDuration) {
        let travel = Travel::new(direction, target);
        let duration = match travel {
            Travel::Disk {
                travel: DiskTravel::Right(_),
                ..
            } => super::empty::SourceDuration::Nine,
            Travel::Step { .. } | Travel::Disk { .. } => super::empty::SourceDuration::Eight,
        };
        (self.in_phase(MurphyPhase::Moving(travel)), duration)
    }

    /// Derives player artwork exclusively from the current legal phase.
    pub fn sprite_pose(self) -> Option<(MurphyAnimation, u8)> {
        let (action, frame) = match self.phase {
            MurphyPhase::Ready | MurphyPhase::PreparingPush { .. } => return None,
            MurphyPhase::PlantingRedDisk { remaining } => {
                (MurphyAnimation::PlantRedDisk, 64 - remaining.index())
            }
            MurphyPhase::Moving(travel) => {
                let (pose, frame) = travel.pose();
                (
                    MurphyAnimation::Move {
                        direction: pose.direction,
                        target: pose.target,
                        looking_left: self.looking_left,
                    },
                    frame,
                )
            }
            MurphyPhase::Resuming(pose) => {
                let action = MurphyAnimation::Move {
                    direction: pose.direction,
                    target: pose.target,
                    looking_left: self.looking_left,
                };
                (action, action.frame_count() - 1)
            }
            MurphyPhase::Snapping(snap) => {
                let (direction, target, frame) = snap.pose();
                (MurphyAnimation::Snap { direction, target }, frame)
            }
            MurphyPhase::Pushing { action, frame } => (
                MurphyAnimation::Push {
                    direction: action.direction(),
                    target: action.target(),
                },
                frame.index(),
            ),
            MurphyPhase::CrossingPort { direction, frame } => {
                (MurphyAnimation::Port { direction }, frame.index())
            }
            MurphyPhase::Exiting(frame) => (MurphyAnimation::Exit, frame.index()),
        };
        Some((action, frame))
    }

    /// Advances one player phase; input-sensitive delays and artwork share one state.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        match self.phase {
            MurphyPhase::Ready => self.ready_transition(position, world),
            MurphyPhase::PreparingPush { action, remaining } => {
                Some(self.continue_push(position, action, remaining, world.input(), world))
            }
            MurphyPhase::PlantingRedDisk { remaining } => {
                Some(self.continue_plant(position, remaining, world.input()))
            }
            MurphyPhase::Moving(travel) => Some(match travel.next() {
                Some(travel) => {
                    Transition::paint(position, self.in_phase(MurphyPhase::Moving(travel)))
                }
                None => self.finish_move(position, travel.pose().0, world),
            }),
            MurphyPhase::Resuming(_) => {
                let ready = self.ready();
                ready.ready_transition(position, world).or_else(|| {
                    Some(Transition::replace(
                        position,
                        State::new(Actor::Murphy(ready)),
                    ))
                })
            }
            MurphyPhase::Snapping(snap) => match snap.next() {
                Some(snap) => Some(Transition::paint(
                    position,
                    self.in_phase(MurphyPhase::Snapping(snap)),
                )),
                None => {
                    let (direction, target, _) = snap.pose();
                    self.finish_snap(position, direction, target, world)
                        .map(|transition| {
                            // Completion removes the held actor, but its terminal
                            // snap picture must be drawn before those cell writes.
                            let picture = self.in_phase(MurphyPhase::Snapping(snap.last_picture()));
                            transition.after_drawing(position, picture.actor().clone())
                        })
                }
            },
            MurphyPhase::Pushing { action, frame } => match next_action_frame(frame) {
                Some(frame) => Some(Transition::paint(
                    position,
                    self.in_phase(MurphyPhase::Pushing { action, frame }),
                )),
                None => self
                    .finish_push(position, action.direction(), action.target(), world)
                    .map(|transition| {
                        let picture = self.in_phase(MurphyPhase::Pushing {
                            action,
                            frame: Frame::last(),
                        });
                        transition.after_drawing(position, picture.actor().clone())
                    }),
            },
            MurphyPhase::CrossingPort { direction, frame } => match next_action_frame(frame) {
                Some(frame) => Some(Transition::paint(
                    position,
                    self.in_phase(MurphyPhase::CrossingPort { direction, frame }),
                )),
                None => self
                    .finish_port(position, direction, world)
                    .map(|transition| {
                        let picture = self.in_phase(MurphyPhase::CrossingPort {
                            direction,
                            frame: Frame::last(),
                        });
                        transition.after_drawing(position, picture.actor().clone())
                    }),
            },
            MurphyPhase::Exiting(frame) => Some(match next_action_frame(frame) {
                Some(frame) => {
                    Transition::paint(position, self.in_phase(MurphyPhase::Exiting(frame)))
                }
                // Completion was recorded at entry; only the sprite disappears here.
                None => Transition::replace(position, State::empty()).after_drawing(
                    position,
                    self.in_phase(MurphyPhase::Exiting(Frame::last()))
                        .actor()
                        .clone(),
                ),
            }),
        }
    }

    /// Creates Murphy facing right, matching the original starting pose.
    pub const fn new() -> Self {
        Self {
            looking_left: false,
            previous_input_was_none: false,
            phase: MurphyPhase::Ready,
        }
    }

    /// Returns Murphy's retained horizontal pose as a conventional direction.
    pub const fn facing(self) -> Direction {
        if self.looking_left {
            Direction::Left
        } else {
            Direction::Right
        }
    }

    /// Interprets player intent against complete neighboring cell states.
    fn ready_transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let input = world.input();

        let input_is_none = input.direction.is_none() && !input.action;
        let mut next_murphy = *self;
        next_murphy.previous_input_was_none = input_is_none;

        // Original Murphy gravity rewrites unsupported input to Down. The only
        // exceptions are plain Up/Left/Right commands that eat Base, plus an
        // upward-capable port directly above Murphy that acts as a handhold.
        if self.is_pulled_by_gravity(position, world) {
            let base_exception = !input.action
                && input.direction.is_some_and(|direction| {
                    direction != Direction::Down
                        && world
                            .offset(position, direction)
                            .and_then(|target| world.state(target))
                            .is_some_and(|state| {
                                state.is_idle() && matches!(state.actor(), Actor::Base(_))
                            })
                });
            if !base_exception {
                return next_murphy.move_or_interact(position, Direction::Down, world);
            }
        }

        // The original accepts planting only after a completely input-free
        // idle update. The first Space-only update installs countdown state one
        // and immediately consumes the first of the 64 hold ticks.
        if input.action
            && input.direction.is_none()
            && self.previous_input_was_none
            && world.red_disks() > 0
            && !world.has_active_red_disk()
        {
            next_murphy.previous_input_was_none = false;
            next_murphy.phase = MurphyPhase::PlantingRedDisk {
                remaining: Frame::last(),
            };
            let murphy = State::new(Actor::Murphy(next_murphy));
            return Some(
                Transition::new(
                    vec![CellWrite::new(position, murphy)],
                    vec![GameEvent::BeginPlantRedDisk(position)],
                )
                .with_drawing(position, Actor::Murphy(next_murphy)),
            );
        }

        if let Some(direction) = input.direction {
            next_murphy.previous_input_was_none = false;
            if input.action {
                return next_murphy.snap(position, direction, world);
            }
            return next_murphy.move_or_interact(position, direction, world);
        }

        // Remember the release even though no visible actor action occurred;
        // this latch is what permits a later Space-only placement command.
        (next_murphy != *self)
            .then(|| Transition::replace(position, State::new(Actor::Murphy(next_murphy))))
    }

    /// Advances or cancels the eight-update delay before a push animation.
    fn continue_push(
        &self,
        position: Position,
        action: PushAction,
        remaining: Frame<8>,
        input: crate::game::Input,
        world: &WorldView<'_>,
    ) -> Transition {
        let direction = action.direction();
        let target = action.target();
        let target_position = world.offset(position, direction);
        let destination = target_position.and_then(|cell| world.offset(cell, direction));
        let reservations_intact = target_position
            .and_then(|cell| world.state(cell))
            .is_some_and(|state| {
                state.actor().is_held() && pushed_actor_matches(state.actor(), target)
            })
            && destination
                .and_then(|cell| world.state(cell))
                .is_some_and(|state| {
                    state.reservation() == Some(super::empty::Reservation::MurphyDestination)
                });
        let still_holding = input.direction == Some(direction) && !input.action;

        if reservations_intact && still_holding {
            if remaining.index() == 0 {
                let mut moving = *self;
                moving.phase = MurphyPhase::Ready;
                return Transition::new(
                    vec![CellWrite::new(
                        position,
                        moving.in_phase(MurphyPhase::Pushing {
                            action,
                            frame: Frame::first(),
                        }),
                    )],
                    vec![GameEvent::PlaySound(SoundEffect::Push)],
                )
                .with_drawing(
                    position,
                    moving
                        .in_phase(MurphyPhase::Pushing {
                            action,
                            frame: Frame::first(),
                        })
                        .actor()
                        .clone(),
                );
            }

            let mut waiting = *self;
            waiting.phase = MurphyPhase::PreparingPush {
                action,
                remaining: Frame::new(remaining.index() - 1).unwrap(),
            };
            return Transition::replace(position, State::new(Actor::Murphy(waiting)));
        }

        // Releasing or changing direction restores both reservations, unless a
        // blast has already replaced either cell during the preparation delay.
        let mut writes = Vec::with_capacity(3);
        let mut cancelled = *self;
        cancelled.phase = MurphyPhase::Ready;
        cancelled.previous_input_was_none = input.direction.is_none() && !input.action;
        writes.push(CellWrite::new(
            position,
            State::new(Actor::Murphy(cancelled)),
        ));
        if let Some(target_position) = target_position.filter(|cell| {
            world.state(*cell).is_some_and(|state| {
                state.actor().is_held() && pushed_actor_matches(state.actor(), target)
            })
        }) {
            writes.push(CellWrite::new(
                target_position,
                State::new(actor_for_push_target(target)),
            ));
        }
        if let Some(destination) = destination.filter(|cell| {
            world.state(*cell).is_some_and(|state| {
                state.reservation() == Some(super::empty::Reservation::MurphyDestination)
            })
        }) {
            writes.push(CellWrite::new(destination, State::empty()));
        }
        Transition::new(writes, Vec::new()).with_drawing(position, Actor::Murphy(cancelled))
    }

    /// Advances, completes, or cancels the 64-update Space-only plant action.
    fn continue_plant(
        &self,
        position: Position,
        remaining: Frame<64>,
        input: crate::game::Input,
    ) -> Transition {
        let still_holding = input.action && input.direction.is_none();
        if !still_holding {
            let mut cancelled = *self;
            cancelled.phase = MurphyPhase::Ready;
            cancelled.previous_input_was_none = input.direction.is_none() && !input.action;
            return Transition::new(
                vec![CellWrite::new(
                    position,
                    State::new(Actor::Murphy(cancelled)),
                )],
                vec![GameEvent::CancelPlantRedDisk],
            )
            .with_drawing(position, Actor::Murphy(cancelled));
        }

        if remaining.index() == 0 {
            let mut completed = *self;
            completed.phase = MurphyPhase::Ready;
            completed.previous_input_was_none = false;
            return Transition::new(
                vec![CellWrite::new(
                    position,
                    State::new(Actor::Murphy(completed)),
                )],
                vec![GameEvent::FinishPlantRedDisk],
            )
            .with_drawing(position, Actor::Murphy(completed));
        }

        let mut planting = *self;
        planting.phase = MurphyPhase::PlantingRedDisk {
            remaining: Frame::new(remaining.index() - 1).unwrap(),
        };
        Transition::paint(position, State::new(Actor::Murphy(planting)))
    }

    /// Reports whether gravity must override Murphy's current player command.
    fn is_pulled_by_gravity(&self, position: Position, world: &WorldView<'_>) -> bool {
        if !world.gravity() {
            return false;
        }

        let Some(below) = world.offset(position, Direction::Down) else {
            return false;
        };
        if !world.is_empty(below) {
            return false;
        }

        // Only a port immediately above Murphy can suspend him over empty
        // space, and it must accept travel upward through that tile.
        let held_by_port = world
            .offset(position, Direction::Up)
            .and_then(|above| world.state(above))
            .is_some_and(
                |state| matches!(state.actor(), Actor::Port(port) if port.allows(Direction::Up)),
            );
        !held_by_port
    }

    /// Removes or collects one adjacent actor without moving Murphy.
    fn snap(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let target = world.offset(position, direction)?;
        let target_state = world.state(target)?;
        let (target_kind, held_state) = match target_state.actor() {
            Actor::Base(_) => (
                MurphySnapTarget::Base,
                State::new(Actor::Base(super::Base::Held)),
            ),
            Actor::Bug(_) if !world.is_bug_active(target) => (
                MurphySnapTarget::Base,
                State::new(Actor::Bug(super::Bug::Held)),
            ),
            Actor::Bug(_) => return Some(explode_at(world, position, false)),
            // Every original Space+direction branch requires an idle
            // Infotron. A moving or roll-reserved tile keeps its updater and
            // is allowed to run later in this same Murphy-first linear pass.
            Actor::Infotron(actor) if target_state.is_idle() => (
                MurphySnapTarget::Infotron,
                actor.in_phase(super::rounded::RoundedPhase::Held),
            ),
            Actor::RedDisk(_) if target_state.is_idle() => (
                MurphySnapTarget::RedDisk,
                State::new(Actor::RedDisk(super::RedDisk::Held)),
            ),
            _ => return None,
        };
        let sound = match target_kind {
            MurphySnapTarget::Base => Some(SoundEffect::Base),
            MurphySnapTarget::Infotron => Some(SoundEffect::Infotron),
            // Original Red Disk collection has no dedicated sound request.
            MurphySnapTarget::RedDisk => None,
        };

        // Preserve the target throughout the strip. Its reserved animation
        // prevents row-major actor scheduling, and collection/removal happens
        // only when Murphy reaches the last original coordinate.
        let state = self
            .looking(direction)
            .in_phase(MurphyPhase::Snapping(Snap::new(direction, target_kind)));
        Some(
            Transition::new(
                vec![
                    CellWrite::new(position, state.clone()),
                    CellWrite::new(target, held_state),
                ],
                sound.into_iter().map(GameEvent::PlaySound).collect(),
            )
            .with_drawing(position, state.actor().clone()),
        )
    }

    /// Moves, collects, pushes, crosses a port, or activates an adjacent actor.
    fn move_or_interact(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let target = world.offset(position, direction)?;
        let target_state = world.state(target)?;
        let moving_murphy = self.looking(direction);
        let murphy_actor = moving_murphy;

        match target_state.actor() {
            // An Empty actor can still be a synchronized `Vacating` collision
            // reservation. Consult the complete State before entering it;
            // matching only the actor identity would let Murphy cut through a
            // rock's or Infotron's still-active source animation.
            Actor::Empty(_) if target_state.is_empty() => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Empty,
            )),
            Actor::Base(_) => Some(Transition::move_murphy_with_events(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Base,
                vec![GameEvent::PlaySound(SoundEffect::Base)],
            )),
            Actor::Bug(_) if world.is_bug_active(target) => {
                Some(explode_at(world, position, false))
            }
            Actor::Bug(_) => Some(Transition::move_murphy_with_events(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Base,
                vec![GameEvent::PlaySound(SoundEffect::Base)],
            )),
            // Original ordinary movement checks Infotron state zero from Up,
            // Left, and Right. Its Down branch historically checks only the
            // tile kind, an observable directional quirk retained explicitly.
            Actor::Infotron(_) if direction == Direction::Down || target_state.is_idle() => {
                Some(Transition::move_murphy_with_events(
                    position,
                    target,
                    murphy_actor,
                    direction,
                    MurphyMoveTarget::Infotron,
                    vec![GameEvent::PlaySound(SoundEffect::Infotron)],
                ))
            }
            Actor::RedDisk(_) if world.is_active_red_disk(target) => {
                // A planted disk is position-owned rather than collectible.
                // Murphy may cover it, and the game-level fuse keeps ticking.
                Some(Transition::move_murphy(
                    position,
                    target,
                    murphy_actor,
                    direction,
                    MurphyMoveTarget::PlantedRedDisk,
                ))
            }
            Actor::RedDisk(_) if target_state.is_idle() => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::RedDisk,
            )),
            Actor::Exit(_) if world.remaining_infotrons() == 0 => Some(
                Transition::new(
                    vec![CellWrite::new(
                        position,
                        murphy_actor.in_phase(MurphyPhase::Exiting(Frame::first())),
                    )],
                    // The original sets its successful-level flag as soon as the
                    // unlocked Exit is selected. The forty pictures are a terminal
                    // disappearance sequence, not a deferred success condition.
                    vec![
                        GameEvent::Completed,
                        GameEvent::PlaySound(SoundEffect::Exit),
                    ],
                )
                .with_drawing(
                    position,
                    murphy_actor
                        .in_phase(MurphyPhase::Exiting(Frame::first()))
                        .actor()
                        .clone(),
                ),
            ),
            Actor::Zonk(_) if direction.is_horizontal() && target_state.is_idle() => {
                self.prepare_push(position, target, direction, world, MurphyPushTarget::Zonk)
            }
            Actor::YellowDisk(_) if target_state.is_idle() => self.prepare_push(
                position,
                target,
                direction,
                world,
                MurphyPushTarget::YellowDisk,
            ),
            Actor::OrangeDisk(_) if direction.is_horizontal() && target_state.is_idle() => self
                .prepare_push(
                    position,
                    target,
                    direction,
                    world,
                    MurphyPushTarget::OrangeDisk,
                ),
            Actor::Port(port) if port.allows(direction) => {
                self.cross_port(position, target, direction, murphy_actor, world)
            }
            Actor::Terminal(terminal) if !terminal.is_activated() => Some(Transition::new(
                vec![
                    CellWrite::new(position, State::new(Actor::Murphy(murphy_actor))),
                    // Preserve this panel's independently randomized wait and
                    // visible scroll phase when the level-wide latch is set.
                    CellWrite::new(target, State::new(Actor::Terminal(terminal.activate()))),
                ],
                vec![GameEvent::ActivateTerminal],
            )),
            Actor::SnikSnak(_) => Some(explode_at(world, target, false)),
            Actor::Electron(_) => Some(explode_at(world, target, true)),
            Actor::Explosion(explosion) if explosion.is_harmless() => {
                // Regular explosion states four through seven are already
                // harmless in the DOS collision helper. It erases that cell
                // and then dispatches the ordinary direction handler, so the
                // visible result is exactly an Empty-target Murphy movement.
                Some(Transition::move_murphy(
                    position,
                    target,
                    murphy_actor,
                    direction,
                    MurphyMoveTarget::Empty,
                ))
            }
            Actor::Explosion(_) => {
                // Young regular explosions and every Electron explosion are
                // lethal. The original re-detonates the destination cell, not
                // Murphy's source; because that cell is currently Explosion,
                // the new blast is always a normal, empty-residue explosion.
                Some(explode_at(world, target, false))
            }
            _ => None,
        }
    }

    /// Reserves a push target and destination before the original hold delay.
    fn prepare_push(
        &self,
        position: Position,
        target: Position,
        direction: Direction,
        world: &WorldView<'_>,
        pushed_target: MurphyPushTarget,
    ) -> Option<Transition> {
        let destination = world.offset(target, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        let mut preparing = self.looking(direction);
        preparing.phase = MurphyPhase::PreparingPush {
            action: PushAction::from_input(direction, pushed_target)?,
            // The initiating call decrements the original value eight to seven.
            remaining: Frame::last(),
        };
        let writes = vec![
            CellWrite::new(position, State::new(Actor::Murphy(preparing))),
            CellWrite::new(target, held_push_target(pushed_target)),
            CellWrite::new(destination, State::murphy_destination()),
        ];

        Some(Transition::new(writes, Vec::new()).with_drawing(position, Actor::Murphy(preparing)))
    }

    /// Atomically moves Murphy through a passable port into the cell beyond it.
    fn cross_port(
        &self,
        position: Position,
        port_position: Position,
        direction: Direction,
        murphy_actor: Murphy,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let destination = world.offset(port_position, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        // Murphy remains logically at the source until frame eight. The empty
        // cell beyond the port is reserved so no falling actor can enter it;
        // special-port metadata is deliberately deferred to completion.
        Some(
            Transition::new(
                vec![
                    CellWrite::new(
                        position,
                        murphy_actor.in_phase(MurphyPhase::CrossingPort {
                            direction,
                            frame: Frame::first(),
                        }),
                    ),
                    CellWrite::new(destination, State::murphy_destination()),
                ],
                Vec::new(),
            )
            .with_drawing(
                position,
                murphy_actor
                    .in_phase(MurphyPhase::CrossingPort {
                        direction,
                        frame: Frame::first(),
                    })
                    .actor()
                    .clone(),
            ),
        )
    }

    /// Updates only the horizontal look flag when input points left or right.
    const fn looking(mut self, direction: Direction) -> Self {
        match direction {
            Direction::Left => self.looking_left = true,
            Direction::Right => self.looking_left = false,
            Direction::Up | Direction::Down => {}
        }
        self.previous_input_was_none = false;
        self.phase = MurphyPhase::Ready;
        self
    }
}

/// Reconstructs the stable actor stored behind one push-target discriminator.
fn actor_for_push_target(target: MurphyPushTarget) -> Actor {
    match target {
        MurphyPushTarget::Zonk => Actor::Zonk(Zonk::resting()),
        MurphyPushTarget::YellowDisk => Actor::YellowDisk(YellowDisk::Resting),
        MurphyPushTarget::OrangeDisk => Actor::OrangeDisk(OrangeDisk::resting()),
    }
}

/// Validates that a reserved cell still contains the expected pushed actor.
fn pushed_actor_matches(actor: &Actor, target: MurphyPushTarget) -> bool {
    matches!(
        (actor, target),
        (Actor::Zonk(_), MurphyPushTarget::Zonk)
            | (Actor::YellowDisk(_), MurphyPushTarget::YellowDisk)
            | (Actor::OrangeDisk(_), MurphyPushTarget::OrangeDisk)
    )
}

/// Reports whether an original horizontal push state shields Murphy from a fall.
pub(super) fn murphy_is_protected_from_falling_actor(state: &State) -> bool {
    let Actor::Murphy(murphy) = state.actor() else {
        return false;
    };
    let preparing_horizontal_push = matches!(
        murphy.phase,
        MurphyPhase::PreparingPush { action, .. } if action.direction().is_horizontal()
    );
    let animating_horizontal_push = matches!(
        murphy.phase,
        MurphyPhase::Pushing { action, .. } if action.direction().is_horizontal()
    );
    preparing_horizontal_push || animating_horizontal_push
}

/// Reports whether Murphy is in one of the four port-traversal states.
pub(super) fn murphy_is_crossing_port(state: &State) -> bool {
    // Snik Snak turn-state collision uniquely exempts original Murphy states
    // 0x18 through 0x1b. Those four bytes are precisely the directional port
    // animations represented by this semantic variant.
    matches!(
        state.actor(),
        Actor::Murphy(Murphy {
            phase: MurphyPhase::CrossingPort { .. },
            ..
        })
    )
}

impl Default for Murphy {
    /// Uses the canonical right-facing starting pose.
    fn default() -> Self {
        Self::new()
    }
}

impl Murphy {
    /// Removes the still-held adjacent target and emits its collection effect.
    fn finish_snap(
        &self,
        position: Position,
        direction: Direction,
        target: MurphySnapTarget,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let mut writes = vec![CellWrite::new(
            position,
            State::new(Actor::Murphy(self.ready())),
        )];
        if let Some(target_position) = world.offset(position, direction)
            && world
                .state(target_position)
                .is_some_and(|target_state| target_state.actor().is_held())
        {
            writes.push(CellWrite::new(target_position, State::empty()));
        }
        let events = match target {
            MurphySnapTarget::Base => Vec::new(),
            MurphySnapTarget::Infotron => vec![GameEvent::CollectInfotron],
            MurphySnapTarget::RedDisk => vec![GameEvent::CollectRedDisk],
        };
        Some(Transition::new(writes, events))
    }
}

impl Murphy {
    /// Completes both transfers only if the held target and destination survive.
    fn finish_push(
        &self,
        position: Position,
        direction: Direction,
        target: MurphyPushTarget,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let Some(target_position) = world.offset(position, direction) else {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        };
        let Some(destination) = world.offset(target_position, direction) else {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        };
        let reservations_intact = world.state(target_position).is_some_and(|state| {
            state.actor().is_held() && pushed_actor_matches(state.actor(), target)
        }) && world.state(destination).is_some_and(|state| {
            state.reservation() == Some(super::empty::Reservation::MurphyDestination)
        });
        if !reservations_intact {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        }
        let pushed_state = if target == MurphyPushTarget::OrangeDisk
            && direction == Direction::Right
            && world
                .offset(destination, Direction::Down)
                .is_some_and(|below| world.is_empty(below))
        {
            OrangeDisk::resting()
                .in_phase(super::orange_disk::OrangePhase::AwaitingFall(Frame::first()))
        } else {
            State::new(actor_for_push_target(target))
        };
        let mut writes = vec![
            CellWrite::new(position, State::empty()),
            CellWrite::new(target_position, State::new(Actor::Murphy(self.ready()))),
            CellWrite::new(destination, pushed_state),
        ];
        if target == MurphyPushTarget::OrangeDisk
            && direction == Direction::Right
            && let Some(below) = world.offset(destination, Direction::Down)
            && world.is_empty(below)
        {
            // The original right-push completion immediately installs
            // Orange state 0x20 and its destination reservation. The
            // corresponding left-push path intentionally does not.
            writes.push(CellWrite::new(below, State::rounded_destination()));
        }
        Some(Transition::new(writes, Vec::new()))
    }
}

impl Murphy {
    /// Transfers Murphy through the reserved endpoint and applies special-port settings.
    fn finish_port(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let Some(port_position) = world.offset(position, direction) else {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        };
        let Some(destination) = world.offset(port_position, direction) else {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        };
        if !world.state(destination).is_some_and(|state| {
            state.reservation() == Some(super::empty::Reservation::MurphyDestination)
        }) {
            return Some(Transition::replace(
                position,
                State::new(Actor::Murphy(self.ready())),
            ));
        }
        let events = world
            .state(port_position)
            .and_then(|state| match state.actor() {
                Actor::Port(port) if port.is_special() => world
                    .special_port(port_position)
                    .copied()
                    .map(GameEvent::ApplySpecialPort),
                _ => None,
            })
            .into_iter()
            .collect();
        Some(Transition::new(
            vec![
                CellWrite::new(position, State::empty()),
                CellWrite::new(destination, State::new(Actor::Murphy(self.ready()))),
            ],
            events,
        ))
    }
}

impl Murphy {
    /// Releases a completed step's source and collects its material exactly once.
    fn finish_move(
        &self,
        position: Position,
        pose: FinalPose,
        world: &WorldView<'_>,
    ) -> Transition {
        let mut writes = vec![CellWrite::new(
            position,
            self.in_phase(MurphyPhase::Resuming(pose)),
        )];
        if let Some(source) = world.offset(position, pose.direction.opposite())
            && world.state(source).is_some_and(|state| matches!(state.actor(), Actor::Empty(super::Empty::Reserved(super::empty::Reservation::Vacating { direction, .. })) if *direction == pose.direction)) {
            // Never erase an explosion that replaced the source during travel.
            writes.push(CellWrite::new(source, State::empty()));
        }
        let events = match pose.target {
            MurphyMoveTarget::Empty | MurphyMoveTarget::Base | MurphyMoveTarget::PlantedRedDisk => {
                Vec::new()
            }
            MurphyMoveTarget::Infotron => vec![GameEvent::CollectInfotron],
            MurphyMoveTarget::RedDisk => vec![GameEvent::CollectRedDisk],
        };
        Transition::new(writes, events).with_drawing(
            position,
            self.in_phase(MurphyPhase::Resuming(pose)).actor().clone(),
        )
    }
}

/// Constructs a reserved push target without accepting an arbitrary actor.
fn held_push_target(target: MurphyPushTarget) -> State {
    match target {
        MurphyPushTarget::Zonk => Zonk::resting().in_phase(super::rounded::RoundedPhase::Held),
        MurphyPushTarget::OrangeDisk => {
            OrangeDisk::resting().in_phase(super::orange_disk::OrangePhase::Held)
        }
        MurphyPushTarget::YellowDisk => State::new(Actor::YellowDisk(YellowDisk::Held)),
    }
}
