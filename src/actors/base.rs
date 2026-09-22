//! Base identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Diggable green circuit-board material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Base {
    /// Ordinary level tile available for interaction.
    #[default]
    Resting,
    /// Retained until Murphy completes or cancels the current interaction.
    Held,
}

impl Base {
    /// Derives the static or reserved presentation from this actor's own state.
    pub(super) fn animation(self) -> super::Animation {
        match self {
            Self::Resting => super::Animation::idle(),
            Self::Held => super::Animation::view(super::AnimationKind::MurphyPushTarget, 0, 1),
        }
    }

    /// Remains in place until Murphy or an explosion replaces it atomically.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
