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

/// Integer enlargement used for every original 320×200 front-end screen.
const ORIGINAL_SCREEN_SCALE: i32 = 3;

/// Logical top edge of the centered 600-pixel-high original screen.
const ORIGINAL_SCREEN_Y: i32 = 20;

/// One inclusive rectangle expressed in original 320×200 coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OriginalRect {
    /// Inclusive horizontal coordinate of the rectangle's left edge.
    left: i32,
    /// Inclusive vertical coordinate of the rectangle's top edge.
    top: i32,
    /// Inclusive horizontal coordinate of the rectangle's right edge.
    right: i32,
    /// Inclusive vertical coordinate of the rectangle's bottom edge.
    bottom: i32,
}

impl OriginalRect {
    /// Creates one immutable inclusive original-coordinate rectangle.
    const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        // Keeping descriptor construction in one place makes the historical
        // inclusive edge convention visible instead of relying on SDL widths.
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Reports whether one original-coordinate point lies inside the rectangle.
    const fn contains(self, x: i32, y: i32) -> bool {
        // The DOS menu compares both edges inclusively, so adjacent controls
        // must retain their exact source-table boundaries here.
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }

    /// Exposes the rectangle as an origin plus inclusive width and height.
    const fn dimensions(self) -> (i32, i32, u32, u32) {
        // Every descriptor is ordered, making the one-pixel inclusive adjustment
        // safe and directly usable by SDL outline drawing after scaling.
        (
            self.left,
            self.top,
            (self.right - self.left + 1) as u32,
            (self.bottom - self.top + 1) as u32,
        )
    }
}

/// Action represented by one of the original main-menu mouse regions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainMenuTarget {
    /// Create and select a new eight-character player profile.
    NewPlayer,
    /// Delete the currently selected player after confirmation.
    DeletePlayer,
    /// Spend one of the current player's three level skips.
    SkipLevel,
    /// Open the current player's statistics information page.
    Statistics,
    /// Open the illustrated actor and hardware tutorial.
    GfxTutor,
    /// Start one of the ten supplied original demonstrations.
    Demo,
    /// Open the audio and input controls/options screen.
    Controls,
    /// Move the visible ranking window toward earlier entries.
    RankingUp,
    /// Move the visible ranking window toward later entries.
    RankingDown,
    /// Start the selected playable level or confirm a pending menu operation.
    Ok,
    /// Rotate to the next external level set represented by the floppy button.
    LevelSet,
    /// Select the preceding player profile.
    PlayerUp,
    /// Select the following player profile.
    PlayerDown,
    /// Select a player row directly from the three-row list.
    PlayerList,
    /// Select the preceding level.
    LevelUp,
    /// Select the following level.
    LevelDown,
    /// Open the original level-design credits page.
    Credits,
}

impl MainMenuTarget {
    /// Returns this target's original inclusive hit rectangle as an SDL-style box.
    pub const fn original_bounds(self) -> (i32, i32, u32, u32) {
        // Looking up the same descriptor used by hit testing prevents hover
        // feedback from drifting away from the clickable region.
        main_menu_descriptor(self).dimensions()
    }
}

/// Action represented by one original controls/options mouse region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlsTarget {
    /// Historical AdLib hardware choice, mapped to the available music player.
    Adlib,
    /// Historical Sound Blaster hardware choice, mapped to effects plus music.
    SoundBlaster,
    /// Historical Roland hardware choice, mapped to the available music player.
    Roland,
    /// Historical combined device choice, mapped to effects plus music.
    Combined,
    /// Historical internal speaker device family.
    Internal,
    /// Historical standard internal-speaker synthesis mode.
    Standard,
    /// Historical sampled internal-speaker synthesis mode.
    Samples,
    /// Toggle tracker music independently of effects.
    Music,
    /// Toggle gameplay effects independently of music.
    Effects,
    /// Select keyboard input, the clone's supported gameplay device.
    Keyboard,
    /// Select joystick input when a compatible controller is available.
    Joystick,
    /// Return to the main menu through either original exit region.
    Exit,
}

/// Original main-menu descriptors in the exact first-match order used by DOS.
const MAIN_MENU_DESCRIPTORS: [(OriginalRect, MainMenuTarget); 17] = [
    (OriginalRect::new(5, 6, 157, 14), MainMenuTarget::NewPlayer),
    (
        OriginalRect::new(5, 15, 157, 23),
        MainMenuTarget::DeletePlayer,
    ),
    (OriginalRect::new(5, 24, 157, 32), MainMenuTarget::SkipLevel),
    (
        OriginalRect::new(5, 33, 157, 41),
        MainMenuTarget::Statistics,
    ),
    (OriginalRect::new(5, 42, 157, 50), MainMenuTarget::GfxTutor),
    (OriginalRect::new(5, 51, 157, 59), MainMenuTarget::Demo),
    (OriginalRect::new(5, 60, 157, 69), MainMenuTarget::Controls),
    (
        OriginalRect::new(140, 90, 155, 108),
        MainMenuTarget::RankingUp,
    ),
    (
        OriginalRect::new(140, 121, 155, 138),
        MainMenuTarget::RankingDown,
    ),
    (OriginalRect::new(96, 140, 115, 163), MainMenuTarget::Ok),
    (
        OriginalRect::new(83, 168, 126, 192),
        MainMenuTarget::LevelSet,
    ),
    (
        OriginalRect::new(11, 142, 67, 153),
        MainMenuTarget::PlayerUp,
    ),
    (
        OriginalRect::new(11, 181, 67, 192),
        MainMenuTarget::PlayerDown,
    ),
    (
        OriginalRect::new(11, 154, 67, 180),
        MainMenuTarget::PlayerList,
    ),
    (
        OriginalRect::new(142, 142, 306, 153),
        MainMenuTarget::LevelUp,
    ),
    (
        OriginalRect::new(142, 181, 306, 192),
        MainMenuTarget::LevelDown,
    ),
    (OriginalRect::new(297, 37, 312, 52), MainMenuTarget::Credits),
];

/// Original controls descriptors in the exact first-match order used by DOS.
const CONTROLS_DESCRIPTORS: [(OriginalRect, ControlsTarget); 13] = [
    (OriginalRect::new(12, 13, 107, 36), ControlsTarget::Adlib),
    (
        OriginalRect::new(12, 49, 107, 72),
        ControlsTarget::SoundBlaster,
    ),
    (OriginalRect::new(12, 85, 107, 108), ControlsTarget::Roland),
    (
        OriginalRect::new(12, 121, 107, 144),
        ControlsTarget::Combined,
    ),
    (
        OriginalRect::new(132, 13, 211, 31),
        ControlsTarget::Internal,
    ),
    (
        OriginalRect::new(126, 43, 169, 54),
        ControlsTarget::Standard,
    ),
    (OriginalRect::new(174, 43, 217, 54), ControlsTarget::Samples),
    (OriginalRect::new(132, 86, 175, 120), ControlsTarget::Music),
    (
        OriginalRect::new(134, 132, 168, 152),
        ControlsTarget::Effects,
    ),
    (
        OriginalRect::new(201, 80, 221, 154),
        ControlsTarget::Keyboard,
    ),
    (
        OriginalRect::new(233, 80, 252, 154),
        ControlsTarget::Joystick,
    ),
    (OriginalRect::new(0, 181, 319, 199), ControlsTarget::Exit),
    (OriginalRect::new(284, 0, 319, 180), ControlsTarget::Exit),
];

/// Maps one logical-window point to its original 320×200 screen position.
fn original_screen_point(logical_x: i32, logical_y: i32) -> Option<(i32, i32)> {
    // The original picture is centered vertically in a 960×640 logical
    // viewport. Rejecting both letterbox bars avoids negative integer division
    // turning a click above the picture into a false row-zero hit.
    let relative_y = logical_y - ORIGINAL_SCREEN_Y;
    let screen_height = 200 * ORIGINAL_SCREEN_SCALE;
    if !(0..screen_height).contains(&relative_y)
        || !(0..320 * ORIGINAL_SCREEN_SCALE).contains(&logical_x)
    {
        return None;
    }
    Some((
        logical_x / ORIGINAL_SCREEN_SCALE,
        relative_y / ORIGINAL_SCREEN_SCALE,
    ))
}

/// Returns the original main-menu control under one logical-window point.
pub fn main_menu_target_at(logical_x: i32, logical_y: i32) -> Option<MainMenuTarget> {
    // SDL transforms window mouse events into the configured 960×640 logical
    // space. This final division reaches the descriptor table's DOS coordinates.
    let (x, y) = original_screen_point(logical_x, logical_y)?;
    MAIN_MENU_DESCRIPTORS
        .iter()
        .find_map(|(bounds, target)| bounds.contains(x, y).then_some(*target))
}

/// Returns the original controls-screen control under one logical-window point.
pub fn controls_target_at(logical_x: i32, logical_y: i32) -> Option<ControlsTarget> {
    // Both screens share the same centered three-times transform, so only the
    // ordered descriptor table differs from main-menu hit testing.
    let (x, y) = original_screen_point(logical_x, logical_y)?;
    CONTROLS_DESCRIPTORS
        .iter()
        .find_map(|(bounds, target)| bounds.contains(x, y).then_some(*target))
}

/// Retrieves the historical rectangle belonging to one main-menu target.
const fn main_menu_descriptor(target: MainMenuTarget) -> OriginalRect {
    // A direct match remains usable in a const context and makes missing enum
    // variants a compile-time error if the public target set changes.
    match target {
        MainMenuTarget::NewPlayer => OriginalRect::new(5, 6, 157, 14),
        MainMenuTarget::DeletePlayer => OriginalRect::new(5, 15, 157, 23),
        MainMenuTarget::SkipLevel => OriginalRect::new(5, 24, 157, 32),
        MainMenuTarget::Statistics => OriginalRect::new(5, 33, 157, 41),
        MainMenuTarget::GfxTutor => OriginalRect::new(5, 42, 157, 50),
        MainMenuTarget::Demo => OriginalRect::new(5, 51, 157, 59),
        MainMenuTarget::Controls => OriginalRect::new(5, 60, 157, 69),
        MainMenuTarget::RankingUp => OriginalRect::new(140, 90, 155, 108),
        MainMenuTarget::RankingDown => OriginalRect::new(140, 121, 155, 138),
        MainMenuTarget::Ok => OriginalRect::new(96, 140, 115, 163),
        MainMenuTarget::LevelSet => OriginalRect::new(83, 168, 126, 192),
        MainMenuTarget::PlayerUp => OriginalRect::new(11, 142, 67, 153),
        MainMenuTarget::PlayerDown => OriginalRect::new(11, 181, 67, 192),
        MainMenuTarget::PlayerList => OriginalRect::new(11, 154, 67, 180),
        MainMenuTarget::LevelUp => OriginalRect::new(142, 142, 306, 153),
        MainMenuTarget::LevelDown => OriginalRect::new(142, 181, 306, 192),
        MainMenuTarget::Credits => OriginalRect::new(297, 37, 312, 52),
    }
}

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
        ControlsTarget, MainMenuTarget, MenuSelection, ORIGINAL_FADE_DURATION,
        SPLASH_HOLD_DURATION, SplashFrame, controls_target_at, fade_in_opacity, fade_out_opacity,
        main_menu_target_at, splash_frame,
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

    /// Confirms centered logical coordinates reach representative menu regions.
    #[test]
    fn main_menu_hit_testing_accounts_for_scale_and_letterbox() {
        assert_eq!(
            main_menu_target_at(15, 20 + 18),
            Some(MainMenuTarget::NewPlayer)
        );
        assert_eq!(main_menu_target_at(300, 20 + 450), Some(MainMenuTarget::Ok));
        assert_eq!(
            main_menu_target_at(900, 20 + 120),
            Some(MainMenuTarget::Credits)
        );
        assert_eq!(main_menu_target_at(500, 10), None);
        assert_eq!(main_menu_target_at(959, 639), None);
    }

    /// Confirms inclusive source edges and overlapping exit regions remain usable.
    #[test]
    fn controls_hit_testing_preserves_original_edges() {
        assert_eq!(controls_target_at(36, 20 + 39), Some(ControlsTarget::Adlib));
        assert_eq!(
            controls_target_at(396, 20 + 258),
            Some(ControlsTarget::Music)
        );
        assert_eq!(controls_target_at(0, 20 + 597), Some(ControlsTarget::Exit));
        assert_eq!(controls_target_at(957, 20), Some(ControlsTarget::Exit));
        assert_eq!(controls_target_at(330, 20 + 300), None);
    }
}
