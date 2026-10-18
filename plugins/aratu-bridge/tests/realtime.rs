//! Proves that the plugin's processing never touches the heap while a page
//! sends it everything it can: events by beat and for now, parts made,
//! changed and removed, chains with new effects, SFZ instruments, panics,
//! and a whole new session, while the host loops, jumps, stops and changes
//! its tempo and the gain moves.
//!
//! The global allocator counts heap calls made by the audio thread, this
//! test's thread, while it is inside `process`. The page is a client on
//! another thread, and the plugin's server and loader threads allocate as
//! they please.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aratu_bridge::AratuBridge;
use aratu_bridge::client::Client;
use aratu_bridge::protocol::{BeatRecord, page};
use common::*;
use manguebyte::prelude::*;
use manguebyte_web::{Chain, Effect, EventRecord, InstrumentKind};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn heap_calls_during(work: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

const MAX_BLOCK: usize = 512;

/// What the page does, over and over, until told to stop.
fn page_script(port: u16, done: &AtomicBool, rounds: usize) {
    let (text, files) = tone_instrument();
    for round in 0..rounds {
        let mut client = Client::connect(port, ORIGIN).unwrap();
        let epoch = client.hello().unwrap().epoch;
        for part in 0..4u16 {
            let kind = [
                InstrumentKind::Synth,
                InstrumentKind::Drums,
                InstrumentKind::Sampler,
                InstrumentKind::Plucked,
            ][part as usize];
            assert_eq!(client.create_part(part, kind, Chain::FULL).unwrap(), 0);
        }
        client.load_sfz(2, &text, &files).unwrap().unwrap();
        for step in 0..40u16 {
            let base = f64::from(step) * 0.25;
            let mut records = Vec::new();
            for part in 0..4u16 {
                records.push(BeatRecord {
                    beat: base,
                    record: EventRecord::note_on(0, part, 48 + (step % 12) as u8, 0.9),
                });
                records.push(BeatRecord {
                    beat: base + 0.2,
                    record: EventRecord::note_off(0, part, 48 + (step % 12) as u8),
                });
            }
            records.push(BeatRecord {
                beat: f64::NAN,
                record: EventRecord::note_on(0, 1, 38, 0.5),
            });
            // This lap, the next, and a stale one.
            client.events(epoch, &records).unwrap();
            client.events(epoch + 1, &records[..2]).unwrap();
            client.events(epoch.wrapping_sub(3), &records[..2]).unwrap();
            match step % 8 {
                0 => {
                    client
                        .set_chain(0, Chain::FULL.with(Effect::Delay).with(Effect::Chorus))
                        .unwrap();
                }
                2 => {
                    client.set_instrument(1, InstrumentKind::DrumKit).unwrap();
                }
                4 => {
                    client.remove_part(3).unwrap();
                    client
                        .create_part(3, InstrumentKind::Fm, Chain::NONE)
                        .unwrap();
                }
                5 => {
                    client.load_sfz(2, &text, &files).unwrap().unwrap();
                }
                6 => {
                    client.request(page::PANIC, &[]).unwrap();
                }
                _ => {}
            }
            client.listen(Duration::from_millis(2)).unwrap();
        }
        client.sync().unwrap();
        if round + 1 < rounds {
            client.close(1000).unwrap();
        }
    }
    done.store(true, Ordering::Release);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn processing_never_touches_the_heap_while_a_page_plays() {
    assert!(heap_calls_during(|| drop(vec![0u8; 16])) >= 2);

    let mut plugin = AratuBridge::new();
    plugin.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: MAX_BLOCK,
        num_channels: 2,
    });
    let params = plugin.params();
    plugin.set_port_override(Some(0));
    let port = plugin.wait_listening(Duration::from_secs(10)).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let page = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || page_script(port, &done, 2))
    };

    let mut buffer = AudioBuffer::<f32>::new(2, MAX_BLOCK);
    let events = EventBuffer::with_capacity(8);
    let mut transport = Transport {
        loop_beats: Some((0.0, 4.0)),
        ..Transport::playing_at(120.0)
    };
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut below = move |n: u64| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    };
    let mut heap = 0;
    let mut blocks = 0u64;
    let mut loudest = 0.0f32;
    let mut after = 0;
    while after < 16 || blocks < 2_000 {
        if done.load(Ordering::Acquire) {
            after += 1;
        }
        let frames = 1 + below(MAX_BLOCK as u64) as usize;
        buffer.set_num_frames(frames);
        match below(400) {
            0 => transport.position_beats = Some(below(16) as f64),
            1 => transport.playing = !transport.playing,
            2 => transport.tempo_bpm = Some(60.0 + below(120) as f64),
            3 => params.gain.set(-(below(24) as f32)),
            _ => {}
        }
        let context = ProcessContext::new(&events, &transport);
        heap += heap_calls_during(|| plugin.process(&mut buffer, &context));
        loudest = buffer
            .iter_channels()
            .flat_map(|channel| channel.iter())
            .fold(loudest, |peak, sample| peak.max(sample.abs()));
        transport.advance(frames, RATE);
        blocks += 1;
        // Leave the page room to work, as a real audio callback does.
        if blocks % 8 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    page.join().unwrap();
    assert_eq!(heap, 0, "processing touched the heap");
    assert!(loudest > 0.01, "the page was heard");
    let status = plugin.status().unwrap();
    assert!(status.immediate > 0 && status.stale > 0, "{status:?}");
    let engine = plugin.engine().unwrap();
    assert_eq!(engine.part_kind(1), Some(InstrumentKind::DrumKit));
    assert_eq!(engine.part_kind(3), Some(InstrumentKind::Fm));
}
