use core::f64::consts::TAU;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::curve::ChorusSweep;
use crate::delay::DelayLine;
use crate::numeric::{glide_to, limit, step_towards};
use crate::{ProcessSpec, Processor};

/// Most voices a [`Chorus`] can have.
const MAX_VOICES: usize = 8;

/// Where each voice sits in the modulation cycle, as a fraction of it.
///
/// This is the van der Corput sequence in base two. Its first one, two, four
/// or eight members are evenly spaced, and every member keeps its place when
/// more are added, so a voice can be faded in or out without moving the
/// others.
const VOICE_PHASES: [f64; MAX_VOICES] = [0.0, 0.5, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875];

/// Limits of the settings.
const MAX_CENTRE_MS: f64 = 100.0;
const MAX_DEPTH_MS: f64 = 50.0;
const MAX_RATE_HZ: f64 = 100.0;
const MAX_FEEDBACK: f64 = 0.95;

/// Shortest delay a voice reads at, in samples: the least at which the cubic
/// interpolation has a sample on either side of its middle two.
pub(crate) const MIN_DELAY: f64 = 2.0;

/// The length of the delay lines at `sample_rate`, in samples: the longest
/// delay the settings allow, and room for the cubic read on its far side.
pub(crate) fn longest_delay(sample_rate: f64) -> usize {
    let longest = (MAX_CENTRE_MS + MAX_DEPTH_MS) * sample_rate / 1_000.0;
    longest.ceil().max(0.0) as usize + 4
}

/// A centre delay, a depth (both in milliseconds) and a rate as the
/// setters limit them.
pub(crate) fn limited(centre_ms: f64, depth_ms: f64, rate_hz: f64) -> (f64, f64, f64) {
    (
        limit(centre_ms, 0.0, MAX_CENTRE_MS),
        limit(depth_ms, 0.0, MAX_DEPTH_MS),
        limit(rate_hz, 0.0, MAX_RATE_HZ),
    )
}

/// How long a change of delay, depth, spread or voice count takes.
const DELAY_GLIDE_SECONDS: f64 = 0.05;
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
    /// Centre delay in samples.
    centre: Smoothed<f64>,
    /// Sweep either side of the centre, in samples.
    depth: Smoothed<f64>,
    feedback: Smoothed<f64>,
    mix: Smoothed<f64>,
    /// How far the modulation of the last channel would be ahead of the
    /// first if there were one more channel, in cycles.
    spread: Smoothed<f64>,
    /// How much of each voice is heard, from 0 to 1.
    voice_gains: [f64; MAX_VOICES],
    total_gain: f64,
    /// Whether a voice is fading in or out.
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
    fn step(&mut self, voices: usize, increment: f64, fade_step: f64) -> Step {
        if self.fading {
            self.fade(voices, fade_step);
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

    fn fade(&mut self, voices: usize, fade_step: f64) {
        let mut total = 0.0;
        let mut moving = false;
        for (index, gain) in self.voice_gains.iter_mut().enumerate() {
            let target = if index < voices { 1.0 } else { 0.0 };
            *gain = step_towards(*gain, target, fade_step);
            moving |= *gain != target;
            total += *gain;
        }
        self.total_gain = total;
        self.fading = moving;
    }
}

/// A chorus: copies of the signal, each delayed by an amount that sweeps
/// slowly back and forth, mixed with the original.
///
/// A delay that changes shifts the pitch a little, up while it shortens and
/// down while it lengthens, so the copies sound like further players that are
/// never quite in tune or in time with the first. With one voice, a delay of
/// a few milliseconds and some feedback, the same structure is a flanger:
/// see [`flanger`](Self::flanger).
///
/// - Each channel has one delay line, read by up to eight voices through
///   cubic interpolation ([`DelayLine::read_cubic`]).
/// - Voice `k` reads at `centre + depth * sin(2π (phase + p[k]))`, with the
///   voices placed around the cycle at `p = 0, 1/2, 1/4, 3/4, 1/8, 5/8, 3/8,
///   7/8`. One, two, four and eight voices are therefore evenly spaced.
/// - The wet signal is the average of the voices, so its level at low
///   frequencies does not depend on how many there are.
/// - The output is `dry * (1 - mix) + wet * mix`.
/// - The feedback returns the average of the same voices to the delay line,
///   read by linear interpolation. A straight line between two samples
///   cannot overshoot them, so however the delays move, the delay line
///   never holds more than `1 / (1 - |feedback|)` times the largest sample
///   that was put in: the effect cannot run away. For a delay of a whole
///   number of samples the two ways of reading are the same.
///
/// No setting clicks when it changes: the delay, the depth and the spread
/// glide over 50 ms, the feedback and the mix over 20 ms, a voice fades in
/// or out over 50 ms without the others moving, and a change of rate keeps
/// the position in the cycle. All setters are real-time safe.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{Chorus, ProcessSpec, Processor};
///
/// let mut chorus = Chorus::<f32>::new()
///     .with_voices(3)
///     .with_rate_hz(0.6)
///     .with_depth_ms(3.0)
///     .with_center_delay_ms(15.0)
///     .with_mix(0.5);
/// chorus.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 512, num_channels: 2 });
///
/// // A steady level comes out as it went in once the delay has filled up,
/// // however the voices sweep.
/// let mut block = AudioBuffer::<f32>::new(2, 512);
/// for _ in 0..4 {
///     for channel in block.iter_channels_mut() {
///         channel.fill(0.5);
///     }
///     chorus.process(&mut block);
/// }
/// assert!((block.channel(1)[511] - 0.5).abs() < 1e-5);
/// ```
///
/// # Limits
///
/// A voice never reads closer than two samples behind the input, so the
/// sweep stops there if the depth is larger than the centre delay: this is
/// not a through-zero flanger. The dry signal is not delayed, and the effect
/// reports no latency.
///
/// With feedback, a delay that falls between two samples loses a little of
/// the highest frequencies on every trip round the loop, because the
/// feedback is read by linear interpolation: up to 3 dB per trip at a
/// quarter of the sample rate.
#[derive(Debug, Clone)]
pub struct Chorus<S: Sample = f32> {
    voices: usize,
    rate_hz: f64,
    depth_ms: f64,
    centre_ms: f64,
    feedback: f64,
    spread: f64,
    mix: f64,
    sample_rate: f64,
    /// Change in a voice's gain per sample while it fades in or out.
    fade_step: f64,
    motion: Motion,
    /// One delay line per channel, read by every voice.
    lines: Vec<DelayLine<S>>,
}

impl<S: Sample> Default for Chorus<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Sample> Chorus<S> {
    /// The largest number of voices.
    pub const MAX_VOICES: usize = MAX_VOICES;

    /// Creates a chorus with three voices sweeping 2.5 ms either side of
    /// 15 ms at 0.8 Hz, no feedback, full spread and an equal mix.
    pub fn new() -> Self {
        let mut chorus = Self {
            voices: 3,
            rate_hz: 0.8,
            depth_ms: 2.5,
            centre_ms: 15.0,
            feedback: 0.0,
            spread: 1.0,
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
                voice_gains: [0.0; MAX_VOICES],
                total_gain: 1.0,
                fading: false,
            },
            lines: Vec::new(),
        };
        chorus.snap();
        chorus
    }

    /// Creates a flanger: one voice sweeping 1.5 ms either side of 2 ms at
    /// 0.25 Hz, with a feedback of 0.7 and an equal mix. The dry signal and
    /// the short moving delay cancel at evenly spaced frequencies, and the
    /// feedback sharpens what is left between them into a sweeping
    /// resonance.
    pub fn flanger() -> Self {
        Self::new()
            .with_voices(1)
            .with_rate_hz(0.25)
            .with_depth_ms(1.5)
            .with_center_delay_ms(2.0)
            .with_feedback(0.7)
    }

    /// Sets the number of voices, limited to `1..=8`. A voice that is added
    /// or removed fades in or out.
    pub fn set_voices(&mut self, voices: usize) {
        let voices = voices.clamp(1, MAX_VOICES);
        if voices != self.voices {
            self.voices = voices;
            self.motion.fading = true;
            // The room between two voices has changed, and with it how far
            // apart the channels can be.
            let spread = self.spread_cycles();
            glide_to(&mut self.motion.spread, spread);
        }
    }

    /// Sets how many times a second the delay sweeps back and forth, in
    /// hertz, limited to `0.0..=100.0`. Zero stops the sweep where it is.
    pub fn set_rate_hz(&mut self, hertz: f64) {
        self.rate_hz = limit(hertz, 0.0, MAX_RATE_HZ);
    }

    /// Sets how far the delay sweeps either side of its centre, in
    /// milliseconds, limited to `0.0..=50.0`.
    pub fn set_depth_ms(&mut self, milliseconds: f64) {
        self.depth_ms = limit(milliseconds, 0.0, MAX_DEPTH_MS);
        let depth = self.depth_samples();
        glide_to(&mut self.motion.depth, depth);
    }

    /// Sets the delay the sweep is centred on, in milliseconds, limited to
    /// `0.0..=100.0`. Around 15 ms gives a chorus, a few milliseconds a
    /// flanger.
    pub fn set_center_delay_ms(&mut self, milliseconds: f64) {
        self.centre_ms = limit(milliseconds, 0.0, MAX_CENTRE_MS);
        let centre = self.centre_samples();
        glide_to(&mut self.motion.centre, centre);
    }

    /// Sets how much of the wet signal is fed back into the delay, limited
    /// to `-0.95..=0.95`. A negative value feeds it back upside down, which
    /// moves the resonances of a flanger to where the gaps were.
    pub fn set_feedback(&mut self, feedback: f64) {
        self.feedback = if feedback.is_nan() {
            0.0
        } else {
            limit(feedback, -MAX_FEEDBACK, MAX_FEEDBACK)
        };
        glide_to(&mut self.motion.feedback, self.feedback);
    }

    /// Sets how far apart the modulation of the channels is, from `0.0`
    /// (every channel sweeps together) to `1.0` (as far apart as they can
    /// be).
    ///
    /// Channel `c` of `n` is `spread * c / n` of the gap between two voices
    /// ahead of the first channel. With one voice and two channels, a spread
    /// of 1 puts left and right half a cycle apart; with two voices, a
    /// quarter. A wider spread gives a wider image, at the cost of more
    /// movement when the channels are summed to mono.
    pub fn set_spread(&mut self, spread: f64) {
        self.spread = limit(spread, 0.0, 1.0);
        let spread = self.spread_cycles();
        glide_to(&mut self.motion.spread, spread);
    }

    /// Sets the balance between the untouched signal (`0.0`) and the voices
    /// (`1.0`). An equal mix gives the fullest chorus and the deepest
    /// flanger.
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = limit(mix, 0.0, 1.0);
        glide_to(&mut self.motion.mix, self.mix);
    }

    /// Builder form of [`set_voices`](Self::set_voices).
    #[must_use]
    pub fn with_voices(mut self, voices: usize) -> Self {
        self.set_voices(voices);
        self
    }

    /// Builder form of [`set_rate_hz`](Self::set_rate_hz).
    #[must_use]
    pub fn with_rate_hz(mut self, hertz: f64) -> Self {
        self.set_rate_hz(hertz);
        self
    }

    /// Builder form of [`set_depth_ms`](Self::set_depth_ms).
    #[must_use]
    pub fn with_depth_ms(mut self, milliseconds: f64) -> Self {
        self.set_depth_ms(milliseconds);
        self
    }

    /// Builder form of [`set_center_delay_ms`](Self::set_center_delay_ms).
    #[must_use]
    pub fn with_center_delay_ms(mut self, milliseconds: f64) -> Self {
        self.set_center_delay_ms(milliseconds);
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

    /// The number of voices, as last set.
    pub fn voices(&self) -> usize {
        self.voices
    }

    /// How the delay of voice `voice` on channel `channel` sweeps over one
    /// cycle, at the settings the chorus is at or heading for, for drawing;
    /// `None` for a voice beyond [`voices`](Self::voices). The channel's
    /// place in the spread counts the channels the chorus was prepared for.
    /// See [`ChorusSweep`] and [`curve`](crate::curve).
    pub fn sweep(&self, voice: usize, channel: usize) -> Option<ChorusSweep> {
        if voice >= self.voices {
            return None;
        }
        let channels = self.lines.len().max(1) as f64;
        Some(ChorusSweep::from_samples(
            self.rate_hz,
            self.centre_samples(),
            self.depth_samples(),
            self.spread_cycles() * (channel as f64 / channels),
            VOICE_PHASES[voice],
            self.sample_rate,
        ))
    }

    fn centre_samples(&self) -> f64 {
        self.centre_ms * self.sample_rate / 1_000.0
    }

    fn depth_samples(&self) -> f64 {
        self.depth_ms * self.sample_rate / 1_000.0
    }

    /// The spread as a part of the cycle: a part of the gap between two
    /// voices, which is the whole cycle for one voice, half of it for two, a
    /// quarter for up to four and an eighth beyond.
    fn spread_cycles(&self) -> f64 {
        self.spread / self.voices.next_power_of_two() as f64
    }

    /// Jumps every gliding value to where it is heading.
    fn snap(&mut self) {
        let (centre, depth, spread) = (
            self.centre_samples(),
            self.depth_samples(),
            self.spread_cycles(),
        );
        let motion = &mut self.motion;
        motion.centre.set_immediate(centre);
        motion.depth.set_immediate(depth);
        motion.spread.set_immediate(spread);
        motion.feedback.set_immediate(self.feedback);
        motion.mix.set_immediate(self.mix);
        for (index, gain) in motion.voice_gains.iter_mut().enumerate() {
            *gain = if index < self.voices { 1.0 } else { 0.0 };
        }
        motion.total_gain = self.voices as f64;
        motion.fading = false;
    }
}

impl<S: Sample> Processor<S> for Chorus<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        let glide = DELAY_GLIDE_SECONDS * spec.sample_rate;
        self.fade_step = if glide >= 1.0 { 1.0 / glide } else { 1.0 };

        let longest = longest_delay(spec.sample_rate);
        self.lines = (0..spec.num_channels)
            .map(|_| DelayLine::new(longest))
            .collect();

        let motion = &mut self.motion;
        for value in [&mut motion.centre, &mut motion.depth, &mut motion.spread] {
            value.set_ramp(spec.sample_rate, DELAY_GLIDE_SECONDS);
        }
        for value in [&mut motion.feedback, &mut motion.mix] {
            value.set_ramp(spec.sample_rate, GAIN_GLIDE_SECONDS);
        }
        motion.phase = 0.0;
        // The delays are kept in samples, so they depend on the rate.
        self.snap();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let increment = self.rate_hz / self.sample_rate;
        let (voices, fade_step) = (self.voices, self.fade_step);
        let channel_count = self.lines.len().max(1) as f64;

        let mut after = self.motion;
        let mut ran = false;
        for (index, (samples, line)) in buffer.iter_channels_mut().zip(&mut self.lines).enumerate()
        {
            let mut motion = self.motion;
            let position = index as f64 / channel_count;
            let longest = line.max_delay() as f64;
            for sample in samples {
                let step = motion.step(voices, increment, fade_step);
                let base = step.phase + step.spread * position;

                let feeding_back = step.feedback != 0.0;
                let mut wet = S::ZERO;
                let mut returned = S::ZERO;
                for (&gain, &place) in motion.voice_gains.iter().zip(&VOICE_PHASES) {
                    if gain > 0.0 {
                        let sweep = (TAU * (base + place)).sin();
                        let delay = limit(step.centre + step.depth * sweep, MIN_DELAY, longest);
                        let gain = S::from_f64(gain);
                        wet += line.read_cubic(delay) * gain;
                        if feeding_back {
                            // A straight line between two samples never
                            // leaves the range they span, which is what
                            // keeps the loop from growing.
                            returned += line.read(delay) * gain;
                        }
                    }
                }
                let average = S::from_f64(1.0 / motion.total_gain);
                wet *= average;
                returned *= average;

                let input = *sample;
                line.push(input + returned * S::from_f64(step.feedback));
                *sample = input * S::from_f64(1.0 - step.mix) + wet * S::from_f64(step.mix);
            }
            after = motion;
            ran = true;
        }
        if !ran {
            // No channel to process, but time still passes.
            for _ in 0..buffer.num_frames() {
                after.step(voices, increment, fade_step);
            }
        }
        self.motion = after;
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.motion.phase = 0.0;
        self.snap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{Complex, RealFft};
    use crate::numeric::strict_buffer_peak;

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    fn prepared(chorus: Chorus<f64>, channels: usize) -> Chorus<f64> {
        let mut chorus = chorus;
        chorus.prepare(&spec(channels));
        chorus
    }

    /// Runs `signal` through the chorus in blocks of `block` frames.
    fn run(chorus: &mut Chorus<f64>, signal: &mut AudioBuffer<f64>, block: usize) {
        let frames = signal.num_frames();
        let mut start = 0;
        while start < frames {
            let end = (start + block).min(frames);
            signal.windowed(start..end, |piece| chorus.process(piece));
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

    /// The signal `n^power` on every channel. Cubic interpolation reads a
    /// polynomial of up to third degree without error, so what comes back
    /// from a delay line shows exactly where it was read.
    fn probe(channels: usize, frames: usize, power: i32) -> AudioBuffer<f64> {
        let mut buffer = AudioBuffer::new(channels, frames);
        for channel in buffer.iter_channels_mut() {
            for (n, sample) in channel.iter_mut().enumerate() {
                *sample = (n as f64).powi(power);
            }
        }
        buffer
    }

    /// A chorus with nothing but its delay: wet only, no feedback, sweeping
    /// 2 ms (96 samples) either side of 10 ms (480 samples) twice a second.
    fn bare(voices: usize) -> Chorus<f64> {
        Chorus::new()
            .with_voices(voices)
            .with_rate_hz(2.0)
            .with_depth_ms(2.0)
            .with_center_delay_ms(10.0)
            .with_feedback(0.0)
            .with_spread(0.0)
            .with_mix(1.0)
    }

    /// What `bare` should put out on frame `n` for the probe `n^power`,
    /// worked out from the documented behaviour: every voice reads the probe
    /// at its own delay, and the voices are averaged. `ahead` is how far
    /// this channel's modulation is ahead, in cycles.
    fn model(n: usize, power: i32, voices: usize, ahead: f64) -> f64 {
        let phase = 2.0 * n as f64 / RATE;
        let mut sum = 0.0;
        for place in &VOICE_PHASES[..voices] {
            let delay = 480.0 + 96.0 * (TAU * (phase + ahead + place)).sin();
            sum += (n as f64 - delay).powi(power);
        }
        sum / voices as f64
    }

    #[test]
    fn one_voice_without_a_sweep_is_an_exact_delay() {
        let mut chorus = prepared(bare(1).with_depth_ms(0.0), 1);
        let original = noise(1, 2_000);
        let mut signal = original.clone();
        run(&mut chorus, &mut signal, 512);
        assert_eq!(signal.channel(0)[..480], [0.0; 480]);
        assert_eq!(signal.channel(0)[480..], original.channel(0)[..1_520]);

        // The times are in milliseconds, so at another rate the same
        // setting is a different number of samples.
        let mut chorus = bare(1).with_depth_ms(0.0);
        chorus.prepare(&ProcessSpec {
            sample_rate: 96_000.0,
            max_block_size: 512,
            num_channels: 1,
        });
        let mut signal = original.clone();
        run(&mut chorus, &mut signal, 512);
        assert_eq!(signal.channel(0)[960..], original.channel(0)[..1_040]);
    }

    #[test]
    fn the_delay_sweeps_as_a_sine_of_the_set_rate_and_depth() {
        let mut chorus = prepared(bare(1), 1);
        // The input is its own frame number, so the frame number less the
        // output is the delay the voice read at.
        let mut signal = probe(1, 98_000, 1);
        run(&mut chorus, &mut signal, 512);

        let (mut shortest, mut longest) = (f64::MAX, 0.0f64);
        let mut upward_crossings = Vec::new();
        let mut previous = 480.0;
        for n in 2_000..98_000 {
            let delay = n as f64 - signal.channel(0)[n];
            let expected = 480.0 + 96.0 * (TAU * 2.0 * n as f64 / RATE).sin();
            assert!(
                (delay - expected).abs() < 1e-6,
                "frame {n}: {delay} against {expected}"
            );
            shortest = shortest.min(delay);
            longest = longest.max(delay);
            if previous < 480.0 && delay >= 480.0 {
                upward_crossings.push(n);
            }
            previous = delay;
        }
        // 2 ms either side of 10 ms, at 48 kHz.
        assert!((shortest - 384.0).abs() < 1e-3 && (longest - 576.0).abs() < 1e-3);
        // Twice a second: a cycle every 24,000 frames. On those frames the
        // delay is at its centre to within rounding, so the crossing is seen
        // there or one frame later.
        assert_eq!(upward_crossings.len(), 4);
        for (cycle, &crossing) in upward_crossings.iter().enumerate() {
            let expected = 24_000 * (cycle + 1);
            assert!(
                crossing == expected || crossing == expected + 1,
                "crossing at {crossing}"
            );
        }
    }

    #[test]
    fn voices_are_placed_around_the_cycle_and_averaged() {
        // The documented places: evenly spaced for one, two, four and eight
        // voices.
        for voices in [1, 2, 4, 8] {
            let mut places = VOICE_PHASES[..voices].to_vec();
            places.sort_by(f64::total_cmp);
            for (index, place) in places.iter().enumerate() {
                assert_eq!(*place, index as f64 / voices as f64);
            }
        }

        for voices in 1..=MAX_VOICES {
            // A squared frame number tells the voices apart: the average of
            // their delays alone would not.
            for power in [1, 2] {
                let mut chorus = prepared(bare(voices), 1);
                assert_eq!(chorus.voices(), voices);
                let mut signal = probe(1, 27_000, power);
                run(&mut chorus, &mut signal, 512);
                for n in (2_000..27_000).step_by(7) {
                    let expected = model(n, power, voices, 0.0);
                    let output = signal.channel(0)[n];
                    assert!(
                        (output - expected).abs() < 1e-3,
                        "{voices} voices, power {power}, frame {n}: {output} against {expected}"
                    );
                }
            }

            // A steady level comes through at the same level whatever the
            // number of voices.
            let mut chorus = prepared(bare(voices), 1);
            let mut level = AudioBuffer::<f64>::new(1, 4_000);
            level.channel_mut(0).fill(0.7);
            run(&mut chorus, &mut level, 512);
            assert!((level.channel(0)[3_999] - 0.7).abs() < 1e-12);
        }
    }

    #[test]
    fn spread_puts_the_channels_out_of_step_by_the_stated_amount() {
        // Voices, the gap between two of them as a part of the cycle,
        // channels, spread.
        for (voices, gap, channels, spread) in [
            (1, 1.0, 2usize, 0.0),
            (1, 1.0, 2, 0.5),
            (1, 1.0, 2, 1.0),
            (1, 1.0, 3, 1.0),
            (2, 0.5, 2, 0.5),
            (2, 0.5, 2, 1.0),
            (3, 0.25, 2, 1.0),
            (4, 0.25, 2, 1.0),
            (5, 0.125, 3, 0.6),
            (8, 0.125, 2, 1.0),
        ] {
            let mut chorus = prepared(bare(voices).with_spread(spread), channels);
            let mut signal = probe(channels, 14_000, 2);
            run(&mut chorus, &mut signal, 512);
            for channel in 0..channels {
                let ahead = spread * gap * channel as f64 / channels as f64;
                for n in (2_000..14_000).step_by(11) {
                    let expected = model(n, 2, voices, ahead);
                    let output = signal.channel(channel)[n];
                    assert!(
                        (output - expected).abs() < 1e-3,
                        "{voices} voices, spread {spread}, channel {channel} of {channels}, \
                         frame {n}: {output} against {expected}"
                    );
                }
            }
            if spread == 0.0 {
                assert_eq!(signal.channel(0), signal.channel(1));
            } else if voices != 4 && voices != 8 {
                // Four or eight evenly spaced voices read this probe the
                // same wherever they are in the cycle, so they are no test
                // of the spread.
                let apart = (signal.channel(0)[10_000] - signal.channel(1)[10_000]).abs();
                assert!(apart > 1.0, "{voices} voices, spread {spread}: {apart}");
            }
        }
    }

    /// Magnitude of the frequency response, from the impulse response.
    fn magnitudes(chorus: &mut Chorus<f64>, len: usize) -> Vec<f64> {
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = 1.0;
        run(chorus, &mut impulse, 512);
        let mut fft = RealFft::<f64>::new(len);
        let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
        fft.forward(impulse.channel(0), &mut spectrum);
        spectrum.iter().map(|bin| bin.norm()).collect()
    }

    #[test]
    fn a_fixed_delay_with_feedback_is_the_comb_filter_theory_predicts() {
        // 1 ms is 48 samples. With the sweep stopped the effect is
        //   H(z) = (1 - mix) + mix * z^-D / (1 - feedback * z^-D).
        // The impulse response is measured for half a second, long enough
        // for the strongest feedback to have died away, which makes bin `k`
        // the frequency `2k` hertz. Every bin is compared.
        const DELAY: f64 = 48.0;
        const LEN: usize = 24_000;
        let response = |feedback: f64, mix: f64| {
            let mut chorus = prepared(
                bare(1)
                    .with_depth_ms(0.0)
                    .with_center_delay_ms(1.0)
                    .with_feedback(feedback)
                    .with_mix(mix),
                1,
            );
            assert_eq!(chorus.motion.centre.target(), DELAY);
            let measured = magnitudes(&mut chorus, LEN);
            for (bin, &magnitude) in measured.iter().enumerate() {
                let angle = -TAU * bin as f64 * DELAY / LEN as f64;
                let (sin, cos) = angle.sin_cos();
                // wet = e / (1 - feedback * e), with e = cos + i sin.
                let (re, im) = (1.0 - feedback * cos, -feedback * sin);
                let scale = mix / (re * re + im * im);
                let wet = ((cos * re + sin * im) * scale, (sin * re - cos * im) * scale);
                let expected = (1.0 - mix + wet.0).hypot(wet.1);
                assert!(
                    (magnitude - expected).abs() < 1e-9,
                    "feedback {feedback}, mix {mix}, bin {bin}: {magnitude} against {expected}"
                );
            }
            measured
        };
        response(0.95, 1.0);
        response(-0.95, 0.5);

        // The landmarks of that curve, for a flanger with an equal mix. At
        // 500 Hz (bin 250) the delay is half a cycle; at 1 kHz (bin 500) it
        // is a whole one.
        // No feedback: complete cancellation at 500 Hz, 1.5 kHz, ...
        let plain = response(0.0, 0.5);
        assert!(plain[250] < 1e-12 && plain[750] < 1e-12);
        assert!((plain[500] - 1.0).abs() < 1e-12);
        // Positive feedback: resonances at 1 kHz, 2 kHz, ... of
        // 0.5 + 0.5 / (1 - 0.7).
        let positive = response(0.7, 0.5);
        assert!((positive[500] - (0.5 + 0.5 / 0.3)).abs() < 1e-9);
        assert!((positive[250] - (0.5 - 0.5 / 1.7)).abs() < 1e-9);
        // Negative feedback: the resonances move to where the gaps were.
        let negative = response(-0.7, 0.5);
        assert!((negative[250] - (0.5 / 0.3 - 0.5)).abs() < 1e-9);
        assert!((negative[500] - (0.5 + 0.5 / 1.7)).abs() < 1e-9);
    }

    /// A change of settings, made at some point in a test.
    type Change = fn(&mut Chorus<f64>);

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
            |chorus| chorus.set_center_delay_ms(30.0),
            |chorus| chorus.set_depth_ms(4.0),
            |chorus| chorus.set_voices(8),
            |chorus| chorus.set_spread(0.0),
            |chorus| chorus.set_voices(1),
            |chorus| chorus.set_spread(1.0),
            |chorus| chorus.set_feedback(0.5),
            |chorus| chorus.set_mix(1.0),
            |chorus| chorus.set_rate_hz(3.0),
            |chorus| chorus.set_feedback(-0.5),
            |chorus| chorus.set_center_delay_ms(5.0),
            |chorus| chorus.set_mix(0.3),
        ];
        let mut chorus = prepared(Chorus::new(), 2);
        let mut signal = sine(2, SEGMENT * (changes.len() + 1), 220.0);
        for (index, change) in changes.iter().enumerate() {
            signal.windowed(index * SEGMENT..(index + 1) * SEGMENT, |part| {
                run(&mut chorus, part, 512);
            });
            change(&mut chorus);
        }
        signal.windowed(
            changes.len() * SEGMENT..(changes.len() + 1) * SEGMENT,
            |part| {
                run(&mut chorus, part, 512);
            },
        );
        for channel in signal.iter_channels() {
            let jump = largest_jump(channel);
            assert!(jump < 0.02, "a jump of {jump}");
        }

        // The measurement does see a click when there is one: clearing the
        // delay line under a steady level cuts the wet signal off at once.
        let mut chorus = prepared(Chorus::new().with_mix(1.0), 1);
        let mut signal = AudioBuffer::<f64>::new(1, 2 * SEGMENT);
        signal.channel_mut(0).fill(0.5);
        signal.windowed(0..SEGMENT, |part| run(&mut chorus, part, 512));
        chorus.reset();
        signal.windowed(SEGMENT..2 * SEGMENT, |part| run(&mut chorus, part, 512));
        let cut = largest_jump(&signal.channel(0)[SEGMENT - 100..SEGMENT + 100]);
        assert!((cut - 0.5).abs() < 1e-9, "the cut was a jump of {cut}");
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let render = |block: usize| {
            let mut chorus = prepared(
                Chorus::new()
                    .with_voices(5)
                    .with_rate_hz(3.3)
                    .with_feedback(-0.5),
                2,
            );
            let mut signal = noise(2, 6_000);
            // The same changes on the same frames, whatever the block size.
            let changes: [(usize, Change); 4] = [
                (1_000, |chorus| chorus.set_voices(8)),
                (1_700, |chorus| {
                    chorus.set_center_delay_ms(4.0);
                    chorus.set_depth_ms(1.0);
                    chorus.set_spread(0.3);
                }),
                (3_100, |chorus| {
                    chorus.set_voices(2);
                    chorus.set_feedback(0.8);
                    chorus.set_mix(0.9);
                    chorus.set_rate_hz(0.2);
                }),
                (6_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run(&mut chorus, part, block));
                change(&mut chorus);
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

    #[test]
    fn it_stays_bounded_at_the_extremes_and_survives_garbage() {
        for feedback in [0.95, -0.95] {
            let mut chorus = prepared(
                Chorus::new()
                    .with_voices(8)
                    .with_rate_hz(1e9)
                    .with_depth_ms(1e9)
                    .with_center_delay_ms(1e9)
                    .with_feedback(feedback * 100.0)
                    .with_mix(1.0),
                2,
            );
            // Noise, and the signal that drives a loop hardest: full scale,
            // changing sign as rarely or as often as it can.
            let mut signals = vec![noise(2, 24_000)];
            for period in [2, 96, 4_800] {
                let mut square = AudioBuffer::<f64>::new(1, 24_000);
                for (n, sample) in square.channel_mut(0).iter_mut().enumerate() {
                    *sample = if (n / period) % 2 == 0 { 1.0 } else { -1.0 };
                }
                signals.push(square);
            }
            for mut signal in signals {
                chorus.reset();
                run(&mut chorus, &mut signal, 512);
                // Every sample counts here: one that is not a number makes
                // the peak infinite.
                let peak = strict_buffer_peak(&signal);
                // The delay line holds at most 1 / (1 - 0.95) = 20 times
                // the input's peak of one, and a cubic read can overshoot
                // what it reads by a quarter.
                assert!(peak > 0.1 && peak <= 25.0, "feedback {feedback}: {peak}");
                for line in &chorus.lines {
                    for delay in 1..=line.max_delay() {
                        let held = line.read(delay as f64).abs();
                        assert!(
                            held <= 20.0 + 1e-9,
                            "feedback {feedback}: the line holds {held}"
                        );
                    }
                }
            }
        }

        // A flanger at full feedback, held on a resonance by a steady tone:
        // the gain is at most 1 / (1 - 0.95).
        let mut flanger = prepared(
            Chorus::flanger()
                .with_feedback(0.95)
                .with_depth_ms(0.0)
                .with_center_delay_ms(1.0)
                .with_mix(1.0),
            1,
        );
        let mut tone = sine(1, 96_000, 1_000.0);
        run(&mut flanger, &mut tone, 512);
        let peak = strict_buffer_peak(&tone);
        assert!((peak - 0.5 * 20.0).abs() < 0.01, "resonance reached {peak}");

        let mut chorus = prepared(Chorus::new(), 2);
        for garbage in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e300] {
            chorus.set_rate_hz(garbage);
            chorus.set_depth_ms(garbage);
            chorus.set_center_delay_ms(garbage);
            chorus.set_feedback(garbage);
            chorus.set_spread(garbage);
            chorus.set_mix(garbage);
            chorus.set_voices(usize::MAX);
            chorus.set_voices(0);
            let mut signal = noise(2, 4_800);
            run(&mut chorus, &mut signal, 512);
            // Every sample is a number, something has come through, and it
            // is within what the delay line can hold at the strongest
            // feedback, with the overshoot of the cubic read.
            let peak = strict_buffer_peak(&signal);
            assert!(peak > 0.1 && peak <= 25.0, "after {garbage}: {peak}");
        }
    }

    #[test]
    fn reset_clears_the_tail_and_starts_the_sweep_again() {
        let build = || prepared(Chorus::new().with_feedback(0.8).with_rate_hz(5.0), 2);
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
        // Not prepared: there are no delay lines, so audio passes untouched.
        let mut chorus = Chorus::<f32>::new();
        let mut buffer = AudioBuffer::<f32>::new(2, 64);
        buffer.channel_mut(0).fill(0.25);
        chorus.process(&mut buffer);
        assert_eq!(buffer.channel(0), &[0.25; 64]);

        chorus.prepare(&ProcessSpec {
            sample_rate: 44_100.0,
            max_block_size: 64,
            num_channels: 1,
        });
        assert_eq!(Processor::<f32>::latency_samples(&chorus), 0);
        // More channels than prepared for: the extra one is left alone.
        let mut steady = AudioBuffer::<f32>::new(2, 64);
        for _ in 0..100 {
            for channel in steady.iter_channels_mut() {
                channel.fill(0.25);
            }
            chorus.process(&mut steady);
        }
        assert!((steady.channel(0)[63] - 0.25).abs() < 1e-6);
        assert_eq!(steady.channel(1), &[0.25; 64]);
        // No channels at all.
        chorus.process(&mut AudioBuffer::<f32>::new(0, 64));
    }
}
