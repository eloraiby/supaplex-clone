//! SDL audio-device ownership and original Sound Blaster effect playback.
//!
//! The DOS `.snd` files bundled in `data/` contain executable driver code, so
//! they cannot be queued as PCM. This module embeds WAV renders of their seven
//! gameplay effects, converts them to the exact opened-device format once, and
//! mixes the selected effect in SDL's real-time callback.

use sdl2::{
    AudioSubsystem,
    audio::{
        AudioCVT, AudioCallback, AudioDevice, AudioFormat, AudioSpec, AudioSpecDesired,
        AudioSpecWAV,
    },
    rwops::RWops,
};

use crate::game::SoundEffect;

/// Requested device frequency used by every decoded embedded clip.
const OUTPUT_FREQUENCY: i32 = 44_100;

/// Stereo output matches the modern SDL renderer and leaves room for music.
const OUTPUT_CHANNELS: u8 = 2;

/// Small callback buffers keep short Base and Bug effects responsive to input.
const OUTPUT_BUFFER_SAMPLES: u16 = 512;

/// Original effect-priority time is measured in 20-millisecond units.
const PRIORITY_UNIT_MILLISECONDS: usize = 20;

/// Sound Blaster renders of the seven effects in [`SoundEffect`] index order.
const EMBEDDED_EFFECTS: [&[u8]; 7] = [
    include_bytes!("../assets/audio/explosion.wav"),
    include_bytes!("../assets/audio/infotron.wav"),
    include_bytes!("../assets/audio/push.wav"),
    include_bytes!("../assets/audio/fall.wav"),
    include_bytes!("../assets/audio/bug.wav"),
    include_bytes!("../assets/audio/base.wav"),
    include_bytes!("../assets/audio/exit.wav"),
];

/// Owns the live SDL playback device and its callback mixer.
pub struct AudioPlayer {
    /// SDL serializes callback access while this handle mutates mixer state.
    device: AudioDevice<Mixer>,
}

impl AudioPlayer {
    /// Opens the default 44.1-kHz stereo device and starts effect playback.
    pub fn new(subsystem: &AudioSubsystem) -> Result<Self, String> {
        // Decode before opening the device so malformed embedded data produces
        // a normal initialization error rather than panicking inside SDL's
        // callback-construction closure.
        let effects = EffectBank::decode(OUTPUT_FREQUENCY, OUTPUT_CHANNELS)?;
        let desired = AudioSpecDesired {
            freq: Some(OUTPUT_FREQUENCY),
            channels: Some(OUTPUT_CHANNELS),
            samples: Some(OUTPUT_BUFFER_SAMPLES),
        };
        let device = subsystem
            .open_playback(None, &desired, move |spec| Mixer::new(spec, effects))
            .map_err(|error| format!("open playback device: {error}"))?;

        // SDL devices begin paused so construction cannot race the caller.
        // Resume only after the fully initialized handle is ready to return.
        device.resume();
        Ok(Self { device })
    }

    /// Offers one simulation-selected effect to the original priority gate.
    pub fn play(&mut self, effect: SoundEffect) {
        // Locking is brief: clips were decoded at startup, so this only swaps a
        // small cursor and policy state while SDL pauses its callback thread.
        self.device.lock().play_effect(effect);
    }

    /// Toggles gameplay effects and returns their new enabled state.
    pub fn toggle_effects(&mut self) -> bool {
        // Disabling immediately clears the active voice and its priority. A
        // later re-enable therefore cannot resume half of an obsolete effect.
        let mut mixer = self.device.lock();
        mixer.effects_enabled = !mixer.effects_enabled;
        if !mixer.effects_enabled {
            mixer.effect_voice = None;
            mixer.current_priority = 0;
            mixer.priority_frames_remaining = 0;
        }
        mixer.effects_enabled
    }
}

/// Device-format sample arrays indexed by [`SoundEffect::index`].
struct EffectBank {
    /// Interleaved stereo floating-point samples for every original effect.
    clips: [Box<[f32]>; 7],
}

impl EffectBank {
    /// Decodes every embedded WAV into one uniform callback format.
    fn decode(frequency: i32, channels: u8) -> Result<Self, String> {
        // Preserve enum order explicitly so adding an effect cannot silently
        // associate one trigger with a different clip.
        let mut clips = Vec::with_capacity(EMBEDDED_EFFECTS.len());
        for (index, encoded) in EMBEDDED_EFFECTS.into_iter().enumerate() {
            clips.push(
                decode_wav(encoded, frequency, channels)
                    .map_err(|error| format!("decode embedded effect {index}: {error}"))?
                    .into_boxed_slice(),
            );
        }
        let clips = clips
            .try_into()
            .map_err(|_| "embedded effect count does not match SoundEffect".to_owned())?;
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
    /// Current single-channel effect, replaced when a request is accepted.
    effect_voice: Option<EffectVoice>,
    /// Original global priority value, where zero means no protected sound.
    current_priority: u8,
    /// Device frames before [`Self::current_priority`] resets to zero.
    priority_frames_remaining: usize,
    /// User-controlled effect switch toggled by the S key.
    effects_enabled: bool,
}

impl Mixer {
    /// Creates silent callback state around already converted effect samples.
    fn new(spec: AudioSpec, effects: EffectBank) -> Self {
        // The audio device owns this value for its full lifetime; no callback
        // path allocates, decodes, locks a Rust mutex, or touches simulation.
        Self {
            spec,
            effects,
            effect_voice: None,
            current_priority: 0,
            priority_frames_remaining: 0,
            effects_enabled: true,
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
        true
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

    /// Produces one silent-or-effect buffer whenever SDL requests more audio.
    fn callback(&mut self, output: &mut [Self::Channel]) {
        // SDL may recycle buffers, so every callback must establish silence
        // before selectively adding active voices.
        output.fill(0.0);
        self.mix_effect(output);
        self.advance_priority(output.len());
    }
}

impl SoundEffect {
    /// Maps semantic variants to the stable embedded-asset array order.
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

    use super::{AudioCallback, AudioSpec, EffectBank, Mixer, OUTPUT_CHANNELS, OUTPUT_FREQUENCY};
    use crate::game::SoundEffect;

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
        let effects = EffectBank::decode(spec.freq, spec.channels)
            .expect("embedded Sound Blaster effects should decode");
        Mixer::new(spec, effects)
    }

    /// Confirms each production WAV becomes non-empty interleaved stereo data.
    #[test]
    fn embedded_effects_decode_for_the_callback_layout() {
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

        mixer.callback(&mut output);
        assert!(output.iter().any(|sample| *sample != 0.0));
        mixer.effects_enabled = false;
        mixer.effect_voice = None;
        mixer.callback(&mut output);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert!(!mixer.play_effect(SoundEffect::Bug));
    }
}
