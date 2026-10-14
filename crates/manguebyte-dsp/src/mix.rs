use core::f64::consts::FRAC_PI_2;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::ProcessSpec;
use crate::numeric::limit;

/// How two signals share the level while a control moves from the first to
/// the second: dry and wet in a [`DryWet`] mixer, left and right in a
/// [`Panner`](crate::Panner).
///
/// Every law gives the first signal alone at one end and the second alone at
/// the other. They differ in between, and the usual way to name the
/// difference is by the level of each side at the centre.
///
/// | Law | Each side at the centre | What it keeps |
/// |---|---|---|
/// | [`Linear`](Self::Linear) | -6.0 dB | the sum of the two gains is one |
/// | [`Balanced`](Self::Balanced) | 0 dB | the louder side is always at full level |
/// | [`EqualPower`](Self::EqualPower) | -3.0 dB | the sum of the two powers is one |
/// | [`Sine4Point5dB`](Self::Sine4Point5dB) | -4.5 dB | half-way between the two above |
/// | [`Sine6dB`](Self::Sine6dB) | -6.0 dB | the sum of the gains is one, with gentler ends |
/// | [`SquareRoot`](Self::SquareRoot) | -3.0 dB | the sum of the powers is one, with steeper ends |
/// | [`SquareRoot4Point5dB`](Self::SquareRoot4Point5dB) | -4.5 dB | half-way, with steeper ends |
///
/// Which one is right depends on how the two signals add up. Signals that
/// are copies of each other, such as an equaliser's input and output, or
/// the two speakers of a mix that is later summed to mono, add in level:
/// [`Linear`](Self::Linear) keeps their sum steady. Signals that are
/// unrelated, such as a voice and its reverb tail, or two speakers heard in
/// a room, add in power: [`EqualPower`](Self::EqualPower) keeps that steady.
/// The -4.5 dB laws are the compromise between the two.
///
/// ```
/// use manguebyte_dsp::MixLaw;
///
/// let (dry, wet) = MixLaw::EqualPower.gains(0.5);
/// assert!((dry * dry + wet * wet - 1.0).abs() < 1e-12);
/// assert!((MixLaw::EqualPower.center_gain_db() + 3.01).abs() < 0.01);
///
/// assert_eq!(MixLaw::Linear.gains(0.25), (0.75, 0.25));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MixLaw {
    /// Two straight lines: `1 - x` and `x`.
    #[default]
    Linear,
    /// Both at full level in the centre; from there, one side stays and the
    /// other falls in a straight line. This is how a balance control works.
    Balanced,
    /// A quarter of a cosine and of a sine: `cos(x π/2)` and `sin(x π/2)`.
    /// Also called constant power.
    EqualPower,
    /// The equal-power curves raised to the power 1.5.
    Sine4Point5dB,
    /// The equal-power curves squared.
    Sine6dB,
    /// Square roots of the straight lines: `sqrt(1 - x)` and `sqrt(x)`.
    SquareRoot,
    /// The straight lines raised to the power 0.75.
    SquareRoot4Point5dB,
}

impl MixLaw {
    /// Every law, in the order they are declared. For tests and menus.
    pub const ALL: [Self; 7] = [
        Self::Linear,
        Self::Balanced,
        Self::EqualPower,
        Self::Sine4Point5dB,
        Self::Sine6dB,
        Self::SquareRoot,
        Self::SquareRoot4Point5dB,
    ];

    /// The gains of the first and the second signal at `position`, where
    /// `0.0` is all of the first and `1.0` all of the second.
    ///
    /// `position` is limited to `0.0..=1.0`. At the two ends the result is
    /// exactly `(1.0, 0.0)` and `(0.0, 1.0)`.
    pub fn gains(self, position: f64) -> (f64, f64) {
        let x = limit(position, 0.0, 1.0);
        // Spelled out, because a sine or a cosine of a quarter turn is only
        // nearly zero.
        if x <= 0.0 {
            return (1.0, 0.0);
        }
        if x >= 1.0 {
            return (0.0, 1.0);
        }
        let (sin, cos) = (x * FRAC_PI_2).sin_cos();
        match self {
            Self::Linear => (1.0 - x, x),
            Self::Balanced => ((2.0 * (1.0 - x)).min(1.0), (2.0 * x).min(1.0)),
            Self::EqualPower => (cos, sin),
            Self::Sine4Point5dB => (cos * cos.sqrt(), sin * sin.sqrt()),
            Self::Sine6dB => (cos * cos, sin * sin),
            Self::SquareRoot => ((1.0 - x).sqrt(), x.sqrt()),
            Self::SquareRoot4Point5dB => ((1.0 - x).powf(0.75), x.powf(0.75)),
        }
    }

    /// The level of each side at the centre, in decibels.
    pub fn center_gain_db(self) -> f64 {
        20.0 * self.gains(0.5).0.log10()
    }
}

/// Mixes the signal that went into an effect with the signal that came out
/// of it.
///
/// An effect that processes a buffer in place overwrites its input, so the
/// dry signal has to be put aside first. And an effect with latency puts
/// its output out late, so the dry signal has to be made late by the same
/// amount, or the two no longer line up and their sum is comb-filtered. This
/// type does both.
///
/// Use it around the effect: [`push_dry`](Self::push_dry) before,
/// [`mix_wet`](Self::mix_wet) after, with the same number of frames.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{DryWet, Limiter, MixLaw, ProcessSpec, Processor};
///
/// let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 };
///
/// // A limiter looks ahead, so what it puts out is late: 239 frames here.
/// let mut limiter = Limiter::<f32>::new();
/// limiter.prepare(&spec);
///
/// let mut mixer = DryWet::<f32>::new()
///     .with_law(MixLaw::EqualPower)
///     .with_mix(0.5)
///     .with_latency(limiter.latency_samples());
/// mixer.prepare(&spec);
///
/// let mut block = AudioBuffer::<f32>::new(2, 256);
/// for _ in 0..2 {
///     for channel in block.iter_channels_mut() {
///         channel.fill(0.5);
///     }
///     mixer.push_dry(&block);
///     limiter.process(&mut block);
///     mixer.mix_wet(&mut block);
/// }
/// // Both paths now carry the input, each 3 dB down.
/// assert!((block.channel(0)[255] - 0.5 * 2f32.sqrt()).abs() < 1e-6);
/// ```
///
/// # The order of calls
///
/// Memory for the dry signal is allocated by [`prepare`](Self::prepare), from
/// the block size and the longest latency the mixer has been told about by
/// then. An effect usually knows its latency only once it has been prepared
/// itself, so prepare the effect first, then tell the mixer, then prepare
/// the mixer, as the example does. For a mixer that already exists:
///
/// ```
/// use manguebyte_dsp::{DryWet, Limiter, ProcessSpec, Processor};
///
/// let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 };
/// let mut limiter = Limiter::<f32>::new();
/// let mut mixer = DryWet::<f32>::new().with_mix(0.5);
///
/// // In the plugin's own `prepare`, every time it is called:
/// limiter.prepare(&spec);
/// mixer.set_max_latency(limiter.latency_samples());
/// mixer.set_latency(limiter.latency_samples());
/// mixer.prepare(&spec);
/// assert_eq!(mixer.latency(), limiter.latency_samples());
/// ```
///
/// A latency the mixer has no memory for is not applied: the dry signal is
/// then delayed by as much as there is memory for, and
/// [`latency`](Self::latency) says how much that is.
///
/// # Limits
///
/// The latency is a whole number of samples. A block longer than the
/// prepared maximum loses the start of its dry signal.
/// [`push_dry`](Self::push_dry) and [`mix_wet`](Self::mix_wet) are real-time
/// safe.
///
/// It is the two gains that glide when the mix changes, not the position on
/// the law's curve, so half-way through a jump from all dry to all wet each
/// path is at half level whatever the law.
#[derive(Debug, Clone)]
pub struct DryWet<S: Sample = f32> {
    law: MixLaw,
    mix: f64,
    ramp_seconds: f64,
    /// Longest latency the next [`prepare`](Self::prepare) makes room for.
    max_latency: usize,
    /// Longest latency the memory allocated by the last
    /// [`prepare`](Self::prepare) can serve. `None` until the first one.
    allocated_latency: Option<usize>,
    /// The latency that was last asked for.
    requested_latency: usize,
    /// The delay of the dry signal: the latency asked for, limited to what
    /// can be served.
    latency: usize,
    dry_gain: Smoothed<S>,
    wet_gain: Smoothed<S>,
    /// The dry signal of each channel, as a ring.
    dry: Vec<Vec<S>>,
    mask: usize,
    /// Slot the next captured frame goes to.
    write: usize,
    /// Frames captured by the last [`push_dry`](Self::push_dry) and not yet
    /// mixed.
    pending: usize,
}

impl<S: Sample> Default for DryWet<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> DryWet<S> {
    /// Default glide time for changes of the mix.
    pub const DEFAULT_RAMP_SECONDS: f64 = 0.02;

    /// Creates a mixer that passes the wet signal only, with the
    /// [`MixLaw::Linear`] law and no latency to make up for.
    pub fn new() -> Self {
        Self {
            law: MixLaw::Linear,
            mix: 1.0,
            ramp_seconds: Self::DEFAULT_RAMP_SECONDS,
            max_latency: 0,
            allocated_latency: None,
            requested_latency: 0,
            latency: 0,
            dry_gain: Smoothed::new(S::ZERO),
            wet_gain: Smoothed::new(S::ONE),
            dry: Vec::new(),
            mask: 0,
            write: 0,
            pending: 0,
        }
    }

    /// Sets the balance: `0.0` is all dry, `1.0` all wet. The gains glide to
    /// their new values, so the change does not click. Real-time safe.
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = limit(mix, 0.0, 1.0);
        self.retarget();
    }

    /// Sets the law that turns the balance into two gains. The gains glide
    /// to their new values. Real-time safe.
    pub fn set_law(&mut self, law: MixLaw) {
        self.law = law;
        self.retarget();
    }

    /// Sets how late the wet signal is, in samples: usually the effect's
    /// [`latency_samples`](crate::Processor::latency_samples). The dry
    /// signal is delayed by this much.
    ///
    /// The delay cannot be longer than the mixer has memory for, which is
    /// the maximum that was in force when it was last prepared: see
    /// [`set_max_latency`](Self::set_max_latency). A longer latency is
    /// remembered, applied as far as the memory goes, and applied in full by
    /// the next [`prepare`](Self::prepare) that makes room for it.
    /// [`latency`](Self::latency) reports the delay really in use.
    ///
    /// Real-time safe, but the dry signal jumps if the delay changes while
    /// audio is running.
    pub fn set_latency(&mut self, samples: usize) {
        self.requested_latency = samples;
        self.apply_latency();
    }

    /// Sets the longest latency [`set_latency`](Self::set_latency) will be
    /// able to match. It takes effect the next time the mixer is prepared,
    /// because it changes the amount of memory needed: until then the mixer
    /// goes on serving what it has memory for.
    ///
    /// This only stores a number and is real-time safe, but the
    /// [`prepare`](Self::prepare) that has to follow is not.
    pub fn set_max_latency(&mut self, samples: usize) {
        self.max_latency = samples;
        self.apply_latency();
    }

    /// Builder form of [`set_max_latency`](Self::set_max_latency).
    #[must_use]
    pub fn with_max_latency(mut self, samples: usize) -> Self {
        self.set_max_latency(samples);
        self
    }

    /// Builder form of [`set_latency`](Self::set_latency), which also raises
    /// the maximum to that latency if it was lower.
    ///
    /// Like any change of the maximum, the extra room comes with the next
    /// [`prepare`](Self::prepare). On a mixer that has already been prepared
    /// for less, the delay stays within the memory it has until it is
    /// prepared again.
    #[must_use]
    pub fn with_latency(mut self, samples: usize) -> Self {
        self.max_latency = self.max_latency.max(samples);
        self.set_latency(samples);
        self
    }

    /// Builder form of [`set_mix`](Self::set_mix).
    #[must_use]
    pub fn with_mix(mut self, mix: f64) -> Self {
        self.set_mix(mix);
        self
    }

    /// Builder form of [`set_law`](Self::set_law).
    #[must_use]
    pub fn with_law(mut self, law: MixLaw) -> Self {
        self.set_law(law);
        self
    }

    /// Sets how long a change of the mix takes, in seconds. Applies from the
    /// next [`prepare`](Self::prepare).
    #[must_use]
    pub fn with_ramp(mut self, seconds: f64) -> Self {
        self.ramp_seconds = seconds;
        self
    }

    /// The balance, as last set.
    pub fn mix(&self) -> f64 {
        self.mix
    }

    /// The law in use.
    pub fn law(&self) -> MixLaw {
        self.law
    }

    /// The delay of the dry signal, in samples: the latency last asked for,
    /// or less if the mixer has no memory for that much.
    ///
    /// Before the first [`prepare`](Self::prepare) no memory has been
    /// allocated, and this is the delay that `prepare` will set up.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// The longest latency the next [`prepare`](Self::prepare) makes room
    /// for.
    pub fn max_latency(&self) -> usize {
        self.max_latency
    }

    /// Works out the delay of the dry signal from the latency asked for and
    /// the memory there is to serve it.
    fn apply_latency(&mut self) {
        let most = self.allocated_latency.unwrap_or(self.max_latency);
        self.latency = self.requested_latency.min(most);
    }

    fn retarget(&mut self) {
        let (dry, wet) = self.law.gains(self.mix);
        let (dry, wet) = (S::from_f64(dry), S::from_f64(wet));
        // Setting a target again would start its ramp again.
        if self.dry_gain.target() != dry {
            self.dry_gain.set_target(dry);
        }
        if self.wet_gain.target() != wet {
            self.wet_gain.set_target(wet);
        }
    }

    /// Gets ready for blocks of up to `spec.max_block_size` frames on
    /// `spec.num_channels` channels, with room for the longest latency set
    /// so far. This allocates; the dry signal is cleared and the gains jump
    /// to their targets.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        // The dry sample that goes with the first frame of a block is the
        // whole block plus the latency behind the newest one.
        let len = (self.max_latency + spec.max_block_size)
            .max(1)
            .next_power_of_two();
        self.dry = vec![vec![S::ZERO; len]; spec.num_channels];
        self.mask = len - 1;
        self.write = 0;
        self.pending = 0;
        self.allocated_latency = Some(self.max_latency);
        self.apply_latency();
        self.dry_gain.set_ramp(spec.sample_rate, self.ramp_seconds);
        self.wet_gain.set_ramp(spec.sample_rate, self.ramp_seconds);
    }

    /// Captures the dry signal. Call it with the block before the effect
    /// processes it. Real-time safe.
    pub fn push_dry(&mut self, buffer: &AudioBuffer<S>) {
        let (start, mask) = (self.write, self.mask);
        for (ring, samples) in self.dry.iter_mut().zip(buffer.iter_channels()) {
            for (frame, &sample) in samples.iter().enumerate() {
                ring[start.wrapping_add(frame) & mask] = sample;
            }
        }
        self.pending = buffer.num_frames();
        self.write = start.wrapping_add(self.pending) & mask;
    }

    /// Mixes the captured dry signal, delayed by the latency, into the wet
    /// signal in `buffer`. Call it with the block after the effect has
    /// processed it. Real-time safe.
    ///
    /// The block should hold as many frames as the one given to
    /// [`push_dry`](Self::push_dry). Frames beyond that count have no dry
    /// signal and are mixed as if it were silent, and so are channels the
    /// mixer was not prepared for, which are left as they are.
    pub fn mix_wet(&mut self, buffer: &mut AudioBuffer<S>) {
        let frames = buffer.num_frames();
        let covered = frames.min(self.pending);
        let mask = self.mask;
        // Slot of the dry sample that belongs with the first wet one.
        let start = self
            .write
            .wrapping_sub(self.pending)
            .wrapping_sub(self.latency);
        self.pending = 0;

        if !self.dry_gain.is_smoothing() && !self.wet_gain.is_smoothing() {
            let (dry_gain, wet_gain) = (self.dry_gain.target(), self.wet_gain.target());
            if dry_gain == S::ZERO && wet_gain == S::ONE {
                // All wet: there is nothing to do, and nothing is read.
                return;
            }
            if wet_gain == S::ZERO {
                // All dry: whatever the effect put out is not looked at, so
                // that even an effect that has blown up cannot get through.
                for (ring, samples) in self.dry.iter().zip(buffer.iter_channels_mut()) {
                    for (frame, sample) in samples.iter_mut().enumerate() {
                        *sample = if frame < covered {
                            ring[start.wrapping_add(frame) & mask] * dry_gain
                        } else {
                            S::ZERO
                        };
                    }
                }
                return;
            }
        }

        // Every channel must see the same ramp, so each one runs a copy of
        // the two gains and the copies of the last channel are kept.
        let mut after = (self.dry_gain, self.wet_gain);
        let mut ran = false;
        for (ring, samples) in self.dry.iter().zip(buffer.iter_channels_mut()) {
            let (mut dry_gain, mut wet_gain) = (self.dry_gain, self.wet_gain);
            for (frame, sample) in samples.iter_mut().enumerate() {
                let dry = if frame < covered {
                    ring[start.wrapping_add(frame) & mask]
                } else {
                    S::ZERO
                };
                *sample = *sample * wet_gain.next_value() + dry * dry_gain.next_value();
            }
            after = (dry_gain, wet_gain);
            ran = true;
        }
        if !ran {
            for _ in 0..frames {
                after.0.next_value();
                after.1.next_value();
            }
        }
        (self.dry_gain, self.wet_gain) = after;
    }

    /// Forgets the captured dry signal and finishes any glide of the gains.
    /// Real-time safe.
    pub fn reset(&mut self) {
        for ring in &mut self.dry {
            ring.fill(S::ZERO);
        }
        self.write = 0;
        self.pending = 0;
        self.dry_gain.set_immediate(self.dry_gain.target());
        self.wet_gain.set_immediate(self.wet_gain.target());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::{strict_buffer_peak, strict_peak};
    use crate::{Limiter, Processor};

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize, block: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: block,
            num_channels: channels,
        }
    }

    /// Repeatable noise between -1 and 1, different on each channel.
    fn noise(channels: usize, frames: usize) -> AudioBuffer<f64> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut buffer = AudioBuffer::new(channels, frames);
        for channel in buffer.iter_channels_mut() {
            for sample in channel {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *sample = (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
            }
        }
        buffer
    }

    /// A stand-in for an effect with latency: it delays by a whole number of
    /// frames and multiplies by a gain, and does nothing else.
    struct Late {
        history: Vec<Vec<f64>>,
        position: usize,
        gain: f64,
    }

    impl Late {
        fn new(channels: usize, frames: usize, gain: f64) -> Self {
            Self {
                history: vec![vec![0.0; frames.max(1)]; channels],
                position: 0,
                gain,
            }
        }

        fn process(&mut self, buffer: &mut AudioBuffer<f64>) {
            let mut position = self.position;
            for (history, samples) in self.history.iter_mut().zip(buffer.iter_channels_mut()) {
                position = self.position;
                for sample in samples {
                    let delayed = history[position];
                    history[position] = *sample;
                    *sample = delayed * self.gain;
                    position = (position + 1) % history.len();
                }
            }
            self.position = position;
        }
    }

    /// Runs `signal` through `effect` with the mixer around it, in blocks of
    /// `block` frames.
    fn run(
        mixer: &mut DryWet<f64>,
        signal: &mut AudioBuffer<f64>,
        block: usize,
        mut effect: impl FnMut(&mut AudioBuffer<f64>),
    ) {
        let frames = signal.num_frames();
        let mut start = 0;
        while start < frames {
            let end = (start + block).min(frames);
            signal.windowed(start..end, |piece| {
                mixer.push_dry(piece);
                effect(piece);
                mixer.mix_wet(piece);
            });
            start = end;
        }
    }

    #[test]
    fn every_law_has_its_centre_level_and_meets_the_ends_exactly() {
        let expected_db = [-6.0206, 0.0, -3.0103, -4.5154, -6.0206, -3.0103, -4.5154];
        for (law, expected) in MixLaw::ALL.into_iter().zip(expected_db) {
            let (first, second) = law.gains(0.5);
            assert!((first - second).abs() < 1e-15, "{law:?} is lopsided");
            let level = law.center_gain_db();
            assert!((level - expected).abs() < 1e-4, "{law:?}: {level} dB");

            assert_eq!(law.gains(0.0), (1.0, 0.0), "{law:?}");
            assert_eq!(law.gains(1.0), (0.0, 1.0), "{law:?}");
            // Out of range and not-a-number are treated as the nearest end.
            assert_eq!(law.gains(-3.0), (1.0, 0.0));
            assert_eq!(law.gains(7.0), (0.0, 1.0));
            assert_eq!(law.gains(f64::NAN), (1.0, 0.0));

            // One side only ever falls and the other only ever rises, the
            // two mirror each other, and nothing jumps on the way.
            let mut previous = law.gains(0.0);
            for step in 1..=1_000 {
                let x = f64::from(step) / 1_000.0;
                let (first, second) = law.gains(x);
                assert!(
                    first <= previous.0 && second >= previous.1,
                    "{law:?} at {x}"
                );
                assert!(
                    previous.0 - first < 0.04 && second - previous.1 < 0.04,
                    "{law:?} jumps at {x}"
                );
                let mirrored = law.gains(1.0 - x);
                assert!((mirrored.0 - second).abs() < 1e-12 && (mirrored.1 - first).abs() < 1e-12);
                previous = (first, second);
            }
        }
    }

    #[test]
    fn each_law_keeps_what_it_says_it_keeps() {
        for step in 0..=100 {
            let x = f64::from(step) / 100.0;
            for law in [MixLaw::Linear, MixLaw::Sine6dB] {
                let (first, second) = law.gains(x);
                assert!((first + second - 1.0).abs() < 1e-12, "{law:?} at {x}");
            }
            for law in [MixLaw::EqualPower, MixLaw::SquareRoot] {
                let (first, second) = law.gains(x);
                let power = first * first + second * second;
                assert!((power - 1.0).abs() < 1e-12, "{law:?} at {x}");
            }
            let (first, second) = MixLaw::Balanced.gains(x);
            assert_eq!(first.max(second), 1.0);
            // The -4.5 dB laws are the geometric mean of the -3 and -6 dB
            // laws of the same family.
            for (law, low, high) in [
                (MixLaw::Sine4Point5dB, MixLaw::EqualPower, MixLaw::Sine6dB),
                (
                    MixLaw::SquareRoot4Point5dB,
                    MixLaw::SquareRoot,
                    MixLaw::Linear,
                ),
            ] {
                let expected = (low.gains(x).1 * high.gains(x).1).sqrt();
                assert!((law.gains(x).1 - expected).abs() < 1e-12, "{law:?} at {x}");
            }
        }
    }

    #[test]
    fn the_dry_path_is_delayed_to_the_sample() {
        // The stand-in effect delays and inverts. With an even linear mix
        // the two paths cancel, but only if the mixer delays the dry signal
        // by exactly the same number of frames.
        for latency in [0, 1, 7, 239, 1_000] {
            let cancel = |told: usize| {
                let mut mixer = DryWet::<f64>::new()
                    .with_mix(0.5)
                    .with_max_latency(1_001)
                    .with_latency(told);
                mixer.prepare(&spec(2, 256));
                let mut effect = Late::new(2, latency, -1.0);
                let mut signal = noise(2, 4_000);
                run(&mut mixer, &mut signal, 256, |piece| {
                    if latency > 0 {
                        effect.process(piece);
                    } else {
                        piece.apply_gain(-1.0);
                    }
                });
                strict_buffer_peak(&signal)
            };
            assert_eq!(cancel(latency), 0.0, "latency {latency}");
            // One frame out either way and the cancellation is gone.
            assert!(cancel(latency + 1) > 0.5, "latency {latency}, one late");
            if latency > 0 {
                assert!(cancel(latency - 1) > 0.5, "latency {latency}, one early");
            }
        }
    }

    #[test]
    fn it_lines_up_with_a_real_processor_that_reports_latency() {
        // Under its ceiling a limiter does nothing but delay.
        let mut limiter = Limiter::<f64>::new().with_ceiling_db(6.0);
        limiter.prepare(&spec(2, 512));
        let latency = limiter.latency_samples();
        assert_eq!(latency, 239);

        let mut mixer = DryWet::<f64>::new().with_mix(0.5).with_latency(latency);
        mixer.prepare(&spec(2, 512));
        let original = noise(2, 6_000);
        let mut signal = original.clone();
        run(&mut mixer, &mut signal, 512, |piece| limiter.process(piece));

        // Half of the delayed dry signal plus half of the same thing from
        // the limiter is the delayed signal itself, bit for bit.
        for channel in 0..2 {
            assert_eq!(signal.channel(channel)[..latency], vec![0.0; latency][..]);
            assert_eq!(
                signal.channel(channel)[latency..],
                original.channel(channel)[..6_000 - latency]
            );
        }
    }

    #[test]
    fn all_wet_and_all_dry_are_exact() {
        let original = noise(2, 2_000);

        // All wet: the effect's output comes through untouched.
        let mut mixer = DryWet::<f64>::new().with_latency(10);
        mixer.prepare(&spec(2, 128));
        let mut signal = original.clone();
        run(&mut mixer, &mut signal, 128, |piece| piece.apply_gain(0.25));
        let mut expected = original.clone();
        expected.apply_gain(0.25);
        assert!(signal == expected);

        // All dry: the input comes through, late by the latency, and what
        // the effect put out does not matter, even if it is not a number.
        for law in MixLaw::ALL {
            let mut mixer = DryWet::<f64>::new()
                .with_law(law)
                .with_mix(0.0)
                .with_latency(10);
            mixer.prepare(&spec(2, 128));
            let mut signal = original.clone();
            run(&mut mixer, &mut signal, 128, |piece| {
                for channel in piece.iter_channels_mut() {
                    channel.fill(f64::NAN);
                }
            });
            for channel in 0..2 {
                assert_eq!(signal.channel(channel)[..10], [0.0; 10]);
                assert_eq!(
                    signal.channel(channel)[10..],
                    original.channel(channel)[..1_990],
                    "{law:?}"
                );
            }
        }
    }

    #[test]
    fn a_change_of_mix_is_a_ramp_of_the_stated_length() {
        // 10 ms at 48 kHz is 480 frames.
        let mut mixer = DryWet::<f64>::new().with_mix(0.0).with_ramp(0.01);
        mixer.prepare(&spec(1, 1_024));

        // A dry signal of one and a wet signal of minus one: the output is
        // the dry gain minus the wet gain.
        let mut settled = AudioBuffer::<f64>::new(1, 64);
        settled.channel_mut(0).fill(1.0);
        run(&mut mixer, &mut settled, 64, |piece| piece.apply_gain(-1.0));
        assert_eq!(settled.channel(0), &[1.0; 64]);

        mixer.set_mix(1.0);
        let mut signal = AudioBuffer::<f64>::new(1, 1_000);
        signal.channel_mut(0).fill(1.0);
        run(&mut mixer, &mut signal, 64, |piece| piece.apply_gain(-1.0));
        let out = signal.channel(0);
        for (frame, &sample) in out.iter().enumerate() {
            let expected = if frame < 480 {
                1.0 - 2.0 * (frame + 1) as f64 / 480.0
            } else {
                -1.0
            };
            assert!((sample - expected).abs() < 1e-12, "frame {frame}: {sample}");
        }
        // It lands exactly, and never moves by more than one step.
        assert_eq!(out[479], -1.0);
        let largest = out
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f64::max);
        assert!(largest < 2.0 / 480.0 + 1e-12);

        // A change of law glides in the same way.
        mixer.set_mix(0.5);
        let mut glide = AudioBuffer::<f64>::new(1, 480);
        run(&mut mixer, &mut glide, 480, |_| {});
        mixer.set_law(MixLaw::Balanced);
        let mut signal = AudioBuffer::<f64>::new(1, 600);
        signal.channel_mut(0).fill(1.0);
        run(&mut mixer, &mut signal, 600, |_| {});
        let out = signal.channel(0);
        // Dry and wet are both one here, so the output is the sum of the
        // gains: from 0.5 + 0.5 to 1 + 1.
        assert!((out[0] - (1.0 + 1.0 / 480.0)).abs() < 1e-12);
        assert!(out.windows(2).all(|pair| pair[1] >= pair[0]));
        assert_eq!(out[479], 2.0);
        assert_eq!(out[599], 2.0);
    }

    #[test]
    fn a_jump_of_the_mix_moves_the_gains_in_a_straight_line_whatever_the_law() {
        // The limit stated in the documentation: it is the gains that
        // glide, not the position on the law's curve. 10 ms is 480 frames.
        for law in MixLaw::ALL {
            let mut mixer = DryWet::<f64>::new()
                .with_law(law)
                .with_mix(0.0)
                .with_ramp(0.01);
            mixer.prepare(&spec(1, 480));
            mixer.set_mix(1.0);
            // A dry signal of one and a wet signal of three.
            let mut signal = AudioBuffer::<f64>::new(1, 480);
            signal.channel_mut(0).fill(1.0);
            run(&mut mixer, &mut signal, 480, |piece| piece.apply_gain(3.0));
            // Half-way each path is at half level, which is the centre of
            // the linear law only.
            assert!((signal.channel(0)[239] - 2.0).abs() < 1e-12, "{law:?}");
            assert_eq!(signal.channel(0)[479], 3.0, "{law:?}");
        }
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let render = |block: usize| {
            let mut mixer = DryWet::<f64>::new()
                .with_law(MixLaw::Sine4Point5dB)
                .with_mix(0.3)
                .with_latency(37);
            mixer.prepare(&spec(2, 1_024));
            let mut effect = Late::new(2, 37, 0.8);
            let original = noise(2, 3_000);
            let mut signal = original.clone();
            // The same changes on the same frames, whatever the block size.
            let mut start = 0;
            for (end, mix, law) in [
                (1_000, 0.9, MixLaw::Sine4Point5dB),
                (1_100, 0.1, MixLaw::EqualPower),
                (3_000, 0.6, MixLaw::Linear),
            ] {
                signal.windowed(start..end, |part| {
                    run(&mut mixer, part, block, |piece| effect.process(piece));
                });
                mixer.set_mix(mix);
                mixer.set_law(law);
                start = end;
            }
            signal
        };
        let reference = render(1_024);
        assert!(reference.peak() > 0.5);
        for block in [1, 7, 64, 997] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    #[test]
    fn reset_forgets_the_dry_signal() {
        let mut mixer = DryWet::<f64>::new().with_mix(0.0).with_latency(50);
        mixer.prepare(&spec(1, 256));
        let mut signal = noise(1, 256);
        run(&mut mixer, &mut signal, 256, |_| {});
        mixer.reset();
        let mut silence = AudioBuffer::<f64>::new(1, 256);
        run(&mut mixer, &mut silence, 256, |_| {});
        assert_eq!(strict_buffer_peak(&silence), 0.0);
    }

    /// Found in review. An effect's latency is usually known only after it
    /// has been prepared, so telling the mixer after its own `prepare` is an
    /// easy order to fall into. `with_latency` then raised the latency the
    /// mixer reported without there being memory for it: prepared for blocks
    /// of 256 with no latency and then given 239, the mixer reported 239,
    /// and 3,585 of the 3,857 dry samples compared were not the input
    /// delayed by 239 samples, because they were read from slots the current
    /// block had already overwritten.
    #[test]
    fn the_dry_signal_is_delayed_by_the_latency_the_mixer_reports() {
        let mut mixer = DryWet::<f64>::new().with_mix(0.0);
        mixer.prepare(&spec(1, 256));
        let mut mixer = mixer.with_latency(239);
        let latency = mixer.latency();

        let original = noise(1, 4_096);
        let mut signal = original.clone();
        // The effect is left out: all dry, so the output is the dry path
        // alone.
        run(&mut mixer, &mut signal, 256, |_| {});

        let wrong = (latency..4_096)
            .filter(|&n| signal.channel(0)[n] != original.channel(0)[n - latency])
            .count();
        assert_eq!(
            wrong,
            0,
            "the mixer reports a latency of {latency}, but {wrong} of {} dry samples \
             are not the input delayed by that much",
            4_096 - latency
        );
        // There is no memory for a delay yet, and the mixer says so.
        assert_eq!(latency, 0);
        assert_eq!(mixer.max_latency(), 239);
    }

    /// Runs noise through an all-dry mixer in blocks of `block` frames and
    /// returns how many output samples are not the input delayed by `delay`
    /// frames, with silence before the input starts.
    fn misplaced(mixer: &mut DryWet<f64>, block: usize, delay: usize) -> usize {
        mixer.set_mix(0.0);
        mixer.reset();
        let original = noise(2, 4_000);
        let mut signal = original.clone();
        run(mixer, &mut signal, block, |_| {});
        (0..2)
            .map(|channel| {
                let (output, input) = (signal.channel(channel), original.channel(channel));
                (0..4_000)
                    .filter(|&n| output[n] != if n < delay { 0.0 } else { input[n - delay] })
                    .count()
            })
            .sum()
    }

    #[test]
    fn a_latency_given_after_prepare_is_applied_as_far_as_the_memory_goes() {
        // Told by the builder after `prepare`: remembered, and applied in
        // full by the next `prepare`.
        let mut mixer = DryWet::<f64>::new();
        mixer.prepare(&spec(2, 256));
        let mut mixer = mixer.with_latency(239);
        assert_eq!((mixer.latency(), mixer.max_latency()), (0, 239));
        assert_eq!(misplaced(&mut mixer, 256, 0), 0);
        mixer.prepare(&spec(2, 256));
        assert_eq!(mixer.latency(), 239);
        assert_eq!(misplaced(&mut mixer, 256, 239), 0);
        // One frame either way is a different signal: the check can tell.
        assert!(misplaced(&mut mixer, 256, 238) > 7_000);
        assert!(misplaced(&mut mixer, 256, 240) > 7_000);

        // Told by the setters, in the order a plugin's own `prepare` would
        // use: nothing is lost, whichever of the two comes first.
        for latency_first in [false, true] {
            let mut mixer = DryWet::<f64>::new();
            mixer.prepare(&spec(2, 64));
            if latency_first {
                mixer.set_latency(1_000);
                // No room yet.
                assert_eq!(mixer.latency(), 0);
                mixer.set_max_latency(1_000);
            } else {
                mixer.set_max_latency(1_000);
                mixer.set_latency(1_000);
            }
            // Still no room: a new maximum needs a `prepare`.
            assert_eq!(mixer.latency(), 0);
            assert_eq!(misplaced(&mut mixer, 64, 0), 0);
            mixer.prepare(&spec(2, 64));
            assert_eq!(mixer.latency(), 1_000);
            assert_eq!(misplaced(&mut mixer, 64, 1_000), 0);
        }

        // More than there is room for: as much as the memory holds, exactly
        // at the edge of it, with blocks of the largest size and of others.
        let mut mixer = DryWet::<f64>::new().with_max_latency(100);
        mixer.prepare(&spec(2, 156));
        mixer.set_latency(5_000);
        assert_eq!(mixer.latency(), 100);
        for block in [156, 1, 37, 155] {
            assert_eq!(misplaced(&mut mixer, block, 100), 0, "blocks of {block}");
        }
        // Less again, and back up: within the memory, any latency is
        // applied at once.
        mixer.set_latency(3);
        assert_eq!(mixer.latency(), 3);
        assert_eq!(misplaced(&mut mixer, 156, 3), 0);
        mixer.set_latency(100);
        assert_eq!(misplaced(&mut mixer, 156, 100), 0);

        // A lower maximum does not take memory away before the next
        // `prepare`, and then it does.
        mixer.set_max_latency(10);
        assert_eq!(mixer.latency(), 100);
        assert_eq!(misplaced(&mut mixer, 156, 100), 0);
        mixer.prepare(&spec(2, 156));
        assert_eq!(mixer.latency(), 10);
        assert_eq!(misplaced(&mut mixer, 156, 10), 0);

        // Before the first `prepare` the mixer reports what that `prepare`
        // will set up.
        let mixer = DryWet::<f64>::new().with_latency(77);
        assert_eq!((mixer.latency(), mixer.max_latency()), (77, 77));
        let mixer = mixer.with_max_latency(50);
        assert_eq!((mixer.latency(), mixer.max_latency()), (50, 50));
        let mut mixer = mixer.with_max_latency(80);
        assert_eq!(mixer.latency(), 77);
        mixer.prepare(&spec(1, 16));
        assert_eq!(mixer.latency(), 77);
    }

    #[test]
    fn awkward_calls_do_not_panic() {
        // Not prepared: there are no channels to mix, so the block is left
        // alone.
        let mut mixer = DryWet::<f32>::new().with_mix(0.5);
        let mut buffer = AudioBuffer::<f32>::new(2, 64);
        buffer.channel_mut(0).fill(1.0);
        mixer.push_dry(&buffer);
        mixer.mix_wet(&mut buffer);
        assert_eq!(buffer.channel(0), &[1.0; 64]);

        // More channels and more frames than prepared for, a latency above
        // the maximum, mixing twice, and mixing more frames than captured.
        let mut mixer = DryWet::<f32>::new().with_mix(0.5).with_max_latency(8);
        mixer.prepare(&spec(1, 16));
        mixer.set_latency(1_000);
        assert_eq!(mixer.latency(), 8);
        let mut buffer = AudioBuffer::<f32>::new(3, 100);
        for channel in buffer.iter_channels_mut() {
            channel.fill(1.0);
        }
        mixer.push_dry(&buffer);
        mixer.mix_wet(&mut buffer);
        mixer.mix_wet(&mut buffer);
        buffer.set_num_frames(10);
        mixer.push_dry(&buffer);
        buffer.set_num_frames(100);
        mixer.mix_wet(&mut buffer);
        // Every sample is a number, and none is larger than what went in:
        // the gains of an even linear mix add up to one.
        assert!(strict_buffer_peak(&buffer) <= 1.0);
        // The channel it was prepared for has been mixed,
        assert!(strict_peak(buffer.channel(0)) > 0.0);
        // and the ones it was not prepared for are untouched.
        assert_eq!(buffer.channel(1), &[1.0; 100]);
        assert_eq!(buffer.channel(2), &[1.0; 100]);

        // No channels at all still moves the ramp on.
        let mut mixer = DryWet::<f64>::new().with_mix(0.0).with_ramp(0.001);
        mixer.prepare(&spec(1, 64));
        mixer.set_mix(1.0);
        let mut empty = AudioBuffer::<f64>::new(0, 64);
        mixer.push_dry(&empty);
        mixer.mix_wet(&mut empty);
        let mut buffer = AudioBuffer::<f64>::new(1, 4);
        buffer.channel_mut(0).fill(1.0);
        run(&mut mixer, &mut buffer, 4, |piece| piece.apply_gain(3.0));
        // The 48 frames of ramp are over: all wet.
        assert_eq!(buffer.channel(0), &[3.0; 4]);
    }
}
