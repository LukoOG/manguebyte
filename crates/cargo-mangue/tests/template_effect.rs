//! Compiles the effect template exactly as it is embedded in the tool, and
//! runs the tests it carries.
//!
//! `cargo mangue new` promises a crate that builds and passes its own
//! tests. The template is valid Rust for a plugin called "Plugin Template",
//! so it is included here as a module: if the framework changes under it,
//! this crate's tests fail, and the template is fixed before anyone
//! generates a broken plugin from it. `tests/scaffold.rs` shows that a
//! generated crate differs from the template in its names and in nothing
//! else.
//!
//! The template also tells its reader that nothing in `process` touches
//! the heap. The test below holds it to that, with the counting allocator
//! the framework uses on its own processors.

#[path = "common/heap.rs"]
mod heap;
#[path = "../templates/effect.rs"]
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
fn processing_does_not_touch_the_heap() {
    let mut plugin = PluginTemplate::new();
    plugin.prepare(&ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 256,
        num_channels: 2,
    });

    // Automation in the middle of a block, twice, the second while the
    // first is still gliding.
    let gain = ParamId::from_name("gain");
    let mut events = EventBuffer::with_capacity(8);
    for (offset, value) in [(10, -12.0), (100, 3.0)] {
        let kind = EventKind::ParamValue { param: gain, value };
        events.push(Event { offset, kind }).unwrap();
    }
    let transport = Transport::playing_at(120.0);
    let mut buffer = AudioBuffer::<f32>::new(2, 256);

    let calls = heap::calls_during(|| {
        for frames in [256, 1, 64, 255, 256] {
            buffer.set_num_frames(frames);
            for channel in buffer.iter_channels_mut() {
                channel.fill(0.25);
            }
            plugin.process(&mut buffer, &ProcessContext::new(&events, &transport));
        }
        plugin.reset();
        buffer.set_num_frames(256);
        for channel in buffer.iter_channels_mut() {
            channel.fill(0.25);
        }
        plugin.process(&mut buffer, &ProcessContext::empty());
    });
    assert_eq!(calls, 0, "process used the heap {calls} time(s)");
    // It was processing all the while: the last block came out at the
    // gain the automation asked for, 3 dB above the 0.25 that went in.
    let expected = 0.25 * manguebyte::decibels::db_to_gain(3.0) as f32;
    assert!((buffer.peak() - expected).abs() < 1e-6, "{}", buffer.peak());
}
