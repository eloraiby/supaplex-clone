//! Direct playback of the original Supaplex `ADLIB.SND` driver and score.
//!
//! `ADLIB.SND` is not sampled audio or a conventional music container. It is a
//! 5,354-byte DOS sound driver containing compact sequence bytecode, instrument
//! definitions, frequency tables, and 16-bit routines that interpret them. This
//! module ports the music-specific behavior of those routines to safe Rust. At
//! the original 50 Hz interrupt rate the sequencer reads the source file's own
//! event streams and emits register changes directly into a YM3812 emulator.
//! Nothing is captured, prerendered, or expanded into a register timeline.

use std::{error::Error, fmt};

use oplon::Opl2;

/// Exact byte length of the original Supaplex `ADLIB.SND` file.
const DRIVER_LENGTH: usize = 5_354;

/// CRC-32 of the exact original driver accepted by this source-specific port.
const DRIVER_CRC32: u32 = 0xd536_9dfa;

/// Rate at which Supaplex invokes command one of the DOS sound driver.
const ORIGINAL_TICK_RATE: u32 = 50;

/// Divisor mapping oplon's signed integer samples into normalized SDL samples.
const OPL_SAMPLE_SCALE: f32 = 32_768.0;

/// Number of melodic OPL channels advanced by the original music routine.
const MUSIC_CHANNEL_COUNT: usize = 5;

/// Offset of the selected score's tempo byte inside `ADLIB.SND`.
const SCORE_METADATA_OFFSET: usize = 0x0f93;

/// Offset of the selected score's five pattern-list pointers.
const TRACK_POINTERS_OFFSET: usize = SCORE_METADATA_OFFSET + 1;

/// Offset of the little-endian instrument-definition pointer table.
const INSTRUMENT_POINTERS_OFFSET: usize = 0x0128;

/// Offset of the low eight bits of the driver's 96-note frequency table.
const FREQUENCY_LOW_OFFSET: usize = 0x0068;

/// Offset of the high frequency and block bits for the same 96 notes.
const FREQUENCY_HIGH_OFFSET: usize = 0x00c8;

/// Register bases programmed for each operator in an instrument definition.
const OPERATOR_REGISTER_BASES: [u8; 5] = [0x60, 0x80, 0x20, 0x40, 0xe0];

/// One hardware programming operation produced by the live sequencer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegisterWrite {
    /// YM3812 register selected through the AdLib address port.
    register: u8,
    /// Eight-bit value sent through the AdLib data port.
    value: u8,
}

/// Original OPL channel and operator offsets used by one melodic voice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChannelRouting {
    /// Zero-based melodic channel added to the `A0`, `B0`, and `C0` registers.
    channel: u8,
    /// Register offset receiving bytes zero through four of an instrument.
    first_operator: u8,
    /// Register offset receiving bytes five through nine of an instrument.
    second_operator: u8,
}

/// Literal routing assigned before each of the driver's five channel calls.
const CHANNEL_ROUTING: [ChannelRouting; MUSIC_CHANNEL_COUNT] = [
    ChannelRouting {
        channel: 0,
        first_operator: 3,
        second_operator: 0,
    },
    ChannelRouting {
        channel: 1,
        first_operator: 4,
        second_operator: 1,
    },
    ChannelRouting {
        channel: 2,
        first_operator: 5,
        second_operator: 2,
    },
    ChannelRouting {
        channel: 3,
        first_operator: 11,
        second_operator: 8,
    },
    ChannelRouting {
        channel: 4,
        first_operator: 12,
        second_operator: 9,
    },
];

/// Music bytecodes dispatched by the DOS routine's jump table at offset `0x52`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SequenceCommand {
    /// Release the current note and wait for the selected duration.
    KeyOff,
    /// Wait without changing the current channel registers.
    FinishEvent,
    /// Select the next pattern pointer, wrapping the current pattern list.
    NextPattern,
    /// Stop all melodic sequencing immediately.
    Stop,
    /// Replace the signed wrapping transpose applied only to this channel.
    SetChannelTranspose,
    /// Replace the signed wrapping transpose shared by all five channels.
    SetGlobalTranspose,
    /// Load one eleven-byte instrument and program both operators plus feedback.
    SetInstrument,
    /// Replace the first operator's output level while preserving its scale bits.
    SetVolume,
    /// Replace the current channel's pattern-list pointer.
    SetPatternList,
    /// Replace the score tempo increment used by the 8-bit rate accumulator.
    SetTempo,
    /// Replace the wrapping adjustment applied to each note's low frequency byte.
    SetFrequencyAdjustment,
}

impl SequenceCommand {
    /// Converts the exact byte range represented by the original jump table.
    fn from_byte(byte: u8) -> Option<Self> {
        // Explicit values document the source driver's bytecode and prevent a
        // Rust enum representation from becoming part of the file format.
        match byte {
            0x80 => Some(Self::KeyOff),
            0x81 => Some(Self::FinishEvent),
            0x82 => Some(Self::NextPattern),
            0x83 => Some(Self::Stop),
            0x84 => Some(Self::SetChannelTranspose),
            0x85 => Some(Self::SetGlobalTranspose),
            0x86 => Some(Self::SetInstrument),
            0x87 => Some(Self::SetVolume),
            0x88 => Some(Self::SetPatternList),
            0x89 => Some(Self::SetTempo),
            0x8a => Some(Self::SetFrequencyAdjustment),
            _ => None,
        }
    }
}

/// Immutable, validated original driver bytes used as live playback data.
#[derive(Debug)]
struct DriverImage {
    /// Exact `ADLIB.SND` payload, including its code, tables, instruments, and score.
    bytes: Box<[u8]>,
}

impl DriverImage {
    /// Validates and owns the one driver revision understood by this faithful port.
    fn new(bytes: &[u8]) -> Result<Self, OplError> {
        // The sequencer intentionally uses the original driver's absolute data
        // offsets. Checking both size and checksum makes all later reads safe and
        // rejects a different driver revision instead of misinterpreting it.
        if bytes.len() != DRIVER_LENGTH {
            return Err(OplError::InvalidDriverLength {
                actual: bytes.len(),
            });
        }
        let actual = crc32(bytes);
        if actual != DRIVER_CRC32 {
            return Err(OplError::WrongDriverChecksum { actual });
        }
        Ok(Self {
            bytes: bytes.to_vec().into_boxed_slice(),
        })
    }

    /// Reads one source byte at a driver-defined absolute offset.
    fn byte(&self, offset: usize) -> u8 {
        // Construction proves this is the exact known-size source image, while
        // every dynamic pointer comes from that same checksum-validated image.
        self.bytes[offset]
    }

    /// Reads one source pointer or scalar in the 8086 driver's little-endian order.
    fn word(&self, offset: usize) -> u16 {
        // Driver pointers are offsets in its own segment; keeping them as `u16`
        // retains the original arithmetic until an individual read is required.
        u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]])
    }

    /// Resolves one instrument number through the original pointer table.
    fn instrument_offset(&self, instrument: u8) -> usize {
        // The assembly doubles the eight-bit index before reading this table.
        usize::from(self.word(INSTRUMENT_POINTERS_OFFSET + usize::from(instrument) * 2))
    }
}

/// Mutable twelve-byte logical state maintained by one original music channel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ChannelState {
    /// First pointer in the current zero-terminated, wrapping pattern list.
    pattern_list_start: u16,
    /// Pointer to the current entry within that pattern list.
    pattern_list_cursor: u16,
    /// Pointer to the next bytecode in the current pattern stream.
    event_cursor: u16,
    /// Music-rate advances remaining before another event is decoded.
    ticks_remaining: u8,
    /// Most recently decoded duration token, reused by the next terminal event.
    next_duration: u8,
    /// Instrument number used by a later volume bytecode.
    instrument: u8,
    /// Key-off form of the current `B0` frequency byte.
    frequency_high: u8,
    /// Wrapping adjustment added to the low frequency byte for every note.
    frequency_adjustment: u8,
    /// Wrapping semitone offset applied only to this channel.
    transpose: u8,
}

/// Safe Rust port of commands zero and one from the original DOS music driver.
#[derive(Debug)]
struct AdlibSequencer {
    /// Original file that remains the sole source of score and instrument data.
    driver: DriverImage,
    /// Five independent pattern cursors and current-voice parameters.
    channels: [ChannelState; MUSIC_CHANNEL_COUNT],
    /// Increment added to the wrapping rate accumulator on every 50 Hz call.
    tempo_increment: u8,
    /// Eight-bit accumulator whose carry advances all five music channels.
    tempo_accumulator: u8,
    /// Wrapping semitone offset applied before each channel-specific transpose.
    global_transpose: u8,
    /// Driver flag cleared only if the score executes its stop bytecode.
    playing: bool,
}

impl AdlibSequencer {
    /// Executes the state setup performed by driver command zero with song zero.
    fn new(bytes: &[u8]) -> Result<Self, OplError> {
        let driver = DriverImage::new(bytes)?;
        let mut channels = [ChannelState::default(); MUSIC_CHANNEL_COUNT];

        // Song zero stores a tempo byte followed by five pattern-list pointers.
        // Each list's first word names the opening bytecode stream.
        for (channel_index, channel) in channels.iter_mut().enumerate() {
            let list = driver.word(TRACK_POINTERS_OFFSET + channel_index * 2);
            *channel = ChannelState {
                pattern_list_start: list,
                pattern_list_cursor: list,
                event_cursor: driver.word(usize::from(list)),
                ticks_remaining: 1,
                ..ChannelState::default()
            };
        }

        Ok(Self {
            tempo_increment: driver.byte(SCORE_METADATA_OFFSET),
            tempo_accumulator: 0xff,
            driver,
            channels,
            global_transpose: 0,
            playing: true,
        })
    }

    /// Emits the chip initialization and channel reset performed by command zero.
    fn initialize_chip(&self, output: &mut impl FnMut(RegisterWrite)) {
        // These first four writes are the original hardware initialization at
        // driver offset 0x41c, including rhythm-mode setup in register BD.
        emit(output, 0x01, 0x00);
        emit(output, 0x04, 0x00);
        emit(output, 0x08, 0x00);
        emit(output, 0xbd, 0x40);

        // Command zero then releases the five music channels and the two effect
        // channels. Fresh channel states contain the same zero frequency bytes
        // read by the original routine at offset 0x734.
        for (channel, state) in self.channels.iter().enumerate() {
            emit(output, 0xb0 + channel as u8, state.frequency_high);
        }
        emit(output, 0xb8, 0x00);
        emit(output, 0xb7, 0x00);
    }

    /// Executes one command-one call at the original 50 Hz interrupt cadence.
    fn advance_tick(&mut self, output: &mut impl FnMut(RegisterWrite)) {
        // The DOS driver advances music only when this eight-bit addition
        // carries. Its initial FF value deliberately advances on the first tick.
        let (next_accumulator, carry) =
            self.tempo_accumulator.overflowing_add(self.tempo_increment);
        self.tempo_accumulator = next_accumulator;
        if !carry || !self.playing {
            return;
        }

        // Channel order is significant because tempo and global-transpose
        // bytecodes take effect immediately for every channel that follows.
        for (channel_index, routing) in CHANNEL_ROUTING.into_iter().enumerate() {
            self.advance_channel(channel_index, routing, output);
            if !self.playing {
                break;
            }
        }
    }

    /// Decodes bytecodes until one event establishes the channel's next delay.
    fn advance_channel(
        &mut self,
        channel_index: usize,
        routing: ChannelRouting,
        output: &mut impl FnMut(RegisterWrite),
    ) {
        // Work on a copy so commands may update sequencer-wide state without
        // aliasing the selected channel; terminal events copy it back exactly once.
        let mut channel = self.channels[channel_index];
        channel.ticks_remaining = channel.ticks_remaining.wrapping_sub(1);
        if channel.ticks_remaining != 0 {
            self.channels[channel_index] = channel;
            return;
        }
        let mut cursor = channel.event_cursor;

        loop {
            let byte = self.driver.byte(usize::from(cursor));
            cursor = cursor.wrapping_add(1);

            // E0 through FF encode durations one through 32. Several duration
            // tokens may precede a command; the final one remains in effect.
            if byte >= 0xe0 {
                channel.next_duration = byte.wrapping_add(0x20).wrapping_add(1);
                continue;
            }

            if byte < 0x80 {
                // Notes index the source driver's split F-number table after two
                // wrapping transposes, exactly matching the original 8-bit adds.
                let note = byte
                    .wrapping_add(self.global_transpose)
                    .wrapping_add(channel.transpose);
                emit(output, 0xb0 + routing.channel, channel.frequency_high);
                emit(
                    output,
                    0xa0 + routing.channel,
                    self.driver
                        .byte(FREQUENCY_LOW_OFFSET + usize::from(note))
                        .wrapping_add(channel.frequency_adjustment),
                );
                channel.frequency_high =
                    self.driver.byte(FREQUENCY_HIGH_OFFSET + usize::from(note));
                emit(
                    output,
                    0xb0 + routing.channel,
                    channel.frequency_high | 0x20,
                );
                self.finish_channel_event(channel_index, channel, cursor);
                return;
            }

            let command = SequenceCommand::from_byte(byte)
                .expect("checksum-validated ADLIB.SND contains only known music bytecodes");
            match command {
                SequenceCommand::KeyOff => {
                    emit(output, 0xb0 + routing.channel, channel.frequency_high);
                    self.finish_channel_event(channel_index, channel, cursor);
                    return;
                }
                SequenceCommand::FinishEvent => {
                    self.finish_channel_event(channel_index, channel, cursor);
                    return;
                }
                SequenceCommand::NextPattern => {
                    // Move to the next word in the active pattern list; its zero
                    // terminator loops to the list start without resetting voices.
                    let mut list_cursor = channel.pattern_list_cursor.wrapping_add(2);
                    if self.driver.word(usize::from(list_cursor)) == 0 {
                        list_cursor = channel.pattern_list_start;
                    }
                    channel.pattern_list_cursor = list_cursor;
                    cursor = self.driver.word(usize::from(list_cursor));
                }
                SequenceCommand::Stop => {
                    // The original handler abandons the current channel call and
                    // clears the global music-active flag without a final delay.
                    self.playing = false;
                    return;
                }
                SequenceCommand::SetChannelTranspose => {
                    channel.transpose = self.read_event_operand(&mut cursor);
                }
                SequenceCommand::SetGlobalTranspose => {
                    self.global_transpose = self.read_event_operand(&mut cursor);
                }
                SequenceCommand::SetInstrument => {
                    channel.instrument = self.read_event_operand(&mut cursor);
                    self.program_instrument(channel.instrument, routing, output);
                }
                SequenceCommand::SetVolume => {
                    let volume = self.read_event_operand(&mut cursor) & 0x3f;
                    let instrument = self.driver.instrument_offset(channel.instrument);
                    let scale_bits = self.driver.byte(instrument + 3) & 0xc0;
                    emit(
                        output,
                        0x40 + routing.first_operator,
                        scale_bits | (0x3f - volume),
                    );
                }
                SequenceCommand::SetPatternList => {
                    // This operand is a little-endian absolute driver pointer,
                    // unlike all other one-byte command operands.
                    let list = self.driver.word(usize::from(cursor));
                    channel.pattern_list_start = list;
                    channel.pattern_list_cursor = list;
                    cursor = self.driver.word(usize::from(list));
                }
                SequenceCommand::SetTempo => {
                    self.tempo_increment = self.read_event_operand(&mut cursor);
                }
                SequenceCommand::SetFrequencyAdjustment => {
                    channel.frequency_adjustment = self.read_event_operand(&mut cursor);
                }
            }
        }
    }

    /// Reads one ordinary bytecode operand and advances its 16-bit source cursor.
    fn read_event_operand(&self, cursor: &mut u16) -> u8 {
        // Wrapping retains the 8086 offset semantics; the accepted source image
        // never approaches the segment boundary during music playback.
        let value = self.driver.byte(usize::from(*cursor));
        *cursor = cursor.wrapping_add(1);
        value
    }

    /// Stores the event cursor and reloads the delay chosen by duration bytecode.
    fn finish_channel_event(
        &mut self,
        channel_index: usize,
        mut channel: ChannelState,
        cursor: u16,
    ) {
        // The driver permits a zero duration through mutable state, so wrapping
        // decrement behavior remains deliberate rather than being normalized.
        channel.ticks_remaining = channel.next_duration;
        channel.event_cursor = cursor;
        self.channels[channel_index] = channel;
    }

    /// Programs both operators and channel feedback from one source instrument.
    fn program_instrument(
        &self,
        instrument_number: u8,
        routing: ChannelRouting,
        output: &mut impl FnMut(RegisterWrite),
    ) {
        let instrument = self.driver.instrument_offset(instrument_number);

        // The eleven-byte layout and unusual operator order are copied directly
        // from the original routine at offset 0x603.
        for (field, register_base) in OPERATOR_REGISTER_BASES.into_iter().enumerate() {
            emit(
                output,
                register_base + routing.first_operator,
                self.driver.byte(instrument + field),
            );
        }
        for (field, register_base) in OPERATOR_REGISTER_BASES.into_iter().enumerate() {
            emit(
                output,
                register_base + routing.second_operator,
                self.driver.byte(instrument + 5 + field),
            );
        }
        emit(
            output,
            0xc0 + routing.channel,
            self.driver.byte(instrument + 10),
        );
    }
}

/// Native OPL2 synthesizer and exact fixed-rate scheduler owned by SDL audio.
pub(crate) struct OplPlayer {
    /// Integer YM3812 emulator receiving live sequencer register operations.
    chip: Opl2,
    /// Direct interpreter over the compact score and instruments in `ADLIB.SND`.
    sequencer: AdlibSequencer,
    /// Fractional 50 Hz tick numerator retained across frames and callbacks.
    tick_phase: u32,
    /// Obtained SDL output frequency used as the scheduler denominator.
    sample_rate: u32,
    /// Total stereo frames rendered, exposed for callback timing verification.
    rendered_frames: u64,
}

impl OplPlayer {
    /// Validates `ADLIB.SND`, initializes its driver state, and creates the chip.
    pub(crate) fn new(bytes: &[u8], sample_rate: u32) -> Result<Self, OplError> {
        // A rate below 50 Hz cannot schedule every original interrupt call, and
        // zero would also make the rational-clock denominator invalid.
        if sample_rate < ORIGINAL_TICK_RATE {
            return Err(OplError::InvalidSampleRate { sample_rate });
        }
        let sequencer = AdlibSequencer::new(bytes)?;
        let mut chip = Opl2::new(sample_rate);
        sequencer.initialize_chip(&mut |write| {
            chip.write_reg(write.register, write.value);
        });

        Ok(Self {
            chip,
            sequencer,
            tick_phase: 0,
            sample_rate,
            rendered_frames: 0,
        })
    }

    /// Adds freshly synthesized stereo frames to one interleaved SDL buffer.
    pub(crate) fn mix_stereo(&mut self, output: &mut [f32], gain: f32) {
        // SDL requests complete stereo frames. Ignoring a hypothetical odd tail
        // is safer than shifting channel alignment for all subsequent samples.
        let (frames, remainder) = output.as_chunks_mut::<2>();
        debug_assert!(remainder.is_empty(), "SDL supplies complete stereo frames");
        for frame in frames {
            let (left, right) = self.chip.render_frame();
            frame[0] = (frame[0] + left as f32 / OPL_SAMPLE_SCALE * gain).clamp(-1.0, 1.0);
            frame[1] = (frame[1] + right as f32 / OPL_SAMPLE_SCALE * gain).clamp(-1.0, 1.0);
            self.rendered_frames = self.rendered_frames.saturating_add(1);

            // Rational accumulation is drift-free even when SDL opens a device
            // at a rate that is not evenly divisible by the original 50 Hz.
            self.tick_phase += ORIGINAL_TICK_RATE;
            while self.tick_phase >= self.sample_rate {
                self.tick_phase -= self.sample_rate;
                let (sequencer, chip) = (&mut self.sequencer, &mut self.chip);
                sequencer.advance_tick(&mut |write| {
                    chip.write_reg(write.register, write.value);
                });
            }
        }
    }

    /// Returns the number of stereo frames consumed by this player in tests.
    #[cfg(test)]
    pub(crate) fn rendered_frames(&self) -> u64 {
        // This monotonic clock advances beside exactly one chip render operation.
        self.rendered_frames
    }
}

/// Describes why the original source file could not become live playback state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OplError {
    /// The supplied file is not the exact 5,354-byte original driver image.
    InvalidDriverLength {
        /// Number of bytes supplied by the bundled or unbundled asset source.
        actual: usize,
    },
    /// A same-sized file differs from the supported original driver revision.
    WrongDriverChecksum {
        /// CRC-32 calculated from the supplied bytes for diagnostic reporting.
        actual: u32,
    },
    /// The requested SDL output rate cannot represent every original driver tick.
    InvalidSampleRate {
        /// Unsupported output frames per second supplied by the audio device.
        sample_rate: u32,
    },
}

impl fmt::Display for OplError {
    /// Formats a concise startup error identifying the invalid input contract.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Include actual values so unbundled installations can identify a wrong
        // source file without exposing any opaque internal parser state.
        match self {
            Self::InvalidDriverLength { actual } => write!(
                formatter,
                "ADLIB.SND is {actual} bytes; expected {DRIVER_LENGTH}"
            ),
            Self::WrongDriverChecksum { actual } => write!(
                formatter,
                "ADLIB.SND CRC-32 is {actual:08x}; expected {DRIVER_CRC32:08x}"
            ),
            Self::InvalidSampleRate { sample_rate } => write!(
                formatter,
                "OPL output rate {sample_rate} cannot represent {ORIGINAL_TICK_RATE} Hz playback"
            ),
        }
    }
}

impl Error for OplError {}

/// Sends one register/value pair to a caller-selected chip or test collector.
fn emit(output: &mut impl FnMut(RegisterWrite), register: u8, value: u8) {
    // Keeping construction here makes every sequencer path use the same typed
    // representation without allocating or dynamically dispatching callbacks.
    output(RegisterWrite { register, value });
}

/// Calculates the standard reflected IEEE CRC-32 used to identify `ADLIB.SND`.
fn crc32(bytes: &[u8]) -> u32 {
    // This small startup-only implementation avoids a runtime dependency merely
    // to validate one 5 KB file. The complemented accumulator and polynomial
    // match the common CRC-32 value reported for the historical source asset.
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    //! Direct-source sequencing and synthesis tests independent of SDL devices.

    use super::{AdlibSequencer, DRIVER_CRC32, DRIVER_LENGTH, OplError, OplPlayer, crc32};

    /// Exact original driver restored from repository history.
    const DRIVER: &[u8] = include_bytes!("../assets/audio/ADLIB.SND");

    /// Confirms the runtime asset is the exact compact original file.
    #[test]
    fn production_driver_has_original_identity() {
        assert_eq!(DRIVER.len(), DRIVER_LENGTH);
        assert_eq!(crc32(DRIVER), DRIVER_CRC32);
        AdlibSequencer::new(DRIVER).expect("the original driver should initialize");
    }

    /// Locks the complete natural traversal to the original driver's behavior.
    #[test]
    fn direct_sequencer_matches_the_original_complete_traversal() {
        /// FNV-1a basis used to compactly retain tick boundaries and writes.
        const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        /// FNV-1a multiplier applied after each byte of sequencer output.
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        /// Number of original 50 Hz calls in one natural score traversal.
        const TRAVERSAL_TICKS: usize = 15_621;
        /// Initialization plus live writes produced during that traversal.
        const EXPECTED_WRITES: usize = 30_559;
        /// Digest of every per-tick write count, register, and value in order.
        const EXPECTED_DIGEST: u64 = 0x2487_fb4b_8fe4_eaa2;

        let mut sequencer = AdlibSequencer::new(DRIVER).expect("driver should initialize");
        let mut digest = FNV_OFFSET_BASIS;
        let mut write_count = 0;
        let mut writes = Vec::new();

        // Include record lengths in the digest so moving an otherwise identical
        // register write to a different 50 Hz tick cannot pass this regression.
        sequencer.initialize_chip(&mut |write| writes.push(write));
        digest = fnv_byte(digest, writes.len() as u8, FNV_PRIME);
        for write in writes.drain(..) {
            digest = fnv_byte(digest, write.register, FNV_PRIME);
            digest = fnv_byte(digest, write.value, FNV_PRIME);
            write_count += 1;
        }
        for _ in 0..TRAVERSAL_TICKS {
            sequencer.advance_tick(&mut |write| writes.push(write));
            digest = fnv_byte(digest, writes.len() as u8, FNV_PRIME);
            for write in writes.drain(..) {
                digest = fnv_byte(digest, write.register, FNV_PRIME);
                digest = fnv_byte(digest, write.value, FNV_PRIME);
                write_count += 1;
            }
        }

        assert_eq!(write_count, EXPECTED_WRITES);
        assert_eq!(digest, EXPECTED_DIGEST);
    }

    /// Confirms direct playback produces audible centered OPL2 samples.
    #[test]
    fn production_driver_renders_audible_mono_chip_output() {
        let mut player = OplPlayer::new(DRIVER, 44_100).expect("OPL player should construct");
        let mut output = vec![0.0; 44_100 * 2];

        player.mix_stereo(&mut output, 1.0);

        assert_eq!(player.rendered_frames(), 44_100);
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
        let (frames, remainder) = output.as_chunks::<2>();
        assert!(remainder.is_empty());
        assert!(frames.iter().all(|frame| frame[0] == frame[1]));
    }

    /// Confirms source corruption is rejected before any chip is constructed.
    #[test]
    fn rejects_a_modified_original_driver() {
        let mut corrupted = DRIVER.to_vec();
        corrupted[0x1210] ^= 0x01;

        assert!(matches!(
            AdlibSequencer::new(&corrupted),
            Err(OplError::WrongDriverChecksum { .. })
        ));
    }

    /// Confirms an incomplete file cannot expose absolute driver offsets.
    #[test]
    fn rejects_a_truncated_original_driver() {
        assert!(matches!(
            AdlibSequencer::new(&DRIVER[..DRIVER.len() - 1]),
            Err(OplError::InvalidDriverLength { actual }) if actual == DRIVER_LENGTH - 1
        ));
    }

    /// Confirms output rates below the source interrupt cadence are rejected.
    #[test]
    fn rejects_an_output_rate_below_the_original_tick_rate() {
        assert!(matches!(
            OplPlayer::new(DRIVER, 49),
            Err(OplError::InvalidSampleRate { sample_rate: 49 })
        ));
    }

    /// Adds one byte to a wrapping FNV-1a regression digest.
    fn fnv_byte(digest: u64, byte: u8, prime: u64) -> u64 {
        // Wrapping multiplication is the defined FNV operation, not overflow
        // recovery, and therefore remains identical in debug and release tests.
        (digest ^ u64::from(byte)).wrapping_mul(prime)
    }
}
