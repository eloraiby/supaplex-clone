//! Player input, hold-sensitive actions, and Murphy animation descriptors.

use super::{
    Actor, Animation, AnimationKind, CellWrite, Direction, ExplosionResidue, GameEvent, OrangeDisk,
    Port, Position, State, Transition, YellowDisk, Zonk, explode_at,
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
    pub(super) const fn frame_count(self) -> u8 {
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
            // not by the generic finite-animation path.
            Self::PlantRedDisk => 65,
        }
    }

    /// Reports whether Murphy changes board cells when the action completes.
    pub(super) const fn changes_cell(self) -> bool {
        matches!(self, Self::Move { .. } | Self::Port { .. })
    }
}

/// Hold-sensitive substate that cannot be represented by a free-running strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MurphyPhase {
    /// Murphy is available for ordinary movement and interaction input.
    Ready,
    /// A push target and its destination are reserved while direction is held.
    PreparingPush {
        /// Direction that must remain held until the preparation delay expires.
        direction: Direction,
        /// Reserved actor that determines both validation and final animation.
        target: MurphyPushTarget,
        /// Original counter after the initial update, counting from seven to zero.
        remaining: u8,
    },
    /// Space-only Red Disk placement that can still be cancelled by releasing.
    PlantingRedDisk {
        /// Original counter after the initial update, counting from 63 to zero.
        remaining: u8,
    },
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
    /// Reports whether input must advance or cancel planting before animation.
    pub(super) const fn is_planting_red_disk(self) -> bool {
        matches!(self.phase, MurphyPhase::PlantingRedDisk { .. })
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
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let input = world.input();

        // Push and planting delays are input-sensitive. They must be handled
        // before generic animation advancement so releasing the required key
        // cancels the reserved action on the exact original update.
        match self.phase {
            MurphyPhase::PreparingPush {
                direction,
                target,
                remaining,
            } => {
                return Some(
                    self.continue_push(position, direction, target, remaining, input, world),
                );
            }
            MurphyPhase::PlantingRedDisk { remaining } => {
                return Some(self.continue_plant(position, remaining, input));
            }
            MurphyPhase::Ready => {}
        }

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
            next_murphy.phase = MurphyPhase::PlantingRedDisk { remaining: 0x3f };
            let murphy = State::animated(Actor::Murphy(next_murphy), Animation::murphy_plant(1));
            return Some(Transition::new(
                vec![CellWrite::new(position, murphy)],
                vec![GameEvent::BeginPlantRedDisk(position)],
            ));
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
        direction: Direction,
        target: MurphyPushTarget,
        remaining: u8,
        input: crate::game::Input,
        world: &WorldView<'_>,
    ) -> Transition {
        let target_position = world.offset(position, direction);
        let destination = target_position.and_then(|cell| world.offset(cell, direction));
        let reservations_intact = target_position
            .and_then(|cell| world.state(cell))
            .is_some_and(|state| {
                state.animation.kind == AnimationKind::MurphyPushTarget
                    && pushed_actor_matches(state.actor(), target)
            })
            && destination
                .and_then(|cell| world.state(cell))
                .is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::MurphyDestination
                });
        let still_holding = input.direction == Some(direction) && !input.action;

        if reservations_intact && still_holding {
            if remaining == 0 {
                let mut moving = *self;
                moving.phase = MurphyPhase::Ready;
                return Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::animated(
                            Actor::Murphy(moving),
                            Animation::murphy_push(direction, target),
                        ),
                    )],
                    vec![GameEvent::PlaySound(SoundEffect::Push)],
                );
            }

            let mut waiting = *self;
            waiting.phase = MurphyPhase::PreparingPush {
                direction,
                target,
                remaining: remaining - 1,
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
                state.animation.kind == AnimationKind::MurphyPushTarget
                    && pushed_actor_matches(state.actor(), target)
            })
        }) {
            writes.push(CellWrite::new(
                target_position,
                State::new(actor_for_push_target(target)),
            ));
        }
        if let Some(destination) = destination.filter(|cell| {
            world.state(*cell).is_some_and(|state| {
                matches!(state.actor(), Actor::Empty(_))
                    && state.animation.kind == AnimationKind::MurphyDestination
            })
        }) {
            writes.push(CellWrite::new(destination, State::empty()));
        }
        Transition::new(writes, Vec::new())
    }

    /// Advances, completes, or cancels the 64-update Space-only plant action.
    fn continue_plant(
        &self,
        position: Position,
        remaining: u8,
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
            );
        }

        if remaining == 0 {
            let mut completed = *self;
            completed.phase = MurphyPhase::Ready;
            completed.previous_input_was_none = false;
            return Transition::new(
                vec![CellWrite::new(
                    position,
                    State::new(Actor::Murphy(completed)),
                )],
                vec![GameEvent::FinishPlantRedDisk],
            );
        }

        let mut planting = *self;
        planting.phase = MurphyPhase::PlantingRedDisk {
            remaining: remaining - 1,
        };
        let elapsed = 0x40 - (remaining - 1);
        Transition::replace(
            position,
            State::animated(Actor::Murphy(planting), Animation::murphy_plant(elapsed)),
        )
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
        let target_kind = match target_state.actor() {
            Actor::Base(_) => MurphySnapTarget::Base,
            Actor::Bug(_) if !world.is_bug_active(target) => MurphySnapTarget::Base,
            Actor::Bug(_) => return Some(explode_at(world, position, false)),
            // Every original Space+direction branch requires an idle
            // Infotron. A moving or roll-reserved tile keeps its updater and
            // is allowed to run later in this same Murphy-first linear pass.
            Actor::Infotron(_) if target_state.is_idle() => MurphySnapTarget::Infotron,
            Actor::RedDisk(_) if target_state.is_idle() => MurphySnapTarget::RedDisk,
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
        let actor = Actor::Murphy(self.looking(direction));
        let state = State::animated(actor, Animation::murphy_snap(direction, target_kind));
        Some(Transition::new(
            vec![
                CellWrite::new(position, state),
                CellWrite::new(
                    target,
                    State::animated(
                        target_state.actor().clone(),
                        Animation::murphy_push_target(),
                    ),
                ),
            ],
            sound.into_iter().map(GameEvent::PlaySound).collect(),
        ))
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
        let murphy_actor = Actor::Murphy(moving_murphy);
        let looking_left = moving_murphy.looking_left;

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
                looking_left,
            )),
            Actor::Base(_) => Some(Transition::move_murphy_with_events(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Base,
                looking_left,
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
                looking_left,
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
                    looking_left,
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
                    looking_left,
                ))
            }
            Actor::RedDisk(_) if target_state.is_idle() => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::RedDisk,
                looking_left,
            )),
            Actor::Exit(_) if world.remaining_infotrons() == 0 => Some(Transition::new(
                vec![CellWrite::new(
                    position,
                    State::animated(murphy_actor, Animation::murphy_exit()),
                )],
                // The original sets its successful-level flag as soon as the
                // unlocked Exit is selected. The forty pictures are a terminal
                // disappearance sequence, not a deferred success condition.
                vec![
                    GameEvent::Completed,
                    GameEvent::PlaySound(SoundEffect::Exit),
                ],
            )),
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
                self.cross_port(position, target, direction, murphy_actor, *port, world)
            }
            Actor::Terminal(terminal) if !terminal.is_activated() => Some(Transition::new(
                vec![
                    CellWrite::new(position, State::new(murphy_actor)),
                    // Preserve this panel's independently randomized wait and
                    // visible scroll phase when the level-wide latch is set.
                    CellWrite::new(target, State::new(Actor::Terminal(terminal.activate()))),
                ],
                vec![GameEvent::ActivateTerminal],
            )),
            Actor::SnikSnak(_) => Some(explode_at(world, target, false)),
            Actor::Electron(_) => Some(explode_at(world, target, true)),
            Actor::Explosion(explosion)
                if explosion.residue() == ExplosionResidue::Empty
                    && target_state.animation.frame >= 4 =>
            {
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
                    looking_left,
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
            direction,
            target: pushed_target,
            // The initiating call decrements the original value eight to seven.
            remaining: 7,
        };
        let writes = vec![
            CellWrite::new(position, State::new(Actor::Murphy(preparing))),
            CellWrite::new(
                target,
                State::murphy_push_target(actor_for_push_target(pushed_target)),
            ),
            CellWrite::new(destination, State::murphy_destination()),
        ];

        Some(Transition::new(writes, Vec::new()))
    }

    /// Atomically moves Murphy through a passable port into the cell beyond it.
    fn cross_port(
        &self,
        position: Position,
        port_position: Position,
        direction: Direction,
        murphy_actor: Actor,
        _port: Port,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let destination = world.offset(port_position, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        // Murphy remains logically at the source until frame eight. The empty
        // cell beyond the port is reserved so no falling actor can enter it;
        // special-port metadata is deliberately deferred to completion.
        Some(Transition::new(
            vec![
                CellWrite::new(
                    position,
                    State::animated(murphy_actor, Animation::murphy_port(direction)),
                ),
                CellWrite::new(destination, State::murphy_destination()),
            ],
            Vec::new(),
        ))
    }

    /// Updates only the horizontal look flag when input points left or right.
    const fn looking(mut self, direction: Direction) -> Self {
        if matches!(direction, Direction::Left) {
            self.looking_left = true;
        } else if matches!(direction, Direction::Right) {
            self.looking_left = false;
        }
        self.previous_input_was_none = false;
        self.phase = MurphyPhase::Ready;
        self
    }
}

/// Reconstructs the stable actor stored behind one push-target discriminator.
pub(super) fn actor_for_push_target(target: MurphyPushTarget) -> Actor {
    match target {
        MurphyPushTarget::Zonk => Actor::Zonk(Zonk::resting()),
        MurphyPushTarget::YellowDisk => Actor::YellowDisk(YellowDisk),
        MurphyPushTarget::OrangeDisk => Actor::OrangeDisk(OrangeDisk::resting()),
    }
}

/// Validates that a reserved cell still contains the expected pushed actor.
pub(super) fn pushed_actor_matches(actor: &Actor, target: MurphyPushTarget) -> bool {
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
        MurphyPhase::PreparingPush { direction, .. } if direction.is_horizontal()
    );
    let animating_horizontal_push = matches!(
        state.animation.kind,
        AnimationKind::Murphy(MurphyAnimation::Push { direction, .. })
            if direction.is_horizontal()
    );
    preparing_horizontal_push || animating_horizontal_push
}

/// Reports whether Murphy is in one of the four port-traversal states.
pub(super) fn murphy_is_crossing_port(state: &State) -> bool {
    // Snik Snak turn-state collision uniquely exempts original Murphy states
    // 0x18 through 0x1b. Those four bytes are precisely the directional port
    // animations represented by this semantic variant.
    matches!(
        state,
        State {
            actor: Actor::Murphy(_),
            animation: Animation {
                kind: AnimationKind::Murphy(MurphyAnimation::Port { .. }),
                ..
            },
        }
    )
}

impl Default for Murphy {
    /// Uses the canonical right-facing starting pose.
    fn default() -> Self {
        Self::new()
    }
}
