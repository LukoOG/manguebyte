//! The polyphonic synth: two detuned oscillators and a sub, a resonant
//! low-pass with its own envelope, and an amplitude envelope.

use core::f64::consts::LN_2;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::{
    Oscillator, ProcessSpec, Processor, StateVariableFilter, SvfMode, Synth, Voice, Waveform,
};
use manguebyte_params::ParamId;

use crate::params::{self, SynthParams};

/// How many notes a synth part plays at once.
pub const SYNTH_VOICES: usize = 8;

/// Level below which an envelope counts as silent: -80 dB.
pub(crate) const SILENT: f64 = 1e-4;

/// The factor per sample that takes a level down by 60 dB in `seconds`.
pub(crate) fn decay_coefficient(seconds: f64, sample_rate: f64) -> f64 {
    let samples = (seconds * sample_rate).max(1.0);
    (-3.0 * core::f64::consts::LN_10 / samples).exp()
}

/// The synth's settings in plain units, as its parameters hold them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SynthSettings {
    wave: Waveform,
    detune_cents: f64,
    sub: f64,
    cutoff: f64,
    resonance: f64,
    env_amount: f64,
    filter_decay: f64,
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
}

fn waveform(index: f64) -> Waveform {
    match index as u32 {
        1 => Waveform::Square,
        2 => Waveform::Triangle,
        3 => Waveform::Sine,
        _ => Waveform::Saw,
    }
}

impl SynthSettings {
    pub(crate) fn from_params(params: &SynthParams) -> Self {
        let mut settings = Self {
            wave: Waveform::Saw,
            detune_cents: 0.0,
            sub: 0.0,
            cutoff: 1_000.0,
            resonance: 0.0,
            env_amount: 0.0,
            filter_decay: 0.1,
            attack: 0.01,
            decay: 0.1,
            sustain: 1.0,
            release: 0.1,
        };
        params.visit(&mut |param| {
            settings.set(param.param_id(), param.plain());
        });
        settings
    }

    /// Takes a new value; `false` if `id` is not a synth parameter.
    pub(crate) fn set(&mut self, id: ParamId, value: f64) -> bool {
        match id {
            params::SYNTH_WAVE => self.wave = waveform(value),
            params::SYNTH_DETUNE => self.detune_cents = value,
            params::SYNTH_SUB => self.sub = value,
            params::SYNTH_CUTOFF => self.cutoff = value,
            params::SYNTH_RESONANCE => self.resonance = value,
            params::SYNTH_ENV_AMOUNT => self.env_amount = value,
            params::SYNTH_FILTER_DECAY => self.filter_decay = value,
            params::SYNTH_ATTACK => self.attack = value,
            params::SYNTH_DECAY => self.decay = value,
            params::SYNTH_SUSTAIN => self.sustain = value,
            params::SYNTH_RELEASE => self.release = value,
            _ => return false,
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Release,
}

/// Linear attack, then exponential decay to the sustain level and
/// exponential release: the shape of an analogue envelope generator.
#[derive(Debug, Clone, Copy)]
struct Envelope {
    stage: Stage,
    level: f64,
    attack_step: f64,
    decay_coefficient: f64,
    sustain: f64,
    release_coefficient: f64,
    /// How many attack steps one frame takes: 1, or more for an accent.
    attack_speed: f64,
}

impl Envelope {
    fn new() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: 1.0,
            decay_coefficient: 0.0,
            sustain: 1.0,
            release_coefficient: 0.0,
            attack_speed: 1.0,
        }
    }

    fn configure(&mut self, settings: &SynthSettings, sample_rate: f64) {
        self.attack_step = 1.0 / (settings.attack * sample_rate).max(1.0);
        self.decay_coefficient = decay_coefficient(settings.decay, sample_rate);
        self.sustain = settings.sustain.clamp(0.0, 1.0);
        self.release_coefficient = decay_coefficient(settings.release, sample_rate);
    }

    fn next(&mut self) -> f64 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_step * self.attack_speed;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coefficient;
                if self.sustain < SILENT && self.level < SILENT {
                    self.silence();
                }
            }
            Stage::Release => {
                self.level *= self.release_coefficient;
                if self.level < SILENT {
                    self.silence();
                }
            }
        }
        self.level
    }

    fn silence(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }
}

/// How long a voice that is taken over for a new note takes to fade out:
/// a straight fade over `round(STEAL_FADE_SECONDS × sample rate)` frames,
/// heard alongside the new note, so that the old note does not stop with a
/// click.
pub const STEAL_FADE_SECONDS: f64 = 0.002;

/// How long a voice takes to move to a new velocity's level when its key
/// is struck again while it still sounds: a straight glide over
/// `round(RETRIGGER_GLIDE_SECONDS × sample rate)` frames.
pub const RETRIGGER_GLIDE_SECONDS: f64 = 0.005;

/// Scales the sum of the oscillators so that one note at full velocity
/// peaks around -9 dB, leaving room for chords and other parts.
const VOICE_LEVEL: f64 = 0.2;

/// How much louder an accented synth note is, in decibels: its level is
/// multiplied by `10^(SYNTH_ACCENT_DB / 20)`, about 1.5.
pub const SYNTH_ACCENT_DB: f64 = 3.5;

/// How much further an accented synth note's filter envelope opens the
/// filter, in octaves, on top of the `synth.env_amount` setting. The extra
/// opening falls away with the filter envelope, so the accent brightens
/// the start of the note, whatever the envelope amount is set to.
pub const SYNTH_ACCENT_OCTAVES: f64 = 1.0;

/// The factor on an accented synth note's attack time: it rises to full
/// level in half the time the `synth.attack` setting gives.
pub const SYNTH_ACCENT_ATTACK: f64 = 0.5;

fn resonance_to_q(resonance: f64) -> f64 {
    0.6 * 18f64.powf(resonance.clamp(0.0, 1.0))
}

fn frames_for(seconds: f64, sample_rate: f64) -> u32 {
    (seconds * sample_rate).round().max(1.0) as u32
}

/// A level moving in a straight line to a target over a set number of
/// frames.
#[derive(Debug, Clone, Copy)]
struct Ramp {
    value: f64,
    target: f64,
    step: f64,
    remaining: u32,
}

impl Ramp {
    const fn at(value: f64) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    fn start(&mut self, target: f64, frames: u32) {
        self.target = target;
        self.remaining = frames.max(1);
        self.step = (target - self.value) / f64::from(self.remaining);
    }

    fn next(&mut self) -> f64 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.value = if self.remaining == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }
}

/// Everything that sounds in one note: oscillators, filter, envelopes.
#[derive(Debug, Clone)]
struct Tone {
    oscillators: [Oscillator; 2],
    sub: Oscillator,
    filter: StateVariableFilter<f64>,
    /// The cutoff the filter is gliding towards the setting from.
    cutoff: f64,
    amplitude: Envelope,
    filter_envelope: f64,
    key: u8,
    velocity: f64,
    /// [`SYNTH_ACCENT_OCTAVES`] for an accented note, 0 for another.
    accent_octaves: f64,
    /// `VOICE_LEVEL × velocity`, and the accent's gain, gliding when the
    /// key is struck again.
    gain: Ramp,
    /// The gain is falling to silence for a cancel: the tone ends when it
    /// gets there.
    fading: bool,
}

impl Tone {
    fn new(settings: &SynthSettings) -> Self {
        Self {
            oscillators: [
                Oscillator::new(settings.wave, 440.0),
                Oscillator::new(settings.wave, 440.0),
            ],
            sub: Oscillator::new(Waveform::Square, 220.0),
            filter: StateVariableFilter::new(
                SvfMode::Lowpass,
                settings.cutoff,
                resonance_to_q(settings.resonance),
            ),
            cutoff: settings.cutoff,
            amplitude: Envelope::new(),
            filter_envelope: 0.0,
            key: 60,
            velocity: 0.0,
            accent_octaves: 0.0,
            gain: Ramp::at(0.0),
            fading: false,
        }
    }

    fn prepare(&mut self, spec: &ProcessSpec) {
        for oscillator in &mut self.oscillators {
            oscillator.set_sample_rate(spec.sample_rate);
        }
        self.sub.set_sample_rate(spec.sample_rate);
        Processor::<f64>::prepare(
            &mut self.filter,
            &ProcessSpec {
                num_channels: 1,
                ..*spec
            },
        );
    }

    fn configure(&mut self, settings: &SynthSettings, sample_rate: f64) {
        self.amplitude.configure(settings, sample_rate);
        self.filter.set_q(resonance_to_q(settings.resonance));
        for oscillator in &mut self.oscillators {
            oscillator.set_waveform(settings.wave);
        }
        self.tune(settings);
    }

    fn tune(&mut self, settings: &SynthSettings) {
        let frequency = 440.0 * ((f64::from(self.key) - 69.0) / 12.0 * LN_2).exp();
        let spread = (settings.detune_cents / 2_400.0 * LN_2).exp();
        self.oscillators[0].set_frequency(frequency / spread);
        self.oscillators[1].set_frequency(frequency * spread);
        self.sub.set_frequency(frequency * 0.5);
    }

    fn sounding(&self) -> bool {
        self.amplitude.stage != Stage::Idle
    }

    /// Back to the state of a new tone.
    fn reset(&mut self, settings: &SynthSettings) {
        self.amplitude.silence();
        self.amplitude.attack_speed = 1.0;
        self.filter_envelope = 0.0;
        self.accent_octaves = 0.0;
        for oscillator in &mut self.oscillators {
            oscillator.reset_phase();
        }
        self.sub.reset_phase();
        Processor::<f64>::reset(&mut self.filter);
        self.cutoff = settings.cutoff;
        self.gain = Ramp::at(0.0);
        self.fading = false;
    }

    /// One sample of the tone.
    fn next(&mut self, settings: &SynthSettings, coefficients: &Coefficients) -> f64 {
        let tracking = (f64::from(self.key) - 60.0) / 24.0;
        let depth = settings.env_amount * (0.4 + 0.6 * self.velocity) + self.accent_octaves;
        self.cutoff += (settings.cutoff - self.cutoff) * coefficients.cutoff;
        let octaves = depth * self.filter_envelope + tracking;
        self.filter_envelope *= coefficients.filter;
        let frequency =
            (self.cutoff * (octaves * LN_2).exp()).clamp(20.0, coefficients.nyquist_guard);
        self.filter.set_frequency(frequency);

        let tone = 0.5 * (self.oscillators[0].next_sample() + self.oscillators[1].next_sample())
            + settings.sub * self.sub.next_sample();
        let filtered = self.filter.process_sample(0, tone);
        let level = self.amplitude.next();
        let value = filtered * level * self.gain.next();
        if self.fading && self.gain.remaining == 0 {
            self.amplitude.silence();
            self.fading = false;
        }
        value
    }
}

/// Per-sample factors every tone of a voice shares.
#[derive(Debug, Clone, Copy)]
struct Coefficients {
    /// The cutoff glide.
    cutoff: f64,
    /// The filter envelope's decay.
    filter: f64,
    /// The highest cutoff the filter is given.
    nyquist_guard: f64,
}

/// One note of the synth.
///
/// A voice holds two tones: the note it plays, and the note it played
/// before it was taken over for a new one, which fades out over
/// [`STEAL_FADE_SECONDS`] alongside the new note.
#[derive(Debug, Clone)]
pub(crate) struct SynthVoice {
    settings: SynthSettings,
    sample_rate: f64,
    coefficients: Coefficients,
    tone: Tone,
    /// The note that was taken over, fading out.
    tail: Tone,
    tail_fade: Ramp,
    steal_frames: u32,
    glide_frames: u32,
    /// Whether the next note this voice is given is accented. The part
    /// sets it on every voice before each note on, since only the synth
    /// knows which voice will take the note.
    next_accent: bool,
}

impl SynthVoice {
    pub(crate) fn new(settings: SynthSettings) -> Self {
        let mut voice = Self {
            settings,
            sample_rate: 48_000.0,
            coefficients: Coefficients {
                cutoff: 1.0,
                filter: 0.0,
                nyquist_guard: 48_000.0 * 0.45,
            },
            tone: Tone::new(&settings),
            tail: Tone::new(&settings),
            tail_fade: Ramp::at(0.0),
            steal_frames: 1,
            glide_frames: 1,
            next_accent: false,
        };
        voice.configure();
        voice
    }

    fn configure(&mut self) {
        let rate = self.sample_rate;
        self.coefficients = Coefficients {
            // A one-pole glide that settles within 0.1% in 10 ms, the
            // smoothing the cutoff parameter declares.
            cutoff: 1.0 - decay_coefficient(0.010, rate),
            filter: decay_coefficient(self.settings.filter_decay, rate),
            nyquist_guard: rate * 0.45,
        };
        self.steal_frames = frames_for(STEAL_FADE_SECONDS, rate);
        self.glide_frames = frames_for(RETRIGGER_GLIDE_SECONDS, rate);
        self.tone.configure(&self.settings, rate);
        self.tail.configure(&self.settings, rate);
    }

    fn tail_sounding(&self) -> bool {
        self.tail_fade.remaining > 0
    }

    /// Stops the fading note of a voice that was taken over, at once. A
    /// choke of the whole part uses it, since a choke stops every note
    /// without a fade.
    pub(crate) fn set_next_accent(&mut self, accent: bool) {
        self.next_accent = accent;
    }

    pub(crate) fn silence_tail(&mut self) {
        self.tail_fade = Ramp::at(0.0);
        self.tail.reset(&self.settings);
    }

    /// The note fades out in a straight line over `seconds`, from where it
    /// is, and ends; sooner if it is already fading faster. Struck again
    /// meanwhile, it glides back up as a note struck while it sounds does.
    pub(crate) fn fade_out(&mut self, seconds: f64) {
        let frames = frames_for(seconds, self.sample_rate);
        let tone = &mut self.tone;
        if tone.sounding() && (!tone.fading || frames < tone.gain.remaining) {
            tone.gain.start(0.0, frames);
            tone.fading = true;
        }
    }
}

impl Voice for SynthVoice {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.tone.prepare(spec);
        self.tail.prepare(spec);
        self.configure();
    }

    fn note_on(&mut self, note: NoteRef, velocity: f32) {
        let tone = &mut self.tone;
        tone.key = note.key.min(127);
        tone.velocity = f64::from(velocity);
        tone.tune(&self.settings);
        let accent = self.next_accent;
        tone.accent_octaves = if accent { SYNTH_ACCENT_OCTAVES } else { 0.0 };
        tone.amplitude.attack_speed = if accent {
            1.0 / SYNTH_ACCENT_ATTACK
        } else {
            1.0
        };
        let accent_gain = if accent {
            (SYNTH_ACCENT_DB / 20.0 * core::f64::consts::LN_10).exp()
        } else {
            1.0
        };
        let gain = VOICE_LEVEL * tone.velocity * accent_gain;
        if tone.amplitude.level > 0.0 {
            // Struck again while it sounds: glide to the new level rather
            // than jump to it.
            tone.gain.start(gain, self.glide_frames);
        } else {
            tone.gain = Ramp::at(gain);
        }
        tone.amplitude.stage = Stage::Attack;
        tone.filter_envelope = 1.0;
        tone.fading = false;
    }

    fn note_off(&mut self, _velocity: f32) {
        if self.tone.amplitude.stage != Stage::Idle {
            self.tone.amplitude.stage = Stage::Release;
        }
    }

    /// The synth calls this before every new note. A voice that is still
    /// sounding is being taken over: its note moves to the tail and fades
    /// out over [`STEAL_FADE_SECONDS`] while the new note starts from a
    /// clean tone. A silent voice goes back to the state it had when new,
    /// tail and all.
    fn choke(&mut self) {
        if self.tone.sounding() {
            core::mem::swap(&mut self.tone, &mut self.tail);
            self.tail_fade = Ramp::at(1.0);
            self.tail_fade.start(0.0, self.steal_frames);
        } else {
            self.silence_tail();
        }
        self.tone.reset(&self.settings);
    }

    fn is_active(&self) -> bool {
        self.tone.sounding() || self.tail_sounding()
    }

    fn render(&mut self, buffer: &mut AudioBuffer) {
        if !self.is_active() || buffer.num_channels() == 0 {
            return;
        }
        let settings = self.settings;
        let coefficients = self.coefficients;
        for sample in buffer.channel_mut(0) {
            if !self.is_active() {
                break;
            }
            let mut value = 0.0;
            if self.tone.sounding() {
                value += self.tone.next(&settings, &coefficients);
            }
            if self.tail_sounding() {
                let fade = self.tail_fade.value;
                value += self.tail.next(&settings, &coefficients) * fade;
                self.tail_fade.next();
                if !self.tail.sounding() {
                    self.tail_fade = Ramp::at(0.0);
                }
            }
            *sample += value as f32;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f64) {
        if self.settings.set(param, value) {
            self.configure();
        }
    }
}

/// A synth part's instrument.
pub(crate) type SynthInstrument = Synth<SynthVoice>;

/// Starts a note, accented or not.
pub(crate) fn synth_note_on(
    synth: &mut SynthInstrument,
    note: NoteRef,
    velocity: f32,
    accent: bool,
) {
    for voice in synth.voices_mut() {
        voice.set_next_accent(accent);
    }
    synth.note_on(note, velocity);
}

pub(crate) fn new_synth(params: &SynthParams, spec: &ProcessSpec) -> SynthInstrument {
    let settings = SynthSettings::from_params(params);
    let mut synth = Synth::with_voices(SYNTH_VOICES, || SynthVoice::new(settings));
    synth.prepare(spec);
    synth
}

/// Every note of a synth is released and fades out in a straight line
/// over `seconds`, from where it is: a cancel.
pub(crate) fn fade_synth(synth: &mut SynthInstrument, seconds: f64) {
    synth.all_notes_off();
    for voice in synth.voices_mut() {
        voice.fade_out(seconds);
    }
}

/// Every note of a synth stops at once, with no fade: a choke.
pub(crate) fn choke_synth(synth: &mut SynthInstrument) {
    synth.reset();
    for voice in synth.voices_mut() {
        voice.silence_tail();
    }
}
