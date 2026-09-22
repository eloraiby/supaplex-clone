//! RamChipShape identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

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
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
