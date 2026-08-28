//! Register-level playback of the original Supaplex Yamaha YM3812 soundtrack.
//!
//! The DOS `ADLIB.SND` file combines 16-bit driver code, sequencing data, and
//! instrument definitions. The production `music.opl` asset is a deterministic
//! capture of the writes made by that driver during one complete composition,
//! preserving its instruments without executing real-mode code at runtime.
//! This module validates that compact stream, schedules its original 50 Hz
//! ticks, and sends each register/value pair to a native Rust OPL2 emulator.
//!
//! The little-endian stream begins with an eight-byte versioned signature, a
//! `u16` tick rate, a `u32` tick count, a `u8` initialization-write count, and
//! the 32-byte SHA-256 of its source `ADLIB.SND`. Initialization register/value
//! pairs follow directly. Every timer record then stores a `u8` write count and
//! that many register/value pairs; a zero count represents one silent delay
//! tick. The capture calls DOS command zero once and command one at the game's
//! 50 Hz PIT rate. Tick 15,621 reproduces the opening register sequence, making
//! records 0 through 15,620 one natural looping traversal.

use std::{error::Error, fmt};

use oplon::Opl2;

/// Versioned signature at the start of every supported register-stream asset.
const STREAM_MAGIC: [u8; 8] = *b"SPOPL\x1a\x01\0";

/// Exact rate at which Supaplex's replacement interrupt-eight handler ticks.
const ORIGINAL_TICK_RATE: u32 = 50;

/// SHA-256 of the exact 5,354-byte DOS `ADLIB.SND` used for the capture.
///
/// Retaining this identity in both code and the asset prevents an unrelated
/// register dump from being mislabeled as the original Supaplex soundtrack.
const ADLIB_SND_SHA256: [u8; 32] = [
    0xc4, 0xfe, 0x78, 0x65, 0x15, 0x7e, 0xe8, 0xf9, 0x82, 0x82, 0xa8, 0x47, 0xf0, 0x55, 0xef, 0x8f,
    0xf5, 0xb7, 0x5a, 0x72, 0x9a, 0xed, 0x82, 0xb4, 0x99, 0x13, 0x22, 0x0a, 0x8f, 0xb5, 0xcf, 0x44,
];

/// Divisor that maps oplon's signed integer mix into normalized SDL samples.
const OPL_SAMPLE_SCALE: f32 = 32_768.0;

/// One hardware programming operation emitted by the original DOS driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegisterWrite {
    /// YM3812 register selected through the AdLib address port.
    register: u8,
    /// Eight-bit value sent through the AdLib data port.
    value: u8,
}

/// Contiguous location of one timer tick's writes in the shared write array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TickWrites {
    /// Index of the first write belonging to this tick.
    start: usize,
    /// Number of consecutive writes emitted during this tick.
    count: usize,
}

/// Fully validated register program used by the allocation-free audio callback.
#[derive(Debug)]
struct OplSong {
    /// Original driver interrupt frequency in ticks per second.
    ticks_per_second: u32,
    /// One-time chip initialization performed by DOS command zero.
    initial_writes: Box<[RegisterWrite]>,
    /// Per-tick spans in [`Self::writes`], including deliberately empty ticks.
    ticks: Box<[TickWrites]>,
    /// Contiguous storage for every write emitted after initialization.
    writes: Box<[RegisterWrite]>,
}

impl OplSong {
    /// Parses and validates one complete Supaplex OPL register-stream payload.
    fn parse(bytes: &[u8]) -> Result<Self, OplError> {
        // All variable-length sections use one checked cursor. A malformed
        // count therefore becomes a startup error and cannot reach playback.
        let mut reader = StreamReader::new(bytes);
        let magic = reader.read_array::<8>()?;
        if magic != STREAM_MAGIC {
            return Err(OplError::InvalidMagic);
        }
        let ticks_per_second = u32::from(reader.read_u16()?);
        if ticks_per_second != ORIGINAL_TICK_RATE {
            return Err(OplError::InvalidTickRate { ticks_per_second });
        }
        let tick_count =
            usize::try_from(reader.read_u32()?).map_err(|_| OplError::TickCountTooLarge)?;
        if tick_count == 0 {
            return Err(OplError::EmptySong);
        }
        let initial_write_count = usize::from(reader.read_u8()?);
        let source_hash = reader.read_array::<32>()?;
        if source_hash != ADLIB_SND_SHA256 {
            return Err(OplError::WrongSourceDriver);
        }

        // Initialization writes remain separate because they are applied once
        // at construction, before the repeating 50 Hz sequence begins.
        let mut initial_writes = Vec::with_capacity(initial_write_count);
        for _ in 0..initial_write_count {
            initial_writes.push(reader.read_register_write()?);
        }

        // A single contiguous vector avoids thousands of small allocations.
        // Tick spans make empty delay records just as cheap as populated ones.
        let mut ticks = Vec::with_capacity(tick_count);
        let mut writes = Vec::new();
        for _ in 0..tick_count {
            let write_count = usize::from(reader.read_u8()?);
            let start = writes.len();
            for _ in 0..write_count {
                writes.push(reader.read_register_write()?);
            }
            ticks.push(TickWrites {
                start,
                count: write_count,
            });
        }
        if reader.remaining() != 0 {
            return Err(OplError::TrailingBytes {
                count: reader.remaining(),
            });
        }

        Ok(Self {
            ticks_per_second,
            initial_writes: initial_writes.into_boxed_slice(),
            ticks: ticks.into_boxed_slice(),
            writes: writes.into_boxed_slice(),
        })
    }

    /// Borrows all register writes emitted by one validated timer tick.
    fn tick_writes(&self, tick_index: usize) -> &[RegisterWrite] {
        // Parser-created spans are internal and proven to lie inside `writes`;
        // indexing the tick still deliberately panics on a player logic defect.
        let tick = self.ticks[tick_index];
        &self.writes[tick.start..tick.start + tick.count]
    }
}

/// Native OPL2 synthesizer and fixed-rate scheduler owned by SDL's callback.
pub(crate) struct OplPlayer {
    /// Integer YM3812 emulator receiving the original register operations.
    chip: Opl2,
    /// Immutable parsed song data shared by all successive callback buffers.
    song: OplSong,
    /// Next timer-tick record to apply, wrapping at the composition boundary.
    next_tick: usize,
    /// Fractional tick numerator retained across output frames and callbacks.
    tick_phase: u32,
    /// Obtained SDL output frequency used as the scheduler denominator.
    sample_rate: u32,
    /// Total stereo frames rendered, exposed for callback timing verification.
    rendered_frames: u64,
}

impl OplPlayer {
    /// Parses the original stream and prepares synthesis at one output rate.
    pub(crate) fn new(bytes: &[u8], sample_rate: u32) -> Result<Self, OplError> {
        // Neither the chip resampler nor the rational tick scheduler can use a
        // zero denominator, so reject it before constructing callback state.
        if sample_rate == 0 {
            return Err(OplError::InvalidSampleRate { sample_rate });
        }
        let song = OplSong::parse(bytes)?;
        if sample_rate < song.ticks_per_second {
            return Err(OplError::InvalidSampleRate { sample_rate });
        }
        let mut chip = Opl2::new(sample_rate);

        // DOS command zero initializes global chip mode and silences all nine
        // channels. The first musical tick intentionally follows 20 ms later.
        for write in &song.initial_writes {
            chip.write_reg(write.register, write.value);
        }

        Ok(Self {
            chip,
            song,
            next_tick: 0,
            tick_phase: 0,
            sample_rate,
            rendered_frames: 0,
        })
    }

    /// Adds synthesized stereo frames to an interleaved floating-point buffer.
    pub(crate) fn mix_stereo(&mut self, output: &mut [f32], gain: f32) {
        // SDL always requests whole stereo frames. Ignoring a hypothetical odd
        // tail is safer than treating it as a left sample and losing alignment.
        let (frames, remainder) = output.as_chunks_mut::<2>();
        debug_assert!(remainder.is_empty(), "SDL supplies whole stereo frames");
        for frame in frames {
            let (left, right) = self.chip.render_frame();
            let left = left as f32 / OPL_SAMPLE_SCALE * gain;
            let right = right as f32 / OPL_SAMPLE_SCALE * gain;
            frame[0] = (frame[0] + left).clamp(-1.0, 1.0);
            frame[1] = (frame[1] + right).clamp(-1.0, 1.0);
            self.rendered_frames = self.rendered_frames.saturating_add(1);

            // Adding the tick rate once per frame and subtracting the sample
            // rate at each crossing produces an exact rational clock. It also
            // remains drift-free when a backend supplies a non-44.1-kHz rate.
            self.tick_phase += self.song.ticks_per_second;
            while self.tick_phase >= self.sample_rate {
                self.tick_phase -= self.sample_rate;
                self.advance_tick();
            }
        }
    }

    /// Returns the number of stereo sample frames consumed by this player.
    #[cfg(test)]
    pub(crate) fn rendered_frames(&self) -> u64 {
        // The monotonically increasing counter advances only beside one call to
        // `render_frame`, so callers can compare it directly with buffer frames.
        self.rendered_frames
    }

    /// Applies one captured DOS timer record and advances the looping cursor.
    fn advance_tick(&mut self) {
        // Borrow distinct fields so the immutable stream and mutable chip can
        // be used together without copying or allocating register records.
        let writes = self.song.tick_writes(self.next_tick);
        for write in writes {
            self.chip.write_reg(write.register, write.value);
        }
        self.next_tick += 1;
        if self.next_tick == self.song.ticks.len() {
            // The captured traversal begins and ends at the driver's own song
            // restart, so retaining chip envelopes makes the boundary natural.
            self.next_tick = 0;
        }
    }
}

/// Describes why an OPL stream could not become safe callback state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OplError {
    /// A declared field or register record extends beyond the payload.
    Truncated {
        /// Byte offset at which the requested field begins.
        offset: usize,
        /// Number of bytes required by the requested field.
        needed: usize,
        /// Number of bytes actually remaining in the payload.
        remaining: usize,
    },
    /// The versioned `SPOPL` signature is missing or unsupported.
    InvalidMagic,
    /// The asset's timer frequency differs from the original fixed 50 Hz rate.
    InvalidTickRate {
        /// Invalid number of driver ticks requested per second.
        ticks_per_second: u32,
    },
    /// The declared 32-bit tick count cannot fit this platform's indices.
    TickCountTooLarge,
    /// The stream contains no timer records and therefore cannot loop.
    EmptySong,
    /// The embedded provenance hash does not identify Supaplex `ADLIB.SND`.
    WrongSourceDriver,
    /// Complete parsing left bytes outside every declared stream record.
    TrailingBytes {
        /// Number of unclaimed bytes following the last timer record.
        count: usize,
    },
    /// The requested SDL output rate cannot represent every original tick.
    InvalidSampleRate {
        /// Invalid output sample frequency supplied by the caller.
        sample_rate: u32,
    },
}

impl fmt::Display for OplError {
    /// Formats enough structural context to diagnose a bad production asset.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keep errors independent of filesystem paths because assets may be
        // embedded; the caller supplies the production-music context.
        match self {
            Self::Truncated {
                offset,
                needed,
                remaining,
            } => write!(
                formatter,
                "OPL offset {offset}: need {needed} bytes but only {remaining} remain"
            ),
            Self::InvalidMagic => write!(formatter, "missing supported SPOPL stream signature"),
            Self::InvalidTickRate { ticks_per_second } => {
                write!(
                    formatter,
                    "invalid OPL tick rate {ticks_per_second}; expected {ORIGINAL_TICK_RATE}"
                )
            }
            Self::TickCountTooLarge => {
                write!(formatter, "OPL tick count does not fit this platform")
            }
            Self::EmptySong => write!(formatter, "OPL stream contains no timer ticks"),
            Self::WrongSourceDriver => {
                write!(
                    formatter,
                    "OPL stream was not captured from Supaplex ADLIB.SND"
                )
            }
            Self::TrailingBytes { count } => {
                write!(formatter, "OPL stream leaves {count} trailing bytes")
            }
            Self::InvalidSampleRate { sample_rate } => {
                write!(formatter, "invalid OPL output sample rate {sample_rate}")
            }
        }
    }
}

impl Error for OplError {}

/// Bounds-checked little-endian cursor for the compact stream format.
struct StreamReader<'bytes> {
    /// Complete immutable asset being parsed.
    bytes: &'bytes [u8],
    /// Offset of the next unread byte.
    position: usize,
}

impl<'bytes> StreamReader<'bytes> {
    /// Starts a cursor at byte zero of one borrowed asset.
    fn new(bytes: &'bytes [u8]) -> Self {
        // The cursor carries no hidden format state; callers consume every
        // field explicitly in its documented on-disk order.
        Self { bytes, position: 0 }
    }

    /// Returns exactly `length` bytes or a position-rich truncation error.
    fn take(&mut self, length: usize) -> Result<&'bytes [u8], OplError> {
        // Checked addition handles theoretical index overflow before comparing
        // the requested end with the concrete payload length.
        let start = self.position;
        let end = start.checked_add(length).ok_or(OplError::Truncated {
            offset: start,
            needed: length,
            remaining: self.bytes.len().saturating_sub(start),
        })?;
        if end > self.bytes.len() {
            return Err(OplError::Truncated {
                offset: start,
                needed: length,
                remaining: self.bytes.len().saturating_sub(start),
            });
        }
        self.position = end;
        Ok(&self.bytes[start..end])
    }

    /// Reads one byte from the current position.
    fn read_u8(&mut self) -> Result<u8, OplError> {
        // `take` proves the single-byte index before it is dereferenced.
        Ok(self.take(1)?[0])
    }

    /// Reads one little-endian unsigned 16-bit field.
    fn read_u16(&mut self) -> Result<u16, OplError> {
        // The stream is little-endian like the original x86 driver data.
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    /// Reads one little-endian unsigned 32-bit field.
    fn read_u32(&mut self) -> Result<u32, OplError> {
        // A fixed array makes byte order explicit and avoids alignment casts.
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    /// Reads a compile-time-sized byte array without alignment assumptions.
    fn read_array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], OplError> {
        // The slice length is proven by `take`, so conversion cannot fail; use
        // `copy_from_slice` to keep the invariant independent of `unwrap`.
        let mut array = [0; LENGTH];
        array.copy_from_slice(self.take(LENGTH)?);
        Ok(array)
    }

    /// Reads one adjacent register/value pair from the stream.
    fn read_register_write(&mut self) -> Result<RegisterWrite, OplError> {
        // On disk, the address byte precedes the value exactly as captured from
        // the driver's central AdLib output routine.
        Ok(RegisterWrite {
            register: self.read_u8()?,
            value: self.read_u8()?,
        })
    }

    /// Reports the number of bytes not yet claimed by parsed fields.
    fn remaining(&self) -> usize {
        // `position` never exceeds the payload because only `take` mutates it.
        self.bytes.len() - self.position
    }
}

#[cfg(test)]
mod tests {
    //! Production-format and synthesis tests independent of an SDL device.

    use super::{ADLIB_SND_SHA256, OplError, OplPlayer, OplSong, STREAM_MAGIC};

    /// Exact register stream embedded or loaded by the production audio layer.
    const MUSIC: &[u8] = include_bytes!("../assets/audio/music.opl");

    /// Confirms the generated stream retains its source identity and dimensions.
    #[test]
    fn production_stream_has_original_driver_metadata() {
        let song = OplSong::parse(MUSIC).expect("the production OPL stream should parse");

        assert_eq!(&MUSIC[..8], &STREAM_MAGIC);
        assert_eq!(&MUSIC[15..47], &ADLIB_SND_SHA256);
        assert_eq!(song.ticks_per_second, 50);
        assert_eq!(song.initial_writes.len(), 11);
        assert_eq!(song.ticks.len(), 15_621);
        assert_eq!(song.writes.len(), 30_548);
    }

    /// Confirms the original stream produces audible centered OPL2 samples.
    #[test]
    fn production_stream_renders_audible_mono_chip_output() {
        let mut player = OplPlayer::new(MUSIC, 44_100).expect("OPL player should construct");
        let mut output = vec![0.0; 44_100 * 2];

        player.mix_stereo(&mut output, 1.0);

        assert_eq!(player.rendered_frames(), 44_100);
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
        let (frames, remainder) = output.as_chunks::<2>();
        assert!(remainder.is_empty());
        assert!(frames.iter().all(|frame| frame[0] == frame[1]));
    }

    /// Confirms provenance corruption is rejected before any chip is created.
    #[test]
    fn rejects_a_stream_with_the_wrong_source_driver() {
        let mut corrupted = MUSIC.to_vec();
        corrupted[15] ^= 0xff;

        assert!(matches!(
            OplSong::parse(&corrupted),
            Err(OplError::WrongSourceDriver)
        ));
    }

    /// Confirms a corrupt timing header cannot silently change music tempo.
    #[test]
    fn rejects_a_stream_with_a_non_original_tick_rate() {
        let mut corrupted = MUSIC.to_vec();
        corrupted[8..10].copy_from_slice(&49_u16.to_le_bytes());

        assert!(matches!(
            OplSong::parse(&corrupted),
            Err(OplError::InvalidTickRate {
                ticks_per_second: 49
            })
        ));
    }

    /// Confirms declared records may not extend beyond the supplied bytes.
    #[test]
    fn rejects_a_truncated_tick_record() {
        let error = OplSong::parse(&MUSIC[..MUSIC.len() - 1])
            .expect_err("a shortened final record must fail");

        assert!(matches!(error, OplError::Truncated { .. }));
    }
}
