//! Each part describes its parameters as JSON, by stable identifier.

mod common;

use common::*;
use manguebyte_web::{
    Chain, Effect, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, ids,
};

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

const PLUCKED: [&str; 6] = [
    ids::PLUCKED_PRESET,
    ids::PLUCKED_DECAY,
    ids::PLUCKED_BRIGHTNESS,
    ids::PLUCKED_POSITION,
    ids::PLUCKED_TONE,
    ids::PLUCKED_RELEASE,
];
const FM: [&str; 4] = [
    ids::FM_PRESET,
    ids::FM_BRIGHTNESS,
    ids::FM_DECAY,
    ids::FM_RELEASE,
];
const ORGAN: [&str; 16] = [
    ids::ORGAN_PRESET,
    ids::ORGAN_DRAWBAR_16,
    ids::ORGAN_DRAWBAR_5_1_3,
    ids::ORGAN_DRAWBAR_8,
    ids::ORGAN_DRAWBAR_4,
    ids::ORGAN_DRAWBAR_2_2_3,
    ids::ORGAN_DRAWBAR_2,
    ids::ORGAN_DRAWBAR_1_3_5,
    ids::ORGAN_DRAWBAR_1_1_3,
    ids::ORGAN_DRAWBAR_1,
    ids::ORGAN_PERCUSSION,
    ids::ORGAN_CLICK,
    ids::ORGAN_LEAKAGE,
    ids::ORGAN_VELOCITY,
    ids::ORGAN_ROTARY,
    ids::ORGAN_SPEAKER,
];
const ANALOG: [&str; 3] = [ids::ANALOG_TONE, ids::ANALOG_SNAP, ids::ANALOG_KICK_DECAY];
const KICK_BASS: [&str; 6] = [
    ids::KICK_BASS_TUNE,
    ids::KICK_BASS_DECAY,
    ids::KICK_BASS_SWEEP,
    ids::KICK_BASS_DRIVE,
    ids::KICK_BASS_GLIDE,
    ids::KICK_BASS_CLICK,
];
const ACID: [&str; 10] = [
    ids::ACID_PRESET,
    ids::ACID_WAVE,
    ids::ACID_CUTOFF,
    ids::ACID_RESONANCE,
    ids::ACID_ENV_MOD,
    ids::ACID_DECAY,
    ids::ACID_ACCENT,
    ids::ACID_SLIDE,
    ids::ACID_DRIVE,
    ids::ACID_VELOCITY,
];

/// The identifiers of the instrument of `kind`, in the order its part
/// lists them.
fn instrument_ids(kind: InstrumentKind) -> Vec<&'static str> {
    match kind {
        InstrumentKind::Synth => SYNTH.to_vec(),
        InstrumentKind::Drums => KIT.to_vec(),
        InstrumentKind::Sampler => SAMPLER.to_vec(),
        InstrumentKind::SamplerKit => SAMPLER.iter().chain(&KIT).copied().collect(),
        InstrumentKind::Plucked => PLUCKED.to_vec(),
        InstrumentKind::Fm => FM.to_vec(),
        InstrumentKind::Organ => ORGAN.to_vec(),
        InstrumentKind::AnalogKit => KIT.iter().chain(&ANALOG).copied().collect(),
        InstrumentKind::KickBass => KICK_BASS.to_vec(),
        InstrumentKind::Acid => ACID.to_vec(),
    }
}

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
        let engine = engine_with(kind, Chain::ALL);
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

/// The parameters of each effect, its switch first, the rest in the order
/// a method chain's positional arguments fill them.
fn effect_ids(effect: Effect) -> Vec<&'static str> {
    match effect {
        Effect::Lowpass => vec![ids::LOWPASS_ON, ids::LOWPASS_CUTOFF, ids::LOWPASS_RESONANCE],
        Effect::Reverb => vec![ids::REVERB_ON, ids::REVERB_MIX, ids::REVERB_DECAY],
        Effect::Highpass => vec![
            ids::HIGHPASS_ON,
            ids::HIGHPASS_CUTOFF,
            ids::HIGHPASS_RESONANCE,
        ],
        Effect::Eq => vec![
            ids::EQ_ON,
            ids::EQ_LOW_GAIN,
            ids::EQ_MID_GAIN,
            ids::EQ_HIGH_GAIN,
            ids::EQ_LOW_FREQ,
            ids::EQ_MID_FREQ,
            ids::EQ_MID_Q,
            ids::EQ_HIGH_FREQ,
        ],
        Effect::Drive => vec![
            ids::DRIVE_ON,
            ids::DRIVE_AMOUNT,
            ids::DRIVE_OUTPUT,
            ids::DRIVE_MIX,
            ids::DRIVE_SHAPE,
        ],
        Effect::Bitcrush => vec![
            ids::BITCRUSH_ON,
            ids::BITCRUSH_BITS,
            ids::BITCRUSH_RATE,
            ids::BITCRUSH_MIX,
        ],
        Effect::Compressor => vec![
            ids::COMPRESSOR_ON,
            ids::COMPRESSOR_THRESHOLD,
            ids::COMPRESSOR_RATIO,
            ids::COMPRESSOR_ATTACK,
            ids::COMPRESSOR_RELEASE,
            ids::COMPRESSOR_KNEE,
            ids::COMPRESSOR_MAKEUP,
            ids::COMPRESSOR_MIX,
        ],
        Effect::Chorus => vec![
            ids::CHORUS_ON,
            ids::CHORUS_MIX,
            ids::CHORUS_RATE,
            ids::CHORUS_DEPTH,
            ids::CHORUS_DELAY,
            ids::CHORUS_VOICES,
            ids::CHORUS_SPREAD,
        ],
        Effect::Flanger => vec![
            ids::FLANGER_ON,
            ids::FLANGER_MIX,
            ids::FLANGER_RATE,
            ids::FLANGER_DEPTH,
            ids::FLANGER_FEEDBACK,
            ids::FLANGER_DELAY,
        ],
        Effect::Phaser => vec![
            ids::PHASER_ON,
            ids::PHASER_MIX,
            ids::PHASER_RATE,
            ids::PHASER_DEPTH,
            ids::PHASER_CENTER,
            ids::PHASER_FEEDBACK,
            ids::PHASER_STAGES,
            ids::PHASER_SPREAD,
        ],
        Effect::Tremolo => vec![
            ids::TREMOLO_ON,
            ids::TREMOLO_RATE,
            ids::TREMOLO_DEPTH,
            ids::TREMOLO_SHAPE,
            ids::TREMOLO_SYNC,
        ],
        Effect::Autopan => vec![
            ids::AUTOPAN_ON,
            ids::AUTOPAN_RATE,
            ids::AUTOPAN_DEPTH,
            ids::AUTOPAN_SHAPE,
            ids::AUTOPAN_SYNC,
        ],
        Effect::Delay => vec![
            ids::DELAY_ON,
            ids::DELAY_TIME,
            ids::DELAY_FEEDBACK,
            ids::DELAY_MIX,
            ids::DELAY_DAMPING,
            ids::DELAY_PINGPONG,
            ids::DELAY_SYNC,
        ],
        _ => vec![],
    }
}

#[test]
fn every_part_lists_its_instrument_then_its_stages_then_gain_pan_and_send() {
    // A part for every kind and chain.
    let mut engine = Engine::new(EngineConfig {
        max_parts: 128,
        ..EngineConfig::new(RATE)
    })
    .unwrap();
    let mut part = 0;
    let chains = (0..4)
        .chain([Chain::ALL.bits(), 1 << 12 | 1, 0b1010_1010_1010])
        .map(|bits| Chain::from_bits(bits).unwrap());
    let chains: Vec<Chain> = chains.collect();
    for kind in InstrumentKind::ALL {
        for &chain in &chains {
            engine.create_part_with(part, kind, chain).unwrap();
            let json = engine.params_json(part).unwrap();
            let mut expected: Vec<&str> = instrument_ids(kind);
            for effect in Effect::PART_ORDER {
                if chain.has(effect) {
                    expected.extend(effect_ids(effect));
                }
            }
            expected.extend([ids::GAIN, ids::PAN, ids::SEND]);
            assert_eq!(ids_in(&json), expected, "{kind:?} {chain:?}");
            let hashes: Vec<u32> = expected.iter().map(|id| ParamId::from_name(id).0).collect();
            assert_eq!(param_ids_in(&json), hashes);
            assert!(json.starts_with('[') && json.ends_with(']'));
            // Each effect's parameter names its effect and its picture.
            for effect in Effect::PART_ORDER.into_iter().filter(|&e| chain.has(e)) {
                let tag = format!(
                    r#""effect":"{}","visual":"{}"}}"#,
                    effect.name(),
                    effect.visual().name()
                );
                assert_eq!(
                    json.matches(&tag).count(),
                    effect_ids(effect).len(),
                    "{effect:?}"
                );
            }
            assert_eq!(
                json.matches(r#""effect":"#).count(),
                expected.len() - 3 - instrument_ids(kind).len()
            );
            part += 1;
        }
    }
    assert_eq!(engine.params_json(99), None);
}

/// Every identifier a part or the master bus has hashes to its own
/// `ParamId`: an event names exactly one.
#[test]
fn no_two_identifiers_share_a_param_id() {
    let mut hashes: Vec<u32> = ids::ALL
        .iter()
        .chain(&ids::MASTER)
        .map(|id| ParamId::from_name(id).0)
        .collect();
    let count = hashes.len();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), count);
    assert!(ids::MASTER.iter().all(|id| id.starts_with("master.")));
    assert!(!ids::ALL.iter().any(|id| id.starts_with("master.")));
}

#[test]
fn the_master_bus_describes_its_parameters() {
    let engine = engine();
    let json = engine.master_params_json();
    assert_eq!(ids_in(&json), ids::MASTER);
    for (id, effect) in [
        (ids::MASTER_REVERB_DECAY, Some(Effect::Reverb)),
        (ids::GLUE_THRESHOLD, Some(Effect::Glue)),
        (ids::LIMITER_CEILING, Some(Effect::Limiter)),
        (ids::MASTER_TEMPO, None),
    ] {
        // What follows the flags, which end the fields every parameter has.
        let start = json.find(&format!(r#"{{"id":"{id}""#)).unwrap();
        let flags = start + json[start..].find(r#""flags":{"#).unwrap();
        let rest = &json[flags + json[flags..].find('}').unwrap() + 1..];
        match effect {
            Some(effect) => assert!(
                rest.starts_with(&format!(
                    r#","effect":"{}","visual":"{}"}}"#,
                    effect.name(),
                    effect.visual().name()
                )),
                "{id}: {rest:.80}"
            ),
            None => assert!(!rest.starts_with(r#","effect""#), "{id}"),
        }
    }
    assert!(json.contains(r#"{"id":"master.limiter.ceiling","param_id":"#));
    assert!(json.contains(r#""unit":"dBTP","min":-12,"max":-0.3,"default":-0.3,"#));
    assert_eq!(engine.master_param_value(ids::MASTER_TEMPO), Some(120.0));
    assert_eq!(engine.master_param_value(ids::GAIN), None);
}

/// A master parameter reaches the master bus whatever part its record
/// names, and a part's cancel does not drop it.
#[test]
fn master_parameters_belong_to_no_part() {
    let mut engine = engine();
    engine.create_part(3, InstrumentKind::Synth).unwrap();
    push_all(
        &mut engine,
        &[
            EventRecord::param(10, 3, id(ids::MASTER_TEMPO), 90.0),
            EventRecord::param(20, 999, id(ids::LIMITER_CEILING), -3.0),
            EventRecord::param(4_000, 3, id(ids::MASTER_GAIN), -6.0),
            EventRecord::cancel(1_000, 3),
        ],
    );
    render(&mut engine, 8_000, 128);
    assert_eq!(engine.master_param_value(ids::MASTER_TEMPO), Some(90.0));
    assert_eq!(engine.master_param_value(ids::LIMITER_CEILING), Some(-3.0));
    assert_eq!(engine.master_param_value(ids::MASTER_GAIN), Some(-6.0));
    assert_eq!(engine.stats().cancelled, 0);
    assert_eq!(engine.stats().unrouted, 0);
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
            r#""smoothing_ms":20,"flags":{"automatable":true,"hidden":false,"bypass":false},"#,
            r#""effect":"lowpass","visual":"filter_curve"}"#
        ),
        concat!(
            r#"{"id":"reverb.decay","param_id":228043249,"name":"Reverb decay","unit":"s","#,
            r#""min":0.2,"max":10,"default":1.6,"#,
            r#""scale":{"kind":"skew","factor":0.35,"symmetric":false},"#,
            r#""flags":{"automatable":true,"hidden":false,"bypass":false},"#,
            r#""effect":"reverb","visual":"reverb_decay"}"#
        ),
        r#"{"id":"reverb.on","param_id":"#,
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
