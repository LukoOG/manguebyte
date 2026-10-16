//! Playing live: cancelling the rest of a part, all-notes-off that never
//! clicks, and the engine's latency.

mod common;

use common::*;
use manguebyte_web::{
    CANCEL_FADE_SECONDS, Chain, Engine, EngineConfig, EventRecord, InstrumentKind,
    PANIC_FADE_SECONDS, ids,
};

/// The largest step between two neighbouring samples.
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

const CANCEL_FADE: usize = (CANCEL_FADE_SECONDS * RATE) as usize;

/// A part of `kind` through no stage, with an instrument if it is a
/// sampler: the tones for a sampler, the kit for a sampler kit.
fn part_engine(kind: InstrumentKind) -> Engine {
    let mut engine = engine_with(kind, Chain::NONE);
    match kind {
        InstrumentKind::Sampler => {
            let (sfz, files) = tone_instrument();
            load_sfz(&mut engine, 0, &sfz, &files);
        }
        InstrumentKind::SamplerKit => {
            let (sfz, files) = kit_instrument();
            load_sfz(&mut engine, 0, &sfz, &files);
        }
        _ => {}
    }
    engine
}

/// A loud sound that goes on: a chord, or drums that ring.
fn loud(kind: InstrumentKind) -> Vec<EventRecord> {
    let notes: &[u8] = match kind {
        InstrumentKind::Synth => &[45, 52, 57, 61],
        InstrumentKind::Sampler => &[69, 72, 76, 81],
        // The crash, the ride, the open hat and the floor tom.
        InstrumentKind::Drums => &[49, 51, 46, 43],
        // The sampled open hat, and the crash and the floor tom from the
        // synthesized kit.
        InstrumentKind::SamplerKit => &[46, 49, 43],
    };
    notes
        .iter()
        .map(|&note| EventRecord::note_on(0, 0, note, 1.0))
        .collect()
}

#[test]
fn a_cancel_drops_the_rest_of_its_part_and_keeps_what_comes_after() {
    // Part 0 has a bar of notes; part 1 drums beside it. At frame 4 000 the
    // performer edits part 0: a cancel, then the new notes.
    let bar = [
        EventRecord::note_on(1_000, 0, 57, 0.8),
        EventRecord::note_off(4_500, 0, 57),
        EventRecord::note_on(5_000, 0, 60, 0.8),
        EventRecord::param(5_500, 0, id(ids::SYNTH_CUTOFF), 3_000.0),
        EventRecord::note_off(7_000, 0, 60),
        EventRecord::note_on(4_000, 0, 64, 0.8), // on the cancel's frame
        EventRecord::note_on(3_999, 0, 67, 0.8), // just before it: kept
        EventRecord::note_on(5_000, 1, 38, 0.8), // another part: kept
    ];
    let cancel = EventRecord::cancel(4_000, 0);
    let new = [
        EventRecord::note_on(4_000, 0, 62, 0.7),
        EventRecord::note_off(6_000, 0, 62),
    ];
    let two_parts = || {
        let mut engine = engine();
        engine
            .create_part_with(0, InstrumentKind::Synth, Chain::NONE)
            .unwrap();
        engine
            .create_part_with(1, InstrumentKind::Drums, Chain::NONE)
            .unwrap();
        engine
    };
    let mut engine = two_parts();
    let mut reference = two_parts();
    push_all(&mut engine, &bar);
    push_all(&mut engine, &[cancel]);
    push_all(&mut engine, &new);
    // The same without the five events the cancel drops.
    let kept: Vec<EventRecord> = [bar[0], bar[6], bar[7], cancel]
        .into_iter()
        .chain(new)
        .collect();
    push_all(&mut reference, &kept);
    assert_eq!(engine.stats().cancelled, 5);
    assert_eq!(engine.pending_len(), reference.pending_len());
    let (left, right) = render(&mut engine, 12_000, 128);
    let (expected_left, expected_right) = render(&mut reference, 12_000, 128);
    assert_eq!(bits(&left), bits(&expected_left));
    assert_eq!(bits(&right), bits(&expected_right));
    assert_eq!(reference.stats().cancelled, 0);
}

#[test]
fn a_cancel_fades_every_kind_of_part_out_without_a_step() {
    for kind in InstrumentKind::ALL {
        let mut engine = part_engine(kind);
        push_all(&mut engine, &loud(kind));
        let at = 9_600;
        engine.push(EventRecord::cancel(at as u64, 0));
        let (left, _) = render(&mut engine, at + 4_800, 128);
        let before = largest_step(&left[at - 4_800..at]);
        let fading = largest_step(&left[at - 1..at + CANCEL_FADE + 1]);
        let loudest = peak(&left[at - 4_800..at]);
        println!("{kind:?}: step {before:.4} before, {fading:.4} in the fade, peak {loudest:.3}");
        assert!(loudest > 0.1, "{kind:?}");
        assert!(fading <= before, "{kind:?}: {fading} against {before}");
        assert!(
            left[at + CANCEL_FADE..].iter().all(|&sample| sample == 0.0),
            "{kind:?}: still sounding after the fade"
        );
        assert_eq!(engine.active_voices(0), Some(0), "{kind:?}");
    }
}

#[test]
fn notes_after_a_cancel_play_as_on_a_quiet_part() {
    // A note on the cancel's frame starts cleanly beside the fading ones,
    // and a key struck again while it fades glides back up.
    for kind in InstrumentKind::ALL {
        let mut engine = part_engine(kind);
        push_all(&mut engine, &loud(kind));
        let note = loud(kind)[0].note;
        let records = [
            EventRecord::cancel(9_600, 0),
            EventRecord::note_on(9_600, 0, note, 0.9),
            EventRecord::note_on(9_700, 0, note, 0.9),
        ];
        push_all(&mut engine, &records);
        let (left, _) = render(&mut engine, 19_200, 128);
        assert!(
            peak(&left[12_000..]) > 0.05,
            "{kind:?}: the new note sounds"
        );
        assert!(left.iter().all(|sample| sample.is_finite()));
    }
}

/// How much larger the largest step after an all-notes-off may be than the
/// largest step of the sound before it, as a share of that sound's peak.
/// A cut leaves a step as large as the sample it cuts: about 1 of the peak
/// where the tests below stop the sound, at its loudest.
const PANIC_BOUND: f32 = 0.02;

/// Where the sound of `make` is loudest between frames 9 600 and 12 000.
fn loudest_frame(make: &dyn Fn() -> Engine) -> usize {
    let (left, _) = render(&mut make(), 12_000, 128);
    (9_600..12_000)
        .max_by(|&a, &b| left[a].abs().total_cmp(&left[b].abs()))
        .unwrap()
}

/// The largest step after `stop` lands on the loudest frame of `make`'s
/// sound, less the largest step before it, as a share of its peak: below 0
/// when no step after is as large as one before.
fn added_step(make: &dyn Fn() -> Engine, stop: fn(u64) -> EventRecord) -> f32 {
    let at = loudest_frame(make);
    let mut engine = make();
    engine.push(stop(at as u64));
    let (left, right) = render(&mut engine, at + 9_600, 128);
    let mut added = f32::NEG_INFINITY;
    for channel in [&left, &right] {
        let before = largest_step(&channel[at - 4_800..at]);
        let after = largest_step(&channel[at - 1..at + 9_600]);
        let loudest = peak(&channel[at - 4_800..at]);
        added = added.max((after - before) / loudest);
    }
    added
}

fn all_notes_off(at: u64) -> EventRecord {
    EventRecord::all_notes_off(at, 0)
}

/// Every kind of part, playing its loud sound.
fn loud_parts() -> Vec<(String, Box<dyn Fn() -> Engine>)> {
    let mut cases: Vec<(String, Box<dyn Fn() -> Engine>)> = Vec::new();
    for kind in InstrumentKind::ALL {
        cases.push((
            format!("{kind:?}"),
            Box::new(move || {
                let mut engine = part_engine(kind);
                push_all(&mut engine, &loud(kind));
                engine
            }),
        ));
    }
    cases
}

#[test]
fn all_notes_off_fades_every_kind_of_part_without_a_click() {
    let mut cases = loud_parts();
    // The synth with its shortest release, 5 ms.
    cases.push((
        "Synth, release 5 ms".to_owned(),
        Box::new(|| {
            let mut engine = part_engine(InstrumentKind::Synth);
            engine.push(EventRecord::param(0, 0, id(ids::SYNTH_RELEASE), 0.005));
            push_all(&mut engine, &loud(InstrumentKind::Synth));
            engine
        }),
    ));
    // The sampler's tones with no release of their own (SFZ's 1 ms), which
    // fade over `PANIC_FADE_SECONDS` instead, and through the full chain.
    cases.push((
        "Sampler, ampeg_release 1 ms".to_owned(),
        Box::new(|| {
            let (sfz, files) = tone_instrument();
            let sfz = sfz.replace("<global> ampeg_release=0.05", "");
            let mut engine = engine_with(InstrumentKind::Sampler, Chain::FULL);
            load_sfz(&mut engine, 0, &sfz, &files);
            push_all(&mut engine, &loud(InstrumentKind::Sampler));
            engine
        }),
    ));
    let mut worst = f32::NEG_INFINITY;
    for (name, make) in &cases {
        let added = added_step(make.as_ref(), all_notes_off);
        println!("{name}: all notes off adds {added:.5} of the peak");
        assert!(added <= PANIC_BOUND, "{name}: {added}");
        worst = worst.max(added);
    }
    println!("the most all notes off adds: {worst:.5} of the peak");
}

#[test]
fn the_bound_catches_a_click() {
    // A choke stops the synth and the sampler at once: the step it leaves
    // at the loudest frame is far beyond the bound, which would so catch an
    // all-notes-off that cut. (The drums are no test of it: their noise
    // steps as far from one sample to the next as a cut would.)
    let parts = loud_parts();
    for (name, make) in [&parts[0], &parts[2]] {
        let added = added_step(make.as_ref(), |at| EventRecord::choke(at, 0));
        println!("{name}: a choke adds {added:.3} of the peak");
        assert!(added > 10.0 * PANIC_BOUND, "{name}: {added}");
    }
}

#[test]
fn a_sampler_part_lets_go_within_the_panic_fade() {
    // With no release of its own, a sampled note is gone right after the
    // fade of `PANIC_FADE_SECONDS`.
    let fade = (PANIC_FADE_SECONDS * RATE).round() as usize;
    let (sfz, files) = tone_instrument();
    let sfz = sfz.replace("<global> ampeg_release=0.05", "");
    let mut engine = engine_with(InstrumentKind::Sampler, Chain::NONE);
    load_sfz(&mut engine, 0, &sfz, &files);
    push_all(&mut engine, &loud(InstrumentKind::Sampler));
    engine.push(EventRecord::all_notes_off(9_600, 0));
    let (left, _) = render(&mut engine, 9_600 + fade + 200, 128);
    assert!(
        left[9_600 + fade - 20..9_600 + fade]
            .iter()
            .any(|&s| s != 0.0)
    );
    assert!(left[9_600 + fade..].iter().all(|&sample| sample == 0.0));
}

#[test]
fn cancels_and_panics_do_not_depend_on_the_quanta() {
    let scenario = || {
        let mut engine = engine();
        for (part, kind) in InstrumentKind::ALL.into_iter().enumerate() {
            engine.create_part(part as u16, kind).unwrap();
        }
        let (sfz, files) = tone_instrument();
        load_sfz(&mut engine, 2, &sfz, &files);
        let (sfz, files) = kit_instrument();
        load_sfz(&mut engine, 3, &sfz, &files);
        let mut records = Vec::new();
        for (part, kind) in InstrumentKind::ALL.into_iter().enumerate() {
            let part = part as u16;
            for (step, mut record) in loud(kind).into_iter().enumerate() {
                record.part = part;
                record.frame = step as u64 * 333;
                records.push(record);
            }
            records.push(EventRecord::note_on(9_000, part, loud(kind)[1].note, 0.7));
            records.push(EventRecord::cancel(5_000 + u64::from(part) * 77, part));
            records.push(EventRecord::note_on(5_100, part, loud(kind)[0].note, 0.6));
            records.push(EventRecord::all_notes_off(
                12_000 + u64::from(part) * 5,
                part,
            ));
        }
        push_all(&mut engine, &records);
        engine
    };
    let reference = render(&mut scenario(), 24_000, 1);
    assert!(peak(&reference.0) > 0.1);
    for quantum in [7, 128, 512] {
        let output = render(&mut scenario(), 24_000, quantum);
        assert_eq!(bits(&output.0), bits(&reference.0), "left, {quantum}");
        assert_eq!(bits(&output.1), bits(&reference.1), "right, {quantum}");
    }
}

#[test]
fn a_cancel_is_taken_even_when_the_queue_is_full() {
    let mut engine = Engine::new(EngineConfig {
        pending_capacity: 4,
        ..EngineConfig::new(RATE)
    })
    .unwrap();
    engine.create_part(0, InstrumentKind::Drums).unwrap();
    for frame in [100, 200, 300, 400] {
        engine.push(EventRecord::note_on(frame, 0, 38, 1.0));
    }
    let mut bytes = Vec::new();
    for record in [
        EventRecord::cancel(250, 0),
        EventRecord::note_on(260, 0, 36, 1.0),
    ] {
        bytes.extend_from_slice(&record.to_bytes());
    }
    // A full queue: the cancel goes in all the same, drops two, and makes
    // room for the note after it.
    assert_eq!(engine.admit_bytes(&bytes), 2);
    assert_eq!(engine.stats().cancelled, 2);
    assert_eq!(engine.pending_len(), 4);
}

#[test]
fn the_latency_is_one_quantum_and_the_master_look_ahead() {
    let mut engine = engine();
    // 2 ms of look-ahead and 8 frames to read between samples, at 48 kHz.
    let ahead = engine.master_latency_frames();
    assert_eq!(ahead, 111);
    assert_eq!(
        ahead,
        manguebyte_web::TRUE_PEAK_DELAY
            + (manguebyte_web::MASTER_LOOKAHEAD_SECONDS * RATE) as usize
            - 1
    );
    assert_eq!(engine.latency_frames(), 512 + ahead, "before any render");
    engine.render(128);
    assert_eq!(engine.latency_frames(), 128 + ahead);
    engine.render(7);
    assert_eq!(engine.latency_frames(), 7 + ahead);
    engine.render(0);
    assert_eq!(
        engine.latency_frames(),
        7 + ahead,
        "an empty render changes nothing"
    );
    // An event due one quantum and the look-ahead ahead of the frame the
    // engine is at lands on its frame even if it arrives while that
    // quantum is rendered: the parts render it in the next one, and it is
    // heard on its own frame.
    engine.create_part(0, InstrumentKind::Drums).unwrap();
    engine.render(128);
    let due = engine.frame() + engine.latency_frames() as u64;
    engine.render(128); // the quantum it arrives during
    engine.push(EventRecord::note_on(due, 0, 36, 1.0));
    engine.render(128);
    assert_eq!(engine.stats().late, 0);
    let start = engine.frame() - 128;
    assert_eq!(onset(engine.output().0), Some((due - start) as usize));
    assert_eq!(due - start, ahead as u64);
}
