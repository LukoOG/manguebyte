//! The peak programme meter.

use core::f64::consts::TAU;
use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, Row, db, screened, usable_rate};

/// Time constant with which the detector charges towards a signal above
/// it, in seconds. Chosen so that a 5 ms burst of 5 kHz tone reads 2 dB
/// under a steady tone, which is how IEC 60268-10 defines the 5 ms
/// integration time of a Type I meter.
const ATTACK_SECONDS: f64 = 1.361e-3;

/// Time constant with which the detector discharges, in seconds: a fall of
/// 20 dB, a factor of ten, in the 1.7 s return time of a Type I meter.
const RETURN_SECONDS: f64 = 1.7 / core::f64::consts::LN_10;

/// The readings of a [`PpmMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates. A channel the meter does not have reads as silence.
#[derive(Debug, Clone)]
pub struct PpmReadings {
    shared: Arc<PpmShared>,
}

#[derive(Debug)]
struct PpmShared {
    level: Row,
    reset: Flag,
}

impl PpmReadings {
    /// How many channels the meter measures.
    pub fn num_channels(&self) -> usize {
        self.shared.level.len()
    }

    /// The reading of `channel` as a linear amplitude. A steady sine reads
    /// its peak amplitude, as long as its samples fall on every part of
    /// its cycle. The meter reads the samples, not the waveform between
    /// them: a tone whose samples keep landing on the same few points
    /// reads the highest of those points, which is up to 3 dB under the
    /// peak at a quarter of the sample rate. See the limits of
    /// [`PpmMeter`].
    pub fn level(&self, channel: usize) -> f64 {
        self.shared.level.get(channel)
    }

    /// [`level`](Self::level) in dBFS, floored at -100. A steady sine at
    /// -18 dBFS reads -18, with the exception described for
    /// [`level`](Self::level).
    pub fn level_db(&self, channel: usize) -> f64 {
        db(self.level(channel))
    }

    /// Asks the meter to clear its readings. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// A quasi-peak programme meter with IEC 60268-10 Type I ballistics.
///
/// A peak programme meter sits between a VU meter and a sample-peak meter.
/// It rises fast, but not instantly, so that a click too short to hear as
/// loud does not send it to the top, and it falls slowly enough to be read.
/// Broadcasters have aligned levels with it since the 1930s.
///
/// This is the Type I meter of IEC 60268-10 (the DIN and Nordic scales use
/// it), with these constants:
///
/// | | |
/// |---|---|
/// | Integration time | 5 ms: a 5 ms burst of 5 kHz tone reads 2 dB under a steady tone |
/// | Return time | 1.7 s for a fall of 20 dB |
///
/// It is built the way the analogue instrument is: the rectified signal
/// charges a capacitor through one resistance (a time constant of 1.36 ms)
/// and the capacitor discharges all the while through a much larger one
/// (0.738 s). With that single pair of constants the other burst durations
/// the standard lists come out as measured in this crate's tests: 10 ms
/// reads -0.7 dB (nominal -1), 3 ms reads -3.6 dB (nominal -4) and 0.4 ms
/// reads -15.6 dB (nominal -15).
///
/// Such a detector settles a little under the peaks of a steady tone, by
/// 0.18 dB with these constants, because it discharges between them. An
/// instrument is lined up on a steady tone, which takes that out; here
/// [`prepare`](Self::prepare) does the same by running the detector on a
/// 1 kHz sine and dividing by what it reads. Steady tones from 50 Hz to
/// 5 kHz then read within 0.05 dB of their peak level, provided their
/// samples fall on every part of the cycle; the first of the limits below
/// is about the tones whose samples do not.
///
/// # Limits
///
/// The meter reads the samples, not the waveform between them, where the
/// analogue instrument reads the waveform. For most tones that makes no
/// difference: their samples land on every part of the cycle in turn. A
/// tone at a simple fraction of the sample rate keeps presenting the same
/// few points of its cycle, and when none of them is on a crest the
/// reading is low by as much as a [`PeakMeter`](crate::PeakMeter)'s would
/// be. At `p/q` of the sample rate, in lowest terms, the highest sample
/// can be as low as `cos(pi / q)` of the peak for an even `q` and
/// `cos(pi / 2q)` of it for an odd one: 3.0 dB down at a quarter of the
/// sample rate, 1.25 dB at a third or a sixth, 0.7 dB at an eighth and
/// 0.3 dB at a twelfth, which is 4 kHz at 48 kHz. A tone close to such a
/// frequency drifts slowly from its best case to its worst and back, and
/// the reading goes with it.
///
/// Measured at 48 kHz with full-scale sines: 12 kHz sampled 45 degrees
/// either side of its crests reads -2.85 dB, and 8 kHz and 16 kHz started
/// at a zero crossing read -1.10 dB. The reading is 0.15 dB above the
/// highest sample in these cases, and the same tones with a sample on a
/// crest read 0.13 dB to 0.14 dB above their peak: samples this few to
/// the cycle leave the detector no time to discharge, and the line-up
/// assumes it has some. Where the peak between the samples matters, use
/// [`TruePeakMeter`](crate::TruePeakMeter).
///
/// A sample that is not finite is read as silence and one beyond 180 dB
/// over full scale as 180 dB over, so no input leaves the meter stuck (see
/// the [crate documentation](crate#bad-samples)). One sample at that limit
/// charges the detector like any other: at 48 kHz it lifts the reading to
/// +144 dBFS, and the reading is back under full scale 12.2 s later.
///
/// Only Type I is offered. Type II (the BBC and EBU scales: 10 ms
/// integration, 24 dB in 2.8 s) is not: one resistor and one capacitor
/// match its 10 ms and 5 ms figures but miss its 1.5 ms and 0.5 ms figures
/// by about 2 dB, and a meter that does not meet its own table should not
/// carry the name.
///
/// The burst figures above are nominal values quoted from secondary
/// sources; the text of IEC 60268-10 and its tolerances were not available
/// when this was written.
///
/// The readings are levels in dBFS. The scales printed on instruments (DIN
/// -50 to +5, Nordic -42 to +12) and their alignment to a digital level
/// are left to whoever draws the meter.
///
/// ```
/// use manguebyte_analysis::PpmMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = PpmMeter::new(1);
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 24_000, num_channels: 1 });
///
/// // Half a second of a 1 kHz sine at -18 dBFS.
/// let amplitude = 10f32.powf(-18.0 / 20.0);
/// let mut block = AudioBuffer::<f32>::new(1, 24_000);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = amplitude * (core::f32::consts::TAU * n as f32 / 48.0).sin();
/// }
/// meter.measure(&block);
/// assert!((meter.readings().level_db(0) + 18.0).abs() < 0.05);
/// ```
#[derive(Debug)]
pub struct PpmMeter {
    sample_rate: f64,
    detector: Detector,
    /// What the detector's output is multiplied by so that a steady tone
    /// reads its peak.
    calibration: f64,
    charges: Vec<f64>,
    readings: PpmReadings,
}

impl PpmMeter {
    /// Creates a meter for `channels` channels. This allocates; nothing
    /// after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new(channels: usize) -> Self {
        let detector = Detector::new(DEFAULT_SAMPLE_RATE);
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            detector,
            calibration: detector.calibration(DEFAULT_SAMPLE_RATE),
            charges: vec![0.0; channels],
            readings: PpmReadings {
                shared: Arc::new(PpmShared {
                    level: Row::new(channels),
                    reset: Flag::default(),
                }),
            },
        }
    }

    /// Takes the sample rate from `spec`, lines the meter up for it and
    /// clears the readings. The channel count was fixed by
    /// [`new`](Self::new).
    ///
    /// Lining up runs the detector over a quarter of a second of tone, a
    /// few thousand samples. It does not allocate, but like any `prepare`
    /// it is not meant for the audio thread.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.detector = Detector::new(self.sample_rate);
        self.calibration = self.detector.calibration(self.sample_rate);
        self.reset();
    }

    /// Clears every reading.
    pub fn reset(&mut self) {
        self.charges.fill(0.0);
        self.publish();
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &PpmReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// Channels the meter was not created for are ignored, and a channel
    /// missing from the block keeps its reading as it is.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.charges.fill(0.0);
        }
        let detector = self.detector;
        for (state, channel) in self.charges.iter_mut().zip(buffer.iter_channels()) {
            let mut charge = *state;
            for &sample in channel {
                charge = detector.step(charge, screened(sample.to_f64()).abs());
            }
            *state = charge;
        }
        self.publish();
    }

    fn publish(&self) {
        for (channel, &charge) in self.charges.iter().enumerate() {
            self.readings
                .shared
                .level
                .set(channel, charge * self.calibration);
        }
    }
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for PpmMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        PpmMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        PpmMeter::reset(self);
    }
}

/// The rectifier, capacitor and two resistors of a quasi-peak detector.
#[derive(Debug, Clone, Copy)]
struct Detector {
    /// The share of the distance to a higher input covered in one sample.
    attack: f64,
    /// What the charge is multiplied by on each sample.
    release: f64,
}

impl Detector {
    fn new(sample_rate: f64) -> Self {
        Self {
            attack: 1.0 - (-1.0 / (ATTACK_SECONDS * sample_rate)).exp(),
            release: (-1.0 / (RETURN_SECONDS * sample_rate)).exp(),
        }
    }

    /// The charge after one more rectified sample.
    #[inline]
    fn step(self, charge: f64, magnitude: f64) -> f64 {
        let charge = charge * self.release;
        if magnitude > charge {
            charge + self.attack * (magnitude - charge)
        } else {
            charge
        }
    }

    /// The factor that makes a steady tone read its peak: one over what
    /// the detector settles at for a sine of amplitude one.
    fn calibration(self, sample_rate: f64) -> f64 {
        // 1 kHz where the rate allows it, and never fewer than eight
        // samples to the cycle.
        let frequency = 1_000.0f64.min(sample_rate / 8.0);
        // A quarter of a second is some two hundred attack time constants.
        // The count is capped so that an absurd rate cannot stall
        // `prepare`.
        let frames = ((0.25 * sample_rate) as usize).clamp(64, 200_000);
        let settled = frames - frames / 5;
        let (mut charge, mut highest) = (0.0, 0.0f64);
        for frame in 0..frames {
            let sample = (TAU * frequency * frame as f64 / sample_rate).sin();
            charge = self.step(charge, sample.abs());
            if frame >= settled {
                highest = highest.max(charge);
            }
        }
        if highest > 0.0 { 1.0 / highest } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(sample_rate: f64) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 4_800,
            num_channels: 1,
        }
    }

    fn tone(frequency: f64, dbfs: f64, seconds: f64, sample_rate: f64) -> AudioBuffer<f64> {
        let frames = (seconds * sample_rate).round() as usize;
        let amplitude = 10f64.powf(dbfs / 20.0);
        let mut buffer = AudioBuffer::new(1, frames);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = amplitude * (TAU * frequency * n as f64 / sample_rate).sin();
        }
        buffer
    }

    /// The highest reading while `signal` plays, sampled every sample.
    fn highest_reading(meter: &mut PpmMeter, signal: &AudioBuffer<f64>) -> f64 {
        let mut highest = 0.0f64;
        let mut one = AudioBuffer::<f64>::new(1, 1);
        for &sample in signal.channel(0) {
            one.channel_mut(0)[0] = sample;
            meter.measure(&one);
            highest = highest.max(meter.readings().level(0));
        }
        highest
    }

    /// What a burst of 5 kHz tone reads, in decibels under a steady tone of
    /// the same level.
    fn burst_db(sample_rate: f64, seconds: f64) -> f64 {
        let mut meter = PpmMeter::new(1);
        meter.prepare(&spec(sample_rate));
        let steady = highest_reading(&mut meter, &tone(5_000.0, 0.0, 0.5, sample_rate));
        meter.reset();
        let burst = highest_reading(&mut meter, &tone(5_000.0, 0.0, seconds, sample_rate));
        db(burst) - db(steady)
    }

    #[test]
    fn a_steady_tone_reads_its_peak_level() {
        for sample_rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
            // Lined up at 1 kHz, so exact there. Lower tones leave more time
            // to discharge between peaks, and the top of the ripple is what
            // is read.
            for (frequency, level, allowed) in [
                (1_000.0, -18.0, 0.002),
                (1_000.0, 0.0, 0.002),
                (5_000.0, -9.0, 0.02),
                (100.0, -9.0, 0.05),
                (50.0, -9.0, 0.05),
            ] {
                let mut meter = PpmMeter::new(1);
                meter.prepare(&spec(sample_rate));
                let reading = db(highest_reading(
                    &mut meter,
                    &tone(frequency, level, 0.5, sample_rate),
                ));
                assert!(
                    (reading - level).abs() < allowed,
                    "{sample_rate}, {frequency} Hz: {reading:.4} for {level}"
                );
            }
        }
    }

    /// A full-scale sine at `numerator / denominator` of 48 kHz that starts
    /// at `phase` radians, one second of it.
    fn locked_tone(numerator: u32, denominator: u32, phase: f64) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(1, 48_000);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            // Counted in whole steps around the cycle, so that the same
            // points of it come back exactly.
            let position = (n as u64 * u64::from(numerator)) % u64::from(denominator);
            *sample = (TAU * position as f64 / f64::from(denominator) + phase).sin();
        }
        buffer
    }

    #[test]
    fn a_tone_locked_to_the_sample_rate_reads_its_highest_sample_not_its_peak() {
        // The first limit in the documentation. A sine at `p / q` of the
        // sample rate presents `q` points of its cycle for ever. With the
        // phase that puts a crest midway between two of them, the highest
        // sample is `cos(pi / q)` of the peak for an even `q` and
        // `cos(pi / 2q)` for an odd one, and that is what the meter reads:
        // it has no way to know what the waveform does in between.
        use core::f64::consts::PI;
        for (p, q, in_words) in [
            (1u32, 4u32, "3.0 dB at a quarter of the sample rate"),
            (1, 3, "1.25 dB at a third"),
            (1, 6, "1.25 dB at a sixth"),
            (1, 8, "0.7 dB at an eighth"),
            (1, 12, "0.3 dB at a twelfth"),
            (1, 5, "0.4 dB at a fifth"),
            (3, 8, "0.7 dB at three eighths"),
            (1, 48, "0.02 dB at 1 kHz"),
        ] {
            let between = if q % 2 == 0 {
                PI / f64::from(q)
            } else {
                PI / (2.0 * f64::from(q))
            };
            let expected = db(between.cos());
            // The worst case: a crest midway between two samples.
            let signal = locked_tone(p, q, PI / 2.0 - between);
            let highest_sample = db(signal.peak());
            assert!(
                (highest_sample - expected).abs() < 1e-9,
                "{p}/{q}: the highest sample is at {highest_sample} dB, not {expected}"
            );

            let mut meter = PpmMeter::new(1);
            meter.prepare(&spec(48_000.0));
            meter.measure(&signal);
            let reading = meter.readings().level_db(0);
            // The reading is the highest sample, and up to 0.2 dB more:
            // samples this few to the cycle leave the detector no time to
            // discharge, and the line-up assumes it has some.
            assert!(
                (-0.05..0.2).contains(&(reading - highest_sample)),
                "{p}/{q}: reads {reading:.3} dB with its highest sample at {highest_sample:.3}"
            );

            // The best case of the same tone has a sample on a crest, and
            // reads the peak with the same excess.
            let mut meter = PpmMeter::new(1);
            meter.prepare(&spec(48_000.0));
            meter.measure(&locked_tone(p, q, PI / 2.0));
            let best = meter.readings().level_db(0);
            println!(
                "{p}/{q} of 48 kHz at 0 dBFS: highest sample {highest_sample:.2} dB and reading \
                 {reading:.2} dB at the worst phase, reading {best:+.2} dB at the best \
                 ({in_words})"
            );
            assert!((-0.05..0.2).contains(&best), "{p}/{q}: {best:.3} dB");
        }

        // The cases the documentation quotes with their readings.
        for (p, q, phase, quoted) in [
            (1, 4, PI / 4.0, -2.85),
            (1, 6, 0.0, -1.10),
            (1, 3, 0.0, -1.10),
        ] {
            let mut meter = PpmMeter::new(1);
            meter.prepare(&spec(48_000.0));
            meter.measure(&locked_tone(p, q, phase));
            let reading = meter.readings().level_db(0);
            assert!(
                (reading - quoted).abs() < 0.01,
                "{p}/{q}: {reading:.3} dB, quoted as {quoted}"
            );
            // "0.15 dB above the highest sample", and with a sample on a
            // crest "0.13 dB to 0.14 dB above their peak".
            let highest_sample = db(locked_tone(p, q, phase).peak());
            assert!(
                (reading - highest_sample - 0.155).abs() < 0.01,
                "{p}/{q}: {reading:.3} dB with its highest sample at {highest_sample:.3}"
            );
            let mut meter = PpmMeter::new(1);
            meter.prepare(&spec(48_000.0));
            meter.measure(&locked_tone(p, q, PI / 2.0));
            let best = meter.readings().level_db(0);
            assert!((0.125..0.145).contains(&best), "{p}/{q}: {best:.3} dB");
        }
    }

    #[test]
    fn a_tone_near_a_fraction_of_the_sample_rate_drifts_between_its_best_and_worst_case() {
        // A tenth of a hertz above a quarter of the sample rate, the
        // samples take ten seconds to drift once through the cycle. They
        // start on the crests, are 45 degrees off them after 1.25 s and
        // back on them after 2.5 s.
        let sample_rate = 48_000.0;
        let mut meter = PpmMeter::new(1);
        meter.prepare(&spec(sample_rate));
        let signal = tone(12_000.1, 0.0, 3.0, sample_rate);
        let mut block = AudioBuffer::<f64>::new(1, 480);
        let mut trace = Vec::new();
        for chunk in signal.channel(0).chunks_exact(480) {
            block.channel_mut(0).copy_from_slice(chunk);
            meter.measure(&block);
            trace.push(meter.readings().level_db(0));
        }
        let (worst, best) = (trace[124], trace[249]);
        println!(
            "12,000.1 Hz at 0 dBFS reads {worst:.2} dB after 1.25 s and {best:.2} dB after 2.5 s"
        );
        assert!((worst + 2.85).abs() < 0.05, "{worst}");
        assert!((-0.05..0.2).contains(&best), "{best}");
        // And nothing in between is outside those two.
        for &reading in &trace[50..] {
            assert!((worst - 0.05..0.2).contains(&reading), "{reading}");
        }
    }

    #[test]
    fn the_integration_time_is_five_milliseconds() {
        // IEC 60268-10 Type I: a 5 ms burst of 5 kHz reads 2 dB low.
        for sample_rate in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let reading = burst_db(sample_rate, 0.005);
            assert!(
                (reading + 2.0).abs() < 0.1,
                "{sample_rate}: {reading:.2} dB"
            );
        }
    }

    #[test]
    fn other_bursts_read_near_their_nominal_values() {
        // Duration in seconds, the nominal reading, and what this model
        // reads, which is what the documentation of the meter quotes.
        for (seconds, nominal, ours) in [
            (0.010, -1.0, -0.72),
            (0.003, -4.0, -3.62),
            (0.0004, -15.0, -15.6),
        ] {
            for sample_rate in [44_100.0, 48_000.0, 96_000.0] {
                let reading = burst_db(sample_rate, seconds);
                println!("{seconds} s at {sample_rate} Hz: {reading:.3} dB (nominal {nominal})");
                assert!(
                    (reading - ours).abs() < 0.05,
                    "{seconds} s at {sample_rate}: {reading:.2} dB, not {ours}"
                );
                assert!((reading - nominal).abs() < 0.7);
            }
        }
    }

    #[test]
    fn the_reading_returns_twenty_decibels_in_one_point_seven_seconds() {
        let sample_rate = 48_000.0;
        let mut meter = PpmMeter::new(1);
        meter.prepare(&spec(sample_rate));
        meter.measure(&tone(1_000.0, 0.0, 0.5, sample_rate));
        let before = meter.readings().level_db(0);
        meter.measure(&AudioBuffer::<f64>::new(
            1,
            (1.7 * sample_rate).round() as usize,
        ));
        let after = meter.readings().level_db(0);
        assert!((before - after - 20.0).abs() < 1e-6, "{before} to {after}");
    }

    #[test]
    fn a_single_sample_does_not_reach_the_top() {
        // What sets this meter apart from a sample-peak meter: one sample
        // at full scale reads far below full scale.
        let mut meter = PpmMeter::new(1);
        meter.prepare(&spec(48_000.0));
        let mut click = AudioBuffer::<f32>::new(1, 1);
        click.channel_mut(0)[0] = 1.0;
        meter.measure(&click);
        let reading = meter.readings().level_db(0);
        assert!(reading < -30.0 && reading > -40.0, "{reading}");
    }

    #[test]
    fn a_request_or_a_bad_sample_does_not_upset_the_meter() {
        let mut meter = PpmMeter::new(2);
        meter.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 64,
            num_channels: 2,
        });
        let mut buffer = AudioBuffer::<f32>::new(2, 64);
        buffer.channel_mut(0).fill(0.1);
        buffer.channel_mut(1).fill(f32::INFINITY);
        meter.measure(&buffer);
        let readings = meter.readings().clone();
        assert_eq!(readings.num_channels(), 2);
        assert!(readings.level(0) > 0.0);
        assert_eq!((readings.level(1), readings.level(2)), (0.0, 0.0));

        readings.request_reset();
        meter.measure(&AudioBuffer::<f32>::new(2, 1));
        assert_eq!(readings.level(0), 0.0);

        // Rates no sound card has still give a usable meter.
        for rate in [0.0, 50.0, 2_000.0, 1e9, f64::NAN] {
            meter.prepare(&ProcessSpec {
                sample_rate: rate,
                max_block_size: 64,
                num_channels: 2,
            });
            meter.measure(&buffer);
            assert!(meter.readings().level(0).is_finite());
        }
    }
}
