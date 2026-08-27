//! Actor-owned behavior, animation phases, and immediate atomic transitions.
//!
//! Every level cell contains one [`State`].  An [`Actor`] is an enum whose
//! variants wrap actor-specific structs, while [`Animation`] records the visual
//! phase and the semantic transition that follows its final frame.

use crate::{game::WorldView, level::SpecialPort};

/// Number of original updates used by a falling or enemy cell transfer.
const MOVEMENT_FRAMES: u8 = 8;

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
    const fn frame_count(self) -> u8 {
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
    const fn changes_cell(self) -> bool {
        matches!(self, Self::Move { .. } | Self::Port { .. })
    }
}

/// Visual family used to select a frame from the sprite atlas.
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
        Self {
            kind: AnimationKind::Vacating(direction),
            frame: 0,
            frame_count: frame_count.max(1),
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
                return Some(Transition::prepare_rounded_roll(
                    position,
                    side,
                    Actor::Zonk(*self),
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
    fn transition(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
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
                return Transition::replace(
                    position,
                    State::animated(
                        Actor::Murphy(moving),
                        Animation::murphy_push(direction, target),
                    ),
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
            Actor::Infotron(_) => MurphySnapTarget::Infotron,
            Actor::RedDisk(_) if target_state.is_idle() => MurphySnapTarget::RedDisk,
            _ => return None,
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
            Vec::new(),
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
            Actor::Base(_) => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Base,
                looking_left,
            )),
            Actor::Bug(_) if world.is_bug_active(target) => {
                Some(explode_at(world, position, false))
            }
            Actor::Bug(_) => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Base,
                looking_left,
            )),
            Actor::Infotron(_) => Some(Transition::move_murphy(
                position,
                target,
                murphy_actor,
                direction,
                MurphyMoveTarget::Infotron,
                looking_left,
            )),
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
                Vec::new(),
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
            Actor::Explosion(explosion) => Some(explode_at(
                world,
                position,
                explosion.residue == ExplosionResidue::Infotron,
            )),
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
fn actor_for_push_target(target: MurphyPushTarget) -> Actor {
    match target {
        MurphyPushTarget::Zonk => Actor::Zonk(Zonk::resting()),
        MurphyPushTarget::YellowDisk => Actor::YellowDisk(YellowDisk),
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
fn murphy_is_protected_from_falling_actor(state: &State) -> bool {
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
fn murphy_is_crossing_port(state: &State) -> bool {
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
            if self.falling {
                return Some(Transition::move_actor(
                    position,
                    below,
                    Actor::Infotron(*self),
                    Direction::Down,
                ));
            }

            return Some(Transition::replace(
                position,
                State::animated(Actor::Infotron(*self), Animation::infotron_pre_fall()),
            ));
        }

        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Direction::Left, Direction::Right] {
            let side = world.offset(position, direction)?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::prepare_rounded_roll(
                    position,
                    side,
                    Actor::Infotron(*self),
                    direction,
                ));
            }
        }

        None
    }

    /// Starts a resting Infotron fall after its destination survives one update.
    fn begin_fall(&self, position: Position, world: &WorldView<'_>) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if !world.is_empty(below) {
            return None;
        }

        Some(Transition::move_actor(
            position,
            below,
            Actor::Infotron(Self { falling: true }),
            Direction::Down,
        ))
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
            // A resting Orange Disk installs the original state-0x20 delay and
            // reserves the cell below before any falling artwork is shown.
            return Some(Transition::new(
                vec![
                    CellWrite::new(
                        position,
                        State::animated(
                            Actor::OrangeDisk(Self { falling: true }),
                            Animation::orange_pre_fall(),
                        ),
                    ),
                    CellWrite::new(below, State::rounded_destination()),
                ],
                Vec::new(),
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
    /// Creates a Snik Snak whose first left-turn candidate follows `heading`.
    pub const fn new(heading: Direction) -> Self {
        Self { heading }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Advances or evaluates the current globally phased turn animation.
    fn transition(
        &self,
        state: &State,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        if world.freeze_enemies() {
            // Original enemy freeze returns before changing either the state
            // byte or framebuffer, so retaining the complete State is required.
            return None;
        }

        let AnimationKind::SnikSnakTurn(turn) = state.animation.kind else {
            // Transfers are handled by the generic finite-animation path. This
            // fallback makes an internally malformed idle Snik Snak recover to
            // the correct left-turn cycle without inventing an instant step.
            debug_assert!(
                matches!(state.animation.kind, AnimationKind::Idle),
                "Snik Snak decisions require a turn animation"
            );
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::SnikSnak(*self),
                    Animation::snik_snak_turn(
                        EnemyTurn::Left,
                        EnemyTurn::Left.initial_frame(self.heading),
                    ),
                ),
            ));
        };

        if world.tick_count().is_multiple_of(4) {
            // The original draws the current turn picture and then increments
            // its low three state bits, wrapping within the selected cycle.
            let next_frame = (state.animation.frame + 1) & 7;
            return Some(Transition::replace(
                position,
                State::animated(
                    Actor::SnikSnak(*self),
                    Animation::snik_snak_turn(turn, next_frame),
                ),
            ));
        }

        if world.tick_count() % 4 != 3 {
            return None;
        }

        let direction = turn.direction_at_frame(state.animation.frame)?;
        let destination = world.offset(position, direction)?;
        if world.is_empty(destination) {
            return Some(Transition::move_snik_snak(
                position,
                destination,
                Actor::SnikSnak(Self { heading: direction }),
                direction,
            ));
        }

        let target_is_vulnerable_murphy = world.state(destination).is_some_and(|target| {
            matches!(target.actor(), Actor::Murphy(_)) && !murphy_is_crossing_port(target)
        });
        target_is_vulnerable_murphy.then(|| explode_at(world, position, false))
    }

    /// Releases the old source on the original seventh movement callback.
    fn advance_penultimate_movement(
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
                Actor::SnikSnak(*self),
                Animation::snik_snak_move_at(direction, 7),
            ),
        )];

        if let Some(source) = world.offset(position, direction.opposite())
            && world.state(source).is_some_and(|source_state| {
                matches!(source_state.actor(), Actor::Empty(_))
                    && source_state.animation.kind == AnimationKind::SnikSnakVacating(direction)
            })
        {
            // A blast may already have replaced the reservation. As in the DOS
            // routine, never erase an Explosion encountered at the old source.
            writes.push(CellWrite::new(source, State::empty()));
        }

        Transition::new(writes, Vec::new())
    }

    /// Resolves left, forward, right, then turn-around after a completed move.
    fn finish_movement(
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
                return Transition::move_snik_snak(
                    position,
                    forward,
                    Actor::SnikSnak(*self),
                    direction,
                );
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
        let animation = Animation::snik_snak_turn(turn, turn.preceding_frame(candidate));
        Transition::replace(position, State::animated(Actor::SnikSnak(*self), animation))
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
    /// Creates an Electron whose first left-turn candidate follows `heading`.
    pub const fn new(heading: Direction) -> Self {
        Self { heading }
    }

    /// Returns the enemy's current movement heading.
    pub const fn heading(self) -> Direction {
        self.heading
    }

    /// Advances or evaluates the current globally phased turn animation.
    fn transition(
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
    fn advance_penultimate_movement(
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
    fn finish_movement(
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
        // A planting Murphy owns a hold-sensitive counter. Generic animation
        // advancement would make Space release unable to cancel the operation,
        // so this phase delegates directly to the actor state machine.
        if let Self::Murphy(murphy) = self
            && matches!(murphy.phase, MurphyPhase::PlantingRedDisk { .. })
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
                        Self::Zonk(_) => Self::Zonk(Zonk { falling: true }),
                        Self::Infotron(_) => Self::Infotron(Infotron { falling: true }),
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
                                    Self::OrangeDisk(OrangeDisk { falling: true }),
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
                return Some(Transition::replace(
                    position,
                    State::animated(Self::Bug(Bug), Animation::bug_active()),
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
                        Self::OrangeDisk(OrangeDisk { falling: true }),
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
                        Actor::Port(port) if port.special => world
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
                return Some(Transition::new(
                    vec![CellWrite::new(position, State::empty())],
                    vec![GameEvent::Completed],
                ));
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
                EnemyTurn::Left.initial_frame(actor.heading),
            ),
            Self::Electron(actor) => Animation::electron_turn(
                EnemyTurn::Left,
                EnemyTurn::Left.initial_frame(actor.heading),
            ),
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
            Self::Zonk(zonk) if zonk.falling => {
                if let Some(below) = world.offset(position, Direction::Down) {
                    if let Some(target) = world.state(below) {
                        match target.actor() {
                            Actor::Murphy(_) if murphy_is_protected_from_falling_actor(target) => {
                                // Horizontal push states 0x0e/0x0f/0x25/
                                // 0x26/0x28/0x29 are explicit original crush
                                // exceptions; the rounded actor simply lands.
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
                        if matches!(target.actor(), Actor::Murphy(_))
                            && !murphy_is_protected_from_falling_actor(target)
                        {
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
        let animation = Animation::murphy_move(direction, target, looking_left);
        let frame_count = animation.frame_count;
        Self::new(
            vec![
                CellWrite::new(source, State::vacating_for(direction, frame_count)),
                CellWrite::new(destination, State::animated(actor, animation)),
            ],
            Vec::new(),
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
