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

/// What a part of `kind` is made ready with before it plays.
fn setup_for(kind: InstrumentKind) -> Box<dyn Fn(&mut Engine)> {
    let _ = kind;
    Box::new(|_: &mut Engine| {})
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
    for (kind, index, row) in rows() {
        let Some(make_up) = row.make_up_db else {
            // The sampler kinds, measured as their instruments load.
            continue;
        };
        let after = phrase_loudness(kind, index, &*setup_for(kind));
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
            misses.push(format!(
                "{} reads {after:.2} LUFS on its {} phrase, {off:+.2} LU from the target: \
                 its make-up in src/level.rs should be {:+.2} dB",
                label(kind, &row),
                row.phrase.name(),
                make_up - off
            ));
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
        let play = |records: &[EventRecord], quantum: usize| {
            let mut engine = engine_with(kind, Chain::NONE);
            push_all(&mut engine, records);
            render(&mut engine, 24_000, quantum).0
        };
        let early = play(
            &[
                EventRecord::param(0, 0, id(preset), last),
                EventRecord::note_on(4_800, 0, 52, 0.8),
            ],
            128,
        );
        let with_the_note = play(
            &[
                EventRecord::param(4_800, 0, id(preset), last),
                EventRecord::note_on(4_800, 0, 52, 0.8),
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
    for (kind, index, row) in rows() {
        if row.make_up_db.is_none() {
            continue;
        }
        let peak = accented_peak(kind, index, &*setup_for(kind));
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
