//! An enclosed division's swell shutters.

use core::f64::consts::{LN_10, PI};
use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use super::HIGHEST_CUTOFF_RATIO;
use crate::numeric::limit;
use crate::{ProcessSpec, Processor};

/// The lowest cutoff a swell box runs at, in hertz.
const LOWEST_HZ: f64 = 20.0;

/// How many samples' gains a moving swell box works out at a time.
const PIECE: usize = 64;

/// How a swell box's opening sets its level and its cutoff.
///
/// Both laws are even per unit of pedal travel: the level in decibels is
/// a straight line from [`closed_db`](Self::closed_db) at 0 to 0 dB at 1,
/// and the cutoff a straight line in octaves from
/// [`closed_cutoff_hz`](Self::closed_cutoff_hz) to
/// [`open_cutoff_hz`](Self::open_cutoff_hz):
///
/// ```text
/// level(o)  = closed_db · (1 − o)                      decibels
/// cutoff(o) = closed_cutoff · (open_cutoff / closed_cutoff)^o
/// ```
///
/// Real shutters change the sound most near closed, since the open area
/// grows fastest there; a law even in decibels gives the player the same
/// change for the same travel everywhere instead, which is what a pedal
/// with 128 steps of MIDI needs. Halfway open is 15 dB down and, with the
/// defaults, the cutoff is about 4.5 kHz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwellLaw {
    /// The level when closed, in decibels, held to -200 to 0; -30 unless
    /// set.
    pub closed_db: f64,
    /// The cutoff when closed, in hertz; 1 kHz unless set.
    pub closed_cutoff_hz: f64,
    /// The cutoff when open, in hertz; 20 kHz unless set. It is held to
    /// 0.49 of the sample rate, where the one-pole takes 0.06 dB at 20 kHz
    /// and 0.1 dB at 8 kHz of 48: the box is never quite transparent.
    pub open_cutoff_hz: f64,
}

impl Default for SwellLaw {
    fn default() -> Self {
        Self {
            closed_db: -30.0,
            closed_cutoff_hz: 1_000.0,
            open_cutoff_hz: 20_000.0,
        }
    }
}

impl SwellLaw {
    /// The level at `opening` (0 closed, 1 open), in decibels.
    pub fn level_db(&self, opening: f64) -> f64 {
        limit(self.closed_db, -200.0, 0.0) * (1.0 - limit(opening, 0.0, 1.0))
    }

    /// The linear gain at `opening`.
    pub fn gain(&self, opening: f64) -> f64 {
        self.curve().gain(opening)
    }

    /// The cutoff at `opening`, in hertz, before the limits of the sample
    /// rate.
    pub fn cutoff_hz(&self, opening: f64) -> f64 {
        self.curve().cutoff_hz(opening)
    }

    /// The law in the form the audio thread works it out in.
    fn curve(&self) -> Curve {
        let closed = self.closed_cutoff_hz.max(LOWEST_HZ);
        let open = self.open_cutoff_hz.max(LOWEST_HZ);
        Curve {
            closed_nepers: limit(self.closed_db, -200.0, 0.0) * (LN_10 / 20.0),
            log_closed: closed.ln(),
            log_span: (open / closed).ln(),
        }
    }
}

/// A [`SwellLaw`] as two exponentials: `gain = e^(closed_nepers·(1 − o))`
/// and `cutoff = e^(log_closed + log_span·o)`.
#[derive(Debug, Clone, Copy)]
struct Curve {
    closed_nepers: f64,
    log_closed: f64,
    log_span: f64,
}

impl Curve {
    fn gain(&self, opening: f64) -> f64 {
        (self.closed_nepers * (1.0 - limit(opening, 0.0, 1.0))).exp()
    }

    fn cutoff_hz(&self, opening: f64) -> f64 {
        (self.log_closed + self.log_span * limit(opening, 0.0, 1.0)).exp()
    }
}

/// Everything that moves while a block is processed, stepped once a sample
/// for all the channels.
#[derive(Debug, Clone, Copy)]
struct Motion {
    opening: Smoothed<f64>,
    /// The linear gain where the opening is.
    gain: f64,
    /// `g / (1 + g)` for the low-pass's `g = tan(π·fc/fs)`.
    lowpass: f64,
    law: SwellLaw,
    curve: Curve,
    sample_rate: f64,
}

impl Motion {
    /// Works out the gain and the filter where the opening is now.
    fn update(&mut self, opening: f64) {
        self.gain = self.curve.gain(opening);
        let highest = HIGHEST_CUTOFF_RATIO * self.sample_rate;
        let cutoff = limit(self.curve.cutoff_hz(opening), LOWEST_HZ, highest);
        let g = (PI * cutoff / self.sample_rate).tan();
        self.lowpass = g / (1.0 + g);
    }

    /// Moves on by one sample and returns the gain and the filter's gain.
    #[inline]
    fn step(&mut self) -> (f64, f64) {
        if self.opening.is_smoothing() {
            let opening = self.opening.next_value();
            self.update(opening);
        }
        (self.gain, self.lowpass)
    }

    /// Jumps to where the opening is heading.
    fn snap(&mut self) {
        let target = self.opening.target();
        self.opening.set_immediate(target);
        self.update(target);
    }
}

/// The swell box of an enclosed division: its shutters, opened from 0
/// (closed) to 1 (open) by the swell pedal, set a level and a first-order
/// low-pass cutoff by a [`SwellLaw`], quieter and darker as they close.
///
/// The opening glides linearly to a new setting over
/// [`DEFAULT_RAMP_SECONDS`](Self::DEFAULT_RAMP_SECONDS) unless set, and
/// the level and the cutoff follow it sample by sample, so moving the
/// pedal never steps. The low-pass is the topology-preserving one-pole of
/// [`OnePole`](crate::OnePole), whose memory stays meaningful while its
/// cutoff moves; the gain comes after it. Any number of channels, each
/// filtered alike.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::space::SwellBox;
/// use manguebyte_dsp::{ProcessSpec, Processor};
///
/// let mut swell = SwellBox::<f32>::default();
/// swell.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 2 });
/// swell.set_opening(0.0);
/// swell.reset();
///
/// // Closed, a steady signal comes out 30 dB down.
/// let mut block = AudioBuffer::<f32>::new(2, 4_800);
/// for channel in block.iter_channels_mut() {
///     channel.fill(1.0);
/// }
/// swell.process(&mut block);
/// assert!((block.channel(1)[4_799] - 0.0316).abs() < 1e-4);
/// ```
#[derive(Debug, Clone)]
pub struct SwellBox<S: Sample = f32> {
    motion: Motion,
    ramp_seconds: f64,
    /// The low-pass's memory, per channel.
    state: Vec<f64>,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for SwellBox<S> {
    fn default() -> Self {
        Self::new(SwellLaw::default())
    }
}

impl<S: Sample> SwellBox<S> {
    /// Default glide time for a change of opening.
    pub const DEFAULT_RAMP_SECONDS: f64 = 0.05;

    /// A swell box with `law`, open. Until
    /// [`prepare`](Processor::prepare) is called it assumes 44.1 kHz.
    pub fn new(law: SwellLaw) -> Self {
        let mut motion = Motion {
            opening: Smoothed::new(1.0),
            gain: 1.0,
            lowpass: 0.0,
            law,
            curve: law.curve(),
            sample_rate: 44_100.0,
        };
        motion.snap();
        Self {
            motion,
            ramp_seconds: Self::DEFAULT_RAMP_SECONDS,
            state: Vec::new(),
            marker: PhantomData,
        }
    }

    /// Sets how long a change of opening takes. Applies from the next
    /// [`prepare`](Processor::prepare).
    pub fn with_ramp(mut self, seconds: f64) -> Self {
        self.ramp_seconds = seconds;
        self
    }

    /// Glides to `opening`, from 0 (closed) to 1 (open); values outside are
    /// held to the range, and not-a-number closes the box. Real-time safe.
    pub fn set_opening(&mut self, opening: f64) {
        let opening = limit(opening, 0.0, 1.0);
        if self.motion.opening.target() != opening {
            self.motion.opening.set_target(opening);
            if !self.motion.opening.is_smoothing() {
                self.motion.update(opening);
            }
        }
    }

    /// The opening being approached.
    pub fn opening(&self) -> f64 {
        self.motion.opening.target()
    }

    /// The law.
    pub fn law(&self) -> SwellLaw {
        self.motion.law
    }

    /// Changes the law; the level and the cutoff jump to it. Real-time
    /// safe, but meant for setting up, not for playing.
    pub fn set_law(&mut self, law: SwellLaw) {
        self.motion.law = law;
        self.motion.curve = law.curve();
        let opening = self.motion.opening.current();
        self.motion.update(opening);
    }
}

impl<S: Sample> Processor<S> for SwellBox<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.motion.sample_rate = spec.sample_rate;
        self.motion
            .opening
            .set_ramp(spec.sample_rate, self.ramp_seconds);
        self.motion.snap();
        self.state = vec![0.0; spec.num_channels];
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let frames = buffer.num_frames();
        let mut start = 0;
        while start < frames {
            // While the opening moves, its gains are worked out once for
            // every channel, a piece at a time.
            let len = if self.motion.opening.is_smoothing() {
                (frames - start).min(PIECE)
            } else {
                frames - start
            };
            let mut gains = [(0.0, 0.0); PIECE];
            let steady = !self.motion.opening.is_smoothing();
            if !steady {
                for gain in &mut gains[..len] {
                    *gain = self.motion.step();
                }
            }
            let held = (self.motion.gain, self.motion.lowpass);
            for (samples, state) in buffer.iter_channels_mut().zip(&mut self.state) {
                let mut memory = *state;
                for (k, sample) in samples[start..start + len].iter_mut().enumerate() {
                    let (gain, lowpass) = if steady { held } else { gains[k] };
                    let x = sample.to_f64();
                    let v = (x - memory) * lowpass;
                    let low = v + memory;
                    memory = low + v;
                    *sample = S::from_f64(gain * low);
                }
                *state = memory;
            }
            start += len;
        }
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.motion.snap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_runs_from_closed_to_open() {
        let law = SwellLaw::default();
        assert_eq!(law.level_db(0.0), -30.0);
        assert_eq!(law.level_db(0.5), -15.0);
        assert_eq!(law.level_db(1.0), 0.0);
        assert_eq!(law.level_db(2.0), 0.0);
        assert!((law.cutoff_hz(0.0) - 1_000.0).abs() < 1e-9);
        assert!((law.cutoff_hz(0.5) - (20_000.0f64 * 1_000.0).sqrt()).abs() < 1e-6);
        assert!((law.cutoff_hz(1.0) - 20_000.0).abs() < 1e-6);
    }

    #[test]
    fn a_new_opening_glides_and_does_not_restart_when_set_again() {
        let mut swell = SwellBox::<f64>::default().with_ramp(0.01);
        swell.prepare(&ProcessSpec {
            sample_rate: 1_000.0,
            max_block_size: 16,
            num_channels: 1,
        });
        swell.set_opening(0.0);
        let mut block = AudioBuffer::<f64>::new(1, 5);
        block.channel_mut(0).fill(1.0);
        swell.process(&mut block);
        // Halfway through a ten-sample glide, and the same again when the
        // opening is set to where it already heads.
        swell.set_opening(0.0);
        swell.process(&mut block);
        assert!(!swell.motion.opening.is_smoothing());
        assert_eq!(swell.opening(), 0.0);
    }
}
