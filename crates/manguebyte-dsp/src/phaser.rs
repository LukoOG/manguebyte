use core::f64::consts::{PI, TAU};
use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::numeric::{glide_to, limit, step_towards};
use crate::{ProcessSpec, Processor};

/// Fewest and most all-pass stages a [`Phaser`] can have.
const MIN_STAGES: usize = 2;
const MAX_STAGES: usize = 12;

/// Limits of the settings.
const MIN_HZ: f64 = 10.0;
const MAX_CENTRE_HZ: f64 = 20_000.0;
const MAX_DEPTH_OCTAVES: f64 = 6.0;
const MAX_RATE_HZ: f64 = 100.0;
const MAX_FEEDBACK: f64 = 0.95;

/// How long a change of centre frequency, depth, spread or stage count
/// takes.
const SWEEP_GLIDE_SECONDS: f64 = 0.05;
/// How long a change of feedback or mix takes.
const GAIN_GLIDE_SECONDS: f64 = 0.02;

/// Everything that moves while a block is processed.
///
/// It is `Copy` so that every channel can run a copy of its own over the
/// block and see exactly the same values. The copy of the last channel
/// becomes the new state.
#[derive(Debug, Clone, Copy)]
struct Motion {
    /// Position in the modulation cycle, from 0 up to but not including 1.
    phase: f64,
    /// Centre frequency, as the base-two logarithm of hertz, so that a
    /// glide moves by equal musical steps.
    centre: Smoothed<f64>,
    /// Sweep either side of the centre, in octaves.
    depth: Smoothed<f64>,
    feedback: Smoothed<f64>,
    mix: Smoothed<f64>,
    /// How far the modulation of the last channel would be ahead of the
    /// first if there were one more channel, in cycles.
    spread: Smoothed<f64>,
    /// How much of the output is taken after each stage. All of it comes
    /// from the last stage in use, except while the number of stages is
    /// changing, when the old and the new output are cross-faded.
    weights: [f64; MAX_STAGES],
    total_weight: f64,
    /// How many stages have to run.
    top: usize,
    /// Whether a cross-fade is under way.
    fading: bool,
}

/// The values of [`Motion`] for one sample.
#[derive(Debug, Clone, Copy)]
struct Step {
    phase: f64,
    centre: f64,
    depth: f64,
    feedback: f64,
    mix: f64,
    spread: f64,
}

impl Motion {
    /// Moves on by one sample and returns the values for it.
    #[inline]
    fn step(&mut self, stages: usize, increment: f64, fade_step: f64) -> Step {
        if self.fading {
            self.fade(stages, fade_step);
        }
        let step = Step {
            phase: self.phase,
            centre: self.centre.next_value(),
            depth: self.depth.next_value(),
            feedback: self.feedback.next_value(),
            mix: self.mix.next_value(),
            spread: self.spread.next_value(),
        };
        self.phase += increment;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        step
    }

    fn fade(&mut self, stages: usize, fade_step: f64) {
        let mut total = 0.0;
        let mut moving = false;
        let mut top = stages;
        for (index, weight) in self.weights.iter_mut().enumerate() {
            let target = if index + 1 == stages { 1.0 } else { 0.0 };
            *weight = step_towards(*weight, target, fade_step);
            moving |= *weight != target;
            total += *weight;
            if *weight > 0.0 {
                top = top.max(index + 1);
            }
        }
        self.total_weight = total;
        self.top = top;
        self.fading = moving;
    }
}

/// The memory of one channel.
#[derive(Debug, Clone, Copy)]
struct Channel {
    /// The memory of each all-pass stage.
    stages: [f64; MAX_STAGES],
    /// The last output of the chain, for the feedback.
    feedback: f64,
}

impl Channel {
    const SILENT: Self = Self {
        stages: [0.0; MAX_STAGES],
        feedback: 0.0,
    };
}

/// A phaser: a chain of all-pass filters whose output is mixed with the
/// original signal.
///
/// An all-pass filter changes no level, only the phase, by an amount that
/// grows with frequency. Where the chain has turned the phase by half a
/// turn, the filtered and the original signal cancel, and that leaves a
/// notch. Sweeping the filters moves the notches up and down the spectrum,
/// which is the sound of the effect.
///
/// The chain has 2 to 12 first-order stages, all tuned to the same
/// frequency `f`, which a sine sweeps around the centre frequency on a
/// musical scale: `f = centre * 2^(depth * sin(2π phase))`, with the depth
/// in octaves. With `n` stages, an equal mix and no feedback, the notches
/// are where the phase of the chain is an odd number of half turns:
///
/// ```text
/// f[k] = (fs / π) * atan( tan(π f / fs) * tan((2k + 1) π / (2n)) )     k = 0, 1, ... while 2k + 1 < n
/// ```
///
/// which is one notch for every two stages, the only one of a two-stage
/// phaser being at `f` itself. Feedback returns the output of the chain to
/// its input, one sample late, and turns the gentle humps between the
/// notches into resonant peaks.
///
/// Each stage is a first-order all-pass in normalised form, the single
/// section of the structure in A. H. Gray and J. D. Markel, "A normalized
/// digital filter structure", IEEE Transactions on Acoustics, Speech, and
/// Signal Processing, 1975. Its update is a rotation of the pair made of
/// its input and its memory, so a stage never adds energy, however fast or
/// far its frequency is moved. With feedback the effect therefore cannot run
/// away: the energy that comes out of the chain is at most
/// `1 / (1 - |feedback|)²` times the energy that went in.
///
/// No setting clicks when it changes: the centre frequency, the depth and
/// the spread glide over 50 ms, the feedback and the mix over 20 ms, a
/// change in the number of stages cross-fades between the old output of
/// the chain and the new one over 50 ms, and a change of rate keeps the
/// position in the cycle. A stage that has left the chain is cleared once
/// its cross-fade is over, so it starts from silence if the number of
/// stages is raised again later. All setters are real-time safe.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{Phaser, ProcessSpec, Processor};
///
/// // Two stages held still at 1 kHz: a single notch, at 1 kHz.
/// let mut phaser = Phaser::<f32>::new()
///     .with_stages(2)
///     .with_center_hz(1_000.0)
///     .with_depth_octaves(0.0)
///     .with_mix(0.5);
/// phaser.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
/// }
/// phaser.process(&mut block);
/// // Once the filters have settled, the tone is gone.
/// assert!(block.channel(0)[2_400..].iter().all(|sample| sample.abs() < 1e-3));
/// ```
///
/// # Limits
///
/// The feedback path is one sample long, which shifts the peaks slightly
/// from where an analogue phaser of the same settings has them, more so
/// towards high frequencies. The sweep is limited to between 10 Hz and
/// just under half the sample rate.
///
/// A stage that keeps its energy while its frequency moves does not keep
/// its level. A stage tuned low holds a lot of energy for the low content
/// that passes through it, takes more in while the sweep goes down and
/// gives it back while the sweep goes up. A sweep therefore also modulates
/// the level of what lies far below the notches, by more the faster and
/// deeper it is and the more stages there are, and the output can peak
/// above the input although there is no feedback: the bound above is on
/// energy, not on level. Measured with no dry signal and no feedback, where
/// filters that stood still would leave the level exactly as it is:
///
/// | Signal | Stages | Sweep | Level moves by | Highest peak |
/// |---|---|---|---|---|
/// | 50 Hz tone | 4 | 2 octaves around 1 kHz at 0.5 Hz, the default | 0.09 dB | +0.04 dB |
/// | 50 Hz tone | 4 | the same at 5 Hz | 0.7 dB | +0.4 dB |
/// | 50 Hz tone | 12 | the same at 0.5 Hz | 0.26 dB | +0.13 dB |
/// | 50 Hz tone | 12 | the same at 5 Hz | 2.4 dB | +1.2 dB |
/// | steady level, 0 Hz | 12 | 4 octaves around 640 Hz at 5 Hz | 16.6 dB | +8.2 dB |
/// | 40 Hz tone | 12 | 6 octaves around 1 kHz at 100 Hz, the widest and fastest | 4.1 dB | +14.8 dB |
///
/// The first figure is how far apart the loudest and the quietest cycle of
/// the tone are (the loudest and the quietest 10 ms of the steady level),
/// the second is the highest peak of the output against that of the input.
/// With the default settings this is under a tenth of a decibel; with many
/// stages and a fast, wide sweep it is a tremolo on the bass. Where that
/// matters, use fewer stages or a slower or narrower sweep, or keep the low
/// content out of the effect with a high-pass filter before it.
///
/// The energy bound holds for a fixed number of stages. During the 50 ms
/// in which a change of that number cross-fades, the feedback is taken from
/// a blend of two outputs of the chain, which the bound does not cover.
#[derive(Debug, Clone)]
pub struct Phaser<S: Sample = f32> {
    stages: usize,
    rate_hz: f64,
    depth_octaves: f64,
    centre_hz: f64,
    feedback: f64,
    spread: f64,
    mix: f64,
    sample_rate: f64,
    /// Change in a weight per sample while the stage count is changing.
    fade_step: f64,
    motion: Motion,
    channels: Vec<Channel>,
    marker: PhantomData<S>,
}

impl<S: Sample> Default for Phaser<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> Phaser<S> {
    /// The smallest number of stages.
    pub const MIN_STAGES: usize = MIN_STAGES;
    /// The largest number of stages.
    pub const MAX_STAGES: usize = MAX_STAGES;

    /// Creates a phaser with four stages sweeping two octaves either side
    /// of 1 kHz every two seconds, no feedback, no spread and an equal mix.
    pub fn new() -> Self {
        let mut phaser = Self {
            stages: 4,
            rate_hz: 0.5,
            depth_octaves: 2.0,
            centre_hz: 1_000.0,
            feedback: 0.0,
            spread: 0.0,
            mix: 0.5,
            sample_rate: 44_100.0,
            fade_step: 1.0,
            motion: Motion {
                phase: 0.0,
                centre: Smoothed::new(0.0),
                depth: Smoothed::new(0.0),
                feedback: Smoothed::new(0.0),
                mix: Smoothed::new(0.0),
                spread: Smoothed::new(0.0),
                weights: [0.0; MAX_STAGES],
                total_weight: 1.0,
                top: MIN_STAGES,
                fading: false,
            },
            channels: Vec::new(),
            marker: PhantomData,
        };
        phaser.snap();
        phaser
    }

    /// Sets the number of all-pass stages, limited to `2..=12`. Every two
    /// stages add a notch. The output cross-fades to the new chain.
    pub fn set_stages(&mut self, stages: usize) {
        let stages = stages.clamp(MIN_STAGES, MAX_STAGES);
        if stages != self.stages {
            self.stages = stages;
            self.motion.fading = true;
        }
    }

    /// Sets how many times a second the notches sweep back and forth, in
    /// hertz, limited to `0.0..=100.0`. Zero stops the sweep where it is.
    pub fn set_rate_hz(&mut self, hertz: f64) {
        self.rate_hz = limit(hertz, 0.0, MAX_RATE_HZ);
    }

    /// Sets how far the sweep goes either side of the centre frequency, in
    /// octaves, limited to `0.0..=6.0`. Zero holds the filters at the centre
    /// frequency.
    pub fn set_depth_octaves(&mut self, octaves: f64) {
        self.depth_octaves = limit(octaves, 0.0, MAX_DEPTH_OCTAVES);
        glide_to(&mut self.motion.depth, self.depth_octaves);
    }

    /// Sets the frequency the sweep is centred on, in hertz, limited to
    /// `10.0..=20000.0`. This is the frequency of the all-pass stages, at
    /// which each one turns the phase by a quarter turn.
    pub fn set_center_hz(&mut self, hertz: f64) {
        self.centre_hz = limit(hertz, MIN_HZ, MAX_CENTRE_HZ);
        glide_to(&mut self.motion.centre, self.centre_hz.log2());
    }

    /// Sets how much of the chain's output is fed back into it, limited to
    /// `-0.95..=0.95`. Either sign makes the response between the notches
    /// resonant; a negative value puts the peaks in different places.
    pub fn set_feedback(&mut self, feedback: f64) {
        self.feedback = if feedback.is_nan() {
            0.0
        } else {
            limit(feedback, -MAX_FEEDBACK, MAX_FEEDBACK)
        };
        glide_to(&mut self.motion.feedback, self.feedback);
    }

    /// Sets how far apart the sweeps of the channels are, from `0.0` (every
    /// channel sweeps together) to `1.0` (as far apart as they can be).
    ///
    /// Channel `c` of `n` is `spread * c / n` of a cycle ahead of the first
    /// channel: with two channels, a spread of 1 puts left and right half a
    /// cycle apart, and 0.5 a quarter.
    pub fn set_spread(&mut self, spread: f64) {
        self.spread = limit(spread, 0.0, 1.0);
        glide_to(&mut self.motion.spread, self.spread);
    }

    /// Sets the balance between the untouched signal (`0.0`) and the output
    /// of the all-pass chain (`1.0`). An equal mix gives the deepest
    /// notches; at `1.0` there are none, only a phase that moves, which is
    /// a vibrato.
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = limit(mix, 0.0, 1.0);
        glide_to(&mut self.motion.mix, self.mix);
    }

    /// Builder form of [`set_stages`](Self::set_stages).
    #[must_use]
    pub fn with_stages(mut self, stages: usize) -> Self {
        self.set_stages(stages);
        self
    }

    /// Builder form of [`set_rate_hz`](Self::set_rate_hz).
    #[must_use]
    pub fn with_rate_hz(mut self, hertz: f64) -> Self {
        self.set_rate_hz(hertz);
        self
    }

    /// Builder form of [`set_depth_octaves`](Self::set_depth_octaves).
    #[must_use]
    pub fn with_depth_octaves(mut self, octaves: f64) -> Self {
        self.set_depth_octaves(octaves);
        self
    }

    /// Builder form of [`set_center_hz`](Self::set_center_hz).
    #[must_use]
    pub fn with_center_hz(mut self, hertz: f64) -> Self {
        self.set_center_hz(hertz);
        self
    }

    /// Builder form of [`set_feedback`](Self::set_feedback).
    #[must_use]
    pub fn with_feedback(mut self, feedback: f64) -> Self {
        self.set_feedback(feedback);
        self
    }

    /// Builder form of [`set_spread`](Self::set_spread).
    #[must_use]
    pub fn with_spread(mut self, spread: f64) -> Self {
        self.set_spread(spread);
        self
    }

    /// Builder form of [`set_mix`](Self::set_mix).
    #[must_use]
    pub fn with_mix(mut self, mix: f64) -> Self {
        self.set_mix(mix);
        self
    }

    /// The number of stages, as last set.
    pub fn stages(&self) -> usize {
        self.stages
    }

    /// Jumps every gliding value to where it is heading.
    fn snap(&mut self) {
        let motion = &mut self.motion;
        motion.centre.set_immediate(self.centre_hz.log2());
        motion.depth.set_immediate(self.depth_octaves);
        motion.spread.set_immediate(self.spread);
        motion.feedback.set_immediate(self.feedback);
        motion.mix.set_immediate(self.mix);
        for (index, weight) in motion.weights.iter_mut().enumerate() {
            *weight = if index + 1 == self.stages { 1.0 } else { 0.0 };
        }
        motion.total_weight = 1.0;
        motion.top = self.stages;
        motion.fading = false;
    }
}

impl<S: Sample> Processor<S> for Phaser<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        let glide = SWEEP_GLIDE_SECONDS * spec.sample_rate;
        self.fade_step = if glide >= 1.0 { 1.0 / glide } else { 1.0 };
        self.channels = vec![Channel::SILENT; spec.num_channels];

        let motion = &mut self.motion;
        for value in [&mut motion.centre, &mut motion.depth, &mut motion.spread] {
            value.set_ramp(spec.sample_rate, SWEEP_GLIDE_SECONDS);
        }
        for value in [&mut motion.feedback, &mut motion.mix] {
            value.set_ramp(spec.sample_rate, GAIN_GLIDE_SECONDS);
        }
        motion.phase = 0.0;
        self.snap();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let increment = self.rate_hz / self.sample_rate;
        let (stages, fade_step) = (self.stages, self.fade_step);
        let channel_count = self.channels.len().max(1) as f64;
        let highest = 0.49 * self.sample_rate;
        let lowest = MIN_HZ.min(highest);
        let radians_per_hertz = PI / self.sample_rate;

        let mut after = self.motion;
        let mut ran = false;
        for (index, (samples, channel)) in buffer
            .iter_channels_mut()
            .zip(&mut self.channels)
            .enumerate()
        {
            let mut motion = self.motion;
            let position = index as f64 / channel_count;
            for sample in samples {
                let step = motion.step(stages, increment, fade_step);
                let sweep = (TAU * (step.phase + step.spread * position)).sin();
                let frequency = limit((step.centre + step.depth * sweep).exp2(), lowest, highest);
                // The all-pass (a + z^-1) / (1 + a z^-1) as a rotation of
                // input and state: with g = tan(π f / fs),
                //   a = (g - 1) / (g + 1),  c = sqrt(1 - a²) = 2 sqrt(g) / (g + 1),
                //   y = a x + c s,  s' = c x - a s.
                let g = (radians_per_hertz * frequency).tan();
                let a = (g - 1.0) / (g + 1.0);
                let c = 2.0 * g.sqrt() / (g + 1.0);

                let input = sample.to_f64();
                let mut signal = input + step.feedback * channel.feedback;
                let mut blended = 0.0;
                let running = channel.stages[..motion.top].iter_mut();
                for (state, &weight) in running.zip(&motion.weights) {
                    let output = a * signal + c * *state;
                    *state = c * signal - a * *state;
                    signal = output;
                    blended += weight * signal;
                }
                let shifted = if motion.fading {
                    blended / motion.total_weight
                } else {
                    signal
                };
                channel.feedback = shifted;
                *sample = S::from_f64(input * (1.0 - step.mix) + shifted * step.mix);
            }
            after = motion;
            ran = true;
        }
        if !ran {
            // No channel to process, but time still passes.
            for _ in 0..buffer.num_frames() {
                after.step(stages, increment, fade_step);
            }
        }
        self.motion = after;

        // A stage that is no longer part of the chain forgets what it held.
        // Otherwise it would start from that memory when the number of
        // stages is raised again, and put out the audio of a moment that may
        // be long past. A stage is only ever brought back by the first step
        // of a later block, so clearing here, and not on the very sample it
        // dropped out, changes nothing that can be heard. Every channel is
        // cleared, including any this block did not carry.
        for channel in &mut self.channels {
            for state in channel.stages.iter_mut().skip(after.top) {
                *state = 0.0;
            }
        }
    }

    fn reset(&mut self) {
        self.channels.fill(Channel::SILENT);
        self.motion.phase = 0.0;
        self.snap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{Complex, RealFft};
    use crate::numeric::{strict_buffer_peak, strict_peak};

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    fn prepared(phaser: Phaser<f64>, channels: usize) -> Phaser<f64> {
        let mut phaser = phaser;
        phaser.prepare(&spec(channels));
        phaser
    }

    /// Runs `signal` through the phaser in blocks of `block` frames.
    fn run(phaser: &mut Phaser<f64>, signal: &mut AudioBuffer<f64>, block: usize) {
        let frames = signal.num_frames();
        let mut start = 0;
        while start < frames {
            let end = (start + block).min(frames);
            signal.windowed(start..end, |piece| phaser.process(piece));
            start = end;
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

    /// A sine of amplitude 0.5 on every channel.
    fn sine(channels: usize, frames: usize, frequency: f64) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(channels, frames);
        for channel in buffer.iter_channels_mut() {
            for (n, sample) in channel.iter_mut().enumerate() {
                *sample = 0.5 * (TAU * frequency * n as f64 / RATE).sin();
            }
        }
        buffer
    }

    /// A phaser held still: no sweep, no feedback, an equal mix.
    fn still(stages: usize, centre_hz: f64) -> Phaser<f64> {
        Phaser::new()
            .with_stages(stages)
            .with_center_hz(centre_hz)
            .with_depth_octaves(0.0)
            .with_rate_hz(0.0)
            .with_feedback(0.0)
            .with_spread(0.0)
            .with_mix(0.5)
    }

    /// The impulse response of every channel.
    fn impulse_response(phaser: &mut Phaser<f64>, channels: usize, len: usize) -> AudioBuffer<f64> {
        let mut impulse = AudioBuffer::<f64>::new(channels, len);
        for channel in impulse.iter_channels_mut() {
            channel[0] = 1.0;
        }
        run(phaser, &mut impulse, 512);
        // The whole response has to be in view for what follows to hold.
        for channel in impulse.iter_channels() {
            let tail = channel[len - 256..]
                .iter()
                .fold(0.0f64, |peak, sample| peak.max(sample.abs()));
            assert!(tail < 1e-13, "the response has not died away: {tail}");
        }
        impulse
    }

    /// The response at one frequency, as real and imaginary parts, found by
    /// summing the impulse response against a complex sine. There are no
    /// bins here, so any frequency can be asked about.
    fn response_at(impulse_response: &[f64], frequency: f64) -> (f64, f64) {
        let omega = TAU * frequency / RATE;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &sample) in impulse_response.iter().enumerate() {
            let (sin, cos) = (omega * n as f64).sin_cos();
            re += sample * cos;
            im -= sample * sin;
        }
        (re, im)
    }

    fn gain_at(impulse_response: &[f64], frequency: f64) -> f64 {
        let (re, im) = response_at(impulse_response, frequency);
        re.hypot(im)
    }

    /// Where theory puts notch `k` of `stages` stages tuned to `tuned_hz`.
    fn notch_hz(stages: usize, tuned_hz: f64, k: usize) -> f64 {
        let g = (PI * tuned_hz / RATE).tan();
        let quarter_turns = (2 * k + 1) as f64 * PI / (2 * stages) as f64;
        RATE / PI * (g * quarter_turns.tan()).atan()
    }

    /// What theory says the whole effect does at `frequency`: the chain of
    /// `stages` all-pass filters tuned to `tuned_hz` is `e^(i n θ)`, with
    /// `θ = -2 atan(tan(ω/2) / tan(ω_c/2))`, and
    ///   H = (1 - mix) + mix * A / (1 - feedback * e^(-iω) * A).
    fn theory(stages: usize, tuned_hz: f64, feedback: f64, mix: f64, frequency: f64) -> (f64, f64) {
        let omega = TAU * frequency / RATE;
        let g = (PI * tuned_hz / RATE).tan();
        let turn = -2.0 * stages as f64 * ((omega / 2.0).tan() / g).atan();
        let chain = (turn.cos(), turn.sin());
        // The feedback path: one sample late.
        let back = (
            feedback * (chain.0 * omega.cos() + chain.1 * omega.sin()),
            feedback * (chain.1 * omega.cos() - chain.0 * omega.sin()),
        );
        let (re, im) = (1.0 - back.0, -back.1);
        let scale = mix / (re * re + im * im);
        (
            1.0 - mix + (chain.0 * re + chain.1 * im) * scale,
            (chain.1 * re - chain.0 * im) * scale,
        )
    }

    #[test]
    fn the_notches_are_where_theory_puts_them() {
        const LEN: usize = 1 << 14;
        for centre in [300.0, 1_000.0, 4_000.0] {
            for stages in [2, 3, 4, 5, 6, 8, 12] {
                let mut phaser = prepared(still(stages, centre), 1);
                assert_eq!(phaser.stages(), stages);
                let response = impulse_response(&mut phaser, 1, LEN);
                let response = response.channel(0);

                // At every frequency theory names, the output is gone.
                let expected: Vec<f64> = (0..stages / 2)
                    .map(|k| notch_hz(stages, centre, k))
                    .collect();
                for &frequency in &expected {
                    let gain = gain_at(response, frequency);
                    assert!(
                        gain < 1e-9,
                        "{stages} stages at {centre} Hz: {gain} left at {frequency} Hz"
                    );
                }
                // A two-stage phaser has its one notch on the centre itself.
                if stages == 2 {
                    assert!((expected[0] - centre).abs() < 1e-9);
                }

                // And there are no others. Look at the whole spectrum for
                // dips: there must be one per predicted notch, on the bin
                // nearest to it.
                let mut fft = RealFft::<f64>::new(LEN);
                let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
                fft.forward(response, &mut spectrum);
                let gains: Vec<f64> = spectrum.iter().map(|bin| bin.norm()).collect();
                let dips: Vec<usize> = (1..gains.len() - 1)
                    .filter(|&bin| {
                        gains[bin] < 0.1
                            && gains[bin] < gains[bin - 1]
                            && gains[bin] <= gains[bin + 1]
                    })
                    .collect();
                assert_eq!(
                    dips.len(),
                    expected.len(),
                    "{stages} stages at {centre} Hz: dips at bins {dips:?}"
                );
                for (&bin, &frequency) in dips.iter().zip(&expected) {
                    let found = bin as f64 * RATE / LEN as f64;
                    assert!(
                        (found - frequency).abs() <= RATE / LEN as f64,
                        "{stages} stages at {centre} Hz: dip at {found} Hz, not {frequency} Hz"
                    );
                }

                // Half-way round between two notches the two signals add up
                // in step, and the level is back to one.
                for k in 0..stages / 2 {
                    let g = (PI * centre / RATE).tan();
                    let turns = 2.0 * k as f64 * PI / (2 * stages) as f64;
                    let frequency = RATE / PI * (g * turns.tan()).atan();
                    let gain = gain_at(response, frequency);
                    assert!((gain - 1.0).abs() < 1e-9, "{gain} at {frequency} Hz");
                }
            }
        }
    }

    #[test]
    fn the_whole_response_matches_theory_with_and_without_feedback() {
        for (stages, feedback, mix) in [
            (4, 0.0, 0.5),
            (4, 0.7, 0.5),
            (4, -0.7, 0.5),
            (6, 0.95, 0.5),
            (6, -0.95, 0.35),
            (12, 0.5, 1.0),
            (3, 0.6, 0.8),
            // All wet and no feedback: an all-pass, level one everywhere.
            (8, 0.0, 1.0),
        ] {
            let mut phaser = prepared(
                still(stages, 800.0).with_feedback(feedback).with_mix(mix),
                1,
            );
            // Strong feedback rings for a long time, and all of it has to be
            // measured.
            let len = if feedback.abs() > 0.9 {
                1 << 17
            } else {
                1 << 15
            };
            let response = impulse_response(&mut phaser, 1, len);
            let response = response.channel(0);
            // From 20 Hz to 20 kHz in thirds of an octave.
            for third in 0..=30 {
                let frequency = 20.0 * 2f64.powf(f64::from(third) / 3.0);
                let measured = response_at(response, frequency);
                let expected = theory(stages, 800.0, feedback, mix, frequency);
                assert!(
                    (measured.0 - expected.0).abs() < 1e-9
                        && (measured.1 - expected.1).abs() < 1e-9,
                    "{stages} stages, feedback {feedback}, mix {mix}, {frequency} Hz: \
                     {measured:?} against {expected:?}"
                );
                if feedback == 0.0 && mix == 1.0 {
                    assert!((measured.0.hypot(measured.1) - 1.0).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn depth_and_spread_move_the_notch_by_the_stated_amount() {
        // With the rate at zero the sweep stands still at the start of its
        // cycle, where the first channel is on the centre frequency. A
        // channel that is ahead by part of a cycle is held somewhere else:
        // at centre * 2^(depth * sin(2π * ahead)).
        for (channels, spread, depth) in [
            (2usize, 0.5, 1.5),
            (2, 1.0 / 6.0, 1.0),
            (2, 1.0, 2.0),
            (3, 0.75, 1.0),
            (4, 1.0, 0.5),
        ] {
            let mut phaser = prepared(
                still(2, 1_000.0)
                    .with_depth_octaves(depth)
                    .with_spread(spread),
                channels,
            );
            let response = impulse_response(&mut phaser, channels, 1 << 14);
            for channel in 0..channels {
                let ahead = spread * channel as f64 / channels as f64;
                let tuned = 1_000.0 * 2f64.powf(depth * (TAU * ahead).sin());
                let gain = gain_at(response.channel(channel), tuned);
                assert!(
                    gain < 1e-9,
                    "channel {channel} of {channels}, spread {spread}: {gain} left at {tuned} Hz"
                );
                // The notch has really moved: nothing is missing at the
                // centre unless that is where this channel is held.
                if (tuned / 1_000.0).log2().abs() > 0.1 {
                    assert!(gain_at(response.channel(channel), 1_000.0) > 0.05);
                }
            }
        }
    }

    /// The peak of each stretch of `window` frames.
    fn envelope(signal: &[f64], window: usize) -> Vec<f64> {
        signal
            .chunks_exact(window)
            .map(|chunk| {
                chunk
                    .iter()
                    .fold(0.0f64, |peak, sample| peak.max(sample.abs()))
            })
            .collect()
    }

    /// How many separate times the envelope goes below `threshold`.
    fn dips(envelope: &[f64], threshold: f64) -> usize {
        envelope
            .windows(2)
            .filter(|pair| pair[0] >= threshold && pair[1] < threshold)
            .count()
    }

    #[test]
    fn the_sweep_has_the_set_rate_and_covers_the_set_range() {
        // Two stages, so one notch, which is on the frequency the stages
        // are tuned to. It sweeps an octave either side of 1 kHz twice a
        // second, so a steady tone is cancelled each time the notch
        // passes over it.
        let cancelled = |tone: f64, window: usize| {
            let mut phaser = prepared(
                still(2, 1_000.0).with_depth_octaves(1.0).with_rate_hz(2.0),
                1,
            );
            let mut signal = sine(1, 99_000, tone);
            run(&mut phaser, &mut signal, 512);
            // Four cycles of 24,000 frames, starting an eighth of a cycle
            // in, where the notch is well away from all of the tones used
            // here.
            envelope(&signal.channel(0)[3_000..99_000], window)
        };

        // A tone at the centre is crossed twice per cycle: 8 times in the
        // 4 cycles, which is the rate.
        let centre = cancelled(1_000.0, 48);
        assert_eq!(dips(&centre, 0.025), 8);
        // Tones at the two ends of the range are reached once per cycle,
        // at the turning points.
        let top = cancelled(2_000.0, 24);
        assert_eq!(dips(&top, 0.025), 4);
        let bottom = cancelled(500.0, 96);
        assert_eq!(dips(&bottom, 0.025), 4);
        // Tones a quarter of an octave beyond the ends are never reached.
        // At its nearest the notch leaves 17% of them.
        for (tone, window) in [
            (2_000.0 * 2f64.powf(0.25), 24),
            (500.0 / 2f64.powf(0.25), 120),
        ] {
            let outside = cancelled(tone, window);
            let lowest = outside.iter().fold(f64::MAX, |low, &level| low.min(level));
            assert!(
                (lowest - 0.5 * 0.173).abs() < 0.01,
                "{tone} Hz fell to {lowest}"
            );
        }
    }

    /// A change of settings, made at some point in a test.
    type Change = fn(&mut Phaser<f64>);

    /// How far any sample lies from where the two before it were heading:
    /// the size of the largest sudden change. A smooth signal turns a little
    /// from one sample to the next; a click moves the level at once.
    fn largest_jump(signal: &[f64]) -> f64 {
        signal
            .windows(3)
            .map(|three| (three[2] - 2.0 * three[1] + three[0]).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn no_change_of_setting_clicks() {
        // The 220 Hz sine that goes in, of amplitude 0.5, turns by 0.0004
        // from one sample to the next. The effect adds to that: a glide
        // starts and ends with a bend, and feedback makes resonances. A
        // click is of another order: a step the size of the signal.
        const SEGMENT: usize = 9_600;
        let changes: [Change; 12] = [
            |phaser| phaser.set_stages(12),
            |phaser| phaser.set_center_hz(250.0),
            |phaser| phaser.set_stages(2),
            |phaser| phaser.set_depth_octaves(0.5),
            |phaser| phaser.set_feedback(0.7),
            |phaser| phaser.set_stages(7),
            |phaser| phaser.set_spread(1.0),
            |phaser| phaser.set_mix(1.0),
            |phaser| phaser.set_rate_hz(5.0),
            |phaser| phaser.set_feedback(-0.7),
            |phaser| phaser.set_center_hz(3_000.0),
            |phaser| phaser.set_mix(0.4),
        ];
        let mut phaser = prepared(Phaser::new(), 2);
        let mut signal = sine(2, SEGMENT * (changes.len() + 1), 220.0);
        for (index, change) in changes.iter().enumerate() {
            signal.windowed(index * SEGMENT..(index + 1) * SEGMENT, |part| {
                run(&mut phaser, part, 512);
            });
            change(&mut phaser);
        }
        signal.windowed(
            changes.len() * SEGMENT..(changes.len() + 1) * SEGMENT,
            |part| {
                run(&mut phaser, part, 512);
            },
        );
        for channel in signal.iter_channels() {
            let jump = largest_jump(channel);
            assert!(jump < 0.02, "a jump of {jump}");
        }

        // The measurement does see a click when there is one: clearing the
        // filters under a steady level makes their output jump.
        let mut phaser = prepared(still(4, 1_000.0), 1);
        let mut signal = AudioBuffer::<f64>::new(1, 2 * SEGMENT);
        signal.channel_mut(0).fill(0.5);
        signal.windowed(0..SEGMENT, |part| run(&mut phaser, part, 512));
        phaser.reset();
        signal.windowed(SEGMENT..2 * SEGMENT, |part| run(&mut phaser, part, 512));
        let cut = largest_jump(&signal.channel(0)[SEGMENT - 100..SEGMENT + 100]);
        assert!(cut > 0.1, "the cut was a jump of {cut}");
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let render = |block: usize| {
            let mut phaser = prepared(
                Phaser::new()
                    .with_stages(6)
                    .with_rate_hz(3.3)
                    .with_feedback(-0.5)
                    .with_spread(0.6),
                2,
            );
            let mut signal = noise(2, 6_000);
            // The same changes on the same frames, whatever the block size.
            let changes: [(usize, Change); 4] = [
                (1_000, |phaser| phaser.set_stages(11)),
                (1_700, |phaser| {
                    phaser.set_center_hz(300.0);
                    phaser.set_depth_octaves(3.0);
                    phaser.set_spread(0.1);
                    phaser.set_stages(3);
                }),
                (3_100, |phaser| {
                    phaser.set_stages(8);
                    phaser.set_feedback(0.9);
                    phaser.set_mix(0.9);
                    phaser.set_rate_hz(0.2);
                }),
                (6_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run(&mut phaser, part, block));
                change(&mut phaser);
                start = end;
            }
            signal
        };
        let reference = render(512);
        assert!(reference.peak() > 0.5);
        for block in [1, 7, 64, 333, 6_000] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    /// The sum of the squares of every sample.
    fn energy(signal: &AudioBuffer<f64>) -> f64 {
        signal
            .iter_channels()
            .flatten()
            .map(|sample| sample * sample)
            .sum()
    }

    #[test]
    fn the_sweep_never_adds_energy_however_fast_and_far_it_moves() {
        // The widest and fastest sweep there is, over all twelve stages.
        let wildest = || {
            Phaser::<f64>::new()
                .with_stages(12)
                .with_rate_hz(100.0)
                .with_depth_octaves(6.0)
                .with_spread(1.0)
                .with_mix(1.0)
        };
        for centre in [100.0, 1_000.0, 20_000.0] {
            // Without feedback the chain can only hand on the energy it was
            // given: at no moment has more come out than has gone in.
            let mut phaser = prepared(wildest().with_center_hz(centre), 1);
            let original = noise(1, 48_000);
            let mut signal = original.clone();
            run(&mut phaser, &mut signal, 512);
            let (mut energy_in, mut energy_out) = (0.0, 0.0);
            for (input, output) in original.channel(0).iter().zip(signal.channel(0)) {
                energy_in += input * input;
                energy_out += output * output;
                assert!(energy_out <= energy_in * (1.0 + 1e-9), "at {centre} Hz");
            }
            // And nearly all of it has: nothing is lost either.
            assert!(energy_out > 0.99 * energy_in, "at {centre} Hz");

            // With feedback, the loop can multiply the energy by at most
            // 1 / (1 - |feedback|)².
            for feedback in [0.95, -0.95, 0.5] {
                let mut phaser =
                    prepared(wildest().with_center_hz(centre).with_feedback(feedback), 2);
                let original = noise(2, 96_000);
                let mut signal = original.clone();
                run(&mut phaser, &mut signal, 512);
                let gain = energy(&signal) / energy(&original);
                let bound = 1.0 / (1.0 - feedback.abs()).powi(2);
                assert!(
                    gain <= bound,
                    "at {centre} Hz with feedback {feedback}: energy times {gain}"
                );
            }
        }

        // A steady tone held on a resonance reaches that bound: a level of
        // 1 / (1 - 0.95), 26 dB. The chain passes zero hertz unchanged, so
        // with positive feedback zero hertz is a resonance.
        let mut phaser = prepared(still(4, 1_000.0).with_feedback(0.95).with_mix(1.0), 1);
        let mut level = AudioBuffer::<f64>::new(1, 96_000);
        level.channel_mut(0).fill(0.5);
        run(&mut phaser, &mut level, 512);
        assert!((level.channel(0)[95_999] - 0.5 * 20.0).abs() < 1e-6);
    }

    #[test]
    fn an_ordinary_sweep_barely_touches_what_is_far_below_it() {
        // A 50 Hz tone through four stages that sweep between 250 Hz and
        // 4 kHz, with no dry signal. Its level over each of its cycles
        // would be steady if the sweep left it alone.
        let ripple_db = |rate: f64| {
            let mut phaser = prepared(
                Phaser::new()
                    .with_stages(4)
                    .with_rate_hz(rate)
                    .with_depth_octaves(2.0)
                    .with_mix(1.0),
                1,
            );
            let mut signal = sine(1, 240_000, 50.0);
            run(&mut phaser, &mut signal, 512);
            let levels = envelope(&signal.channel(0)[48_000..], 960);
            let lowest = levels.iter().fold(f64::MAX, |low, &level| low.min(level));
            let highest = levels.iter().fold(0.0f64, |high, &level| high.max(level));
            20.0 * (highest / lowest).log10()
        };
        // The figures quoted in the documentation.
        let slow = ripple_db(0.5);
        assert!((slow - 0.09).abs() < 0.02, "{slow} dB at 0.5 Hz");
        let fast = ripple_db(5.0);
        assert!((fast - 0.7).abs() < 0.1, "{fast} dB at 5 Hz");
    }

    /// What a sweep does to the level of `signal`, with no dry signal and no
    /// feedback: a chain of all-pass filters that stood still would leave
    /// the level of a steady signal exactly as it is. Returns, in decibels,
    /// how far apart the loudest and the quietest stretch of `window` frames
    /// are, and the highest peak against that of the input. The first second
    /// is left out.
    fn level_under_a_sweep(
        phaser: Phaser<f64>,
        mut signal: AudioBuffer<f64>,
        window: usize,
    ) -> (f64, f64) {
        let mut phaser = prepared(phaser.with_feedback(0.0).with_mix(1.0), 1);
        let input_peak = strict_buffer_peak(&signal);
        run(&mut phaser, &mut signal, 512);
        assert!(strict_buffer_peak(&signal).is_finite());
        let levels = envelope(&signal.channel(0)[48_000..], window);
        let lowest = levels.iter().fold(f64::MAX, |low, &level| low.min(level));
        let highest = levels.iter().fold(0.0f64, |high, &level| high.max(level));
        (
            20.0 * (highest / lowest).log10(),
            20.0 * (highest / input_peak).log10(),
        )
    }

    #[test]
    fn a_fast_sweep_of_many_stages_moves_the_level_of_low_content() {
        // The table in the documentation, row by row. A 50 Hz tone has
        // cycles of 960 frames and a 40 Hz tone of 1,200.
        let tone = |frequency: f64| sine(1, 240_000, frequency);
        let around_1_khz = |stages: usize, rate: f64| {
            Phaser::<f64>::new()
                .with_stages(stages)
                .with_rate_hz(rate)
                .with_depth_octaves(2.0)
        };
        let near = |measured: (f64, f64), moves: f64, peak: f64, within: f64| {
            (measured.0 - moves).abs() < within && (measured.1 - peak).abs() < within
        };

        // Each figure is held to a little more than its last digit.
        let measured = level_under_a_sweep(around_1_khz(4, 0.5), tone(50.0), 960);
        assert!(near(measured, 0.09, 0.04, 0.01), "{measured:?}");
        let measured = level_under_a_sweep(around_1_khz(4, 5.0), tone(50.0), 960);
        assert!(near(measured, 0.7, 0.4, 0.06), "{measured:?}");
        let measured = level_under_a_sweep(around_1_khz(12, 0.5), tone(50.0), 960);
        assert!(near(measured, 0.26, 0.13, 0.01), "{measured:?}");
        let measured = level_under_a_sweep(around_1_khz(12, 5.0), tone(50.0), 960);
        assert!(near(measured, 2.4, 1.2, 0.06), "{measured:?}");

        // A steady level through twelve stages that sweep four octaves
        // either side of 640 Hz five times a second: it comes out at up to
        // 2.6 times what went in.
        let mut steady = AudioBuffer::<f64>::new(1, 240_000);
        steady.channel_mut(0).fill(0.5);
        let measured = level_under_a_sweep(
            around_1_khz(12, 5.0)
                .with_center_hz(640.0)
                .with_depth_octaves(4.0),
            steady,
            480,
        );
        assert!(near(measured, 16.6, 8.2, 0.1), "{measured:?}");

        // The widest and fastest sweep there is: 5.5 times.
        let measured = level_under_a_sweep(
            around_1_khz(12, 100.0).with_depth_octaves(6.0),
            tone(40.0),
            1_200,
        );
        assert!(near(measured, 4.1, 14.8, 0.1), "{measured:?}");
    }

    /// Found in review: stages above the number in use stopped being
    /// updated when their cross-fade ended, and kept the signal they held.
    /// Raising the number later ran them from that memory. Noise through
    /// twelve stages, down to two while it played, five seconds of silence,
    /// then back to twelve with silence still going in: a burst that peaked
    /// at 0.0062 with the default sweep, and at 0.023 with the filters held
    /// at 60 Hz.
    #[test]
    fn stages_that_come_back_do_not_replay_old_audio() {
        // The default sweep with and without the dry signal, and the filters
        // held still at two low frequencies, where they hold the most.
        let settings: [fn() -> Phaser<f64>; 4] = [
            || Phaser::new().with_stages(12).with_mix(1.0),
            || Phaser::new().with_stages(12).with_mix(0.5),
            || still(12, 200.0).with_mix(1.0),
            || still(12, 60.0).with_mix(1.0),
        ];
        for (index, setting) in settings.into_iter().enumerate() {
            let mut phaser = prepared(setting(), 1);
            assert_eq!(phaser.stages(), 12);

            let mut loud = noise(1, 48_000);
            loud.windowed(0..24_000, |part| run(&mut phaser, part, 512));
            phaser.set_stages(2);
            loud.windowed(24_000..48_000, |part| run(&mut phaser, part, 512));

            let mut silence = AudioBuffer::<f64>::new(1, 240_000);
            run(&mut phaser, &mut silence, 512);
            let tail = strict_peak(&silence.channel(0)[192_000..]);
            assert!(
                tail < 1e-12,
                "setting {index}: the phaser has not fallen silent: {tail:e}"
            );

            phaser.set_stages(12);
            let mut after = AudioBuffer::<f64>::new(1, 48_000);
            run(&mut phaser, &mut after, 512);
            let burst = strict_peak(after.channel(0));
            assert!(
                burst < 1e-9,
                "setting {index}: silence in, and a burst of {burst:e} out when the stages came back"
            );
        }
    }

    #[test]
    fn stages_that_come_back_do_so_the_same_way_whatever_the_block_size() {
        // The number of stages goes down, stays down until the cross-fade
        // is over and the spare stages have dropped out, and comes back up:
        // once with the fade long finished, once on the very frame it ends
        // (2,400 frames after it began), and once a frame before that.
        let render = |block: usize, pause: usize| {
            let mut phaser = prepared(
                Phaser::new()
                    .with_stages(12)
                    .with_rate_hz(3.3)
                    .with_feedback(0.6)
                    .with_spread(0.5),
                2,
            );
            let mut signal = noise(2, 7_000);
            let changes: [(usize, Change); 4] = [
                (500, |phaser| phaser.set_stages(2)),
                (500 + pause, |phaser| phaser.set_stages(9)),
                (500 + pause + 3_000, |phaser| phaser.set_stages(4)),
                (7_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run(&mut phaser, part, block));
                change(&mut phaser);
                start = end;
            }
            signal
        };
        for pause in [3_000, 2_400, 2_399] {
            let reference = render(512, pause);
            assert!(reference.peak() > 0.5);
            for block in [1, 7, 64, 333, 7_000] {
                assert!(
                    render(block, pause) == reference,
                    "blocks of {block}, a pause of {pause}"
                );
            }
        }
    }

    #[test]
    fn it_stays_bounded_at_the_extremes_and_survives_garbage() {
        for feedback in [0.95, -0.95] {
            let mut phaser = prepared(
                Phaser::new()
                    .with_stages(usize::MAX)
                    .with_rate_hz(1e9)
                    .with_depth_octaves(1e9)
                    .with_center_hz(1e9)
                    .with_feedback(feedback * 100.0)
                    .with_spread(1e9)
                    .with_mix(1.0),
                2,
            );
            assert_eq!(phaser.stages(), 12);
            let mut signal = noise(2, 96_000);
            run(&mut phaser, &mut signal, 512);
            // Every sample counts here: one that is not a number makes the
            // peak infinite.
            let peak = strict_buffer_peak(&signal);
            assert!(peak > 0.1 && peak < 20.0, "feedback {feedback}: {peak}");
        }

        let mut phaser = prepared(Phaser::new(), 2);
        for garbage in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e300] {
            phaser.set_rate_hz(garbage);
            phaser.set_depth_octaves(garbage);
            phaser.set_center_hz(garbage);
            phaser.set_feedback(garbage);
            phaser.set_spread(garbage);
            phaser.set_mix(garbage);
            phaser.set_stages(0);
            assert_eq!(phaser.stages(), 2);
            let mut signal = noise(2, 4_800);
            run(&mut phaser, &mut signal, 512);
            // Every sample is a number, and something has come through.
            let peak = strict_buffer_peak(&signal);
            assert!(peak.is_finite() && peak > 0.1, "after {garbage}: {peak}");
        }
    }

    #[test]
    fn reset_clears_the_filters_and_starts_the_sweep_again() {
        let build = || prepared(Phaser::new().with_feedback(0.8).with_rate_hz(5.0), 2);
        let mut fresh = build();
        let mut expected = noise(2, 3_000);
        run(&mut fresh, &mut expected, 512);

        let mut used = build();
        let mut first = noise(2, 3_000);
        run(&mut used, &mut first, 512);
        used.reset();
        let mut silence = AudioBuffer::<f64>::new(2, 3_000);
        run(&mut used, &mut silence, 512);
        assert_eq!(strict_buffer_peak(&silence), 0.0);

        used.reset();
        let mut second = noise(2, 3_000);
        run(&mut used, &mut second, 512);
        assert!(second == expected);
    }

    #[test]
    fn single_precision_and_unprepared_use_work() {
        // Not prepared: there is no filter memory, so audio passes
        // untouched.
        let mut phaser = Phaser::<f32>::new();
        let mut buffer = AudioBuffer::<f32>::new(2, 64);
        buffer.channel_mut(0).fill(0.25);
        phaser.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[0.25; 64]);

        phaser.set_depth_octaves(0.0);
        phaser.prepare(&ProcessSpec {
            sample_rate: 44_100.0,
            max_block_size: 64,
            num_channels: 1,
        });
        assert_eq!(Processor::<f32>::latency_samples(&phaser), 0);
        // A steady level passes an all-pass chain that is held still
        // unchanged. More channels than prepared for: the extra one is left
        // alone.
        let mut steady = AudioBuffer::<f32>::new(2, 64);
        for _ in 0..200 {
            for channel in steady.iter_channels_mut() {
                channel.fill(0.25);
            }
            phaser.process(&mut steady);
        }
        assert!((steady.channel(0)[63] - 0.25).abs() < 1e-6);
        assert_eq!(steady.channel(1), &[0.25; 64]);
        // No channels at all.
        phaser.process(&mut AudioBuffer::<f32>::new(0, 64));
    }
}
