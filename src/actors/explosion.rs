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
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}

/// Creates one visual explosion cell independently of all secondary-wave timers.
fn explosion_state(residue: ExplosionResidue) -> State {
    let actor = Actor::Explosion(Explosion::new(residue));
    State::animated(actor, Animation::explosion(residue))
}

/// Finds the temporary cell owned by one moving Zonk or Infotron phase.
///
/// The original blast dispatcher decodes the actor's state high nibble and
/// clears the corresponding old, side, or diagonal Space marker. Typed
/// animations carry that same topology here, so cleanup stays phase-specific
/// instead of reviving autonomous behavior in temporary Empty cells.
fn rounded_actor_reservation(
    world: &WorldView<'_>,
    position: Position,
    state: &State,
) -> Option<Position> {
    if !matches!(state.actor(), Actor::Zonk(_) | Actor::Infotron(_)) {
        return None;
    }

    let (reservation, expected_kind) = match state.animation.kind {
        AnimationKind::Moving(Direction::Down) => (
            world.offset(position, Direction::Up)?,
            AnimationKind::Vacating(Direction::Down),
        ),
        AnimationKind::RoundedPreRoll(direction) => (
            world.offset(position, direction)?,
            AnimationKind::RoundedSide,
        ),
        AnimationKind::Rolling(_) => (
            world.offset(position, Direction::Down)?,
            AnimationKind::RoundedDestination,
        ),
        _ => return None,
    };

    world
        .state(reservation)
        .is_some_and(|reservation_state| {
            matches!(reservation_state.actor(), Actor::Empty(_))
                && reservation_state.animation.kind == expected_kind
        })
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
