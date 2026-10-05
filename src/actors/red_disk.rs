//! Loose collectible disks and the visible actor over a cell-owned fuse.

use super::{Position, Transition};
use crate::game::WorldView;

/// Legal Red Disk states; planting cannot carry an enemy or player animation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RedDisk {
    /// Loose disk available for collection.
    #[default]
    Collectible,
    /// Retained while Murphy's adjacent collection strip finishes.
    Held,
    /// Visible fuse whose countdown belongs to its board cell.
    Planted,
}

impl RedDisk {
    /// Leaves countdown ownership with the cell, including when hidden by Murphy.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
