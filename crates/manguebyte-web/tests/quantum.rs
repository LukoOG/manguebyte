//! The output does not depend on how the frames are cut into quanta.

mod common;

use common::*;
use manguebyte_web::{Chain, Engine, EngineConfig, EventRecord, InstrumentKind, ids};

const FRAMES: usize = 180_000;
/// Where every part has been silent long enough for the reverbs to sleep.
const QUIET_CHECK: usize = 98_000;

/// Two bars of drums and bass, chords, parameter moves, a choke and an
/// all-notes-off, then silence, then one more hit.
fn scenario() -> Engine {
    let mut engine = engine();
    engine.create_part(0, InstrumentKind::Drums).unwrap();
    engine.create_part(1, InstrumentKind::Synth).unwrap();
    engine
        .create_part_with(
            5,
            InstrumentKind::Synth,
            Chain {
                lowpass: true,
                reverb: false,
                ..Chain::NONE
            },
        )
        .unwrap();
    let step = 1_500u64;
    let mut records = vec![
        EventRecord::param(0, 0, id(ids::REVERB_DECAY), 0.3),
        EventRecord::param(0, 1, id(ids::REVERB_DECAY), 0.3),
        EventRecord::param(0, 1, id(ids::REVERB_MIX), 0.4),
        EventRecord::param(0, 5, id(ids::PAN), -0.5),
        EventRecord::param(0, 0, id(ids::KIT_DECAY), 0.25),
        EventRecord::param(0, 0, id(ids::GAIN), -6.0),
        EventRecord::param(0, 1, id(ids::GAIN), -6.0),
    ];
    // Capitals are accents, as Aratu writes them. The second line has the
    // side stick and a fill down the toms.
    for drums in [
        "B.c.S.coB.c.s.cXbbC.s.crB.cOs.c.",
        "....k.......k.......TtmMff.F....",
    ] {
        for (index, symbol) in drums.chars().enumerate() {
            if let Some(drum) = manguebyte_web::Drum::from_symbol(symbol.to_ascii_lowercase()) {
                let velocity = if index % 4 == 0 { 1.0 } else { 0.7 };
                records.push(
                    EventRecord::note_on(index as u64 * step / 2, 0, drum.note(), velocity)
                        .with_accent(symbol.is_ascii_uppercase()),
                );
            }
        }
    }
    // The guitar tap has no symbol; it is struck by its note.
    records.push(EventRecord::note_on(5_000, 0, 88, 0.9));
    let bass = [33u8, 33, 45, 33, 36, 38, 40, 43];
    for (index, &note) in bass.iter().enumerate() {
        let start = index as u64 * step * 2 + 3;
        records.push(EventRecord::note_on(start, 1, note, 0.9).with_accent(index % 3 == 1));
        records.push(EventRecord::note_off(start + step, 1, note));
        records.push(EventRecord::param(
            start + 100,
            1,
            id(ids::SYNTH_CUTOFF),
            300.0 + 200.0 * index as f64,
        ));
    }
    for (index, &note) in [57u8, 60, 64, 67].iter().enumerate() {
        records.push(
            EventRecord::note_on(2_000 + index as u64, 5, note, 0.5).with_channel(index as u8),
        );
    }
    records.extend([
        EventRecord::param(4_321, 5, id(ids::LOWPASS_CUTOFF), 900.0),
        EventRecord::param(9_999, 5, id(ids::LOWPASS_RESONANCE), 0.8),
        EventRecord::param(11_000, 5, id(ids::GAIN), -6.0),
        EventRecord::all_notes_off(17_000, 5),
        EventRecord::note_on(19_000, 5, 72, 1.0),
        EventRecord::choke(20_500, 5),
        EventRecord::param(12_000, 0, id(ids::KIT_TUNE), -3.0),
        // After the silence: one more hit, to wake the reverb.
        EventRecord::note_on(100_001, 0, 38, 1.0),
        EventRecord::note_on(100_001, 1, 50, 1.0),
        EventRecord::note_off(106_000, 1, 50),
    ]);
    push_all(&mut engine, &records);
    engine
}

fn assert_same(name: &str, a: &(Vec<f32>, Vec<f32>), b: &(Vec<f32>, Vec<f32>)) {
    assert_eq!(a.0.len(), b.0.len());
    for (channel, (x, y)) in [(&a.0, &b.0), (&a.1, &b.1)].into_iter().enumerate() {
        let first = x
            .iter()
            .zip(y)
            .position(|(x, y)| x.to_bits() != y.to_bits());
        assert_eq!(
            first, None,
            "{name}: channel {channel} differs first at frame {first:?}"
        );
    }
}

#[test]
fn quanta_of_1_7_128_and_512_give_the_same_samples() {
    let reference = render(&mut scenario(), FRAMES, 128);
    assert!(peak(&reference.0) > 0.2 && peak(&reference.1) > 0.2);
    assert!(peak(&reference.0) < 1.0);
    // The sound is stereo: the reverbs and the pan see to that.
    assert_ne!(bits(&reference.0), bits(&reference.1));

    for quantum in [1usize, 7, 512] {
        let output = render(&mut scenario(), FRAMES, quantum);
        assert_same(&format!("quantum {quantum}"), &output, &reference);
    }

    // Quanta of changing sizes, and one call for everything.
    let mut engine = scenario();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut output = (Vec::new(), Vec::new());
    while output.0.len() < FRAMES {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let quantum = 1 + (seed % 512) as usize;
        let (left, right) = render(&mut engine, quantum.min(FRAMES - output.0.len()), quantum);
        output.0.extend(left);
        output.1.extend(right);
    }
    assert_same("random quanta", &output, &reference);

    let mut engine = scenario();
    let mut left = vec![0.0; FRAMES];
    let mut right = vec![0.0; FRAMES];
    engine.render_into(&mut left, &mut right);
    assert_same("render_into", &(left, right), &reference);
    assert_eq!(engine.frame(), FRAMES as u64);
}

/// A note of `hold` seconds through a fully wet reverb at `rate`. Returns
/// the left channel, the frame from which the dry note has certainly
/// ended, and whether the reverb was asleep at the end.
fn note_through_a_wet_reverb(rate: f64, hold: f64) -> (Vec<f32>, usize, Option<bool>) {
    let mut engine = Engine::new(EngineConfig::new(rate)).unwrap();
    engine
        .create_part_with(
            0,
            InstrumentKind::Synth,
            Chain {
                lowpass: false,
                reverb: true,
                ..Chain::NONE
            },
        )
        .unwrap();
    let at = |seconds: f64| (seconds * rate).round() as u64;
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::REVERB_MIX), 1.0),
            EventRecord::param(0, 0, id(ids::REVERB_DECAY), 0.2),
            EventRecord::param(0, 0, id(ids::SYNTH_ATTACK), 0.001),
            EventRecord::param(0, 0, id(ids::SYNTH_RELEASE), 0.005),
            // The mix ramp (20 ms) is over long before the note.
            EventRecord::note_on(at(0.060), 0, 57, 1.0),
            EventRecord::note_off(at(0.060 + hold), 0, 57),
        ],
    );
    let (left, _) = render(&mut engine, at(1.5) as usize, 128);
    // Attack 1 ms, then a 5 ms release to -80 dB: the dry note is over by
    // the note-off + 7 ms. Leave a margin.
    (left, at(0.074 + hold) as usize, engine.reverb_asleep(0))
}

/// The reverb is put to sleep after a stretch of quiet output, and nothing
/// comes out of it until a sound has been round its shortest delay line.
/// That line holds more frames at higher rates; when the quiet stretch was
/// a fixed 4 096 frames, a 1 ms note at 384 kHz lost its whole tail. At
/// every rate a short note keeps its tail, and the reverb still falls
/// asleep afterwards.
#[test]
fn a_short_note_keeps_its_reverb_tail_at_every_sample_rate() {
    for rate in [
        EngineConfig::MIN_SAMPLE_RATE,
        8_000.0,
        44_100.0,
        48_000.0,
        96_000.0,
        192_000.0,
        384_000.0,
        EngineConfig::MAX_SAMPLE_RATE,
    ] {
        for hold in [0.001, 0.1] {
            let (left, dry_over, asleep) = note_through_a_wet_reverb(rate, hold);
            let tail = peak(&left[dry_over..]);
            assert!(tail > 1e-3, "{rate} Hz, {hold} s: no tail, peak {tail}");
            assert_eq!(asleep, Some(true), "{rate} Hz, {hold} s");
        }
    }
}

#[test]
fn the_reverbs_sleep_in_silence_and_wake_for_sound() {
    let mut engine = scenario();
    let (_, _) = render(&mut engine, QUIET_CHECK, 128);
    assert_eq!(engine.reverb_asleep(0), Some(true));
    assert_eq!(engine.reverb_asleep(1), Some(true));
    assert_eq!(engine.active_voices(0), Some(0));
    assert_eq!(engine.active_voices(1), Some(0));
    let (left, _) = render(&mut engine, 100_001 - QUIET_CHECK, 128);
    assert!(left.iter().all(|&sample| sample == 0.0));
    render(&mut engine, 100, 100);
    assert_eq!(engine.reverb_asleep(0), Some(false));
    assert_eq!(engine.reverb_asleep(1), Some(false));
    render(&mut engine, FRAMES - 100_101, 512);
    assert_eq!(engine.reverb_asleep(0), Some(true));
}
