//! Hardware identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Indestructible hardware with one of the original decorative appearances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hardware {
    /// Raw visual variant in the inclusive range `0..=10`.
    variant: u8,
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
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
