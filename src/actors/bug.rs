//! Bug identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// A Base-like hazard whose animation alternates safe and active frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Bug;

impl Bug {
    /// Has no movement; its repeating animation controls Murphy interactions.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
