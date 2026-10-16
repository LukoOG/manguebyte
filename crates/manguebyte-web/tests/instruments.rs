//! Every instrument makes sound, stays within full scale, and stops; and
//! each one sounds like what it is meant to be.

mod common;

use common::*;
use manguebyte_web::{
    Chain, DRUM_MAP, Drum, Engine, EngineConfig, EngineStats, EventRecord, InstrumentKind,
    LIMITER_RELEASE_SECONDS, OUTPUT_CEILING, Pushed, ids,
};

fn rms(samples: &[f32]) -> f64 {
    (samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

/// Sign changes per second: high for noise and cymbals, low for a kick.
fn crossings_per_second(samples: &[f32]) -> f64 {
    let changes = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    changes as f64 * RATE / samples.len() as f64
}

/// Magnitude of the component at `frequency`, by Goertzel's algorithm.
fn magnitude(samples: &[f32], frequency: f64) -> f64 {
    let coefficient = 2.0 * (core::f64::consts::TAU * frequency / RATE).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &sample in samples {
        let s0 = f64::from(sample) + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coefficient * s1 * s2).sqrt() / samples.len() as f64
}

/// Plays one note for `seconds`, released after `hold` frames, and returns
/// both channels and the voices still sounding at the end.
fn strike(
    kind: InstrumentKind,
    chain: Chain,
    note: u8,
    velocity: f32,
    hold: u64,
    seconds: f64,
) -> (Vec<f32>, Vec<f32>, usize) {
    let mut engine = engine_with(kind, chain);
    push_all(
        &mut engine,
        &[
            EventRecord::note_on(0, 0, note, velocity),
            EventRecord::note_off(hold, 0, note),
        ],
    );
    let (left, right) = render(&mut engine, (seconds * RATE) as usize, 128);
    (left, right, engine.active_voices(0).unwrap())
}

#[test]
fn every_drum_sounds_within_full_scale_and_ends() {
    for entry in DRUM_MAP {
        for chain in [Chain::NONE, Chain::FULL] {
            let (left, right, voices) =
                strike(InstrumentKind::Drums, chain, entry.note, 1.0, 100, 6.0);
            let name = entry.name;
            assert_eq!(onset(&left), Some(0), "{name}");
            let loud = peak(&left);
            assert!(loud > 0.2 && loud < 0.9, "{name}: peak {loud}");
            assert!(peak(&right) < 0.9, "{name}");
            assert!(
                rms(&left[..4_800]) > 0.02,
                "{name}: rms {}",
                rms(&left[..4_800])
            );
            assert!(left.iter().all(|sample| sample.is_finite()));
            // Every drum has died away within six seconds.
            assert_eq!(voices, 0, "{name} still sounding");
            if chain == Chain::NONE {
                assert!(
                    left[left.len() - 1_000..].iter().all(|&s| s == 0.0),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn the_drums_have_their_own_characters() {
    let mut brightness = Vec::new();
    let mut length = Vec::new();
    for drum in Drum::ALL {
        let (left, _, _) = strike(InstrumentKind::Drums, Chain::NONE, drum.note(), 1.0, 1, 6.0);
        brightness.push(crossings_per_second(&left[..2_400]));
        // How long until the level stays below -60 dB of the peak.
        let floor = peak(&left) * 1e-3;
        length.push(left.iter().rposition(|s| s.abs() > floor).unwrap_or(0) as f64 / RATE);
    }
    let [kick, snare, closed, open, ride, crash] = [0, 1, 2, 3, 4, 5];
    // The kick is low, the snare in between, the cymbals bright.
    assert!(brightness[kick] < 400.0, "{brightness:?}");
    assert!(brightness[snare] > 2.0 * brightness[kick], "{brightness:?}");
    for cymbal in [closed, open, ride, crash] {
        assert!(brightness[cymbal] > 5_000.0, "{brightness:?}");
    }
    // The closed hat is short, the open hat longer, the crash and ride ring.
    assert!(length[closed] < 0.25, "{length:?}");
    assert!(length[open] > 2.0 * length[closed], "{length:?}");
    assert!(length[ride] > 1.0 && length[crash] > 1.0, "{length:?}");
    assert!(length[kick] < 1.2 && length[snare] < 0.6, "{length:?}");
}

#[test]
fn the_synth_sounds_within_full_scale_over_its_range() {
    for note in [24u8, 33, 45, 57, 69, 81, 96] {
        for chain in [Chain::NONE, Chain::FULL] {
            let (left, right, voices) =
                strike(InstrumentKind::Synth, chain, note, 1.0, 24_000, 3.0);
            let loud = peak(&left).max(peak(&right));
            assert!(loud > 0.2 && loud < 0.8, "note {note}: peak {loud}");
            assert!(rms(&left[..24_000]) > 0.03, "note {note}");
            // Released at 0.5 s with a 0.25 s release: silent well before 3 s.
            assert_eq!(voices, 0, "note {note}");
            if chain == Chain::NONE {
                assert!(
                    left[RATE as usize * 2..].iter().all(|&s| s == 0.0),
                    "note {note}"
                );
            }
        }
    }
}

#[test]
fn the_synth_note_is_rich_and_moves_like_an_analogue_one() {
    // A2, 110 Hz.
    let (left, _, _) = strike(InstrumentKind::Synth, Chain::NONE, 45, 0.9, 48_000, 1.5);
    let window = &left[480..480 + 4_800];
    let harmonics: Vec<f64> = (1..=5)
        .map(|n| magnitude(window, 110.0 * n as f64))
        .collect();
    // A saw-like spectrum: every harmonic there, falling with order.
    assert!(harmonics[1] > 0.25 * harmonics[0], "{harmonics:?}");
    assert!(harmonics[2] > 0.1 * harmonics[0], "{harmonics:?}");
    assert!(harmonics[4] > 0.02 * harmonics[0], "{harmonics:?}");
    // The sub-oscillator an octave down.
    assert!(magnitude(window, 55.0) > 0.1 * harmonics[0], "sub");
    // The attack is quick: the note is at full level within 20 ms.
    let attack = peak(&left[..960]);
    assert!(attack > 0.8 * peak(&left), "attack {attack}");
    // The filter envelope closes: brighter at the start than later on.
    let brightness = |window: &[f32]| {
        let upper: f64 = (12..=24).map(|n| magnitude(window, 110.0 * n as f64)).sum();
        upper / magnitude(window, 110.0)
    };
    let early = brightness(&left[240..240 + 2_400]);
    let late = brightness(&left[24_000..26_400]);
    assert!(early > 2.0 * late, "early {early} late {late}");
    // Decay to the sustain level: quieter after 1 s than at the start.
    assert!(rms(&left[48_000 - 4_800..48_000]) < 0.8 * rms(&left[..4_800]));
    // The two oscillators are detuned: the level beats, so it is not flat.
    let blocks: Vec<f64> = left[30_000..48_000].chunks(1_200).map(rms).collect();
    let (low, high) = blocks.iter().fold((f64::MAX, 0.0f64), |(low, high), &x| {
        (low.min(x), high.max(x))
    });
    assert!(high > 1.02 * low, "flat level {low} {high}");
}

/// The loudest settings of two parts together, far beyond full scale
/// before the output limiter: the limiter holds them under
/// [`OUTPUT_CEILING`] without clipping. Before the limiter, the engine
/// clamped every sample to full scale and 87 369 of these 96 000 samples
/// sat at exactly ±1, a square wave; the old version of this test only
/// checked the clamp.
#[test]
fn the_loudest_settings_are_held_under_the_ceiling_not_clipped() {
    let mut engine = engine();
    engine.create_part(0, InstrumentKind::Synth).unwrap();
    engine.create_part(1, InstrumentKind::Drums).unwrap();
    let mut records = Vec::new();
    for part in [0, 1] {
        records.push(EventRecord::param(0, part, id(ids::GAIN), 12.0));
        records.push(EventRecord::param(0, part, id(ids::LOWPASS_RESONANCE), 1.0));
        records.push(EventRecord::param(0, part, id(ids::LOWPASS_CUTOFF), 200.0));
        records.push(EventRecord::param(0, part, id(ids::REVERB_MIX), 1.0));
        records.push(EventRecord::param(0, part, id(ids::REVERB_DECAY), 10.0));
    }
    records.push(EventRecord::param(0, 0, id(ids::SYNTH_RESONANCE), 1.0));
    for note in [24u8, 36, 40, 43, 48, 52, 55, 60, 64] {
        records.push(EventRecord::note_on(0, 0, note, 1.0));
    }
    for drum in Drum::ALL {
        for hit in 0..8 {
            records.push(EventRecord::note_on(hit * 3_000, 1, drum.note(), 1.0));
        }
    }
    push_all(&mut engine, &records);
    let (left, right) = render(&mut engine, 96_000, 128);
    let stats = engine.stats();
    assert!(peak(&left) > 0.9, "the test is not loud enough");
    assert!(
        stats.limited > 90_000,
        "the limiter was not needed: {stats:?}"
    );
    assert_eq!(stats.recovered, 0);
    for sample in left.iter().chain(&right) {
        assert!(sample.is_finite() && sample.abs() <= OUTPUT_CEILING);
    }
    // Held under the ceiling, not flattened against it: few samples sit on
    // it, and never for long, where clipping held 91% of them at full scale
    // in long flat runs.
    for channel in [&left, &right] {
        let mut pinned = 0;
        let mut run = 0;
        let mut longest = 0;
        for sample in channel.iter() {
            if sample.abs() == OUTPUT_CEILING {
                pinned += 1;
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        assert!(
            pinned < channel.len() / 20,
            "{pinned} of {} samples on the ceiling",
            channel.len()
        );
        // Under 2 ms.
        assert!(longest < 96, "{longest} samples in a row on the ceiling");
    }
}

/// Renders `records` (frame 0, parts 0 and 1) through the parts given, at
/// their default settings, and returns the left channel and the stats.
fn mix(parts: &[(u16, InstrumentKind)], records: &[EventRecord]) -> (Vec<f32>, EngineStats) {
    let mut engine = engine();
    for &(part, kind) in parts {
        engine.create_part(part, kind).unwrap();
    }
    for &record in records {
        if parts.iter().any(|&(part, _)| part == record.part) {
            assert_eq!(engine.push(record), Pushed::Queued);
        }
    }
    let (left, _) = render(&mut engine, 24_000, 128);
    (left, engine.stats())
}

/// Two parts at their default settings: three drums and a three-note chord
/// struck together at full velocity. Each part alone stays under the
/// ceiling and is left alone; together they go beyond it, and the limiter
/// turns them down gently, by at most 6 dB, keeping the shape of the
/// waveform. Before the limiter, 90 of these samples were clipped.
#[test]
fn two_parts_at_default_levels_are_limited_gently_not_clipped() {
    let records = [
        EventRecord::note_on(0, 0, 36, 1.0),
        EventRecord::note_on(0, 0, 38, 1.0),
        EventRecord::note_on(0, 0, 49, 1.0),
        EventRecord::note_on(0, 1, 45, 1.0),
        EventRecord::note_on(0, 1, 52, 1.0),
        EventRecord::note_on(0, 1, 57, 1.0),
    ];
    let (drums, drum_stats) = mix(&[(0, InstrumentKind::Drums)], &records);
    let (synth, synth_stats) = mix(&[(1, InstrumentKind::Synth)], &records);
    assert_eq!((drum_stats.limited, synth_stats.limited), (0, 0));
    assert!(peak(&drums) < OUTPUT_CEILING && peak(&synth) < OUTPUT_CEILING);

    let (both, stats) = mix(
        &[(0, InstrumentKind::Drums), (1, InstrumentKind::Synth)],
        &records,
    );
    assert!(stats.limited > 0, "the mix did not need the limiter");
    // The most the gain can rise in one frame, as it comes back to 1.
    let release = 1.0 - (-1.0 / (LIMITER_RELEASE_SECONDS * RATE)).exp();
    let mut lowest_gain = 1.0f64;
    let mut last_gain: Option<f64> = None;
    for (frame, ((&both, &drums), &synth)) in both.iter().zip(&drums).zip(&synth).enumerate() {
        // The engine adds the parts in this order, so this is exactly what
        // reached the limiter.
        let sum = 0.0f32 + drums + synth;
        assert!(both.abs() <= OUTPUT_CEILING);
        assert!(both.abs() < 1.0, "clipped");
        if sum.abs() > 1e-3 {
            let gain = f64::from(both) / f64::from(sum);
            assert!(gain > 0.0 && gain <= 1.0 + 1e-6, "{gain}");
            // A gain, not a clip: it falls at once but rises only slowly,
            // where clipping would let it jump back to 1 the moment the
            // wave falls under the ceiling.
            if let Some(last) = last_gain {
                let most = last + (1.0 - last) * release + 1e-5;
                assert!(
                    gain <= most,
                    "frame {frame}: the gain rose from {last} to {gain}"
                );
            }
            last_gain = Some(gain);
            lowest_gain = lowest_gain.min(gain);
        } else {
            last_gain = None;
        }
    }
    let raw_peak = drums
        .iter()
        .zip(&synth)
        .map(|(&drums, &synth)| (drums + synth).abs())
        .fold(0.0f32, f32::max);
    assert!(raw_peak > 1.0, "the mix is not loud enough: {raw_peak}");
    assert!(
        lowest_gain > 0.5,
        "turned down by more than 6 dB: {lowest_gain}"
    );
}

#[test]
fn velocity_scales_the_level() {
    for (kind, note) in [(InstrumentKind::Synth, 57u8), (InstrumentKind::Drums, 38)] {
        let (loud, _, _) = strike(kind, Chain::NONE, note, 1.0, 10_000, 0.5);
        let (soft, _, _) = strike(kind, Chain::NONE, note, 0.25, 10_000, 0.5);
        let ratio = rms(&soft) / rms(&loud);
        assert!(ratio > 0.1 && ratio < 0.4, "{kind:?}: {ratio}");
    }
}

/// Every value an event can carry, for every parameter of every part, in
/// a random order with notes at the extremes of velocity: no part ever
/// produces a sample that is not a finite number, so the engine never has
/// to silence one ([`EngineStats::recovered`]).
#[test]
fn no_value_an_event_can_carry_makes_a_part_go_wrong() {
    let extremes = [
        f64::MAX,
        -f64::MAX,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        0.0,
        -0.0,
        0.5,
        12.0,
        -60.0,
        20_000.0,
    ];
    let chains = [
        Chain::NONE,
        Chain::FULL,
        Chain {
            lowpass: true,
            reverb: false,
        },
        Chain {
            lowpass: false,
            reverb: true,
        },
    ];
    let mut seed = 0x6A09_E667_F3BC_C909u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut engine = engine();
    let mut part = 0u16;
    for kind in [InstrumentKind::Synth, InstrumentKind::Drums] {
        for chain in chains {
            engine.create_part_with(part, kind, chain).unwrap();
            part += 1;
        }
    }
    let mut ignored = 0;
    for frame in (0..240_000u64).step_by(300) {
        let target = (next() % u64::from(part)) as u16;
        for _ in 0..3 {
            let name = ids::ALL[(next() % ids::ALL.len() as u64) as usize];
            let value = extremes[(next() % extremes.len() as u64) as usize];
            engine.push(EventRecord::param(frame, target, id(name), value));
        }
        // Not finite: refused before they reach a part.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let pushed = engine.push(EventRecord::param(frame, target, id(ids::GAIN), value));
            assert_eq!(pushed, Pushed::Ignored);
            ignored += 1;
        }
        let note = (next() % 128) as u8;
        let velocity = [f32::MIN_POSITIVE, 1e-30, 0.5, 1.0][(next() % 4) as usize];
        engine.push(EventRecord::note_on(frame, target, note, velocity));
        if next() % 3 == 0 {
            engine.push(EventRecord::note_off(frame + 150, target, note));
        }
        if next() % 50 == 0 {
            engine.push(EventRecord::all_notes_off(frame + 7, target));
        }
    }
    let (left, right) = render(&mut engine, 250_000, 512);
    let stats = engine.stats();
    assert_eq!(stats.recovered, 0, "{stats:?}");
    assert_eq!(stats.ignored, ignored);
    assert_eq!(stats.dropped, 0);
    assert!(peak(&left) > 0.1, "nothing sounded");
    for sample in left.iter().chain(&right) {
        assert!(sample.is_finite() && sample.abs() <= OUTPUT_CEILING);
    }
}

/// The largest step from one sample to the next in `samples`.
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

/// A synth part of steady sine tones, so that a click stands out.
fn sine_synth() -> Engine {
    let mut engine = engine_with(InstrumentKind::Synth, Chain::NONE);
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::SYNTH_WAVE), 3.0),
            EventRecord::param(0, 0, id(ids::SYNTH_SUB), 0.0),
            EventRecord::param(0, 0, id(ids::SYNTH_ENV_AMOUNT), 0.0),
            EventRecord::param(0, 0, id(ids::SYNTH_CUTOFF), 16_000.0),
            EventRecord::param(0, 0, id(ids::SYNTH_SUSTAIN), 1.0),
            // No beating between the two oscillators, and an attack slow
            // enough that a new note's start is no steeper than the chord.
            EventRecord::param(0, 0, id(ids::SYNTH_DETUNE), 0.0),
            EventRecord::param(0, 0, id(ids::SYNTH_ATTACK), 0.01),
        ],
    );
    engine
}

/// A ninth note takes over the voice of the oldest of eight held ones. The
/// old note fades out over `STEAL_FADE_SECONDS` instead of stopping
/// between two samples, so no step around the take-over is larger than the
/// steps of the steady chord before it. Tried at several moments, so that
/// the old note is caught at every point of its cycle.
#[test]
fn a_note_that_takes_over_a_voice_does_not_click() {
    for offset in 0..12u64 {
        let at = 24_000 + offset * 53;
        let mut engine = sine_synth();
        for note in 40..48u8 {
            engine.push(EventRecord::note_on(0, 0, note, 1.0));
        }
        engine.push(EventRecord::note_on(at, 0, 52, 1.0));
        let (left, _) = render(&mut engine, at as usize + 4_800, 128);
        let at = at as usize;
        let steady = largest_step(&left[at - 4_800..at]);
        let around = largest_step(&left[at - 1..at + 4_800]);
        assert!(
            around < 2.0 * steady,
            "offset {offset}: a step of {around} where the chord has {steady}"
        );
        assert_eq!(engine.active_voices(0), Some(8));
    }
}

/// A held key struck again harder moves to the new level over a few
/// milliseconds instead of jumping to it between two samples.
#[test]
fn a_key_struck_again_glides_to_its_new_level() {
    for offset in 0..12u64 {
        let at = 24_000 + offset * 37;
        let mut engine = sine_synth();
        push_all(
            &mut engine,
            &[
                EventRecord::note_on(0, 0, 45, 0.2),
                EventRecord::note_on(at, 0, 45, 1.0),
            ],
        );
        let (left, _) = render(&mut engine, at as usize + 24_000, 128);
        let at = at as usize;
        // Steps of the note at its new level, once it has settled.
        let settled = largest_step(&left[at + 9_600..at + 24_000]);
        let around = largest_step(&left[at - 1..at + 9_600]);
        assert!(
            around < 1.5 * settled,
            "offset {offset}: a step of {around} where the note has {settled}"
        );
        assert!(peak(&left[at + 9_600..]) > 3.0 * peak(&left[at - 4_800..at]));
    }
}

/// Every instrument sounds, stays finite and under the ceiling, and dies
/// away, at the lowest and the highest rate browsers allow and in between.
#[test]
fn every_instrument_works_at_every_sample_rate() {
    for rate in [
        EngineConfig::MIN_SAMPLE_RATE,
        22_050.0,
        96_000.0,
        EngineConfig::MAX_SAMPLE_RATE,
    ] {
        let notes = DRUM_MAP
            .iter()
            .map(|entry| (InstrumentKind::Drums, entry.note))
            .chain([(InstrumentKind::Synth, 45), (InstrumentKind::Synth, 96)]);
        for (kind, note) in notes {
            let mut engine = Engine::new(EngineConfig::new(rate)).unwrap();
            engine.create_part(0, kind).unwrap();
            push_all(
                &mut engine,
                &[
                    EventRecord::note_on(0, 0, note, 1.0),
                    EventRecord::note_off((0.2 * rate) as u64, 0, note),
                ],
            );
            let frames = (4.0 * rate) as usize;
            let mut left = vec![0.0; frames];
            let mut right = vec![0.0; frames];
            engine.render_into(&mut left, &mut right);
            let label = format!("{rate} Hz, {kind:?} {note}");
            // A drum starts on its frame; the synth's oscillators all start on a
            // zero, so its first sound can be one frame later.
            let first = onset(&left).unwrap_or(usize::MAX);
            assert!(
                first == 0 || (kind == InstrumentKind::Synth && first == 1),
                "{label}"
            );
            assert!(peak(&left) > 0.05, "{label}: peak {}", peak(&left));
            assert!(
                left.iter()
                    .chain(&right)
                    .all(|sample| sample.is_finite() && sample.abs() <= OUTPUT_CEILING),
                "{label}"
            );
            assert_eq!(engine.stats().recovered, 0, "{label}");
            assert_eq!(engine.active_voices(0), Some(0), "{label}: still sounding");
        }
    }
}
