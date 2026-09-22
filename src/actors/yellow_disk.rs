//! YellowDisk identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Pushable disk detonated by a Terminal's live row-major scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YellowDisk;

impl YellowDisk {
    /// Remains stationary until Murphy pushes it or a Terminal detonates it.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
