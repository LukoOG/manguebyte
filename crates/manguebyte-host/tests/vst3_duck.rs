//! Hosts Mangue Duck, the framework's example effect, as a VST 3 plugin,
//! through the shared library cargo builds for it.
//!
//! Where the instrument's tests are about notes, these are about audio
//! going in: a main bus processed in place, a sidechain on an auxiliary
//! bus, and buffers that do not have the plugin's channel count. As there,
//! the plugin is also driven with no host, and the two have to agree to
//! the bit.
//!
//! Not under Miri, which cannot load a shared library.
#![cfg(all(feature = "vst3", not(miri)))]

mod common;

use mangue_duck::Duck;
use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::Processor;
use manguebyte_dsp::event::{EventKind, NoteRef};
use manguebyte_host::vst3::{ClassId, ProcessStatus, Vst3Module};
use manguebyte_params::{Param, ParamSet};
use manguebyte_plugin::{Harness, Plugin};

use common::{assert_same_bits, for_harness_vst3, render, vst3_id};

const RATE: f64 = 48_000.0;
const FRAMES: usize = 24_000;

fn threshold() -> ParamId {
    vst3_id(ParamId::from_name("threshold"))
}
fn depth() -> ParamId {
    vst3_id(ParamId::from_name("depth"))
}
fn attack() -> ParamId {
    vst3_id(ParamId::from_name("attack"))
}
fn release() -> ParamId {
    vst3_id(ParamId::from_name("release"))
}
fn bypass() -> ParamId {
    vst3_id(ParamId::from_name("bypass"))
}

fn module() -> Vst3Module {
    common::load_vst3(common::built_library("mangue_duck")).expect("Mangue Duck loads")
}

/// The one class in the module.
fn class(module: &Vst3Module) -> ClassId {
    module.classes()[0].id
}

/// Something that is not the same on every frame or in every channel.
fn programme(channels: usize, frames: usize) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(channels, frames);
    for (index, channel) in buffer.iter_channels_mut().enumerate() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            let phase = frame as f32 * (0.05 + 0.01 * index as f32);
            *sample = 0.5 * phase.sin();
        }
    }
    buffer
}

/// A sidechain that comes and goes, louder in one channel than the other.
fn key(channels: usize, frames: usize) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(channels, frames);
    for (index, channel) in buffer.iter_channels_mut().enumerate() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            let on = (frame / 3_000) % 2 == 0;
            *sample = if on { 0.8 / (1 + index) as f32 } else { 0.0 };
        }
    }
    buffer
}

/// Every parameter moved at least once, normalized.
fn automation() -> Vec<(u64, EventKind)> {
    let value = |param, value| EventKind::ParamValue { param, value };
    vec![
        (2_500, value(depth(), 0.75)),
        (7_777, value(release(), 0.2)),
        (9_001, value(threshold(), 0.8)),
        (12_000, value(attack(), 0.6)),
        (15_000, value(bypass(), 1.0)),
        (18_123, value(bypass(), 0.0)),
    ]
}

fn reference(
    max_block: usize,
    pattern: &[usize],
    schedule: &[(u64, EventKind)],
    input: &AudioBuffer,
    sidechain: Option<&AudioBuffer>,
) -> AudioBuffer {
    let mut harness = Harness::<Duck>::new(RATE, max_block).with_block_pattern(pattern);
    let fresh = Duck::new().params();
    for &(frame, kind) in schedule {
        harness.schedule(frame, for_harness_vst3(&*fresh, kind));
    }
    match sidechain {
        Some(sidechain) => harness.process_with_sidechain(input, sidechain),
        None => harness.process(input),
    }
}

fn hosted(
    module: &Vst3Module,
    max_block: usize,
    pattern: &[usize],
    schedule: &[(u64, EventKind)],
    input: &AudioBuffer,
    sidechain: Option<&AudioBuffer>,
) -> (AudioBuffer, common::Rendered) {
    let mut plugin = module.create(class(module)).expect("the plugin is created");
    let mut processor = plugin.activate(RATE, max_block).expect("it activates");
    let mut audio = input.clone();
    let rendered = render(
        &mut processor,
        RATE,
        schedule,
        pattern,
        max_block,
        &mut audio,
        sidechain,
    );
    assert_eq!(processor.status(), ProcessStatus::Processed);
    drop(processor);
    plugin.deactivate().expect("it deactivates");
    assert_eq!(plugin.failed_blocks(), 0);
    assert_eq!(plugin.lost_events(), 0);
    assert_eq!(plugin.undeliverable_events(), 0);
    (audio, rendered)
}

#[test]
fn buses_and_parameters_are_the_ones_the_plugin_declares() {
    let module = module();
    let [class_info] = module.classes() else {
        panic!("expected one class, found {:?}", module.classes());
    };
    assert_eq!(class_info.name, Duck::INFO.name);
    assert_eq!(class_info.sub_categories, ["Fx"]);
    assert!(!class_info.is_instrument());

    let plugin = module.create(class_info.id).unwrap();
    let layout = Duck::LAYOUT;

    // A main input and a sidechain, which starts switched off.
    let [main_in, side] = plugin.audio_inputs() else {
        panic!("expected two inputs, found {:?}", plugin.audio_inputs());
    };
    assert_eq!(main_in.name, "Input");
    assert!(main_in.is_main && main_in.default_active);
    assert_eq!(main_in.channels as usize, layout.inputs);
    assert_eq!(side.name, "Sidechain");
    assert!(!side.is_main && !side.default_active);
    assert_eq!(side.channels as usize, layout.sidechain);
    assert_eq!(side.arrangement, 0b11);
    let [main_out] = plugin.audio_outputs() else {
        panic!("expected one output, found {:?}", plugin.audio_outputs());
    };
    assert!(main_out.is_main);
    assert_eq!(main_out.channels as usize, layout.outputs);
    // An effect takes no notes, and maps no controllers.
    assert!(plugin.event_inputs().is_empty() && plugin.event_outputs().is_empty());
    assert_eq!(plugin.midi_mapping(0, 129), None);

    let declared = Duck::new().params();
    let mut expected: Vec<&dyn Param> = Vec::new();
    declared.visit(&mut |param| expected.push(param));
    assert_eq!(plugin.params().len(), 5);
    for (listed, declared) in plugin.params().iter().zip(expected) {
        assert_eq!(listed.id, vst3_id(declared.param_id()));
        assert_eq!(listed.title, declared.name());
        let flags = declared.info().flags();
        assert_eq!(listed.flags.bypass, flags.bypass);
        assert_eq!(listed.flags.automatable, flags.automatable);
        assert_eq!(listed.step_count, declared.step_count().unwrap_or(0));
        assert_eq!(plugin.param_value(listed.id), Some(listed.default));
    }
    let switch = plugin.param(bypass()).unwrap();
    assert!(switch.flags.bypass);
    assert_eq!((switch.step_count, switch.default), (1, 0.0));
    assert_eq!(plugin.param_to_text(bypass(), 1.0).as_deref(), Some("On"));
    assert_eq!(plugin.text_to_param(bypass(), "off"), Some(0.0));
    assert_eq!(plugin.param_to_text(depth(), 0.5).as_deref(), Some("20.0"));
}

#[test]
fn the_sidechain_reaches_the_plugin_exactly_as_it_does_with_no_host() {
    let module = module();
    let input = programme(2, FRAMES);
    let sidechain = key(2, FRAMES);
    let schedule = automation();

    let cases: [(usize, &[usize]); 4] = [
        (256, &[256]),
        (64, &[64]),
        (1_024, &[1, 999, 13, 1_024]),
        (512, &[1, 511, 7, 300, 64, 2, 129]),
    ];
    let mut first: Option<AudioBuffer> = None;
    for (max_block, pattern) in cases {
        let what = format!("blocks of {pattern:?}, at most {max_block}");
        let expected = reference(max_block, pattern, &schedule, &input, Some(&sidechain));
        assert!(expected != input, "{what}: nothing was ducked");

        let (actual, rendered) = hosted(
            &module,
            max_block,
            pattern,
            &schedule,
            &input,
            Some(&sidechain),
        );
        assert_same_bits(&actual, &expected, &what);
        assert_eq!(
            rendered.heap_calls, 0,
            "{what}: the host used the heap while processing"
        );
        match &first {
            Some(first) => assert_same_bits(&actual, first, &format!("{what}, against the first")),
            None => first = Some(actual),
        }
    }
}

#[test]
fn the_sidechain_is_what_makes_the_difference() {
    let module = module();
    let input = programme(2, FRAMES);
    let sidechain = key(2, FRAMES);
    let fast = [(
        0,
        EventKind::ParamValue {
            param: release(),
            value: 0.0,
        },
    )];

    // Nothing routed to the sidechain: the plugin hears silence there and
    // leaves the input alone, bit for bit.
    let (alone, rendered) = hosted(&module, 256, &[256], &fast, &input, None);
    assert_same_bits(
        &alone,
        &reference(256, &[256], &fast, &input, None),
        "no sidechain",
    );
    assert_same_bits(&alone, &input, "an input with nothing to duck under");
    assert_eq!(rendered.heap_calls, 0);

    let (ducked, _) = hosted(&module, 256, &[256], &fast, &input, Some(&sidechain));
    let level = |buffer: &AudioBuffer, range: std::ops::Range<usize>| {
        let slice = &buffer.channel(0)[range];
        (slice
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
            / slice.len() as f64)
            .sqrt()
    };
    let loud = 1_500..2_900;
    let quiet = 5_000..5_900;
    // The default depth is 12 dB: a quarter of the level.
    let ratio = level(&ducked, loud.clone()) / level(&input, loud);
    assert!(
        (ratio - 0.251).abs() < 0.01,
        "ducked to {ratio} of the input"
    );
    let ratio = level(&ducked, quiet.clone()) / level(&input, quiet);
    assert!((ratio - 1.0).abs() < 0.01, "back to {ratio} of the input");

    // A mono sidechain into the stereo bus.
    let mono = key(1, FRAMES);
    let (actual, _) = hosted(&module, 256, &[256], &fast, &input, Some(&mono));
    let expected = reference(256, &[256], &fast, &input, Some(&mono));
    assert_same_bits(&actual, &expected, "a mono sidechain");
    assert!(actual != alone);

    // A sidechain shorter than the block cannot be lined up with it, and is
    // treated as not connected.
    let short = key(2, 100);
    let mut plugin = module.create(class(&module)).unwrap();
    let mut processor = plugin.activate(RATE, 256).unwrap();
    let mut audio = programme(2, 256);
    let transport = manguebyte_dsp::Transport::STOPPED;
    let events = manguebyte_dsp::event::EventBuffer::new();
    let context = manguebyte_dsp::ProcessContext::new(&events, &transport).with_sidechain(&short);
    processor.process_with(&mut audio, &context);
    assert_same_bits(&audio, &programme(2, 256), "a short sidechain");
}

#[test]
fn buffers_of_another_width_are_served() {
    let module = module();
    let sidechain = key(2, FRAMES);
    let schedule = automation();
    let stereo = programme(2, FRAMES);
    let expected = reference(256, &[256], &schedule, &stereo, Some(&sidechain));

    let wide_input = programme(5, FRAMES);
    let (wide, rendered) = hosted(
        &module,
        256,
        &[256],
        &schedule,
        &wide_input,
        Some(&sidechain),
    );
    assert_eq!(rendered.heap_calls, 0);
    for channel in 0..2 {
        assert!(
            wide.channel(channel) == expected.channel(channel),
            "channel {channel} of a wide buffer"
        );
    }
    for channel in 2..5 {
        assert!(wide.channel(channel).iter().all(|&sample| sample == 0.0));
    }

    let mono_input = programme(1, FRAMES);
    let (mono, rendered) = hosted(
        &module,
        256,
        &[256],
        &schedule,
        &mono_input,
        Some(&sidechain),
    );
    assert_eq!(rendered.heap_calls, 0);
    assert_eq!(mono.num_channels(), 1);
    let expected = reference(256, &[256], &schedule, &mono_input, Some(&sidechain));
    assert!(mono.channel(0) == expected.channel(0));
    assert!(mono != mono_input);
}

#[test]
fn notes_have_nowhere_to_go_in_an_effect_and_are_counted() {
    let module = module();
    let mut plugin = module.create(class(&module)).unwrap();
    let mut processor = plugin.activate(RATE, 256).unwrap();
    let note = NoteRef::new(0, 60);
    let schedule = [
        (
            10,
            EventKind::NoteOn {
                note,
                velocity: 1.0,
            },
        ),
        (
            20,
            EventKind::ParamValue {
                param: depth(),
                value: 1.0,
            },
        ),
        (
            30,
            EventKind::NoteOff {
                note,
                velocity: 0.0,
            },
        ),
        (
            40,
            EventKind::ParamValue {
                param: ParamId(12_345),
                value: 0.5,
            },
        ),
        (
            50,
            EventKind::ParamModulation {
                param: depth(),
                amount: 0.1,
                note: None,
            },
        ),
    ];
    let input = programme(2, 1_024);
    let mut audio = input.clone();
    let rendered = render(
        &mut processor,
        RATE,
        &schedule,
        &[256],
        256,
        &mut audio,
        None,
    );
    assert_eq!(rendered.heap_calls, 0);
    assert_eq!(processor.status(), ProcessStatus::Processed);
    // No event bus, so no notes; no such parameter; no modulation in VST 3.
    assert_eq!(plugin.undeliverable_events(), 4);
    assert_eq!(plugin.lost_events(), 0);
    plugin.pump();
    assert_eq!(plugin.param_value(depth()), Some(1.0));
    assert_same_bits(&audio, &input, "an effect with a quiet sidechain");
}

#[test]
fn the_switch_and_the_state_work_from_the_main_thread() {
    let module = module();
    let input = programme(2, 9_600);
    let sidechain = key(2, 9_600);
    let declared = Duck::new().params();
    let plain = |id, value| {
        common::by_vst3_id(&*declared, id)
            .unwrap()
            .normalized_to_plain(value)
    };

    let mut plugin = module.create(class(&module)).unwrap();
    plugin.set_param(depth(), 1.0).unwrap();
    plugin.set_param(bypass(), 1.0).unwrap();
    assert_eq!(plugin.param_value(bypass()), Some(1.0));
    let saved = plugin.save_state().unwrap();

    // Bypassed before it was switched on: the input comes through whole.
    let mut processor = plugin.activate(RATE, 256).unwrap();
    let mut audio = input.clone();
    render(
        &mut processor,
        RATE,
        &[],
        &[256],
        256,
        &mut audio,
        Some(&sidechain),
    );
    assert_same_bits(&audio, &input, "bypassed from the start");

    // Switched in from the main thread while it runs: the same as
    // automation at the start of the next block.
    plugin.set_param(bypass(), 0.0).unwrap();
    let mut audio = input.clone();
    render(
        &mut processor,
        RATE,
        &[],
        &[256],
        256,
        &mut audio,
        Some(&sidechain),
    );
    plugin.pump();
    assert_eq!(plugin.param_value(bypass()), Some(0.0));

    let mut harness = Harness::<Duck>::new(RATE, 256);
    harness.params().depth.set(plain(depth(), 1.0) as f32);
    harness.params().bypass.set(true);
    harness.plugin_mut().prepare(&manguebyte_dsp::ProcessSpec {
        sample_rate: RATE,
        max_block_size: 256,
        num_channels: 2,
    });
    let through = harness.process_with_sidechain(&input, &sidechain);
    assert_same_bits(&through, &input, "the reference, bypassed");
    harness.automate(9_600, "bypass", 0.0);
    let expected = harness.process_with_sidechain(&input, &sidechain);
    assert_same_bits(&audio, &expected, "switched in from the main thread");
    assert!(audio != input, "the switch did nothing");

    // The state, saved while bypassed, puts another instance in bypass.
    drop(processor);
    plugin.deactivate().unwrap();
    let mut other = module.create(class(&module)).unwrap();
    assert_eq!(other.param_value(bypass()), Some(0.0));
    other.set_stream_chunk(2);
    other.load_state(&saved).unwrap();
    assert_eq!(other.param_value(bypass()), Some(1.0));
    assert_eq!(other.param_value(depth()), Some(1.0));
    let mut processor = other.activate(RATE, 256).unwrap();
    let mut audio = input.clone();
    processor.process(&mut audio);
    assert_same_bits(&audio, &input, "restored into bypass");
}
