//! OrangeDisk identity and scheduled behavior.

use super::{Actor, Animation, CellWrite, Direction, Position, State, Transition, explode_at};
use crate::game::WorldView;

/// Falling explosive disk that detonates when a fall reaches an obstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrangeDisk {
    /// Whether the disk has moved downward and is armed to explode on landing.
    falling: bool,
}

impl OrangeDisk {
    /// Creates the momentum-bearing state used by completed falls and pushes.
    pub(super) const fn falling() -> Self {
        Self { falling: true }
    }

    /// Creates a stable disk that has not begun a fall.
    pub const fn resting() -> Self {
        Self { falling: false }
    }

    /// Reports whether the disk has begun its irreversible fall.
    pub const fn is_falling(self) -> bool {
        self.falling
    }

    /// Falls through empty space or explodes after reaching an obstruction.
    pub(super) fn transition(
        &self,
        position: Position,
        world: &WorldView<'_>,
    ) -> Option<Transition> {
        let below = world.offset(position, Direction::Down)?;
        if world.is_empty(below) {
            // A resting Orange Disk installs the original state-0x20 delay and
            // reserves the cell below before any falling artwork is shown.
            return Some(Transition::new(
                vec![
                    CellWrite::new(
                        position,
                        State::animated(
                            Actor::OrangeDisk(Self { falling: true }),
                            Animation::orange_pre_fall(),
                        ),
                    ),
                    CellWrite::new(below, State::rounded_destination()),
                ],
                Vec::new(),
            ));
        }

        if self.falling {
            return Some(explode_at(world, position, false));
        }

        None
    }
}
