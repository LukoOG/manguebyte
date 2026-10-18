//! Parts that play the instrument voices of `manguebyte-dsp`
//! ([`manguebyte_dsp::instruments`]): plucked strings, FM, the tonewheel
//! organ through its rotating speaker, the analogue drum machine, the kick
//! bass and the acid bass; and, in stereo, the drum kits, the polyphonic
//! synthesiser, the mallets, the bowed strings, the winds and the choir.
//!
//! Each is a [`Synth`] of the voice, built and prepared when the part's
//! instrument is set, as the module's documentation recommends: the organ's
//! voices share one [`OrganManual`] ([`OrganVoice::voices`]), the drum
//! machine's one [`ChokeGroup`](manguebyte_dsp::instruments::ChokeGroup)
//! ([`AnalogKitVoice::voices`]), and the two monophonic basses are a synth
//! of one voice with legato on, so that a note struck while the last is
//! held slides to its pitch. Notes reach the voices through
//! [`Synth::note_on_accented`], with the accent of the record's flags.
//!
//! A part keeps two such synths, two generations: notes go to the current
//! one, and a cancel (or an all-notes-off that must fade) hands it over to
//! fade out in a straight line while the other, clean, takes the notes that
//! follow, so that a note started on the cancel's frame after it plays in
//! full. The voices have no fade of their own to call, and this is the one
//! way to fade the notes that sound without fading those that start.

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
#[cfg(doc)]
use manguebyte_dsp::instruments::OrganManual;
use manguebyte_dsp::instruments::poly::VoiceMode;
use manguebyte_dsp::instruments::{
    AcidParams, AcidVoice, AcidWave, Algorithm, AnalogDrum, AnalogKitParams, AnalogKitVoice,
    BowedString, ChoirVoice, DrumKit as Pieces, DrumKitVoice, FM_OPERATORS, FmParams, FmVoice,
    KickBassParams, KickBassVoice, MalletVoice, OrganParams, OrganVoice, Percussion,
    PercussionDecay, PercussionHarmonic, PluckedString, PolySynthVoice, RotaryParams,
    RotarySpeaker, RotarySpeed, StringParams, WindVoice,
};
use manguebyte_dsp::{ProcessSpec, Processor, Synth, Voice};
use manguebyte_midi::MidiMessage;
use manguebyte_params::{Param, ParamId, ParamSmoother};

use crate::drums::{DAMP_SECONDS, Drum, DrumKit};
use crate::engine::InstrumentKind;
use crate::level::{Trim, make_up_db};
use crate::modelled::{
    bowed_settings, choir_settings, drum_kit_settings, kit_key, mallet_settings, poly_settings,
    wind_settings,
};
use crate::params::{
    self, AcidKnobs, AnalogKitKnobs, BowedKnobs, ChoirKnobs, DrumKitKnobs, FmKnobs,
    InstrumentParams, KickBassKnobs, LEVEL_SMOOTHING, MalletKnobs, OrganKnobs, PluckedKnobs,
    PolyKnobs, WindKnobs,
};
use crate::sampled::PANIC_FADE_SECONDS;

/// How many notes a plucked part plays at once: the six strings of a
/// guitar and two more, so that a chord rings on into the next strum.
pub const PLUCKED_VOICES: usize = 8;

/// How many notes an FM part plays at once.
pub const FM_VOICES: usize = 8;

/// How many keys an organ part holds down at once: two hands' worth.
pub const ORGAN_VOICES: usize = 16;

/// How many drums an analogue kit part sounds at once.
pub const ANALOG_KIT_VOICES: usize = 16;

/// How many pieces a drum kit part sounds at once: a kit's worth, each
/// piece ringing on while the others are struck. See the crate's README
/// for what they cost.
pub const DRUM_KIT_VOICES: usize = 12;

/// How many notes a part of the polyphonic synthesiser plays at once with
/// a polyphonic preset; a monophonic or legato preset plays one. See the
/// crate's README for what they cost.
pub const POLY_VOICES: usize = 8;

/// How many notes a mallet part plays at once.
pub const MALLET_VOICES: usize = 12;

/// How many notes a bowed part plays at once: the four strings of a
/// soloist and room for chords to ring into the next, or a section's
/// divisi. See the crate's README for what they cost.
pub const BOWED_VOICES: usize = 6;

/// How many notes a wind part plays at once: one while `wind.legato` is
/// on and the notes overlap; a section's chords with it off.
pub const WIND_VOICES: usize = 4;

/// How many notes a choir part sings at once. See the crate's README for
/// what they cost.
pub const CHOIR_VOICES: usize = 6;

/// How long the rotating speaker of an organ part has to hear silence and
/// put out next to none (below -120 dB) before it is cleared and skipped
/// until the organ plays again. Its delay lines hold about a millisecond;
/// the stretch is far longer, so nothing audible is cleared away.
pub const SPEAKER_SLEEP_SECONDS: f64 = 0.05;

/// What counts as no output from the rotating speaker: -120 dB.
const SPEAKER_QUIET: f32 = 1e-6;

/// How loud a plucked part plays its strings, in decibels, on top of the
/// level each preset gives them, before the preset's make-up gain. The
/// presets' levels were set for phrases (one guitar strummed, the other
/// picked note by note); the make-up gains of
/// [`InstrumentKind::calibration`], on top of this, bring all four to the
/// loudness target on one phrase each, where the loudest single note,
/// accented at full velocity, peaks at -5.4 dBTP (the steel guitar).
pub const PLUCKED_LEVEL_DB: f64 = -6.0;

/// How loud an FM part plays, in decibels, on top of the presets' levels,
/// before the preset's make-up gain ([`InstrumentKind::calibration`]).
pub const FM_LEVEL_DB: f64 = -3.0;

/// How loud the analogue kit's own drums play, in decibels, on top of the
/// machine's level, before its make-up gain
/// ([`InstrumentKind::calibration`]): with both, its snare, accented at
/// full velocity, peaks at -0.6 dBTP, the loudest single note of any kind.
pub const ANALOG_KIT_LEVEL_DB: f64 = 2.0;

/// The gain of `decibels`.
fn gain_of(decibels: f64) -> f32 {
    (decibels / 20.0 * core::f64::consts::LN_10).exp() as f32
}

fn frames_for(seconds: f64, sample_rate: f64) -> u32 {
    (seconds * sample_rate).round().max(1.0) as u32
}

/// The plucked string preset with this index: nylon guitar, steel guitar,
/// finger bass, slap bass.
pub(crate) fn string_preset(index: usize) -> StringParams {
    match index {
        1 => StringParams::steel_guitar(),
        2 => StringParams::finger_bass(),
        3 => StringParams::slap_bass(),
        _ => StringParams::nylon_guitar(),
    }
}

/// The FM preset with this index: electric piano, tubular bell, FM bass.
pub(crate) fn fm_preset(index: usize) -> FmParams {
    match index {
        1 => FmParams::tubular_bell(),
        2 => FmParams::fm_bass(),
        _ => FmParams::electric_piano(),
    }
}

/// The organ preset with this index: jazz, gospel, full, ballad.
pub(crate) fn organ_preset(index: usize) -> OrganParams {
    match index {
        1 => OrganParams::gospel(),
        2 => OrganParams::full(),
        3 => OrganParams::ballad(),
        _ => OrganParams::jazz(),
    }
}

/// The acid preset with this index: classic, squelch, square.
pub(crate) fn acid_preset(index: usize) -> AcidParams {
    match index {
        1 => AcidParams::squelch(),
        2 => AcidParams::square(),
        _ => AcidParams::classic(),
    }
}

/// The kick bass's one setting, the classic.
pub(crate) fn kick_bass_classic() -> KickBassParams {
    KickBassParams::default()
}

/// The percussion settings of `organ.percussion`, in the order of its
/// values. Normal volume: no preset uses the soft one.
const PERCUSSIONS: [Option<Percussion>; 5] = [
    None,
    Some(Percussion {
        harmonic: PercussionHarmonic::Second,
        decay: PercussionDecay::Fast,
        soft: false,
    }),
    Some(Percussion {
        harmonic: PercussionHarmonic::Second,
        decay: PercussionDecay::Slow,
        soft: false,
    }),
    Some(Percussion {
        harmonic: PercussionHarmonic::Third,
        decay: PercussionDecay::Fast,
        soft: false,
    }),
    Some(Percussion {
        harmonic: PercussionHarmonic::Third,
        decay: PercussionDecay::Slow,
        soft: false,
    }),
];

/// The value of `organ.percussion` for a percussion setting.
pub(crate) fn percussion_index(percussion: Option<Percussion>) -> usize {
    let normal = percussion.map(|p| Percussion { soft: false, ..p });
    PERCUSSIONS
        .iter()
        .position(|&choice| choice == normal)
        .unwrap_or(0)
}

/// The value of `acid.wave` for a wave.
pub(crate) fn acid_wave_index(wave: AcidWave) -> usize {
    match wave {
        AcidWave::Saw => 0,
        AcidWave::Square => 1,
    }
}

/// Which operators of `algorithm` modulate another, from operator 1: the
/// ones whose levels are modulation indices.
fn modulators(algorithm: Algorithm) -> [bool; FM_OPERATORS] {
    match algorithm {
        Algorithm::Stack | Algorithm::ThreeToOne => [false, true, true, true],
        Algorithm::TwoPairs => [false, true, false, true],
        Algorithm::StackAndSine => [false, false, true, true],
        Algorithm::OneToThree => [false, false, false, true],
        Algorithm::Additive => [false; FM_OPERATORS],
    }
}

/// The plucked string's settings: the preset, with the knobs it covers.
fn string_settings(knobs: &PluckedKnobs) -> StringParams {
    StringParams {
        decay: knobs.decay.plain(),
        pluck_brightness: knobs.brightness.plain(),
        pluck_position: knobs.position.plain(),
        tone: knobs.tone.plain(),
        release: knobs.release.plain(),
        ..string_preset(knobs.preset.index())
    }
}

/// The FM voice's settings: the preset, its modulators' indices and its
/// feedback times `fm.brightness`, its decays and releases times
/// `fm.decay` and `fm.release`.
fn fm_settings(knobs: &FmKnobs) -> FmParams {
    let mut params = fm_preset(knobs.preset.index());
    let brightness = knobs.brightness.plain();
    let (decay, release) = (knobs.decay.plain(), knobs.release.plain());
    let modulating = modulators(params.algorithm);
    for (operator, &modulator) in params.operators.iter_mut().zip(&modulating) {
        if modulator {
            operator.level *= brightness;
        }
        operator.feedback *= brightness;
        operator.decay *= decay;
        operator.release *= release;
    }
    params
}

/// The organ's settings: the preset's level and accent, with the knobs.
fn organ_settings(knobs: &OrganKnobs) -> OrganParams {
    OrganParams {
        drawbars: knobs.drawbars.each_ref().map(|drawbar| drawbar.plain()),
        percussion: PERCUSSIONS[knobs.percussion.index().min(PERCUSSIONS.len() - 1)],
        click: knobs.click.plain(),
        leakage: knobs.leakage.plain(),
        velocity: knobs.velocity.plain(),
        ..organ_preset(knobs.preset.index())
    }
}

/// The drum of the analogue kit that plays a drum of the drum map, if it
/// has one. The machine has no ride (its one cymbal is the crash) and no
/// guitar: those two come from the synthesized kit.
pub(crate) fn analog_drum(drum: Drum) -> Option<AnalogDrum> {
    Some(match drum {
        Drum::Kick => AnalogDrum::Kick,
        Drum::Snare => AnalogDrum::Snare,
        Drum::ClosedHat => AnalogDrum::ClosedHat,
        Drum::OpenHat => AnalogDrum::OpenHat,
        Drum::Crash => AnalogDrum::Cymbal,
        Drum::HighTom => AnalogDrum::HighTom,
        Drum::MidTom => AnalogDrum::MidTom,
        Drum::FloorTom => AnalogDrum::LowTom,
        Drum::SideStick => AnalogDrum::Rimshot,
        Drum::Ride | Drum::GuitarTap => return None,
    })
}

/// Whether the analogue kit's own voices play `key`: a drum of the drum
/// map that the machine has, or any other key its General MIDI map gives
/// a drum (the clap on 39, the cowbell on 56, the congas...).
pub(crate) fn machine_plays(key: u8) -> bool {
    match Drum::from_note(key) {
        Some(drum) => analog_drum(drum).is_some(),
        None => AnalogDrum::from_key(key).is_some(),
    }
}

/// The level of `kick.level` and the other drum levels, 0.8 by default,
/// as the machine's own level of a drum, 1 by default.
fn machine_level(level: f64) -> f64 {
    level / 0.8
}

/// The analogue kit's settings from the kit's knobs and the machine's.
fn analog_settings(knobs: &AnalogKitKnobs) -> AnalogKitParams {
    let mut params = AnalogKitParams::default();
    let (tune, decay) = (knobs.kit.tune.plain(), knobs.kit.decay.plain());
    let (tone, snap) = (knobs.tone.plain(), knobs.snap.plain());
    for drum in AnalogDrum::ALL {
        let settings = params.drum_mut(drum);
        settings.tune = tune;
        settings.decay = decay;
        settings.tone = tone;
        settings.snap = snap;
    }
    params.drum_mut(AnalogDrum::Kick).decay *= knobs.kick_decay.plain();
    for (drum, level) in Drum::ALL.into_iter().zip(&knobs.kit.levels) {
        if let Some(analog) = analog_drum(drum) {
            params.drum_mut(analog).level = machine_level(level.plain());
        }
    }
    params
}

/// The kick bass's settings from its knobs.
fn kick_bass_settings(knobs: &KickBassKnobs) -> KickBassParams {
    KickBassParams {
        tune: knobs.tune.plain(),
        decay: knobs.decay.plain(),
        sweep: knobs.sweep.plain(),
        drive: knobs.drive.plain(),
        glide: knobs.glide.plain(),
        click: knobs.click.plain(),
        ..kick_bass_classic()
    }
}

/// The acid bass's settings: the preset's level and tuning, with the
/// knobs.
fn acid_settings(knobs: &AcidKnobs) -> AcidParams {
    AcidParams {
        wave: if knobs.wave.index() == 1 {
            AcidWave::Square
        } else {
            AcidWave::Saw
        },
        cutoff: knobs.cutoff.plain(),
        resonance: knobs.resonance.plain(),
        env_mod: knobs.env_mod.plain(),
        decay: knobs.decay.plain(),
        accent: knobs.accent.plain(),
        slide: knobs.slide.plain(),
        drive_db: knobs.drive.plain(),
        velocity: knobs.velocity.plain(),
        ..acid_preset(knobs.preset.index())
    }
}

/// Two synths of the same voices: the current one, which notes go to, and
/// one that is either clean or fading out what a cancel took from the
/// current one.
pub(crate) struct Generations<V> {
    synths: [Synth<V>; 2],
    current: usize,
    /// One channel, or two for the voices that place their notes in the
    /// stereo field.
    channels: usize,
    /// Frames of the other synth's fade still to go, and the fade's length.
    fade_left: u32,
    fade_length: u32,
    /// The fading synth's sound, for one quantum.
    scratch: AudioBuffer<f32>,
    sample_rate: f64,
    /// How much louder than the voices the part plays, in decibels, before
    /// the make-up gain of its preset.
    level_db: f64,
    /// The gain on what the voices put out: the level and the make-up.
    trim: Trim,
}

impl<V: Voice<f32>> Generations<V> {
    /// The same, playing `level_db` louder than the voices do, and the
    /// make-up gain `make_up_db` on top.
    fn with_level_db(self, level_db: f64, make_up_db: f64) -> Self {
        let gain = gain_of(level_db + make_up_db);
        Self {
            level_db,
            trim: Trim::new(gain, self.sample_rate),
            ..self
        }
    }

    /// Plays the make-up gain `make_up_db` on top of the level, from the
    /// next frame: a preset's, which comes with the preset's settings, and
    /// so on the frame they reach the voices, whose own level changes at
    /// once with them. The presets being as loud as each other, a note
    /// sounding through the change keeps about its level.
    fn set_make_up_db(&mut self, make_up_db: f64) {
        self.trim.set(gain_of(self.level_db + make_up_db), true);
    }

    /// Two synths of the voices `make` makes, prepared for one channel
    /// under `spec`, with legato on or off. Allocates.
    fn new(make: impl FnMut() -> Vec<V>, legato: bool, spec: &ProcessSpec) -> Self {
        Self::with_channels(1, make, legato, spec)
    }

    /// The same for two channels, left and right. Allocates.
    fn stereo(make: impl FnMut() -> Vec<V>, legato: bool, spec: &ProcessSpec) -> Self {
        Self::with_channels(2, make, legato, spec)
    }

    fn with_channels(
        channels: usize,
        mut make: impl FnMut() -> Vec<V>,
        legato: bool,
        spec: &ProcessSpec,
    ) -> Self {
        let prepared = ProcessSpec {
            num_channels: channels,
            ..*spec
        };
        let mut synth = || {
            let mut synth = Synth::new(make());
            synth.set_legato(legato);
            synth.prepare(&prepared);
            synth
        };
        Self {
            synths: [synth(), synth()],
            current: 0,
            channels,
            fade_left: 0,
            fade_length: 1,
            scratch: AudioBuffer::new(channels, spec.max_block_size),
            sample_rate: spec.sample_rate,
            level_db: 0.0,
            trim: Trim::new(1.0, spec.sample_rate),
        }
    }

    /// Calls `apply` on every voice of both synths.
    fn each_voice(&mut self, mut apply: impl FnMut(&mut V)) {
        for synth in &mut self.synths {
            synth.voices_mut().for_each(&mut apply);
        }
    }

    /// Turns legato playing on or off for the notes that start from now
    /// on. Real-time safe.
    fn set_legato(&mut self, legato: bool) {
        for synth in &mut self.synths {
            synth.set_legato(legato);
        }
    }

    /// Lets the notes that start from now on take only the first `limit`
    /// voices of each synth. Real-time safe.
    fn set_voice_limit(&mut self, limit: usize) {
        for synth in &mut self.synths {
            synth.set_voice_limit(limit);
        }
    }

    fn note_on(&mut self, note: NoteRef, velocity: f32, accent: bool) {
        self.synths[self.current].note_on_accented(note, velocity, accent);
    }

    fn note_off(&mut self, note: NoteRef) {
        self.synths[self.current].note_off(note, 0.0);
    }

    /// Every note of the current synth is released, as its voices release.
    fn release_all(&mut self) {
        self.synths[self.current].all_notes_off();
    }

    /// Back to the state of a new synth: a voice choked while it sounds
    /// keeps its sound aside for a note to take over, and a second choke
    /// clears that too.
    fn clear(synth: &mut Synth<V>) {
        Processor::<f32>::reset(synth);
        Processor::<f32>::reset(synth);
    }

    /// Every note sounding now fades out in a straight line over
    /// `round(seconds × rate)` frames, the first of them the next one
    /// rendered, while the notes that start from now on play in full. A
    /// fade still running from before is cut short: its synth is needed.
    fn fade_out(&mut self, seconds: f64) {
        if self.synths[self.current].active_voices() == 0 {
            return;
        }
        let other = 1 - self.current;
        if self.fade_left > 0 {
            Self::clear(&mut self.synths[other]);
        }
        self.current = other;
        self.fade_length = frames_for(seconds, self.sample_rate);
        self.fade_left = self.fade_length;
    }

    /// Every note stops at once, faded or not.
    fn choke(&mut self) {
        for synth in &mut self.synths {
            Self::clear(synth);
        }
        self.fade_left = 0;
    }

    fn active_voices(&self) -> usize {
        let fading = if self.fade_left > 0 {
            self.synths[1 - self.current].active_voices()
        } else {
            0
        };
        self.synths[self.current].active_voices() + fading
    }

    /// Writes the notes' sound over `out`, which has the synths' channels:
    /// one, or two.
    fn render(&mut self, out: &mut AudioBuffer<f32>) {
        let frames = out.num_frames();
        self.synths[self.current].process(out);
        if self.fade_left > 0 {
            let other = 1 - self.current;
            self.scratch.set_num_frames(frames);
            self.synths[other].process(&mut self.scratch);
            let length = self.fade_length as f32;
            if self.channels == 1 {
                for (out, &fading) in out.channel_mut(0).iter_mut().zip(self.scratch.channel(0)) {
                    if self.fade_left == 0 {
                        break;
                    }
                    self.fade_left -= 1;
                    *out += fading * (self.fade_left as f32 / length);
                }
            } else {
                // Frame by frame as on one channel, both sides alike.
                let count = frames.min(self.fade_left as usize);
                for channel in 0..2 {
                    let mut left = self.fade_left;
                    let fading = self.scratch.channel(channel);
                    for (out, &fading) in out.channel_mut(channel)[..count].iter_mut().zip(fading) {
                        left -= 1;
                        *out += fading * (left as f32 / length);
                    }
                }
                self.fade_left -= count as u32;
            }
            if self.fade_left == 0 {
                Self::clear(&mut self.synths[other]);
            }
        }
        if self.channels == 1 {
            self.trim.apply(out.channel_mut(0));
        } else {
            let (left, right) = out.channel_pair_mut(0, 1);
            self.trim.apply_stereo(left, right);
        }
    }
}

/// The rotating speaker an organ part is heard through, mixed with the
/// organ heard straight by `organ.speaker`, and asleep while the organ is
/// silent.
pub(crate) struct Speaker {
    rotary: RotarySpeaker<f32>,
    mix: ParamSmoother,
    /// The organ heard straight, for the mix.
    dry: AudioBuffer<f32>,
    asleep: bool,
    quiet_frames: u32,
    sleep_frames: u32,
}

impl Speaker {
    fn new(knobs: &OrganKnobs, spec: &ProcessSpec) -> Self {
        let mut rotary = RotarySpeaker::new(RotaryParams::default());
        rotary.set_speed(rotary_speed(knobs.rotary.index()));
        rotary.prepare(&ProcessSpec {
            num_channels: 2,
            ..*spec
        });
        let mut mix = ParamSmoother::new(LEVEL_SMOOTHING);
        mix.prepare(spec.sample_rate, knobs.speaker.value());
        Self {
            rotary,
            mix,
            dry: AudioBuffer::new(2, spec.max_block_size),
            asleep: true,
            quiet_frames: 0,
            sleep_frames: frames_for(SPEAKER_SLEEP_SECONDS, spec.sample_rate),
        }
    }

    fn set_speed(&mut self, index: usize) {
        self.rotary.set_speed(rotary_speed(index));
    }

    fn set_mix(&mut self, mix: f32) {
        if self.asleep {
            self.mix.reset(mix);
        } else {
            self.mix.set_target(mix);
        }
    }

    /// Clears the speaker and puts it to sleep: it starts again from its
    /// rotors' resting places when the organ next sounds.
    fn clear(&mut self) {
        self.asleep = true;
        self.quiet_frames = 0;
        self.mix.reset(self.mix.target());
    }

    fn is_asleep(&self) -> bool {
        self.asleep
    }

    /// Puts `buffer`, the organ on both channels, through the speaker.
    ///
    /// Asleep, it waits for the first frame of sound and wakes there, with
    /// its rotors at their resting places turning at the speed set. Awake,
    /// it falls asleep once it has heard silence and put out next to none
    /// for [`SPEAKER_SLEEP_SECONDS`]; the frame it falls asleep on is
    /// worked out frame by frame, so the output does not depend on how the
    /// frames are cut into quanta.
    fn process(&mut self, buffer: &mut AudioBuffer<f32>) {
        let frames = buffer.num_frames();
        let mut start = 0;
        while start < frames {
            if self.asleep {
                let Some(woken) = (start..frames).find(|&frame| buffer.channel(0)[frame] != 0.0)
                else {
                    return;
                };
                Processor::<f32>::reset(&mut self.rotary);
                self.asleep = false;
                self.quiet_frames = 0;
                start = woken;
            }
            let room = (self.sleep_frames - self.quiet_frames) as usize;
            let end = frames.min(start + room);
            self.dry.set_num_frames(frames);
            for (dry, wet) in self.dry.iter_channels_mut().zip(buffer.iter_channels()) {
                dry[start..end].copy_from_slice(&wet[start..end]);
            }
            buffer.windowed(start..end, |piece| self.rotary.process(piece));
            let (left, right) = buffer.channel_pair_mut(0, 1);
            let (dry_left, dry_right) = (self.dry.channel(0), self.dry.channel(1));
            for frame in start..end {
                let mix = self.mix.next_value();
                let quiet = dry_left[frame] == 0.0
                    && left[frame].abs() < SPEAKER_QUIET
                    && right[frame].abs() < SPEAKER_QUIET;
                self.quiet_frames = if quiet { self.quiet_frames + 1 } else { 0 };
                if mix < 1.0 {
                    left[frame] = dry_left[frame] + (left[frame] - dry_left[frame]) * mix;
                    right[frame] = dry_right[frame] + (right[frame] - dry_right[frame]) * mix;
                }
            }
            if self.quiet_frames >= self.sleep_frames {
                self.clear();
            }
            start = end;
        }
    }
}

fn rotary_speed(index: usize) -> RotarySpeed {
    match index {
        0 => RotarySpeed::Stop,
        2 => RotarySpeed::Fast,
        _ => RotarySpeed::Slow,
    }
}

/// An organ part: the organ's voices, sharing a manual, and the speaker.
pub(crate) struct OrganPart {
    gens: Generations<OrganVoice>,
    speaker: Speaker,
}

/// An analogue kit part: the machine's drums, and the synthesized kit for
/// the drums of the drum map it does not have.
pub(crate) struct AnalogKitPart {
    gens: Generations<AnalogKitVoice>,
    fallback: DrumKit,
    /// The synthesized drums' sound, for one quantum.
    scratch: Vec<f32>,
}

/// A drum kit part: the kit's pieces, in stereo, and the synthesized kit
/// for the drums of the drum map it has no piece for.
pub(crate) struct DrumKitPart {
    gens: Generations<DrumKitVoice>,
    /// The settings the voices have: which keys have a piece.
    pieces: Pieces,
    fallback: DrumKit,
    /// The synthesized drums' sound, for one quantum.
    scratch: Vec<f32>,
}

/// A part of the polyphonic synthesiser: voices for its polyphonic
/// presets and one for its monophonic and legato ones, each set to the
/// preset, the notes going to those its preset's mode plays.
pub(crate) struct PolyPart {
    poly: Generations<PolySynthVoice>,
    mono: Generations<PolySynthVoice>,
    mode: VoiceMode,
    /// The tempo the voices' synced LFOs follow, beats per minute.
    tempo: f64,
    /// What the voices that are not the mode's put out while they ring on
    /// after a change of mode, for one quantum.
    scratch: AudioBuffer<f32>,
}

impl PolyPart {
    /// The voices of the preset's mode.
    fn current(&mut self) -> &mut Generations<PolySynthVoice> {
        match self.mode {
            VoiceMode::Poly => &mut self.poly,
            VoiceMode::Mono | VoiceMode::Legato => &mut self.mono,
        }
    }

    fn both(&mut self) -> [&mut Generations<PolySynthVoice>; 2] {
        [&mut self.poly, &mut self.mono]
    }

    /// Takes the mode of a new preset: the notes of the voices it leaves
    /// are released, as their voices release.
    fn set_mode(&mut self, mode: VoiceMode) {
        let was_poly = self.mode == VoiceMode::Poly;
        if was_poly != (mode == VoiceMode::Poly) {
            self.current().release_all();
        }
        self.mode = mode;
        self.mono.set_legato(mode == VoiceMode::Legato);
    }
}

/// A part's instrument made of `manguebyte-dsp`'s voices.
pub(crate) enum Voiced {
    Plucked(Generations<PluckedString>),
    Fm(Generations<FmVoice>),
    Organ(Box<OrganPart>),
    AnalogKit(Box<AnalogKitPart>),
    KickBass(Generations<KickBassVoice>),
    Acid(Generations<AcidVoice>),
    DrumKit(Box<DrumKitPart>),
    Poly(Box<PolyPart>),
    Mallet(Generations<MalletVoice>),
    Bowed(Generations<BowedString>),
    Wind(Generations<WindVoice>),
    Choir(Generations<ChoirVoice>),
}

/// `$body` with `$gens` bound to the part's [`Generations`], whatever its
/// voice: for the polyphonic synthesiser, those of its preset's mode.
macro_rules! with_gens {
    ($voiced:expr, $gens:ident => $body:expr) => {
        match $voiced {
            Voiced::Plucked($gens) => $body,
            Voiced::Fm($gens) => $body,
            Voiced::Organ(organ) => {
                let $gens = &mut organ.gens;
                $body
            }
            Voiced::AnalogKit(kit) => {
                let $gens = &mut kit.gens;
                $body
            }
            Voiced::KickBass($gens) => $body,
            Voiced::Acid($gens) => $body,
            Voiced::DrumKit(kit) => {
                let $gens = &mut kit.gens;
                $body
            }
            Voiced::Poly(poly) => {
                let $gens = poly.current();
                $body
            }
            Voiced::Mallet($gens) => $body,
            Voiced::Bowed($gens) => $body,
            Voiced::Wind($gens) => $body,
            Voiced::Choir($gens) => $body,
        }
    };
}

impl Voiced {
    // Each instrument at the settings of its knobs, prepared under `spec`.
    // They allocate.

    // Each plays at its level, if it has one, and the make-up gain of its
    // preset (`InstrumentKind::calibration`), which a change of preset
    // moves.

    pub(crate) fn plucked(knobs: &PluckedKnobs, spec: &ProcessSpec) -> Self {
        let settings = string_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Plucked, knobs.preset.index());
        Self::Plucked(
            Generations::new(
                || vec![PluckedString::new(settings); PLUCKED_VOICES],
                false,
                spec,
            )
            .with_level_db(PLUCKED_LEVEL_DB, make_up),
        )
    }

    pub(crate) fn fm(knobs: &FmKnobs, spec: &ProcessSpec) -> Self {
        let settings = fm_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Fm, knobs.preset.index());
        Self::Fm(
            Generations::new(|| vec![FmVoice::new(settings); FM_VOICES], false, spec)
                .with_level_db(FM_LEVEL_DB, make_up),
        )
    }

    pub(crate) fn organ(knobs: &OrganKnobs, spec: &ProcessSpec) -> Self {
        let settings = organ_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Organ, knobs.preset.index());
        Self::Organ(Box::new(OrganPart {
            // Every voice of a generation on one manual of its own, so that
            // the percussion is single-triggered.
            gens: Generations::new(|| OrganVoice::voices(ORGAN_VOICES, settings), false, spec)
                .with_level_db(0.0, make_up),
            speaker: Speaker::new(knobs, spec),
        }))
    }

    pub(crate) fn analog_kit(knobs: &AnalogKitKnobs, spec: &ProcessSpec) -> Self {
        let settings = analog_settings(knobs);
        let make_up = make_up_db(InstrumentKind::AnalogKit, 0);
        Self::AnalogKit(Box::new(AnalogKitPart {
            // One choke group a generation: the closed hat chokes the open
            // one.
            gens: Generations::new(
                || AnalogKitVoice::voices(ANALOG_KIT_VOICES, settings),
                false,
                spec,
            )
            .with_level_db(ANALOG_KIT_LEVEL_DB, make_up),
            // At the drums' own make-up, as in a drum part.
            fallback: DrumKit::new(&knobs.kit, spec.sample_rate),
            scratch: vec![0.0; spec.max_block_size],
        }))
    }

    pub(crate) fn kick_bass(knobs: &KickBassKnobs, spec: &ProcessSpec) -> Self {
        let settings = kick_bass_settings(knobs);
        let make_up = make_up_db(InstrumentKind::KickBass, 0);
        Self::KickBass(
            Generations::new(|| vec![KickBassVoice::new(settings)], true, spec)
                .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn acid(knobs: &AcidKnobs, spec: &ProcessSpec) -> Self {
        let settings = acid_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Acid, knobs.preset.index());
        Self::Acid(
            Generations::new(|| vec![AcidVoice::new(settings)], true, spec)
                .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn drum_kit(knobs: &DrumKitKnobs, spec: &ProcessSpec) -> Self {
        let pieces = drum_kit_settings(knobs);
        let make_up = make_up_db(InstrumentKind::DrumKit, knobs.preset.index());
        Self::DrumKit(Box::new(DrumKitPart {
            // One set of choke groups a generation: the closed hat chokes
            // the open one.
            gens: Generations::stereo(
                || DrumKitVoice::voices(DRUM_KIT_VOICES, pieces),
                false,
                spec,
            )
            .with_level_db(0.0, make_up),
            pieces,
            // At the drums' own make-up, as in a drum part.
            fallback: DrumKit::new(&knobs.kit, spec.sample_rate),
            scratch: vec![0.0; spec.max_block_size],
        }))
    }

    pub(crate) fn poly(knobs: &PolyKnobs, spec: &ProcessSpec) -> Self {
        let settings = poly_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Poly, knobs.preset.index());
        let voices = |count: usize| {
            Generations::stereo(|| vec![PolySynthVoice::new(settings); count], false, spec)
                .with_level_db(0.0, make_up)
        };
        let mut part = PolyPart {
            poly: voices(POLY_VOICES),
            mono: voices(1),
            mode: VoiceMode::Poly,
            tempo: 120.0,
            scratch: AudioBuffer::new(2, spec.max_block_size),
        };
        part.set_mode(settings.mode);
        part.poly.set_voice_limit(knobs.voices.value() as usize);
        Self::Poly(Box::new(part))
    }

    pub(crate) fn mallet(knobs: &MalletKnobs, spec: &ProcessSpec) -> Self {
        let settings = mallet_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Mallet, knobs.preset.index());
        Self::Mallet(
            Generations::stereo(
                || vec![MalletVoice::new(settings); MALLET_VOICES],
                false,
                spec,
            )
            .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn bowed(knobs: &BowedKnobs, spec: &ProcessSpec) -> Self {
        let settings = bowed_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Bowed, knobs.preset.index());
        Self::Bowed(
            Generations::stereo(
                || vec![BowedString::new(settings); BOWED_VOICES],
                knobs.legato.value(),
                spec,
            )
            .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn wind(knobs: &WindKnobs, spec: &ProcessSpec) -> Self {
        let settings = wind_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Wind, knobs.preset.index());
        Self::Wind(
            Generations::stereo(
                || vec![WindVoice::new(settings); WIND_VOICES],
                knobs.legato.value(),
                spec,
            )
            .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn choir(knobs: &ChoirKnobs, spec: &ProcessSpec) -> Self {
        let settings = choir_settings(knobs);
        let make_up = make_up_db(InstrumentKind::Choir, knobs.preset.index());
        Self::Choir(
            Generations::stereo(
                || vec![ChoirVoice::new(settings); CHOIR_VOICES],
                knobs.legato.value(),
                spec,
            )
            .with_level_db(0.0, make_up),
        )
    }

    pub(crate) fn kind(&self) -> InstrumentKind {
        match self {
            Self::Plucked(_) => InstrumentKind::Plucked,
            Self::Fm(_) => InstrumentKind::Fm,
            Self::Organ(_) => InstrumentKind::Organ,
            Self::AnalogKit(_) => InstrumentKind::AnalogKit,
            Self::KickBass(_) => InstrumentKind::KickBass,
            Self::Acid(_) => InstrumentKind::Acid,
            Self::DrumKit(_) => InstrumentKind::DrumKit,
            Self::Poly(_) => InstrumentKind::Poly,
            Self::Mallet(_) => InstrumentKind::Mallet,
            Self::Bowed(_) => InstrumentKind::Bowed,
            Self::Wind(_) => InstrumentKind::Wind,
            Self::Choir(_) => InstrumentKind::Choir,
        }
    }

    /// Whether the part's voices place their notes in the stereo field.
    fn is_stereo(&self) -> bool {
        matches!(
            self,
            Self::DrumKit(_)
                | Self::Poly(_)
                | Self::Mallet(_)
                | Self::Bowed(_)
                | Self::Wind(_)
                | Self::Choir(_)
        )
    }

    /// Takes the new value of one of the instrument's knobs, `id`, which
    /// `knobs` already holds (a preset already moved the knobs it covers),
    /// and passes the settings they make to every voice. Real-time safe:
    /// the voices take whole settings without allocating.
    pub(crate) fn set_param(&mut self, id: ParamId, value: f64, knobs: &InstrumentParams) {
        let kind = self.kind();
        // A preset brings its own make-up gain.
        let preset = kind
            .preset_param()
            .is_some_and(|preset| ParamId::from_name(preset) == id);
        match (self, knobs) {
            (Self::Plucked(gens), InstrumentParams::Plucked(knobs)) => {
                let settings = string_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Fm(gens), InstrumentParams::Fm(knobs)) => {
                let settings = fm_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Organ(organ), InstrumentParams::Organ(knobs)) => match id {
                params::ORGAN_ROTARY => organ.speaker.set_speed(knobs.rotary.index()),
                params::ORGAN_SPEAKER => organ.speaker.set_mix(value as f32),
                _ => {
                    let settings = organ_settings(knobs);
                    organ.gens.each_voice(|voice| voice.set_params(settings));
                    if preset {
                        organ
                            .gens
                            .set_make_up_db(make_up_db(kind, knobs.preset.index()));
                    }
                }
            },
            (Self::AnalogKit(kit), InstrumentParams::AnalogKit(knobs)) => {
                // The kit's tune, decay and levels are the synthesized
                // drums' too.
                kit.fallback.set_param(id, value);
                let settings = analog_settings(knobs);
                kit.gens.each_voice(|voice| voice.set_params(settings));
            }
            (Self::KickBass(gens), InstrumentParams::KickBass(knobs)) => {
                let settings = kick_bass_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
            }
            (Self::Acid(gens), InstrumentParams::Acid(knobs)) => {
                let settings = acid_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::DrumKit(kit), InstrumentParams::DrumKit(knobs)) => {
                // The kit's tune, decay and levels are the synthesized
                // drums' too.
                kit.fallback.set_param(id, value);
                kit.pieces = drum_kit_settings(knobs);
                let pieces = kit.pieces;
                kit.gens.each_voice(|voice| voice.set_params(pieces));
                if preset {
                    kit.gens
                        .set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Poly(poly), InstrumentParams::Poly(knobs)) => {
                if id == params::POLY_MOD_WHEEL {
                    // The wheel the presets' matrices read, as the
                    // controller sends it.
                    let wheel = MidiMessage::ControlChange {
                        channel: 0,
                        controller: 1,
                        value: (value.clamp(0.0, 1.0) * 127.0).round() as u8,
                    };
                    for gens in poly.both() {
                        gens.each_voice(|voice| Voice::<f32>::midi(voice, wheel));
                    }
                    return;
                }
                if id == params::POLY_POLYPHONY {
                    poly.poly.set_voice_limit(knobs.voices.value() as usize);
                    return;
                }
                let settings = poly_settings(knobs);
                for gens in poly.both() {
                    gens.each_voice(|voice| voice.set_params(settings));
                }
                if preset {
                    let make_up = make_up_db(kind, knobs.preset.index());
                    for gens in poly.both() {
                        gens.set_make_up_db(make_up);
                    }
                    poly.set_mode(settings.mode);
                    poly.poly.set_voice_limit(knobs.voices.value() as usize);
                }
            }
            (Self::Mallet(gens), InstrumentParams::Mallet(knobs)) => {
                let settings = mallet_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Bowed(gens), InstrumentParams::Bowed(knobs)) => {
                if id == params::BOWED_LEGATO {
                    gens.set_legato(knobs.legato.value());
                    return;
                }
                let settings = bowed_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Wind(gens), InstrumentParams::Wind(knobs)) => {
                if id == params::WIND_LEGATO {
                    gens.set_legato(knobs.legato.value());
                    return;
                }
                let settings = wind_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            (Self::Choir(gens), InstrumentParams::Choir(knobs)) => {
                if id == params::CHOIR_LEGATO {
                    gens.set_legato(knobs.legato.value());
                    return;
                }
                let settings = choir_settings(knobs);
                gens.each_voice(|voice| voice.set_params(settings));
                if preset {
                    gens.set_make_up_db(make_up_db(kind, knobs.preset.index()));
                }
            }
            _ => {}
        }
    }

    /// Lets the voices whose LFOs can follow the tempo know it, from the
    /// frame being rendered on: `beats_per_frame` at the part's rate.
    fn follow_tempo(&mut self, beats_per_frame: f64) {
        if let Self::Poly(poly) = self {
            let tempo = beats_per_frame * poly.poly.sample_rate * 60.0;
            if tempo != poly.tempo && tempo.is_finite() {
                poly.tempo = tempo;
                for gens in poly.both() {
                    gens.each_voice(|voice| voice.set_tempo(tempo));
                }
            }
        }
    }

    /// Starts a note, accented or not. A drum of the analogue kit is a
    /// one-shot, released as it is struck so that its voice is free again
    /// once it has rung out; a drum the machine does not have is the
    /// synthesized kit's.
    pub(crate) fn note_on(&mut self, channel: u8, key: u8, velocity: f32, accent: bool) {
        let note = NoteRef::new(channel, key);
        match self {
            Self::AnalogKit(kit) => {
                if machine_plays(key) {
                    kit.gens.note_on(note, velocity, accent);
                    kit.gens.note_off(note);
                } else {
                    kit.fallback.note_on(key, velocity, accent);
                }
            }
            Self::DrumKit(kit) => match kit_key(&kit.pieces, key) {
                Some(piece) => {
                    let note = NoteRef::new(channel, piece);
                    kit.gens.note_on(note, velocity, accent);
                    kit.gens.note_off(note);
                }
                // A drum of the drum map the kit has no piece for; any
                // other key it has nothing for is silent.
                None => kit.fallback.note_on(key, velocity, accent),
            },
            voiced => with_gens!(voiced, gens => gens.note_on(note, velocity, accent)),
        }
    }

    /// Releases a note. Drums are one-shots: a note off changes nothing.
    pub(crate) fn note_off(&mut self, channel: u8, key: u8) {
        let note = NoteRef::new(channel, key);
        match self {
            Self::AnalogKit(_) | Self::DrumKit(_) => {}
            voiced => with_gens!(voiced, gens => gens.note_off(note)),
        }
    }

    /// Every note stops at once.
    pub(crate) fn choke(&mut self) {
        match self {
            Self::AnalogKit(kit) => kit.fallback.choke(),
            Self::DrumKit(kit) => kit.fallback.choke(),
            Self::Poly(poly) => {
                poly.mono.choke();
                poly.poly.choke();
                return;
            }
            _ => {}
        }
        with_gens!(self, gens => gens.choke());
    }

    /// Every note is let go of without a click: the strings, the FM voices,
    /// the basses, the synthesiser, the mallets, the bowed strings, the
    /// winds and the choir release as they do; the organ, whose contacts
    /// open with a click, fades out over [`PANIC_FADE_SECONDS`], and the
    /// drum kits, whose drums are one-shots, over [`DAMP_SECONDS`].
    pub(crate) fn all_notes_off(&mut self) {
        match self {
            Self::Organ(organ) => organ.gens.fade_out(PANIC_FADE_SECONDS),
            Self::AnalogKit(kit) => {
                kit.gens.fade_out(DAMP_SECONDS);
                kit.fallback.damp();
            }
            Self::DrumKit(kit) => {
                kit.gens.fade_out(DAMP_SECONDS);
                kit.fallback.damp();
            }
            Self::Poly(poly) => {
                for gens in poly.both() {
                    gens.release_all();
                }
            }
            voiced => with_gens!(voiced, gens => gens.release_all()),
        }
    }

    /// Every note sounding fades out in a straight line over `seconds`,
    /// while the notes that start from now on play in full: a cancel.
    pub(crate) fn fade_out(&mut self, seconds: f64) {
        match self {
            Self::AnalogKit(kit) => kit.fallback.fade_out_all(seconds),
            Self::DrumKit(kit) => kit.fallback.fade_out_all(seconds),
            Self::Poly(poly) => {
                for gens in poly.both() {
                    gens.fade_out(seconds);
                }
                return;
            }
            _ => {}
        }
        with_gens!(self, gens => gens.fade_out(seconds));
    }

    pub(crate) fn active_voices(&self) -> usize {
        match self {
            Self::Plucked(gens) => gens.active_voices(),
            Self::Fm(gens) => gens.active_voices(),
            Self::Organ(organ) => organ.gens.active_voices(),
            Self::AnalogKit(kit) => kit.gens.active_voices() + kit.fallback.active_voices(),
            Self::KickBass(gens) => gens.active_voices(),
            Self::Acid(gens) => gens.active_voices(),
            Self::DrumKit(kit) => kit.gens.active_voices() + kit.fallback.active_voices(),
            Self::Poly(poly) => poly.poly.active_voices() + poly.mono.active_voices(),
            Self::Mallet(gens) => gens.active_voices(),
            Self::Bowed(gens) => gens.active_voices(),
            Self::Wind(gens) => gens.active_voices(),
            Self::Choir(gens) => gens.active_voices(),
        }
    }

    /// Whether an organ part's rotating speaker is asleep; `None` for any
    /// other part.
    pub(crate) fn speaker_asleep(&self) -> Option<bool> {
        match self {
            Self::Organ(organ) => Some(organ.speaker.is_asleep()),
            _ => None,
        }
    }

    /// Writes the part's sound over `stereo`, using `mono`, as long, at
    /// `beats_per_frame` of the master's tempo.
    pub(crate) fn render(
        &mut self,
        mono: &mut AudioBuffer<f32>,
        stereo: &mut AudioBuffer<f32>,
        beats_per_frame: f64,
    ) {
        let frames = mono.num_frames();
        if self.is_stereo() {
            self.follow_tempo(beats_per_frame);
            match &mut *self {
                Self::DrumKit(kit) => {
                    kit.gens.render(stereo);
                    if kit.fallback.active_voices() > 0 {
                        let drums = &mut kit.scratch[..frames];
                        kit.fallback.render(drums);
                        for channel in stereo.iter_channels_mut() {
                            for (sample, &drum) in channel.iter_mut().zip(drums.iter()) {
                                *sample += drum;
                            }
                        }
                    }
                }
                Self::Poly(poly) => {
                    let mode = poly.mode;
                    poly.current().render(stereo);
                    // The voices of the mode left behind, ringing on.
                    let other = match mode {
                        VoiceMode::Poly => &mut poly.mono,
                        VoiceMode::Mono | VoiceMode::Legato => &mut poly.poly,
                    };
                    if other.active_voices() > 0 {
                        poly.scratch.set_num_frames(frames);
                        other.render(&mut poly.scratch);
                        for (out, rung) in
                            stereo.iter_channels_mut().zip(poly.scratch.iter_channels())
                        {
                            for (sample, &ringing) in out.iter_mut().zip(rung) {
                                *sample += ringing;
                            }
                        }
                    }
                }
                voiced => with_gens!(voiced, gens => gens.render(stereo)),
            }
            return;
        }
        match &mut *self {
            Self::AnalogKit(kit) => {
                kit.gens.render(mono);
                let out = mono.channel_mut(0);
                if kit.fallback.active_voices() > 0 {
                    let drums = &mut kit.scratch[..frames];
                    kit.fallback.render(drums);
                    for (sample, &drum) in out.iter_mut().zip(drums.iter()) {
                        *sample += drum;
                    }
                }
            }
            voiced => with_gens!(voiced, gens => gens.render(mono)),
        }
        for channel in stereo.iter_channels_mut() {
            channel.copy_from_slice(mono.channel(0));
        }
        if let Self::Organ(organ) = self {
            organ.speaker.process(stereo);
        }
    }

    /// Silences the instrument and clears every memory in it, keeping its
    /// settings: after something that is not a number got in.
    pub(crate) fn recover(&mut self) {
        self.choke();
        if let Self::Organ(organ) = self {
            organ.speaker.clear();
        }
    }

    /// Puts a number that is not finite into an organ's speaker, awake, as
    /// nothing outside the crate can: for the tests of what the engine does
    /// then. `false` for any other kind.
    #[cfg(test)]
    pub(crate) fn poison_speaker(&mut self) -> bool {
        let Self::Organ(organ) = self else {
            return false;
        };
        let mut poison = AudioBuffer::<f32>::new(2, 4);
        for channel in poison.iter_channels_mut() {
            channel.fill(f32::NAN);
        }
        organ.speaker.rotary.process(&mut poison);
        organ.speaker.asleep = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_plays_the_drums_it_has_on_their_own_keys() {
        for drum in Drum::ALL {
            match analog_drum(drum) {
                Some(analog) => {
                    // The synth picks the drum by the key: it must be this one.
                    assert_eq!(AnalogDrum::from_key(drum.note()), Some(analog), "{drum:?}");
                    assert!(machine_plays(drum.note()));
                }
                None => assert!(!machine_plays(drum.note()), "{drum:?}"),
            }
        }
        assert_eq!(analog_drum(Drum::Ride), None);
        assert_eq!(analog_drum(Drum::GuitarTap), None);
        // Keys off the drum map that the machine has, and one it has not.
        assert!(machine_plays(39) && machine_plays(56));
        assert!(!machine_plays(100));
    }

    #[test]
    fn presets_and_their_indices_round_trip() {
        for (index, choice) in PERCUSSIONS.iter().enumerate() {
            assert_eq!(percussion_index(*choice), index);
        }
        assert_eq!(percussion_index(OrganParams::jazz().percussion), 3);
        assert_eq!(percussion_index(OrganParams::gospel().percussion), 1);
        assert_eq!(percussion_index(OrganParams::full().percussion), 0);
        assert_eq!(acid_wave_index(AcidParams::square().wave), 1);
        assert_eq!(acid_wave_index(AcidParams::classic().wave), 0);
        // Every algorithm's modulators: the operators something listens to.
        assert_eq!(
            modulators(FmParams::electric_piano().algorithm),
            [false, true, false, true]
        );
        assert_eq!(
            fm_settings(&crate::params::FmKnobs::new()),
            FmParams::electric_piano()
        );
        // The knobs at their defaults are each preset as it is, to the
        // precision of a parameter.
        let plucked = string_settings(&crate::params::PluckedKnobs::new());
        let nylon = StringParams::nylon_guitar();
        for (knob, preset) in [
            (plucked.decay, nylon.decay),
            (plucked.pluck_brightness, nylon.pluck_brightness),
            (plucked.pluck_position, nylon.pluck_position),
            (plucked.tone, nylon.tone),
            (plucked.release, nylon.release),
        ] {
            assert!((knob / preset - 1.0).abs() < 1e-6, "{knob} {preset}");
        }
        assert_eq!(plucked.body, nylon.body);
        let acid = acid_settings(&crate::params::AcidKnobs::new());
        let classic = AcidParams::classic();
        assert!((acid.resonance / classic.resonance - 1.0).abs() < 1e-6);
        assert_eq!(
            (acid.wave, acid.cutoff, acid.level),
            (classic.wave, classic.cutoff, classic.level)
        );
        assert!((gain_of(-6.0) - 0.501_187_2).abs() < 1e-6);
    }
}
