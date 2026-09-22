//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every level cell contains one [`State`].  An [`Actor`] is an enum whose
//! variants wrap actor-specific structs, while [`Animation`] records the visual
//! phase and the semantic transition that follows its final frame.

pub mod base;
pub mod bug;
pub mod electron;
pub mod empty;
pub mod exit;
pub mod explosion;
pub mod hardware;
pub mod infotron;
pub mod invisible_wall;
pub mod murphy;
pub mod orange_disk;
pub mod port;
pub mod ram_chip;
pub mod red_disk;
pub mod snik_snak;
pub mod terminal;
pub mod yellow_disk;
pub mod zonk;

pub use base::Base;
pub use bug::Bug;
pub use electron::Electron;
pub use empty::Empty;
pub use exit::Exit;
pub use explosion::{Explosion, ExplosionResidue};
pub use hardware::Hardware;
pub use infotron::Infotron;
pub use invisible_wall::InvisibleWall;
pub use murphy::{Murphy, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget, MurphySnapTarget};
pub use orange_disk::OrangeDisk;
pub use port::{Port, PortDirections};
pub use ram_chip::{RamChip, RamChipShape};
pub use red_disk::RedDisk;
pub use snik_snak::SnikSnak;
pub use terminal::Terminal;
pub use yellow_disk::YellowDisk;
pub use zonk::Zonk;

pub(crate) use explosion::explode_at;
use murphy::{
    actor_for_push_target, murphy_is_crossing_port, murphy_is_protected_from_falling_actor,
    pushed_actor_matches,
};

use crate::{
    game::{SoundEffect, WorldView},
    level::SpecialPort,
};

/// Number of original updates used by a falling or enemy cell transfer.
const MOVEMENT_FRAMES: u8 = 8;

/// Number of source frames in either original `MOVING.DAT` explosion strip.
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

/// Rotation family used by the original eight-state enemy turn cycles.
///
/// Snik Snaks and Electrons do not choose a new direction in one update. Their
/// state byte advances around one of these cycles on global quarter ticks, and
/// only even-numbered frames test the direction represented by that picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnemyTurn {
    /// Counter-clockwise cycle whose candidates are Up, Left, Down, and Right.
    Left,
    /// Clockwise cycle whose candidates are Up, Right, Down, and Left.
    Right,
}

impl EnemyTurn {
    /// Returns the direction tested by an even turn frame.
    const fn direction_at_frame(self, frame: u8) -> Option<Direction> {
        // Odd frames are visual intermediates and deliberately perform no
        // collision test on the original frame-counter phase.
        match (self, frame & 7) {
            (Self::Left, 0) | (Self::Right, 0) => Some(Direction::Up),
            (Self::Left, 2) | (Self::Right, 6) => Some(Direction::Left),
            (Self::Left, 4) | (Self::Right, 4) => Some(Direction::Down),
            (Self::Left, 6) | (Self::Right, 2) => Some(Direction::Right),
            _ => None,
        }
    }

    /// Finds the even frame at which this cycle tests `direction`.
    const fn candidate_frame(self, direction: Direction) -> u8 {
        match (self, direction) {
            (Self::Left, Direction::Up) | (Self::Right, Direction::Up) => 0,
            (Self::Left, Direction::Left) | (Self::Right, Direction::Right) => 2,
            (Self::Left, Direction::Down) | (Self::Right, Direction::Down) => 4,
            (Self::Left, Direction::Right) | (Self::Right, Direction::Left) => 6,
        }
    }

    /// Selects the serialized starting frame for an enemy facing `heading`.
    const fn initial_frame(self, heading: Direction) -> u8 {
        // A new enemy begins on the candidate immediately to its preferred
        // side. In particular, a right-facing state-zero enemy tests Up.
        let first_candidate = match self {
            Self::Left => heading.left(),
            Self::Right => heading.right(),
        };
        self.candidate_frame(first_candidate)
    }

    /// Returns the intermediate frame just before `direction` is tested.
    const fn preceding_frame(self, direction: Direction) -> u8 {
        // Wrapping seven positions backwards converts candidate frame zero to
        // frame seven while every other even candidate becomes its prior odd
        // animation frame.
        self.candidate_frame(direction).wrapping_add(7) & 7
    }
}

/// Visual family used to select a frame from the original fixed or moving data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationKind {
    /// A stationary actor rendered with its normal tile sprite.
    Idle,
    /// A Zonk armed to begin falling once its destination is still free.
    ZonkPreFall,
    /// An Infotron armed to begin falling once its destination remains free.
    InfotronPreFall,
    /// A rounded Zonk or Infotron spending its two-update side-roll delay.
    RoundedPreRoll(Direction),
    /// A non-rendered source cell reserved while its actor leaves that cell.
    Vacating(Direction),
    /// An actor entering its current cell from the opposite direction.
    Moving(Direction),
    /// A rounded actor sliding horizontally before its vertical drop begins.
    Rolling(Direction),
    /// Empty side cell reserved during a rounded actor's pre-roll delay.
    RoundedSide,
    /// Empty diagonal cell reserved during a rounded actor's horizontal slide.
    RoundedDestination,
    /// An Orange Disk waiting two updates before its first falling frame.
    OrangePreFall,
    /// An Orange Disk visually falling while logically retained at its source.
    OrangeFalling,
    /// A direction-, target-, and facing-specific original Murphy sequence.
    Murphy(MurphyAnimation),
    /// Pushable actor temporarily locked while Murphy prepares or pushes it.
    MurphyPushTarget,
    /// Empty destination reserved for a Murphy push or port traversal.
    MurphyDestination,
    /// A Bug's fourteen-frame lethal spark cycle.
    Bug,
    /// A safe Bug waiting an independently randomized number of quarter ticks.
    BugDormant,
    /// One of the original eight-frame Snik Snak turn cycles.
    SnikSnakTurn(EnemyTurn),
    /// An eight-update Snik Snak transfer in one cardinal direction.
    SnikSnakMove(Direction),
    /// Stable source reservation retained until the moving Snik Snak releases it.
    SnikSnakVacating(Direction),
    /// One of the original eight-frame Electron turn cycles.
    ElectronTurn(EnemyTurn),
    /// An eight-update Electron transfer in one cardinal direction.
    ElectronMove(Direction),
    /// Stable source reservation retained until the moving Electron releases it.
    ElectronVacating(Direction),
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
    /// Transfer a pre-fall Infotron only if the cell below remains unoccupied.
    BeginInfotronFall,
    /// Transfer a rounded actor sideways when its two-update delay completes.
    BeginRoundedSlide {
        /// Side selected by the original left-first roll preference.
        direction: Direction,
    },
    /// Transfer a completed horizontal slide into its diagonal falling cell.
    BeginRoundedFall {
        /// Side used to identify the reserved cell directly below the actor.
        direction: Direction,
    },
    /// Start Orange Disk falling after its two-update preparation delay.
    BeginOrangeFall,
    /// Move an Orange Disk into its destination and resolve the landing below it.
    FinishOrangeFall,
    /// Give completed movement back to Murphy, settling if there is no input.
    ResumeMurphy,
    /// Remove an adjacent target and apply collection effects after snapping.
    FinishMurphySnap {
        /// Direction from Murphy to the reserved target cell.
        direction: Direction,
        /// Target identity used to choose the completion side effect.
        target: MurphySnapTarget,
    },
    /// Transfer Murphy and one reserved pushable actor after its last frame.
    FinishMurphyPush {
        /// Direction of both cell transfers.
        direction: Direction,
        /// Persistent actor identity written into the reserved destination.
        target: MurphyPushTarget,
    },
    /// Transfer Murphy two cells and apply special-port settings at completion.
    FinishMurphyPort {
        /// Direction from the current cell through the intervening port.
        direction: Direction,
    },
    /// Enter the post-completion sequence after the Exit animation finishes.
    FinishMurphyExit,
    /// Ask the game session to choose this Bug's next dormant duration.
    RandomizeBug,
    /// Return a dormant Bug to lethal active frame zero.
    ActivateBug,
    /// Resolve the original left/forward/right choices after a Snik Snak transfer.
    FinishSnikSnakMove {
        /// Direction of the transfer that has just reached its final frame.
        direction: Direction,
    },
    /// Resolve the original left/forward/right choices after an Electron transfer.
    FinishElectronMove {
        /// Direction of the transfer that has just reached its final frame.
        direction: Direction,
    },
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

    /// Creates the matching one-callback arming phase for a resting Infotron.
    fn infotron_pre_fall() -> Self {
        Self {
            kind: AnimationKind::InfotronPreFall,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginInfotronFall,
        }
    }

    /// Creates the two-update delay before a rounded actor leaves its support.
    fn rounded_pre_roll(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::RoundedPreRoll(direction),
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginRoundedSlide { direction },
        }
    }

    /// Retains the final pre-roll state while a diagonal destination is blocked.
    fn rounded_wait(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::RoundedPreRoll(direction),
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginRoundedSlide { direction },
        }
    }

    /// Creates a synchronized, invisible reservation for a movement source.
    fn vacating(direction: Direction) -> Self {
        Self::vacating_for(direction, MOVEMENT_FRAMES)
    }

    /// Creates a source reservation synchronized to a caller-owned duration.
    fn vacating_for(direction: Direction, frame_count: u8) -> Self {
        // Movement destinations own source cleanup. The retained count records
        // the action contract for diagnostics but is not advanced independently;
        // original temporary Space markers have no dispatcher callback.
        Self {
            kind: AnimationKind::Vacating(direction),
            frame: 0,
            frame_count: frame_count.max(1),
            next: AnimationNext::Act,
        }
    }

    /// Creates interpolation frames for an actor already in its destination.
    fn moving(direction: Direction) -> Self {
        Self::moving_at(direction, 0)
    }

    /// Restores one validated frame within a generic actor transfer.
    fn moving_at(direction: Direction, frame: u8) -> Self {
        // Falling actors use this constructor when advancing to frame six must
        // also clear their original source cell in one atomic transition.
        Self {
            kind: AnimationKind::Moving(direction),
            frame: frame.min(MOVEMENT_FRAMES - 1),
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
            next: AnimationNext::BeginRoundedFall { direction },
        }
    }

    /// Creates stable occupancy for a side cell reserved by pre-roll.
    fn rounded_side() -> Self {
        Self {
            kind: AnimationKind::RoundedSide,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates stable occupancy for a diagonal rolling destination.
    fn rounded_destination() -> Self {
        Self {
            kind: AnimationKind::RoundedDestination,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates the original two-update arming delay for an Orange Disk fall.
    fn orange_pre_fall() -> Self {
        Self {
            kind: AnimationKind::OrangePreFall,
            frame: 0,
            frame_count: 2,
            next: AnimationNext::BeginOrangeFall,
        }
    }

    /// Creates eight visible falling frames retained at the Orange source cell.
    fn orange_falling() -> Self {
        Self {
            kind: AnimationKind::OrangeFalling,
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::FinishOrangeFall,
        }
    }

    /// Holds a completed movement's final pose until Murphy's next update.
    fn murphy_ready(kind: AnimationKind) -> Self {
        debug_assert!(
            matches!(kind, AnimationKind::Murphy(animation) if animation.changes_cell()),
            "only Murphy movement phases can become input-ready"
        );
        let AnimationKind::Murphy(animation) = kind else {
            unreachable!("the debug assertion validates the Murphy animation kind")
        };
        Self {
            kind,
            frame: animation.frame_count() - 1,
            frame_count: animation.frame_count(),
            next: AnimationNext::ResumeMurphy,
        }
    }

    /// Creates a target-specific Murphy movement using its original duration.
    fn murphy_move(direction: Direction, target: MurphyMoveTarget, looking_left: bool) -> Self {
        let action = MurphyAnimation::Move {
            direction,
            target,
            looking_left,
        };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::Settle,
        }
    }

    /// Creates a target-specific stationary Murphy action and completion rule.
    fn murphy_snap(direction: Direction, target: MurphySnapTarget) -> Self {
        let action = MurphyAnimation::Snap { direction, target };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphySnap { direction, target },
        }
    }

    /// Creates a push animation after the eight-tick hold requirement succeeds.
    fn murphy_push(direction: Direction, target: MurphyPushTarget) -> Self {
        let action = MurphyAnimation::Push { direction, target };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyPush { direction, target },
        }
    }

    /// Creates the paired eight-frame passage through one port cell.
    fn murphy_port(direction: Direction) -> Self {
        let action = MurphyAnimation::Port { direction };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyPort { direction },
        }
    }

    /// Creates the original forty-frame Exit disappearance at Murphy's source.
    fn murphy_exit() -> Self {
        let action = MurphyAnimation::Exit;
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyExit,
        }
    }

    /// Creates one hold-sensitive Red Disk placement pose at `elapsed` ticks.
    fn murphy_plant(elapsed: u8) -> Self {
        let action = MurphyAnimation::PlantRedDisk;
        Self {
            kind: AnimationKind::Murphy(action),
            frame: elapsed.min(action.frame_count() - 1),
            frame_count: action.frame_count(),
            // Murphy intercepts this phase before generic advancement. Settle
            // is a safe recovery promise for malformed internal states.
            next: AnimationNext::Settle,
        }
    }

    /// Creates a stable reservation that prevents a push target from updating.
    fn murphy_push_target() -> Self {
        Self {
            kind: AnimationKind::MurphyPushTarget,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates stable collision occupancy for an otherwise empty destination.
    fn murphy_destination() -> Self {
        Self {
            kind: AnimationKind::MurphyDestination,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates an explicitly positioned Snik Snak turn-cycle frame.
    fn snik_snak_turn(turn: EnemyTurn, frame: u8) -> Self {
        // Turn cycles are advanced by the global modulo-four schedule in the
        // Snik Snak state machine, so their generic terminal action is only a
        // defensive fallback and should never be reached in valid play.
        Self {
            kind: AnimationKind::SnikSnakTurn(turn),
            frame: frame & 7,
            frame_count: 8,
            next: AnimationNext::Act,
        }
    }

    /// Creates the eight original transfer frames for a moving Snik Snak.
    fn snik_snak_move(direction: Direction) -> Self {
        Self::snik_snak_move_at(direction, 0)
    }

    /// Restores one validated frame within a Snik Snak transfer.
    fn snik_snak_move_at(direction: Direction, frame: u8) -> Self {
        // Callers use this constructor when the penultimate update must also
        // release the source reservation atomically. Clamp malformed input so
        // no public State can index beyond the eight original coordinates.
        Self {
            kind: AnimationKind::SnikSnakMove(direction),
            frame: frame.min(MOVEMENT_FRAMES - 1),
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::FinishSnikSnakMove { direction },
        }
    }

    /// Creates a non-advancing source reservation owned by a moving Snik Snak.
    fn snik_snak_vacating(direction: Direction) -> Self {
        // The destination updater releases this cell on its seventh transfer
        // callback. Keeping the reservation stable also makes enemy freeze
        // pause both halves of the movement exactly as in the original.
        Self {
            kind: AnimationKind::SnikSnakVacating(direction),
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates an explicitly positioned Electron turn-cycle frame.
    fn electron_turn(turn: EnemyTurn, frame: u8) -> Self {
        // Electron turn states share Snik Snak's global cadence but retain a
        // separate animation kind so their source table and explosion residue
        // cannot accidentally be interchanged.
        Self {
            kind: AnimationKind::ElectronTurn(turn),
            frame: frame & 7,
            frame_count: 8,
            next: AnimationNext::Act,
        }
    }

    /// Creates the eight original transfer frames for a moving Electron.
    fn electron_move(direction: Direction) -> Self {
        Self::electron_move_at(direction, 0)
    }

    /// Restores one validated frame within an Electron transfer.
    fn electron_move_at(direction: Direction, frame: u8) -> Self {
        // The frame-specific constructor lets movement frame seven release its
        // old source in the same atomic transition that advances the Electron.
        Self {
            kind: AnimationKind::ElectronMove(direction),
            frame: frame.min(MOVEMENT_FRAMES - 1),
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::FinishElectronMove { direction },
        }
    }

    /// Creates a non-advancing source reservation owned by a moving Electron.
    fn electron_vacating(direction: Direction) -> Self {
        // The reservation has no autonomous countdown: enemy freeze and a
        // destination-side frame-seven release control its complete lifetime.
        Self {
            kind: AnimationKind::ElectronVacating(direction),
            frame: 0,
            frame_count: 1,
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
            AnimationKind::Moving(_) | AnimationKind::Rolling(_)
        ) || matches!(
            self.kind,
            AnimationKind::Murphy(animation) if animation.changes_cell()
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
        if matches!(
            self.kind,
            AnimationKind::Idle
                | AnimationKind::Vacating(_)
                | AnimationKind::Terminal
                | AnimationKind::MurphyPushTarget
                | AnimationKind::MurphyDestination
                | AnimationKind::RoundedSide
                | AnimationKind::RoundedDestination
                | AnimationKind::SnikSnakVacating(_)
                | AnimationKind::ElectronVacating(_)
        ) {
            return AnimationAdvance::Ready;
        }

        // Starting a Murphy action calls the original animation routine
        // immediately, so frame zero consumes the action's first update. On
        // the update that draws the last coordinate, the DOS routine also
        // performs the promised collision, collection, or board transfer. Our
        // retained `frame` is the last picture already displayed; therefore a
        // Murphy strip must finish when its final picture is the next one, not
        // wait for a subsequent ninth callback after an eight-picture strip.
        if matches!(self.kind, AnimationKind::Murphy(_)) && self.frame + 2 >= self.frame_count {
            return AnimationAdvance::Finished(self.next);
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

    /// Creates a movement source whose release matches a Murphy action length.
    fn vacating_for(direction: Direction, frame_count: u8) -> Self {
        Self::animated(
            Actor::Empty(Empty),
            Animation::vacating_for(direction, frame_count),
        )
    }

    /// Creates a stable non-empty reservation for a future Murphy destination.
    fn murphy_destination() -> Self {
        Self::animated(Actor::Empty(Empty), Animation::murphy_destination())
    }

    /// Retains a pushable actor while suppressing its own fall/update behavior.
    fn murphy_push_target(actor: Actor) -> Self {
        debug_assert!(matches!(
            actor,
            Actor::Zonk(_) | Actor::YellowDisk(_) | Actor::OrangeDisk(_)
        ));
        Self::animated(actor, Animation::murphy_push_target())
    }

    /// Creates the temporary side reservation used by rounded pre-roll.
    fn rounded_side() -> Self {
        Self::animated(Actor::Empty(Empty), Animation::rounded_side())
    }

    /// Creates the diagonal reservation used by a horizontal rounded slide.
    fn rounded_destination() -> Self {
        Self::animated(Actor::Empty(Empty), Animation::rounded_destination())
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

    /// Creates a Snik Snak in an exact turn state selected during level loading.
    pub(crate) fn loaded_snik_snak_turn(frame: u8) -> Self {
        // The serialized tile has no direction byte. `convertToEasyTiles`
        // derives raw state zero or one from neighboring Space before play;
        // retaining that state explicitly avoids inventing a first-tick turn.
        Self::animated(
            Actor::SnikSnak(SnikSnak::new(Direction::Right)),
            Animation::snik_snak_turn(EnemyTurn::Left, frame),
        )
    }

    /// Creates the destination half of a load-time Snik Snak transfer.
    pub(crate) fn loaded_snik_snak_move(direction: Direction) -> Self {
        // Only Up and Right are selected by the original initialization pass.
        // The complete constructor remains directional so that actor heading,
        // movement artwork, and post-transfer wall following cannot diverge.
        debug_assert!(matches!(direction, Direction::Up | Direction::Right));
        Self::animated(
            Actor::SnikSnak(SnikSnak::new(direction)),
            Animation::snik_snak_move(direction),
        )
    }

    /// Creates the collision reservation left by a load-time Snik Snak move.
    pub(crate) fn loaded_snik_snak_source(direction: Direction) -> Self {
        // The original writes tile/state `0xffff` here. Model that otherwise
        // unscheduled, solid marker with the same destination-owned reservation
        // used by later Snik Snak transfers.
        Self::animated(
            Actor::Empty(Empty),
            Animation::snik_snak_vacating(direction),
        )
    }

    /// Creates an Electron in an exact turn state selected during level loading.
    pub(crate) fn loaded_electron_turn(frame: u8) -> Self {
        // Electrons share Snik Snak's raw state-zero/state-one conversion while
        // preserving a distinct actor and sprite family for later explosions.
        Self::animated(
            Actor::Electron(Electron::new(Direction::Right)),
            Animation::electron_turn(EnemyTurn::Left, frame),
        )
    }

    /// Creates the destination half of a load-time Electron transfer.
    pub(crate) fn loaded_electron_move(direction: Direction) -> Self {
        // Preserve the derived direction as the Electron's heading so movement
        // completion begins its next left-hand decision from the correct side.
        debug_assert!(matches!(direction, Direction::Up | Direction::Right));
        Self::animated(
            Actor::Electron(Electron::new(direction)),
            Animation::electron_move(direction),
        )
    }

    /// Creates the collision reservation left by a load-time Electron move.
    pub(crate) fn loaded_electron_source(direction: Direction) -> Self {
        // This stable marker stands in for original `0xffff` until movement
        // frame seven clears it from the destination-side update.
        Self::animated(Actor::Empty(Empty), Animation::electron_vacating(direction))
    }

    /// Reports whether this state is unoccupied for collision purposes.
    pub fn is_empty(&self) -> bool {
        matches!(self.actor, Actor::Empty(_))
            && !matches!(
                self.animation.kind,
                AnimationKind::Vacating(_)
                    | AnimationKind::SnikSnakVacating(_)
                    | AnimationKind::ElectronVacating(_)
                    | AnimationKind::MurphyDestination
                    | AnimationKind::RoundedSide
                    | AnimationKind::RoundedDestination
            )
    }

    /// Reports whether the actor is in its stable, non-moving phase.
    pub fn is_idle(&self) -> bool {
        self.animation.kind == AnimationKind::Idle
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
        // A planting Murphy owns a hold-sensitive counter. Generic animation
        // advancement would make Space release unable to cancel the operation,
        // so this phase delegates directly to the actor state machine.
        if let Self::Murphy(murphy) = self
            && murphy.is_planting_red_disk()
        {
            return murphy.transition(position, world);
        }

        // Snik Snak turn cycles use two different global modulo-four phases,
        // while their penultimate transfer update also owns source cleanup.
        // Intercept both before generic per-tick animation advancement.
        if let Self::SnikSnak(snik_snak) = self {
            if world.freeze_enemies() {
                return None;
            }
            match state.animation.kind {
                AnimationKind::SnikSnakTurn(_) => {
                    return snik_snak.transition(state, position, world);
                }
                AnimationKind::SnikSnakMove(direction) if state.animation.frame == 6 => {
                    return Some(
                        snik_snak.advance_penultimate_movement(position, direction, state, world),
                    );
                }
                AnimationKind::SnikSnakMove(_) => {}
                _ => {}
            }
        }

        // Electron turns and source cleanup obey the same split scheduling as
        // Snik Snaks, while retaining Electron-specific collision and residue.
        if let Self::Electron(electron) = self {
            if world.freeze_enemies() {
                return None;
            }
            match state.animation.kind {
                AnimationKind::ElectronTurn(_) => {
                    return electron.transition(state, position, world);
                }
                AnimationKind::ElectronMove(direction) if state.animation.frame == 6 => {
                    return Some(
                        electron.advance_penultimate_movement(position, direction, state, world),
                    );
                }
                AnimationKind::ElectronMove(_) => {}
                _ => {}
            }
        }

        // Original falling state 0x15 increments to 0x16 and clears the prior
        // cell before the final two pictures play. The temporary Space marker
        // has no updater of its own, so the destination must own this write.
        if matches!(self, Self::Zonk(_) | Self::Infotron(_))
            && let AnimationKind::Moving(direction) = state.animation.kind
            && state.animation.frame == 5
        {
            return Some(self.advance_falling_source(position, direction, world));
        }

        // Reserved push/snap targets retain their actor identity for rendering
        // and blast interactions but must not fall, roll, or otherwise update.
        if state.animation.kind == AnimationKind::MurphyPushTarget {
            return None;
        }

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
                // The DOS Bug updater checks all eight neighbors after each
                // active quarter-tick frame is selected. Dormant frames and
                // non-Bug animations pass through without an audio request.
                let events = if matches!(self, Self::Bug(_))
                    && animation.kind == AnimationKind::Bug
                    && world.has_neighboring_murphy(position)
                {
                    vec![GameEvent::PlaySound(SoundEffect::Bug)]
                } else {
                    Vec::new()
                };
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::animated(self.clone(), animation),
                    )],
                    events,
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
            AnimationAdvance::Finished(AnimationNext::BeginInfotronFall) => {
                let Self::Infotron(infotron) = self else {
                    debug_assert!(false, "only an Infotron may finish Infotron pre-fall");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return infotron.begin_fall(position, world);
            }
            AnimationAdvance::Finished(AnimationNext::BeginRoundedSlide { direction }) => {
                let side = world.offset(position, direction);
                let diagonal = side.and_then(|cell| world.offset(cell, Direction::Down));
                let side_reserved = side
                    .and_then(|cell| world.state(cell))
                    .is_some_and(|state| {
                        matches!(state.actor(), Actor::Empty(_))
                            && state.animation.kind == AnimationKind::RoundedSide
                    });

                if side_reserved && diagonal.is_some_and(|cell| world.is_empty(cell)) {
                    let side = side.expect("a validated side reservation has a position");
                    let diagonal = diagonal.expect("a validated diagonal has a position");
                    let falling_actor = match self {
                        Self::Zonk(_) => Self::Zonk(Zonk::falling()),
                        Self::Infotron(_) => Self::Infotron(Infotron::falling()),
                        _ => {
                            debug_assert!(false, "only rounded actors may begin a side slide");
                            return Some(Transition::replace(position, State::new(self.clone())));
                        }
                    };
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                side,
                                State::animated(falling_actor, Animation::rolling(direction)),
                            ),
                            CellWrite::new(diagonal, State::rounded_destination()),
                        ],
                        Vec::new(),
                    ));
                }

                if side_reserved {
                    // State 0x51 is deliberately sticky: another actor may
                    // vacate the diagonal later while the side stays reserved.
                    return Some(Transition::replace(
                        position,
                        State::animated(self.clone(), Animation::rounded_wait(direction)),
                    ));
                }

                // A blast can consume the reservation. Recover the rounded
                // actor as stable without overwriting the new side occupant.
                return Some(Transition::replace(position, State::new(self.clone())));
            }
            AnimationAdvance::Finished(AnimationNext::BeginRoundedFall { direction: _ }) => {
                debug_assert!(matches!(self, Self::Zonk(_) | Self::Infotron(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::RoundedDestination
                }) {
                    return Some(Transition::replace(position, State::new(self.clone())));
                }
                return Some(Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(
                            destination,
                            State::animated(self.clone(), Animation::moving(Direction::Down)),
                        ),
                    ],
                    Vec::new(),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::BeginOrangeFall) => {
                debug_assert!(matches!(self, Self::OrangeDisk(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::RoundedDestination
                }) {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::OrangeDisk(OrangeDisk::resting())),
                    ));
                }
                return Some(Transition::replace(
                    position,
                    State::animated(self.clone(), Animation::orange_falling()),
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishOrangeFall) => {
                debug_assert!(matches!(self, Self::OrangeDisk(_)));
                let Some(destination) = world.offset(position, Direction::Down) else {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::OrangeDisk(OrangeDisk::resting())),
                    ));
                };
                let destination_reserved = world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::RoundedDestination
                });
                if !destination_reserved {
                    return Some(explode_at(world, position, false));
                }

                let landing_cell = world.offset(destination, Direction::Down);
                if landing_cell.is_some_and(|cell| world.is_empty(cell)) {
                    let landing_cell = landing_cell.expect("validated landing cell exists");
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                destination,
                                State::animated(
                                    Self::OrangeDisk(OrangeDisk::falling()),
                                    Animation::orange_falling(),
                                ),
                            ),
                            CellWrite::new(landing_cell, State::rounded_destination()),
                        ],
                        Vec::new(),
                    ));
                }

                if landing_cell
                    .and_then(|cell| world.state(cell))
                    .is_some_and(|state| matches!(state.actor(), Actor::Explosion(_)))
                {
                    return Some(Transition::new(
                        vec![
                            CellWrite::new(position, State::empty()),
                            CellWrite::new(
                                destination,
                                State::new(Self::OrangeDisk(OrangeDisk::resting())),
                            ),
                        ],
                        Vec::new(),
                    ));
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
                return Some(explosion);
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
                // Reaching active frame zero also performs the proximity check;
                // waiting until frame one would omit one original Bug chirp.
                let events = world
                    .has_neighboring_murphy(position)
                    .then_some(GameEvent::PlaySound(SoundEffect::Bug))
                    .into_iter()
                    .collect();
                return Some(Transition::new(
                    vec![CellWrite::new(
                        position,
                        State::animated(Self::Bug(Bug), Animation::bug_active()),
                    )],
                    events,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishSnikSnakMove { direction }) => {
                let Self::SnikSnak(snik_snak) = self else {
                    // Only `Animation::snik_snak_move` constructs this promise.
                    // Recover malformed state without mutating neighboring cells.
                    debug_assert!(false, "only a Snik Snak may finish this movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return Some(snik_snak.finish_movement(position, direction, world));
            }
            AnimationAdvance::Finished(AnimationNext::FinishElectronMove { direction }) => {
                let Self::Electron(electron) = self else {
                    // Only `Animation::electron_move` constructs this promise;
                    // avoid neighbor writes if internal state is corrupted.
                    debug_assert!(false, "only an Electron may finish this movement");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                return Some(electron.finish_movement(position, direction, world));
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
            AnimationAdvance::Finished(AnimationNext::FinishMurphySnap { direction, target }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a snap action");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let mut writes = vec![CellWrite::new(position, State::new(Self::Murphy(*murphy)))];
                if let Some(target_position) = world.offset(position, direction)
                    && world.state(target_position).is_some_and(|target_state| {
                        target_state.animation.kind == AnimationKind::MurphyPushTarget
                    })
                {
                    writes.push(CellWrite::new(target_position, State::empty()));
                }
                let events = match target {
                    MurphySnapTarget::Base => Vec::new(),
                    MurphySnapTarget::Infotron => vec![GameEvent::CollectInfotron],
                    MurphySnapTarget::RedDisk => vec![GameEvent::CollectRedDisk],
                };
                return Some(Transition::new(writes, events));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyPush { direction, target }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a push action");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(target_position) = world.offset(position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(destination) = world.offset(target_position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let reservations_intact = world.state(target_position).is_some_and(|state| {
                    state.animation.kind == AnimationKind::MurphyPushTarget
                        && pushed_actor_matches(state.actor(), target)
                }) && world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::MurphyDestination
                });
                if !reservations_intact {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
                    ));
                }
                let pushed_state = if target == MurphyPushTarget::OrangeDisk
                    && direction == Direction::Right
                    && world
                        .offset(destination, Direction::Down)
                        .is_some_and(|below| world.is_empty(below))
                {
                    State::animated(
                        Self::OrangeDisk(OrangeDisk::falling()),
                        Animation::orange_pre_fall(),
                    )
                } else {
                    State::new(actor_for_push_target(target))
                };
                let mut writes = vec![
                    CellWrite::new(position, State::empty()),
                    CellWrite::new(target_position, State::new(Self::Murphy(*murphy))),
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
                return Some(Transition::new(writes, Vec::new()));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyPort { direction }) => {
                let Self::Murphy(murphy) = self else {
                    debug_assert!(false, "only Murphy may finish a port traversal");
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(port_position) = world.offset(position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                let Some(destination) = world.offset(port_position, direction) else {
                    return Some(Transition::replace(position, State::new(self.clone())));
                };
                if !world.state(destination).is_some_and(|state| {
                    matches!(state.actor(), Actor::Empty(_))
                        && state.animation.kind == AnimationKind::MurphyDestination
                }) {
                    return Some(Transition::replace(
                        position,
                        State::new(Self::Murphy(*murphy)),
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
                return Some(Transition::new(
                    vec![
                        CellWrite::new(position, State::empty()),
                        CellWrite::new(destination, State::new(Self::Murphy(*murphy))),
                    ],
                    events,
                ));
            }
            AnimationAdvance::Finished(AnimationNext::FinishMurphyExit) => {
                debug_assert!(matches!(self, Self::Murphy(_)));
                // Success was recorded when Murphy entered the Exit. Finishing
                // its artwork now only removes his disappearing sprite.
                return Some(Transition::replace(position, State::empty()));
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
            Self::SnikSnak(actor) => actor.transition(state, position, world),
            Self::YellowDisk(actor) => actor.transition(position, world),
            Self::Terminal(actor) => actor.transition(position, world),
            Self::RedDisk(actor) => actor.transition(position, world),
            Self::Electron(actor) => actor.transition(state, position, world),
            Self::Bug(actor) => actor.transition(position, world),
            Self::InvisibleWall(actor) => actor.transition(position, world),
            Self::Explosion(actor) => actor.transition(position, world),
        }
    }

    /// Returns the validated starting/resting animation for this actor type.
    fn default_animation(&self) -> Animation {
        match self {
            Self::SnikSnak(actor) => Animation::snik_snak_turn(
                EnemyTurn::Left,
                EnemyTurn::Left.initial_frame(actor.heading()),
            ),
            Self::Electron(actor) => Animation::electron_turn(
                EnemyTurn::Left,
                EnemyTurn::Left.initial_frame(actor.heading()),
            ),
            Self::Bug(_) => Animation::bug_active(),
            Self::Terminal(terminal) => Animation::terminal(terminal.screen_frame()),
            Self::Explosion(explosion) => Animation::explosion(explosion.residue()),
            _ => Animation::idle(),
        }
    }

    /// Advances a fall to state `0x16` while releasing its temporary old cell.
    fn advance_falling_source(
        &self,
        position: Position,
        direction: Direction,
        world: &WorldView<'_>,
    ) -> Transition {
        debug_assert!(matches!(self, Self::Zonk(_) | Self::Infotron(_)));
        debug_assert_eq!(direction, Direction::Down);
        let mut writes = vec![CellWrite::new(
            position,
            State::animated(self.clone(), Animation::moving_at(direction, 6)),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                matches!(source_state.actor(), Actor::Empty(_))
                    && source_state.animation.kind == AnimationKind::Vacating(direction)
            })
        {
            // Preserve an actor or explosion that already consumed the source;
            // only the still-matching temporary marker may become true Space.
            writes.push(CellWrite::new(source, State::empty()));
        }

        Transition::new(writes, Vec::new())
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
                let (direction, source_distance, target) = match state.animation.kind {
                    AnimationKind::Murphy(MurphyAnimation::Move {
                        direction, target, ..
                    }) => (direction, 1, target),
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
                let events = match target {
                    MurphyMoveTarget::Empty | MurphyMoveTarget::Base => Vec::new(),
                    MurphyMoveTarget::Infotron => vec![GameEvent::CollectInfotron],
                    MurphyMoveTarget::RedDisk => vec![GameEvent::CollectRedDisk],
                    MurphyMoveTarget::PlantedRedDisk => Vec::new(),
                };
                return Transition::new(writes, events);
            }
            Self::Zonk(_) if world.freeze_zonks() && state.animation.is_movement() => {
                // The in-flight transfer reaches its destination, then loses
                // falling momentum without inspecting the next cell. This is
                // where an original in-flight Zonk first observes freeze.
                return Transition::replace(position, State::new(Self::Zonk(Zonk::resting())));
            }
            Self::Zonk(zonk) if zonk.is_falling() => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        match target.actor() {
                            Actor::Murphy(_) if murphy_is_protected_from_falling_actor(target) => {
                                // Horizontal push states 0x0e/0x0f/0x25/
                                // 0x26/0x28/0x29 are explicit original crush
                                // exceptions. The DOS routine returns before
                                // its later Fall-sound call on this path.
                                return Transition::replace(
                                    position,
                                    State::new(Self::Zonk(Zonk::resting())),
                                );
                            }
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
                            Self::Zonk(Zonk::falling()),
                            Direction::Down,
                        );
                    }
                    // Landing on a non-reactive occupant is the one safe Zonk
                    // terminal path that selects the original Fall effect.
                    return Transition::new(
                        vec![CellWrite::new(
                            position,
                            State::new(Self::Zonk(Zonk::resting())),
                        )],
                        vec![GameEvent::PlaySound(SoundEffect::Fall)],
                    );
                }
            }
            Self::Infotron(infotron) if infotron.is_falling() => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        if matches!(target.actor(), Actor::Murphy(_)) {
                            if murphy_is_protected_from_falling_actor(target) {
                                // Protected push states use the same silent
                                // early return as the Zonk landing routine.
                                return Transition::replace(
                                    position,
                                    State::new(Self::Infotron(Infotron::resting())),
                                );
                            }
                            // Sequential player-first mutation has already
                            // decided whether Murphy escaped before this hit.
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
                    // A continued vertical transfer remains silent. Only the
                    // first obstructed settle matches `playFallSound`.
                    let events = (!still_falling)
                        .then_some(GameEvent::PlaySound(SoundEffect::Fall))
                        .into_iter()
                        .collect();
                    return Transition::new(
                        vec![CellWrite::new(
                            position,
                            State::new(Self::Infotron(if still_falling {
                                Infotron::falling()
                            } else {
                                Infotron::resting()
                            })),
                        )],
                        events,
                    );
                }
            }
            Self::OrangeDisk(disk) if disk.is_falling() => {
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
            Self::RamChip(chip) => match chip.shape() {
                RamChipShape::Center => 5,
                RamChipShape::Left => 26,
                RamChipShape::Right => 27,
                RamChipShape::Top => 38,
                RamChipShape::Bottom => 39,
            },
            Self::Hardware(hardware) => {
                if hardware.variant() == 0 {
                    6
                } else {
                    27 + hardware.variant().min(10)
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
    /// Reserve the one planted-disk slot at countdown one while Space is held.
    BeginPlantRedDisk(Position),
    /// Cancel an incomplete placement without spending a Red Disk.
    CancelPlantRedDisk,
    /// Spend one disk and arm the completed placement at countdown two.
    FinishPlantRedDisk,
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
    /// Forward one actor-selected effect to the platform playback queue.
    PlaySound(SoundEffect),
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

    /// Starts a Snik Snak transfer with a destination-owned release schedule.
    fn move_snik_snak(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        // A generic Vacating animation releases itself after eight callbacks.
        // The DOS enemy instead clears its source from movement frame seven,
        // so this stable reservation is explicitly owned by the destination.
        let destination_state = State::animated(actor, Animation::snik_snak_move(direction));
        let source_state = State::animated(
            Actor::Empty(Empty),
            Animation::snik_snak_vacating(direction),
        );
        Self::new(
            vec![
                CellWrite::new(source, source_state),
                CellWrite::new(destination, destination_state),
            ],
            Vec::new(),
        )
    }

    /// Starts an Electron transfer with destination-owned source cleanup.
    fn move_electron(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        // Separate animation kinds keep Electron reservations distinguishable
        // from a Snik Snak or generic actor crossing the same cells later.
        let destination_state = State::animated(actor, Animation::electron_move(direction));
        let source_state =
            State::animated(Actor::Empty(Empty), Animation::electron_vacating(direction));
        Self::new(
            vec![
                CellWrite::new(source, source_state),
                CellWrite::new(destination, destination_state),
            ],
            Vec::new(),
        )
    }

    /// Moves Murphy with a target-specific eight- or nine-frame descriptor.
    fn move_murphy(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        target: MurphyMoveTarget,
        looking_left: bool,
    ) -> Self {
        Self::move_murphy_with_events(
            source,
            destination,
            actor,
            direction,
            target,
            looking_left,
            Vec::new(),
        )
    }

    /// Moves Murphy while emitting action-start side effects after both writes.
    fn move_murphy_with_events(
        source: Position,
        destination: Position,
        actor: Actor,
        direction: Direction,
        target: MurphyMoveTarget,
        looking_left: bool,
        events: Vec<GameEvent>,
    ) -> Self {
        // The destination owns animation progress from the initiating update;
        // the sound belongs to that same atomic start, never to completion.
        let animation = Animation::murphy_move(direction, target, looking_left);
        let frame_count = animation.frame_count;
        Self::new(
            vec![
                CellWrite::new(source, State::vacating_for(direction, frame_count)),
                CellWrite::new(destination, State::animated(actor, animation)),
            ],
            events,
        )
    }

    /// Begins the two-update side delay while reserving only the adjacent cell.
    fn prepare_rounded_roll(
        source: Position,
        side: Position,
        actor: Actor,
        direction: Direction,
    ) -> Self {
        Self::new(
            vec![
                CellWrite::new(
                    source,
                    State::animated(actor, Animation::rounded_pre_roll(direction)),
                ),
                CellWrite::new(side, State::rounded_side()),
            ],
            Vec::new(),
        )
    }
}
