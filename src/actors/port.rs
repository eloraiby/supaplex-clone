//! PortDirections identity and scheduled behavior.

use super::{Direction, Position, Transition};
use crate::game::WorldView;

/// Directional permissions represented by a port tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortDirections {
    /// Traversal is allowed only in one direction.
    OneWay(Direction),
    /// Traversal is allowed up or down.
    Vertical,
    /// Traversal is allowed left or right.
    Horizontal,
    /// Traversal is allowed in all four directions.
    Any,
}

/// A pass-through tile that may also change global physics settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Port {
    /// Directions Murphy may travel while crossing this tile.
    directions: PortDirections,
    /// Whether this tile consults a matching special-port metadata record.
    special: bool,
}

impl Port {
    /// Creates a regular or metadata-driven port.
    pub const fn new(directions: PortDirections, special: bool) -> Self {
        Self {
            directions,
            special,
        }
    }

    /// Returns the port's directional collision rule.
    pub const fn directions(self) -> PortDirections {
        self.directions
    }

    /// Reports whether crossing this port can update global settings.
    pub const fn is_special(self) -> bool {
        self.special
    }

    /// Reports whether a traversal direction is accepted by this port.
    pub const fn allows(self, direction: Direction) -> bool {
        match self.directions {
            PortDirections::OneWay(allowed) => direction as u8 == allowed as u8,
            PortDirections::Vertical => matches!(direction, Direction::Up | Direction::Down),
            PortDirections::Horizontal => matches!(direction, Direction::Left | Direction::Right),
            PortDirections::Any => true,
        }
    }

    /// Remains stationary because Murphy performs the complete traversal write.
    pub(super) fn transition(
        &self,
        _position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        None
    }
}

impl Port {
    /// Maps port permissions and metadata status back to a sprite-compatible code.
    pub(super) fn tile_code(self) -> u8 {
        match (self.directions, self.special) {
            (PortDirections::OneWay(Direction::Right), false) => 9,
            (PortDirections::OneWay(Direction::Down), false) => 10,
            (PortDirections::OneWay(Direction::Left), false) => 11,
            (PortDirections::OneWay(Direction::Up), false) => 12,
            (PortDirections::OneWay(Direction::Right), true) => 13,
            (PortDirections::OneWay(Direction::Down), true) => 14,
            (PortDirections::OneWay(Direction::Left), true) => 15,
            (PortDirections::OneWay(Direction::Up), true) => 16,
            (PortDirections::Vertical, _) => 21,
            (PortDirections::Horizontal, _) => 22,
            (PortDirections::Any, _) => 23,
        }
    }
}
