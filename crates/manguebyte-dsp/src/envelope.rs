use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor};

/// Timing and level settings of an [`Adsr`] envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrParams {
    /// Seconds to rise from silence to full level.
    pub attack: f64,
    /// Seconds to fall from full level to the sustain level.
    pub decay: f64,
    /// Level held while the note stays down, `0.0..=1.0`.
    pub sustain: f64,
    /// Seconds to fall from the sustain level to silence.
    pub release: f64,
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self {
            attack: 0.01,
            decay: 0.1,
            sustain: 0.7,
            release: 0.2,
        }
    }
}

/// Slack that lets a stage end on time despite accumulated rounding error.
const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A linear attack-decay-sustain-release envelope.
///
/// As a [`Processor`] it multiplies the block by the envelope, applying the
/// same curve to every channel.
#[derive(Debug, Clone, Copy)]
pub struct Adsr {
    params: AdsrParams,
    sample_rate: f64,
    stage: Stage,
    level: f64,
    /// Level change per sample while releasing, fixed when the note ends.
    release_step: f64,
}

impl Adsr {
    /// Creates an idle envelope. Until [`prepare`](Processor::prepare) or
    /// [`set_sample_rate`](Self::set_sample_rate) is called it assumes 44.1 kHz.
    pub fn new(params: AdsrParams) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            stage: Stage::Idle,
            level: 0.0,
            release_step: 0.0,
        }
    }

    /// Replaces the settings. Takes effect from the next sample.
    pub fn set_params(&mut self, params: AdsrParams) {
        self.params = params;
    }

    /// Changes the sample rate in hertz.
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
    }

    /// Starts the attack from the current level, so retriggering a sounding
    /// note does not click.
    pub fn note_on(&mut self) {
        self.stage = Stage::Attack;
    }

    /// Starts the release from the current level.
    pub fn note_off(&mut self) {
        if self.stage != Stage::Idle {
            self.release_step = self.level * self.rate(self.params.release);
            self.stage = Stage::Release;
        }
    }

    /// Drops to silence at once, skipping the release. For a voice that is
    /// being cut off or taken over for another note.
    pub fn silence(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    /// Whether the envelope is producing anything other than silence.
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// The level most recently produced.
    pub fn level(&self) -> f64 {
        self.level
    }

    /// Advances one sample and returns the envelope level, `0.0..=1.0`.
    #[inline]
    pub fn next_sample(&mut self) -> f64 {
        let sustain = self.params.sustain.clamp(0.0, 1.0);
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.rate(self.params.attack);
                if self.level >= 1.0 - EPSILON {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level -= (1.0 - sustain) * self.rate(self.params.decay);
                if self.level <= sustain + EPSILON {
                    self.level = sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = sustain,
            Stage::Release => {
                self.level -= self.release_step;
                // A zero step means the release began from silence.
                if self.level <= EPSILON || self.release_step <= 0.0 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }

    /// Fraction of a full-scale sweep covered per sample for a stage lasting
    /// `seconds`. Stages shorter than one sample complete at once.
    #[inline]
    fn rate(&self, seconds: f64) -> f64 {
        let samples = seconds * self.sample_rate;
        if samples < 1.0 { 1.0 } else { 1.0 / samples }
    }
}

impl<S: Sample> Processor<S> for Adsr {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.set_sample_rate(spec.sample_rate);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        // Every channel must see the same curve, so each one runs a copy.
        let start = *self;
        let mut end = start;
        for _ in 0..buffer.num_frames() {
            end.next_sample();
        }
        for channel in buffer.iter_channels_mut() {
            let mut envelope = start;
            for sample in channel {
                *sample *= S::from_f64(envelope.next_sample());
            }
        }
        *self = end;
    }

    fn reset(&mut self) {
        self.silence();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 samples of attack, 10 of decay, 20 of release.
    fn envelope() -> Adsr {
        let mut adsr = Adsr::new(AdsrParams {
            attack: 0.01,
            decay: 0.01,
            sustain: 0.5,
            release: 0.02,
        });
        adsr.set_sample_rate(1_000.0);
        adsr
    }

    fn take(adsr: &mut Adsr, count: usize) -> Vec<f64> {
        (0..count).map(|_| adsr.next_sample()).collect()
    }

    #[test]
    fn idle_until_triggered() {
        let mut adsr = envelope();
        assert!(!adsr.is_active());
        assert_eq!(take(&mut adsr, 4), vec![0.0; 4]);
    }

    #[test]
    fn walks_through_every_stage() {
        let mut adsr = envelope();
        adsr.note_on();

        let attack = take(&mut adsr, 10);
        assert!((attack[0] - 0.1).abs() < 1e-12);
        assert_eq!(attack[9], 1.0);
        assert!(attack.windows(2).all(|pair| pair[1] > pair[0]));

        let decay = take(&mut adsr, 10);
        assert!((decay[0] - 0.95).abs() < 1e-12);
        assert_eq!(decay[9], 0.5);

        assert_eq!(take(&mut adsr, 50), vec![0.5; 50]);

        adsr.note_off();
        let release = take(&mut adsr, 20);
        assert!((release[0] - 0.475).abs() < 1e-12);
        assert!(release.windows(2).all(|pair| pair[1] < pair[0]));
        assert_eq!(release[19], 0.0);
        assert!(!adsr.is_active());
    }

    #[test]
    fn release_from_mid_attack_takes_the_full_release_time() {
        let mut adsr = envelope();
        adsr.note_on();
        take(&mut adsr, 4);
        adsr.note_off();
        let release = take(&mut adsr, 20);
        assert!(release[18] > 0.0);
        assert_eq!(release[19], 0.0);
    }

    #[test]
    fn zero_length_stages_complete_immediately() {
        let mut adsr = Adsr::new(AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        });
        adsr.note_on();
        assert_eq!(adsr.next_sample(), 1.0);
        assert_eq!(adsr.next_sample(), 1.0);
        adsr.note_off();
        assert_eq!(adsr.next_sample(), 0.0);
        assert!(!adsr.is_active());
    }

    #[test]
    fn processor_shapes_every_channel_alike() {
        let mut adsr = envelope();
        adsr.note_on();
        let mut buffer = AudioBuffer::<f32>::new(2, 16);
        for channel in buffer.iter_channels_mut() {
            channel.fill(1.0);
        }
        adsr.process(&mut buffer);
        assert_eq!(buffer.channel(0), buffer.channel(1));
        assert!((buffer.channel(0)[0] - 0.1).abs() < 1e-6);
        assert_eq!(buffer.channel(0)[9], 1.0);
        // State advanced by exactly one block: 10 attack + 6 decay samples.
        assert!((adsr.level() - 0.7).abs() < 1e-9);
    }
}
