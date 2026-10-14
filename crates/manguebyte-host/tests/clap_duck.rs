//! Hosts Mangue Duck, the framework's example effect, through the shared
//! library cargo builds for it.
//!
//! Where the instrument's tests are about notes, these are about audio
//! going in: a main input processed in place, a sidechain on a second
//! port, and buffers that do not have the plugin's channel count. As
//! there, the plugin is also linked in as an ordinary library and driven
//! with no host, and the two have to agree to the bit.
//!
//! Not under Miri, which cannot load a shared library.
#![cfg(all(feature = "clap", not(miri)))]

mod common;

use mangue_duck::Duck;
use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::Processor;
use manguebyte_dsp::event::{EventKind, NoteRef};
use manguebyte_host::clap::{ClapLibrary, HostEvent, ParamsRescan, ProcessStatus};
use manguebyte_params::{Param, ParamSet};
use manguebyte_plugin::{Harness, Plugin};

use common::{assert_same_bits, for_harness, plain_from_clap, render};

const RATE: f64 = 48_000.0;
const ID: &str = "org.manguebyte.duck";
const FRAMES: usize = 24_000;

const THRESHOLD: ParamId = ParamId::from_name("threshold");
const DEPTH: ParamId = ParamId::from_name("depth");
const ATTACK: ParamId = ParamId::from_name("attack");
const RELEASE: ParamId = ParamId::from_name("release");
const BYPASS: ParamId = ParamId::from_name("bypass");

fn library() -> ClapLibrary {
    common::load(common::built_library("mangue_duck")).expect("Mangue Duck loads")
}

/// Something that is not the same on every frame or in every channel, to
/// pass through the effect.
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

/// Every parameter moved at least once, in the plugin's CLAP scale: a
/// position between 0 and 1, and 0 or 1 for the switch.
fn automation() -> Vec<(u64, EventKind)> {
    let value = |param, value| EventKind::ParamValue { param, value };
    vec![
        (2_500, value(DEPTH, 0.75)),
        (7_777, value(RELEASE, 0.2)),
        (9_001, value(THRESHOLD, 0.8)),
        (12_000, value(ATTACK, 0.6)),
        (15_000, value(BYPASS, 1.0)),
        (18_123, value(BYPASS, 0.0)),
    ]
}

/// The effect as it sounds with nothing between the test and the plugin.
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
        harness.schedule(frame, for_harness(&*fresh, kind));
    }
    match sidechain {
        Some(sidechain) => harness.process_with_sidechain(input, sidechain),
        None => harness.process(input),
    }
}

/// The effect as it sounds when hosted, and what hosting cost.
fn hosted(
    library: &ClapLibrary,
    max_block: usize,
    pattern: &[usize],
    schedule: &[(u64, EventKind)],
    input: &AudioBuffer,
    sidechain: Option<&AudioBuffer>,
) -> (AudioBuffer, common::Rendered) {
    let mut plugin = library.create(ID).expect("the plugin is created");
    let mut processor = plugin.activate(RATE, max_block).expect("it activates");
    // In place, as every processor in the framework works.
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
    assert_eq!(processor.status(), ProcessStatus::Continue);
    drop(processor);
    plugin.deactivate().expect("it deactivates");
    assert_eq!(plugin.failed_blocks(), 0);
    assert_eq!(plugin.lost_events(), 0);
    assert_eq!(plugin.undeliverable_events(), 0);
    (audio, rendered)
}

#[test]
fn ports_and_parameters_are_the_ones_the_plugin_declares() {
    let library = library();
    let [descriptor] = library.descriptors() else {
        panic!("expected one plugin, found {:?}", library.descriptors());
    };
    assert_eq!(descriptor.id, Duck::INFO.id);
    assert_eq!(descriptor.name, Duck::INFO.name);
    assert_eq!(descriptor.features, ["audio-effect"]);

    let plugin = library.create(ID).unwrap();
    let layout = Duck::LAYOUT;

    // A main input and a sidechain, in that order.
    let [main_in, side] = plugin.audio_inputs() else {
        panic!("expected two inputs, found {:?}", plugin.audio_inputs());
    };
    assert_eq!(main_in.name, "Main");
    assert!(main_in.is_main);
    assert_eq!(main_in.channels as usize, layout.inputs);
    assert_eq!(main_in.port_type.as_deref(), Some("stereo"));
    assert_eq!(side.name, "Sidechain");
    assert!(!side.is_main);
    assert_eq!(side.channels as usize, layout.sidechain);
    assert_eq!(side.in_place_pair, None);
    assert_ne!(main_in.id, side.id);

    let [main_out] = plugin.audio_outputs() else {
        panic!("expected one output, found {:?}", plugin.audio_outputs());
    };
    assert!(main_out.is_main);
    assert_eq!(main_out.channels as usize, layout.outputs);
    // Input and output have the same shape, and the plugin says they may
    // share memory.
    assert_eq!(main_in.in_place_pair, Some(main_out.id));
    assert_eq!(main_out.in_place_pair, Some(main_in.id));

    // An effect takes no notes.
    assert!(plugin.note_inputs().is_empty() && plugin.note_outputs().is_empty());

    let declared = Duck::new().params();
    let mut expected: Vec<&dyn Param> = Vec::new();
    declared.visit(&mut |param| expected.push(param));
    assert_eq!(plugin.params().len(), 5);
    for (listed, declared) in plugin.params().iter().zip(expected) {
        assert_eq!(listed.id, declared.param_id());
        assert_eq!(listed.name, declared.name());
        let flags = declared.info().flags();
        assert_eq!(listed.flags.bypass, flags.bypass);
        assert_eq!(listed.flags.automatable, flags.automatable);
        match declared.step_count() {
            // A switch: two positions, one step apart.
            Some(steps) => {
                assert!(listed.flags.stepped && !listed.flags.modulatable);
                assert_eq!((listed.min, listed.max), (0.0, f64::from(steps)));
            }
            None => {
                assert!(!listed.flags.stepped && listed.flags.modulatable);
                assert_eq!((listed.min, listed.max), (0.0, 1.0));
            }
        }
        assert_eq!(plugin.param_value(listed.id), Some(listed.default));
    }
    let bypass = plugin.param(BYPASS).unwrap();
    assert!(bypass.flags.bypass && bypass.flags.stepped);
    assert_eq!((bypass.min, bypass.max, bypass.default), (0.0, 1.0, 0.0));
    assert_eq!(plugin.param_to_text(BYPASS, 1.0).as_deref(), Some("On"));
    assert_eq!(plugin.text_to_param(BYPASS, "off"), Some(0.0));
    assert_eq!(plugin.param_to_text(DEPTH, 0.5).as_deref(), Some("20.0 dB"));
}

#[test]
fn the_sidechain_reaches_the_plugin_exactly_as_it_does_with_no_host() {
    let library = library();
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
            &library,
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

        // This plugin follows events for every parameter, so its sound
        // belongs to the timeline and not to the blocks, hosted or not.
        match &first {
            Some(first) => assert_same_bits(&actual, first, &format!("{what}, against the first")),
            None => first = Some(actual),
        }
    }
}

#[test]
fn the_sidechain_is_what_makes_the_difference() {
    let library = library();
    let input = programme(2, FRAMES);
    let sidechain = key(2, FRAMES);
    // The shortest release the plugin has, 5 ms, so that it lets go well
    // within the gaps in the sidechain.
    let fast = [(
        0,
        EventKind::ParamValue {
            param: RELEASE,
            value: 0.0,
        },
    )];

    // Nothing routed to the sidechain: the plugin hears silence there and
    // leaves the input alone, bit for bit.
    let (alone, rendered) = hosted(&library, 256, &[256], &fast, &input, None);
    assert_same_bits(
        &alone,
        &reference(256, &[256], &fast, &input, None),
        "no sidechain",
    );
    assert_same_bits(&alone, &input, "an input with nothing to duck under");
    assert_eq!(rendered.heap_calls, 0);

    // With it, the input goes down while the sidechain is loud and is back
    // by the time it has been quiet for a while.
    let (ducked, _) = hosted(&library, 256, &[256], &fast, &input, Some(&sidechain));
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

    // A mono sidechain into the stereo port: its one channel is used and
    // the other is silent, as with no host.
    let mono = key(1, FRAMES);
    let (actual, _) = hosted(&library, 256, &[256], &fast, &input, Some(&mono));
    let expected = reference(256, &[256], &fast, &input, Some(&mono));
    assert_same_bits(&actual, &expected, "a mono sidechain");
    assert!(actual != alone);
}

#[test]
fn buffers_of_another_width_are_served() {
    let library = library();
    let sidechain = key(2, FRAMES);
    let schedule = automation();
    let stereo = programme(2, FRAMES);
    let expected = reference(256, &[256], &schedule, &stereo, Some(&sidechain));

    // Wider than the plugin: it processes the channels it has, and the
    // rest come out silent, not as whatever went in.
    let wide_input = programme(5, FRAMES);
    let (wide, rendered) = hosted(
        &library,
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

    // Narrower: the plugin's missing input is silent, and the output
    // channel there is no room for is dropped.
    let mono_input = programme(1, FRAMES);
    let (mono, rendered) = hosted(
        &library,
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
    let library = library();
    let mut plugin = library.create(ID).unwrap();
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
                param: DEPTH,
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
        // A parameter the plugin does not have.
        (
            40,
            EventKind::ParamValue {
                param: ParamId(12_345),
                value: 0.5,
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
    assert_eq!(processor.status(), ProcessStatus::Continue);
    // The plugin declares no note port, so it is sent no notes; the value
    // it can take, it got. Nothing was dropped for want of room.
    assert_eq!(plugin.undeliverable_events(), 3);
    assert_eq!(plugin.lost_events(), 0);
    assert_eq!(plugin.param_value(DEPTH), Some(1.0));
    assert_same_bits(&audio, &input, "an effect with a quiet sidechain");
}

#[test]
fn the_switch_and_the_state_work_from_the_main_thread() {
    let library = library();
    let input = programme(2, 9_600);
    let sidechain = key(2, 9_600);
    let declared = Duck::new().params();
    let plain = |id, value| plain_from_clap(declared.find_param(id).unwrap(), value);

    let mut plugin = library.create(ID).unwrap();
    plugin.set_param(DEPTH, 1.0).unwrap();
    plugin.set_param(BYPASS, 1.0).unwrap();
    assert_eq!(plugin.param_value(BYPASS), Some(1.0));
    assert_eq!(plugin.param_to_text(BYPASS, 1.0).as_deref(), Some("On"));
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
    plugin.set_param(BYPASS, 0.0).unwrap();
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
    assert_eq!(plugin.param_value(BYPASS), Some(0.0));

    let mut harness = Harness::<Duck>::new(RATE, 256);
    harness.params().depth.set(plain(DEPTH, 1.0) as f32);
    harness.params().bypass.set(true);
    // The harness prepared the plugin before the switch was set; the host
    // set it first. Prepare again so both start bypassed.
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
    let mut other = library.create(ID).unwrap();
    assert_eq!(other.param_value(BYPASS), Some(0.0));
    other.set_stream_chunk(2);
    other.load_state(&saved).unwrap();
    assert_eq!(other.param_value(BYPASS), Some(1.0));
    assert_eq!(other.param_value(DEPTH), Some(1.0));
    let rescan = ParamsRescan {
        values: true,
        ..ParamsRescan::default()
    };
    assert_eq!(other.pump(), [HostEvent::ParamsRescanned(rescan)]);
    let mut processor = other.activate(RATE, 256).unwrap();
    let mut audio = input.clone();
    processor.process(&mut audio);
    assert_same_bits(&audio, &input, "restored into bypass");
}
