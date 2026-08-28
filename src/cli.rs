//! Minimal parsing for initial menu selection, simulation rate, and music player.

use std::{error::Error, fmt};

/// The first level number accepted by the original 111-level collection.
pub const FIRST_LEVEL: usize = 1;

/// The final level number accepted by the original 111-level collection.
pub const LAST_LEVEL: usize = 111;

/// Initial main-menu level used when `--level` is omitted.
pub const DEFAULT_LEVEL: usize = FIRST_LEVEL;

/// Slowest supported simulation rate in fixed updates per second.
pub const FIRST_STEP_RATE: u32 = 5;

/// Fastest supported simulation rate in fixed updates per second.
pub const LAST_STEP_RATE: u32 = 60;

/// Original Supaplex and SpeedFix simulation rate used when `--step` is omitted.
pub const DEFAULT_STEP_RATE: u32 = 35;

/// Soundtrack implementation selected before the SDL audio device is opened.
///
/// Both players reproduce the same composition, but [`Self::Opl`] programs an
/// emulated Yamaha chip from the original DOS register stream while [`Self::Xm`]
/// plays the later sampled tracker conversion retained for comparison.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MusicPlayer {
    /// Native OPL2 synthesis using the original AdLib register writes.
    #[default]
    Opl,
    /// Four-channel sampled playback of the FastTracker XM conversion.
    Xm,
}

/// Validated options needed to initialize the front end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// One-based level number matching the numbering shown by Supaplex.
    level_number: usize,
    /// Validated number of fixed simulation updates performed per second.
    steps_per_second: u32,
    /// Soundtrack backend selected by `--player`, defaulting to native OPL2.
    music_player: MusicPlayer,
}

impl Options {
    /// Parses optional `--level`, `--step`, and `--player` argument pairs.
    ///
    /// The level selector defaults to level one and merely chooses the initial
    /// main-menu row; `--step` defaults to the original 35 updates per second;
    /// and `--player` accepts `opl` or `xm`, with OPL selected by default. Pairs
    /// may appear in any order. A deliberately small parser keeps startup
    /// dependencies light and every rejection deterministic.
    pub fn parse<I, S>(arguments: I) -> Result<Self, OptionsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // Consume flag/value pairs without allocating an argument collection.
        // Separate options remain order-independent, while duplicate flags are
        // rejected instead of silently letting the final value win.
        let mut arguments = arguments.into_iter().map(Into::into);
        let mut level_number = None;
        let mut steps_per_second = None;
        let mut music_player = None;
        while let Some(option) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| OptionsError::MissingValue(option.clone()))?;
            match option.as_str() {
                "--level" => {
                    if level_number.is_some() {
                        return Err(OptionsError::DuplicateOption("--level"));
                    }
                    level_number = Some(parse_level(value)?);
                }
                "--step" => {
                    if steps_per_second.is_some() {
                        return Err(OptionsError::DuplicateOption("--step"));
                    }
                    steps_per_second = Some(parse_step_rate(value)?);
                }
                "--player" => {
                    if music_player.is_some() {
                        return Err(OptionsError::DuplicateOption("--player"));
                    }
                    music_player = Some(parse_music_player(value)?);
                }
                _ => return Err(OptionsError::UnknownOption(option)),
            }
        }

        // Both defaults describe front-end startup rather than bypassing it:
        // level one is highlighted and the historical fixed rate is retained.
        Ok(Self {
            level_number: level_number.unwrap_or(DEFAULT_LEVEL),
            steps_per_second: steps_per_second.unwrap_or(DEFAULT_STEP_RATE),
            music_player: music_player.unwrap_or_default(),
        })
    }

    /// Returns the one-based level initially highlighted in the main menu.
    pub fn level_number(self) -> usize {
        // Copying the validated scalar cannot expose an invalid record index.
        self.level_number
    }

    /// Returns the selected number of fixed simulation updates per second.
    pub fn steps_per_second(self) -> u32 {
        // Parsing guarantees the inclusive 5..=60 contract before startup.
        self.steps_per_second
    }

    /// Returns the validated soundtrack backend chosen for this process.
    pub fn music_player(self) -> MusicPlayer {
        // The closed enum prevents arbitrary user text from reaching the audio
        // constructor or selecting a partially initialized callback backend.
        self.music_player
    }

    /// Returns a concise invocation string suitable for startup errors.
    pub fn usage() -> &'static str {
        // Every pair is optional because ordinary startup uses the original
        // menu defaults and the new native OPL soundtrack implementation.
        "Usage: supaplex-clone [--level <1-111>] [--step <5-60>] [--player <opl|xm>]"
    }
}

/// Parses and bounds-checks one level option value.
fn parse_level(value: String) -> Result<usize, OptionsError> {
    // Parsing as `usize` rejects negative and non-numeric text before applying
    // the original collection's one-based record bounds.
    let level_number = value
        .parse::<usize>()
        .map_err(|_| OptionsError::InvalidLevel(value))?;
    if !(FIRST_LEVEL..=LAST_LEVEL).contains(&level_number) {
        return Err(OptionsError::LevelOutOfRange(level_number));
    }
    Ok(level_number)
}

/// Parses and bounds-checks one fixed-update frequency.
fn parse_step_rate(value: String) -> Result<u32, OptionsError> {
    // `u32` rejects signs and fractions while comfortably representing the
    // small accepted interval used as a duration divisor by the SDL loop.
    let steps_per_second = value
        .parse::<u32>()
        .map_err(|_| OptionsError::InvalidStepRate(value))?;
    if !(FIRST_STEP_RATE..=LAST_STEP_RATE).contains(&steps_per_second) {
        return Err(OptionsError::StepRateOutOfRange(steps_per_second));
    }
    Ok(steps_per_second)
}

/// Converts the two deliberately lowercase player names into a closed enum.
fn parse_music_player(value: String) -> Result<MusicPlayer, OptionsError> {
    // Exact matching keeps the public command line stable across platforms and
    // makes misspellings visible instead of silently falling back to OPL.
    match value.as_str() {
        "opl" => Ok(MusicPlayer::Opl),
        "xm" => Ok(MusicPlayer::Xm),
        _ => Err(OptionsError::InvalidMusicPlayer { value }),
    }
}

/// Describes why command-line arguments could not select a playable level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionsError {
    /// A recognized or unknown flag was not followed by a value.
    MissingValue(String),
    /// A flag outside the supported option grammar was supplied.
    UnknownOption(String),
    /// One of the three supported options appeared more than once.
    DuplicateOption(&'static str),
    /// The option value was not an unsigned integer.
    InvalidLevel(String),
    /// The value was numeric but outside the original level-set range.
    LevelOutOfRange(usize),
    /// The step-rate value was not an unsigned integer.
    InvalidStepRate(String),
    /// The step rate was numeric but outside the supported frequency range.
    StepRateOutOfRange(u32),
    /// The player name was neither the native `opl` nor converted `xm` backend.
    InvalidMusicPlayer {
        /// Unrecognized value supplied immediately after `--player`.
        value: String,
    },
}

impl fmt::Display for OptionsError {
    /// Formats a short message that can be followed by [`Options::usage`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(option) => write!(formatter, "missing value after {option}"),
            Self::UnknownOption(option) => write!(formatter, "unknown option {option:?}"),
            Self::DuplicateOption(option) => write!(formatter, "duplicate option {option}"),
            Self::InvalidLevel(value) => write!(formatter, "invalid level number {value:?}"),
            Self::LevelOutOfRange(value) => write!(
                formatter,
                "level {value} is outside the supported range {FIRST_LEVEL}..={LAST_LEVEL}"
            ),
            Self::InvalidStepRate(value) => {
                write!(formatter, "invalid step rate {value:?}")
            }
            Self::StepRateOutOfRange(value) => write!(
                formatter,
                "step rate {value} is outside the supported range {FIRST_STEP_RATE}..={LAST_STEP_RATE}"
            ),
            Self::InvalidMusicPlayer { value } => write!(
                formatter,
                "invalid music player {value:?}; expected \"opl\" or \"xm\""
            ),
        }
    }
}

impl Error for OptionsError {}

#[cfg(test)]
mod tests {
    //! Unit tests for the complete public command-line grammar.

    use super::{
        DEFAULT_LEVEL, DEFAULT_STEP_RATE, FIRST_LEVEL, FIRST_STEP_RATE, LAST_LEVEL, LAST_STEP_RATE,
        MusicPlayer, Options, OptionsError,
    };

    /// Confirms that both inclusive endpoints produce usable options.
    #[test]
    fn accepts_first_and_last_original_levels() {
        let first = Options::parse(["--level", "1"]).expect("first level should parse");
        let last = Options::parse(["--level", "111"]).expect("last level should parse");

        assert_eq!(first.level_number(), FIRST_LEVEL);
        assert_eq!(last.level_number(), LAST_LEVEL);
        assert_eq!(first.steps_per_second(), DEFAULT_STEP_RATE);
        assert_eq!(last.steps_per_second(), DEFAULT_STEP_RATE);
        assert_eq!(first.music_player(), MusicPlayer::Opl);
        assert_eq!(last.music_player(), MusicPlayer::Opl);
    }

    /// Confirms both inclusive step endpoints parse in either pair order.
    #[test]
    fn accepts_step_rate_boundaries_in_either_order() {
        let slow = Options::parse(["--step", "5", "--level", "1"])
            .expect("minimum step rate should parse first");
        let fast = Options::parse(["--level", "111", "--step", "60"])
            .expect("maximum step rate should parse last");

        assert_eq!(slow.steps_per_second(), FIRST_STEP_RATE);
        assert_eq!(fast.steps_per_second(), LAST_STEP_RATE);
    }

    /// Confirms that zero cannot accidentally become a wrapping record offset.
    #[test]
    fn rejects_zero_level() {
        assert_eq!(
            Options::parse(["--level", "0"]),
            Err(OptionsError::LevelOutOfRange(0))
        );
    }

    /// Confirms ordinary startup and a rate-only override default to level one.
    #[test]
    fn defaults_the_initial_menu_level_when_omitted() {
        let defaults = Options::parse(Vec::<&str>::new()).expect("empty arguments should parse");
        let rate_only = Options::parse(["--step", "35"]).expect("rate-only form should parse");

        assert_eq!(defaults.level_number(), DEFAULT_LEVEL);
        assert_eq!(defaults.steps_per_second(), DEFAULT_STEP_RATE);
        assert_eq!(rate_only.level_number(), DEFAULT_LEVEL);
        assert_eq!(defaults.music_player(), MusicPlayer::Opl);
    }

    /// Confirms unknown, incomplete, and repeated pairs receive precise errors.
    #[test]
    fn rejects_malformed_or_duplicate_option_pairs() {
        assert_eq!(
            Options::parse(["--other", "1"]),
            Err(OptionsError::UnknownOption("--other".to_owned()))
        );
        assert_eq!(
            Options::parse(["--level"]),
            Err(OptionsError::MissingValue("--level".to_owned()))
        );
        assert_eq!(
            Options::parse(["--level", "1", "--level", "2"]),
            Err(OptionsError::DuplicateOption("--level"))
        );
        assert_eq!(
            Options::parse(["--level", "1", "--step", "35", "--step", "36"]),
            Err(OptionsError::DuplicateOption("--step"))
        );
        assert_eq!(
            Options::parse(["--player", "opl", "--player", "xm"]),
            Err(OptionsError::DuplicateOption("--player"))
        );
    }

    /// Confirms step values must be unsigned integers inside 5..=60.
    #[test]
    fn rejects_invalid_and_out_of_range_step_rates() {
        assert_eq!(
            Options::parse(["--level", "1", "--step", "fast"]),
            Err(OptionsError::InvalidStepRate("fast".to_owned()))
        );
        assert_eq!(
            Options::parse(["--level", "1", "--step", "4"]),
            Err(OptionsError::StepRateOutOfRange(4))
        );
        assert_eq!(
            Options::parse(["--level", "1", "--step", "61"]),
            Err(OptionsError::StepRateOutOfRange(61))
        );
    }

    /// Confirms both named music backends parse and all other names are rejected.
    #[test]
    fn selects_only_the_documented_music_players() {
        let opl = Options::parse(["--player", "opl"]).expect("OPL should parse");
        let xm = Options::parse(["--player", "xm"]).expect("XM should parse");

        assert_eq!(opl.music_player(), MusicPlayer::Opl);
        assert_eq!(xm.music_player(), MusicPlayer::Xm);
        assert_eq!(
            Options::parse(["--player", "midi"]),
            Err(OptionsError::InvalidMusicPlayer {
                value: "midi".to_owned()
            })
        );
    }
}
