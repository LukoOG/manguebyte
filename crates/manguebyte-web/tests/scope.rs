//! Each part's scope and levels: what the part puts into the master bus,
//! read between render calls, the same whatever the quanta.

mod common;

use common::*;
use manguebyte_web::{
    Chain, Engine, EventRecord, InstrumentKind, PEAK_RETURN_SECONDS, PartLevels, SCOPE_FRAMES, ids,
};

/// Renders until the engine's output has reached frame `frame`, in quanta
/// of `quantum` at most, and returns the output.
fn render_to(engine: &mut Engine, frame: u64, quantum: usize) -> (Vec<f32>, Vec<f32>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    while engine.frame() < frame {
        let count = quantum.min((frame - engine.frame()) as usize);
        engine.render(count);
        let (l, r) = engine.output();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
    }
    (left, right)
}

#[test]
fn the_scope_is_what_the_part_puts_into_the_master_bus() {
    let tone = sine(330.0, RATE, 30_000, 0.6);
    let mut engine = signal_engine(&tone, Chain::NONE.with(manguebyte_web::Effect::Lowpass));
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::GAIN), -4.0),
            EventRecord::param(0, 0, id(ids::PAN), 0.6),
            EventRecord::param(0, 0, id(ids::LOWPASS_CUTOFF), 900.0),
            EventRecord::note_on(500, 0, 60, 1.0),
        ],
    );
    let (left, right) = render_to(&mut engine, 12_000, 128);
    let ahead = engine.master_latency_frames() as u64;
    assert_eq!(engine.part_scope_end(0), Some(12_000 + ahead));

    let scope = engine.part_scope(0, SCOPE_FRAMES).unwrap();
    assert_eq!(scope.left.len(), SCOPE_FRAMES);
    assert_eq!(scope.frame, 12_000 + ahead - SCOPE_FRAMES as u64);
    // One part, under the limiter's ceiling, through a master bus that
    // leaves it as it is: what came out on a frame is the scope's frame.
    let heard = (12_000 - scope.frame) as usize;
    let from = scope.frame as usize;
    assert_eq!(bits(&scope.left[..heard]), bits(&left[from..]));
    assert_eq!(bits(&scope.right[..heard]), bits(&right[from..]));
    assert!(peak(&scope.left) > 0.01);
    assert_ne!(scope.left, scope.right, "panned");
    // The newest frames have not been heard yet: they come out next.
    let newest = scope.left[heard..].to_vec();
    let (next, _) = render_to(&mut engine, 12_000 + ahead, 128);
    assert_eq!(bits(&newest), bits(&next));

    // Fewer frames are the newest of them.
    let short = engine.part_scope(0, 100).unwrap();
    let all = engine.part_scope(0, SCOPE_FRAMES + 1_000).unwrap();
    assert_eq!(all.left.len(), SCOPE_FRAMES);
    assert_eq!(short.frame, all.frame + (SCOPE_FRAMES - 100) as u64);
    assert_eq!(short.left[..], all.left[SCOPE_FRAMES - 100..]);
    // The same without allocating.
    let mut into_left = [0.0f32; 300];
    let mut into_right = [0.0f32; 200];
    assert_eq!(
        engine.part_scope_into(0, &mut into_left, &mut into_right),
        Some((200, all.frame + (SCOPE_FRAMES - 200) as u64))
    );
    assert_eq!(into_right[..], all.right[SCOPE_FRAMES - 200..]);
    assert_eq!(engine.part_scope(1, 10), None);
    assert_eq!(engine.part_levels(1), None);
}

#[test]
fn the_levels_read_a_steady_tone_and_the_peak_falls_after_it() {
    // Two seconds of a sine at 0.8, and then silence.
    let amplitude = 0.8;
    let tone = sine(1_000.0, RATE, 96_000, amplitude);
    let mut engine = signal_engine(&tone, Chain::NONE);
    push_all(&mut engine, &[EventRecord::note_on(0, 0, 60, 1.0)]);
    let level = f64::from(sampler_level()) * amplitude;
    // Rendered up to the end of the tone, for the parts.
    let ahead = engine.master_latency_frames() as u64;
    render_to(&mut engine, 96_000 - ahead, 512);
    let levels = engine.part_levels(0).unwrap();
    for side in 0..2 {
        let peak = f64::from(levels.peak[side]);
        let rms = f64::from(levels.rms[side]);
        assert!((peak / level - 1.0).abs() < 1e-3, "{levels:?}");
        let expected = level * core::f64::consts::FRAC_1_SQRT_2;
        assert!((20.0 * (rms / expected).log10()).abs() < 0.05, "{levels:?}");
    }
    let [loud, _] = levels.peak_db();
    render_to(
        &mut engine,
        96_000 - ahead + (PEAK_RETURN_SECONDS * RATE) as u64,
        512,
    );
    let [quiet, _] = engine.part_levels(0).unwrap().peak_db();
    assert!((loud - quiet - 20.0).abs() < 0.05, "{loud} {quiet}");
    // And in a long silence, nothing at all.
    render_to(&mut engine, 60 * 48_000, 512);
    assert_eq!(engine.part_levels(0), Some(PartLevels::default()));
}

/// A beat on two parts, one with every effect.
fn beat(engine: &mut Engine) {
    engine
        .create_part_with(0, InstrumentKind::Drums, Chain::FULL)
        .unwrap();
    engine
        .create_part_with(1, InstrumentKind::Synth, Chain::ALL)
        .unwrap();
    let mut records = vec![
        EventRecord::param(0, 1, id(ids::DRIVE_ON), 1.0),
        EventRecord::param(0, 1, id(ids::DRIVE_SHAPE), 4.0),
        EventRecord::param(0, 1, id(ids::DELAY_ON), 1.0),
        EventRecord::param(0, 1, id(ids::CHORUS_ON), 1.0),
        EventRecord::param(0, 0, id(ids::PAN), -0.4),
    ];
    for step in 0..24u64 {
        let frame = 37 + step * 3_001;
        records.push(EventRecord::note_on(
            frame,
            0,
            [36, 38, 42][step as usize % 3],
            0.9,
        ));
        records.push(EventRecord::note_on(
            frame + 11,
            1,
            45 + (step % 7) as u8,
            0.8,
        ));
        records.push(EventRecord::note_off(
            frame + 2_000,
            1,
            45 + (step % 7) as u8,
        ));
    }
    push_all(engine, &records);
}

/// A reading of a part: its scope's left and right channels, as bits, and
/// its levels.
type Reading = (Vec<u32>, Vec<u32>, PartLevels);

#[test]
fn the_scope_and_the_levels_do_not_depend_on_the_quanta() {
    let checkpoints: &[u64] = if cfg!(miri) {
        &[300, 1_000]
    } else {
        &[1_000, 4_801, 20_000, 77_777]
    };
    let mut reference: Option<Vec<Reading>> = None;
    for quantum in [1, 7, 128, 512] {
        let mut engine = engine();
        beat(&mut engine);
        let mut seen = Vec::new();
        for &frame in checkpoints {
            render_to(&mut engine, frame, quantum);
            for part in 0..2 {
                let scope = engine.part_scope(part, SCOPE_FRAMES).unwrap();
                seen.push((
                    bits(&scope.left),
                    bits(&scope.right),
                    engine.part_levels(part).unwrap(),
                ));
            }
        }
        match &reference {
            None => reference = Some(seen),
            Some(reference) => {
                for (index, (got, wanted)) in seen.iter().zip(reference).enumerate() {
                    assert!(got == wanted, "quantum {quantum}, reading {index}");
                }
            }
        }
    }
    let reference = reference.unwrap();
    assert!(
        reference
            .iter()
            .any(|(left, ..)| left.iter().any(|&b| b != 0))
    );
}

#[test]
fn a_part_made_again_keeps_its_scope_and_a_new_part_starts_from_silence() {
    let mut engine = engine();
    engine.create_part(0, InstrumentKind::Drums).unwrap();
    push_all(&mut engine, &[EventRecord::note_on(0, 0, 38, 1.0)]);
    render_to(&mut engine, 2_000, 128);
    let before = engine.part_scope(0, SCOPE_FRAMES).unwrap();
    assert!(peak(&before.left) > 0.01);

    // Made again as a synth, and given another instrument: the frames it
    // played are still there.
    engine.create_part(0, InstrumentKind::Synth).unwrap();
    engine.set_instrument(0, InstrumentKind::Fm).unwrap();
    assert_eq!(engine.part_scope(0, SCOPE_FRAMES), Some(before.clone()));
    let levels = engine.part_levels(0).unwrap();
    assert!(levels.peak[0] > 0.0);

    // A part made now has nothing before this frame.
    let end = engine.part_scope_end(0).unwrap();
    engine.create_part(1, InstrumentKind::Drums).unwrap();
    assert_eq!(engine.part_scope_end(1), Some(end));
    let fresh = engine.part_scope(1, 500).unwrap();
    assert_eq!(fresh.frame, end - 500);
    assert!(fresh.left.iter().all(|&sample| sample == 0.0));
    assert_eq!(engine.part_levels(1), Some(PartLevels::default()));

    // Gone with the part.
    engine.remove_part(0).unwrap();
    assert_eq!(engine.part_scope(0, 10), None);
    assert_eq!(engine.part_scope_end(0), None);
    // Before the first render there are no frames at all.
    let mut new = engine_with(InstrumentKind::Synth, Chain::FULL);
    assert_eq!(new.part_scope_end(0), Some(0));
    assert!(new.part_scope(0, 64).unwrap().left.is_empty());
    new.render(1);
    let ahead = new.master_latency_frames();
    let first = new.part_scope(0, SCOPE_FRAMES).unwrap();
    assert_eq!((first.frame, first.left.len()), (0, ahead + 1));
}
