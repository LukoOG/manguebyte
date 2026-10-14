//! Compiles the instrument template exactly as it is embedded in the tool,
//! and runs the tests it carries.
//!
//! See `template_effect.rs` for why. The two templates are in separate
//! test programs because each exports the entry points of a CLAP and a
//! VST 3 plugin, and a program can export those names only once.

#[path = "common/heap.rs"]
mod heap;
#[path = "../templates/instrument.rs"]
mod template;

use manguebyte::prelude::*;
use template::PluginTemplate;

#[test]
fn the_guard_itself_catches_an_allocation() {
    let calls = heap::calls_during(|| drop(std::hint::black_box(vec![0u8; 64])));
    assert!(calls >= 2, "the allocation guard saw {calls} calls");
    assert_eq!(heap::calls_during(|| ()), 0);
}

#[test]
fn playing_does_not_touch_the_heap() {
    let mut plugin = PluginTemplate::new();
    plugin.prepare(&ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 256,
        num_channels: 2,
    });

    // More notes than the instrument has voices, so that voices are taken
    // over; a change to the release time; the output level turned down and
    // up again while it is still gliding; and the notes let go.
    let mut events = EventBuffer::with_capacity(64);
    let mut add = |offset: u32, kind: EventKind| events.push(Event { offset, kind }).unwrap();
    for key in 0..20u8 {
        let note = NoteRef::new(0, 48 + key);
        add(
            u32::from(key),
            EventKind::NoteOn {
                note,
                velocity: 0.7,
            },
        );
    }
    add(
        40,
        EventKind::ParamValue {
            param: ParamId::from_name("release"),
            value: 0.05,
        },
    );
    for (offset, value) in [(50, -24.0), (90, -9.0)] {
        let param = ParamId::from_name("level");
        add(offset, EventKind::ParamValue { param, value });
    }
    for key in 0..20u8 {
        let note = NoteRef::new(0, 48 + key);
        add(
            100 + u32::from(key),
            EventKind::NoteOff {
                note,
                velocity: 0.0,
            },
        );
    }
    let transport = Transport::playing_at(120.0);
    let mut buffer = AudioBuffer::<f32>::new(2, 256);
    let mut loudest = 0.0f32;

    let calls = heap::calls_during(|| {
        for frames in [256, 1, 64, 255, 256] {
            buffer.set_num_frames(frames);
            buffer.clear();
            plugin.process(&mut buffer, &ProcessContext::new(&events, &transport));
            loudest = loudest.max(buffer.peak());
        }
        plugin.reset();
        buffer.set_num_frames(256);
        buffer.clear();
        plugin.process(&mut buffer, &ProcessContext::empty());
    });
    assert_eq!(calls, 0, "process used the heap {calls} time(s)");
    // It was playing all the while, and is silent once reset.
    assert!(loudest > 0.01, "nothing sounded: {loudest}");
    assert_eq!(buffer.peak(), 0.0);
}

/// The template tells whoever generates a plugin from it that the output
/// does not depend on how the host sizes its blocks. That has to hold for
/// the output level as much as for the notes: the wrappers store every
/// parameter value of a block in the parameter before `process` is called,
/// so a plugin that reads the level once per block starts a change of level
/// at the start of whichever block it falls in.
///
/// Found in review, when the template did read it once per block: with the
/// level automated at frame 5,000 the render in blocks of 64 first differed
/// from the render in blocks of 256 at frame 4,864, and with the pattern
/// 1, 999, 13, 1,024 at frame 4,075, which is 19 ms early.
#[test]
fn the_instrument_sounds_the_same_whatever_the_block_size_when_its_level_is_automated() {
    const FRAMES: usize = 12_000;
    let render = |max: usize, pattern: &[usize]| {
        let mut harness = Harness::<PluginTemplate>::new(48_000.0, max).with_block_pattern(pattern);
        harness.note(0, 20_000, 57, 0.8);
        harness.automate(5_000, "level", -30.0);
        harness.render(FRAMES)
    };
    let reference = render(256, &[256]);
    assert!(reference.peak() > 0.01, "nothing sounded");
    // The first frame at which each other way of cutting the blocks gives
    // a different sample, if any.
    let differences: Vec<(&[usize], Option<usize>)> =
        [(64, &[64][..]), (1_024, &[1, 999, 13, 1_024])]
            .into_iter()
            .map(|(max, pattern)| {
                let other = render(max, pattern);
                let first = (0..FRAMES)
                    .find(|&frame| other.channel(0)[frame] != reference.channel(0)[frame]);
                (pattern, first)
            })
            .collect();
    assert!(
        differences.iter().all(|(_, first)| first.is_none()),
        "with the level automated at frame 5000, the sound differs from that of blocks of 256 \
         from these frames on (block pattern, first frame): {differences:?}"
    );

    // And the change is heard where it was asked for: next to a render that
    // was left alone, nothing differs before frame 5,000, and once the glide
    // of 20 ms is over the sound is 18 dB below it.
    let mut untouched = Harness::<PluginTemplate>::new(48_000.0, 256);
    untouched.note(0, 20_000, 57, 0.8);
    let untouched = untouched.render(FRAMES);
    assert_eq!(untouched.channel(0)[..5_000], reference.channel(0)[..5_000]);
    let ratio = manguebyte::decibels::db_to_gain(-18.0) as f32;
    for frame in 6_000..FRAMES {
        let expected = untouched.channel(0)[frame] * ratio;
        assert!(
            (reference.channel(0)[frame] - expected).abs() < 1e-6,
            "frame {frame}"
        );
    }
}
