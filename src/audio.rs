//! Audio-device ownership, original AdLib music, and Sound Blaster effects.
//!
//! This module reads both original DOS driver images from the configured asset
//! source. It interprets `ADLIB.SND` as live OPL2 music and extracts the seven
//! embedded Creative VOC records from `BLASTER.SND` as Sound Blaster PCM. The
//! compact original samples are converted once before the real-time callback.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use crate::platform as sdl2;
use sdl2::{
    AudioSubsystem,
    audio::{AudioCallback, AudioDevice, AudioSpec, AudioSpecDesired},
};

use crate::{assets, game::SoundEffect, opl::OplPlayer};

/// Requested device frequency used by every decoded production clip.
const OUTPUT_FREQUENCY: i32 = 44_100;

/// Stereo output matches the modern SDL renderer and leaves room for music.
const OUTPUT_CHANNELS: u8 = 2;

/// Small callback buffers keep short Base and Bug effects responsive to input.
const OUTPUT_BUFFER_SAMPLES: u16 = 512;

/// Original effect-priority time is measured in 20-millisecond units.
const PRIORITY_UNIT_MILLISECONDS: usize = 20;

/// Background level used before effects are added to the callback buffer.
const MUSIC_VOLUME: f32 = 0.38;

/// Exact byte length of the original Supaplex `BLASTER.SND` driver image.
const BLASTER_DRIVER_LENGTH: usize = 39_195;

/// CRC-32 of the exact original Sound Blaster driver accepted by this decoder.
const BLASTER_DRIVER_CRC32: u32 = 0xf800_d380;

/// Standard twenty-byte signature beginning every embedded Creative VOC file.
const VOC_SIGNATURE: &[u8; 20] = b"Creative Voice File\x1a";

/// Header size stored by each version-1.10 VOC record in the original driver.
const VOC_HEADER_LENGTH: usize = 26;

/// Little-endian version word identifying Creative Voice File revision 1.10.
const VOC_VERSION: u16 = 0x010a;

/// Revision complement stored beside [`VOC_VERSION`] by every embedded record.
const VOC_VERSION_CHECKSUM: u16 = 0x1129;

/// VOC block type containing the time constant, codec, and unsigned PCM bytes.
const VOC_SOUND_DATA_BLOCK: u8 = 1;

/// Original VOC time constant producing `1_000_000 / (256 - 0x88)` hertz.
const VOC_TIME_CONSTANT: u8 = 0x88;

/// Creative VOC codec zero denotes uncompressed unsigned eight-bit PCM.
const VOC_PCM_CODEC: u8 = 0;

/// VOC block type zero marks the end immediately following each PCM payload.
const VOC_TERMINATOR_BLOCK: u8 = 0;

/// Reduced numerator of the exact 8,333-and-one-third-hertz source rate.
const BLASTER_SAMPLE_RATE_NUMERATOR: usize = 25_000;

/// Reduced denominator of the exact 8,333-and-one-third-hertz source rate.
const BLASTER_SAMPLE_RATE_DENOMINATOR: usize = 3;

/// Linear gain matching the previous conversions' intentional -3.6 dB headroom.
const BLASTER_EFFECT_GAIN: f32 = 0.660_693_47;

/// Start offsets of Explosion through Exit VOC records inside `BLASTER.SND`.
const BLASTER_VOC_OFFSETS: [usize; 7] = [0x028f, 0x2dd4, 0x3d1f, 0x4488, 0x4cdb, 0x549a, 0x5515];

/// Exact unsigned PCM lengths corresponding to [`BLASTER_VOC_OFFSETS`].
const BLASTER_PCM_LENGTHS: [usize; 7] = [11_044, 3_882, 1_864, 2_098, 1_950, 90, 14_960];

/// Number of ordered operations retained between the game and audio callback.
///
/// The game can produce several effects during one catch-up frame. Two hundred
/// fifty-six atomic slots absorb that burst without allocation while keeping
/// main-thread submission strictly non-blocking if a backend stops consuming.
const COMMAND_QUEUE_CAPACITY: usize = 256;

/// Atomic slot value that means no command is ready for the callback.
const EMPTY_COMMAND_SLOT: u8 = 0;

/// Owns the live platform playback device and its callback mixer.
pub struct AudioPlayer {
    /// Live callback device taken and closed explicitly by [`Self::drop`].
    ///
    /// The option is populated throughout normal playback. Its empty state
    /// exists only while destruction is already in progress, allowing `Drop`
    /// to move the device out and close it before releasing other fields.
    device: Option<AudioDevice<Mixer>>,
    /// Independent handle keeping the audio subsystem initialized through closure.
    ///
    /// `AudioDevice` internally declares its subsystem handle before its device
    /// identifier. If that internal handle is the last one, its generated field
    /// destruction calls `SDL_QuitSubSystem` before `SDL_CloseAudioDevice`.
    /// Retaining this second handle prevents that invalid backend teardown order.
    _subsystem_guard: AudioSubsystem,
    /// Non-waiting producer for ordered callback operations.
    command_sender: AudioCommandSender,
    /// Atomic request that makes subsequent callbacks return immediate silence.
    stop_requested: Arc<AtomicBool>,
    /// Main-thread copy of the user's persistent music preference.
    music_enabled: bool,
    /// Main-thread copy of the user's persistent effect preference.
    effects_enabled: bool,
}

impl AudioPlayer {
    /// Opens the default stereo device and starts the original AdLib score.
    pub fn new(subsystem: &AudioSubsystem) -> Result<Self, String> {
        // Load and decode before opening the device so missing or malformed
        // production data produces a normal initialization error rather than
        // panicking inside SDL's callback-construction closure.
        let asset_set = assets::load_audio().map_err(|error| error.to_string())?;
        let effects = EffectBank::decode(
            asset_set.effects.as_ref(),
            OUTPUT_FREQUENCY,
            OUTPUT_CHANNELS,
        )?;
        let music = OplPlayer::new(asset_set.music.as_ref(), OUTPUT_FREQUENCY as u32)
            .map_err(|error| format!("decode production OPL music: {error}"))?;
        let (command_sender, command_receiver) = audio_command_channel();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let mixer_stop_requested = Arc::clone(&stop_requested);
        // The returned player must retain a handle separate from the one owned
        // inside `AudioDevice`. `Drop` then closes the device explicitly while
        // this guard still keeps the backend initialized, including on errors.
        let subsystem_guard = subsystem.clone();
        let desired = AudioSpecDesired {
            freq: Some(OUTPUT_FREQUENCY),
            channels: Some(OUTPUT_CHANNELS),
            samples: Some(OUTPUT_BUFFER_SAMPLES),
        };
        let device = subsystem
            .open_playback(None, &desired, move |spec| {
                Mixer::new(spec, effects, music, command_receiver, mixer_stop_requested)
            })
            .map_err(|error| format!("open playback device: {error}"))?;

        // Platform devices begin paused so construction cannot race the caller.
        // Resume only after the fully initialized handle is ready to return.
        device.resume();
        Ok(Self {
            device: Some(device),
            _subsystem_guard: subsystem_guard,
            command_sender,
            stop_requested,
            music_enabled: true,
            effects_enabled: true,
        })
    }

    /// Offers one simulation-selected effect to the original priority gate.
    pub fn play(&mut self, effect: SoundEffect) {
        // A muted request is discarded immediately, matching the callback's
        // policy while avoiding an obsolete effect after a later re-enable.
        if self.effects_enabled {
            self.queue_command(AudioCommand::PlayEffect { effect });
        }
    }

    /// Toggles gameplay effects and returns their new enabled state.
    pub fn toggle_effects(&mut self) -> bool {
        // Disabling immediately clears the active voice and its priority. A
        // later re-enable therefore cannot resume half of an obsolete effect.
        self.effects_enabled = !self.effects_enabled;
        self.queue_command(AudioCommand::SetEffectsEnabled {
            enabled: self.effects_enabled,
        });
        self.effects_enabled
    }

    /// Sets gameplay-effect playback explicitly and returns the retained state.
    pub fn set_effects_enabled(&mut self, enabled: bool) -> bool {
        // Explicit setters let the original hardware buttons choose coherent
        // music/effect combinations without depending on a previous toggle.
        self.effects_enabled = enabled;
        self.queue_command(AudioCommand::SetEffectsEnabled { enabled });
        self.effects_enabled
    }

    /// Returns whether gameplay effects currently accept new requests.
    pub fn effects_enabled(&self) -> bool {
        // This main-thread preference changes synchronously when controls are
        // activated; callback application may follow by at most one buffer.
        self.effects_enabled
    }

    /// Toggles soundtrack playback and returns its new enabled state.
    pub fn toggle_music(&mut self) -> bool {
        // User muting and Exit pausing are distinct: toggling back on is an
        // explicit request to resume from the retained loop cursor.
        self.music_enabled = !self.music_enabled;
        self.queue_command(AudioCommand::SetMusicEnabled {
            enabled: self.music_enabled,
        });
        self.music_enabled
    }

    /// Sets soundtrack playback explicitly and returns the retained state.
    pub fn set_music_enabled(&mut self, enabled: bool) -> bool {
        // The user-selected enabled flag and active playback flag change
        // together here; only protected Exit playback may pause them separately.
        self.music_enabled = enabled;
        self.queue_command(AudioCommand::SetMusicEnabled { enabled });
        self.music_enabled
    }

    /// Returns whether the soundtrack is enabled by the user.
    pub fn music_enabled(&self) -> bool {
        // Returning local control state avoids waiting on a platform callback
        // merely to draw the controls screen.
        self.music_enabled
    }

    /// Clears transient effects and resumes enabled music for a restarted level.
    pub fn restart_level(&mut self) {
        // Exit protects its effect for five seconds and pauses music. A manual
        // level restart is a new session, so neither terminal state may leak.
        self.queue_command(AudioCommand::RestartLevel);
    }

    /// Consumes the player and performs its ordered synchronous destruction.
    pub fn shutdown(self) {
        // `Drop` requests callback silence, takes and closes the device while its
        // subsystem guard remains live, then releases the remaining fields. This
        // call makes that same sequence explicit at every normal application exit.
        drop(self);
    }

    /// Offers one operation without ever waiting for the audio callback.
    fn queue_command(&mut self, command: AudioCommand) {
        // A stopped or severely delayed backend may fill the bounded channel.
        // Dropping that excess request keeps input and shutdown responsive; the
        // ordinary restart/Explosion pair occupies only two of 256 slots.
        let _accepted = self.command_sender.send(command);
    }
}

impl Drop for AudioPlayer {
    /// Makes every destruction path quiesce the callback before device closure.
    fn drop(&mut self) {
        // Errors can leave the application after audio initialization without
        // reaching `shutdown`. Publishing the same terminal flag here gives
        // implicit and explicit destruction identical callback behavior.
        self.stop_requested.store(true, Ordering::Release);

        // Taking the device closes it here, while `_subsystem_guard` is still a
        // live field. Only after this method returns does Rust release that
        // guard and permit SDL to quit the audio subsystem.
        if let Some(device) = self.device.take() {
            drop(device);
        }
    }
}

/// Main-thread audio operation applied in order by the next SDL callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioCommand {
    /// Offer one gameplay effect to the original single-channel priority gate.
    PlayEffect {
        /// Semantic clip and policy selected by the simulation.
        effect: SoundEffect,
    },
    /// Replace the persistent user preference for gameplay effects.
    SetEffectsEnabled {
        /// Whether new effects may play after this command is applied.
        enabled: bool,
    },
    /// Replace the persistent user preference for soundtrack playback.
    SetMusicEnabled {
        /// Whether music should actively advance after this command.
        enabled: bool,
    },
    /// Clear terminal audio state before a new level or menu session.
    RestartLevel,
}

impl AudioCommand {
    /// Packs one command into the non-zero byte stored by an atomic queue slot.
    const fn encode(self) -> u8 {
        // Stable explicit values avoid depending on either enum's Rust layout.
        // Effects occupy one contiguous range and controls follow afterward.
        match self {
            Self::PlayEffect {
                effect: SoundEffect::Explosion,
            } => 1,
            Self::PlayEffect {
                effect: SoundEffect::Infotron,
            } => 2,
            Self::PlayEffect {
                effect: SoundEffect::Push,
            } => 3,
            Self::PlayEffect {
                effect: SoundEffect::Fall,
            } => 4,
            Self::PlayEffect {
                effect: SoundEffect::Bug,
            } => 5,
            Self::PlayEffect {
                effect: SoundEffect::Base,
            } => 6,
            Self::PlayEffect {
                effect: SoundEffect::Exit,
            } => 7,
            Self::SetEffectsEnabled { enabled: false } => 8,
            Self::SetEffectsEnabled { enabled: true } => 9,
            Self::SetMusicEnabled { enabled: false } => 10,
            Self::SetMusicEnabled { enabled: true } => 11,
            Self::RestartLevel => 12,
        }
    }

    /// Restores one command published by the single producer.
    fn decode(code: u8) -> Option<Self> {
        // Zero is reserved for an empty queue slot. Any other unrecognized code
        // would indicate an internal producer defect rather than external data.
        match code {
            1 => Some(Self::PlayEffect {
                effect: SoundEffect::Explosion,
            }),
            2 => Some(Self::PlayEffect {
                effect: SoundEffect::Infotron,
            }),
            3 => Some(Self::PlayEffect {
                effect: SoundEffect::Push,
            }),
            4 => Some(Self::PlayEffect {
                effect: SoundEffect::Fall,
            }),
            5 => Some(Self::PlayEffect {
                effect: SoundEffect::Bug,
            }),
            6 => Some(Self::PlayEffect {
                effect: SoundEffect::Base,
            }),
            7 => Some(Self::PlayEffect {
                effect: SoundEffect::Exit,
            }),
            8 => Some(Self::SetEffectsEnabled { enabled: false }),
            9 => Some(Self::SetEffectsEnabled { enabled: true }),
            10 => Some(Self::SetMusicEnabled { enabled: false }),
            11 => Some(Self::SetMusicEnabled { enabled: true }),
            12 => Some(Self::RestartLevel),
            _ => None,
        }
    }
}

/// Shared atomic storage for the one-producer, one-consumer command channel.
struct AudioCommandSlots {
    /// Ring entries where zero is empty and non-zero is an encoded command.
    slots: [AtomicU8; COMMAND_QUEUE_CAPACITY],
}

impl AudioCommandSlots {
    /// Creates a completely empty ring before either endpoint becomes visible.
    fn new() -> Self {
        // Every atomic is initialized independently because `AtomicU8` is not
        // `Copy`; construction occurs before the SDL callback can run.
        Self {
            slots: std::array::from_fn(|_| AtomicU8::new(EMPTY_COMMAND_SLOT)),
        }
    }
}

/// Main-thread endpoint of the non-waiting audio command channel.
struct AudioCommandSender {
    /// Atomic ring shared with exactly one callback-side receiver.
    storage: Arc<AudioCommandSlots>,
    /// Next ring position considered by the single main-thread producer.
    next_slot: usize,
}

impl AudioCommandSender {
    /// Publishes one command or reports a full ring without blocking.
    fn send(&mut self, command: AudioCommand) -> bool {
        let slot = &self.storage.slots[self.next_slot];

        // One Acquire load determines availability without a retry or a
        // read-modify-write instruction. Because there is exactly one producer,
        // no other thread can claim the empty slot before the following store.
        if slot.load(Ordering::Acquire) != EMPTY_COMMAND_SLOT {
            return false;
        }
        // Release publication makes the encoded byte visible before the
        // callback's corresponding Acquire load.
        slot.store(command.encode(), Ordering::Release);
        self.next_slot = next_command_slot(self.next_slot);
        true
    }
}

/// Audio-callback endpoint of the non-waiting audio command channel.
struct AudioCommandReceiver {
    /// Atomic ring shared with exactly one main-thread sender.
    storage: Arc<AudioCommandSlots>,
    /// Next ring position consumed in strict publication order.
    next_slot: usize,
}

impl AudioCommandReceiver {
    /// Takes one ready command or returns immediately when the ring is empty.
    fn receive(&mut self) -> Option<AudioCommand> {
        let slot = &self.storage.slots[self.next_slot];
        let code = slot.load(Ordering::Acquire);
        if code == EMPTY_COMMAND_SLOT {
            return None;
        }
        let command = AudioCommand::decode(code)
            .expect("the private audio producer publishes only valid command codes");

        // Release clearing lets the producer safely reuse this position only
        // after the command has been copied into callback-owned stack state.
        slot.store(EMPTY_COMMAND_SLOT, Ordering::Release);
        self.next_slot = next_command_slot(self.next_slot);
        Some(command)
    }
}

/// Creates the unique producer and consumer endpoints for one audio device.
fn audio_command_channel() -> (AudioCommandSender, AudioCommandReceiver) {
    // Both cursors begin at zero and advance only in their owning threads. The
    // atomic slot values provide publication; no cursor itself is shared.
    let storage = Arc::new(AudioCommandSlots::new());
    (
        AudioCommandSender {
            storage: Arc::clone(&storage),
            next_slot: 0,
        },
        AudioCommandReceiver {
            storage,
            next_slot: 0,
        },
    )
}

/// Advances a ring cursor without allowing it to leave the fixed slot array.
const fn next_command_slot(current: usize) -> usize {
    // A branch avoids modulo division in both the event loop and real-time
    // callback while preserving exact wrap at the final array element.
    if current + 1 == COMMAND_QUEUE_CAPACITY {
        0
    } else {
        current + 1
    }
}

/// Device-format sample arrays indexed by [`SoundEffect::index`].
struct EffectBank {
    /// Interleaved stereo floating-point samples for every original effect.
    clips: [Box<[f32]>; 7],
}

impl EffectBank {
    /// Extracts and converts every original VOC into one callback sample layout.
    fn decode(driver: &[u8], frequency: i32, channels: u8) -> Result<Self, String> {
        // Parsing returns the exact semantic order used by `SoundEffect::index`.
        // Conversion happens before opening the SDL device, leaving the callback
        // with immutable device-rate samples and no format work or allocation.
        let encoded_effects = parse_blaster_effects(driver)?;
        let mut clips = Vec::with_capacity(encoded_effects.len());
        for (index, encoded) in encoded_effects.into_iter().enumerate() {
            clips.push(
                convert_blaster_pcm(encoded, frequency, channels)
                    .map_err(|error| format!("decode production effect {index}: {error}"))?
                    .into_boxed_slice(),
            );
        }
        let clips = clips
            .try_into()
            .map_err(|_| "production effect count does not match SoundEffect".to_owned())?;
        Ok(Self { clips })
    }

    /// Returns the device-format samples assigned to one semantic effect.
    fn clip(&self, effect: SoundEffect) -> &[f32] {
        // `index` is a total match rather than a numeric enum cast, keeping the
        // asset contract stable if enum discriminants ever change.
        &self.clips[effect.index()]
    }
}

/// Cursor for the single effect channel used by the original sound routines.
struct EffectVoice {
    /// Semantic identity used to fetch immutable samples from [`EffectBank`].
    effect: SoundEffect,
    /// Next interleaved floating-point sample copied by the callback.
    cursor: usize,
}

/// Original priority and blocking behavior for one effect request.
#[derive(Clone, Copy)]
struct EffectPolicy {
    /// Priority stored after this request interrupts the current effect.
    priority: u8,
    /// Current priority at or above this value rejects the request.
    rejection_threshold: u8,
    /// Number of original 20-millisecond units before priority returns to zero.
    duration_units: usize,
    /// Whether the request bypasses priority rejection, as Exit does.
    unconditional: bool,
}

/// Callback-owned state mixed into SDL's interleaved floating-point buffer.
struct Mixer {
    /// Obtained device layout used for duration and frame calculations.
    spec: AudioSpec,
    /// Decoded immutable effect samples shared by successive voices.
    effects: EffectBank,
    /// Register-level synthesizer for the original DOS AdLib soundtrack.
    music: OplPlayer,
    /// Persistent user preference controlled by the M key.
    music_enabled: bool,
    /// Session playback state paused independently when Exit is accepted.
    music_playing: bool,
    /// Current single-channel effect, replaced when a request is accepted.
    effect_voice: Option<EffectVoice>,
    /// Original global priority value, where zero means no protected sound.
    current_priority: u8,
    /// Device frames before [`Self::current_priority`] resets to zero.
    priority_frames_remaining: usize,
    /// User-controlled effect switch toggled by the S key.
    effects_enabled: bool,
    /// Non-waiting endpoint receiving ordered main-thread operations.
    command_receiver: AudioCommandReceiver,
    /// Atomic terminal request checked before any synthesis work.
    stop_requested: Arc<AtomicBool>,
}

impl Mixer {
    /// Creates silent callback state around already converted effect samples.
    fn new(
        spec: AudioSpec,
        effects: EffectBank,
        music: OplPlayer,
        command_receiver: AudioCommandReceiver,
        stop_requested: Arc<AtomicBool>,
    ) -> Self {
        // The audio device owns this value for its full lifetime; no callback
        // path allocates, decodes assets, or touches simulation state.
        Self {
            spec,
            effects,
            music,
            music_enabled: true,
            music_playing: true,
            effect_voice: None,
            current_priority: 0,
            priority_frames_remaining: 0,
            effects_enabled: true,
            command_receiver,
            stop_requested,
        }
    }

    /// Applies every main-thread operation queued since the previous callback.
    fn apply_pending_commands(&mut self) {
        // `receive` performs one Acquire load and never waits. The fixed loop
        // limit prevents a producer that is continually publishing operations
        // from keeping the real-time callback here indefinitely.
        for _ in 0..COMMAND_QUEUE_CAPACITY {
            let Some(command) = self.command_receiver.receive() else {
                break;
            };
            match command {
                AudioCommand::PlayEffect { effect } => {
                    self.play_effect(effect);
                }
                AudioCommand::SetEffectsEnabled { enabled } => {
                    self.effects_enabled = enabled;
                    if !enabled {
                        // Muting discards the active cursor and protection, so
                        // re-enabling cannot resume an obsolete partial clip.
                        self.effect_voice = None;
                        self.current_priority = 0;
                        self.priority_frames_remaining = 0;
                    }
                }
                AudioCommand::SetMusicEnabled { enabled } => {
                    // Explicit user control resumes or pauses the retained song
                    // cursor independently of a previous accepted Exit effect.
                    self.music_enabled = enabled;
                    self.music_playing = enabled;
                }
                AudioCommand::RestartLevel => {
                    // New sessions discard protected effects while respecting
                    // the user's persistent music-enabled preference.
                    self.effect_voice = None;
                    self.current_priority = 0;
                    self.priority_frames_remaining = 0;
                    self.music_playing = self.music_enabled;
                }
            }
        }
    }

    /// Applies the DOS priority gate and replaces the one effect channel.
    fn play_effect(&mut self, effect: SoundEffect) -> bool {
        // Muting rejects requests without retaining them for a later unmute.
        // Exit is otherwise unconditional and can replace any current sound.
        if !self.effects_enabled {
            return false;
        }
        let policy = effect.policy();
        if !policy.unconditional && self.current_priority >= policy.rejection_threshold {
            return false;
        }

        self.effect_voice = Some(EffectVoice { effect, cursor: 0 });
        self.current_priority = policy.priority;
        self.priority_frames_remaining = self.spec.freq.max(1).unsigned_abs() as usize
            * PRIORITY_UNIT_MILLISECONDS
            * policy.duration_units
            / 1_000;
        if effect == SoundEffect::Exit {
            // The Exit routine pauses music only after its effect request is
            // accepted; muting effects therefore leaves music untouched.
            self.music_playing = false;
        }
        true
    }

    /// Adds the looping original AdLib music to the cleared stereo output buffer.
    fn mix_music(&mut self, output: &mut [f32]) {
        // Exit and M both silence the player without advancing or rewinding it.
        // A restart or explicit toggle therefore resumes at the retained tick.
        if !self.music_enabled || !self.music_playing {
            return;
        }
        debug_assert_eq!(
            self.spec.channels, OUTPUT_CHANNELS,
            "the OPL player is constructed for the requested stereo layout"
        );
        self.music.mix_stereo(output, MUSIC_VOLUME);
    }

    /// Copies the active effect over the already cleared output slice.
    fn mix_effect(&mut self, output: &mut [f32]) {
        let Some(voice) = self.effect_voice.as_mut() else {
            return;
        };
        let clip = self.effects.clip(voice.effect);
        let remaining = &clip[voice.cursor.min(clip.len())..];
        let copied = remaining.len().min(output.len());

        // Effects occupy the one original channel, so accepted requests replace
        // instead of overlapping one another. The final clamp also protects a
        // future music layer from integer-WAV peaks at full scale.
        for (destination, source) in output.iter_mut().zip(&remaining[..copied]) {
            *destination = (*destination + *source).clamp(-1.0, 1.0);
        }
        voice.cursor += copied;
        if voice.cursor >= clip.len() {
            self.effect_voice = None;
        }
    }

    /// Advances the wall-clock priority timer by one callback buffer.
    fn advance_priority(&mut self, sample_count: usize) {
        // The output is interleaved, so priority durations count sample frames
        // rather than individual left and right channel values.
        let channels = usize::from(self.spec.channels.max(1));
        let frames = sample_count / channels;
        self.priority_frames_remaining = self.priority_frames_remaining.saturating_sub(frames);
        if self.priority_frames_remaining == 0 {
            self.current_priority = 0;
        }
    }
}

impl AudioCallback for Mixer {
    type Channel = f32;

    /// Produces one mixed music-and-effect buffer whenever SDL requests audio.
    fn callback(&mut self, output: &mut [Self::Channel]) {
        // SDL may recycle buffers, so every callback must establish silence
        // before selectively adding active voices.
        output.fill(0.0);
        if self.stop_requested.load(Ordering::Acquire) {
            // Shutdown must never enter either synthesizer after the main
            // thread requests quiescence; returning silence is immediate.
            return;
        }
        self.apply_pending_commands();
        self.mix_music(output);
        self.mix_effect(output);
        self.advance_priority(output.len());
    }
}

impl SoundEffect {
    /// Maps semantic variants to the stable production-asset array order.
    const fn index(self) -> usize {
        match self {
            Self::Explosion => 0,
            Self::Infotron => 1,
            Self::Push => 2,
            Self::Fall => 3,
            Self::Bug => 4,
            Self::Base => 5,
            Self::Exit => 6,
        }
    }

    /// Returns the exact global priority parameters used by the DOS routines.
    const fn policy(self) -> EffectPolicy {
        // Infotron deliberately rejects only priority five or higher even
        // though it stores priority four; repeated collections may retrigger it.
        match self {
            Self::Explosion => EffectPolicy::new(5, 5, 0x0f),
            Self::Infotron => EffectPolicy::new(4, 5, 0x0f),
            Self::Push => EffectPolicy::new(2, 2, 7),
            Self::Fall => EffectPolicy::new(2, 2, 7),
            Self::Bug => EffectPolicy::new(3, 3, 3),
            Self::Base => EffectPolicy::new(1, 1, 3),
            Self::Exit => EffectPolicy::unconditional(0x0a, 0xfa),
        }
    }
}

impl EffectPolicy {
    /// Creates a priority-gated effect policy from original byte values.
    const fn new(priority: u8, rejection_threshold: u8, duration_units: usize) -> Self {
        Self {
            priority,
            rejection_threshold,
            duration_units,
            unconditional: false,
        }
    }

    /// Creates the unconditional Exit policy that also supersedes explosions.
    const fn unconditional(priority: u8, duration_units: usize) -> Self {
        Self {
            priority,
            rejection_threshold: 0,
            duration_units,
            unconditional: true,
        }
    }
}

/// Validates the original driver and returns its seven borrowed PCM payloads.
fn parse_blaster_effects(driver: &[u8]) -> Result<[&[u8]; 7], String> {
    // Fixed offsets are appropriate only for the one source revision whose
    // complete identity is established first. This rejects repacks and damage
    // before any driver-derived index reaches a slice operation.
    if driver.len() != BLASTER_DRIVER_LENGTH {
        return Err(format!(
            "BLASTER.SND is {} bytes; expected {BLASTER_DRIVER_LENGTH}",
            driver.len()
        ));
    }
    let checksum = crc32(driver);
    if checksum != BLASTER_DRIVER_CRC32 {
        return Err(format!(
            "BLASTER.SND CRC-32 is {checksum:08x}; expected {BLASTER_DRIVER_CRC32:08x}"
        ));
    }

    // Each record is parsed independently even though the complete checksum is
    // known. These structural checks document the embedded VOC contract and
    // make a future deliberately supported driver revision fail descriptively.
    let mut effects = Vec::with_capacity(BLASTER_VOC_OFFSETS.len());
    for (effect_index, (&offset, &expected_length)) in BLASTER_VOC_OFFSETS
        .iter()
        .zip(&BLASTER_PCM_LENGTHS)
        .enumerate()
    {
        effects.push(parse_voc_pcm(
            driver,
            offset,
            expected_length,
            effect_index,
        )?);
    }
    effects
        .try_into()
        .map_err(|_| "BLASTER.SND effect table does not contain seven records".to_owned())
}

/// Parses one exact uncompressed Creative VOC record from the driver image.
fn parse_voc_pcm(
    driver: &[u8],
    offset: usize,
    expected_length: usize,
    effect_index: usize,
) -> Result<&[u8], String> {
    // The full driver length and checksum make every fixed read safe. Checked
    // range construction remains explicit so this parser also documents each
    // boundary instead of relying on an opaque indexing panic.
    let signature_end = offset
        .checked_add(VOC_SIGNATURE.len())
        .ok_or_else(|| format!("effect {effect_index} VOC signature offset overflow"))?;
    if driver.get(offset..signature_end) != Some(VOC_SIGNATURE.as_slice()) {
        return Err(format!(
            "effect {effect_index} has no Creative VOC signature"
        ));
    }

    let header_length = read_u16(driver, offset + 20, effect_index, "header length")?;
    let version = read_u16(driver, offset + 22, effect_index, "version")?;
    let version_checksum = read_u16(driver, offset + 24, effect_index, "version checksum")?;
    if usize::from(header_length) != VOC_HEADER_LENGTH
        || version != VOC_VERSION
        || version_checksum != VOC_VERSION_CHECKSUM
    {
        return Err(format!(
            "effect {effect_index} has unsupported VOC header {header_length}/{version:04x}/{version_checksum:04x}"
        ));
    }

    let block_offset = offset + VOC_HEADER_LENGTH;
    if driver.get(block_offset).copied() != Some(VOC_SOUND_DATA_BLOCK) {
        return Err(format!(
            "effect {effect_index} does not begin with a VOC sound-data block"
        ));
    }
    let block_length = read_u24(driver, block_offset + 1, effect_index)?;
    if block_length != expected_length + 2 {
        return Err(format!(
            "effect {effect_index} contains {} PCM bytes; expected {expected_length}",
            block_length.saturating_sub(2)
        ));
    }

    let payload_offset = block_offset + 4;
    if driver.get(payload_offset).copied() != Some(VOC_TIME_CONSTANT)
        || driver.get(payload_offset + 1).copied() != Some(VOC_PCM_CODEC)
    {
        return Err(format!(
            "effect {effect_index} is not 8,333 Hz unsigned eight-bit VOC PCM"
        ));
    }
    let samples_start = payload_offset + 2;
    let samples_end = samples_start
        .checked_add(expected_length)
        .ok_or_else(|| format!("effect {effect_index} PCM range overflow"))?;
    let samples = driver
        .get(samples_start..samples_end)
        .ok_or_else(|| format!("effect {effect_index} PCM extends past BLASTER.SND"))?;
    if driver.get(samples_end).copied() != Some(VOC_TERMINATOR_BLOCK) {
        return Err(format!("effect {effect_index} VOC has no terminator"));
    }
    Ok(samples)
}

/// Reads one little-endian VOC word with a field-specific corruption error.
fn read_u16(bytes: &[u8], offset: usize, effect_index: usize, field: &str) -> Result<u16, String> {
    // `get` keeps this helper total if another source revision is supported in
    // the future without first extending every fixed record boundary.
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| format!("effect {effect_index} VOC {field} is truncated"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

/// Reads one little-endian 24-bit VOC block length into a native `usize`.
fn read_u24(bytes: &[u8], offset: usize, effect_index: usize) -> Result<usize, String> {
    // VOC uses three-byte block lengths, so reconstruct the value explicitly
    // rather than borrowing a four-byte integer and accidentally crossing data.
    let value = bytes
        .get(offset..offset + 3)
        .ok_or_else(|| format!("effect {effect_index} VOC block length is truncated"))?;
    Ok(usize::from(value[0]) | (usize::from(value[1]) << 8) | (usize::from(value[2]) << 16))
}

/// Converts one original unsigned PCM clip to interleaved device-rate floats.
fn convert_blaster_pcm(source: &[u8], frequency: i32, channels: u8) -> Result<Vec<f32>, String> {
    // Both values originate in the requested and obtained SDL layout, but
    // validate them here so arithmetic cannot silently accept an invalid host.
    let output_frequency = usize::try_from(frequency)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("invalid output frequency {frequency}"))?;
    let output_channels = usize::from(channels);
    if output_channels == 0 {
        return Err("output channel count is zero".to_owned());
    }
    if source.is_empty() {
        return Err("original Sound Blaster PCM is empty".to_owned());
    }

    // One source sample spans three 1/25,000-second units. Round the resulting
    // device-frame count to the nearest frame without floating-point duration
    // drift, then reserve the final interleaved storage exactly once.
    let frame_numerator = source
        .len()
        .checked_mul(output_frequency)
        .and_then(|value| value.checked_mul(BLASTER_SAMPLE_RATE_DENOMINATOR))
        .ok_or_else(|| "Sound Blaster output frame count overflow".to_owned())?;
    let output_frames = frame_numerator
        .checked_add(BLASTER_SAMPLE_RATE_NUMERATOR / 2)
        .ok_or_else(|| "Sound Blaster frame rounding overflow".to_owned())?
        / BLASTER_SAMPLE_RATE_NUMERATOR;
    let output_length = output_frames
        .checked_mul(output_channels)
        .ok_or_else(|| "Sound Blaster interleaved output length overflow".to_owned())?;
    let mut output = Vec::with_capacity(output_length);
    let position_denominator = output_frequency
        .checked_mul(BLASTER_SAMPLE_RATE_DENOMINATOR)
        .ok_or_else(|| "Sound Blaster resampling denominator overflow".to_owned())?;

    for output_frame in 0..output_frames {
        // Linear interpolation retains the original duration and endpoints while
        // avoiding the harsh staircase of merely repeating each 8.3-kHz byte.
        let position_numerator = output_frame
            .checked_mul(BLASTER_SAMPLE_RATE_NUMERATOR)
            .ok_or_else(|| "Sound Blaster resampling position overflow".to_owned())?;
        let source_index = (position_numerator / position_denominator).min(source.len() - 1);
        let next_index = (source_index + 1).min(source.len() - 1);
        let fraction =
            (position_numerator % position_denominator) as f32 / position_denominator as f32;
        let current = normalize_blaster_sample(source[source_index]);
        let next = normalize_blaster_sample(source[next_index]);
        let sample = current + (next - current) * fraction;

        // Original PCM is mono. Replicating the converted value into every
        // obtained channel preserves centered output without another buffer.
        output.extend(std::iter::repeat_n(sample, output_channels));
    }
    Ok(output)
}

/// Maps one unsigned DAC byte to normalized PCM with retained mixing headroom.
fn normalize_blaster_sample(sample: u8) -> f32 {
    // Creative PCM centers silence at 128. Dividing by 128 maps the asymmetric
    // byte domain to [-1.0, 0.9921875] before applying the historical -3.6 dB.
    (f32::from(sample) - 128.0) / 128.0 * BLASTER_EFFECT_GAIN
}

/// Calculates reflected IEEE CRC-32 for exact `BLASTER.SND` identification.
fn crc32(bytes: &[u8]) -> u32 {
    // The complemented accumulator and polynomial match standard CRC-32 while
    // avoiding a dependency for one small startup-only integrity check.
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
    //! Decoder and mixer checks that do not open a host audio device.

    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::{
        AudioCallback, AudioCommand, AudioCommandSender, AudioSpec, BLASTER_DRIVER_CRC32,
        BLASTER_DRIVER_LENGTH, BLASTER_PCM_LENGTHS, COMMAND_QUEUE_CAPACITY, EffectBank, Mixer,
        OUTPUT_CHANNELS, OUTPUT_FREQUENCY, audio_command_channel, crc32, parse_blaster_effects,
    };
    use crate::{assets, game::SoundEffect, opl::OplPlayer};

    /// Builds a mixer with decoded production assets and a production layout.
    fn decoded_mixer() -> (Mixer, AudioCommandSender) {
        // AudioSpec fields are public precisely so callback code can retain the
        // obtained layout; the tests construct the desired layout directly.
        let spec = AudioSpec {
            freq: OUTPUT_FREQUENCY,
            format: crate::platform::audio::AudioFormat::f32_sys(),
            channels: OUTPUT_CHANNELS,
            silence: 0,
            samples: 512,
            size: 4_096,
        };
        let asset_set = assets::load_audio().expect("production audio should load");
        let effects = EffectBank {
            clips: std::array::from_fn(|index| {
                // Distinct non-zero samples make effect selection observable
                // without invoking SDL's process-global converter in parallel.
                vec![(index + 1) as f32 / 16.0; 4_096].into_boxed_slice()
            }),
        };
        let music = OplPlayer::new(asset_set.music.as_ref(), OUTPUT_FREQUENCY as u32)
            .expect("production OPL music should decode");
        let (command_sender, command_receiver) = audio_command_channel();
        (
            Mixer::new(
                spec,
                effects,
                music,
                command_receiver,
                Arc::new(AtomicBool::new(false)),
            ),
            command_sender,
        )
    }

    /// Confirms the exact original driver exposes all seven embedded VOC clips.
    #[test]
    fn production_blaster_driver_decodes_for_the_callback_layout() {
        let asset_set = assets::load_audio().expect("production audio should load");
        let driver = asset_set.effects.as_ref();
        let source_effects = parse_blaster_effects(driver)
            .expect("production Sound Blaster VOC records should decode");
        let effects = EffectBank::decode(driver, OUTPUT_FREQUENCY, OUTPUT_CHANNELS)
            .expect("production Sound Blaster effects should decode");

        // Source identity and lengths prevent a generated replacement from
        // silently becoming the production asset. Converted clips must retain
        // complete centered frames with at least one audible sample.
        assert_eq!(driver.len(), BLASTER_DRIVER_LENGTH);
        assert_eq!(crc32(driver), BLASTER_DRIVER_CRC32);
        assert_eq!(source_effects.map(<[u8]>::len), BLASTER_PCM_LENGTHS);
        for clip in &effects.clips {
            assert!(!clip.is_empty());
            assert!(clip.len().is_multiple_of(usize::from(OUTPUT_CHANNELS)));
            assert!(clip.iter().any(|sample| *sample != 0.0));
        }
    }

    /// Confirms damaged or incomplete driver images fail before VOC indexing.
    #[test]
    fn rejects_modified_and_truncated_blaster_drivers() {
        let asset_set = assets::load_audio().expect("production audio should load");
        let driver = asset_set.effects.as_ref();
        let mut modified = driver.to_vec();
        modified[0x028f] ^= 1;

        // A same-sized mutation reaches the checksum diagnostic, while removing
        // one byte is rejected by the size contract before checksum traversal.
        assert!(
            parse_blaster_effects(&modified)
                .expect_err("modified BLASTER.SND should fail")
                .contains("CRC-32")
        );
        assert!(
            parse_blaster_effects(&driver[..driver.len() - 1])
                .expect_err("truncated BLASTER.SND should fail")
                .contains("39194 bytes")
        );
    }

    /// Confirms requests obey the original global priority comparisons.
    #[test]
    fn effect_priority_rejects_and_interrupts_the_original_pairs() {
        let (mut mixer, _) = decoded_mixer();

        assert!(mixer.play_effect(SoundEffect::Base));
        assert!(!mixer.play_effect(SoundEffect::Base));
        assert!(mixer.play_effect(SoundEffect::Bug));
        assert!(!mixer.play_effect(SoundEffect::Push));
        assert!(mixer.play_effect(SoundEffect::Infotron));
        assert!(mixer.play_effect(SoundEffect::Explosion));
        assert!(!mixer.play_effect(SoundEffect::Explosion));
        assert!(mixer.play_effect(SoundEffect::Exit));
        assert_eq!(mixer.current_priority, 0x0a);
    }

    /// Confirms the callback emits samples and disabling clears current state.
    #[test]
    fn callback_mixes_one_effect_and_muting_discards_it() {
        let (mut mixer, _) = decoded_mixer();
        assert!(mixer.play_effect(SoundEffect::Base));
        let mut output = [0.0; 128];

        // Isolate the effect voice from the independently tested music layer.
        mixer.music_enabled = false;
        mixer.music_playing = false;
        mixer.callback(&mut output);
        assert!(output.iter().any(|sample| *sample != 0.0));
        mixer.effects_enabled = false;
        mixer.effect_voice = None;
        mixer.callback(&mut output);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert!(!mixer.play_effect(SoundEffect::Bug));
    }

    /// Confirms main-thread commands take effect at the next callback boundary.
    #[test]
    fn callback_applies_shared_commands_before_mixing() {
        let (mut mixer, mut sender) = decoded_mixer();
        let mut output = [0.0; 128];

        // Queue an effect exactly as the Escape explosion path does. The main
        // thread only publishes one atomic byte before the callback begins.
        assert!(sender.send(AudioCommand::PlayEffect {
            effect: SoundEffect::Explosion,
        }));
        mixer.music_enabled = false;
        mixer.music_playing = false;
        mixer.callback(&mut output);
        assert!(mixer.effect_voice.is_some());
        assert!(output.iter().any(|sample| *sample != 0.0));

        assert!(sender.send(AudioCommand::SetEffectsEnabled { enabled: false }));
        mixer.callback(&mut output);
        assert!(!mixer.effects_enabled);
        assert!(mixer.effect_voice.is_none());
    }

    /// Reproduces restart followed by Escape without losing the explosion.
    #[test]
    fn restart_then_escape_resets_priority_and_plays_the_explosion() {
        let (mut mixer, mut sender) = decoded_mixer();
        let mut output = [0.0; 1_024];

        // Begin in the terminal state left by an accepted Exit sound. Restart
        // must clear that protected voice before the following Escape explosion.
        assert!(mixer.play_effect(SoundEffect::Exit));
        assert!(!mixer.music_playing);
        assert!(sender.send(AudioCommand::RestartLevel));
        assert!(sender.send(AudioCommand::PlayEffect {
            effect: SoundEffect::Explosion,
        }));

        // Isolate the effect samples while retaining Restart's command behavior.
        mixer.music_enabled = false;
        mixer.callback(&mut output);
        assert_eq!(
            mixer.effect_voice.map(|voice| voice.effect),
            Some(SoundEffect::Explosion)
        );
        assert!(output.iter().any(|sample| *sample != 0.0));
    }

    /// Confirms a stalled callback can never block the main-thread producer.
    #[test]
    fn full_command_ring_rejects_excess_input_immediately() {
        let (_, mut sender) = decoded_mixer();

        // With no callback consuming slots, exactly the fixed capacity succeeds.
        // The next operation reports saturation after its single occupancy read.
        for _ in 0..COMMAND_QUEUE_CAPACITY {
            assert!(sender.send(AudioCommand::RestartLevel));
        }
        assert!(!sender.send(AudioCommand::PlayEffect {
            effect: SoundEffect::Explosion,
        }));
    }

    /// Confirms shutdown quiescence bypasses synthesis and returns only silence.
    #[test]
    fn callback_honors_the_atomic_stop_request() {
        let (mut mixer, _) = decoded_mixer();
        let starting_frame = mixer.music.rendered_frames();
        let mut output = [1.0; 128];

        mixer.stop_requested.store(true, Ordering::Release);
        mixer.callback(&mut output);

        assert!(output.iter().all(|sample| *sample == 0.0));
        assert_eq!(mixer.music.rendered_frames(), starting_frame);
    }

    /// Confirms the OPL soundtrack advances and Exit pauses only its voice.
    #[test]
    fn music_advances_and_an_accepted_exit_pauses_it() {
        let (mut mixer, _) = decoded_mixer();
        let mut output = [0.0; 4_096];
        let starting_frame = mixer.music.rendered_frames();

        // OPL deliberately waits one original 20 ms timer interval before its
        // first note, while this 46 ms buffer is long enough to become audible.
        mixer.callback(&mut output);
        let paused_frame = mixer.music.rendered_frames();
        assert_eq!(paused_frame - starting_frame, 2_048);
        assert!(output.iter().any(|sample| *sample != 0.0));
        assert!(mixer.play_effect(SoundEffect::Exit));
        assert!(!mixer.music_playing);
        mixer.callback(&mut output);
        assert_eq!(mixer.music.rendered_frames(), paused_frame);
    }
}
