//! InvisibleWall identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Hidden indestructible wall used by extended classic level files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InvisibleWall;

impl InvisibleWall {
    /// Never changes and is deliberately rendered as empty for its whole lifetime.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        // Tile 40 is an accidental but historically adopted collision wall. It
        // has no reveal state: Murphy merely fails to enter the invisible cell.
        None
    }
}
