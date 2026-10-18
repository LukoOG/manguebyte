//! Proves that pushing events, draining the ring and rendering never touch
//! the heap.
//!
//! The global allocator is replaced with one that counts, as in
//! `crates/manguebyte-dsp/tests/realtime.rs`, and the test fails if
//! anything allocates, reallocates or frees while the engine runs.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_web::ffi;
use manguebyte_web::{
    Chain, Drum, Effect, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, event_ring,
    ids,
};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and returns how many heap calls it made.
fn heap_calls(work: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

#[test]
fn the_counter_counts() {
    assert!(heap_calls(|| drop(vec![1u8; 16])) >= 2);
}

fn events(start: u64) -> Vec<EventRecord> {
    let id = ParamId::from_name;
    let mut records = Vec::new();
    for step in 0..32u64 {
        let frame = start + step * 300;
        let drum = Drum::ALL[step as usize % Drum::ALL.len()];
        // Every third note accented.
        let accent = step % 3 == 0;
        records.push(EventRecord::note_on(frame, 0, drum.note(), 0.9).with_accent(accent));
        records.push(
            EventRecord::note_on(frame + 7, 1, 40 + (step % 12) as u8, 0.8).with_accent(accent),
        );
        records.push(EventRecord::note_off(
            frame + 250,
            1,
            40 + (step % 12) as u8,
        ));
        records.push(EventRecord::param(
            frame + 1,
            1,
            id(ids::SYNTH_CUTOFF),
            200.0 + step as f64 * 90.0,
        ));
        records.push(EventRecord::param(
            frame + 2,
            1,
            id(ids::LOWPASS_CUTOFF),
            3_000.0,
        ));
        records.push(EventRecord::param(
            frame + 3,
            0,
            id(ids::REVERB_MIX),
            (step % 3) as f64 * 0.3,
        ));
        records.push(EventRecord::param(frame + 4, 0, id(ids::PAN), -0.5));
        records.push(EventRecord::param(frame + 5, 0, id(ids::KIT_TUNE), 2.0));
        records.push(EventRecord::param(
            frame + 6,
            1,
            id(ids::SYNTH_WAVE),
            (step % 4) as f64,
        ));
    }
    // Every parameter, sent to every part, at the ends of its range and
    // beyond: routed or not, nothing may allocate.
    let values = [-1e9, 0.0, 0.5, 1.0, 3.0, 1e9];
    for (index, name) in ids::ALL.iter().enumerate() {
        for part in 0..PARTS {
            let value = values[(index + usize::from(part)) % values.len()];
            records.push(EventRecord::param(
                start + 40 + index as u64 * 97,
                part,
                id(name),
                value,
            ));
        }
    }
    // Parts 8 and 9 with every effect switched on, sending to the shared
    // reverb, through the glue compressor, synced to a tempo; switched
    // off and on again, and changed while they play.
    records.push(EventRecord::param(start, 0, id(ids::GLUE_ON), 1.0));
    records.push(EventRecord::param(start, 0, id(ids::MASTER_TEMPO), 133.0));
    for part in [8u16, 9] {
        for effect in Effect::PART_ORDER {
            let switch = id(&format!("{}.on", effect.name()));
            records.push(EventRecord::param(start, part, switch, 1.0));
            records.push(EventRecord::param(start + 4_000, part, switch, 0.0));
            records.push(EventRecord::param(start + 5_000, part, switch, 1.0));
        }
        for (name, value) in [
            (ids::SEND, 0.6),
            (ids::TREMOLO_SYNC, 0.5),
            (ids::DELAY_SYNC, 0.25),
            (ids::DELAY_PINGPONG, 1.0),
            (ids::BITCRUSH_RATE, 8_000.0),
            (ids::EQ_MID_GAIN, 9.0),
        ] {
            records.push(EventRecord::param(start + 1, part, id(name), value));
        }
        records.push(EventRecord::param(
            start + 2_000,
            part,
            id(ids::DELAY_TIME),
            0.2,
        ));
        records.push(EventRecord::param(
            start + 2_100,
            part,
            id(ids::DRIVE_SHAPE),
            3.0,
        ));
        records.push(EventRecord::param(
            start + 2_200,
            part,
            id(ids::PHASER_STAGES),
            7.0,
        ));
        records.push(EventRecord::param(
            start + 2_300,
            part,
            id(ids::CHORUS_VOICES),
            8.0,
        ));
        for step in 0..16u64 {
            records.push(EventRecord::note_on(
                start + 3 + step * 600,
                part,
                38 + step as u8,
                1.0,
            ));
        }
    }
    records.push(EventRecord::param(
        start + 3_000,
        0,
        id(ids::MASTER_TEMPO),
        77.0,
    ));
    records.push(EventRecord::param(
        start + 3_000,
        0,
        id(ids::MASTER_BEAT),
        16.0,
    ));
    // Nine notes on eight voices: a voice is taken over and fades out.
    for note in 0..9u8 {
        records.push(EventRecord::note_on(
            start + 3_000 + u64::from(note),
            4,
            50 + note,
            0.7,
        ));
    }
    // The sampler part: notes on both of its regions, accents, releases.
    for step in 0..24u64 {
        let key = [57u8, 69, 72, 81, 84, 93][step as usize % 6];
        let frame = start + 200 + step * 211;
        records.push(EventRecord::note_on(frame, 6, key, 0.9).with_accent(step % 2 == 0));
        records.push(EventRecord::note_off(frame + 400, 6, key));
    }
    records.push(EventRecord::choke(start + 4_000, 6));
    records.push(EventRecord::note_on(start + 4_100, 6, 69, 1.0));
    records.push(EventRecord::all_notes_off(start + 5_500, 6));
    // The sampler kit: its samples, and the drums it has none for.
    for (step, drum) in [36u8, 88, 42, 50, 46, 38, 43, 37].into_iter().enumerate() {
        let frame = start + 150 + step as u64 * 300;
        records.push(EventRecord::note_on(frame, 7, drum, 0.8).with_accent(step % 3 == 0));
    }
    records.push(EventRecord::choke(start + 3_000, 7));
    records.push(EventRecord::note_on(start + 3_100, 7, 88, 1.0));
    records.push(EventRecord::all_notes_off(start + 4_000, 7));
    // Cancels: each drops what is left of its part and fades its notes.
    for part in [0u16, 1, 4, 6, 7] {
        records.push(EventRecord::cancel(
            start + 2_500 + u64::from(part) * 31,
            part,
        ));
    }
    // A held key struck again: it glides to its new level.
    records.push(EventRecord::note_on(start + 3_500, 4, 50, 0.2));
    records.push(EventRecord::note_on(start + 3_900, 4, 50, 1.0));
    records.push(EventRecord::choke(start + 4_500, 4));
    records.push(EventRecord::all_notes_off(start + 5_000, 1));
    records.push(EventRecord::choke(start + 6_000, 0));
    // The instruments of manguebyte-dsp: plucked, FM, organ, analogue kit,
    // kick bass and acid, parts 10 to 15. Chords and drums with accents,
    // the basses sliding into every other note, presets, the speaker sped
    // up, then a cancel, a choke and an all-notes-off on each.
    for step in 0..24u64 {
        let frame = start + 11 + step * 250;
        let accent = step % 3 == 2;
        let key = 40 + (step * 5 % 24) as u8;
        for part in [10u16, 11, 12] {
            records.push(EventRecord::note_on(frame, part, key + 12, 0.8).with_accent(accent));
            records.push(EventRecord::note_off(frame + 400, part, key + 12));
        }
        let drum = [36u8, 38, 42, 46, 49, 51, 88, 39][step as usize % 8];
        records.push(EventRecord::note_on(frame, 13, drum, 0.9).with_accent(accent));
        let off = if step % 2 == 0 {
            frame + 260
        } else {
            frame + 200
        };
        for part in [14u16, 15] {
            records.push(EventRecord::note_on(frame, part, key - 12, 0.9).with_accent(accent));
            records.push(EventRecord::note_off(off, part, key - 12));
        }
    }
    for (frame, part, name, value) in [
        (start + 900, 10, ids::PLUCKED_PRESET, 3.0),
        (start + 901, 11, ids::FM_PRESET, 2.0),
        (start + 902, 12, ids::ORGAN_PRESET, 2.0),
        (start + 903, 12, ids::ORGAN_ROTARY, 2.0),
        (start + 904, 12, ids::ORGAN_SPEAKER, 0.4),
        (start + 905, 13, ids::KIT_TUNE, 5.0),
        (start + 906, 15, ids::ACID_PRESET, 1.0),
        (start + 907, 14, ids::KICK_BASS_GLIDE, 0.3),
    ] {
        records.push(EventRecord::param(frame, part, id(name), value));
    }
    // The newer voices: the drum kit, the polyphonic synthesiser, the
    // mallets, the bowed strings, the winds and the choir, parts 16 to 21.
    // Chords, drums and Aratu's shaker with accents, overlapping notes that
    // slur, every kit, a change from a monophonic preset to a polyphonic one
    // and back, the wheel, legato switched, then a cancel, a choke and an
    // all-notes-off on each.
    for step in 0..24u64 {
        let frame = start + 13 + step * 250;
        let accent = step % 3 == 1;
        let key = 48 + (step * 7 % 24) as u8;
        let drum = [36u8, 38, 42, 46, 49, 51, 88, 39, 82, 43][step as usize % 10];
        records.push(EventRecord::note_on(frame, 16, drum, 0.9).with_accent(accent));
        for part in 17..22u16 {
            records.push(EventRecord::note_on(frame, part, key, 0.8).with_accent(accent));
            records.push(EventRecord::note_on(frame, part, key + 7, 0.7));
            // Held into the next note: a slur where the part slurs.
            records.push(EventRecord::note_off(frame + 300, part, key));
            records.push(EventRecord::note_off(frame + 300, part, key + 7));
        }
    }
    for preset in 0..10u64 {
        records.push(EventRecord::param(
            start + 100 + preset * 500,
            16,
            id(ids::DRUM_KIT_PRESET),
            preset as f64,
        ));
    }
    for (frame, part, name, value) in [
        (start + 700, 17, ids::POLY_PRESET, 26.0),
        (start + 1_700, 17, ids::POLY_PRESET, 11.0),
        (start + 2_700, 17, ids::POLY_PRESET, 15.0),
        (start + 800, 17, ids::POLY_MOD_WHEEL, 0.9),
        (start + 900, 17, ids::POLY_CUTOFF, -1.5),
        (start + 1_000, 18, ids::MALLET_PRESET, 2.0),
        (start + 1_100, 18, ids::MALLET_MOTOR, 7.0),
        (start + 1_200, 19, ids::BOWED_LEGATO, 1.0),
        (start + 1_300, 19, ids::BOWED_PRESET, 4.0),
        (start + 1_400, 20, ids::WIND_PRESET, 3.0),
        (start + 1_500, 20, ids::WIND_LEGATO, 0.0),
        (start + 1_600, 21, ids::CHOIR_PRESET, 1.0),
        (start + 1_650, 21, ids::CHOIR_LEGATO, 1.0),
    ] {
        records.push(EventRecord::param(frame, part, id(name), value));
    }
    for part in 10..22u16 {
        records.push(EventRecord::cancel(start + 3_200 + u64::from(part), part));
        records.push(EventRecord::note_on(start + 3_300, part, 45, 1.0));
        records.push(EventRecord::choke(start + 4_200, part));
        records.push(EventRecord::note_on(start + 4_300, part, 50, 1.0));
        records.push(EventRecord::all_notes_off(start + 5_200, part));
    }
    records.push(EventRecord::note_on(start, 19, 60, 1.0)); // no such part
    records.push(EventRecord {
        kind: 200,
        ..EventRecord::default()
    }); // ignored
    records
}

/// How many parts `parts` makes.
const PARTS: u16 = 22;

/// Both instruments through every chain: parts 0 to 5; part 6, a sampler
/// with an instrument; part 7, a sampler kit with one; parts 8 and 9 with
/// every effect; parts 10 to 21, the instruments of manguebyte-dsp, some
/// with every effect.
fn parts(engine: &mut Engine) {
    let lowpass = Chain {
        lowpass: true,
        reverb: false,
        ..Chain::NONE
    };
    let reverb = Chain {
        lowpass: false,
        reverb: true,
        ..Chain::NONE
    };
    for (part, kind, chain) in [
        (0, InstrumentKind::Drums, Chain::FULL),
        (1, InstrumentKind::Synth, Chain::FULL),
        (2, InstrumentKind::Synth, Chain::NONE),
        (3, InstrumentKind::Drums, lowpass),
        (4, InstrumentKind::Synth, reverb),
        (5, InstrumentKind::Drums, Chain::NONE),
        (6, InstrumentKind::Sampler, Chain::FULL),
        (7, InstrumentKind::SamplerKit, Chain::FULL),
        (8, InstrumentKind::Drums, Chain::ALL),
        (9, InstrumentKind::Synth, Chain::ALL),
        (10, InstrumentKind::Plucked, Chain::FULL),
        (11, InstrumentKind::Fm, Chain::NONE),
        (12, InstrumentKind::Organ, Chain::ALL),
        (13, InstrumentKind::AnalogKit, Chain::FULL),
        (14, InstrumentKind::KickBass, Chain::NONE),
        (15, InstrumentKind::Acid, Chain::ALL),
        (16, InstrumentKind::DrumKit, Chain::FULL),
        (17, InstrumentKind::Poly, Chain::ALL),
        (18, InstrumentKind::Mallet, Chain::NONE),
        (19, InstrumentKind::Bowed, Chain::FULL),
        (20, InstrumentKind::Wind, Chain::NONE),
        (21, InstrumentKind::Choir, Chain::ALL),
    ] {
        engine.create_part_with(part, kind, chain).unwrap();
    }
    let (sfz, files) = common::tone_instrument();
    common::load_sfz(engine, 6, &sfz, &files);
    let (sfz, files) = common::kit_instrument();
    common::load_sfz(engine, 7, &sfz, &files);
}

#[test]
fn pushing_draining_and_rendering_never_allocate() {
    let mut engine = Engine::new(EngineConfig {
        pending_capacity: 32_768,
        ..EngineConfig::new(48_000.0)
    })
    .unwrap();
    parts(&mut engine);
    let (mut producer, mut consumer) = event_ring(8_192);
    let first = events(0);
    let second = events(20_000);
    let mut bytes = Vec::new();
    for record in events(40_000) {
        bytes.extend_from_slice(&record.to_bytes());
    }
    let mut left = vec![0.0f32; 1_000];
    let mut right = vec![0.0f32; 1_000];

    let calls = heap_calls(|| {
        for &record in &first {
            engine.push(record);
        }
        for &record in &second {
            producer.push(record).unwrap();
        }
        engine.drain(&mut consumer);
        engine.push_bytes(&bytes);
        // Every quantum size, and a long stretch of silence for the reverb
        // to fall asleep in, then sound to wake it.
        for quantum in [1, 7, 128, 512, 333] {
            for _ in 0..40 {
                engine.render(quantum);
            }
        }
        while engine.frame() < 120_000 {
            engine.render(128);
        }
        engine.push(EventRecord::note_on(0, 0, 38, 1.0)); // late
        engine.render_into(&mut left, &mut right);
        engine.clear_pending();
    });
    assert_eq!(calls, 0, "the engine touched the heap {calls} times");
    let stats = engine.stats();
    assert_eq!(stats.dropped, 0);
    assert!(stats.applied > 1_000, "{stats:?}");
    assert!(stats.cancelled > 0, "{stats:?}");
    assert!(stats.unrouted > 0, "{stats:?}");
    // The extreme values drive the limiter.
    assert!(stats.limited > 0, "{stats:?}");
}

/// Loudness normalisation measures an instrument in the control call that
/// loads it, never while the engine renders: the parts have their make-up
/// before they first render, and rendering, with notes sounding and
/// normalisation switched off and on under them, touches no heap. The
/// measurement itself allocates, so the counter would see it there.
#[test]
fn loudness_normalisation_is_measured_outside_the_render_call() {
    let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
    engine.create_part(0, InstrumentKind::Sampler).unwrap();
    engine.create_part(1, InstrumentKind::SamplerKit).unwrap();
    let (sfz, files) = common::tone_instrument();
    let (kit_sfz, kit_files) = common::kit_instrument();
    let loads = heap_calls(|| {
        common::load_sfz(&mut engine, 0, &sfz, &files);
        common::load_sfz(&mut engine, 1, &kit_sfz, &kit_files);
    });
    assert!(loads > 0, "measuring allocates");
    assert!(engine.sampler_make_up_db(0).is_some());
    assert!(engine.sampler_make_up_db(1).is_some());
    let normalize = ParamId::from_name(ids::SAMPLER_NORMALIZE);
    let mut records = Vec::new();
    for step in 0..40u64 {
        let frame = step * 1_200;
        records.push(EventRecord::note_on(frame, 0, 57 + (step % 12) as u8, 0.8));
        records.push(EventRecord::note_off(
            frame + 1_000,
            0,
            57 + (step % 12) as u8,
        ));
        records.push(EventRecord::note_on(
            frame,
            1,
            [36, 38, 42, 88][step as usize % 4],
            0.8,
        ));
        for part in [0, 1] {
            records.push(EventRecord::param(
                frame + 300,
                part,
                normalize,
                (step % 2) as f64,
            ));
        }
    }
    let calls = heap_calls(|| {
        for &record in &records {
            engine.push(record);
        }
        for _ in 0..400 {
            engine.render(128);
        }
    });
    assert_eq!(calls, 0, "the engine touched the heap {calls} times");
    assert_eq!(engine.stats().dropped, 0);
    assert_eq!(engine.stats().unrouted, 0);
}

/// An instrument handed over before it is measured plays while the
/// measurement goes on between the quanta: the render calls, with notes
/// sounding, normalisation switched under them and the glide to the
/// make-up measured, touch no heap; the steps, which do, are control
/// calls of their own.
#[test]
fn rendering_while_an_instrument_is_measured_never_allocates() {
    let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
    engine.create_part(0, InstrumentKind::Sampler).unwrap();
    engine.create_part(1, InstrumentKind::SamplerKit).unwrap();
    let (sfz, files) = common::tone_instrument();
    let (kit_sfz, kit_files) = common::kit_instrument();
    engine
        .install_instrument(0, common::build_sfz(&sfz, &files))
        .unwrap();
    engine
        .install_instrument(1, common::build_sfz(&kit_sfz, &kit_files))
        .unwrap();
    let normalize = ParamId::from_name(ids::SAMPLER_NORMALIZE);
    let mut records = Vec::new();
    for step in 0..60u64 {
        let frame = step * 1_000;
        let key = 57 + (step % 12) as u8;
        records.push(EventRecord::note_on(frame, 0, key, 0.8));
        records.push(EventRecord::note_off(frame + 900, 0, key));
        records.push(EventRecord::note_on(
            frame,
            1,
            [36, 38, 42, 88][step as usize % 4],
            0.8,
        ));
        if step % 7 == 3 {
            records.push(EventRecord::param(frame + 300, 0, normalize, 0.0));
            records.push(EventRecord::param(frame + 600, 0, normalize, 1.0));
        }
    }
    for &record in &records {
        engine.push(record);
    }
    let mut calls = 0;
    let mut steps = 0;
    while engine.sampler_measuring(0) == Some(true) || engine.sampler_measuring(1) == Some(true) {
        calls += heap_calls(|| {
            for _ in 0..4 {
                engine.render(128);
            }
        });
        for part in [0, 1] {
            engine
                .measure_instrument(part, manguebyte_web::PROBE_STEP_FRAMES)
                .unwrap();
        }
        steps += 1;
    }
    // The glides to the make-ups measured, and what rings on.
    calls += heap_calls(|| {
        for _ in 0..200 {
            engine.render(128);
        }
    });
    assert_eq!(calls, 0, "the engine touched the heap {calls} times");
    assert!(steps >= 27, "{steps} steps");
    assert!(engine.sampler_make_up_db(0).is_some());
    assert!(engine.sampler_make_up_db(1).is_some());
    assert_eq!(engine.stats().dropped, 0);
    assert_eq!(engine.stats().unrouted, 0);
}

/// Every path a full queue takes: waiting events pushed out by earlier
/// ones, records refused, and drains and staged copies that stop with
/// records left over. (Silencing a part that produced something that is
/// not a number cannot be reached from outside the crate; that code only
/// resets state in place.)
#[test]
fn a_full_queue_never_allocates() {
    let mut engine = Engine::new(EngineConfig {
        pending_capacity: 8,
        ..EngineConfig::new(48_000.0)
    })
    .unwrap();
    parts(&mut engine);
    let (mut producer, mut consumer) = event_ring(64);
    let far: Vec<EventRecord> = (0..8u64)
        .map(|n| EventRecord::note_on(1_000_000 + n, 0, 36, 1.0))
        .collect();
    let mut bytes = Vec::new();
    for frame in [500u64, 2_000_000, 600] {
        bytes.extend_from_slice(&EventRecord::note_on(frame, 1, 45, 1.0).to_bytes());
    }
    let calls = heap_calls(|| {
        for &record in &far {
            engine.push(record);
        }
        // Earlier than everything waiting: each pushes out a far event.
        for frame in 0..4u64 {
            producer
                .push(EventRecord::note_on(frame * 100, 1, 50, 1.0))
                .unwrap();
        }
        // Later than everything waiting: the drain stops here.
        producer
            .push(EventRecord::note_on(3_000_000, 1, 50, 1.0))
            .unwrap();
        engine.drain(&mut consumer);
        engine.drain(&mut consumer);
        engine.push(EventRecord::note_on(5_000_000, 0, 36, 1.0)); // refused
        engine.admit_bytes(&bytes);
        for _ in 0..20 {
            engine.render(128);
        }
    });
    assert_eq!(calls, 0, "the engine touched the heap {calls} times");
    let stats = engine.stats();
    assert_eq!(stats.stalled, 3, "{stats:?}");
    assert_eq!(stats.dropped, 6, "{stats:?}");
    assert_eq!(consumer.len(), 1);
}

#[test]
fn the_webassembly_exports_never_allocate_while_rendering() {
    let engine = ffi::mb_engine_new(48_000.0, 128, 4, 1_024, 64);
    assert!(!engine.is_null());
    // SAFETY: `engine` came from `mb_engine_new` and is freed only at the
    // end; the staging area holds 64 records, and the records written fit.
    unsafe {
        assert_eq!(ffi::mb_create_part(engine, 0, 1, 3), ffi::status::OK);
        assert_eq!(ffi::mb_create_part(engine, 1, 0, 3), ffi::status::OK);
        // Parts 12 and 15 of `events`: an organ and an acid bass.
        let (organ, acid) = (InstrumentKind::Organ.code(), InstrumentKind::Acid.code());
        assert_eq!(ffi::mb_create_part(engine, 12, 1, 3), ffi::status::OK);
        assert_eq!(ffi::mb_set_instrument(engine, 12, organ), ffi::status::OK);
        assert_eq!(ffi::mb_create_part(engine, 15, acid, 3), ffi::status::OK);
        let staging = ffi::mb_staging_ptr(engine);
        let records = events(0);
        let calls = heap_calls(|| {
            for (number, chunk) in records.chunks(64).enumerate() {
                for (index, record) in chunk.iter().enumerate() {
                    let bytes = record.to_bytes();
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), staging.add(index * 32), 32);
                }
                if number % 2 == 0 {
                    ffi::mb_push_staged(engine, chunk.len() as u32);
                } else {
                    ffi::mb_admit_staged(engine, chunk.len() as u32);
                }
            }
            for _ in 0..200 {
                ffi::mb_render(engine, 128);
                ffi::mb_output_ptr(engine, 0);
                ffi::mb_output_ptr(engine, 1);
                ffi::mb_pending_room(engine);
                ffi::mb_frame(engine);
                ffi::mb_stat(engine, 4);
                ffi::mb_latency_frames(engine);
            }
        });
        assert_eq!(calls, 0, "the exports touched the heap {calls} times");
        assert!(ffi::mb_stat(engine, 4) > 200.0);
        ffi::mb_engine_free(engine);
    }
}
