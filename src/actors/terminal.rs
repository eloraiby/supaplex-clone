//! Terminal identity and scheduled behavior.

use super::{Actor, GameEvent, Position, State, Transition};
use crate::game::WorldView;

/// Computer terminal with an independently delayed scrolling display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Terminal {
    /// Whether Murphy has already used this panel during the current level.
    activated: bool,
    /// Signed original-style counter incremented once per simulation update.
    delay: i8,
    /// FIXED.DAT scanline phase representing the screen's current scroll offset.
    screen_frame: u8,
}

impl Terminal {
    /// Creates an unused panel ready to choose its first randomized delay.
    pub const fn new() -> Self {
        Self {
            activated: false,
            delay: 0,
            screen_frame: 0,
        }
    }

    /// Returns a copy with the level-wide Yellow Disk latch marked as consumed.
    ///
    /// Activating a terminal must not reset its randomized delay or its current
    /// screen offset; the original panel continues scrolling after detonation.
    pub(crate) const fn activate(self) -> Self {
        Self {
            activated: true,
            ..self
        }
    }

    /// Reports whether this panel has already been used.
    pub const fn is_activated(self) -> bool {
        self.activated
    }

    /// Returns the current FIXED.DAT scanline phase of the scrolling screen.
    pub const fn screen_frame(self) -> u8 {
        self.screen_frame
    }

    /// Replaces the signed delay and advances the displayed scroll position.
    ///
    /// The game owns the shared pseudo-random stream, so the actor requests a
    /// randomized value through an event and receives the resulting state in a
    /// single row-ordered write.
    pub(crate) const fn after_scroll(self, delay: i8) -> Self {
        Self {
            delay,
            screen_frame: (self.screen_frame + 1) % 7,
            ..self
        }
    }

    /// Advances the original signed wait counter or requests one screen scroll.
    pub(super) fn transition(
        &self,
        position: Position,
        _world: &WorldView<'_>,
    ) -> Option<Transition> {
        // The original byte is interpreted as signed and incremented before it
        // is tested.  Negative and zero results continue waiting; a positive
        // result consumes the shared RNG and scrolls the terminal once.
        let next_delay = self.delay.wrapping_add(1);
        if next_delay <= 0 {
            let terminal = Self {
                delay: next_delay,
                ..*self
            };
            return Some(Transition::replace(
                position,
                State::new(Actor::Terminal(terminal)),
            ));
        }

        Some(Transition::new(
            Vec::new(),
            vec![GameEvent::RandomizeTerminal(position)],
        ))
    }
}

impl Default for Terminal {
    /// Creates the normal unused panel found in serialized levels.
    fn default() -> Self {
        Self::new()
    }
}
