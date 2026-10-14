//! A cutoff frequency that glides instead of jumping.

use core::f64::consts::{PI, TAU};

use manguebyte_core::Smoothed;

use crate::numeric::{glide_to, limit, step_towards};

/// The cutoff of a filter built from trapezoidal integrators, kept as the
/// integrator gain `g = tan(π·f/fs)` that such a filter needs.
///
/// The frequency moves on a musical scale: a glide covers equal intervals
/// in equal times, so a sweep sounds even. It is `Copy`, so that every
/// channel of a block can run a copy of its own and see exactly the same
/// values; the copy of the last channel becomes the new state.
///
/// A glide can also be held to a pace: no more than a given fraction of an
/// octave in one period of the frequency it has reached, and no more than
/// a given number of octaves per second (see
/// [`with_pace_limit`](Self::with_pace_limit)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct GlidingCutoff {
    /// The frequency that was asked for, in hertz, before any limit.
    requested: f64,
    /// Lowest frequency, in hertz.
    lowest: f64,
    /// Highest frequency, as a fraction of the sample rate.
    highest_ratio: f64,
    /// How long a change takes, in seconds.
    glide_seconds: f64,
    sample_rate: f64,
    /// Base-two logarithm of the frequency in hertz. Under a pace limit it
    /// holds where the glide is heading, and `now` where it is.
    pitch: Smoothed<f64>,
    /// `tan(π·f/fs)` where the glide is now.
    g: f64,
    /// The most a glide moves in one period of the frequency it is at, in
    /// octaves; infinite for no limit.
    per_period: f64,
    /// The most a glide moves in one second, in octaves, under a pace
    /// limit.
    per_second: f64,
    /// Under a pace limit: the pitch now, the most it moves in one sample
    /// (which makes a glide that the limit does not hold back take
    /// `glide_seconds`), and how many samples a glide takes, zero until
    /// [`prepare`](Self::prepare) is called.
    now: f64,
    pace: f64,
    ramp_samples: f64,
}

impl GlidingCutoff {
    /// Creates a cutoff at `hertz`, limited to `lowest` hertz at one end and
    /// to `highest_ratio` of the sample rate at the other. Until
    /// [`prepare`](Self::prepare) is called it assumes 44.1 kHz and changes
    /// at once.
    pub(crate) fn new(hertz: f64, lowest: f64, highest_ratio: f64, glide_seconds: f64) -> Self {
        let mut cutoff = Self {
            requested: hertz,
            lowest,
            highest_ratio,
            glide_seconds,
            sample_rate: 44_100.0,
            pitch: Smoothed::new(0.0),
            g: 0.0,
            per_period: f64::INFINITY,
            per_second: f64::INFINITY,
            now: 0.0,
            pace: 0.0,
            ramp_samples: 0.0,
        };
        cutoff.snap();
        cutoff
    }

    /// The same cutoff, with its glides held to at most `per_period`
    /// octaves in one period of the frequency they have reached and to at
    /// most `per_second` octaves per second, as well as taking at least
    /// `glide_seconds`.
    ///
    /// A filter whose frequency moves is not quite the filter at any one
    /// frequency, and how far it strays depends on how far the frequency
    /// moves while the filter rings, that is, per period. A glide over a
    /// fixed time moves a low frequency by many octaves per period; the
    /// first limit holds every part of a glide to the same pace instead,
    /// so that what it does is about the same at every frequency. The
    /// period is that of the frequency as the integrators see it,
    /// pre-warped: the most a sample moves is `per_period·sin(2π·f/fs) /
    /// 2π`, which is `per_period·f/fs` well below half the sample rate and
    /// less near it. Close to half the sample rate that is not enough by
    /// itself (there the integrators' gain changes several times as fast
    /// as the frequency), and the second limit holds a long glide from
    /// there back.
    pub(crate) fn with_pace_limit(mut self, per_period: f64, per_second: f64) -> Self {
        self.per_period = per_period;
        self.per_second = per_second;
        self.snap();
        self
    }

    /// Whether glides are held to a pace.
    fn paced(&self) -> bool {
        self.per_period.is_finite()
    }

    /// Takes a new sample rate and jumps to the frequency last asked for.
    pub(crate) fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.pitch.set_ramp(sample_rate, self.glide_seconds);
        self.ramp_samples = (sample_rate * self.glide_seconds).max(0.0).round();
        self.snap();
    }

    /// Starts gliding towards `hertz`.
    pub(crate) fn set(&mut self, hertz: f64) {
        self.requested = hertz;
        let pitch = self.limited().log2();
        if self.paced() {
            if pitch == self.pitch.target() {
                return;
            }
            self.pitch.set_immediate(pitch);
            if self.ramp_samples >= 1.0 {
                let fastest = self.per_second / self.sample_rate;
                self.pace = ((pitch - self.now).abs() / self.ramp_samples).min(fastest);
            } else {
                self.now = pitch;
                self.g = self.gain_at(pitch);
            }
            return;
        }
        glide_to(&mut self.pitch, pitch);
        if !self.pitch.is_smoothing() {
            self.g = self.gain_at(self.pitch.current());
        }
    }

    /// Jumps to the frequency last asked for.
    pub(crate) fn snap(&mut self) {
        self.pitch.set_immediate(self.limited().log2());
        self.now = self.pitch.current();
        self.g = self.gain_at(self.now);
    }

    /// The frequency the cutoff is heading for, in hertz, after the limits.
    pub(crate) fn limited(&self) -> f64 {
        let highest = self.highest_ratio * self.sample_rate;
        limit(self.requested, self.lowest.min(highest), highest)
    }

    /// Whether the cutoff is still moving.
    #[inline]
    pub(crate) fn is_gliding(&self) -> bool {
        if self.paced() {
            self.now != self.pitch.target()
        } else {
            self.pitch.is_smoothing()
        }
    }

    /// The integrator gain where the glide is now.
    #[inline]
    pub(crate) fn gain(&self) -> f64 {
        self.g
    }

    /// Moves on by one sample and returns the integrator gain for it.
    #[inline]
    pub(crate) fn next_gain(&mut self) -> f64 {
        if self.paced() {
            let target = self.pitch.target();
            if self.now != target {
                let most = self.per_period * (TAU * self.now.exp2() / self.sample_rate).sin() / TAU;
                self.now = step_towards(self.now, target, self.pace.min(most));
                self.g = self.gain_at(self.now);
            }
        } else if self.pitch.is_smoothing() {
            let pitch = self.pitch.next_value();
            self.g = self.gain_at(pitch);
        }
        self.g
    }

    fn gain_at(&self, pitch: f64) -> f64 {
        (PI * pitch.exp2() / self.sample_rate).tan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_limited_at_both_ends_and_survives_garbage() {
        for octaves in [f64::INFINITY, 0.1] {
            let mut cutoff =
                GlidingCutoff::new(1_000.0, 5.0, 0.45, 0.01).with_pace_limit(octaves, octaves);
            cutoff.prepare(48_000.0);
            assert_eq!(cutoff.limited(), 1_000.0);
            for (asked, expected) in [
                (0.0, 5.0),
                (-3.0, 5.0),
                (f64::NAN, 5.0),
                (f64::NEG_INFINITY, 5.0),
                (f64::INFINITY, 21_600.0),
                (1e9, 21_600.0),
            ] {
                cutoff.set(asked);
                assert_eq!(cutoff.limited(), expected, "asked for {asked}");
                cutoff.snap();
                assert!(cutoff.gain() > 0.0 && cutoff.gain().is_finite());
            }
            // A sample rate too low for the lower limit moves that limit down.
            cutoff.prepare(8.0);
            cutoff.set(1_000.0);
            assert_eq!(cutoff.limited(), 3.6);
        }
    }

    #[test]
    fn it_glides_by_equal_intervals_and_arrives_exactly() {
        let mut cutoff = GlidingCutoff::new(100.0, 5.0, 0.49, 0.01);
        cutoff.prepare(48_000.0);
        let start = cutoff.gain();
        assert!((start - (PI * 100.0 / 48_000.0).tan()).abs() < 1e-15);

        // 10 ms at 48 kHz is 480 samples for the four octaves to 1600 Hz.
        cutoff.set(1_600.0);
        assert!(cutoff.is_gliding());
        assert_eq!(cutoff.gain(), start, "nothing moves before the first step");
        let mut gains = Vec::new();
        while cutoff.is_gliding() {
            gains.push(cutoff.next_gain());
        }
        assert_eq!(gains.len(), 480);
        // Half-way through the time is half-way through the interval: 400 Hz.
        let midway = (PI * 400.0 / 48_000.0).tan();
        assert!((gains[239] - midway).abs() < 1e-9 * midway);
        // The end of the glide is the very value a jump gives.
        let mut jumped = GlidingCutoff::new(1_600.0, 5.0, 0.49, 0.01);
        jumped.prepare(48_000.0);
        assert_eq!(gains[479], jumped.gain());
        assert_eq!(cutoff.next_gain(), jumped.gain());

        // Asking for the same frequency again does not start the glide again.
        cutoff.set(100.0);
        cutoff.next_gain();
        let before = cutoff.gain();
        cutoff.set(100.0);
        assert!(cutoff.next_gain() < before);
    }

    /// The pitches a paced glide from `from` to `to` hertz goes through, at
    /// 48 kHz, 10 ms at least, a tenth of an octave per period at most and
    /// `per_second` octaves per second at most.
    fn paced_glide(from: f64, to: f64, per_second: f64) -> Vec<f64> {
        let mut cutoff = GlidingCutoff::new(from, 1.0, 0.49, 0.01).with_pace_limit(0.1, per_second);
        cutoff.prepare(48_000.0);
        cutoff.set(to);
        let mut pitches = vec![cutoff.now];
        while cutoff.is_gliding() {
            cutoff.next_gain();
            pitches.push(cutoff.now);
            assert!(pitches.len() < 1_000_000, "it never arrives");
        }
        assert_eq!(cutoff.gain(), cutoff.gain_at(to.log2()));
        pitches
    }

    #[test]
    fn a_paced_glide_never_moves_faster_than_its_limit_nor_its_time() {
        for (from, to) in [
            (20_000.0, 20.0),
            (20.0, 20_000.0),
            (1_000.0, 100.0),
            (900.0, 1_000.0),
        ] {
            let pitches = paced_glide(from, to, f64::INFINITY);
            let octaves = (to / from).log2().abs();
            for pair in pitches.windows(2) {
                // (Room for the rounding of a pitch near 14 to its last
                // digit, 1e-9 of the smallest step here.)
                let step = (pair[1] - pair[0]).abs();
                // Never faster than the 10 ms glide...
                assert!(step <= octaves / 480.0 * (1.0 + 1e-9), "{from} to {to}");
                // ...nor than a tenth of an octave per (pre-warped) period.
                let at = (TAU * pair[0].exp2() / 48_000.0).sin() / TAU;
                assert!(
                    step <= 0.1 * at * (1.0 + 1e-9),
                    "{from} to {to} at {}",
                    pair[0]
                );
            }
            // At least the 10 ms; where the limit does not bite, no longer.
            assert!(pitches.len() > 480, "{from} to {to}: {}", pitches.len());
            if (from, to) == (900.0, 1_000.0) {
                assert!(pitches.len() <= 482, "{}", pitches.len());
            }
        }
        // A tenth of an octave per period is 0.1·f octaves per second well
        // below half the sample rate, so a glide from `high` down to `low`
        // takes 10·(1/low - 1/high) / ln 2 seconds, a little more where the
        // pre-warping slows it near the top: from 1 kHz to 100 Hz 0.1298 s,
        // over the whole range 0.7206 s and a little more.
        let seconds =
            |from: f64, to: f64| paced_glide(from, to, f64::INFINITY).len() as f64 / 48_000.0;
        let expected = |low: f64, high: f64| 10.0 * (1.0 / low - 1.0 / high) / 2f64.ln();
        let time = seconds(1_000.0, 100.0);
        assert!(
            (time / expected(100.0, 1_000.0) - 1.0).abs() < 2e-3,
            "{time} s"
        );
        for (from, to) in [(20_000.0, 20.0), (20.0, 20_000.0)] {
            let time = seconds(from, to);
            assert!(time > expected(20.0, 20_000.0) && time < 0.723, "{time} s");
        }
        // At most 50 octaves per second: the top octaves of that glide
        // slow down to it, and nothing else does.
        let pitches = paced_glide(20_000.0, 20.0, 50.0);
        for pair in pitches.windows(2) {
            assert!((pair[1] - pair[0]).abs() <= 50.0 / 48_000.0 * (1.0 + 1e-9));
        }
        let slower = pitches.len() as f64 / 48_000.0;
        assert!(slower > 0.75 && slower < 0.81, "{slower} s");
        // From 1 kHz, the octave down to 500 Hz goes at 50 octaves per
        // second, 20 ms, and the rest at a tenth of an octave per period.
        let time = paced_glide(1_000.0, 100.0, 50.0).len() as f64 / 48_000.0;
        let expected = 0.02 + expected(100.0, 500.0);
        assert!(
            (time / expected - 1.0).abs() < 2e-3,
            "{time} s, not {expected}"
        );
    }

    #[test]
    fn before_it_is_prepared_a_change_is_immediate() {
        for octaves in [f64::INFINITY, 0.1] {
            let mut cutoff =
                GlidingCutoff::new(100.0, 5.0, 0.49, 0.01).with_pace_limit(octaves, octaves);
            cutoff.set(2_000.0);
            assert!(!cutoff.is_gliding());
            let expected = (PI * 2_000f64.log2().exp2() / 44_100.0).tan();
            assert_eq!(cutoff.gain(), expected);
        }
    }
}
