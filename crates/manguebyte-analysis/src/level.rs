//! Sample-peak and RMS level.

use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, Row, db, screened, smoothing_factor, usable_rate};

/// How fast a peak reading falls back by default: 20 dB in 1.7 s, the
/// return time IEC 60268-18 gives for digital peak level meters.
pub(crate) const DEFAULT_DECAY_DB_PER_SECOND: f64 = 20.0 / 1.7;

/// How long the hold marker stays on a peak by default, in seconds.
pub(crate) const DEFAULT_HOLD_SECONDS: f64 = 2.0;

/// The fall-back and hold of a peak reading, in samples.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeakTiming {
    /// What the level is multiplied by on each sample.
    decay: f64,
    /// Samples a held peak stays. `u64::MAX` means for ever.
    hold: u64,
}

impl PeakTiming {
    pub(crate) fn new(sample_rate: f64, decay_db_per_second: f64, hold_seconds: f64) -> Self {
        let decay_db = if decay_db_per_second > 0.0 {
            decay_db_per_second
        } else {
            0.0
        };
        let hold = if hold_seconds == f64::INFINITY {
            u64::MAX
        } else if hold_seconds > 0.0 {
            // The cast saturates, so an absurd time becomes "for ever".
            (hold_seconds * sample_rate).round() as u64
        } else {
            0
        };
        Self {
            decay: 10f64.powf(-decay_db / (20.0 * sample_rate)),
            hold,
        }
    }
}

/// One channel of a peak reading: a level that rises at once and falls
/// slowly, a marker that holds a peak for a while, and the largest value
/// since the last reset.
///
/// The marker is the usual peak hold, with one timer. A magnitude at or
/// above the marker takes it and starts the hold time. A smaller one
/// leaves the marker and its timer alone and is not held itself. When the
/// time runs out the marker drops to the level and falls with it, until a
/// magnitude reaches it again.
///
/// The sample-peak and the true-peak meter share it; they differ only in
/// the magnitudes they feed it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PeakFollower {
    pub(crate) level: f64,
    pub(crate) held: f64,
    pub(crate) max: f64,
    hold_left: u64,
}

impl PeakFollower {
    /// Takes the magnitude of one more sample.
    ///
    /// The magnitude must be finite: an infinite one would stay in the
    /// level for ever, since no decay brings infinity down. Both meters
    /// screen their input before it gets here. (A NaN fails every
    /// comparison below and would be ignored.)
    #[inline]
    pub(crate) fn push(&mut self, magnitude: f64, timing: &PeakTiming) {
        self.level *= timing.decay;
        if magnitude > self.level {
            self.level = magnitude;
        }
        if magnitude >= self.held {
            self.held = magnitude;
            self.hold_left = timing.hold;
        } else if self.hold_left == 0 {
            // The hold has run out: the marker falls back with the level.
            self.held = self.level;
        } else if self.hold_left != u64::MAX {
            self.hold_left -= 1;
        }
        if magnitude > self.max {
            self.max = magnitude;
        }
    }
}

/// What the peak meters publish, and the request to start again.
#[derive(Debug)]
pub(crate) struct PeakShared {
    pub(crate) level: Row,
    pub(crate) held: Row,
    pub(crate) max: Row,
    pub(crate) reset: Flag,
}

impl PeakShared {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            level: Row::new(channels),
            held: Row::new(channels),
            max: Row::new(channels),
            reset: Flag::default(),
        }
    }

    pub(crate) fn publish(&self, followers: &[PeakFollower]) {
        for (channel, follower) in followers.iter().enumerate() {
            self.level.set(channel, follower.level);
            self.held.set(channel, follower.held);
            self.max.set(channel, follower.max);
        }
    }
}

/// The readings of a [`PeakMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates. A channel the meter does not have reads as silence.
#[derive(Debug, Clone)]
pub struct PeakReadings {
    shared: Arc<PeakShared>,
}

impl PeakReadings {
    /// How many channels the meter measures.
    pub fn num_channels(&self) -> usize {
        self.shared.level.len()
    }

    /// The level of `channel` as a linear amplitude: it jumps to each peak
    /// and falls back at the meter's decay rate.
    pub fn level(&self, channel: usize) -> f64 {
        self.shared.level.get(channel)
    }

    /// [`level`](Self::level) in dBFS, floored at -100.
    pub fn level_db(&self, channel: usize) -> f64 {
        db(self.level(channel))
    }

    /// The hold marker of `channel` as a linear amplitude.
    ///
    /// A sample at or above the marker takes it and starts the hold time.
    /// When that time has passed without another sample reaching the
    /// marker, it drops to [`level`](Self::level) and falls with it.
    ///
    /// A smaller peak that arrives during a hold is not held itself, so
    /// this is not the largest sample of the last hold time: with a hold
    /// of two seconds, a peak of 1.0 followed 1.9 s later by one of 0.9
    /// leaves the marker at 1.0 until two seconds are up and then at the
    /// level, which by then has been falling from 0.9 for 100 ms.
    pub fn held(&self, channel: usize) -> f64 {
        self.shared.held.get(channel)
    }

    /// [`held`](Self::held) in dBFS, floored at -100.
    pub fn held_db(&self, channel: usize) -> f64 {
        db(self.held(channel))
    }

    /// The largest sample of `channel` since the last reset, as a linear
    /// amplitude.
    pub fn max(&self, channel: usize) -> f64 {
        self.shared.max.get(channel)
    }

    /// [`max`](Self::max) in dBFS, floored at -100.
    pub fn max_db(&self, channel: usize) -> f64 {
        db(self.max(channel))
    }

    /// The largest sample of any channel since the last reset.
    pub fn max_overall(&self) -> f64 {
        self.shared.max.max()
    }

    /// [`max_overall`](Self::max_overall) in dBFS, floored at -100.
    pub fn max_overall_db(&self) -> f64 {
        db(self.max_overall())
    }

    /// The highest hold marker of any channel, as a linear amplitude.
    pub fn held_overall(&self) -> f64 {
        self.shared.held.max()
    }

    /// [`held_overall`](Self::held_overall) in dBFS, floored at -100.
    pub fn held_overall_db(&self) -> f64 {
        db(self.held_overall())
    }

    /// Asks the meter to clear its readings. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// A sample-peak meter with fall-back and hold.
///
/// For each channel it gives three readings: a level that jumps to every
/// peak and falls back at a fixed rate in decibels per second, a hold
/// marker that stays on a peak for a while and then joins the level (see
/// [`PeakReadings::held`]), and the maximum since the last reset.
///
/// The defaults are a fall of 20 dB in 1.7 s, the return time IEC 60268-18
/// gives for digital peak meters, and a hold of two seconds.
///
/// # Limits
///
/// This reads the samples as they are. The waveform between two samples can
/// be higher, by 3 dB for a sine at a quarter of the sample rate and by
/// more for transients. [`TruePeakMeter`](crate::TruePeakMeter) measures
/// that.
///
/// A sample that is not finite is read as silence and one beyond 180 dB
/// over full scale as 180 dB over, so no input leaves the meter stuck (see
/// the [crate documentation](crate#bad-samples)). A sample at that limit is
/// still a peak: the marker holds it for its time, the level needs 15.3 s
/// at the default rate to fall back to full scale, and the maximum keeps it
/// until the next reset.
///
/// ```
/// use manguebyte_analysis::PeakMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = PeakMeter::new(2);
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 2 });
/// // The handle a user interface keeps. It reads from any thread.
/// let readings = meter.readings().clone();
///
/// let mut block = AudioBuffer::<f32>::new(2, 64);
/// block.channel_mut(0)[10] = -0.5;
/// meter.measure(&block);
///
/// assert_eq!(readings.max(0), 0.5);
/// assert!((readings.max_db(0) + 6.02).abs() < 0.01);
/// assert_eq!(readings.max(1), 0.0);
/// // 53 samples later the level has begun to fall; the marker has not.
/// assert!(readings.level(0) < 0.5 && readings.level(0) > 0.49);
/// assert_eq!(readings.held(0), 0.5);
/// ```
#[derive(Debug)]
pub struct PeakMeter {
    sample_rate: f64,
    decay_db_per_second: f64,
    hold_seconds: f64,
    timing: PeakTiming,
    followers: Vec<PeakFollower>,
    readings: PeakReadings,
}

impl PeakMeter {
    /// Creates a meter for `channels` channels. This allocates; nothing
    /// after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new(channels: usize) -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            decay_db_per_second: DEFAULT_DECAY_DB_PER_SECOND,
            hold_seconds: DEFAULT_HOLD_SECONDS,
            timing: PeakTiming::new(
                DEFAULT_SAMPLE_RATE,
                DEFAULT_DECAY_DB_PER_SECOND,
                DEFAULT_HOLD_SECONDS,
            ),
            followers: vec![PeakFollower::default(); channels],
            readings: PeakReadings {
                shared: Arc::new(PeakShared::new(channels)),
            },
        }
    }

    /// Sets how fast the level falls back, in decibels per second. Zero or
    /// less stops it falling at all.
    pub fn with_decay(mut self, db_per_second: f64) -> Self {
        self.set_decay(db_per_second);
        self
    }

    /// Sets how long the hold marker stays on a peak, in seconds.
    /// `f64::INFINITY` holds until the next reset.
    pub fn with_hold(mut self, seconds: f64) -> Self {
        self.set_hold(seconds);
        self
    }

    /// Changes the fall-back rate. See [`with_decay`](Self::with_decay).
    pub fn set_decay(&mut self, db_per_second: f64) {
        self.decay_db_per_second = db_per_second;
        self.update_timing();
    }

    /// Changes the hold time. See [`with_hold`](Self::with_hold).
    pub fn set_hold(&mut self, seconds: f64) {
        self.hold_seconds = seconds;
        self.update_timing();
    }

    /// Takes the sample rate from `spec` and clears the readings. The
    /// channel count was fixed by [`new`](Self::new).
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.update_timing();
        self.reset();
    }

    /// Clears every reading.
    pub fn reset(&mut self) {
        self.followers.fill(PeakFollower::default());
        self.readings.shared.publish(&self.followers);
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &PeakReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// Channels the meter was not created for are ignored, and a channel
    /// missing from the block keeps its readings as they are.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.followers.fill(PeakFollower::default());
        }
        let timing = self.timing;
        for (follower, channel) in self.followers.iter_mut().zip(buffer.iter_channels()) {
            let mut state = *follower;
            for &sample in channel {
                state.push(screened(sample.to_f64()).abs(), &timing);
            }
            *follower = state;
        }
        self.readings.shared.publish(&self.followers);
    }

    fn update_timing(&mut self) {
        self.timing = PeakTiming::new(
            self.sample_rate,
            self.decay_db_per_second,
            self.hold_seconds,
        );
    }
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for PeakMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        PeakMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        PeakMeter::reset(self);
    }
}

/// The readings of an [`RmsMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates. A channel the meter does not have reads as silence.
#[derive(Debug, Clone)]
pub struct RmsReadings {
    shared: Arc<RmsShared>,
}

#[derive(Debug)]
struct RmsShared {
    mean_square: Row,
    reset: Flag,
}

impl RmsReadings {
    /// How many channels the meter measures.
    pub fn num_channels(&self) -> usize {
        self.shared.mean_square.len()
    }

    /// The root-mean-square level of `channel` as a linear amplitude. A
    /// sine reads 0.707 times its peak.
    pub fn rms(&self, channel: usize) -> f64 {
        self.shared.mean_square.get(channel).sqrt()
    }

    /// [`rms`](Self::rms) in dBFS, floored at -100. A full-scale sine reads
    /// -3.01, a full-scale square wave 0.
    pub fn rms_db(&self, channel: usize) -> f64 {
        db(self.rms(channel))
    }

    /// Asks the meter to clear its readings. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// An RMS level meter.
///
/// The square of each sample goes through a one-pole low-pass, and the
/// reading is the square root of its output. The time constant, 300 ms by
/// default, is the time the mean square takes to cover 63% of a step.
///
/// # Limits
///
/// The reading is the plain root mean square: a full-scale sine reads
/// -3.01 dBFS. Meters that follow AES17 add 3.01 dB so that the same sine
/// reads 0 dBFS; add it to [`RmsReadings::rms_db`] if that is the
/// convention you want.
///
/// The averaging is exponential, not a rectangular window, so the mean
/// square of a steady tone ripples at twice the tone's frequency `f`, by
/// `1 / (4π f × time constant)` of itself: 1.3% at 20 Hz with the default
/// time constant, which is 0.06 dB up and down.
///
/// A sample that is not finite is read as silence and one beyond 180 dB
/// over full scale as 180 dB over, so no input leaves the meter stuck (see
/// the [crate documentation](crate#bad-samples)). One sample at that limit
/// is averaged like any other: at 48 kHz and the default time constant it
/// lifts the reading to +138 dBFS, and the reading is back under full scale
/// 9.6 s later.
///
/// ```
/// use manguebyte_analysis::RmsMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = RmsMeter::new(1).with_time_constant(0.05);
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 48_000, num_channels: 1 });
///
/// // One second of a 1 kHz sine at half scale.
/// let mut block = AudioBuffer::<f32>::new(1, 48_000);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = 0.5 * (core::f32::consts::TAU * n as f32 / 48.0).sin();
/// }
/// meter.measure(&block);
/// assert!((meter.readings().rms_db(0) + 9.03).abs() < 0.02);
/// ```
#[derive(Debug)]
pub struct RmsMeter {
    sample_rate: f64,
    time_constant: f64,
    factor: f64,
    mean_squares: Vec<f64>,
    readings: RmsReadings,
}

impl RmsMeter {
    /// Creates a meter for `channels` channels. This allocates; nothing
    /// after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new(channels: usize) -> Self {
        let time_constant = 0.3;
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            time_constant,
            factor: smoothing_factor(time_constant, DEFAULT_SAMPLE_RATE),
            mean_squares: vec![0.0; channels],
            readings: RmsReadings {
                shared: Arc::new(RmsShared {
                    mean_square: Row::new(channels),
                    reset: Flag::default(),
                }),
            },
        }
    }

    /// Sets the averaging time constant in seconds. Zero makes the reading
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

    /// Takes the sample rate from `spec` and clears the readings. The
    /// channel count was fixed by [`new`](Self::new).
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.factor = smoothing_factor(self.time_constant, self.sample_rate);
        self.reset();
    }

    /// Clears every reading.
    pub fn reset(&mut self) {
        self.mean_squares.fill(0.0);
        self.publish();
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &RmsReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// Channels the meter was not created for are ignored, and a channel
    /// missing from the block keeps its reading as it is.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.mean_squares.fill(0.0);
        }
        let factor = self.factor;
        for (state, channel) in self.mean_squares.iter_mut().zip(buffer.iter_channels()) {
            let mut mean_square = *state;
            for &sample in channel {
                let sample = screened(sample.to_f64());
                mean_square += factor * (sample * sample - mean_square);
            }
            *state = mean_square;
        }
        self.publish();
    }

    fn publish(&self) {
        for (channel, &mean_square) in self.mean_squares.iter().enumerate() {
            self.readings.shared.mean_square.set(channel, mean_square);
        }
    }
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for RmsMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        RmsMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        RmsMeter::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 4_800,
            num_channels: channels,
        }
    }

    fn sine(frequency: f64, amplitude: f64, frames: usize) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(1, frames);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = amplitude * (TAU * frequency * n as f64 / RATE).sin();
        }
        buffer
    }

    #[test]
    fn the_peak_is_the_largest_sample_of_either_sign() {
        let mut meter = PeakMeter::new(2);
        meter.prepare(&spec(2));
        let mut buffer = AudioBuffer::<f32>::new(2, 100);
        buffer.channel_mut(0)[3] = 0.25;
        buffer.channel_mut(0)[40] = -0.75;
        buffer.channel_mut(1)[99] = 0.125;
        meter.measure(&buffer);
        let readings = meter.readings();
        assert_eq!(readings.num_channels(), 2);
        assert_eq!((readings.max(0), readings.max(1)), (0.75, 0.125));
        assert_eq!(readings.max_overall(), 0.75);
        assert!((readings.max_overall_db() + 2.499).abs() < 1e-3);
        assert_eq!(readings.held(0), 0.75);
        assert_eq!(readings.held_overall(), 0.75);
        assert!((readings.held_overall_db() + 2.499).abs() < 1e-3);
        // The last sample of the second channel is its peak: no decay yet.
        assert_eq!(readings.level(1), 0.125);
        // A channel the meter does not have is silent.
        assert_eq!((readings.max(5), readings.max_db(5)), (0.0, -100.0));
    }

    #[test]
    fn the_level_falls_twenty_decibels_in_one_point_seven_seconds() {
        let mut meter = PeakMeter::new(1);
        meter.prepare(&spec(1));
        let mut click = AudioBuffer::<f64>::new(1, 1);
        click.channel_mut(0)[0] = 1.0;
        meter.measure(&click);
        assert_eq!(meter.readings().level_db(0), 0.0);

        // 1.7 s of silence, in blocks.
        let silence = AudioBuffer::<f64>::new(1, 4_800);
        for _ in 0..17 {
            meter.measure(&silence);
        }
        let fallen = meter.readings().level_db(0);
        assert!((fallen + 20.0).abs() < 1e-6, "{fallen}");
        // The maximum does not fall.
        assert_eq!(meter.readings().max(0), 1.0);
    }

    #[test]
    fn decay_and_hold_can_be_changed_or_switched_off() {
        let mut click = AudioBuffer::<f64>::new(1, 1);
        click.channel_mut(0)[0] = 0.5;
        let silence = AudioBuffer::<f64>::new(1, 48_000);

        let mut fast = PeakMeter::new(1).with_decay(60.0);
        fast.prepare(&spec(1));
        fast.measure(&click);
        fast.measure(&silence);
        assert!((fast.readings().level_db(0) + 66.02).abs() < 0.01);

        let mut frozen = PeakMeter::new(1).with_decay(0.0).with_hold(f64::INFINITY);
        frozen.prepare(&spec(1));
        frozen.measure(&click);
        for _ in 0..10 {
            frozen.measure(&silence);
        }
        assert_eq!(frozen.readings().level(0), 0.5);
        assert_eq!(frozen.readings().held(0), 0.5);
    }

    #[test]
    fn the_marker_holds_for_its_time_and_then_joins_the_level() {
        let mut meter = PeakMeter::new(1).with_hold(0.5);
        meter.prepare(&spec(1));
        let mut click = AudioBuffer::<f64>::new(1, 1);
        click.channel_mut(0)[0] = 1.0;
        meter.measure(&click);

        // Up to the last sample of the half second it is still on the peak.
        let mut silence = AudioBuffer::<f64>::new(1, 24_000);
        meter.measure(&silence);
        assert_eq!(meter.readings().held(0), 1.0);
        // One sample later it has dropped to the level, which fell all the
        // while.
        silence.set_num_frames(1);
        meter.measure(&silence);
        let (held, level) = (meter.readings().held(0), meter.readings().level(0));
        assert_eq!(held, level);
        assert!((db(held) + 20.0 / 1.7 * 0.5).abs() < 0.01, "{}", db(held));

        // A smaller peak that is still above the fallen level takes the
        // marker again and holds it.
        let mut smaller = AudioBuffer::<f64>::new(1, 1);
        smaller.channel_mut(0)[0] = 0.9;
        meter.measure(&smaller);
        silence.set_num_frames(12_000);
        meter.measure(&silence);
        assert_eq!(meter.readings().held(0), 0.9);
    }

    #[test]
    fn a_smaller_peak_during_a_hold_is_not_held_itself() {
        // The example in the documentation of `PeakReadings::held`: a hold
        // of two seconds, a peak of 1.0 at 0 s and one of 0.9 at 1.9 s.
        let mut meter = PeakMeter::new(1).with_hold(2.0);
        meter.prepare(&spec(1));
        let mut click = AudioBuffer::<f64>::new(1, 1);
        let mut silence = AudioBuffer::<f64>::new(1, 96_000);

        click.channel_mut(0)[0] = 1.0;
        meter.measure(&click);
        silence.set_num_frames(91_199);
        meter.measure(&silence);
        click.channel_mut(0)[0] = 0.9;
        meter.measure(&click);
        // The level took the second peak. The marker is still on the first.
        assert_eq!(meter.readings().level(0), 0.9);
        assert_eq!(meter.readings().held(0), 1.0);

        // At 1.95 s the two seconds of the first peak are not over.
        silence.set_num_frames(2_400);
        meter.measure(&silence);
        assert_eq!(meter.readings().held(0), 1.0);

        // At 2.05 s they are. The second peak started no hold of its own,
        // so the marker is on the level, which has been falling from 0.9
        // for 150 ms: 20 dB / 1.7 s * 0.15 s = 1.76 dB.
        silence.set_num_frames(4_800);
        meter.measure(&silence);
        let (held, level) = (meter.readings().held(0), meter.readings().level(0));
        println!("at 2.05 s the marker is at {held}, the level at {level}");
        assert_eq!(held, level);
        assert!(
            (db(held) - db(0.9) + 20.0 / 1.7 * 0.15).abs() < 1e-6,
            "{}",
            db(held)
        );
        // The largest sample of the last two seconds is 0.9, and the
        // marker is not there: it is not a maximum over the hold time.
        assert!((held - 0.7345).abs() < 1e-4, "{held}");

        // A peak that reaches the marker where it is now takes it, and is
        // held in its turn.
        click.channel_mut(0)[0] = 0.8;
        meter.measure(&click);
        silence.set_num_frames(48_000);
        meter.measure(&silence);
        assert_eq!(meter.readings().held(0), 0.8);
    }

    #[test]
    fn a_reset_can_be_asked_for_from_another_thread() {
        let mut meter = PeakMeter::new(1);
        meter.prepare(&spec(1));
        let readings = meter.readings().clone();
        meter.measure(&sine(1_000.0, 0.5, 4_800));
        assert!((readings.max(0) - 0.5).abs() < 1e-6);

        std::thread::spawn({
            let readings = readings.clone();
            move || readings.request_reset()
        })
        .join()
        .unwrap();
        // The request waits for the audio thread.
        assert!(readings.max(0) > 0.0);
        meter.measure(&AudioBuffer::<f64>::new(1, 16));
        assert_eq!(readings.max(0), 0.0);
        assert_eq!(readings.held(0), 0.0);
    }

    #[test]
    fn samples_that_are_not_numbers_do_not_stick() {
        let mut peak = PeakMeter::new(1);
        let mut rms = RmsMeter::new(1);
        peak.prepare(&spec(1));
        rms.prepare(&spec(1));
        let mut buffer = AudioBuffer::<f32>::new(1, 4);
        buffer
            .channel_mut(0)
            .copy_from_slice(&[0.25, f32::NAN, -0.5, f32::NAN]);
        peak.measure(&buffer);
        rms.measure(&buffer);
        assert_eq!(peak.readings().max(0), 0.5);
        assert!(peak.readings().level(0).is_finite());
        assert!(rms.readings().rms(0).is_finite());
    }

    #[test]
    fn rms_of_a_sine_is_its_peak_over_root_two() {
        let mut meter = RmsMeter::new(1);
        meter.prepare(&spec(1));
        // Ten time constants, so the average has settled.
        meter.measure(&sine(1_000.0, 0.5, 144_000));
        let rms = meter.readings().rms(0);
        assert!((rms - 0.5 / 2f64.sqrt()).abs() < 1e-4, "{rms}");
        assert!((meter.readings().rms_db(0) + 9.031).abs() < 5e-3);
    }

    #[test]
    fn rms_of_a_square_wave_is_its_peak() {
        let mut meter = RmsMeter::new(1).with_time_constant(0.01);
        meter.prepare(&spec(1));
        let mut buffer = AudioBuffer::<f32>::new(1, 9_600);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = if (n / 24) % 2 == 0 { 0.25 } else { -0.25 };
        }
        meter.measure(&buffer);
        assert!((meter.readings().rms(0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_low_tone_makes_the_reading_ripple_by_the_documented_amount() {
        // 20 Hz with the default 300 ms: the mean square moves by
        // 1 / (4 pi * 20 * 0.3) = 1.3% either way.
        let mut meter = RmsMeter::new(1);
        meter.prepare(&spec(1));
        let signal = sine(20.0, 0.5, 192_000);
        let mut one = AudioBuffer::<f64>::new(1, 1);
        let (mut lowest, mut highest) = (f64::MAX, f64::MIN);
        for (n, &sample) in signal.channel(0).iter().enumerate() {
            one.channel_mut(0)[0] = sample;
            meter.measure(&one);
            // The last two cycles, long after the average has settled.
            if n >= 192_000 - 4_800 {
                lowest = lowest.min(meter.readings().rms_db(0));
                highest = highest.max(meter.readings().rms_db(0));
            }
        }
        let middle = 0.5 * (lowest + highest);
        assert!((middle + 9.03).abs() < 0.01, "{middle}");
        assert!(
            (highest - lowest - 0.115).abs() < 0.005,
            "{}",
            highest - lowest
        );
    }

    #[test]
    fn the_time_constant_is_the_time_to_63_percent() {
        for seconds in [0.3, 0.05] {
            let mut meter = RmsMeter::new(1).with_time_constant(seconds);
            meter.prepare(&spec(1));
            let frames = (seconds * RATE).round() as usize;
            let mut step = AudioBuffer::<f64>::new(1, frames);
            step.channel_mut(0).fill(1.0);
            meter.measure(&step);
            // The mean square of a unit step after one time constant.
            let mean_square = meter.readings().rms(0).powi(2);
            let expected = 1.0 - (-1.0f64).exp();
            assert!((mean_square - expected).abs() < 1e-9, "{mean_square}");

            // And it falls by the same law.
            let silence = AudioBuffer::<f64>::new(1, frames);
            meter.measure(&silence);
            let after = meter.readings().rms(0).powi(2);
            assert!((after - expected * (-1.0f64).exp()).abs() < 1e-9, "{after}");
        }
    }

    #[test]
    fn meters_pass_audio_through_a_chain_untouched() {
        let mut chain = (PeakMeter::new(1), RmsMeter::new(1));
        Processor::<f64>::prepare(&mut chain, &spec(1));
        let original = sine(440.0, 0.8, 480);
        let mut buffer = original.clone();
        chain.process(&mut buffer);
        assert!(buffer == original);
        assert!((chain.0.readings().max(0) - 0.8).abs() < 1e-3);
        assert!(chain.1.readings().rms(0) > 0.0);
        Processor::<f64>::reset(&mut chain);
        assert_eq!(chain.0.readings().max(0), 0.0);
        assert_eq!(chain.1.readings().rms(0), 0.0);
    }
}
