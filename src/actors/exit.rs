//! Exit identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Locked goal tile that completes a level after all required Infotrons.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Exit;

impl Exit {
    /// Remains stationary; Murphy owns the interaction and completion event.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
