//! The poly synth, played and measured.

use core::f64::consts::TAU;

use manguebyte_core::AudioBuffer;
use manguebyte_midi::MidiMessage;

use super::super::common::key_hz;
use super::super::measure::{
    Act, RATE, amplitude_at, between, centroid, cents, decibels, finite_within, frequency_near,
    largest_jump, off, on, peak, play, rms, synth_of,
};
use super::super::modal::bessel_j;
use super::*;
use crate::curve::{FrequencyResponse, LadderResponse};
use crate::fft::{Complex, RealFft};
use crate::{LadderMode, LfoShape, NoiseColor, ProcessSpec, Processor, Synth, Voice};

/// A patch of one oscillator through no filter, with a quick, flat
/// envelope.
fn plain(osc: OscParams) -> PolySynthParams {
    PolySynthParams {
        oscillators: [osc, OscParams::OFF, OscParams::OFF],
        filters: [FilterParams::OFF, FilterParams::OFF],
        envelopes: [
            EnvParams {
                attack: 0.002,
                decay: 0.0,
                sustain: 1.0,
                release: 0.01,
                attack_curve: 0.0,
                decay_curve: 0.0,
                release_curve: 0.0,
            },
            EnvParams::AMP,
            EnvParams::AMP,
            EnvParams::AMP,
        ],
        velocity: 0.0,
        level: 1.0,
        ..PolySynthParams::INIT
    }
}

fn voice(params: PolySynthParams) -> [PolySynthVoice; 1] {
    [PolySynthVoice::new(params)]
}

/// One note held for `held` seconds of `seconds`, mono.
fn hold(params: PolySynthParams, key: u8, held: f64, seconds: f64) -> Vec<f64> {
    super::super::measure::note(voice(params), on(key, 1.0), held, seconds)
}

/// A synth set up for the mode of `params`, prepared for `channels`.
fn synth(
    params: &PolySynthParams,
    polyphony: usize,
    channels: usize,
) -> Synth<PolySynthVoice, f64> {
    let mut synth = PolySynthVoice::synth::<f64>(params, polyphony);
    synth.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: 512,
        num_channels: channels,
    });
    synth
}

/// Both channels of `seconds` of one note on a fresh voice, with
/// `before` done to the synth first.
fn stereo(
    params: PolySynthParams,
    key: u8,
    seconds: f64,
    before: impl FnOnce(&mut Synth<PolySynthVoice, f64>),
) -> (Vec<f64>, Vec<f64>) {
    let mut synth = synth(&params, 1, 2);
    before(&mut synth);
    synth.note_on(NoteRef::new(0, key), 1.0);
    let frames = (seconds * RATE) as usize;
    let mut buffer = AudioBuffer::<f64>::new(2, 256);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(256);
        buffer.set_num_frames(n);
        synth.process(&mut buffer);
        left.extend_from_slice(buffer.channel(0));
        right.extend_from_slice(buffer.channel(1));
        done += n;
    }
    (left, right)
}

use crate::event::NoteRef;

/// The mod wheel all the way up, before a note.
fn wheel_up(synth: &mut Synth<PolySynthVoice, f64>) {
    synth.voices_mut().for_each(|voice| {
        Voice::<f64>::midi(
            voice,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 1,
                value: 127,
            },
        );
    });
}

/// The power spectrum of `signal`, averaged over Hann windows of `len`
/// (a power of two) overlapping by half: bin `k` is `k·RATE/len` hertz.
fn power_spectrum(signal: &[f64], len: usize) -> Vec<f64> {
    let mut fft = RealFft::<f64>::new(len);
    let mut spectrum = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
    let mut power = vec![0.0; fft.spectrum_len()];
    let mut input = vec![0.0; len];
    let mut start = 0;
    while start + len <= signal.len() {
        for (n, slot) in input.iter_mut().enumerate() {
            let w = 0.5 - 0.5 * (TAU * n as f64 / len as f64).cos();
            *slot = signal[start + n] * w;
        }
        fft.forward(&input, &mut spectrum);
        for (total, bin) in power.iter_mut().zip(&spectrum) {
            *total += bin.norm_sqr();
        }
        start += len / 2;
    }
    power
}

/// The share of the energy of `signal` below `limit` hertz that is not
/// within `tolerance` hertz of a multiple of `fundamental`, in decibels:
/// the aliasing in the audible band. Kaiser-windowed with β = 18, whose
/// leakage is more than 150 dB down beyond 20 Hz of a component (the
/// window's main lobe, at this length): a pure sine measures below
/// -140 dB.
fn aliasing_db(signal: &[f64], fundamental: f64, tolerance: f64, limit: f64) -> f64 {
    use crate::window::{Symmetry, Window};
    let len = 16_384;
    let mut input = vec![0.0; len];
    for (n, (slot, &x)) in input.iter_mut().zip(signal).enumerate() {
        *slot = x * Window::Kaiser(18.0).value(n, len, Symmetry::Periodic);
    }
    let mut fft = RealFft::<f64>::new(len);
    let mut spectrum = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
    fft.forward(&input, &mut spectrum);
    let (mut harmonic, mut other) = (0.0, 0.0);
    for (bin, value) in spectrum.iter().enumerate().skip(1) {
        let frequency = bin as f64 * RATE / len as f64;
        if frequency > limit {
            break;
        }
        let nearest = (frequency / fundamental).round() * fundamental;
        if (frequency - nearest).abs() <= tolerance && nearest > 0.0 {
            harmonic += value.norm_sqr();
        } else {
            other += value.norm_sqr();
        }
    }
    10.0 * (other / (harmonic + other).max(1e-300)).max(1e-30).log10()
}

/// The correlation coefficient of two signals.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    ab / (aa * bb).sqrt().max(1e-300)
}

#[test]
fn every_note_is_in_tune_across_the_keyboard() {
    let mut worst = 0.0f64;
    let shapes = [
        OscParams::new(Wave::Sine, 1.0),
        OscParams::new(Wave::Saw, 1.0),
        OscParams::new(Wave::Pulse, 1.0),
        OscParams::new(Wave::Triangle, 1.0),
        OscParams::table(TableShape::Basic, 0.7, 1.0),
    ];
    for osc in shapes {
        for key in (21u8..=108).step_by(11) {
            let signal = hold(plain(osc), key, 0.6, 0.6);
            let expected = key_hz(f64::from(key));
            let measured = frequency_near(between(&signal, 0.1, 0.6), expected);
            let off = cents(measured, expected).abs();
            worst = worst.max(off);
            assert!(off < 0.1, "{:?} key {key}: {off} cents", osc.wave);
        }
    }
    // Transposed, fine-tuned and an octave down on the sub oscillator.
    let transposed = plain(OscParams::new(Wave::Sine, 1.0).tuned(7.0, 33.0));
    let signal = hold(transposed, 57, 0.6, 0.6);
    let expected = key_hz(57.0 + 7.33);
    let off = cents(
        frequency_near(between(&signal, 0.1, 0.6), expected),
        expected,
    );
    assert!(off.abs() < 0.1, "transposed: {off}");
    let sub = PolySynthParams {
        sub: SubParams {
            wave: SubWave::Sine,
            octaves: 2,
            level: 1.0,
        },
        ..plain(OscParams::OFF)
    };
    let signal = hold(sub, 69, 0.6, 0.6);
    let off = cents(frequency_near(between(&signal, 0.1, 0.6), 110.0), 110.0);
    assert!(off.abs() < 0.1, "sub: {off}");
    println!("worst {worst:.5} cents");
}

#[test]
fn high_notes_are_kept_free_of_aliasing() {
    let shapes = [
        ("saw", OscParams::new(Wave::Saw, 1.0), -79.0),
        ("square", OscParams::new(Wave::Pulse, 1.0), -79.0),
        (
            "narrow pulse",
            OscParams {
                shape: 0.8,
                ..OscParams::new(Wave::Pulse, 1.0)
            },
            -75.0,
        ),
        ("triangle", OscParams::new(Wave::Triangle, 1.0), -105.0),
        ("sine", OscParams::new(Wave::Sine, 1.0), -130.0),
        (
            "table saw",
            OscParams::table(TableShape::Basic, 2.0 / 3.0, 1.0),
            -89.0,
        ),
        (
            "table digital",
            OscParams::table(TableShape::Digital, 1.0, 1.0),
            -92.0,
        ),
        (
            "table spectral",
            OscParams::table(TableShape::Spectral, 0.5, 1.0),
            -84.0,
        ),
    ];
    let mut failures = Vec::new();
    for (name, osc, bound) in shapes {
        let mut levels = Vec::new();
        for key in [72u8, 84, 96, 103] {
            let signal = hold(plain(osc), key, 0.5, 0.5);
            let f0 = key_hz(f64::from(key));
            let other = aliasing_db(between(&signal, 0.1, 0.5), f0, 25.0, 20_000.0);
            levels.push(format!("{other:.1}"));
            if other >= bound {
                failures.push(format!("{name} key {key}: {other:.1} dB"));
            }
        }
        println!(
            "{name}: {} dB of aliasing below 20 kHz at keys 72, 84, 96, 103",
            levels.join(", ")
        );
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn hard_sync_is_heard_at_the_master_pitch() {
    let master = OscParams::new(Wave::Saw, 0.0);
    let slave = |semitones: f64, sync: bool| OscParams {
        sync,
        ..OscParams::new(Wave::Saw, 1.0).tuned(semitones, 0.0)
    };
    let patch = |semitones: f64, sync: bool| PolySynthParams {
        oscillators: [master, slave(semitones, sync), OscParams::OFF],
        ..plain(OscParams::OFF)
    };
    let f0 = key_hz(48.0);
    let mut brightness = Vec::new();
    let mut worst = f64::NEG_INFINITY;
    for semitones in [3.7, 7.3, 15.1, 26.4] {
        // From 0.2 s, when the DC blocker has taken away the synced saw's
        // offset.
        let synced = hold(patch(semitones, true), 48, 0.6, 0.6);
        let window = between(&synced, 0.2, 0.6);
        // Periodic at the master's pitch: nothing between its harmonics.
        let other = aliasing_db(window, f0, 20.0, 20_000.0);
        worst = worst.max(other);
        assert!(other < -70.0, "{semitones}: {other:.1} dB");
        let pitch = cents(frequency_near(window, f0), f0);
        assert!(pitch.abs() < 0.1, "{semitones}: {pitch} cents");
        brightness.push(centroid(window, 8_192));
        // Without sync the slave is heard at its own pitch.
        let free = hold(patch(semitones, false), 48, 0.5, 0.5);
        let other = aliasing_db(between(&free, 0.1, 0.5), f0, 20.0, 20_000.0);
        assert!(other > -20.0, "free {semitones}: {other:.1} dB");
    }
    // The slave's pitch is the colour: higher is brighter.
    assert!(brightness.windows(2).all(|w| w[1] > w[0]), "{brightness:?}");
    println!("synced: at most {worst:.1} dB between the master's harmonics");
}

#[test]
fn phase_modulation_makes_the_bessel_sidebands() {
    let carrier = 440.0;
    let modulator = 110.0;
    for index in [0.5, 1.0, 1.8] {
        let params = PolySynthParams {
            oscillators: [
                OscParams::new(Wave::Sine, 0.0).tuned(-24.0, 0.0),
                OscParams {
                    fm: index,
                    mod_source: OscSource::Osc1,
                    ..OscParams::new(Wave::Sine, 1.0)
                },
                OscParams::OFF,
            ],
            ..plain(OscParams::OFF)
        };
        let signal = hold(params, 69, 0.6, 0.6);
        let window = between(&signal, 0.1, 0.6);
        let centre = amplitude_at(window, carrier);
        let j0 = bessel_j(0, index);
        for n in 1..=2u32 {
            let expected = bessel_j(n, index) / j0;
            for side in [-1.0, 1.0] {
                let measured =
                    amplitude_at(window, carrier + side * f64::from(n) * modulator) / centre;
                let error = decibels(measured / expected.abs());
                assert!(
                    error.abs() < 0.1,
                    "index {index}, n {n}, side {side}: {error:.3} dB"
                );
            }
        }
    }
}

#[test]
fn ring_modulation_leaves_only_the_sum_and_difference() {
    let params = PolySynthParams {
        oscillators: [
            OscParams::new(Wave::Sine, 0.0),
            OscParams {
                ring: 1.0,
                mod_source: OscSource::Osc1,
                ..OscParams::new(Wave::Sine, 1.0).tuned(7.0, 0.0)
            },
            OscParams::OFF,
        ],
        ..plain(OscParams::OFF)
    };
    let signal = hold(params, 69, 0.6, 0.6);
    let window = between(&signal, 0.1, 0.6);
    let (a, b) = (440.0, key_hz(76.0));
    let sum = amplitude_at(window, a + b);
    let difference = amplitude_at(window, b - a);
    // A product of two sines: two halves.
    assert!((decibels(sum / difference)).abs() < 0.1);
    assert!((sum - 0.5).abs() < 0.02, "{sum}");
    for carrier in [a, b] {
        let left = decibels(amplitude_at(window, carrier) / sum);
        assert!(left < -80.0, "{carrier}: {left:.1} dB");
    }
}

#[test]
fn unison_voices_are_detuned_spread_and_unrelated() {
    let stack = |voices: u8, detune: f64, spread: f64| {
        plain(OscParams::new(Wave::Saw, 1.0).unison(voices, detune, spread))
    };
    // The level stays where it is whatever the number of voices.
    let level = |voices: u8| {
        let signal = hold(stack(voices, 0.5, 0.0), 45, 1.0, 1.0);
        rms(between(&signal, 0.2, 1.0))
    };
    let one = level(1);
    for voices in [2, 4, 7, 16] {
        let change = decibels(level(voices) / one);
        assert!(change.abs() < 1.5, "{voices} voices: {change:.2} dB");
    }
    // The spectrum is dense: around the sixteenth harmonic the energy is
    // spread over the detune instead of sitting on the harmonic.
    let f16 = 16.0 * key_hz(45.0);
    let share_on_harmonic = |voices: u8| {
        let signal = hold(stack(voices, 0.8, 0.0), 45, 1.5, 1.5);
        let power = power_spectrum(between(&signal, 0.2, 1.5), 32_768);
        let bin = |f: f64| (f * 32_768.0 / RATE).round() as usize;
        let near: f64 = power[bin(f16) - 3..=bin(f16) + 3].iter().sum();
        let around: f64 = power[bin(f16 * 0.98)..=bin(f16 * 1.02)].iter().sum();
        near / around
    };
    let single = share_on_harmonic(1);
    let sixteen = share_on_harmonic(16);
    println!("energy on the 16th harmonic: {single:.3} for one voice, {sixteen:.3} for sixteen");
    assert!(single > 0.95 && sixteen < 0.35, "{single} {sixteen}");
    // Two voices spread all the way are one per channel: what the
    // channels share is what the voices share, which is nothing.
    let channels = |voices: u8, detune: f64, spread: f64| {
        let (left, right) = stereo(stack(voices, detune, spread), 45, 1.5, |_| {});
        correlation(between(&left, 0.2, 1.5), between(&right, 0.2, 1.5))
    };
    let pair = channels(2, 1.0, 1.0);
    // Sixteen voices spread all the way: as correlated as unrelated
    // voices at their places in the field are, Σ L·R / √(Σ L² Σ R²); not
    // spread, the channels are the same.
    let (mut lr, mut ll, mut rr) = (0.0, 0.0, 0.0);
    for index in 0..16 {
        let (l, r) = super::osc::pan_gains(super::osc::pan_share(index, 16));
        (lr, ll, rr) = (lr + l * r, ll + l * l, rr + r * r);
    }
    let unrelated = lr / (ll * rr).sqrt();
    let (wide, narrow) = (channels(16, 0.8, 1.0), channels(16, 0.8, 0.0));
    println!(
        "correlation of the channels: {pair:.3} for two voices, {wide:.3} for sixteen \
         ({unrelated:.3} if unrelated), {narrow:.6} not spread"
    );
    assert!(pair.abs() < 0.05, "{pair}");
    assert!((wide - unrelated).abs() < 0.1, "{wide} against {unrelated}");
    assert!(narrow > 0.999_999);
    // Random phases: no voice lines up with another at the start, so the
    // attack has no spike the rest of the note lacks.
    let signal = hold(stack(16, 0.5, 0.0), 45, 1.0, 1.0);
    let crest = |part: &[f64]| peak(part) / rms(part);
    let start = crest(between(&signal, 0.003, 0.05));
    let later = crest(between(&signal, 0.5, 0.9));
    assert!(start < 1.3 * later, "{start} at the start, {later} later");
    // The phases are drawn for the note: the same note sounds the same,
    // another velocity draws again.
    let again = hold(stack(16, 0.5, 0.0), 45, 1.0, 1.0);
    assert!(again == signal);
    let softer = super::super::measure::note(voice(stack(16, 0.5, 0.0)), on(45, 0.9), 1.0, 1.0);
    assert!(softer != signal);
}

#[test]
fn the_cutoff_follows_the_key_the_envelope_and_the_matrix_as_the_curve_says() {
    // A sine of 1 kHz into a ladder at 1 kHz, at key 60 and an octave up
    // (the oscillator transposed down to stay at 1 kHz).
    let frequency = 1_000.0;
    let semitones = 12.0 * (frequency / key_hz(60.0)).log2();
    let patch = |key_offset: f64, filter: FilterParams| PolySynthParams {
        filters: [filter, FilterParams::OFF],
        envelopes: [
            plain(OscParams::OFF).envelopes[0],
            EnvParams::adsr(0.0, 0.0, 1.0, 0.1),
            EnvParams::AMP,
            EnvParams::AMP,
        ],
        // Quiet, so that the ladder's saturation stays out of the way.
        ..plain(OscParams::new(Wave::Sine, 0.01).tuned(semitones - key_offset, 0.0))
    };
    let base = FilterParams::new(FilterModel::LadderLp24, frequency, 0.3);
    let level = |params: PolySynthParams, key: u8, before: fn(&mut Synth<PolySynthVoice, f64>)| {
        let (left, _) = stereo(params, key, 0.5, before);
        amplitude_at(between(&left, 0.15, 0.5), frequency)
    };
    let reference = level(patch(0.0, base), 60, |_| {});
    let curve = |cutoff: f64| {
        let response = LadderResponse::new(LadderMode::Lp24, cutoff, 0.3, 0.0, 2.0 * RATE);
        // The voice gives back half the bass the resonance takes.
        response.response_at(frequency).norm() * (1.0 + 0.5 * 4.0 * 0.3)
    };
    let expected = |cutoff: f64| decibels(curve(cutoff) / curve(frequency));
    let tracked = level(patch(12.0, base.track(1.0)), 72, |_| {});
    let enveloped = level(patch(0.0, base.env(1.0)), 60, |_| {});
    let mut wheeled = patch(0.0, base);
    wheeled.matrix = matrix(&[ModSlot::new(
        ModSource::ModWheel,
        ModDestination::Cutoff,
        1.0,
    )]);
    let wheeled = level(wheeled, 60, wheel_up);
    for (what, measured) in [
        ("key", tracked),
        ("envelope", enveloped),
        ("wheel", wheeled),
    ] {
        let rise = decibels(measured / reference);
        let wanted = expected(2.0 * frequency);
        println!("{what}: {rise:.3} dB, the curve says {wanted:.3} dB");
        assert!(
            (rise - wanted).abs() < 0.05,
            "{what}: {rise} against {wanted}"
        );
    }
    // And half tracking moves it half as far: by half an octave.
    let half = level(patch(12.0, base.track(0.5)), 72, |_| {});
    let rise = decibels(half / reference);
    let wanted = expected(2f64.sqrt() * frequency);
    assert!(
        (rise - wanted).abs() < 0.05,
        "half: {rise} against {wanted}"
    );
}

#[test]
fn the_amplifier_follows_its_envelope_on_time() {
    let mut params = plain(OscParams::new(Wave::Sine, 1.0));
    params.envelopes[0] = EnvParams {
        attack: 0.2,
        decay: 0.2,
        sustain: 0.5,
        release: 0.3,
        attack_curve: 0.0,
        decay_curve: 0.0,
        release_curve: 0.0,
    };
    let f = key_hz(69.0);
    let signal = hold(params, 69, 1.0, 1.4);
    // The level over 4 ms windows centred on `t`.
    let at = |t: f64| amplitude_at(between(&signal, t - 0.002, t + 0.002), f);
    let full = at(0.2);
    for (t, wanted) in [
        (0.05, 0.25),
        (0.1, 0.5),
        (0.15, 0.75),
        (0.3, 0.75),
        (0.4, 0.5),
        (0.7, 0.5),
        (1.15, 0.25),
    ] {
        let level = at(t) / full;
        assert!(
            (level - wanted).abs() < 0.01,
            "{t} s: {level} against {wanted}"
        );
    }
    assert!(peak(between(&signal, 1.31, 1.4)) < 1e-6);
}

#[test]
fn lfos_keep_their_rate_lock_to_the_tempo_and_fade_in() {
    let tremolo = |lfo: LfoParams| PolySynthParams {
        lfos: [lfo, LfoParams::DEFAULT, LfoParams::DEFAULT],
        matrix: matrix(&[ModSlot::new(ModSource::Lfo1, ModDestination::Volume, -6.0)]),
        ..plain(OscParams::new(Wave::Sine, 1.0))
    };
    let f = key_hz(81.0);
    // The level every 2.5 ms, and the times it peaks.
    let peaks = |signal: &[f64]| {
        let levels: Vec<f64> = signal
            .chunks(120)
            .map(|chunk| amplitude_at(chunk, f))
            .collect();
        let mut times = Vec::new();
        for i in 1..levels.len() - 1 {
            if levels[i] > levels[i - 1] && levels[i] >= levels[i + 1] && levels[i] > 0.0 {
                times.push(i as f64 * 0.0025);
            }
        }
        (levels, times)
    };
    // 5 Hz: a level at its lowest every 0.2 s (the LFO starts at the top
    // of the volume's swing going down... a sine from 0 going up lowers it).
    let signal = hold(
        tremolo(LfoParams::new(LfoShape::Triangle, 5.0)),
        81,
        1.0,
        1.0,
    );
    let (_, times) = peaks(&signal);
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.len() >= 3, "{times:?}");
    for gap in &gaps {
        assert!((gap - 0.2).abs() < 0.006, "{gaps:?}");
    }
    // Locked to the tempo: a beat at 150 bpm is 0.4 s.
    let mut synced = tremolo(LfoParams::synced(LfoShape::Triangle, 1.0));
    synced.lfos[0].retrigger = true;
    let mut synth = synth(&synced, 1, 1);
    synth.voices_mut().for_each(|voice| voice.set_tempo(150.0));
    let signal = play(&mut synth, &[(0, on(81, 1.0))], (1.4 * RATE) as usize, 256);
    let (_, times) = peaks(&signal);
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        !gaps.is_empty() && gaps.iter().all(|gap| (gap - 0.4).abs() < 0.006),
        "{gaps:?}"
    );
    // Faded in over half a second: shallow at first, full after.
    let faded = tremolo(LfoParams {
        fade_in: 0.5,
        ..LfoParams::new(LfoShape::Sine, 10.0)
    });
    let signal = hold(faded, 81, 1.0, 1.0);
    let (levels, _) = peaks(&signal);
    let swing = |from: usize, to: usize| {
        let part = &levels[from..to];
        let high = part.iter().fold(0.0f64, |m, &x| m.max(x));
        let low = part.iter().fold(f64::INFINITY, |m, &x| m.min(x));
        decibels(high / low)
    };
    let early = swing(4, 44);
    let late = swing(240, 400);
    assert!(early < 3.0 && (late - 12.0).abs() < 0.5, "{early} {late}");
}

#[test]
fn the_matrix_takes_every_source_to_its_destination() {
    let sine = OscParams::new(Wave::Sine, 1.0);
    let wheel = |destination: ModDestination, amount: f64, mut params: PolySynthParams| {
        params.matrix = matrix(&[ModSlot::new(ModSource::ModWheel, destination, amount)]);
        let rest = stereo(params, 57, 0.5, |_| {});
        let moved = stereo(params, 57, 0.5, wheel_up);
        (rest, moved)
    };
    let f = key_hz(57.0);
    let window = |signal: &(Vec<f64>, Vec<f64>)| between(&signal.0, 0.1, 0.5).to_vec();

    // Pitch: up an octave.
    let (_, moved) = wheel(ModDestination::Pitch, 12.0, plain(sine));
    let pitch = cents(frequency_near(&window(&moved), 2.0 * f), 2.0 * f);
    assert!(pitch.abs() < 0.1, "pitch: {pitch}");
    // One oscillator's pitch: oscillator 2 a fifth up, oscillator 1 still.
    let two = PolySynthParams {
        oscillators: [sine, OscParams::new(Wave::Sine, 1.0), OscParams::OFF],
        ..plain(sine)
    };
    let (_, moved) = wheel(ModDestination::Osc2Pitch, 7.0, two);
    let w = window(&moved);
    assert!(amplitude_at(&w, f) > 0.4 && amplitude_at(&w, key_hz(64.0)) > 0.4);
    // Levels: oscillator 1 out, the sub and the noise in.
    let (rest, moved) = wheel(ModDestination::Osc1Level, -1.0, plain(sine));
    assert!(rms(&window(&rest)) > 0.5 && peak(&window(&moved)) < 1e-6);
    let quiet = PolySynthParams {
        sub: SubParams {
            level: 0.0,
            ..SubParams::OFF
        },
        ..plain(OscParams::OFF)
    };
    let mut silent = quiet;
    silent.oscillators[0] = OscParams { level: 0.0, ..sine };
    silent.envelopes[0].sustain = 1.0;
    let (rest, moved) = wheel(ModDestination::SubLevel, 1.0, silent);
    let note_level = |signal: &(Vec<f64>, Vec<f64>)| rms(&window(signal));
    assert!(note_level(&rest) < 1e-9, "{}", note_level(&rest));
    assert!(amplitude_at(&window(&moved), f / 2.0) > 0.9);
    let (_, moved) = wheel(ModDestination::NoiseLevel, 1.0, silent);
    assert!(note_level(&moved) > 0.1);
    // Shape: the pulse narrows, and even harmonics appear.
    let pulse = plain(OscParams::new(Wave::Pulse, 1.0));
    let (rest, moved) = wheel(ModDestination::Osc1Shape, 0.5, pulse);
    let second = |signal: &(Vec<f64>, Vec<f64>)| {
        let w = window(signal);
        amplitude_at(&w, 2.0 * f) / amplitude_at(&w, f)
    };
    assert!(
        second(&rest) < 1e-3 && second(&moved) > 0.3,
        "{} {}",
        second(&rest),
        second(&moved)
    );
    // Shape on a table: from its sine frame to its square, so the third
    // harmonic appears.
    let table = plain(OscParams::table(TableShape::Basic, 0.0, 1.0));
    let (rest, moved) = wheel(ModDestination::Osc1Shape, 1.0, table);
    let third = |signal: &(Vec<f64>, Vec<f64>)| {
        let w = window(signal);
        amplitude_at(&w, 3.0 * f) / amplitude_at(&w, f)
    };
    assert!(third(&rest) < 1e-3 && (third(&moved) - 1.0 / 3.0).abs() < 0.02);
    // FM: sidebands appear.
    let fm = PolySynthParams {
        oscillators: [
            OscParams::new(Wave::Sine, 0.0),
            OscParams {
                mod_source: OscSource::Osc1,
                ..sine
            },
            OscParams::OFF,
        ],
        ..plain(sine)
    };
    let (rest, moved) = wheel(ModDestination::Osc2Fm, 1.0, fm);
    let sideband = |signal: &(Vec<f64>, Vec<f64>)| amplitude_at(&window(signal), 2.0 * f);
    assert!(sideband(&rest) < 1e-4 && sideband(&moved) > 0.1);
    // Detune: a unison stack spreads.
    let stack = plain(OscParams::new(Wave::Saw, 1.0).unison(8, 0.0, 0.0));
    let (rest, moved) = wheel(ModDestination::Detune, 0.6, stack);
    let on_harmonic = |signal: &(Vec<f64>, Vec<f64>)| {
        let w = window(signal);
        amplitude_at(&w, 6.0 * f) / amplitude_at(&w, f).max(1e-12)
    };
    assert!(on_harmonic(&moved) < 0.5 * on_harmonic(&rest));
    // Cutoff, resonance, morph and drive change the colour.
    let filtered = |model: FilterModel| PolySynthParams {
        filters: [FilterParams::new(model, 400.0, 0.2), FilterParams::OFF],
        ..plain(OscParams::new(Wave::Saw, 1.0))
    };
    let bright = |signal: &(Vec<f64>, Vec<f64>)| centroid(&window(signal), 8_192);
    for destination in [ModDestination::Cutoff, ModDestination::Cutoff1] {
        let (rest, moved) = wheel(destination, 2.0, filtered(FilterModel::LadderLp24));
        assert!(bright(&moved) > 1.5 * bright(&rest), "{destination:?}");
    }
    let mut second_filter = filtered(FilterModel::Off);
    second_filter.filters[1] = FilterParams::new(FilterModel::SvfLp, 400.0, 0.2);
    let (rest, moved) = wheel(ModDestination::Cutoff2, 2.0, second_filter);
    assert!(bright(&moved) > 1.5 * bright(&rest));
    let (rest, moved) = wheel(ModDestination::Resonance, 0.8, filtered(FilterModel::SvfLp));
    let at_cutoff = |signal: &(Vec<f64>, Vec<f64>)| {
        let w = window(signal);
        // The harmonic nearest 400 Hz.
        let harmonic = (400.0 / f).round() * f;
        amplitude_at(&w, harmonic) / amplitude_at(&w, f)
    };
    assert!(at_cutoff(&moved) > 3.0 * at_cutoff(&rest));
    let (rest, moved) = wheel(ModDestination::Morph, 1.0, filtered(FilterModel::SemNotch));
    assert!(bright(&moved) > 2.0 * bright(&rest));
    let (rest, moved) = wheel(ModDestination::Drive, 24.0, filtered(FilterModel::Ms20Lp));
    assert!(bright(&moved) > 1.1 * bright(&rest));
    // Volume in decibels, pan across.
    let (rest, moved) = wheel(ModDestination::Volume, -6.0, plain(sine));
    let change = decibels(rms(&window(&moved)) / rms(&window(&rest)));
    assert!((change + 6.0).abs() < 0.01, "{change}");
    let (rest, moved) = wheel(ModDestination::Pan, 1.0, plain(sine));
    let balance = |signal: &(Vec<f64>, Vec<f64>)| {
        rms(between(&signal.0, 0.1, 0.5)) / rms(between(&signal.1, 0.1, 0.5)).max(1e-12)
    };
    assert!((balance(&rest) - 1.0).abs() < 1e-9);
    assert!(rms(between(&moved.0, 0.1, 0.5)) < 1e-6 && rms(between(&moved.1, 0.1, 0.5)) > 0.99);
    // An LFO's rate: the tremolo of LFO 1 runs twice as fast.
    let mut tremolo = plain(sine);
    tremolo.lfos[0] = LfoParams::new(LfoShape::Sine, 2.0);
    tremolo.matrix = matrix(&[
        ModSlot::new(ModSource::Lfo1, ModDestination::Volume, -6.0),
        ModSlot::new(ModSource::ModWheel, ModDestination::Lfo1Rate, 1.0),
    ]);
    // Cycles of the level, measured every 20 ms, over two seconds.
    let swings = |before: fn(&mut Synth<PolySynthVoice, f64>)| {
        let (left, _) = stereo(tremolo, 57, 2.0, before);
        let levels: Vec<f64> = left.chunks(960).map(|c| amplitude_at(c, f)).collect();
        let mean = levels.iter().sum::<f64>() / levels.len() as f64;
        levels
            .windows(2)
            .filter(|w| w[0] < mean && w[1] >= mean)
            .count() as i64
    };
    let (slow, fast) = (swings(|_| {}), swings(wheel_up));
    assert!((fast - 2 * slow).abs() <= 1 && slow >= 3, "{slow} {fast}");
    // Via: velocity scales what the wheel does.
    let mut via = plain(sine);
    via.matrix = matrix(&[
        ModSlot::new(ModSource::ModWheel, ModDestination::Pitch, 12.0).via(ModSource::Velocity),
    ]);
    let mut synth = synth(&via, 1, 1);
    wheel_up(&mut synth);
    let signal = play(&mut synth, &[(0, on(57, 0.5))], 24_000, 256);
    let half = key_hz(63.0);
    let measured = cents(frequency_near(between(&signal, 0.1, 0.5), half), half);
    assert!(measured.abs() < 0.1, "via: {measured}");
}

#[test]
fn glides_take_their_time_and_the_modes_play_as_they_say() {
    let glide = 0.2;
    let mut params = plain(OscParams::new(Wave::Sine, 1.0));
    params.glide = glide;
    params.envelopes[0] = EnvParams::adsr(0.002, 0.1, 0.4, 0.05);
    let at = (0.3 * RATE) as usize;
    let script = [(0, on(48, 1.0)), (at, on(60, 1.0)), (at + 100, off(48))];
    for mode in [VoiceMode::Mono, VoiceMode::Legato] {
        params.mode = mode;
        let mut synth = synth(&params, 4, 1);
        let signal = play(&mut synth, &script, 3 * at, 128);
        // Halfway there halfway through, in semitones.
        let middle = (at as f64 / RATE) + glide / 2.0;
        let measured = frequency_near(
            between(&signal, middle - 0.005, middle + 0.005),
            key_hz(54.0),
        );
        let semitones = 12.0 * (measured / key_hz(48.0)).log2();
        assert!((semitones - 6.0).abs() < 0.3, "{mode:?}: {semitones}");
        // Arrived when the glide time is up.
        let arrived = (at as f64 / RATE) + glide + 0.01;
        let end = frequency_near(between(&signal, arrived, 0.9), key_hz(60.0));
        assert!(cents(end, key_hz(60.0)).abs() < 0.1, "{mode:?}");
        // Mono strikes the envelope again (the level rises from the
        // sustain); legato does not.
        let before = rms(between(&signal, 0.25, 0.3));
        let after = rms(between(&signal, 0.302, 0.312));
        let rise = decibels(after / before);
        match mode {
            VoiceMode::Mono => assert!(rise > 4.0, "{rise}"),
            _ => assert!(rise.abs() < 0.5, "{rise}"),
        }
    }
    // Poly: a chord sounds on three voices.
    params.mode = VoiceMode::Poly;
    let mut synth = synth(&params, 4, 1);
    for key in [60, 64, 67] {
        synth.note_on(NoteRef::new(0, key), 1.0);
    }
    let mut block = AudioBuffer::<f64>::new(1, 256);
    synth.process(&mut block);
    assert_eq!(synth.active_voices(), 3);
}

#[test]
fn notes_start_end_and_are_taken_over_without_a_click() {
    let mut params = plain(OscParams::new(Wave::Sine, 1.0));
    params.envelopes[0] = EnvParams::adsr(0.0, 0.0, 1.0, 0.0);
    let alone = hold(params, 52, 0.3, 0.5);
    // The fastest envelope the voice allows rises no faster than the sine
    // itself moves.
    let steady = largest_jump(between(&alone, 0.1, 0.3));
    let attack = largest_jump(between(&alone, 0.0, 0.01));
    let release = largest_jump(between(&alone, 0.3, 0.5));
    assert!(
        attack <= 1.05 * steady && release <= 1.05 * steady,
        "{attack} {release} {steady}"
    );
    // Taken over: the old note fades out under the new one.
    let steal = (0.25 * RATE) as usize;
    let mut synth = synth_of(voice(params));
    let both = play(
        &mut synth,
        &[(0, on(52, 1.0)), (steal, on(40, 1.0))],
        2 * steal,
        256,
    );
    let mut fresh = synth_of(voice(params));
    let second = play(&mut fresh, &[(steal, on(40, 1.0))], 2 * steal, 256);
    let residual: Vec<f64> = both.iter().zip(&second).map(|(a, b)| a - b).collect();
    assert!(largest_jump(&residual[steal - 64..steal + 512]) <= 1.01 * steady);
    assert!(peak(&residual[steal + 400..]) < 1e-12);
    // Settings that jump while a note sounds do not jump in the sound.
    let mut synth = synth_of(voice(PolySynthParams {
        filters: [
            FilterParams::new(FilterModel::SvfLp, 300.0, 0.2),
            FilterParams::OFF,
        ],
        ..plain(OscParams::new(Wave::Triangle, 1.0))
    }));
    let mut jumped = PolySynthParams {
        filters: [
            FilterParams::new(FilterModel::Ms20Lp, 4_000.0, 0.6),
            FilterParams::OFF,
        ],
        ..plain(OscParams::new(Wave::Saw, 1.0).unison(5, 0.3, 0.0))
    };
    jumped.level = 0.6;
    jumped.oscillators[0].semitones = 0.0;
    let mut out = Vec::new();
    let mut block = AudioBuffer::<f64>::new(1, 256);
    synth.note_on(NoteRef::new(0, 45), 1.0);
    for round in 0..40 {
        if round == 20 {
            synth
                .voices_mut()
                .for_each(|voice| voice.set_params(jumped));
        }
        synth.process(&mut block);
        out.extend_from_slice(block.channel(0));
    }
    let change = 20 * 256;
    let around = largest_jump(&out[change - 32..change + 480]);
    let settled = largest_jump(&out[change + 2_400..]);
    let before = largest_jump(&out[2_400..change]);
    assert!(
        around <= 1.05 * settled.max(before),
        "{around} against {settled}, {before}"
    );
}

#[test]
fn the_output_does_not_depend_on_the_block_size() {
    let script = [
        (0, on(40, 0.9)),
        (
            1_000,
            Act::On {
                key: 47,
                velocity: 0.7,
                accent: true,
            },
        ),
        (7_000, on(52, 0.5)),
        (9_001, off(40)),
        (9_500, Act::Bend(0.7)),
        (12_000, on(40, 1.0)),
        (20_000, off(47)),
    ];
    let mut busy = PolySynthParams::INIT;
    busy.oscillators = [
        OscParams::new(Wave::Saw, 0.7).unison(3, 0.4, 0.8),
        OscParams {
            sync: true,
            ..OscParams::new(Wave::Pulse, 0.5).tuned(7.0, 0.0)
        },
        OscParams {
            fm: 1.0,
            ..OscParams::table(TableShape::Vowel, 0.3, 0.4)
        },
    ];
    busy.noise = NoiseParams {
        color: NoiseColor::Pink,
        level: 0.1,
    };
    busy.filters = [
        FilterParams::new(FilterModel::Ms20Lp, 900.0, 0.5).env(2.0),
        FilterParams::new(FilterModel::CombPlus, 300.0, 0.4).track(1.0),
    ];
    busy.lfos[0] = LfoParams::new(LfoShape::SampleAndHold, 7.0);
    busy.matrix = matrix(&[
        ModSlot::new(ModSource::Lfo1, ModDestination::Cutoff, 1.0),
        ModSlot::new(ModSource::Env3, ModDestination::Osc3Shape, 0.5),
        ModSlot::new(ModSource::Random, ModDestination::Pan, 0.5),
    ]);
    for params in [
        busy,
        PolySynthParams {
            mode: VoiceMode::Legato,
            glide: 0.05,
            ..busy
        },
    ] {
        let run = |block| {
            let mut synth = synth_of([PolySynthVoice::new(params), PolySynthVoice::new(params)]);
            synth.set_legato(params.mode == VoiceMode::Legato);
            play(&mut synth, &script, 24_000, block)
        };
        let reference = run(256);
        assert!(rms(&reference) > 0.01);
        for block in [1, 7, 64, 1_000] {
            assert!(
                run(block) == reference,
                "{:?}, blocks of {block}",
                params.mode
            );
        }
    }
}

#[test]
fn noise_has_its_colour() {
    let noisy = |color: NoiseColor| PolySynthParams {
        noise: NoiseParams { color, level: 1.0 },
        ..plain(OscParams::OFF)
    };
    // The power in octave bands from 200 Hz to 12.8 kHz, per octave.
    let slope = |color: NoiseColor| {
        let signal = hold(noisy(color), 60, 2.0, 2.0);
        let power = power_spectrum(between(&signal, 0.05, 2.0), 4_096);
        let band = |low: f64| {
            let bin = |f: f64| (f * 4_096.0 / RATE) as usize;
            power[bin(low)..bin(2.0 * low)].iter().sum::<f64>()
        };
        let levels: Vec<f64> = (0..6)
            .map(|o| 10.0 * band(200.0 * f64::from(1 << o)).log10())
            .collect();
        let steps: Vec<f64> = levels.windows(2).map(|w| w[1] - w[0]).collect();
        (steps, rms(&signal))
    };
    let (white, white_level) = slope(NoiseColor::White);
    let (pink, pink_level) = slope(NoiseColor::Pink);
    println!("per octave: white {white:.2?} dB, pink {pink:.2?} dB");
    // White doubles its power every octave, pink keeps it.
    assert!(white.iter().all(|step| (step - 3.01).abs() < 0.5));
    assert!(pink.iter().all(|step| step.abs() < 0.75));
    assert!(decibels(pink_level / white_level).abs() < 3.0);
}

#[test]
fn extreme_settings_and_notes_stay_finite() {
    let wild_osc = OscParams {
        wave: Wave::Pulse,
        level: f64::INFINITY,
        semitones: 1e9,
        cents: f64::NAN,
        shape: -5.0,
        unison: 255,
        detune: 1e9,
        spread: f64::NAN,
        fm: 1e9,
        ring: f64::INFINITY,
        sync: true,
        mod_source: OscSource::Osc3,
        ..OscParams::SAW
    };
    let mut wild = PolySynthParams::INIT;
    wild.oscillators = [
        wild_osc,
        wild_osc,
        OscParams {
            wave: Wave::Table,
            ..wild_osc
        },
    ];
    wild.filters = [
        FilterParams {
            model: FilterModel::Ms20Lp,
            cutoff: f64::INFINITY,
            resonance: 1e9,
            drive: 1e9,
            key_track: 1e9,
            env_amount: f64::NAN,
            morph: -1.0,
        },
        FilterParams {
            model: FilterModel::CombMinus,
            cutoff: 0.0,
            resonance: 1.0,
            ..FilterParams::LOWPASS
        },
    ];
    wild.noise = NoiseParams {
        color: NoiseColor::Brown,
        level: 1e9,
    };
    wild.saturation = f64::NAN;
    wild.level = 1e9;
    wild.matrix = [ModSlot::new(ModSource::Random, ModDestination::Pitch, 48.0); MOD_SLOTS];
    for mode in VoiceMode::ALL {
        wild.mode = mode;
        let mut synth = synth_of([PolySynthVoice::new(wild), PolySynthVoice::new(wild)]);
        let signal = play(
            &mut synth,
            &[
                (0, on(127, 1.0)),
                (10, on(0, 1.0)),
                (100, Act::Bend(48.0)),
                (4_000, off(0)),
            ],
            8_000,
            64,
        );
        assert!(finite_within(&signal, 1e3), "{mode:?}: {}", peak(&signal));
    }
}

#[test]
fn the_library_is_large_covers_every_category_and_is_documented_in_order() {
    assert!(PRESETS.len() >= 64, "{}", PRESETS.len());
    for category in Category::ALL {
        let count = PRESETS.iter().filter(|p| p.category == category).count();
        assert!(count >= 6, "{category:?}: {count}");
    }
    // The table in the module's documentation lists every preset, by its
    // index, name, category and description.
    let docs = include_str!("mod.rs");
    for (index, preset) in PRESETS.iter().enumerate() {
        let row = format!(
            "//! | {index} | {} | {} | {} |",
            preset.name,
            preset.category.name(),
            preset.description
        );
        assert!(
            docs.contains(&row),
            "{} is not in the table as it is",
            preset.name
        );
    }
}

#[test]
fn every_preset_plays_and_keeps_its_level() {
    let mut names = std::collections::BTreeSet::new();
    for preset in PRESETS.iter() {
        assert!(
            names.insert(preset.name.to_ascii_lowercase()),
            "{} twice",
            preset.name
        );
        assert_eq!(
            preset.params.limited(),
            preset.params,
            "{} is out of range",
            preset.name
        );
        let key = match preset.category {
            Category::Bass => 36,
            Category::Lead | Category::Pluck => 69,
            _ => 60,
        };
        let mut synth = synth(&preset.params, 2, 1);
        let signal = play(
            &mut synth,
            &[(0, on(key, 0.8)), (14_400, off(key))],
            19_200,
            256,
        );
        let level = peak(&signal);
        assert!(level > 0.01 && level < 2.0, "{}: peak {level}", preset.name);
    }
    assert!(preset("trance supersaw").is_some());
    assert!(preset("no such preset").is_none());
}
