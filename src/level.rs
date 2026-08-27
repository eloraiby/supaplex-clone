//! Decoder for the fixed-size records in the original `LEVELS.DAT` file.

use std::{error::Error, fmt};

/// Number of board columns stored in every original Supaplex level.
pub const LEVEL_WIDTH: usize = 60;

/// Number of board rows stored in every original Supaplex level.
pub const LEVEL_HEIGHT: usize = 24;

/// Number of row-major tile bytes at the start of each record.
pub const TILE_COUNT: usize = LEVEL_WIDTH * LEVEL_HEIGHT;

/// Total byte size of one level, including its trailing metadata.
pub const LEVEL_RECORD_SIZE: usize = 1_536;

/// Byte offset of the initial-gravity flag within one record.
const GRAVITY_OFFSET: usize = 1_444;

/// Byte offset of the historical SpeedFix/version byte within one record.
const VERSION_OFFSET: usize = 1_445;

/// Byte offset of the 23-byte, space-padded level title.
const TITLE_OFFSET: usize = 1_446;

/// Fixed byte width of the title field.
const TITLE_LENGTH: usize = 23;

/// Byte offset of the initial freeze-Zonks flag.
const FREEZE_ZONKS_OFFSET: usize = 1_469;

/// Byte offset of the required-Infotron count.
const REQUIRED_INFOTRONS_OFFSET: usize = 1_470;

/// Byte offset of the number of meaningful special-port records.
const SPECIAL_PORT_COUNT_OFFSET: usize = 1_471;

/// Byte offset of the first six-byte special-port record.
const SPECIAL_PORTS_OFFSET: usize = 1_472;

/// Byte width of each special-port record.
const SPECIAL_PORT_SIZE: usize = 6;

/// Maximum number of special ports reserved in an original level record.
const MAX_SPECIAL_PORTS: usize = 10;

/// Highest static tile identifier recognized by classic level editors.
const MAX_TILE_ID: u8 = 40;

/// Read-only view over all fixed-size records in a `LEVELS.DAT` byte slice.
#[derive(Clone, Copy, Debug)]
pub struct LevelSet<'bytes> {
    /// Complete backing bytes; individual records borrow from this slice.
    bytes: &'bytes [u8],
}

impl<'bytes> LevelSet<'bytes> {
    /// Creates a level-set view without doing work until a record is requested.
    pub fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes }
    }

    /// Returns the number of complete records when the file length is valid.
    pub fn level_count(self) -> Result<usize, LevelError> {
        // Partial records are rejected instead of ignored because their
        // presence normally means the source data was truncated.
        if self.bytes.len() % LEVEL_RECORD_SIZE != 0 {
            return Err(LevelError::InvalidFileSize(self.bytes.len()));
        }

        Ok(self.bytes.len() / LEVEL_RECORD_SIZE)
    }

    /// Decodes a one-based level number into tiles and typed metadata.
    pub fn load(self, level_number: usize) -> Result<Level, LevelError> {
        let level_count = self.level_count()?;

        if level_number == 0 || level_number > level_count {
            return Err(LevelError::LevelOutOfRange {
                requested: level_number,
                available: level_count,
            });
        }

        // Convert one-based display numbering to a byte range only after bounds
        // validation, avoiding subtraction underflow for a requested level zero.
        let start = (level_number - 1) * LEVEL_RECORD_SIZE;
        let record = &self.bytes[start..start + LEVEL_RECORD_SIZE];
        Level::from_record(record)
    }
}

/// Fully decoded level ready to become a runtime board.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Level {
    /// Original tile identifiers in row-major `width * y + x` order.
    tiles: Vec<u8>,
    /// Trimmed 23-byte title shown in the HUD and window title.
    title: String,
    /// Whether Murphy is affected by gravity when play begins.
    gravity: bool,
    /// Whether Zonks remain frozen when play begins.
    freeze_zonks: bool,
    /// Stored requirement; zero means count every Infotron on the board.
    required_infotrons: u8,
    /// Original SpeedFix/version marker retained for diagnostics.
    version: u8,
    /// Decoded behaviors for the special one-way ports in this level.
    special_ports: Vec<SpecialPort>,
}

impl Level {
    /// Decodes one already-sized record and validates its tile and port data.
    fn from_record(record: &[u8]) -> Result<Self, LevelError> {
        // This private check keeps the constructor correct if it is later reused
        // for standalone `.SP` files rather than only through `LevelSet`.
        if record.len() != LEVEL_RECORD_SIZE {
            return Err(LevelError::InvalidRecordSize(record.len()));
        }

        let tiles = record[..TILE_COUNT].to_vec();
        for (index, tile) in tiles.iter().copied().enumerate() {
            if tile > MAX_TILE_ID {
                return Err(LevelError::UnknownTile { index, tile });
            }
        }

        // DOS titles are ASCII and padded with spaces/NULs. Lossy decoding is
        // intentional: a damaged title must not prevent an otherwise valid map.
        let title_bytes = &record[TITLE_OFFSET..TITLE_OFFSET + TITLE_LENGTH];
        let title = String::from_utf8_lossy(title_bytes)
            .trim_matches([' ', '\0'])
            .to_owned();

        let port_count = usize::from(record[SPECIAL_PORT_COUNT_OFFSET]);
        if port_count > MAX_SPECIAL_PORTS {
            return Err(LevelError::TooManySpecialPorts(port_count));
        }

        let mut special_ports = Vec::with_capacity(port_count);
        for port_number in 0..port_count {
            let offset = SPECIAL_PORTS_OFFSET + port_number * SPECIAL_PORT_SIZE;
            let port_bytes = &record[offset..offset + SPECIAL_PORT_SIZE];

            // The original coordinate is twice the row-major cell index and is
            // stored in big-endian byte order. Odd values cannot name a cell.
            let encoded_position = u16::from_be_bytes([port_bytes[0], port_bytes[1]]);
            if encoded_position % 2 != 0 {
                return Err(LevelError::InvalidSpecialPortPosition(encoded_position));
            }

            let cell_index = usize::from(encoded_position / 2);
            if cell_index >= TILE_COUNT {
                return Err(LevelError::InvalidSpecialPortPosition(encoded_position));
            }

            // The historical format uses exact sentinel values rather than
            // ordinary booleans: notably, the Zonk flag is enabled by `2`.
            special_ports.push(SpecialPort {
                x: cell_index % LEVEL_WIDTH,
                y: cell_index / LEVEL_WIDTH,
                gravity: port_bytes[2] == 1,
                freeze_zonks: port_bytes[3] == 2,
                freeze_enemies: port_bytes[4] == 1,
            });
        }

        Ok(Self {
            tiles,
            title,
            gravity: record[GRAVITY_OFFSET] == 1,
            freeze_zonks: record[FREEZE_ZONKS_OFFSET] == 2,
            required_infotrons: record[REQUIRED_INFOTRONS_OFFSET],
            version: record[VERSION_OFFSET],
            special_ports,
        })
    }

    /// Returns all tile identifiers in row-major storage order.
    pub fn tiles(&self) -> &[u8] {
        &self.tiles
    }

    /// Returns the tile identifier at `(x, y)` using `width * y + x` access.
    pub fn tile(&self, x: usize, y: usize) -> Option<u8> {
        // Explicit axis checks prevent a large `x` from wrapping into another
        // otherwise-valid row-major index.
        if x >= LEVEL_WIDTH || y >= LEVEL_HEIGHT {
            return None;
        }

        self.tiles.get(LEVEL_WIDTH * y + x).copied()
    }

    /// Returns the trimmed title stored in the record.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Reports whether player gravity starts enabled.
    pub fn gravity(&self) -> bool {
        self.gravity
    }

    /// Reports whether falling Zonks start frozen.
    pub fn freeze_zonks(&self) -> bool {
        self.freeze_zonks
    }

    /// Returns the raw Infotron requirement, where zero means "count the map".
    pub fn required_infotrons(&self) -> u8 {
        self.required_infotrons
    }

    /// Returns the retained original/SpeedFix version marker.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Returns the decoded special-port behavior records.
    pub fn special_ports(&self) -> &[SpecialPort] {
        &self.special_ports
    }
}

/// Behavior toggles applied when Murphy traverses a configured special port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpecialPort {
    /// Zero-based board column containing this port.
    pub x: usize,
    /// Zero-based board row containing this port.
    pub y: usize,
    /// Gravity setting that replaces the current setting after traversal.
    pub gravity: bool,
    /// Zonk-freeze setting that replaces the current setting after traversal.
    pub freeze_zonks: bool,
    /// Enemy-freeze setting that replaces the current setting after traversal.
    pub freeze_enemies: bool,
}

/// Describes malformed level data or a record selection outside the file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LevelError {
    /// The complete file ends with a partial 1,536-byte record.
    InvalidFileSize(usize),
    /// A direct record decoder received a slice of the wrong length.
    InvalidRecordSize(usize),
    /// A one-based level selection was zero or exceeded available records.
    LevelOutOfRange {
        /// One-based number requested by the caller.
        requested: usize,
        /// Number of complete records available in the file.
        available: usize,
    },
    /// A tile byte did not correspond to an original actor identifier.
    UnknownTile {
        /// Row-major location of the invalid byte.
        index: usize,
        /// Unsupported identifier read from the record.
        tile: u8,
    },
    /// More than the ten reserved special-port records were marked active.
    TooManySpecialPorts(usize),
    /// A special-port coordinate was odd or outside the 60×24 board.
    InvalidSpecialPortPosition(u16),
}

impl fmt::Display for LevelError {
    /// Formats enough byte-level context to diagnose corrupt source data.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFileSize(size) => write!(
                formatter,
                "file size {size} is not a multiple of {LEVEL_RECORD_SIZE} bytes"
            ),
            Self::InvalidRecordSize(size) => write!(
                formatter,
                "record size {size} does not equal {LEVEL_RECORD_SIZE} bytes"
            ),
            Self::LevelOutOfRange {
                requested,
                available,
            } => write!(
                formatter,
                "level {requested} is outside the available range 1..={available}"
            ),
            Self::UnknownTile { index, tile } => {
                write!(formatter, "tile {tile} at cell index {index} is unknown")
            }
            Self::TooManySpecialPorts(count) => {
                write!(
                    formatter,
                    "record declares {count} special ports; maximum is 10"
                )
            }
            Self::InvalidSpecialPortPosition(position) => write!(
                formatter,
                "special-port encoded position {position} does not name a board cell"
            ),
        }
    }
}

impl Error for LevelError {}

#[cfg(test)]
mod tests {
    //! Byte-oriented tests for level selection, metadata, and validation.

    use super::{
        FREEZE_ZONKS_OFFSET, GRAVITY_OFFSET, LEVEL_HEIGHT, LEVEL_RECORD_SIZE, LEVEL_WIDTH,
        LevelError, LevelSet, REQUIRED_INFOTRONS_OFFSET, SPECIAL_PORT_COUNT_OFFSET,
        SPECIAL_PORTS_OFFSET, TILE_COUNT, TITLE_LENGTH, TITLE_OFFSET,
    };

    /// Builds one valid, mostly empty record for focused metadata tests.
    fn record() -> Vec<u8> {
        let mut bytes = vec![0; LEVEL_RECORD_SIZE];

        // A single Murphy makes the fixture resemble a minimally valid map;
        // record parsing deliberately does not enforce gameplay design.
        bytes[LEVEL_WIDTH + 1] = 3;
        bytes[TITLE_OFFSET..TITLE_OFFSET + TITLE_LENGTH]
            .copy_from_slice(b"----- TEST LEVEL ----- ");
        bytes
    }

    /// Confirms the canonical dimensions use the requested row-major formula.
    #[test]
    fn reads_tiles_as_width_times_y_plus_x() {
        let mut bytes = record();
        let index = LEVEL_WIDTH * (LEVEL_HEIGHT - 1) + (LEVEL_WIDTH - 1);
        bytes[index] = 7;
        let level = LevelSet::new(&bytes).load(1).expect("record should parse");

        assert_eq!(level.tiles().len(), TILE_COUNT);
        assert_eq!(level.tile(LEVEL_WIDTH - 1, LEVEL_HEIGHT - 1), Some(7));
        assert_eq!(level.tile(LEVEL_WIDTH, 0), None);
    }

    /// Confirms title padding and exact sentinel flags become typed values.
    #[test]
    fn decodes_metadata() {
        let mut bytes = record();
        bytes[GRAVITY_OFFSET] = 1;
        bytes[FREEZE_ZONKS_OFFSET] = 2;
        bytes[REQUIRED_INFOTRONS_OFFSET] = 12;
        let level = LevelSet::new(&bytes).load(1).expect("record should parse");

        assert_eq!(level.title(), "----- TEST LEVEL -----");
        assert!(level.gravity());
        assert!(level.freeze_zonks());
        assert_eq!(level.required_infotrons(), 12);
    }

    /// Confirms big-endian, doubled row-major special-port positions are decoded.
    #[test]
    fn decodes_special_port_coordinates_and_toggles() {
        let mut bytes = record();
        let cell_index = LEVEL_WIDTH * 5 + 7;
        let encoded = u16::try_from(cell_index * 2).expect("fixture index should fit");
        bytes[SPECIAL_PORT_COUNT_OFFSET] = 1;
        bytes[SPECIAL_PORTS_OFFSET..SPECIAL_PORTS_OFFSET + 2]
            .copy_from_slice(&encoded.to_be_bytes());
        bytes[SPECIAL_PORTS_OFFSET + 2] = 1;
        bytes[SPECIAL_PORTS_OFFSET + 3] = 2;
        bytes[SPECIAL_PORTS_OFFSET + 4] = 1;

        let level = LevelSet::new(&bytes).load(1).expect("record should parse");
        let port = level.special_ports()[0];

        assert_eq!((port.x, port.y), (7, 5));
        assert!(port.gravity);
        assert!(port.freeze_zonks);
        assert!(port.freeze_enemies);
    }

    /// Confirms truncation is reported instead of silently dropping data.
    #[test]
    fn rejects_partial_records() {
        let bytes = vec![0; LEVEL_RECORD_SIZE - 1];

        assert_eq!(
            LevelSet::new(&bytes).load(1),
            Err(LevelError::InvalidFileSize(LEVEL_RECORD_SIZE - 1))
        );
    }

    /// Confirms one-based numbering selects the corresponding byte record.
    #[test]
    fn selects_requested_record() {
        let first = record();
        let mut second = record();
        second[0] = 7;
        let bytes = [first, second].concat();

        let level = LevelSet::new(&bytes)
            .load(2)
            .expect("second record should parse");

        assert_eq!(level.tile(0, 0), Some(7));
    }
}
