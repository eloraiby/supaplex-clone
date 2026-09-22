//! Base identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Diggable green circuit-board material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Base;

impl Base {
    /// Remains in place until Murphy or an explosion replaces it atomically.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
