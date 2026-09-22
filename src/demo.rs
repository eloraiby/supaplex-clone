//! Decoder and deterministic iterator for original Supaplex demo streams.
//!
//! A legacy `DEMO?.BIN` starts with a one-based level number. Every following
//! non-`0xff` byte stores an input in its low nibble and a repeat count minus one
//! in its high nibble. The terminal `0xff` is a marker, not player input.

use std::{error::Error, fmt};

use crate::{actors::Direction, game::Input};

/// One decoded run of identical player input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DemoRun {
    /// Direction and Space state sampled during this run.
    input: Input,
    /// Number of consecutive fixed simulation steps using `input`.
    steps: u8,
}

/// One validated original demonstration and its compressed input runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Demo {
    /// One-based `LEVELS.DAT` record selected by the legacy header byte.
    level_number: usize,
    /// Ordered non-empty run-length input sequence before the terminator.
    runs: Vec<DemoRun>,
    /// Expanded number of fixed simulation steps represented by `runs`.
    step_count: usize,
}

impl Demo {
    /// Decodes one complete legacy stream for the supplied level collection.
    pub fn decode(bytes: &[u8], level_count: usize) -> Result<Self, DemoError> {
        // A level byte and terminal marker are the minimum meaningful stream.
        if bytes.len() < 2 {
            return Err(DemoError::TooShort {
                actual: bytes.len(),
            });
        }
        let level_number = usize::from(bytes[0]);
        if !(1..=level_count).contains(&level_number) {
            return Err(DemoError::InvalidLevelNumber {
                level_number,
                level_count,
            });
        }
        let terminator = bytes[1..]
            .iter()
            .position(|byte| *byte == 0xff)
            .map(|offset| offset + 1)
            .ok_or(DemoError::MissingTerminator)?;
        if terminator + 1 != bytes.len() {
            return Err(DemoError::TrailingBytes {
                count: bytes.len() - terminator - 1,
            });
        }

        let mut runs = Vec::with_capacity(terminator.saturating_sub(1));
        let mut step_count = 0usize;
        for (offset, encoded) in bytes[1..terminator].iter().copied().enumerate() {
            let command = encoded & 0x0f;
            let input = decode_input(command).ok_or(DemoError::InvalidInput {
                offset: offset + 1,
                command,
            })?;
            let steps = (encoded >> 4) + 1;
            step_count = step_count.saturating_add(usize::from(steps));
            runs.push(DemoRun { input, steps });
        }
        Ok(Self {
            level_number,
            runs,
            step_count,
        })
    }

    /// Returns the one-based level selected by this demonstration.
    pub const fn level_number(&self) -> usize {
        // Header validation guarantees the value belongs to the decoded set.
        self.level_number
    }

    /// Returns the expanded number of fixed input samples in the stream.
    pub const fn step_count(&self) -> usize {
        // The total was accumulated once during validation for constant-time UI.
        self.step_count
    }

    /// Creates a fresh iterator beginning at the first decoded input sample.
    pub fn playback(&self) -> DemoPlayback<'_> {
        // Zero indices and remaining count naturally handle a marker-only demo.
        DemoPlayback {
            demo: self,
            run_index: 0,
            remaining_steps: 0,
        }
    }
}

/// Expanded fixed-step iterator over one borrowed demonstration.
#[derive(Clone, Debug)]
pub struct DemoPlayback<'demo> {
    /// Immutable run collection being expanded.
    demo: &'demo Demo,
    /// Index of the next run to load when `remaining_steps` reaches zero.
    run_index: usize,
    /// Number of samples still owed from the currently loaded run.
    remaining_steps: u8,
}

impl Iterator for DemoPlayback<'_> {
    type Item = Input;

    /// Returns the next fixed-step input while expanding run lengths lazily.
    fn next(&mut self) -> Option<Self::Item> {
        // A zero remainder loads one new run and advances the run cursor. Every
        // call then decrements exactly once and returns that run's copied input.
        if self.remaining_steps == 0 {
            let run = self.demo.runs.get(self.run_index)?;
            self.run_index += 1;
            self.remaining_steps = run.steps;
        }
        let run = self
            .demo
            .runs
            .get(self.run_index - 1)
            .expect("a positive remainder always has a current demo run");
        self.remaining_steps -= 1;
        Some(run.input)
    }

    /// Reports exact remaining samples without expanding the runs.
    fn size_hint(&self) -> (usize, Option<usize>) {
        // Include the current run remainder and all complete subsequent runs.
        let remaining = usize::from(self.remaining_steps)
            + self.demo.runs[self.run_index..]
                .iter()
                .map(|run| usize::from(run.steps))
                .sum::<usize>();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DemoPlayback<'_> {
    /// Returns the exact number of remaining fixed-step samples.
    fn len(&self) -> usize {
        // `size_hint` calculates a single exact bound from compressed runs.
        self.size_hint().0
    }
}

/// Decodes one original low-nibble input command.
fn decode_input(command: u8) -> Option<Input> {
    // Original command order is Up, Left, Down, Right; adding four combines the
    // same direction with Space, while nine represents Space without direction.
    let (direction, action) = match command {
        0 => (None, false),
        1 => (Some(Direction::Up), false),
        2 => (Some(Direction::Left), false),
        3 => (Some(Direction::Down), false),
        4 => (Some(Direction::Right), false),
        5 => (Some(Direction::Up), true),
        6 => (Some(Direction::Left), true),
        7 => (Some(Direction::Down), true),
        8 => (Some(Direction::Right), true),
        9 => (None, true),
        _ => return None,
    };
    Some(Input { direction, action })
}

/// Failure to validate one untrusted legacy demo stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DemoError {
    /// The stream cannot contain both a level byte and terminator.
    TooShort {
        /// Actual byte length supplied to the decoder.
        actual: usize,
    },
    /// The header does not select a playable level record.
    InvalidLevelNumber {
        /// Invalid one-based header value.
        level_number: usize,
        /// Inclusive upper bound in the current level collection.
        level_count: usize,
    },
    /// No `0xff` terminal marker appears after the level header.
    MissingTerminator,
    /// Bytes remain after the first terminal marker.
    TrailingBytes {
        /// Number of unexpected bytes after `0xff`.
        count: usize,
    },
    /// A low nibble falls outside the original command range zero through nine.
    InvalidInput {
        /// Absolute byte offset of the invalid encoded run.
        offset: usize,
        /// Invalid low-nibble value.
        command: u8,
    },
}

impl fmt::Display for DemoError {
    /// Formats one concise stream-validation diagnostic.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Each variant includes enough context to locate a malformed asset.
        match self {
            Self::TooShort { actual } => {
                write!(formatter, "demo is {actual} bytes; expected at least 2")
            }
            Self::InvalidLevelNumber {
                level_number,
                level_count,
            } => write!(
                formatter,
                "demo level {level_number} is outside 1..={level_count}"
            ),
            Self::MissingTerminator => formatter.write_str("demo has no 0xff terminator"),
            Self::TrailingBytes { count } => {
                write!(formatter, "demo has {count} bytes after its terminator")
            }
            Self::InvalidInput { offset, command } => {
                write!(
                    formatter,
                    "demo input {command:#x} is invalid at byte {offset}"
                )
            }
        }
    }
}

impl Error for DemoError {}

#[cfg(test)]
mod tests {
    //! Format rejection and exact run-expansion checks.

    use super::{Demo, DemoError};
    use crate::{actors::Direction, assets, game::Input};

    /// Confirms all ten production files decode with their original level headers.
    #[test]
    fn production_demos_decode_in_function_key_order() {
        let assets = assets::load_demos().expect("production demos should load");
        let levels = [1, 3, 7, 11, 29, 38, 55, 95, 104, 108];

        for (index, bytes) in assets.demos.iter().enumerate() {
            let demo = Demo::decode(bytes.as_ref(), 111).expect("production demo should decode");
            assert_eq!(demo.level_number(), levels[index]);
            assert!(demo.step_count() > 0);
            assert_eq!(demo.playback().count(), demo.step_count());
        }
    }

    /// Confirms high nibbles expand and low nibbles preserve direction plus Space.
    #[test]
    fn playback_expands_original_run_length_bytes() {
        let demo = Demo::decode(&[7, 0x22, 0x08, 0xff], 111).expect("valid demo");
        let samples: Vec<_> = demo.playback().collect();

        assert_eq!(samples.len(), 4);
        assert_eq!(
            samples[..3],
            [Input {
                direction: Some(Direction::Left),
                action: false,
            }; 3]
        );
        assert_eq!(
            samples[3],
            Input {
                direction: Some(Direction::Right),
                action: true,
            }
        );
    }

    /// Confirms structural and command corruption are rejected before playback.
    #[test]
    fn decoder_rejects_invalid_stream_boundaries_and_commands() {
        assert_eq!(
            Demo::decode(&[1], 111),
            Err(DemoError::TooShort { actual: 1 })
        );
        assert_eq!(
            Demo::decode(&[0, 0xff], 111),
            Err(DemoError::InvalidLevelNumber {
                level_number: 0,
                level_count: 111,
            })
        );
        assert_eq!(
            Demo::decode(&[1, 0x00], 111),
            Err(DemoError::MissingTerminator)
        );
        assert_eq!(
            Demo::decode(&[1, 0xff, 0], 111),
            Err(DemoError::TrailingBytes { count: 1 })
        );
        assert_eq!(
            Demo::decode(&[1, 0x0a, 0xff], 111),
            Err(DemoError::InvalidInput {
                offset: 1,
                command: 10,
            })
        );
    }
}
