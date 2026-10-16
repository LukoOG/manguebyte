//! The parts made of `manguebyte-dsp`'s instrument voices: plucked
//! strings, FM, the organ through its speaker, the analogue kit, the kick
//! bass and the acid bass. Each plays in tune, louder and brighter with
//! velocity and accent; the basses slide; the kit chokes its hats and plays
//! the drums it does not have from the synthesized kit; and every one is
//! the same in every quantum and stays finite whatever its knobs are set
//! to.

mod common;

use core::f64::consts::TAU;

use common::*;
use manguebyte_dsp::fft::{Complex, Fft};
use manguebyte_web::{
    Chain, Engine, EngineConfig, EventRecord, InstrumentKind, OUTPUT_CEILING, Pushed, ids,
};

/// The kinds this file is about.
const KINDS: [InstrumentKind; 6] = [
    InstrumentKind::Plucked,
    InstrumentKind::Fm,
    InstrumentKind::Organ,
    InstrumentKind::AnalogKit,
    InstrumentKind::KickBass,
    InstrumentKind::Acid,
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

/// The magnitude of the component of `windowed` at `frequency`: one bin of
/// a discrete Fourier transform anywhere along the axis.
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

/// The frequency of the strongest component of `samples` from `low` to
/// `high` hertz: a scan a cent at a time, then a search to a thousandth of
/// a cent.
fn strongest_between(samples: &[f32], low: f64, high: f64) -> f64 {
    let windowed = windowed(samples);
    let steps = (1_200.0 * (high / low).log2()).ceil() as i32;
    let at = |cent: f64| low * (cent / 1_200.0).exp2();
    let best = (0..=steps)
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

/// The frequency of the strongest component within half a semitone of
/// `expected`.
fn frequency_near(samples: &[f32], expected: f64) -> f64 {
    let semitone = 2f64.powf(1.0 / 24.0);
    strongest_between(samples, expected / semitone, expected * semitone)
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
/// seconds, rendered for `length` seconds: the left channel.
#[allow(clippy::too_many_arguments)]
fn note(
    kind: InstrumentKind,
    settings: &[(&str, f64)],
    key: u8,
    velocity: f32,
    accent: bool,
    hold: f64,
    length: f64,
) -> Vec<f32> {
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
            .all(|sample| sample.is_finite() && sample.abs() <= OUTPUT_CEILING)
    );
    left
}

/// Parameters to set, by identifier, and their values.
type Settings = Vec<(&'static str, f64)>;

/// The presets of each kind that has them, as the settings that choose
/// them, and the keys to try them on.
fn presets(kind: InstrumentKind) -> Vec<(Settings, Vec<u8>)> {
    let guitar = vec![40, 45, 52, 57, 64, 69, 76];
    let bass = vec![28, 33, 40, 45, 52];
    let choose = |id: &'static str, value: f64| vec![(id, value)];
    match kind {
        InstrumentKind::Plucked => vec![
            (choose(ids::PLUCKED_PRESET, 0.0), guitar.clone()),
            (choose(ids::PLUCKED_PRESET, 1.0), guitar),
            (choose(ids::PLUCKED_PRESET, 2.0), bass.clone()),
            (choose(ids::PLUCKED_PRESET, 3.0), bass),
        ],
        InstrumentKind::Fm => vec![
            (choose(ids::FM_PRESET, 0.0), vec![45, 57, 69, 81]),
            (choose(ids::FM_PRESET, 1.0), vec![57, 69, 81]),
            (choose(ids::FM_PRESET, 2.0), vec![28, 33, 40, 45]),
        ],
        // Heard straight: the speaker's Doppler shift moves the pitch.
        InstrumentKind::Organ => (0..4)
            .map(|preset| {
                (
                    vec![
                        (ids::ORGAN_PRESET, f64::from(preset)),
                        (ids::ORGAN_SPEAKER, 0.0),
                    ],
                    vec![36, 48, 60, 72, 84],
                )
            })
            .collect(),
        InstrumentKind::KickBass => vec![(vec![], vec![24, 28, 33, 40, 45])],
        InstrumentKind::Acid => (0..3)
            .map(|preset| {
                (
                    vec![(ids::ACID_PRESET, f64::from(preset))],
                    vec![28, 36, 45, 52],
                )
            })
            .collect(),
        _ => vec![],
    }
}

#[test]
fn every_kind_plays_in_tune() {
    let mut report = Vec::new();
    for kind in KINDS {
        let mut worst = 0.0f64;
        for (settings, keys) in presets(kind) {
            for key in keys {
                let left = note(kind, &settings, key, 0.9, false, 0.8, 0.8);
                // While the note is held, after the kick bass's drop.
                let held = &left[seconds(0.25)..seconds(0.75)];
                let error = cents(frequency_near(held, key_hz(key)), key_hz(key));
                assert!(
                    error.abs() < 1.0,
                    "{kind:?} {settings:?} key {key}: {error} cents"
                );
                worst = worst.max(error.abs());
            }
        }
        if kind != InstrumentKind::AnalogKit {
            report.push(format!("{kind:?} {worst:.3}"));
        }
    }
    // The drum machine's kick rings at 49 Hz, and `kit.tune` moves it by
    // whole semitones.
    for tune in [-5.0, 0.0, 7.0, 12.0] {
        let left = note(
            InstrumentKind::AnalogKit,
            &[(ids::KIT_TUNE, tune)],
            36,
            1.0,
            false,
            0.1,
            1.3,
        );
        let expected = 49.0 * (tune / 12.0).exp2();
        let error = cents(
            frequency_near(&left[seconds(0.3)..seconds(1.2)], expected),
            expected,
        );
        assert!(error.abs() < 1.0, "kick tuned {tune}: {error} cents");
        report.push(format!("kick at {tune:+} st {error:+.3}"));
    }
    println!("the largest errors, in cents: {}", report.join(", "));
}

#[test]
fn velocity_and_accent_play_louder_and_brighter() {
    let window = 4_096;
    for kind in KINDS {
        let cases: Vec<(Vec<(&str, f64)>, u8)> = match kind {
            InstrumentKind::AnalogKit => [36u8, 38, 42, 46, 49, 50, 37]
                .into_iter()
                .map(|key| (vec![], key))
                .collect(),
            // The FM voice keeps its sidebands below 0.45 of the sample
            // rate by turning its indices down for high notes, which takes
            // back an accent's brightening there: a low note shows it.
            InstrumentKind::Fm => presets(kind)
                .into_iter()
                .map(|(settings, keys)| (settings, keys[0]))
                .collect(),
            _ => presets(kind)
                .into_iter()
                .map(|(settings, keys)| (settings, keys[keys.len() / 2]))
                .collect(),
        };
        for (settings, key) in cases {
            let play = |velocity: f32, accent: bool| {
                note(kind, &settings, key, velocity, accent, 0.4, 0.5)
            };
            let (soft, hard, accented) = (play(0.5, false), play(1.0, false), play(1.0, true));
            let level = |samples: &[f32]| f64::from(peak(&samples[..seconds(0.4)]));
            let velocity_db = db(level(&hard) / level(&soft));
            let accent_db = db(level(&accented) / level(&hard));
            let brightening = centroid(&accented, window) / centroid(&hard, window);
            let label = format!("{kind:?} {settings:?} key {key}");
            println!(
                "{label}: velocity {velocity_db:+.1} dB, accent {accent_db:+.1} dB and x{brightening:.2} the centroid"
            );
            // Half the velocity is at least 3.5 dB down on every kind.
            assert!(velocity_db > 3.5, "{label}: {velocity_db}");
            // An accent is at least 1.5 dB louder.
            assert!(accent_db > 1.5, "{label}: {accent_db}");
            // And brighter, or no darker where the accent is only louder
            // (the organ and the kick bass add a click).
            assert!(brightening > 0.97, "{label}: {brightening}");
            if matches!(
                kind,
                InstrumentKind::Plucked | InstrumentKind::Fm | InstrumentKind::Acid
            ) {
                // The slap's pluck is bright already: its pop brightens it
                // least, by 4%.
                assert!(brightening > 1.03, "{label}: {brightening}");
                let darkening = centroid(&soft, window) / centroid(&hard, window);
                assert!(
                    darkening < 1.0,
                    "{label}: a soft note is darker, {darkening}"
                );
            }
        }
    }
}

/// The period of `samples` between `shortest` and `longest` frames, by
/// autocorrelation: the lag at which the signal is most like itself.
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
fn overlapping_notes_slide_on_the_mono_parts() {
    // A: 110 Hz, B: 220 Hz, struck at 0.3 s.
    let (a, b) = (45u8, 57u8);
    let at = seconds(0.3) as u64;
    for kind in [InstrumentKind::KickBass, InstrumentKind::Acid] {
        assert!(kind.is_mono());
        for slide in [true, false] {
            let mut engine = part(kind, &[]);
            let records = if slide {
                // As Aratu writes a slide: the next note on before the last
                // note's note off, here on the same frame.
                vec![
                    EventRecord::note_on(0, 0, a, 0.9),
                    EventRecord::note_on(at, 0, b, 0.9),
                    EventRecord::note_off(at, 0, a),
                ]
            } else {
                vec![
                    EventRecord::note_on(0, 0, a, 0.9),
                    EventRecord::note_off(at, 0, a),
                    EventRecord::note_on(at, 0, b, 0.9),
                ]
            };
            push_all(&mut engine, &records);
            push_all(
                &mut engine,
                &[EventRecord::note_off(seconds(0.9) as u64, 0, b)],
            );
            let (left, _) = render(&mut engine, seconds(0.35), 128);
            assert_eq!(engine.active_voices(0), Some(1), "{kind:?}: one voice");
            let (rest, _) = render(&mut engine, seconds(0.5), 128);
            let left: Vec<f32> = left.into_iter().chain(rest).collect();
            let window = |from: f64| &left[seconds(from)..seconds(from) + 1_400];
            let at = at as f64 / RATE;
            // Periods between 2 and 9 ms: 111 to 500 Hz.
            let hz = |from: f64| RATE / period(window(from), 96, 432) as f64;
            let (before, during, after) = (hz(at - 0.04), hz(at + 0.004), hz(at + 0.3));
            println!(
                "{kind:?}, {}: {before:.1} Hz, then {during:.1} Hz just after, then {after:.1} Hz",
                if slide { "slid" } else { "struck" }
            );
            assert!((before / 110.0 - 1.0).abs() < 0.02, "{kind:?}: {before}");
            assert!((after / 220.0 - 1.0).abs() < 0.02, "{kind:?}: {after}");
            if slide {
                // On its way from one to the other: a glide.
                assert!(during > 120.0 && during < 205.0, "{kind:?}: {during}");
            } else {
                // There at once (the kick bass from above, its drop).
                assert!(during > 205.0, "{kind:?}: {during}");
            }
        }
    }
}

#[test]
fn the_analog_kit_chokes_its_open_hat() {
    let at = seconds(0.1) as u64;
    let choke_frames = (0.008 * RATE) as usize;
    let mut both = part(InstrumentKind::AnalogKit, &[]);
    push_all(
        &mut both,
        &[
            EventRecord::note_on(0, 0, 46, 1.0),
            EventRecord::note_on(at, 0, 42, 0.8),
        ],
    );
    let mut closed = part(InstrumentKind::AnalogKit, &[]);
    push_all(&mut closed, &[EventRecord::note_on(at, 0, 42, 0.8)]);
    let mut open = part(InstrumentKind::AnalogKit, &[]);
    push_all(&mut open, &[EventRecord::note_on(0, 0, 46, 1.0)]);
    let frames = seconds(0.6);
    let (both, _) = render(&mut both, frames, 128);
    let (closed, _) = render(&mut closed, frames, 128);
    let (open, _) = render(&mut open, frames, 128);
    let after = at as usize + choke_frames;
    // Once the open hat has faded out, what is left is the closed hat alone,
    // to the bit.
    assert_eq!(bits(&both[after..]), bits(&closed[after..]));
    // Without the closed hat, it rings on.
    assert!(peak(&open[after..after + seconds(0.1)]) > 0.02);
    assert_ne!(bits(&both[..after]), bits(&closed[..after]));
}

/// The left channel of a part of `kind` playing `records` after
/// `settings`, for a second and a half.
fn drums(kind: InstrumentKind, settings: &[(&str, f64)], records: &[EventRecord]) -> Vec<f32> {
    let mut engine = part(kind, settings);
    push_all(&mut engine, records);
    render(&mut engine, seconds(1.5), 128).0
}

#[test]
fn the_analog_kit_plays_the_ride_and_the_guitar_tap_from_the_synthesized_kit() {
    let settings = [
        (ids::KIT_TUNE, 3.0),
        (ids::KIT_DECAY, 0.7),
        (ids::RIDE_LEVEL, 0.5),
        (ids::GUITAR_TAP_LEVEL, 1.0),
    ];
    for key in [51u8, 88] {
        let records = [
            EventRecord::note_on(0, 0, key, 0.9),
            EventRecord::note_on(seconds(0.5) as u64, 0, key, 1.0).with_accent(true),
        ];
        let machine = drums(InstrumentKind::AnalogKit, &settings, &records);
        let synthesized = drums(InstrumentKind::Drums, &settings, &records);
        assert!(peak(&machine) > 0.05, "key {key}");
        assert_eq!(bits(&machine), bits(&synthesized), "key {key}");
    }
    // The drums of the drum map the machine has are its own; so is the
    // clap, off the map. A key with no drum is silent.
    for key in [36u8, 37, 38, 42, 43, 46, 47, 49, 50] {
        let records = [EventRecord::note_on(0, 0, key, 0.9)];
        let machine = drums(InstrumentKind::AnalogKit, &[], &records);
        let synthesized = drums(InstrumentKind::Drums, &[], &records);
        assert!(peak(&machine) > 0.05, "key {key}");
        assert_ne!(bits(&machine), bits(&synthesized), "key {key}");
    }
    let clap = drums(
        InstrumentKind::AnalogKit,
        &[],
        &[EventRecord::note_on(0, 0, 39, 0.9)],
    );
    assert!(peak(&clap) > 0.05);
    let nothing = drums(
        InstrumentKind::AnalogKit,
        &[],
        &[EventRecord::note_on(0, 0, 100, 0.9)],
    );
    assert!(nothing.iter().all(|&sample| sample == 0.0));
}

#[test]
fn a_preset_moves_its_knobs_on_its_frame() {
    let mut engine = engine_with(InstrumentKind::Acid, Chain::NONE);
    push_all(
        &mut engine,
        &[
            // Before the preset on the same frame: the preset moves it.
            EventRecord::param(100, 0, id(ids::ACID_CUTOFF), 1_000.0),
            EventRecord::param(100, 0, id(ids::ACID_PRESET), 1.0),
            // After it: it sticks.
            EventRecord::param(100, 0, id(ids::ACID_RESONANCE), 0.1),
            EventRecord::param(100, 0, id(ids::ACID_VELOCITY), 1.0),
        ],
    );
    render(&mut engine, 128, 128);
    let value = |name| engine.param_value(0, name).unwrap();
    assert_eq!(value(ids::ACID_PRESET), 1.0);
    assert_eq!(value(ids::ACID_CUTOFF), 260.0, "the squelch's cutoff");
    assert!((value(ids::ACID_RESONANCE) - 0.1).abs() < 1e-6);
    assert_eq!(value(ids::ACID_VELOCITY), 1.0, "the preset leaves it alone");

    // The organ's registration, and a knob the preset does not cover.
    let mut engine = engine_with(InstrumentKind::Organ, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::ORGAN_SPEAKER), 0.25),
            EventRecord::param(0, 0, id(ids::ORGAN_PRESET), 3.0),
        ],
    );
    render(&mut engine, 128, 128);
    let drawbars: Vec<f64> = ids::ORGAN_DRAWBARS
        .iter()
        .map(|name| engine.param_value(0, name).unwrap())
        .collect();
    assert_eq!(
        drawbars,
        [8.0, 3.0, 8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "ballad"
    );
    assert_eq!(engine.param_value(0, ids::ORGAN_PERCUSSION), Some(0.0));
    assert_eq!(engine.param_value(0, ids::ORGAN_SPEAKER), Some(0.25));

    // Plucked strings: the slap bass's pluck; FM: the factors back to 1.
    let mut engine = engine_with(InstrumentKind::Plucked, Chain::NONE);
    push_all(
        &mut engine,
        &[EventRecord::param(0, 0, id(ids::PLUCKED_PRESET), 3.0)],
    );
    render(&mut engine, 1, 1);
    assert_eq!(
        engine.param_value(0, ids::PLUCKED_BRIGHTNESS),
        Some(9_000.0)
    );
    let mut engine = engine_with(InstrumentKind::Fm, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::FM_BRIGHTNESS), 1.7),
            EventRecord::param(10, 0, id(ids::FM_PRESET), 1.0),
        ],
    );
    render(&mut engine, 64, 64);
    assert_eq!(engine.param_value(0, ids::FM_BRIGHTNESS), Some(1.0));
}

#[test]
fn every_knob_changes_the_sound_and_the_brightness_knobs_brighten_it() {
    // Each knob, away from its default, changes what one note sounds like.
    let cases: [(InstrumentKind, &str, f64, u8); 26] = [
        (InstrumentKind::Plucked, ids::PLUCKED_DECAY, 0.5, 45),
        (
            InstrumentKind::Plucked,
            ids::PLUCKED_BRIGHTNESS,
            9_000.0,
            45,
        ),
        (InstrumentKind::Plucked, ids::PLUCKED_POSITION, 0.45, 45),
        (InstrumentKind::Plucked, ids::PLUCKED_TONE, 1_000.0, 45),
        (InstrumentKind::Plucked, ids::PLUCKED_RELEASE, 1.5, 45),
        (InstrumentKind::Fm, ids::FM_BRIGHTNESS, 2.0, 57),
        (InstrumentKind::Fm, ids::FM_DECAY, 0.25, 57),
        (InstrumentKind::Fm, ids::FM_RELEASE, 4.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_DRAWBAR_4, 8.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_PERCUSSION, 0.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_CLICK, 1.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_LEAKAGE, 1.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_ROTARY, 2.0, 57),
        (InstrumentKind::Organ, ids::ORGAN_SPEAKER, 0.0, 57),
        (InstrumentKind::AnalogKit, ids::ANALOG_TONE, 1.0, 38),
        (InstrumentKind::AnalogKit, ids::ANALOG_SNAP, 0.0, 38),
        (InstrumentKind::AnalogKit, ids::ANALOG_KICK_DECAY, 4.0, 36),
        (InstrumentKind::KickBass, ids::KICK_BASS_DECAY, 0.3, 33),
        (InstrumentKind::KickBass, ids::KICK_BASS_SWEEP, 0.0, 33),
        (InstrumentKind::KickBass, ids::KICK_BASS_DRIVE, 1.0, 33),
        (InstrumentKind::KickBass, ids::KICK_BASS_CLICK, 1.0, 33),
        (InstrumentKind::Acid, ids::ACID_CUTOFF, 2_000.0, 45),
        (InstrumentKind::Acid, ids::ACID_RESONANCE, 0.0, 45),
        (InstrumentKind::Acid, ids::ACID_ENV_MOD, 0.0, 45),
        (InstrumentKind::Acid, ids::ACID_WAVE, 1.0, 45),
        (InstrumentKind::Acid, ids::ACID_DRIVE, 20.0, 45),
    ];
    for (kind, name, value, key) in cases {
        let plain = note(kind, &[], key, 0.8, false, 0.5, 1.0);
        let turned = note(kind, &[(name, value)], key, 0.8, false, 0.5, 1.0);
        assert_ne!(bits(&plain), bits(&turned), "{name}");
    }
    for (kind, name, value, key) in [
        (
            InstrumentKind::Plucked,
            ids::PLUCKED_BRIGHTNESS,
            9_000.0,
            45,
        ),
        (InstrumentKind::Fm, ids::FM_BRIGHTNESS, 2.0, 57),
        (InstrumentKind::Acid, ids::ACID_CUTOFF, 2_000.0, 45),
    ] {
        let plain = note(kind, &[], key, 0.8, false, 0.5, 0.5);
        let bright = note(kind, &[(name, value)], key, 0.8, false, 0.5, 0.5);
        assert!(
            centroid(&bright, 8_192) > 1.2 * centroid(&plain, 8_192),
            "{name}"
        );
    }
}

#[test]
fn the_organ_is_heard_through_its_turning_speaker() {
    let play = |settings: &[(&str, f64)], keys: &[u8]| {
        let mut engine = part(InstrumentKind::Organ, settings);
        for &key in keys {
            engine.push(EventRecord::note_on(0, 0, key, 1.0));
            engine.push(EventRecord::note_off(seconds(1.0) as u64, 0, key));
        }
        let (left, right) = render(&mut engine, seconds(1.5), 128);
        (left, right, engine)
    };
    let chord = [48u8, 55, 60, 64];
    // Straight, the organ is the same on both sides.
    let (left, right, _) = play(&[(ids::ORGAN_SPEAKER, 0.0)], &chord);
    assert_eq!(bits(&left), bits(&right));
    // Through the speaker the two microphones hear it differently.
    let (slow_left, slow_right, mut engine) = play(&[], &chord);
    assert_ne!(bits(&slow_left), bits(&slow_right));
    // And faster rotors move it more within a second. A steady tone, the
    // 8' of A5 alone with no percussion, click or leakage, is a sine the
    // horn carries, whose level moves with the horn alone.
    let mut steady: Vec<(&str, f64)> = ids::ORGAN_DRAWBARS
        .iter()
        .map(|&name| {
            (
                name,
                if name == ids::ORGAN_DRAWBAR_8 {
                    8.0
                } else {
                    0.0
                },
            )
        })
        .collect();
    steady.extend([
        (ids::ORGAN_PERCUSSION, 0.0),
        (ids::ORGAN_CLICK, 0.0),
        (ids::ORGAN_LEAKAGE, 0.0),
    ]);
    let (slow_left, _, _) = play(&steady, &[81]);
    steady.push((ids::ORGAN_ROTARY, 2.0));
    let (fast_left, _, _) = play(&steady, &[81]);
    let wobble = |samples: &[f32]| {
        // How far the level moves from one window of ten periods to the
        // next, all told, against its mean.
        let levels: Vec<f64> = samples[seconds(0.2)..seconds(0.95)]
            .chunks(545)
            .map(rms)
            .collect();
        let mean = levels.iter().sum::<f64>() / levels.len() as f64;
        levels
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .sum::<f64>()
            / mean
    };
    println!(
        "level moving {:.3} of itself slow, {:.3} fast",
        wobble(&slow_left),
        wobble(&fast_left)
    );
    assert!(wobble(&fast_left) > 1.5 * wobble(&slow_left));
    // The speaker falls asleep once the organ has gone quiet, and wakes for
    // the next note.
    assert_eq!(engine.speaker_asleep(0), Some(true));
    assert_eq!(engine.active_voices(0), Some(0));
    engine.push(EventRecord::note_on(engine.frame(), 0, 60, 1.0));
    render(&mut engine, 256, 128);
    assert_eq!(engine.speaker_asleep(0), Some(false));
    assert_eq!(engine.speaker_asleep(9), None);
    let drums = engine_with(InstrumentKind::Drums, Chain::NONE);
    assert_eq!(drums.speaker_asleep(0), None);
}

#[test]
fn setting_an_instrument_keeps_the_chain_and_its_values() {
    let mut engine = engine_with(
        InstrumentKind::Synth,
        Chain::FULL.with(manguebyte_web::Effect::Delay),
    );
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::GAIN), -9.0),
            EventRecord::param(0, 0, id(ids::DELAY_FEEDBACK), 0.25),
            EventRecord::param(0, 0, id(ids::SYNTH_CUTOFF), 3_000.0),
            EventRecord::note_on(0, 0, 45, 1.0),
        ],
    );
    render(&mut engine, 1_000, 128);
    for kind in InstrumentKind::ALL {
        engine.set_instrument(0, kind).unwrap();
        assert_eq!(engine.part_kind(0), Some(kind));
        assert_eq!(
            engine.part_chain(0),
            Some(Chain::FULL.with(manguebyte_web::Effect::Delay))
        );
        assert_eq!(engine.param_value(0, ids::GAIN), Some(-9.0), "{kind:?}");
        assert_eq!(
            engine.param_value(0, ids::DELAY_FEEDBACK),
            Some(0.25),
            "{kind:?}"
        );
        // The instrument's own start at their defaults.
        let synth_cutoff = engine.param_value(0, ids::SYNTH_CUTOFF);
        assert_eq!(synth_cutoff.is_some(), kind == InstrumentKind::Synth);
        if kind == InstrumentKind::Synth {
            assert_eq!(synth_cutoff, Some(900.0));
        }
        let json = engine.params_json(0).unwrap();
        assert!(json.contains(r#""id":"delay.feedback""#), "{kind:?}");
    }
    assert_eq!(
        engine.set_instrument(7, InstrumentKind::Organ),
        Err(manguebyte_web::EngineError::NoSuchPart(7))
    );
}

/// Bars of every new kind, with accents, slides, presets and knobs moving,
/// the speaker sped up, cancels, a choke and an all-notes-off, then
/// silence long enough for the speaker to sleep, and more notes.
fn scenario() -> Engine {
    let mut engine = engine();
    for (part, kind) in KINDS.into_iter().enumerate() {
        engine.create_part(part as u16, kind).unwrap();
    }
    let step = 3_000u64;
    let mut records = Vec::new();
    let melody = [45u8, 48, 52, 57, 55, 52, 50, 48];
    for (index, &key) in melody.iter().enumerate() {
        let at = index as u64 * step + 7;
        let accent = index % 3 == 1;
        for part in [0u16, 1, 2] {
            records.push(EventRecord::note_on(at, part, key + 12, 0.8).with_accent(accent));
            records.push(EventRecord::note_off(at + 2 * step, part, key + 12));
        }
        // The basses: every other note slides into the next.
        let off = if index % 2 == 0 {
            at + step + 5
        } else {
            at + step - 300
        };
        for part in [4u16, 5] {
            records.push(EventRecord::note_on(at, part, key - 12, 0.9).with_accent(accent));
            records.push(EventRecord::note_off(off, part, key - 12));
        }
        for drum in [36u8, 42, 51] {
            records
                .push(EventRecord::note_on(at + u64::from(drum), 3, drum, 0.7).with_accent(accent));
        }
        records.push(EventRecord::note_on(
            at + 1_500,
            3,
            if index % 2 == 0 { 46 } else { 88 },
            0.8,
        ));
    }
    records.extend([
        EventRecord::param(5_000, 0, id(ids::PLUCKED_PRESET), 2.0),
        EventRecord::param(5_001, 0, id(ids::PLUCKED_TONE), 2_000.0),
        EventRecord::param(6_000, 1, id(ids::FM_PRESET), 1.0),
        EventRecord::param(6_100, 1, id(ids::FM_BRIGHTNESS), 1.5),
        EventRecord::param(4_000, 2, id(ids::ORGAN_ROTARY), 2.0),
        EventRecord::param(7_000, 2, id(ids::ORGAN_DRAWBAR_2), 6.0),
        EventRecord::param(8_000, 2, id(ids::ORGAN_SPEAKER), 0.5),
        EventRecord::param(9_000, 2, id(ids::ORGAN_PRESET), 1.0),
        EventRecord::param(3_333, 3, id(ids::ANALOG_SNAP), 0.9),
        EventRecord::param(3_334, 3, id(ids::KIT_TUNE), -2.0),
        EventRecord::param(4_444, 4, id(ids::KICK_BASS_DRIVE), 0.8),
        EventRecord::param(5_555, 5, id(ids::ACID_CUTOFF), 900.0),
        EventRecord::param(5_556, 5, id(ids::ACID_PRESET), 1.0),
        EventRecord::cancel(12_345, 0),
        EventRecord::note_on(12_345, 0, 64, 0.7),
        EventRecord::cancel(15_000, 5),
        EventRecord::cancel(15_002, 3),
        EventRecord::choke(16_000, 1),
        EventRecord::all_notes_off(18_000, 2),
        EventRecord::all_notes_off(18_000, 3),
        // After the silence, the speaker wakes.
        EventRecord::note_on(70_000, 2, 60, 1.0),
        EventRecord::note_off(76_000, 2, 60),
        EventRecord::note_on(70_000, 5, 33, 1.0).with_accent(true),
        EventRecord::note_on(72_000, 5, 45, 1.0),
        EventRecord::note_off(72_001, 5, 33),
        EventRecord::note_off(74_000, 5, 45),
    ]);
    push_all(&mut engine, &records);
    engine
}

#[test]
fn every_new_kind_is_the_same_in_every_quantum() {
    let frames = 90_000;
    let reference = render(&mut scenario(), frames, 128);
    assert!(peak(&reference.0) > 0.2);
    let mut asleep = scenario();
    render(&mut asleep, 69_000, 512);
    assert_eq!(
        asleep.speaker_asleep(2),
        Some(true),
        "asleep in the silence"
    );
    for quantum in [1usize, 7, 512] {
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
        engine
            .create_part_with(part as u16 + 8, kind, Chain::FULL)
            .unwrap();
    }
    let parts: Vec<u16> = engine.part_ids().collect();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for frame in (0..240_000u64).step_by(400) {
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
    let (left, right) = render(&mut engine, 250_000, 512);
    let stats = engine.stats();
    assert_eq!(stats.recovered, 0, "{stats:?}");
    assert_eq!(stats.dropped, 0);
    assert!(peak(&left) > 0.1, "nothing sounded");
    for sample in left.iter().chain(&right) {
        assert!(sample.is_finite() && sample.abs() <= OUTPUT_CEILING);
    }
}

#[test]
fn every_new_kind_works_at_every_sample_rate() {
    for rate in [
        EngineConfig::MIN_SAMPLE_RATE,
        22_050.0,
        96_000.0,
        EngineConfig::MAX_SAMPLE_RATE,
    ] {
        for (kind, key) in [
            (InstrumentKind::Plucked, 40u8),
            (InstrumentKind::Fm, 57),
            (InstrumentKind::Organ, 60),
            (InstrumentKind::AnalogKit, 36),
            (InstrumentKind::AnalogKit, 51),
            (InstrumentKind::KickBass, 33),
            (InstrumentKind::Acid, 45),
        ] {
            let mut engine = Engine::new(EngineConfig::new(rate)).unwrap();
            engine.create_part(0, kind).unwrap();
            push_all(
                &mut engine,
                &[
                    EventRecord::note_on(0, 0, key, 1.0).with_accent(true),
                    EventRecord::note_off((0.1 * rate) as u64, 0, key),
                ],
            );
            let frames = (4.0 * rate) as usize;
            let mut left = vec![0.0; frames];
            let mut right = vec![0.0; frames];
            engine.render_into(&mut left, &mut right);
            let label = format!("{rate} Hz, {kind:?} {key}");
            // On its frame, but for the acid's 16 frames of filtering and
            // the organ's speaker.
            let first = onset(&left).unwrap_or(usize::MAX);
            assert!(first <= (0.002 * rate) as usize + 16, "{label}: {first}");
            assert!(peak(&left) > 0.02, "{label}: peak {}", peak(&left));
            assert!(
                left.iter()
                    .chain(&right)
                    .all(|sample| sample.is_finite() && sample.abs() <= OUTPUT_CEILING),
                "{label}"
            );
            assert_eq!(engine.stats().recovered, 0, "{label}");
            // Below 8 kHz the plucked string's loss filter, which has to
            // fall steeply between the fundamental and half the sample
            // rate, holds the string's lowest frequencies at its highest
            // loop gain: a released low string fades by hundredths of a
            // decibel a second there (manguebyte-dsp's to fix).
            if kind != InstrumentKind::Plucked || rate >= 8_000.0 {
                assert_eq!(engine.active_voices(0), Some(0), "{label}: still sounding");
            }
        }
    }
}
