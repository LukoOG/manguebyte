//! Test signals, synthesised the way the EBU documents describe them.
//!
//! Each test file compiles this module for itself and uses only part of it.
#![allow(dead_code)]

use core::f64::consts::{PI, TAU};

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::ProcessSpec;

/// The peak amplitude of a sine at `dbfs`.
pub fn amplitude(dbfs: f64) -> f64 {
    10f64.powf(dbfs / 20.0)
}

/// Linear amplitude in decibels, without a floor.
pub fn db(amplitude: f64) -> f64 {
    20.0 * amplitude.log10()
}

/// A processing specification; the meters only take the sample rate from
/// it.
pub fn spec(sample_rate: f64, channels: usize) -> ProcessSpec {
    ProcessSpec {
        sample_rate,
        max_block_size: 4_800,
        num_channels: channels,
    }
}

/// A sine generator that keeps counting samples from one segment to the
/// next, the way a test file made of "tones similar to #1" at different
/// levels does: the level changes, the phase carries on.
pub struct Tone {
    sample_rate: f64,
    frequency: f64,
    position: u64,
    sine: Vec<f64>,
    block: AudioBuffer<f32>,
}

impl Tone {
    /// A tone of `frequency` hertz for `channels` channels. It is played in
    /// blocks of 10 ms, so a test can look at the meter a hundred times a
    /// second.
    pub fn new(sample_rate: f64, frequency: f64, channels: usize) -> Self {
        let frames = (sample_rate / 100.0).round() as usize;
        Self {
            sample_rate,
            frequency,
            position: 0,
            sine: vec![0.0; frames],
            block: AudioBuffer::new(channels, frames),
        }
    }

    /// Plays `seconds` of the tone with a peak level of `levels[channel]`
    /// dBFS in each channel, `None` being silence, and hands every block to
    /// `sink`. The samples are 32-bit floats, which hold the 24 bits of the
    /// EBU's files exactly.
    pub fn play(
        &mut self,
        levels: &[Option<f64>],
        seconds: f64,
        mut sink: impl FnMut(&AudioBuffer<f32>),
    ) {
        assert_eq!(levels.len(), self.block.num_channels());
        let amplitudes: Vec<f64> = levels
            .iter()
            .map(|level| level.map_or(0.0, amplitude))
            .collect();
        let mut left = (seconds * self.sample_rate).round() as usize;
        let capacity = self.sine.len();
        while left > 0 {
            let frames = left.min(capacity);
            for (frame, value) in self.sine[..frames].iter_mut().enumerate() {
                let n = (self.position + frame as u64) as f64;
                *value = (TAU * self.frequency * n / self.sample_rate).sin();
            }
            self.block.set_num_frames(frames);
            for (channel, &amplitude) in self.block.iter_channels_mut().zip(&amplitudes) {
                for (sample, &value) in channel.iter_mut().zip(&self.sine) {
                    *sample = (amplitude * value) as f32;
                }
            }
            sink(&self.block);
            self.position += frames as u64;
            left -= frames;
        }
    }
}

/// The same level in both channels of a stereo pair.
pub fn both(dbfs: f64) -> [Option<f64>; 2] {
    [Some(dbfs); 2]
}

/// Silence in both channels of a stereo pair.
pub const SILENCE: [Option<f64>; 2] = [None; 2];

/// A raised-cosine fade over the first and last `frames` samples.
pub fn fade(signal: &mut [f64], frames: usize) {
    let len = signal.len();
    for index in 0..frames.min(len / 2) {
        let gain = 0.5 - 0.5 * (PI * index as f64 / frames as f64).cos();
        signal[index] *= gain;
        signal[len - 1 - index] *= gain;
    }
}

/// The same signal in both channels of a stereo buffer of 32-bit floats.
pub fn stereo(signal: &[f64]) -> AudioBuffer<f32> {
    let mut buffer = AudioBuffer::new(2, signal.len());
    for channel in buffer.iter_channels_mut() {
        for (sample, &value) in channel.iter_mut().zip(signal) {
            *sample = value as f32;
        }
    }
    buffer
}

/// The periodic tones the pitch tests synthesise. Every harmonic below half
/// the sample rate is present unless the kind leaves it out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToneKind {
    Sine,
    /// Harmonics 1/k.
    Saw,
    /// Odd harmonics 1/k.
    Square,
    /// A sawtooth without its first harmonic.
    MissingFundamental,
    /// A fundamental and a second harmonic this many times as strong.
    StrongSecond(f64),
}

impl ToneKind {
    /// The amplitude of harmonic number `harmonic`, from 1.
    pub fn amplitude(self, harmonic: usize) -> f64 {
        let k = harmonic as f64;
        match self {
            ToneKind::Sine => f64::from(harmonic == 1),
            ToneKind::Saw => 1.0 / k,
            ToneKind::Square if harmonic % 2 == 1 => 1.0 / k,
            ToneKind::Square => 0.0,
            ToneKind::MissingFundamental if harmonic == 1 => 0.0,
            ToneKind::MissingFundamental => 1.0 / k,
            ToneKind::StrongSecond(gain) => match harmonic {
                1 => 1.0,
                2 => gain,
                _ => 0.0,
            },
        }
    }

    /// How many of its harmonics lie below half the sample rate.
    pub fn harmonics_present(self, frequency: f64, sample_rate: f64) -> usize {
        let highest = ((sample_rate / 2.0 - 1.0) / frequency).floor() as usize;
        (1..=highest)
            .filter(|&harmonic| self.amplitude(harmonic) != 0.0)
            .count()
    }
}

/// `frames` samples of `kind` at `frequency`, with each harmonic starting
/// at `phase` times its number, scaled to a peak of 0.9.
pub fn harmonic_tone(
    kind: ToneKind,
    frequency: f64,
    sample_rate: f64,
    frames: usize,
    phase: f64,
) -> Vec<f32> {
    let mut signal = vec![0.0f64; frames];
    let highest = ((sample_rate / 2.0 - 1.0) / frequency).floor() as usize;
    for harmonic in 1..=highest {
        let amplitude = kind.amplitude(harmonic);
        if amplitude == 0.0 {
            continue;
        }
        // Each harmonic as a rotating pointer: one complex multiplication
        // per sample instead of a sine, exact to about 1e-13 over the few
        // thousand samples a test needs.
        let step = TAU * harmonic as f64 * frequency / sample_rate;
        let (step_sin, step_cos) = step.sin_cos();
        let (mut sin, mut cos) = (phase * harmonic as f64).sin_cos();
        for sample in signal.iter_mut() {
            *sample += amplitude * sin;
            (sin, cos) = (
                sin * step_cos + cos * step_sin,
                cos * step_cos - sin * step_sin,
            );
        }
    }
    let peak = signal
        .iter()
        .fold(0.0f64, |peak, value| peak.max(value.abs()));
    signal
        .iter()
        .map(|value| (0.9 * value / peak) as f32)
        .collect()
}

/// A small deterministic generator (SplitMix64) of numbers in `-1.0..1.0`.
pub struct Noise(pub u64);

impl Noise {
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Feeds `signal` to `sink` cut into blocks whose lengths cycle through
/// `pattern`.
pub fn in_blocks(
    signal: &AudioBuffer<f32>,
    pattern: &[usize],
    mut sink: impl FnMut(&AudioBuffer<f32>),
) {
    let largest = pattern.iter().copied().max().unwrap_or(1).max(1);
    let mut block = AudioBuffer::<f32>::new(signal.num_channels(), largest);
    let mut start = 0;
    let mut turn = 0;
    while start < signal.num_frames() {
        let frames = pattern[turn % pattern.len()]
            .max(1)
            .min(signal.num_frames() - start);
        turn += 1;
        block.set_num_frames(frames);
        for (channel, source) in block.iter_channels_mut().zip(signal.iter_channels()) {
            channel.copy_from_slice(&source[start..start + frames]);
        }
        sink(&block);
        start += frames;
    }
}
