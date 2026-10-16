use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// The curve a [`Waveshaper`] bends the signal through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaperCurve {
    /// Flat above full scale. The harshest: many strong harmonics.
    HardClip,
    /// A smooth S that never quite reaches full scale, like tape or a tube.
    Tanh,
    /// A cubic that eases into full scale and stays there. Milder than
    /// `Tanh` at low levels.
    SoftClip,
    /// Reflects what exceeds full scale back inwards, for the metallic
    /// sound of a wavefolder.
    Fold,
}

impl ShaperCurve {
    /// Applies the curve to one value.
    #[inline]
    pub fn apply(self, x: f64) -> f64 {
        match self {
            Self::HardClip => x.clamp(-1.0, 1.0),
            Self::Tanh => x.tanh(),
            Self::SoftClip => {
                let x = x.clamp(-1.5, 1.5);
                x - (4.0 / 27.0) * x * x * x
            }
            Self::Fold => {
                // A triangle wave of period 4 that is the identity near zero.
                let phase = (x + 1.0).rem_euclid(4.0);
                if phase < 2.0 {
                    phase - 1.0
                } else {
                    3.0 - phase
                }
            }
        }
    }
}

/// Distortion: gain into a curve, then gain out.
///
/// Bending a waveform creates frequencies that were not there, and the ones
/// above half the sample rate fold back as aliasing. For anything but gentle
/// settings, wrap the shaper in [`Oversampled`](crate::Oversampled).
///
/// ```
/// use manguebyte_dsp::{ShaperCurve, Waveshaper};
///
/// let shaper = Waveshaper::<f32>::new(ShaperCurve::Tanh).with_drive_db(12.0);
/// // Quiet signals come out louder by the drive; loud ones are held back.
/// assert!((shaper.shape(0.01) - 0.0398).abs() < 1e-3);
/// assert!(shaper.shape(1.0) < 1.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waveshaper<S: Sample = f32> {
    curve: ShaperCurve,
    drive: f64,
    output: f64,
    marker: PhantomData<S>,
}

impl<S: Sample> Waveshaper<S> {
    /// Creates a shaper with no drive and no output gain.
    pub fn new(curve: ShaperCurve) -> Self {
        Self {
            curve,
            drive: 1.0,
            output: 1.0,
            marker: PhantomData,
        }
    }

    /// A hard clipper.
    pub fn hard_clip() -> Self {
        Self::new(ShaperCurve::HardClip)
    }

    /// Sets the gain applied before the curve, in decibels. More drive
    /// pushes more of the signal into the bend.
    pub fn set_drive_db(&mut self, db: f64) {
        self.drive = 10f64.powf(db / 20.0);
    }

    /// Sets the gain applied after the curve, in decibels.
    pub fn set_output_db(&mut self, db: f64) {
        self.output = 10f64.powf(db / 20.0);
    }

    /// Changes the curve.
    pub fn set_curve(&mut self, curve: ShaperCurve) {
        self.curve = curve;
    }

    /// Builder form of [`set_drive_db`](Self::set_drive_db).
    #[must_use]
    pub fn with_drive_db(mut self, db: f64) -> Self {
        self.set_drive_db(db);
        self
    }

    /// Builder form of [`set_output_db`](Self::set_output_db).
    #[must_use]
    pub fn with_output_db(mut self, db: f64) -> Self {
        self.set_output_db(db);
        self
    }

    /// The plain gain applied after the curve.
    pub(crate) fn output_gain(&self) -> f64 {
        self.output
    }

    /// Shapes one value.
    #[inline]
    pub fn shape(&self, sample: S) -> S {
        S::from_f64(self.curve.apply(sample.to_f64() * self.drive) * self.output)
    }
}

impl<S: Sample> Processor<S> for Waveshaper<S> {
    fn prepare(&mut self, _spec: &ProcessSpec) {}

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        for channel in buffer.iter_channels_mut() {
            for sample in channel {
                *sample = self.shape(*sample);
            }
        }
    }
}

/// Removes a constant offset from a signal.
///
/// Asymmetric distortion and some synthesis methods leave the waveform
/// sitting above or below zero. That offset wastes headroom and makes
/// compressors and meters misbehave. This is a first-order high-pass with
/// its corner at about 5 Hz, far below anything audible.
#[derive(Debug, Clone)]
pub struct DcBlocker<S: Sample = f32> {
    pole: f64,
    /// Previous input and output of each channel.
    state: Vec<[f64; 2]>,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for DcBlocker<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> DcBlocker<S> {
    /// Creates a blocker.
    pub fn new() -> Self {
        Self {
            pole: 0.999,
            state: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<S: Sample> Processor<S> for DcBlocker<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.pole = (-core::f64::consts::TAU * 5.0 / spec.sample_rate).exp();
        self.state = vec![[0.0; 2]; spec.num_channels];
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let channels = buffer.num_channels().min(self.state.len());
        for channel in 0..channels {
            let [previous_in, previous_out] = &mut self.state[channel];
            for sample in buffer.channel_mut(channel) {
                let input = sample.to_f64();
                let output = input - *previous_in + self.pole * *previous_out;
                *previous_in = input;
                *previous_out = output;
                *sample = S::from_f64(output);
            }
        }
    }

    fn reset(&mut self) {
        self.state.fill([0.0; 2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ShaperCurve; 4] = [
        ShaperCurve::HardClip,
        ShaperCurve::Tanh,
        ShaperCurve::SoftClip,
        ShaperCurve::Fold,
    ];

    #[test]
    fn curves_are_odd_bounded_and_transparent_near_zero() {
        for curve in ALL {
            for step in 0..=2_000 {
                let x = f64::from(step) / 100.0;
                let y = curve.apply(x);
                assert!(y.abs() <= 1.0 + 1e-12, "{curve:?} at {x} gave {y}");
                assert!(
                    (curve.apply(-x) + y).abs() < 1e-12,
                    "{curve:?} is not odd at {x}"
                );
            }
            // Unity slope at the origin: quiet signals are left alone.
            assert!((curve.apply(1e-4) / 1e-4 - 1.0).abs() < 1e-6, "{curve:?}");
        }
    }

    #[test]
    fn curves_have_their_characteristic_shapes() {
        assert_eq!(ShaperCurve::HardClip.apply(0.7), 0.7);
        assert_eq!(ShaperCurve::HardClip.apply(3.0), 1.0);
        // The soft clipper reaches full scale smoothly at 1.5 and holds it.
        assert!((ShaperCurve::SoftClip.apply(1.5) - 1.0).abs() < 1e-12);
        assert!((ShaperCurve::SoftClip.apply(9.0) - 1.0).abs() < 1e-12);
        // The folder turns back at full scale: 1.25 comes out as 0.75.
        assert!((ShaperCurve::Fold.apply(1.25) - 0.75).abs() < 1e-12);
        assert!((ShaperCurve::Fold.apply(3.0) + 1.0).abs() < 1e-12);
        assert!(ShaperCurve::Tanh.apply(10.0) < 1.0);
    }

    #[test]
    fn drive_and_output_gains_apply_around_the_curve() {
        let shaper = Waveshaper::<f64>::hard_clip()
            .with_drive_db(20.0)
            .with_output_db(-6.0);
        assert!((shaper.shape(0.01) - 0.1 * 0.501_187).abs() < 1e-6);
        assert!((shaper.shape(0.5) - 0.501_187).abs() < 1e-6);

        let mut buffer = AudioBuffer::<f64>::new(2, 4);
        buffer.channel_mut(1).fill(-0.5);
        let mut processor = shaper;
        processor.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[0.0; 4]);
        assert!((buffer.channel(1)[0] + 0.501_187).abs() < 1e-6);
    }

    #[test]
    fn dc_blocker_removes_an_offset_and_leaves_audio_alone() {
        let mut blocker = DcBlocker::<f64>::new();
        blocker.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 48_000,
            num_channels: 1,
        });

        // A constant settles to nothing.
        let mut offset = AudioBuffer::<f64>::new(1, 48_000);
        offset.channel_mut(0).fill(0.5);
        blocker.process(&mut offset);
        assert!(offset.channel(0)[47_999].abs() < 1e-6);

        // A 100 Hz sine riding on an offset keeps its size and loses the
        // offset.
        blocker.reset();
        let mut signal = AudioBuffer::<f64>::new(1, 48_000);
        for (n, sample) in signal.channel_mut(0).iter_mut().enumerate() {
            *sample = 0.3 + 0.5 * (core::f64::consts::TAU * 100.0 * n as f64 / 48_000.0).sin();
        }
        blocker.process(&mut signal);
        let tail = &signal.channel(0)[24_000..];
        let mean = tail.iter().sum::<f64>() / tail.len() as f64;
        let peak = tail.iter().fold(0.0f64, |peak, s| peak.max(s.abs()));
        assert!(mean.abs() < 1e-3, "offset left: {mean}");
        assert!((peak - 0.5).abs() < 0.002, "sine changed to {peak}");
    }
}
