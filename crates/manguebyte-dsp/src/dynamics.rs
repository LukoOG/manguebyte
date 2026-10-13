use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// Level below which a signal is treated as silence, in decibels.
const FLOOR_DB: f64 = -120.0;

fn to_db(level: f64) -> f64 {
    if level > 1e-6 {
        20.0 * level.log10()
    } else {
        FLOOR_DB
    }
}

fn to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// One-pole smoothing coefficient that covers 63% of a step in
/// `milliseconds`. Zero or less means "instantly".
fn time_coefficient(milliseconds: f64, sample_rate: f64) -> f64 {
    if milliseconds <= 0.0 {
        0.0
    } else {
        (-1.0 / (milliseconds * 1e-3 * sample_rate)).exp()
    }
}

/// A feed-forward compressor: it turns down what is louder than a threshold.
///
/// The level is measured across all channels together, so a stereo image does
/// not shift when only one side is loud. The gain computer and the smoothing
/// both work in decibels, following D. Giannoulis, M. Massberg and J. D.
/// Reiss, "Digital dynamic range compressor design: a tutorial and analysis",
/// Journal of the Audio Engineering Society, 2012.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{Compressor, ProcessSpec, Processor};
///
/// let mut compressor = Compressor::<f32>::new()
///     .with_threshold_db(-20.0)
///     .with_ratio(4.0)
///     .with_attack_ms(5.0)
///     .with_release_ms(100.0);
/// compressor.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A full-scale signal is 20 dB over the threshold, so 4:1 leaves 5 dB of
/// // that: 15 dB of gain reduction once the attack has passed.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// block.channel_mut(0).fill(1.0);
/// compressor.process(&mut block);
/// assert!((compressor.gain_reduction_db() - 15.0).abs() < 0.1);
/// ```
#[derive(Debug, Clone)]
pub struct Compressor<S: Sample = f32> {
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    attack_ms: f64,
    release_ms: f64,
    makeup_db: f64,
    sample_rate: f64,
    attack: f64,
    release: f64,
    makeup: f64,
    /// Smoothed gain reduction in decibels, zero or positive.
    reduction_db: f64,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for Compressor<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> Compressor<S> {
    /// Creates a compressor with a threshold of -18 dB, a ratio of 4:1, a
    /// hard knee, 10 ms attack and 100 ms release.
    pub fn new() -> Self {
        let mut compressor = Self {
            threshold_db: -18.0,
            ratio: 4.0,
            knee_db: 0.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            sample_rate: 44_100.0,
            attack: 0.0,
            release: 0.0,
            makeup: 1.0,
            reduction_db: 0.0,
            marker: PhantomData,
        };
        compressor.update();
        compressor
    }

    /// Sets the level above which gain is reduced, in decibels.
    pub fn set_threshold_db(&mut self, db: f64) {
        self.threshold_db = db;
    }

    /// Sets how strongly the excess is reduced: `4.0` lets 1 dB out for every
    /// 4 dB over the threshold. Values below 1 are treated as 1; infinity
    /// makes a limiter.
    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = ratio.max(1.0);
    }

    /// Sets the width, in decibels, of the region around the threshold where
    /// compression eases in. Zero is a hard knee.
    pub fn set_knee_db(&mut self, db: f64) {
        self.knee_db = db.max(0.0);
    }

    /// Sets how quickly gain is reduced when the level rises, in milliseconds.
    pub fn set_attack_ms(&mut self, milliseconds: f64) {
        self.attack_ms = milliseconds;
        self.update();
    }

    /// Sets how quickly gain recovers when the level falls, in milliseconds.
    pub fn set_release_ms(&mut self, milliseconds: f64) {
        self.release_ms = milliseconds;
        self.update();
    }

    /// Sets the gain applied after compression, in decibels.
    pub fn set_makeup_db(&mut self, db: f64) {
        self.makeup_db = db;
        self.update();
    }

    /// Builder form of [`set_threshold_db`](Self::set_threshold_db).
    #[must_use]
    pub fn with_threshold_db(mut self, db: f64) -> Self {
        self.set_threshold_db(db);
        self
    }

    /// Builder form of [`set_ratio`](Self::set_ratio).
    #[must_use]
    pub fn with_ratio(mut self, ratio: f64) -> Self {
        self.set_ratio(ratio);
        self
    }

    /// Builder form of [`set_knee_db`](Self::set_knee_db).
    #[must_use]
    pub fn with_knee_db(mut self, db: f64) -> Self {
        self.set_knee_db(db);
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

    /// Builder form of [`set_makeup_db`](Self::set_makeup_db).
    #[must_use]
    pub fn with_makeup_db(mut self, db: f64) -> Self {
        self.set_makeup_db(db);
        self
    }

    /// How much the gain is currently being reduced, in decibels, as a
    /// positive number. For driving a meter.
    pub fn gain_reduction_db(&self) -> f64 {
        self.reduction_db
    }

    /// The gain reduction, in decibels, that a steady input at `level_db`
    /// settles at.
    pub fn static_reduction_db(&self, level_db: f64) -> f64 {
        let over = level_db - self.threshold_db;
        let slope = 1.0 - 1.0 / self.ratio;
        if 2.0 * over <= -self.knee_db {
            0.0
        } else if 2.0 * over.abs() < self.knee_db {
            let into_knee = over + self.knee_db / 2.0;
            slope * into_knee * into_knee / (2.0 * self.knee_db)
        } else {
            slope * over
        }
    }

    fn update(&mut self) {
        self.attack = time_coefficient(self.attack_ms, self.sample_rate);
        self.release = time_coefficient(self.release_ms, self.sample_rate);
        self.makeup = to_gain(self.makeup_db);
    }
}

impl<S: Sample> Processor<S> for Compressor<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.update();
        self.reduction_db = 0.0;
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let channels = buffer.num_channels();
        for frame in 0..buffer.num_frames() {
            let mut level = 0.0f64;
            for channel in 0..channels {
                level = level.max(buffer.channel(channel)[frame].to_f64().abs());
            }
            let wanted = self.static_reduction_db(to_db(level));
            let coefficient = if wanted > self.reduction_db {
                self.attack
            } else {
                self.release
            };
            self.reduction_db = wanted + coefficient * (self.reduction_db - wanted);

            let gain = S::from_f64(to_gain(-self.reduction_db) * self.makeup);
            for channel in 0..channels {
                buffer.channel_mut(channel)[frame] *= gain;
            }
        }
    }

    fn reset(&mut self) {
        self.reduction_db = 0.0;
    }
}

/// A noise gate: it closes when the signal falls below a threshold.
///
/// The gate opens over the attack time once the level reaches the threshold,
/// stays open for the hold time after the level drops, then closes over the
/// release time. Levels are measured across all channels together.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{NoiseGate, ProcessSpec, Processor};
///
/// let mut gate = NoiseGate::<f32>::new().with_threshold_db(-40.0);
/// gate.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // Hiss at -60 dB is below the threshold and is silenced.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// block.channel_mut(0).fill(0.001);
/// gate.process(&mut block);
/// assert_eq!(block.peak(), 0.0);
/// ```
#[derive(Debug, Clone)]
pub struct NoiseGate<S: Sample = f32> {
    threshold_db: f64,
    attack_ms: f64,
    hold_ms: f64,
    release_ms: f64,
    floor_db: f64,
    sample_rate: f64,
    threshold: f64,
    attack: f64,
    release: f64,
    floor: f64,
    hold_samples: u32,
    hold_left: u32,
    /// Current gain, between the floor and 1.
    gain: f64,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for NoiseGate<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> NoiseGate<S> {
    /// Creates a gate with a threshold of -50 dB, 1 ms attack, 50 ms hold and
    /// 100 ms release, closing to silence.
    pub fn new() -> Self {
        let mut gate = Self {
            threshold_db: -50.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            release_ms: 100.0,
            floor_db: f64::NEG_INFINITY,
            sample_rate: 44_100.0,
            threshold: 0.0,
            attack: 0.0,
            release: 0.0,
            floor: 0.0,
            hold_samples: 0,
            hold_left: 0,
            gain: 0.0,
            marker: PhantomData,
        };
        gate.update();
        gate
    }

    /// Sets the level below which the gate closes, in decibels.
    pub fn set_threshold_db(&mut self, db: f64) {
        self.threshold_db = db;
        self.update();
    }

    /// Sets how quickly the gate opens, in milliseconds.
    pub fn set_attack_ms(&mut self, milliseconds: f64) {
        self.attack_ms = milliseconds;
        self.update();
    }

    /// Sets how long the gate stays open after the level drops, in
    /// milliseconds.
    pub fn set_hold_ms(&mut self, milliseconds: f64) {
        self.hold_ms = milliseconds;
        self.update();
    }

    /// Sets how quickly the gate closes, in milliseconds.
    pub fn set_release_ms(&mut self, milliseconds: f64) {
        self.release_ms = milliseconds;
        self.update();
    }

    /// Sets the gain of the closed gate, in decibels. Negative infinity, the
    /// default, closes to silence; `-20.0` only turns the noise down.
    pub fn set_floor_db(&mut self, db: f64) {
        self.floor_db = db;
        self.update();
    }

    /// Builder form of [`set_threshold_db`](Self::set_threshold_db).
    #[must_use]
    pub fn with_threshold_db(mut self, db: f64) -> Self {
        self.set_threshold_db(db);
        self
    }

    /// Builder form of [`set_attack_ms`](Self::set_attack_ms).
    #[must_use]
    pub fn with_attack_ms(mut self, milliseconds: f64) -> Self {
        self.set_attack_ms(milliseconds);
        self
    }

    /// Builder form of [`set_hold_ms`](Self::set_hold_ms).
    #[must_use]
    pub fn with_hold_ms(mut self, milliseconds: f64) -> Self {
        self.set_hold_ms(milliseconds);
        self
    }

    /// Builder form of [`set_release_ms`](Self::set_release_ms).
    #[must_use]
    pub fn with_release_ms(mut self, milliseconds: f64) -> Self {
        self.set_release_ms(milliseconds);
        self
    }

    /// Builder form of [`set_floor_db`](Self::set_floor_db).
    #[must_use]
    pub fn with_floor_db(mut self, db: f64) -> Self {
        self.set_floor_db(db);
        self
    }

    /// Whether the gate is currently more open than closed.
    pub fn is_open(&self) -> bool {
        self.gain > 0.5 * (1.0 + self.floor)
    }

    fn update(&mut self) {
        self.threshold = to_gain(self.threshold_db);
        self.attack = time_coefficient(self.attack_ms, self.sample_rate);
        self.release = time_coefficient(self.release_ms, self.sample_rate);
        self.floor = if self.floor_db <= FLOOR_DB {
            0.0
        } else {
            to_gain(self.floor_db).min(1.0)
        };
        self.hold_samples = (self.hold_ms.max(0.0) * 1e-3 * self.sample_rate).round() as u32;
    }
}

impl<S: Sample> Processor<S> for NoiseGate<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.update();
        self.reset();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let channels = buffer.num_channels();
        for frame in 0..buffer.num_frames() {
            let mut level = 0.0f64;
            for channel in 0..channels {
                level = level.max(buffer.channel(channel)[frame].to_f64().abs());
            }

            let (target, coefficient) = if level >= self.threshold {
                self.hold_left = self.hold_samples;
                (1.0, self.attack)
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
                (1.0, self.attack)
            } else {
                (self.floor, self.release)
            };
            self.gain = target + coefficient * (self.gain - target);
            // An exponential approach never arrives; finish the last step so
            // a closed gate is truly silent.
            if (self.gain - target).abs() < 1e-5 {
                self.gain = target;
            }

            let gain = S::from_f64(self.gain);
            for channel in 0..channels {
                buffer.channel_mut(channel)[frame] *= gain;
            }
        }
    }

    fn reset(&mut self) {
        self.gain = self.floor;
        self.hold_left = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ProcessSpec = ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 48_000,
        num_channels: 1,
    };

    fn steady(level: f64, frames: usize) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(1, frames);
        buffer.channel_mut(0).fill(level);
        buffer
    }

    #[test]
    fn leaves_quiet_signals_alone() {
        let mut compressor = Compressor::<f64>::new().with_threshold_db(-12.0);
        compressor.prepare(&SPEC);
        let mut buffer = steady(to_gain(-20.0), 4_800);
        compressor.process(&mut buffer);
        assert!((buffer.channel(0)[4_799] - to_gain(-20.0)).abs() < 1e-12);
        assert_eq!(compressor.gain_reduction_db(), 0.0);
    }

    #[test]
    fn steady_state_follows_the_ratio() {
        for (ratio, expected_out_db) in [(2.0, -15.0), (4.0, -17.5), (f64::INFINITY, -20.0)] {
            let mut compressor = Compressor::<f64>::new()
                .with_threshold_db(-20.0)
                .with_ratio(ratio)
                .with_attack_ms(1.0);
            compressor.prepare(&SPEC);
            let mut buffer = steady(to_gain(-10.0), 9_600);
            compressor.process(&mut buffer);
            let out_db = to_db(buffer.channel(0)[9_599]);
            assert!(
                (out_db - expected_out_db).abs() < 0.01,
                "ratio {ratio}: {out_db} dB"
            );
        }
    }

    #[test]
    fn soft_knee_is_continuous_and_eases_in() {
        let compressor = Compressor::<f64>::new()
            .with_threshold_db(-20.0)
            .with_ratio(4.0)
            .with_knee_db(12.0);
        // Below the knee: nothing. Above it: the straight line.
        assert_eq!(compressor.static_reduction_db(-26.0), 0.0);
        assert!((compressor.static_reduction_db(-14.0) - 0.75 * 6.0).abs() < 1e-12);
        // At the threshold a hard knee does nothing; a soft one already acts.
        let at_threshold = compressor.static_reduction_db(-20.0);
        assert!(at_threshold > 0.0 && at_threshold < 0.75 * 6.0);
        // No jumps anywhere along the curve.
        let mut previous = 0.0;
        for step in 0..=400 {
            let level = -30.0 + 0.05 * f64::from(step);
            let reduction = compressor.static_reduction_db(level);
            assert!(reduction >= previous && reduction - previous < 0.05);
            previous = reduction;
        }
    }

    #[test]
    fn attack_and_release_take_their_stated_time() {
        let mut compressor = Compressor::<f64>::new()
            .with_threshold_db(-20.0)
            .with_ratio(f64::INFINITY)
            .with_attack_ms(10.0)
            .with_release_ms(100.0);
        compressor.prepare(&SPEC);

        // 20 dB over the threshold; after one attack time, 63% of the way.
        let mut loud = steady(1.0, 480);
        compressor.process(&mut loud);
        let after_attack = compressor.gain_reduction_db();
        assert!((after_attack - 20.0 * (1.0 - (-1.0f64).exp())).abs() < 0.1);

        let mut settle = steady(1.0, 48_000);
        compressor.process(&mut settle);
        assert!((compressor.gain_reduction_db() - 20.0).abs() < 1e-6);

        // Silence: after one release time, 37% of the reduction is left.
        let mut quiet = steady(0.0, 4_800);
        compressor.process(&mut quiet);
        assert!((compressor.gain_reduction_db() - 20.0 * (-1.0f64).exp()).abs() < 0.1);
    }

    #[test]
    fn channels_are_linked_and_makeup_is_applied() {
        let mut compressor = Compressor::<f64>::new()
            .with_threshold_db(-20.0)
            .with_ratio(f64::INFINITY)
            .with_attack_ms(0.0)
            .with_makeup_db(6.0);
        compressor.prepare(&ProcessSpec {
            num_channels: 2,
            ..SPEC
        });
        let mut buffer = AudioBuffer::<f64>::new(2, 64);
        buffer.channel_mut(0).fill(1.0);
        buffer.channel_mut(1).fill(0.01);
        compressor.process(&mut buffer);
        // Both sides are turned down by the same 20 dB, then up by 6.
        assert!((to_db(buffer.channel(0)[63]) + 14.0).abs() < 1e-6);
        assert!((to_db(buffer.channel(1)[63]) + 54.0).abs() < 1e-6);
    }

    #[test]
    fn gate_opens_holds_and_closes() {
        let mut gate = NoiseGate::<f64>::new()
            .with_threshold_db(-30.0)
            .with_attack_ms(1.0)
            .with_hold_ms(20.0)
            .with_release_ms(10.0);
        gate.prepare(&SPEC);
        assert!(!gate.is_open());

        let mut loud = steady(0.5, 960);
        gate.process(&mut loud);
        assert!(gate.is_open());
        assert!((loud.channel(0)[959] - 0.5).abs() < 1e-6);

        // Just inside the hold time, it is still fully open.
        let mut quiet = steady(0.001, 900);
        gate.process(&mut quiet);
        assert!(gate.is_open());
        assert!((quiet.channel(0)[899] - 0.001).abs() < 1e-9);

        // Well after hold and release, it is silent.
        let mut tail = steady(0.001, 9_600);
        gate.process(&mut tail);
        assert!(!gate.is_open());
        assert_eq!(tail.channel(0)[9_599], 0.0);
    }

    #[test]
    fn gate_floor_attenuates_instead_of_silencing() {
        let mut gate = NoiseGate::<f64>::new()
            .with_threshold_db(-30.0)
            .with_floor_db(-20.0);
        gate.prepare(&SPEC);
        let mut buffer = steady(0.001, 4_800);
        gate.process(&mut buffer);
        assert!((buffer.channel(0)[4_799] - 0.0001).abs() < 1e-9);
    }
}
