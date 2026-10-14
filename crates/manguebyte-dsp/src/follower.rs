use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample};

use crate::numeric::limit;
use crate::{ProcessSpec, Processor};

/// Level below which the follower's memory is cleared to exactly zero.
///
/// A release never reaches zero by itself. Left alone it would shrink until
/// the numbers are too small for the processor to handle at full speed. This
/// is a thousand decibels or more below full scale, far beneath anything
/// that can be heard or measured.
const FLOOR: f64 = 1e-100;

/// Largest sample size the follower takes at face value: a million times
/// full scale, 120 dB above it. Anything larger counts as this much.
///
/// No real signal comes near it, but a broken one does: a single infinite
/// sample, or one of 1e300, would otherwise put a level into the memory that
/// a release needs hundreds of its time constants to bring back down. From
/// this ceiling a peak reading is back at full scale after 14 release times
/// and an RMS reading after 28, and the reading fits any sample type.
const CEILING: f64 = 1e6;

/// What an [`EnvelopeFollower`] measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LevelDetector {
    /// The size of each sample, whatever its sign. With a fast attack and a
    /// slow release this follows the peaks of the waveform, which is what a
    /// limiter or a peak meter needs.
    #[default]
    Peak,
    /// The root of the mean of the squared samples: the level in terms of
    /// power, which is closer to how loud something sounds. With the attack
    /// and the release set to the same time, a sine of amplitude 1 reads
    /// 0.707. With a faster attack than release it reads higher: see the
    /// limits of [`EnvelopeFollower`].
    Rms,
}

/// One-pole smoothing coefficient that covers 63% of a step in
/// `milliseconds`. Zero or less means "instantly".
fn time_coefficient(milliseconds: f64, sample_rate: f64) -> f64 {
    let samples = milliseconds * 1e-3 * sample_rate;
    if samples > 0.0 {
        (-1.0 / samples).exp()
    } else {
        0.0
    }
}

/// Follows the level of a signal: the detector behind compressors, gates,
/// auto-wah, ducking and meters.
///
/// Each sample is first made positive, by taking its size for
/// [`LevelDetector::Peak`] or its square for [`LevelDetector::Rms`]. The
/// result is then smoothed by a one-pole filter that uses the attack time
/// while the level is rising and the release time while it is falling:
///
/// ```text
/// level[n] = input[n] + c * (level[n-1] - input[n])     c = exp(-1 / (time * sample_rate))
/// ```
///
/// A time is therefore the time the follower takes to cover 63% of a sudden
/// change, the same meaning it has in [`Compressor`](crate::Compressor).
/// After a step from silence to a steady level `A`, `n` samples into the
/// attack, the reading is exactly `A * (1 - c^(n+1))` for peak detection and
/// `A * sqrt(1 - c^(n+1))` for RMS, where the smoothing acts on the power.
///
/// Channels are followed separately, or together when
/// [linked](Self::set_linked): then one level is kept for all of them, taken
/// from the largest sample of each frame (peak) or from the mean power of
/// the frame (RMS), so that a stereo image does not shift when only one side
/// is loud.
///
/// It can be driven a sample at a time with
/// [`process_sample`](Self::process_sample) or
/// [`process_frame`](Self::process_frame), or a block at a time: as a
/// [`Processor`] it replaces the audio by its level, and
/// [`follow`](Self::follow) reads a block without changing it.
///
/// There are two memories behind this: a level for each channel, and one
/// level shared by all of them. Which call uses which:
///
/// | Call | Memory |
/// |---|---|
/// | [`process_sample`](Self::process_sample), [`channel_level`](Self::channel_level) | each channel's own, always |
/// | [`process_frame`](Self::process_frame), [`shared_level`](Self::shared_level) | the shared one, always |
/// | [`process`](Processor::process), [`follow`](Self::follow), [`level`](Self::level) | the shared one when linked, each channel's own otherwise |
///
/// The two do not disturb each other, so one follower can give a level per
/// channel and a level for all of them at the same time. Changing
/// [`set_linked`](Self::set_linked) hands the present reading from one
/// memory to the other.
///
/// ```
/// use manguebyte_dsp::{EnvelopeFollower, LevelDetector};
///
/// let mut follower = EnvelopeFollower::<f32>::new()
///     .with_detector(LevelDetector::Peak)
///     .with_attack_ms(1.0)
///     .with_release_ms(100.0);
/// follower.set_sample_rate(48_000.0);
///
/// // Ten attack times of a full-scale signal.
/// let mut level = 0.0;
/// for _ in 0..480 {
///     level = follower.process_sample(0, 1.0);
/// }
/// assert!(level > 0.999);
///
/// // Then silence: one release time later, 37% of the level is left.
/// for _ in 0..4_800 {
///     level = follower.process_sample(0, 0.0);
/// }
/// assert!((level - 0.368).abs() < 0.001);
/// ```
///
/// # Limits
///
/// For a true RMS reading set the attack and the release to the same time.
/// A faster attack than release makes the follower lean towards the peaks
/// of the squared signal and read high: with the 10 ms and 100 ms a new
/// follower has, a 1 kHz sine of amplitude 1 reads 0.90 instead of 0.707.
/// The reading of a steady tone also ripples at twice its frequency, by
/// less the longer the times are.
///
/// A sample larger than a million times full scale, 120 dB above it, counts
/// as that much, and a sample that is not a number counts as silence. A
/// broken sample therefore cannot hold the reading up for long: after an
/// infinite one, a peak reading is back below full scale within 14 release
/// times and an RMS reading within 28.
#[derive(Debug, Clone)]
pub struct EnvelopeFollower<S: Sample = f32> {
    detector: LevelDetector,
    attack_ms: f64,
    release_ms: f64,
    linked: bool,
    sample_rate: f64,
    attack: f64,
    release: f64,
    /// Smoothed level of each channel; its square when measuring RMS.
    channels: Vec<f64>,
    /// The same for all channels together.
    shared: f64,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for EnvelopeFollower<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> EnvelopeFollower<S> {
    /// Creates a peak follower with a 10 ms attack and a 100 ms release
    /// that follows each channel separately. It has one channel until it is
    /// prepared, and assumes 44.1 kHz.
    pub fn new() -> Self {
        let mut follower = Self {
            detector: LevelDetector::Peak,
            attack_ms: 10.0,
            release_ms: 100.0,
            linked: false,
            sample_rate: 44_100.0,
            attack: 0.0,
            release: 0.0,
            channels: vec![0.0],
            shared: 0.0,
            marker: PhantomData,
        };
        follower.update();
        follower
    }

    /// Chooses between peak and RMS. The current reading carries over.
    /// Real-time safe.
    pub fn set_detector(&mut self, detector: LevelDetector) {
        if detector == self.detector {
            return;
        }
        // The memory holds a level for one and a power for the other.
        let convert = |state: f64| match detector {
            LevelDetector::Peak => state.sqrt(),
            LevelDetector::Rms => state * state,
        };
        for state in &mut self.channels {
            *state = convert(*state);
        }
        self.shared = convert(self.shared);
        self.detector = detector;
    }

    /// Sets how quickly a rising level is followed, in milliseconds. Zero
    /// follows it at once. Real-time safe.
    pub fn set_attack_ms(&mut self, milliseconds: f64) {
        self.attack_ms = milliseconds;
        self.update();
    }

    /// Sets how quickly a falling level is followed, in milliseconds. Zero
    /// follows it at once. Real-time safe.
    pub fn set_release_ms(&mut self, milliseconds: f64) {
        self.release_ms = milliseconds;
        self.update();
    }

    /// Chooses whether block processing keeps one level for all channels
    /// (`true`) or one for each (`false`, the default). Real-time safe.
    ///
    /// The reading carries over, so that it does not jump to whatever the
    /// other memory happened to hold. On linking, the shared level starts
    /// from the channels' own levels taken together: the largest of them
    /// for peak detection, their mean power for RMS. On unlinking, every
    /// channel starts from the shared level. Setting what is already set
    /// changes nothing.
    pub fn set_linked(&mut self, linked: bool) {
        if linked == self.linked {
            return;
        }
        if linked {
            self.shared = self.combine(self.channels.iter().copied());
        } else {
            self.channels.fill(self.shared);
        }
        self.linked = linked;
    }

    /// Changes the sample rate in hertz, for use without
    /// [`prepare`](Processor::prepare). Real-time safe.
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.update();
    }

    /// Builder form of [`set_detector`](Self::set_detector).
    #[must_use]
    pub fn with_detector(mut self, detector: LevelDetector) -> Self {
        self.set_detector(detector);
        self
    }

    /// Builder form of [`set_attack_ms`](Self::set_attack_ms).
    #[must_use]
    pub fn with_attack_ms(mut self, milliseconds: f64) -> Self {
        self.set_attack_ms(milliseconds);
        self
    }

    /// Builder form of [`set_release_ms`](Self::set_release_ms).
    #[must_use]
    pub fn with_release_ms(mut self, milliseconds: f64) -> Self {
        self.set_release_ms(milliseconds);
        self
    }

    /// Builder form of [`set_linked`](Self::set_linked).
    #[must_use]
    pub fn with_linked(mut self, linked: bool) -> Self {
        self.set_linked(linked);
        self
    }

    /// Whether block processing keeps one level for all channels.
    pub fn is_linked(&self) -> bool {
        self.linked
    }

    fn update(&mut self) {
        self.attack = time_coefficient(self.attack_ms, self.sample_rate);
        self.release = time_coefficient(self.release_ms, self.sample_rate);
    }

    /// Turns a sample into the positive quantity that is smoothed.
    #[inline]
    fn rectify(&self, sample: f64) -> f64 {
        // Not-a-number becomes silence, and anything beyond the ceiling the
        // ceiling, so that one broken sample cannot hold the reading up for
        // long. Everything the memory ever holds comes through here.
        let size = limit(sample.abs(), 0.0, CEILING);
        match self.detector {
            LevelDetector::Peak => size,
            LevelDetector::Rms => size * size,
        }
    }

    /// What several rectified samples, or several memories, count as when
    /// they are taken together: the largest of them for peak detection,
    /// their mean for RMS. None at all count as silence.
    #[inline]
    fn combine(&self, values: impl Iterator<Item = f64>) -> f64 {
        let mut combined = 0.0f64;
        let mut count = 0usize;
        for value in values {
            combined = match self.detector {
                LevelDetector::Peak => combined.max(value),
                LevelDetector::Rms => combined + value,
            };
            count += 1;
        }
        if self.detector == LevelDetector::Rms && count > 0 {
            combined /= count as f64;
        }
        combined
    }

    /// What one frame of a buffer counts as when its first `channels`
    /// channels are linked.
    #[inline]
    fn combine_frame(&self, buffer: &AudioBuffer<S>, frame: usize, channels: usize) -> f64 {
        self.combine(
            (0..channels).map(|channel| self.rectify(buffer.channel(channel)[frame].to_f64())),
        )
    }

    /// One step of the smoothing filter. `input` comes from `rectify`, so it
    /// is a number and within bounds.
    #[inline]
    fn smooth(&self, state: f64, input: f64) -> f64 {
        let coefficient = if input > state {
            self.attack
        } else {
            self.release
        };
        let next = input + coefficient * (state - input);
        if next >= FLOOR { next } else { 0.0 }
    }

    /// Turns the smoothed quantity into a level.
    #[inline]
    fn reading(&self, state: f64) -> f64 {
        match self.detector {
            LevelDetector::Peak => state,
            LevelDetector::Rms => state.sqrt(),
        }
    }

    /// Follows one sample of one channel and returns that channel's level.
    /// Real-time safe.
    ///
    /// Each channel has its own memory here, whether or not the follower is
    /// linked: read it back with [`channel_level`](Self::channel_level). A
    /// channel the follower was not prepared for is not followed and reads
    /// as silence.
    #[inline]
    pub fn process_sample(&mut self, channel: usize, input: S) -> S {
        let Some(&state) = self.channels.get(channel) else {
            return S::ZERO;
        };
        let next = self.smooth(state, self.rectify(input.to_f64()));
        self.channels[channel] = next;
        S::from_f64(self.reading(next))
    }

    /// Follows one frame, the samples of all channels at one instant, and
    /// returns the level they share. Real-time safe.
    ///
    /// The frame counts as its largest sample for peak detection and as its
    /// mean power for RMS. An empty frame counts as silence. This is the
    /// linked follower driven a sample at a time. It works on the shared
    /// memory whether or not the follower is linked, and leaves the memory
    /// of [`process_sample`](Self::process_sample) alone: read it back with
    /// [`shared_level`](Self::shared_level).
    #[inline]
    pub fn process_frame(&mut self, frame: &[S]) -> S {
        let combined = self.combine(frame.iter().map(|sample| self.rectify(sample.to_f64())));
        self.shared = self.smooth(self.shared, combined);
        S::from_f64(self.reading(self.shared))
    }

    /// Follows a block without changing it. Read the result with
    /// [`level`](Self::level). Real-time safe.
    ///
    /// This is the call for a meter or a sidechain, where the audio itself
    /// must pass untouched. Channels the follower was not prepared for are
    /// not looked at, linked or not.
    pub fn follow(&mut self, buffer: &AudioBuffer<S>) {
        if self.linked {
            let channels = buffer.num_channels().min(self.channels.len());
            for frame in 0..buffer.num_frames() {
                let combined = self.combine_frame(buffer, frame, channels);
                self.shared = self.smooth(self.shared, combined);
            }
        } else {
            for (index, samples) in buffer.iter_channels().enumerate() {
                let Some(&start) = self.channels.get(index) else {
                    break;
                };
                let mut state = start;
                for &sample in samples {
                    state = self.smooth(state, self.rectify(sample.to_f64()));
                }
                self.channels[index] = state;
            }
        }
    }

    /// The level the block calls last arrived at: that of one channel, or
    /// the one all channels share when the follower is linked.
    ///
    /// This follows [`set_linked`](Self::set_linked), not the call that was
    /// used last. After [`process_sample`](Self::process_sample) on a linked
    /// follower, or [`process_frame`](Self::process_frame) on one that is not
    /// linked, read [`channel_level`](Self::channel_level) or
    /// [`shared_level`](Self::shared_level) instead.
    pub fn level(&self, channel: usize) -> S {
        if self.linked {
            self.shared_level()
        } else {
            self.channel_level(channel)
        }
    }

    /// The level of one channel's own memory, whether or not the follower is
    /// linked: what [`process_sample`](Self::process_sample) last returned
    /// for it. A channel the follower was not prepared for reads as silence.
    pub fn channel_level(&self, channel: usize) -> S {
        let state = self.channels.get(channel).copied().unwrap_or(0.0);
        S::from_f64(self.reading(state))
    }

    /// The level all channels share, whether or not the follower is linked:
    /// what [`process_frame`](Self::process_frame) last returned.
    pub fn shared_level(&self) -> S {
        S::from_f64(self.reading(self.shared))
    }
}

impl<S: Sample> Processor<S> for EnvelopeFollower<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.update();
        self.channels = vec![0.0; spec.num_channels];
        self.shared = 0.0;
    }

    /// Replaces the audio by its level: each channel by its own, or every
    /// channel by the shared one when the follower is linked. Channels the
    /// follower was not prepared for are left as they are, and when linked
    /// they take no part in the shared level either.
    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        if self.linked {
            let channels = buffer.num_channels().min(self.channels.len());
            for frame in 0..buffer.num_frames() {
                let combined = self.combine_frame(buffer, frame, channels);
                self.shared = self.smooth(self.shared, combined);
                let level = S::from_f64(self.reading(self.shared));
                for channel in 0..channels {
                    buffer.channel_mut(channel)[frame] = level;
                }
            }
        } else {
            for (index, samples) in buffer.iter_channels_mut().enumerate() {
                let Some(&start) = self.channels.get(index) else {
                    break;
                };
                let mut state = start;
                for sample in samples {
                    state = self.smooth(state, self.rectify(sample.to_f64()));
                    *sample = S::from_f64(self.reading(state));
                }
                self.channels[index] = state;
            }
        }
    }

    fn reset(&mut self) {
        self.channels.fill(0.0);
        self.shared = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;
    use crate::numeric::strict_peak;

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 4_800,
            num_channels: channels,
        }
    }

    fn prepared(detector: LevelDetector, attack_ms: f64, release_ms: f64) -> EnvelopeFollower<f64> {
        let mut follower = EnvelopeFollower::new()
            .with_detector(detector)
            .with_attack_ms(attack_ms)
            .with_release_ms(release_ms);
        follower.prepare(&spec(1));
        follower
    }

    #[test]
    fn peak_attack_and_release_match_the_analytic_step_response() {
        // 5 ms is 240 samples, 50 ms is 2400.
        let mut follower = prepared(LevelDetector::Peak, 5.0, 50.0);
        let attack = (-1.0f64 / 240.0).exp();
        let release = (-1.0f64 / 2_400.0).exp();

        // A step from silence to 0.8: level[n] = 0.8 (1 - c^(n+1)).
        let mut level = 0.0;
        for n in 0..2_400 {
            level = follower.process_sample(0, 0.8);
            let expected = 0.8 * (1.0 - attack.powi(n + 1));
            assert!((level - expected).abs() < 1e-12, "attack, sample {n}");
            if n == 239 {
                // One attack time in: 63.2% of the way.
                assert!((level - 0.8 * (1.0 - (-1.0f64).exp())).abs() < 1e-12);
            }
        }
        // Then a step down to silence: level[n] = start * c^(n+1).
        let start = level;
        for n in 0..12_000 {
            level = follower.process_sample(0, 0.0);
            let expected = start * release.powi(n + 1);
            assert!((level - expected).abs() < 1e-12, "release, sample {n}");
            if n == 2_399 {
                // One release time in: 36.8% is left.
                assert!((level - start * (-1.0f64).exp()).abs() < 1e-12);
            }
        }
        // A negative input counts by its size.
        let mut negative = prepared(LevelDetector::Peak, 5.0, 50.0);
        let mut positive = prepared(LevelDetector::Peak, 5.0, 50.0);
        for _ in 0..100 {
            assert_eq!(
                negative.process_sample(0, -0.3),
                positive.process_sample(0, 0.3)
            );
        }
    }

    #[test]
    fn rms_attack_and_release_match_the_analytic_step_response() {
        let mut follower = prepared(LevelDetector::Rms, 5.0, 50.0);
        let attack = (-1.0f64 / 240.0).exp();
        let release = (-1.0f64 / 2_400.0).exp();

        // The power follows the one-pole curve, and the reading is its root.
        let mut level = 0.0;
        for n in 0..2_400 {
            level = follower.process_sample(0, -0.8);
            let expected = 0.8 * (1.0 - attack.powi(n + 1)).sqrt();
            assert!((level - expected).abs() < 1e-12, "attack, sample {n}");
        }
        let start = level;
        for n in 0..12_000 {
            level = follower.process_sample(0, 0.0);
            let expected = start * release.powi(n + 1).sqrt();
            assert!((level - expected).abs() < 1e-12, "release, sample {n}");
        }
    }

    #[test]
    fn a_sine_reads_its_rms_and_its_peak() {
        let sine = |n: usize| 0.5 * (TAU * 1_000.0 * n as f64 / RATE).sin();

        // Equal times make a true RMS meter: amplitude over the root of two.
        let mut rms = prepared(LevelDetector::Rms, 50.0, 50.0);
        let (mut lowest, mut highest) = (f64::MAX, 0.0f64);
        for n in 0..48_000 {
            let level = rms.process_sample(0, sine(n));
            if n >= 24_000 {
                lowest = lowest.min(level);
                highest = highest.max(level);
            }
        }
        let expected = 0.5 / 2f64.sqrt();
        assert!((lowest - expected).abs() < 0.001 && (highest - expected).abs() < 0.001);
        // What is left is the ripple at twice the frequency.
        assert!(highest - lowest > 0.0 && highest - lowest < 0.001);

        // An instant attack and a slow release hold the peaks.
        let mut peak = prepared(LevelDetector::Peak, 0.0, 500.0);
        let mut lowest = f64::MAX;
        for n in 0..48_000 {
            let level = peak.process_sample(0, sine(n));
            assert!(level <= 0.5 + 1e-12);
            if n >= 24_000 {
                lowest = lowest.min(level);
            }
        }
        // Between two peaks, half a millisecond apart, a 500 ms release
        // lets go of one part in a thousand.
        assert!(lowest > 0.5 * 0.998, "sagged to {lowest}");
    }

    #[test]
    fn zero_times_follow_at_once() {
        let mut follower = prepared(LevelDetector::Peak, 0.0, 0.0);
        for input in [0.5, -0.25, 0.0, 2.0, 1e-3] {
            assert_eq!(follower.process_sample(0, input), input.abs());
        }
        // Negative times mean the same.
        follower.set_attack_ms(-5.0);
        follower.set_release_ms(f64::NAN);
        assert_eq!(follower.process_sample(0, 0.75), 0.75);
        assert_eq!(follower.process_sample(0, 0.0), 0.0);
    }

    #[test]
    fn blocks_and_single_samples_give_the_same_result() {
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            for linked in [false, true] {
                let mut signal = AudioBuffer::<f64>::new(2, 3_000);
                for (index, channel) in signal.iter_channels_mut().enumerate() {
                    for (n, sample) in channel.iter_mut().enumerate() {
                        let burst = if (n / 500) % 2 == 0 { 1.0 } else { 0.05 };
                        *sample = burst * ((n * (3 + 2 * index)) % 17) as f64 / 8.5 - burst;
                    }
                }
                let build = || {
                    let mut follower = EnvelopeFollower::<f64>::new()
                        .with_detector(detector)
                        .with_attack_ms(2.0)
                        .with_release_ms(20.0)
                        .with_linked(linked);
                    follower.prepare(&spec(2));
                    follower
                };

                // Sample by sample.
                let mut by_sample = build();
                let mut expected = signal.clone();
                for n in 0..3_000 {
                    let frame = [signal.channel(0)[n], signal.channel(1)[n]];
                    if linked {
                        let level = by_sample.process_frame(&frame);
                        expected.channel_mut(0)[n] = level;
                        expected.channel_mut(1)[n] = level;
                    } else {
                        expected.channel_mut(0)[n] = by_sample.process_sample(0, frame[0]);
                        expected.channel_mut(1)[n] = by_sample.process_sample(1, frame[1]);
                    }
                }
                assert!(expected.peak() > 0.2);

                // In blocks of every size, in place.
                for block in [1, 7, 64, 997, 3_000] {
                    let mut by_block = build();
                    let mut rendered = signal.clone();
                    let mut start = 0;
                    while start < 3_000 {
                        let end = (start + block).min(3_000);
                        rendered.windowed(start..end, |piece| by_block.process(piece));
                        start = end;
                    }
                    assert!(
                        rendered == expected,
                        "{detector:?}, linked {linked}, blocks of {block}"
                    );

                    // And without touching the audio.
                    let mut watching = build();
                    let mut untouched = signal.clone();
                    let mut start = 0;
                    while start < 3_000 {
                        let end = (start + block).min(3_000);
                        untouched.windowed(start..end, |piece| watching.follow(piece));
                        start = end;
                    }
                    assert!(untouched == signal);
                    for channel in 0..2 {
                        assert_eq!(watching.level(channel), expected.channel(channel)[2_999]);
                        assert_eq!(by_block.level(channel), expected.channel(channel)[2_999]);
                    }
                }
            }
        }
    }

    #[test]
    fn linked_channels_share_one_level_and_separate_ones_do_not() {
        let mut buffer = AudioBuffer::<f64>::new(2, 4_800);
        buffer.channel_mut(0).fill(0.8);
        buffer.channel_mut(1).fill(-0.2);

        // Separate: each channel settles on its own level.
        let mut separate = EnvelopeFollower::<f64>::new().with_attack_ms(1.0);
        separate.prepare(&spec(2));
        separate.follow(&buffer);
        assert!((separate.level(0) - 0.8).abs() < 1e-9);
        assert!((separate.level(1) - 0.2).abs() < 1e-9);

        // Linked, peak: both read the larger one.
        let mut linked = EnvelopeFollower::<f64>::new()
            .with_attack_ms(1.0)
            .with_linked(true);
        linked.prepare(&spec(2));
        linked.follow(&buffer);
        assert!(linked.is_linked());
        assert!((linked.level(0) - 0.8).abs() < 1e-9);
        assert_eq!(linked.level(0), linked.level(1));

        // Linked, RMS: both read the root of the mean power of the frame.
        let mut linked = EnvelopeFollower::<f64>::new()
            .with_detector(LevelDetector::Rms)
            .with_attack_ms(1.0)
            .with_linked(true);
        linked.prepare(&spec(2));
        linked.follow(&buffer);
        let expected = ((0.8f64 * 0.8 + 0.2 * 0.2) / 2.0).sqrt();
        assert!((linked.level(1) - expected).abs() < 1e-9);

        // As a processor it writes the shared level to every channel.
        linked.reset();
        linked.process(&mut buffer);
        assert_eq!(buffer.channel(0), buffer.channel(1));
        assert!((buffer.channel(0)[4_799] - expected).abs() < 1e-9);
    }

    #[test]
    fn changing_the_detector_keeps_the_reading() {
        let mut follower = prepared(LevelDetector::Peak, 1.0, 1_000.0);
        let mut level = 0.0;
        for _ in 0..4_800 {
            level = follower.process_sample(0, 0.6);
        }
        follower.set_detector(LevelDetector::Rms);
        assert!((follower.level(0) - level).abs() < 1e-12);
        follower.set_detector(LevelDetector::Peak);
        assert!((follower.level(0) - level).abs() < 1e-12);
    }

    #[test]
    fn bad_samples_do_not_stick_and_silence_reaches_zero() {
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            let mut follower = prepared(detector, 1.0, 10.0);
            for input in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e200, f64::MAX] {
                let level = follower.process_sample(0, input);
                assert!(level.is_finite(), "{detector:?} after {input}: {level}");
            }
            // 10 ms of release: after ten seconds of silence nothing at all
            // is left, not even a number too small to compute with quickly.
            let mut level = 1.0;
            for _ in 0..480_000 {
                level = follower.process_sample(0, 0.0);
            }
            assert_eq!(level, 0.0, "{detector:?}");
            assert_eq!(follower.process_frame(&[]), 0.0);
        }
    }

    /// Found in review: an infinite sample used to be stored as the largest
    /// double. That is far beyond the range of `f32`, the default sample
    /// type, so `EnvelopeFollower::<f32>` returned infinity for it, with
    /// both detectors.
    #[test]
    fn a_single_precision_follower_never_reads_infinity() {
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            // With the times a new follower has, and with an attack that
            // takes a sample at once, which is the worst case.
            for attack_ms in [10.0, 0.0] {
                let mut follower = EnvelopeFollower::<f32>::new()
                    .with_detector(detector)
                    .with_attack_ms(attack_ms);
                follower.prepare(&spec(1));
                for bad in [f32::NAN, f32::MAX, f32::INFINITY, f32::NEG_INFINITY] {
                    let level = follower.process_sample(0, bad);
                    assert!(
                        level.is_finite(),
                        "{detector:?}: the reading after a sample of {bad} is {level}"
                    );
                    let level = follower.process_frame(&[bad, 0.0]);
                    assert!(
                        level.is_finite(),
                        "{detector:?}: the reading after a frame with {bad} is {level}"
                    );
                }
                let level = follower.process_sample(0, 0.0);
                assert!(
                    level.is_finite(),
                    "{detector:?}: still {level} a sample later"
                );

                // The same through the block calls, linked and not.
                for linked in [false, true] {
                    follower.set_linked(linked);
                    let mut block = AudioBuffer::<f32>::new(1, 8);
                    block.channel_mut(0).copy_from_slice(&[
                        0.5,
                        f32::INFINITY,
                        f32::NAN,
                        f32::MIN,
                        0.0,
                        1.0,
                        0.0,
                        0.0,
                    ]);
                    follower.follow(&block);
                    assert!(follower.level(0).is_finite());
                    follower.process(&mut block);
                    assert!(strict_peak(block.channel(0)) <= CEILING);
                }
            }
        }
    }

    /// Found in review: "one bad sample cannot get stuck in the memory for
    /// ever" was true only to the letter. An infinite sample was stored as
    /// about 1e308, which a release needs some 700 time constants to bring
    /// back to full scale: with the default release of 100 ms the reading
    /// stayed above full scale for 70 seconds, and was still 7e283 after
    /// five.
    #[test]
    fn an_infinite_sample_is_forgotten_within_fifty_release_times() {
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            let mut follower = EnvelopeFollower::<f64>::new()
                .with_detector(detector)
                .with_release_ms(100.0);
            follower.prepare(&spec(1));
            follower.process_sample(0, f64::INFINITY);
            // Fifty release times: five seconds.
            let mut level = f64::INFINITY;
            for _ in 0..240_000 {
                level = follower.process_sample(0, 0.0);
            }
            assert!(
                level < 1.0,
                "{detector:?}: five seconds after one infinite sample the reading is {level:e}"
            );
        }
    }

    #[test]
    fn a_sample_beyond_the_ceiling_counts_as_the_ceiling() {
        // An attack that takes a sample at once shows what the sample
        // counted as, and is the worst case for how long it is remembered.
        // The release time is 100 ms: 4,800 samples.
        for (detector, release_times) in [(LevelDetector::Peak, 14), (LevelDetector::Rms, 28)] {
            let mut follower = prepared(detector, 0.0, 100.0);
            // Up to a million times full scale a sample is what it is.
            for sample in [16.0, -999_999.0, 1e6] {
                let level = follower.process_sample(0, sample);
                assert!((level - sample.abs()).abs() < 1e-9, "{detector:?}");
                follower.reset();
            }
            // Beyond that it is a million.
            for sample in [1.000_001e6, -1e30, 1e300, f64::MAX, f64::NEG_INFINITY] {
                let level = follower.process_sample(0, sample);
                assert!((level - 1e6).abs() < 1e-9, "{detector:?}: {level}");
                follower.reset();
            }
            // And something that is not a number is silence: it does not
            // raise the level, and the release goes on through it.
            follower.process_sample(0, 0.5);
            let after = follower.process_sample(0, f64::NAN);
            assert!(after > 0.49 && after < 0.5, "{detector:?}: {after}");
            follower.reset();

            // The figures quoted in the documentation: after an infinite
            // sample the reading is back below full scale within 14 release
            // times for peak detection and 28 for RMS, and not a whole
            // release time sooner.
            follower.process_sample(0, f64::INFINITY);
            let mut samples = 0;
            while follower.process_sample(0, 0.0) >= 1.0 {
                samples += 1;
                assert!(samples < 1_000_000, "{detector:?}: it never comes back");
            }
            assert!(
                samples > (release_times - 1) * 4_800 && samples < release_times * 4_800,
                "{detector:?}: back at full scale after {samples} samples"
            );

            // A frame is limited sample by sample, before it is combined.
            follower.reset();
            let level = follower.process_frame(&[f64::INFINITY, 0.0]);
            let expected = match detector {
                LevelDetector::Peak => 1e6,
                LevelDetector::Rms => 1e6 / 2f64.sqrt(),
            };
            assert!((level - expected).abs() < 1e-6, "{detector:?}: {level}");
        }

        // In single precision too.
        let mut follower = EnvelopeFollower::<f32>::new().with_attack_ms(0.0);
        follower.prepare(&spec(1));
        assert_eq!(follower.process_sample(0, f32::INFINITY), 1e6);
        assert_eq!(follower.process_sample(0, f32::MAX), 1e6);
    }

    /// Found in review: toggling `set_linked` switched between two memories
    /// without handing the reading over, so the level jumped to whatever
    /// the other memory had last been left with.
    #[test]
    fn linking_and_unlinking_carry_the_reading_over() {
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            let mut follower = EnvelopeFollower::<f64>::new()
                .with_detector(detector)
                .with_attack_ms(1.0)
                .with_release_ms(1_000.0);
            follower.prepare(&spec(2));

            // Something else was once measured with the shared memory, and
            // it has been left holding a high level.
            for _ in 0..4_800 {
                follower.process_frame(&[3.0, 3.0]);
            }
            assert!((follower.shared_level() - 3.0).abs() < 1e-9);

            // Two channels at steady levels, followed separately.
            let mut buffer = AudioBuffer::<f64>::new(2, 4_800);
            buffer.channel_mut(0).fill(0.8);
            buffer.channel_mut(1).fill(-0.2);
            follower.follow(&buffer);
            let (loud, quiet) = (follower.level(0), follower.level(1));
            assert!((loud - 0.8).abs() < 1e-9 && (quiet - 0.2).abs() < 1e-9);

            // Linking starts from those two taken together, not from the
            // stale 3.0.
            follower.set_linked(true);
            let together = match detector {
                LevelDetector::Peak => loud,
                LevelDetector::Rms => ((loud * loud + quiet * quiet) / 2.0).sqrt(),
            };
            assert!((follower.level(0) - together).abs() < 1e-12, "{detector:?}");
            assert_eq!(follower.level(0), follower.level(1));
            // That is the level the same signal has when it is followed
            // linked, so carrying on makes no jump either.
            let mut linked = buffer.clone();
            follower.process(&mut linked);
            for &level in linked.channel(1) {
                assert!((level - together).abs() < 1e-9, "{detector:?}: {level}");
            }
            // Setting what is already set does not hand anything over.
            follower.process_sample(0, 5.0);
            follower.set_linked(true);
            assert!((follower.level(0) - together).abs() < 1e-9);

            // Meanwhile the channels' own memories have gone stale: one of
            // them has just been fed something loud. Unlinking starts every
            // channel from the shared level.
            assert!(follower.channel_level(0) > together);
            let shared = follower.shared_level();
            follower.set_linked(false);
            assert_eq!(follower.level(0), shared);
            assert_eq!(follower.level(1), shared);
            // From there each channel goes its own way again.
            let mut separate = buffer.clone();
            follower.process(&mut separate);
            let first = (separate.channel(0)[0], separate.channel(1)[0]);
            assert!((first.0 - shared).abs() < 0.05 && (first.1 - shared).abs() < 0.05);
            assert!(separate.channel(0)[4_799] > separate.channel(1)[4_799]);
        }
    }

    /// Found in review: with the follower not linked, `process_frame`
    /// returned 0.5 and `level` then read 0, with nothing to say why and no
    /// other way to read the level back.
    #[test]
    fn each_memory_can_be_read_whatever_the_follower_is_set_to() {
        let mut follower = prepared(LevelDetector::Peak, 0.0, 100.0);
        follower.prepare(&spec(2));
        assert!(!follower.is_linked());

        assert_eq!(follower.process_frame(&[0.5, -0.5]), 0.5);
        assert_eq!(follower.shared_level(), 0.5);
        // `level` follows the setting, and the channels' own memories have
        // not been touched.
        assert_eq!(follower.level(0), 0.0);
        assert_eq!(follower.channel_level(0), 0.0);

        assert_eq!(follower.process_sample(1, -0.25), 0.25);
        assert_eq!(follower.channel_level(1), 0.25);
        assert_eq!(follower.level(1), 0.25);
        // Nor has this touched the shared memory.
        assert_eq!(follower.shared_level(), 0.5);

        // Linked, `level` reads the shared memory for every channel, and
        // each channel's own memory can still be driven and read.
        follower.set_linked(true);
        assert_eq!(follower.level(0), follower.shared_level());
        assert_eq!(follower.process_sample(0, 0.75), 0.75);
        assert_eq!(follower.channel_level(0), 0.75);
        assert_eq!(follower.level(0), follower.shared_level());
        assert!(follower.shared_level() < 0.75);
        // Channels that do not exist read as silence.
        assert_eq!(follower.channel_level(9), 0.0);
    }

    #[test]
    fn the_default_times_make_an_rms_reading_lean_towards_the_peaks() {
        // The figure quoted in the documentation: a 1 kHz sine of amplitude
        // one reads 0.90 with a 10 ms attack and a 100 ms release, where a
        // true RMS meter reads 0.707.
        let mut follower = EnvelopeFollower::<f64>::new().with_detector(LevelDetector::Rms);
        follower.prepare(&spec(1));
        let (mut lowest, mut highest) = (f64::MAX, 0.0f64);
        for n in 0..96_000 {
            let level = follower.process_sample(0, (TAU * 1_000.0 * n as f64 / RATE).sin());
            if n >= 48_000 {
                lowest = lowest.min(level);
                highest = highest.max(level);
            }
        }
        assert!(
            lowest > 0.895 && highest < 0.905,
            "it reads from {lowest} to {highest}"
        );
    }

    #[test]
    fn unprepared_channels_read_as_silence() {
        let mut follower = EnvelopeFollower::<f32>::new();
        // One channel exists before `prepare`, for use a sample at a time.
        assert!(follower.process_sample(0, 1.0) > 0.0);
        assert_eq!(follower.process_sample(5, 1.0), 0.0);
        assert_eq!(follower.level(5), 0.0);

        // A buffer wider than the follower: the extra channel is left alone.
        let mut buffer = AudioBuffer::<f32>::new(2, 8);
        buffer.channel_mut(1).fill(0.5);
        follower.process(&mut buffer);
        follower.follow(&buffer);
        assert_eq!(buffer.channel(1), &[0.5; 8]);

        follower.reset();
        assert_eq!(follower.level(0), 0.0);
    }

    /// Found in review: "Channels the follower was not prepared for are left
    /// as they are" held only while the channels were followed separately.
    /// Linked, every channel of the buffer was both measured and
    /// overwritten: prepared for one channel and given three, the follower
    /// wrote a level taken from the 0.9 of the third channel over all of
    /// them.
    #[test]
    fn a_linked_follower_leaves_channels_it_was_not_prepared_for_alone() {
        let mut follower = EnvelopeFollower::<f64>::new().with_linked(true);
        follower.prepare(&spec(1));
        let mut buffer = AudioBuffer::<f64>::new(3, 64);
        buffer.channel_mut(0).fill(0.1);
        buffer.channel_mut(1).fill(0.5);
        buffer.channel_mut(2).fill(0.9);
        let original = buffer.clone();
        follower.process(&mut buffer);
        assert_eq!(buffer.channel(1), &[0.5; 64]);
        assert_eq!(buffer.channel(2), &[0.9; 64]);

        // Nor did the other channels take part in the level: it is the one
        // a follower of the first channel alone arrives at.
        let mut alone = EnvelopeFollower::<f64>::new();
        alone.prepare(&spec(1));
        let mut expected = AudioBuffer::<f64>::new(1, 64);
        expected.channel_mut(0).fill(0.1);
        alone.process(&mut expected);
        assert_eq!(buffer.channel(0), expected.channel(0));
        assert!(buffer.channel(0)[63] > 0.0 && buffer.channel(0)[63] < 0.1);

        // `follow` measures the same channels as `process`.
        let mut watching = EnvelopeFollower::<f64>::new().with_linked(true);
        watching.prepare(&spec(1));
        watching.follow(&original);
        assert_eq!(watching.level(0), buffer.channel(0)[63]);

        // Prepared for two of the three, the third is still left out.
        for detector in [LevelDetector::Peak, LevelDetector::Rms] {
            let mut follower = EnvelopeFollower::<f64>::new()
                .with_detector(detector)
                .with_attack_ms(0.0)
                .with_linked(true);
            follower.prepare(&spec(2));
            let mut buffer = original.clone();
            follower.process(&mut buffer);
            let expected = match detector {
                LevelDetector::Peak => 0.5,
                LevelDetector::Rms => ((0.1f64 * 0.1 + 0.5 * 0.5) / 2.0).sqrt(),
            };
            assert!((buffer.channel(0)[63] - expected).abs() < 1e-12);
            assert_eq!(buffer.channel(0), buffer.channel(1));
            assert_eq!(buffer.channel(2), &[0.9; 64]);
        }
    }
}
