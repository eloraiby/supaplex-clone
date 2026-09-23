//! Cadence-gated Bug activity and safe intervals selected by the session RNG.

use super::{Actor, CellWrite, Frame, GameEvent, Position, State, Transition};
use crate::game::{SoundEffect, WorldView};

/// A safe interval whose private fields preserve `elapsed < duration`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cooldown {
    /// Number of quarter ticks already consumed.
    elapsed: u8,
    /// Positive length chosen by the original random-delay calculation.
    duration: std::num::NonZeroU8,
}

impl Cooldown {
    /// Returns consumed quarter ticks for inspection without exposing mutation.
    pub const fn elapsed(self) -> u8 {
        self.elapsed
    }

    /// Returns the validated positive interval length.
    pub const fn duration(self) -> u8 {
        self.duration.get()
    }
}

/// The complete set of legal Bug phases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bug {
    /// Fourteen lethal pictures, advanced only on quarter ticks.
    Active(Frame<14>),
    /// Safe interval whose expiry returns to active picture zero.
    Dormant(Cooldown),
    /// Safe Bug retained while Murphy completes an adjacent snap.
    Held,
}

impl Bug {
    /// Creates the synchronized active phase used by newly loaded levels.
    pub const fn new() -> Self {
        Self::Active(Frame::first())
    }

    /// Starts an independently randomized, strictly positive safe interval.
    pub(super) fn dormant(delay: u8) -> Self {
        Self::Dormant(Cooldown {
            elapsed: 0,
            duration: std::num::NonZeroU8::new(delay).expect("Bug cooldown must be positive"),
        })
    }

    /// Advances on quarter ticks and asks the game to consume RNG in board order.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        if !world.tick_count().is_multiple_of(4) {
            return None;
        }
        let next = match *self {
            Self::Held => return None,
            Self::Active(frame) => match frame.next() {
                Some(frame) => Self::Active(frame),
                None => {
                    return Some(Transition::new(
                        Vec::new(),
                        vec![GameEvent::RandomizeBug(position)],
                    ));
                }
            },
            Self::Dormant(mut cooldown) => {
                cooldown.elapsed += 1;
                match cooldown.elapsed < cooldown.duration.get() {
                    true => Self::Dormant(cooldown),
                    false => Self::new(),
                }
            }
        };
        // Activation at picture zero chirps too; deferring to picture one loses a sound.
        let events = (matches!(next, Self::Active(_)) && world.has_neighboring_murphy(position))
            .then_some(GameEvent::PlaySound(SoundEffect::Bug))
            .into_iter()
            .collect();
        let transition = Transition::new(
            vec![CellWrite::new(position, State::new(Actor::Bug(next)))],
            events,
        );
        Some(match next {
            Self::Active(_) => transition.with_drawing(position, Actor::Bug(next)),
            Self::Dormant(_) | Self::Held => transition,
        })
    }
}

impl Default for Bug {
    /// Starts at the canonical active picture zero.
    fn default() -> Self {
        Self::new()
    }
}
