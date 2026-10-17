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
//! The measurement allocates (its meter, its voices) and renders a few
//! seconds of sound, so it never runs in the render call: a control call
//! does it, [`Engine::load_instrument`](crate::Engine::load_instrument) at
//! once, or `js/worklet.js` between render quanta, [`PROBE_STEP_FRAMES`]
//! frames a quantum, through `mb_sfz_measure`.

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
    instrument: Arc<Instrument>,
    kit: bool,
    sample_rate: f64,
    measuring: Option<Box<Measurement>>,
    /// Whether it measures at all: a part that does not normalise leaves
    /// the instrument unmeasured.
    measures: bool,
    make_up_db: Option<f64>,
}

impl LoudnessProbe {
    /// Starts measuring `instrument` as a sampler part plays it, a kit if
    /// `kit`, at `sample_rate`; or, if not `measure`, does nothing with it.
    /// Allocates.
    pub(crate) fn new(instrument: Instrument, kit: bool, sample_rate: f64, measure: bool) -> Self {
        let instrument = Arc::new(instrument);
        let mut probe = Self {
            instrument,
            kit,
            sample_rate,
            measuring: None,
            measures: measure,
            make_up_db: None,
        };
        if !measure {
            return probe;
        }
        let steps = phrase_steps(&probe.instrument, kit, sample_rate);
        if steps.is_empty() {
            // No key to play: nothing to measure.
            return probe;
        }
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

    /// The instrument being measured.
    pub(crate) fn instrument(&self) -> &Instrument {
        &self.instrument
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

    /// Finishes the measurement, all at once, and gives the instrument back
    /// with the make-up gain it measured: `None` if it did not measure, or
    /// the instrument played nothing.
    pub(crate) fn finish(mut self) -> (Instrument, Option<f64>) {
        while !self.step(usize::MAX) {}
        let make_up = self.make_up_db;
        (self.into_instrument(), make_up)
    }

    /// The same instrument, measured again from the start for a part of
    /// another kind, or one that normalises.
    pub(crate) fn again(self, kit: bool, measure: bool) -> Self {
        let sample_rate = self.sample_rate;
        Self::new(self.into_instrument(), kit, sample_rate, measure)
    }

    fn into_instrument(mut self) -> Instrument {
        self.measuring = None;
        match Arc::try_unwrap(self.instrument) {
            Ok(instrument) => instrument,
            // Only the voices, just dropped, ever held it besides.
            Err(_) => unreachable!("the probe's voices are gone"),
        }
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

/// The notes an instrument is measured with, as steps sorted by frame (an
/// end before a start on the same frame).
///
/// A melodic instrument plays the melody, moved by whole octaves to where
/// it has the most of its notes, each note it has no region for played on
/// the nearest key it has. A kit plays the groove on the drums of it that
/// it has; one with none of them plays its own keys in turn, one on each
/// eighth note of the groove.
fn phrase_steps(instrument: &Instrument, kit: bool, sample_rate: f64) -> Vec<Step> {
    let covered: Vec<u8> = (0..=127)
        .filter(|&key| instrument.covers_key(key))
        .collect();
    if covered.is_empty() {
        return Vec::new();
    }
    let mut steps = Vec::new();
    let frames = |seconds: f64| (seconds * sample_rate).round() as u64;
    let mut play = |note: &PhraseNote, key: u8| {
        let on = frames(note.start);
        steps.push(Step {
            frame: on,
            key,
            on: true,
        });
        steps.push(Step {
            frame: on + frames(note.length),
            key,
            on: false,
        });
    };
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
    steps.sort_by_key(|step| (step.frame, step.on));
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use manguebyte_sampler::MemorySource;

    fn instrument(text: &str) -> Instrument {
        Instrument::from_sfz(text, &mut MemorySource::new()).unwrap()
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
        let steps = phrase_steps(&piano, false, 48_000.0);
        assert_eq!(keys(&steps), [60, 64, 67, 69, 67, 64, 62, 60, 62, 64, 60]);
        // A glockenspiel, two octaves up.
        let high = instrument("<region> sample=*sine lokey=79 hikey=108");
        assert_eq!(
            keys(&phrase_steps(&high, false, 48_000.0))[..4],
            [84, 88, 91, 93]
        );
        // Three keys only: each note on the nearest.
        let sparse = instrument(
            "<region> sample=*sine key=48 <region> sample=*sine key=55 <region> sample=*sine key=67",
        );
        let played = keys(&phrase_steps(&sparse, false, 48_000.0));
        assert!(
            played.iter().all(|key| [48, 55, 67].contains(key)),
            "{played:?}"
        );
        // Ends before starts on one frame, every start with its end.
        let steps = phrase_steps(&piano, false, 48_000.0);
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
        let played = keys(&phrase_steps(&kit, true, 48_000.0));
        assert_eq!(played.iter().filter(|&&key| key == 36).count(), 4);
        assert_eq!(played.iter().filter(|&&key| key == 42).count(), 16);
        assert!(!played.contains(&38), "the snare is not the kit's");
        // A pandeiro: none of the groove's drums, its own strokes in turn.
        let pandeiro = instrument(
            "<region> sample=*sine key=89 <region> sample=*sine key=90 <region> sample=*sine key=91",
        );
        let played = keys(&phrase_steps(&pandeiro, true, 48_000.0));
        assert_eq!(played.len(), 16);
        assert_eq!(played[..4], [89, 90, 91, 89]);
        // Nothing on the keyboard: nothing to measure.
        let nothing = instrument("<region> sample=*sine key=60 trigger=release");
        assert!(phrase_steps(&nothing, false, 48_000.0).is_empty());
        let probe = LoudnessProbe::new(nothing, false, 48_000.0, true);
        assert_eq!(probe.finish().1, None);
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
    fn a_probe_that_does_not_measure_hands_the_instrument_back_untouched() {
        let probe = LoudnessProbe::new(instrument("<region> sample=*sine"), true, 48_000.0, false);
        assert!(probe.fits(true, false));
        assert!(!probe.fits(true, true), "a part that normalises measures");
        assert!(!probe.fits(false, false), "another kind measures");
        let (instrument, make_up) = probe.finish();
        assert_eq!((instrument.num_regions(), make_up), (1, None));
    }
}
