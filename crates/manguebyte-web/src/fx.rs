//! The effects of a part's chain, each behind a [`Switch`] that fades it in
//! and out.
//!
//! Every effect works one frame at a time, so its output does not depend
//! on how the frames are cut into blocks, and none of them allocates once
//! made. The signal processing comes from `manguebyte-dsp` wherever it has
//! it: the filters ([`StateVariableFilter`], [`Biquad`] designed by
//! [`BiquadCoefficients`]), [`Compressor`], [`Chorus`] (which is the flanger
//! too), [`Phaser`], [`ShaperCurve`], [`Lfo`] and [`DelayLine`]. What is
//! here is the glue: smoothing every setting so that nothing clicks, the
//! bitcrusher, the gain stages of the tremolo and the auto-pan, and the
//! delay's two read heads, ping-pong routing and damping.

use core::f64::consts::{FRAC_PI_4, SQRT_2, TAU};

use manguebyte_core::{AudioBuffer, Sample};
#[cfg(doc)]
use manguebyte_dsp::MixLaw;
use manguebyte_dsp::{
    Biquad, BiquadCoefficients, Chorus, Compressor, DelayLine, FilterShape, Lfo, LfoShape, Phaser,
    ProcessSpec, Processor, ShaperCurve, StateVariableFilter, SvfMode,
};
use manguebyte_params::{Param, ParamId, ParamSmoother, Smoothing};

use crate::effect::Effect;
use crate::params::{self, ChainParams, FILTER_SMOOTHING, LEVEL_SMOOTHING};

/// How long an effect takes to fade in when it is switched on, and out
/// when it is switched off: a straight cross-fade between the signal that
/// goes into it and the one that comes out, over
/// `round(EFFECT_FADE_SECONDS × sample rate)` frames. The delay and the
/// reverb fade their input instead, so that their echoes and tails ring on.
pub const EFFECT_FADE_SECONDS: f64 = 0.01;

/// The longest delay time, seconds, set or synced.
pub const DELAY_MAX_SECONDS: f64 = 4.0;

/// How long the delay takes to move from one delay time to another: it
/// cross-fades between a read head at the old time and one at the new, so
/// that a change of time or tempo neither clicks nor bends the pitch of
/// the echoes already sounding.
pub const DELAY_GLIDE_SECONDS: f64 = 0.02;

/// How long the tremolo's gain and the auto-pan's position take to follow
/// their wave: the time constant of a one-pole smoother, which turns the
/// steps of the square and saw waves into ramps of a few milliseconds.
pub const WAVE_SMOOTHING_SECONDS: f64 = 0.001;

/// Below this, -120 dB, a delay's echoes count as silent.
const QUIET: f32 = 1e-6;

/// How often the equaliser recalculates its filters while a setting
/// glides, in frames: on the frames that are a multiple of this since it
/// was switched on, so that the result does not depend on the blocks.
const EQ_GRID: u32 = 32;

/// The Q of the equaliser's shelves: the steepest slope without a bump.
const SHELF_Q: f64 = core::f64::consts::FRAC_1_SQRT_2;

/// Where the beat is: what tempo-synced effects follow.
///
/// The beat of a frame is worked out from a whole number of frames since
/// the tempo or the beat was last set, never added up frame by frame or
/// block by block, so that it is the same number however the frames are
/// cut into blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Clock {
    /// The beat when the tempo or the beat was last set.
    pub(crate) origin: f64,
    /// Frames from then to the first frame of the block.
    pub(crate) since: u64,
    pub(crate) beats_per_frame: f64,
}

impl Clock {
    /// 120 beats per minute at `sample_rate`, from beat 0.
    pub(crate) fn new(sample_rate: f64) -> Self {
        Self {
            origin: 0.0,
            since: 0,
            beats_per_frame: 2.0 / sample_rate,
        }
    }

    /// The beat at frame `frame` of the block.
    #[inline]
    pub(crate) fn beat_at(&self, frame: usize) -> f64 {
        self.origin + (self.since + frame as u64) as f64 * self.beats_per_frame
    }
}

pub(crate) fn resonance_to_q(resonance: f64) -> f64 {
    core::f64::consts::FRAC_1_SQRT_2 * 16f64.powf(resonance.clamp(0.0, 1.0))
}

pub(crate) fn gain_from_db(db: f64) -> f32 {
    if db <= f64::from(params::GAIN_FLOOR_DB) {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// Left and right gains for `pan`: constant power, normalised so that the
/// centre leaves both sides untouched. It is [`MixLaw::EqualPower`] scaled
/// by √2, worked out from one angle, as the part's pan always has been.
pub(crate) fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (f64::from(pan.clamp(-1.0, 1.0)) + 1.0) * FRAC_PI_4;
    ((SQRT_2 * angle.cos()) as f32, (SQRT_2 * angle.sin()) as f32)
}

fn smoother(style: Smoothing, sample_rate: f64, value: f32) -> ParamSmoother {
    let mut smoother = ParamSmoother::new(style);
    smoother.prepare(sample_rate, value);
    smoother
}

/// Moves a smoother to `value`: at once while the effect is silent, so
/// that it starts with its settings and does not glide into them,
/// smoothly otherwise.
fn set(smoother: &mut ParamSmoother, value: f32, idle: bool) {
    if idle {
        smoother.reset(value);
    } else {
        smoother.set_target(value);
    }
}

/// Whether an effect is in or out, and how far it has faded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Switch {
    on: bool,
    /// Frames into the fade, from 0 (out) to `length` (in).
    position: u32,
    length: u32,
}

impl Switch {
    pub(crate) fn new(sample_rate: f64, on: bool) -> Self {
        let length = ((EFFECT_FADE_SECONDS * sample_rate).round() as u32).max(1);
        Self {
            on,
            position: if on { length } else { 0 },
            length,
        }
    }

    pub(crate) fn set(&mut self, on: bool) {
        self.on = on;
    }

    /// Jumps to the end of the fade.
    pub(crate) fn snap(&mut self) {
        self.position = if self.on { self.length } else { 0 };
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    /// Out, and done fading out.
    pub(crate) fn is_silent(&self) -> bool {
        !self.on && self.position == 0
    }

    /// In, and done fading in.
    pub(crate) fn is_full(&self) -> bool {
        self.on && self.position == self.length
    }

    /// Frames until it falls silent, while it fades out; `None` while it is
    /// in or on its way in.
    fn frames_left(&self) -> Option<usize> {
        (!self.on).then_some(self.position as usize)
    }

    /// Moves one frame on and returns the level of that frame, 0 to 1.
    #[inline]
    pub(crate) fn next(&mut self) -> f32 {
        if self.on {
            if self.position < self.length {
                self.position += 1;
            }
        } else if self.position > 0 {
            self.position -= 1;
        }
        self.position as f32 / self.length as f32
    }
}

/// An effect that works on the signal in place and is cross-faded with it
/// as it is switched.
pub(crate) trait Insert {
    /// Processes `buffer`, two channels, in place.
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, clock: Clock);
    /// Takes a new value of one of its parameters, not its switch. `idle`
    /// while the effect is silent: the value then applies at once.
    fn set_param(&mut self, id: ParamId, value: f64, idle: bool);
    /// Clears every memory and jumps to the values in `params`.
    fn reset(&mut self, params: &ChainParams);
}

/// What the chain does with each of its stages.
pub(crate) trait StageOps {
    /// Runs the stage over `buffer`, if it is not silent. `dry` is scratch
    /// space for a cross-fade.
    fn run(&mut self, buffer: &mut AudioBuffer<f32>, dry: &mut AudioBuffer<f32>, clock: Clock);
    /// Switches the stage in or out. One that was silent starts afresh
    /// from `params`.
    fn set_on(&mut self, on: bool, params: &ChainParams);
    /// Takes a new value of one of its parameters.
    fn set_param(&mut self, id: ParamId, value: f64);
    /// Starts again from `params` after something that is not a number got
    /// in: memory cleared, the switch at the end of its fade.
    fn recover(&mut self, params: &ChainParams);
    /// Whether it has nothing to do: out, faded, and with no tail left.
    fn is_idle(&self) -> bool;
}

/// `dry` and `wet` mixed in proportion `level`, `wet` itself at 1 and
/// `dry` itself at 0, so that a fade or a mix that has arrived gives
/// exactly what the effect without it would.
#[inline]
pub(crate) fn blend(dry: f32, wet: f32, level: f32) -> f32 {
    if level >= 1.0 {
        wet
    } else {
        dry + (wet - dry) * level
    }
}

/// Whether the last frame of `buffer` is silent on both sides.
fn ends_silent(buffer: &AudioBuffer<f32>) -> bool {
    let last = buffer.num_frames().wrapping_sub(1);
    buffer
        .channel(0)
        .get(last)
        .is_none_or(|&sample| sample == 0.0)
        && buffer
            .channel(1)
            .get(last)
            .is_none_or(|&sample| sample == 0.0)
}

/// An [`Insert`] with its switch.
///
/// A fade only matters while sound passes: on a frame after one on which
/// nothing went in and nothing came out, the switch jumps to the end of
/// its fade, as no step can come of it. So an effect switched on together
/// with a note, or in a rest, applies in full from the first frame of the
/// note, and still fades in under a sound already playing.
pub(crate) struct Stage<E> {
    pub(crate) switch: Switch,
    pub(crate) fx: E,
    /// Whether the last frame was silent, in and out.
    quiet: bool,
}

impl<E: Insert> Stage<E> {
    /// A stage that is silent, or fully in if `on`.
    pub(crate) fn new(fx: E, sample_rate: f64, on: bool) -> Self {
        Self {
            switch: Switch::new(sample_rate, on),
            fx,
            quiet: true,
        }
    }
}

impl<E: Insert> StageOps for Stage<E> {
    fn run(&mut self, buffer: &mut AudioBuffer<f32>, dry: &mut AudioBuffer<f32>, clock: Clock) {
        let frames = buffer.num_frames();
        if frames == 0 {
            return;
        }
        if self.quiet && !self.switch.is_silent() && !self.switch.is_full() {
            self.switch.snap();
        }
        if self.switch.is_silent() {
            self.quiet = ends_silent(buffer);
            return;
        }
        if self.switch.is_full() {
            let went_in_silent = ends_silent(buffer);
            self.fx.process(buffer, clock);
            self.quiet = went_in_silent && ends_silent(buffer);
            return;
        }
        // Fading: the effect runs until it falls silent, and its output is
        // cross-faded with what went in. After that the signal goes past.
        let active = self
            .switch
            .frames_left()
            .map_or(frames, |left| left.min(frames));
        dry.set_num_frames(frames);
        dry.copy_from(buffer);
        buffer.windowed(0..active, |piece| self.fx.process(piece, clock));
        let (left, right) = buffer.channel_pair_mut(0, 1);
        let (dry_left, dry_right) = (dry.channel(0), dry.channel(1));
        let mut quiet = self.quiet;
        for frame in 0..active {
            if quiet {
                self.switch.snap();
            }
            let level = self.switch.next();
            let (wet_left, wet_right) = (left[frame], right[frame]);
            quiet = dry_left[frame] == 0.0
                && dry_right[frame] == 0.0
                && wet_left == 0.0
                && wet_right == 0.0;
            left[frame] = blend(dry_left[frame], wet_left, level);
            right[frame] = blend(dry_right[frame], wet_right, level);
        }
        // Past the end of a fade out, what went in comes out.
        self.quiet = if active < frames {
            ends_silent(buffer)
        } else {
            quiet
        };
    }

    fn set_on(&mut self, on: bool, params: &ChainParams) {
        if on && self.switch.is_silent() {
            self.fx.reset(params);
        }
        self.switch.set(on);
    }

    fn set_param(&mut self, id: ParamId, value: f64) {
        self.fx.set_param(id, value, self.switch.is_silent());
    }

    fn recover(&mut self, params: &ChainParams) {
        self.fx.reset(params);
        self.switch.snap();
        self.quiet = true;
    }

    fn is_idle(&self) -> bool {
        self.switch.is_silent()
    }
}

/// A resonant state-variable filter, low-pass or high-pass, whose cutoff
/// and resonance glide.
pub(crate) struct ResonantFilter {
    effect: Effect,
    filter: StateVariableFilter<f64>,
    cutoff: ParamSmoother,
    resonance: ParamSmoother,
    setting: (f32, f32),
}

impl ResonantFilter {
    /// The chain's low-pass ([`Effect::Lowpass`]) or its high-pass.
    pub(crate) fn new(effect: Effect, params: &ChainParams, spec: &ProcessSpec) -> Self {
        let (mode, cutoff, resonance) = Self::settings(effect, params);
        let mut filter = StateVariableFilter::new(
            mode,
            f64::from(cutoff),
            resonance_to_q(f64::from(resonance)),
        );
        Processor::<f64>::prepare(
            &mut filter,
            &ProcessSpec {
                num_channels: 2,
                ..*spec
            },
        );
        Self {
            effect,
            filter,
            cutoff: smoother(FILTER_SMOOTHING, spec.sample_rate, cutoff),
            resonance: smoother(FILTER_SMOOTHING, spec.sample_rate, resonance),
            setting: (cutoff, resonance),
        }
    }

    fn settings(effect: Effect, params: &ChainParams) -> (SvfMode, f32, f32) {
        if effect == Effect::Highpass {
            let p = &params.highpass;
            (SvfMode::Highpass, p.cutoff.value(), p.resonance.value())
        } else {
            let p = &params.lowpass;
            (SvfMode::Lowpass, p.cutoff.value(), p.resonance.value())
        }
    }
}

impl Insert for ResonantFilter {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (left, right) in left.iter_mut().zip(right) {
            let setting = (self.cutoff.next_value(), self.resonance.next_value());
            if setting != self.setting {
                if setting.0 != self.setting.0 {
                    self.filter.set_frequency(f64::from(setting.0));
                }
                if setting.1 != self.setting.1 {
                    self.filter.set_q(resonance_to_q(f64::from(setting.1)));
                }
                self.setting = setting;
            }
            *left = self.filter.process_sample(0, f64::from(*left)) as f32;
            *right = self.filter.process_sample(1, f64::from(*right)) as f32;
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        match id {
            params::LOWPASS_CUTOFF | params::HIGHPASS_CUTOFF => {
                set(&mut self.cutoff, value as f32, idle);
            }
            params::LOWPASS_RESONANCE | params::HIGHPASS_RESONANCE => {
                set(&mut self.resonance, value as f32, idle);
            }
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let (_, cutoff, resonance) = Self::settings(self.effect, params);
        Processor::<f64>::reset(&mut self.filter);
        self.cutoff.reset(cutoff);
        self.resonance.reset(resonance);
        self.filter.set_frequency(f64::from(cutoff));
        self.filter.set_q(resonance_to_q(f64::from(resonance)));
        self.setting = (cutoff, resonance);
    }
}

/// The three-band equaliser: a low shelf, a peak and a high shelf, each a
/// biquad from Robert Bristow-Johnson's Audio EQ Cookbook, run in double
/// precision.
pub(crate) struct Equaliser {
    sample_rate: f64,
    bands: [Biquad<f64>; 3],
    /// Low, mid and high gain in decibels.
    gains: [ParamSmoother; 3],
    /// Low, mid and high frequency in hertz.
    frequencies: [ParamSmoother; 3],
    mid_q: ParamSmoother,
    /// A setting changed since the filters were last designed.
    dirty: bool,
    /// Frames since the last frame of the grid, 0 to [`EQ_GRID`] - 1.
    grid: u32,
    scratch: AudioBuffer<f64>,
}

impl Equaliser {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec) -> Self {
        let rate = spec.sample_rate;
        let p = &params.eq;
        let mut bands = [
            Biquad::new(BiquadCoefficients::IDENTITY),
            Biquad::new(BiquadCoefficients::IDENTITY),
            Biquad::new(BiquadCoefficients::IDENTITY),
        ];
        for band in &mut bands {
            band.prepare(&ProcessSpec {
                num_channels: 2,
                ..*spec
            });
        }
        let mut equaliser = Self {
            sample_rate: rate,
            bands,
            gains: [
                smoother(LEVEL_SMOOTHING, rate, p.low_gain.value()),
                smoother(LEVEL_SMOOTHING, rate, p.mid_gain.value()),
                smoother(LEVEL_SMOOTHING, rate, p.high_gain.value()),
            ],
            frequencies: [
                smoother(FILTER_SMOOTHING, rate, p.low_freq.value()),
                smoother(FILTER_SMOOTHING, rate, p.mid_freq.value()),
                smoother(FILTER_SMOOTHING, rate, p.high_freq.value()),
            ],
            mid_q: smoother(FILTER_SMOOTHING, rate, p.mid_q.value()),
            dirty: true,
            grid: 0,
            scratch: AudioBuffer::new(2, spec.max_block_size),
        };
        equaliser.design();
        equaliser
    }

    /// The coefficients of the three bands at these settings: what the
    /// equaliser runs and what its curve is drawn from.
    pub(crate) fn coefficients(
        sample_rate: f64,
        gains: [f64; 3],
        frequencies: [f64; 3],
        mid_q: f64,
    ) -> [BiquadCoefficients; 3] {
        [
            BiquadCoefficients::design(
                FilterShape::LowShelf { gain_db: gains[0] },
                sample_rate,
                frequencies[0],
                SHELF_Q,
            ),
            BiquadCoefficients::design(
                FilterShape::Peak { gain_db: gains[1] },
                sample_rate,
                frequencies[1],
                mid_q,
            ),
            BiquadCoefficients::design(
                FilterShape::HighShelf { gain_db: gains[2] },
                sample_rate,
                frequencies[2],
                SHELF_Q,
            ),
        ]
    }

    fn moving(&self) -> bool {
        self.dirty
            || self.mid_q.is_smoothing()
            || self
                .gains
                .iter()
                .chain(&self.frequencies)
                .any(ParamSmoother::is_smoothing)
    }

    /// Designs the filters for the settings as they stand.
    fn design(&mut self) {
        let value = |smoother: &ParamSmoother| f64::from(smoother.current());
        let coefficients = Self::coefficients(
            self.sample_rate,
            [
                value(&self.gains[0]),
                value(&self.gains[1]),
                value(&self.gains[2]),
            ],
            [
                value(&self.frequencies[0]),
                value(&self.frequencies[1]),
                value(&self.frequencies[2]),
            ],
            value(&self.mid_q),
        );
        for (band, coefficients) in self.bands.iter_mut().zip(coefficients) {
            band.set_coefficients(coefficients);
        }
        self.dirty = self.mid_q.is_smoothing()
            || self
                .gains
                .iter()
                .chain(&self.frequencies)
                .any(ParamSmoother::is_smoothing);
    }
}

impl Insert for Equaliser {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        let frames = buffer.num_frames();
        self.scratch.set_num_frames(frames);
        for (wide, narrow) in self.scratch.iter_channels_mut().zip(buffer.iter_channels()) {
            for (wide, &narrow) in wide.iter_mut().zip(narrow) {
                *wide = f64::from(narrow);
            }
        }
        let mut start = 0;
        while start < frames {
            if self.grid == 0 && self.moving() {
                self.design();
            }
            let run = ((EQ_GRID - self.grid) as usize).min(frames - start);
            if self.moving() {
                for smoother in self
                    .gains
                    .iter_mut()
                    .chain(&mut self.frequencies)
                    .chain(core::iter::once(&mut self.mid_q))
                {
                    if smoother.is_smoothing() {
                        for _ in 0..run {
                            smoother.next_value();
                        }
                    }
                }
            }
            let bands = &mut self.bands;
            self.scratch.windowed(start..start + run, |piece| {
                for band in bands.iter_mut() {
                    band.process(piece);
                }
            });
            self.grid = (self.grid + run as u32) % EQ_GRID;
            start += run;
        }
        for (narrow, wide) in buffer.iter_channels_mut().zip(self.scratch.iter_channels()) {
            for (narrow, &wide) in narrow.iter_mut().zip(wide) {
                *narrow = wide as f32;
            }
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        let smoother = match id {
            params::EQ_LOW_GAIN => &mut self.gains[0],
            params::EQ_MID_GAIN => &mut self.gains[1],
            params::EQ_HIGH_GAIN => &mut self.gains[2],
            params::EQ_LOW_FREQ => &mut self.frequencies[0],
            params::EQ_MID_FREQ => &mut self.frequencies[1],
            params::EQ_HIGH_FREQ => &mut self.frequencies[2],
            params::EQ_MID_Q => &mut self.mid_q,
            _ => return,
        };
        set(smoother, value as f32, idle);
        self.dirty = true;
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = &params.eq;
        for (smoother, param) in self
            .gains
            .iter_mut()
            .zip([&p.low_gain, &p.mid_gain, &p.high_gain])
        {
            smoother.reset(param.value());
        }
        for (smoother, param) in
            self.frequencies
                .iter_mut()
                .zip([&p.low_freq, &p.mid_freq, &p.high_freq])
        {
            smoother.reset(param.value());
        }
        self.mid_q.reset(p.mid_q.value());
        for band in &mut self.bands {
            Processor::<f64>::reset(band);
        }
        self.grid = 0;
        self.design();
    }
}

/// The drive's curves, in the order of their values.
const DRIVE_CURVES: [ShaperCurve; 4] = [
    ShaperCurve::Tanh,
    ShaperCurve::SoftClip,
    ShaperCurve::HardClip,
    ShaperCurve::Fold,
];

pub(crate) fn drive_curve(index: f64) -> ShaperCurve {
    DRIVE_CURVES[(index.max(0.0) as usize).min(DRIVE_CURVES.len() - 1)]
}

/// Drive: gain into one of `manguebyte-dsp`'s [`ShaperCurve`]s, gain out,
/// mixed with the clean signal. A change of curve cross-faded over
/// [`EFFECT_FADE_SECONDS`].
pub(crate) struct Drive {
    drive: ParamSmoother,
    output: ParamSmoother,
    mix: ParamSmoother,
    curve: ShaperCurve,
    previous: ShaperCurve,
    /// Frames of the cross-fade from the previous curve still to go.
    fade: u32,
    fade_length: u32,
}

impl Drive {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec) -> Self {
        let rate = spec.sample_rate;
        let p = &params.drive;
        let curve = drive_curve(p.shape.plain());
        Self {
            drive: smoother(
                LEVEL_SMOOTHING,
                rate,
                gain_from_db(f64::from(p.amount.value())),
            ),
            output: smoother(
                LEVEL_SMOOTHING,
                rate,
                gain_from_db(f64::from(p.output.value())),
            ),
            mix: smoother(LEVEL_SMOOTHING, rate, p.mix.value()),
            curve,
            previous: curve,
            fade: 0,
            fade_length: ((EFFECT_FADE_SECONDS * rate).round() as u32).max(1),
        }
    }
}

impl Insert for Drive {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (left, right) in left.iter_mut().zip(right) {
            let drive = f64::from(self.drive.next_value());
            let output = f64::from(self.output.next_value());
            let mix = f64::from(self.mix.next_value());
            let old = if self.fade > 0 {
                self.fade -= 1;
                f64::from(self.fade) / f64::from(self.fade_length)
            } else {
                0.0
            };
            for sample in [left, right] {
                let input = f64::from(*sample);
                let mut shaped = self.curve.apply(input * drive);
                if old > 0.0 {
                    shaped += (self.previous.apply(input * drive) - shaped) * old;
                }
                *sample = if mix >= 1.0 {
                    (shaped * output) as f32
                } else {
                    (input + (shaped * output - input) * mix) as f32
                };
            }
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        match id {
            params::DRIVE_AMOUNT => set(&mut self.drive, gain_from_db(value), idle),
            params::DRIVE_OUTPUT => set(&mut self.output, gain_from_db(value), idle),
            params::DRIVE_MIX => set(&mut self.mix, value as f32, idle),
            params::DRIVE_SHAPE => {
                let curve = drive_curve(value);
                if curve != self.curve {
                    self.previous = self.curve;
                    self.curve = curve;
                    self.fade = if idle { 0 } else { self.fade_length };
                }
            }
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = &params.drive;
        self.drive.reset(gain_from_db(f64::from(p.amount.value())));
        self.output.reset(gain_from_db(f64::from(p.output.value())));
        self.mix.reset(p.mix.value());
        self.curve = drive_curve(p.shape.plain());
        self.previous = self.curve;
        self.fade = 0;
    }
}

/// The size of the bitcrusher's step at a bit depth: `2^(1 - bits)`, so
/// that `bits` bits cover -1 to 1 as a converter of that many bits does.
pub(crate) fn crush_step(bits: f64) -> f64 {
    (1.0 - bits.clamp(1.0, 16.0)).exp2()
}

/// Rounds `sample` to the nearest multiple of `step`. Half-way rounds away
/// from zero, so the staircase is odd: no offset, and silence stays exact
/// silence.
#[inline]
pub(crate) fn crush(sample: f32, step: f32) -> f32 {
    (sample / step).round() * step
}

/// The bitcrusher: holds the signal for whole periods of its rate (a
/// sample-and-hold, aliasing on purpose) and rounds it to steps of
/// [`crush_step`].
pub(crate) struct Bitcrush {
    sample_rate: f64,
    bits: ParamSmoother,
    mix: ParamSmoother,
    /// How far a frame moves the hold's phase: the crush rate over the
    /// sample rate.
    increment: f64,
    /// The hold's phase; a new value is taken each time it passes 1.
    phase: f64,
    held: [f32; 2],
    step: f32,
    /// The bit depth `step` was worked out for.
    step_bits: f32,
}

impl Bitcrush {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec) -> Self {
        let mut crush = Self {
            sample_rate: spec.sample_rate,
            bits: smoother(LEVEL_SMOOTHING, spec.sample_rate, 8.0),
            mix: smoother(LEVEL_SMOOTHING, spec.sample_rate, 1.0),
            increment: 1.0,
            phase: 1.0,
            held: [0.0; 2],
            step: 0.0,
            step_bits: f32::NAN,
        };
        crush.reset(params);
        crush
    }

    fn set_rate(&mut self, hertz: f64) {
        self.increment = (hertz / self.sample_rate).clamp(0.0, 1.0);
    }
}

impl Insert for Bitcrush {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (left, right) in left.iter_mut().zip(right) {
            let bits = self.bits.next_value();
            if bits != self.step_bits {
                self.step = crush_step(f64::from(bits)) as f32;
                self.step_bits = bits;
            }
            let mix = self.mix.next_value();
            let take = self.phase >= 1.0;
            if take {
                self.phase -= self.phase.floor();
            }
            self.phase += self.increment;
            for (sample, held) in [left, right].into_iter().zip(&mut self.held) {
                if take {
                    *held = crush(*sample, self.step);
                }
                *sample = blend(*sample, *held, mix);
            }
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        match id {
            params::BITCRUSH_BITS => set(&mut self.bits, value as f32, idle),
            params::BITCRUSH_RATE => self.set_rate(value),
            params::BITCRUSH_MIX => set(&mut self.mix, value as f32, idle),
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = &params.bitcrush;
        self.bits.reset(p.bits.value());
        self.mix.reset(p.mix.value());
        self.set_rate(p.rate.plain());
        // The first frame takes a value.
        self.phase = 1.0;
        self.held = [0.0; 2];
    }
}

/// The compressor: `manguebyte-dsp`'s [`Compressor`], with a make-up gain
/// and a mix that glide.
pub(crate) struct Squeeze {
    compressor: Compressor<f32>,
    makeup: ParamSmoother,
    mix: ParamSmoother,
    dry: AudioBuffer<f32>,
}

impl Squeeze {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec) -> Self {
        let mut compressor = Compressor::new();
        compressor.prepare(&ProcessSpec {
            num_channels: 2,
            ..*spec
        });
        let mut squeeze = Self {
            compressor,
            makeup: smoother(LEVEL_SMOOTHING, spec.sample_rate, 1.0),
            mix: smoother(LEVEL_SMOOTHING, spec.sample_rate, 1.0),
            dry: AudioBuffer::new(2, spec.max_block_size),
        };
        squeeze.reset(params);
        squeeze
    }

    /// Gives `compressor` the settings in `params`, with no make-up of its
    /// own: the make-up glides here.
    pub(crate) fn configure<S: Sample>(
        compressor: &mut Compressor<S>,
        threshold: f64,
        ratio: f64,
        knee: f64,
        attack: f64,
        release: f64,
    ) {
        compressor.set_threshold_db(threshold);
        compressor.set_ratio(ratio);
        compressor.set_knee_db(knee);
        compressor.set_attack_ms(attack);
        compressor.set_release_ms(release);
        compressor.set_makeup_db(0.0);
    }
}

impl Insert for Squeeze {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        let plain = !self.makeup.is_smoothing()
            && !self.mix.is_smoothing()
            && self.makeup.current() == 1.0
            && self.mix.current() == 1.0;
        if plain {
            self.compressor.process(buffer);
            return;
        }
        let frames = buffer.num_frames();
        self.dry.set_num_frames(frames);
        self.dry.copy_from(buffer);
        self.compressor.process(buffer);
        let (left, right) = buffer.channel_pair_mut(0, 1);
        let (dry_left, dry_right) = (self.dry.channel(0), self.dry.channel(1));
        for frame in 0..frames {
            let makeup = self.makeup.next_value();
            let mix = self.mix.next_value();
            left[frame] = blend(dry_left[frame], left[frame] * makeup, mix);
            right[frame] = blend(dry_right[frame], right[frame] * makeup, mix);
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        let compressor = &mut self.compressor;
        match id {
            params::COMPRESSOR_THRESHOLD => compressor.set_threshold_db(value),
            params::COMPRESSOR_RATIO => compressor.set_ratio(value),
            params::COMPRESSOR_KNEE => compressor.set_knee_db(value),
            params::COMPRESSOR_ATTACK => compressor.set_attack_ms(value),
            params::COMPRESSOR_RELEASE => compressor.set_release_ms(value),
            params::COMPRESSOR_MAKEUP => set(&mut self.makeup, gain_from_db(value), idle),
            params::COMPRESSOR_MIX => set(&mut self.mix, value as f32, idle),
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = &params.compressor;
        Self::configure(
            &mut self.compressor,
            p.threshold.plain(),
            p.ratio.plain(),
            p.knee.plain(),
            p.attack.plain(),
            p.release.plain(),
        );
        Processor::<f32>::reset(&mut self.compressor);
        self.makeup.reset(gain_from_db(p.makeup.plain()));
        self.mix.reset(p.mix.value());
    }
}

/// The chorus and the flanger: `manguebyte-dsp`'s [`Chorus`], which glides
/// its own settings.
pub(crate) struct Modulated {
    effect: Effect,
    chorus: Chorus<f32>,
}

impl Modulated {
    /// The chorus ([`Effect::Chorus`]) or the flanger.
    pub(crate) fn new(effect: Effect, params: &ChainParams, spec: &ProcessSpec) -> Self {
        let mut chorus = if effect == Effect::Flanger {
            Chorus::flanger()
        } else {
            Chorus::new()
        };
        chorus.prepare(&ProcessSpec {
            num_channels: 2,
            ..*spec
        });
        let mut modulated = Self { effect, chorus };
        modulated.reset(params);
        modulated
    }
}

impl Insert for Modulated {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        self.chorus.process(buffer);
    }

    fn set_param(&mut self, id: ParamId, value: f64, _idle: bool) {
        // Gliding while silent does not matter: switching on resets.
        let chorus = &mut self.chorus;
        match id {
            params::CHORUS_MIX | params::FLANGER_MIX => chorus.set_mix(value),
            params::CHORUS_RATE | params::FLANGER_RATE => chorus.set_rate_hz(value),
            params::CHORUS_DEPTH | params::FLANGER_DEPTH => chorus.set_depth_ms(value),
            params::CHORUS_DELAY | params::FLANGER_DELAY => chorus.set_center_delay_ms(value),
            params::CHORUS_VOICES => chorus.set_voices(value as usize),
            params::CHORUS_SPREAD => chorus.set_spread(value),
            params::FLANGER_FEEDBACK => chorus.set_feedback(value),
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let chorus = &mut self.chorus;
        if self.effect == Effect::Flanger {
            let p = &params.flanger;
            chorus.set_mix(p.mix.plain());
            chorus.set_rate_hz(p.rate.plain());
            chorus.set_depth_ms(p.depth.plain());
            chorus.set_center_delay_ms(p.delay.plain());
            chorus.set_feedback(p.feedback.plain());
        } else {
            let p = &params.chorus;
            chorus.set_mix(p.mix.plain());
            chorus.set_rate_hz(p.rate.plain());
            chorus.set_depth_ms(p.depth.plain());
            chorus.set_center_delay_ms(p.delay.plain());
            chorus.set_voices(p.voices.plain() as usize);
            chorus.set_spread(p.spread.plain());
        }
        // Clears the lines and jumps every glide to where it is heading.
        Processor::<f32>::reset(chorus);
    }
}

/// The phaser: `manguebyte-dsp`'s [`Phaser`].
pub(crate) struct PhaserFx {
    phaser: Phaser<f32>,
}

impl PhaserFx {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec) -> Self {
        let mut phaser = Phaser::new();
        phaser.prepare(&ProcessSpec {
            num_channels: 2,
            ..*spec
        });
        let mut fx = Self { phaser };
        fx.reset(params);
        fx
    }
}

impl Insert for PhaserFx {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _clock: Clock) {
        self.phaser.process(buffer);
    }

    fn set_param(&mut self, id: ParamId, value: f64, _idle: bool) {
        let phaser = &mut self.phaser;
        match id {
            params::PHASER_MIX => phaser.set_mix(value),
            params::PHASER_RATE => phaser.set_rate_hz(value),
            params::PHASER_DEPTH => phaser.set_depth_octaves(value),
            params::PHASER_CENTER => phaser.set_center_hz(value),
            params::PHASER_FEEDBACK => phaser.set_feedback(value),
            params::PHASER_STAGES => phaser.set_stages(value as usize),
            params::PHASER_SPREAD => phaser.set_spread(value),
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = &params.phaser;
        let phaser = &mut self.phaser;
        phaser.set_mix(p.mix.plain());
        phaser.set_rate_hz(p.rate.plain());
        phaser.set_depth_octaves(p.depth.plain());
        phaser.set_center_hz(p.center.plain());
        phaser.set_feedback(p.feedback.plain());
        phaser.set_stages(p.stages.plain() as usize);
        phaser.set_spread(p.spread.plain());
        Processor::<f32>::reset(phaser);
    }
}

/// The waves of the tremolo and the auto-pan, in the order of their
/// values.
const LFO_SHAPES: [LfoShape; 5] = [
    LfoShape::Sine,
    LfoShape::Triangle,
    LfoShape::Square,
    LfoShape::SawUp,
    LfoShape::SawDown,
];

pub(crate) fn lfo_shape(index: f64) -> LfoShape {
    LFO_SHAPES[(index.max(0.0) as usize).min(LFO_SHAPES.len() - 1)]
}

/// What a tremolo or an auto-pan does with its wave at `wave` (-1 to 1)
/// and `depth`: the tremolo's gain, from `1 - depth` at the trough to 1 at
/// the crest, or the auto-pan's position, `depth × wave`.
pub(crate) fn wobble_target(effect: Effect, wave: f64, depth: f64) -> f64 {
    if effect == Effect::Tremolo {
        1.0 - depth * (1.0 - (wave + 1.0) / 2.0)
    } else {
        depth * wave
    }
}

/// The tremolo and the auto-pan: an [`Lfo`] that moves the part's gain, or
/// its place between left and right, free at a rate in hertz or locked to
/// the beat.
pub(crate) struct Wobble {
    effect: Effect,
    lfo: Lfo,
    depth: ParamSmoother,
    /// Beats per cycle; 0 runs free.
    sync: f64,
    /// The gain or the position, following its target through a one-pole
    /// smoother.
    value: f64,
    coefficient: f64,
}

impl Wobble {
    /// The tremolo ([`Effect::Tremolo`]) or the auto-pan.
    pub(crate) fn new(effect: Effect, params: &ChainParams, spec: &ProcessSpec) -> Self {
        let mut lfo = Lfo::new(LfoShape::Sine, 1.0);
        lfo.set_sample_rate(spec.sample_rate);
        let mut wobble = Self {
            effect,
            lfo,
            depth: smoother(LEVEL_SMOOTHING, spec.sample_rate, 0.0),
            sync: 0.0,
            value: 0.0,
            coefficient: 1.0 - (-1.0 / (WAVE_SMOOTHING_SECONDS * spec.sample_rate)).exp(),
        };
        wobble.reset(params);
        wobble
    }

    fn params(effect: Effect, params: &ChainParams) -> &params::ModulatorParams {
        if effect == Effect::Tremolo {
            &params.tremolo
        } else {
            &params.autopan
        }
    }

    /// The wave at the next frame, `beat` being that frame's beat.
    #[inline]
    fn wave(&mut self, beat: f64) -> f64 {
        if self.sync > 0.0 {
            self.lfo.set_phase(beat / self.sync);
            self.lfo.value()
        } else {
            self.lfo.next_value()
        }
    }
}

impl Insert for Wobble {
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, clock: Clock) {
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (frame, (left, right)) in left.iter_mut().zip(right).enumerate() {
            let wave = self.wave(clock.beat_at(frame));
            let depth = f64::from(self.depth.next_value());
            let target = wobble_target(self.effect, wave, depth);
            self.value += (target - self.value) * self.coefficient;
            if self.effect == Effect::Tremolo {
                let gain = self.value as f32;
                *left *= gain;
                *right *= gain;
            } else {
                let (left_gain, right_gain) = pan_gains(self.value as f32);
                *left *= left_gain;
                *right *= right_gain;
            }
        }
    }

    fn set_param(&mut self, id: ParamId, value: f64, idle: bool) {
        match id {
            params::TREMOLO_RATE | params::AUTOPAN_RATE => self.lfo.set_frequency(value),
            params::TREMOLO_DEPTH | params::AUTOPAN_DEPTH => {
                set(&mut self.depth, value as f32, idle)
            }
            params::TREMOLO_SHAPE | params::AUTOPAN_SHAPE => self.lfo.set_shape(lfo_shape(value)),
            params::TREMOLO_SYNC | params::AUTOPAN_SYNC => self.sync = value.max(0.0),
            _ => {}
        }
    }

    fn reset(&mut self, params: &ChainParams) {
        let p = Self::params(self.effect, params);
        self.lfo.set_frequency(p.rate.plain());
        self.lfo.set_shape(lfo_shape(p.shape.plain()));
        self.lfo.set_phase(0.0);
        self.sync = p.sync.plain().max(0.0);
        self.depth.reset(p.depth.value());
        // Starts where the wave starts, so that it does not glide in.
        self.value = wobble_target(self.effect, self.lfo.value(), f64::from(p.depth.value()));
    }
}

/// The delay's damping as the pole of a one-pole low-pass in its loop:
/// none at 0, a corner falling from 20 kHz at a hair above 0 to 500 Hz at
/// 1, evenly in octaves.
pub(crate) fn damping_pole(damping: f64, sample_rate: f64) -> f64 {
    if damping <= 0.0 {
        return 0.0;
    }
    let corner = (20_000.0 * 0.025f64.powf(damping.min(1.0))).min(0.45 * sample_rate);
    (-TAU * corner / sample_rate).exp()
}

/// The delay time in frames: `sync` beats at the clock's tempo, or `time`
/// seconds, between one frame and [`DELAY_MAX_SECONDS`].
pub(crate) fn delay_frames(time: f64, sync: f64, beats_per_frame: f64, sample_rate: f64) -> usize {
    let frames = if sync > 0.0 && beats_per_frame > 0.0 {
        sync / beats_per_frame
    } else {
        time * sample_rate
    };
    frames
        .round()
        .clamp(1.0, (DELAY_MAX_SECONDS * sample_rate).ceil()) as usize
}

/// The delay: a line per side, read at a whole number of frames, with
/// feedback through a damping low-pass, routed from side to side as much
/// as `delay.pingpong` says.
///
/// Switched off, its input fades out over [`EFFECT_FADE_SECONDS`] and the
/// echoes already in it ring on. Once they and its input have been below
/// -120 dB for longer than the delay time, the lines are cleared and the
/// delay sleeps, costing nothing, until sound reaches it again.
pub(crate) struct Echo {
    sample_rate: f64,
    lines: [DelayLine<f32>; 2],
    switch: Switch,
    time: f64,
    sync: f64,
    /// The delay heard, in frames, and the one being faded to.
    current: usize,
    next: usize,
    /// Frames into the cross-fade from `current` to `next`; 0 when there is
    /// none.
    glide: u32,
    glide_length: u32,
    feedback: ParamSmoother,
    mix: ParamSmoother,
    pingpong: ParamSmoother,
    damping: ParamSmoother,
    pole: f32,
    /// The damping `pole` was worked out for.
    pole_damping: f32,
    /// The damping low-pass of each side.
    lowpass: [f32; 2],
    asleep: bool,
    /// Frames in a row on which nothing went in and nothing came out.
    quiet: usize,
    /// Whether nothing came in on the last frame: the switch's fade then
    /// jumps to its end, as the delay's dry level and its input can change
    /// at once without a step.
    input_quiet: bool,
}

impl Echo {
    pub(crate) fn new(params: &ChainParams, spec: &ProcessSpec, on: bool) -> Self {
        let rate = spec.sample_rate;
        let longest = (DELAY_MAX_SECONDS * rate).ceil() as usize + 1;
        let p = &params.delay;
        let mut echo = Self {
            sample_rate: rate,
            lines: [DelayLine::new(longest), DelayLine::new(longest)],
            switch: Switch::new(rate, on),
            time: p.time.plain(),
            sync: p.sync.plain(),
            current: 1,
            next: 1,
            glide: 0,
            glide_length: ((DELAY_GLIDE_SECONDS * rate).round() as u32).max(1),
            feedback: smoother(LEVEL_SMOOTHING, rate, 0.0),
            mix: smoother(LEVEL_SMOOTHING, rate, 0.0),
            pingpong: smoother(LEVEL_SMOOTHING, rate, 0.0),
            damping: smoother(LEVEL_SMOOTHING, rate, 0.0),
            pole: 0.0,
            pole_damping: f32::NAN,
            lowpass: [0.0; 2],
            asleep: true,
            quiet: 0,
            input_quiet: true,
        };
        echo.restart(params, Clock::new(rate).beats_per_frame);
        echo
    }

    /// Jumps to the settings in `params`, empty: the lines are cleared
    /// unless the delay is asleep, which it only falls into once they have
    /// been.
    fn restart(&mut self, params: &ChainParams, beats_per_frame: f64) {
        let p = &params.delay;
        if !self.asleep {
            for line in &mut self.lines {
                line.clear();
            }
        }
        self.lowpass = [0.0; 2];
        self.time = p.time.plain();
        self.sync = p.sync.plain();
        self.current = delay_frames(self.time, self.sync, beats_per_frame, self.sample_rate);
        self.next = self.current;
        self.glide = 0;
        self.feedback.reset(p.feedback.value());
        self.mix.reset(p.mix.value());
        self.pingpong.reset(p.pingpong.value());
        self.damping.reset(p.damping.value());
        self.asleep = true;
        self.quiet = 0;
    }

    fn idle(&self) -> bool {
        self.asleep
    }

    fn process(&mut self, buffer: &mut AudioBuffer<f32>, clock: Clock) {
        let frames = buffer.num_frames();
        let target = delay_frames(
            self.time,
            self.sync,
            clock.beats_per_frame,
            self.sample_rate,
        );
        let (left, right) = buffer.channel_pair_mut(0, 1);
        let mut frame = 0;
        while frame < frames {
            if self.asleep {
                // Asleep, silence stays silence: find where sound starts.
                let sounding = self.switch.is_on() || !self.switch.is_silent();
                let woken = (frame..frames).find(|&at| left[at] != 0.0 || right[at] != 0.0);
                match woken {
                    Some(at) if sounding => {
                        if at > frame {
                            self.input_quiet = true;
                        }
                        frame = at;
                        self.asleep = false;
                        self.quiet = 0;
                        // A delay that slept is empty: the heads may jump.
                        self.current = target;
                        self.next = target;
                        self.glide = 0;
                    }
                    _ => {
                        // Nothing comes in, so the switch's fade is over.
                        self.switch.snap();
                        self.input_quiet = true;
                        return;
                    }
                }
            }
            // A new time waits for the glide under way to end.
            if self.glide == 0 && target != self.current {
                self.next = target;
                self.glide = 1;
            }
            // A fade only matters while sound comes in.
            if self.input_quiet {
                self.switch.snap();
            }
            let level = self.switch.next();
            let feedback = self.feedback.next_value();
            let mix = self.mix.next_value();
            let pingpong = self.pingpong.next_value();
            let damping = self.damping.next_value();
            if damping != self.pole_damping {
                self.pole = damping_pole(f64::from(damping), self.sample_rate) as f32;
                self.pole_damping = damping;
            }

            // The two heads, cross-faded while the time moves.
            let mut wet = [0.0f32; 2];
            for (wet, line) in wet.iter_mut().zip(&self.lines) {
                let heard = line.read(self.current as f64);
                *wet = if self.glide > 0 {
                    let fade = self.glide as f32 / self.glide_length as f32;
                    heard + (line.read(self.next as f64) - heard) * fade
                } else {
                    heard
                };
            }
            if self.glide > 0 {
                self.glide += 1;
                if self.glide > self.glide_length {
                    self.current = self.next;
                    self.glide = 0;
                }
            }
            for (wet, state) in wet.iter_mut().zip(&mut self.lowpass) {
                *state = *wet + (*state - *wet) * self.pole;
                *wet = *state;
            }

            let (dry_left, dry_right) = (left[frame], right[frame]);
            self.input_quiet = dry_left == 0.0 && dry_right == 0.0;
            let (in_left, in_right) = (dry_left * level, dry_right * level);
            let straight = 1.0 - pingpong;
            let mono = (in_left + in_right) * 0.5;
            let push_left = in_left * straight
                + mono * pingpong
                + feedback * (straight * wet[0] + pingpong * wet[1]);
            let push_right =
                in_right * straight + feedback * (straight * wet[1] + pingpong * wet[0]);
            self.lines[0].push(push_left);
            self.lines[1].push(push_right);
            left[frame] = dry_left * (1.0 - mix * level) + wet[0] * mix;
            right[frame] = dry_right * (1.0 - mix * level) + wet[1] * mix;

            let silent = in_left == 0.0
                && in_right == 0.0
                && push_left.abs() < QUIET
                && push_right.abs() < QUIET
                && wet[0].abs() < QUIET
                && wet[1].abs() < QUIET;
            self.quiet = if silent { self.quiet + 1 } else { 0 };
            if self.quiet > self.current.max(self.next) + 1 {
                // Everything a head can reach is below -120 dB.
                for line in &mut self.lines {
                    line.clear();
                }
                self.lowpass = [0.0; 2];
                self.asleep = true;
                self.quiet = 0;
            }
            frame += 1;
        }
    }
}

impl StageOps for Echo {
    fn run(&mut self, buffer: &mut AudioBuffer<f32>, _dry: &mut AudioBuffer<f32>, clock: Clock) {
        if buffer.num_frames() == 0 {
            return;
        }
        if self.switch.is_silent() && self.asleep {
            self.input_quiet = ends_silent(buffer);
            return;
        }
        self.process(buffer, clock);
    }

    fn set_on(&mut self, on: bool, params: &ChainParams) {
        if on && self.switch.is_silent() && self.asleep {
            let beats_per_frame = Clock::new(self.sample_rate).beats_per_frame;
            self.restart(params, beats_per_frame);
        }
        self.switch.set(on);
    }

    fn set_param(&mut self, id: ParamId, value: f64) {
        let idle = self.switch.is_silent() && self.asleep;
        match id {
            params::DELAY_TIME => self.time = value,
            params::DELAY_SYNC => self.sync = value.max(0.0),
            params::DELAY_FEEDBACK => set(&mut self.feedback, value as f32, idle),
            params::DELAY_MIX => set(&mut self.mix, value as f32, idle),
            params::DELAY_PINGPONG => set(&mut self.pingpong, value as f32, idle),
            params::DELAY_DAMPING => set(&mut self.damping, value as f32, idle),
            _ => {}
        }
    }

    fn recover(&mut self, params: &ChainParams) {
        self.restart(params, Clock::new(self.sample_rate).beats_per_frame);
        self.switch.snap();
        self.input_quiet = true;
    }

    fn is_idle(&self) -> bool {
        self.switch.is_silent() && self.idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_fades_over_its_length_and_back() {
        let mut switch = Switch::new(1_000.0, false);
        assert!(switch.is_silent());
        switch.set(true);
        let up: Vec<f32> = (0..12).map(|_| switch.next()).collect();
        assert_eq!(
            &up[..10],
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]
        );
        assert!(switch.is_full());
        switch.set(false);
        assert_eq!(switch.frames_left(), Some(10));
        switch.next();
        switch.set(true);
        assert_eq!(switch.next(), 1.0, "it turns round where it is");
    }

    #[test]
    fn the_crusher_is_odd_and_keeps_silence() {
        for bits in [1.0, 4.0, 8.0, 12.5, 16.0] {
            let step = crush_step(bits) as f32;
            assert_eq!(crush(0.0, step), 0.0);
            assert_eq!(crush(1e-30, step), 0.0, "no denormal survives");
            for x in [0.013f32, 0.4, 0.77, 1.0] {
                assert_eq!(crush(-x, step), -crush(x, step));
            }
        }
        assert_eq!(crush_step(8.0), 1.0 / 128.0);
    }

    #[test]
    fn synced_delays_follow_the_tempo() {
        let rate = 48_000.0;
        let clock = Clock::new(rate);
        assert_eq!(delay_frames(0.25, 0.0, clock.beats_per_frame, rate), 12_000);
        // Half a beat at 120 beats a minute: a quarter of a second.
        assert_eq!(delay_frames(0.9, 0.5, clock.beats_per_frame, rate), 12_000);
        assert_eq!(
            delay_frames(99.0, 0.0, clock.beats_per_frame, rate),
            192_000
        );
        assert_eq!(delay_frames(0.0, 0.0, clock.beats_per_frame, rate), 1);
        assert_eq!(damping_pole(0.0, rate), 0.0);
        assert!(damping_pole(1.0, rate) > 0.9);
    }
}
