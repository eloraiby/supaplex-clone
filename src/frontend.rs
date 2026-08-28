//! Deterministic timing and selection state for non-gameplay screens.
//!
//! SDL rendering and event polling stay in the binary and renderer modules.
//! Keeping timing calculations here makes the original palette-fade cadence
//! testable without opening a window or depending on a display refresh rate.

use std::time::Duration;

/// Duration of the original 64-frame palette fade on a 70 Hz display.
pub const ORIGINAL_FADE_DURATION: Duration = Duration::from_millis(64 * 1_000 / 70);

/// Time the fully visible title remains on screen before its exit fade.
const SPLASH_HOLD_DURATION: Duration = Duration::from_millis(1_000);

/// One rendered title-screen instant expressed as a black overlay opacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplashFrame {
    /// Black opacity over the title, from transparent zero to opaque 255.
    pub black_opacity: u8,
    /// Whether the complete fade-in, hold, and fade-out sequence has elapsed.
    pub finished: bool,
}

/// Valid one-based level selection maintained by the main-menu input loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuSelection {
    /// Currently highlighted one-based level number.
    selected_level: usize,
    /// Inclusive upper bound obtained from the validated level collection.
    level_count: usize,
}

impl MenuSelection {
    /// Creates a selection clamped to a non-empty level collection.
    pub fn new(initial_level: usize, level_count: usize) -> Option<Self> {
        // A zero-sized collection has no valid one-based selection. Otherwise,
        // clamping makes this state safe for callers other than the validated CLI.
        if level_count == 0 {
            return None;
        }
        Some(Self {
            selected_level: initial_level.clamp(1, level_count),
            level_count,
        })
    }

    /// Returns the currently highlighted one-based level number.
    pub const fn selected_level(self) -> usize {
        // The private fields and mutation methods maintain `1..=level_count`.
        self.selected_level
    }

    /// Returns the preceding level number when the selection is not first.
    pub fn previous_level(self) -> Option<usize> {
        // `checked_sub` expresses the absence of a row above level one without
        // introducing a synthetic level zero into rendering code.
        self.selected_level
            .checked_sub(1)
            .filter(|level| *level >= 1)
    }

    /// Returns the following level number when the selection is not last.
    pub fn next_level(self) -> Option<usize> {
        // The explicit bound prevents the menu from asking `LevelSet` for the
        // sentinel record after the final playable level.
        (self.selected_level < self.level_count).then_some(self.selected_level + 1)
    }

    /// Moves by a signed number of rows while clamping at both ends.
    pub fn move_by(&mut self, offset: isize) {
        // Saturating arithmetic handles Page Up at the first level and avoids
        // converting a negative value to an enormous unsigned index.
        self.selected_level = if offset.is_negative() {
            self.selected_level
                .saturating_sub(offset.unsigned_abs())
                .max(1)
        } else {
            self.selected_level
                .saturating_add(offset as usize)
                .min(self.level_count)
        };
    }

    /// Selects the first playable level directly.
    pub fn select_first(&mut self) {
        // One is always valid because construction rejects an empty collection.
        self.selected_level = 1;
    }

    /// Selects the final playable level directly.
    pub fn select_last(&mut self) {
        // Retaining the validated count avoids duplicating knowledge of the
        // original 111-level collection in the event adapter.
        self.selected_level = self.level_count;
    }
}

/// Returns black opacity for a screen fading in over the original duration.
pub fn fade_in_opacity(elapsed: Duration) -> u8 {
    // Once elapsed time reaches the duration, the subtraction yields zero and
    // subsequent frames remain fully visible without special state handling.
    u8::MAX - fade_component(elapsed, ORIGINAL_FADE_DURATION)
}

/// Returns black opacity for a screen fading out over the original duration.
pub fn fade_out_opacity(elapsed: Duration) -> u8 {
    // The shared component reaches 255 at and after the historical duration,
    // leaving the completed transition fully black until its caller changes state.
    fade_component(elapsed, ORIGINAL_FADE_DURATION)
}

/// Calculates the title frame corresponding to an elapsed wall-clock duration.
pub fn splash_frame(elapsed: Duration) -> SplashFrame {
    // The opening half fades black away, the middle preserves the decoded title
    // unchanged, and the closing half returns to black for the following menu.
    if elapsed < ORIGINAL_FADE_DURATION {
        return SplashFrame {
            black_opacity: u8::MAX - fade_component(elapsed, ORIGINAL_FADE_DURATION),
            finished: false,
        };
    }

    let fade_out_start = ORIGINAL_FADE_DURATION + SPLASH_HOLD_DURATION;
    if elapsed < fade_out_start {
        return SplashFrame {
            black_opacity: 0,
            finished: false,
        };
    }

    let fade_out_elapsed = elapsed.saturating_sub(fade_out_start);
    if fade_out_elapsed < ORIGINAL_FADE_DURATION {
        return SplashFrame {
            black_opacity: fade_component(fade_out_elapsed, ORIGINAL_FADE_DURATION),
            finished: false,
        };
    }

    // Leave the terminal sample opaque so a caller can replace the screen
    // without a one-frame flash of the title between state-machine iterations.
    SplashFrame {
        black_opacity: u8::MAX,
        finished: true,
    }
}

/// Maps a partial duration monotonically onto the inclusive alpha range 0..=255.
fn fade_component(elapsed: Duration, duration: Duration) -> u8 {
    // Integer arithmetic makes boundary samples stable in tests and avoids a
    // platform-dependent floating-point truncation near full opacity.
    let elapsed = elapsed.min(duration).as_nanos();
    let duration = duration.as_nanos().max(1);
    u8::try_from(elapsed * u128::from(u8::MAX) / duration)
        .expect("clamped fade component is always within one byte")
}

#[cfg(test)]
mod tests {
    //! Boundary checks for the title sequence's three timing phases.

    use super::{
        MenuSelection, ORIGINAL_FADE_DURATION, SPLASH_HOLD_DURATION, SplashFrame, fade_in_opacity,
        fade_out_opacity, splash_frame,
    };
    use std::time::Duration;

    /// Confirms the title starts and ends behind an opaque black palette state.
    #[test]
    fn splash_sequence_has_black_terminal_frames() {
        let complete = ORIGINAL_FADE_DURATION * 2 + SPLASH_HOLD_DURATION;

        assert_eq!(
            splash_frame(Duration::ZERO),
            SplashFrame {
                black_opacity: u8::MAX,
                finished: false,
            }
        );
        assert_eq!(
            splash_frame(complete),
            SplashFrame {
                black_opacity: u8::MAX,
                finished: true,
            }
        );
    }

    /// Confirms the hold phase exposes the unmodified title without an overlay.
    #[test]
    fn splash_hold_is_fully_visible() {
        let hold_middle = ORIGINAL_FADE_DURATION + SPLASH_HOLD_DURATION / 2;

        assert_eq!(splash_frame(hold_middle).black_opacity, 0);
        assert!(!splash_frame(hold_middle).finished);
    }

    /// Confirms both fade halves move in opposite monotonic directions.
    #[test]
    fn splash_fades_are_monotonic() {
        let quarter = ORIGINAL_FADE_DURATION / 4;
        let opening_early = splash_frame(quarter).black_opacity;
        let opening_late = splash_frame(quarter * 3).black_opacity;
        let closing_start = ORIGINAL_FADE_DURATION + SPLASH_HOLD_DURATION;
        let closing_early = splash_frame(closing_start + quarter).black_opacity;
        let closing_late = splash_frame(closing_start + quarter * 3).black_opacity;

        assert!(opening_early > opening_late);
        assert!(closing_early < closing_late);
    }

    /// Confirms menu movement clamps and never exposes non-level sentinel rows.
    #[test]
    fn menu_selection_clamps_navigation_to_the_collection() {
        let mut selection = MenuSelection::new(1, 111).expect("collection is non-empty");

        selection.move_by(-10);
        assert_eq!(selection.selected_level(), 1);
        assert_eq!(selection.previous_level(), None);
        assert_eq!(selection.next_level(), Some(2));

        selection.select_last();
        selection.move_by(10);
        assert_eq!(selection.selected_level(), 111);
        assert_eq!(selection.previous_level(), Some(110));
        assert_eq!(selection.next_level(), None);

        selection.select_first();
        assert_eq!(selection.selected_level(), 1);
    }

    /// Confirms initial selections are clamped and empty collections are rejected.
    #[test]
    fn menu_selection_validates_construction() {
        assert_eq!(
            MenuSelection::new(0, 111).map(MenuSelection::selected_level),
            Some(1)
        );
        assert_eq!(
            MenuSelection::new(999, 111).map(MenuSelection::selected_level),
            Some(111)
        );
        assert_eq!(MenuSelection::new(1, 0), None);
    }

    /// Confirms reusable fade-in opacity reaches transparent black at completion.
    #[test]
    fn fade_in_reaches_fully_visible() {
        assert_eq!(fade_in_opacity(Duration::ZERO), u8::MAX);
        assert_eq!(fade_in_opacity(ORIGINAL_FADE_DURATION), 0);
        assert_eq!(fade_in_opacity(ORIGINAL_FADE_DURATION * 2), 0);
    }

    /// Confirms reusable fade-out opacity reaches and retains opaque black.
    #[test]
    fn fade_out_reaches_fully_black() {
        assert_eq!(fade_out_opacity(Duration::ZERO), 0);
        assert_eq!(fade_out_opacity(ORIGINAL_FADE_DURATION), u8::MAX);
        assert_eq!(fade_out_opacity(ORIGINAL_FADE_DURATION * 2), u8::MAX);
    }
}
