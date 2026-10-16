//! An accented note plays louder and brighter than the same note without
//! the accent, on the synth and on every drum.

mod common;

use common::*;
use manguebyte_dsp::fft::{Complex, Fft};
use manguebyte_web::{
    Chain, DRUM_ACCENT_DB, DRUM_MAP, EventRecord, InstrumentKind, OUTPUT_CEILING, SYNTH_ACCENT_DB,
};

/// The frames the synth's measurements look at: the first 4 096, 85 ms,
/// over which the accent's filter opening falls away.
const SYNTH_WINDOW: usize = 4_096;

/// The frames the drums' measurements look at: the first 1 024, 21 ms, the
/// attack an accent sharpens.
const DRUM_WINDOW: usize = 1_024;

/// Plays one note, accented or not, through no chain, and returns the left
/// channel.
fn play(kind: InstrumentKind, note: u8, velocity: f32, accent: bool, frames: usize) -> Vec<f32> {
    let mut engine = engine_with(kind, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::note_on(0, 0, note, velocity).with_accent(accent),
            EventRecord::note_off(frames as u64 / 2, 0, note),
        ],
    );
    render(&mut engine, frames, 128).0
}

/// The spectral centroid of `samples[..window]` in hertz: the mean of the
/// frequencies weighted by their magnitudes, through a Hann window.
fn centroid(samples: &[f32], window: usize) -> f64 {
    let mut fft = Fft::<f64>::new(window);
    let mut data: Vec<Complex<f64>> = samples[..window]
        .iter()
        .enumerate()
        .map(|(n, &sample)| {
            let hann = 0.5 - 0.5 * (core::f64::consts::TAU * n as f64 / window as f64).cos();
            Complex::new(f64::from(sample) * hann, 0.0)
        })
        .collect();
    fft.forward(&mut data);
    let (mut weighted, mut total) = (0.0, 0.0);
    for (bin, value) in data[1..window / 2].iter().enumerate() {
        let frequency = (bin + 1) as f64 * RATE / window as f64;
        let magnitude = value.norm();
        weighted += frequency * magnitude;
        total += magnitude;
    }
    weighted / total
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The peak and the centroid of a plain and an accented note, and the
/// accented note's lead over the plain one in both.
struct Comparison {
    plain_peak: f64,
    accent_peak: f64,
    gain_db: f64,
    plain_centroid: f64,
    accent_centroid: f64,
}

fn compare(kind: InstrumentKind, note: u8, velocity: f32) -> Comparison {
    let window = match kind {
        InstrumentKind::Synth => SYNTH_WINDOW,
        InstrumentKind::Drums => DRUM_WINDOW,
        InstrumentKind::Sampler | InstrumentKind::SamplerKit => {
            unreachable!("tests/sampler.rs measures their accents")
        }
        _ => unreachable!("tests/voices.rs measures their accents"),
    };
    let plain = play(kind, note, velocity, false, 24_000);
    let accented = play(kind, note, velocity, true, 24_000);
    // The same events give the same samples, accent or not, every time.
    assert_eq!(
        bits(&plain),
        bits(&play(kind, note, velocity, false, 24_000))
    );
    assert_eq!(
        bits(&accented),
        bits(&play(kind, note, velocity, true, 24_000))
    );
    let plain_peak = f64::from(peak(&plain[..window]));
    let accent_peak = f64::from(peak(&accented[..window]));
    Comparison {
        plain_peak,
        accent_peak,
        gain_db: db(accent_peak / plain_peak),
        plain_centroid: centroid(&plain, window),
        accent_centroid: centroid(&accented, window),
    }
}

#[test]
fn an_accented_synth_note_is_louder_and_brighter() {
    for (note, velocity) in [(33u8, 0.9f32), (45, 0.8), (57, 0.6), (69, 1.0)] {
        let c = compare(InstrumentKind::Synth, note, velocity);
        eprintln!(
            "synth {note} v{velocity}: peak {:.4} -> {:.4} ({:+.2} dB), centroid {:.0} -> {:.0} Hz",
            c.plain_peak, c.accent_peak, c.gain_db, c.plain_centroid, c.accent_centroid
        );
        // The level rises by the accent's gain, give or take what the more
        // open filter adds to the peak.
        assert!(
            (c.gain_db - SYNTH_ACCENT_DB).abs() < 1.5,
            "note {note}: {:+.2} dB",
            c.gain_db
        );
        assert!(
            c.accent_centroid > 1.1 * c.plain_centroid,
            "note {note}: centroid {} -> {}",
            c.plain_centroid,
            c.accent_centroid
        );
        assert!(c.accent_peak < f64::from(OUTPUT_CEILING));
    }
}

#[test]
fn an_accented_drum_is_louder_and_brighter() {
    for entry in DRUM_MAP {
        let c = compare(InstrumentKind::Drums, entry.note, 1.0);
        eprintln!(
            "{}: peak {:.4} -> {:.4} ({:+.2} dB), centroid {:.0} -> {:.0} Hz",
            entry.name, c.plain_peak, c.accent_peak, c.gain_db, c.plain_centroid, c.accent_centroid
        );
        // At least the accent's gain: the sharper attack adds to the peak.
        assert!(
            c.gain_db > DRUM_ACCENT_DB - 0.1 && c.gain_db < DRUM_ACCENT_DB + 3.0,
            "{}: {:+.2} dB",
            entry.name,
            c.gain_db
        );
        // Brighter on the attack: by tens of hertz for the cymbals, which
        // are bright already, by hundreds for the kick and the snare.
        assert!(
            c.accent_centroid > c.plain_centroid + 15.0,
            "{}: centroid {} -> {}",
            entry.name,
            c.plain_centroid,
            c.accent_centroid
        );
        // An accented drum at full velocity still stays below the ceiling.
        assert!(
            c.accent_peak < f64::from(OUTPUT_CEILING),
            "{}: peak {}",
            entry.name,
            c.accent_peak
        );
    }
}

/// An accent changes only the note it is on: the next note, struck the
/// same way without it, sounds exactly as if no accent had come before.
#[test]
fn an_accent_belongs_to_its_note_only() {
    for (kind, note) in [(InstrumentKind::Synth, 45u8), (InstrumentKind::Drums, 38)] {
        let second = 30_000u64;
        let run = |first_accented: bool| {
            let mut engine = engine_with(kind, Chain::NONE);
            push_all(
                &mut engine,
                &[
                    EventRecord::note_on(0, 0, note, 0.8).with_accent(first_accented),
                    EventRecord::note_off(10_000, 0, note),
                    EventRecord::note_on(second, 0, note, 0.8),
                    EventRecord::note_off(second + 10_000, 0, note),
                ],
            );
            render(&mut engine, 60_000, 128).0
        };
        let (after_plain, after_accent) = (run(false), run(true));
        assert_ne!(bits(&after_plain[..1_000]), bits(&after_accent[..1_000]));
        let tail = second as usize..;
        assert_eq!(
            bits(&after_plain[tail.clone()]),
            bits(&after_accent[tail]),
            "{kind:?}"
        );
    }
}
