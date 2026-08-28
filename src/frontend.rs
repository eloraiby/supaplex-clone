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

    use super::{ORIGINAL_FADE_DURATION, SPLASH_HOLD_DURATION, SplashFrame, splash_frame};
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
}
