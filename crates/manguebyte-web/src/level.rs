//! How loud each kind of instrument plays: one loudness target, the
//! standard phrases it is measured on, and the calibration table that
//! brings every kind and preset to it.
//!
//! Loudness is integrated loudness after ITU-R BS.1770-4 (EBU R 128),
//! measured on the part's stereo output with its chain neutral (no effect,
//! gain 0 dB, centred) and the master bus's limiter not engaged.

use manguebyte_params::ParamSmoother;

use crate::engine::InstrumentKind;
use crate::params::{
    ACID_PRESET_NAMES, BOWED_PRESET_NAMES, CHOIR_PRESET_NAMES, DRUM_KIT_PRESET_NAMES,
    FM_PRESET_NAMES, LEVEL_SMOOTHING, MALLET_PRESET_NAMES, ORGAN_PRESET_NAMES,
    PLUCKED_PRESET_NAMES, POLY_PRESET_NAMES, WIND_PRESET_NAMES, ids,
};

/// The integrated loudness, in LUFS, that a part of every kind plays its
/// standard [`Phrase`] at, at velocity [`Phrase::VELOCITY`], through a
/// neutral chain.
///
/// It is the loudest target at which every kind's loudest single note,
/// accented at full velocity, still stays under the master limiter's
/// ceiling (-0.3 dBTP), so that the limiter leaves one note alone as it
/// always has. The drum machine sets it: its velocity and accent span
/// 9 dB, so its snare struck that hard is 23.4 dB above its groove's
/// loudness, and peaks at -0.6 dBTP. Close to the -23 LUFS that EBU R 128
/// asks of a whole programme (and the -24 of ATSC A/85), it is where the
/// engine's melodic parts mostly were already; a few parts together come
/// to about -18 LUFS, and `master.gain` takes a set further.
pub const LOUDNESS_TARGET_LUFS: f64 = -24.0;

/// How far a calibrated kind may land from [`LOUDNESS_TARGET_LUFS`] on its
/// standard phrase, in LU: `tests/loudness.rs` holds every kind and preset
/// to it.
pub const LOUDNESS_TOLERANCE_LU: f64 = 1.0;

/// A short piece of music a kind of instrument is measured on: two bars at
/// [`Phrase::TEMPO`], every note at [`Phrase::VELOCITY`], none accented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phrase {
    /// A melody around middle C, one note at a time, in quarter and eighth
    /// notes held for nine tenths of their length: C E G A G, E D C D E C.
    Melody,
    /// The melody two octaves down, around C2: a bass line.
    Bass,
    /// Two chords of three notes in the middle of the keyboard, C major
    /// and A minor, a bar each, held for nine tenths of it.
    Chord,
    /// A groove on the drum map: the closed hat on every eighth note, the
    /// kick on beats 1 and 3, the snare on 2 and 4.
    Groove,
}

/// One note of a [`Phrase`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhraseNote {
    /// When it starts, in seconds from the start of the phrase.
    pub start: f64,
    /// How long it is held, in seconds.
    pub length: f64,
    /// Its key.
    pub key: u8,
    /// Its velocity.
    pub velocity: f32,
}

/// The length of a beat at [`Phrase::TEMPO`], in seconds.
const BEAT: f64 = 60.0 / Phrase::TEMPO;

/// How much of its written length a melodic note is held.
const LEGATO: f64 = 0.9;

const fn note(beat: f64, beats: f64, key: u8) -> PhraseNote {
    PhraseNote {
        start: beat * BEAT,
        length: beats * BEAT * LEGATO,
        key,
        velocity: Phrase::VELOCITY,
    }
}

const MELODY: [PhraseNote; 11] = [
    note(0.0, 1.0, 60),
    note(1.0, 0.5, 64),
    note(1.5, 0.5, 67),
    note(2.0, 1.0, 69),
    note(3.0, 1.0, 67),
    note(4.0, 0.5, 64),
    note(4.5, 0.5, 62),
    note(5.0, 0.5, 60),
    note(5.5, 0.5, 62),
    note(6.0, 1.0, 64),
    note(7.0, 1.0, 60),
];

const fn transposed<const N: usize>(notes: [PhraseNote; N], semitones: i32) -> [PhraseNote; N] {
    let mut notes = notes;
    let mut index = 0;
    while index < N {
        notes[index].key = (notes[index].key as i32 + semitones) as u8;
        index += 1;
    }
    notes
}

const BASS: [PhraseNote; 11] = transposed(MELODY, -24);

const CHORD: [PhraseNote; 6] = [
    note(0.0, 4.0, 60),
    note(0.0, 4.0, 64),
    note(0.0, 4.0, 67),
    note(4.0, 4.0, 57),
    note(4.0, 4.0, 60),
    note(4.0, 4.0, 64),
];

/// The drum map's kick, snare and closed hat.
const KICK: u8 = 36;
const SNARE: u8 = 38;
const CLOSED_HAT: u8 = 42;

/// A drum struck on eighth note `eighth`.
const fn hit(eighth: usize, key: u8) -> PhraseNote {
    PhraseNote {
        start: eighth as f64 * BEAT / 2.0,
        length: BEAT / 4.0,
        key,
        velocity: Phrase::VELOCITY,
    }
}

const fn groove() -> [PhraseNote; 24] {
    let mut notes = [hit(0, CLOSED_HAT); 24];
    let mut eighth = 0;
    let mut index = 0;
    while eighth < 16 {
        // The kick or the snare first, then the hat, on the same frame.
        match eighth % 4 {
            0 => {
                notes[index] = hit(eighth, KICK);
                index += 1;
            }
            2 => {
                notes[index] = hit(eighth, SNARE);
                index += 1;
            }
            _ => {}
        }
        notes[index] = hit(eighth, CLOSED_HAT);
        index += 1;
        eighth += 1;
    }
    notes
}

const GROOVE: [PhraseNote; 24] = groove();

impl Phrase {
    /// Every phrase.
    pub const ALL: [Self; 4] = [Self::Melody, Self::Bass, Self::Chord, Self::Groove];

    /// The tempo of every phrase, in beats per minute.
    pub const TEMPO: f64 = 120.0;

    /// The velocity of every note of every phrase.
    pub const VELOCITY: f32 = 0.8;

    /// How long a phrase is measured for, in seconds from its first note:
    /// its two bars, and half a second of what rings on after them.
    pub const MEASURED_SECONDS: f64 = 4.5;

    /// The phrase's notes, in the order they start; notes that start
    /// together are listed one after the other.
    pub const fn notes(self) -> &'static [PhraseNote] {
        match self {
            Self::Melody => &MELODY,
            Self::Bass => &BASS,
            Self::Chord => &CHORD,
            Self::Groove => &GROOVE,
        }
    }

    /// The phrase's name: `"melody"`, `"bass"`, `"chord"` or `"groove"`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Melody => "melody",
            Self::Bass => "bass",
            Self::Chord => "chord",
            Self::Groove => "groove",
        }
    }
}

/// How one preset of a kind of instrument, or a kind without presets, is
/// brought to [`LOUDNESS_TARGET_LUFS`]: a row of
/// [`InstrumentKind::calibration`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// The preset's name, as the parameter JSON lists it under
    /// `<kind>.preset`; `None` for a kind without presets.
    pub preset: Option<&'static str>,
    /// The phrase it is measured on.
    pub phrase: Phrase,
    /// The make-up gain the part plays at, in decibels, on top of the
    /// level its voices put out, measured so that the phrase lands on the
    /// target. `None` for the sampler kinds, whose instruments are
    /// measured on the phrase when they load (see
    /// [Sampler parts](crate#sampler-parts)).
    pub make_up_db: Option<f64>,
}

const fn row(preset: Option<&'static str>, phrase: Phrase, make_up_db: f64) -> Calibration {
    Calibration {
        preset,
        phrase,
        make_up_db: Some(make_up_db),
    }
}

const fn measured(phrase: Phrase) -> Calibration {
    Calibration {
        preset: None,
        phrase,
        make_up_db: None,
    }
}

// The calibration table. Each kind's rows are in the order of its
// presets' values, one row for a kind without presets. The arrays are as
// long as the lists of preset names, so a preset added without a row does
// not compile, and a new kind does not compile until `calibration` has its
// rows. `tests/loudness.rs` measures every row on its phrase and fails if
// one lands more than LOUDNESS_TOLERANCE_LU from the target. To calibrate
// a row, give it a make-up of 0, run
// `cargo test -p manguebyte-web --test loudness -- --nocapture`, and put
// in the make-up the table it prints asks for.

const SYNTH: [Calibration; 1] = [row(None, Phrase::Melody, -1.43)];

/// Every synthesized drum, wherever it plays: a drum part, and the drums a
/// sampler kit or the drum machine takes from it.
const DRUMS: [Calibration; 1] = [row(None, Phrase::Groove, -4.76)];

const SAMPLER: [Calibration; 1] = [measured(Phrase::Melody)];

const SAMPLER_KIT: [Calibration; 1] = [measured(Phrase::Groove)];

const PLUCKED: [Calibration; PLUCKED_PRESET_NAMES.len()] = [
    row(Some(PLUCKED_PRESET_NAMES[0]), Phrase::Melody, -1.42),
    row(Some(PLUCKED_PRESET_NAMES[1]), Phrase::Melody, 8.96),
    row(Some(PLUCKED_PRESET_NAMES[2]), Phrase::Bass, -2.91),
    row(Some(PLUCKED_PRESET_NAMES[3]), Phrase::Bass, 0.35),
];

const FM: [Calibration; FM_PRESET_NAMES.len()] = [
    row(Some(FM_PRESET_NAMES[0]), Phrase::Chord, 1.06),
    row(Some(FM_PRESET_NAMES[1]), Phrase::Melody, -0.68),
    row(Some(FM_PRESET_NAMES[2]), Phrase::Bass, -6.26),
];

const ORGAN: [Calibration; ORGAN_PRESET_NAMES.len()] = [
    row(Some(ORGAN_PRESET_NAMES[0]), Phrase::Chord, -1.42),
    row(Some(ORGAN_PRESET_NAMES[1]), Phrase::Chord, -2.35),
    row(Some(ORGAN_PRESET_NAMES[2]), Phrase::Chord, -3.67),
    row(Some(ORGAN_PRESET_NAMES[3]), Phrase::Chord, -2.07),
];

/// The machine's own drums; the ride and the guitar tap it takes from the
/// synthesized kit play at the drums' make-up.
const ANALOG_KIT: [Calibration; 1] = [row(None, Phrase::Groove, 0.26)];

const KICK_BASS: [Calibration; 1] = [row(None, Phrase::Bass, -9.91)];

const ACID: [Calibration; ACID_PRESET_NAMES.len()] = [
    row(Some(ACID_PRESET_NAMES[0]), Phrase::Bass, -3.70),
    row(Some(ACID_PRESET_NAMES[1]), Phrase::Bass, -2.00),
    row(Some(ACID_PRESET_NAMES[2]), Phrase::Bass, -5.66),
];

/// Each kit's own pieces; the drums of the drum map a kit has no piece for
/// are the synthesized kit's, at the drums' make-up.
const DRUM_KIT: [Calibration; DRUM_KIT_PRESET_NAMES.len()] = [
    row(Some(DRUM_KIT_PRESET_NAMES[0]), Phrase::Groove, 2.76),
    row(Some(DRUM_KIT_PRESET_NAMES[1]), Phrase::Groove, 2.26),
    row(Some(DRUM_KIT_PRESET_NAMES[2]), Phrase::Groove, 3.34),
    row(Some(DRUM_KIT_PRESET_NAMES[3]), Phrase::Groove, 4.09),
    row(Some(DRUM_KIT_PRESET_NAMES[4]), Phrase::Groove, 4.54),
    row(Some(DRUM_KIT_PRESET_NAMES[5]), Phrase::Groove, -0.96),
    row(Some(DRUM_KIT_PRESET_NAMES[6]), Phrase::Groove, 2.78),
    row(Some(DRUM_KIT_PRESET_NAMES[7]), Phrase::Groove, 3.58),
    row(Some(DRUM_KIT_PRESET_NAMES[8]), Phrase::Groove, 1.85),
    row(Some(DRUM_KIT_PRESET_NAMES[9]), Phrase::Groove, 4.47),
];

/// The make-up of each preset of the polyphonic synthesiser, in the order
/// of `poly.preset`.
const POLY_MAKE_UP: [f64; POLY_PRESET_NAMES.len()] = [
    -2.69,  // Moog Bass
    -9.47,  // Reese
    -6.78,  // Wobble Bass
    -11.62, // Deep Sub
    -9.83,  // FM Growl
    -7.47,  // House Organ Bass
    -7.92,  // Acid Line
    -7.21,  // Garage Square
    -11.36, // Wavetable Growl
    -8.30,  // Synthpop Bass
    0.85,   // Funk Bass
    -11.14, // 808 Glide
    -2.66,  // Rolling Bass
    -6.91,  // Rubber Bass
    -6.78,  // Talking Bass
    -8.04,  // Trance Supersaw
    -11.13, // Square Lead
    -11.94, // Sync Lead
    -10.95, // Chiptune
    -6.79,  // Hoover
    -7.96,  // Mono Saw Lead
    -11.33, // Whistle Lead
    -10.70, // Digital Lead
    -2.83,  // Talking Lead
    -13.57, // Screamer
    -9.52,  // Future Lead
    -13.15, // Warm Analog Pad
    -10.87, // String Machine
    -11.49, // Glass Pad
    -14.12, // Dark Drone
    -13.14, // Choir Pad
    -15.58, // Formant Pad
    -15.68, // PWM Pad
    -10.32, // Shimmer Pad
    -11.19, // Evolving Spectra
    -14.10, // Soft Sine Pad
    -14.34, // Comb Wash
    -11.47, // Sweep Pad
    -1.88,  // House Pluck
    -11.71, // Bell Pluck
    -8.15,  // Marimba
    0.16,   // Plucked String
    -5.12,  // Future Pluck
    -7.15,  // Glass Pluck
    -2.08,  // Trance Pluck
    2.00,   // Pizzicato
    -9.67,  // Kalimba
    -5.83,  // Polysynth Piano
    -0.40,  // Clavinet
    -9.20,  // FM Electric Piano
    -15.42, // Drawbar Organ
    -10.22, // Juno Keys
    -10.07, // Wurly
    -9.25,  // Vibraphone
    -11.34, // Synth Brass
    0.01,   // Brass Stab
    -12.39, // Jump Brass
    -0.98,  // Rave Chord Stab
    -13.31, // Soft Horns
    -11.70, // Fanfare
    -5.49,  // Riser
    -8.19,  // Downlifter
    5.07,   // Noise Sweep
    -4.01,  // Laser Zap
    -9.69,  // Random Bleeps
    -3.25,  // Wind
    -13.12, // Impact Boom
    -15.75, // Siren
];

/// The phrase preset `index` of the polyphonic synthesiser is measured on,
/// by the category the library files it under
/// (`modelled::poly_phrase`): presets 0 to 14 are basses, 15 to 25 leads,
/// 26 to 37 pads, 38 to 46 plucks, 47 to 53 keys, 54 to 59 brass, 60 to 67
/// effects.
const fn poly_phrase_of(index: usize) -> Phrase {
    match index {
        0..=14 => Phrase::Bass,
        26..=37 | 47..=59 => Phrase::Chord,
        _ => Phrase::Melody,
    }
}

const fn poly_rows() -> [Calibration; POLY_PRESET_NAMES.len()] {
    let mut rows = [row(None, Phrase::Melody, 0.0); POLY_PRESET_NAMES.len()];
    let mut index = 0;
    while index < rows.len() {
        rows[index] = row(
            Some(POLY_PRESET_NAMES[index]),
            poly_phrase_of(index),
            POLY_MAKE_UP[index],
        );
        index += 1;
    }
    rows
}

const POLY: [Calibration; POLY_PRESET_NAMES.len()] = poly_rows();

const MALLET: [Calibration; MALLET_PRESET_NAMES.len()] = [
    row(Some(MALLET_PRESET_NAMES[0]), Phrase::Melody, -7.18),
    row(Some(MALLET_PRESET_NAMES[1]), Phrase::Melody, -2.54),
    row(Some(MALLET_PRESET_NAMES[2]), Phrase::Melody, -3.68),
    row(Some(MALLET_PRESET_NAMES[3]), Phrase::Melody, -6.52),
    row(Some(MALLET_PRESET_NAMES[4]), Phrase::Melody, -3.00),
    row(Some(MALLET_PRESET_NAMES[5]), Phrase::Melody, -9.39),
    row(Some(MALLET_PRESET_NAMES[6]), Phrase::Melody, -8.81),
    row(Some(MALLET_PRESET_NAMES[7]), Phrase::Melody, -10.54),
];

const BOWED: [Calibration; BOWED_PRESET_NAMES.len()] = [
    row(Some(BOWED_PRESET_NAMES[0]), Phrase::Melody, 5.28),
    row(Some(BOWED_PRESET_NAMES[1]), Phrase::Melody, 4.96),
    row(Some(BOWED_PRESET_NAMES[2]), Phrase::Melody, 5.19),
    row(Some(BOWED_PRESET_NAMES[3]), Phrase::Bass, 6.54),
    row(Some(BOWED_PRESET_NAMES[4]), Phrase::Chord, -0.27),
    row(Some(BOWED_PRESET_NAMES[5]), Phrase::Melody, 11.52),
    row(Some(BOWED_PRESET_NAMES[6]), Phrase::Melody, 12.33),
    row(Some(BOWED_PRESET_NAMES[7]), Phrase::Bass, 8.75),
];

const WIND: [Calibration; WIND_PRESET_NAMES.len()] = [
    row(Some(WIND_PRESET_NAMES[0]), Phrase::Melody, 3.27),
    row(Some(WIND_PRESET_NAMES[1]), Phrase::Melody, -2.48),
    row(Some(WIND_PRESET_NAMES[2]), Phrase::Melody, -3.31),
    row(Some(WIND_PRESET_NAMES[3]), Phrase::Melody, -0.25),
    row(Some(WIND_PRESET_NAMES[4]), Phrase::Melody, -1.91),
];

const CHOIR: [Calibration; CHOIR_PRESET_NAMES.len()] = [
    row(Some(CHOIR_PRESET_NAMES[0]), Phrase::Chord, -9.56),
    row(Some(CHOIR_PRESET_NAMES[1]), Phrase::Chord, -7.51),
    row(Some(CHOIR_PRESET_NAMES[2]), Phrase::Melody, -8.99),
];

impl InstrumentKind {
    /// How the kind is brought to [`LOUDNESS_TARGET_LUFS`]: one
    /// [`Calibration`] per preset, in the order of the preset's values
    /// (`plucked.preset` 1 is row 1), or one for a kind without presets.
    ///
    /// A new kind does not compile until it has its rows here, and
    /// `tests/loudness.rs` measures every row.
    ///
    /// ```
    /// use manguebyte_web::{InstrumentKind, Phrase};
    ///
    /// let steel = InstrumentKind::Plucked.calibration()[1];
    /// assert_eq!(steel.preset, Some("Steel guitar"));
    /// assert_eq!(steel.phrase, Phrase::Melody);
    /// assert!(steel.make_up_db.is_some());
    /// // A sampler's instruments are measured as they load.
    /// assert_eq!(InstrumentKind::Sampler.calibration()[0].make_up_db, None);
    /// ```
    pub const fn calibration(self) -> &'static [Calibration] {
        match self {
            Self::Synth => &SYNTH,
            Self::Drums => &DRUMS,
            Self::Sampler => &SAMPLER,
            Self::SamplerKit => &SAMPLER_KIT,
            Self::Plucked => &PLUCKED,
            Self::Fm => &FM,
            Self::Organ => &ORGAN,
            Self::AnalogKit => &ANALOG_KIT,
            Self::KickBass => &KICK_BASS,
            Self::Acid => &ACID,
            Self::DrumKit => &DRUM_KIT,
            Self::Poly => &POLY,
            Self::Mallet => &MALLET,
            Self::Bowed => &BOWED,
            Self::Wind => &WIND,
            Self::Choir => &CHOIR,
        }
    }

    /// The identifier of the parameter that chooses the kind's preset
    /// (`"plucked.preset"`...), whose values number the rows of
    /// [`calibration`](Self::calibration); `None` for a kind without
    /// presets.
    pub const fn preset_param(self) -> Option<&'static str> {
        match self {
            Self::Plucked => Some(ids::PLUCKED_PRESET),
            Self::Fm => Some(ids::FM_PRESET),
            Self::Organ => Some(ids::ORGAN_PRESET),
            Self::Acid => Some(ids::ACID_PRESET),
            Self::DrumKit => Some(ids::DRUM_KIT_PRESET),
            Self::Poly => Some(ids::POLY_PRESET),
            Self::Mallet => Some(ids::MALLET_PRESET),
            Self::Bowed => Some(ids::BOWED_PRESET),
            Self::Wind => Some(ids::WIND_PRESET),
            Self::Choir => Some(ids::CHOIR_PRESET),
            Self::Synth
            | Self::Drums
            | Self::Sampler
            | Self::SamplerKit
            | Self::AnalogKit
            | Self::KickBass => None,
        }
    }
}

/// The gain of `decibels`.
pub(crate) fn gain_of(decibels: f64) -> f64 {
    (decibels / 20.0 * core::f64::consts::LN_10).exp()
}

/// The make-up gain, in decibels, of preset `preset` of `kind` (the first
/// row for a kind without presets); 0 where the table has none.
pub(crate) fn make_up_db(kind: InstrumentKind, preset: usize) -> f64 {
    let rows = kind.calibration();
    rows.get(preset)
        .or(rows.first())
        .and_then(|row| row.make_up_db)
        .unwrap_or(0.0)
}

/// A gain on what an instrument puts out, such as its make-up. A new value
/// either applies at once or glides in over 20 ms in a straight line
/// ([`LEVEL_SMOOTHING`]), so that it does not click. Worked out frame by
/// frame, so the output does not depend on how the frames are cut into
/// quanta. Real-time safe.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Trim {
    smoother: ParamSmoother,
}

impl Trim {
    pub(crate) fn new(gain: f32, sample_rate: f64) -> Self {
        let mut smoother = ParamSmoother::new(LEVEL_SMOOTHING);
        smoother.prepare(sample_rate, gain);
        Self { smoother }
    }

    /// Moves to `gain`: at once, from the next frame, or gliding.
    pub(crate) fn set(&mut self, gain: f32, at_once: bool) {
        if at_once {
            self.smoother.reset(gain);
        } else {
            self.smoother.set_target(gain);
        }
    }

    /// Multiplies both channels by the gain, frame by frame.
    pub(crate) fn apply_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.smoother.is_smoothing() {
            for (left, right) in left.iter_mut().zip(right) {
                let gain = self.smoother.next_value();
                *left *= gain;
                *right *= gain;
            }
        } else {
            let gain = self.smoother.current();
            if gain != 1.0 {
                for sample in left.iter_mut().chain(right) {
                    *sample *= gain;
                }
            }
        }
    }

    /// Multiplies `samples` by the gain, frame by frame.
    pub(crate) fn apply(&mut self, samples: &mut [f32]) {
        if self.smoother.is_smoothing() {
            for sample in samples {
                *sample *= self.smoother.next_value();
            }
        } else {
            let gain = self.smoother.current();
            if gain != 1.0 {
                for sample in samples {
                    *sample *= gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Chain;
    use crate::params::PartParams;

    /// Every kind has a row for each of its presets, named as the preset
    /// is, or one row if it has none. A preset is any parameter of the
    /// instrument whose identifier ends in `.preset`, so a kind that gains
    /// one fails here until it has its rows.
    #[test]
    fn every_preset_of_every_kind_has_its_row() {
        for kind in InstrumentKind::ALL {
            let rows = kind.calibration();
            let params = PartParams::new(kind, Chain::NONE);
            let mut presets = Vec::new();
            params.instrument.visit(&mut |param| {
                if param.id().ends_with(".preset") {
                    presets.push(param);
                }
            });
            assert!(presets.len() <= 1, "{kind:?}: one preset parameter at most");
            assert_eq!(
                presets.first().map(|preset| preset.id()),
                kind.preset_param(),
                "{kind:?}: preset_param names its preset parameter"
            );
            match presets.first() {
                Some(preset) => {
                    let count = preset.step_count().unwrap() as usize + 1;
                    assert_eq!(rows.len(), count, "{kind:?}: a row per preset");
                    for (index, row) in rows.iter().enumerate() {
                        let name = preset.plain_to_text(index as f64);
                        assert_eq!(row.preset, Some(name.as_str()), "{kind:?} row {index}");
                    }
                }
                None => {
                    assert_eq!(rows.len(), 1, "{kind:?} has no presets: one row");
                    assert_eq!(rows[0].preset, None, "{kind:?}");
                }
            }
            assert_eq!(
                rows.iter().all(|row| row.make_up_db.is_none()),
                kind.is_sampler(),
                "{kind:?}: only the samplers are measured as they load"
            );
        }
        // Past the last preset, the first row; nothing for the samplers.
        assert_eq!(
            make_up_db(InstrumentKind::Synth, 7),
            make_up_db(InstrumentKind::Synth, 0)
        );
        assert_eq!(make_up_db(InstrumentKind::Sampler, 0), 0.0);
    }

    /// The polyphonic synthesiser's rows are measured on the phrase of the
    /// category the library files each preset under.
    #[test]
    fn every_poly_preset_is_measured_on_its_categorys_phrase() {
        use manguebyte_dsp::instruments::poly::PRESETS;
        for (index, (row, preset)) in POLY.iter().zip(&PRESETS).enumerate() {
            assert_eq!(
                row.phrase,
                crate::modelled::poly_phrase(preset.category),
                "{index} {}",
                preset.name
            );
            assert_eq!(row.preset, Some(preset.name));
        }
    }

    #[test]
    fn a_trim_glides_while_sound_passes_and_jumps_in_silence() {
        let mut trim = Trim::new(1.0, 1_000.0);
        let mut ones = [1.0f32; 30];
        trim.apply(&mut ones);
        assert!(ones.iter().all(|&sample| sample == 1.0));
        // 20 ms at 1 kHz: 20 frames, in a straight line.
        trim.set(0.5, false);
        trim.apply(&mut ones);
        assert!((ones[0] - 0.975).abs() < 1e-6, "{}", ones[0]);
        assert!((ones[9] - 0.75).abs() < 1e-6, "{}", ones[9]);
        assert!(ones[19..].iter().all(|&sample| sample == 0.5));
        trim.set(2.0, true);
        let mut ones = [1.0f32; 4];
        trim.apply(&mut ones);
        assert_eq!(ones, [2.0; 4]);
        // Both channels alike, gliding or not.
        trim.set(1.0, false);
        let (mut left, mut right) = ([1.0f32; 24], [-1.0f32; 24]);
        trim.apply_stereo(&mut left, &mut right);
        assert!((left[9] - 1.5).abs() < 1e-6, "{}", left[9]);
        assert!(
            left.iter()
                .zip(&right)
                .all(|(left, right)| *left == -*right)
        );
        assert_eq!(left[23], 1.0);
        trim.set(0.25, true);
        let (mut left, mut right) = ([1.0f32; 4], [-1.0f32; 4]);
        trim.apply_stereo(&mut left, &mut right);
        assert_eq!((left, right), ([0.25; 4], [-0.25; 4]));
    }

    #[test]
    fn the_phrases_are_two_bars_long() {
        for phrase in Phrase::ALL {
            let notes = phrase.notes();
            assert!(!notes.is_empty());
            let end = notes
                .iter()
                .map(|note| note.start + note.length)
                .fold(0.0, f64::max);
            assert!(end <= 8.0 * BEAT && end > 7.0 * BEAT, "{phrase:?} {end}");
            assert!(end < Phrase::MEASURED_SECONDS);
            assert!(notes.windows(2).all(|pair| pair[0].start <= pair[1].start));
            assert!(notes.iter().all(|note| note.velocity == Phrase::VELOCITY));
        }
        assert_eq!(BASS[0].key, 36);
        assert_eq!(
            GROOVE.iter().filter(|note| note.key == CLOSED_HAT).count(),
            16
        );
        assert_eq!(GROOVE.iter().filter(|note| note.key == KICK).count(), 4);
        assert_eq!(GROOVE[0].key, KICK);
        assert_eq!(GROOVE[2].key, CLOSED_HAT);
        assert_eq!(GROOVE[3].key, SNARE);
        assert!((gain_of(-6.0) - 0.501_187).abs() < 1e-6);
    }
}
