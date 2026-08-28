//! Persistent original-style player profiles and level progression.
//!
//! The DOS menu stores twenty eight-character players, their accumulated time,
//! and one result byte per level. This module retains those semantics while
//! using a versioned clone-native file so malformed or incompatible state can
//! be rejected without confusing it with `PLAYER.LST` from another port.

use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

/// Maximum number of player slots shown and accepted by the original menu.
pub const MAX_PLAYERS: usize = 20;

/// Maximum number of levels one player may mark as skipped.
pub const MAX_SKIPPED_LEVELS: usize = 3;

/// Maximum visible player-name length supported by the original CHARS6 field.
pub const MAX_PLAYER_NAME_LENGTH: usize = 8;

/// Versioned signature at the beginning of each clone-native profile file.
const PROFILE_MAGIC: &[u8; 8] = b"SPPLYR01";

/// Sentinel selected-player byte used when the persisted player list is empty.
const NO_SELECTED_PLAYER: u8 = u8::MAX;

/// One player's persistent result for one original level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelResult {
    /// The level has not yet been completed or skipped.
    Unfinished,
    /// Murphy reached the exit after collecting the required Infotrons.
    Completed,
    /// The player spent one of the original three permitted skips.
    Skipped,
}

impl LevelResult {
    /// Encodes one result into its stable clone-native on-disk byte.
    const fn to_byte(self) -> u8 {
        // Explicit values prevent declaration reordering from changing files.
        match self {
            Self::Unfinished => 0,
            Self::Completed => 1,
            Self::Skipped => 2,
        }
    }

    /// Decodes one stable result byte and rejects unknown future values.
    fn from_byte(value: u8, offset: usize) -> Result<Self, ProfileError> {
        // Treating unknown states as corruption avoids silently unlocking or
        // completing a level after a truncated or incompatible write.
        match value {
            0 => Ok(Self::Unfinished),
            1 => Ok(Self::Completed),
            2 => Ok(Self::Skipped),
            _ => Err(ProfileError::InvalidLevelResult { offset, value }),
        }
    }
}

/// Persistent state and aggregate play time for one named player.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerProfile {
    /// Uppercase one-to-eight-character name displayed by the original menu.
    name: String,
    /// Accumulated duration of successfully completed sessions in whole seconds.
    total_seconds: u64,
    /// Per-level results in zero-based `LEVELS.DAT` record order.
    level_results: Vec<LevelResult>,
}

impl PlayerProfile {
    /// Creates a fresh profile with every level unfinished and zero elapsed time.
    fn new(name: String, level_count: usize) -> Self {
        // The caller has already normalized and validated the name; allocating
        // all result bytes here gives every later indexed operation one length.
        Self {
            name,
            total_seconds: 0,
            level_results: vec![LevelResult::Unfinished; level_count],
        }
    }

    /// Returns the normalized player name without padding spaces.
    pub fn name(&self) -> &str {
        // Names are immutable after profile construction, so callers may retain
        // this borrow throughout one menu-rendering frame.
        &self.name
    }

    /// Returns accumulated successful-play time in whole seconds.
    pub const fn total_seconds(&self) -> u64 {
        // The stored counter is already expressed in the renderer's unit.
        self.total_seconds
    }

    /// Returns the one-based first unfinished level, if any remains.
    pub fn next_level_to_play(&self) -> Option<usize> {
        // Completed and skipped records both advance progression; only the first
        // unfinished record is exposed while every later unfinished one is locked.
        self.level_results
            .iter()
            .position(|result| *result == LevelResult::Unfinished)
            .map(|index| index + 1)
    }

    /// Returns one level's persistent result when the one-based number is valid.
    pub fn level_result(&self, level_number: usize) -> Option<LevelResult> {
        // Checked subtraction rejects synthetic level zero before slice lookup.
        self.level_results
            .get(level_number.checked_sub(1)?)
            .copied()
    }

    /// Reports whether one level is selectable under original progression rules.
    pub fn can_play(&self, level_number: usize) -> bool {
        // Completed and skipped rows remain revisit-able, while only the first
        // unfinished row is open among the unfinished suffix.
        match self.level_result(level_number) {
            Some(LevelResult::Completed | LevelResult::Skipped) => true,
            Some(LevelResult::Unfinished) => self.next_level_to_play() == Some(level_number),
            None => false,
        }
    }

    /// Counts levels completed by reaching their exit.
    pub fn completed_levels(&self) -> usize {
        // A direct filter keeps skipped levels separate in Statistics.
        self.level_results
            .iter()
            .filter(|result| **result == LevelResult::Completed)
            .count()
    }

    /// Counts levels advanced with the limited Skip Level option.
    pub fn skipped_levels(&self) -> usize {
        // This count enforces the original allowance of three skipped records.
        self.level_results
            .iter()
            .filter(|result| **result == LevelResult::Skipped)
            .count()
    }

    /// Marks one valid level completed and adds its successful session duration.
    fn complete(&mut self, level_number: usize, elapsed_seconds: u64) -> Result<(), ProfileError> {
        // Completion replaces a prior skip if the player later solves that level.
        let index = checked_level_index(level_number, self.level_results.len())?;
        self.level_results[index] = LevelResult::Completed;
        self.total_seconds = self.total_seconds.saturating_add(elapsed_seconds);
        Ok(())
    }

    /// Marks the currently available unfinished level skipped.
    fn skip(&mut self, level_number: usize) -> Result<(), ProfileError> {
        // Validate every policy before mutation so a failed request leaves the
        // persisted profile byte-for-byte unchanged.
        let index = checked_level_index(level_number, self.level_results.len())?;
        if self.skipped_levels() >= MAX_SKIPPED_LEVELS
            || self.next_level_to_play() != Some(level_number)
            || self.level_results[index] != LevelResult::Unfinished
        {
            return Err(ProfileError::SkipNotPossible { level_number });
        }
        self.level_results[index] = LevelResult::Skipped;
        Ok(())
    }
}

/// Complete ordered player list and current menu selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerBook {
    /// Non-empty and empty profiles retained in visible menu ordering.
    players: Vec<PlayerProfile>,
    /// Index of the selected player, or zero while `players` is empty.
    selected_index: usize,
    /// Number of results required in every profile for this level collection.
    level_count: usize,
}

impl PlayerBook {
    /// Creates the first-run player list with a fresh `MURPHY` profile.
    pub fn first_run(level_count: usize) -> Result<Self, ProfileError> {
        // A zero-level collection cannot support progression or meaningful saves.
        validate_level_count(level_count)?;
        Ok(Self {
            players: vec![PlayerProfile::new("MURPHY".to_owned(), level_count)],
            selected_index: 0,
            level_count,
        })
    }

    /// Loads a profile file, creating first-run state when the file is absent.
    pub fn load(path: &Path, level_count: usize) -> Result<Self, ProfileError> {
        // Missing state is a normal first launch. Other I/O failures remain
        // visible because silently replacing an unreadable save would lose data.
        validate_level_count(level_count)?;
        match fs::read(path) {
            Ok(bytes) => Self::decode(&bytes, level_count),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Self::first_run(level_count),
            Err(source) => Err(ProfileError::Io {
                action: "read",
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the complete current list to its versioned profile file.
    pub fn save(&self, path: &Path) -> Result<(), ProfileError> {
        // SDL's preference path normally has a parent, but custom paths may be
        // bare filenames. Create a parent only when it is a non-empty component.
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ProfileError::Io {
                action: "create profile directory for",
                path: parent.to_owned(),
                source,
            })?;
        }
        fs::write(path, self.encode()).map_err(|source| ProfileError::Io {
            action: "write",
            path: path.to_owned(),
            source,
        })
    }

    /// Returns the number of retained player profiles.
    pub fn len(&self) -> usize {
        // The vector never exceeds [`MAX_PLAYERS`] through the public API.
        self.players.len()
    }

    /// Reports whether no player is currently available.
    pub fn is_empty(&self) -> bool {
        // This remains distinct from first-run state because deleting the last
        // profile is a valid persistent menu action.
        self.players.is_empty()
    }

    /// Returns the selected profile, or `None` after deleting the final player.
    pub fn current(&self) -> Option<&PlayerProfile> {
        // Empty state uses selected index zero but slice lookup still returns None.
        self.players.get(self.selected_index)
    }

    /// Returns a mutable selected profile for progression updates.
    fn current_mut(&mut self) -> Option<&mut PlayerProfile> {
        // Centralizing mutable lookup keeps index repair inside deletion/decoding.
        self.players.get_mut(self.selected_index)
    }

    /// Returns the previous, current, and next visible player names.
    pub fn visible_names(&self) -> [Option<&str>; 3] {
        // Checked neighbor lookup exactly matches the three original list rows
        // without wrapping from the first player to the last.
        let previous = self
            .selected_index
            .checked_sub(1)
            .and_then(|index| self.players.get(index))
            .map(PlayerProfile::name);
        let current = self.current().map(PlayerProfile::name);
        let next = self
            .players
            .get(self.selected_index.saturating_add(1))
            .map(PlayerProfile::name);
        [previous, current, next]
    }

    /// Selects the preceding profile when one exists.
    pub fn select_previous(&mut self) {
        // Saturation keeps an empty or first-row book at index zero.
        self.selected_index = self.selected_index.saturating_sub(1);
    }

    /// Selects the following profile when one exists.
    pub fn select_next(&mut self) {
        // `saturating_sub` avoids underflow for an empty list while the final
        // `min` clamps repeated arrow clicks at the last visible player.
        self.selected_index = self
            .selected_index
            .saturating_add(1)
            .min(self.players.len().saturating_sub(1));
    }

    /// Adds and selects one normalized new player.
    pub fn add(&mut self, requested_name: &str) -> Result<(), ProfileError> {
        // Capacity and duplicate checks happen before allocation and mutation.
        if self.players.len() >= MAX_PLAYERS {
            return Err(ProfileError::PlayerListFull);
        }
        let name = normalize_name(requested_name)?;
        if self.players.iter().any(|player| player.name == name) {
            return Err(ProfileError::PlayerExists(name));
        }
        self.players
            .push(PlayerProfile::new(name, self.level_count));
        self.selected_index = self.players.len() - 1;
        Ok(())
    }

    /// Deletes and returns the selected profile, if one exists.
    pub fn delete_current(&mut self) -> Option<PlayerProfile> {
        // Removing the final row yields the intentionally empty state; otherwise
        // the index selects the row that shifted into the deleted row's place.
        if self.players.is_empty() {
            return None;
        }
        let removed = self.players.remove(self.selected_index);
        self.selected_index = self
            .selected_index
            .min(self.players.len().saturating_sub(1));
        Some(removed)
    }

    /// Reports whether the selected player may start one level.
    pub fn can_play(&self, level_number: usize) -> bool {
        // With no player, the original menu exposes no playable progression.
        self.current()
            .is_some_and(|player| player.can_play(level_number))
    }

    /// Records a successful level and its elapsed duration for the current player.
    pub fn complete_current(
        &mut self,
        level_number: usize,
        elapsed_seconds: u64,
    ) -> Result<(), ProfileError> {
        // Absence is a normal menu state but cannot own gameplay progress.
        let player = self.current_mut().ok_or(ProfileError::NoPlayerSelected)?;
        player.complete(level_number, elapsed_seconds)
    }

    /// Spends a skip on the selected player's currently available level.
    pub fn skip_current(&mut self, level_number: usize) -> Result<(), ProfileError> {
        // Player-specific validation remains within `PlayerProfile::skip` after
        // this shared no-selection diagnostic.
        let player = self.current_mut().ok_or(ProfileError::NoPlayerSelected)?;
        player.skip(level_number)
    }

    /// Returns profiles ordered for the menu ranking list.
    pub fn rankings(&self) -> Vec<&PlayerProfile> {
        // More solved levels rank first; equal progress uses lower accumulated
        // time, then name, producing deterministic output across saves.
        let mut profiles: Vec<_> = self.players.iter().collect();
        profiles.sort_by(|left, right| {
            right
                .completed_levels()
                .cmp(&left.completed_levels())
                .then_with(|| left.total_seconds.cmp(&right.total_seconds))
                .then_with(|| left.name.cmp(&right.name))
        });
        profiles
    }

    /// Encodes all state into the stable compact profile-file representation.
    fn encode(&self) -> Vec<u8> {
        // A precise capacity estimate avoids repeated growth for the normal
        // twenty-player, 111-level maximum file.
        let record_size = 1 + MAX_PLAYER_NAME_LENGTH + 8 + self.level_count;
        let mut bytes = Vec::with_capacity(12 + self.players.len() * record_size);
        bytes.extend_from_slice(PROFILE_MAGIC);
        bytes.extend_from_slice(&(self.level_count as u16).to_le_bytes());
        bytes.push(
            self.current()
                .map_or(NO_SELECTED_PLAYER, |_| self.selected_index as u8),
        );
        bytes.push(self.players.len() as u8);
        for player in &self.players {
            bytes.push(player.name.len() as u8);
            let mut name = [b' '; MAX_PLAYER_NAME_LENGTH];
            name[..player.name.len()].copy_from_slice(player.name.as_bytes());
            bytes.extend_from_slice(&name);
            bytes.extend_from_slice(&player.total_seconds.to_le_bytes());
            bytes.extend(player.level_results.iter().map(|result| result.to_byte()));
        }
        bytes
    }

    /// Decodes and validates one complete clone-native profile file.
    fn decode(bytes: &[u8], expected_level_count: usize) -> Result<Self, ProfileError> {
        // Header fields are consumed through one checked cursor so every short
        // input reports a controlled truncation error rather than indexing panic.
        let mut cursor = ProfileCursor::new(bytes);
        let magic = cursor.take(PROFILE_MAGIC.len())?;
        if magic != PROFILE_MAGIC {
            return Err(ProfileError::InvalidMagic);
        }
        let stored_level_count = usize::from(cursor.read_u16()?);
        if stored_level_count != expected_level_count {
            return Err(ProfileError::LevelCountMismatch {
                expected: expected_level_count,
                actual: stored_level_count,
            });
        }
        let selected = cursor.read_u8()?;
        let player_count = usize::from(cursor.read_u8()?);
        if player_count > MAX_PLAYERS {
            return Err(ProfileError::TooManyPlayers { player_count });
        }

        let mut players = Vec::with_capacity(player_count);
        for _ in 0..player_count {
            let name_length = usize::from(cursor.read_u8()?);
            let name_field = cursor.take(MAX_PLAYER_NAME_LENGTH)?;
            if name_length == 0 || name_length > MAX_PLAYER_NAME_LENGTH {
                return Err(ProfileError::InvalidNameLength { name_length });
            }
            let raw_name = std::str::from_utf8(&name_field[..name_length])
                .map_err(|_| ProfileError::InvalidNameEncoding)?;
            let name = normalize_name(raw_name)?;
            if players
                .iter()
                .any(|player: &PlayerProfile| player.name == name)
            {
                return Err(ProfileError::PlayerExists(name));
            }
            let total_seconds = cursor.read_u64()?;
            let result_offset = cursor.offset;
            let result_bytes = cursor.take(expected_level_count)?;
            let mut level_results = Vec::with_capacity(expected_level_count);
            for (index, value) in result_bytes.iter().copied().enumerate() {
                level_results.push(LevelResult::from_byte(value, result_offset + index)?);
            }
            players.push(PlayerProfile {
                name,
                total_seconds,
                level_results,
            });
        }
        if cursor.offset != bytes.len() {
            return Err(ProfileError::TrailingBytes {
                count: bytes.len() - cursor.offset,
            });
        }

        let selected_index = if players.is_empty() {
            if selected != NO_SELECTED_PLAYER {
                return Err(ProfileError::InvalidSelectedPlayer {
                    selected: usize::from(selected),
                    player_count,
                });
            }
            0
        } else {
            let selected = usize::from(selected);
            if selected >= player_count {
                return Err(ProfileError::InvalidSelectedPlayer {
                    selected,
                    player_count,
                });
            }
            selected
        };

        Ok(Self {
            players,
            selected_index,
            level_count: expected_level_count,
        })
    }
}

/// Checked byte reader used while parsing an untrusted profile file.
struct ProfileCursor<'bytes> {
    /// Complete immutable file contents.
    bytes: &'bytes [u8],
    /// Index of the next unread byte.
    offset: usize,
}

impl<'bytes> ProfileCursor<'bytes> {
    /// Starts a cursor before the first byte of one file.
    const fn new(bytes: &'bytes [u8]) -> Self {
        // No validation is needed until the first typed read requests bytes.
        Self { bytes, offset: 0 }
    }

    /// Borrows and advances over exactly `count` bytes.
    fn take(&mut self, count: usize) -> Result<&'bytes [u8], ProfileError> {
        // Checked addition prevents a crafted count from wrapping the end index.
        let end = self
            .offset
            .checked_add(count)
            .ok_or(ProfileError::Truncated {
                offset: self.offset,
                needed: count,
            })?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProfileError::Truncated {
                offset: self.offset,
                needed: count,
            })?;
        self.offset = end;
        Ok(value)
    }

    /// Reads one unsigned byte.
    fn read_u8(&mut self) -> Result<u8, ProfileError> {
        // `take` guarantees the one-byte slice exists.
        Ok(self.take(1)?[0])
    }

    /// Reads one little-endian unsigned 16-bit integer.
    fn read_u16(&mut self) -> Result<u16, ProfileError> {
        // The fixed array conversion cannot fail after the checked two-byte read.
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two-byte profile field"),
        ))
    }

    /// Reads one little-endian unsigned 64-bit integer.
    fn read_u64(&mut self) -> Result<u64, ProfileError> {
        // The fixed array conversion cannot fail after the checked eight-byte read.
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight-byte profile field"),
        ))
    }
}

/// Validates and normalizes a user-entered original-style player name.
fn normalize_name(requested_name: &str) -> Result<String, ProfileError> {
    // Trimming avoids invisible duplicates while uppercase matches the source
    // menu and the only glyph case represented by its CHARS6 font.
    let name = requested_name.trim().to_ascii_uppercase();
    if name.is_empty() || name.len() > MAX_PLAYER_NAME_LENGTH {
        return Err(ProfileError::InvalidName(requested_name.to_owned()));
    }
    if !name.bytes().all(|byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ' || byte == b'-'
    }) || name.bytes().all(|byte| byte == b'-')
    {
        return Err(ProfileError::InvalidName(requested_name.to_owned()));
    }
    Ok(name)
}

/// Converts one one-based level number into a checked zero-based result index.
fn checked_level_index(level_number: usize, level_count: usize) -> Result<usize, ProfileError> {
    // The paired subtraction and upper-bound check reject zero and the sentinel
    // record after the final level through one diagnostic.
    let index = level_number
        .checked_sub(1)
        .filter(|index| *index < level_count)
        .ok_or(ProfileError::InvalidLevelNumber {
            level_number,
            level_count,
        })?;
    Ok(index)
}

/// Validates the level count representable in the profile-file header.
fn validate_level_count(level_count: usize) -> Result<(), ProfileError> {
    // Zero is unusable and values above `u16` cannot round-trip through disk.
    if level_count == 0 || u16::try_from(level_count).is_err() {
        return Err(ProfileError::InvalidLevelCount { level_count });
    }
    Ok(())
}

/// Failure to validate, update, decode, or persist player state.
#[derive(Debug)]
pub enum ProfileError {
    /// The current level collection cannot be represented by this format.
    InvalidLevelCount {
        /// Unsupported number of playable records.
        level_count: usize,
    },
    /// A requested one-based level falls outside the current collection.
    InvalidLevelNumber {
        /// Invalid one-based number supplied by the caller.
        level_number: usize,
        /// Inclusive upper bound for valid level numbers.
        level_count: usize,
    },
    /// A new name is empty, too long, or contains unsupported characters.
    InvalidName(String),
    /// A new or decoded profile duplicates an existing normalized name.
    PlayerExists(String),
    /// No free slot remains in the original twenty-player list.
    PlayerListFull,
    /// A progression update was requested while the list is empty.
    NoPlayerSelected,
    /// A level cannot be skipped because it is locked, resolved, or exceeds quota.
    SkipNotPossible {
        /// One-based level rejected by the skip policy.
        level_number: usize,
    },
    /// The file does not begin with the supported clone-native signature.
    InvalidMagic,
    /// Persisted progression belongs to a different-sized level collection.
    LevelCountMismatch {
        /// Number of levels loaded by the current application.
        expected: usize,
        /// Number of per-level results stored in the file.
        actual: usize,
    },
    /// The file declares more profiles than the menu can display.
    TooManyPlayers {
        /// Invalid declared player count.
        player_count: usize,
    },
    /// A decoded name length is zero or exceeds eight bytes.
    InvalidNameLength {
        /// Invalid declared byte length.
        name_length: usize,
    },
    /// A stored name is not valid ASCII-compatible UTF-8.
    InvalidNameEncoding,
    /// A level-result byte is not part of the stable encoding.
    InvalidLevelResult {
        /// Absolute byte offset of the invalid value.
        offset: usize,
        /// Unknown encoded value.
        value: u8,
    },
    /// The selected-player byte does not name one declared player.
    InvalidSelectedPlayer {
        /// Invalid zero-based selected index.
        selected: usize,
        /// Number of decoded players available for selection.
        player_count: usize,
    },
    /// The file ends before one declared field or record is complete.
    Truncated {
        /// Offset at which the incomplete field begins.
        offset: usize,
        /// Number of bytes required by that field.
        needed: usize,
    },
    /// Unrecognized bytes remain after every declared record.
    TrailingBytes {
        /// Number of unexpected bytes at the end of the file.
        count: usize,
    },
    /// A filesystem operation failed for the profile path.
    Io {
        /// Short operation description used in the diagnostic.
        action: &'static str,
        /// Exact path involved in the failed operation.
        path: PathBuf,
        /// Original operating-system error.
        source: io::Error,
    },
}

impl fmt::Display for ProfileError {
    /// Formats one actionable player-state diagnostic.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Each branch retains the value needed to distinguish invalid input,
        // incompatible data, corruption, and an environmental I/O failure.
        match self {
            Self::InvalidLevelCount { level_count } => {
                write!(formatter, "invalid profile level count {level_count}")
            }
            Self::InvalidLevelNumber {
                level_number,
                level_count,
            } => write!(
                formatter,
                "level {level_number} is outside 1..={level_count}"
            ),
            Self::InvalidName(name) => write!(formatter, "invalid player name {name:?}"),
            Self::PlayerExists(name) => write!(formatter, "player {name:?} already exists"),
            Self::PlayerListFull => formatter.write_str("player list is full"),
            Self::NoPlayerSelected => formatter.write_str("no player is selected"),
            Self::SkipNotPossible { level_number } => {
                write!(formatter, "level {level_number} cannot be skipped")
            }
            Self::InvalidMagic => formatter.write_str("unrecognized player profile file"),
            Self::LevelCountMismatch { expected, actual } => write!(
                formatter,
                "profile contains {actual} levels but the loaded set contains {expected}"
            ),
            Self::TooManyPlayers { player_count } => {
                write!(formatter, "profile declares {player_count} players")
            }
            Self::InvalidNameLength { name_length } => {
                write!(
                    formatter,
                    "profile contains invalid name length {name_length}"
                )
            }
            Self::InvalidNameEncoding => formatter.write_str("profile name is not valid UTF-8"),
            Self::InvalidLevelResult { offset, value } => write!(
                formatter,
                "profile contains invalid level result {value} at byte {offset}"
            ),
            Self::InvalidSelectedPlayer {
                selected,
                player_count,
            } => write!(
                formatter,
                "profile selects player {selected} from only {player_count} players"
            ),
            Self::Truncated { offset, needed } => write!(
                formatter,
                "profile ends at byte {offset} while {needed} bytes are required"
            ),
            Self::TrailingBytes { count } => {
                write!(formatter, "profile contains {count} trailing bytes")
            }
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "could not {action} {}: {source}", path.display()),
        }
    }
}

impl Error for ProfileError {
    /// Exposes an operating-system source only for filesystem failures.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // Validation errors are complete on their own; I/O retains its typed cause.
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Round-trip and progression checks for persistent menu players.

    use super::{
        LevelResult, MAX_PLAYER_NAME_LENGTH, MAX_SKIPPED_LEVELS, PlayerBook, ProfileError,
    };

    /// Confirms new profiles expose only their first unfinished level.
    #[test]
    fn progression_unlocks_one_unfinished_level_at_a_time() {
        let mut players = PlayerBook::first_run(5).expect("valid level count");

        assert!(players.can_play(1));
        assert!(!players.can_play(2));
        players.complete_current(1, 42).expect("complete level one");
        assert!(players.can_play(1));
        assert!(players.can_play(2));
        assert_eq!(players.current().expect("player").total_seconds(), 42);
    }

    /// Confirms the original three-skip quota and current-level policy.
    #[test]
    fn skip_policy_rejects_locked_and_fourth_levels() {
        let mut players = PlayerBook::first_run(6).expect("valid level count");

        assert!(matches!(
            players.skip_current(2),
            Err(ProfileError::SkipNotPossible { level_number: 2 })
        ));
        for level in 1..=MAX_SKIPPED_LEVELS {
            players.skip_current(level).expect("available skip");
        }
        assert!(matches!(
            players.skip_current(4),
            Err(ProfileError::SkipNotPossible { level_number: 4 })
        ));
    }

    /// Confirms add, selection, duplicate handling, and final deletion are safe.
    #[test]
    fn player_list_mutations_preserve_a_valid_selection() {
        let mut players = PlayerBook::first_run(3).expect("valid level count");

        players.add("player 2").expect("new player");
        assert_eq!(
            players.current().expect("selected player").name(),
            "PLAYER 2"
        );
        assert!(matches!(
            players.add("Player 2"),
            Err(ProfileError::PlayerExists(name)) if name == "PLAYER 2"
        ));
        players.select_previous();
        assert_eq!(players.current().expect("previous player").name(), "MURPHY");
        players.delete_current().expect("delete Murphy");
        players.delete_current().expect("delete second player");
        assert!(players.is_empty());
        assert_eq!(players.visible_names(), [None, None, None]);
    }

    /// Confirms the compact representation round-trips all semantic fields.
    #[test]
    fn encoded_profiles_round_trip_exactly() {
        let mut players = PlayerBook::first_run(4).expect("valid level count");
        players.complete_current(1, 75).expect("complete level");
        players.skip_current(2).expect("skip next level");
        players.add("ALPHA-7").expect("add player");

        let encoded = players.encode();
        let decoded = PlayerBook::decode(&encoded, 4).expect("decode own format");
        assert_eq!(decoded, players);
        assert_eq!(
            decoded.visible_names().map(|name| name.map(str::to_owned)),
            [Some("MURPHY".to_owned()), Some("ALPHA-7".to_owned()), None]
        );
    }

    /// Confirms malformed names and result bytes cannot enter live state.
    #[test]
    fn decoder_rejects_corrupt_profile_fields() {
        let players = PlayerBook::first_run(2).expect("valid level count");
        let mut encoded = players.encode();
        let result_offset = 8 + 2 + 1 + 1 + 1 + MAX_PLAYER_NAME_LENGTH + 8;
        encoded[result_offset] = 7;
        assert!(matches!(
            PlayerBook::decode(&encoded, 2),
            Err(ProfileError::InvalidLevelResult { value: 7, .. })
        ));

        let current = players.current().expect("default player");
        assert_eq!(current.level_result(1), Some(LevelResult::Unfinished));
    }
}
