//! Playing the instruments and measuring what comes out, for their tests.

use core::f64::consts::{PI, TAU};

use manguebyte_core::AudioBuffer;

use crate::event::NoteRef;
use crate::fft::{Complex, RealFft};
use crate::{ProcessSpec, Processor, Synth, Voice};

/// The sample rate the tests run at.
pub(crate) const RATE: f64 = 48_000.0;

/// Something the player does at a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Act {
    /// A note starts.
    On {
        key: u8,
        velocity: f32,
        accent: bool,
    },
    /// A note is released.
    Off(u8),
    /// The pitch wheel moves, in semitones.
    Bend(f64),
}

pub(crate) fn on(key: u8, velocity: f32) -> Act {
    Act::On {
        key,
        velocity,
        accent: false,
    }
}

pub(crate) fn accented(key: u8, velocity: f32) -> Act {
    Act::On {
        key,
        velocity,
        accent: true,
    }
}

pub(crate) fn off(key: u8) -> Act {
    Act::Off(key)
}

/// A synth of `voices`, prepared for one channel at [`RATE`].
pub(crate) fn synth_of<V: Voice<f64>>(voices: impl IntoIterator<Item = V>) -> Synth<V, f64> {
    let mut synth = Synth::new(voices);
    synth.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: 512,
        num_channels: 1,
    });
    synth
}

/// Plays `script` (frames and acts, in order) on `synth` for `frames`
/// frames, rendering at most `block` frames at a time, and returns the
/// output. The acts reach the synth between renders, on their exact frame.
pub(crate) fn play<V: Voice<f64>>(
    synth: &mut Synth<V, f64>,
    script: &[(usize, Act)],
    frames: usize,
    block: usize,
) -> Vec<f64> {
    let block = block.max(1);
    let mut output = Vec::with_capacity(frames);
    let mut buffer = AudioBuffer::<f64>::new(1, block);
    let mut next = 0;
    let mut frame = 0;
    while frame < frames {
        while let Some(&(at, act)) = script.get(next) {
            if at > frame {
                break;
            }
            match act {
                Act::On {
                    key,
                    velocity,
                    accent,
                } => synth.note_on_accented(NoteRef::new(0, key), velocity, accent),
                Act::Off(key) => synth.note_off(NoteRef::new(0, key), 0.0),
                Act::Bend(semitones) => {
                    synth
                        .voices_mut()
                        .for_each(|voice| voice.pitch_bend(semitones));
                }
            }
            next += 1;
        }
        let until = script.get(next).map_or(frames, |&(at, _)| at.min(frames));
        let end = (frame + block).min(until).max(frame + 1).min(frames);
        buffer.set_num_frames(end - frame);
        synth.process(&mut buffer);
        output.extend_from_slice(buffer.channel(0));
        frame = end;
    }
    output
}

/// One note on a fresh synth of `voices`, held for `held` seconds, the
/// whole render lasting `seconds`.
pub(crate) fn note<V: Voice<f64>>(
    voices: impl IntoIterator<Item = V>,
    act: Act,
    held: f64,
    seconds: f64,
) -> Vec<f64> {
    let Act::On { key, .. } = act else {
        panic!("a note starts with a note on");
    };
    let mut synth = synth_of(voices);
    let release = (held * RATE) as usize;
    play(
        &mut synth,
        &[(0, act), (release, off(key))],
        (seconds * RATE) as usize,
        256,
    )
}

/// The part of `signal` from `from` to `to` seconds.
pub(crate) fn between(signal: &[f64], from: f64, to: f64) -> &[f64] {
    let start = ((from * RATE) as usize).min(signal.len());
    let end = ((to * RATE) as usize).clamp(start, signal.len());
    &signal[start..end]
}

/// The component of `signal` at `frequency`, through a Hann window: its
/// amplitude is twice the size of the result over the window's sum.
fn component(signal: &[f64], frequency: f64) -> (f64, f64) {
    component_at(signal, RATE, frequency)
}

/// [`component`] for a signal sampled at `rate`.
fn component_at(signal: &[f64], rate: f64, frequency: f64) -> (f64, f64) {
    let len = signal.len().max(2);
    let omega = TAU * frequency / rate;
    let (mut re, mut im, mut weight) = (0.0, 0.0, 0.0);
    for (n, &x) in signal.iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * n as f64 / (len - 1) as f64).cos();
        let (sin, cos) = (omega * n as f64).sin_cos();
        re += w * x * cos;
        im -= w * x * sin;
        weight += w;
    }
    (2.0 * re / weight, 2.0 * im / weight)
}

/// The amplitude of the component of `signal` at `frequency`.
pub(crate) fn amplitude_at(signal: &[f64], frequency: f64) -> f64 {
    let (re, im) = component(signal, frequency);
    re.hypot(im)
}

/// The frequency of the component of `signal` near `expected`, from how
/// far its phase turns between two overlapping windows.
///
/// The windows are first 32 samples apart, which reads any frequency within
/// 750 Hz of `expected` without ambiguity, and then further apart, each
/// step refining the last. The answer is exact to far less than a cent for
/// a steady or decaying tone, whatever the other components are, as long
/// as they are a few window widths (a few times `RATE / signal.len()`
/// hertz) away.
pub(crate) fn frequency_near(signal: &[f64], expected: f64) -> f64 {
    frequency_near_at(signal, RATE, expected)
}

/// [`frequency_near`] for a signal sampled at `rate`. Steps whose windows
/// would be shorter than half the signal are left out.
pub(crate) fn frequency_near_at(signal: &[f64], rate: f64, expected: f64) -> f64 {
    let mut estimate = expected;
    for hop in [32, 1_024, signal.len() / 3] {
        if hop == 0 || hop > signal.len() / 2 {
            continue;
        }
        let window = signal.len() - hop;
        let (re1, im1) = component_at(&signal[..window], rate, estimate);
        let (re2, im2) = component_at(&signal[hop..hop + window], rate, estimate);
        let turned = im2.atan2(re2) - im1.atan2(re1);
        let predicted = TAU * estimate * hop as f64 / rate;
        let error = (turned - predicted + PI).rem_euclid(TAU) - PI;
        estimate += error * rate / (TAU * hop as f64);
    }
    estimate
}

/// How far `measured` is from `expected`, in cents.
pub(crate) fn cents(measured: f64, expected: f64) -> f64 {
    1200.0 * (measured / expected).log2()
}

/// How long the component of `signal` at `frequency` takes to fall by
/// 60 dB, measured between `from` and `to` seconds by a straight line
/// through its level in decibels, taken every 20 ms over windows of 80 ms.
pub(crate) fn t60_at(signal: &[f64], frequency: f64, from: f64, to: f64) -> f64 {
    let window = 0.08;
    let mut points = Vec::new();
    let mut t = from;
    while t + window <= to {
        let level = amplitude_at(between(signal, t, t + window), frequency);
        points.push((t + window / 2.0, 20.0 * level.max(1e-300).log10()));
        t += 0.02;
    }
    let count = points.len() as f64;
    let mean_t = points.iter().map(|p| p.0).sum::<f64>() / count;
    let mean_db = points.iter().map(|p| p.1).sum::<f64>() / count;
    let covariance: f64 = points
        .iter()
        .map(|p| (p.0 - mean_t) * (p.1 - mean_db))
        .sum();
    let variance: f64 = points.iter().map(|p| (p.0 - mean_t).powi(2)).sum();
    -60.0 / (covariance / variance)
}

/// How long the whole of `signal` takes to fall from its loudest 20 ms to
/// 60 dB below, in seconds, judged by the RMS level of 20 ms windows; the
/// whole length if it never does.
pub(crate) fn time_to_fall(signal: &[f64], decibels: f64) -> f64 {
    let window = (0.02 * RATE) as usize;
    let levels: Vec<f64> = signal.chunks(window).map(rms).collect();
    let (loudest_at, loudest) =
        levels
            .iter()
            .enumerate()
            .fold((0, 0.0f64), |best, (index, &level)| {
                if level > best.1 { (index, level) } else { best }
            });
    let quiet = loudest * super::common::db(-decibels);
    let after = levels[loudest_at..]
        .iter()
        .position(|&level| level < quiet)
        .map_or(levels.len(), |index| index + loudest_at);
    (after - loudest_at) as f64 * 0.02
}

/// The root-mean-square level.
pub(crate) fn rms(signal: &[f64]) -> f64 {
    if signal.is_empty() {
        return 0.0;
    }
    (signal.iter().map(|x| x * x).sum::<f64>() / signal.len() as f64).sqrt()
}

/// A level in decibels.
pub(crate) fn decibels(level: f64) -> f64 {
    20.0 * level.max(1e-300).log10()
}

/// The largest sample, with anything that is not a finite number counted
/// as infinite.
pub(crate) fn peak(signal: &[f64]) -> f64 {
    crate::numeric::strict_peak(signal)
}

/// The spectral centroid, in hertz: the frequencies of a Hann-windowed
/// spectrum of the first `len` samples (a power of two), weighted by their
/// magnitudes.
pub(crate) fn centroid(signal: &[f64], len: usize) -> f64 {
    let mut input = vec![0.0; len];
    for (n, (slot, &x)) in input.iter_mut().zip(signal).enumerate() {
        *slot = x * (0.5 - 0.5 * (TAU * n as f64 / len as f64).cos());
    }
    let mut fft = RealFft::<f64>::new(len);
    let mut spectrum = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
    fft.forward(&input, &mut spectrum);
    let (mut weighted, mut total) = (0.0, 0.0);
    for (bin, value) in spectrum.iter().enumerate() {
        let magnitude = value.norm();
        weighted += magnitude * bin as f64 * RATE / len as f64;
        total += magnitude;
    }
    weighted / total.max(1e-300)
}

/// The largest difference between one sample and the next.
pub(crate) fn largest_jump(signal: &[f64]) -> f64 {
    crate::testing::largest_jump(signal)
}

/// Whether every sample is a finite number no larger than `bound`.
pub(crate) fn finite_within(signal: &[f64], bound: f64) -> bool {
    peak(signal) <= bound
}

/// The share of the energy of `signal` that is not within `tolerance` hertz
/// of a multiple of `fundamental`, in decibels: how much inharmonic content,
/// aliasing among it, a harmonic sound has.
pub(crate) fn inharmonic_db(signal: &[f64], fundamental: f64, tolerance: f64, len: usize) -> f64 {
    let mut input = vec![0.0; len];
    for (n, (slot, &x)) in input.iter_mut().zip(signal).enumerate() {
        // Blackman-Harris, so that the leakage of strong harmonics does not
        // count as inharmonic.
        let phase = TAU * n as f64 / len as f64;
        let w = 0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
            - 0.01168 * (3.0 * phase).cos();
        *slot = x * w;
    }
    let mut fft = RealFft::<f64>::new(len);
    let mut spectrum = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
    fft.forward(&input, &mut spectrum);
    let (mut harmonic, mut other) = (0.0, 0.0);
    for (bin, value) in spectrum.iter().enumerate().skip(1) {
        let frequency = bin as f64 * RATE / len as f64;
        let nearest = (frequency / fundamental).round() * fundamental;
        let power = value.norm_sqr();
        if (frequency - nearest).abs() <= tolerance && nearest > 0.0 {
            harmonic += power;
        } else {
            other += power;
        }
    }
    10.0 * (other / (harmonic + other).max(1e-300)).max(1e-30).log10()
}

/// `signal` through a Butterworth band-pass from `low` to `high` hertz
/// built on a prototype of order 4, run forwards and then backwards so
/// that it delays nothing (as `scipy.signal.sosfiltfilt` does): how the
/// recordings the acoustic drums are calibrated against were measured.
pub(crate) fn band_passed(signal: &[f64], low: f64, high: f64) -> Vec<f64> {
    use crate::iir::{Band, Design};
    let design = Design::butterworth(
        Band::Bandpass {
            low_hz: low,
            high_hz: high,
        },
        4,
    );
    let sections = design.sections(RATE);
    let mut out = signal.to_vec();
    for _ in 0..2 {
        for section in sections.as_slice() {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            for value in out.iter_mut() {
                let x = *value;
                let y = section.b0 * x + section.b1 * x1 + section.b2 * x2
                    - section.a1 * y1
                    - section.a2 * y2;
                (x2, x1, y2, y1) = (x1, x, y1, y);
                *value = y;
            }
        }
        out.reverse();
    }
    out
}

/// The RMS level in decibels of consecutive frames `seconds` long.
pub(crate) fn frame_levels(signal: &[f64], seconds: f64) -> Vec<f64> {
    let len = ((seconds * RATE) as usize).max(1);
    signal
        .chunks_exact(len)
        .map(|frame| decibels(rms(frame)))
        .collect()
}

/// How long `levels` (frames `seconds` apart, in decibels) take to fall
/// 60 dB, from the time they take to fall `drop` dB below their loudest
/// frame, as T20 is read for a room: `None` if they never fall that far.
pub(crate) fn decay_from_peak(levels: &[f64], seconds: f64, drop: f64) -> Option<f64> {
    let (loudest, peak) =
        levels
            .iter()
            .enumerate()
            .fold((0, f64::NEG_INFINITY), |best, (index, &level)| {
                if level > best.1 { (index, level) } else { best }
            });
    let below = levels[loudest..]
        .iter()
        .position(|&level| level < peak - drop)?;
    Some(below as f64 * seconds * 60.0 / drop)
}

/// The time of the loudest of `levels`, frames `seconds` apart.
pub(crate) fn loudest_at(levels: &[f64], seconds: f64) -> f64 {
    let index = levels
        .iter()
        .enumerate()
        .fold((0, f64::NEG_INFINITY), |best, (index, &level)| {
            if level > best.1 { (index, level) } else { best }
        })
        .0;
    index as f64 * seconds
}

/// The spectral centroid of each of `count` consecutive frames `seconds`
/// long, through a Hann window, weighted by magnitude.
pub(crate) fn centroid_track(signal: &[f64], seconds: f64, count: usize) -> Vec<f64> {
    let len = (seconds * RATE) as usize;
    let mut fft = RealFft::<f64>::new(len);
    let mut input = vec![0.0; len];
    let mut spectrum = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
    signal
        .chunks_exact(len)
        .take(count)
        .map(|frame| {
            for (n, (slot, &x)) in input.iter_mut().zip(frame).enumerate() {
                // numpy's hanning: symmetric, zero at both ends.
                *slot = x * (0.5 - 0.5 * (TAU * n as f64 / (len - 1) as f64).cos());
            }
            fft.forward(&input, &mut spectrum);
            let (mut weighted, mut total) = (0.0, 0.0);
            for (bin, value) in spectrum.iter().enumerate() {
                let magnitude = value.norm();
                weighted += magnitude * bin as f64 * RATE / len as f64;
                total += magnitude;
            }
            weighted / total.max(1e-300)
        })
        .collect()
}

/// The energy of `signal` between `low` and `high` hertz over its first
/// `seconds`, through [`band_passed`].
pub(crate) fn band_energy(signal: &[f64], low: f64, high: f64, seconds: f64) -> f64 {
    let filtered = band_passed(signal, low, high);
    let end = ((seconds * RATE) as usize).min(filtered.len());
    filtered[..end].iter().map(|x| x * x).sum()
}
