use manguebyte_core::{AudioBuffer, Sample};

use crate::fir::{kaiser_beta, sinc};
use crate::window::bessel_i0;

/// Zero crossings of the interpolation kernel on each side of its centre.
/// More means a narrower transition band and more work per sample. With 32
/// the response is flat to about 0.45 of the lower of the two sample rates.
const ZERO_CROSSINGS: usize = 32;

/// How far down everything beyond the transition band is, in decibels.
const ATTENUATION_DB: f64 = 100.0;

/// Table entries per zero crossing. Values in between are interpolated
/// linearly, which at this density is accurate to better than -100 dB.
const TABLE_RESOLUTION: usize = 512;

/// Converts audio from one sample rate to another, in a stream.
///
/// Each output sample is a weighted sum of the input samples around it, with
/// weights taken from a windowed sinc: the ideal interpolator, shortened to a
/// practical length. When going down in rate the same kernel is stretched so
/// that it also removes what the lower rate cannot hold, which would
/// otherwise come back as aliasing.
///
/// Any ratio works, including ones that are not simple fractions, and the
/// position is tracked so that the output does not depend on how the input
/// is cut into blocks.
///
/// [`process`](Self::process) allocates nothing. The method is the
/// band-limited interpolation of J. O. Smith and P. Gossett, "A flexible
/// sampling-rate conversion method", ICASSP 1984.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::Resampler;
///
/// // From CD rate to 48 kHz, in blocks of up to 441 frames.
/// let mut resampler = Resampler::<f32>::new(44_100.0, 48_000.0, 2, 441);
/// let input = AudioBuffer::<f32>::new(2, 441);
/// let mut output = AudioBuffer::<f32>::new(2, resampler.max_output_frames(441));
///
/// let produced = resampler.process(&input, &mut output);
/// assert_eq!(produced, output.num_frames());
/// assert!((479..=481).contains(&produced)); // 10 ms in, 10 ms out
/// ```
#[derive(Debug, Clone)]
pub struct Resampler<S: Sample = f32> {
    /// Output rate divided by input rate.
    ratio: f64,
    /// How much the kernel is stretched: 1 going up, the ratio going down.
    scale: f64,
    /// Input samples the kernel reaches on each side of an output sample.
    half_width: usize,
    /// One side of the kernel, sampled finely.
    table: Vec<f64>,
    /// Per channel: the last `2 * half_width` input samples, then the block
    /// being converted.
    work: Vec<Vec<S>>,
    max_input: usize,
    /// Where the next output sample falls, in `work`: a whole index and the
    /// fraction past it. Kept apart so that moving to the next block, which
    /// changes only the index, is exact.
    index: usize,
    fraction: f64,
    step_index: usize,
    step_fraction: f64,
}

impl<S: Sample> Resampler<S> {
    /// Creates a converter from `input_rate` to `output_rate`, both in
    /// hertz, for blocks of up to `max_input_frames`. Allocates.
    ///
    /// # Panics
    ///
    /// Panics unless both rates are positive and finite and
    /// `max_input_frames` is non-zero.
    pub fn new(
        input_rate: f64,
        output_rate: f64,
        channels: usize,
        max_input_frames: usize,
    ) -> Self {
        assert!(
            input_rate.is_finite()
                && output_rate.is_finite()
                && input_rate > 0.0
                && output_rate > 0.0,
            "sample rates must be positive"
        );
        assert!(max_input_frames > 0, "max_input_frames must be non-zero");
        let ratio = output_rate / input_rate;
        let scale = ratio.min(1.0);
        let half_width = (ZERO_CROSSINGS as f64 / scale).ceil() as usize;

        let beta = kaiser_beta(ATTENUATION_DB);
        let span = (ZERO_CROSSINGS * TABLE_RESOLUTION) as f64;
        // Two entries past the end, both zero, so that interpolating at the
        // very edge needs no special case.
        let table = (0..=ZERO_CROSSINGS * TABLE_RESOLUTION + 1)
            .map(|entry| {
                let position = entry as f64 / span;
                if position >= 1.0 {
                    return 0.0;
                }
                let window = bessel_i0(beta * (1.0 - position * position).sqrt()) / bessel_i0(beta);
                sinc(entry as f64 / TABLE_RESOLUTION as f64) * window
            })
            .collect();

        let step = 1.0 / ratio;
        let mut resampler = Self {
            ratio,
            scale,
            half_width,
            table,
            work: vec![vec![S::ZERO; 2 * half_width + max_input_frames]; channels],
            max_input: max_input_frames,
            index: 0,
            fraction: 0.0,
            step_index: step.floor() as usize,
            step_fraction: step.fract(),
        };
        resampler.reset();
        resampler
    }

    /// Output rate divided by input rate.
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// How late the output is, in output frames. The first output frames
    /// correspond to the time before the input started.
    pub fn latency_frames(&self) -> f64 {
        self.half_width as f64 * self.ratio
    }

    /// The most output frames that `input_frames` of input can produce. Size
    /// the output buffer with it.
    pub fn max_output_frames(&self, input_frames: usize) -> usize {
        (input_frames as f64 * self.ratio).ceil() as usize + 1
    }

    /// Forgets the audio seen so far.
    pub fn reset(&mut self) {
        for channel in &mut self.work {
            channel.fill(S::ZERO);
        }
        // The first output is centred half a kernel before the first input
        // sample, so everything it reaches for is either silence or there.
        self.index = self.half_width;
        self.fraction = 0.0;
    }

    /// Starts the output at the first input sample instead of half a kernel
    /// before it. The converter then produces nothing until it has seen
    /// enough input, and its output is lined up with the input exactly,
    /// whatever the ratio. For converting a whole recording.
    fn align_to_input(&mut self) {
        self.index = 2 * self.half_width;
        self.fraction = 0.0;
    }

    /// Converts all of `input`, writes the result to the start of `output`
    /// and sets its length. Returns the number of frames produced, which
    /// varies by one from block to block when the ratio is not whole.
    ///
    /// Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics if the buffers have fewer channels than the converter, or if
    /// `output` cannot hold
    /// [`max_output_frames`](Self::max_output_frames) for this input.
    pub fn process(&mut self, input: &AudioBuffer<S>, output: &mut AudioBuffer<S>) -> usize {
        let frames = input.num_frames();
        assert!(
            output.capacity() >= self.max_output_frames(frames),
            "the output buffer is too small for {frames} input frames"
        );
        output.set_num_frames(output.capacity());
        let mut produced = 0;
        let mut start = 0;
        while start < frames {
            let end = (start + self.max_input).min(frames);
            produced += self.convert(input, start..end, output, produced);
            start = end;
        }
        output.set_num_frames(produced);
        produced
    }

    /// Converts one piece of the input that fits the work buffers.
    fn convert(
        &mut self,
        input: &AudioBuffer<S>,
        range: core::ops::Range<usize>,
        output: &mut AudioBuffer<S>,
        offset: usize,
    ) -> usize {
        let history = 2 * self.half_width;
        let count = range.len();
        let available = history + count;
        let mut produced = 0;
        let (mut index, mut fraction) = (self.index, self.fraction);

        for (channel, work) in self.work.iter_mut().enumerate() {
            work[history..available].copy_from_slice(&input.channel(channel)[range.clone()]);
            let out = &mut output.channel_mut(channel)[offset..];
            (index, fraction) = (self.index, self.fraction);
            produced = 0;

            // An output can be made once every input sample its kernel
            // reaches has arrived.
            while index + self.half_width < available {
                let first = index + 1 - self.half_width;
                let mut sum = 0.0;
                let mut weight_sum = 0.0;
                for (tap, &sample) in work[first..=index + self.half_width].iter().enumerate() {
                    let distance = ((first + tap) as f64 - index as f64 - fraction).abs();
                    // Position in the table, and the two entries around it.
                    let position = distance * self.scale * TABLE_RESOLUTION as f64;
                    let entry = position as usize;
                    if entry + 1 >= self.table.len() {
                        continue;
                    }
                    let between = position - entry as f64;
                    let weight =
                        self.table[entry] + between * (self.table[entry + 1] - self.table[entry]);
                    sum += weight * sample.to_f64();
                    weight_sum += weight;
                }
                // Dividing by the sum of the weights makes a steady level
                // come out exactly as it went in.
                out[produced] = S::from_f64(sum / weight_sum);
                produced += 1;

                index += self.step_index;
                fraction += self.step_fraction;
                if fraction >= 1.0 {
                    fraction -= 1.0;
                    index += 1;
                }
            }

            // Keep the tail as history for the next piece.
            work.copy_within(count..available, 0);
        }

        if !self.work.is_empty() {
            self.index = index - count;
            self.fraction = fraction;
        }
        produced
    }
}

/// Converts a whole buffer from one sample rate to another.
///
/// The result has `frames * to_rate / from_rate` frames, rounded, and is
/// lined up with the input exactly: there is no delay to account for. This
/// allocates and is meant for loading files, not for the audio thread;
/// there, use a [`Resampler`].
pub fn resample<S: Sample>(
    buffer: &AudioBuffer<S>,
    from_rate: f64,
    to_rate: f64,
) -> AudioBuffer<S> {
    let channels = buffer.num_channels();
    let frames = buffer.num_frames();
    let block = 4_096;
    let mut resampler = Resampler::<S>::new(from_rate, to_rate, channels, block);
    resampler.align_to_input();
    let wanted = (frames as f64 * resampler.ratio()).round() as usize;

    let mut input = AudioBuffer::<S>::new(channels, block);
    let mut piece = AudioBuffer::<S>::new(channels, resampler.max_output_frames(block));
    let mut collected: Vec<Vec<S>> = vec![Vec::with_capacity(wanted + block); channels];

    // Feed the input, then silence until the delayed tail has come out.
    let mut position = 0;
    while collected.first().is_some_and(|c| c.len() < wanted) {
        input.clear();
        let count = block.min(frames.saturating_sub(position));
        for channel in 0..channels {
            input.channel_mut(channel)[..count]
                .copy_from_slice(&buffer.channel(channel)[position..position + count]);
        }
        position += count;
        resampler.process(&input, &mut piece);
        for (channel, samples) in collected.iter_mut().enumerate() {
            samples.extend_from_slice(piece.channel(channel));
        }
    }

    let mut result = AudioBuffer::<S>::new(channels, wanted);
    for (channel, samples) in collected.iter().enumerate() {
        result
            .channel_mut(channel)
            .copy_from_slice(&samples[..wanted]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::TAU;

    fn sine(frequency: f64, rate: f64, frames: usize) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(1, frames);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = (TAU * frequency * n as f64 / rate).sin();
        }
        buffer
    }

    /// Largest difference from an ideal sine, in decibels, away from the
    /// edges where the input's sudden start and end are being smoothed.
    fn error_db(output: &AudioBuffer<f64>, frequency: f64, rate: f64) -> f64 {
        let samples = output.channel(0);
        let margin = samples.len() / 8;
        let worst = samples[margin..samples.len() - margin]
            .iter()
            .enumerate()
            .map(|(n, &sample)| {
                let ideal = (TAU * frequency * (n + margin) as f64 / rate).sin();
                (sample - ideal).abs()
            })
            .fold(0.0, f64::max);
        20.0 * worst.max(1e-300).log10()
    }

    #[test]
    fn a_sine_survives_conversion_between_common_rates() {
        for (from, to) in [
            (44_100.0, 48_000.0),
            (48_000.0, 44_100.0),
            (48_000.0, 96_000.0),
            (96_000.0, 44_100.0),
            (48_000.0, 8_000.0),
            (44_100.0, 44_100.0),
        ] {
            for frequency in [100.0, 1_000.0, 3_000.0] {
                let input = sine(frequency, from, (from / 2.0) as usize);
                let output = resample(&input, from, to);
                assert_eq!(output.num_frames(), (to / 2.0) as usize);
                let error = error_db(&output, frequency, to);
                assert!(
                    error < -90.0,
                    "{from} to {to} at {frequency} Hz: error {error:.1} dB"
                );
            }
        }
    }

    #[test]
    fn the_band_is_flat_almost_to_half_the_lower_rate() {
        // 0.45 of 44.1 kHz is 19.8 kHz.
        let input = sine(19_000.0, 44_100.0, 44_100);
        let output = resample(&input, 44_100.0, 48_000.0);
        assert!(error_db(&output, 19_000.0, 48_000.0) < -60.0);
    }

    #[test]
    fn what_the_lower_rate_cannot_hold_is_removed() {
        // 10 kHz does not fit below half of 16 kHz. Left in, it would fold
        // down to 6 kHz.
        let input = sine(10_000.0, 48_000.0, 48_000);
        let output = resample(&input, 48_000.0, 16_000.0);
        let middle = &output.channel(0)[2_000..14_000];
        let peak = middle.iter().fold(0.0f64, |peak, s| peak.max(s.abs()));
        assert!(
            20.0 * peak.log10() < -90.0,
            "left {:.1} dB",
            20.0 * peak.log10()
        );
    }

    #[test]
    fn a_steady_level_comes_out_exactly() {
        let mut input = AudioBuffer::<f64>::new(1, 4_000);
        input.channel_mut(0).fill(0.7);
        for to in [48_000.0, 22_050.0, 37_123.0] {
            let output = resample(&input, 44_100.0, to);
            let middle = &output.channel(0)[500..output.num_frames() - 500];
            for &sample in middle {
                assert!((sample - 0.7).abs() < 1e-12, "{to}: {sample}");
            }
        }
    }

    #[test]
    fn frame_count_follows_the_ratio_and_does_not_drift() {
        let mut resampler = Resampler::<f64>::new(44_100.0, 48_000.0, 1, 441);
        let input = AudioBuffer::<f64>::new(1, 441);
        let mut output = AudioBuffer::<f64>::new(1, resampler.max_output_frames(441));
        let mut total = 0;
        for block in 1..=1_000 {
            let produced = resampler.process(&input, &mut output);
            assert!((479..=481).contains(&produced));
            total += produced;
            let expected = block * 480;
            assert!(
                total.abs_diff(expected) <= 1,
                "after {block} blocks: {total}"
            );
        }
    }

    #[test]
    fn output_does_not_depend_on_how_the_input_is_cut_up() {
        let source = sine(440.0, 44_100.0, 5_000);
        let render = |pattern: &[usize]| {
            let mut resampler = Resampler::<f64>::new(44_100.0, 48_000.0, 1, 64);
            let mut input = AudioBuffer::<f64>::new(1, 5_000);
            let mut piece = AudioBuffer::<f64>::new(1, resampler.max_output_frames(5_000));
            let mut all = Vec::new();
            let mut position = 0;
            for &length in pattern.iter().cycle() {
                if position == 5_000 {
                    break;
                }
                let length = length.min(5_000 - position);
                input.set_num_frames(length);
                input
                    .channel_mut(0)
                    .copy_from_slice(&source.channel(0)[position..position + length]);
                resampler.process(&input, &mut piece);
                all.extend_from_slice(piece.channel(0));
                position += length;
            }
            all
        };
        let reference = render(&[64]);
        assert!(reference.len().abs_diff(5_442) <= 1);
        assert_eq!(render(&[1]), reference);
        assert_eq!(render(&[7, 300, 1, 64]), reference);
        // Longer than the converter was built for: it splits the block.
        assert_eq!(render(&[5_000]), reference);
    }

    #[test]
    fn an_impulse_arrives_after_the_stated_latency() {
        let mut resampler = Resampler::<f64>::new(48_000.0, 48_000.0, 1, 256);
        let mut input = AudioBuffer::<f64>::new(1, 256);
        input.channel_mut(0)[5] = 1.0;
        let mut output = AudioBuffer::<f64>::new(1, resampler.max_output_frames(256));
        resampler.process(&input, &mut output);
        let peak = (0..output.num_frames())
            .max_by(|&a, &b| {
                output.channel(0)[a]
                    .abs()
                    .total_cmp(&output.channel(0)[b].abs())
            })
            .unwrap();
        assert_eq!(resampler.latency_frames(), 32.0);
        assert_eq!(peak, 5 + 32);
        assert!((output.channel(0)[peak] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn stereo_channels_stay_separate() {
        let mut input = AudioBuffer::<f64>::new(2, 2_000);
        input.channel_mut(0).fill(1.0);
        input.channel_mut(1).fill(-0.5);
        let output = resample(&input, 48_000.0, 44_100.0);
        assert_eq!(output.num_channels(), 2);
        assert!((output.channel(0)[900] - 1.0).abs() < 1e-12);
        assert!((output.channel(1)[900] + 0.5).abs() < 1e-12);
    }
}
