//! Validated animation phases and their typed completion actions.
//!
//! Constructors keep duration, artwork, and completion semantics together.
//! Only the actor family can create active phases; consumers receive read-only
//! queries for rendering. Cadence-sensitive actors intercept generic advancement
//! in the coordinator before these ordinary finite phases are stepped.

use super::{
    Direction, EnemyTurn, ExplosionResidue, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget,
    MurphySnapTarget, RED_DISK_DETONATION_COUNTDOWN,
};

/// Number of original updates used by a falling or enemy cell transfer.
const MOVEMENT_FRAMES: u8 = 8;

/// Number of source frames in either original `MOVING.DAT` explosion strip.
const EXPLOSION_FRAMES: u8 = 8;

/// Number of lethal logical phases in each active Bug cycle.
const BUG_ACTIVE_FRAMES: u8 = 14;

/// Number of simulation frames between a Zonk strike and Orange Disk blast.
const ORANGE_DISK_TRIGGER_FRAMES: u8 = 6;

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
pub(super) enum AnimationNext {
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
    pub(super) fn zonk_pre_fall() -> Self {
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
    pub(super) fn infotron_pre_fall() -> Self {
        Self {
            kind: AnimationKind::InfotronPreFall,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginInfotronFall,
        }
    }

    /// Creates the two-update delay before a rounded actor leaves its support.
    pub(super) fn rounded_pre_roll(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::RoundedPreRoll(direction),
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginRoundedSlide { direction },
        }
    }

    /// Retains the final pre-roll state while a diagonal destination is blocked.
    pub(super) fn rounded_wait(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::RoundedPreRoll(direction),
            frame: 0,
            frame_count: 1,
            next: AnimationNext::BeginRoundedSlide { direction },
        }
    }

    /// Creates a synchronized, invisible reservation for a movement source.
    pub(super) fn vacating(direction: Direction) -> Self {
        Self::vacating_for(direction, MOVEMENT_FRAMES)
    }

    /// Creates a source reservation synchronized to a caller-owned duration.
    pub(super) fn vacating_for(direction: Direction, frame_count: u8) -> Self {
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
    pub(super) fn moving(direction: Direction) -> Self {
        Self::moving_at(direction, 0)
    }

    /// Restores one validated frame within a generic actor transfer.
    pub(super) fn moving_at(direction: Direction, frame: u8) -> Self {
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
    pub(super) fn rolling(direction: Direction) -> Self {
        Self {
            kind: AnimationKind::Rolling(direction),
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::BeginRoundedFall { direction },
        }
    }

    /// Creates stable occupancy for a side cell reserved by pre-roll.
    pub(super) fn rounded_side() -> Self {
        Self {
            kind: AnimationKind::RoundedSide,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates stable occupancy for a diagonal rolling destination.
    pub(super) fn rounded_destination() -> Self {
        Self {
            kind: AnimationKind::RoundedDestination,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates the original two-update arming delay for an Orange Disk fall.
    pub(super) fn orange_pre_fall() -> Self {
        Self {
            kind: AnimationKind::OrangePreFall,
            frame: 0,
            frame_count: 2,
            next: AnimationNext::BeginOrangeFall,
        }
    }

    /// Creates eight visible falling frames retained at the Orange source cell.
    pub(super) fn orange_falling() -> Self {
        Self {
            kind: AnimationKind::OrangeFalling,
            frame: 0,
            frame_count: MOVEMENT_FRAMES,
            next: AnimationNext::FinishOrangeFall,
        }
    }

    /// Holds a completed movement's final pose until Murphy's next update.
    pub(super) fn murphy_ready(kind: AnimationKind) -> Self {
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
    pub(super) fn murphy_move(
        direction: Direction,
        target: MurphyMoveTarget,
        looking_left: bool,
    ) -> Self {
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
    pub(super) fn murphy_snap(direction: Direction, target: MurphySnapTarget) -> Self {
        let action = MurphyAnimation::Snap { direction, target };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphySnap { direction, target },
        }
    }

    /// Creates a push animation after the eight-tick hold requirement succeeds.
    pub(super) fn murphy_push(direction: Direction, target: MurphyPushTarget) -> Self {
        let action = MurphyAnimation::Push { direction, target };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyPush { direction, target },
        }
    }

    /// Creates the paired eight-frame passage through one port cell.
    pub(super) fn murphy_port(direction: Direction) -> Self {
        let action = MurphyAnimation::Port { direction };
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyPort { direction },
        }
    }

    /// Creates the original forty-frame Exit disappearance at Murphy's source.
    pub(super) fn murphy_exit() -> Self {
        let action = MurphyAnimation::Exit;
        Self {
            kind: AnimationKind::Murphy(action),
            frame: 0,
            frame_count: action.frame_count(),
            next: AnimationNext::FinishMurphyExit,
        }
    }

    /// Creates one hold-sensitive Red Disk placement pose at `elapsed` ticks.
    pub(super) fn murphy_plant(elapsed: u8) -> Self {
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
    pub(super) fn murphy_push_target() -> Self {
        Self {
            kind: AnimationKind::MurphyPushTarget,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates stable collision occupancy for an otherwise empty destination.
    pub(super) fn murphy_destination() -> Self {
        Self {
            kind: AnimationKind::MurphyDestination,
            frame: 0,
            frame_count: 1,
            next: AnimationNext::Act,
        }
    }

    /// Creates an explicitly positioned Snik Snak turn-cycle frame.
    pub(super) fn snik_snak_turn(turn: EnemyTurn, frame: u8) -> Self {
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
    pub(super) fn snik_snak_move(direction: Direction) -> Self {
        Self::snik_snak_move_at(direction, 0)
    }

    /// Restores one validated frame within a Snik Snak transfer.
    pub(super) fn snik_snak_move_at(direction: Direction, frame: u8) -> Self {
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
    pub(super) fn snik_snak_vacating(direction: Direction) -> Self {
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
    pub(super) fn electron_turn(turn: EnemyTurn, frame: u8) -> Self {
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
    pub(super) fn electron_move(direction: Direction) -> Self {
        Self::electron_move_at(direction, 0)
    }

    /// Restores one validated frame within an Electron transfer.
    pub(super) fn electron_move_at(direction: Direction, frame: u8) -> Self {
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
    pub(super) fn electron_vacating(direction: Direction) -> Self {
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
    pub(super) fn terminal(frame: u8) -> Self {
        Self {
            kind: AnimationKind::Terminal,
            frame: frame % 7,
            frame_count: 7,
            next: AnimationNext::Act,
        }
    }

    /// Creates the synchronized lethal phase used by every newly loaded Bug.
    pub(super) fn bug_active() -> Self {
        Self {
            kind: AnimationKind::Bug,
            frame: 0,
            frame_count: BUG_ACTIVE_FRAMES,
            next: AnimationNext::RandomizeBug,
        }
    }

    /// Creates one safe per-Bug cooldown measured in quarter-rate updates.
    pub(super) fn bug_dormant(delay: u8) -> Self {
        debug_assert!(delay > 0, "a Bug cooldown must consume at least one update");
        Self {
            kind: AnimationKind::BugDormant,
            frame: 0,
            frame_count: delay.max(1),
            next: AnimationNext::ActivateBug,
        }
    }

    /// Creates the finite fuse placed on a dropped Red Disk.
    pub(super) fn red_disk_fuse(frame: u8) -> Self {
        Self {
            kind: AnimationKind::RedDiskFuse,
            frame: frame.min(RED_DISK_DETONATION_COUNTDOWN - 1),
            frame_count: RED_DISK_DETONATION_COUNTDOWN,
            next: AnimationNext::Explode(ExplosionResidue::Empty),
        }
    }

    /// Creates the short delayed fuse caused by a Zonk striking Orange Disk.
    pub(super) fn orange_disk_fuse() -> Self {
        Self {
            kind: AnimationKind::OrangeDiskFuse,
            frame: 0,
            frame_count: ORANGE_DISK_TRIGGER_FRAMES,
            next: AnimationNext::Explode(ExplosionResidue::Empty),
        }
    }

    /// Reports whether the phase interpolates an actor between board cells.
    pub(super) const fn is_movement(&self) -> bool {
        matches!(
            self.kind,
            AnimationKind::Moving(_) | AnimationKind::Rolling(_)
        ) || matches!(
            self.kind,
            AnimationKind::Murphy(animation) if animation.changes_cell()
        )
    }

    /// Creates one of the finite eight-frame explosion animations.
    pub(super) fn explosion(residue: ExplosionResidue) -> Self {
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
    pub(super) fn advance(&self) -> AnimationAdvance {
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
pub(super) enum AnimationAdvance {
    /// The actor is idle and should inspect the current world.
    Ready,
    /// The next frame should replace the current cell state.
    Frame(Animation),
    /// The final frame promises a semantic transition now.
    Finished(AnimationNext),
}
