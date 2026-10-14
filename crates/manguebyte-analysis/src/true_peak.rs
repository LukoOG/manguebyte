//! True-peak level, after ITU-R BS.1770-4 Annex 2.

use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::fir;
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::level::{
    DEFAULT_DECAY_DB_PER_SECOND, DEFAULT_HOLD_SECONDS, PeakFollower, PeakShared, PeakTiming,
};
use crate::shared::{DEFAULT_SAMPLE_RATE, db, screened, usable_rate};

/// Input samples each interpolated value is computed from.
///
/// With the stopband below, this many taps put the transition band between
/// 0.45 and 0.55 of the input sample rate: everything up to 21.6 kHz at
/// 48 kHz, or 19.8 kHz at 44.1 kHz, is reconstructed at its true level.
///
/// The filter is centred, so half of these samples lie after the point
/// that is interpolated: the meter looks [`LOOK_AHEAD`] samples ahead.
const TAPS_PER_PHASE: usize = 48;

/// Input samples that must follow a point of the waveform before the meter
/// can read it: half the length of the filter.
const LOOK_AHEAD: usize = TAPS_PER_PHASE / 2;

/// Stopband of the interpolation filter, in decibels. An image this far
/// down changes a reading by less than a thousandth of a decibel.
const STOPBAND_DB: f64 = 80.0;

/// The readings of a [`TruePeakMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates. A channel the meter does not have reads as silence.
#[derive(Debug, Clone)]
pub struct TruePeakReadings {
    shared: Arc<PeakShared>,
}

impl TruePeakReadings {
    /// How many channels the meter measures.
    pub fn num_channels(&self) -> usize {
        self.shared.level.len()
    }

    /// The true-peak level of `channel` as a linear amplitude: it jumps to
    /// each peak and falls back at the meter's decay rate.
    pub fn level(&self, channel: usize) -> f64 {
        self.shared.level.get(channel)
    }

    /// [`level`](Self::level) in dBTP, floored at -100.
    pub fn level_db(&self, channel: usize) -> f64 {
        db(self.level(channel))
    }

    /// The hold marker of `channel` as a linear amplitude.
    ///
    /// A true peak at or above the marker takes it and starts the hold
    /// time. When that time has passed without another peak reaching the
    /// marker, it drops to [`level`](Self::level) and falls with it.
    ///
    /// A smaller peak that arrives during a hold is not held itself, so
    /// this is not the largest true peak of the last hold time. It is the
    /// same marker as [`PeakReadings::held`](crate::PeakReadings::held),
    /// which has an example.
    pub fn held(&self, channel: usize) -> f64 {
        self.shared.held.get(channel)
    }

    /// [`held`](Self::held) in dBTP, floored at -100.
    pub fn held_db(&self, channel: usize) -> f64 {
        db(self.held(channel))
    }

    /// The maximum true-peak level of `channel` since the last reset, as a
    /// linear amplitude.
    pub fn max(&self, channel: usize) -> f64 {
        self.shared.max.get(channel)
    }

    /// [`max`](Self::max) in dBTP, floored at -100.
    pub fn max_db(&self, channel: usize) -> f64 {
        db(self.max(channel))
    }

    /// The maximum true-peak level of any channel since the last reset, as
    /// a linear amplitude.
    pub fn max_overall(&self) -> f64 {
        self.shared.max.max()
    }

    /// [`max_overall`](Self::max_overall) in dBTP, floored at -100. This is
    /// the "maximum true peak level" of EBU R 128.
    pub fn max_overall_db(&self) -> f64 {
        db(self.max_overall())
    }

    /// The highest hold marker of any channel, as a linear amplitude.
    pub fn held_overall(&self) -> f64 {
        self.shared.held.max()
    }

    /// [`held_overall`](Self::held_overall) in dBTP, floored at -100.
    pub fn held_overall_db(&self) -> f64 {
        db(self.held_overall())
    }

    /// Asks the meter to clear its readings. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// A true-peak meter: the peak of the waveform, not of its samples.
///
/// The samples of a digital signal describe a continuous waveform, and that
/// waveform is usually highest somewhere between two samples. A sine at a
/// quarter of the sample rate whose samples fall 45 degrees either side of
/// each crest never shows a sample above 0.707 of its true height, 3 dB
/// low; on transients the gap can be larger. Recommendation ITU-R BS.1770-4,
/// Annex 2, asks for the level to be read from an oversampled copy of the
/// signal instead, and EBU R 128 sets its limit of -1 dBTP on that reading.
///
/// This meter interpolates each channel by four (or two, eight or sixteen)
/// with a polyphase FIR filter and follows the largest magnitude. For each
/// channel it gives a level that falls back at a fixed rate, a hold marker,
/// and the maximum since the last reset, and the maximum over all channels.
///
/// # The filter
///
/// A windowed sinc with a Kaiser window, designed by
/// [`manguebyte_dsp::fir::lowpass`]: 48 taps for each interpolated sample,
/// 80 dB of stopband, and a passband that is flat within 0.01 dB up to 0.45
/// of the sample rate. The Recommendation prints a filter of 12 taps per
/// phase as one that would satisfy its requirements; this one is four times
/// as long so that the top of the audio band is read at its true level too.
///
/// The interpolated copy contains the input samples themselves, so the
/// reading is never below the sample peak.
///
/// # Look-ahead, and the end of a file
///
/// What the waveform does between two samples depends on the samples after
/// them as much as on those before, so the filter is centred: the stretch
/// between two samples is read when the 24th sample after the earlier of
/// the two arrives, half a millisecond later at 48 kHz. The samples
/// themselves are read at once. On a running signal that delay goes
/// unnoticed. At the end of a file it means that the waveform between the
/// last 24 samples has not been read when the last block has been
/// measured, and neither has what the waveform does on its way down from
/// the last sample. [`flush`](Self::flush) reads both: call it after the
/// last block, before taking the reading of a file.
///
/// # Limits
///
/// Interpolation by `n` still samples the waveform, `n` times as often. The
/// worst case is a sine whose crest falls midway between two interpolated
/// samples: the reading is then low by `20 log10(cos(pi f / (n fs)))` dB
/// (BS.1770-4, Appendix 1 to Annex 2). For four times that is 0.17 dB at a
/// quarter of the sample rate and 0.69 dB at half of it; for eight times,
/// 0.04 dB and 0.17 dB. Components above 0.45 of the sample rate are in the
/// filter's transition band and read low as well.
///
/// Until [`flush`](Self::flush) is called, the last 24 samples that were
/// measured have only been read as samples, which can be 3 dB under the
/// peak of the waveform between them, or more.
///
/// A sample that is not finite is read as silence and one beyond 180 dB
/// over full scale as 180 dB over, before it enters the filter, so no
/// input leaves the meter stuck (see the
/// [crate documentation](crate#bad-samples)). A sample at that limit is
/// still a peak: the marker holds it for its time, the level needs 15.3 s
/// at the default rate to fall back to full scale, and the maximum keeps it
/// until the next reset. The limit is on the samples: the waveform between
/// several of them at the limit is higher, and is read as it is.
///
/// The cost is `(n - 1) * 48` multiplications for each sample of each
/// channel.
///
/// ```
/// use manguebyte_analysis::TruePeakMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = TruePeakMeter::new(1);
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A 12 kHz sine at half scale, sampled 45 degrees either side of its
/// // crests: no sample is above 0.354. It fades in over 10 ms, because a
/// // tone that starts abruptly really does overshoot.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     let phase = core::f64::consts::FRAC_PI_2 * n as f64 + core::f64::consts::FRAC_PI_4;
///     let fade = (n as f64 / 480.0).min(1.0);
///     *sample = (0.5 * fade * phase.sin()) as f32;
/// }
/// assert!((20.0 * f64::from(block.peak()).log10() + 9.03).abs() < 0.01);
///
/// meter.measure(&block);
/// assert!((meter.readings().max_db(0) + 6.02).abs() < 0.01);
/// ```
#[derive(Debug)]
pub struct TruePeakMeter {
    factor: usize,
    sample_rate: f64,
    decay_db_per_second: f64,
    hold_seconds: f64,
    timing: PeakTiming,
    /// The filter split into its phases, each stored oldest input first.
    /// Phase zero is left out: it reproduces the input sample.
    phases: Vec<f64>,
    /// The last `TAPS_PER_PHASE` inputs of each channel, written twice so
    /// that they are always contiguous. Screened on the way in, so always
    /// finite.
    history: Vec<f64>,
    /// Where each channel writes next. One per channel, so that a channel
    /// missing from a block simply stands still.
    positions: Vec<usize>,
    followers: Vec<PeakFollower>,
    readings: TruePeakReadings,
}

impl TruePeakMeter {
    /// Creates a meter for `channels` channels that interpolates by four,
    /// the factor BS.1770-4 asks for at 48 kHz. This allocates; nothing
    /// after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz. Only
    /// the fall-back and hold times depend on the rate.
    pub fn new(channels: usize) -> Self {
        Self {
            factor: 4,
            sample_rate: DEFAULT_SAMPLE_RATE,
            decay_db_per_second: DEFAULT_DECAY_DB_PER_SECOND,
            hold_seconds: DEFAULT_HOLD_SECONDS,
            timing: PeakTiming::new(
                DEFAULT_SAMPLE_RATE,
                DEFAULT_DECAY_DB_PER_SECOND,
                DEFAULT_HOLD_SECONDS,
            ),
            phases: design_phases(4),
            history: vec![0.0; channels * 2 * TAPS_PER_PHASE],
            positions: vec![0; channels],
            followers: vec![PeakFollower::default(); channels],
            readings: TruePeakReadings {
                shared: Arc::new(PeakShared::new(channels)),
            },
        }
    }

    /// Sets the interpolation factor. Two is what BS.1770-4 suggests for
    /// signals already at 96 kHz; eight and sixteen read closer to the true
    /// peak at a proportionate cost. This allocates.
    ///
    /// # Panics
    ///
    /// Panics unless `factor` is 2, 4, 8 or 16.
    pub fn with_oversampling(mut self, factor: usize) -> Self {
        assert!(
            matches!(factor, 2 | 4 | 8 | 16),
            "oversampling factor must be 2, 4, 8 or 16, got {factor}"
        );
        self.factor = factor;
        self.phases = design_phases(factor);
        self
    }

    /// Sets how fast the level falls back, in decibels per second. Zero or
    /// less stops it falling at all. The default is 20 dB in 1.7 s.
    pub fn with_decay(mut self, db_per_second: f64) -> Self {
        self.decay_db_per_second = db_per_second;
        self.update_timing();
        self
    }

    /// Sets how long the hold marker stays on a peak, in seconds.
    /// `f64::INFINITY` holds until the next reset. The default is two
    /// seconds.
    pub fn with_hold(mut self, seconds: f64) -> Self {
        self.hold_seconds = seconds;
        self.update_timing();
        self
    }

    /// The interpolation factor.
    pub fn oversampling(&self) -> usize {
        self.factor
    }

    /// Takes the sample rate from `spec` and clears the meter. The channel
    /// count was fixed by [`new`](Self::new).
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.update_timing();
        self.reset();
    }

    /// Clears every reading and the filter's memory.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.positions.fill(0);
        self.followers.fill(PeakFollower::default());
        self.readings.shared.publish(&self.followers);
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &TruePeakReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// Channels the meter was not created for are ignored, and a channel
    /// missing from the block keeps its readings as they are.
    ///
    /// The waveform between the newest 24 samples is read by later blocks,
    /// or by [`flush`](Self::flush) when no more are coming.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        self.obey();
        let timing = self.timing;
        let channels = self
            .followers
            .iter_mut()
            .zip(&mut self.positions)
            .zip(self.history.chunks_exact_mut(2 * TAPS_PER_PHASE))
            .zip(buffer.iter_channels());
        for (((follower, position), history), input) in channels {
            // Screened before it enters the filter: one bad sample would
            // otherwise come out of it 47 more times.
            let samples = input.iter().map(|sample| screened(sample.to_f64()));
            follow(&self.phases, history, position, follower, &timing, samples);
        }
        self.readings.shared.publish(&self.followers);
    }

    /// Reads what is still inside the filter, as if silence followed.
    /// Real-time safe.
    ///
    /// The meter looks 24 samples ahead (see the section on look-ahead
    /// above), so when the last block of a file has been measured, the
    /// waveform between its last 24 samples has not been read. This runs
    /// 48 samples of silence through every channel, exactly as
    /// [`measure`](Self::measure) would with a silent block of that length:
    /// the rest of the waveform is read, the level falls for 48 samples,
    /// and the filter is left empty.
    ///
    /// Call it once, after the last block of a file or of a stream that
    /// stops. Audio measured afterwards is read as following that silence.
    ///
    /// ```
    /// use manguebyte_analysis::TruePeakMeter;
    /// use manguebyte_core::AudioBuffer;
    /// use manguebyte_dsp::ProcessSpec;
    ///
    /// let mut meter = TruePeakMeter::new(1);
    /// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 1 });
    ///
    /// // A file that ends on two samples at full scale. The waveform
    /// // through them rises to 1.27 halfway between the two.
    /// let mut file = AudioBuffer::<f32>::new(1, 64);
    /// file.channel_mut(0)[62] = 1.0;
    /// file.channel_mut(0)[63] = 1.0;
    ///
    /// meter.measure(&file);
    /// assert_eq!(meter.readings().max(0), 1.0);
    /// meter.flush();
    /// assert!((meter.readings().max(0) - 1.27).abs() < 0.01);
    /// ```
    pub fn flush(&mut self) {
        self.obey();
        let timing = self.timing;
        let channels = self
            .followers
            .iter_mut()
            .zip(&mut self.positions)
            .zip(self.history.chunks_exact_mut(2 * TAPS_PER_PHASE));
        for ((follower, position), history) in channels {
            // `LOOK_AHEAD` samples bring the interpolation up to the last
            // sample, and as many again read the waveform's way down from
            // it. Together that is the length of the filter, which is then
            // empty.
            let silence = core::iter::repeat_n(0.0, 2 * LOOK_AHEAD);
            follow(&self.phases, history, position, follower, &timing, silence);
        }
        self.readings.shared.publish(&self.followers);
    }

    /// Carries out what another thread asked for through the readings.
    fn obey(&mut self) {
        if self.readings.shared.reset.take() {
            // Only the readings: the filter goes on following the signal.
            self.followers.fill(PeakFollower::default());
        }
    }

    fn update_timing(&mut self) {
        self.timing = PeakTiming::new(
            self.sample_rate,
            self.decay_db_per_second,
            self.hold_seconds,
        );
    }
}

/// Runs `samples` through one channel of the meter: each goes into the
/// channel's `history`, and the largest magnitude of the interpolated
/// waveform it completes goes to the `follower`.
///
/// The samples must be finite, which is what keeps the history and the
/// follower finite.
#[inline]
fn follow(
    phases: &[f64],
    history: &mut [f64],
    position: &mut usize,
    follower: &mut PeakFollower,
    timing: &PeakTiming,
    samples: impl Iterator<Item = f64>,
) {
    let mut at = *position;
    let mut state = *follower;
    for sample in samples {
        history[at] = sample;
        history[at + TAPS_PER_PHASE] = sample;
        at = (at + 1) % TAPS_PER_PHASE;
        // The newest `TAPS_PER_PHASE` inputs, oldest first.
        let recent = &history[at..at + TAPS_PER_PHASE];

        // Phase zero of the interpolated signal is the input itself. The
        // other phases are points of the waveform between two older
        // inputs: the one `LOOK_AHEAD` samples back and the one after it.
        let mut magnitude = sample.abs();
        for phase in phases.chunks_exact(TAPS_PER_PHASE) {
            let mut value = 0.0;
            for (&tap, &past) in phase.iter().zip(recent) {
                value += tap * past;
            }
            magnitude = magnitude.max(value.abs());
        }
        state.push(magnitude, timing);
    }
    *follower = state;
    *position = at;
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for TruePeakMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        TruePeakMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        TruePeakMeter::reset(self);
    }
}

/// The interpolation filter for `factor`, as `factor - 1` phases of
/// [`TAPS_PER_PHASE`] taps each, every phase reversed so that it lines up
/// with a history that runs oldest first.
fn design_phases(factor: usize) -> Vec<f64> {
    // An odd length centred on a tap, with the cutoff at half the input
    // rate. The taps a whole number of input samples from the centre are
    // then zero crossings of the sinc, so phase zero is a pure delay and
    // needs no arithmetic.
    let length = TAPS_PER_PHASE * factor + 1;
    let prototype = fir::lowpass(length, 0.5 / factor as f64, STOPBAND_DB);

    let mut phases = Vec::with_capacity((factor - 1) * TAPS_PER_PHASE);
    for phase in 1..factor {
        let taps: Vec<f64> = (0..TAPS_PER_PHASE)
            .map(|index| prototype[phase + index * factor])
            .collect();
        // Each phase on its own must pass a constant unchanged. Scaling
        // them one by one makes that exact, where the prototype is only
        // close.
        let sum: f64 = taps.iter().sum();
        phases.extend(taps.iter().rev().map(|tap| tap / sum));
    }
    phases
}

#[cfg(test)]
mod tests {
    use core::f64::consts::{PI, TAU};

    use super::*;

    const RATE: f64 = 48_000.0;

    fn spec() -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 4_800,
            num_channels: 1,
        }
    }

    /// A sine at `frequency`, as a fraction of the sample rate, starting at
    /// `phase` radians, with a raised-cosine fade at both ends.
    fn sine(frequency: f64, amplitude: f64, phase: f64, frames: usize) -> AudioBuffer<f64> {
        let fade = 480.min(frames / 2);
        let mut buffer = AudioBuffer::new(1, frames);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            let edge = n.min(frames - 1 - n);
            let gain = if edge < fade {
                0.5 - 0.5 * (PI * edge as f64 / fade as f64).cos()
            } else {
                1.0
            };
            *sample = gain * amplitude * (TAU * frequency * n as f64 + phase).sin();
        }
        buffer
    }

    fn measured_db(factor: usize, signal: &AudioBuffer<f64>) -> f64 {
        let mut meter = TruePeakMeter::new(1).with_oversampling(factor);
        meter.prepare(&spec());
        meter.measure(signal);
        meter.readings().max_db(0)
    }

    /// The whole interpolation filter as the meter applies it, put back
    /// together from its phases, with phase zero as the single tap it is.
    fn whole_filter(factor: usize) -> Vec<f64> {
        let phases = design_phases(factor);
        let mut filter = vec![0.0; TAPS_PER_PHASE * factor + 1];
        filter[TAPS_PER_PHASE * factor / 2] = 1.0;
        for (phase, taps) in phases.chunks_exact(TAPS_PER_PHASE).enumerate() {
            for (index, &tap) in taps.iter().rev().enumerate() {
                filter[phase + 1 + index * factor] = tap;
            }
        }
        filter
    }

    /// Gain of `filter` at `frequency`, a fraction of the input sample
    /// rate. The filter runs at `factor` times that rate, and spreading
    /// each input sample over `factor` outputs divides the level by as
    /// much.
    fn response_db(filter: &[f64], factor: usize, frequency: f64) -> f64 {
        20.0 * (fir::magnitude_at(filter, frequency / factor as f64) / factor as f64).log10()
    }

    #[test]
    fn the_filter_is_flat_where_audio_is_and_closed_where_images_are() {
        for factor in [2, 4, 8, 16] {
            let filter = whole_filter(factor);
            // Up to 0.45 of the sample rate, in steps of a thousandth.
            let mut passband = 0.0f64;
            for step in 0..=450 {
                let frequency = f64::from(step) / 1_000.0;
                passband = passband.max(response_db(&filter, factor, frequency).abs());
            }
            assert!(passband < 0.005, "{factor}x: {passband:.5} dB of ripple");

            // From 0.6 of the sample rate, where the first image of the
            // audio band has begun, to half the interpolated rate.
            let mut stopband = f64::MIN;
            for step in 600..=(factor * 500) as u32 {
                let frequency = f64::from(step) / 1_000.0;
                stopband = stopband.max(response_db(&filter, factor, frequency));
            }
            assert!(stopband < -80.0, "{factor}x: stopband at {stopband:.1} dB");
        }
    }

    #[test]
    fn every_phase_passes_a_constant_exactly() {
        for factor in [2, 4, 8, 16] {
            let phases = design_phases(factor);
            assert_eq!(phases.len(), (factor - 1) * TAPS_PER_PHASE);
            for phase in phases.chunks_exact(TAPS_PER_PHASE) {
                assert!((phase.iter().sum::<f64>() - 1.0).abs() < 1e-14);
            }
            // The phases either side of the centre mirror each other.
            let (first, last) = (
                &phases[..TAPS_PER_PHASE],
                &phases[phases.len() - TAPS_PER_PHASE..],
            );
            for (a, b) in first.iter().zip(last.iter().rev()) {
                assert!((a - b).abs() < 1e-14);
            }
        }
    }

    #[test]
    fn a_steady_level_reads_as_itself() {
        let mut meter = TruePeakMeter::new(1);
        meter.prepare(&spec());
        let mut buffer = AudioBuffer::<f32>::new(1, 480);
        buffer.channel_mut(0).fill(0.25);
        meter.measure(&buffer);
        // The step at the start rings, as it would in a converter...
        assert!(meter.readings().max(0) > 0.25);
        // ...but once the filter is full of the constant, every phase
        // returns it.
        meter.readings().request_reset();
        meter.measure(&buffer);
        assert!((meter.readings().max(0) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn a_sine_between_the_samples_is_found() {
        // A quarter of the sample rate, sampled 45 degrees off the crests:
        // the samples say -3.01 dB, the waveform says 0.
        let signal = sine(0.25, 1.0, PI / 4.0, 4_800);
        let sample_peak = 20.0 * signal.peak().log10();
        assert!((sample_peak + 3.01).abs() < 0.01, "{sample_peak}");
        for factor in [2, 4, 8, 16] {
            let reading = measured_db(factor, &signal);
            assert!(reading.abs() < 0.01, "{factor}x: {reading}");
        }
    }

    #[test]
    fn the_reading_is_never_below_the_sample_peak() {
        for (frequency, phase) in [(0.01, 0.3), (0.21, 1.0), (0.37, 2.0), (0.49, 0.1)] {
            let signal = sine(frequency, 0.8, phase, 2_400);
            for factor in [2, 4, 8, 16] {
                let mut meter = TruePeakMeter::new(1).with_oversampling(factor);
                meter.prepare(&spec());
                meter.measure(&signal);
                assert!(meter.readings().max(0) >= signal.peak());
                assert_eq!(meter.oversampling(), factor);
            }
        }
    }

    #[test]
    fn the_worst_case_reads_low_by_what_the_recommendation_says() {
        // BS.1770-4, Appendix 1 to Annex 2: a sine at `f` times the sample
        // rate whose crests fall midway between two interpolated samples
        // reads low by 20 log10(cos(pi f / n)). The phase below puts every
        // crest exactly there, for frequencies with a whole number of
        // interpolated samples to the half cycle.
        for factor in [2usize, 4, 8, 16] {
            for frequency in [1.0 / 8.0, 1.0 / 4.0, 1.0 / 3.0] {
                let step = PI * frequency / factor as f64;
                let signal = sine(frequency, 1.0, PI / 2.0 - step, 4_800);
                let reading = measured_db(factor, &signal);
                let bound = 20.0 * step.cos().log10();
                assert!(
                    (reading - bound).abs() < 0.01,
                    "{factor}x at {frequency}: {reading:.4} dB, expected {bound:.4} dB"
                );
            }
        }
        // The figures quoted in the documentation of the meter.
        let low =
            |factor: usize, frequency: f64| 20.0 * (PI * frequency / factor as f64).cos().log10();
        assert!((low(4, 0.25) + 0.17).abs() < 0.005);
        assert!((low(4, 0.5) + 0.69).abs() < 0.005);
        assert!((low(8, 0.25) + 0.04).abs() < 0.005);
        assert!((low(8, 0.5) + 0.17).abs() < 0.005);

        // Moved a little off those frequencies, the crests drift through
        // every position, one of them lands on a sample, and the reading
        // is the full level.
        for frequency in [0.1251, 0.2503, 0.3331, 0.4499] {
            let reading = measured_db(4, &sine(frequency, 1.0, 0.0, 12_000));
            assert!(reading.abs() < 0.01, "{frequency}: {reading:.4} dB");
        }
    }

    #[test]
    fn channels_are_measured_separately_and_together() {
        let mut meter = TruePeakMeter::new(3);
        meter.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 480,
            num_channels: 3,
        });
        let mut buffer = AudioBuffer::<f32>::new(3, 480);
        buffer.channel_mut(0).fill(0.5);
        buffer.channel_mut(2).fill(-0.125);
        for _ in 0..4 {
            meter.measure(&buffer);
        }
        let readings = meter.readings();
        assert_eq!(readings.num_channels(), 3);
        assert_eq!(readings.max(1), 0.0);
        assert!(readings.max(0) > readings.max(2) && readings.max(2) >= 0.125);
        assert_eq!(readings.max_overall(), readings.max(0));
        assert_eq!(readings.max_overall_db(), readings.max_db(0));
        assert_eq!(readings.held_overall(), readings.held(0));
        assert_eq!(readings.held_overall_db(), readings.held_db(0));
        assert_eq!((readings.max(3), readings.level_db(3)), (0.0, -100.0));

        // A block with fewer channels than the meter, and one with more.
        meter.measure(&AudioBuffer::<f32>::new(1, 64));
        meter.measure(&AudioBuffer::<f32>::new(5, 64));
        meter.measure(&AudioBuffer::<f32>::new(0, 64));
        assert!(meter.readings().max(0) > 0.5);
    }

    #[test]
    fn level_and_marker_follow_the_same_rules_as_the_sample_peak_meter() {
        let mut meter = TruePeakMeter::new(1).with_decay(20.0).with_hold(0.5);
        meter.prepare(&spec());
        meter.measure(&sine(0.02, 0.5, 0.0, 4_800));
        let peak = meter.readings().max(0);
        assert!((peak - 0.5).abs() < 1e-3, "{peak}");

        // Half a second of silence: the marker is still up, the level has
        // fallen by about 10 dB.
        let silence = AudioBuffer::<f64>::new(1, 4_800);
        for _ in 0..4 {
            meter.measure(&silence);
        }
        let readings = meter.readings();
        assert_eq!(readings.held(0), readings.max(0));
        assert!(readings.level_db(0) < readings.held_db(0) - 7.0);
        for _ in 0..2 {
            meter.measure(&silence);
        }
        assert_eq!(meter.readings().held(0), meter.readings().level(0));
        assert_eq!(meter.readings().max(0), peak);

        meter.reset();
        assert_eq!(meter.readings().max(0), 0.0);
    }

    #[test]
    fn a_point_between_two_samples_is_read_24_samples_later() {
        // Two samples at full scale next to each other, in silence. The
        // waveform through them rises to 1.27 halfway between the two, and
        // nowhere else above 1.
        for factor in [2, 4, 8, 16] {
            let mut meter = TruePeakMeter::new(1).with_oversampling(factor);
            meter.prepare(&spec());
            let mut one = AudioBuffer::<f64>::new(1, 1);
            let mut feed = |sample: f64, meter: &mut TruePeakMeter| {
                one.channel_mut(0)[0] = sample;
                meter.measure(&one);
                meter.readings().max(0)
            };
            for _ in 0..100 {
                assert_eq!(feed(0.0, &mut meter), 0.0);
            }
            // The samples themselves are read at once.
            assert_eq!(feed(1.0, &mut meter), 1.0);
            assert_eq!(feed(1.0, &mut meter), 1.0);
            // The earlier of the two is now one sample back. For the next
            // 22 samples nothing more is known about what lies between
            // them.
            for _ in 0..LOOK_AHEAD - 2 {
                assert_eq!(feed(0.0, &mut meter), 1.0);
            }
            // The 24th sample after the earlier one completes the filter
            // around that point.
            let read = feed(0.0, &mut meter);
            assert!(
                (read - 1.273).abs() < 0.002,
                "{factor}x: the peak between the samples reads {read}"
            );
            // And that was all there was to find.
            for _ in 0..100 {
                assert_eq!(feed(0.0, &mut meter), read);
            }
        }
    }

    #[test]
    fn the_true_peak_of_a_whole_file_includes_its_last_samples() {
        // Silence, and in the last 24 samples a Hann-shaped burst of fs/4 at
        // 45 degrees and amplitude one. It ends at zero, so nothing is cut
        // off. Its samples reach -3.05 dBFS and its waveform -0.16 dBTP.
        let frames = 4_800;
        let mut file = AudioBuffer::<f64>::new(1, frames);
        for index in 0..24 {
            let n = frames - 24 + index;
            let hann = 0.5 - 0.5 * (TAU * index as f64 / 23.0).cos();
            file.channel_mut(0)[n] = hann * (PI / 2.0 * n as f64 + PI / 4.0).sin();
        }
        let sample_peak = db(file.peak());
        assert!((sample_peak + 3.051).abs() < 1e-3, "{sample_peak}");

        // Measured to its last sample and no further, the waveform between
        // the samples of the burst has not been read: the look-ahead.
        let mut meter = TruePeakMeter::new(1);
        meter.prepare(&spec());
        meter.measure(&file);
        let unflushed = meter.readings().max_db(0);
        assert_eq!(unflushed, sample_peak);

        // The same file with silence after it, which lets the filter finish.
        let mut reference = TruePeakMeter::new(1);
        reference.prepare(&spec());
        reference.measure(&file);
        reference.measure(&AudioBuffer::<f64>::new(1, 64));
        let true_peak = reference.readings().max_db(0);
        assert!((true_peak + 0.163).abs() < 1e-3, "{true_peak}");

        // Flushing reads it without the silence having to exist.
        meter.flush();
        let flushed = meter.readings().max_db(0);
        println!(
            "sample peak {sample_peak:.3} dBFS; {unflushed:.3} dBTP at the end of the file; \
             {flushed:.3} dBTP after flushing; {true_peak:.3} dBTP with 64 samples of silence"
        );
        // The tolerance Tech 3341 gives for a true-peak reading is 0.4 dB
        // under. This is the same number to the last bit.
        assert_eq!(flushed, true_peak);
    }

    #[test]
    fn flushing_is_measuring_a_filter_length_of_silence() {
        // A level of 0.5 that is faded in over 200 samples, held for 300 and
        // then cut off. While it is held every phase of the filter returns
        // it. The cut makes the waveform ring: between the last two samples
        // it swings to 0.57, and the meter cannot know that before it has
        // been told that the level ends.
        let mut signal = AudioBuffer::<f64>::new(1, 500);
        for (n, sample) in signal.channel_mut(0).iter_mut().enumerate() {
            let fade = 0.5 - 0.5 * (PI * (n as f64 / 200.0).min(1.0)).cos();
            *sample = 0.5 * fade;
        }
        for factor in [2, 4, 8, 16] {
            let mut flushed = TruePeakMeter::new(2).with_oversampling(factor);
            let mut silenced = TruePeakMeter::new(2).with_oversampling(factor);
            let two_channel_spec = ProcessSpec {
                sample_rate: RATE,
                max_block_size: 500,
                num_channels: 2,
            };
            flushed.prepare(&two_channel_spec);
            silenced.prepare(&two_channel_spec);
            // The block has one channel. The meter's second channel is not
            // in it, and is flushed all the same.
            flushed.measure(&signal);
            silenced.measure(&signal);
            // (The fade-in overshoots by a millionth or so.)
            let unflushed = flushed.readings().max(0);
            assert!((unflushed - 0.5).abs() < 1e-5, "{factor}x: {unflushed}");

            flushed.flush();
            silenced.measure(&AudioBuffer::<f64>::new(2, TAPS_PER_PHASE));
            for channel in 0..2 {
                let (a, b) = (flushed.readings(), silenced.readings());
                assert_eq!(a.max(channel).to_bits(), b.max(channel).to_bits());
                assert_eq!(a.level(channel).to_bits(), b.level(channel).to_bits());
                assert_eq!(a.held(channel).to_bits(), b.held(channel).to_bits());
            }
            let rung = flushed.readings().max(0);
            assert!((rung - 0.568).abs() < 0.005, "{factor}x: {rung}");
            assert_eq!(flushed.readings().max(1), 0.0);

            // The filter is empty now: flushing again finds nothing, and a
            // steady level that follows is read from a clean start.
            let before = flushed.readings().max(0);
            flushed.flush();
            assert_eq!(flushed.readings().max(0), before);
            assert!(flushed.history.iter().all(|&sample| sample == 0.0));

            // A reset asked for from another thread is carried out by a
            // flush as it is by a block.
            flushed.readings().request_reset();
            flushed.flush();
            assert_eq!(flushed.readings().max(0), 0.0);
        }
    }

    #[test]
    #[should_panic(expected = "oversampling factor must be 2, 4, 8 or 16")]
    fn an_odd_factor_is_refused() {
        let _ = TruePeakMeter::new(1).with_oversampling(3);
    }
}
