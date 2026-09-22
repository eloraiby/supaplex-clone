//! Complete board-cell values and constructors for movement reservations.
//!
//! Actor identity and animation remain private so callers cannot independently
//! replace half of a cell. The actor family builds complete replacement values;
//! the game applies them through owned transitions.

use super::{
    Actor, Animation, AnimationKind, Bug, Direction, Electron, Empty, EnemyTurn, RedDisk, SnikSnak,
};

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
    pub(super) fn vacating(direction: Direction) -> Self {
        Self::animated(Actor::Empty(Empty), Animation::vacating(direction))
    }

    /// Creates a movement source whose release matches a Murphy action length.
    pub(super) fn vacating_for(direction: Direction, frame_count: u8) -> Self {
        Self::animated(
            Actor::Empty(Empty),
            Animation::vacating_for(direction, frame_count),
        )
    }

    /// Creates a stable non-empty reservation for a future Murphy destination.
    pub(super) fn murphy_destination() -> Self {
        Self::animated(Actor::Empty(Empty), Animation::murphy_destination())
    }

    /// Retains a pushable actor while suppressing its own fall/update behavior.
    pub(super) fn murphy_push_target(actor: Actor) -> Self {
        debug_assert!(matches!(
            actor,
            Actor::Zonk(_) | Actor::YellowDisk(_) | Actor::OrangeDisk(_)
        ));
        Self::animated(actor, Animation::murphy_push_target())
    }

    /// Creates the temporary side reservation used by rounded pre-roll.
    pub(super) fn rounded_side() -> Self {
        Self::animated(Actor::Empty(Empty), Animation::rounded_side())
    }

    /// Creates the diagonal reservation used by a horizontal rounded slide.
    pub(super) fn rounded_destination() -> Self {
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
    pub(super) fn animated(actor: Actor, animation: Animation) -> Self {
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
                self.animation.kind(),
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
        self.animation.kind() == AnimationKind::Idle
    }
}
