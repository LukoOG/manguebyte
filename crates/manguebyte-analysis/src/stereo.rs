//! Correlation and balance of a stereo pair.

use core::sync::atomic::Ordering;
use std::sync::Arc;

use manguebyte_core::{AtomicF64, AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, db, screened, smoothing_factor, usable_rate};

/// Mean square below which a channel counts as silent: -100 dB, the floor
/// the framework uses everywhere.
const SILENCE: f64 = 1e-10;

/// The readings of a [`StereoMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates.
#[derive(Debug, Clone)]
pub struct StereoReadings {
    shared: Arc<StereoShared>,
}

#[derive(Debug)]
struct StereoShared {
    /// Smoothed mean of left times left, right times right, left times
    /// right.
    left: AtomicF64,
    right: AtomicF64,
    cross: AtomicF64,
    reset: Flag,
}

impl StereoReadings {
    /// How alike the two channels are, from -1 to +1.
    ///
    /// +1 means the channels are the same signal, at any two levels: it
    /// would survive being summed to mono. 0 means they are unrelated, or
    /// a quarter of a cycle apart. -1 means one is the other inverted, and
    /// a mono sum would cancel. While either channel is silent the reading
    /// is 0.
    ///
    /// The result is always a number in that range, whatever the meter was
    /// given.
    pub fn correlation(&self) -> f64 {
        let (left, right) = self.mean_squares();
        // Written so that a level that is not a number counts as silent.
        let both_sound = left > SILENCE && right > SILENCE;
        if !both_sound {
            return 0.0;
        }
        let cross = self.shared.cross.load(Ordering::Relaxed);
        // The meter screens its input, so a mean square is at most 1e18
        // and the product cannot overflow. Should the three values ever
        // not make a number all the same, the reading is 0, not NaN.
        let correlation = cross / (left * right).sqrt();
        if correlation.is_nan() {
            0.0
        } else {
            correlation.clamp(-1.0, 1.0)
        }
    }

    /// Where the sound sits between the channels, from -1 (all left) to
    /// +1 (all right), as `(R - L) / (R + L)` of the two RMS levels. While
    /// both channels are silent the reading is 0.
    ///
    /// The result is always a number in that range, whatever the meter was
    /// given.
    pub fn balance(&self) -> f64 {
        let (left, right) = self.mean_squares();
        let either_sounds = left > SILENCE || right > SILENCE;
        if !either_sounds {
            return 0.0;
        }
        let (left, right) = (left.sqrt(), right.sqrt());
        let balance = (right - left) / (right + left);
        if balance.is_nan() {
            0.0
        } else {
            balance.clamp(-1.0, 1.0)
        }
    }

    /// How much louder the right channel is than the left, in decibels.
    /// Each level is floored at -100 dBFS first, so the reading is 0 for
    /// silence and always a number. With neither channel above full scale
    /// it stays within plus or minus 100; a channel above full scale
    /// against a silent one reads more than that.
    pub fn level_difference_db(&self) -> f64 {
        let (left, right) = self.mean_squares();
        db(right.sqrt()) - db(left.sqrt())
    }

    /// Asks the meter to clear its readings. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }

    fn mean_squares(&self) -> (f64, f64) {
        (
            self.shared.left.load(Ordering::Relaxed),
            self.shared.right.load(Ordering::Relaxed),
        )
    }
}

/// A phase-correlation and balance meter for two channels.
///
/// It keeps three running averages: the square of the left channel, the
/// square of the right, and their product. Correlation is the product's
/// average over the geometric mean of the other two, which is the cosine of
/// the angle between the channels seen as vectors; balance compares the two
/// levels. The averages are exponential with a time constant of 300 ms by
/// default.
///
/// By default it reads channels 0 and 1.
/// [`with_channels`](Self::with_channels) picks another pair out of a wider
/// buffer.
///
/// # Limits
///
/// The three averages are published one after another, so a reader may
/// combine values from two consecutive blocks. Correlation and balance are
/// clamped to the range they must lie in, and are never NaN.
///
/// A sample that is not finite is read as silence and one beyond 180 dB
/// over full scale as 180 dB over, so no input leaves the meter stuck (see
/// the [crate documentation](crate#bad-samples)). One sample at that limit
/// in one channel is averaged like any other, and until its share of that
/// channel's average has faded the balance leans to that channel and the
/// correlation reads low. At 48 kHz and the default time constant, with a
/// tone at -20 dBFS in both channels, the correlation is back above 0.99
/// after 12.3 s and the balance within 0.01 of the middle after 12.1 s.
///
/// Balance is defined on amplitudes, so a sound panned with a linear law
/// reads its pan position exactly. With a constant-power law a sound
/// panned halfway to one side reads 0.41, not 0.5.
///
/// ```
/// use manguebyte_analysis::StereoMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = StereoMeter::new();
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 48_000, num_channels: 2 });
///
/// // The same sine in both channels, the right one inverted and at half
/// // the level.
/// let mut block = AudioBuffer::<f32>::new(2, 48_000);
/// for n in 0..48_000 {
///     let sample = (core::f32::consts::TAU * n as f32 / 48.0).sin();
///     block.channel_mut(0)[n] = 0.5 * sample;
///     block.channel_mut(1)[n] = -0.25 * sample;
/// }
/// meter.measure(&block);
///
/// let readings = meter.readings();
/// assert!((readings.correlation() + 1.0).abs() < 1e-6);
/// assert!((readings.balance() + 1.0 / 3.0).abs() < 1e-3);
/// assert!((readings.level_difference_db() + 6.02).abs() < 0.01);
/// ```
#[derive(Debug)]
pub struct StereoMeter {
    sample_rate: f64,
    time_constant: f64,
    factor: f64,
    pair: (usize, usize),
    /// Smoothed left square, right square and product.
    averages: [f64; 3],
    readings: StereoReadings,
}

impl Default for StereoMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoMeter {
    /// Creates a meter for channels 0 and 1. This allocates; nothing after
    /// it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new() -> Self {
        let time_constant = 0.3;
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            time_constant,
            factor: smoothing_factor(time_constant, DEFAULT_SAMPLE_RATE),
            pair: (0, 1),
            averages: [0.0; 3],
            readings: StereoReadings {
                shared: Arc::new(StereoShared {
                    left: AtomicF64::new(0.0),
                    right: AtomicF64::new(0.0),
                    cross: AtomicF64::new(0.0),
                    reset: Flag::default(),
                }),
            },
        }
    }

    /// Chooses which two channels of the buffer are compared.
    pub fn with_channels(mut self, left: usize, right: usize) -> Self {
        self.pair = (left, right);
        self
    }

    /// Sets the averaging time constant in seconds. Zero makes the readings
    /// follow each sample.
    pub fn with_time_constant(mut self, seconds: f64) -> Self {
        self.set_time_constant(seconds);
        self
    }

    /// Changes the averaging time constant.
    /// See [`with_time_constant`](Self::with_time_constant).
    pub fn set_time_constant(&mut self, seconds: f64) {
        self.time_constant = seconds;
        self.factor = smoothing_factor(seconds, self.sample_rate);
    }

    /// Takes the sample rate from `spec` and clears the readings.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.factor = smoothing_factor(self.time_constant, self.sample_rate);
        self.reset();
    }

    /// Clears the readings.
    pub fn reset(&mut self) {
        self.averages = [0.0; 3];
        self.publish();
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &StereoReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// A block that does not have both channels of the pair leaves the
    /// readings as they are.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.averages = [0.0; 3];
        }
        let (left, right) = self.pair;
        if left < buffer.num_channels() && right < buffer.num_channels() {
            let factor = self.factor;
            let [mut left_square, mut right_square, mut product] = self.averages;
            for (&l, &r) in buffer.channel(left).iter().zip(buffer.channel(right)) {
                let (l, r) = (screened(l.to_f64()), screened(r.to_f64()));
                left_square += factor * (l * l - left_square);
                right_square += factor * (r * r - right_square);
                product += factor * (l * r - product);
            }
            self.averages = [left_square, right_square, product];
        }
        self.publish();
    }

    fn publish(&self) {
        let shared = &self.readings.shared;
        shared.left.store(self.averages[0], Ordering::Relaxed);
        shared.right.store(self.averages[1], Ordering::Relaxed);
        shared.cross.store(self.averages[2], Ordering::Relaxed);
    }
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for StereoMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        StereoMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        StereoMeter::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

    use super::*;

    const RATE: f64 = 48_000.0;

    fn spec() -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 48_000,
            num_channels: 2,
        }
    }

    /// One second of a 997 Hz sine at `left` in one channel and at `right`,
    /// shifted by `phase` radians, in the other.
    fn pair(left: f64, right: f64, phase: f64) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(2, 48_000);
        for n in 0..48_000 {
            let angle = TAU * 997.0 * n as f64 / RATE;
            buffer.channel_mut(0)[n] = left * angle.sin();
            buffer.channel_mut(1)[n] = right * (angle + phase).sin();
        }
        buffer
    }

    fn measured(signal: &AudioBuffer<f64>) -> StereoReadings {
        // A short time constant, so that one second is fully settled.
        let mut meter = StereoMeter::new().with_time_constant(0.05);
        meter.prepare(&spec());
        meter.measure(signal);
        meter.readings().clone()
    }

    #[test]
    fn correlation_is_the_cosine_of_the_phase_between_the_channels() {
        for degrees in [0.0, 30.0, 60.0, 90.0, 120.0, 180.0] {
            let phase = degrees * PI / 180.0;
            // The levels do not matter. What is left is the ripple of a
            // 50 ms average at twice 997 Hz, 0.16% at most.
            let correlation = measured(&pair(0.8, 0.1, phase)).correlation();
            assert!(
                (correlation - phase.cos()).abs() < 3e-3,
                "{degrees} degrees: {correlation}"
            );
        }
        assert_eq!(measured(&pair(0.5, 0.5, 0.0)).correlation(), 1.0);
        assert_eq!(measured(&pair(0.5, -0.5, 0.0)).correlation(), -1.0);
    }

    #[test]
    fn unrelated_signals_have_no_correlation() {
        // Two tones of different pitch.
        let mut buffer = AudioBuffer::<f64>::new(2, 48_000);
        for n in 0..48_000 {
            buffer.channel_mut(0)[n] = (TAU * 440.0 * n as f64 / RATE).sin();
            buffer.channel_mut(1)[n] = (TAU * 1_234.0 * n as f64 / RATE).sin();
        }
        let correlation = measured(&buffer).correlation();
        assert!(correlation.abs() < 0.01, "{correlation}");
    }

    #[test]
    fn balance_is_the_pan_position_of_a_linear_pan() {
        for position in [-1.0, -0.5, 0.0, 0.25, 1.0] {
            let (left, right) = (0.5 * (1.0 - position), 0.5 * (1.0 + position));
            let readings = measured(&pair(left, right, 0.0));
            assert!(
                (readings.balance() - position).abs() < 1e-9,
                "{position}: {}",
                readings.balance()
            );
        }
        // Constant power, halfway to the right.
        let angle = FRAC_PI_4 * 1.5;
        let readings = measured(&pair(angle.cos(), angle.sin(), 0.0));
        assert!((readings.balance() - 0.414).abs() < 1e-3);
        assert!((readings.level_difference_db() - 7.656).abs() < 1e-3);
    }

    #[test]
    fn silence_reads_as_the_middle() {
        let silent = measured(&AudioBuffer::<f64>::new(2, 48_000));
        assert_eq!(silent.correlation(), 0.0);
        assert_eq!(silent.balance(), 0.0);
        assert_eq!(silent.level_difference_db(), 0.0);

        // One channel only: no correlation to speak of, hard to one side.
        let left_only = measured(&pair(0.5, 0.0, 0.0));
        assert_eq!(left_only.correlation(), 0.0);
        assert_eq!(left_only.balance(), -1.0);
        assert!(left_only.level_difference_db() < -90.0);
    }

    #[test]
    fn the_time_constant_is_the_time_to_63_percent() {
        let mut meter = StereoMeter::new();
        meter.prepare(&spec());
        // A constant in both channels for one time constant, 300 ms.
        let mut step = AudioBuffer::<f64>::new(2, 14_400);
        step.channel_mut(0).fill(1.0);
        step.channel_mut(1).fill(1.0);
        meter.measure(&step);
        // The averages themselves, which the readings only show as ratios.
        let shared = &meter.readings().shared;
        let expected = 1.0 - (-1.0f64).exp();
        assert!((shared.left.load(Ordering::Relaxed) - expected).abs() < 1e-9);
        assert!((shared.cross.load(Ordering::Relaxed) - expected).abs() < 1e-9);
        assert_eq!(meter.readings().correlation(), 1.0);
    }

    #[test]
    fn another_pair_can_be_chosen_and_a_missing_one_is_ignored() {
        let mut meter = StereoMeter::new().with_channels(2, 3);
        Processor::<f64>::prepare(&mut meter, &spec());
        let mut buffer = AudioBuffer::<f64>::new(4, 4_800);
        for n in 0..4_800 {
            let sample = (FRAC_PI_2 * 0.1 * n as f64).sin();
            buffer.channel_mut(0)[n] = sample;
            buffer.channel_mut(2)[n] = sample;
            buffer.channel_mut(3)[n] = -sample;
        }
        let original = buffer.clone();
        meter.process(&mut buffer);
        assert!(buffer == original);
        assert_eq!(meter.readings().correlation(), -1.0);

        // A stereo block has no channels 2 and 3.
        meter.measure(&AudioBuffer::<f32>::new(2, 64));
        assert_eq!(meter.readings().correlation(), -1.0);

        meter.readings().request_reset();
        meter.measure(&AudioBuffer::<f32>::new(2, 64));
        assert_eq!(meter.readings().correlation(), 0.0);
        Processor::<f32>::reset(&mut meter);
        assert_eq!(StereoMeter::default().readings().balance(), 0.0);
    }

    #[test]
    fn the_readings_are_numbers_whatever_the_averages_hold() {
        // The meter screens its input, so its averages are always finite.
        // The readings do not rely on that: whatever the three shared
        // values are, correlation and balance are numbers in their range.
        let meter = StereoMeter::new();
        let readings = meter.readings().clone();
        let strange = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            -1.0,
            0.0,
            1e-300,
            0.25,
        ];
        for left in strange {
            for right in strange {
                for cross in strange {
                    let shared = &readings.shared;
                    shared.left.store(left, Ordering::Relaxed);
                    shared.right.store(right, Ordering::Relaxed);
                    shared.cross.store(cross, Ordering::Relaxed);
                    let (correlation, balance) = (readings.correlation(), readings.balance());
                    assert!(
                        (-1.0..=1.0).contains(&correlation),
                        "{left}, {right}, {cross}: correlation {correlation}"
                    );
                    assert!(
                        (-1.0..=1.0).contains(&balance),
                        "{left}, {right}, {cross}: balance {balance}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_bad_sample_does_not_stick() {
        let mut meter = StereoMeter::new();
        meter.prepare(&spec());
        let mut buffer = AudioBuffer::<f32>::new(2, 8);
        buffer.channel_mut(0).fill(f32::NAN);
        buffer.channel_mut(1).fill(0.5);
        meter.measure(&buffer);
        assert_eq!(meter.readings().correlation(), 0.0);
        assert!(meter.readings().balance().is_finite());
    }
}
