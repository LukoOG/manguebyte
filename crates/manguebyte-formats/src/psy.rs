//! What a lossy encoder may leave out: a masking threshold for each block
//! of a transform, and where the transients are that call for short
//! blocks. The Vorbis and MP3 encoders share it.
//!
//! The model is a simple one, of the kind textbooks start from (Zwicker and
//! Fastl, *Psychoacoustics*, 1990; Painter and Spanias, "Perceptual coding
//! of digital audio", Proc. IEEE 88(4), 2000), with the tonality estimate
//! of Johnston ("Transform coding of audio signals using perceptual noise
//! criteria", IEEE JSAC 6(2), 1988): in each critical band, the spectral
//! flatness of the power spectrum tells a tone (flatness of -60 dB or less)
//! from noise (0 dB), and the components of the band are lowered by
//! `14.5 + z` dB for a tone in band `z` and by 5.5 dB for noise, in
//! proportion. They are then spread along the Bark scale with a slope of
//! 27 dB per Bark below each component and 12 dB per Bark above it. What a
//! band masks is noise over the whole band, so each bin's threshold is its
//! share of it, over the bins within half a Bark of it; and the threshold
//! is never below the absolute threshold of hearing in
//! Terhardt's approximation, with full scale taken as 96 dB SPL. The
//! model does not mask from block to block, which would let the encoder
//! spend fewer bits for the same quality.

/// How far below a masker the noise it masks lies, in dB: for a tone in
/// critical band `z`, `TONE_OFFSET_DB + z`, and for noise,
/// `NOISE_OFFSET_DB`.
const TONE_OFFSET_DB: f64 = 14.5;
const NOISE_OFFSET_DB: f64 = 5.5;
/// The slopes of the spreading, in dB per Bark, towards lower and higher
/// frequencies.
const SLOPE_DOWN_DB: f64 = 27.0;
const SLOPE_UP_DB: f64 = 12.0;
/// The sound pressure level a full-scale sine is taken to have.
const FULL_SCALE_SPL: f64 = 96.0;

/// The Bark of a frequency in hertz (Zwicker and Terhardt, 1980).
pub(crate) fn bark(hz: f64) -> f64 {
    13.0 * (0.000_76 * hz).atan() + 3.5 * (hz / 7_500.0).powi(2).atan()
}

/// The absolute threshold of hearing in dB SPL (Terhardt, 1979).
pub(crate) fn threshold_in_quiet(hz: f64) -> f64 {
    let khz = (hz / 1_000.0).clamp(0.02, 20.0);
    3.64 * khz.powf(-0.8) - 6.5 * (-0.6 * (khz - 3.3).powi(2)).exp() + 1e-3 * khz.powi(4)
}

/// The tables of the model for one block size.
pub(crate) struct Model {
    /// The attenuation from each bin to the next, as power factors, up and
    /// down the scale.
    step_up: Vec<f32>,
    step_down: Vec<f32>,
    /// The threshold in quiet of each bin, as power in the units of the
    /// transform.
    quiet: Vec<f32>,
    /// Where each critical band starts, and the end of the last.
    bands: Vec<usize>,
    /// One over the bins of the critical band around each bin: the share
    /// of a band's masked noise that one bin may hold.
    share: Vec<f32>,
    power: Vec<f32>,
}

impl Model {
    /// The model for blocks of `n` samples at `rate` hertz, whose
    /// transform gives a full-scale sine a power of `full_scale` in its
    /// strongest bin.
    pub(crate) fn new(n: usize, rate: u32, full_scale: f64) -> Self {
        let half = n / 2;
        let hz = |bin: f64| (bin + 0.5) * f64::from(rate) / n as f64;
        let mut step_up = vec![1.0f32; half];
        let mut step_down = vec![1.0f32; half];
        for bin in 1..half {
            let distance = bark(hz(bin as f64)) - bark(hz(bin as f64 - 1.0));
            step_up[bin] = 10f64.powf(-SLOPE_UP_DB * distance / 10.0) as f32;
            step_down[bin - 1] = 10f64.powf(-SLOPE_DOWN_DB * distance / 10.0) as f32;
        }
        let quiet = (0..half)
            .map(|bin| {
                let level = threshold_in_quiet(hz(bin as f64)) - FULL_SCALE_SPL;
                (full_scale * 10f64.powf(level / 10.0)) as f32
            })
            .collect();
        let mut bands = vec![0usize];
        for bin in 1..half {
            if bark(hz(bin as f64)).floor() > bark(hz(bin as f64 - 1.0)).floor() {
                bands.push(bin);
            }
        }
        bands.push(half);
        // The critical band around each bin is a Bark wide; the Bark rises
        // with the bin, so its edges only move up.
        let barks: Vec<f64> = (0..half).map(|bin| bark(hz(bin as f64))).collect();
        let (mut low, mut high) = (0usize, 0usize);
        let share = barks
            .iter()
            .map(|&z| {
                while barks[low] <= z - 0.5 {
                    low += 1;
                }
                while high < half && barks[high] < z + 0.5 {
                    high += 1;
                }
                1.0 / (high - low).max(1) as f32
            })
            .collect();
        Self {
            step_up,
            step_down,
            quiet,
            bands,
            share,
            power: vec![0.0; half],
        }
    }

    /// The masking threshold of the spectrum `x`, as power in each bin,
    /// into `threshold`.
    pub(crate) fn threshold(&mut self, x: &[f32], threshold: &mut [f32]) {
        let half = x.len();
        // The power, smoothed over neighbouring bins, which the phase of a
        // component spreads its energy across.
        for bin in 0..half {
            let here = x[bin] * x[bin];
            let below = if bin > 0 {
                x[bin - 1] * x[bin - 1]
            } else {
                0.0
            };
            let above = if bin + 1 < half {
                x[bin + 1] * x[bin + 1]
            } else {
                0.0
            };
            self.power[bin] = here + 0.5 * (below + above);
        }
        // Each band lowered by what its tonality says it masks.
        for (z, edges) in self.bands.windows(2).enumerate() {
            let band = &mut self.power[edges[0]..edges[1]];
            let count = band.len() as f64;
            let floor = 1e-20f64;
            let arithmetic = band.iter().map(|&p| f64::from(p) + floor).sum::<f64>() / count;
            let geometric = (band
                .iter()
                .map(|&p| (f64::from(p) + floor).ln())
                .sum::<f64>()
                / count)
                .exp();
            let flatness_db = 10.0 * (geometric / arithmetic).log10();
            let tonality = (flatness_db / -60.0).clamp(0.0, 1.0);
            let offset_db =
                tonality * (TONE_OFFSET_DB + z as f64) + (1.0 - tonality) * NOISE_OFFSET_DB;
            let factor = 10f64.powf(-offset_db / 10.0) as f32;
            for value in band {
                *value *= factor;
            }
        }
        // The spreading, as the largest of each component's slopes: up
        // the scale, then down it.
        let mut carried = 0.0f32;
        for ((value, &step), &power) in threshold[..half]
            .iter_mut()
            .zip(&self.step_up)
            .zip(&self.power)
        {
            carried = (carried * step).max(power);
            *value = carried;
        }
        carried = 0.0;
        for bin in (0..half).rev() {
            carried = (carried * self.step_down[bin]).max(threshold[bin]);
            threshold[bin] = carried;
        }
        // The masked noise of a band is spread over its bins.
        for ((value, &quiet), &share) in threshold.iter_mut().zip(&self.quiet).zip(&self.share) {
            *value = (*value * share).max(quiet);
        }
    }
}

/// Finds the transients of a stream: the stretches whose high frequencies
/// are much louder than those just before.
///
/// The signal is differenced, which lifts the high frequencies, and its
/// energy summed over hops of [`HOP`] samples; a hop is a transient if it
/// holds more than eight times the mean of the eight before it, and more
/// than a floor below which nothing is heard.
pub(crate) struct Transients {
    previous: Vec<f32>,
    history: [f32; 8],
    at: usize,
    sum: f32,
    count: usize,
}

/// Samples per hop of the transient detector.
pub(crate) const HOP: usize = 64;

impl Transients {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            previous: vec![0.0; channels],
            history: [0.0; 8],
            at: 0,
            sum: 0.0,
            count: 0,
        }
    }

    /// Takes in one sample of a channel. After [`HOP`] samples of every
    /// channel, [`end_hop`](Self::end_hop) says whether the hop was a
    /// transient.
    #[inline]
    pub(crate) fn add(&mut self, channel: usize, sample: f32) {
        let difference = sample - self.previous[channel];
        self.previous[channel] = sample;
        self.sum += difference * difference;
    }

    /// Closes a hop: whether it was a transient.
    pub(crate) fn end_hop(&mut self) -> bool {
        let energy = self.sum;
        self.sum = 0.0;
        // The mean of the hops seen, up to eight.
        let seen = self.count.clamp(1, self.history.len());
        let mean = self.history.iter().sum::<f32>() / seen as f32;
        self.history[self.at] = energy;
        self.at = (self.at + 1) % self.history.len();
        self.count += 1;
        // 8 times is 9 dB; the floor is about -70 dB of full scale.
        self.count > 1 && energy > 8.0 * mean && energy > 1e-4 * HOP as f32 * 1e-3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scales_are_the_published_ones() {
        assert!((bark(1_000.0) - 8.5).abs() < 0.2);
        assert!((bark(4_000.0) - 17.3).abs() < 0.3);
        assert!(threshold_in_quiet(3_300.0) < -3.0);
        assert!(threshold_in_quiet(100.0) > 20.0);
    }

    #[test]
    fn a_tone_masks_its_neighbours_and_not_far_away() {
        let n = 2_048;
        let mut model = Model::new(n, 48_000, 1.0);
        let mut x = vec![0.0f32; n / 2];
        x[100] = 1.0;
        let mut threshold = vec![0.0f32; n / 2];
        model.threshold(&x, &mut threshold);
        let db = |value: f32| 10.0 * f64::from(value).log10();
        // At the tone, the offset of a tone in its band below it, shared
        // among the bins of a critical band.
        let z = bark((100.5) * 48_000.0 / n as f64).floor();
        let expected = -(TONE_OFFSET_DB + z) + db(model.share[100]);
        assert!(
            (db(threshold[100]) - expected).abs() < 1.5,
            "{} {expected}",
            db(threshold[100])
        );
        // Masking falls off faster below than above.
        assert!(threshold[90] < threshold[110]);
        assert!(db(threshold[400]) < db(threshold[100]) - 40.0);
    }

    #[test]
    fn a_click_after_quiet_is_a_transient() {
        let mut detector = Transients::new(1);
        let mut flags = Vec::new();
        for hop in 0..20 {
            for i in 0..HOP {
                let sample = if hop == 12 && i == 30 {
                    0.8
                } else {
                    0.001 * (i as f32).sin()
                };
                detector.add(0, sample);
            }
            flags.push(detector.end_hop());
        }
        assert!(flags[12]);
        assert_eq!(flags.iter().filter(|&&flag| flag).count(), 1);
    }
}
