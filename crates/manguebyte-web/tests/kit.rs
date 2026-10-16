//! The toms, the side stick and the guitar body tap: each starts at once,
//! dies away as documented, peaks in a sane range, and sounds like what it
//! is, by measurement.

mod common;

use common::*;
use manguebyte_dsp::fft::{Complex, Fft};
use manguebyte_web::{Chain, Drum, EventRecord, InstrumentKind, ids};

/// One strike at full velocity, through no chain, rendered for two
/// seconds, with the voices still sounding at the end.
fn strike(drum: Drum, tune: f64) -> (Vec<f32>, usize) {
    let mut engine = engine_with(InstrumentKind::Drums, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::KIT_TUNE), tune),
            EventRecord::note_on(0, 0, drum.note(), 1.0),
        ],
    );
    let (left, _) = render(&mut engine, 2 * RATE as usize, 128);
    (left, engine.active_voices(0).unwrap())
}

/// Magnitudes of the spectrum of `samples` through a Hann window, bin `k`
/// at `k × RATE / samples.len()` hertz. The length must be a power of two.
fn spectrum(samples: &[f32]) -> Vec<f64> {
    let n = samples.len();
    let mut fft = Fft::<f64>::new(n);
    let mut data: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(i, &sample)| {
            let hann = 0.5 - 0.5 * (core::f64::consts::TAU * i as f64 / n as f64).cos();
            Complex::new(f64::from(sample) * hann, 0.0)
        })
        .collect();
    fft.forward(&mut data);
    data[..n / 2].iter().map(|value| value.norm()).collect()
}

/// The frequency of the strongest component between `low` and `high`
/// hertz, refined between bins by a parabola through the log magnitudes.
fn strongest(samples: &[f32], low: f64, high: f64) -> f64 {
    let magnitudes = spectrum(samples);
    let hz = RATE / samples.len() as f64;
    let range = (low / hz).ceil() as usize..=(high / hz).floor() as usize;
    let bin = range
        .max_by(|&a, &b| magnitudes[a].total_cmp(&magnitudes[b]))
        .unwrap();
    let [a, b, c] = [bin - 1, bin, bin + 1].map(|k| magnitudes[k].ln());
    let offset = 0.5 * (a - c) / (a - 2.0 * b + c);
    (bin as f64 + offset) * hz
}

/// The energy of `samples` between `low` and `high` hertz, as a share of
/// all of it.
fn share(samples: &[f32], low: f64, high: f64) -> f64 {
    let magnitudes = spectrum(samples);
    let hz = RATE / samples.len() as f64;
    let total: f64 = magnitudes.iter().map(|m| m * m).sum();
    let band: f64 = magnitudes
        .iter()
        .enumerate()
        .filter(|&(k, _)| (low..high).contains(&(k as f64 * hz)))
        .map(|(_, m)| m * m)
        .sum();
    band / total
}

/// How fast the sound moves: the level of its second difference over its
/// own level. About 2.4 for white noise, under 0.001 for anything below
/// 400 Hz, 0.1 for a sine at 2.6 kHz.
fn edge(samples: &[f32]) -> f64 {
    let second: Vec<f32> = samples
        .windows(3)
        .map(|w| w[2] - 2.0 * w[1] + w[0])
        .collect();
    rms(&second) / rms(samples)
}

fn rms(samples: &[f32]) -> f64 {
    (samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

/// What the documentation promises for each drum: how long until the
/// level stays 60 dB below its peak (the longest resonance's decay, at a
/// kit decay of 1), and by when, in seconds, the peak has come.
const PROMISES: [(Drum, f64, f64); 5] = [
    (Drum::HighTom, 0.5, 0.01),
    (Drum::MidTom, 0.6, 0.01),
    (Drum::FloorTom, 0.8, 0.012),
    (Drum::SideStick, 0.08, 0.004),
    (Drum::GuitarTap, 0.3, 0.012),
];

#[test]
fn each_new_drum_starts_at_once_decays_as_documented_and_ends() {
    for (drum, length, attack) in PROMISES {
        let name = drum.name();
        let (left, voices) = strike(drum, 0.0);
        assert!(left.iter().all(|sample| sample.is_finite()), "{name}");
        assert_eq!(onset(&left), Some(0), "{name}");
        let loud = peak(&left);
        let at = left.iter().position(|s| s.abs() == loud).unwrap() as f64 / RATE;
        let floor = loud * 1e-3;
        let heard = left.iter().rposition(|s| s.abs() > floor).unwrap() as f64 / RATE;
        eprintln!(
            "{name}: peak {loud:.3} at {:.1} ms, -60 dB after {heard:.3} s",
            at * 1e3
        );
        assert!(loud > 0.25 && loud < 0.8, "{name}: peak {loud}");
        assert!(at <= attack, "{name}: peak at {at} s");
        assert!(
            heard > 0.6 * length && heard < 1.1 * length,
            "{name}: {heard} s against {length} s"
        );
        // Falling: every twelfth of its length is quieter than the last,
        // from the end of the attack down to -40 dB (below that, resonances a few hertz
        // apart beat against each other).
        let block = (length / 12.0 * RATE) as usize;
        let blocks: Vec<f64> = left[block..(heard * RATE) as usize]
            .chunks(block)
            .map(rms)
            .take_while(|&level| level > 0.01 * f64::from(loud))
            .collect();
        assert!(blocks.len() >= 3, "{name}: {blocks:?}");
        for pair in blocks.windows(2) {
            assert!(pair[1] < pair[0] * 1.05, "{name}: rises {pair:?}");
        }
        // Silent and done well before two seconds.
        assert_eq!(voices, 0, "{name} still sounding");
        assert!(
            left[(1.5 * RATE) as usize..].iter().all(|&s| s == 0.0),
            "{name}"
        );
    }
}

#[test]
fn the_toms_are_tuned_high_to_low_and_fall_in_pitch_as_they_settle() {
    let mut settled = Vec::new();
    for (drum, documented) in [
        (Drum::HighTom, 165.0),
        (Drum::MidTom, 123.0),
        (Drum::FloorTom, 84.0),
    ] {
        let (left, _) = strike(drum, 0.0);
        let early = strongest(&left[..2_048], 50.0, 400.0);
        // From 0.15 s on, 8 192 frames: 5.9 Hz between bins.
        let late = strongest(&left[7_200..7_200 + 8_192], 50.0, 400.0);
        eprintln!(
            "{}: {early:.1} Hz in the first 43 ms, {late:.1} Hz from 0.15 s",
            drum.name()
        );
        assert!(
            (late / documented - 1.0).abs() < 0.03,
            "{}: {late} Hz",
            drum.name()
        );
        assert!(early > 1.04 * late, "{}: {early} -> {late}", drum.name());
        // A drum, not a sine: the noise of the stick is in the attack.
        let (stick, ring) = (edge(&left[..240]), edge(&left[4_800..9_600]));
        eprintln!(
            "{}: edge {stick:.4} in the first 5 ms, {ring:.5} at 0.1 s",
            drum.name()
        );
        assert!(stick > 0.02 && stick > 20.0 * ring, "{}", drum.name());
        settled.push(late);
    }
    assert!(
        settled[0] > settled[1] && settled[1] > settled[2],
        "{settled:?}"
    );
    // Twelve semitones of kit tune is an octave.
    let (up, _) = strike(Drum::MidTom, 12.0);
    let late = strongest(&up[7_200..7_200 + 8_192], 50.0, 600.0);
    assert!((late / (2.0 * settled[1]) - 1.0).abs() < 0.02, "{late}");
}

#[test]
fn the_side_stick_is_a_short_woody_click() {
    let (left, _) = strike(Drum::SideStick, 0.0);
    let window = &left[..2_048];
    // Its knock lives between 300 Hz and 3 kHz, not in the low end.
    let knock = share(window, 300.0, 3_200.0);
    let low = share(window, 0.0, 200.0);
    eprintln!("side stick: {knock:.2} of the energy at 0.3-3.2 kHz, {low:.3} below 200 Hz");
    assert!(knock > 0.7 && low < 0.02, "{knock} {low}");
    // Gone, to -40 dB, within 50 ms.
    let loud = peak(&left);
    assert!(peak(&left[(0.05 * RATE) as usize..]) < 0.01 * loud);
}

#[test]
fn the_guitar_tap_is_a_body_thump_with_a_nail_click() {
    let (left, _) = strike(Drum::GuitarTap, 0.0);
    // From 10 ms on, 8 192 frames, the body: the air resonance and the
    // top's modes hold nearly all of it.
    let body = &left[480..480 + 8_192];
    let air = strongest(body, 70.0, 150.0);
    let top = strongest(body, 180.0, 260.0);
    let modes = share(body, 80.0, 120.0) + share(body, 185.0, 260.0);
    eprintln!("guitar tap: air {air:.1} Hz, top {top:.1} Hz, {modes:.2} of the body there");
    assert!((air - 100.0).abs() < 4.0, "{air}");
    assert!((195.0..=250.0).contains(&top), "{top}");
    assert!(modes > 0.8, "{modes}");
    // The click: the first 2 ms move fast, as the body never does.
    let (click, later) = (edge(&left[..96]), edge(&left[2_400..4_800]));
    eprintln!("guitar tap: edge {click:.4} in the first 2 ms, {later:.5} from 50 ms");
    assert!(click > 0.1 && click > 100.0 * later, "{click} {later}");
}
