//! The WebAssembly exports, called natively: null pointers, bad arguments,
//! hostile records, and calls in random orders.

use manguebyte_web::ffi::{self, MAX_STAGING_RECORDS, WebEngine, status};
use manguebyte_web::{
    Chain, Engine, EngineConfig, EventRecord, InstrumentKind, OUTPUT_CEILING, ParamId,
    drum_map_json, ids,
};

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The text the last text call wrote.
///
/// # Safety
///
/// `engine` is live and `start` is what that text call returned.
unsafe fn text(engine: *mut WebEngine, start: *const u8) -> String {
    // SAFETY: the caller passes a live engine.
    let len = unsafe { ffi::mb_text_len(engine) } as usize;
    // SAFETY: the text buffer holds `len` bytes from `start` until the next
    // text call, and nothing else runs meanwhile.
    let bytes = unsafe { std::slice::from_raw_parts(start, len) };
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The two output channels of the last render.
///
/// # Safety
///
/// `engine` is live, and `frames` is at most what the last render returned.
unsafe fn output(engine: *mut WebEngine, frames: usize) -> (Vec<f32>, Vec<f32>) {
    // SAFETY: the caller passes a live engine.
    let (left, right) = unsafe { (ffi::mb_output_ptr(engine, 0), ffi::mb_output_ptr(engine, 1)) };
    assert!(!left.is_null() && !right.is_null());
    // SAFETY: each output buffer holds at least `frames` rendered samples
    // and does not move until the engine is freed.
    unsafe {
        (
            std::slice::from_raw_parts(left, frames).to_vec(),
            std::slice::from_raw_parts(right, frames).to_vec(),
        )
    }
}

/// Copies records into the staging area and pushes them.
///
/// # Safety
///
/// `engine` is live and the staging area holds `records.len()` records.
unsafe fn push(engine: *mut WebEngine, records: &[EventRecord]) -> u32 {
    // SAFETY: as the caller promises.
    let staging = unsafe { ffi::mb_staging_ptr(engine) };
    for (index, record) in records.iter().enumerate() {
        // SAFETY: slot `index` is inside the staging area, as promised.
        unsafe {
            core::ptr::copy_nonoverlapping(record.to_bytes().as_ptr(), staging.add(index * 32), 32);
        }
    }
    // SAFETY: as the caller promises.
    unsafe { ffi::mb_push_staged(engine, records.len() as u32) }
}

#[test]
fn a_null_engine_is_refused_by_every_export() {
    let null = core::ptr::null_mut();
    assert_eq!(ffi::mb_abi_version(), ffi::ABI_VERSION);
    assert_eq!(ffi::mb_record_size(), 32);
    // SAFETY: a null engine is allowed everywhere.
    unsafe {
        ffi::mb_engine_free(null);
        assert!(ffi::mb_staging_ptr(null).is_null());
        assert_eq!(ffi::mb_staging_capacity(null), 0);
        assert_eq!(ffi::mb_push_staged(null, 5), 0);
        assert_eq!(ffi::mb_pending_room(null), 0);
        ffi::mb_clear_pending(null);
        assert_eq!(ffi::mb_render(null, 128), 0);
        assert!(ffi::mb_output_ptr(null, 0).is_null());
        assert!(ffi::mb_frame(null).is_nan());
        assert_eq!(ffi::mb_create_part(null, 0, 0, 0), status::NO_ENGINE);
        assert_eq!(ffi::mb_remove_part(null, 0), status::NO_ENGINE);
        assert_eq!(ffi::mb_set_chain(null, 0, 0), status::NO_ENGINE);
        assert!(ffi::mb_params_json(null, 0).is_null());
        assert!(ffi::mb_drum_map_json(null).is_null());
        assert_eq!(ffi::mb_text_len(null), 0);
        assert!(ffi::mb_param_value(null, 0, 0).is_nan());
        assert_eq!(ffi::mb_active_voices(null, 0), status::NO_ENGINE);
        assert!(ffi::mb_stat(null, 0).is_nan());
    }
}

#[test]
fn engines_with_arguments_out_of_range_are_not_made() {
    for (rate, quantum, parts, pending, staging) in [
        (f64::NAN, 128, 4, 64, 64),
        (f64::INFINITY, 128, 4, 64, 64),
        (2_999.0, 128, 4, 64, 64),
        (768_001.0, 128, 4, 64, 64),
        (48_000.0, 0, 4, 64, 64),
        (48_000.0, 8_193, 4, 64, 64),
        (48_000.0, 128, 0, 64, 64),
        (48_000.0, 128, 1_025, 64, 64),
        (48_000.0, 128, 4, 0, 64),
        (48_000.0, 128, 4, (1 << 20) + 1, 64),
        (48_000.0, 128, 4, 64, 0),
        (48_000.0, 128, 4, 64, MAX_STAGING_RECORDS + 1),
        (48_000.0, u32::MAX, u32::MAX, u32::MAX, u32::MAX),
    ] {
        let engine = ffi::mb_engine_new(rate, quantum, parts, pending, staging);
        assert!(
            engine.is_null(),
            "{rate} {quantum} {parts} {pending} {staging}"
        );
    }
    assert_eq!(ffi::mb_min_sample_rate(), 3_000.0);
    assert_eq!(ffi::mb_max_sample_rate(), 768_000.0);
    for rate in [ffi::mb_min_sample_rate(), ffi::mb_max_sample_rate()] {
        let engine = ffi::mb_engine_new(rate, 1, 1, 1, 1);
        assert!(!engine.is_null(), "{rate}");
        // SAFETY: `engine` is live and freed once.
        unsafe {
            assert_eq!(ffi::mb_staging_capacity(engine), 1);
            assert_eq!(ffi::mb_render(engine, 1_000), 1);
            ffi::mb_engine_free(engine);
        }
    }
}

#[test]
fn control_calls_report_their_status() {
    let engine = ffi::mb_engine_new(44_100.0, 128, 2, 64, 8);
    // SAFETY: `engine` is live until it is freed at the end.
    unsafe {
        assert_eq!(ffi::mb_create_part(engine, 65_536, 0, 0), status::BAD_PART);
        assert_eq!(ffi::mb_create_part(engine, 0, 2, 0), status::BAD_INSTRUMENT);
        assert_eq!(ffi::mb_create_part(engine, 0, 0, 4), status::BAD_CHAIN);
        assert_eq!(ffi::mb_create_part(engine, 65_535, 0, 3), status::OK);
        assert_eq!(ffi::mb_create_part(engine, 0, 1, 1), status::OK);
        assert_eq!(ffi::mb_create_part(engine, 1, 1, 1), status::TOO_MANY_PARTS);
        assert_eq!(ffi::mb_create_part(engine, 0, 0, 2), status::OK); // replaces
        assert_eq!(ffi::mb_set_chain(engine, 0, 8), status::BAD_CHAIN);
        assert_eq!(ffi::mb_set_chain(engine, 70_000, 0), status::BAD_PART);
        assert_eq!(ffi::mb_set_chain(engine, 5, 0), status::NO_SUCH_PART);
        assert_eq!(ffi::mb_set_chain(engine, 0, 3), status::OK);
        assert_eq!(ffi::mb_remove_part(engine, 5), status::NO_SUCH_PART);
        assert_eq!(ffi::mb_remove_part(engine, u32::MAX), status::BAD_PART);
        assert_eq!(ffi::mb_active_voices(engine, 0), 0);
        assert_eq!(ffi::mb_active_voices(engine, 5), -1);
        assert_eq!(ffi::mb_active_voices(engine, u32::MAX), -1);
        assert_eq!(ffi::mb_remove_part(engine, 65_535), status::OK);
        assert_eq!(ffi::mb_active_voices(engine, 65_535), -1);

        let gain = ParamId::from_name(ids::GAIN).0;
        assert_eq!(ffi::mb_param_value(engine, 0, gain), 0.0);
        assert!(ffi::mb_param_value(engine, 0, 12_345).is_nan());
        assert!(ffi::mb_param_value(engine, 9, gain).is_nan());
        assert!(ffi::mb_param_value(engine, u32::MAX, gain).is_nan());

        let json = ffi::mb_params_json(engine, 0);
        assert!(!json.is_null());
        let reference = {
            let mut twin = Engine::new(EngineConfig::new(44_100.0)).unwrap();
            twin.create_part(0, InstrumentKind::Synth).unwrap();
            twin.params_json(0).unwrap()
        };
        assert_eq!(text(engine, json), reference);
        assert!(ffi::mb_params_json(engine, 9).is_null());
        assert_eq!(ffi::mb_text_len(engine), 0);
        assert!(ffi::mb_params_json(engine, 1 << 20).is_null());
        let map = ffi::mb_drum_map_json(engine);
        assert_eq!(text(engine, map), drum_map_json());

        for index in 0..10 {
            assert_eq!(ffi::mb_stat(engine, index), 0.0);
        }
        assert!(ffi::mb_stat(engine, 10).is_nan());
        assert!(ffi::mb_output_ptr(engine, 2).is_null());
        assert!(ffi::mb_output_ptr(engine, u32::MAX).is_null());
        ffi::mb_engine_free(engine);
    }
}

#[test]
fn staged_records_play_exactly_as_pushed_ones() {
    let records: Vec<EventRecord> = (0..40u64)
        .flat_map(|step| {
            let frame = step * 600 + step % 5;
            [
                EventRecord::note_on(frame, 0, [36, 38, 42, 46, 49, 51][step as usize % 6], 0.9),
                EventRecord::note_on(frame + 3, 1, 33 + (step % 12) as u8, 0.8),
                EventRecord::note_off(frame + 400, 1, 33 + (step % 12) as u8),
                EventRecord::param(
                    frame + 9,
                    1,
                    ParamId::from_name(ids::SYNTH_CUTOFF),
                    300.0 + step as f64 * 50.0,
                ),
            ]
        })
        .collect();
    let mut twin = Engine::new(EngineConfig {
        max_quantum: 128,
        max_parts: 4,
        pending_capacity: 1_024,
        ..EngineConfig::new(48_000.0)
    })
    .unwrap();
    twin.create_part_with(0, InstrumentKind::Drums, Chain::FULL)
        .unwrap();
    twin.create_part_with(
        1,
        InstrumentKind::Synth,
        Chain {
            lowpass: true,
            reverb: false,
        },
    )
    .unwrap();
    for &record in &records {
        twin.push(record);
    }

    let engine = ffi::mb_engine_new(48_000.0, 128, 4, 1_024, 16);
    // SAFETY: `engine` is live until it is freed at the end; every push
    // writes at most the 16 records the staging area holds.
    unsafe {
        assert_eq!(ffi::mb_create_part(engine, 0, 1, 3), status::OK);
        assert_eq!(ffi::mb_create_part(engine, 1, 0, 1), status::OK);
        for chunk in records.chunks(16) {
            assert_eq!(push(engine, chunk), chunk.len() as u32);
        }
        assert_eq!(ffi::mb_stat(engine, 0), records.len() as f64);
        // Fewer under Miri, which checks every pointer the exports touch
        // and is thousands of times slower.
        let quanta = if cfg!(miri) { 8 } else { 200 };
        for quantum in 0..quanta {
            let frames = [128u32, 1, 77, 128][quantum % 4];
            assert_eq!(ffi::mb_render(engine, frames), frames);
            twin.render(frames as usize);
            let (left, right) = output(engine, frames as usize);
            let (twin_left, twin_right) = twin.output();
            assert_eq!(left, twin_left);
            assert_eq!(right, twin_right);
        }
        assert_eq!(ffi::mb_frame(engine), twin.frame() as f64);
        assert!(output(engine, 1).0.iter().all(|s| s.is_finite()));
        ffi::mb_engine_free(engine);
    }
}

#[test]
fn hostile_calls_in_random_orders_never_break_the_engine() {
    let mut state = 0xC0FF_EE00_1234_5678u64;
    let mut full_queue_paths = 0;
    // Fewer and shorter under Miri, which checks every pointer the exports
    // touch and is thousands of times slower: one round with each queue.
    let (rounds, calls) = if cfg!(miri) { (2, 120) } else { (12, 1_500) };
    for round in 0..rounds {
        // Every other round has a small queue, which fills up.
        let pending = if round % 2 == 0 { 512 } else { 16 };
        let engine = ffi::mb_engine_new(48_000.0, 256, 6, pending, 32);
        let mut parts = std::collections::BTreeSet::new();
        let mut pushed = 0u64;
        let mut frame = 0u64;
        // SAFETY: `engine` is live until it is freed at the end of the
        // round; every staging write stays inside its 32 records.
        unsafe {
            let staging = ffi::mb_staging_ptr(engine);
            for _ in 0..calls {
                let roll = xorshift(&mut state);
                let part = (roll >> 8) as u32 % 9;
                match roll % 10 {
                    0 => {
                        let kind = (roll >> 16) as u32 % 3;
                        let chain = (roll >> 20) as u32 % 5;
                        let result = ffi::mb_create_part(engine, part, kind, chain);
                        let expected = if kind > 1 {
                            status::BAD_INSTRUMENT
                        } else if chain > 3 {
                            status::BAD_CHAIN
                        } else if !parts.contains(&part) && parts.len() == 6 {
                            status::TOO_MANY_PARTS
                        } else {
                            parts.insert(part);
                            status::OK
                        };
                        assert_eq!(result, expected);
                    }
                    1 => {
                        let expected = if parts.remove(&part) {
                            status::OK
                        } else {
                            status::NO_SUCH_PART
                        };
                        assert_eq!(ffi::mb_remove_part(engine, part), expected);
                    }
                    2 => {
                        let chain = (roll >> 20) as u32 % 4;
                        let expected = if parts.contains(&part) {
                            status::OK
                        } else {
                            status::NO_SUCH_PART
                        };
                        assert_eq!(ffi::mb_set_chain(engine, part, chain), expected);
                    }
                    3..=5 => {
                        // Records: random bytes, or plausible ones near now.
                        let count = (roll >> 24) as u32 % 40; // may exceed the staging area
                        let written = count.min(32) as usize;
                        for slot in 0..written {
                            let mut bytes = [0u8; 32];
                            for chunk in bytes.chunks_exact_mut(8) {
                                chunk.copy_from_slice(&xorshift(&mut state).to_le_bytes());
                            }
                            if roll & 1 == 0 {
                                bytes[8] %= 6;
                                bytes[10] %= 9;
                                bytes[11] = 0;
                                bytes[12] =
                                    [36, 38, 42, 46, 49, 51, 45, 57][bytes[12] as usize % 8];
                                bytes[13] %= 16;
                                let velocity = f32::from(bytes[16]) / 255.0;
                                bytes[16..20].copy_from_slice(&velocity.to_le_bytes());
                                let at = frame.saturating_sub(64) + u64::from(bytes[0]) * 8;
                                bytes[0..8].copy_from_slice(&at.to_le_bytes());
                            }
                            core::ptr::copy_nonoverlapping(
                                bytes.as_ptr(),
                                staging.add(slot * 32),
                                32,
                            );
                        }
                        if roll & (1 << 40) == 0 {
                            let queued = ffi::mb_push_staged(engine, count);
                            assert!(queued as usize <= written);
                            pushed += written as u64;
                        } else {
                            // Takes records until the full queue would
                            // drop one; the rest are not pushed.
                            let taken = ffi::mb_admit_staged(engine, count);
                            assert!(taken as usize <= written);
                            pushed += u64::from(taken);
                        }
                    }
                    6 => ffi::mb_clear_pending(engine),
                    7 => {
                        let json = ffi::mb_params_json(engine, part);
                        assert_eq!(json.is_null(), !parts.contains(&part));
                        if !json.is_null() {
                            assert!(text(engine, json).starts_with(r#"[{"id":""#));
                        }
                        assert_eq!(
                            ffi::mb_active_voices(engine, part) < 0,
                            !parts.contains(&part)
                        );
                    }
                    _ => {
                        let frames = (roll >> 32) as u32 % 600;
                        let rendered = ffi::mb_render(engine, frames);
                        assert_eq!(rendered, frames.min(256));
                        frame += u64::from(rendered);
                        assert_eq!(ffi::mb_frame(engine), frame as f64);
                        let (left, right) = output(engine, rendered as usize);
                        for sample in left.iter().chain(&right) {
                            assert!(sample.is_finite() && sample.abs() <= OUTPUT_CEILING);
                        }
                    }
                }
            }
            let stat = |index| ffi::mb_stat(engine, index) as u64;
            let (queued, ignored, dropped, applied, unrouted, displaced) =
                (stat(0), stat(1), stat(2), stat(4), stat(5), stat(6));
            assert!(displaced <= dropped && displaced <= queued);
            assert_eq!(
                queued + ignored + dropped - displaced,
                pushed,
                "round {round}"
            );
            assert_eq!(stat(8), 0, "round {round}: a part went wrong");
            full_queue_paths += displaced.min(stat(7));
            assert!(applied <= queued && unrouted <= applied && stat(3) <= applied);
            assert!(
                cfg!(miri) || (queued > 0 && ignored > 0 && applied > 0),
                "round {round}: nothing happened"
            );
            ffi::mb_engine_free(engine);
        }
    }
    // The short run under Miri may not fill the small queue.
    assert!(
        full_queue_paths > 0 || cfg!(miri),
        "the queue never filled: no event was pushed out and no admit stopped"
    );
}

/// `mb_admit_staged` takes records until the full queue would drop one,
/// and says how many it took, so the caller can keep the rest.
#[test]
fn admitting_staged_records_stops_where_a_full_queue_would_drop() {
    let engine = ffi::mb_engine_new(48_000.0, 128, 2, 2, 8);
    // SAFETY: `engine` is live until it is freed; at most 8 records are
    // staged.
    unsafe {
        assert_eq!(ffi::mb_admit_staged(core::ptr::null_mut(), 3), 0);
        assert_eq!(ffi::mb_create_part(engine, 0, 1, 0), status::OK);
        let staging = ffi::mb_staging_ptr(engine);
        let ignored = EventRecord {
            kind: 99,
            ..EventRecord::default()
        };
        let records = [
            EventRecord::note_on(900, 0, 36, 1.0),
            EventRecord::note_on(800, 0, 36, 1.0),
            ignored,
            EventRecord::note_on(100, 0, 38, 1.0), // pushes out frame 900
            EventRecord::note_on(5_000, 0, 36, 1.0), // would be dropped: stop
            EventRecord::note_on(10, 0, 36, 1.0),
        ];
        for (index, record) in records.iter().enumerate() {
            core::ptr::copy_nonoverlapping(record.to_bytes().as_ptr(), staging.add(index * 32), 32);
        }
        assert_eq!(ffi::mb_admit_staged(engine, 6), 4);
        let stat = |index| ffi::mb_stat(engine, index);
        // queued, ignored, dropped, displaced, stalled
        assert_eq!(
            [stat(0), stat(1), stat(2), stat(6), stat(7)],
            [3.0, 1.0, 1.0, 1.0, 1.0]
        );
        // Asking for more than the staging area holds takes at most that,
        // and stops at the same record.
        ffi::mb_clear_pending(engine);
        assert_eq!(ffi::mb_admit_staged(engine, u32::MAX), 4);
        assert_eq!(ffi::mb_pending_room(engine), 0);
        assert_eq!(stat(7), 2.0);
        ffi::mb_engine_free(engine);
    }
}
