use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// Number of delay lines in the feedback network.
const LINES: usize = 8;

/// Delay line lengths in samples at 48 kHz and size 1: primes spread between
/// about 21 and 50 ms, so their echoes never line up into a repeating
/// pattern.
const LINE_LENGTHS: [usize; LINES] = [1009, 1193, 1381, 1543, 1777, 1951, 2203, 2411];

/// Input diffuser lengths in samples at 48 kHz, two per side.
const DIFFUSER_LENGTHS: [[usize; 2]; 2] = [[241, 83], [263, 97]];
const DIFFUSER_GAIN: f64 = 0.6;

/// Which delay lines each side listens to, and with what sign. The two
/// patterns are orthogonal, which makes left and right decorrelated.
const LEFT_TAPS: [f64; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];
const RIGHT_TAPS: [f64; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];

const SIZE_RANGE: (f64, f64) = (0.4, 1.6);

/// A circular buffer read and written at the same position, giving a delay
/// equal to its active length.
#[derive(Debug, Clone, Default)]
struct Line {
    buffer: Vec<f64>,
    position: usize,
    len: usize,
}

impl Line {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity.max(1)],
            position: 0,
            len: capacity.max(1),
        }
    }

    fn set_len(&mut self, len: usize) {
        if self.buffer.is_empty() {
            // Not prepared yet; `prepare` sets the length.
            return;
        }
        self.len = len.clamp(1, self.buffer.len());
        self.position %= self.len;
    }

    #[inline]
    fn read(&self) -> f64 {
        self.buffer[self.position]
    }

    #[inline]
    fn write_and_advance(&mut self, value: f64) {
        self.buffer[self.position] = value;
        self.position += 1;
        if self.position == self.len {
            self.position = 0;
        }
    }

    /// One step of a Schroeder all-pass section built on this line.
    #[inline]
    fn allpass(&mut self, input: f64, gain: f64) -> f64 {
        let delayed = self.read();
        let output = delayed - gain * input;
        self.write_and_advance(input + gain * output);
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

/// A stereo reverb built on a feedback delay network.
///
/// Eight delay lines feed each other through a Householder matrix, which
/// mixes every line into every other without adding or losing energy. Each
/// line then loses exactly as much per trip as the chosen decay time asks
/// for, and a one-pole low-pass in each loop makes high frequencies die away
/// sooner, as they do in a real room. Two all-pass diffusers per side smear
/// the input so the first echoes are not heard one by one.
///
/// The design follows J.-M. Jot and A. Chaigne, "Digital delay networks for
/// designing artificial reverberators", 90th AES Convention, 1991.
///
/// The first two channels of a buffer are treated as left and right; a mono
/// buffer gets the left output. Any further channels pass through untouched.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{ProcessSpec, Processor, Reverb};
///
/// let mut reverb = Reverb::<f32>::new()
///     .with_decay_seconds(2.5)
///     .with_damping(0.4)
///     .with_mix(0.3);
/// reverb.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 512, num_channels: 2 });
///
/// let mut block = AudioBuffer::<f32>::new(2, 512);
/// block.channel_mut(0)[0] = 1.0; // a click on the left
/// reverb.process(&mut block);
/// ```
#[derive(Debug, Clone)]
pub struct Reverb<S: Sample = f32> {
    decay_seconds: f64,
    damping: f64,
    size: f64,
    mix: f64,
    width: f64,
    sample_rate: f64,
    lines: [Line; LINES],
    /// Gain applied on each trip round a line, from the decay time.
    gains: [f64; LINES],
    /// Low-pass state of each line.
    lowpass: [f64; LINES],
    diffusers: [[Line; 2]; 2],
    marker: PhantomData<S>,
}

impl<S: Sample> Default for Reverb<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> Reverb<S> {
    /// Creates a reverb with a two-second decay, moderate damping, medium
    /// size, full width and a 30% mix.
    pub fn new() -> Self {
        Self {
            decay_seconds: 2.0,
            damping: 0.3,
            size: 0.5,
            mix: 0.3,
            width: 1.0,
            sample_rate: 44_100.0,
            lines: Default::default(),
            gains: [0.0; LINES],
            lowpass: [0.0; LINES],
            diffusers: Default::default(),
            marker: PhantomData,
        }
    }

    /// Sets how long the tail takes to fade by 60 dB, in seconds. Real-time
    /// safe.
    pub fn set_decay_seconds(&mut self, seconds: f64) {
        self.decay_seconds = seconds.clamp(0.05, 120.0);
        self.update_gains();
    }

    /// Sets how much faster high frequencies fade than low ones, from `0`
    /// (bright, like a tiled room) to `1` (dark, like a furnished one).
    /// Real-time safe.
    pub fn set_damping(&mut self, damping: f64) {
        self.damping = damping.clamp(0.0, 1.0);
    }

    /// Sets the size of the simulated space, from `0` (small) to `1` (large).
    /// Real-time safe, but changing it while sound is ringing is audible.
    pub fn set_size(&mut self, size: f64) {
        self.size = size.clamp(0.0, 1.0);
        self.update_lengths();
    }

    /// Sets the balance between the untouched signal (`0`) and the reverb
    /// (`1`). Real-time safe.
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Sets the stereo width of the reverb, from `0` (mono) to `1` (fully
    /// wide). Real-time safe.
    pub fn set_width(&mut self, width: f64) {
        self.width = width.clamp(0.0, 1.0);
    }

    /// Builder form of [`set_decay_seconds`](Self::set_decay_seconds).
    #[must_use]
    pub fn with_decay_seconds(mut self, seconds: f64) -> Self {
        self.set_decay_seconds(seconds);
        self
    }

    /// Builder form of [`set_damping`](Self::set_damping).
    #[must_use]
    pub fn with_damping(mut self, damping: f64) -> Self {
        self.set_damping(damping);
        self
    }

    /// Builder form of [`set_size`](Self::set_size).
    #[must_use]
    pub fn with_size(mut self, size: f64) -> Self {
        self.set_size(size);
        self
    }

    /// Builder form of [`set_mix`](Self::set_mix).
    #[must_use]
    pub fn with_mix(mut self, mix: f64) -> Self {
        self.set_mix(mix);
        self
    }

    /// Builder form of [`set_width`](Self::set_width).
    #[must_use]
    pub fn with_width(mut self, width: f64) -> Self {
        self.set_width(width);
        self
    }

    fn rate_scale(&self) -> f64 {
        self.sample_rate / 48_000.0
    }

    fn update_lengths(&mut self) {
        let scale = self.rate_scale() * (SIZE_RANGE.0 + (SIZE_RANGE.1 - SIZE_RANGE.0) * self.size);
        for (line, &base) in self.lines.iter_mut().zip(&LINE_LENGTHS) {
            line.set_len((base as f64 * scale).round() as usize);
        }
        self.update_gains();
    }

    fn update_gains(&mut self) {
        // A line of L samples is passed fs/L times a second. For the level to
        // fall 60 dB in the decay time, each pass must multiply by
        // 10^(-3·L / (fs·T)).
        for (gain, line) in self.gains.iter_mut().zip(&self.lines) {
            let trips = line.len as f64 / (self.sample_rate * self.decay_seconds);
            *gain = 10f64.powf(-3.0 * trips);
        }
    }
}

impl<S: Sample> Processor<S> for Reverb<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        let scale = self.rate_scale();
        for (line, &base) in self.lines.iter_mut().zip(&LINE_LENGTHS) {
            *line = Line::with_capacity((base as f64 * scale * SIZE_RANGE.1).ceil() as usize + 1);
        }
        for (side, lengths) in self.diffusers.iter_mut().zip(&DIFFUSER_LENGTHS) {
            for (diffuser, &base) in side.iter_mut().zip(lengths) {
                *diffuser = Line::with_capacity((base as f64 * scale).round() as usize);
            }
        }
        self.lowpass = [0.0; LINES];
        self.update_lengths();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let channels = buffer.num_channels();
        if channels == 0 || self.lines[0].buffer.is_empty() {
            return;
        }
        let stereo = channels >= 2;
        // The pole of the damping filter. Kept below one so that even full
        // damping lets the lowest frequencies ring for the whole decay time.
        let pole = 0.75 * self.damping;
        let wet_gain = self.mix * 0.35;
        let dry_gain = 1.0 - self.mix;
        // Width blends each side's wet signal with the other's.
        let same = 0.5 * (1.0 + self.width);
        let other = 0.5 * (1.0 - self.width);

        for frame in 0..buffer.num_frames() {
            let dry_left = buffer.channel(0)[frame].to_f64();
            let dry_right = if stereo {
                buffer.channel(1)[frame].to_f64()
            } else {
                dry_left
            };

            let mut inputs = [dry_left, dry_right];
            for (input, side) in inputs.iter_mut().zip(&mut self.diffusers) {
                for diffuser in side {
                    *input = diffuser.allpass(*input, DIFFUSER_GAIN);
                }
            }

            let mut returned = [0.0f64; LINES];
            let mut total = 0.0;
            let mut left = 0.0;
            let mut right = 0.0;
            for index in 0..LINES {
                let delayed = self.lines[index].read();
                left += LEFT_TAPS[index] * delayed;
                right += RIGHT_TAPS[index] * delayed;

                let filtered = delayed + pole * (self.lowpass[index] - delayed);
                self.lowpass[index] = filtered;
                returned[index] = filtered * self.gains[index];
                total += returned[index];
            }

            // Householder reflection: every line receives itself minus a
            // quarter of the sum of all eight.
            let shared = total * (2.0 / LINES as f64);
            for index in 0..LINES {
                let feedback = returned[index] - shared;
                self.lines[index].write_and_advance(inputs[index % 2] + feedback);
            }

            let wet_left = wet_gain * (same * left + other * right);
            let wet_right = wet_gain * (same * right + other * left);
            buffer.channel_mut(0)[frame] = S::from_f64(dry_gain * dry_left + wet_left);
            if stereo {
                buffer.channel_mut(1)[frame] = S::from_f64(dry_gain * dry_right + wet_right);
            }
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        for diffuser in self.diffusers.iter_mut().flatten() {
            diffuser.clear();
        }
        self.lowpass = [0.0; LINES];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;
    const SPEC: ProcessSpec = ProcessSpec {
        sample_rate: SAMPLE_RATE,
        max_block_size: 4_800,
        num_channels: 2,
    };

    /// The reverb's response to a click on both channels, wet only.
    fn impulse_response(mut reverb: Reverb<f64>, seconds: f64) -> AudioBuffer<f64> {
        reverb.set_mix(1.0);
        reverb.prepare(&SPEC);
        let frames = (seconds * SAMPLE_RATE) as usize;
        let mut response = AudioBuffer::<f64>::new(2, frames);
        response.channel_mut(0)[0] = 1.0;
        response.channel_mut(1)[0] = 1.0;

        let mut block = AudioBuffer::<f64>::new(2, 4_800);
        for start in (0..frames).step_by(4_800) {
            let count = 4_800.min(frames - start);
            block.set_num_frames(count);
            for channel in 0..2 {
                block
                    .channel_mut(channel)
                    .copy_from_slice(&response.channel(channel)[start..start + count]);
            }
            reverb.process(&mut block);
            for channel in 0..2 {
                response.channel_mut(channel)[start..start + count]
                    .copy_from_slice(block.channel(channel));
            }
        }
        response
    }

    /// Level in decibels of each 50 ms slice of a signal.
    fn levels_db(signal: &[f64]) -> Vec<f64> {
        signal
            .chunks_exact(2_400)
            .map(|slice| {
                let power = slice.iter().map(|s| s * s).sum::<f64>() / slice.len() as f64;
                10.0 * power.max(1e-30).log10()
            })
            .collect()
    }

    /// Decay rate in decibels per second, by a least-squares line through the
    /// levels between `from` and `to` seconds.
    fn decay_rate(levels: &[f64], from: f64, to: f64) -> f64 {
        let points: Vec<(f64, f64)> = levels
            .iter()
            .enumerate()
            .map(|(index, &level)| ((index as f64 + 0.5) * 0.05, level))
            .filter(|&(time, _)| time >= from && time <= to)
            .collect();
        let count = points.len() as f64;
        let mean_time = points.iter().map(|p| p.0).sum::<f64>() / count;
        let mean_level = points.iter().map(|p| p.1).sum::<f64>() / count;
        let covariance: f64 = points
            .iter()
            .map(|p| (p.0 - mean_time) * (p.1 - mean_level))
            .sum();
        let variance: f64 = points.iter().map(|p| (p.0 - mean_time).powi(2)).sum();
        covariance / variance
    }

    #[test]
    fn the_tail_fades_sixty_decibels_in_the_decay_time() {
        for decay in [1.0, 3.0] {
            let reverb = Reverb::<f64>::new()
                .with_decay_seconds(decay)
                .with_damping(0.0);
            let response = impulse_response(reverb, decay);
            let rate = decay_rate(&levels_db(response.channel(0)), 0.1, 0.9 * decay);
            let expected = -60.0 / decay;
            assert!(
                (rate - expected).abs() < 0.1 * expected.abs(),
                "decay {decay} s: {rate:.1} dB/s, expected {expected:.1} dB/s"
            );
        }
    }

    #[test]
    fn damping_shortens_the_high_frequencies() {
        let brightness = |damping: f64| {
            let reverb = Reverb::<f64>::new()
                .with_decay_seconds(2.0)
                .with_damping(damping);
            let response = impulse_response(reverb, 1.0);
            // Energy of the sample-to-sample difference is a crude measure of
            // high-frequency content.
            let tail = &response.channel(0)[24_000..];
            let high: f64 = tail.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
            let all: f64 = tail.iter().map(|s| s * s).sum();
            high / all
        };
        assert!(brightness(0.9) < 0.25 * brightness(0.0));
    }

    #[test]
    fn dry_mix_is_transparent_and_extra_channels_pass_through() {
        let mut reverb = Reverb::<f32>::new().with_mix(0.0);
        reverb.prepare(&ProcessSpec {
            num_channels: 3,
            ..SPEC
        });
        let mut buffer = AudioBuffer::<f32>::new(3, 256);
        for (index, channel) in buffer.iter_channels_mut().enumerate() {
            for (frame, sample) in channel.iter_mut().enumerate() {
                *sample = ((frame + index * 7) % 13) as f32 / 13.0 - 0.5;
            }
        }
        let original = buffer.clone();
        reverb.process(&mut buffer);
        assert_eq!(buffer, original);

        reverb.set_mix(1.0);
        reverb.process(&mut buffer);
        assert_ne!(buffer.channel(0), original.channel(0));
        assert_eq!(buffer.channel(2), original.channel(2));
    }

    #[test]
    fn left_and_right_differ_and_width_zero_makes_them_equal() {
        let wide = impulse_response(Reverb::<f64>::new(), 0.5);
        let difference: f64 = wide
            .channel(0)
            .iter()
            .zip(wide.channel(1))
            .map(|(l, r)| (l - r).abs())
            .sum();
        assert!(difference > 1.0);

        let narrow = impulse_response(Reverb::<f64>::new().with_width(0.0), 0.5);
        for (left, right) in narrow.channel(0).iter().zip(narrow.channel(1)) {
            assert!((left - right).abs() < 1e-12);
        }
    }

    #[test]
    fn a_long_decay_stays_bounded_under_sustained_input() {
        let mut reverb = Reverb::<f64>::new()
            .with_decay_seconds(120.0)
            .with_damping(0.0)
            .with_size(1.0)
            .with_mix(1.0);
        reverb.prepare(&SPEC);
        let mut block = AudioBuffer::<f64>::new(2, 4_800);
        let mut peak = 0.0f64;
        for round in 0..100 {
            for channel in block.iter_channels_mut() {
                for (frame, sample) in channel.iter_mut().enumerate() {
                    *sample = if (frame + round) % 97 < 48 { 0.5 } else { -0.5 };
                }
            }
            reverb.process(&mut block);
            peak = peak.max(block.peak());
        }
        assert!(peak.is_finite() && peak < 200.0, "peak {peak}");
    }

    #[test]
    fn reset_silences_the_tail_and_mono_works() {
        let mut reverb = Reverb::<f32>::new().with_mix(1.0);
        reverb.prepare(&ProcessSpec {
            num_channels: 1,
            ..SPEC
        });
        let mut buffer = AudioBuffer::<f32>::new(1, 4_800);
        buffer.channel_mut(0)[0] = 1.0;
        reverb.process(&mut buffer);
        assert!(buffer.peak() > 0.0);

        reverb.reset();
        buffer.clear();
        reverb.process(&mut buffer);
        assert_eq!(buffer.peak(), 0.0);
    }

    #[test]
    fn processing_before_prepare_does_nothing() {
        let mut reverb = Reverb::<f32>::new();
        let mut buffer = AudioBuffer::<f32>::new(2, 16);
        buffer.channel_mut(0).fill(0.5);
        reverb.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[0.5; 16]);
    }
}
