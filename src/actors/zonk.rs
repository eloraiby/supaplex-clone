//! Zonk gravity, delayed falls, and left-first rounded-support rolls.

use super::murphy::murphy_is_protected_from_falling_actor;
use super::rounded::{RoundedActor, RoundedPhase};
use super::{
    Actor, CellWrite, Direction, Frame, GameEvent, Horizontal, OrangeDisk, Position, State,
    Transition, explode_at,
};
use crate::game::{SoundEffect, WorldView};

/// A rounded rock that falls, rolls, can be pushed, and can crush actors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Zonk {
    /// The complete legal physical phase; momentum is derived from this value.
    phase: RoundedPhase,
}

impl Zonk {
    /// Creates this actor from a phase belonging to rounded-object physics.
    ///
    /// A player phase cannot be assigned to a rock.
    /// ```compile_fail
    /// use supaplex_clone::actors::{Zonk, murphy::MurphyPhase};
    /// let rock = Zonk::from_phase(MurphyPhase::Ready);
    /// ```
    pub const fn from_phase(phase: RoundedPhase) -> Self {
        Self { phase }
    }

    /// Returns the physical state for typed dispatch and collision queries.
    pub const fn phase(self) -> RoundedPhase {
        self.phase
    }

    /// Replaces only this concrete actor's physical phase.
    pub(super) fn in_phase(mut self, phase: RoundedPhase) -> State {
        self.phase = phase;
        State::new(Actor::Zonk(self))
    }

    /// Creates the momentum-bearing state used by completed falls and pushes.
    pub(super) const fn falling() -> Self {
        Self {
            phase: RoundedPhase::Momentum,
        }
    }

    /// Creates a stationary Zonk as read from a level record.
    pub const fn resting() -> Self {
        Self {
            phase: RoundedPhase::Resting,
        }
    }

    /// Reports whether this Zonk currently carries falling momentum.
    pub const fn is_falling(self) -> bool {
        self.phase.is_falling()
    }

    /// Chooses a fall or roll from the full states of neighboring cells.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        // Frozen Zonks keep both their position and their exact animation state.
        if world.freeze_zonks() {
            return None;
        }

        let below = world.offset(position, Direction::Down)?;
        if world.is_empty(below) {
            if self.phase.is_falling() {
                // Momentum from a completed fall continues directly into the
                // next cell. The one-update arming delay belongs only to a
                // stable Zonk beginning a new fall from rest.
                return Some(Transition::move_actor(
                    position,
                    below,
                    RoundedActor::Zonk(*self),
                ));
            }

            // The original engine changes a resting Zonk to pre-fall state
            // `0x41` on this callback and transfers it only on the following
            // callback. That distinction lets Murphy move first when his old
            // source opens directly below a trailing Zonk.
            return Some(Transition::replace(
                position,
                self.in_phase(RoundedPhase::AwaitingFall),
            ));
        }

        // Only a stable rounded support permits a diagonal roll. Inspecting the
        // support animation prevents rolling from a Zonk that is itself moving.
        if !world.is_rounded_stable_support(below) {
            return None;
        }

        for direction in [Horizontal::Left, Horizontal::Right] {
            let side = world.offset(position, direction.direction())?;
            let diagonal = world.offset(side, Direction::Down)?;
            if world.is_empty(side) && world.is_empty(diagonal) {
                return Some(Transition::prepare_rounded_roll(
                    position,
                    side,
                    RoundedActor::Zonk(*self),
                    direction,
                ));
            }
        }

        None
    }

    /// Starts the armed fall when its destination survived the intervening tick.
    pub(super) fn begin_fall(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
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
            RoundedActor::Zonk(Self {
                phase: RoundedPhase::Momentum,
            }),
        ))
    }
}

impl Zonk {
    /// Resolves crushes, landing sounds, and retained momentum after a full fall.
    pub(super) fn land(&self, position: Position, world: &WorldView<'_>) -> Transition {
        if world.freeze_zonks() {
            return Transition::replace(position, State::new(Actor::Zonk(Self::resting())));
        }
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
                            State::new(Actor::Zonk(Zonk::resting())),
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
                        let orange = OrangeDisk::resting()
                            .in_phase(super::orange_disk::OrangePhase::Fuse(Frame::first()));
                        return Transition::new(
                            vec![
                                CellWrite::new(position, State::new(Actor::Zonk(Zonk::resting()))),
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
                    RoundedActor::Zonk(Zonk::falling()),
                );
            }
            // Landing on a non-reactive occupant is the one safe Zonk
            // terminal path that selects the original Fall effect.
            return Transition::new(
                vec![CellWrite::new(
                    position,
                    State::new(Actor::Zonk(Zonk::resting())),
                )],
                vec![GameEvent::PlaySound(SoundEffect::Fall)],
            );
        }
        Transition::replace(position, self.in_phase(RoundedPhase::Momentum))
    }
}
