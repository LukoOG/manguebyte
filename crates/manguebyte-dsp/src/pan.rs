use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::mix::MixLaw;
use crate::numeric::limit;
use crate::{ProcessSpec, Processor};

/// What a [`Panner`] does with the first two channels of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PanMode {
    /// Stereo balance: the left channel is scaled by the left gain and the
    /// right channel by the right gain, and neither is moved to the other
    /// side.
    ///
    /// The gains are scaled so that the centre leaves the signal exactly as
    /// it was. Turning to one side fades the other out and raises the
    /// remaining side by as much as the law takes off at the centre: 3 dB
    /// for [`MixLaw::EqualPower`], which keeps the total power of an
    /// ordinary stereo signal steady, and nothing for [`MixLaw::Balanced`],
    /// which is the classic balance control.
    #[default]
    StereoBalance,
    /// Mono to stereo: the first channel holds a mono signal, which is
    /// written to the left and right channels at the levels the law gives.
    /// Whatever was in the second channel is replaced.
    ///
    /// In the centre each side is down by the law's centre level, 3 dB for
    /// [`MixLaw::EqualPower`].
    MonoToStereo,
}

/// Places a signal between left and right.
///
/// The position runs from `-1.0` (left) through `0.0` (centre) to `1.0`
/// (right). A [`MixLaw`] turns it into a gain for each side, and
/// [`PanMode`] says whether the buffer holds a stereo signal to be balanced
/// or a mono signal to be placed.
///
/// No change of setting clicks. The two gains glide to their new values in
/// a straight line, and a change of mode also cross-fades what feeds the
/// right output, from the right input to the left one or back, over the
/// same time.
///
/// Only the first two channels are touched. A buffer with a single channel
/// has no second side to pan to and is left as it is.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{MixLaw, PanMode, Panner, ProcessSpec, Processor};
///
/// // A mono source in the first channel, placed half-way to the right.
/// let mut panner = Panner::<f32>::new()
///     .with_mode(PanMode::MonoToStereo)
///     .with_law(MixLaw::EqualPower)
///     .with_pan(0.5);
/// panner.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 2 });
///
/// let mut block = AudioBuffer::<f32>::new(2, 64);
/// block.channel_mut(0).fill(1.0);
/// panner.process(&mut block);
///
/// let (left, right) = (block.channel(0)[0], block.channel(1)[0]);
/// assert!(right > left);
/// // Constant power: the two sides together are as loud as the source.
/// assert!((left * left + right * right - 1.0).abs() < 1e-6);
/// ```
///
/// # Limits
///
/// It is the gains that glide, not the position, so half-way through a jump
/// from one side to the other the level is not what the law gives for the
/// centre. Half-way through a change of mode the right output carries half
/// of each input, which is quieter than either if the two have nothing in
/// common, and silent if one is the other upside down.
#[derive(Debug, Clone)]
pub struct Panner<S: Sample = f32> {
    law: MixLaw,
    mode: PanMode,
    pan: f64,
    ramp_seconds: f64,
    left: Smoothed<S>,
    right: Smoothed<S>,
    /// How much of the right output is taken from the left input: none of
    /// it when balancing, all of it when placing a mono source, and
    /// something in between while a change of mode cross-fades.
    from_left: Smoothed<S>,
}

impl<S: Sample> Default for Panner<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> Panner<S> {
    /// Default glide time for changes of position, law or mode.
    pub const DEFAULT_RAMP_SECONDS: f64 = 0.02;

    /// Creates a panner in the centre, with the [`MixLaw::EqualPower`] law,
    /// in [`PanMode::StereoBalance`]: it changes nothing until it is moved.
    pub fn new() -> Self {
        Self {
            law: MixLaw::EqualPower,
            mode: PanMode::StereoBalance,
            pan: 0.0,
            ramp_seconds: Self::DEFAULT_RAMP_SECONDS,
            left: Smoothed::new(S::ONE),
            right: Smoothed::new(S::ONE),
            from_left: Smoothed::new(S::ZERO),
        }
    }

    /// Sets the position, from `-1.0` (left) to `1.0` (right). Real-time
    /// safe.
    pub fn set_pan(&mut self, pan: f64) {
        self.pan = if pan.is_nan() {
            0.0
        } else {
            limit(pan, -1.0, 1.0)
        };
        self.retarget();
    }

    /// Sets the pan law. Real-time safe.
    pub fn set_law(&mut self, law: MixLaw) {
        self.law = law;
        self.retarget();
    }

    /// Sets what the buffer is taken to hold. The gains glide to those of
    /// the new mode, and the right output cross-fades from one input to the
    /// other. Real-time safe.
    pub fn set_mode(&mut self, mode: PanMode) {
        self.mode = mode;
        self.retarget();
    }

    /// Builder form of [`set_pan`](Self::set_pan).
    #[must_use]
    pub fn with_pan(mut self, pan: f64) -> Self {
        self.set_pan(pan);
        self
    }

    /// Builder form of [`set_law`](Self::set_law).
    #[must_use]
    pub fn with_law(mut self, law: MixLaw) -> Self {
        self.set_law(law);
        self
    }

    /// Builder form of [`set_mode`](Self::set_mode).
    #[must_use]
    pub fn with_mode(mut self, mode: PanMode) -> Self {
        self.set_mode(mode);
        self
    }

    /// Sets how long a change takes, in seconds. Applies from the next
    /// [`prepare`](Processor::prepare).
    #[must_use]
    pub fn with_ramp(mut self, seconds: f64) -> Self {
        self.ramp_seconds = seconds;
        self
    }

    /// The position, as last set.
    pub fn pan(&self) -> f64 {
        self.pan
    }

    /// The left and right gains the current settings ask for. While a
    /// change is gliding, the gains in use are still on their way there.
    pub fn gains(&self) -> (f64, f64) {
        let (left, right) = self.law.gains(0.5 * (self.pan + 1.0));
        match self.mode {
            PanMode::MonoToStereo => (left, right),
            PanMode::StereoBalance => {
                let centre = self.law.gains(0.5);
                (left / centre.0, right / centre.1)
            }
        }
    }

    /// Moves on by one sample and returns the left and right gains for it.
    /// For panning sample by sample, to somewhere other than a buffer's
    /// first two channels. Real-time safe.
    ///
    /// Which signal each gain is applied to is then the caller's business:
    /// a change of mode moves these gains and nothing else.
    #[inline]
    pub fn next_gains(&mut self) -> (S, S) {
        let (left, right, _) = self.step();
        (left, right)
    }

    /// Moves on by one sample. Returns the two gains and how much of the
    /// right output comes from the left input.
    #[inline]
    fn step(&mut self) -> (S, S, S) {
        (
            self.left.next_value(),
            self.right.next_value(),
            self.from_left.next_value(),
        )
    }

    fn retarget(&mut self) {
        let (left, right) = self.gains();
        let (left, right) = (S::from_f64(left), S::from_f64(right));
        let from_left = match self.mode {
            PanMode::MonoToStereo => S::ONE,
            PanMode::StereoBalance => S::ZERO,
        };
        // Setting a target again would start its ramp again.
        if self.left.target() != left {
            self.left.set_target(left);
        }
        if self.right.target() != right {
            self.right.set_target(right);
        }
        if self.from_left.target() != from_left {
            self.from_left.set_target(from_left);
        }
    }
}

impl<S: Sample> Processor<S> for Panner<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.left.set_ramp(spec.sample_rate, self.ramp_seconds);
        self.right.set_ramp(spec.sample_rate, self.ramp_seconds);
        self.from_left.set_ramp(spec.sample_rate, self.ramp_seconds);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        if buffer.num_channels() < 2 {
            // Nothing to pan, but time still passes for a glide.
            for _ in 0..buffer.num_frames() {
                self.step();
            }
            return;
        }
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (left, right) in left.iter_mut().zip(right) {
            let (left_gain, right_gain, from_left) = self.step();
            // The two ends are spelled out so that a settled panner passes
            // on exactly the input it is set to.
            let source = if from_left == S::ZERO {
                *right
            } else if from_left == S::ONE {
                *left
            } else {
                *right + (*left - *right) * from_left
            };
            *left *= left_gain;
            *right = source * right_gain;
        }
    }

    fn reset(&mut self) {
        self.left.set_immediate(self.left.target());
        self.right.set_immediate(self.right.target());
        self.from_left.set_immediate(self.from_left.target());
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;

    const RATE: f64 = 48_000.0;
    const SPEC: ProcessSpec = ProcessSpec {
        sample_rate: RATE,
        max_block_size: 4_800,
        num_channels: 2,
    };

    fn prepared(panner: Panner<f64>) -> Panner<f64> {
        let mut panner = panner;
        panner.prepare(&SPEC);
        panner
    }

    /// A stereo block with a 1 kHz sine on the left and a 1.5 kHz sine on
    /// the right, each of amplitude one: two unrelated signals of the same
    /// power.
    fn stereo(frames: usize) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(2, frames);
        for (index, channel) in buffer.iter_channels_mut().enumerate() {
            let frequency = 1_000.0 + 500.0 * index as f64;
            for (n, sample) in channel.iter_mut().enumerate() {
                *sample = (TAU * frequency * n as f64 / RATE).sin();
            }
        }
        buffer
    }

    fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    #[test]
    fn a_mono_source_is_placed_by_every_law() {
        for law in MixLaw::ALL {
            let place = |pan: f64| {
                let mut panner = prepared(
                    Panner::new()
                        .with_mode(PanMode::MonoToStereo)
                        .with_law(law)
                        .with_pan(pan),
                );
                // What is in the second channel must not matter.
                let mut buffer = stereo(4_800);
                let source = buffer.rms(0);
                panner.process(&mut buffer);
                (buffer.rms(0) / source, buffer.rms(1) / source)
            };

            // In the centre both sides carry the source at the law's level.
            let (left, right) = place(0.0);
            assert!((db(left) - law.center_gain_db()).abs() < 1e-9, "{law:?}");
            assert!((db(right) - law.center_gain_db()).abs() < 1e-9, "{law:?}");

            // At the ends one side has all of it and the other nothing.
            assert_eq!(place(-1.0), (1.0, 0.0), "{law:?}");
            assert_eq!(place(1.0), (0.0, 1.0), "{law:?}");

            // In between, the measured levels are the law's gains.
            for pan in [-0.8, -0.3, 0.25, 0.6] {
                let (left, right) = place(pan);
                let expected = law.gains(0.5 * (pan + 1.0));
                assert!((left - expected.0).abs() < 1e-12, "{law:?} at {pan}");
                assert!((right - expected.1).abs() < 1e-12, "{law:?} at {pan}");
            }
        }
    }

    #[test]
    fn the_nominal_centre_levels_are_the_ones_measured() {
        let centre = |law: MixLaw| {
            let mut panner = prepared(Panner::new().with_mode(PanMode::MonoToStereo).with_law(law));
            let mut buffer = stereo(4_800);
            let source = buffer.rms(0);
            panner.process(&mut buffer);
            db(buffer.rms(1) / source)
        };
        assert!((centre(MixLaw::Linear) + 6.02).abs() < 0.01);
        assert!((centre(MixLaw::EqualPower) + 3.01).abs() < 0.01);
        assert!((centre(MixLaw::Sine4Point5dB) + 4.52).abs() < 0.01);
        assert!((centre(MixLaw::Sine6dB) + 6.02).abs() < 0.01);
        assert!((centre(MixLaw::SquareRoot) + 3.01).abs() < 0.01);
        assert!((centre(MixLaw::SquareRoot4Point5dB) + 4.52).abs() < 0.01);
        assert!(centre(MixLaw::Balanced).abs() < 1e-9);
    }

    #[test]
    fn constant_power_laws_keep_the_power_and_linear_laws_keep_the_sum() {
        for step in 0..=40 {
            let pan = -1.0 + 0.05 * f64::from(step);
            for law in MixLaw::ALL {
                let mut panner = prepared(
                    Panner::new()
                        .with_mode(PanMode::MonoToStereo)
                        .with_law(law)
                        .with_pan(pan),
                );
                let mut buffer = stereo(4_800);
                let source = buffer.rms(0);
                panner.process(&mut buffer);

                let power = (buffer.rms(0).powi(2) + buffer.rms(1).powi(2)) / source.powi(2);
                // The two sides summed to mono, as a level.
                let mut sum = 0.0;
                for (left, right) in buffer.channel(0).iter().zip(buffer.channel(1)) {
                    sum += (left + right) * (left + right);
                }
                let mono = (sum / 4_800.0).sqrt() / source;

                match law {
                    MixLaw::EqualPower | MixLaw::SquareRoot => {
                        assert!((power - 1.0).abs() < 1e-12, "{law:?} at {pan}: {power}");
                    }
                    MixLaw::Linear | MixLaw::Sine6dB => {
                        assert!((mono - 1.0).abs() < 1e-12, "{law:?} at {pan}: {mono}");
                    }
                    // In between: quieter in the centre than constant power,
                    // louder there than a constant sum.
                    MixLaw::Sine4Point5dB | MixLaw::SquareRoot4Point5dB => {
                        assert!(power <= 1.0 + 1e-12 && mono >= 1.0 - 1e-12);
                    }
                    MixLaw::Balanced => assert!(power >= 1.0 - 1e-12),
                }
            }
        }
    }

    #[test]
    fn balance_leaves_the_centre_alone_and_fades_one_side() {
        for law in MixLaw::ALL {
            // In the centre nothing changes, bit for bit.
            let mut panner = prepared(Panner::new().with_law(law));
            let original = stereo(4_800);
            let mut buffer = original.clone();
            panner.process(&mut buffer);
            assert!(buffer == original, "{law:?}");

            // All the way right, the left channel is gone and the right one
            // is raised by what the law takes off at the centre. Neither
            // channel has moved to the other side.
            panner.set_pan(1.0);
            panner.reset();
            let mut buffer = original.clone();
            panner.process(&mut buffer);
            assert_eq!(buffer.rms(0), 0.0, "{law:?}");
            let lift = db(buffer.rms(1) / original.rms(1));
            assert!(
                (lift + law.center_gain_db()).abs() < 1e-9,
                "{law:?}: {lift}"
            );
            let scale = buffer.channel(1)[100] / original.channel(1)[100];
            for (after, before) in buffer.channel(1).iter().zip(original.channel(1)) {
                assert!((after - before * scale).abs() < 1e-12);
            }
        }

        // With the constant-power law, two unrelated signals of equal power
        // keep their total power wherever the control is.
        for step in 0..=20 {
            let pan = -1.0 + 0.1 * f64::from(step);
            let mut panner = prepared(Panner::new().with_pan(pan));
            let original = stereo(4_800);
            let mut buffer = original.clone();
            panner.process(&mut buffer);
            let before = original.rms(0).powi(2) + original.rms(1).powi(2);
            let after = buffer.rms(0).powi(2) + buffer.rms(1).powi(2);
            assert!((after / before - 1.0).abs() < 1e-9, "pan {pan}: {after}");
        }
    }

    #[test]
    fn a_change_of_position_is_a_ramp_of_the_stated_length() {
        // 10 ms at 48 kHz is 480 frames.
        let mut panner = Panner::<f64>::new()
            .with_mode(PanMode::MonoToStereo)
            .with_law(MixLaw::Linear)
            .with_pan(-1.0)
            .with_ramp(0.01);
        panner.prepare(&SPEC);
        panner.set_pan(1.0);

        let mut buffer = AudioBuffer::<f64>::new(2, 1_000);
        buffer.channel_mut(0).fill(1.0);
        panner.process(&mut buffer);
        for frame in 0..1_000 {
            let moved = ((frame + 1) as f64 / 480.0).min(1.0);
            assert!((buffer.channel(0)[frame] - (1.0 - moved)).abs() < 1e-12);
            assert!((buffer.channel(1)[frame] - moved).abs() < 1e-12);
        }
        // It lands exactly on the new gains.
        assert_eq!(buffer.channel(0)[479], 0.0);
        assert_eq!(buffer.channel(1)[479], 1.0);

        // A jump from one end to the other is the largest change there is,
        // and no sample moves by more than one step of the ramp.
        let largest = buffer
            .channel(1)
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f64::max);
        assert!(largest < 1.0 / 480.0 + 1e-12);

        // `gains` reports where it is heading, and `next_gains` where it is.
        panner.set_pan(0.0);
        assert_eq!(panner.gains(), (0.5, 0.5));
        let (left, right) = panner.next_gains();
        assert!((left - 0.5 / 480.0).abs() < 1e-12 && (right - (1.0 - 0.5 / 480.0)).abs() < 1e-12);

        // The limit stated in the documentation: it is the gains that
        // glide, not the position. Half-way through a jump from one side to
        // the other each gain is a half, whatever the law, and not what the
        // law gives for the centre.
        for law in MixLaw::ALL {
            let mut panner = Panner::<f64>::new()
                .with_mode(PanMode::MonoToStereo)
                .with_law(law)
                .with_pan(-1.0)
                .with_ramp(0.01);
            panner.prepare(&SPEC);
            panner.set_pan(1.0);
            for _ in 0..239 {
                panner.next_gains();
            }
            let (left, right) = panner.next_gains();
            assert!(
                (left - 0.5).abs() < 1e-12 && (right - 0.5).abs() < 1e-12,
                "{law:?}"
            );
        }
    }

    /// Runs `signal` through the panner in blocks of `block` frames.
    fn run(panner: &mut Panner<f64>, signal: &mut AudioBuffer<f64>, block: usize) {
        let frames = signal.num_frames();
        let mut start = 0;
        while start < frames {
            let end = (start + block).min(frames);
            signal.windowed(start..end, |piece| panner.process(piece));
            start = end;
        }
    }

    /// Found in review: a change of mode used to glide the two gains while
    /// switching the input of the right output on the spot. With 220 Hz
    /// sines of amplitude 0.5 a quarter of a cycle apart on the two
    /// channels, the right output stepped by 0.50 between two neighbouring
    /// samples on the way from balance to mono-to-stereo, and by 0.36 on the
    /// way back.
    #[test]
    fn a_change_of_pan_mode_does_not_click() {
        for (first, second) in [
            (PanMode::StereoBalance, PanMode::MonoToStereo),
            (PanMode::MonoToStereo, PanMode::StereoBalance),
        ] {
            let mut panner = Panner::<f64>::new().with_mode(first);
            panner.prepare(&ProcessSpec {
                sample_rate: RATE,
                max_block_size: 512,
                num_channels: 2,
            });

            let mut signal = AudioBuffer::<f64>::new(2, 4_800);
            for (n, sample) in signal.channel_mut(0).iter_mut().enumerate() {
                *sample = 0.5 * (TAU * 220.0 * n as f64 / RATE).sin();
            }
            for (n, sample) in signal.channel_mut(1).iter_mut().enumerate() {
                *sample = 0.5 * (TAU * 220.0 * n as f64 / RATE).cos();
            }
            // The change falls on frame 2,400, where the left input crosses
            // zero and the right input is at its peak of 0.5.
            signal.windowed(0..2_400, |piece| run(&mut panner, piece, 512));
            panner.set_mode(second);
            signal.windowed(2_400..4_800, |piece| run(&mut panner, piece, 512));

            for channel in 0..2 {
                let largest_step = signal
                    .channel(channel)
                    .windows(2)
                    .map(|pair| (pair[1] - pair[0]).abs())
                    .fold(0.0, f64::max);
                // Each input moves by at most 0.0144 per sample, and no gain
                // here is above the square root of two.
                assert!(
                    largest_step < 0.05,
                    "{first:?} to {second:?}: channel {channel} steps by {largest_step}"
                );
            }
            // The change has been made, not just smoothed away: once the
            // glide is over the right output follows the other input.
            let (left_in, right_in) = (
                0.5 * (TAU * 220.0 * 4_799.0 / RATE).sin(),
                0.5 * (TAU * 220.0 * 4_799.0 / RATE).cos(),
            );
            let (_, right_gain) = panner.gains();
            let expected = match second {
                PanMode::MonoToStereo => left_in * right_gain,
                PanMode::StereoBalance => right_in * right_gain,
            };
            assert!((signal.channel(1)[4_799] - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn a_change_of_mode_cross_fades_the_right_output_over_the_stated_time() {
        // With the balance law in the centre both gains are one in either
        // mode, so the only thing a change of mode moves is where the right
        // output comes from. 10 ms at 48 kHz is 480 frames.
        let mut panner = Panner::<f64>::new()
            .with_law(MixLaw::Balanced)
            .with_ramp(0.01);
        panner.prepare(&SPEC);
        assert_eq!(panner.gains(), (1.0, 1.0));
        let steady = || {
            let mut buffer = AudioBuffer::<f64>::new(2, 1_000);
            buffer.channel_mut(0).fill(1.0);
            buffer.channel_mut(1).fill(0.25);
            buffer
        };

        // From the right input to the left one, in a straight line.
        panner.set_mode(PanMode::MonoToStereo);
        assert_eq!(panner.gains(), (1.0, 1.0));
        let mut buffer = steady();
        run(&mut panner, &mut buffer, 64);
        for frame in 0..1_000 {
            let moved = ((frame + 1) as f64 / 480.0).min(1.0);
            let expected = 0.25 + 0.75 * moved;
            assert!((buffer.channel(1)[frame] - expected).abs() < 1e-12);
            assert_eq!(buffer.channel(0)[frame], 1.0);
        }
        // It lands exactly on the left input, and stays there.
        assert_eq!(buffer.channel(1)[479..], [1.0; 521]);

        // And back.
        panner.set_mode(PanMode::StereoBalance);
        let mut buffer = steady();
        run(&mut panner, &mut buffer, 64);
        for frame in 0..1_000 {
            let moved = ((frame + 1) as f64 / 480.0).min(1.0);
            let expected = 1.0 - 0.75 * moved;
            assert!((buffer.channel(1)[frame] - expected).abs() < 1e-12);
        }
        assert_eq!(buffer.channel(1)[479..], [0.25; 521]);

        // Setting the mode it already has, as a host does on every block,
        // does not start the cross-fade again, and `reset` finishes one.
        panner.set_mode(PanMode::MonoToStereo);
        let mut buffer = steady();
        buffer.set_num_frames(240);
        panner.process(&mut buffer);
        panner.set_mode(PanMode::MonoToStereo);
        buffer.channel_mut(1).fill(0.25);
        panner.process(&mut buffer);
        assert_eq!(buffer.channel(1)[239], 1.0);
        panner.set_mode(PanMode::StereoBalance);
        panner.reset();
        buffer.channel_mut(1).fill(0.25);
        panner.process(&mut buffer);
        assert_eq!(buffer.channel(1), &[0.25; 240]);
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let render = |block: usize| {
            let mut panner = Panner::<f64>::new().with_pan(-0.4);
            panner.prepare(&SPEC);
            let mut signal = stereo(3_000);
            let mut start = 0;
            // The same changes on the same frames, whatever the block size.
            for (end, pan, law, mode) in [
                (700, 0.9, MixLaw::EqualPower, PanMode::StereoBalance),
                (900, -1.0, MixLaw::Sine4Point5dB, PanMode::MonoToStereo),
                (3_000, 0.2, MixLaw::Linear, PanMode::StereoBalance),
            ] {
                while start < end {
                    let stop = (start + block).min(end);
                    signal.windowed(start..stop, |piece| panner.process(piece));
                    start = stop;
                }
                panner.set_pan(pan);
                panner.set_law(law);
                panner.set_mode(mode);
            }
            signal
        };
        let reference = render(3_000);
        assert!(reference.peak() > 0.5);
        for block in [1, 7, 64, 997] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    #[test]
    fn other_channel_counts_are_left_alone() {
        // One channel: nothing to pan, but a glide still moves on.
        let mut panner = Panner::<f64>::new()
            .with_mode(PanMode::MonoToStereo)
            .with_law(MixLaw::Linear)
            .with_pan(-1.0)
            .with_ramp(0.001);
        panner.prepare(&SPEC);
        panner.set_pan(1.0);
        let mut mono = AudioBuffer::<f64>::new(1, 48);
        mono.channel_mut(0).fill(0.7);
        panner.process(&mut mono);
        assert_eq!(mono.channel(0), &[0.7; 48]);
        // The 48 frames of the glide are over.
        assert_eq!(panner.next_gains(), (0.0, 1.0));

        // More than two: the rest are untouched.
        let mut wide = AudioBuffer::<f64>::new(4, 16);
        for channel in wide.iter_channels_mut() {
            channel.fill(0.7);
        }
        panner.process(&mut wide);
        assert_eq!(wide.channel(0), &[0.0; 16]);
        assert_eq!(wide.channel(1), &[0.7; 16]);
        assert_eq!(wide.channel(2), &[0.7; 16]);
        assert_eq!(wide.channel(3), &[0.7; 16]);

        // No channels, and no frames.
        panner.process(&mut AudioBuffer::<f64>::new(0, 16));
        panner.process(&mut AudioBuffer::<f64>::new(2, 0));
    }

    #[test]
    fn positions_out_of_range_are_limited() {
        let mut panner = Panner::<f32>::new().with_mode(PanMode::MonoToStereo);
        panner.set_pan(5.0);
        assert_eq!(panner.pan(), 1.0);
        assert_eq!(panner.gains(), (0.0, 1.0));
        panner.set_pan(f64::NEG_INFINITY);
        assert_eq!(panner.gains(), (1.0, 0.0));
        panner.set_pan(f64::NAN);
        assert_eq!(panner.pan(), 0.0);
    }
}
