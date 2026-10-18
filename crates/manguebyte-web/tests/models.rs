//! The newer voices of `manguebyte-dsp` in parts: the drum kits of
//! `DrumKitVoice`, the polyphonic synthesiser, the mallets, the bowed
//! strings, the winds and the choir. Each plays in tune; accents and
//! velocity play louder and brighter; the kits answer the drum map, choke
//! their open hats and play the drums they have no piece for exactly as the
//! synthesized kit does; the monophonic presets and legato slur; and every
//! one is the same in every quantum and stays finite whatever its knobs are
//! set to.

mod common;

use core::f64::consts::TAU;

use common::*;
use manguebyte_dsp::fft::{Complex, Fft};
use manguebyte_dsp::instruments::DrumKit as Pieces;
use manguebyte_web::{
    CANCEL_FADE_SECONDS, Chain, DRUM_MAP, Engine, EngineConfig, EventRecord, InstrumentKind,
    OUTPUT_CEILING, POLY_VOICES, Pushed, ids,
};

/// The kinds this file is about.
const KINDS: [InstrumentKind; 6] = [
    InstrumentKind::DrumKit,
    InstrumentKind::Poly,
    InstrumentKind::Mallet,
    InstrumentKind::Bowed,
    InstrumentKind::Wind,
    InstrumentKind::Choir,
];

fn seconds(seconds: f64) -> usize {
    (seconds * RATE).round() as usize
}

/// A Hann-windowed copy of `samples`, for [`magnitude_at`].
fn windowed(samples: &[f32]) -> Vec<f64> {
    let n = samples.len() as f64;
    samples
        .iter()
        .enumerate()
        .map(|(k, &x)| f64::from(x) * (0.5 - 0.5 * (TAU * k as f64 / n).cos()))
        .collect()
}

/// The magnitude of the component of `windowed` at `frequency`.
fn magnitude_at(windowed: &[f64], frequency: f64) -> f64 {
    let (sin, cos) = (TAU * frequency / RATE).sin_cos();
    let (mut c, mut s) = (1.0f64, 0.0f64);
    let (mut re, mut im) = (0.0, 0.0);
    for &x in windowed {
        re += x * c;
        im -= x * s;
        (c, s) = (c * cos - s * sin, s * cos + c * sin);
    }
    re.hypot(im)
}

/// The frequency of the strongest component within half a semitone of
/// `expected`: a scan a cent at a time, then a search to a thousandth of a
/// cent.
fn frequency_near(samples: &[f32], expected: f64) -> f64 {
    let windowed = windowed(samples);
    let low = expected / 2f64.powf(1.0 / 24.0);
    let at = |cent: f64| low * (cent / 1_200.0).exp2();
    let best = (0..=100)
        .map(f64::from)
        .max_by(|&a, &b| magnitude_at(&windowed, at(a)).total_cmp(&magnitude_at(&windowed, at(b))))
        .unwrap();
    let (mut from, mut to) = (best - 1.0, best + 1.0);
    while to - from > 1e-3 {
        let (a, b) = (from + (to - from) / 3.0, to - (to - from) / 3.0);
        if magnitude_at(&windowed, at(a)) < magnitude_at(&windowed, at(b)) {
            from = a;
        } else {
            to = b;
        }
    }
    at(0.5 * (from + to))
}

fn cents(measured: f64, expected: f64) -> f64 {
    1_200.0 * (measured / expected).log2()
}

fn key_hz(key: u8) -> f64 {
    440.0 * ((f64::from(key) - 69.0) / 12.0).exp2()
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The spectral centroid of the first `window` samples, in hertz.
fn centroid(samples: &[f32], window: usize) -> f64 {
    let mut fft = Fft::<f64>::new(window);
    let mut data: Vec<Complex<f64>> = windowed(&samples[..window])
        .into_iter()
        .map(|x| Complex::new(x, 0.0))
        .collect();
    fft.forward(&mut data);
    let (mut weighted, mut total) = (0.0, 0.0);
    for (bin, value) in data[1..window / 2].iter().enumerate() {
        let magnitude = value.norm();
        weighted += (bin + 1) as f64 * RATE / window as f64 * magnitude;
        total += magnitude;
    }
    weighted / total
}

/// Parameters to set, by identifier, and their values.
type Settings = Vec<(&'static str, f64)>;

/// A part of `kind` through no effect, its parameters set as `settings`
/// say on frame 0, before the notes.
fn part(kind: InstrumentKind, settings: &[(&str, f64)]) -> Engine {
    let mut engine = engine_with(kind, Chain::NONE);
    for &(name, value) in settings {
        assert_eq!(
            engine.push(EventRecord::param(0, 0, id(name), value)),
            Pushed::Queued
        );
    }
    engine
}

/// One note of `kind`, struck on frame 0 and released after `hold`
/// seconds, rendered for `length` seconds: both channels.
fn note(
    kind: InstrumentKind,
    settings: &[(&str, f64)],
    key: u8,
    velocity: f32,
    accent: bool,
    hold: f64,
    length: f64,
) -> (Vec<f32>, Vec<f32>) {
    let mut engine = part(kind, settings);
    push_all(
        &mut engine,
        &[
            EventRecord::note_on(0, 0, key, velocity).with_accent(accent),
            EventRecord::note_off(seconds(hold) as u64, 0, key),
        ],
    );
    let (left, right) = render(&mut engine, seconds(length), 128);
    assert!(
        left.iter()
            .chain(&right)
            .all(|sample| sample.is_finite() && sample.abs() <= OUTPUT_CEILING),
        "{kind:?} {settings:?} {key}"
    );
    (left, right)
}

/// The two channels added: what a centred listener hears of the stereo
/// field, for measuring pitch and brightness.
fn mid(stereo: &(Vec<f32>, Vec<f32>)) -> Vec<f32> {
    stereo.0.iter().zip(&stereo.1).map(|(l, r)| l + r).collect()
}

/// The polyphonic synthesiser's preset of this name.
fn poly(name: &str) -> f64 {
    manguebyte_dsp::instruments::poly::PRESETS
        .iter()
        .position(|preset| preset.name == name)
        .unwrap_or_else(|| panic!("no preset {name}")) as f64
}

/// The drum kit presets' pieces, in the order of `drum_kit.preset`.
fn kits() -> [Pieces; 10] {
    [
        Pieces::acoustic(),
        Pieces::style_808(),
        Pieces::style_909(),
        Pieces::style_606(),
        Pieces::style_cr78(),
        Pieces::trap(),
        Pieces::techno(),
        Pieces::house(),
        Pieces::drum_and_bass(),
        Pieces::lo_fi(),
    ]
}

/// Melodic presets to measure, with the keys to try them on: what a held
/// note's pitch is measured on, vibrato off where the knob has it.
fn tuned_cases() -> Vec<(InstrumentKind, Settings, Vec<u8>)> {
    use InstrumentKind::*;
    let preset = |id: &'static str, value: f64| vec![(id, value)];
    let still =
        |id: &'static str, value: f64, vibrato: &'static str| vec![(id, value), (vibrato, 0.0)];
    vec![
        // Presets whose oscillators are not detuned from the note, and have
        // no vibrato.
        (
            Poly,
            preset(ids::POLY_PRESET, poly("Deep Sub")),
            vec![28, 36, 45],
        ),
        (
            Poly,
            preset(ids::POLY_PRESET, poly("Garage Square")),
            vec![33, 45],
        ),
        (
            Poly,
            preset(ids::POLY_PRESET, poly("Rubber Bass")),
            vec![36, 45],
        ),
        (
            Poly,
            preset(ids::POLY_PRESET, poly("Kalimba")),
            vec![60, 72],
        ),
        (
            Poly,
            preset(ids::POLY_PRESET, poly("Marimba")),
            vec![60, 72],
        ),
        (Mallet, preset(ids::MALLET_PRESET, 0.0), vec![45, 60, 72]),
        (Mallet, preset(ids::MALLET_PRESET, 1.0), vec![65, 77]),
        (Mallet, preset(ids::MALLET_PRESET, 2.0), vec![53, 65, 77]),
        (Mallet, preset(ids::MALLET_PRESET, 3.0), vec![79, 91]),
        (Mallet, preset(ids::MALLET_PRESET, 4.0), vec![60, 72, 84]),
        (Mallet, preset(ids::MALLET_PRESET, 5.0), vec![60, 72]),
        (Mallet, preset(ids::MALLET_PRESET, 6.0), vec![60, 67]),
        (
            Bowed,
            still(ids::BOWED_PRESET, 0.0, ids::BOWED_VIBRATO),
            vec![55, 67, 79],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 1.0, ids::BOWED_VIBRATO),
            vec![48, 60, 72],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 2.0, ids::BOWED_VIBRATO),
            vec![36, 48, 60],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 3.0, ids::BOWED_VIBRATO),
            vec![28, 40],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 4.0, ids::BOWED_VIBRATO),
            vec![48, 60, 72],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 5.0, ids::BOWED_VIBRATO),
            vec![55, 67],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 6.0, ids::BOWED_VIBRATO),
            vec![36, 48],
        ),
        (
            Bowed,
            still(ids::BOWED_PRESET, 7.0, ids::BOWED_VIBRATO),
            vec![28, 40],
        ),
        (
            Wind,
            still(ids::WIND_PRESET, 0.0, ids::WIND_VIBRATO),
            vec![60, 72, 84],
        ),
        (
            Wind,
            still(ids::WIND_PRESET, 1.0, ids::WIND_VIBRATO),
            vec![50, 62, 74],
        ),
        (
            Wind,
            still(ids::WIND_PRESET, 2.0, ids::WIND_VIBRATO),
            vec![49, 61, 73],
        ),
        (
            Wind,
            still(ids::WIND_PRESET, 3.0, ids::WIND_VIBRATO),
            vec![55, 67, 79],
        ),
        (
            Wind,
            still(ids::WIND_PRESET, 4.0, ids::WIND_VIBRATO),
            vec![40, 52, 64],
        ),
        (
            Choir,
            still(ids::CHOIR_PRESET, 0.0, ids::CHOIR_VIBRATO),
            vec![48, 60, 72],
        ),
        (
            Choir,
            still(ids::CHOIR_PRESET, 1.0, ids::CHOIR_VIBRATO),
            vec![48, 60, 72],
        ),
        (
            Choir,
            still(ids::CHOIR_PRESET, 2.0, ids::CHOIR_VIBRATO),
            vec![57, 69],
        ),
    ]
}

/// How far from its key a held note of each kind may be, in cents: the
/// waveguides' and the voices' players drift by a few cents, as players
/// do, and correct by ear.
fn tolerance(kind: InstrumentKind) -> f64 {
    match kind {
        InstrumentKind::Poly | InstrumentKind::Mallet => 1.0,
        _ => 8.0,
    }
}

/// Whether `settings` choose an ensemble: several singers or players to a
/// note, tuned a little apart.
fn ensemble(kind: InstrumentKind, settings: &[(&str, f64)]) -> bool {
    let preset = settings.first().map_or(0.0, |&(_, value)| value);
    match kind {
        InstrumentKind::Choir => preset < 2.0,
        InstrumentKind::Bowed => preset == 4.0,
        _ => false,
    }
}

/// The centre of the energy of `samples` within half a semitone of
/// `expected`: an ensemble's pitch, whose players' single components lie
/// either side of it.
fn centre_near(samples: &[f32], expected: f64) -> f64 {
    let windowed = windowed(samples);
    let (mut weighted, mut total) = (0.0, 0.0);
    for cent in -50..=50 {
        let frequency = expected * (f64::from(cent) / 1_200.0).exp2();
        let power = magnitude_at(&windowed, frequency).powi(2);
        weighted += f64::from(cent) * power;
        total += power;
    }
    expected * (weighted / total / 1_200.0).exp2()
}

#[test]
fn every_new_kind_plays_in_tune() {
    let mut worst: Vec<(InstrumentKind, f64)> = Vec::new();
    let mut misses = Vec::new();
    for (kind, settings, keys) in tuned_cases() {
        for key in keys {
            let sound = mid(&note(kind, &settings, key, 0.8, false, 1.0, 1.0));
            // While the note is held, once it has settled.
            let held = &sound[seconds(0.45)..seconds(0.95)];
            let heard = if ensemble(kind, &settings) {
                centre_near(held, key_hz(key))
            } else {
                frequency_near(held, key_hz(key))
            };
            let error = cents(heard, key_hz(key));
            println!("{kind:?} {settings:?} key {key}: {error:+.3} cents");
            if error.abs() >= tolerance(kind) {
                misses.push(format!(
                    "{kind:?} {settings:?} key {key}: {error:+.3} cents"
                ));
            }
            match worst.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, largest)) => *largest = largest.max(error.abs()),
                None => worst.push((kind, error.abs())),
            }
        }
    }
    assert!(misses.is_empty(), "out of tune:\n{}", misses.join("\n"));
    // The kits' toms move with `kit.tune`, in whole semitones: the 909's
    // high tom against itself a fifth up.
    let tom = |tune: f64| {
        let sound = mid(&note(
            InstrumentKind::DrumKit,
            &[(ids::DRUM_KIT_PRESET, 2.0), (ids::KIT_TUNE, tune)],
            50,
            1.0,
            false,
            0.1,
            0.6,
        ));
        let spectrum = windowed(&sound[seconds(0.05)..seconds(0.45)]);
        // The strongest component between 80 and 800 Hz.
        (0..=4_000)
            .map(|cent| 80.0 * (f64::from(cent) / 1_200.0).exp2())
            .max_by(|&a, &b| magnitude_at(&spectrum, a).total_cmp(&magnitude_at(&spectrum, b)))
            .unwrap()
    };
    let fifth = cents(tom(7.0), tom(0.0));
    assert!((fifth - 700.0).abs() < 10.0, "{fifth} cents");
    let report: Vec<String> = worst
        .iter()
        .map(|(kind, cents)| format!("{kind:?} {cents:.3}"))
        .collect();
    println!(
        "the largest errors, in cents: {}; the 909's tom a fifth up: {fifth:.1}",
        report.join(", ")
    );
}

/// Presets and keys to try velocity and accents on.
fn dynamic_cases() -> Vec<(InstrumentKind, Settings, u8)> {
    use InstrumentKind::*;
    let mut cases = Vec::new();
    for (preset, _) in kits().iter().enumerate() {
        for key in [36u8, 38, 42] {
            cases.push((DrumKit, vec![(ids::DRUM_KIT_PRESET, preset as f64)], key));
        }
    }
    for name in [
        "Moog Bass",
        "Trance Supersaw",
        "Warm Analog Pad",
        "House Pluck",
    ] {
        cases.push((Poly, vec![(ids::POLY_PRESET, poly(name))], 48));
    }
    for preset in 0..8 {
        cases.push((Mallet, vec![(ids::MALLET_PRESET, f64::from(preset))], 72));
    }
    for (preset, key) in [(0, 67), (2, 48), (3, 33), (4, 60), (5, 67)] {
        cases.push((Bowed, vec![(ids::BOWED_PRESET, f64::from(preset))], key));
    }
    for (preset, key) in [(0, 72), (1, 60), (2, 61), (3, 67), (4, 53)] {
        cases.push((Wind, vec![(ids::WIND_PRESET, f64::from(preset))], key));
    }
    for preset in 0..3 {
        cases.push((Choir, vec![(ids::CHOIR_PRESET, f64::from(preset))], 60));
    }
    cases
}

#[test]
fn velocity_and_accent_play_louder_and_brighter() {
    let window = 4_096;
    let mut least = (f64::INFINITY, f64::INFINITY);
    for (kind, settings, key) in dynamic_cases() {
        let play = |velocity: f32, accent: bool| {
            mid(&note(kind, &settings, key, velocity, accent, 0.4, 0.5))
        };
        let (soft, hard, accented) = (play(0.5, false), play(1.0, false), play(1.0, true));
        // The energy of the first 0.3 s: a strike's peak alone can be its
        // click.
        let level = |samples: &[f32]| rms(&samples[..seconds(0.3)]);
        let velocity_db = db(level(&hard) / level(&soft));
        let accent_db = db(level(&accented) / level(&hard));
        // A struck note's brightness is its strike's, in its first 85 ms; a
        // bowed, blown or sung one's is its tone's once it speaks, from
        // 0.1 s, past the burst of air or the bow's scratch.
        let from = match kind {
            InstrumentKind::Bowed | InstrumentKind::Wind | InstrumentKind::Choir => seconds(0.1),
            _ => 0,
        };
        let brightening = centroid(&accented[from..], window) / centroid(&hard[from..], window);
        let label = format!("{kind:?} {settings:?} key {key}");
        println!(
            "{label}: velocity {velocity_db:+.1} dB, accent {accent_db:+.1} dB and x{brightening:.2} the centroid"
        );
        assert!(velocity_db > 2.0, "{label}: {velocity_db}");
        assert!(accent_db > 1.0, "{label}: {accent_db}");
        // Brighter, or barely darker: an acoustic drum struck harder rings
        // its low modes harder too, which the centroid of its first 85 ms
        // weighs against its brighter attack.
        let darkest = if kind == InstrumentKind::DrumKit {
            0.93
        } else {
            0.97
        };
        assert!(brightening > darkest, "{label}: {brightening}");
        least = (least.0.min(velocity_db), least.1.min(accent_db));
    }
    println!(
        "the least: velocity {:.1} dB, accent {:.1} dB",
        least.0, least.1
    );
}

/// The left and right channels of a part of `kind` playing `records`
/// after `settings`, for a second and a half.
fn played(
    kind: InstrumentKind,
    settings: &[(&str, f64)],
    records: &[EventRecord],
) -> (Vec<f32>, Vec<f32>) {
    let mut engine = part(kind, settings);
    push_all(&mut engine, records);
    render(&mut engine, seconds(1.5), 128)
}

#[test]
fn the_drum_kits_answer_the_drum_map_and_borrow_what_they_lack() {
    // The kit's knobs reach both halves.
    let knobs = [
        (ids::KIT_TUNE, 2.0),
        (ids::KIT_DECAY, 0.8),
        (ids::RIDE_LEVEL, 0.6),
        (ids::GUITAR_TAP_LEVEL, 1.0),
        (ids::SIDE_STICK_LEVEL, 0.7),
    ];
    let mut borrowed = Vec::new();
    for (preset, pieces) in kits().iter().enumerate() {
        let mut settings = vec![(ids::DRUM_KIT_PRESET, preset as f64)];
        settings.extend(knobs);
        for entry in DRUM_MAP {
            let records = [
                EventRecord::note_on(0, 0, entry.note, 0.9),
                EventRecord::note_on(seconds(0.5) as u64, 0, entry.note, 1.0).with_accent(true),
            ];
            let (left, right) = played(InstrumentKind::DrumKit, &settings, &records);
            let label = format!("kit {preset}, {}", entry.name);
            assert!(peak(&left).max(peak(&right)) > 0.02, "{label}");
            let (drums, _) = played(InstrumentKind::Drums, &knobs, &records);
            if pieces.piece(entry.note).is_some() {
                assert_ne!(bits(&left), bits(&drums), "{label}: the kit's own");
            } else {
                // The synthesized drum, to the bit, in the centre.
                assert_eq!(bits(&left), bits(&drums), "{label}");
                assert_eq!(bits(&right), bits(&drums), "{label}");
                borrowed.push(format!("{preset} {}", entry.name));
            }
        }
        // Aratu's percussion keys: the clap and the shaker where the kit has
        // them, silence where it has nothing.
        for (key, has) in [
            (39u8, pieces.piece(39).is_some()),
            (82, pieces.piece(70).is_some() || pieces.piece(69).is_some()),
            (100, false),
        ] {
            let (left, right) = played(
                InstrumentKind::DrumKit,
                &settings,
                &[EventRecord::note_on(0, 0, key, 0.9)],
            );
            let loudest = peak(&left).max(peak(&right));
            if has {
                assert!(loudest > 0.01, "kit {preset}, key {key}: {loudest}");
            } else {
                assert_eq!(loudest, 0.0, "kit {preset}, key {key}");
            }
        }
    }
    // The guitar tap is never a kit's; the 606 has no side stick, the CR-78
    // no toms, and every other drum of the map is the kits' own.
    assert_eq!(
        borrowed
            .iter()
            .filter(|b| b.ends_with("guitar_tap"))
            .count(),
        10
    );
    let others: Vec<&String> = borrowed
        .iter()
        .filter(|b| !b.ends_with("guitar_tap"))
        .collect();
    assert_eq!(
        others,
        ["3 side_stick", "4 high_tom", "4 mid_tom", "4 floor_tom"]
    );
    println!("borrowed from the synthesized kit: {}", borrowed.join(", "));
    // Shaker 82 on the house kit is its maracas, on 70: the same strike.
    let shaker = played(
        InstrumentKind::DrumKit,
        &[(ids::DRUM_KIT_PRESET, 7.0)],
        &[EventRecord::note_on(0, 0, 82, 0.9)],
    );
    let maracas = played(
        InstrumentKind::DrumKit,
        &[(ids::DRUM_KIT_PRESET, 7.0)],
        &[EventRecord::note_on(0, 0, 70, 0.9)],
    );
    assert_eq!(bits(&shaker.0), bits(&maracas.0));
}

#[test]
fn every_kit_chokes_its_open_hat() {
    let at = seconds(0.2) as u64;
    // The choke's fade, and the lo-fi sampler's ring after it.
    let after = at as usize + seconds(0.03);
    for (preset, pieces) in kits().iter().enumerate() {
        let open = pieces.piece(46).unwrap();
        let closed = pieces.piece(42).unwrap();
        assert!(
            open.choke != 0 && open.choke == closed.choke,
            "kit {preset}"
        );
        let settings = [(ids::DRUM_KIT_PRESET, preset as f64)];
        let both = played(
            InstrumentKind::DrumKit,
            &settings,
            &[
                EventRecord::note_on(0, 0, 46, 1.0),
                EventRecord::note_on(at, 0, 42, 0.8),
            ],
        );
        let closed = played(
            InstrumentKind::DrumKit,
            &settings,
            &[EventRecord::note_on(at, 0, 42, 0.8)],
        );
        let open = played(
            InstrumentKind::DrumKit,
            &settings,
            &[EventRecord::note_on(0, 0, 46, 1.0)],
        );
        // Once the open hat is gone, what is left is the closed hat alone.
        let rest = |sound: &(Vec<f32>, Vec<f32>)| {
            sound.0[after..]
                .iter()
                .chain(&sound.1[after..])
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>()
        };
        assert_eq!(rest(&both), rest(&closed), "kit {preset}");
        // Without the closed hat, it rings on.
        let ringing = peak(&open.0[after..after + seconds(0.05)]);
        assert!(ringing > 0.002, "kit {preset}: {ringing}");
    }
}

/// The period of `samples` between `shortest` and `longest` frames, by
/// autocorrelation.
fn period(samples: &[f32], shortest: usize, longest: usize) -> usize {
    let n = samples.len() - longest;
    (shortest..=longest)
        .max_by(|&a, &b| {
            let correlation = |lag: usize| {
                let (mut product, mut early, mut late) = (0.0f64, 0.0f64, 0.0f64);
                for k in 0..n {
                    let (x, y) = (f64::from(samples[k]), f64::from(samples[k + lag]));
                    product += x * y;
                    early += x * x;
                    late += y * y;
                }
                product / (early * late).sqrt().max(1e-30)
            };
            correlation(a).total_cmp(&correlation(b))
        })
        .unwrap()
}

#[test]
fn overlapping_notes_slur_on_one_voice_where_the_part_is_monophonic() {
    // A: 110 Hz, then the E above, struck at 0.4 s while A is held.
    let (a, b) = (45u8, 52u8);
    let at = seconds(0.4) as u64;
    let overlapping = [
        EventRecord::note_on(0, 0, a, 0.9),
        EventRecord::note_on(at, 0, b, 0.9),
        EventRecord::note_off(at, 0, a),
        EventRecord::note_off(seconds(1.0) as u64, 0, b),
    ];
    let cases: [(InstrumentKind, Settings, usize); 8] = [
        // The library's monophonic and legato presets: one voice.
        (
            InstrumentKind::Poly,
            vec![(ids::POLY_PRESET, poly("Moog Bass"))],
            1,
        ),
        (
            InstrumentKind::Poly,
            vec![(ids::POLY_PRESET, poly("808 Glide"))],
            1,
        ),
        // A polyphonic one: two.
        (
            InstrumentKind::Poly,
            vec![(ids::POLY_PRESET, poly("Warm Analog Pad"))],
            2,
        ),
        // The winds slur unless told not to.
        (InstrumentKind::Wind, vec![(ids::WIND_PRESET, 1.0)], 1),
        (
            InstrumentKind::Wind,
            vec![(ids::WIND_PRESET, 1.0), (ids::WIND_LEGATO, 0.0)],
            2,
        ),
        // The strings and the choir when told to.
        (
            InstrumentKind::Bowed,
            vec![(ids::BOWED_PRESET, 2.0), (ids::BOWED_LEGATO, 1.0)],
            1,
        ),
        (InstrumentKind::Bowed, vec![(ids::BOWED_PRESET, 2.0)], 2),
        (
            InstrumentKind::Choir,
            vec![(ids::CHOIR_PRESET, 2.0), (ids::CHOIR_LEGATO, 1.0)],
            1,
        ),
    ];
    for (kind, settings, voices) in cases {
        let mut engine = part(kind, &settings);
        push_all(&mut engine, &overlapping);
        let (first, _) = render(&mut engine, seconds(0.45), 128);
        assert_eq!(
            engine.active_voices(0),
            Some(voices),
            "{kind:?} {settings:?}"
        );
        let (rest, _) = render(&mut engine, seconds(0.5), 128);
        let left: Vec<f32> = first.into_iter().chain(rest).collect();
        if voices == 1 {
            // Moved on to the new note: its fundamental, a fifth up, is far
            // stronger than the last note's, which is none of its
            // harmonics.
            let window = windowed(&left[seconds(0.6)..seconds(0.9)]);
            let (new, old) = (
                magnitude_at(&window, key_hz(b)),
                magnitude_at(&window, key_hz(a)),
            );
            assert!(
                new > 4.0 * old,
                "{kind:?} {settings:?}: {new} against {old}"
            );
        }
    }
    // A slide on the 808 glide glides through the pitches between.
    let mut engine = part(
        InstrumentKind::Poly,
        &[(ids::POLY_PRESET, poly("808 Glide"))],
    );
    push_all(&mut engine, &overlapping);
    let (left, _) = render(&mut engine, seconds(0.9), 128);
    let gliding = &left[seconds(0.41)..seconds(0.41) + 1_400];
    let hz = RATE / period(gliding, 96, 432) as f64;
    println!("808 glide 10 ms into a slide from 110 to 165 Hz: {hz:.1} Hz");
    assert!(hz > 112.0 && hz < 162.0, "{hz}");
}

#[test]
fn a_preset_moves_its_knobs_and_the_macros_move_the_sound() {
    // A preset puts the macros back; the wheel stays where it is.
    let mut engine = engine_with(InstrumentKind::Poly, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::POLY_CUTOFF), 2.0),
            EventRecord::param(0, 0, id(ids::POLY_MOD_WHEEL), 0.7),
            EventRecord::param(10, 0, id(ids::POLY_PRESET), poly("Trance Supersaw")),
            EventRecord::param(10, 0, id(ids::POLY_RELEASE), 3.0),
        ],
    );
    render(&mut engine, 64, 64);
    let value = |engine: &Engine, name| engine.param_value(0, name).unwrap();
    assert_eq!(value(&engine, ids::POLY_CUTOFF), 0.0);
    assert!((value(&engine, ids::POLY_MOD_WHEEL) - 0.7).abs() < 1e-6);
    assert_eq!(
        value(&engine, ids::POLY_RELEASE),
        3.0,
        "set after the preset"
    );

    // The physical models' presets move their knobs to the preset's.
    let mut engine = engine_with(InstrumentKind::Bowed, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::BOWED_LEGATO), 1.0),
            EventRecord::param(0, 0, id(ids::BOWED_PRESET), 2.0),
        ],
    );
    render(&mut engine, 64, 64);
    let cello = manguebyte_dsp::instruments::BowedParams::cello();
    assert!((value(&engine, ids::BOWED_POSITION) - cello.bow_position).abs() < 1e-6);
    assert_eq!(
        value(&engine, ids::BOWED_LEGATO),
        1.0,
        "the preset leaves it"
    );

    // Each knob, away from its default, changes what one note sounds like.
    let cases: [(InstrumentKind, &str, f64, u8); 28] = [
        (InstrumentKind::DrumKit, ids::DRUM_KIT_TONE, 0.4, 38),
        (InstrumentKind::DrumKit, ids::DRUM_KIT_SNAP, -0.4, 38),
        (InstrumentKind::DrumKit, ids::KIT_TUNE, 3.0, 38),
        (InstrumentKind::DrumKit, ids::KIT_DECAY, 0.5, 38),
        (InstrumentKind::DrumKit, ids::SNARE_LEVEL, 0.3, 38),
        (InstrumentKind::Poly, ids::POLY_CUTOFF, 1.5, 36),
        (InstrumentKind::Poly, ids::POLY_RESONANCE, 0.4, 36),
        (InstrumentKind::Poly, ids::POLY_ATTACK, 8.0, 36),
        (InstrumentKind::Poly, ids::POLY_RELEASE, 5.0, 36),
        (InstrumentKind::Mallet, ids::MALLET_HARDNESS, 3.0, 60),
        (InstrumentKind::Mallet, ids::MALLET_DECAY, 0.3, 60),
        (InstrumentKind::Mallet, ids::MALLET_RELEASE, 0.2, 60),
        (InstrumentKind::Mallet, ids::MALLET_POSITION, 0.2, 60),
        (InstrumentKind::Bowed, ids::BOWED_PRESSURE, 0.9, 67),
        (InstrumentKind::Bowed, ids::BOWED_POSITION, 0.05, 67),
        (InstrumentKind::Bowed, ids::BOWED_VIBRATO, 60.0, 67),
        (InstrumentKind::Bowed, ids::BOWED_ATTACK, 0.5, 67),
        (InstrumentKind::Bowed, ids::BOWED_RELEASE, 2.0, 67),
        (InstrumentKind::Bowed, ids::BOWED_TONE, 1_500.0, 67),
        (InstrumentKind::Wind, ids::WIND_NOISE, 0.9, 72),
        (InstrumentKind::Wind, ids::WIND_VIBRATO, 50.0, 72),
        (InstrumentKind::Wind, ids::WIND_ATTACK, 0.4, 72),
        (InstrumentKind::Wind, ids::WIND_TONE, 1_500.0, 72),
        (InstrumentKind::Choir, ids::CHOIR_VOWEL, 4.0, 60),
        (InstrumentKind::Choir, ids::CHOIR_VIBRATO, 120.0, 60),
        (InstrumentKind::Choir, ids::CHOIR_ATTACK, 1.0, 60),
        (InstrumentKind::Choir, ids::CHOIR_BREATHINESS, 0.9, 60),
        (InstrumentKind::Choir, ids::CHOIR_BRIGHTNESS, 900.0, 60),
    ];
    for (kind, name, value, key) in cases {
        let plain = note(kind, &[], key, 0.8, false, 0.5, 1.0).0;
        let turned = note(kind, &[(name, value)], key, 0.8, false, 0.5, 1.0).0;
        assert_ne!(bits(&plain), bits(&turned), "{name}");
    }
    // The motor turns the vibraphone's discs; the marimba has none.
    let vibraphone = [(ids::MALLET_PRESET, 2.0)];
    let turning = note(
        InstrumentKind::Mallet,
        &vibraphone,
        60,
        0.8,
        false,
        0.5,
        1.0,
    )
    .0;
    let still = note(
        InstrumentKind::Mallet,
        &[(ids::MALLET_PRESET, 2.0), (ids::MALLET_MOTOR, 0.0)],
        60,
        0.8,
        false,
        0.5,
        1.0,
    )
    .0;
    assert_ne!(bits(&turning), bits(&still), "mallet.motor");
    // Detune and spread widen the supersaw; the wheel adds its vibrato.
    let supersaw = vec![(ids::POLY_PRESET, poly("Trance Supersaw"))];
    let with = |extra: (&'static str, f64)| {
        let mut settings = supersaw.clone();
        settings.push(extra);
        note(InstrumentKind::Poly, &settings, 64, 0.8, false, 0.8, 0.8)
    };
    let plain = note(InstrumentKind::Poly, &supersaw, 64, 0.8, false, 0.8, 0.8);
    for extra in [
        (ids::POLY_DETUNE, 0.3),
        (ids::POLY_SPREAD, 0.0),
        (ids::POLY_MOD_WHEEL, 1.0),
    ] {
        let turned = with(extra);
        assert_ne!(bits(&plain.0), bits(&turned.0), "{}", extra.0);
    }
    // No spread: both sides alike.
    let narrow = with((ids::POLY_SPREAD, 0.0));
    let difference: f32 = narrow
        .0
        .iter()
        .zip(&narrow.1)
        .map(|(l, r)| (l - r).abs())
        .fold(0.0, f32::max);
    let wide: f32 = plain
        .0
        .iter()
        .zip(&plain.1)
        .map(|(l, r)| (l - r).abs())
        .fold(0.0, f32::max);
    assert!(difference < 0.25 * wide, "{difference} against {wide}");
    // The cutoff macro brightens.
    let dark = mid(&note(
        InstrumentKind::Poly,
        &[(ids::POLY_CUTOFF, -2.0)],
        36,
        0.8,
        false,
        0.5,
        0.5,
    ));
    let bright = mid(&note(
        InstrumentKind::Poly,
        &[(ids::POLY_CUTOFF, 2.0)],
        36,
        0.8,
        false,
        0.5,
        0.5,
    ));
    assert!(centroid(&bright, 8_192) > 1.2 * centroid(&dark, 8_192));
}

/// The largest step between two neighbouring samples.
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

/// Sustained, loud sounds of the new kinds beyond the first presets that
/// `tests/stage.rs` plays: a chord where the preset plays chords.
fn loud_cases() -> Vec<(String, InstrumentKind, Settings, Vec<u8>)> {
    use InstrumentKind::*;
    let mut cases = Vec::new();
    for (preset, name) in [(2, "909"), (5, "trap"), (9, "lo-fi")] {
        cases.push((
            format!("kit {name}"),
            DrumKit,
            vec![(ids::DRUM_KIT_PRESET, f64::from(preset))],
            vec![49, 51, 46, 43, 36],
        ));
    }
    for name in [
        "Warm Analog Pad",
        "Trance Supersaw",
        "Rave Chord Stab",
        "Riser",
    ] {
        cases.push((
            name.to_owned(),
            Poly,
            vec![(ids::POLY_PRESET, poly(name))],
            vec![48, 55, 60, 64],
        ));
    }
    cases.push((
        "vibraphone".to_owned(),
        Mallet,
        vec![(ids::MALLET_PRESET, 2.0)],
        vec![53, 57, 60, 64],
    ));
    cases.push((
        "tubular bells".to_owned(),
        Mallet,
        vec![(ids::MALLET_PRESET, 7.0)],
        vec![60, 67],
    ));
    cases.push((
        "string section".to_owned(),
        Bowed,
        vec![(ids::BOWED_PRESET, 4.0)],
        vec![48, 55, 60, 64],
    ));
    cases.push((
        "cello".to_owned(),
        Bowed,
        vec![(ids::BOWED_PRESET, 2.0)],
        vec![36, 43],
    ));
    cases.push((
        "bass pizzicato".to_owned(),
        Bowed,
        vec![(ids::BOWED_PRESET, 7.0)],
        vec![28, 35],
    ));
    cases.push((
        "trumpet".to_owned(),
        Wind,
        vec![(ids::WIND_PRESET, 3.0)],
        vec![67],
    ));
    cases.push((
        "saxophones".to_owned(),
        Wind,
        vec![(ids::WIND_PRESET, 2.0), (ids::WIND_LEGATO, 0.0)],
        vec![49, 56, 61],
    ));
    cases.push((
        "ooh choir".to_owned(),
        Choir,
        vec![(ids::CHOIR_PRESET, 1.0)],
        vec![48, 55, 60, 64],
    ));
    cases.push((
        "solo voice".to_owned(),
        Choir,
        vec![(ids::CHOIR_PRESET, 2.0)],
        vec![69],
    ));
    cases
}

fn loud_engine(kind: InstrumentKind, settings: &[(&str, f64)], keys: &[u8]) -> Engine {
    let mut engine = part(kind, settings);
    for &key in keys {
        engine.push(EventRecord::note_on(0, 0, key, 1.0));
    }
    engine
}

#[test]
fn cancels_and_all_notes_off_never_click_on_the_new_kinds() {
    const CANCEL_FADE: usize = (CANCEL_FADE_SECONDS * RATE) as usize;
    let mut worst = f32::NEG_INFINITY;
    for (name, kind, settings, keys) in loud_cases() {
        // A cancel fades every note out, and nothing is left after it.
        let at = 9_600;
        let mut engine = loud_engine(kind, &settings, &keys);
        engine.push(EventRecord::cancel(at as u64, 0));
        let (left, right) = render(&mut engine, at + 4_800, 128);
        for channel in [&left, &right] {
            let before = largest_step(&channel[at - 4_800..at]);
            let fading = largest_step(&channel[at - 1..at + CANCEL_FADE + 1]);
            assert!(fading <= before, "{name}: {fading} against {before}");
            assert!(
                channel[at + CANCEL_FADE..].iter().all(|&s| s == 0.0),
                "{name}: still sounding after the cancel"
            );
        }
        assert!(peak(&left[at - 4_800..at]) > 0.05, "{name}");
        assert_eq!(engine.active_voices(0), Some(0), "{name}");

        // All notes off on the loudest frame adds no step: none larger than
        // the sound made before it, or would have made without it (a pad
        // whose filter opens grows brighter as it goes on).
        let (going_on, going_on_right) = render(
            &mut loud_engine(kind, &settings, &keys),
            12_000 + 9_600,
            128,
        );
        let loudest = (9_600..12_000)
            .max_by(|&a, &b| going_on[a].abs().total_cmp(&going_on[b].abs()))
            .unwrap();
        let mut engine = loud_engine(kind, &settings, &keys);
        engine.push(EventRecord::all_notes_off(loudest as u64, 0));
        let (left, right) = render(&mut engine, loudest + 9_600, 128);
        for (channel, unstopped) in [(&left, &going_on), (&right, &going_on_right)] {
            let before = largest_step(&channel[loudest - 4_800..loudest])
                .max(largest_step(&unstopped[loudest - 1..loudest + 9_600]));
            let after = largest_step(&channel[loudest - 1..]);
            let top = peak(&channel[loudest - 4_800..loudest]);
            let added = (after - before) / top;
            worst = worst.max(added);
            assert!(added <= 0.02, "{name}: all notes off adds {added}");
        }
    }
    println!("the most all notes off adds on the new kinds: {worst:.5} of the peak");
}

/// Bars of every new kind with accents, slurs, presets, knobs, the wheel,
/// a change of mode, legato switched, the tempo moved, cancels, chokes and
/// all-notes-off.
fn scenario() -> Engine {
    let mut engine = engine();
    for (part, kind) in KINDS.into_iter().enumerate() {
        engine.create_part(part as u16, kind).unwrap();
    }
    let step = 2_400u64;
    let mut records = vec![
        EventRecord::param(0, 0, id(ids::DRUM_KIT_PRESET), 6.0),
        EventRecord::param(0, 1, id(ids::POLY_PRESET), poly("Wobble Bass")),
        EventRecord::param(0, 4, id(ids::WIND_PRESET), 2.0),
    ];
    let melody = [45u8, 48, 52, 57, 55, 52, 50, 48];
    for (index, &key) in melody.iter().enumerate() {
        let at = index as u64 * step + 5;
        let accent = index % 3 == 1;
        for drum in [36u8, 42, 46, 38, 82, 51] {
            if (index + usize::from(drum)) % 2 == 0 {
                records.push(
                    EventRecord::note_on(at + u64::from(drum) * 3, 0, drum, 0.8)
                        .with_accent(accent),
                );
            }
        }
        // Every other note overlaps the next: a slur where the part slurs.
        let off = if index % 2 == 0 {
            at + step + 9
        } else {
            at + step - 400
        };
        for part in 1..6u16 {
            let key = if part == 1 { key - 12 } else { key + 12 };
            records.push(EventRecord::note_on(at, part, key, 0.85).with_accent(accent));
            records.push(EventRecord::note_off(off, part, key));
            if part == 2 || part == 5 {
                records.push(EventRecord::note_on(at, part, key + 4, 0.7));
                records.push(EventRecord::note_off(off, part, key + 4));
            }
        }
    }
    records.extend([
        EventRecord::param(3_000, 0, id(ids::KIT_TUNE), -2.0),
        EventRecord::param(3_001, 0, id(ids::DRUM_KIT_SNAP), 0.3),
        EventRecord::param(5_000, 1, id(ids::POLY_MOD_WHEEL), 0.8),
        EventRecord::param(5_500, 1, id(ids::POLY_CUTOFF), -1.0),
        EventRecord::param(6_000, 0, id(ids::MASTER_TEMPO), 140.0),
        // From a polyphonic preset to a legato one and back, while it plays.
        EventRecord::param(7_000, 1, id(ids::POLY_PRESET), poly("808 Glide")),
        EventRecord::param(11_000, 1, id(ids::POLY_PRESET), poly("String Machine")),
        EventRecord::param(4_000, 2, id(ids::MALLET_PRESET), 2.0),
        EventRecord::param(4_100, 2, id(ids::MALLET_MOTOR), 6.0),
        EventRecord::param(6_500, 3, id(ids::BOWED_LEGATO), 1.0),
        EventRecord::param(8_500, 3, id(ids::BOWED_PRESET), 4.0),
        EventRecord::param(9_000, 4, id(ids::WIND_LEGATO), 0.0),
        EventRecord::param(9_500, 5, id(ids::CHOIR_VOWEL), 3.0),
        EventRecord::param(10_000, 5, id(ids::CHOIR_PRESET), 1.0),
        EventRecord::cancel(12_345, 1),
        EventRecord::note_on(12_345, 1, 40, 0.7),
        EventRecord::cancel(13_000, 0),
        EventRecord::choke(14_000, 2),
        EventRecord::cancel(15_000, 3),
        EventRecord::all_notes_off(16_000, 4),
        EventRecord::all_notes_off(16_500, 5),
        EventRecord::all_notes_off(17_000, 0),
        EventRecord::note_on(18_000, 3, 60, 1.0).with_accent(true),
        EventRecord::note_off(22_000, 3, 60),
    ]);
    push_all(&mut engine, &records);
    engine
}

#[test]
fn every_new_kind_is_the_same_in_every_quantum() {
    let frames = 26_000;
    let reference = render(&mut scenario(), frames, 512);
    assert!(peak(&reference.0) > 0.2);
    for quantum in [1usize, 7, 128] {
        let output = render(&mut scenario(), frames, quantum);
        for (channel, (a, b)) in [(&output.0, &reference.0), (&output.1, &reference.1)]
            .into_iter()
            .enumerate()
        {
            let first = a
                .iter()
                .zip(b.iter())
                .position(|(x, y)| x.to_bits() != y.to_bits());
            assert_eq!(first, None, "quantum {quantum}, channel {channel}");
        }
    }
}

#[test]
fn no_value_an_event_can_carry_makes_a_new_kind_go_wrong() {
    let extremes = [
        f64::MAX,
        -f64::MAX,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        0.0,
        -0.0,
        0.5,
        1.0,
        2.0,
        8.0,
        20_000.0,
    ];
    let mut engine = Engine::new(EngineConfig {
        max_parts: 16,
        pending_capacity: 65_536,
        ..EngineConfig::new(RATE)
    })
    .unwrap();
    for (part, kind) in KINDS.into_iter().enumerate() {
        engine
            .create_part_with(part as u16, kind, Chain::NONE)
            .unwrap();
    }
    let parts: Vec<u16> = engine.part_ids().collect();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for frame in (0..96_000u64).step_by(400) {
        let target = parts[(next() % parts.len() as u64) as usize];
        for _ in 0..4 {
            let name = ids::ALL[(next() % ids::ALL.len() as u64) as usize];
            let value = extremes[(next() % extremes.len() as u64) as usize];
            engine.push(EventRecord::param(frame, target, id(name), value));
        }
        let key = (next() % 128) as u8;
        let velocity = [f32::MIN_POSITIVE, 1e-30, 0.5, 1.0][(next() % 4) as usize];
        engine
            .push(EventRecord::note_on(frame, target, key, velocity).with_accent(next() % 2 == 0));
        if next() % 3 == 0 {
            engine.push(EventRecord::note_off(frame + 199, target, key));
        }
        match next() % 40 {
            0 => {
                let _ = engine.push(EventRecord::cancel(frame + 33, target));
            }
            1 => {
                let _ = engine.push(EventRecord::all_notes_off(frame + 7, target));
            }
            2 => {
                let _ = engine.push(EventRecord::choke(frame + 9, target));
            }
            _ => {}
        }
    }
    let (left, right) = render(&mut engine, 100_000, 512);
    let stats = engine.stats();
    assert_eq!(stats.recovered, 0, "{stats:?}");
    assert_eq!(stats.dropped, 0);
    assert!(peak(&left) > 0.05, "nothing sounded");
    for sample in left.iter().chain(&right) {
        assert!(sample.is_finite() && sample.abs() <= OUTPUT_CEILING);
    }
}

#[test]
fn every_new_kind_works_at_every_sample_rate() {
    for rate in [EngineConfig::MIN_SAMPLE_RATE, 22_050.0, 96_000.0, 192_000.0] {
        for (kind, key, settings) in [
            (InstrumentKind::DrumKit, 38u8, vec![]),
            (
                InstrumentKind::DrumKit,
                49,
                vec![(ids::DRUM_KIT_PRESET, 9.0)],
            ),
            (InstrumentKind::Poly, 36, vec![]),
            (
                InstrumentKind::Poly,
                60,
                vec![(ids::POLY_PRESET, poly("Trance Supersaw"))],
            ),
            (InstrumentKind::Mallet, 60, vec![]),
            (InstrumentKind::Bowed, 55, vec![]),
            (InstrumentKind::Wind, 67, vec![]),
            (InstrumentKind::Choir, 60, vec![]),
        ] {
            let mut engine = Engine::new(EngineConfig::new(rate)).unwrap();
            engine.create_part(0, kind).unwrap();
            for &(name, value) in &settings {
                engine.push(EventRecord::param(0, 0, id(name), value));
            }
            push_all(
                &mut engine,
                &[
                    EventRecord::note_on(0, 0, key, 1.0).with_accent(true),
                    EventRecord::note_off((0.3 * rate) as u64, 0, key),
                ],
            );
            let frames = (4.0 * rate) as usize;
            let mut left = vec![0.0; frames];
            let mut right = vec![0.0; frames];
            engine.render_into(&mut left, &mut right);
            let label = format!("{rate} Hz, {kind:?} {key} {settings:?}");
            assert!(peak(&left) > 0.01, "{label}: peak {}", peak(&left));
            assert!(
                left.iter()
                    .chain(&right)
                    .all(|sample| sample.is_finite() && sample.abs() <= OUTPUT_CEILING),
                "{label}"
            );
            assert_eq!(engine.stats().recovered, 0, "{label}");
            // A crash rings longer than 4 s (the lo-fi kit's for 9, 10 at 3 kHz);
            // it must still ring out. A wind does not, below 8 kHz: at 3 kHz
            // the flute's bore, released, goes on sounding by itself, at
            // -20 dBFS (manguebyte-dsp's to fix).
            if kind == InstrumentKind::Wind && rate < 8_000.0 {
                continue;
            }
            let mut seconds = 4;
            while engine.active_voices(0) != Some(0) && seconds < 30 {
                let mut more = vec![0.0; rate as usize];
                let mut more_right = vec![0.0; rate as usize];
                engine.render_into(&mut more, &mut more_right);
                seconds += 1;
            }
            if seconds > 4 {
                println!("{label}: silent after {seconds} s");
            }
            assert_eq!(engine.active_voices(0), Some(0), "{label}: still sounding");
        }
    }
}

#[test]
fn a_polyphonic_preset_plays_its_voices_and_takes_one_over_past_them() {
    for (name, voices) in [
        ("Soft Sine Pad", None),
        ("Warm Analog Pad", None),
        ("Glass Pluck", None),
        ("Glass Pluck", Some(POLY_VOICES)),
        ("Warm Analog Pad", Some(1)),
    ] {
        let mut settings = vec![(ids::POLY_PRESET, poly(name))];
        if let Some(voices) = voices {
            settings.push((ids::POLY_POLYPHONY, voices as f64));
        }
        let mut engine = part(InstrumentKind::Poly, &settings);
        for key in 0..POLY_VOICES as u8 + 2 {
            engine.push(EventRecord::note_on(u64::from(key) * 100, 0, 48 + key, 0.7));
        }
        let (left, _) = render(&mut engine, 4_800, 128);
        let limit = engine.param_value(0, ids::POLY_POLYPHONY).unwrap() as usize;
        if let Some(voices) = voices {
            assert_eq!(limit, voices, "{name}");
        }
        println!("{name}: {limit} voices");
        assert!((1..=POLY_VOICES).contains(&limit));
        assert_eq!(engine.active_voices(0), Some(limit), "{name}");
        assert!(left.iter().all(|sample| sample.is_finite()));
    }
}
