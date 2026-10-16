//! A rotating loudspeaker, the cabinet a tonewheel organ is usually heard
//! through.

use core::f64::consts::TAU;
use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample};

use super::common::setting;
use crate::{DelayLine, ProcessSpec, Processor, StateVariableFilter, SvfMode};

/// The longest Doppler excursion of either rotor, in seconds: the horn's
/// at full Doppler. The delay lines are sized for it.
const LONGEST_EXCURSION_SECONDS: f64 = 0.000_4;

/// The drum's excursion against the horn's: the drum's baffle turns at a
/// smaller radius, and what it shifts in pitch is mostly the low end.
const DRUM_EXCURSION: f64 = 0.35;

/// How deep each rotor's tremolo goes at a tremolo of 1: the horn faces
/// away from a microphone 8 dB quieter than it faces it, the drum 3.7 dB.
const HORN_TREMOLO: f64 = 0.6;
const DRUM_TREMOLO: f64 = 0.35;

/// The cut-off of the one-pole low-pass the horn is heard through when it
/// faces away from a microphone, in hertz.
const HORN_DULL_HZ: f64 = 4_000.0;

/// How fast the rotors turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RotarySpeed {
    /// Braked: the rotors come to rest, wherever they are.
    Stop,
    /// Chorale: about a turn a second.
    #[default]
    Slow,
    /// Tremolo: six or seven turns a second.
    Fast,
}

/// The settings of a [`RotarySpeaker`]. Out-of-range values are limited;
/// a value that is not a number takes its default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotaryParams {
    /// Turns a second of the treble horn at slow speed, 0 to 2, and fast,
    /// 2 to 10.
    pub horn_slow: f64,
    /// See [`horn_slow`](Self::horn_slow).
    pub horn_fast: f64,
    /// Turns a second of the bass drum at slow speed, 0 to 2, and fast, 2
    /// to 10.
    pub drum_slow: f64,
    /// See [`drum_slow`](Self::drum_slow).
    pub drum_fast: f64,
    /// How long the horn takes to get 95% of the way to a new speed, 0.05
    /// to 10 seconds. The light horn changes speed quickly.
    pub horn_ramp: f64,
    /// The same for the heavy drum, 0.05 to 20 seconds.
    pub drum_ramp: f64,
    /// Where the bass goes to the drum and the treble to the horn, in hertz,
    /// 100 to 4 000.
    pub crossover: f64,
    /// How much the rotors' motion shifts the pitch, 0 to 1.
    pub doppler: f64,
    /// How much louder a rotor is facing a microphone than facing away, 0
    /// to 1.
    pub tremolo: f64,
    /// The angle between the two microphones, in degrees, 0 to 180. At 0
    /// both hear the same; at 180 they sit on opposite sides.
    pub spread: f64,
    /// The levels of the horn and the drum, as gains, 0 to 4.
    pub horn_level: f64,
    /// See [`horn_level`](Self::horn_level).
    pub drum_level: f64,
}

impl Default for RotaryParams {
    /// The proportions of the classic cabinet: horn 0.8 and 6.7 turns a
    /// second, drum 0.7 and 5.9, the horn changing speed in under a second
    /// and the drum in four and a half, crossing over at 800 Hz, heard by
    /// two microphones 120 degrees apart.
    fn default() -> Self {
        Self {
            horn_slow: 0.8,
            horn_fast: 6.7,
            drum_slow: 0.7,
            drum_fast: 5.9,
            horn_ramp: 0.8,
            drum_ramp: 4.5,
            crossover: 800.0,
            doppler: 1.0,
            tremolo: 1.0,
            spread: 120.0,
            horn_level: 1.0,
            drum_level: 1.0,
        }
    }
}

impl RotaryParams {
    fn limited(self) -> Self {
        let default = Self::default();
        Self {
            horn_slow: setting(self.horn_slow, 0.0, 2.0, default.horn_slow),
            horn_fast: setting(self.horn_fast, 2.0, 10.0, default.horn_fast),
            drum_slow: setting(self.drum_slow, 0.0, 2.0, default.drum_slow),
            drum_fast: setting(self.drum_fast, 2.0, 10.0, default.drum_fast),
            horn_ramp: setting(self.horn_ramp, 0.05, 10.0, default.horn_ramp),
            drum_ramp: setting(self.drum_ramp, 0.05, 20.0, default.drum_ramp),
            crossover: setting(self.crossover, 100.0, 4_000.0, default.crossover),
            doppler: setting(self.doppler, 0.0, 1.0, default.doppler),
            tremolo: setting(self.tremolo, 0.0, 1.0, default.tremolo),
            spread: setting(self.spread, 0.0, 180.0, default.spread),
            horn_level: setting(self.horn_level, 0.0, 4.0, default.horn_level),
            drum_level: setting(self.drum_level, 0.0, 4.0, default.drum_level),
        }
    }
}

/// A rotor: where it is in its turn and how fast it turns.
#[derive(Debug, Clone, Copy)]
struct Rotor {
    /// Position, in turns from 0 up to 1, and where it starts.
    angle: f64,
    start: f64,
    /// Turns a second now, and where that is heading.
    rate: f64,
    target: f64,
    /// The share of the way to the target covered in a sample.
    follow: f64,
}

impl Rotor {
    const fn new(start: f64) -> Self {
        Self {
            angle: start,
            start,
            rate: 0.0,
            target: 0.0,
            follow: 1.0,
        }
    }

    #[inline]
    fn advance(&mut self, sample_rate: f64) {
        self.rate += (self.target - self.rate) * self.follow;
        self.angle += self.rate / sample_rate;
        if self.angle >= 1.0 {
            self.angle -= 1.0;
        }
    }
}

/// A rotating loudspeaker, after the cabinet a tonewheel organ is usually
/// played through: a treble horn and a bass drum turning at different
/// speeds, heard by two microphones.
///
/// The input (the average of its channels) is split at the crossover by a
/// fourth-order Linkwitz-Riley pair, two Butterworth stages each way, whose
/// halves add back up to an all-pass. Each rotor then reaches each
/// microphone
///
/// - **delayed** by an amount that swings with the rotor's angle, read
///   through a cubic from a delay line, which is the Doppler shift of a
///   source that moves towards and away from the microphone;
/// - **louder** facing the microphone than facing away (the tremolo);
/// - and, for the horn, **brighter** facing it: facing away it is heard
///   through a one-pole low-pass at 4 kHz, facing it directly, and in
///   between through a mix of the two, as deep as the tremolo.
///
/// The microphones stand [`spread`](RotaryParams::spread) degrees apart:
/// the first goes to the first channel and the second to the second, and
/// further channels, or a single one, get their average. Speed changes
/// ([`set_speed`](Self::set_speed)) are not instant: the light horn gets up
/// to speed in under a second, the heavy drum in several, and that
/// acceleration is much of the sound.
///
/// The effect is a model of what the listener hears, not of the cabinet's
/// acoustics: there is no cabinet resonance and no amplifier. For the
/// overdriven sound, put an oversampled
/// [`Waveshaper`](crate::Waveshaper) in front. The output is about a
/// millisecond late, as the air between a loudspeaker and a microphone
/// makes it; [`latency_samples`](Processor::latency_samples) reports none.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::instruments::{RotaryParams, RotarySpeaker, RotarySpeed};
/// use manguebyte_dsp::{ProcessSpec, Processor};
///
/// let mut leslie = RotarySpeaker::<f32>::new(RotaryParams::default());
/// leslie.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 });
/// leslie.set_speed(RotarySpeed::Fast);
///
/// let mut block = AudioBuffer::<f32>::new(2, 256);
/// block.channel_mut(0).fill(0.5);
/// leslie.process(&mut block);
/// // The horn has begun to speed up.
/// assert!(leslie.horn_rate() > RotaryParams::default().horn_slow);
/// ```
#[derive(Debug, Clone)]
pub struct RotarySpeaker<S: Sample = f32> {
    params: RotaryParams,
    speed: RotarySpeed,
    sample_rate: f64,
    horn: Rotor,
    drum: Rotor,
    low: [StateVariableFilter<f64>; 2],
    high: [StateVariableFilter<f64>; 2],
    horn_line: DelayLine<f64>,
    drum_line: DelayLine<f64>,
    /// The horn heard facing away, through a low-pass, for each
    /// microphone, and that low-pass's coefficient.
    horn_tone: [f64; 2],
    dull: f64,
    marker: PhantomData<S>,
}

impl<S: Sample> RotarySpeaker<S> {
    /// A speaker turning slowly. Until it is prepared it passes nothing.
    pub fn new(params: RotaryParams) -> Self {
        let params = params.limited();
        let filter = |mode: SvfMode| {
            StateVariableFilter::new(mode, params.crossover, core::f64::consts::FRAC_1_SQRT_2)
        };
        let mut speaker = Self {
            params,
            speed: RotarySpeed::Slow,
            sample_rate: 48_000.0,
            horn: Rotor::new(0.0),
            drum: Rotor::new(0.37),
            low: [filter(SvfMode::Lowpass), filter(SvfMode::Lowpass)],
            high: [filter(SvfMode::Highpass), filter(SvfMode::Highpass)],
            horn_line: DelayLine::new(4),
            drum_line: DelayLine::new(4),
            horn_tone: [0.0; 2],
            dull: 1.0,
            marker: PhantomData,
        };
        speaker.set_speed(RotarySpeed::Slow);
        speaker.settle();
        speaker
    }

    /// Takes new settings, limited to their ranges. Real-time safe. A change
    /// of speed setting is approached at the rotors' own pace.
    pub fn set_params(&mut self, params: RotaryParams) {
        self.params = params.limited();
        for filter in self.low.iter_mut().chain(&mut self.high) {
            filter.set_frequency(self.params.crossover);
        }
        self.update_rotors();
    }

    /// The settings, as limited.
    pub fn params(&self) -> &RotaryParams {
        &self.params
    }

    /// Changes the speed. Real-time safe. The rotors accelerate or brake
    /// to it over their ramp times.
    pub fn set_speed(&mut self, speed: RotarySpeed) {
        self.speed = speed;
        self.update_rotors();
    }

    /// The speed last set.
    pub fn speed(&self) -> RotarySpeed {
        self.speed
    }

    /// How many turns a second the horn is making now.
    pub fn horn_rate(&self) -> f64 {
        self.horn.rate
    }

    /// How many turns a second the drum is making now.
    pub fn drum_rate(&self) -> f64 {
        self.drum.rate
    }

    fn update_rotors(&mut self) {
        let params = &self.params;
        let (horn, drum) = match self.speed {
            RotarySpeed::Stop => (0.0, 0.0),
            RotarySpeed::Slow => (params.horn_slow, params.drum_slow),
            RotarySpeed::Fast => (params.horn_fast, params.drum_fast),
        };
        // 95% of the way in the ramp time: e^-3 is 5%.
        let follow = |seconds: f64| 1.0 - (-3.0 / (seconds * self.sample_rate)).exp();
        self.horn.target = horn;
        self.horn.follow = follow(params.horn_ramp);
        self.drum.target = drum;
        self.drum.follow = follow(params.drum_ramp);
    }

    /// Puts the rotors at their starting angles, turning at their target
    /// speeds.
    fn settle(&mut self) {
        for rotor in [&mut self.horn, &mut self.drum] {
            rotor.angle = rotor.start;
            rotor.rate = rotor.target;
        }
    }
}

impl<S: Sample> Processor<S> for RotarySpeaker<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        let mono = ProcessSpec {
            num_channels: 1,
            ..*spec
        };
        for filter in self.low.iter_mut().chain(&mut self.high) {
            Processor::<f64>::prepare(filter, &mono);
        }
        let longest = (2.0 * LONGEST_EXCURSION_SECONDS * spec.sample_rate).ceil() as usize + 8;
        self.horn_line = DelayLine::new(longest);
        self.drum_line = DelayLine::new(longest);
        let cutoff = HORN_DULL_HZ.min(0.45 * spec.sample_rate);
        self.dull = 1.0 - (-TAU * cutoff / spec.sample_rate).exp();
        self.update_rotors();
        Processor::<S>::reset(self);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let channels = buffer.num_channels();
        if channels == 0 {
            return;
        }
        let rate = self.sample_rate;
        let params = self.params;
        let excursion = params.doppler * LONGEST_EXCURSION_SECONDS * rate;
        let drum_excursion = DRUM_EXCURSION * excursion;
        // Far enough back that the cubic always has a sample on each side,
        // and the same for both rotors, so that at rest their halves add
        // back up as the crossover made them.
        let base = excursion + 2.0;
        let microphones = [params.spread / 720.0, -params.spread / 720.0];
        let horn_depth = HORN_TREMOLO * params.tremolo;
        let drum_depth = DRUM_TREMOLO * params.tremolo;
        for frame in 0..buffer.num_frames() {
            let mut input = 0.0;
            for channel in 0..channels {
                input += buffer.channel(channel)[frame].to_f64();
            }
            input /= channels as f64;
            let low = self.low[0].process_sample(0, input);
            let low = self.low[1].process_sample(0, low);
            let high = self.high[0].process_sample(0, input);
            let high = self.high[1].process_sample(0, high);
            self.horn_line.push(high);
            self.drum_line.push(low);
            self.horn.advance(rate);
            self.drum.advance(rate);

            let mut out = [0.0; 2];
            for ((out, &microphone), tone) in
                out.iter_mut().zip(&microphones).zip(&mut self.horn_tone)
            {
                let facing = (TAU * (self.horn.angle - microphone)).cos();
                let horn = self.horn_line.read_cubic(base - excursion * facing);
                // Duller and quieter as it turns away from the microphone.
                *tone += (horn - *tone) * self.dull;
                let away = horn_depth * 0.5 * (1.0 - facing);
                let horn = (1.0 - away) * ((1.0 - away) * horn + away * *tone);

                let facing = (TAU * (self.drum.angle - microphone)).cos();
                let drum = self.drum_line.read_cubic(base - drum_excursion * facing);
                let drum_gain = 1.0 - drum_depth * 0.5 * (1.0 - facing);
                *out = params.horn_level * horn + params.drum_level * drum_gain * drum;
            }
            let both = 0.5 * (out[0] + out[1]);
            for channel in 0..channels {
                let value = match (channels, channel) {
                    (1, _) => both,
                    (_, 0) => out[0],
                    (_, 1) => out[1],
                    _ => both,
                };
                buffer.channel_mut(channel)[frame] = S::from_f64(value);
            }
        }
    }

    /// Clears the delay lines and the filters, and puts the rotors back at
    /// their starting angles, turning at the speed set.
    fn reset(&mut self) {
        for filter in self.low.iter_mut().chain(&mut self.high) {
            Processor::<f64>::reset(filter);
        }
        self.horn_line.clear();
        self.drum_line.clear();
        self.horn_tone = [0.0; 2];
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments::measure::{
        RATE, amplitude_at, decibels, frequency_near, frequency_near_at,
    };
    use crate::testing::{run_in_blocks, sine};

    fn speaker(params: RotaryParams, speed: RotarySpeed) -> RotarySpeaker<f64> {
        let mut speaker = RotarySpeaker::new(params);
        speaker.set_speed(speed);
        speaker.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: 2,
        });
        speaker
    }

    #[test]
    fn standing_still_with_no_effects_it_passes_every_frequency() {
        let params = RotaryParams {
            doppler: 0.0,
            tremolo: 0.0,
            ..RotaryParams::default()
        };
        for frequency in [60.0, 400.0, 800.0, 1_600.0, 8_000.0] {
            let mut leslie = speaker(params, RotarySpeed::Stop);
            let mut signal = sine::<f64>(2, 24_000, frequency, RATE, 0.5);
            run_in_blocks(&mut leslie, &mut signal, 256);
            let gain = amplitude_at(&signal.channel(0)[8_000..], frequency) / 0.5;
            assert!(
                decibels(gain).abs() < 0.01,
                "{frequency} Hz: {} dB",
                decibels(gain)
            );
            assert_eq!(
                signal.channel(0),
                signal.channel(1),
                "the microphones agree"
            );
        }
    }

    #[test]
    fn the_horn_turns_shifts_the_pitch_and_the_level_and_speeds_up_in_its_time() {
        let params = RotaryParams::default();
        let mut leslie = speaker(params, RotarySpeed::Fast);
        assert!((leslie.horn_rate() - params.horn_fast).abs() < 1e-12);
        let frequency = 3_000.0;
        let mut signal = sine::<f64>(2, 96_000, frequency, RATE, 0.5);
        run_in_blocks(&mut leslie, &mut signal, 256);
        let left = signal.channel(0);

        // The level swings once a turn: the envelope's strongest component
        // is at the horn's speed.
        let window = 96;
        let levels: Vec<f64> = left
            .chunks(window)
            .map(|chunk| chunk.iter().fold(0.0f64, |peak, x| peak.max(x.abs())))
            .collect();
        let rate = RATE / window as f64;
        let swing = frequency_near_at(&levels[200..], rate, params.horn_fast);
        assert!((swing - params.horn_fast).abs() < 0.01, "{swing}");
        let (loud, quiet) = levels[200..]
            .iter()
            .fold((0.0f64, 1.0f64), |(high, low), &x| {
                (high.max(x), low.min(x))
            });
        let depth = decibels(loud / quiet);
        println!("tremolo {depth:.1} dB at {swing:.3} turns a second");
        assert!((5.0..12.0).contains(&depth), "{depth}");

        // The pitch swings by the Doppler shift of the excursion.
        let excursion = LONGEST_EXCURSION_SECONDS * RATE;
        let expected = TAU * params.horn_fast * excursion / RATE;
        let mut deviation = 0.0f64;
        for start in (24_000..90_000).step_by(240) {
            let local = frequency_near(&left[start..start + 480], frequency);
            deviation = deviation.max((local / frequency - 1.0).abs());
        }
        println!(
            "pitch swings by {:.2}% against {:.2}%",
            100.0 * deviation,
            100.0 * expected
        );
        assert!(
            (deviation / expected - 1.0).abs() < 0.2,
            "{deviation} against {expected}"
        );

        // The microphones hear different things.
        assert!(signal.channel(0) != signal.channel(1));

        // From slow to fast, the horn is 95% of the way in its ramp time.
        let mut leslie = speaker(params, RotarySpeed::Slow);
        leslie.set_speed(RotarySpeed::Fast);
        let mut block = AudioBuffer::<f64>::new(2, (params.horn_ramp * RATE) as usize);
        leslie.process(&mut block);
        let covered =
            (leslie.horn_rate() - params.horn_slow) / (params.horn_fast - params.horn_slow);
        assert!((covered - 0.95).abs() < 0.01, "{covered}");
        // The drum is far behind.
        let drum = (leslie.drum_rate() - params.drum_slow) / (params.drum_fast - params.drum_slow);
        assert!(drum < 0.5, "{drum}");
    }

    #[test]
    fn the_output_does_not_depend_on_the_block_size_and_stays_finite() {
        let reference = {
            let mut leslie = speaker(RotaryParams::default(), RotarySpeed::Fast);
            let mut signal = crate::testing::noise::<f64>(2, 20_000, 3);
            run_in_blocks(&mut leslie, &mut signal, 256);
            signal
        };
        for block in [1, 7, 64, 1_000] {
            let mut leslie = speaker(RotaryParams::default(), RotarySpeed::Fast);
            let mut signal = crate::testing::noise::<f64>(2, 20_000, 3);
            run_in_blocks(&mut leslie, &mut signal, block);
            assert!(signal == reference, "blocks of {block}");
        }

        let wild = RotaryParams {
            horn_slow: f64::NAN,
            horn_fast: f64::INFINITY,
            drum_slow: -1.0,
            drum_fast: 1e9,
            horn_ramp: 0.0,
            drum_ramp: f64::NAN,
            crossover: 1e9,
            doppler: 1e9,
            tremolo: f64::NEG_INFINITY,
            spread: 1e9,
            horn_level: 1e9,
            drum_level: f64::NAN,
        };
        let mut leslie = speaker(wild, RotarySpeed::Fast);
        let mut signal = crate::testing::noise::<f64>(1, 10_000, 4);
        signal.channel_mut(0)[100] = 1e30;
        run_in_blocks(&mut leslie, &mut signal, 100);
        assert!(crate::numeric::strict_buffer_peak(&signal).is_finite());
    }
}
