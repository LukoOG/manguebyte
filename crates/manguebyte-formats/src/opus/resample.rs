//! The conversion to 48 kHz of audio the Opus encoder is given at another
//! rate.
//!
//! This is the converter of `manguebyte-dsp` (`Resampler`, with its
//! `align_to_input`), the band-limited interpolation of J. O. Smith and P.
//! Gossett, "A flexible sampling-rate conversion method", ICASSP 1984,
//! with the same kernel: a Kaiser-windowed sinc of 32 zero crossings a side,
//! 100 dB down beyond its transition band, stretched to filter out what
//! 48 kHz cannot hold when the rate goes down. It is kept here because this
//! crate depends on `manguebyte-core` alone (`docs/ARCHITECTURE.md`); the two
//! give the same samples.
//!
//! The output is lined up with the input: the first output sample is at the
//! time of the first input sample, so the conversion adds no delay, and a
//! stream of `frames` input frames gives `frames * 48000 / rate` output
//! frames, rounded, once [`Resampler::finish`] has fed the silence its
//! kernel reaches past the end.

use std::f64::consts::PI;

const ZERO_CROSSINGS: usize = 32;
const ATTENUATION_DB: f64 = 100.0;
const TABLE_RESOLUTION: usize = 512;

/// `sin(pi x) / (pi x)`.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// The Kaiser window parameter that gives a stopband this far down
/// (Kaiser's formula).
fn kaiser_beta(attenuation_db: f64) -> f64 {
    if attenuation_db > 50.0 {
        0.1102 * (attenuation_db - 8.7)
    } else if attenuation_db >= 21.0 {
        0.5842 * (attenuation_db - 21.0).powf(0.4) + 0.07886 * (attenuation_db - 21.0)
    } else {
        0.0
    }
}

/// The zeroth-order modified Bessel function of the first kind.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Converts the channels of a stream from one rate to another as their
/// samples arrive.
#[derive(Debug, Clone)]
pub(crate) struct Resampler {
    ratio: f64,
    scale: f64,
    half_width: usize,
    table: Vec<f64>,
    /// For each channel, the input from the first sample the kernel may
    /// still reach.
    work: Vec<Vec<f32>>,
    /// Where the next output sample falls in `work`: a whole index and the
    /// fraction past it.
    index: usize,
    fraction: f64,
    step_index: usize,
    step_fraction: f64,
    /// Input frames taken and output frames given.
    taken: u64,
    given: u64,
    /// The weights of the output sample being made.
    weights: Vec<f64>,
}

impl Resampler {
    pub(crate) fn new(input_rate: f64, output_rate: f64, channels: usize) -> Self {
        let ratio = output_rate / input_rate;
        let scale = ratio.min(1.0);
        let half_width = (ZERO_CROSSINGS as f64 / scale).ceil() as usize;
        let beta = kaiser_beta(ATTENUATION_DB);
        let span = (ZERO_CROSSINGS * TABLE_RESOLUTION) as f64;
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
        Self {
            ratio,
            scale,
            half_width,
            table,
            // Silence before the first sample, which the kernel reaches.
            work: vec![vec![0.0; 2 * half_width]; channels],
            index: 2 * half_width,
            fraction: 0.0,
            step_index: step.floor() as usize,
            step_fraction: step.fract(),
            taken: 0,
            given: 0,
            weights: Vec::with_capacity(2 * half_width),
        }
    }

    /// Takes `frames` frames, channel `c` of frame `i` at
    /// `input(c, i)`, and appends what they complete to `out`, a vector per
    /// channel.
    pub(crate) fn process(
        &mut self,
        frames: usize,
        input: impl Fn(usize, usize) -> f32,
        out: &mut [Vec<f32>],
    ) {
        for (channel, work) in self.work.iter_mut().enumerate() {
            work.extend((0..frames).map(|i| input(channel, i)));
        }
        self.taken += frames as u64;
        self.run(out, u64::MAX);
    }

    /// Feeds silence past the end until every output frame the input
    /// gives is out, and appends them to `out`.
    pub(crate) fn finish(&mut self, out: &mut [Vec<f32>]) {
        let wanted = (self.taken as f64 * self.ratio).round() as u64;
        while self.given < wanted {
            let more = self.half_width + 64;
            for work in &mut self.work {
                work.resize(work.len() + more, 0.0);
            }
            self.run(out, wanted);
        }
    }

    /// Makes every output sample whose kernel the input reaches, up to
    /// `limit` in all.
    fn run(&mut self, out: &mut [Vec<f32>], limit: u64) {
        let available = self.work.first().map_or(0, Vec::len);
        while self.index + self.half_width < available && self.given < limit {
            let first = self.index + 1 - self.half_width;
            // The weights, the same for every channel.
            self.weights.clear();
            let mut weight_sum = 0.0;
            for tap in 0..2 * self.half_width {
                let distance = ((first + tap) as f64 - self.index as f64 - self.fraction).abs();
                let position = distance * self.scale * TABLE_RESOLUTION as f64;
                let entry = position as usize;
                let weight = if entry + 1 >= self.table.len() {
                    0.0
                } else {
                    let between = position - entry as f64;
                    self.table[entry] + between * (self.table[entry + 1] - self.table[entry])
                };
                self.weights.push(weight);
                weight_sum += weight;
            }
            for (work, out) in self.work.iter().zip(out.iter_mut()) {
                let taps = &work[first..=self.index + self.half_width];
                let sum: f64 = taps
                    .iter()
                    .zip(&self.weights)
                    .map(|(&sample, &weight)| weight * f64::from(sample))
                    .sum();
                out.push((sum / weight_sum) as f32);
            }
            self.given += 1;
            self.index += self.step_index;
            self.fraction += self.step_fraction;
            if self.fraction >= 1.0 {
                self.fraction -= 1.0;
                self.index += 1;
            }
        }
        // Forget the input no kernel reaches any more.
        let keep_from = (self.index + 1).saturating_sub(self.half_width);
        if keep_from > 4_096 {
            for work in &mut self.work {
                work.drain(..keep_from);
            }
            self.index -= keep_from;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_keeps_its_pitch_level_and_timing_and_the_length_is_scaled() {
        for rate in [44_100.0f64, 32_000.0, 96_000.0, 8_000.0] {
            let frames = 4_410usize;
            let hz = 440.0;
            let tone = |i: usize| (2.0 * PI * hz * i as f64 / rate).sin() as f32;
            let mut resampler = Resampler::new(rate, 48_000.0, 1);
            let mut out = vec![Vec::new()];
            // In uneven pieces: the output does not depend on them.
            let mut done = 0;
            for piece in [1usize, 77, 1_000, 3_332] {
                resampler.process(piece, |_, i| tone(done + i), &mut out);
                done += piece;
            }
            resampler.finish(&mut out);
            let expected = (frames as f64 * 48_000.0 / rate).round() as usize;
            assert_eq!(out[0].len(), expected, "{rate}");
            let mut worst = 0.0f64;
            for (j, &value) in out[0].iter().enumerate().skip(200).take(expected - 400) {
                let want = (2.0 * PI * hz * j as f64 / 48_000.0).sin();
                worst = worst.max((f64::from(value) - want).abs());
            }
            assert!(worst < 1e-3, "{rate}: {worst}");
        }
    }
}
