//! SDL audio-device ownership, original AdLib music, and Sound Blaster effects.
//!
//! This module loads the seven playback-ready WAV effects from the configured
//! asset source, converts those short clips to the opened-device format, and
//! mixes them with native OPL2 synthesis in SDL's real-time callback.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sdl2::{
    AudioSubsystem,
    audio::{
        AudioCVT, AudioCallback, AudioDevice, AudioFormat, AudioSpec, AudioSpecDesired,
        AudioSpecWAV,
    },
    rwops::RWops,
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

/// Bounded grace period in which the callback observes a shutdown request.
///
/// Two 512-frame buffers at 44.1 kHz take less than 24 milliseconds. Waiting
/// 25 milliseconds therefore lets an in-flight mix finish and the following
/// callback return silence before SDL synchronously pauses the device.
const SHUTDOWN_QUIESCE_TIME: Duration = Duration::from_millis(25);

/// Owns the live SDL playback device and its callback mixer.
pub struct AudioPlayer {
    /// Live SDL callback device, mutated only during final shutdown.
    device: AudioDevice<Mixer>,
    /// Short commands transferred without taking SDL's device-wide audio lock.
    pending_commands: Arc<Mutex<Vec<AudioCommand>>>,
    /// Lock-free request that makes subsequent callbacks return immediate silence.
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
        let effect_bytes = std::array::from_fn(|index| asset_set.effects[index].as_ref());
        let effects = EffectBank::decode(effect_bytes, OUTPUT_FREQUENCY, OUTPUT_CHANNELS)?;
        let music = OplPlayer::new(asset_set.music.as_ref(), OUTPUT_FREQUENCY as u32)
            .map_err(|error| format!("decode production OPL music: {error}"))?;
        let pending_commands = Arc::new(Mutex::new(Vec::with_capacity(16)));
        let mixer_commands = Arc::clone(&pending_commands);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let mixer_stop_requested = Arc::clone(&stop_requested);
        let desired = AudioSpecDesired {
            freq: Some(OUTPUT_FREQUENCY),
            channels: Some(OUTPUT_CHANNELS),
            samples: Some(OUTPUT_BUFFER_SAMPLES),
        };
        let device = subsystem
            .open_playback(None, &desired, move |spec| {
                Mixer::new(spec, effects, music, mixer_commands, mixer_stop_requested)
            })
            .map_err(|error| format!("open playback device: {error}"))?;

        // SDL devices begin paused so construction cannot race the caller.
        // Resume only after the fully initialized handle is ready to return.
        device.resume();
        Ok(Self {
            device,
            pending_commands,
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

    /// Stops the callback thread and closes its SDL device in a defined order.
    pub fn shutdown(self) {
        // Do not take SDL's callback lock while exiting: a backend that has
        // stopped servicing audio could otherwise hold the main thread forever.
        // The atomic is visible even when no callback command can be drained.
        self.stop_requested.store(true, Ordering::Release);
        std::thread::sleep(SHUTDOWN_QUIESCE_TIME);

        // The callback is now either idle or producing immediate silence.
        // Pausing preserves the explicit close order required by SDL backends;
        // normal field drop then closes an already-quiescent device.
        self.device.pause();
    }

    /// Appends one main-thread request for the beginning of the next callback.
    fn queue_command(&self, command: AudioCommand) {
        // This mutex protects only a tiny command vector; the callback releases
        // it before OPL rendering, so game input never waits for synthesis or
        // for SDL's potentially backend-dependent device lock.
        let mut pending = self
            .pending_commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.push(command);
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

/// Device-format sample arrays indexed by [`SoundEffect::index`].
struct EffectBank {
    /// Interleaved stereo floating-point samples for every original effect.
    clips: [Box<[f32]>; 7],
}

impl EffectBank {
    /// Decodes every production WAV into one uniform callback format.
    fn decode(encoded_effects: [&[u8]; 7], frequency: i32, channels: u8) -> Result<Self, String> {
        // Preserve enum order explicitly so adding an effect cannot silently
        // associate one trigger with a different clip.
        let mut clips = Vec::with_capacity(encoded_effects.len());
        for (index, encoded) in encoded_effects.into_iter().enumerate() {
            clips.push(
                decode_wav(encoded, frequency, channels)
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
    /// Ordered main-thread operations waiting for a callback boundary.
    pending_commands: Arc<Mutex<Vec<AudioCommand>>>,
    /// Lock-free terminal request checked before any synthesis work.
    stop_requested: Arc<AtomicBool>,
}

impl Mixer {
    /// Creates silent callback state around already converted effect samples.
    fn new(
        spec: AudioSpec,
        effects: EffectBank,
        music: OplPlayer,
        pending_commands: Arc<Mutex<Vec<AudioCommand>>>,
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
            pending_commands,
            stop_requested,
        }
    }

    /// Applies every main-thread operation queued since the previous callback.
    fn apply_pending_commands(&mut self) {
        // Clone the lightweight Arc so the guard borrows a local owner instead
        // of borrowing `self`; command application can then mutate mixer state.
        // The guard remains held only across small scalar/cursor operations and
        // is released before the OPL player synthesizes a single frame.
        let queue = Arc::clone(&self.pending_commands);
        let mut pending = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for command in pending.drain(..) {
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

/// Decodes one in-memory WAV and converts it to native-endian stereo `f32`.
fn decode_wav(encoded: &[u8], frequency: i32, channels: u8) -> Result<Vec<f32>, String> {
    // SDL_LoadWAV parses headers and expands supported compressed WAV payloads;
    // AudioCVT then normalizes rate, layout, and sample representation once.
    let mut source =
        RWops::from_bytes(encoded).map_err(|error| format!("open memory WAV: {error}"))?;
    let wav = AudioSpecWAV::load_wav_rw(&mut source)
        .map_err(|error| format!("load WAV payload: {error}"))?;
    let converter = AudioCVT::new(
        wav.format,
        wav.channels,
        wav.freq,
        AudioFormat::f32_sys(),
        channels,
        frequency,
    )
    .map_err(|error| format!("build WAV conversion: {error}"))?;
    let converted = converter.convert(wav.buffer().to_vec());
    if !converted.len().is_multiple_of(size_of::<f32>()) {
        return Err("converted WAV has a partial floating-point sample".to_owned());
    }

    // AudioCVT writes native-endian samples because the requested callback
    // channel is native `f32`; explicit chunks avoid alignment assumptions.
    let (sample_bytes, remainder) = converted.as_chunks::<{ size_of::<f32>() }>();
    debug_assert!(remainder.is_empty(), "the length check rejects a remainder");
    let samples = sample_bytes
        .iter()
        .map(|bytes| f32::from_ne_bytes(*bytes))
        .collect::<Vec<_>>();
    if samples.is_empty() {
        return Err("converted WAV contains no samples".to_owned());
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    //! Decoder and mixer checks that do not open a host audio device.

    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use super::{
        AudioCallback, AudioCommand, AudioSpec, EffectBank, Mixer, OUTPUT_CHANNELS,
        OUTPUT_FREQUENCY,
    };
    use crate::{assets, game::SoundEffect, opl::OplPlayer};

    /// Builds a mixer with decoded production assets and a production layout.
    fn decoded_mixer() -> Mixer {
        // AudioSpec fields are public precisely so callback code can retain the
        // obtained layout; the tests construct the desired layout directly.
        let spec = AudioSpec {
            freq: OUTPUT_FREQUENCY,
            format: sdl2::audio::AudioFormat::f32_sys(),
            channels: OUTPUT_CHANNELS,
            silence: 0,
            samples: 512,
            size: 4_096,
        };
        let asset_set = assets::load_audio().expect("production audio should load");
        let effect_bytes = std::array::from_fn(|index| asset_set.effects[index].as_ref());
        let effects = EffectBank::decode(effect_bytes, spec.freq, spec.channels)
            .expect("production Sound Blaster effects should decode");
        let music = OplPlayer::new(asset_set.music.as_ref(), OUTPUT_FREQUENCY as u32)
            .expect("production OPL music should decode");
        Mixer::new(
            spec,
            effects,
            music,
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Confirms each production WAV becomes non-empty interleaved stereo data.
    #[test]
    fn production_effects_decode_for_the_callback_layout() {
        let mixer = decoded_mixer();

        // Every clip must contain whole stereo frames and audible non-zero PCM.
        for clip in &mixer.effects.clips {
            assert!(!clip.is_empty());
            assert!(clip.len().is_multiple_of(usize::from(OUTPUT_CHANNELS)));
            assert!(clip.iter().any(|sample| *sample != 0.0));
        }
    }

    /// Confirms requests obey the original global priority comparisons.
    #[test]
    fn effect_priority_rejects_and_interrupts_the_original_pairs() {
        let mut mixer = decoded_mixer();

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
        let mut mixer = decoded_mixer();
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

    /// Confirms main-thread commands take effect without SDL's callback lock.
    #[test]
    fn callback_applies_shared_commands_before_mixing() {
        let mut mixer = decoded_mixer();
        let queue = Arc::clone(&mixer.pending_commands);
        let mut output = [0.0; 128];

        // Queue an effect exactly as the Escape explosion path does. The main
        // thread releases the small command mutex before the callback begins.
        queue
            .lock()
            .expect("test command queue should not be poisoned")
            .push(AudioCommand::PlayEffect {
                effect: SoundEffect::Explosion,
            });
        mixer.music_enabled = false;
        mixer.music_playing = false;
        mixer.callback(&mut output);
        assert!(mixer.effect_voice.is_some());
        assert!(output.iter().any(|sample| *sample != 0.0));

        queue
            .lock()
            .expect("test command queue should not be poisoned")
            .push(AudioCommand::SetEffectsEnabled { enabled: false });
        mixer.callback(&mut output);
        assert!(!mixer.effects_enabled);
        assert!(mixer.effect_voice.is_none());
    }

    /// Confirms shutdown quiescence bypasses synthesis and returns only silence.
    #[test]
    fn callback_honors_the_lock_free_stop_request() {
        let mut mixer = decoded_mixer();
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
        let mut mixer = decoded_mixer();
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
