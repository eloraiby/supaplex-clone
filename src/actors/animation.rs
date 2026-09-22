//! Read-only presentation derived from actor-owned simulation phases.
//!
//! This value carries no completion command and cannot be written back to a
//! board cell. Actor-specific matches own all timing and gameplay transitions.

use super::{Direction, EnemyTurn, MurphyAnimation};

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

/// A sprite selection and interpolation position computed from a legal actor phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Animation {
    /// Sprite family chosen by the actor's exhaustive presentation match.
    kind: AnimationKind,
    /// Zero-based picture derived from the actor's bounded frame value.
    frame: u8,
    /// Strip length fixed by the actor phase or its checked cooldown.
    frame_count: u8,
}

impl Animation {
    /// Builds a presentation value; it has no path back into simulation state.
    pub(super) const fn view(kind: AnimationKind, frame: u8, frame_count: u8) -> Self {
        Self {
            kind,
            frame,
            frame_count,
        }
    }

    /// Describes an inert actor's static sprite.
    pub const fn idle() -> Self {
        Self::view(AnimationKind::Idle, 0, 1)
    }

    /// Returns the sprite family selected by the actor.
    pub const fn kind(&self) -> AnimationKind {
        self.kind
    }

    /// Returns the current sprite coordinate index.
    pub const fn frame(&self) -> u8 {
        self.frame
    }

    /// Returns the number of pictures used for interpolation.
    pub const fn frame_count(&self) -> u8 {
        self.frame_count
    }

    /// Returns normalized interpolation, treating static sprites as complete.
    pub fn progress(&self) -> f32 {
        match self.frame_count {
            0 | 1 => 1.0,
            count => f32::from(self.frame) / f32::from(count - 1),
        }
    }
}
