//! Loose collectible disks and the visible portion of a session-owned fuse.

use super::{Frame, Position, Transition};
use crate::game::WorldView;

/// Legal Red Disk states; planting cannot carry an enemy or player animation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RedDisk {
    /// Loose disk available for collection.
    #[default]
    Collectible,
    /// Retained while Murphy's adjacent collection strip finishes.
    Held,
    /// Visible fuse frame; the game owns the timer even while Murphy covers it.
    Planted(Frame<40>),
}

impl RedDisk {
    /// Converts the original session countdown to its bounded visible frame.
    pub(super) fn planted(frame: u8) -> Self {
        Self::Planted(Frame::new(frame.min(39)).unwrap())
    }

    /// Leaves countdown ownership with the game, including when hidden by Murphy.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
