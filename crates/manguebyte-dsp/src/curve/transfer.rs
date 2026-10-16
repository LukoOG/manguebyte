//! Static transfer curves: what level or value comes out for what goes in.

use manguebyte_core::Sample;

use super::{Axis, Curve, CurveKind, CurveSpec, Scale, Unit, gain_to_db, level_transfer_spec};
use crate::{ShaperCurve, Waveshaper, dynamics};

/// The static curve of a [`Compressor`](crate::Compressor): the level a
/// steady input settles at, in decibels, against its level.
///
/// It follows the compressor's own steps for a steady input of that peak
/// level: the level as the detector reads it (anything at or below
/// -120 dB counts as silence), the gain reduction of the soft or hard knee
/// for that level, and the makeup gain.
///
/// ```
/// use manguebyte_dsp::curve::{CompressorTransfer, Curve};
///
/// // 4:1 above -20 dB: 0 dB in, 20 dB over, comes out 5 dB over.
/// let transfer = CompressorTransfer::new(-20.0, 4.0, 0.0, 0.0);
/// assert!((transfer.value_at(0.0) + 15.0).abs() < 1e-9);
/// assert!((transfer.value_at(-30.0) + 30.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressorTransfer {
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    makeup_db: f64,
}

impl CompressorTransfer {
    /// The curve of a compressor with these settings, limited as the
    /// compressor's setters limit them: a ratio below 1 (or not a number)
    /// is 1, a negative knee is a hard one.
    pub fn new(threshold_db: f64, ratio: f64, knee_db: f64, makeup_db: f64) -> Self {
        Self {
            threshold_db,
            ratio: ratio.max(1.0),
            knee_db: knee_db.max(0.0),
            makeup_db,
        }
    }

    /// The gain reduction a steady input whose level reads `level_db` on
    /// the detector settles at, as a positive number of decibels: what
    /// [`Compressor::static_reduction_db`](crate::Compressor::static_reduction_db)
    /// gives.
    pub fn reduction_db(&self, level_db: f64) -> f64 {
        dynamics::compressor_reduction_db(self.threshold_db, self.ratio, self.knee_db, level_db)
    }

    /// The threshold, in decibels.
    pub fn threshold_db(&self) -> f64 {
        self.threshold_db
    }
}

impl Curve for CompressorTransfer {
    fn spec(&self) -> CurveSpec {
        let lowest = self.threshold_db - 0.5 * self.knee_db;
        level_transfer_spec(lowest, self.value_at(6.0))
    }

    fn value_at(&self, input_db: f64) -> f64 {
        let level = dynamics::to_gain(input_db);
        let reduction = self.reduction_db(dynamics::to_db(level));
        gain_to_db(level * dynamics::to_gain(-reduction) * dynamics::to_gain(self.makeup_db))
    }
}

/// The static curve of an [`Expander`](crate::Expander): the level a steady
/// input settles at, in decibels, against its level.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, ExpanderTransfer};
///
/// // 2:1 below -40 dB, never more than 30 dB down.
/// let transfer = ExpanderTransfer::new(-40.0, 2.0, 0.0, 30.0);
/// assert!((transfer.value_at(-50.0) + 60.0).abs() < 1e-9);
/// assert!((transfer.value_at(-100.0) + 130.0).abs() < 1e-9);
/// assert!((transfer.value_at(-10.0) + 10.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpanderTransfer {
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    range_db: f64,
}

impl ExpanderTransfer {
    /// The curve of an expander with these settings, limited as the
    /// expander's setters limit them: a ratio below 1 is 1, a negative knee
    /// is a hard one, the range is limited to `0.0..=240.0` dB with
    /// not-a-number taken as zero.
    pub fn new(threshold_db: f64, ratio: f64, knee_db: f64, range_db: f64) -> Self {
        Self {
            threshold_db,
            ratio: ratio.max(1.0),
            knee_db: knee_db.max(0.0),
            range_db: dynamics::limited_range_db(range_db),
        }
    }

    /// The gain reduction a steady input whose level reads `level_db` on
    /// the detector settles at, as a positive number of decibels: what
    /// [`Expander::static_reduction_db`](crate::Expander::static_reduction_db)
    /// gives.
    pub fn reduction_db(&self, level_db: f64) -> f64 {
        dynamics::expander_reduction_db(
            self.threshold_db,
            self.ratio,
            self.knee_db,
            self.range_db,
            level_db,
        )
    }

    /// The threshold, in decibels.
    pub fn threshold_db(&self) -> f64 {
        self.threshold_db
    }
}

impl Curve for ExpanderTransfer {
    fn spec(&self) -> CurveSpec {
        // Where the reduction reaches the range.
        let full = if self.ratio > 1.0 {
            self.range_db / (self.ratio - 1.0)
        } else {
            0.0
        };
        let lowest = self.threshold_db - 0.5 * self.knee_db - full;
        level_transfer_spec(lowest, self.value_at(6.0))
    }

    fn value_at(&self, input_db: f64) -> f64 {
        let level = dynamics::to_gain(input_db);
        let reduction = self.reduction_db(dynamics::to_db(level));
        gain_to_db(level * dynamics::to_gain(-reduction))
    }
}

/// The static curve of a [`NoiseGate`](crate::NoiseGate): a steady input
/// at or above the threshold comes out as it went in, one below it at the
/// level of the closed gate.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, GateTransfer, MIN_DB};
///
/// let transfer = GateTransfer::new(-40.0, f64::NEG_INFINITY);
/// assert!((transfer.value_at(-30.0) + 30.0).abs() < 1e-9);
/// assert_eq!(transfer.value_at(-50.0), MIN_DB);
/// // A gate that only turns the noise down by 20 dB.
/// let transfer = GateTransfer::new(-40.0, -20.0);
/// assert!((transfer.value_at(-50.0) + 70.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateTransfer {
    threshold_db: f64,
    floor_db: f64,
}

impl GateTransfer {
    /// The curve of a gate with these settings. A floor at or below
    /// -120 dB closes the gate to silence, as the gate does.
    pub fn new(threshold_db: f64, floor_db: f64) -> Self {
        Self {
            threshold_db,
            floor_db,
        }
    }

    /// Whether a steady input at `input_db` holds the gate open.
    pub fn is_open_at(&self, input_db: f64) -> bool {
        dynamics::to_gain(input_db) >= dynamics::to_gain(self.threshold_db)
    }

    /// The threshold, in decibels.
    pub fn threshold_db(&self) -> f64 {
        self.threshold_db
    }
}

impl Curve for GateTransfer {
    fn spec(&self) -> CurveSpec {
        level_transfer_spec(self.threshold_db, 6.0)
    }

    fn value_at(&self, input_db: f64) -> f64 {
        let level = dynamics::to_gain(input_db);
        let gain = if self.is_open_at(input_db) {
            1.0
        } else {
            dynamics::gate_floor_gain(self.floor_db)
        };
        gain_to_db(level * gain)
    }
}

/// The static curve of a [`Limiter`](crate::Limiter): nothing comes out
/// above the ceiling, and what is below it comes out as it went in.
///
/// ```
/// use manguebyte_dsp::curve::{Curve, LimiterTransfer};
///
/// let transfer = LimiterTransfer::new(-1.0);
/// assert!((transfer.value_at(6.0) + 1.0).abs() < 1e-9);
/// assert!((transfer.value_at(-10.0) + 10.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimiterTransfer {
    ceiling_db: f64,
}

impl LimiterTransfer {
    /// The curve of a limiter whose ceiling is `ceiling_db`.
    pub fn new(ceiling_db: f64) -> Self {
        Self { ceiling_db }
    }

    /// The ceiling, in decibels.
    pub fn ceiling_db(&self) -> f64 {
        self.ceiling_db
    }
}

impl Curve for LimiterTransfer {
    fn spec(&self) -> CurveSpec {
        level_transfer_spec(self.ceiling_db, 6.0)
    }

    fn value_at(&self, input_db: f64) -> f64 {
        let peak = dynamics::to_gain(input_db);
        let ceiling = dynamics::to_gain(self.ceiling_db);
        let needed = if peak > ceiling { ceiling / peak } else { 1.0 };
        gain_to_db(peak * needed)
    }
}

/// The spec of a curve of sample values: full scale either side in, and
/// as far as `largest` either side out.
fn value_transfer_spec(largest: f64) -> CurveSpec {
    let largest = if largest > 0.0 && largest.is_finite() {
        largest
    } else {
        1.0
    };
    CurveSpec {
        kind: CurveKind::ValueTransfer,
        x: Axis::new(Unit::Value, Scale::Linear, -1.0, 1.0),
        y: Axis::new(Unit::Value, Scale::Linear, -largest, largest),
    }
}

/// The shaper's output for each input sample value, exactly as
/// [`Waveshaper::shape`] computes it in the precision of `S`.
///
/// ```
/// use manguebyte_dsp::curve::Curve;
/// use manguebyte_dsp::{ShaperCurve, Waveshaper};
///
/// let shaper = Waveshaper::<f32>::new(ShaperCurve::HardClip).with_drive_db(6.0);
/// assert_eq!(shaper.value_at(0.75), 1.0);
/// assert_eq!(shaper.value_at(0.25), f64::from(shaper.shape(0.25)));
/// ```
impl<S: Sample> Curve for Waveshaper<S> {
    /// Sample values from -1 to 1 in, against as much either side as the
    /// output gain lets out.
    fn spec(&self) -> CurveSpec {
        value_transfer_spec(self.output_gain())
    }

    fn value_at(&self, input: f64) -> f64 {
        let output = self.shape(S::from_f64(input)).to_f64();
        if output.is_finite() { output } else { 0.0 }
    }
}

/// The curve itself, without drive or output gain.
impl Curve for ShaperCurve {
    /// Sample values from -1 to 1 in and out.
    fn spec(&self) -> CurveSpec {
        value_transfer_spec(1.0)
    }

    fn value_at(&self, input: f64) -> f64 {
        let output = self.apply(input);
        if output.is_finite() { output } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use manguebyte_core::AudioBuffer;

    use super::*;
    use crate::curve::{MIN_DB, linear_spaced};
    use crate::testing::run_in_blocks;
    use crate::{Compressor, Expander, Limiter, NoiseGate, ProcessSpec, Processor};

    const SPEC: ProcessSpec = ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 512,
        num_channels: 2,
    };

    /// Input levels from -100 to +6 dB, every 0.5 dB.
    fn levels() -> Vec<f64> {
        let mut levels = vec![0.0; 213];
        linear_spaced(-100.0, 6.0, &mut levels);
        levels
    }

    /// The level a steady input at `input_db` (a constant, of either sign
    /// on the two channels) comes out at once `processor` has settled, in
    /// decibels.
    fn settled_db<P: Processor<f64>>(processor: &mut P, input_db: f64) -> f64 {
        processor.prepare(&SPEC);
        let level = dynamics::to_gain(input_db);
        let mut steady = AudioBuffer::<f64>::new(2, 24_000);
        steady.channel_mut(0).fill(level);
        steady.channel_mut(1).fill(-level);
        run_in_blocks(processor, &mut steady, 512);
        let last = steady.channel(0)[23_999];
        assert_eq!(steady.channel(1)[23_999], -last);
        gain_to_db(last.abs())
    }

    /// The largest difference between `curve` and what `make` measures,
    /// over [`levels`].
    fn worst_error<P: Processor<f64>>(curve: &impl Curve, mut make: impl FnMut() -> P) -> f64 {
        levels().into_iter().fold(0.0, |worst: f64, input_db| {
            let measured = settled_db(&mut make(), input_db);
            worst.max((measured - curve.value_at(input_db)).abs())
        })
    }

    #[test]
    fn a_compressor_settles_on_its_curve() {
        for (threshold, ratio, knee, makeup) in [
            (-20.0, 4.0, 0.0, 0.0),
            (-30.0, 2.0, 12.0, 6.0),
            (-6.0, f64::INFINITY, 6.0, -3.0),
            (-50.0, 1.5, 40.0, 10.0),
        ] {
            let make = || {
                Compressor::<f64>::new()
                    .with_threshold_db(threshold)
                    .with_ratio(ratio)
                    .with_knee_db(knee)
                    .with_makeup_db(makeup)
                    .with_attack_ms(1.0)
                    .with_release_ms(5.0)
            };
            let curve = make().transfer();
            assert_eq!(
                curve,
                CompressorTransfer::new(threshold, ratio, knee, makeup)
            );
            let worst = worst_error(&curve, make);
            assert!(worst < 1e-9, "{threshold} dB at {ratio}:1: {worst}");
            // The reduction is the compressor's own.
            for level in [-60.0, threshold, -1.0] {
                assert_eq!(curve.reduction_db(level), make().static_reduction_db(level));
            }
        }
    }

    #[test]
    fn an_expander_settles_on_its_curve() {
        for (threshold, ratio, knee, range) in [
            (-40.0, 2.0, 0.0, 30.0),
            (-30.0, 4.0, 10.0, 80.0),
            (-20.0, f64::INFINITY, 0.0, 60.0),
        ] {
            let make = || {
                Expander::<f64>::new()
                    .with_threshold_db(threshold)
                    .with_ratio(ratio)
                    .with_knee_db(knee)
                    .with_range_db(range)
                    .with_attack_ms(1.0)
                    .with_release_ms(5.0)
            };
            let curve = make().transfer();
            assert_eq!(curve, ExpanderTransfer::new(threshold, ratio, knee, range));
            let worst = worst_error(&curve, make);
            assert!(worst < 1e-9, "{threshold} dB at {ratio}:1: {worst}");
        }
    }

    #[test]
    fn a_gate_settles_on_its_curve() {
        for (threshold, floor) in [(-40.0, f64::NEG_INFINITY), (-60.0, -20.0), (-12.0, -6.0)] {
            let make = || {
                NoiseGate::<f64>::new()
                    .with_threshold_db(threshold)
                    .with_floor_db(floor)
                    .with_attack_ms(1.0)
                    .with_hold_ms(0.0)
                    .with_release_ms(5.0)
            };
            let curve = make().transfer();
            assert_eq!(curve, GateTransfer::new(threshold, floor));
            let worst = worst_error(&curve, make);
            assert!(worst < 1e-9, "{threshold} dB: {worst}");
        }
        // A gate that closes to silence gives the floor of the curves.
        assert_eq!(GateTransfer::new(-40.0, -200.0).value_at(-41.0), MIN_DB);
    }

    #[test]
    fn a_limiter_settles_on_its_curve() {
        for ceiling in [-0.3, -6.0, -24.0] {
            let make = || Limiter::<f64>::new().with_ceiling_db(ceiling);
            let curve = make().transfer();
            assert_eq!(curve, LimiterTransfer::new(ceiling));
            let worst = worst_error(&curve, make);
            assert!(worst < 1e-9, "{ceiling} dB: {worst}");
        }
    }

    #[test]
    fn a_shaper_curve_is_what_the_shaper_puts_out() {
        let curves = [
            ShaperCurve::HardClip,
            ShaperCurve::Tanh,
            ShaperCurve::SoftClip,
            ShaperCurve::Fold,
        ];
        let mut inputs = vec![0.0; 401];
        linear_spaced(-2.0, 2.0, &mut inputs);
        for curve in curves {
            for (drive, output) in [(0.0, 0.0), (12.0, -3.0), (-6.0, 6.0)] {
                let shaper = Waveshaper::<f32>::new(curve)
                    .with_drive_db(drive)
                    .with_output_db(output);
                let mut processor = shaper;
                let mut buffer = AudioBuffer::<f32>::new(1, inputs.len());
                for (sample, &input) in buffer.channel_mut(0).iter_mut().zip(&inputs) {
                    *sample = input as f32;
                }
                processor.process(&mut buffer);
                for (&out, &input) in buffer.channel(0).iter().zip(&inputs) {
                    assert_eq!(
                        shaper.value_at(input),
                        f64::from(out),
                        "{curve:?} at {input}"
                    );
                }
                let spec = shaper.spec();
                assert_eq!(spec.kind, CurveKind::ValueTransfer);
                assert!((spec.y.max - 10f64.powf(output / 20.0)).abs() < 1e-12);
            }
            for &input in &inputs {
                assert_eq!(curve.value_at(input), curve.apply(input));
            }
        }
        // Garbage in gives a number out.
        assert_eq!(ShaperCurve::Fold.value_at(f64::INFINITY), 0.0);
        assert_eq!(Waveshaper::<f64>::hard_clip().value_at(f64::NAN), 0.0);
    }

    #[test]
    fn the_axes_show_where_the_curves_bend() {
        let spec = CompressorTransfer::new(-18.0, 4.0, 6.0, 0.0).spec();
        assert_eq!(spec.kind, CurveKind::LevelTransfer);
        assert_eq!((spec.x.min, spec.x.max), (-60.0, 6.0));
        assert_eq!(spec.x.unit, Unit::Decibels);
        // Makeup raises the top of the output axis to show it: +6 dB in,
        // 24 dB over, comes out 8 dB down and 12 dB up.
        let spec = CompressorTransfer::new(-18.0, 1.5, 0.0, 12.0).spec();
        assert_eq!(spec.y.max, 12.0);
        // An expander that reaches its range far below its threshold.
        let spec = ExpanderTransfer::new(-50.0, 2.0, 0.0, 40.0).spec();
        assert_eq!(spec.x.min, -102.0);
        let spec = GateTransfer::new(-70.0, f64::NEG_INFINITY).spec();
        assert_eq!(spec.x.min, -84.0);
        let spec = LimiterTransfer::new(f64::NAN).spec();
        assert_eq!(spec.x.min, -60.0);
        // Everything is a number, whatever the settings.
        for curve in [
            &CompressorTransfer::new(f64::NAN, f64::NAN, f64::NAN, f64::NAN) as &dyn Curve,
            &ExpanderTransfer::new(f64::NAN, f64::NAN, f64::NAN, f64::NAN),
            &GateTransfer::new(f64::NAN, f64::NAN),
            &LimiterTransfer::new(f64::INFINITY),
        ] {
            for input in [f64::NEG_INFINITY, -50.0, 0.0, f64::INFINITY, f64::NAN] {
                assert!(curve.value_at(input).is_finite());
            }
        }
    }
}
