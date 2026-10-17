//! Loudness normalisation of sampler parts: how loud an SFZ instrument
//! plays its standard phrase, measured when it loads, a step at a time.
//!
//! Recorded instruments come at any level, so a sampler part cannot be
//! calibrated once like the other kinds. Instead the instrument plays the
//! phrase its kind is calibrated on (the melody for a sampler, the groove
//! for a sampler kit; [`InstrumentKind::calibration`]) offline, into a
//! scratch buffer, through voices of the same sampler code the part plays
//! it with, and the loudness meter of `manguebyte-analysis` reads it. That
//! accounts for everything that sets the level of what is heard, and that
//! the samples alone do not say: the regions' `volume` and `amplitude`,
//! their velocity tracking at velocity 0.8, their envelopes and loops, the
//! layers a key starts, and how the regions share the keyboard. Measuring
//! the samples themselves, weighted by the keys they cover, would cost
//! about as much (one pass over every sample) and miss all of that. The
//! voices are [`SamplerVoice`]s, one note each, which hand the instrument
//! back when they are dropped (the part's own sampler keeps it); they do
//! not apply choke groups between notes or `polyphony` limits, which the
//! phrases, a note at a time or a groove of short drums, barely touch.
//!
//! The part then plays the instrument at the make-up gain that brings the
//! phrase to [`LOUDNESS_TARGET_LUFS`], never so loud that the phrase's
//! loudest frame, struck at full velocity and accented, would pass
//! [`NORMALIZE_PEAK_DB`], and never more than [`NORMALIZE_RANGE_DB`]
//! either way.
//!
//! The probe measures a clone of the instrument
//! ([`Instrument`]'s clones share its samples), so the part can play the
//! instrument while it is measured, and neither hears the other's notes in
//! its round robins and random choices: what the part plays does not
//! depend on how far the measurement has got. Until it is done, the part
//! plays at an estimate the probe makes as it starts, which costs almost
//! nothing: the energy each note of the phrase reads from the samples
//! ([`Instrument::note_energy`]), as loud as that much energy over the
//! phrase's two bars would be. It hears the regions' gains, layers,
//! velocity tracking, loops and pitch, but not their envelopes, the
//! K-weighting of the meter nor its gating, and it does not hold the
//! make-up back for the peaks.
//!
//! The measurement allocates (its meter, its voices) and renders a few
//! seconds of sound, so it never runs in the render call: a control call
//! does it, [`Engine::load_instrument`](crate::Engine::load_instrument) at
//! once, [`Engine::measure_instrument`](crate::Engine::measure_instrument)
//! a step at a time, or `js/worklet.js` between render quanta,
//! [`PROBE_STEP_FRAMES`] frames a quantum, through `mb_sampler_measure`
//! (or `mb_sfz_measure`, before the instrument plays).

#[cfg(doc)]
use crate::engine::InstrumentKind;

use std::sync::Arc;

use manguebyte_analysis::LoudnessMeter;
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::{ProcessSpec, Processor, Synth};
use manguebyte_sampler::{Instrument, Interpolation, SamplerVoice};

use crate::level::{LOUDNESS_TARGET_LUFS, Phrase, PhraseNote};
use crate::sampled::{SAMPLER_ACCENT_DB, SAMPLER_LEVEL_DB, SAMPLER_VOICES};

/// The most a sampler part's loudness normalisation turns an instrument up
/// or down, in decibels. Further than that, an instrument is more likely
/// broken or nearly silent than recorded at that level.
pub const NORMALIZE_RANGE_DB: f64 = 24.0;

/// The loudest a normalised instrument may play the loudest frame of its
/// phrase, struck at full velocity and accented, in dBFS: the phrase's
/// peak, at velocity 0.8, with the 3.9 dB more a full velocity gives under
/// the SFZ square law and the accent's [`SAMPLER_ACCENT_DB`]. An instrument
/// whose peaks stand that far above its loudness is turned up less than
/// the target asks, and played under it, rather than into the limiter.
pub const NORMALIZE_PEAK_DB: f64 = -1.0;

/// The most frames one step of a measurement renders: about what decoding
/// one 64 KiB sample file costs, so that `js/worklet.js` can take one step
/// between two render quanta. A phrase of 4.5 s at 48 kHz takes 27.
pub const PROBE_STEP_FRAMES: usize = 8_192;

/// The block the voices render in.
const BLOCK: usize = 512;

/// How long each drum of a kit's phrase counts for in the estimate, in
/// seconds: a beat, by when a drum has given most of its energy and the
/// next one has been struck.
const DRUM_ESTIMATE_SECONDS: f64 = 60.0 / Phrase::TEMPO;

/// A note starting or ending, on a frame from the start of the phrase.
#[derive(Debug, Clone, Copy)]
struct Step {
    frame: u64,
    key: u8,
    on: bool,
}

/// A measurement under way.
struct Measurement {
    synth: Synth<SamplerVoice>,
    meter: LoudnessMeter,
    buffer: AudioBuffer<f32>,
    steps: Vec<Step>,
    next: usize,
    frame: u64,
    length: u64,
    /// The largest sample so far.
    peak: f32,
}

/// An SFZ instrument being measured for a sampler part's loudness
/// normalisation. Made, stepped and finished by control calls.
pub(crate) struct LoudnessProbe {
    /// The probe's own clone of the instrument, which the part does not
    /// play.
    instrument: Arc<Instrument>,
    kit: bool,
    sample_rate: f64,
    measuring: Option<Box<Measurement>>,
    /// Whether it measures at all: a part that does not normalise leaves
    /// the instrument unmeasured.
    measures: bool,
    /// The make-up estimated as the measurement began.
    estimate_db: Option<f64>,
    make_up_db: Option<f64>,
}

impl LoudnessProbe {
    /// Starts measuring `instrument`, a clone of the one the part plays, as
    /// a sampler part plays it, a kit if `kit`, at `sample_rate`, and
    /// estimates the make-up the measurement will find; or, if not
    /// `measure`, does nothing with it. Allocates.
    pub(crate) fn new(instrument: Instrument, kit: bool, sample_rate: f64, measure: bool) -> Self {
        let instrument = Arc::new(instrument);
        let mut probe = Self {
            instrument,
            kit,
            sample_rate,
            measuring: None,
            measures: measure,
            estimate_db: None,
            make_up_db: None,
        };
        if !measure {
            return probe;
        }
        let notes = phrase_notes(&probe.instrument, kit);
        if notes.is_empty() {
            // No key to play: nothing to measure.
            return probe;
        }
        probe.estimate_db = estimate_make_up_db(&probe.instrument, &notes, kit);
        let steps = phrase_steps(&notes, sample_rate);
        let spec = ProcessSpec {
            sample_rate,
            max_block_size: BLOCK,
            num_channels: 2,
        };
        let shared = &probe.instrument;
        let mut synth = Synth::with_voices(SAMPLER_VOICES, || {
            let mut voice = SamplerVoice::new(Arc::clone(shared));
            // What a part reads with by default; the sinc reads the same
            // level, at more than three times the cost.
            voice.set_interpolation(Interpolation::Cubic);
            voice
        });
        Processor::<f32>::prepare(&mut synth, &spec);
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec);
        probe.measuring = Some(Box::new(Measurement {
            synth,
            meter,
            buffer: AudioBuffer::new(2, BLOCK),
            steps,
            next: 0,
            frame: 0,
            length: (Phrase::MEASURED_SECONDS * sample_rate).round() as u64,
            peak: 0.0,
        }));
        probe
    }

    /// Whether it was made for a part of this kind, and measures if the
    /// part normalises: if not, the part measures the instrument again.
    pub(crate) fn fits(&self, kit: bool, normalize: bool) -> bool {
        self.kit == kit && (self.measures || !normalize)
    }

    /// Whether the measurement is over, or was never begun.
    pub(crate) fn is_done(&self) -> bool {
        self.measuring.is_none()
    }

    /// The make-up estimated as the measurement began, for the part to
    /// play at until it is over: `None` if it does not measure, or the
    /// instrument plays nothing.
    pub(crate) fn estimate_db(&self) -> Option<f64> {
        self.estimate_db
    }

    /// The make-up measured, once [`step`](Self::step) has said the
    /// measurement is over: `None` until then, if it did not measure, or if
    /// the instrument played nothing.
    pub(crate) fn make_up_db(&self) -> Option<f64> {
        self.make_up_db
    }

    /// Renders up to `frames` more frames of the phrase. Returns whether
    /// the measurement is over.
    pub(crate) fn step(&mut self, frames: usize) -> bool {
        let Some(measurement) = &mut self.measuring else {
            return true;
        };
        let mut left = frames as u64;
        while left > 0 && measurement.frame < measurement.length {
            measurement.start_due_notes();
            let until = measurement
                .steps
                .get(measurement.next)
                .map_or(measurement.length, |step| step.frame)
                .min(measurement.length);
            let count = (until - measurement.frame).min(BLOCK as u64).min(left);
            measurement.render(count as usize);
            left -= count;
        }
        if measurement.frame < measurement.length {
            return false;
        }
        let loudness = measurement.meter.readings().integrated();
        let peak = measurement.peak;
        // Dropping the voices lets go of their hold on the instrument.
        self.measuring = None;
        self.make_up_db = make_up_db(loudness, peak);
        true
    }

    /// Finishes the measurement, all at once, and gives the make-up gain it
    /// measured: `None` if it did not measure, or the instrument played
    /// nothing.
    pub(crate) fn finish(mut self) -> Option<f64> {
        while !self.step(usize::MAX) {}
        self.make_up_db
    }

    /// The same instrument, measured again from the start for a part of
    /// another kind, or one that normalises.
    pub(crate) fn again(mut self, kit: bool, measure: bool) -> Self {
        // Dropping the voices lets go of their hold on the instrument.
        self.measuring = None;
        let instrument =
            Arc::try_unwrap(self.instrument).unwrap_or_else(|shared| Instrument::clone(&shared));
        Self::new(instrument, kit, self.sample_rate, measure)
    }
}

impl Measurement {
    /// Starts and ends the notes due on the frame reached.
    fn start_due_notes(&mut self) {
        while let Some(step) = self.steps.get(self.next) {
            if step.frame > self.frame {
                break;
            }
            let note = NoteRef::new(0, step.key);
            if step.on {
                self.synth.note_on(note, Phrase::VELOCITY);
            } else {
                self.synth.note_off(note, 0.0);
            }
            self.next += 1;
        }
    }

    /// Renders and measures `count` frames, at most a block.
    fn render(&mut self, count: usize) {
        self.buffer.set_num_frames(count);
        self.synth.process(&mut self.buffer);
        self.meter.measure(&self.buffer);
        self.peak = self.peak.max(self.buffer.peak());
        self.frame += count as u64;
    }
}

/// The make-up gain, in decibels, that brings a phrase measured at
/// `loudness` LUFS, its loudest sample `peak`, both as the voices play it,
/// to the target at a sampler part's level, within the peak and the range
/// allowed. `None` for a phrase with no sound in it.
fn make_up_db(loudness: f64, peak: f32) -> Option<f64> {
    // The peak is the largest of the samples' magnitudes, which the
    // voices keep finite: positive, or 0 for silence.
    if !loudness.is_finite() || !peak.is_finite() || peak <= 0.0 {
        return None;
    }
    let at_the_part = loudness + SAMPLER_LEVEL_DB;
    let mut make_up = LOUDNESS_TARGET_LUFS - at_the_part;
    // Full velocity under the square law, and the accent, over the phrase.
    let full_velocity = -40.0 * f64::from(Phrase::VELOCITY).log10();
    let loudest = 20.0 * f64::from(peak).log10()
        + SAMPLER_LEVEL_DB
        + make_up
        + full_velocity
        + SAMPLER_ACCENT_DB;
    if loudest > NORMALIZE_PEAK_DB {
        make_up -= loudest - NORMALIZE_PEAK_DB;
    }
    Some(make_up.clamp(-NORMALIZE_RANGE_DB, NORMALIZE_RANGE_DB))
}

/// The make-up gain, in decibels, that a quick estimate of how loud the
/// instrument plays `notes`, its phrase, asks for, at a sampler part's
/// level and within the range allowed: `None` if they read no sound.
///
/// The estimate is the energy each note reads from the samples, held for
/// its length (a kit's drums for [`DRUM_ESTIMATE_SECONDS`], as one-shots
/// ring on), spread over the phrase's two bars, taken as loud as a signal
/// of that power in two channels is by BS.1770 with its K-weighting flat:
/// `-0.691 + 10·log10(power)` LUFS. It costs a pass over the frames the
/// phrase's distinct notes read, a small part of the measurement.
fn estimate_make_up_db(instrument: &Instrument, notes: &[PhraseNote], kit: bool) -> Option<f64> {
    // The phrase strikes a few keys many times: each distinct note once.
    let mut read: Vec<(u8, f64, f64)> = Vec::new();
    let mut energy = 0.0;
    for note in notes {
        let seconds = if kit {
            DRUM_ESTIMATE_SECONDS
        } else {
            note.length
        };
        let known = read
            .iter()
            .find(|&&(key, held, _)| key == note.key && held == seconds);
        energy += match known {
            Some(&(_, _, energy)) => energy,
            None => {
                let energy = instrument.note_energy(note.key, note.velocity, seconds);
                read.push((note.key, seconds, energy));
                energy
            }
        };
    }
    let power = energy / (8.0 * 60.0 / Phrase::TEMPO);
    if !power.is_finite() || power <= 0.0 {
        return None;
    }
    let loudness = -0.691 + 10.0 * power.log10() + SAMPLER_LEVEL_DB;
    Some((LOUDNESS_TARGET_LUFS - loudness).clamp(-NORMALIZE_RANGE_DB, NORMALIZE_RANGE_DB))
}

/// `notes` as steps, sorted by frame (an end before a start on the same
/// frame).
fn phrase_steps(notes: &[PhraseNote], sample_rate: f64) -> Vec<Step> {
    let frames = |seconds: f64| (seconds * sample_rate).round() as u64;
    let mut steps = Vec::with_capacity(2 * notes.len());
    for note in notes {
        let on = frames(note.start);
        steps.push(Step {
            frame: on,
            key: note.key,
            on: true,
        });
        steps.push(Step {
            frame: on + frames(note.length),
            key: note.key,
            on: false,
        });
    }
    steps.sort_by_key(|step| (step.frame, step.on));
    steps
}

/// The notes an instrument is measured with, in the order they start,
/// each with the key it is played on.
///
/// A melodic instrument plays the melody, moved by whole octaves to where
/// it has the most of its notes, each note it has no region for played on
/// the nearest key it has. A kit plays the groove on the drums of it that
/// it has; one with none of them plays its own keys in turn, one on each
/// eighth note of the groove.
fn phrase_notes(instrument: &Instrument, kit: bool) -> Vec<PhraseNote> {
    let covered: Vec<u8> = (0..=127)
        .filter(|&key| instrument.covers_key(key))
        .collect();
    if covered.is_empty() {
        return Vec::new();
    }
    let mut notes = Vec::new();
    let mut play = |note: &PhraseNote, key: u8| notes.push(PhraseNote { key, ..*note });
    if kit {
        let groove = Phrase::Groove.notes();
        let its_drums: Vec<&PhraseNote> = groove
            .iter()
            .filter(|note| covered.contains(&note.key))
            .collect();
        if its_drums.is_empty() {
            let mut eighths = groove.to_vec();
            eighths.dedup_by(|later, earlier| later.start == earlier.start);
            for (index, note) in eighths.iter().enumerate() {
                play(note, covered[index % covered.len()]);
            }
        } else {
            for note in its_drums {
                play(note, note.key);
            }
        }
    } else {
        let melody = Phrase::Melody.notes();
        let fits = |shift: i32| {
            melody
                .iter()
                .filter(|note| {
                    u8::try_from(i32::from(note.key) + shift)
                        .is_ok_and(|key| covered.contains(&key))
                })
                .count()
        };
        // The octave with the most notes, the nearest to where the melody
        // is written if several have as many.
        let shifts = [0, -12, 12, -24, 24, -36, 36, -48, 48, -60, 60];
        let most = shifts.iter().map(|&shift| fits(shift)).max().unwrap_or(0);
        let shift = shifts
            .into_iter()
            .find(|&shift| fits(shift) == most)
            .unwrap_or(0);
        for note in melody {
            let wanted = i32::from(note.key) + shift;
            let nearest = covered
                .iter()
                .copied()
                .min_by_key(|&key| (i32::from(key) - wanted).abs())
                .unwrap_or(note.key);
            play(note, nearest);
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use manguebyte_sampler::MemorySource;

    fn instrument(text: &str) -> Instrument {
        Instrument::from_sfz(text, &mut MemorySource::new()).unwrap()
    }

    /// The steps `instrument` is measured with at 48 kHz.
    fn steps_of(instrument: &Instrument, kit: bool) -> Vec<Step> {
        phrase_steps(&phrase_notes(instrument, kit), 48_000.0)
    }

    fn keys(steps: &[Step]) -> Vec<u8> {
        steps
            .iter()
            .filter(|step| step.on)
            .map(|step| step.key)
            .collect()
    }

    #[test]
    fn the_melody_moves_to_where_the_instrument_has_its_keys() {
        // Where it is written.
        let piano = instrument("<region> sample=*sine lokey=21 hikey=108");
        let steps = steps_of(&piano, false);
        assert_eq!(keys(&steps), [60, 64, 67, 69, 67, 64, 62, 60, 62, 64, 60]);
        // A glockenspiel, two octaves up.
        let high = instrument("<region> sample=*sine lokey=79 hikey=108");
        assert_eq!(keys(&steps_of(&high, false))[..4], [84, 88, 91, 93]);
        // Three keys only: each note on the nearest.
        let sparse = instrument(
            "<region> sample=*sine key=48 <region> sample=*sine key=55 <region> sample=*sine key=67",
        );
        let played = keys(&steps_of(&sparse, false));
        assert!(
            played.iter().all(|key| [48, 55, 67].contains(key)),
            "{played:?}"
        );
        // Ends before starts on one frame, every start with its end.
        let steps = steps_of(&piano, false);
        assert!(
            steps
                .windows(2)
                .all(|pair| (pair[0].frame, pair[0].on) <= (pair[1].frame, pair[1].on))
        );
        assert_eq!(steps.len(), 2 * Phrase::Melody.notes().len());
    }

    #[test]
    fn a_kit_plays_the_groove_on_its_own_drums() {
        let kit = instrument("<region> sample=*sine key=36 <region> sample=*sine key=42");
        let played = keys(&steps_of(&kit, true));
        assert_eq!(played.iter().filter(|&&key| key == 36).count(), 4);
        assert_eq!(played.iter().filter(|&&key| key == 42).count(), 16);
        assert!(!played.contains(&38), "the snare is not the kit's");
        // A pandeiro: none of the groove's drums, its own strokes in turn.
        let pandeiro = instrument(
            "<region> sample=*sine key=89 <region> sample=*sine key=90 <region> sample=*sine key=91",
        );
        let played = keys(&steps_of(&pandeiro, true));
        assert_eq!(played.len(), 16);
        assert_eq!(played[..4], [89, 90, 91, 89]);
        // Nothing on the keyboard: nothing to measure.
        let nothing = instrument("<region> sample=*sine key=60 trigger=release");
        assert!(steps_of(&nothing, false).is_empty());
        let probe = LoudnessProbe::new(nothing, false, 48_000.0, true);
        assert_eq!((probe.estimate_db(), probe.finish()), (None, None));
    }

    #[test]
    fn the_make_up_is_held_by_the_peak_and_the_range() {
        // A sine of amplitude 0.25 in both channels: about -12.7 LUFS as
        // the voices play it, -18.7 at the part's level.
        let make_up = make_up_db(-12.7, 0.25).unwrap();
        assert!((make_up - (LOUDNESS_TARGET_LUFS + 18.7)).abs() < 1e-9);
        // Peaks 30 dB over the loudness: held back by the peak.
        let spiky = make_up_db(-40.0, 0.316).unwrap();
        let loudest =
            20.0 * 0.316f64.log10() + SAMPLER_LEVEL_DB + spiky + 3.876 + SAMPLER_ACCENT_DB;
        assert!((loudest - NORMALIZE_PEAK_DB).abs() < 0.01, "{loudest}");
        // Nearly silent: held by the range.
        assert_eq!(make_up_db(-90.0, 1e-4), Some(NORMALIZE_RANGE_DB));
        assert_eq!(make_up_db(f64::NEG_INFINITY, 0.5), None);
        assert_eq!(make_up_db(-20.0, 0.0), None);
    }

    #[test]
    fn a_probe_that_does_not_measure_finds_nothing() {
        let probe = LoudnessProbe::new(instrument("<region> sample=*sine"), true, 48_000.0, false);
        assert!(probe.fits(true, false));
        assert!(!probe.fits(true, true), "a part that normalises measures");
        assert!(!probe.fits(false, false), "another kind measures");
        assert!(probe.is_done());
        assert_eq!((probe.estimate_db(), probe.finish()), (None, None));
    }

    #[test]
    fn the_estimate_is_the_energy_the_phrase_reads() {
        // A full-scale sine, held: at velocity 0.8 (102 of 127, squared, on
        // the gain) for the melody's 3.6 s of held notes, over its 4 s.
        let sine = instrument("<region> sample=*sine lokey=0 hikey=127");
        let gain = (102.0f64 / 127.0).powi(2);
        let power = gain * gain * 3.6 / 4.0;
        let loudness = -0.691 + 10.0 * power.log10() + SAMPLER_LEVEL_DB;
        let probe = LoudnessProbe::new(sine, false, 48_000.0, true);
        let estimate = probe.estimate_db().unwrap();
        assert!(
            (estimate - (LOUDNESS_TARGET_LUFS - loudness)).abs() < 0.01,
            "{estimate}"
        );
        // A sine is what the estimate leaves nothing out of: the
        // measurement finds the same, within the K-weighting and the gate.
        let measured = probe.finish().unwrap();
        assert!(
            (estimate - measured).abs() < 0.5,
            "{estimate} against {measured}"
        );
        // Held by the range, like the measurement.
        let faint = instrument("<region> sample=*sine lokey=0 hikey=127 volume=-90");
        let probe = LoudnessProbe::new(faint, false, 48_000.0, true);
        assert_eq!(probe.estimate_db(), Some(NORMALIZE_RANGE_DB));
        // A kit's drums count for a beat each.
        let kit = instrument("<region> sample=*sine key=36");
        let probe = LoudnessProbe::new(kit, true, 48_000.0, true);
        let power = gain * gain * 4.0 * DRUM_ESTIMATE_SECONDS / 4.0;
        let loudness = -0.691 + 10.0 * power.log10() + SAMPLER_LEVEL_DB;
        let estimate = probe.estimate_db().unwrap();
        assert!(
            (estimate - (LOUDNESS_TARGET_LUFS - loudness)).abs() < 0.01,
            "{estimate}"
        );
    }
}
