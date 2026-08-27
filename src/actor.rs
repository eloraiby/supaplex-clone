//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every level cell contains one [`State`].  An [`Actor`] is an enum whose
//! variants wrap actor-specific structs, while [`Animation`] records the visual
//! phase and the semantic transition that follows its final frame.

use crate::{game::WorldView, level::SpecialPort};

/// Number of simulation frames used for one cell-to-cell movement.
const MOVEMENT_FRAMES: u8 = 4;

/// Number of source frames in either explosion strip of `RocksSP.png`.
const EXPLOSION_FRAMES: u8 = 8;

/// Number of lethal logical phases in each active Bug cycle.
const BUG_ACTIVE_FRAMES: u8 = 14;

/// Delay before an actor touched by one blast emits its own secondary wave.
pub(crate) const CHAIN_REACTION_FRAMES: u8 = 13;

/// Number of simulation frames between a Zonk strike and Orange Disk blast.
const ORANGE_DISK_TRIGGER_FRAMES: u8 = 6;

/// Countdown value at which a completely planted Red Disk detonates.
pub(crate) const RED_DISK_DETONATION_COUNTDOWN: u8 = 0x28;

/// A zero-based location on the row-major board.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Position {
    /// Horizontal cell coordinate, increasing to the right.
    pub x: usize,
    /// Vertical cell coordinate, increasing downward.
    pub y: usize,
}

impl Position {
    /// Creates a board position without assuming any particular dimensions.
    pub const fn new(x: usize, y: usize) -> Self {
        Self { x, y }
    }
}

/// One of the four orthogonal movement and facing directions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// One row toward the top of the board.
    Up,
    /// One column toward the right edge of the board.
    Right,
    /// One row toward the bottom of the board.
    Down,
    /// One column toward the left edge of the board.
    Left,
}

impl Direction {
    /// Stable iteration order used by neighborhood and enemy checks.
    pub const ALL: [Self; 4] = [Self::Up, Self::Right, Self::Down, Self::Left];

    /// Returns the direction obtained by turning counter-clockwise.
    pub const fn left(self) -> Self {
        match self {
            Self::Up => Self::Left,
            Self::Right => Self::Up,
            Self::Down => Self::Right,
            Self::Left => Self::Down,
        }
    }

    /// Returns the direction obtained by turning clockwise.
    pub const fn right(self) -> Self {
        match self {
            Self::Up => Self::Right,
            Self::Right => Self::Down,
            Self::Down => Self::Left,
            Self::Left => Self::Up,
        }
    }

    /// Returns the direction facing back toward the current cell.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Right => Self::Left,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
        }
    }

    /// Reports whether this direction can be used for a horizontal push.
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// Visual family used to select a frame from the sprite atlas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationKind {
    /// A stationary actor rendered with its normal tile sprite.
    Idle,
    /// A Zonk armed to begin falling once its destination is still free.
    ZonkPreFall,
    /// A non-rendered source cell reserved while its actor leaves that cell.
    Vacating(Direction),
    /// An actor entering its current cell from the opposite direction.
    Moving(Direction),
    /// A rounded actor entering its current cell diagonally from the row above.
    Rolling(Direction),
    /// Murphy entering a cell from two cells away through an intervening port.
    PortTraversal(Direction),
    /// Murphy acting on an adjacent cell without changing cells.
    Snapping(Direction),
    /// A Bug's fourteen-frame lethal spark cycle.
    Bug,
    /// A safe Bug waiting an independently randomized number of quarter ticks.
    BugDormant,
    /// The cyclic movement frames of a Snik Snak.
    SnikSnak,
    /// The cyclic spark frames of an Electron.
    Electron,
    /// The current retained scroll frame of a Terminal screen.
    Terminal,
    /// A Red Disk counting down before it explodes.
    RedDiskFuse,
    /// An Orange Disk counting down after being struck by a falling Zonk.
    OrangeDiskFuse,
    /// A normal explosion that ultimately leaves empty space.
    Explosion,
    /// An Electron explosion that ultimately leaves Infotrons.
    ElectronExplosion,
}

/// Semantic action guaranteed to follow an animation's final frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnimationNext {
    /// Ask the actor to return to its default resting/cyclic animation.
    Settle,
    /// Run the actor's environment-dependent behavior immediately.
    Act,
    /// Transfer a pre-fall Zonk only if the cell below remains unoccupied.
    BeginZonkFall,
    /// Give completed movement back to Murphy, settling if there is no input.
    ResumeMurphy,
    /// Ask the game session to choose this Bug's next dormant duration.
    RandomizeBug,
    /// Return a dormant Bug to lethal active frame zero.
    ActivateBug,
    /// Release a movement source reservation as ordinary empty space.
    Release,
    /// Replace the animated cell with empty space.
    BecomeEmpty,
    /// Replace the animated cell with a stationary Infotron.
    BecomeInfotron,
    /// Emit a fuse-owned 3×3 wave after a Red or Orange Disk countdown.
    Explode(ExplosionResidue),
}

/// A validated animation phase, frame, duration, and terminal transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Animation {
    /// Sprite family rendered for this animation.
    kind: AnimationKind,
    /// Zero-based frame currently visible.
    frame: u8,
    /// Number of frames before the terminal transition must occur.
    frame_count: u8,
    /// Semantic transition implied by completing the final frame.
    next: AnimationNext,
}

impl Animation {
    /// Creates the single-frame resting state used by inert actors.
    pub fn idle() -> Self {
        Self {
            kind: AnimationKind::Idle,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Settle,
        }
    }

    /// Creates the one-update arming phase before a resting Zonk falls.
    fn zonk_pre_fall() -> Self {
        // A single-frame non-idle animation completes on the Zonk's next
        // scheduled callback. Keeping the promised transfer in `next` makes
        // the delay part of cell state rather than an implicit game-loop flag.
        Self {
            kind: AnimationKind::ZonkPreFall,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginZonkFall,
        }
    }

    /// Creates a synchronized, invisible reservation for a movement source.
    fn vacating(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Vacating(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Release,
        }
    }

    /// Creates interpolation frames for an actor already in its destination.
    fn moving(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Moving(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Settle,
        }
    }

    /// Creates interpolation frames for a diagonal rounded-object roll.
    fn rolling(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Rolling(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Settle,
        }
    }

    /// Creates interpolation frames for Murphy's two-cell port traversal.
    fn port_traversal(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::PortTraversal(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Settle,
        }
    }

    /// Holds a completed movement's final pose until Murphy's next update.
    fn murphy_ready(kind: AnimationKind) -> Self {
        debug_assert!(
            matches!(
                kind,
                AnimationKind::Moving(_) | AnimationKind::PortTraversal(_)
            ),
            "only Murphy movement phases can become input-ready"
        );
        Self {
            kind,
            frame: MOVEMENT_FRAMES - 1,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::ResumeMurphy,
        }
    }

    /// Creates a short non-moving Murphy action animation.
    fn snapping(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Snapping(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Settle,
        }
    }

    /// Creates an actor cycle that requests a movement decision after it plays.
    fn cycle_then_act(kind: AnimationKind, frame_count: u8) -> Self {
        Self {
            kind,
            frame: 0,
            frame_count,
            next: AnimationNext::Act,
        }
    }

    /// Creates the passive visual state for one Terminal screen offset.
    ///
    /// Terminal scrolling is scheduled by the actor's signed delay rather than
    /// by animation completion.  Keeping the currently displayed screen in an
    /// `Animation` still gives the renderer one validated frame without making
    /// the generic animation engine advance it every simulation tick.
    fn terminal(frame: u8) -> Self {
        Self {
            kind: AnimationKind::Terminal,
            frame: frame % 7,
            frame_count: 7,
            next: AnimationNext::Act,
        }
    }

    /// Creates the synchronized lethal phase used by every newly loaded Bug.
    fn bug_active() -> Self {
        Self {
            kind: AnimationKind::Bug,
            frame: 0,
            frame_count: BUG_ACTIVE_FRAMES,
            next: AnimationNext::RandomizeBug,
        }
    }

    /// Creates one safe per-Bug cooldown measured in quarter-rate updates.
    fn bug_dormant(delay: u8) -> Self {
        debug_assert!(delay > 0, "a Bug cooldown must consume at least one update");
        Self {
            kind: AnimationKind::BugDormant,
            frame: 0,
            frame_count: delay.max(1),
            next: AnimationNext::ActivateBug,
        }
    }

    /// Creates the finite fuse placed on a dropped Red Disk.
    fn red_disk_fuse(frame: u8) -> Self {
        Self {
            kind: AnimationKind::RedDiskFuse,
            frame: frame.min(RED_DISK_DETONATION_COUNTDOWN - 1),
            frame_count: RED_DISK_DETONATION_COUNTDOWN,
            next: AnimationNext::Explode(ExplosionResidue::Empty),
        }
    }

    /// Creates the short delayed fuse caused by a Zonk striking Orange Disk.
    fn orange_disk_fuse() -> Self {
        Self {
            kind: AnimationKind::OrangeDiskFuse,
            frame: 0,
            frame_count: ORANGE_DISK_TRIGGER_FRAMES,
            next: AnimationNext::Explode(ExplosionResidue::Empty),
        }
    }

    /// Reports whether the phase interpolates an actor between board cells.
    const fn is_movement(&self) -> bool {
        matches!(
            self.kind,
            AnimationKind::Moving(_) | AnimationKind::Rolling(_) | AnimationKind::PortTraversal(_)
        )
    }

    /// Creates one of the finite eight-frame explosion animations.
    fn explosion(residue: ExplosionResidue) -> Self {
        let (kind, next) = match residue {
            ExplosionResidue::Empty => (AnimationKind::Explosion, AnimationNext::BecomeEmpty),
            ExplosionResidue::Infotron => (
                AnimationKind::ElectronExplosion,
                AnimationNext::BecomeInfotron,
            ),
        };

        Self {
            kind,
            frame: 0,
            frame_count: EXPLOSION_FRAMES,
            next,
        }
    }

    /// Returns the sprite family represented by this animation.
    pub fn kind(&self) -> AnimationKind {
        self.kind
    }

    /// Returns the zero-based visible frame.
    pub fn frame(&self) -> u8 {
        self.frame
    }

    /// Returns the number of frames in this validated animation.
    pub fn frame_count(&self) -> u8 {
        self.frame_count
    }

    /// Returns normalized visual progress in the inclusive range `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        // A one-frame idle animation has no interpolation and therefore reports
        // completion instead of dividing by zero.
        if self.frame_count <= 1 {
            return 1.0;
        }

        f32::from(self.frame) / f32::from(self.frame_count - 1)
    }

    /// Advances one frame or exposes the promised terminal transition.
    fn advance(&self) -> AnimationAdvance {
        // Idle is a stable state rather than a finite animation; actors in this
        // state are allowed to inspect their neighbors and choose new behavior.
        if matches!(self.kind, AnimationKind::Idle | AnimationKind::Terminal) {
            return AnimationAdvance::Ready;
        }

        if self.frame + 1 < self.frame_count {
            let mut animation = self.clone();
            animation.frame += 1;
            AnimationAdvance::Frame(animation)
        } else {
            AnimationAdvance::Finished(self.next)
        }
    }
}

/// Result of asking an animation to perform one simulation step.
#[derive(Clone, Debug, Eq, PartialEq)]
enum AnimationAdvance {
    /// The actor is idle and should inspect the current world.
    Ready,
    /// The next frame should replace the current cell state.
    Frame(Animation),
    /// The final frame promises a semantic transition now.
    Finished(AnimationNext),
}

/// Complete content of one board cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    /// Actor-specific identity and persistent behavior data.
    actor: Actor,
    /// Current visual frame and the semantic transition it implies.
    animation: Animation,
}

impl State {
    /// Creates a state using the actor's validated default animation.
    pub fn new(actor: Actor) -> Self {
        let animation = actor.default_animation();
        Self { actor, animation }
    }

    /// Creates the canonical empty state used by transition writes.
    pub fn empty() -> Self {
        Self::new(Actor::Empty(Empty))
    }

    /// Creates an Empty actor whose animation still reserves a movement source.
    fn vacating(direction: Direction) -> Self {
        Self::animated(Actor::Empty(Empty), Animation::vacating(direction))
    }

    /// Returns this cell's actor identity and actor-specific fields.
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Returns the current animation, including its frame and direction.
    pub fn animation(&self) -> &Animation {
        &self.animation
    }

    /// Creates a state with an explicitly validated animation.
    fn animated(actor: Actor, animation: Animation) -> Self {
        Self { actor, animation }
    }

    /// Creates a planted Red Disk whose visible fuse resumes at `frame`.
    pub(crate) fn planted_red_disk(frame: u8) -> Self {
        Self::animated(Actor::RedDisk(RedDisk), Animation::red_disk_fuse(frame))
    }

    /// Creates a safe Bug whose independently selected cooldown has just begun.
    pub(crate) fn dormant_bug(delay: u8) -> Self {
        Self::animated(Actor::Bug(Bug), Animation::bug_dormant(delay))
    }

    /// Reports whether this state is unoccupied for collision purposes.
    pub fn is_empty(&self) -> bool {
        matches!(self.actor, Actor::Empty(_))
            && !matches!(self.animation.kind, AnimationKind::Vacating(_))
    }

    /// Reports whether the actor is in its stable, non-moving phase.
    pub fn is_idle(&self) -> bool {
        self.animation.kind == AnimationKind::Idle
    }
}

/// Empty space, which never changes itself during its scheduled update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Empty;

impl Empty {
    /// Leaves empty space unchanged; neighboring actors may still write here.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// A rounded rock that falls, rolls, can be pushed, and can crush actors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Zonk {
    /// Whether this Zonk has downward momentum from an earlier fall.
    falling: bool,
}

impl Zonk {
    /// Creates a stationary Zonk as read from a level record.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether this Zonk currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    /// Chooses a fall or roll from the full states of neighboring cells.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
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
                return Some(Transition::roll_actor(
                    position,
                    diagonal,
                    Actor::Zonk(Self { falling: false }),
                    direction,
                ));
            }
        }

        None
    }

    /// Starts the armed fall when its destination survived the intervening tick.
    fn begin_fall(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
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

/// Diggable green circuit-board material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Base;

impl Base {
    /// Remains in place until Murphy or an explosion replaces it atomically.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// The player-controlled actor and its persistent facing direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Murphy {
    /// Direction used for the stationary sprite and Red Disk placement.
    facing: Direction,
}

impl Murphy {
    /// Creates Murphy facing right, matching the original starting pose.
    pub const fn new() -> Self {
        Self {
            facing: Direction::Right,
        }
    }

    /// Returns the direction Murphy most recently attempted to move.
    pub const fn facing(self) -> Direction {
        self.facing
    }

    /// Interprets player intent against complete neighboring cell states.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let input = world.input();

        // Original Murphy gravity rewrites unsupported input to Down. The only
        // exceptions are plain Up/Left/Right commands that eat Base, plus an
        // upward-capable port directly above Murphy that acts as a handhold.
        if self.is_pulled_by_gravity(position, world) {
            let base_exception = !input.action
                && !input.drop_disk
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
                return self.move_or_interact(position, Direction::Down, world);
            }
        }

        // Space without a direction is the classic plant command; D is kept as
        // an ergonomic one-shot alias. The game event records this exact cell,
        // so the disk appears underneath Murphy only after he moves away.
        let plant_disk = input.drop_disk || (input.action && input.direction.is_none());
        if plant_disk && world.red_disks() > 0 && !world.has_active_red_disk() {
            let murphy = State::animated(Actor::Murphy(*self), Animation::snapping(self.facing));
            return Some(Transition::new(
                vec![CellWrite::new(position, murphy)],
                vec![GameEvent::PlantRedDisk(position)],
            ));
        }

        if let Some(direction) = input.direction {
            if input.action {
                return self.snap(position, direction, world);
            }
            return self.move_or_interact(position, direction, world);
        }

        None
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
        let mut events = Vec::new();

        match target_state.actor() {
            Actor::Base(_) => {}
            Actor::Bug(_) if !world.is_bug_active(target) => {}
            Actor::Bug(_) => return Some(explode_at(world, position, false)),
            Actor::Infotron(_) => events.push(GameEvent::CollectInfotron),
            Actor::RedDisk(_) if target_state.is_idle() => events.push(GameEvent::CollectRedDisk),
            _ => return None,
        }

        // The source animation and target removal share one transition so no
        // later actor observes only half of the completed snap action.
        let actor = Actor::Murphy(Self { facing: direction });
        let state = State::animated(actor, Animation::snapping(direction));
        Some(Transition::new(
            vec![
                CellWrite::new(position, state),
                CellWrite::new(target, State::empty()),
            ],
            events,
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
        let murphy_actor = Actor::Murphy(Self { facing: direction });

        match target_state.actor() {
            // An Empty actor can still be a synchronized `Vacating` collision
            // reservation. Consult the complete State before entering it;
            // matching only the actor identity would let Murphy cut through a
            // rock's or Infotron's still-active source animation.
            Actor::Empty(_) if target_state.is_empty() => Some(Transition::move_actor(
                position,
                target,
                murphy_actor,
                direction,
            )),
            Actor::Base(_) => Some(Transition::move_actor(
                position,
                target,
                murphy_actor,
                direction,
            )),
            Actor::Bug(_) if world.is_bug_active(target) => {
                Some(explode_at(world, position, false))
            }
            Actor::Bug(_) => Some(Transition::move_actor(
                position,
                target,
                murphy_actor,
                direction,
            )),
            Actor::Infotron(_) => Some(Transition::move_actor_with_events(
                position,
                target,
                murphy_actor,
                direction,
                vec![GameEvent::CollectInfotron],
            )),
            Actor::RedDisk(_) if world.is_active_red_disk(target) => {
                // A planted disk is position-owned rather than collectible.
                // Murphy may cover it, and the game-level fuse keeps ticking.
                Some(Transition::move_actor(
                    position,
                    target,
                    murphy_actor,
                    direction,
                ))
            }
            Actor::RedDisk(_) if target_state.is_idle() => {
                Some(Transition::move_actor_with_events(
                    position,
                    target,
                    murphy_actor,
                    direction,
                    vec![GameEvent::CollectRedDisk],
                ))
            }
            Actor::Exit(_) if world.remaining_infotrons() == 0 => Some(Transition::new(
                vec![CellWrite::new(
                    position,
                    State::animated(murphy_actor, Animation::snapping(direction)),
                )],
                vec![GameEvent::Completed],
            )),
            Actor::Zonk(_) if direction.is_horizontal() && target_state.is_idle() => self.push(
                position,
                target,
                direction,
                murphy_actor,
                world,
                Actor::Zonk(Zonk::resting()),
            ),
            Actor::YellowDisk(_) if target_state.is_idle() => self.push(
                position,
                target,
                direction,
                murphy_actor,
                world,
                Actor::YellowDisk(YellowDisk),
            ),
            Actor::OrangeDisk(_) if direction.is_horizontal() && target_state.is_idle() => self
                .push(
                    position,
                    target,
                    direction,
                    murphy_actor,
                    world,
                    Actor::OrangeDisk(OrangeDisk::resting()),
                ),
            Actor::Port(port) if port.allows(direction) => {
                self.cross_port(position, target, direction, murphy_actor, *port, world)
            }
            Actor::Terminal(terminal) if !terminal.is_activated() => Some(Transition::new(
                vec![
                    CellWrite::new(
                        position,
                        State::animated(murphy_actor, Animation::snapping(direction)),
                    ),
                    // Preserve this panel's independently randomized wait and
                    // visible scroll phase when the level-wide latch is set.
                    CellWrite::new(target, State::new(Actor::Terminal(terminal.activate()))),
                ],
                vec![GameEvent::ActivateTerminal],
            )),
            Actor::SnikSnak(_) => Some(explode_at(world, target, false)),
            Actor::Electron(_) => Some(explode_at(world, target, true)),
            Actor::Explosion(explosion) => Some(explode_at(
                world,
                position,
                explosion.residue == ExplosionResidue::Infotron,
            )),
            _ => None,
        }
    }

    /// Atomically pushes one eligible actor and moves Murphy into its old cell.
    fn push(
        &self,
        position: Position,
        target: Position,
        direction: Direction,
        murphy_actor: Actor,
        world: &WorldView<'_>,
        pushed_actor: Actor,
    ) -> Option<Transition> {
        let destination = world.offset(target, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        let writes = vec![
            CellWrite::new(position, State::vacating(direction)),
            CellWrite::new(
                target,
                State::animated(murphy_actor, Animation::moving(direction)),
            ),
            CellWrite::new(
                destination,
                State::animated(pushed_actor, Animation::moving(direction)),
            ),
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
        port: Port,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let destination = world.offset(port_position, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        let mut events = Vec::new();
        if port.special
            && let Some(settings) = world.special_port(port_position)
        {
            events.push(GameEvent::ApplySpecialPort(*settings));
        }

        // The port remains unchanged between the source and destination writes.
        Some(Transition::new(
            vec![
                CellWrite::new(position, State::vacating(direction)),
                CellWrite::new(
                    destination,
                    State::animated(murphy_actor, Animation::port_traversal(direction)),
                ),
            ],
            events,
        ))
    }
}

impl Default for Murphy {
    /// Uses the canonical right-facing starting pose.
    fn default() -> Self {
        Self::new()
    }
}

/// A collectible that falls and rolls with Zonk-like physics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Infotron {
    /// Whether this Infotron has downward momentum from an earlier fall.
    falling: bool,
}

impl Infotron {
    /// Creates a stationary collectible as read from a level record.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether this Infotron currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    /// Chooses a fall or rounded-support roll from neighboring states.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if world.is_empty(below) {
            return Some(Transition::move_actor(
                position,
                below,
                Actor::Infotron(Self { falling: true }),
                Direction::Down,
            ));
        }

        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Direction::Left, Direction::Right] {
            let side = world.offset(position, direction)?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::roll_actor(
                    position,
                    diagonal,
                    Actor::Infotron(Self { falling: false }),
                    direction,
                ));
            }
        }

        None
    }
}

/// Visual orientation of a destructible RAM-chip wall segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RamChipShape {
    /// Standalone square chip.
    Center,
    /// Left edge of a horizontal chip strip.
    Left,
    /// Right edge of a horizontal chip strip.
    Right,
    /// Top edge of a vertical chip strip.
    Top,
    /// Bottom edge of a vertical chip strip.
    Bottom,
}

/// A destructible wall whose orientation is retained for rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RamChip {
    /// On-disk visual shape with identical collision behavior for all values.
    shape: RamChipShape,
}

impl RamChip {
    /// Creates a RAM-chip wall segment with the requested visual shape.
    pub const fn new(shape: RamChipShape) -> Self {
        Self { shape }
    }

    /// Returns the visual shape loaded from the level tile code.
    pub const fn shape(self) -> RamChipShape {
        self.shape
    }

    /// Remains stationary until an explosion replaces this destructible wall.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Indestructible hardware with one of the original decorative appearances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hardware {
    /// Raw visual variant in the inclusive range `0..=10`.
    variant: u8,
}

/// Hidden indestructible wall used by extended classic level files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InvisibleWall;

impl InvisibleWall {
    /// Never changes and deliberately renders as empty until Murphy touches it.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

impl Hardware {
    /// Creates hardware while retaining the level's decorative variant.
    pub const fn new(variant: u8) -> Self {
        Self { variant }
    }

    /// Returns the raw decorative variant used by sprite mapping.
    pub const fn variant(self) -> u8 {
        self.variant
    }

    /// Never changes; even explosion transitions deliberately skip Hardware.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Locked goal tile that completes a level after all required Infotrons.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Exit;

impl Exit {
    /// Remains stationary; Murphy owns the interaction and completion event.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Falling explosive disk that detonates when a fall reaches an obstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrangeDisk {
    /// Whether the disk has moved downward and is armed to explode on landing.
    falling: bool,
}

impl OrangeDisk {
    /// Creates a stable disk that has not begun a fall.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether the disk has begun its irreversible fall.
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    /// Falls through empty space or explodes after reaching an obstruction.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if world.is_empty(below) {
            return Some(Transition::move_actor(
                position,
                below,
                Actor::OrangeDisk(Self { falling: true }),
                Direction::Down,
            ));
        }

        if self.falling {
            return Some(explode_at(world, position, false));
        }

        None
    }
}

/// Directional permissions represented by a port tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortDirections {
    /// Traversal is allowed only in one direction.
    OneWay(Direction),
    /// Traversal is allowed up or down.
    Vertical,
    /// Traversal is allowed left or right.
    Horizontal,
    /// Traversal is allowed in all four directions.
    Any,
}

/// A pass-through tile that may also change global physics settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Port {
    /// Directions Murphy may travel while crossing this tile.
    directions: PortDirections,
    /// Whether this tile consults a matching special-port metadata record.
    special: bool,
}

impl Port {
    /// Creates a regular or metadata-driven port.
    pub const fn new(directions: PortDirections, special: bool) -> Self {
        Self {
            directions,
            special,
        }
    }

    /// Returns the port's directional collision rule.
    pub const fn directions(self) -> PortDirections {
        self.directions
    }

    /// Reports whether crossing this port can update global settings.
    pub const fn is_special(self) -> bool {
        self.special
    }

    /// Reports whether a traversal direction is accepted by this port.
    pub const fn allows(self, direction: Direction) -> bool {
        match self.directions {
            PortDirections::OneWay(allowed) => direction as u8 == allowed as u8,
            PortDirections::Vertical => matches!(direction, Direction::Up | Direction::Down),
            PortDirections::Horizontal => matches!(direction, Direction::Left | Direction::Right),
            PortDirections::Any => true,
        }
    }

    /// Remains stationary because Murphy performs the complete traversal write.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Scissor-like enemy that follows walls and explodes on contact with Murphy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnikSnak {
    /// Direction used as the basis of the next left-hand wall-following choice.
    heading: Direction,
}

impl SnikSnak {
    /// Creates a Snik Snak with a deterministic initial heading.
    pub const fn new(heading: Direction) -> Self {
        Self { heading }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Attacks adjacent Murphy or chooses the next wall-following step.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        if world.freeze_enemies() {
            return Some(Transition::replace(
                position,
                State::new(Actor::SnikSnak(*self)),
            ));
        }

        if adjacent_murphy(position, world).is_some() {
            // The blast belongs to the enemy's cell; Murphy is merely one of
            // the adjacent affected cells and therefore receives the death event.
            return Some(explode_at(world, position, false));
        }

        // Snik Snaks keep their left side near a wall. Each candidate is read
        // from the board left by every earlier cell in the linear update pass.
        for direction in [
            self.heading.left(),
            self.heading,
            self.heading.right(),
            self.heading.opposite(),
        ] {
            if let Some(destination) = world.offset(position, direction)
                && world.is_empty(destination)
            {
                return Some(Transition::move_actor(
                    position,
                    destination,
                    Actor::SnikSnak(Self { heading: direction }),
                    direction,
                ));
            }
        }

        Some(Transition::replace(
            position,
            State::new(Actor::SnikSnak(*self)),
        ))
    }
}

/// Pushable disk detonated by a Terminal's live row-major scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YellowDisk;

impl YellowDisk {
    /// Remains stationary until Murphy pushes it or a Terminal detonates it.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Computer terminal with an independently delayed scrolling display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Terminal {
    /// Whether Murphy has already used this panel during the current level.
    activated: bool,
    /// Signed original-style counter incremented once per simulation update.
    delay: i8,
    /// Repacked atlas frame representing the screen's current scroll offset.
    screen_frame: u8,
}

impl Terminal {
    /// Creates an unused panel ready to choose its first randomized delay.
    pub const fn new() -> Self {
        Self {
            activated: false,
            delay: 0,
            screen_frame: 0,
        }
    }

    /// Returns a copy with the level-wide Yellow Disk latch marked as consumed.
    ///
    /// Activating a terminal must not reset its randomized delay or its current
    /// screen offset; the original panel continues scrolling after detonation.
    pub(crate) const fn activate(self) -> Self {
        Self {
            activated: true,
            ..self
        }
    }

    /// Reports whether this panel has already been used.
    pub const fn is_activated(self) -> bool {
        self.activated
    }

    /// Returns the current atlas frame of the scrolling screen.
    pub const fn screen_frame(self) -> u8 {
        self.screen_frame
    }

    /// Replaces the signed delay and advances the displayed scroll position.
    ///
    /// The game owns the shared pseudo-random stream, so the actor requests a
    /// randomized value through an event and receives the resulting state in a
    /// single row-ordered write.
    pub(crate) const fn after_scroll(self, delay: i8) -> Self {
        Self {
            delay,
            screen_frame: (self.screen_frame + 1) % 7,
            ..self
        }
    }

    /// Advances the original signed wait counter or requests one screen scroll.
    fn transition(&self, position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        // The original byte is interpreted as signed and incremented before it
        // is tested.  Negative and zero results continue waiting; a positive
        // result consumes the shared RNG and scrolls the terminal once.
        let next_delay = self.delay.wrapping_add(1);
        if next_delay <= 0 {
            let terminal = Self {
                delay: next_delay,
                ..*self
            };
            return Some(Transition::replace(
                position,
                State::new(Actor::Terminal(terminal)),
            ));
        }

        Some(Transition::new(
            Vec::new(),
            vec![GameEvent::RandomizeTerminal(position)],
        ))
    }
}

impl Default for Terminal {
    /// Creates the normal unused panel found in serialized levels.
    fn default() -> Self {
        Self::new()
    }
}

/// Collectible and droppable explosive disk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RedDisk;

impl RedDisk {
    /// Remains inert unless its animation's promised fuse transition fires.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Spark enemy that follows walls and produces Infotrons when destroyed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Electron {
    /// Direction used as the basis of the next right-hand wall-following choice.
    heading: Direction,
}

impl Electron {
    /// Creates an Electron with a deterministic initial heading.
    pub const fn new(heading: Direction) -> Self {
        Self { heading }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Attacks adjacent Murphy or chooses the next wall-following step.
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        if world.freeze_enemies() {
            return Some(Transition::replace(
                position,
                State::new(Actor::Electron(*self)),
            ));
        }

        if adjacent_murphy(position, world).is_some() {
            // Electron residue is centered on the Electron rather than shifted
            // onto the adjacent player cell.
            return Some(explode_at(world, position, true));
        }

        // Electrons use the mirrored wall-following preference of Snik Snaks.
        for direction in [
            self.heading.right(),
            self.heading,
            self.heading.left(),
            self.heading.opposite(),
        ] {
            if let Some(destination) = world.offset(position, direction)
                && world.is_empty(destination)
            {
                return Some(Transition::move_actor(
                    position,
                    destination,
                    Actor::Electron(Self { heading: direction }),
                    direction,
                ));
            }
        }

        Some(Transition::replace(
            position,
            State::new(Actor::Electron(*self)),
        ))
    }
}

/// A Base-like hazard whose animation alternates safe and active frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Bug;

impl Bug {
    /// Has no movement; its repeating animation controls Murphy interactions.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Runtime residue produced while a 3×3 explosion animation is active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplosionResidue {
    /// The cell becomes empty after the last explosion frame.
    Empty,
    /// The cell becomes an Infotron after the last explosion frame.
    Infotron,
}

/// Runtime-only actor for one cell of a normal or Electron explosion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Explosion {
    /// Actor that replaces this cell after the visual explosion finishes.
    residue: ExplosionResidue,
}

impl Explosion {
    /// Creates one explosion cell with an explicit terminal residue.
    pub const fn new(residue: ExplosionResidue) -> Self {
        Self { residue }
    }

    /// Returns what this explosion cell will become after its final frame.
    pub const fn residue(self) -> ExplosionResidue {
        self.residue
    }

    /// Defers behavior to the finite animation resolved before actor dispatch.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Runtime identity for every original tile and temporary explosion cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Actor {
    /// Unoccupied cell.
    Empty(Empty),
    /// Falling and rolling rock.
    Zonk(Zonk),
    /// Diggable circuit-board material.
    Base(Base),
    /// Player character.
    Murphy(Murphy),
    /// Falling collectible.
    Infotron(Infotron),
    /// Destructible RAM-chip wall.
    RamChip(RamChip),
    /// Indestructible decorative wall.
    Hardware(Hardware),
    /// Locked or open goal.
    Exit(Exit),
    /// Falling explosive disk.
    OrangeDisk(OrangeDisk),
    /// Directional pass-through tile.
    Port(Port),
    /// Scissor-like wall-following enemy.
    SnikSnak(SnikSnak),
    /// Terminal-detonated pushable disk.
    YellowDisk(YellowDisk),
    /// Yellow-Disk detonation terminal.
    Terminal(Terminal),
    /// Collectible and droppable timed explosive.
    RedDisk(RedDisk),
    /// Infotron-producing wall-following enemy.
    Electron(Electron),
    /// Periodically dangerous Base tile.
    Bug(Bug),
    /// Hidden indestructible wall from extended classic level files.
    InvisibleWall(InvisibleWall),
    /// Runtime-only animated blast cell.
    Explosion(Explosion),
}

impl Actor {
    /// Delegates behavior to the concrete struct wrapped by this enum variant.
    pub(crate) fn transition(
        &self,
        state: &State,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        // Freeze pauses both stable and pre-fall Zonks. A transfer already
        // represented by synchronized destination/source animations must
        // finish, or its Vacating source would release out of phase with a
        // permanently paused destination.
        if matches!(self, Self::Zonk(_)) && world.freeze_zonks() && !state.animation.is_movement() {
            return None;
        }

        // The game session owns a planted fuse even while Murphy covers its
        // cell. It updates the State frame after the linear actor pass, preventing the
        // visible actor and concealed timer from advancing independently.
        if state.animation.kind == AnimationKind::RedDiskFuse && world.is_active_red_disk(position)
        {
            return None;
        }

        // Bug state changes happen only on the global four-tick cadence. Murphy
        // has already interacted before this row-major actor callback, which
        // preserves the active→safe and safe→active collision boundaries.
        if matches!(
            state.animation.kind,
            AnimationKind::Bug
                | AnimationKind::BugDormant
                | AnimationKind::Explosion
                | AnimationKind::ElectronExplosion
        ) && !world.tick_count().is_multiple_of(4)
        {
            return None;
        }

        // Animations advance before actor decisions. Their terminal action may
        // settle, explode, or explicitly hand control back to the actor.
        match state.animation.advance() {
            AnimationAdvance::Frame(animation) => {
                return Some(Transition::replace(
                    position,
                    State::animated(self.clone(), animation),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Settle) => {
                return Some(self.settle(position, state, world));
            }
            AnimationAdvance::Finished(AnimationNext::BeginZonkFall) => {
                let Self::Zonk(zonk) = self else {
                    // `Animation::zonk_pre_fall` is the only constructor for
                    // this promise. Recover malformed internal state as idle
                    // instead of allowing an unrelated actor to fall.
                    debug_assert!(false, "only a Zonk may finish pre-fall");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return zonk.begin_fall(position, world);
            }
            AnimationAdvance::Finished(AnimationNext::RandomizeBug) => {
                // The shared session RNG must be consumed at application time
                // so Bugs ending together draw distinct values in row order.
                debug_assert!(matches!(self, Self::Bug(_)));
                return Some(Transition::new(
                    Vec::new(),
                    vec![GameEvent::RandomizeBug(position)],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::ActivateBug) => {
                debug_assert!(matches!(self, Self::Bug(_)));
                return Some(Transition::replace(
                    position,
                    State::animated(Self::Bug(Bug), Animation::bug_active()),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Release) => {
                return Some(Transition::replace(position, State::empty()));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeEmpty) => {
                return Some(Transition::new(
                    vec![CellWrite::new(position, State::empty())],
                    vec![GameEvent::ExplosionFinished],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeInfotron) => {
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::new(Actor::Infotron(Infotron::resting())),
                    )],
                    vec![GameEvent::ExplosionFinished],
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Explode(residue)) => {
                // Only disk fuses use this animation promise. Secondary actor
                // chains are represented by the game's independent timer array.
                return Some(explode_at(
                    world,
                    position,
                    residue == ExplosionResidue::Infotron,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::ResumeMurphy) => {
                // A movement's final pose intentionally survives one complete
                // update after its source reservation is released. Only now
                // may fresh input begin another move, matching the original
                // separation between movement completion and direction input.
                let Self::Murphy(murphy) = self else {
                    // `Animation::murphy_ready` is the sole constructor for
                    // this promise, so a different actor would be an internal
                    // state-construction bug. Recover as a stable actor in
                    // release builds instead of leaving an immortal animation.
                    debug_assert!(false, "only Murphy may resume after movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };

                return murphy.transition(position, world).or_else(|| {
                    // No usable input leaves Murphy genuinely idle. This is
                    // not an extra movement frame: the final moving pose was
                    // already retained for the preceding simulation update.
                    Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
                    ))
                });
            }
            AnimationAdvance::Finished(AnimationNext::Act) | AnimationAdvance::Ready => {}
        }

        match self {
            Self::Empty(actor) => actor.transition(position, world),
            Self::Zonk(actor) => actor.transition(position, world),
            Self::Base(actor) => actor.transition(position, world),
            Self::Murphy(actor) => actor.transition(position, world),
            Self::Infotron(actor) => actor.transition(position, world),
            Self::RamChip(actor) => actor.transition(position, world),
            Self::Hardware(actor) => actor.transition(position, world),
            Self::Exit(actor) => actor.transition(position, world),
            Self::OrangeDisk(actor) => actor.transition(position, world),
            Self::Port(actor) => actor.transition(position, world),
            Self::SnikSnak(actor) => actor.transition(position, world),
            Self::YellowDisk(actor) => actor.transition(position, world),
            Self::Terminal(actor) => actor.transition(position, world),
            Self::RedDisk(actor) => actor.transition(position, world),
            Self::Electron(actor) => actor.transition(position, world),
            Self::Bug(actor) => actor.transition(position, world),
            Self::InvisibleWall(actor) => actor.transition(position, world),
            Self::Explosion(actor) => actor.transition(position, world),
        }
    }

    /// Returns the validated starting/resting animation for this actor type.
    fn default_animation(&self) -> Animation {
        match self {
            Self::SnikSnak(_) => Animation::cycle_then_act(AnimationKind::SnikSnak, 4),
            Self::Electron(_) => Animation::cycle_then_act(AnimationKind::Electron, 8),
            Self::Bug(_) => Animation::bug_active(),
            Self::Terminal(terminal) => Animation::terminal(terminal.screen_frame),
            Self::Explosion(explosion) => Animation::explosion(explosion.residue),
            _ => Animation::idle(),
        }
    }

    /// Resolves movement completion using the latest neighboring cell states.
    fn settle(&self, position: Position, state: &State, world: &WorldView<'_>) -> Transition {
        match self {
            Self::Murphy(_) if state.animation.is_movement() => {
                // Completion keeps the last interpolated pose, but changes its
                // promised action to input resumption. The synchronized source
                // reservation releases in this same tick. Murphy is processed
                // before the later row-major actor pass, so trailing hazards
                // observe the updated reservation rather than stale occupancy.
                let (direction, source_distance) = match state.animation.kind {
                    AnimationKind::Moving(direction) => (direction, 1),
                    AnimationKind::PortTraversal(direction) => (direction, 2),
                    // Murphy never owns Rolling, but retaining a total fallback
                    // makes malformed internal states settle without erasing a
                    // potentially unrelated neighboring cell.
                    _ => {
                        return Transition::replace(
                            position,
                            State::animated(
                                self.clone(),
                                Animation::murphy_ready(state.animation.kind),
                            ),
                        );
                    }
                };
                let mut source = Some(position);
                for _ in 0..source_distance {
                    source = source.and_then(|cell| world.offset(cell, direction.opposite()));
                }

                let mut writes = vec![CellWrite::new(
                    position,
                    State::animated(self.clone(), Animation::murphy_ready(state.animation.kind)),
                )];
                if let Some(source) = source
                    && world.state(source).is_some_and(|source_state| {
                        matches!(source_state.actor(), Actor::Empty(_))
                            && source_state.animation.kind == AnimationKind::Vacating(direction)
                    })
                {
                    // Never erase an explosion or actor that replaced the
                    // reservation while Murphy was in flight.
                    writes.push(CellWrite::new(source, State::empty()));
                }
                return Transition::new(writes, Vec::new());
            }
            Self::Zonk(_) if world.freeze_zonks() && state.animation.is_movement() => {
                // The in-flight transfer reaches its destination, then loses
                // falling momentum without inspecting the next cell. This is
                // where an original in-flight Zonk first observes freeze.
                return Transition::replace(position, State::new(Self::Zonk(Zonk::resting())));
            }
            Self::Zonk(zonk) if zonk.falling => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        match target.actor() {
                            Actor::Murphy(_) => {
                                // Murphy has already taken his player-first
                                // update this tick. Remaining here therefore
                                // means the falling Zonk genuinely crushes him.
                                return explode_at(world, below, false);
                            }
                            Actor::SnikSnak(_) | Actor::Electron(_) => {
                                return explode_at(world, below, false);
                            }
                            Actor::OrangeDisk(_) if target.is_idle() => {
                                // A Zonk arms an otherwise stable Orange Disk
                                // after a short delay while itself comes to rest.
                                let orange = State::animated(
                                    Actor::OrangeDisk(OrangeDisk::resting()),
                                    Animation::orange_disk_fuse(),
                                );
                                return Transition::new(
                                    vec![
                                        CellWrite::new(
                                            position,
                                            State::new(Self::Zonk(Zonk::resting())),
                                        ),
                                        CellWrite::new(below, orange),
                                    ],
                                    Vec::new(),
                                );
                            }
                            _ => {}
                        }
                    }
                    if world.is_empty(below) {
                        // Retained momentum begins the next cell transfer on
                        // this completion callback. Only the first unsupported
                        // resting state uses `ZonkPreFall`; inserting an idle
                        // update here would make a long fall visibly stutter.
                        return Transition::move_actor(
                            position,
                            below,
                            Self::Zonk(Zonk { falling: true }),
                            Direction::Down,
                        );
                    }
                    return Transition::replace(position, State::new(Self::Zonk(Zonk::resting())));
                }
            }
            Self::Infotron(infotron) if infotron.falling => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        if matches!(target.actor(), Actor::Murphy(_)) {
                            // As with a Zonk, sequential player-first mutation
                            // has already decided whether Murphy escaped.
                            return explode_at(world, below, false);
                        }
                        let hits_living_actor =
                            matches!(target.actor(), Actor::SnikSnak(_) | Actor::Electron(_));
                        let hits_idle_disk = target.is_idle()
                            && matches!(
                                target.actor(),
                                Actor::RedDisk(_) | Actor::YellowDisk(_) | Actor::OrangeDisk(_)
                            );
                        let hits_active_red_disk = matches!(target.actor(), Actor::RedDisk(_))
                            && world.is_active_red_disk(below);
                        if hits_living_actor || hits_idle_disk || hits_active_red_disk {
                            return explode_at(world, below, false);
                        }
                    }
                    let still_falling = world.is_empty(below);
                    return Transition::replace(
                        position,
                        State::new(Self::Infotron(Infotron {
                            falling: still_falling,
                        })),
                    );
                }
            }
            Self::OrangeDisk(disk) if disk.falling => {
                if let Some(below) = world.offset(position, Direction::Down)
                    && !world.is_empty(below)
                {
                    return explode_at(world, position, false);
                }
                return Transition::replace(position, State::new(Self::OrangeDisk(*disk)));
            }
            _ => {}
        }

        // Actors without a special landing rule simply retain their persistent
        // fields and return to their type-specific idle or cyclic animation.
        Transition::replace(position, State::new(self.clone()))
    }

    /// Returns the serialized tile code used for this actor's static sprite.
    pub fn tile_code(&self) -> u8 {
        match self {
            Self::Empty(_) => 0,
            Self::Zonk(_) => 1,
            Self::Base(_) => 2,
            Self::Murphy(_) => 3,
            Self::Infotron(_) => 4,
            Self::RamChip(chip) => match chip.shape {
                RamChipShape::Center => 5,
                RamChipShape::Left => 26,
                RamChipShape::Right => 27,
                RamChipShape::Top => 38,
                RamChipShape::Bottom => 39,
            },
            Self::Hardware(hardware) => {
                if hardware.variant == 0 {
                    6
                } else {
                    27 + hardware.variant.min(10)
                }
            }
            Self::Exit(_) => 7,
            Self::OrangeDisk(_) => 8,
            Self::Port(port) => port.tile_code(),
            Self::SnikSnak(_) => 17,
            Self::YellowDisk(_) => 18,
            Self::Terminal(_) => 19,
            Self::RedDisk(_) => 20,
            Self::Electron(_) => 24,
            Self::Bug(_) => 25,
            Self::InvisibleWall(_) => 40,
            Self::Explosion(_) => 0,
        }
    }
}

impl Port {
    /// Maps port permissions and metadata status back to a sprite-compatible code.
    fn tile_code(self) -> u8 {
        match (self.directions, self.special) {
            (PortDirections::OneWay(Direction::Right), false) => 9,
            (PortDirections::OneWay(Direction::Down), false) => 10,
            (PortDirections::OneWay(Direction::Left), false) => 11,
            (PortDirections::OneWay(Direction::Up), false) => 12,
            (PortDirections::OneWay(Direction::Right), true) => 13,
            (PortDirections::OneWay(Direction::Down), true) => 14,
            (PortDirections::OneWay(Direction::Left), true) => 15,
            (PortDirections::OneWay(Direction::Up), true) => 16,
            (PortDirections::Vertical, _) => 21,
            (PortDirections::Horizontal, _) => 22,
            (PortDirections::Any, _) => 23,
        }
    }
}

/// One atomic write included in an actor's immediate board transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CellWrite {
    /// Cell replaced as part of its actor's indivisible transition.
    pub(crate) position: Position,
    /// Complete actor and animation state written to that cell.
    pub(crate) state: State,
}

impl CellWrite {
    /// Creates one write to be committed before the next actor is called.
    pub(crate) fn new(position: Position, state: State) -> Self {
        Self { position, state }
    }
}

/// Gameplay side effect emitted after its transition is applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameEvent {
    /// Decrease the number of Infotrons still required.
    CollectInfotron,
    /// Add one Red Disk to Murphy's inventory.
    CollectRedDisk,
    /// Spend one disk and begin its one-at-a-time fuse under Murphy's cell.
    PlantRedDisk(Position),
    /// Mark the current level as successfully completed.
    Completed,
    /// Mark Murphy as destroyed.
    Died,
    /// Replace global toggles with a special port's metadata.
    ApplySpecialPort(SpecialPort),
    /// Detonate all idle Yellow Disks currently present on the board.
    ActivateTerminal,
    /// Consume the shared RNG stream and schedule one Bug's safe interval.
    RandomizeBug(Position),
    /// Consume the shared RNG stream and schedule one Terminal screen scroll.
    RandomizeTerminal(Position),
    /// Start an independent signed thirteen-tick secondary explosion timer.
    ScheduleExplosion {
        /// Center whose delayed wave will be emitted when the timer reaches zero.
        position: Position,
        /// Whether the delayed wave uses Electron graphics and Infotron residue.
        electron: bool,
    },
    /// Mark the global explosion effect active for deterministic RNG consumption.
    ExplosionStarted,
    /// Clear the original global explosion flag when one visual cell completes.
    ExplosionFinished,
}

/// Atomic multi-cell change applied immediately during the linear update pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Transition {
    /// Cell replacements committed together before the next actor is updated.
    pub(crate) writes: Vec<CellWrite>,
    /// Side effects applied after all cell replacements in this transition.
    pub(crate) events: Vec<GameEvent>,
}

impl Transition {
    /// Creates one fully specified immediate board transition.
    fn new(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self { writes, events }
    }

    /// Creates an explosion transition for immediate sequential application.
    fn blast(writes: Vec<CellWrite>, events: Vec<GameEvent>) -> Self {
        Self::new(writes, events)
    }

    /// Replaces only the currently updating actor's cell.
    fn replace(position: Position, state: State) -> Self {
        Self::new(vec![CellWrite::new(position, state)], Vec::new())
    }

    /// Moves an actor atomically without producing a gameplay event.
    fn move_actor(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        Self::move_actor_with_events(source, destination, actor, direction, Vec::new())
    }

    /// Moves an actor atomically and emits side effects after its cell writes.
    fn move_actor_with_events(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        events: Vec<GameEvent>,
    ) -> Self {
        let destination_state = State::animated(actor, Animation::moving(direction));
        Self::new(
            vec![
                CellWrite::new(source, State::vacating(direction)),
                CellWrite::new(destination, destination_state),
            ],
            events,
        )
    }

    /// Moves a rounded actor diagonally while reserving its final cell at once.
    fn roll_actor(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        let destination_state = State::animated(actor, Animation::rolling(direction));
        Self::new(
            vec![
                CellWrite::new(source, State::vacating(direction)),
                CellWrite::new(destination, destination_state),
            ],
            Vec::new(),
        )
    }
}

/// Finds an orthogonally adjacent Murphy on the current live board.
fn adjacent_murphy(position: Position, world: &WorldView<'_>) -> Option<Position> {
    for direction in Direction::ALL {
        if let Some(neighbor) = world.offset(position, direction)
            && matches!(world.state(neighbor)?.actor(), Actor::Murphy(_))
        {
            return Some(neighbor);
        }
    }

    None
}

/// Creates one visual explosion cell independently of all secondary-wave timers.
fn explosion_state(residue: ExplosionResidue) -> State {
    let actor = Actor::Explosion(Explosion::new(residue));
    State::animated(actor, Animation::explosion(residue))
}

/// Builds one immediate 3×3 wave and schedules touched reactive actors.
pub(crate) fn explode_at(world: &WorldView<'_>, center: Position, electron: bool) -> Transition {
    // A live Electron always seeds an Electron wave even when the caller only
    // knows that a generic falling object or player collision caused the blast.
    let electron_wave = electron
        || world
            .state(center)
            .is_some_and(|state| matches!(state.actor(), Actor::Electron(_)));
    explode_wave(world, center, electron_wave)
}

/// Implements one bounded, immediately visible wave in original write order.
fn explode_wave(world: &WorldView<'_>, center: Position, electron_wave: bool) -> Transition {
    let incoming_residue = if electron_wave {
        ExplosionResidue::Infotron
    } else {
        ExplosionResidue::Empty
    };
    let mut writes = Vec::new();
    // The original engine uses one global flag for explosion sound and camera
    // shake rather than counting live cells.  Every emitted wave sets it again.
    let mut events = vec![GameEvent::ExplosionStarted];

    // Signed offsets make edge clipping explicit. Both visible and invisible
    // Hardware are skipped so their indestructibility survives every wave.
    for delta_y in -1_isize..=1 {
        for delta_x in -1_isize..=1 {
            let Some(position) = world.offset_xy(center, delta_x, delta_y) else {
                continue;
            };
            let Some(state) = world.state(position) else {
                continue;
            };
            if matches!(state.actor(), Actor::Hardware(_) | Actor::InvisibleWall(_)) {
                continue;
            }

            let is_murphy = matches!(state.actor(), Actor::Murphy(_));
            if is_murphy && !events.contains(&GameEvent::Died) {
                events.push(GameEvent::Died);
            }

            // Only actors touched outside the seed receive a delayed secondary
            // wave.  Electron timers are always negative; the other reactive
            // actors inherit the sign of the wave that reached them.
            if position != center {
                let delayed_electron = match state.actor() {
                    Actor::Electron(_) => Some(true),
                    Actor::OrangeDisk(_)
                    | Actor::YellowDisk(_)
                    | Actor::SnikSnak(_)
                    | Actor::Murphy(_) => Some(electron_wave),
                    _ => None,
                };
                if let Some(electron) = delayed_electron {
                    events.push(GameEvent::ScheduleExplosion { position, electron });
                }
            }

            // A directly touched Electron uses Electron graphics immediately;
            // every other cell uses the current wave's ordinary residue.  Later
            // waves overwrite the visual state instead of merging strengths.
            let residue = if matches!(state.actor(), Actor::Electron(_)) {
                ExplosionResidue::Infotron
            } else {
                incoming_residue
            };
            writes.push(CellWrite::new(position, explosion_state(residue)));
        }
    }

    Transition::blast(writes, events)
}
