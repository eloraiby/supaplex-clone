//! RedDisk identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Collectible and droppable explosive disk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RedDisk;

impl RedDisk {
    /// Remains inert unless its animation's promised fuse transition fires.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
