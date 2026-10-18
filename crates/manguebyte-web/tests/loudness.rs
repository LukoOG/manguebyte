//! Every kind of instrument, and every preset, plays its standard phrase
//! at the loudness target, and no single note of any of them, accented at
//! full velocity, reaches the master limiter.
//!
//! Loudness is integrated loudness after ITU-R BS.1770-4, read by the
//! meter of `manguebyte-analysis`, on the part's stereo output with its
//! chain neutral and the master limiter out of the way: the master gain is
//! turned down by [`MASTER_DB`] and the readings put back up by as much,
//! and every measurement insists that the limiter did nothing.
//!
//! `cargo test -p manguebyte-web --test loudness -- --nocapture` prints
//! the table of `src/level.rs` as measured, with the make-up each row
//! needs: what to put in a new row.

mod common;

use common::*;
use manguebyte_analysis::{LoudnessMeter, TruePeakMeter};
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::ProcessSpec;
use manguebyte_web::{
    Calibration, Chain, DRUM_MAP, Engine, EventRecord, InstrumentKind, LOUDNESS_TARGET_LUFS,
    LOUDNESS_TOLERANCE_LU, Phrase, ids,
};

/// The master gain the measurements run at, so that the limiter is never
/// engaged; the readings are put back up by as much.
const MASTER_DB: f64 = -20.0;

/// Where the measured sound starts: after the master bus has turned its
/// gain down and come to rest.
const LEAD_IN: u64 = 19_200;

/// The ceiling no accented note at full velocity may reach: the master
/// limiter's, by default, in dBTP.
const CEILING_DBTP: f64 = -0.3;

fn frames(seconds: f64) -> u64 {
    (seconds * RATE).round() as u64
}

/// An engine with part 0 of `kind` through no effect, made ready by
/// `setup`, whose master gain is [`MASTER_DB`] from [`LEAD_IN`] on. A gain
/// set while the bus is at rest glides in once sound comes, so a blip on
/// part 1 wakes the bus and lets it glide; it comes to rest again, its gain
/// set, long before the lead-in is over.
fn quiet_engine(kind: InstrumentKind, setup: &dyn Fn(&mut Engine)) -> Engine {
    let mut engine = engine_with(kind, Chain::NONE);
    setup(&mut engine);
    engine
        .create_part_with(1, InstrumentKind::Synth, Chain::NONE)
        .unwrap();
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 1, id(ids::MASTER_GAIN), MASTER_DB),
            EventRecord::param(0, 1, id(ids::SYNTH_RELEASE), 0.005),
            EventRecord::note_on(0, 1, 60, 0.5),
            EventRecord::note_off(96, 1, 60),
        ],
    );
    engine
}

/// The records of `phrase` on part 0, from frame `start`.
fn phrase_records(phrase: Phrase, start: u64) -> Vec<EventRecord> {
    let mut records = Vec::new();
    for note in phrase.notes() {
        let on = start + frames(note.start);
        records.push(EventRecord::note_on(on, 0, note.key, note.velocity));
        records.push(EventRecord::note_off(on + frames(note.length), 0, note.key));
    }
    records
}

/// Renders `seconds` from [`LEAD_IN`] and returns them, insisting that the
/// limiter stayed out.
fn after_lead_in(engine: &mut Engine, seconds: f64) -> AudioBuffer<f32> {
    let length = frames(seconds) as usize;
    let (left, right) = render(engine, LEAD_IN as usize + length, 512);
    assert_eq!(engine.stats().limited, 0, "the limiter stayed out");
    let mut buffer = AudioBuffer::<f32>::new(2, length);
    buffer
        .channel_mut(0)
        .copy_from_slice(&left[LEAD_IN as usize..]);
    buffer
        .channel_mut(1)
        .copy_from_slice(&right[LEAD_IN as usize..]);
    buffer
}

fn spec(frames: usize) -> ProcessSpec {
    ProcessSpec {
        sample_rate: RATE,
        max_block_size: frames,
        num_channels: 2,
    }
}

/// The settings that choose preset `index` of `kind`, if it has presets.
fn preset_records(kind: InstrumentKind, index: usize) -> Vec<EventRecord> {
    match kind.preset_param() {
        Some(preset) => vec![EventRecord::param(0, 0, id(preset), index as f64)],
        None => vec![],
    }
}

/// The integrated loudness, LUFS, of part 0 of `kind` playing the phrase
/// of row `index`, made ready by `setup`.
fn phrase_loudness(kind: InstrumentKind, index: usize, setup: &dyn Fn(&mut Engine)) -> f64 {
    let mut engine = quiet_engine(kind, setup);
    push_all(&mut engine, &preset_records(kind, index));
    push_all(
        &mut engine,
        &phrase_records(kind.calibration()[index].phrase, LEAD_IN),
    );
    let sound = after_lead_in(&mut engine, Phrase::MEASURED_SECONDS);
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(sound.num_frames()));
    meter.measure(&sound);
    meter.readings().integrated() - MASTER_DB
}

/// How long each note of [`accented_peak`] is given.
const PEAK_SPACING: f64 = 0.5;

/// The true peak, dBTP, of the loudest single note of `kind` (preset
/// `index`) accented at full velocity, over the keys of its phrase (and
/// every drum of the map for a kit), each struck alone.
fn accented_peak(kind: InstrumentKind, index: usize, setup: &dyn Fn(&mut Engine)) -> f64 {
    let phrase = kind.calibration()[index].phrase;
    let mut keys: Vec<u8> = phrase.notes().iter().map(|note| note.key).collect();
    if phrase == Phrase::Groove {
        keys.extend(DRUM_MAP.iter().map(|entry| entry.note));
    }
    keys.sort_unstable();
    keys.dedup();
    let mut engine = quiet_engine(kind, setup);
    push_all(&mut engine, &preset_records(kind, index));
    let spacing = frames(PEAK_SPACING);
    for (slot, &key) in keys.iter().enumerate() {
        let at = LEAD_IN + slot as u64 * spacing;
        push_all(
            &mut engine,
            &[
                EventRecord::note_on(at, 0, key, 1.0).with_accent(true),
                EventRecord::note_off(at + spacing / 2, 0, key),
                // Each note alone: what rings on is cut before the next.
                EventRecord::choke(at + spacing - 1, 0),
            ],
        );
    }
    let sound = after_lead_in(&mut engine, keys.len() as f64 * PEAK_SPACING);
    let mut meter = TruePeakMeter::new(2);
    meter.prepare(&spec(sound.num_frames()));
    meter.measure(&sound);
    meter.flush();
    meter.readings().max_overall_db() - MASTER_DB
}

/// A recorded instrument of a kind: for a sampler, two tones like a
/// plucked string's, harmonics dying away, at `amplitude`; for a sampler
/// kit, a kick, a snare and a hat, as recorded, `amplitude` their peak.
fn recorded(kind: InstrumentKind, amplitude: f64) -> (String, Vec<(String, Vec<u8>)>) {
    let rate = 44_100.0;
    let tone = |frequency: f64, frames: usize, decay: f64| -> Vec<f32> {
        let mut samples = vec![0.0f32; frames];
        for (n, sample) in samples.iter_mut().enumerate() {
            let t = n as f64 / rate;
            let mut sum = 0.0;
            for harmonic in 1..=4 {
                let h = f64::from(harmonic);
                sum +=
                    (core::f64::consts::TAU * frequency * h * t).sin() / h * (-t * h / decay).exp();
            }
            *sample = (amplitude / 1.6 * sum) as f32;
        }
        samples
    };
    if kind == InstrumentKind::SamplerKit {
        let mut snare = noise(12_000, amplitude, 11);
        for (n, sample) in snare.iter_mut().enumerate() {
            *sample = (f64::from(*sample) * (-(n as f64) / 2_500.0).exp()) as f32;
        }
        let files = vec![
            (
                "kick.wav".to_owned(),
                wav16(&tone(55.0, 22_050, 0.25), 44_100),
            ),
            ("snare.wav".to_owned(), wav16(&snare, 44_100)),
            (
                "hat.wav".to_owned(),
                wav16(&noise(3_000, amplitude * 0.5, 13), 44_100),
            ),
        ];
        let sfz = "<group> loop_mode=one_shot
            <region> sample=kick.wav key=36
            <region> sample=snare.wav key=38
            <region> sample=hat.wav key=42";
        return (sfz.to_owned(), files);
    }
    let files = vec![
        (
            "a3.wav".to_owned(),
            wav16(&tone(220.0, 66_150, 0.6), 44_100),
        ),
        (
            "a4.wav".to_owned(),
            wav16(&tone(440.0, 66_150, 0.45), 44_100),
        ),
    ];
    let sfz = "<global> ampeg_release=0.3
        <region> sample=a3.wav pitch_keycenter=57 lokey=0 hikey=62
        <region> sample=a4.wav pitch_keycenter=69 lokey=63 hikey=127";
    (sfz.to_owned(), files)
}

/// What a part of `kind` is made ready with before it plays: a sampler
/// kind loads [`recorded`] at -6 dBFS, measured if `measured`.
fn setup_with(kind: InstrumentKind, measured: bool) -> Box<dyn Fn(&mut Engine)> {
    if !kind.is_sampler() {
        return Box::new(|_: &mut Engine| {});
    }
    Box::new(move |engine: &mut Engine| {
        let (sfz, files) = recorded(kind, 0.5);
        if measured {
            load_sfz(engine, 0, &sfz, &files);
        } else {
            load_sfz_unmeasured(engine, 0, &sfz, &files);
        }
    })
}

fn setup_for(kind: InstrumentKind) -> Box<dyn Fn(&mut Engine)> {
    setup_with(kind, true)
}

/// Every row of the calibration table, with its kind and index.
fn rows() -> Vec<(InstrumentKind, usize, Calibration)> {
    InstrumentKind::ALL
        .into_iter()
        .flat_map(|kind| {
            assert!(
                !kind.calibration().is_empty(),
                "{kind:?} has no row in the calibration table"
            );
            kind.calibration()
                .iter()
                .enumerate()
                .map(move |(index, &row)| (kind, index, row))
        })
        .collect()
}

/// `measure` of every row, in the order of the rows, on up to four threads:
/// each row is an engine of its own.
fn each_row<T: Send>(
    rows: &[(InstrumentKind, usize, Calibration)],
    measure: impl Fn(InstrumentKind, usize, &Calibration) -> T + Sync,
) -> Vec<T> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<T>>> = Mutex::new((0..rows.len()).map(|_| None).collect());
    // Four at most: the tests run beside each other.
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(4));
    std::thread::scope(|scope| {
        for _ in 0..threads.min(rows.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((kind, preset, row)) = rows.get(index) else {
                        break;
                    };
                    let value = measure(*kind, *preset, row);
                    results.lock().unwrap()[index] = Some(value);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|value| value.expect("every row measured"))
        .collect()
}

fn label(kind: InstrumentKind, row: &Calibration) -> String {
    match row.preset {
        Some(preset) => format!("{} {preset:?}", kind.name()),
        None => kind.name().to_owned(),
    }
}

/// The table test: every kind and every preset on its phrase, within
/// [`LOUDNESS_TOLERANCE_LU`] of the target. A kind or a preset added
/// without its make-up fails here, saying what the make-up should be.
#[test]
fn every_kind_and_preset_plays_its_phrase_at_the_target() {
    println!(
        "{:<28} {:<7} {:>9} {:>9} {:>9}",
        "kind", "phrase", "before", "make-up", "after"
    );
    let mut misses = Vec::new();
    let rows = rows();
    let measured = each_row(&rows, |kind, index, row| {
        let after = phrase_loudness(kind, index, &*setup_for(kind));
        let make_up = match row.make_up_db {
            Some(make_up) => make_up,
            // A sampler kind, its instrument measured as it loads: the
            // make-up is what that measured, here for a recorded one.
            None => after - phrase_loudness(kind, index, &*setup_with(kind, false)),
        };
        (after, make_up)
    });
    for ((kind, _, row), (after, make_up)) in rows.into_iter().zip(measured) {
        println!(
            "{:<28} {:<7} {:>9.2} {:>+9.2} {:>9.2}",
            label(kind, &row),
            row.phrase.name(),
            after - make_up,
            make_up,
            after
        );
        let off = after - LOUDNESS_TARGET_LUFS;
        if off.abs() > LOUDNESS_TOLERANCE_LU {
            misses.push(match row.make_up_db {
                Some(_) => format!(
                    "{} reads {after:.2} LUFS on its {} phrase, {off:+.2} LU from the \
                     target: its make-up in src/level.rs should be {:+.2} dB",
                    label(kind, &row),
                    row.phrase.name(),
                    make_up - off
                ),
                None => format!(
                    "{}, normalised, reads {after:.2} LUFS on its {} phrase, {off:+.2} LU \
                     from the target",
                    label(kind, &row),
                    row.phrase.name()
                ),
            });
        }
    }
    assert!(misses.is_empty(), "uncalibrated:\n{}", misses.join("\n"));
}

/// A preset brings its make-up gain on the frame it lands on, with its
/// settings: a note struck on that frame plays as it would had the preset
/// been chosen long before, and a change while notes sound is the same in
/// every quantum.
#[test]
fn a_presets_make_up_lands_on_its_frame_in_every_quantum() {
    for kind in InstrumentKind::ALL {
        let Some(preset) = kind.preset_param() else {
            continue;
        };
        let last = (kind.calibration().len() - 1) as f64;
        // A note every preset plays: a snare on the drum kits.
        let key = if kind == InstrumentKind::DrumKit {
            38
        } else {
            52
        };
        let play = |records: &[EventRecord], quantum: usize| {
            let mut engine = engine_with(kind, Chain::NONE);
            push_all(&mut engine, records);
            render(&mut engine, 24_000, quantum).0
        };
        let early = play(
            &[
                EventRecord::param(0, 0, id(preset), last),
                EventRecord::note_on(4_800, 0, key, 0.8),
            ],
            128,
        );
        let with_the_note = play(
            &[
                EventRecord::param(4_800, 0, id(preset), last),
                EventRecord::note_on(4_800, 0, key, 0.8),
            ],
            128,
        );
        assert!(peak(&early) > 0.01, "{kind:?}");
        assert_eq!(bits(&early), bits(&with_the_note), "{kind:?}");

        let changing = [
            EventRecord::note_on(0, 0, 45, 0.8),
            EventRecord::note_on(0, 0, 57, 0.8),
            EventRecord::param(3_001, 0, id(preset), last),
            EventRecord::note_on(5_000, 0, 64, 0.8).with_accent(true),
            EventRecord::param(7_777, 0, id(preset), 1.0),
            EventRecord::param(12_345, 0, id(preset), 0.0),
            EventRecord::all_notes_off(18_000, 0),
        ];
        let reference = play(&changing, 512);
        for quantum in [1, 7, 128, 333] {
            assert_eq!(
                bits(&play(&changing, quantum)),
                bits(&reference),
                "{kind:?} in quanta of {quantum}"
            );
        }
    }
}

/// Peak safety: no kind, at the target, plays a single note accented at
/// full velocity loud enough to reach the master limiter.
#[test]
fn no_accented_note_at_full_velocity_reaches_the_limiter() {
    let mut worst = (f64::NEG_INFINITY, String::new());
    let rows = rows();
    let peaks = each_row(&rows, |kind, index, _| {
        accented_peak(kind, index, &*setup_for(kind))
    });
    for ((kind, _, row), peak) in rows.into_iter().zip(peaks) {
        println!("{:<28} {peak:>7.2} dBTP", label(kind, &row));
        if peak > worst.0 {
            worst = (peak, label(kind, &row));
        }
    }
    println!("worst: {} at {:.2} dBTP", worst.1, worst.0);
    assert!(
        worst.0 < CEILING_DBTP,
        "{} peaks at {:.2} dBTP",
        worst.1,
        worst.0
    );
}

/// The loudness of a part of `kind` playing its phrase, `setup` having
/// loaded it, and the make-up its normalisation measured.
fn normalised(kind: InstrumentKind, setup: &dyn Fn(&mut Engine)) -> (f64, Option<f64>) {
    let mut engine = quiet_engine(kind, setup);
    let make_up = engine.sampler_make_up_db(0);
    push_all(
        &mut engine,
        &phrase_records(kind.calibration()[0].phrase, LEAD_IN),
    );
    let sound = after_lead_in(&mut engine, Phrase::MEASURED_SECONDS);
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&spec(sound.num_frames()));
    meter.measure(&sound);
    (meter.readings().integrated() - MASTER_DB, make_up)
}

/// Two instruments recorded 12 dB apart, the same otherwise, play at the
/// same loudness once normalised, melodic or a kit, and the make-ups the
/// parts measured are 12 dB apart.
#[test]
fn normalisation_brings_instruments_recorded_12_db_apart_together() {
    for kind in [InstrumentKind::Sampler, InstrumentKind::SamplerKit] {
        let at = |amplitude: f64, measured: bool| {
            let (sfz, files) = recorded(kind, amplitude);
            normalised(kind, &move |engine: &mut Engine| {
                if measured {
                    load_sfz(engine, 0, &sfz, &files);
                } else {
                    load_sfz_unmeasured(engine, 0, &sfz, &files);
                }
            })
        };
        let (loud_before, none) = at(0.8, false);
        let (quiet_before, _) = at(0.2, false);
        assert_eq!(none, None, "an instrument loaded unmeasured has no make-up");
        let (loud, loud_make_up) = at(0.8, true);
        let (quiet, quiet_make_up) = at(0.2, true);
        println!(
            "{kind:?}: {loud_before:.2} and {quiet_before:.2} LUFS as recorded, \
             {loud:.2} and {quiet:.2} normalised"
        );
        assert!(
            (loud_before - quiet_before - 12.04).abs() < 0.1,
            "recorded 12 dB apart"
        );
        assert!(
            (loud - quiet).abs() < 1.0,
            "{kind:?}: {loud:.2} against {quiet:.2}"
        );
        for loudness in [loud, quiet] {
            assert!(
                (loudness - LOUDNESS_TARGET_LUFS).abs() < LOUDNESS_TOLERANCE_LU,
                "{kind:?}: {loudness:.2} LUFS"
            );
        }
        let apart = quiet_make_up.unwrap() - loud_make_up.unwrap();
        assert!((apart - 12.04).abs() < 0.1, "{kind:?}: {apart:.2} dB");
    }
}

/// `sampler.normalize` off, the part plays the samples as they are, as if
/// the instrument had not been measured; switched while a note sounds,
/// the level glides over 20 ms, the same in every quantum.
#[test]
fn normalisation_switches_off_and_on_without_a_jump() {
    let (sfz, files) = tone_instrument();
    let off = EventRecord::param(0, 0, id(ids::SAMPLER_NORMALIZE), 0.0);
    let note = EventRecord::note_on(0, 0, 69, 0.8);
    let mut measured = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz(&mut measured, 0, &sfz, &files);
    let make_up = measured.sampler_make_up_db(0).unwrap();
    assert!(
        make_up < -6.0,
        "a loud sine is turned down: {make_up:.2} dB"
    );
    push_all(&mut measured, &[off, note]);
    let mut unmeasured = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz_unmeasured(&mut unmeasured, 0, &sfz, &files);
    push_all(&mut unmeasured, &[note]);
    let (switched_off, _) = render(&mut measured, 9_600, 128);
    let (as_recorded, _) = render(&mut unmeasured, 9_600, 128);
    assert!(peak(&as_recorded) > 0.1);
    assert_eq!(bits(&switched_off), bits(&as_recorded));
    assert_eq!(measured.param_value(0, ids::SAMPLER_NORMALIZE), Some(0.0));

    // Off and on again while the note sounds.
    let switch = 12_000u64;
    let play = |quantum: usize| {
        let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
        load_sfz(&mut engine, 0, &sfz, &files);
        push_all(
            &mut engine,
            &[
                note,
                EventRecord::param(switch, 0, id(ids::SAMPLER_NORMALIZE), 0.0),
                EventRecord::param(switch + 4_800, 0, id(ids::SAMPLER_NORMALIZE), 1.0),
            ],
        );
        render(&mut engine, 24_000, quantum).0
    };
    let reference = play(512);
    for quantum in [1, 7, 128, 333] {
        assert_eq!(
            bits(&play(quantum)),
            bits(&reference),
            "quanta of {quantum}"
        );
    }
    // The sine's crest over a few periods, around the switch.
    let at = |frame: u64| f64::from(peak(&reference[frame as usize..frame as usize + 240]));
    let (before, after) = (at(switch - 480), at(switch + 1_440));
    let expected = 10f64.powf(-make_up / 20.0);
    assert!(
        (after / before - expected).abs() < 0.01 * expected,
        "{} against {expected}",
        after / before
    );
    // Part of the way through the glide, part of the way there; no step at
    // the switch larger than those of the louder sound after it.
    let middle = at(switch + 360) / before;
    assert!(middle > 1.2 && middle < expected - 0.2, "{middle}");
    let steps = |from: u64| largest_step(&reference[from as usize..from as usize + 960]);
    assert!(steps(switch) <= steps(switch + 960) * 1.01);
}

/// The largest error [`the_provisional_make_up_is_near_the_measured_one`]
/// allows the estimate, in dB. It measures 0.22 at most.
const ESTIMATE_TOLERANCE_DB: f64 = 1.0;

/// While an instrument is measured, its part plays at an estimate of the
/// make-up; for every fixture instrument the estimate lands within
/// [`ESTIMATE_TOLERANCE_DB`] of what the measurement finds, where playing
/// the samples as they are, 0 dB, would be as far off as the make-up
/// itself, -11 to +11 dB.
#[test]
fn the_provisional_make_up_is_near_the_measured_one() {
    let mut fixtures = Vec::new();
    for amplitude in [0.2, 0.5, 0.8] {
        for kind in [InstrumentKind::Sampler, InstrumentKind::SamplerKit] {
            fixtures.push((
                format!("{} recorded at {amplitude}", kind.name()),
                kind,
                recorded(kind, amplitude),
            ));
        }
    }
    fixtures.push((
        "looped sines".to_owned(),
        InstrumentKind::Sampler,
        tone_instrument(),
    ));
    fixtures.push((
        "kit".to_owned(),
        InstrumentKind::SamplerKit,
        kit_instrument(),
    ));
    let mut worst: f64 = 0.0;
    for (name, kind, (sfz, files)) in &fixtures {
        let mut engine = engine_with(*kind, Chain::NONE);
        engine.install_instrument(0, build_sfz(sfz, files)).unwrap();
        let provisional = engine.sampler_provisional_make_up_db(0).unwrap();
        assert_eq!(
            engine.sampler_make_up_db(0),
            None,
            "{name}: not measured yet"
        );
        while !engine
            .measure_instrument(0, manguebyte_web::PROBE_STEP_FRAMES)
            .unwrap()
        {}
        let measured = engine.sampler_make_up_db(0).unwrap();
        println!(
            "{name:<28} provisional {provisional:+6.2} dB, measured {measured:+6.2} dB, off by {:+5.2}",
            provisional - measured
        );
        worst = worst.max((provisional - measured).abs());
    }
    println!("worst: {worst:.2} dB");
    assert!(
        worst < ESTIMATE_TOLERANCE_DB,
        "the estimate is {worst:.2} dB off"
    );
}

/// Renders `engine` from its frame to `end` in quanta of `quantum`, calling
/// `between` before each quantum with the frame it starts on; returns
/// what it rendered.
fn render_with(
    engine: &mut Engine,
    end: u64,
    quantum: usize,
    between: &mut dyn FnMut(&mut Engine, u64),
) -> Vec<f32> {
    let mut left = Vec::new();
    while engine.frame() < end {
        between(engine, engine.frame());
        let frames = quantum.min((end - engine.frame()) as usize);
        engine.render(frames);
        left.extend_from_slice(engine.output().0);
    }
    left
}

/// A note struck on the frame after an instrument is handed over sounds,
/// at the provisional make-up, before it is measured; when the
/// measurement ends the part glides to the make-up measured in a straight
/// line over 20 ms, the ramp `sampler.normalize` takes.
#[test]
fn an_instrument_plays_before_it_is_measured_and_then_glides() {
    let (sfz, files) = tone_instrument();
    let note = EventRecord::note_on(0, 0, 69, 0.8);
    let measured_at = 4_864u64;
    let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
    engine
        .install_instrument(0, build_sfz(&sfz, &files))
        .unwrap();
    assert_eq!(engine.sampler_measuring(0), Some(true));
    assert_eq!(engine.sampler_make_up_db(0), None);
    let provisional = engine.sampler_provisional_make_up_db(0).unwrap();
    push_all(&mut engine, &[note]);
    let played = render_with(&mut engine, 12_000, 128, &mut |engine, frame| {
        if frame == measured_at {
            assert!(engine.measure_instrument(0, usize::MAX).unwrap());
        }
    });
    assert_eq!(engine.sampler_measuring(0), Some(false));
    let measured = engine.sampler_make_up_db(0).unwrap();
    // The same note as recorded, at the part's level and no make-up.
    let mut recorded = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz_unmeasured(&mut recorded, 0, &sfz, &files);
    push_all(&mut recorded, &[note]);
    let reference = render(&mut recorded, 12_000, 128).0;
    // A sine starts at 0: the note sounds from its second frame.
    assert_eq!(onset(&played), Some(1));
    let gain = |db: f64| 10f64.powf(db / 20.0);
    let ratio = |frame: usize| f64::from(played[frame]) / f64::from(reference[frame]);
    let (from, to) = (gain(provisional), gain(measured));
    // The parts are rendered the master's look-ahead ahead of the output,
    // so a change made on frame 4 864 is heard that much later.
    let start = measured_at as usize + engine.master_latency_frames();
    let ramp = 960;
    for frame in (0..12_000).filter(|&frame| reference[frame].abs() > 0.05) {
        let expected = if frame < start {
            from
        } else if frame >= start + ramp - 1 {
            to
        } else {
            from + (to - from) * (frame - start + 1) as f64 / ramp as f64
        };
        assert!(
            (ratio(frame) - expected).abs() < 1e-4 * expected,
            "frame {frame}: {} against {expected}",
            ratio(frame)
        );
    }
    println!(
        "provisional {provisional:+.2} dB, measured {measured:+.2} dB, glide from frame {start} over {ramp} frames"
    );
}

/// With `sampler.normalize` off an instrument handed over before it is
/// measured plays as recorded, bit for bit, and is not measured; switched
/// off while it is measured, the end of the measurement changes nothing.
#[test]
fn with_normalisation_off_a_measurement_changes_nothing() {
    let (sfz, files) = tone_instrument();
    let off = EventRecord::param(0, 0, id(ids::SAMPLER_NORMALIZE), 0.0);
    let notes = [
        EventRecord::note_on(128, 0, 69, 0.8),
        EventRecord::note_on(3_000, 0, 76, 0.6),
    ];
    let as_recorded = || {
        let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
        push_all(&mut engine, &[off]);
        engine.render(128);
        load_sfz_unmeasured(&mut engine, 0, &sfz, &files);
        push_all(&mut engine, &notes);
        render(&mut engine, 9_600, 128).0
    };
    let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
    push_all(&mut engine, &[off]);
    engine.render(128);
    engine
        .install_instrument(0, build_sfz(&sfz, &files))
        .unwrap();
    assert_eq!(engine.sampler_measuring(0), Some(false));
    assert_eq!(engine.sampler_provisional_make_up_db(0), None);
    assert!(engine.measure_instrument(0, 0).unwrap());
    push_all(&mut engine, &notes);
    let off_from_the_start = render(&mut engine, 9_600, 128).0;
    assert_eq!(bits(&off_from_the_start), bits(&as_recorded()));
    assert_eq!(engine.sampler_make_up_db(0), None);

    // Switched off on frame 1 000, while measured; the measurement ends on
    // frame 4 864, or never.
    let play = |measure: bool| {
        let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
        engine
            .install_instrument(0, build_sfz(&sfz, &files))
            .unwrap();
        push_all(&mut engine, &notes);
        push_all(
            &mut engine,
            &[EventRecord::param(
                1_000,
                0,
                id(ids::SAMPLER_NORMALIZE),
                0.0,
            )],
        );
        let left = render_with(&mut engine, 9_600, 128, &mut |engine, frame| {
            if measure && frame == 4_864 {
                assert!(engine.measure_instrument(0, usize::MAX).unwrap());
            }
        });
        (left, engine.sampler_make_up_db(0))
    };
    let (measured, make_up) = play(true);
    let (unmeasured, none) = play(false);
    assert!(make_up.is_some() && none.is_none());
    assert!(peak(&measured) > 0.1);
    assert_eq!(bits(&measured), bits(&unmeasured));
}

/// What a part plays while its instrument is measured depends only on the
/// frame the measurement ends before, not on the size of the quanta nor on
/// how far the measurement had got at each one: the part plays the
/// instrument with round robins of its own, which the measurement, on a
/// clone, never moves. Here the instrument alternates two regions, one of
/// them 6 dB down, and replaces one whose notes still ring.
#[test]
fn a_measurement_under_way_is_the_same_in_every_quantum_and_every_schedule() {
    let (first_sfz, files) = tone_instrument();
    let alternating = "<global> ampeg_release=0.05 pitch_keycenter=69 lokey=0 hikey=127
        loop_mode=loop_continuous loop_start=12000 loop_end=17999
        <region> sample=a4.wav seq_length=2 seq_position=1
        <region> sample=a4.wav seq_length=2 seq_position=2 volume=-6";
    let (installed_at, measured_at, end) = (672u64, 2_688u64, 6_000u64);
    let phrase_frames = (Phrase::MEASURED_SECONDS * RATE) as usize;
    let play = |quantum: usize, stepped: bool| {
        let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
        load_sfz_unmeasured(&mut engine, 0, &first_sfz, &files);
        let mut records = vec![EventRecord::note_on(0, 0, 57, 0.9)];
        for (step, frame) in (700..end).step_by(400).enumerate() {
            let key = 60 + (step % 5) as u8;
            records.push(EventRecord::note_on(frame, 0, key, 0.8));
            records.push(EventRecord::note_off(frame + 350, 0, key));
        }
        records.push(EventRecord::note_off(1_500, 0, 57));
        push_all(&mut engine, &records);
        let mut taken = 0;
        let left = render_with(&mut engine, end, quantum, &mut |engine, frame| {
            if frame == installed_at {
                let instrument = build_sfz(alternating, &files);
                engine.install_instrument(0, instrument).unwrap();
            } else if frame == measured_at {
                assert!(engine.measure_instrument(0, usize::MAX).unwrap());
            } else if stepped && frame > installed_at && frame < measured_at {
                // Steps of every size between the quanta, short of the end.
                let step = 997.min(phrase_frames - 1_000 - taken);
                assert!(!engine.measure_instrument(0, step).unwrap());
                taken += step;
            }
        });
        assert!(engine.sampler_make_up_db(0).is_some());
        left
    };
    let reference = play(224, false);
    assert!(peak(&reference) > 0.05);
    for quantum in [1, 7, 32, 96, 224] {
        for stepped in [false, true] {
            assert_eq!(
                bits(&play(quantum, stepped)),
                bits(&reference),
                "quanta of {quantum}, stepped {stepped}"
            );
        }
    }
}
