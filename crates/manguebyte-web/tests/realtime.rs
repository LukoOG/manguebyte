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
    Chain, Drum, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, event_ring, ids,
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
    records.push(EventRecord::note_on(start, 9, 60, 1.0)); // no such part
    records.push(EventRecord {
        kind: 200,
        ..EventRecord::default()
    }); // ignored
    records
}

/// How many parts `parts` makes.
const PARTS: u16 = 8;

/// Both instruments through every chain: parts 0 to 5; part 6, a sampler
/// with an instrument; part 7, a sampler kit with one.
fn parts(engine: &mut Engine) {
    let lowpass = Chain {
        lowpass: true,
        reverb: false,
    };
    let reverb = Chain {
        lowpass: false,
        reverb: true,
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
    let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
    parts(&mut engine);
    let (mut producer, mut consumer) = event_ring(1_024);
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
