use crate::FloatParam;

/// How a [`ParamSmoother`] moves towards a new value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Smoothing {
    /// Jump straight to the new value.
    None,
    /// Move in a straight line, arriving after this many milliseconds. Good
    /// for gains and mixes.
    Linear(f64),
    /// Close a fixed fraction of the remaining distance each sample, getting
    /// within 0.1% after this many milliseconds. Starts fast and settles
    /// gently, which suits filter cutoffs and anything heard as pitch.
    Exponential(f64),
}

/// The audio thread's click-free view of a [`FloatParam`].
///
/// A parameter can change at any moment and by any amount. Applying such a
/// jump directly to audio produces a click, so the audio thread keeps one of
/// these per parameter and reads the value through it.
///
/// ```
/// use manguebyte_params::{FloatParam, FloatRange, ParamSmoother, Smoothing};
///
/// let gain = FloatParam::new("gain", "Gain", FloatRange::linear(0.0, 1.0), 1.0);
/// let mut smoother = ParamSmoother::new(Smoothing::Linear(10.0));
/// smoother.prepare(48_000.0, gain.value());
///
/// gain.set(0.0); // the user moves a slider
///
/// // In the audio callback, once per block:
/// smoother.follow(&gain);
/// let mut block = [1.0f32; 480];
/// for sample in &mut block {
///     *sample *= smoother.next_value();
/// }
/// assert!(block[0] > 0.99); // no jump at the start
/// assert_eq!(block[479], 0.0); // there after 10 ms
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSmoother {
    style: Smoothing,
    sample_rate: f64,
    // The state is kept in double precision. In single precision an
    // exponential ramp stalls short of its target once the remaining step
    // drops below the rounding error of the value itself.
    current: f64,
    target: f64,
    /// Linear: amount added per sample.
    step: f64,
    /// Linear: samples left until arrival.
    remaining: u32,
    /// Exponential: fraction of the distance closed per sample.
    coefficient: f64,
}

/// Fraction of the distance an exponential ramp has left when it is
/// considered to have arrived.
const SETTLED: f64 = 1e-3;

impl ParamSmoother {
    /// Creates a smoother. Call [`prepare`](Self::prepare) before using it.
    pub fn new(style: Smoothing) -> Self {
        Self {
            style,
            sample_rate: 0.0,
            current: 0.0,
            target: 0.0,
            step: 0.0,
            remaining: 0,
            coefficient: 1.0,
        }
    }

    /// Sets the sample rate and jumps to `value`.
    pub fn prepare(&mut self, sample_rate: f64, value: f32) {
        self.sample_rate = sample_rate;
        self.coefficient = match self.style {
            Smoothing::Exponential(milliseconds) => {
                let samples = milliseconds * 1e-3 * sample_rate;
                if samples >= 1.0 {
                    1.0 - SETTLED.powf(1.0 / samples)
                } else {
                    1.0
                }
            }
            _ => 1.0,
        };
        self.reset(value);
    }

    /// Jumps straight to `value`.
    pub fn reset(&mut self, value: f32) {
        self.current = f64::from(value);
        self.target = f64::from(value);
        self.step = 0.0;
        self.remaining = 0;
    }

    /// Starts moving towards the parameter's current value. Call it once per
    /// block. Real-time safe.
    #[inline]
    pub fn follow(&mut self, param: &FloatParam) {
        self.set_target(param.value());
    }

    /// Starts moving towards `target`. Real-time safe.
    pub fn set_target(&mut self, target: f32) {
        let target = f64::from(target);
        if target == self.target || target.is_nan() {
            return;
        }
        self.target = target;
        match self.style {
            Smoothing::None => self.current = target,
            Smoothing::Linear(milliseconds) => {
                let samples = (milliseconds * 1e-3 * self.sample_rate).round();
                if samples >= 1.0 {
                    self.remaining = samples as u32;
                    self.step = (target - self.current) / samples;
                } else {
                    self.current = target;
                    self.remaining = 0;
                }
            }
            Smoothing::Exponential(_) => {
                if self.coefficient >= 1.0 {
                    self.current = target;
                }
            }
        }
    }

    /// Advances one sample and returns the value to use for it. Real-time
    /// safe.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        match self.style {
            Smoothing::None => {}
            Smoothing::Linear(_) => {
                if self.remaining > 0 {
                    self.remaining -= 1;
                    self.current = if self.remaining == 0 {
                        self.target
                    } else {
                        self.current + self.step
                    };
                }
            }
            Smoothing::Exponential(_) => {
                if self.current != self.target {
                    let next = self.current + (self.target - self.current) * self.coefficient;
                    let tolerance = 1e-7 * self.target.abs().max(1e-3);
                    let arrived = (self.target - next).abs() <= tolerance || next == self.current;
                    self.current = if arrived { self.target } else { next };
                }
            }
        }
        self.current as f32
    }

    /// Fills `out` with one value per sample. Real-time safe.
    pub fn next_block(&mut self, out: &mut [f32]) {
        if self.is_smoothing() {
            for value in out {
                *value = self.next_value();
            }
        } else {
            out.fill(self.current as f32);
        }
    }

    /// The value most recently returned.
    pub fn current(&self) -> f32 {
        self.current as f32
    }

    /// The value being moved towards.
    pub fn target(&self) -> f32 {
        self.target as f32
    }

    /// Whether the value is still moving. When it is not, a processor can
    /// take a cheaper path that treats the parameter as constant.
    pub fn is_smoothing(&self) -> bool {
        self.current != self.target
    }
}

/// Tells the audio thread when a value has changed since it last looked.
///
/// Useful for work that is too expensive to repeat every block, such as
/// redesigning a filter:
///
/// ```
/// use manguebyte_params::Changed;
///
/// let mut cutoff = Changed::new();
/// assert_eq!(cutoff.update(440.0), Some(440.0)); // first look always reports
/// assert_eq!(cutoff.update(440.0), None);
/// assert_eq!(cutoff.update(880.0), Some(880.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Changed<T> {
    last: Option<T>,
}

impl<T: Copy + PartialEq> Changed<T> {
    /// Creates a detector that has not seen any value yet.
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// Returns `value` if it differs from the one seen last time.
    #[inline]
    pub fn update(&mut self, value: T) -> Option<T> {
        if self.last == Some(value) {
            None
        } else {
            self.last = Some(value);
            Some(value)
        }
    }

    /// Forgets the last value, so the next [`update`](Self::update) reports.
    pub fn invalidate(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_jumps() {
        let mut smoother = ParamSmoother::new(Smoothing::None);
        smoother.prepare(48_000.0, 1.0);
        smoother.set_target(5.0);
        assert!(!smoother.is_smoothing());
        assert_eq!(smoother.next_value(), 5.0);
    }

    #[test]
    fn linear_arrives_exactly_on_time_in_equal_steps() {
        let mut smoother = ParamSmoother::new(Smoothing::Linear(1.0));
        smoother.prepare(8_000.0, 0.0);
        smoother.set_target(1.0);

        let mut values = [0.0f32; 10];
        smoother.next_block(&mut values);
        for (index, value) in values.iter().enumerate().take(7) {
            assert!((value - (index + 1) as f32 / 8.0).abs() < 1e-6);
        }
        assert_eq!(values[7], 1.0);
        assert_eq!(values[9], 1.0);
        assert!(!smoother.is_smoothing());
    }

    #[test]
    fn linear_retargets_from_where_it_is() {
        let mut smoother = ParamSmoother::new(Smoothing::Linear(1.0));
        smoother.prepare(8_000.0, 0.0);
        smoother.set_target(1.0);
        for _ in 0..4 {
            smoother.next_value();
        }
        smoother.set_target(0.0);
        let next = smoother.next_value();
        assert!(next < 0.5 && next > 0.4, "jumped to {next}");
    }

    #[test]
    fn exponential_settles_in_the_stated_time_without_overshoot() {
        let mut smoother = ParamSmoother::new(Smoothing::Exponential(10.0));
        smoother.prepare(48_000.0, 0.0);
        smoother.set_target(1.0);

        let mut previous = 0.0;
        for _ in 0..480 {
            let value = smoother.next_value();
            assert!(value >= previous && value <= 1.0);
            previous = value;
        }
        assert!((previous - 0.999).abs() < 1e-3, "after 10 ms: {previous}");

        // It then lands exactly, so `is_smoothing` can turn off.
        for _ in 0..4_800 {
            smoother.next_value();
        }
        assert_eq!(smoother.current(), 1.0);
        assert!(!smoother.is_smoothing());
    }

    #[test]
    fn follows_a_parameter_and_ignores_nan() {
        let param = FloatParam::new("x", "X", crate::FloatRange::linear(0.0, 10.0), 2.0);
        let mut smoother = ParamSmoother::new(Smoothing::Linear(0.0));
        smoother.prepare(48_000.0, param.value());
        param.set(7.0);
        smoother.follow(&param);
        assert_eq!(smoother.next_value(), 7.0);
        smoother.set_target(f32::NAN);
        assert_eq!(smoother.next_value(), 7.0);
    }

    #[test]
    fn a_short_time_at_a_low_rate_degrades_to_a_jump() {
        for style in [Smoothing::Linear(0.01), Smoothing::Exponential(0.01)] {
            let mut smoother = ParamSmoother::new(style);
            smoother.prepare(8_000.0, 0.0);
            smoother.set_target(1.0);
            assert_eq!(smoother.next_value(), 1.0, "{style:?}");
        }
    }

    #[test]
    fn a_long_exponential_ramp_does_not_stall_short_of_its_target() {
        let mut smoother = ParamSmoother::new(Smoothing::Exponential(2_000.0));
        smoother.prepare(96_000.0, 0.0);
        smoother.set_target(1.0);
        for _ in 0..96_000 * 8 {
            smoother.next_value();
        }
        assert_eq!(smoother.current(), 1.0);
        assert!(!smoother.is_smoothing());
    }

    #[test]
    fn changed_reports_only_differences() {
        let mut changed = Changed::new();
        assert_eq!(changed.update(1), Some(1));
        assert_eq!(changed.update(1), None);
        changed.invalidate();
        assert_eq!(changed.update(1), Some(1));
    }
}
