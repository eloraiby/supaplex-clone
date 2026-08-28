//! Bounded FastTracker II playback for the original Supaplex soundtrack.
//!
//! The embedded module uses a deliberately small part of XM 1.04: linear
//! periods, ordinary notes and key-offs, volume-column assignments, volume
//! slides, and pattern breaks. Parsing rejects every unsupported command so a
//! future asset change cannot silently turn into subtly incorrect playback.
//! Samples, envelopes, forward loops, and ping-pong loops are mixed directly
//! into the SDL callback without allocating on the real-time audio thread.

/// FastTracker's fixed number of note-to-sample map entries per instrument.
const NOTES_PER_INSTRUMENT: usize = 96;

/// Maximum unsigned channel volume stored by XM samples and effects.
const MAX_VOLUME: u8 = 64;

/// Neutral value and maximum ordinate used by XM panning envelopes.
const CENTER_PANNING_ENVELOPE: u16 = 32;

/// Maximum internal fade value used in FastTracker's final-volume formula.
const MAX_FADE_VOLUME: u32 = 65_536;

/// A parsed XM module containing only data required by the callback player.
struct XmModule {
    /// Pattern numbers visited by successive song positions.
    orders: Box<[u8]>,
    /// Song position selected after the final order finishes.
    restart_order: usize,
    /// Number of simultaneous pattern columns and playback voices.
    channel_count: usize,
    /// Row-major decoded event data for every pattern named by the order list.
    patterns: Box<[Pattern]>,
    /// Sample maps, envelopes, and decoded PCM belonging to each instrument.
    instruments: Box<[Instrument]>,
    /// Number of tracker ticks for which each row remains active.
    initial_tempo: u16,
    /// Initial tracker beats per minute used to derive tick duration.
    initial_bpm: u16,
}

impl XmModule {
    /// Parses one complete XM 1.04 byte stream and validates the supported subset.
    fn parse(encoded: &[u8]) -> Result<Self, String> {
        // Parse through a checked cursor: every numeric field and payload slice
        // reports its byte offset on truncation instead of indexing and panicking.
        let mut reader = Reader::new(encoded);
        if reader.take(17)? != b"Extended Module: " {
            return Err("XM offset 0: missing Extended Module signature".to_owned());
        }
        let _module_name = reader.take(20)?;
        if reader.byte()? != 0x1a {
            return Err("XM offset 37: missing 0x1a name terminator".to_owned());
        }
        let _tracker_name = reader.take(20)?;
        let version = reader.word()?;
        if version != 0x0104 {
            return Err(format!(
                "XM offset 58: version {version:#06x} is unsupported; expected 0x0104"
            ));
        }

        let header_size = usize::try_from(reader.dword()?)
            .map_err(|_| "XM header size does not fit this platform".to_owned())?;
        if header_size < 276 {
            return Err(format!(
                "XM offset 60: header is {header_size} bytes; at least 276 are required"
            ));
        }
        let header_end = 60usize
            .checked_add(header_size)
            .ok_or_else(|| "XM header end overflows this platform".to_owned())?;
        let song_length = usize::from(reader.word()?);
        let restart_order = usize::from(reader.word()?);
        let channel_count = usize::from(reader.word()?);
        let pattern_count = usize::from(reader.word()?);
        let instrument_count = usize::from(reader.word()?);
        let flags = reader.word()?;
        let initial_tempo = reader.word()?;
        let initial_bpm = reader.word()?;
        let full_order_table = reader.take(256)?;

        // The player deliberately implements the module's linear-period mode.
        // Rejecting Amiga periods and unknown flag bits makes pitch behavior
        // explicit and keeps malformed headers away from the audio callback.
        if !(1..=256).contains(&song_length) {
            return Err(format!("XM song length {song_length} is outside 1..=256"));
        }
        if restart_order >= song_length {
            return Err(format!(
                "XM restart order {restart_order} is outside song length {song_length}"
            ));
        }
        if !(1..=32).contains(&channel_count) {
            return Err(format!(
                "XM channel count {channel_count} is outside 1..=32"
            ));
        }
        if pattern_count == 0 {
            return Err("XM contains no patterns".to_owned());
        }
        if flags != 1 {
            return Err(format!(
                "XM flags {flags:#06x} request unsupported non-linear periods or extensions"
            ));
        }
        if initial_tempo == 0 || initial_bpm == 0 {
            return Err("XM tempo and BPM must both be non-zero".to_owned());
        }
        reader.seek(header_end)?;

        let orders = full_order_table[..song_length].to_vec().into_boxed_slice();
        for (position, pattern) in orders.iter().copied().enumerate() {
            if usize::from(pattern) >= pattern_count {
                return Err(format!(
                    "XM order {position} names missing pattern {pattern}"
                ));
            }
        }

        // Patterns precede instruments in XM files. Decode their packed cells
        // now, using the header's instrument count to validate every event.
        let mut patterns = Vec::with_capacity(pattern_count);
        for pattern_index in 0..pattern_count {
            patterns.push(Pattern::parse(
                &mut reader,
                pattern_index,
                channel_count,
                instrument_count,
            )?);
        }

        // Instrument parsing also consumes the delta-encoded sample payloads,
        // leaving no file-backed data or decoding work for real-time playback.
        let mut instruments = Vec::with_capacity(instrument_count);
        for instrument_index in 0..instrument_count {
            instruments.push(Instrument::parse(&mut reader, instrument_index)?);
        }
        if reader.position() != encoded.len() {
            return Err(format!(
                "XM offset {}: {} trailing bytes remain after the final sample",
                reader.position(),
                encoded.len() - reader.position()
            ));
        }

        Ok(Self {
            orders,
            restart_order,
            channel_count,
            patterns: patterns.into_boxed_slice(),
            instruments: instruments.into_boxed_slice(),
            initial_tempo,
            initial_bpm,
        })
    }
}

/// One decoded pattern with its events stored in row-major channel order.
struct Pattern {
    /// Number of rows traversed before playback advances to another order.
    rows: usize,
    /// Decoded cells addressed as `row * channel_count + channel`.
    cells: Box<[PatternCell]>,
}

impl Pattern {
    /// Expands one packed XM pattern into fixed-width [`PatternCell`] records.
    fn parse(
        reader: &mut Reader<'_>,
        pattern_index: usize,
        channel_count: usize,
        instrument_count: usize,
    ) -> Result<Self, String> {
        // Pattern headers are extensible, so consume the defined prefix and
        // seek to the declared end before interpreting the packed payload.
        let header_start = reader.position();
        let header_size = usize::try_from(reader.dword()?)
            .map_err(|_| format!("XM pattern {pattern_index} header size is too large"))?;
        if header_size < 9 {
            return Err(format!(
                "XM pattern {pattern_index} header is {header_size} bytes; at least 9 are required"
            ));
        }
        let packing_type = reader.byte()?;
        if packing_type != 0 {
            return Err(format!(
                "XM pattern {pattern_index} uses unsupported packing type {packing_type}"
            ));
        }
        let rows = usize::from(reader.word()?);
        if !(1..=256).contains(&rows) {
            return Err(format!(
                "XM pattern {pattern_index} row count {rows} is outside 1..=256"
            ));
        }
        let packed_size = usize::from(reader.word()?);
        let header_end = header_start
            .checked_add(header_size)
            .ok_or_else(|| format!("XM pattern {pattern_index} header end overflows"))?;
        reader.seek(header_end)?;
        let packed = reader.take(packed_size)?;
        let mut packed_reader = Reader::new(packed);
        let cell_count = rows
            .checked_mul(channel_count)
            .ok_or_else(|| format!("XM pattern {pattern_index} cell count overflows"))?;
        let mut cells = Vec::with_capacity(cell_count);

        // A zero-byte payload represents an entirely empty pattern. Otherwise
        // every cell has either five raw bytes or a mask followed by present
        // fields; absent fields retain their semantic zero values.
        if packed_size == 0 {
            cells.resize(cell_count, PatternCell::default());
        } else {
            for cell_index in 0..cell_count {
                let first = packed_reader.byte().map_err(|error| {
                    format!("XM pattern {pattern_index}, cell {cell_index}: {error}")
                })?;
                let cell = if first & 0x80 == 0 {
                    PatternCell {
                        note: first,
                        instrument: packed_reader.byte()?,
                        volume: packed_reader.byte()?,
                        effect: packed_reader.byte()?,
                        parameter: packed_reader.byte()?,
                    }
                } else {
                    PatternCell {
                        note: if first & 0x01 != 0 {
                            packed_reader.byte()?
                        } else {
                            0
                        },
                        instrument: if first & 0x02 != 0 {
                            packed_reader.byte()?
                        } else {
                            0
                        },
                        volume: if first & 0x04 != 0 {
                            packed_reader.byte()?
                        } else {
                            0
                        },
                        effect: if first & 0x08 != 0 {
                            packed_reader.byte()?
                        } else {
                            0
                        },
                        parameter: if first & 0x10 != 0 {
                            packed_reader.byte()?
                        } else {
                            0
                        },
                    }
                };
                cell.validate(pattern_index, cell_index, instrument_count)?;
                cells.push(cell);
            }
            if packed_reader.position() != packed.len() {
                return Err(format!(
                    "XM pattern {pattern_index} leaves {} unused packed bytes",
                    packed.len() - packed_reader.position()
                ));
            }
        }

        Ok(Self {
            rows,
            cells: cells.into_boxed_slice(),
        })
    }

    /// Returns the event at one already-validated row and channel coordinate.
    fn cell(&self, row: usize, channel: usize, channel_count: usize) -> PatternCell {
        // Parser and sequencer invariants guarantee this index. Keeping the
        // conversion here documents the single row-major pattern layout.
        self.cells[row * channel_count + channel]
    }
}

/// The five semantic fields represented by one XM pattern cell.
#[derive(Clone, Copy, Default)]
struct PatternCell {
    /// Zero for no note, 1..=96 for a pitch, or 97 for key-off.
    note: u8,
    /// One-based instrument number, with zero retaining the selected instrument.
    instrument: u8,
    /// Zero or the supported direct-volume column range `0x10..=0x50`.
    volume: u8,
    /// Supported standard effect number, currently 0, A, or D.
    effect: u8,
    /// Effect-specific operand stored beside [`Self::effect`].
    parameter: u8,
}

impl PatternCell {
    /// Rejects event vocabulary that the bounded player cannot reproduce.
    fn validate(
        self,
        pattern_index: usize,
        cell_index: usize,
        instrument_count: usize,
    ) -> Result<(), String> {
        // Validation is performed once at load time, keeping the callback free
        // of fallible branches while guaranteeing unsupported effects are loud.
        if self.note > 97 {
            return Err(format!(
                "XM pattern {pattern_index}, cell {cell_index} has invalid note {}",
                self.note
            ));
        }
        if usize::from(self.instrument) > instrument_count {
            return Err(format!(
                "XM pattern {pattern_index}, cell {cell_index} names missing instrument {}",
                self.instrument
            ));
        }
        if self.volume != 0 && !(0x10..=0x50).contains(&self.volume) {
            return Err(format!(
                "XM pattern {pattern_index}, cell {cell_index} uses unsupported volume command {:#04x}",
                self.volume
            ));
        }
        match (self.effect, self.parameter) {
            (0, 0) | (0x0a, _) => {}
            (0, parameter) => {
                return Err(format!(
                    "XM pattern {pattern_index}, cell {cell_index} uses unsupported arpeggio 0{parameter:02x}"
                ));
            }
            (0x0d, parameter) if parameter >> 4 <= 9 && parameter & 0x0f <= 9 => {}
            (0x0d, parameter) => {
                return Err(format!(
                    "XM pattern {pattern_index}, cell {cell_index} has non-BCD pattern break {parameter:02x}"
                ));
            }
            (effect, parameter) => {
                return Err(format!(
                    "XM pattern {pattern_index}, cell {cell_index} uses unsupported effect {effect:x}{parameter:02x}"
                ));
            }
        }
        if self.effect == 0x0a && self.parameter >> 4 != 0 && self.parameter & 0x0f != 0 {
            return Err(format!(
                "XM pattern {pattern_index}, cell {cell_index} has ambiguous volume slide {:02x}",
                self.parameter
            ));
        }
        Ok(())
    }
}

/// One instrument's keyboard map, envelopes, fadeout, and sample collection.
struct Instrument {
    /// Zero-based sample selected for each of XM's 96 playable notes.
    sample_map: [u8; NOTES_PER_INSTRUMENT],
    /// Per-tick amplitude envelope, including sustain and loop behavior.
    volume_envelope: Envelope,
    /// Per-tick stereo-position envelope centered around the sample panning.
    panning_envelope: Envelope,
    /// Amount subtracted from the channel fade accumulator after key-off.
    fadeout: u16,
    /// Decoded signed PCM samples addressed by [`Self::sample_map`].
    samples: Box<[Sample]>,
}

impl Instrument {
    /// Parses one instrument header, all sample headers, and their PCM payloads.
    fn parse(reader: &mut Reader<'_>, instrument_index: usize) -> Result<Self, String> {
        // XM stores all sample headers before all sample payloads. Retain small
        // raw descriptors temporarily, then decode each delta stream in order.
        let header_start = reader.position();
        let header_size = usize::try_from(reader.dword()?)
            .map_err(|_| format!("XM instrument {instrument_index} header is too large"))?;
        if header_size < 29 {
            return Err(format!(
                "XM instrument {instrument_index} header is {header_size} bytes; at least 29 are required"
            ));
        }
        let _name = reader.take(22)?;
        let _instrument_type = reader.byte()?;
        let sample_count = usize::from(reader.word()?);
        if sample_count > 16 {
            return Err(format!(
                "XM instrument {instrument_index} has {sample_count} samples; at most 16 are supported"
            ));
        }

        let mut sample_map = [0; NOTES_PER_INSTRUMENT];
        let mut volume_envelope = Envelope::disabled();
        let mut panning_envelope = Envelope::disabled();
        let mut fadeout = 0;
        let mut sample_header_size = 0;
        if sample_count != 0 {
            if header_size < 243 {
                return Err(format!(
                    "XM instrument {instrument_index} sample header is shorter than 243 bytes"
                ));
            }
            sample_header_size = usize::try_from(reader.dword()?).map_err(|_| {
                format!("XM instrument {instrument_index} sample-header size is too large")
            })?;
            if sample_header_size < 40 {
                return Err(format!(
                    "XM instrument {instrument_index} sample header is {sample_header_size} bytes; at least 40 are required"
                ));
            }
            sample_map.copy_from_slice(reader.take(NOTES_PER_INSTRUMENT)?);
            let volume_points = read_envelope_points(reader)?;
            let panning_points = read_envelope_points(reader)?;
            let volume_point_count = reader.byte()?;
            let panning_point_count = reader.byte()?;
            let volume_sustain = reader.byte()?;
            let volume_loop_start = reader.byte()?;
            let volume_loop_end = reader.byte()?;
            let panning_sustain = reader.byte()?;
            let panning_loop_start = reader.byte()?;
            let panning_loop_end = reader.byte()?;
            let volume_flags = reader.byte()?;
            let panning_flags = reader.byte()?;
            let vibrato_type = reader.byte()?;
            let vibrato_sweep = reader.byte()?;
            let vibrato_depth = reader.byte()?;
            let vibrato_rate = reader.byte()?;
            fadeout = reader.word()?;
            let _reserved = reader.word()?;

            // The Supaplex module does not use instrument auto-vibrato. It is
            // rejected rather than ignored because it changes sustained pitch.
            if [vibrato_type, vibrato_sweep, vibrato_depth, vibrato_rate]
                .into_iter()
                .any(|value| value != 0)
            {
                return Err(format!(
                    "XM instrument {instrument_index} uses unsupported auto-vibrato"
                ));
            }
            volume_envelope = Envelope::new(
                volume_points,
                volume_point_count,
                volume_sustain,
                volume_loop_start,
                volume_loop_end,
                volume_flags,
                MAX_VOLUME.into(),
                instrument_index,
                "volume",
            )?;
            panning_envelope = Envelope::new(
                panning_points,
                panning_point_count,
                panning_sustain,
                panning_loop_start,
                panning_loop_end,
                panning_flags,
                MAX_VOLUME.into(),
                instrument_index,
                "panning",
            )?;
            for (note, mapped_sample) in sample_map.iter().copied().enumerate() {
                if usize::from(mapped_sample) >= sample_count {
                    return Err(format!(
                        "XM instrument {instrument_index} maps note {} to missing sample {mapped_sample}",
                        note + 1
                    ));
                }
            }
        }

        let header_end = header_start
            .checked_add(header_size)
            .ok_or_else(|| format!("XM instrument {instrument_index} header end overflows"))?;
        reader.seek(header_end)?;
        let mut raw_samples = Vec::with_capacity(sample_count);
        for sample_index in 0..sample_count {
            raw_samples.push(RawSample::parse(
                reader,
                instrument_index,
                sample_index,
                sample_header_size,
            )?);
        }
        let mut samples = Vec::with_capacity(sample_count);
        for (sample_index, raw_sample) in raw_samples.into_iter().enumerate() {
            samples.push(raw_sample.decode(reader, instrument_index, sample_index)?);
        }

        Ok(Self {
            sample_map,
            volume_envelope,
            panning_envelope,
            fadeout,
            samples: samples.into_boxed_slice(),
        })
    }
}

/// A single coordinate on an XM volume or panning envelope.
#[derive(Clone, Copy, Default)]
struct EnvelopePoint {
    /// Tick position measured from the channel's most recent note trigger.
    tick: u16,
    /// Envelope ordinate in the inclusive tracker range 0..=64.
    value: u16,
}

/// Validated interpolation and control metadata for an instrument envelope.
struct Envelope {
    /// Active prefix of the twelve coordinates reserved by the file format.
    points: Box<[EnvelopePoint]>,
    /// Whether the envelope contributes to final volume or panning.
    enabled: bool,
    /// Point index held while the note's key remains on.
    sustain: Option<usize>,
    /// Inclusive point-index pair repeated after its end coordinate.
    loop_range: Option<(usize, usize)>,
}

impl Envelope {
    /// Creates the neutral representation used by sample-only instruments.
    fn disabled() -> Self {
        // Disabled envelopes contain no coordinates; callers use their stated
        // neutral value and never ask them to interpolate an empty collection.
        Self {
            points: Box::new([]),
            enabled: false,
            sustain: None,
            loop_range: None,
        }
    }

    /// Validates flags and selects the active prefix of twelve raw points.
    #[allow(clippy::too_many_arguments)]
    fn new(
        all_points: [EnvelopePoint; 12],
        point_count: u8,
        sustain: u8,
        loop_start: u8,
        loop_end: u8,
        flags: u8,
        maximum_value: u16,
        instrument_index: usize,
        kind: &str,
    ) -> Result<Self, String> {
        // Bits 0, 1, and 2 mean enabled, sustain, and loop respectively; no
        // other flag has defined XM 1.04 envelope semantics.
        if flags & !0x07 != 0 {
            return Err(format!(
                "XM instrument {instrument_index} {kind} envelope has unknown flags {flags:#04x}"
            ));
        }
        let point_count = usize::from(point_count);
        if point_count > all_points.len() {
            return Err(format!(
                "XM instrument {instrument_index} {kind} envelope has {point_count} points"
            ));
        }
        let points = all_points[..point_count].to_vec().into_boxed_slice();
        if flags & 1 != 0 && points.is_empty() {
            return Err(format!(
                "XM instrument {instrument_index} enables an empty {kind} envelope"
            ));
        }
        for (point_index, point) in points.iter().enumerate() {
            if point.value > maximum_value {
                return Err(format!(
                    "XM instrument {instrument_index} {kind} point {point_index} exceeds {maximum_value}"
                ));
            }
            if point_index != 0 && point.tick < points[point_index - 1].tick {
                return Err(format!(
                    "XM instrument {instrument_index} {kind} envelope ticks are not monotonic"
                ));
            }
        }

        let sustain = if flags & 2 != 0 {
            let index = usize::from(sustain);
            if index >= points.len() {
                return Err(format!(
                    "XM instrument {instrument_index} {kind} sustain point {index} is missing"
                ));
            }
            Some(index)
        } else {
            None
        };
        let loop_range = if flags & 4 != 0 {
            let start = usize::from(loop_start);
            let end = usize::from(loop_end);
            if start >= points.len() || end >= points.len() || start > end {
                return Err(format!(
                    "XM instrument {instrument_index} {kind} loop {start}..={end} is invalid"
                ));
            }
            Some((start, end))
        } else {
            None
        };

        Ok(Self {
            points,
            enabled: flags & 1 != 0,
            sustain,
            loop_range,
        })
    }

    /// Linearly interpolates the envelope ordinate at one channel tick.
    fn value_at(&self, tick: u16, neutral: u16) -> u16 {
        // Disabled envelopes do not alter their channel. Enabled envelopes hold
        // their last ordinate after the final point, matching tracker behavior.
        if !self.enabled {
            return neutral;
        }
        let first = self.points[0];
        if tick <= first.tick {
            return first.value;
        }
        for pair in self.points.windows(2) {
            let left = pair[0];
            let right = pair[1];
            if tick <= right.tick {
                let width = u32::from(right.tick - left.tick);
                if width == 0 {
                    return right.value;
                }
                let offset = u32::from(tick - left.tick);
                let left_value = u32::from(left.value);
                let delta = i32::from(right.value) - i32::from(left.value);
                let interpolated =
                    i64::from(left_value) + i64::from(delta) * i64::from(offset) / i64::from(width);
                return interpolated.clamp(0, i64::from(u16::MAX)) as u16;
            }
        }
        self.points.last().map_or(neutral, |point| point.value)
    }

    /// Advances an envelope tick while honoring key-on sustain and loop points.
    fn advance(&self, tick: &mut u16, key_on: bool) {
        // A sustain coordinate freezes only while the key is held. Envelope
        // loops remain active after release and wrap on the tick after loop end.
        if !self.enabled {
            return;
        }
        if key_on
            && self
                .sustain
                .is_some_and(|index| *tick >= self.points[index].tick)
        {
            return;
        }
        let mut next = tick.saturating_add(1);
        if let Some((start, end)) = self.loop_range
            && next > self.points[end].tick
        {
            next = self.points[start].tick;
        }
        *tick = next;
    }
}

/// Reads the twelve fixed coordinate slots in an XM instrument header.
fn read_envelope_points(reader: &mut Reader<'_>) -> Result<[EnvelopePoint; 12], String> {
    // `array::try_from_fn` is not stable on every supported toolchain, so fill
    // the small Copy array explicitly while retaining checked cursor errors.
    let mut points = [EnvelopePoint::default(); 12];
    for point in &mut points {
        point.tick = reader.word()?;
        point.value = reader.word()?;
    }
    Ok(points)
}

/// Header information retained until its following delta stream is decoded.
struct RawSample {
    /// Encoded payload length in bytes, before 8/16-bit normalization.
    byte_length: usize,
    /// Encoded loop start in bytes from the start of this sample.
    loop_start_bytes: usize,
    /// Encoded loop length in bytes.
    loop_length_bytes: usize,
    /// Default channel volume copied on a note trigger.
    volume: u8,
    /// Signed sixteenth-semitone correction used by linear-period pitch.
    finetune: i8,
    /// Sample loop mode plus the 16-bit payload flag.
    flags: u8,
    /// Default channel panning from fully left 0 to fully right 255.
    panning: u8,
    /// Signed semitone offset added to the pattern note.
    relative_note: i8,
}

impl RawSample {
    /// Parses the defined 40-byte prefix of one extensible sample header.
    fn parse(
        reader: &mut Reader<'_>,
        instrument_index: usize,
        sample_index: usize,
        header_size: usize,
    ) -> Result<Self, String> {
        // The enclosing instrument declares one common header size. Seek past
        // extensions only after all standard pitch, loop, and panning fields.
        let header_start = reader.position();
        let byte_length = usize::try_from(reader.dword()?).map_err(|_| {
            format!("XM instrument {instrument_index} sample {sample_index} is too large")
        })?;
        let loop_start_bytes = usize::try_from(reader.dword()?).map_err(|_| {
            format!(
                "XM instrument {instrument_index} sample {sample_index} loop start is too large"
            )
        })?;
        let loop_length_bytes = usize::try_from(reader.dword()?).map_err(|_| {
            format!("XM instrument {instrument_index} sample {sample_index} loop is too large")
        })?;
        let volume = reader.byte()?;
        let finetune = reader.signed_byte()?;
        let flags = reader.byte()?;
        let panning = reader.byte()?;
        let relative_note = reader.signed_byte()?;
        let _reserved = reader.byte()?;
        let _name = reader.take(22)?;
        let header_end = header_start.checked_add(header_size).ok_or_else(|| {
            format!("XM instrument {instrument_index} sample {sample_index} header overflows")
        })?;
        reader.seek(header_end)?;

        if volume > MAX_VOLUME {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} volume {volume} exceeds 64"
            ));
        }
        if flags & !0x13 != 0 || flags & 0x03 == 0x03 {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} has unsupported flags {flags:#04x}"
            ));
        }
        let bytes_per_frame = if flags & 0x10 != 0 { 2 } else { 1 };
        if byte_length % bytes_per_frame != 0
            || loop_start_bytes % bytes_per_frame != 0
            || loop_length_bytes % bytes_per_frame != 0
        {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} has a partial 16-bit frame"
            ));
        }
        let loop_end = loop_start_bytes
            .checked_add(loop_length_bytes)
            .ok_or_else(|| {
                format!("XM instrument {instrument_index} sample {sample_index} loop overflows")
            })?;
        if loop_end > byte_length {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} loop exceeds its payload"
            ));
        }
        if flags & 0x03 != 0 && loop_length_bytes == 0 {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} enables an empty loop"
            ));
        }

        Ok(Self {
            byte_length,
            loop_start_bytes,
            loop_length_bytes,
            volume,
            finetune,
            flags,
            panning,
            relative_note,
        })
    }

    /// Delta-decodes one sample payload and normalizes loop offsets to frames.
    fn decode(
        self,
        reader: &mut Reader<'_>,
        instrument_index: usize,
        sample_index: usize,
    ) -> Result<Sample, String> {
        // XM stores signed differences rather than absolute PCM. Wrapping
        // addition is intentional for both widths and reproduces the tracker
        // accumulator at the integer boundary.
        let encoded = reader.take(self.byte_length)?;
        let sixteen_bit = self.flags & 0x10 != 0;
        let mut frames = Vec::with_capacity(self.byte_length / if sixteen_bit { 2 } else { 1 });
        if sixteen_bit {
            let mut accumulator = 0i16;
            let (encoded_frames, remainder) = encoded.as_chunks::<2>();
            debug_assert!(remainder.is_empty(), "16-bit sample lengths were validated");
            for bytes in encoded_frames {
                accumulator = accumulator.wrapping_add(i16::from_le_bytes(*bytes));
                frames.push(accumulator);
            }
        } else {
            let mut accumulator = 0i8;
            for difference in encoded.iter().copied() {
                accumulator = accumulator.wrapping_add(difference as i8);
                frames.push(i16::from(accumulator) << 8);
            }
        }
        if frames.is_empty() {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} contains no PCM frames"
            ));
        }

        let divisor = if sixteen_bit { 2 } else { 1 };
        let loop_start = self.loop_start_bytes / divisor;
        let loop_length = self.loop_length_bytes / divisor;
        let loop_kind = match self.flags & 0x03 {
            0 => LoopKind::None,
            1 => LoopKind::Forward,
            2 => LoopKind::PingPong,
            _ => unreachable!("sample flags were validated before payload decoding"),
        };
        if loop_kind == LoopKind::PingPong && loop_length < 2 {
            return Err(format!(
                "XM instrument {instrument_index} sample {sample_index} ping-pong loop is shorter than two frames"
            ));
        }
        Ok(Sample {
            frames: frames.into_boxed_slice(),
            loop_kind,
            loop_start,
            loop_end: loop_start + loop_length,
            volume: self.volume,
            finetune: self.finetune,
            panning: self.panning,
            relative_note: self.relative_note,
        })
    }
}

/// Decoded signed mono PCM and playback defaults for one instrument sample.
struct Sample {
    /// Absolute signed PCM frames produced from the file's delta stream.
    frames: Box<[i16]>,
    /// Whether and how playback repeats the declared sample interval.
    loop_kind: LoopKind,
    /// First frame in the declared repeat interval.
    loop_start: usize,
    /// Exclusive frame after the declared repeat interval.
    loop_end: usize,
    /// Initial tracker channel volume in the range 0..=64.
    volume: u8,
    /// Signed sixteenth-semitone pitch correction.
    finetune: i8,
    /// Initial stereo position from left 0 through right 255.
    panning: u8,
    /// Signed semitone transposition applied to pattern notes.
    relative_note: i8,
}

/// Sample traversal behavior encoded in the low two sample-type bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopKind {
    /// Playback stops on reaching the final decoded frame.
    None,
    /// Playback wraps from the exclusive loop end to its loop start.
    Forward,
    /// Playback reverses direction at both boundaries of the loop.
    PingPong,
}

/// Mutable playback state belonging to one XM pattern channel.
struct ChannelState {
    /// Most recently selected zero-based instrument, retained across rows.
    selected_instrument: Option<usize>,
    /// Instrument owning the currently sounding sample and envelopes.
    sounding_instrument: Option<usize>,
    /// Sample within [`Self::sounding_instrument`] currently being traversed.
    sounding_sample: Option<usize>,
    /// Fractional source-frame position used for linear interpolation.
    sample_position: f64,
    /// Positive source frames advanced for each output frame.
    sample_step: f64,
    /// Positive one or negative one for forward or reverse traversal.
    sample_direction: f64,
    /// Tracker channel volume after sample defaults and row effects.
    volume: u8,
    /// Base stereo position copied from the triggered sample.
    panning: u8,
    /// Whether envelope sustain points continue to hold.
    key_on: bool,
    /// Current tick coordinate within the volume envelope.
    volume_envelope_tick: u16,
    /// Current tick coordinate within the panning envelope.
    panning_envelope_tick: u16,
    /// Full-scale accumulator reduced by instrument fadeout after release.
    fade_volume: u32,
    /// Last non-zero Axy operand, implementing XM effect memory.
    volume_slide_memory: u8,
    /// Axy operand applied on non-zero ticks of the current row.
    active_volume_slide: Option<u8>,
}

impl ChannelState {
    /// Creates a silent channel with neutral mixer and effect-memory state.
    fn silent() -> Self {
        // Instrument selection and sample activity are independent options so
        // an instrument-only row can prepare a later note without sounding.
        Self {
            selected_instrument: None,
            sounding_instrument: None,
            sounding_sample: None,
            sample_position: 0.0,
            sample_step: 0.0,
            sample_direction: 1.0,
            volume: 0,
            panning: 128,
            key_on: false,
            volume_envelope_tick: 0,
            panning_envelope_tick: 0,
            fade_volume: MAX_FADE_VOLUME,
            volume_slide_memory: 0,
            active_volume_slide: None,
        }
    }

    /// Starts the mapped sample for a one-based XM pattern note.
    fn trigger(&mut self, module: &XmModule, note: u8, output_frequency: u32) {
        // A note without a selected instrument is inert. All indices below were
        // validated by the parser, so callback playback uses direct lookups.
        let Some(instrument_index) = self.selected_instrument else {
            return;
        };
        let instrument = &module.instruments[instrument_index];
        let note_index = usize::from(note - 1);
        let sample_index = usize::from(instrument.sample_map[note_index]);
        let Some(sample) = instrument.samples.get(sample_index) else {
            // Empty instruments are legal XM placeholders. Selecting one and
            // entering a note changes no existing voice and must not panic.
            return;
        };

        // FastTracker converts the file's one-based note byte to a zero-based
        // semitone before applying relative note and fine tuning. Keeping that
        // normalization explicit prevents a one-semitone upward transposition.
        let real_note = f64::from(note - 1) + f64::from(sample.relative_note);
        let period = 7680.0 - real_note * 64.0 - f64::from(sample.finetune) / 2.0;
        let frequency = 8363.0 * 2.0f64.powf((4608.0 - period) / 768.0);
        self.sounding_instrument = Some(instrument_index);
        self.sounding_sample = Some(sample_index);
        self.sample_position = 0.0;
        self.sample_step = frequency / f64::from(output_frequency.max(1));
        self.sample_direction = 1.0;
        self.volume = sample.volume;
        self.panning = sample.panning;
        self.key_on = true;
        self.volume_envelope_tick = 0;
        self.panning_envelope_tick = 0;
        self.fade_volume = MAX_FADE_VOLUME;
    }

    /// Releases the current key and handles instruments without a volume envelope.
    fn release(&mut self, module: &XmModule) {
        // FastTracker volume envelopes continue after key-off; without an
        // enabled volume envelope, key-off silences the channel immediately.
        self.key_on = false;
        let has_volume_envelope = self
            .sounding_instrument
            .is_some_and(|index| module.instruments[index].volume_envelope.enabled);
        if !has_volume_envelope {
            self.volume = 0;
        }
    }

    /// Applies the active standard Axy volume slide on a non-zero row tick.
    fn apply_volume_slide(&mut self) {
        // Parser validation forbids simultaneous up and down nibbles. Saturating
        // arithmetic reproduces tracker clamping at the legal 0..=64 bounds.
        let Some(parameter) = self.active_volume_slide else {
            return;
        };
        let upward = parameter >> 4;
        let downward = parameter & 0x0f;
        self.volume = self
            .volume
            .saturating_add(upward)
            .saturating_sub(downward)
            .min(MAX_VOLUME);
    }

    /// Produces this channel's current left and right contribution.
    fn render(&self, module: &XmModule) -> (f32, f32) {
        // Silent and completed voices return before touching sample arrays. A
        // live voice uses wrapped next-frame selection for continuous loops.
        let (Some(instrument_index), Some(sample_index)) =
            (self.sounding_instrument, self.sounding_sample)
        else {
            return (0.0, 0.0);
        };
        let instrument = &module.instruments[instrument_index];
        let sample = &instrument.samples[sample_index];
        let frame_index = self.sample_position.floor() as usize;
        if frame_index >= sample.frames.len() {
            return (0.0, 0.0);
        }
        let next_index = sample.next_frame_index(frame_index, self.sample_direction);
        let fraction = (self.sample_position - frame_index as f64) as f32;
        let left_pcm = f32::from(sample.frames[frame_index]);
        let right_pcm = f32::from(sample.frames[next_index]);
        let pcm = (left_pcm + (right_pcm - left_pcm) * fraction) / 32_768.0;

        // Apply FastTracker's volume envelope/fade formula and panning-envelope
        // displacement before the final linear left/right split.
        let envelope_volume = instrument
            .volume_envelope
            .value_at(self.volume_envelope_tick, MAX_VOLUME.into());
        let amplitude = f32::from(self.volume) / f32::from(MAX_VOLUME) * envelope_volume as f32
            / f32::from(MAX_VOLUME)
            * self.fade_volume as f32
            / MAX_FADE_VOLUME as f32;
        let envelope_panning = instrument
            .panning_envelope
            .value_at(self.panning_envelope_tick, CENTER_PANNING_ENVELOPE);
        let base_panning = i32::from(self.panning);
        let distance_from_center = (base_panning - 128).unsigned_abs() as i32;
        let displacement = (i32::from(envelope_panning) - 32) * (128 - distance_from_center) / 32;
        let final_panning = (base_panning + displacement).clamp(0, 255) as f32;
        let right_gain = final_panning / 255.0;
        let left_gain = 1.0 - right_gain;
        (pcm * amplitude * left_gain, pcm * amplitude * right_gain)
    }

    /// Advances the active sample position by one output frame.
    fn advance_sample(&mut self, module: &XmModule) {
        // Position advances even at zero volume so later row effects cannot
        // resurrect an earlier part of a nominally silent sample.
        let (Some(instrument_index), Some(sample_index)) =
            (self.sounding_instrument, self.sounding_sample)
        else {
            return;
        };
        let sample = &module.instruments[instrument_index].samples[sample_index];
        self.sample_position += self.sample_step * self.sample_direction;
        match sample.loop_kind {
            LoopKind::None => {
                if self.sample_position >= sample.frames.len() as f64 {
                    self.sounding_sample = None;
                }
            }
            LoopKind::Forward => {
                let loop_end = sample.loop_end as f64;
                let loop_length = (sample.loop_end - sample.loop_start) as f64;
                if self.sample_position >= loop_end {
                    self.sample_position =
                        sample.loop_start as f64 + (self.sample_position - loop_end) % loop_length;
                }
            }
            LoopKind::PingPong => {
                // Reflect at the first and last loop frames. Typical XM steps
                // cross at most one edge, but the loop handles high transposes.
                let start = sample.loop_start as f64;
                let end = (sample.loop_end - 1) as f64;
                while self.sample_position > end || self.sample_position < start {
                    if self.sample_position > end {
                        self.sample_position = end - (self.sample_position - end);
                        self.sample_direction = -1.0;
                    } else if self.sample_position < start {
                        self.sample_position = start + (start - self.sample_position);
                        self.sample_direction = 1.0;
                    }
                }
            }
        }
    }

    /// Advances both envelopes and applies post-release instrument fadeout.
    fn finish_tick(&mut self, module: &XmModule) {
        // Envelope ownership follows the sounding sample, not a later selected
        // instrument-only event. Fadeout participates only with volume envelopes.
        let Some(instrument_index) = self.sounding_instrument else {
            return;
        };
        let instrument = &module.instruments[instrument_index];
        instrument
            .volume_envelope
            .advance(&mut self.volume_envelope_tick, self.key_on);
        instrument
            .panning_envelope
            .advance(&mut self.panning_envelope_tick, self.key_on);
        if !self.key_on && instrument.volume_envelope.enabled {
            self.fade_volume = self
                .fade_volume
                .saturating_sub(u32::from(instrument.fadeout));
        }
    }
}

impl Sample {
    /// Selects the interpolation neighbor, wrapping at a forward-loop boundary.
    fn next_frame_index(&self, frame_index: usize, direction: f64) -> usize {
        // Interpolation follows the current traversal direction at ping-pong
        // edges, while forward loops connect their last frame back to the start.
        if self.loop_kind == LoopKind::PingPong && direction < 0.0 {
            return frame_index.saturating_sub(1).max(self.loop_start);
        }
        let next = frame_index + 1;
        if self.loop_kind == LoopKind::Forward && next >= self.loop_end {
            self.loop_start
        } else if self.loop_kind == LoopKind::PingPong && next >= self.loop_end {
            self.loop_end - 2
        } else {
            next.min(self.frames.len() - 1)
        }
    }
}

/// Allocation-free sequencer and sample mixer for one parsed XM module.
pub(crate) struct XmPlayer {
    /// Immutable decoded song shared by the sequencer's channel lookups.
    module: XmModule,
    /// Output sample rate used to turn note frequency into sample steps.
    output_frequency: u32,
    /// Mutable voice state for each pattern channel.
    channels: Box<[ChannelState]>,
    /// Current position within [`XmModule::orders`].
    order: usize,
    /// Current row within the pattern at [`Self::order`].
    row: usize,
    /// Current tick from zero through tempo minus one.
    tick: u16,
    /// Output frames remaining before the next tracker tick boundary.
    tick_frames_remaining: usize,
    /// Remainder retained by the rational BPM-to-frame conversion.
    tick_frame_remainder: u64,
    /// Optional BCD row selected by a Dxx effect for the following order.
    pending_pattern_break: Option<usize>,
    /// Number of output frames rendered, exposed to deterministic unit tests.
    rendered_frames: u64,
}

impl XmPlayer {
    /// Parses an XM payload and prepares silent real-time playback state.
    pub(crate) fn new(encoded: &[u8], output_frequency: u32) -> Result<Self, String> {
        // Parsing and sample decoding happen before SDL opens the callback. The
        // returned player performs no heap allocation while mixing audio.
        if output_frequency == 0 {
            return Err("XM output frequency must be non-zero".to_owned());
        }
        let module = XmModule::parse(encoded)?;
        let channels = (0..module.channel_count)
            .map(|_| ChannelState::silent())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            module,
            output_frequency,
            channels,
            order: 0,
            row: 0,
            tick: 0,
            tick_frames_remaining: 0,
            tick_frame_remainder: 0,
            pending_pattern_break: None,
            rendered_frames: 0,
        })
    }

    /// Adds tracker output to an interleaved stereo floating-point slice.
    pub(crate) fn mix_stereo(&mut self, output: &mut [f32], gain: f32) {
        // SDL supplies complete stereo frames. Ignore a hypothetical trailing
        // scalar rather than indexing it, then advance sequencing per frame.
        let (frames, remainder) = output.as_chunks_mut::<2>();
        debug_assert!(remainder.is_empty(), "SDL supplies whole stereo frames");
        for frame in frames {
            if self.tick_frames_remaining == 0 {
                self.begin_tick();
            }
            let mut left = 0.0;
            let mut right = 0.0;
            for channel in &self.channels {
                let contribution = channel.render(&self.module);
                left += contribution.0;
                right += contribution.1;
            }
            frame[0] += left * gain;
            frame[1] += right * gain;
            for channel in &mut self.channels {
                channel.advance_sample(&self.module);
            }
            self.tick_frames_remaining -= 1;
            self.rendered_frames = self.rendered_frames.wrapping_add(1);
            if self.tick_frames_remaining == 0 {
                self.finish_tick();
            }
        }
    }

    /// Applies row events or continuing effects and schedules one tick duration.
    fn begin_tick(&mut self) {
        // Tick zero reads a fresh row; subsequent ticks apply continuous Axy
        // slides. Duration uses an integer remainder to prevent long-song drift.
        if self.tick == 0 {
            self.process_row();
        } else {
            for channel in &mut self.channels {
                channel.apply_volume_slide();
            }
        }
        let numerator = u64::from(self.output_frequency) * 5 + self.tick_frame_remainder;
        let denominator = u64::from(self.module.initial_bpm) * 2;
        self.tick_frames_remaining = (numerator / denominator).max(1) as usize;
        self.tick_frame_remainder = numerator % denominator;
    }

    /// Dispatches every channel event on the sequencer's current pattern row.
    fn process_row(&mut self) {
        // Copy each compact cell before mutating channels to keep immutable
        // pattern storage and mutable playback state cleanly separated.
        self.pending_pattern_break = None;
        let pattern_index = usize::from(self.module.orders[self.order]);
        for channel_index in 0..self.module.channel_count {
            let cell = self.module.patterns[pattern_index].cell(
                self.row,
                channel_index,
                self.module.channel_count,
            );
            let channel = &mut self.channels[channel_index];
            channel.active_volume_slide = None;
            if cell.instrument != 0 {
                channel.selected_instrument = Some(usize::from(cell.instrument - 1));
            }
            match cell.note {
                1..=96 => channel.trigger(&self.module, cell.note, self.output_frequency),
                97 => channel.release(&self.module),
                _ => {}
            }
            if cell.volume != 0 {
                channel.volume = cell.volume - 0x10;
            }
            match cell.effect {
                0x0a => {
                    if cell.parameter != 0 {
                        channel.volume_slide_memory = cell.parameter;
                    }
                    channel.active_volume_slide = Some(channel.volume_slide_memory);
                }
                0x0d => {
                    self.pending_pattern_break = Some(usize::from(
                        (cell.parameter >> 4) * 10 + (cell.parameter & 0x0f),
                    ));
                }
                _ => {}
            }
        }
    }

    /// Completes envelopes and advances the tracker tick, row, and order cursors.
    fn finish_tick(&mut self) {
        // Every channel observes the just-finished tick before the sequencer
        // decides whether the next tick continues this row or begins another.
        for channel in &mut self.channels {
            channel.finish_tick(&self.module);
        }
        self.tick += 1;
        if self.tick < self.module.initial_tempo {
            return;
        }
        self.tick = 0;

        let pattern_index = usize::from(self.module.orders[self.order]);
        let pattern_rows = self.module.patterns[pattern_index].rows;
        if let Some(break_row) = self.pending_pattern_break.take() {
            self.advance_order();
            let next_pattern = usize::from(self.module.orders[self.order]);
            self.row = break_row.min(self.module.patterns[next_pattern].rows - 1);
        } else if self.row + 1 < pattern_rows {
            self.row += 1;
        } else {
            self.advance_order();
            self.row = 0;
        }
    }

    /// Moves to the next order or the module's declared restart position.
    fn advance_order(&mut self) {
        // XM restart is an order-list index, not a pattern number. This module
        // declares zero, but honoring the field costs no callback complexity.
        self.order += 1;
        if self.order >= self.module.orders.len() {
            self.order = self.module.restart_order;
        }
    }

    /// Returns the number of output frames consumed by active playback.
    #[cfg(test)]
    pub(crate) const fn rendered_frames(&self) -> u64 {
        // The monotonic counter deliberately does not reset at song restart;
        // callers use it only to distinguish running music from paused music.
        self.rendered_frames
    }
}

/// Checked little-endian cursor used for all variable-length XM structures.
struct Reader<'a> {
    /// Complete bytes of the current file or packed-pattern sub-slice.
    bytes: &'a [u8],
    /// Offset of the next unread byte within [`Self::bytes`].
    position: usize,
}

impl<'a> Reader<'a> {
    /// Starts reading at byte zero of a borrowed immutable slice.
    const fn new(bytes: &'a [u8]) -> Self {
        // The cursor borrows input and never copies file payloads by itself.
        Self { bytes, position: 0 }
    }

    /// Returns the offset of the next unread byte.
    const fn position(&self) -> usize {
        // Exposing only the numeric cursor keeps all movement checked by methods.
        self.position
    }

    /// Returns `length` bytes and advances, or describes a truncated structure.
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        // Checked addition handles both arithmetic overflow and ordinary EOF.
        let start = self.position;
        let end = start
            .checked_add(length)
            .ok_or_else(|| format!("XM offset {start}: field length overflows"))?;
        let bytes = self.bytes.get(start..end).ok_or_else(|| {
            format!(
                "XM offset {start}: need {length} bytes but only {} remain",
                self.bytes.len().saturating_sub(start)
            )
        })?;
        self.position = end;
        Ok(bytes)
    }

    /// Moves to a declared absolute structure end without permitting rewind.
    fn seek(&mut self, position: usize) -> Result<(), String> {
        // Header extensions may be skipped forward, but a short declared header
        // must not make the parser reinterpret already consumed bytes.
        if position < self.position {
            return Err(format!(
                "XM offset {}: declared structure end {position} precedes parsed fields",
                self.position
            ));
        }
        let distance = position - self.position;
        let _ = self.take(distance)?;
        Ok(())
    }

    /// Reads one unsigned byte.
    fn byte(&mut self) -> Result<u8, String> {
        // `take` centralizes EOF reporting for every scalar width.
        Ok(self.take(1)?[0])
    }

    /// Reads one signed two's-complement byte.
    fn signed_byte(&mut self) -> Result<i8, String> {
        // Rust's byte-to-i8 cast preserves the encoded two's-complement bits.
        Ok(self.byte()? as i8)
    }

    /// Reads one little-endian unsigned 16-bit word.
    fn word(&mut self) -> Result<u16, String> {
        // A fixed array conversion avoids alignment and host-endian assumptions.
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .expect("a two-byte slice always converts to a two-byte array");
        Ok(u16::from_le_bytes(bytes))
    }

    /// Reads one little-endian unsigned 32-bit double word.
    fn dword(&mut self) -> Result<u32, String> {
        // XM is little-endian regardless of the machine running the clone.
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .expect("a four-byte slice always converts to a four-byte array");
        Ok(u32::from_le_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    //! Production-asset parser and real-time sequencing regression checks.

    use super::{LoopKind, XmModule, XmPlayer};

    /// Exact original tracker module embedded by the SDL audio layer.
    const MUSIC: &[u8] = include_bytes!("../assets/audio/music.xm");

    /// Confirms the production arrangement retains its expected structural metadata.
    #[test]
    fn production_module_parses_with_expected_song_layout() {
        let module = XmModule::parse(MUSIC).expect("the Supaplex XM should parse");

        // These values catch accidental replacement with a render or a
        // different tracker export while leaving instrument names irrelevant.
        assert_eq!(module.orders.len(), 41);
        assert_eq!(module.restart_order, 0);
        assert_eq!(module.channel_count, 4);
        assert_eq!(module.patterns.len(), 8);
        assert_eq!(module.instruments.len(), 9);
        assert_eq!(module.initial_tempo, 8);
        assert_eq!(module.initial_bpm, 168);
        assert_eq!(module.patterns[7].rows, 49);
    }

    /// Confirms 16-bit delta decoding and byte-to-frame loop conversion.
    #[test]
    fn production_samples_decode_with_exact_lengths_and_loops() {
        let module = XmModule::parse(MUSIC).expect("the Supaplex XM should parse");
        let bass = &module.instruments[0].samples[0];
        let high_synth = &module.instruments[1].samples[0];

        // The first sample is unlooped; the second declares byte offsets that
        // must be halved exactly once for its 16-bit PCM representation.
        assert_eq!(bass.frames.len(), 83_787);
        assert_eq!(bass.loop_kind, LoopKind::None);
        assert_eq!(high_synth.frames.len(), 15_309);
        assert_eq!(high_synth.loop_kind, LoopKind::Forward);
        assert_eq!(high_synth.loop_start, 14_460);
        assert_eq!(high_synth.loop_end, 15_309);
    }

    /// Confirms the player produces finite audible stereo output and advances time.
    #[test]
    fn production_player_mixes_tracker_frames_without_allocating_state() {
        let mut player = XmPlayer::new(MUSIC, 44_100).expect("the Supaplex XM should parse");
        let mut output = vec![0.0; 44_100 * 2];

        player.mix_stereo(&mut output, 0.38);
        assert_eq!(player.rendered_frames(), 44_100);
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().any(|sample| sample.abs() > 0.000_001));
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
    }

    /// Confirms malformed embedded data becomes an initialization error, not a panic.
    #[test]
    fn truncated_module_reports_the_missing_payload() {
        let error = XmPlayer::new(&MUSIC[..MUSIC.len() - 1], 44_100)
            .err()
            .expect("a truncated final sample must fail");

        assert!(error.contains("need"), "unexpected parser error: {error}");
        assert!(error.contains("remain"), "unexpected parser error: {error}");
    }
}
