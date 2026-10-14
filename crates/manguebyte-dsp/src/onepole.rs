use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::cutoff::GlidingCutoff;
use crate::numeric::glide_to;
use crate::{ProcessSpec, Processor};

/// Limits of the cutoff: a lowest frequency in hertz, and a highest one as
/// a fraction of the sample rate.
const LOWEST_HZ: f64 = 0.01;
const HIGHEST_RATIO: f64 = 0.49;

/// How long the cutoff takes to reach a new value when a block is
/// processed.
const CUTOFF_GLIDE_SECONDS: f64 = 0.01;
/// How long a change of mode cross-fades.
const MODE_FADE_SECONDS: f64 = 0.01;

/// Which response a [`OnePole`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnePoleMode {
    /// Passes what is below the cutoff, 6 dB per octave.
    Lowpass,
    /// Passes what is above the cutoff, 6 dB per octave.
    Highpass,
    /// Passes everything, turning the phase by a quarter turn at the cutoff
    /// and by half a turn far above it.
    Allpass,
}

impl OnePoleMode {
    /// How much of the low-pass and of the high-pass output the mode takes.
    fn weights(self) -> (f64, f64) {
        match self {
            Self::Lowpass => (1.0, 0.0),
            Self::Highpass => (0.0, 1.0),
            Self::Allpass => (1.0, -1.0),
        }
    }
}

/// Everything that moves while a block is processed. Every channel runs a
/// copy of its own, so all of them see the same values.
#[derive(Debug, Clone, Copy)]
struct Motion {
    cutoff: GlidingCutoff,
    /// `g / (1 + g)` for the integrator gain `g` of the cutoff.
    gain: f64,
    /// Share of the low-pass output in the result.
    low: Smoothed<f64>,
    /// Share of the high-pass output in the result.
    high: Smoothed<f64>,
}

impl Motion {
    /// Moves on by one sample and returns the gain and the two shares.
    #[inline]
    fn step(&mut self) -> (f64, f64, f64) {
        if self.cutoff.is_gliding() {
            let g = self.cutoff.next_gain();
            self.gain = g / (1.0 + g);
        }
        (self.gain, self.low.next_value(), self.high.next_value())
    }

    fn is_moving(&self) -> bool {
        self.cutoff.is_gliding() || self.low.is_smoothing() || self.high.is_smoothing()
    }

    /// Jumps to where everything is heading.
    fn snap(&mut self) {
        self.cutoff.snap();
        let g = self.cutoff.gain();
        self.gain = g / (1.0 + g);
        self.low.set_immediate(self.low.target());
        self.high.set_immediate(self.high.target());
    }
}

/// One step of the filter: the low-pass and the high-pass output for `x`.
#[inline]
fn tick(state: &mut f64, x: f64, gain: f64) -> (f64, f64) {
    let v = (x - *state) * gain;
    let low = v + *state;
    *state = low + v;
    (low, x - low)
}

/// A first-order filter: low-pass, high-pass or all-pass, 6 dB per octave.
///
/// This is the gentlest filter there is, and the building block of many
/// others: tone controls, the smoothing of a control signal, the removal of
/// a steady offset, the phase shifter of a crossover. It is built in the
/// topology-preserving (zero-delay feedback) form of Vadim Zavalishin, "The
/// Art of VA Filter Design", chapter 3: one trapezoidal integrator whose
/// memory is the charge of the capacitor in the analogue circuit. That
/// memory stays meaningful whatever the cutoff does, so the cutoff can be
/// moved on every sample without the filter clicking or running away.
///
/// With `g = tan(π·fc/fs)`, the low-pass gain at frequency `f` is
/// `1 / sqrt(1 + (tan(π·f/fs) / g)²)`: exactly 3.01 dB down at the cutoff,
/// and zero at half the sample rate. The high-pass output is the input
/// minus the low-pass output, and the all-pass output is the low-pass minus
/// the high-pass output.
///
/// There are two ways to use it:
///
/// - As a [`Processor`], on blocks. A new cutoff is then approached over
///   10 ms on a musical scale and a new mode is cross-faded in over 10 ms,
///   so settings changed once per block neither click nor step.
/// - By the sample, with [`process_sample`](Self::process_sample), which
///   applies the settings at once. Call
///   [`set_frequency`](Self::set_frequency) before each sample to modulate
///   the cutoff at audio rate.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{OnePole, OnePoleMode, ProcessSpec, Processor};
///
/// let mut filter = OnePole::<f32>::new(OnePoleMode::Lowpass, 1_000.0);
/// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A tone at the cutoff comes out 3 dB down: at 0.707 of its level.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
/// }
/// filter.process(&mut block);
/// let peak = block.channel(0)[2_400..].iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
/// assert!((peak - 0.707).abs() < 0.002);
/// ```
///
/// # Limits
///
/// The cutoff is limited to between 0.01 Hz and 0.49 of the sample rate.
///
/// While the cutoff stays at or below a quarter of the sample rate (counted
/// from the last `prepare` or `reset`), the low-pass output never exceeds
/// the largest input sample so far, however the cutoff moves; the
/// high-pass, which is the input minus the low-pass, never exceeds twice
/// it, and the all-pass, twice the low-pass minus the input, three times.
/// Those are the bounds of the mode in force: while a change of mode
/// cross-fades (10 ms, when a block is processed), the output is held by
/// the larger of the old and the new mode's bound instead, though
/// [`mode`](Self::mode) reports the new one at once. Above that cutoff the memory of the filter can hold more than the input: up to
/// `tan(π·fc/fs)` times its peak, for an input at half the sample rate,
/// which is 31.8 at the upper limit. A steady filter cancels that in its
/// output; a cutoff that drops while the memory is full lets it out.
/// Whatever the cutoff does, the low-pass output is never larger than 31.8
/// times the input peak, the high-pass that plus the input peak (32.8
/// times), and the all-pass twice that plus the input peak (64.6 times).
#[derive(Debug, Clone)]
pub struct OnePole<S: Sample = f32> {
    mode: OnePoleMode,
    motion: Motion,
    /// The integrator memory of each channel.
    state: Vec<f64>,
    marker: PhantomData<S>,
}

impl<S: Sample> OnePole<S> {
    /// Creates a filter. Until [`prepare`](Processor::prepare) is called it
    /// assumes 44.1 kHz.
    pub fn new(mode: OnePoleMode, frequency: f64) -> Self {
        let (low, high) = mode.weights();
        let mut motion = Motion {
            cutoff: GlidingCutoff::new(frequency, LOWEST_HZ, HIGHEST_RATIO, CUTOFF_GLIDE_SECONDS),
            gain: 0.0,
            low: Smoothed::new(low),
            high: Smoothed::new(high),
        };
        motion.snap();
        Self {
            mode,
            motion,
            state: Vec::new(),
            marker: PhantomData,
        }
    }

    /// Changes the response. Real-time safe. A block cross-fades to the new
    /// response; [`process_sample`](Self::process_sample) switches at once,
    /// which is still free of a jump in the filter's memory.
    pub fn set_mode(&mut self, mode: OnePoleMode) {
        self.mode = mode;
        let (low, high) = mode.weights();
        glide_to(&mut self.motion.low, low);
        glide_to(&mut self.motion.high, high);
    }

    /// Changes the cutoff in hertz, limited to between 0.01 Hz and 0.49 of
    /// the sample rate. Real-time safe, and cheap enough to call for every
    /// sample.
    pub fn set_frequency(&mut self, frequency: f64) {
        self.motion.cutoff.set(frequency);
        if !self.motion.cutoff.is_gliding() {
            let g = self.motion.cutoff.gain();
            self.motion.gain = g / (1.0 + g);
        }
    }

    /// The response, as last set.
    pub fn mode(&self) -> OnePoleMode {
        self.mode
    }

    /// The cutoff in hertz the filter is at or heading for, after the
    /// limits.
    pub fn frequency(&self) -> f64 {
        self.motion.cutoff.limited()
    }

    /// Filters one sample of one channel with the settings as they stand,
    /// without gliding. Real-time safe.
    ///
    /// Use this, with [`set_frequency`](Self::set_frequency) before each
    /// call, to modulate the cutoff at audio rate. A channel the filter was
    /// not prepared for is passed through untouched.
    #[inline]
    pub fn process_sample(&mut self, channel: usize, input: S) -> S {
        if self.motion.is_moving() {
            self.motion.snap();
        }
        let Some(state) = self.state.get_mut(channel) else {
            return input;
        };
        let (low, high) = tick(state, input.to_f64(), self.motion.gain);
        S::from_f64(self.motion.low.target() * low + self.motion.high.target() * high)
    }
}

impl<S: Sample> Processor<S> for OnePole<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.motion.cutoff.prepare(spec.sample_rate);
        self.motion
            .low
            .set_ramp(spec.sample_rate, MODE_FADE_SECONDS);
        self.motion
            .high
            .set_ramp(spec.sample_rate, MODE_FADE_SECONDS);
        self.motion.snap();
        self.state = vec![0.0; spec.num_channels];
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let mut after = self.motion;
        let mut ran = false;
        for (samples, state) in buffer.iter_channels_mut().zip(&mut self.state) {
            let mut motion = self.motion;
            let mut memory = *state;
            for sample in samples {
                let (gain, low_share, high_share) = motion.step();
                let (low, high) = tick(&mut memory, sample.to_f64(), gain);
                *sample = S::from_f64(low_share * low + high_share * high);
            }
            *state = memory;
            after = motion;
            ran = true;
        }
        if !ran {
            // No channel to process, but time still passes.
            for _ in 0..buffer.num_frames() {
                after.step();
            }
        }
        self.motion = after;
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.motion.snap();
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::{FRAC_1_SQRT_2, PI};

    use super::*;
    use crate::numeric::{strict_buffer_peak, strict_peak};
    use crate::testing::{
        Random, db, gain_at, largest_jump, noise, response_at, run_in_blocks, sine, tail_peak,
    };

    const RATE: f64 = 48_000.0;

    fn spec(channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    fn prepared(mode: OnePoleMode, frequency: f64) -> OnePole<f64> {
        let mut filter = OnePole::new(mode, frequency);
        filter.prepare(&spec(1));
        filter
    }

    /// The impulse response, checked to have died away within `len`.
    fn impulse_response(filter: &mut OnePole<f64>, len: usize) -> Vec<f64> {
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = 1.0;
        run_in_blocks(filter, &mut impulse, 512);
        let response = impulse.channel(0).to_vec();
        // A high-pass or all-pass response ends in the tail of the low-pass.
        assert!(tail_peak(&response, 64) < 1e-13, "the response is cut off");
        response
    }

    /// The gain of the analogue low-pass, at the frequency the bilinear
    /// transform maps `frequency` to.
    fn lowpass_theory(cutoff: f64, frequency: f64) -> f64 {
        let ratio = (PI * frequency / RATE).tan() / (PI * cutoff / RATE).tan();
        1.0 / (1.0 + ratio * ratio).sqrt()
    }

    #[test]
    fn low_pass_and_high_pass_are_three_decibels_down_at_the_cutoff_and_fall_six_per_octave() {
        const LEN: usize = 1 << 15;
        for cutoff in [20.0, 1_000.0, 15_000.0] {
            let low = impulse_response(&mut prepared(OnePoleMode::Lowpass, cutoff), LEN);
            let high = impulse_response(&mut prepared(OnePoleMode::Highpass, cutoff), LEN);
            assert!((gain_at(&low, cutoff, RATE) - FRAC_1_SQRT_2).abs() < 1e-12);
            assert!((gain_at(&high, cutoff, RATE) - FRAC_1_SQRT_2).abs() < 1e-12);
            assert!((db(gain_at(&low, cutoff, RATE)) + 3.0103).abs() < 1e-4);

            for frequency in [5.0, 60.0, 440.0, 3_000.0, 12_000.0, 23_000.0] {
                let theory = lowpass_theory(cutoff, frequency);
                let measured = gain_at(&low, frequency, RATE);
                assert!(
                    (measured - theory).abs() < 1e-12,
                    "low-pass at {cutoff} Hz, {frequency} Hz: {measured} against {theory}"
                );
                // The two outputs are complementary in power.
                let measured = gain_at(&high, frequency, RATE);
                let theory = (1.0 - theory * theory).sqrt();
                assert!(
                    (measured - theory).abs() < 1e-11,
                    "high-pass at {cutoff} Hz, {frequency} Hz: {measured} against {theory}"
                );
            }
        }

        // Far from the cutoff the slope is 6.02 dB per octave.
        let low = impulse_response(&mut prepared(OnePoleMode::Lowpass, 20.0), LEN);
        let slope = db(gain_at(&low, 640.0, RATE)) - db(gain_at(&low, 320.0, RATE));
        assert!((slope + 6.02).abs() < 0.01, "low-pass slope {slope}");
        let high = impulse_response(&mut prepared(OnePoleMode::Highpass, 15_000.0), LEN);
        let slope = db(gain_at(&high, 200.0, RATE)) - db(gain_at(&high, 100.0, RATE));
        assert!((slope - 6.02).abs() < 0.01, "high-pass slope {slope}");
        // Nothing passes a low-pass at half the sample rate, or a high-pass
        // at 0 Hz.
        assert!(gain_at(&low, RATE / 2.0, RATE) < 1e-12);
        assert!(gain_at(&high, 0.0, RATE) < 1e-12);
    }

    #[test]
    fn the_all_pass_changes_the_phase_and_nothing_else() {
        const LEN: usize = 1 << 15;
        for cutoff in [30.0, 1_000.0, 12_000.0] {
            let response = impulse_response(&mut prepared(OnePoleMode::Allpass, cutoff), LEN);
            for frequency in [0.0, 10.0, 250.0, 1_000.0, 9_000.0, 23_999.0] {
                let gain = gain_at(&response, frequency, RATE);
                assert!(
                    (gain - 1.0).abs() < 1e-12,
                    "{cutoff} Hz at {frequency}: {gain}"
                );
            }
            // A quarter turn behind at the cutoff, none at 0 Hz, half a turn
            // at the top.
            let (re, im) = response_at(&response, cutoff, RATE);
            assert!(re.abs() < 1e-12 && (im + 1.0).abs() < 1e-12, "{re}, {im}");
            let (re, im) = response_at(&response, 0.0, RATE);
            assert!((re - 1.0).abs() < 1e-12 && im.abs() < 1e-12);
            let (re, im) = response_at(&response, RATE / 2.0, RATE);
            assert!((re + 1.0).abs() < 1e-12 && im.abs() < 1e-9);
        }
    }

    #[test]
    fn up_to_a_quarter_of_the_sample_rate_no_modulation_makes_it_louder_than_its_input() {
        let mut random = Random::new(11);
        let mut low = prepared(OnePoleMode::Lowpass, 100.0);
        let mut high = prepared(OnePoleMode::Highpass, 100.0);
        let mut all = prepared(OnePoleMode::Allpass, 100.0);
        let (mut low_peak, mut high_peak, mut all_peak) = (0.0f64, 0.0f64, 0.0f64);
        for n in 0..200_000 {
            // A new cutoff on every sample, anywhere in the eleven octaves
            // up to a quarter of the sample rate.
            let cutoff = random.log_between(0.01, RATE / 4.0);
            // Noise for the first half, then the worst input there is.
            let input = if n < 100_000 {
                random.between(-1.0, 1.0)
            } else if n % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            for filter in [&mut low, &mut high, &mut all] {
                filter.set_frequency(cutoff);
            }
            low_peak = low_peak.max(low.process_sample(0, input).abs());
            high_peak = high_peak.max(high.process_sample(0, input).abs());
            all_peak = all_peak.max(all.process_sample(0, input).abs());
        }
        // The bounds, with room for the rounding of the last bit only.
        assert!(low_peak <= 1.0 + 1e-12, "low-pass reached {low_peak}");
        assert!(low_peak > 0.9, "the test did not exercise the filter");
        // The other outputs are the input minus the low-pass, once or twice.
        assert!(high_peak <= 2.0 + 1e-12, "high-pass reached {high_peak}");
        assert!(all_peak <= 3.0 + 1e-12, "all-pass reached {all_peak}");
    }

    #[test]
    fn above_that_the_memory_can_hold_more_than_the_input_by_the_stated_factor() {
        let top = (PI * 0.49).tan();
        assert!((top - 31.82).abs() < 0.01);

        // The worst case: an input at half the sample rate with the cutoff
        // at its upper limit fills the memory, and a drop of the cutoff
        // lets it out.
        let mut filter = prepared(OnePoleMode::Lowpass, RATE);
        assert_eq!(filter.frequency(), 0.49 * RATE);
        let mut steady = 0.0f64;
        for n in 0..2_000 {
            let input = if n % 2 == 0 { 1.0 } else { -1.0 };
            steady = filter.process_sample(0, input).abs();
        }
        // Held still, the filter lets almost nothing of that through.
        assert!(steady < 0.04, "steady output {steady}");
        filter.set_frequency(0.01);
        let released = filter.process_sample(0, 1.0).abs();
        assert!(
            released > 0.99 * top && released <= top,
            "released {released}"
        );

        // Random modulation over the whole range stays within the bound.
        let mut random = Random::new(5);
        let mut peak = 0.0f64;
        for n in 0..200_000 {
            filter.set_frequency(random.log_between(0.01, RATE));
            let input = if n % 3 == 0 {
                random.between(-1.0, 1.0)
            } else if n % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            peak = peak.max(filter.process_sample(0, input).abs());
        }
        assert!(peak > 1.0 && peak <= top, "peak {peak}");

        // The high-pass and the all-pass add the input once and the low-pass
        // once or twice, and the worst case reaches those bounds: the
        // memory is filled, then the cutoff drops on a sample whose input
        // has the other sign.
        let bounds = [
            (OnePoleMode::Highpass, top + 1.0),
            (OnePoleMode::Allpass, 2.0 * top + 1.0),
        ];
        for (mode, bound) in bounds {
            // Which sign the memory ends up with is not the point, so both
            // signs of the releasing input are tried.
            let released = |input: f64| {
                let mut filter = prepared(mode, RATE);
                for n in 0..2_000 {
                    filter.process_sample(0, if n % 2 == 0 { 1.0 } else { -1.0 });
                }
                filter.set_frequency(0.01);
                filter.process_sample(0, input).abs()
            };
            let worst = released(1.0).max(released(-1.0));
            assert!(
                worst > 0.99 * bound && worst <= bound,
                "{mode:?}: {worst} of {bound}"
            );

            let mut filter = prepared(mode, RATE);
            let mut random = Random::new(9);
            let mut peak = 0.0f64;
            for n in 0..100_000 {
                filter.set_frequency(random.log_between(0.01, RATE));
                let input = if n % 3 == 0 {
                    random.between(-1.0, 1.0)
                } else if n % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
                peak = peak.max(filter.process_sample(0, input).abs());
            }
            assert!(peak <= bound, "{mode:?} reached {peak}");
        }
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        type Change = fn(&mut OnePole<f64>);
        let render = |block: usize| {
            let mut filter = OnePole::<f64>::new(OnePoleMode::Lowpass, 300.0);
            filter.prepare(&spec(2));
            let mut signal = noise::<f64>(2, 6_000, 21);
            // The same changes on the same frames, whatever the block size.
            let changes: [(usize, Change); 4] = [
                (700, |filter| filter.set_frequency(6_000.0)),
                (1_000, |filter| {
                    filter.set_mode(OnePoleMode::Highpass);
                    filter.set_frequency(40.0);
                }),
                (3_333, |filter| filter.set_mode(OnePoleMode::Allpass)),
                (6_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run_in_blocks(&mut filter, part, block));
                change(&mut filter);
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
    fn a_block_glides_to_new_settings_and_a_single_sample_takes_them_at_once() {
        // A 100 Hz tone through a low-pass well above it is the tone itself;
        // through a high-pass it is a tenth of it.
        let mut filter = prepared(OnePoleMode::Lowpass, 1_000.0);
        let mut signal = sine::<f64>(1, 9_600, 100.0, RATE, 0.5);
        let step = largest_jump(signal.channel(0));
        signal.windowed(0..4_920, |part| run_in_blocks(&mut filter, part, 512));
        // At a peak of the tone, where a switch would show most.
        filter.set_mode(OnePoleMode::Highpass);
        signal.windowed(4_920..7_200, |part| run_in_blocks(&mut filter, part, 512));
        filter.set_frequency(8_000.0);
        signal.windowed(7_200..9_600, |part| run_in_blocks(&mut filter, part, 512));
        let jump = largest_jump(signal.channel(0));
        assert!(
            jump < 1.5 * step,
            "a change jumped by {jump}, the tone by {step}"
        );
        // The settings have arrived: 100 Hz under an 8 kHz high-pass.
        let settled = strict_peak(&signal.channel(0)[9_120..]);
        let theory = 0.5 * (1.0 - lowpass_theory(8_000.0, 100.0).powi(2)).sqrt();
        assert!(
            (settled - theory).abs() < 1e-3 * theory,
            "{settled}, {theory}"
        );

        // Taken at once, the same switch is a step of nearly the whole tone.
        let mut filter = prepared(OnePoleMode::Lowpass, 1_000.0);
        let tone = sine::<f64>(1, 9_600, 100.0, RATE, 0.5);
        let mut output = Vec::new();
        for (n, &input) in tone.channel(0).iter().enumerate() {
            if n == 4_920 {
                filter.set_mode(OnePoleMode::Highpass);
            }
            output.push(filter.process_sample(0, input));
        }
        assert!(largest_jump(&output) > 0.4);
        // By the sample, a new cutoff is in force on the very next sample:
        // a low-pass that is opened wide passes a step almost at once.
        let mut filter = prepared(OnePoleMode::Lowpass, 10.0);
        assert!(filter.process_sample(0, 1.0) < 0.01);
        filter.set_frequency(RATE);
        assert!(filter.process_sample(0, 1.0) > 0.95);
    }

    #[test]
    fn a_change_of_mode_is_bounded_by_the_larger_bound_until_it_arrives() {
        // A 10 Hz square wave through an all-pass at 100 Hz: right after
        // each step the all-pass puts out nearly three times the input.
        // Switched to the low-pass 32 frames before a step, it cross-fades
        // over 480 frames, so the step falls in the fade: the output then
        // exceeds the low-pass bound of 1, not the all-pass's 3; once the
        // fade has arrived, it keeps to 1.
        let mut filter = prepared(OnePoleMode::Allpass, 100.0);
        let mut block = AudioBuffer::<f64>::new(1, 64);
        let (mut fading, mut arrived) = (0.0f64, 0.0f64);
        for index in 0..300 {
            if index == 112 {
                filter.set_mode(OnePoleMode::Lowpass);
            }
            for (i, sample) in block.channel_mut(0).iter_mut().enumerate() {
                *sample = if ((index * 64 + i) / 2_400) % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
            }
            filter.process(&mut block);
            let peak = strict_buffer_peak(&block);
            if (112..120).contains(&index) {
                assert_eq!(filter.mode(), OnePoleMode::Lowpass);
                fading = fading.max(peak);
            } else if index >= 120 {
                arrived = arrived.max(peak);
            }
        }
        assert!(fading > 2.5 && fading <= 3.0, "{fading}");
        assert!(arrived <= 1.0 && arrived > 0.9, "{arrived}");
    }

    #[test]
    fn channels_are_independent_and_reset_clears_them() {
        let mut filter = OnePole::<f32>::new(OnePoleMode::Lowpass, 200.0);
        filter.prepare(&spec(2));
        let mut buffer = AudioBuffer::<f32>::new(2, 32);
        buffer.channel_mut(0).fill(1.0);
        filter.process(&mut buffer);
        assert!(buffer.channel(0)[31] > 0.0);
        assert_eq!(buffer.channel(1), &[0.0; 32]);

        filter.reset();
        buffer.clear();
        filter.process(&mut buffer);
        assert_eq!(buffer.peak(), 0.0);
    }

    #[test]
    fn it_survives_garbage_and_use_before_it_is_prepared() {
        // Not prepared: there is no memory for any channel, so audio passes
        // untouched, and nothing panics.
        let mut filter = OnePole::<f32>::new(OnePoleMode::Highpass, 100.0);
        assert_eq!(filter.mode(), OnePoleMode::Highpass);
        let mut buffer = noise::<f32>(2, 64, 1);
        let before = buffer.clone();
        filter.process(&mut buffer);
        assert!(buffer == before);
        assert_eq!(filter.process_sample(0, 0.25), 0.25);

        filter.prepare(&spec(1));
        assert_eq!(filter.process_sample(7, 0.25), 0.25);
        // More channels and more frames than prepared for.
        let mut wide = noise::<f32>(3, 2_000, 2);
        filter.process(&mut wide);
        assert!(strict_buffer_peak(&wide) <= 2.0);

        // Settings that make no sense are limited.
        for frequency in [f64::NAN, -1.0, 0.0, f64::INFINITY, f64::MAX, 1e-300] {
            filter.set_frequency(frequency);
            let frequency = filter.frequency();
            assert!((0.01..=0.49 * RATE).contains(&frequency), "{frequency}");
            let mut block = noise::<f32>(1, 600, 3);
            filter.process(&mut block);
            // The bound of a high-pass: the input plus the memory.
            assert!(strict_buffer_peak(&block) <= 1.0 + (PI * 0.49).tan());
        }

        // A block with no channel still moves the glide on.
        let mut filter = prepared(OnePoleMode::Lowpass, 100.0);
        filter.set_frequency(10_000.0);
        let mut empty = AudioBuffer::<f64>::new(0, 480);
        filter.process(&mut empty);
        let mut impulse = AudioBuffer::<f64>::new(1, 8);
        impulse.channel_mut(0)[0] = 1.0;
        filter.process(&mut impulse);
        let g = (PI * 10_000.0 / RATE).tan();
        assert!((impulse.channel(0)[0] - g / (1.0 + g)).abs() < 1e-9);
    }
}
