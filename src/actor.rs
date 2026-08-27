//! Actor-owned behavior, animation phases, and atomic transition proposals.
//!
//! Every level cell contains one [`State`].  An [`Actor`] is an enum whose
//! variants wrap actor-specific structs, while [`Animation`] records the visual
//! phase and the semantic transition that follows its final frame.

use crate::{game::WorldView, level::SpecialPort};

/// Number of simulation frames used for one cell-to-cell movement.
const MOVEMENT_FRAMES: u8 = 4;

/// Number of source frames in either explosion strip of `RocksSP.png`.
const EXPLOSION_FRAMES: u8 = 8;

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
    /// An actor entering its current cell from the opposite direction.
    Moving(Direction),
    /// Murphy acting on an adjacent cell without changing cells.
    Snapping(Direction),
    /// The cyclic spark frames of a Bug hidden inside Base.
    Bug,
    /// The cyclic movement frames of a Snik Snak.
    SnikSnak,
    /// The cyclic spark frames of an Electron.
    Electron,
    /// The cyclic screen frames of a Terminal.
    Terminal,
    /// A Red Disk counting down before it explodes.
    RedDiskFuse,
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
    /// Restart the same cyclic animation from frame zero.
    Repeat,
    /// Replace the animated cell with empty space.
    BecomeEmpty,
    /// Replace the animated cell with a stationary Infotron.
    BecomeInfotron,
    /// Replace the armed disk and its neighborhood with an explosion.
    Explode,
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

    /// Creates interpolation frames for an actor already in its destination.
    fn moving(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Moving(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::Settle,
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

    /// Creates a purely visual cycle that starts again after its last frame.
    fn repeating(kind: AnimationKind, frame_count: u8) -> Self {
        Self {
            kind,
            frame: 0,
            frame_count,
            next: AnimationNext::Repeat,
        }
    }

    /// Creates the finite fuse placed on a dropped Red Disk.
    fn red_disk_fuse() -> Self {
        Self {
            kind: AnimationKind::RedDiskFuse,
            frame: 0,
            frame_count: 24,
            next: AnimationNext::Explode,
        }
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
        if self.kind == AnimationKind::Idle {
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

    /// Rewinds a cyclic animation while preserving its validated metadata.
    fn restarted(&self) -> Self {
        let mut animation = self.clone();
        animation.frame = 0;
        animation
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

    /// Reports whether this state is unoccupied for collision purposes.
    pub fn is_empty(&self) -> bool {
        matches!(self.actor, Actor::Empty(_))
    }

    /// Reports whether the actor is in its stable, non-moving phase.
    pub fn is_idle(&self) -> bool {
        self.animation.kind == AnimationKind::Idle
    }
}

/// Empty space, which never proposes a transition by itself.
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
            let actor = Actor::Zonk(Self { falling: true });
            return Some(Transition::move_actor(
                position,
                below,
                actor,
                Direction::Down,
                TransitionPriority::Physics,
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
                return Some(Transition::move_actor(
                    position,
                    diagonal,
                    Actor::Zonk(Self { falling: false }),
                    direction,
                    TransitionPriority::Physics,
                ));
            }
        }

        None
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
        // Dropping a collected Red Disk is a distinct action so it cannot be
        // confused with snapping an adjacent collectible.
        if world.input().drop_disk
            && world.red_disks() > 0
            && let Some(target) = world.offset(position, self.facing)
            && world.is_empty(target)
        {
            let murphy = State::animated(Actor::Murphy(*self), Animation::snapping(self.facing));
            let disk = State::animated(Actor::RedDisk(RedDisk), Animation::red_disk_fuse());
            return Some(Transition::new(
                position,
                TransitionPriority::Player,
                vec![
                    CellWrite::new(position, murphy),
                    CellWrite::new(target, disk),
                ],
                vec![GameEvent::SpendRedDisk],
            ));
        }

        if let Some(direction) = world.input().direction {
            if world.input().action {
                return self.snap(position, direction, world);
            }
            return self.move_or_interact(position, direction, world);
        }

        // Gravity is evaluated only when no directional command was supplied.
        // This preserves responsive horizontal movement while gravity is active.
        if world.gravity() {
            let below = world.offset(position, Direction::Down)?;
            if world.is_empty(below) {
                return Some(Transition::move_actor(
                    position,
                    below,
                    Actor::Murphy(Self {
                        facing: Direction::Down,
                    }),
                    Direction::Down,
                    TransitionPriority::Player,
                ));
            }
        }

        None
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
            Actor::RedDisk(_) => events.push(GameEvent::CollectRedDisk),
            _ => return None,
        }

        // The source animation and target removal share one proposal so an
        // overlapping explosion cannot accept only half of the action.
        let actor = Actor::Murphy(Self { facing: direction });
        let state = State::animated(actor, Animation::snapping(direction));
        Some(Transition::new(
            position,
            TransitionPriority::Player,
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
            Actor::Empty(_) | Actor::Base(_) => Some(Transition::move_actor(
                position,
                target,
                murphy_actor,
                direction,
                TransitionPriority::Player,
            )),
            Actor::Bug(_) if world.is_bug_active(target) => {
                Some(explode_at(world, position, false))
            }
            Actor::Bug(_) => Some(Transition::move_actor(
                position,
                target,
                murphy_actor,
                direction,
                TransitionPriority::Player,
            )),
            Actor::Infotron(_) => Some(Transition::move_actor_with_events(
                position,
                target,
                murphy_actor,
                direction,
                TransitionPriority::Player,
                vec![GameEvent::CollectInfotron],
            )),
            Actor::RedDisk(_) => Some(Transition::move_actor_with_events(
                position,
                target,
                murphy_actor,
                direction,
                TransitionPriority::Player,
                vec![GameEvent::CollectRedDisk],
            )),
            Actor::Exit(_) if world.remaining_infotrons() == 0 => Some(Transition::new(
                position,
                TransitionPriority::Player,
                vec![CellWrite::new(
                    position,
                    State::animated(murphy_actor, Animation::snapping(direction)),
                )],
                vec![GameEvent::Completed],
            )),
            Actor::Zonk(_) if direction.is_horizontal() && target_state.is_idle() => {
                self.push(position, target, direction, murphy_actor, world, true)
            }
            Actor::YellowDisk(_) if target_state.is_idle() => {
                self.push(position, target, direction, murphy_actor, world, false)
            }
            Actor::Port(port) if port.allows(direction) => {
                self.cross_port(position, target, direction, murphy_actor, *port, world)
            }
            Actor::Terminal(_) => Some(Transition::new(
                position,
                TransitionPriority::Player,
                vec![CellWrite::new(
                    position,
                    State::animated(murphy_actor, Animation::snapping(direction)),
                )],
                vec![GameEvent::ActivateTerminal],
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
        zonk: bool,
    ) -> Option<Transition> {
        let destination = world.offset(target, direction)?;
        if !world.is_empty(destination) {
            return None;
        }

        let pushed_actor = if zonk {
            Actor::Zonk(Zonk::resting())
        } else {
            Actor::YellowDisk(YellowDisk)
        };
        let writes = vec![
            CellWrite::new(position, State::empty()),
            CellWrite::new(
                target,
                State::animated(murphy_actor, Animation::moving(direction)),
            ),
            CellWrite::new(
                destination,
                State::animated(pushed_actor, Animation::moving(direction)),
            ),
        ];

        Some(Transition::new(
            position,
            TransitionPriority::Player,
            writes,
            Vec::new(),
        ))
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
            position,
            TransitionPriority::Player,
            vec![
                CellWrite::new(position, State::empty()),
                CellWrite::new(
                    destination,
                    State::animated(murphy_actor, Animation::moving(direction)),
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
                TransitionPriority::Physics,
            ));
        }

        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Direction::Left, Direction::Right] {
            let side = world.offset(position, direction)?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::move_actor(
                    position,
                    diagonal,
                    Actor::Infotron(Self { falling: false }),
                    direction,
                    TransitionPriority::Physics,
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
                TransitionPriority::Physics,
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

    /// Remains stationary because traversal is proposed by Murphy atomically.
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
                TransitionPriority::Animation,
            ));
        }

        if let Some(murphy) = adjacent_murphy(position, world) {
            return Some(explode_at(world, murphy, false));
        }

        // Snik Snaks keep their left side near a wall. All candidates are read
        // from the same immutable snapshot, so competing enemies cannot overlap.
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
                    TransitionPriority::Physics,
                ));
            }
        }

        Some(Transition::replace(
            position,
            State::new(Actor::SnikSnak(*self)),
            TransitionPriority::Animation,
        ))
    }
}

/// Pushable disk detonated simultaneously by any activated Terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YellowDisk;

impl YellowDisk {
    /// Remains stationary until Murphy pushes it or a Terminal detonates it.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
    }
}

/// Computer terminal whose interaction detonates every Yellow Disk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Terminal;

impl Terminal {
    /// Has no physical behavior; its repeating animation is resolved centrally.
    fn transition(&self, _position: Position, _world: &WorldView<'_>) -> Option<Transition> {
        None
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
                TransitionPriority::Animation,
            ));
        }

        if let Some(murphy) = adjacent_murphy(position, world) {
            return Some(explode_at(world, murphy, true));
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
                    TransitionPriority::Physics,
                ));
            }
        }

        Some(Transition::replace(
            position,
            State::new(Actor::Electron(*self)),
            TransitionPriority::Animation,
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
        // Animations advance before actor decisions. Their terminal action may
        // settle, repeat, explode, or explicitly hand control back to the actor.
        match state.animation.advance() {
            AnimationAdvance::Frame(animation) => {
                return Some(Transition::replace(
                    position,
                    State::animated(self.clone(), animation),
                    TransitionPriority::Animation,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Settle) => {
                return Some(self.settle(position, state, world));
            }
            AnimationAdvance::Finished(AnimationNext::Repeat) => {
                return Some(Transition::replace(
                    position,
                    State::animated(self.clone(), state.animation.restarted()),
                    TransitionPriority::Animation,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeEmpty) => {
                return Some(Transition::replace(
                    position,
                    State::empty(),
                    TransitionPriority::Explosion,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BecomeInfotron) => {
                return Some(Transition::replace(
                    position,
                    State::new(Actor::Infotron(Infotron::resting())),
                    TransitionPriority::Explosion,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::Explode) => {
                return Some(explode_at(world, position, false));
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
            Self::Bug(_) => Animation::repeating(AnimationKind::Bug, 8),
            Self::Terminal(_) => Animation::repeating(AnimationKind::Terminal, 7),
            Self::Explosion(explosion) => Animation::explosion(explosion.residue),
            _ => Animation::idle(),
        }
    }

    /// Resolves movement completion using the latest neighboring cell states.
    fn settle(&self, position: Position, state: &State, world: &WorldView<'_>) -> Transition {
        match self {
            Self::Zonk(zonk) if zonk.falling => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if world.is_crushable(below) {
                        return explode_at(world, below, false);
                    }
                    let still_falling = world.is_empty(below);
                    return Transition::replace(
                        position,
                        State::new(Self::Zonk(Zonk {
                            falling: still_falling,
                        })),
                        TransitionPriority::Physics,
                    );
                }
            }
            Self::Infotron(infotron) if infotron.falling => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if world.is_crushable(below) {
                        return explode_at(world, below, false);
                    }
                    let still_falling = world.is_empty(below);
                    return Transition::replace(
                        position,
                        State::new(Self::Infotron(Infotron {
                            falling: still_falling,
                        })),
                        TransitionPriority::Physics,
                    );
                }
            }
            Self::OrangeDisk(disk) if disk.falling => {
                if let Some(below) = world.offset(position, Direction::Down)
                    && !world.is_empty(below)
                {
                    return explode_at(world, position, false);
                }
                return Transition::replace(
                    position,
                    State::new(Self::OrangeDisk(*disk)),
                    TransitionPriority::Physics,
                );
            }
            _ => {}
        }

        // Actors without a special landing rule simply retain their persistent
        // fields and return to their type-specific idle or cyclic animation.
        Transition::replace(
            position,
            State::new(self.clone()),
            if matches!(state.animation.kind, AnimationKind::Moving(_)) {
                TransitionPriority::Physics
            } else {
                TransitionPriority::Animation
            },
        )
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

/// One atomic write included in a proposed board transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CellWrite {
    /// Cell replaced when this proposal is accepted.
    pub(crate) position: Position,
    /// Complete actor and animation state written to that cell.
    pub(crate) state: State,
}

impl CellWrite {
    /// Creates a write whose position participates in conflict arbitration.
    fn new(position: Position, state: State) -> Self {
        Self { position, state }
    }
}

/// Gameplay side effect emitted only when its transition is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameEvent {
    /// Decrease the number of Infotrons still required.
    CollectInfotron,
    /// Add one Red Disk to Murphy's inventory.
    CollectRedDisk,
    /// Remove one Red Disk after placing its timed instance.
    SpendRedDisk,
    /// Mark the current level as successfully completed.
    Completed,
    /// Mark Murphy as destroyed.
    Died,
    /// Replace global toggles with a special port's metadata.
    ApplySpecialPort(SpecialPort),
    /// Detonate all Yellow Disks currently present on the board.
    ActivateTerminal,
}

/// Conflict priority for simultaneous proposals from one immutable snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum TransitionPriority {
    /// Pure frame advancement, which yields to every physical action.
    Animation,
    /// Falling, rolling, and enemy movement.
    Physics,
    /// Direct player intent, which wins ordinary destination races.
    Player,
    /// Explosion writes, which override every lower-priority action.
    Explosion,
}

/// Atomic multi-cell change proposed by one actor for the next board snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Transition {
    /// Actor location used to break equal-priority ties deterministically.
    pub(crate) source: Position,
    /// Arbitration priority of every write and event in this proposal.
    pub(crate) priority: TransitionPriority,
    /// Cell replacements accepted or rejected as a single unit.
    pub(crate) writes: Vec<CellWrite>,
    /// Side effects applied only if all write locations were claimable.
    pub(crate) events: Vec<GameEvent>,
}

impl Transition {
    /// Creates a fully specified atomic proposal.
    fn new(
        source: Position,
        priority: TransitionPriority,
        writes: Vec<CellWrite>,
        events: Vec<GameEvent>,
    ) -> Self {
        Self {
            source,
            priority,
            writes,
            events,
        }
    }

    /// Replaces only the proposing actor's current cell.
    fn replace(position: Position, state: State, priority: TransitionPriority) -> Self {
        Self::new(
            position,
            priority,
            vec![CellWrite::new(position, state)],
            Vec::new(),
        )
    }

    /// Moves an actor atomically without producing a gameplay event.
    fn move_actor(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        priority: TransitionPriority,
    ) -> Self {
        Self::move_actor_with_events(source, destination, actor, direction, priority, Vec::new())
    }

    /// Moves an actor atomically and emits side effects after acceptance.
    fn move_actor_with_events(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        priority: TransitionPriority,
        events: Vec<GameEvent>,
    ) -> Self {
        let destination_state = State::animated(actor, Animation::moving(direction));
        Self::new(
            source,
            priority,
            vec![
                CellWrite::new(source, State::empty()),
                CellWrite::new(destination, destination_state),
            ],
            events,
        )
    }
}

/// Finds an orthogonally adjacent Murphy from the current immutable snapshot.
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

/// Builds a priority explosion proposal over the in-bounds 3×3 neighborhood.
pub(crate) fn explode_at(world: &WorldView<'_>, center: Position, electron: bool) -> Transition {
    let mut blast_centers = vec![(center, electron)];
    let mut cursor = 0;

    // Discover every Electron touched by the growing blast. Each discovered
    // Electron contributes its own Infotron-producing 3x3 neighborhood, which
    // can discover further Electrons without recursive stack growth.
    while cursor < blast_centers.len() {
        let (blast_center, _) = blast_centers[cursor];
        cursor += 1;
        for delta_y in -1_isize..=1 {
            for delta_x in -1_isize..=1 {
                let Some(position) = world.offset_xy(blast_center, delta_x, delta_y) else {
                    continue;
                };
                let is_electron = world
                    .state(position)
                    .is_some_and(|state| matches!(state.actor(), Actor::Electron(_)));
                if is_electron
                    && !blast_centers
                        .iter()
                        .any(|(existing, _)| *existing == position)
                {
                    blast_centers.push((position, true));
                }
            }
        }
    }

    let mut affected: Vec<(Position, ExplosionResidue)> = Vec::new();
    let mut events = Vec::new();

    for (blast_center, electron_blast) in blast_centers {
        // Signed offsets make edge clipping explicit. Both visible and invisible
        // Hardware are skipped so their indestructibility survives every chain.
        for delta_y in -1_isize..=1 {
            for delta_x in -1_isize..=1 {
                let Some(position) = world.offset_xy(blast_center, delta_x, delta_y) else {
                    continue;
                };
                let Some(state) = world.state(position) else {
                    continue;
                };
                if matches!(state.actor(), Actor::Hardware(_) | Actor::InvisibleWall(_)) {
                    continue;
                }

                if matches!(state.actor(), Actor::Murphy(_)) && !events.contains(&GameEvent::Died) {
                    events.push(GameEvent::Died);
                }

                let residue = if electron_blast {
                    ExplosionResidue::Infotron
                } else {
                    ExplosionResidue::Empty
                };
                if let Some((_, existing_residue)) = affected
                    .iter_mut()
                    .find(|(existing, _)| *existing == position)
                {
                    // Infotron residue wins where a normal and Electron blast
                    // overlap because the Electron blast is the stronger result.
                    if residue == ExplosionResidue::Infotron {
                        *existing_residue = residue;
                    }
                } else {
                    affected.push((position, residue));
                }
            }
        }
    }

    let writes = affected
        .into_iter()
        .map(|(position, residue)| {
            let explosion = Actor::Explosion(Explosion::new(residue));
            CellWrite::new(
                position,
                State::animated(explosion, Animation::explosion(residue)),
            )
        })
        .collect();

    Transition::new(center, TransitionPriority::Explosion, writes, events)
}
