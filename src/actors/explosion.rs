//! Explosion residue and ordered blast propagation with reservation cleanup.

use super::{
    Actor, Animation, AnimationKind, CellWrite, Direction, GameEvent, Position, State, Transition,
};
use crate::game::SoundEffect;
use crate::game::WorldView;

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
    /// Bounded progress through the eight quarter-rate pictures.
    frame: super::Frame<8>,
}

impl Explosion {
    /// Creates one explosion cell with an explicit terminal residue.
    pub const fn new(residue: ExplosionResidue) -> Self {
        Self {
            residue,
            frame: super::Frame::first(),
        }
    }

    /// Returns what this explosion cell will become after its final frame.
    pub const fn residue(self) -> ExplosionResidue {
        self.residue
    }

    /// Reports the original safe tail of a normal explosion for Murphy movement.
    pub const fn is_harmless(self) -> bool {
        matches!(self.residue, ExplosionResidue::Empty) && self.frame.index() >= 4
    }

    /// Selects normal or Electron artwork from the same residue used at completion.
    pub(super) fn animation(self) -> Animation {
        let kind = match self.residue {
            ExplosionResidue::Empty => AnimationKind::Explosion,
            ExplosionResidue::Infotron => AnimationKind::ElectronExplosion,
        };
        Animation::view(kind, self.frame.index(), 8)
    }

    /// Advances the blast on quarter ticks and installs its typed residue at completion.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        if !world.tick_count().is_multiple_of(4) {
            return None;
        }
        Some(match self.frame.next() {
            Some(frame) => Transition::replace(
                position,
                State::new(Actor::Explosion(Self { frame, ..*self })),
            ),
            None => {
                let state = match self.residue {
                    ExplosionResidue::Empty => State::empty(),
                    ExplosionResidue::Infotron => {
                        State::new(Actor::Infotron(super::Infotron::resting()))
                    }
                };
                Transition::new(
                    vec![CellWrite::new(position, state)],
                    vec![GameEvent::ExplosionFinished],
                )
            }
        })
    }
}

/// Creates one visual explosion cell independently of all secondary-wave timers.
fn explosion_state(residue: ExplosionResidue) -> State {
    let actor = Actor::Explosion(Explosion::new(residue));
    State::new(actor)
}

/// Finds the temporary cell owned by one moving Zonk or Infotron phase.
///
/// The original blast dispatcher decodes the actor's state high nibble and
/// clears the corresponding old, side, or diagonal Space marker. Typed
/// physical phases carry that same topology here, so cleanup stays phase-specific
/// instead of reviving autonomous behavior in temporary Empty cells.
fn rounded_actor_reservation(
    world: &WorldView<'_>,
    position: Position,
    state: &State,
) -> Option<Position> {
    use super::{empty::Reservation, rounded::RoundedPhase};

    let phase = match state.actor() {
        Actor::Zonk(actor) => actor.phase(),
        Actor::Infotron(actor) => actor.phase(),
        _ => return None,
    };
    let reservation = match phase {
        RoundedPhase::Falling(_) => world.offset(position, Direction::Up)?,
        RoundedPhase::PreparingRoll(direction) => world.offset(position, direction.direction())?,
        RoundedPhase::Rolling { .. } => world.offset(position, Direction::Down)?,
        RoundedPhase::Resting
        | RoundedPhase::Momentum
        | RoundedPhase::AwaitingFall
        | RoundedPhase::Held => return None,
    };
    // Cross-cell ownership must still be checked against the live board: an
    // earlier blast or mover may have legitimately replaced this marker.
    let marker = world.state(reservation)?.reservation()?;
    matches!(
        (phase, marker),
        (
            RoundedPhase::Falling(_),
            Reservation::Vacating {
                direction: Direction::Down,
                ..
            }
        ) | (RoundedPhase::PreparingRoll(_), Reservation::RoundedSide)
            | (
                RoundedPhase::Rolling { .. },
                Reservation::RoundedDestination
            )
    )
    .then_some(reservation)
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
    let mut rounded_cleanup = Vec::new();
    // The original engine uses one global flag for explosion sound and camera
    // shake rather than counting live cells.  Every emitted wave sets it again.
    let mut events = vec![
        GameEvent::ExplosionStarted,
        GameEvent::PlaySound(SoundEffect::Explosion),
    ];

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

            if let Some(reservation) = rounded_actor_reservation(world, position, state)
                && !rounded_cleanup.contains(&reservation)
            {
                rounded_cleanup.push(reservation);
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

    // A reservation inside the 3x3 footprint has already become Explosion and
    // must win over cleanup, just as the original helper preserves a blast it
    // encounters. Only out-of-wave markers become ordinary empty space.
    for reservation in rounded_cleanup {
        if !writes.iter().any(|write| write.position == reservation) {
            writes.push(CellWrite::new(reservation, State::empty()));
        }
    }

    Transition::blast(writes, events)
}
