//! YellowDisk identity and scheduled behavior.

use super::{Position, Transition};
use crate::game::WorldView;

/// Pushable disk detonated by a Terminal's live row-major scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum YellowDisk {
    /// Ordinary level tile available for interaction.
    #[default]
    Resting,
    /// Retained until Murphy completes or cancels the current interaction.
    Held,
}

impl YellowDisk {
    /// Derives the static or reserved presentation from this actor's own state.
    pub(super) fn animation(self) -> super::Animation {
        match self {
            Self::Resting => super::Animation::idle(),
            Self::Held => super::Animation::view(super::AnimationKind::MurphyPushTarget, 0, 1),
        }
    }

    /// Remains stationary until Murphy pushes it or a Terminal detonates it.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}
