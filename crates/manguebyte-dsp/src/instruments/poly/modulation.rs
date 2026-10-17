//! The poly synth's envelopes and LFOs.

use super::super::common::Noise;
use super::params::{EnvParams, LfoParams};
use crate::LfoShape;

/// How strongly a curve of 1 bends a segment: the exponent of the
/// exponential it follows.
const CURVE: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A segment's shape: how far along it the level is, `(1 - e^(-a·x)) /
/// (1 - e^(-a))` at a share `x` of its time, a straight line for `a` of
/// zero. The exponential is kept as a running product, one multiplication
/// a sample.
#[derive(Debug, Clone, Copy)]
struct Curve {
    /// `e^(-a·x)` so far, and its factor per sample.
    product: f64,
    factor: f64,
    /// `1 / (1 - e^(-a))`, or 0 for a straight line.
    scale: f64,
}

impl Curve {
    const LINE: Self = Self {
        product: 1.0,
        factor: 1.0,
        scale: 0.0,
    };

    fn new(curve: f64, step: f64) -> Self {
        let a = CURVE * curve;
        if a.abs() < 1e-6 {
            Self::LINE
        } else {
            Self {
                product: 1.0,
                factor: (-a * step).exp(),
                scale: 1.0 / (1.0 - (-a).exp()),
            }
        }
    }

    /// Keeps the place `x` along the segment but takes a new curve and
    /// step.
    fn restep(&mut self, curve: f64, step: f64, x: f64) {
        *self = Self::new(curve, step);
        if self.scale != 0.0 {
            self.product = (-CURVE * curve * x).exp();
        }
    }

    /// The shape at `x`, the share of the segment done, having moved on.
    #[inline]
    fn at(&mut self, x: f64) -> f64 {
        if self.scale == 0.0 {
            x
        } else {
            self.product *= self.factor;
            ((1.0 - self.product) * self.scale).clamp(0.0, 1.0)
        }
    }
}

/// The shortest times an envelope's segments may take, in seconds: the
/// attack and decay, and the release; and the time below which a curve is
/// straightened, in proportion, so that a short curved segment does not
/// start more steeply than the floor allows.
#[derive(Debug, Clone, Copy)]
pub(super) struct Floor {
    pub(super) rise: f64,
    pub(super) release: f64,
    pub(super) tame: f64,
}

impl Floor {
    /// No floor: a segment of no time is over at once.
    pub(super) const NONE: Self = Self {
        rise: 0.0,
        release: 0.0,
        tame: 0.0,
    };

    /// The curve a segment of `seconds` takes.
    fn tamed(self, curve: f64, seconds: f64) -> f64 {
        if seconds < self.tame {
            curve * seconds / self.tame
        } else {
            curve
        }
    }
}

/// An envelope with curved segments. See [`EnvParams`].
#[derive(Debug, Clone, Copy)]
pub(super) struct Envelope {
    stage: Stage,
    level: f64,
    /// The level the segment started from.
    from: f64,
    /// The share of the segment done, and per sample.
    x: f64,
    step: f64,
    /// The time the step was worked out for.
    seconds: f64,
    curve: Curve,
    /// The sustain level heard: it glides to the setting, so a changed
    /// setting does not jump.
    sustain: f64,
    /// The share of the way the sustain follows its setting in a sample:
    /// about 10 ms.
    follow: f64,
}

impl Envelope {
    pub(super) const IDLE: Self = Self {
        stage: Stage::Idle,
        level: 0.0,
        from: 0.0,
        x: 0.0,
        step: 1.0,
        seconds: 0.0,
        curve: Curve::LINE,
        sustain: 0.0,
        follow: 1.0,
    };

    /// Whether it is anything but silent.
    pub(super) fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    fn begin(&mut self, stage: Stage, seconds: f64, curve: f64, rate: f64, floor: Floor) {
        self.stage = stage;
        self.from = self.level;
        self.x = 0.0;
        self.seconds = seconds;
        self.step = step_for(seconds, rate);
        self.curve = Curve::new(floor.tamed(curve, seconds), self.step);
    }

    /// Starts the attack from where the level is, taking the share of the
    /// attack time that is left to climb: a note struck again rises from
    /// where it was.
    pub(super) fn strike(&mut self, params: &EnvParams, rate: f64, floor: Floor) {
        self.sustain = params.sustain;
        self.follow = 1.0 - (-100.0 / rate).exp();
        if self.stage == Stage::Idle {
            self.level = 0.0;
        }
        let seconds = (params.attack * (1.0 - self.level)).max(floor.rise);
        self.begin(Stage::Attack, seconds, params.attack_curve, rate, floor);
    }

    /// Starts the release from where the level is.
    pub(super) fn release(&mut self, params: &EnvParams, rate: f64, floor: Floor) {
        if self.stage != Stage::Idle && self.stage != Stage::Release {
            let seconds = params.release.max(floor.release);
            self.begin(Stage::Release, seconds, params.release_curve, rate, floor);
        }
    }

    /// The next level, 0 to 1.
    #[inline]
    pub(super) fn next(&mut self, params: &EnvParams, rate: f64, floor: Floor) -> f64 {
        // The sustain follows its setting over about 10 ms.
        self.sustain += (params.sustain - self.sustain) * self.follow;
        let (seconds, curve) = match self.stage {
            Stage::Idle => return 0.0,
            Stage::Sustain => {
                self.level = self.sustain;
                return self.level;
            }
            Stage::Attack => (
                (params.attack * (1.0 - self.from)).max(floor.rise),
                params.attack_curve,
            ),
            Stage::Decay => (params.decay.max(floor.rise), params.decay_curve),
            Stage::Release => (params.release.max(floor.release), params.release_curve),
        };
        if seconds != self.seconds {
            // The setting changed during the segment: carry on from the
            // same place at the new pace.
            self.seconds = seconds;
            self.step = step_for(seconds, rate);
            self.curve
                .restep(floor.tamed(curve, seconds), self.step, self.x);
        }
        self.x += self.step;
        let done = self.x >= 1.0 - 1e-9;
        let shape = if done { 1.0 } else { self.curve.at(self.x) };
        match self.stage {
            Stage::Attack => {
                self.level = self.from + (1.0 - self.from) * shape;
                if done {
                    self.level = 1.0;
                    let seconds = params.decay.max(floor.rise);
                    self.begin(Stage::Decay, seconds, params.decay_curve, rate, floor);
                    self.from = 1.0;
                }
            }
            Stage::Decay => {
                self.level = 1.0 + (self.sustain - 1.0) * shape;
                if done {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Release => {
                self.level = self.from * (1.0 - shape);
                if done {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
            Stage::Idle | Stage::Sustain => {}
        }
        self.level
    }
}

/// The share of a segment `seconds` long that passes in a sample: all of
/// it for a segment shorter than a sample.
#[inline]
fn step_for(seconds: f64, rate: f64) -> f64 {
    let samples = seconds * rate;
    if samples > 1.0 { 1.0 / samples } else { 1.0 }
}

/// A low-frequency oscillator. See [`LfoParams`].
#[derive(Debug, Clone, Copy)]
pub(super) struct Lfo {
    /// Where in the cycle it is, 0 up to 1.
    phase: f64,
    /// The level a sample-and-hold holds.
    held: f64,
    /// The output, rounded where the shape steps.
    smoothed: f64,
    /// How far the fade-in has come, 0 to 1.
    fade: f64,
    random: Noise,
    /// The share of the way the rounding of a step goes in a sample.
    rounding: f64,
}

impl Lfo {
    pub(super) fn new() -> Self {
        Self {
            phase: 0.0,
            held: 0.0,
            smoothed: 0.0,
            fade: 1.0,
            random: Noise::for_note(0, 0.0, 300),
            rounding: 1.0,
        }
    }

    /// Starts a note: from `params.phase` if it retriggers or the
    /// oscillator is new (`fresh`), and from no depth if it fades in.
    pub(super) fn strike(&mut self, params: &LfoParams, fresh: bool, random: Noise, rate: f64) {
        if params.retrigger || fresh {
            self.phase = params.phase;
            self.random = random;
            self.held = self.random.next();
            self.smoothed = self.value(params.shape);
        }
        self.fade = if params.fade_in > 0.0 { 0.0 } else { 1.0 };
        self.rounding = 1.0 - (-1_000.0 / rate).exp();
    }

    #[inline]
    fn value(&self, shape: LfoShape) -> f64 {
        match shape {
            LfoShape::SampleAndHold => self.held,
            shape => shape.value_at(self.phase),
        }
    }

    /// The next value, -1 to 1, at `hertz`, for a sample `rate`.
    #[inline]
    pub(super) fn next(&mut self, params: &LfoParams, hertz: f64, rate: f64) -> f64 {
        let raw = self.value(params.shape);
        let steps = matches!(
            params.shape,
            LfoShape::Square | LfoShape::SawUp | LfoShape::SawDown | LfoShape::SampleAndHold
        );
        self.smoothed = if steps {
            // A millisecond's rounding.
            self.smoothed + (raw - self.smoothed) * self.rounding
        } else {
            raw
        };
        let out = self.smoothed * self.fade;
        if self.fade < 1.0 {
            self.fade = (self.fade + 1.0 / (params.fade_in * rate).max(1.0)).min(1.0);
        }
        let moved = self.phase + (hertz / rate).clamp(0.0, 0.5);
        if moved >= 1.0 {
            self.held = self.random.next();
        }
        self.phase = moved - moved.floor();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn run(envelope: &mut Envelope, params: &EnvParams, samples: usize) -> Vec<f64> {
        (0..samples)
            .map(|_| envelope.next(params, RATE, Floor::NONE))
            .collect()
    }

    #[test]
    fn every_segment_takes_its_time_whatever_its_curve() {
        for curve in [-1.0, -0.4, 0.0, 0.5, 1.0] {
            let params = EnvParams {
                attack: 0.1,
                decay: 0.2,
                sustain: 0.4,
                release: 0.3,
                attack_curve: curve,
                decay_curve: curve,
                release_curve: curve,
            };
            let mut envelope = Envelope::IDLE;
            envelope.strike(&params, RATE, Floor::NONE);
            let attack = run(&mut envelope, &params, 4_800);
            // At the peak exactly at the end of the attack, not before.
            assert_eq!(attack[4_799], 1.0, "{curve}");
            assert!(attack[4_798] < 1.0, "{curve}");
            assert!(attack.windows(2).all(|w| w[1] >= w[0]), "{curve}");
            let decay = run(&mut envelope, &params, 9_600);
            assert_eq!(decay[9_599], 0.4, "{curve}");
            assert!(decay[9_598] > 0.4, "{curve}");
            let held = run(&mut envelope, &params, 1_000);
            assert!(held.iter().all(|&level| level == 0.4));
            envelope.release(&params, RATE, Floor::NONE);
            let release = run(&mut envelope, &params, 14_400);
            assert_eq!(release[14_399], 0.0, "{curve}");
            assert!(release[14_398] > 0.0, "{curve}");
            assert!(!envelope.is_active());
            // Halfway through, the curve says where the level is: above
            // the line for a positive curve, on it for none.
            let middle = attack[2_399];
            if curve > 0.0 {
                assert!(middle > 0.55, "{curve}: {middle}");
            } else if curve < 0.0 {
                assert!(middle < 0.45, "{curve}: {middle}");
            } else {
                assert!((middle - 0.5).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn a_strike_again_rises_from_where_the_level_is() {
        let params = EnvParams::adsr(0.1, 0.1, 0.5, 0.2);
        let mut envelope = Envelope::IDLE;
        envelope.strike(&params, RATE, Floor::NONE);
        run(&mut envelope, &params, 20_000);
        envelope.release(&params, RATE, Floor::NONE);
        let falling = run(&mut envelope, &params, 2_000);
        let level = *falling.last().unwrap();
        envelope.strike(&params, RATE, Floor::NONE);
        let rising = run(&mut envelope, &params, 10);
        assert!(
            (rising[0] - level).abs() < 0.01,
            "{level} then {}",
            rising[0]
        );
        // The rest of the climb takes its share of the attack.
        let left = (0.1 * (1.0 - level) * RATE).ceil() as usize;
        let more = run(&mut envelope, &params, left);
        assert_eq!(more[left - 11], 1.0);
    }

    #[test]
    fn the_lfo_runs_at_its_rate_and_fades_in() {
        let params = LfoParams {
            fade_in: 0.5,
            ..LfoParams::new(LfoShape::Sine, 4.0)
        };
        let mut lfo = Lfo::new();
        lfo.strike(&params, true, Noise::for_note(60, 1.0, 1), RATE);
        // A second and a little: the cycles start at 0, 0.25, 0.5, 0.75
        // and 1 s.
        let values: Vec<f64> = (0..RATE as usize + 100)
            .map(|_| lfo.next(&params, 4.0, RATE))
            .collect();
        let rising = values
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert_eq!(rising, 4);
        // Faded in after half a second, not before: a tenth of the depth
        // after a twentieth of a second.
        let early = values[..2_400].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let late = values[24_000..].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(early <= 0.1 && late > 0.999, "{early} {late}");
        // A sample-and-hold draws a new level every cycle, repeatably.
        let params = LfoParams::new(LfoShape::SampleAndHold, 10.0);
        let draw = || {
            let mut lfo = Lfo::new();
            lfo.strike(&params, true, Noise::for_note(60, 0.5, 2), RATE);
            (0..48_000)
                .map(|_| lfo.next(&params, 10.0, RATE))
                .collect::<Vec<_>>()
        };
        let a = draw();
        assert_eq!(a, draw());
        let distinct = (0..10)
            .map(|cycle| (a[cycle * 4_800 + 4_000] * 1e6) as i64)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), 10);
    }
}
