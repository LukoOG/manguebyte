//! Aratu's Brazilian percussion keys on a drum kit part: every key sounds
//! on every kit preset, as the same instrument at the same level whatever
//! the kit, its line sits at its level against the kit's groove (which is
//! at the loudness target), and it falls silent.
//!
//! Loudness is read as in `tests/loudness.rs`: integrated loudness after
//! ITU-R BS.1770-4 on the part's stereo output, the master gain turned down
//! by [`MASTER_DB`] so that the limiter stays out, the readings put back up
//! by as much.

mod common;

use common::*;
use manguebyte_analysis::LoudnessMeter;
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::ProcessSpec;
use manguebyte_dsp::instruments::percussion;
use manguebyte_web::{Chain, Engine, EventRecord, InstrumentKind, LOUDNESS_TARGET_LUFS, ids};

/// The master gain the measurements run at, and where they start.
const MASTER_DB: f64 = -20.0;
const LEAD_IN: u64 = 19_200;

/// How many kit presets there are.
const KITS: usize = 10;

/// The loudness every piece's line of eighth notes at velocity 0.8 (two
/// bars at 120 beats a minute) reaches, against the target the kit's
/// groove is brought to: from `LINE_LU.0` to `LINE_LU.1` below it. A line
/// of one instrument is quieter than a whole kit's groove; a surdo's,
/// ringing, louder than a shaker's.
const LINE_LU: (f64, f64) = (17.0, 2.0);

/// How far apart the loudness of one piece's line may be from one kit to
/// another.
const ACROSS_KITS_LU: f64 = 4.5;

fn frames(seconds: f64) -> u64 {
    (seconds * RATE).round() as u64
}

/// A drum kit part on preset `kit`, the master turned down from the
/// lead-in on.
fn quiet_kit(kit: usize) -> Engine {
    let mut engine = engine_with(InstrumentKind::DrumKit, Chain::NONE);
    engine
        .create_part_with(1, InstrumentKind::Synth, Chain::NONE)
        .unwrap();
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::DRUM_KIT_PRESET), kit as f64),
            EventRecord::param(0, 1, id(ids::MASTER_GAIN), MASTER_DB),
            EventRecord::param(0, 1, id(ids::SYNTH_RELEASE), 0.005),
            EventRecord::note_on(0, 1, 60, 0.5),
            EventRecord::note_off(96, 1, 60),
        ],
    );
    engine
}

/// The integrated loudness of `key` played as a line of sixteen eighth
/// notes at velocity 0.8 on kit preset `kit`.
fn line_loudness(kit: usize, key: u8) -> f64 {
    let mut engine = quiet_kit(kit);
    for step in 0..16 {
        let at = LEAD_IN + frames(0.25 * f64::from(step));
        push_all(
            &mut engine,
            &[
                EventRecord::note_on(at, 0, key, 0.8),
                EventRecord::note_off(at + frames(0.1), 0, key),
            ],
        );
    }
    let length = frames(4.5) as usize;
    let (left, right) = render(&mut engine, LEAD_IN as usize + length, 512);
    assert_eq!(engine.stats().limited, 0, "the limiter stayed out");
    let mut sound = AudioBuffer::<f32>::new(2, length);
    sound
        .channel_mut(0)
        .copy_from_slice(&left[LEAD_IN as usize..]);
    sound
        .channel_mut(1)
        .copy_from_slice(&right[LEAD_IN as usize..]);
    let mut meter = LoudnessMeter::stereo();
    meter.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: length,
        num_channels: 2,
    });
    meter.measure(&sound);
    meter.readings().integrated() - MASTER_DB
}

#[test]
fn every_percussion_key_plays_on_every_kit_at_its_level() {
    let mut wrong = Vec::new();
    for key in percussion::ALL {
        let levels: Vec<f64> = (0..KITS).map(|kit| line_loudness(kit, key)).collect();
        let (low, high) = levels
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &level| {
                (low.min(level), high.max(level))
            });
        println!(
            "{key:3}: {:?} LUFS",
            levels
                .iter()
                .map(|level| (level * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
        let window = (
            LOUDNESS_TARGET_LUFS - LINE_LU.0,
            LOUDNESS_TARGET_LUFS - LINE_LU.1,
        );
        // The kits' own claps are theirs; the rest is the same instrument.
        let spread = if key == percussion::HAND_CLAP {
            f64::INFINITY
        } else {
            ACROSS_KITS_LU
        };
        if low < window.0 || high > window.1 || high - low > spread {
            wrong.push(format!("{key}: {low:.1} to {high:.1} LUFS"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn every_percussion_key_falls_silent_on_every_kit() {
    for kit in 0..KITS {
        let mut engine = engine_with(InstrumentKind::DrumKit, Chain::NONE);
        engine.push(EventRecord::param(
            0,
            0,
            id(ids::DRUM_KIT_PRESET),
            kit as f64,
        ));
        for (step, &key) in percussion::ALL.iter().enumerate() {
            engine.push(EventRecord::note_on(step as u64 * 4_800, 0, key, 1.0).with_accent(true));
        }
        let (left, right) = render(&mut engine, 2 * RATE as usize, 128);
        assert!(peak(&left) > 0.01 && peak(&right) > 0.01, "kit {kit}");
        let mut seconds = 2;
        while engine.active_voices(0) != Some(0) && seconds < 30 {
            render(&mut engine, RATE as usize, 128);
            seconds += 1;
        }
        assert_eq!(
            engine.active_voices(0),
            Some(0),
            "kit {kit}: still sounding"
        );
    }
}
