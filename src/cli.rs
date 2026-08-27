//! Minimal command-line parsing for choosing a level to play.

use std::{error::Error, fmt};

/// The first level number accepted by the original 111-level collection.
pub const FIRST_LEVEL: usize = 1;

/// The final level number accepted by the original 111-level collection.
pub const LAST_LEVEL: usize = 111;

/// Validated options needed to start one play session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// One-based level number matching the numbering shown by Supaplex.
    level_number: usize,
}

impl Options {
    /// Parses `--level <number>` from an iterator of command-line arguments.
    ///
    /// Exactly one level selector is required. A deliberately small parser
    /// keeps startup dependencies light and every rejection deterministic.
    pub fn parse<I, S>(arguments: I) -> Result<Self, OptionsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // Materialize the arguments because the accepted grammar contains
        // exactly two tokens and reporting extras is clearer with a slice.
        let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();

        if arguments.len() != 2 || arguments[0] != "--level" {
            return Err(OptionsError::ExpectedLevelOption);
        }

        // Parse as `usize` first, then apply the game's one-based bounds. This
        // rejects negative values and non-numeric text through the same error.
        let level_number = arguments[1]
            .parse::<usize>()
            .map_err(|_| OptionsError::InvalidLevel(arguments[1].clone()))?;

        if !(FIRST_LEVEL..=LAST_LEVEL).contains(&level_number) {
            return Err(OptionsError::LevelOutOfRange(level_number));
        }

        Ok(Self { level_number })
    }

    /// Returns the one-based number of the level selected by the player.
    pub fn level_number(self) -> usize {
        self.level_number
    }

    /// Returns a concise invocation string suitable for startup errors.
    pub fn usage() -> &'static str {
        "Usage: supaplex-clone --level <1-111>"
    }
}

/// Describes why command-line arguments could not select a playable level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionsError {
    /// The required `--level <number>` pair was missing or had extras.
    ExpectedLevelOption,
    /// The option value was not an unsigned integer.
    InvalidLevel(String),
    /// The value was numeric but outside the original level-set range.
    LevelOutOfRange(usize),
}

impl fmt::Display for OptionsError {
    /// Formats a short message that can be followed by [`Options::usage`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedLevelOption => write!(formatter, "expected exactly --level <number>"),
            Self::InvalidLevel(value) => write!(formatter, "invalid level number {value:?}"),
            Self::LevelOutOfRange(value) => write!(
                formatter,
                "level {value} is outside the supported range {FIRST_LEVEL}..={LAST_LEVEL}"
            ),
        }
    }
}

impl Error for OptionsError {}

#[cfg(test)]
mod tests {
    //! Unit tests for the complete public command-line grammar.

    use super::{FIRST_LEVEL, LAST_LEVEL, Options, OptionsError};

    /// Confirms that both inclusive endpoints produce usable options.
    #[test]
    fn accepts_first_and_last_original_levels() {
        let first = Options::parse(["--level", "1"]).expect("first level should parse");
        let last = Options::parse(["--level", "111"]).expect("last level should parse");

        assert_eq!(first.level_number(), FIRST_LEVEL);
        assert_eq!(last.level_number(), LAST_LEVEL);
    }

    /// Confirms that zero cannot accidentally become a wrapping record offset.
    #[test]
    fn rejects_zero_level() {
        assert_eq!(
            Options::parse(["--level", "0"]),
            Err(OptionsError::LevelOutOfRange(0))
        );
    }

    /// Confirms malformed syntax does not silently select a default level.
    #[test]
    fn requires_the_explicit_level_pair() {
        assert_eq!(
            Options::parse(["1"]),
            Err(OptionsError::ExpectedLevelOption)
        );
        assert_eq!(
            Options::parse(["--other", "1"]),
            Err(OptionsError::ExpectedLevelOption)
        );
    }
}
