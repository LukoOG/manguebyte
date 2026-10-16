//! Each part describes its parameters as JSON, by stable identifier.

mod common;

use common::*;
use manguebyte_web::{Chain, EventRecord, InstrumentKind, ParamId, ids};

/// The `"id"` of every object in a description, in order.
fn ids_in(json: &str) -> Vec<String> {
    json.split(r#"{"id":""#)
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
        .collect()
}

/// The `"param_id"` of every object, in order.
fn param_ids_in(json: &str) -> Vec<u32> {
    json.split(r#""param_id":"#)
        .skip(1)
        .map(|rest| rest[..rest.find(',').unwrap()].parse().unwrap())
        .collect()
}

const SYNTH: [&str; 11] = [
    ids::SYNTH_WAVE,
    ids::SYNTH_DETUNE,
    ids::SYNTH_SUB,
    ids::SYNTH_CUTOFF,
    ids::SYNTH_RESONANCE,
    ids::SYNTH_ENV_AMOUNT,
    ids::SYNTH_FILTER_DECAY,
    ids::SYNTH_ATTACK,
    ids::SYNTH_DECAY,
    ids::SYNTH_SUSTAIN,
    ids::SYNTH_RELEASE,
];
const KIT: [&str; 13] = [
    ids::KIT_TUNE,
    ids::KIT_DECAY,
    ids::KICK_LEVEL,
    ids::SNARE_LEVEL,
    ids::CLOSED_HAT_LEVEL,
    ids::OPEN_HAT_LEVEL,
    ids::RIDE_LEVEL,
    ids::CRASH_LEVEL,
    ids::HIGH_TOM_LEVEL,
    ids::MID_TOM_LEVEL,
    ids::FLOOR_TOM_LEVEL,
    ids::SIDE_STICK_LEVEL,
    ids::GUITAR_TAP_LEVEL,
];

const SAMPLER: [&str; 1] = [ids::SAMPLER_INTERPOLATION];

#[test]
fn the_identifiers_are_the_published_ones() {
    assert_eq!(
        [
            ids::LOWPASS_CUTOFF,
            ids::LOWPASS_RESONANCE,
            ids::REVERB_MIX,
            ids::REVERB_DECAY,
            ids::GAIN,
            ids::PAN
        ],
        [
            "lowpass.cutoff",
            "lowpass.resonance",
            "reverb.mix",
            "reverb.decay",
            "gain",
            "pan"
        ]
    );
    assert_eq!(
        SYNTH,
        [
            "synth.wave",
            "synth.detune",
            "synth.sub",
            "synth.cutoff",
            "synth.resonance",
            "synth.env_amount",
            "synth.filter_decay",
            "synth.attack",
            "synth.decay",
            "synth.sustain",
            "synth.release"
        ]
    );
    assert_eq!(
        KIT,
        [
            "kit.tune",
            "kit.decay",
            "kick.level",
            "snare.level",
            "closed_hat.level",
            "open_hat.level",
            "ride.level",
            "crash.level",
            "high_tom.level",
            "mid_tom.level",
            "floor_tom.level",
            "side_stick.level",
            "guitar_tap.level"
        ]
    );
    // The hashes events carry, as the JavaScript computes them too.
    assert_eq!(ParamId::from_name(ids::GAIN).0, 458_499_838);
    assert_eq!(ParamId::from_name(ids::LOWPASS_CUTOFF).0, 3_456_878_227);
}

/// `ids::ALL` names every parameter a part can have, once each.
#[test]
fn the_list_of_every_identifier_is_complete() {
    let mut described = Vec::new();
    for kind in InstrumentKind::ALL {
        let engine = engine_with(kind, Chain::FULL);
        for id in ids_in(&engine.params_json(0).unwrap()) {
            if !described.contains(&id) {
                described.push(id);
            }
        }
    }
    let mut all: Vec<String> = ids::ALL.iter().map(|id| (*id).to_owned()).collect();
    let count = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), count, "a duplicate in ids::ALL");
    described.sort();
    assert_eq!(all, described);
}

#[test]
fn every_part_lists_its_instrument_then_its_stages_then_gain_and_pan() {
    let mut engine = engine();
    let mut part = 0;
    for kind in InstrumentKind::ALL {
        for bits in 0..4 {
            let chain = Chain::from_bits(bits).unwrap();
            engine.create_part_with(part, kind, chain).unwrap();
            let json = engine.params_json(part).unwrap();
            let mut expected: Vec<&str> = match kind {
                InstrumentKind::Synth => SYNTH.to_vec(),
                InstrumentKind::Drums => KIT.to_vec(),
                InstrumentKind::Sampler => SAMPLER.to_vec(),
                InstrumentKind::SamplerKit => SAMPLER.iter().chain(&KIT).copied().collect(),
            };
            if chain.lowpass {
                expected.extend([ids::LOWPASS_CUTOFF, ids::LOWPASS_RESONANCE]);
            }
            if chain.reverb {
                expected.extend([ids::REVERB_MIX, ids::REVERB_DECAY]);
            }
            expected.extend([ids::GAIN, ids::PAN]);
            assert_eq!(ids_in(&json), expected, "{kind:?} {bits}");
            let hashes: Vec<u32> = expected.iter().map(|id| ParamId::from_name(id).0).collect();
            assert_eq!(param_ids_in(&json), hashes);
            assert!(json.starts_with('[') && json.ends_with(']'));
            part += 1;
        }
    }
    assert_eq!(engine.params_json(99), None);
}

#[test]
fn the_descriptions_are_exact() {
    let mut engine = engine();
    engine.create_part(0, InstrumentKind::Synth).unwrap();
    engine.create_part(1, InstrumentKind::Drums).unwrap();
    let synth = engine.params_json(0).unwrap();
    let drums = engine.params_json(1).unwrap();
    for expected in [
        concat!(
            r#"{"id":"gain","param_id":458499838,"name":"Gain","unit":"dB","#,
            r#""min":-60,"max":12,"default":0,"scale":{"kind":"linear"},"smoothing_ms":20,"#,
            r#""flags":{"automatable":true,"hidden":false,"bypass":false}}"#
        ),
        concat!(
            r#"{"id":"lowpass.cutoff","param_id":3456878227,"name":"Low-pass cutoff","#,
            r#""unit":"Hz","min":20,"max":20000,"default":20000,"scale":{"kind":"log"},"#,
            r#""smoothing_ms":20,"flags":{"automatable":true,"hidden":false,"bypass":false}}"#
        ),
        concat!(
            r#"{"id":"reverb.decay","param_id":228043249,"name":"Reverb decay","unit":"s","#,
            r#""min":0.2,"max":10,"default":1.6,"#,
            r#""scale":{"kind":"skew","factor":0.35,"symmetric":false},"#,
            r#""flags":{"automatable":true,"hidden":false,"bypass":false}}"#
        ),
    ] {
        assert!(synth.contains(expected), "{expected}");
        assert!(drums.contains(expected), "{expected}");
    }
    assert!(synth.contains(concat!(
        r#"{"id":"synth.wave","param_id":3407224228,"name":"Wave","unit":"","#,
        r#""min":0,"max":3,"default":0,"#,
        r#""scale":{"kind":"stepped","steps":3,"step":1,"names":["Saw","Square","Triangle","Sine"]},"#
    )));
    assert!(drums.contains(concat!(
        r#"{"id":"kit.decay","param_id":1779692795,"name":"Decay","unit":"x","#,
        r#""min":0.25,"max":4,"default":1,"scale":{"kind":"log"},"#
    )));
    assert!(!drums.contains("synth."));
    assert!(!synth.contains("kick."));
}

#[test]
fn the_description_follows_the_values_and_the_text_has_units() {
    let mut engine = engine_with(InstrumentKind::Synth, Chain::FULL);
    assert_eq!(engine.param_value(0, ids::GAIN), Some(0.0));
    push_all(
        &mut engine,
        &[
            EventRecord::param(0, 0, id(ids::GAIN), -6.0),
            EventRecord::param(0, 0, id(ids::SYNTH_CUTOFF), 2_000.0),
        ],
    );
    render(&mut engine, 1, 1);
    assert_eq!(engine.param_value(0, ids::GAIN), Some(-6.0));
    assert_eq!(
        engine.param_value_by_id(0, id(ids::SYNTH_CUTOFF)),
        Some(2_000.0)
    );
    assert_eq!(engine.param_value(0, "no.such"), None);
    assert_eq!(engine.param_value(7, ids::GAIN), None);
    // The default in the description stays the default.
    assert!(
        engine
            .params_json(0)
            .unwrap()
            .contains(r#""min":-60,"max":12,"default":0,"#)
    );
    assert_eq!(
        engine.value_to_text(0, ids::GAIN, -6.0).as_deref(),
        Some("-6.0 dB")
    );
    assert_eq!(
        engine.value_to_text(0, ids::SYNTH_WAVE, 2.0).as_deref(),
        Some("Triangle")
    );
    assert_eq!(
        engine.value_to_text(0, ids::GAIN, 99.0).as_deref(),
        Some("12.0 dB")
    );
    assert_eq!(engine.value_to_text(0, "nope", 1.0), None);
}
