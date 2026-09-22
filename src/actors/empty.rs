//! Empty identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Empty space, which never changes itself during its scheduled update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Empty;

impl Empty {
    /// Leaves empty space unchanged; neighboring actors may still write here.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
