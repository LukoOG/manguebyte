//! The VU meter.

use core::f64::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use manguebyte_core::decibels::db_to_gain;
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, Row, db, screened, usable_rate};

/// Time the needle takes to reach 99% of a tone that is switched on, in
/// seconds.
const RISE_SECONDS: f64 = 0.3;

/// How far the needle swings past the tone before settling. The standard
/// allows 1% to 1.5%; this is the middle.
const OVERSHOOT: f64 = 0.0125;

/// The level of a sine that reads 0 VU by default, in dBFS.
const DEFAULT_REFERENCE_DBFS: f64 = -18.0;

/// The readings of a [`VuMeter`], for any thread.
///
/// Cloning is cheap and every clone reads the same meter. Nothing here
/// locks or allocates. A channel the meter does not have reads as silence.
#[derive(Debug, Clone)]
pub struct VuReadings {
    shared: Arc<VuShared>,
}

#[derive(Debug)]
struct VuShared {
    deflection: Row,
    reset: Flag,
}

impl VuReadings {
    /// How many channels the meter measures.
    pub fn num_channels(&self) -> usize {
        self.shared.deflection.len()
    }

    /// Where the needle of `channel` is, as a linear ratio: 1.0 is 0 VU,
    /// 0.5 is about -6 VU, 0 is the rest position.
    pub fn deflection(&self, channel: usize) -> f64 {
        self.shared.deflection.get(channel)
    }

    /// The reading of `channel` in VU, floored at -100. A steady sine at
    /// the reference level reads 0.
    pub fn vu(&self, channel: usize) -> f64 {
        db(self.deflection(channel))
    }

    /// Asks the meter to drop its needles to rest. It does so at the start
    /// of the next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// A volume-unit meter with the ballistics of the standard instrument.
///
/// The VU meter of 1939 (later ANSI C16.5 and IEC 60268-17) is a moving-coil
/// instrument behind a full-wave rectifier. It shows the average of the
/// rectified signal, and its needle is slow on purpose: switched onto a
/// steady tone it reaches 99% of the reading in 300 ms, swings past by 1%
/// to 1.5%, and falls back as slowly when the tone stops. What it shows is
/// closer to how loud programme material sounds than to its peaks, which
/// it misses by 6 dB to 10 dB or more.
///
/// The needle is modelled as the mass on a spring that it is: a second-order
/// low-pass, with its damping set for an overshoot of 1.25% and its natural
/// frequency (about 2.15 Hz) set for the 300 ms rise. The model is stepped
/// with its exact solution for an input held over one sample, so the
/// ballistics are the same at every sample rate.
///
/// # Limits
///
/// "0 VU" is an analogue level, +4 dBu. What it corresponds to in a digital
/// system is a convention, not part of any VU standard. The default here is
/// a sine at -18 dBFS; [`with_reference_dbfs`](Self::with_reference_dbfs)
/// sets another, such as -20 dBFS.
///
/// A real needle stops at its rest pin. The reading here likewise never
/// goes below zero deflection, although the model behind it swings 1.25%
/// past rest on the way down.
///
/// A real needle also stops at a pin on the right. This one does not: it
/// reads whatever the audio drives it to. A sample that is not finite is
/// read as silence and one beyond 180 dB over full scale as 180 dB over,
/// so no input leaves the meter stuck (see the
/// [crate documentation](crate#bad-samples)). One sample at that limit
/// moves the needle like any other: at 48 kHz and the default reference it
/// swings to +123 VU, and is back under 0 VU for good 1.2 s later.
///
/// ```
/// use manguebyte_analysis::VuMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = VuMeter::new(1);
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 48_000, num_channels: 1 });
///
/// // One second of a 1 kHz sine at -18 dBFS.
/// let amplitude = 10f32.powf(-18.0 / 20.0);
/// let mut block = AudioBuffer::<f32>::new(1, 48_000);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = amplitude * (core::f32::consts::TAU * n as f32 / 48.0).sin();
/// }
/// meter.measure(&block);
/// assert!(meter.readings().vu(0).abs() < 0.05);
/// ```
#[derive(Debug)]
pub struct VuMeter {
    sample_rate: f64,
    reference_dbfs: f64,
    /// What a rectified sample is multiplied by so that a sine at the
    /// reference level averages to one.
    scale: f64,
    /// How the needle's distance from its target and its speed carry over
    /// from one sample to the next.
    step: [[f64; 2]; 2],
    /// Position and speed of each needle.
    needles: Vec<[f64; 2]>,
    readings: VuReadings,
}

impl VuMeter {
    /// Creates a meter for `channels` channels, with 0 VU at -18 dBFS. This
    /// allocates; nothing after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new(channels: usize) -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            reference_dbfs: DEFAULT_REFERENCE_DBFS,
            scale: scale_for(DEFAULT_REFERENCE_DBFS),
            step: step_matrix(DEFAULT_SAMPLE_RATE),
            needles: vec![[0.0; 2]; channels],
            readings: VuReadings {
                shared: Arc::new(VuShared {
                    deflection: Row::new(channels),
                    reset: Flag::default(),
                }),
            },
        }
    }

    /// Sets the peak level, in dBFS, of the sine that reads 0 VU.
    pub fn with_reference_dbfs(mut self, dbfs: f64) -> Self {
        self.set_reference_dbfs(dbfs);
        self
    }

    /// Changes the level that reads 0 VU.
    /// See [`with_reference_dbfs`](Self::with_reference_dbfs).
    pub fn set_reference_dbfs(&mut self, dbfs: f64) {
        self.reference_dbfs = dbfs;
        self.scale = scale_for(dbfs);
    }

    /// The peak level, in dBFS, of the sine that reads 0 VU.
    pub fn reference_dbfs(&self) -> f64 {
        self.reference_dbfs
    }

    /// Takes the sample rate from `spec` and drops the needles to rest. The
    /// channel count was fixed by [`new`](Self::new).
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.step = step_matrix(self.sample_rate);
        self.reset();
    }

    /// Drops the needles to rest.
    pub fn reset(&mut self) {
        self.needles.fill([0.0; 2]);
        self.publish();
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &VuReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    ///
    /// Channels the meter was not created for are ignored, and a channel
    /// missing from the block keeps its needle where it is.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.needles.fill([0.0; 2]);
        }
        let scale = self.scale;
        let [[keep, drift], [pull, damp]] = self.step;
        for (needle, channel) in self.needles.iter_mut().zip(buffer.iter_channels()) {
            let [mut position, mut speed] = *needle;
            for &sample in channel {
                // The scale is at most 157,080 (a reference just above
                // -100 dBFS), so a screened sample keeps the target finite.
                let target = screened(sample.to_f64()).abs() * scale;
                // While the target holds still, the distance to it and the
                // speed evolve on their own.
                let distance = position - target;
                position = target + keep * distance + drift * speed;
                speed = pull * distance + damp * speed;
            }
            *needle = [position, speed];
        }
        self.publish();
    }

    fn publish(&self) {
        for (channel, needle) in self.needles.iter().enumerate() {
            self.readings
                .shared
                .deflection
                .set(channel, needle[0].max(0.0));
        }
    }
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for VuMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        VuMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        VuMeter::reset(self);
    }
}

/// The factor that makes a rectified sine at `reference_dbfs` average to
/// one. The average of a rectified sine is `2 / pi` of its peak.
///
/// The result is always finite: a reference at or below -100 dBFS, or one
/// that is not a number, has a peak of zero and gives a scale of zero.
fn scale_for(reference_dbfs: f64) -> f64 {
    let peak = db_to_gain(reference_dbfs);
    if peak > 0.0 { FRAC_PI_2 / peak } else { 0.0 }
}

/// Damping ratio and natural frequency, in radians per second, of a needle
/// with the standard rise time and overshoot.
fn movement() -> (f64, f64) {
    // The step response of a second-order system overshoots by
    // exp(-pi * zeta / sqrt(1 - zeta^2)). Solved for the damping ratio:
    let ratio = -OVERSHOOT.ln() / PI;
    let damping = ratio / (1.0 + ratio * ratio).sqrt();

    // With the natural frequency set to one, the response first passes 99%
    // at some time between zero and the time of the overshoot's peak, and
    // it rises all the way there, so bisection finds it.
    let ringing = (1.0 - damping * damping).sqrt();
    let response = |time: f64| {
        1.0 - (-damping * time).exp()
            * ((ringing * time).cos() + damping / ringing * (ringing * time).sin())
    };
    let (mut low, mut high) = (0.0, PI / ringing);
    for _ in 0..100 {
        let middle = 0.5 * (low + high);
        if response(middle) < 0.99 {
            low = middle;
        } else {
            high = middle;
        }
    }
    // Time scales inversely with the natural frequency.
    (damping, high / RISE_SECONDS)
}

/// The exact solution of the needle's equation of motion over one sample
/// with nothing driving it, as the matrix that takes (distance from target,
/// speed) at one sample to the next.
fn step_matrix(sample_rate: f64) -> [[f64; 2]; 2] {
    let (damping, natural) = movement();
    let period = 1.0 / sample_rate;
    let decay_rate = damping * natural;
    let ringing = natural * (1.0 - damping * damping).sqrt();
    let envelope = (-decay_rate * period).exp();
    let (sin, cos) = (ringing * period).sin_cos();
    [
        [
            envelope * (cos + decay_rate / ringing * sin),
            envelope * sin / ringing,
        ],
        [
            -envelope * natural * natural / ringing * sin,
            envelope * (cos - decay_rate / ringing * sin),
        ],
    ]
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;

    fn spec(sample_rate: f64) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 4_800,
            num_channels: 1,
        }
    }

    /// Feeds `seconds` of a 997 Hz sine at `dbfs` one sample at a time and
    /// returns the deflection after each sample.
    ///
    /// 997 Hz, the reference frequency of IEC 61606, shares no factor with
    /// the sample rates, so the samples visit every part of the cycle. At
    /// exactly 1 kHz and 48 kHz they keep landing on the same 48 points,
    /// whose rectified average is 0.14% under that of the waveform.
    fn trace(meter: &mut VuMeter, sample_rate: f64, dbfs: f64, seconds: f64) -> Vec<f64> {
        let amplitude = if dbfs <= -200.0 {
            0.0
        } else {
            10f64.powf(dbfs / 20.0)
        };
        let frames = (seconds * sample_rate).round() as usize;
        let mut buffer = AudioBuffer::<f64>::new(1, 1);
        (0..frames)
            .map(|n| {
                buffer.channel_mut(0)[0] = amplitude * (TAU * 997.0 * n as f64 / sample_rate).sin();
                meter.measure(&buffer);
                meter.readings().deflection(0)
            })
            .collect()
    }

    /// A moving average over one millisecond, to take the ripple of the
    /// rectified tone out of a trace before timing it.
    fn smooth(values: &[f64], sample_rate: f64) -> Vec<f64> {
        let width = (sample_rate / 1_000.0).round() as usize;
        values
            .windows(width)
            .map(|window| window.iter().sum::<f64>() / width as f64)
            .collect()
    }

    #[test]
    fn the_needle_takes_300_ms_to_reach_99_percent_and_overshoots_a_little() {
        for sample_rate in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let mut meter = VuMeter::new(1);
            meter.prepare(&spec(sample_rate));
            let needle = smooth(&trace(&mut meter, sample_rate, -18.0, 1.5), sample_rate);

            let reached = needle.iter().position(|&value| value >= 0.99).unwrap();
            let rise_ms = reached as f64 / sample_rate * 1_000.0;
            // The standard allows 300 ms give or take 10%.
            assert!(
                (rise_ms - 300.0).abs() < 3.0,
                "{sample_rate}: 99% after {rise_ms:.1} ms"
            );

            let peak = needle.iter().copied().fold(0.0, f64::max);
            assert!(
                (1.010..=1.015).contains(&peak),
                "{sample_rate}: overshoot to {peak:.4}"
            );
            // And it settles on the tone.
            let settled = needle[needle.len() - 1];
            assert!((settled - 1.0).abs() < 1e-3, "{sample_rate}: {settled}");
        }
    }

    #[test]
    fn the_needle_falls_as_slowly_as_it_rises() {
        let sample_rate = 48_000.0;
        let mut meter = VuMeter::new(1);
        meter.prepare(&spec(sample_rate));
        trace(&mut meter, sample_rate, -18.0, 2.0);
        let start = meter.readings().deflection(0);
        assert!((start - 1.0).abs() < 2e-3);

        let fall = trace(&mut meter, sample_rate, -1_000.0, 1.0);
        let reached = fall.iter().position(|&value| value <= 0.01).unwrap();
        let fall_ms = reached as f64 / sample_rate * 1_000.0;
        assert!((fall_ms - 300.0).abs() < 3.0, "1% after {fall_ms:.1} ms");
        // The reading stops at rest where the model swings below it, and
        // a second later there is nothing left of the swing.
        assert!(fall.iter().all(|&value| value >= 0.0));
        assert!(fall[reached..].contains(&0.0));
        assert!(fall[fall.len() - 1] < 1e-4);
    }

    #[test]
    fn a_steady_tone_reads_its_level_against_the_reference() {
        for (reference, level, expected) in [
            (-18.0, -18.0, 0.0),
            (-18.0, -28.0, -10.0),
            (-18.0, -15.0, 3.0),
            (-20.0, -18.0, 2.0),
            (-14.0, -18.0, -4.0),
        ] {
            let mut meter = VuMeter::new(1).with_reference_dbfs(reference);
            assert_eq!(meter.reference_dbfs(), reference);
            meter.prepare(&spec(48_000.0));
            let needle = trace(&mut meter, 48_000.0, level, 2.0);
            let reading = db(smooth(&needle, 48_000.0).last().copied().unwrap());
            assert!((reading - expected).abs() < 0.01, "{reading} vs {expected}");
        }
    }

    #[test]
    fn it_shows_the_average_not_the_peak_or_the_rms() {
        // A square wave with the same peak as the reference sine has an
        // average pi / 2 times as high: +3.92 VU. An RMS meter would say
        // +3.01, a peak meter 0.
        let mut meter = VuMeter::new(1);
        meter.prepare(&spec(48_000.0));
        let amplitude = 10f32.powf(-18.0 / 20.0);
        let mut buffer = AudioBuffer::<f32>::new(1, 96_000);
        for (n, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = if (n / 24) % 2 == 0 {
                amplitude
            } else {
                -amplitude
            };
        }
        meter.measure(&buffer);
        let reading = meter.readings().vu(0);
        assert!((reading - 3.922).abs() < 0.01, "{reading}");
    }

    #[test]
    fn the_movement_has_the_standard_figures() {
        let (damping, natural) = movement();
        assert!((damping - 0.8127).abs() < 1e-4, "{damping}");
        assert!((natural / TAU - 2.1505).abs() < 1e-3, "{}", natural / TAU);
    }

    #[test]
    fn absurd_rates_and_references_still_give_a_working_meter() {
        let mut buffer = AudioBuffer::<f32>::new(1, 64);
        buffer.channel_mut(0).fill(0.1);
        // Positive, finite, and no sample rate. The smallest of them has a
        // sample period that is not a number any more; all are taken as
        // 1 Hz or 10 MHz.
        for rate in [5e-324, 1e-300, 0.5, 1e9, 1e300, f64::MAX] {
            let mut meter = VuMeter::new(1);
            meter.prepare(&spec(rate));
            assert!(
                meter.step.iter().flatten().all(|value| value.is_finite()),
                "{rate:e}: {:?}",
                meter.step
            );
            meter.measure(&buffer);
            // The needle moved, which it does not when its position is not
            // a number.
            let deflection = meter.readings().deflection(0);
            assert!(deflection.is_finite() && deflection > 0.0, "{rate:e}");
        }

        // The reference level only ever becomes a finite scale.
        for reference in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -5_000.0,
            -100.0,
            -99.9,
            0.0,
            5_000.0,
        ] {
            let scale = scale_for(reference);
            assert!((0.0..157_080.0).contains(&scale), "{reference}: {scale}");
            let mut meter = VuMeter::new(1).with_reference_dbfs(reference);
            meter.prepare(&spec(48_000.0));
            meter.measure(&buffer);
            assert!(meter.readings().deflection(0).is_finite(), "{reference}");
        }
    }

    #[test]
    fn a_request_or_a_bad_sample_does_not_upset_the_needle() {
        let mut meter = VuMeter::new(2);
        meter.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 64,
            num_channels: 2,
        });
        let mut buffer = AudioBuffer::<f32>::new(2, 64);
        buffer.channel_mut(0).fill(0.1);
        buffer.channel_mut(1).fill(f32::NAN);
        meter.measure(&buffer);
        let readings = meter.readings().clone();
        assert_eq!(readings.num_channels(), 2);
        assert!(readings.deflection(0) > 0.0);
        assert_eq!(readings.deflection(1), 0.0);
        assert_eq!(readings.deflection(2), 0.0);

        readings.request_reset();
        meter.measure(&AudioBuffer::<f32>::new(2, 1));
        assert_eq!(readings.deflection(0), 0.0);
    }
}
