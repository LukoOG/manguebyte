//! Proves that the audio thread reads published snapshots without touching
//! the heap.
//!
//! The global allocator is replaced with one that counts, per thread, and
//! the reading side runs on a thread of its own while another thread
//! builds and publishes snapshots from a document.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use manguebyte_state::{Document, NodeTree, Value, snapshot};

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

/// Runs `body` and returns how many times it touched the heap.
fn heap_calls(body: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    body();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

/// What an audio thread might need from a sampler's document.
#[derive(Default)]
struct Zones {
    version: u64,
    keys: Vec<i64>,
    names: Vec<String>,
}

fn zones(doc: &Document) -> Zones {
    let zones: Vec<_> = doc.children_of_kind(doc.root(), "zone").collect();
    Zones {
        version: doc.version(),
        keys: zones
            .iter()
            .filter_map(|zone| doc.get(*zone, "key")?.as_int())
            .collect(),
        names: zones
            .iter()
            .filter_map(|zone| Some(doc.get(*zone, "name")?.as_text()?.to_owned()))
            .collect(),
    }
}

#[test]
fn the_heap_counter_works() {
    assert!(heap_calls(|| drop(Box::new(1))) >= 2);
}

#[test]
fn reading_snapshots_never_touches_the_heap() {
    let rounds: usize = if cfg!(miri) { 20 } else { 600 };
    let mut doc = Document::new("instrument");
    let (mut writer, mut reader) = snapshot::channel(Zones::default(), 4);
    let done = Arc::new(AtomicBool::new(false));

    let audio = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let mut seen = 0u64;
            let mut updates = 0usize;
            let mut total = 0i64;
            let calls = heap_calls(|| {
                loop {
                    let finished = done.load(Ordering::Acquire);
                    if reader.update() {
                        updates += 1;
                    }
                    let zones = reader.get();
                    assert!(zones.version >= seen, "snapshots arrive in order");
                    seen = zones.version;
                    total = total.wrapping_add(zones.keys.iter().sum::<i64>());
                    total = total.wrapping_add(
                        zones
                            .names
                            .iter()
                            .map(|name| name.len() as i64)
                            .sum::<i64>(),
                    );
                    let _ = reader.read().keys.len();
                    if finished && !reader.update() {
                        break;
                    }
                    std::hint::spin_loop();
                }
            });
            (calls, updates, seen, total, reader)
        })
    };

    let mut published = 0;
    for round in 0..rounds {
        doc.edit("Add zone", |tx| {
            let zone = tx.add_child(tx.root(), "zone")?;
            tx.set(zone, "key", round as i64)?;
            tx.set(zone, "name", format!("zone {round}"))
        })
        .unwrap();
        if round % 7 == 0 {
            doc.undo();
        }
        if writer.publish_if_changed(&doc, zones) {
            published += 1;
        } else {
            thread::yield_now();
        }
        // Reuse a box the reader gave back, now and then.
        if round % 3 == 0 {
            if let Some(mut boxed) = writer.take_retired() {
                *boxed = zones(&doc);
                let _ = writer.publish_boxed(boxed);
            }
        }
    }
    // Make sure the newest state goes out.
    let last = doc.version();
    while writer.published_version() != Some(last) {
        writer.publish_if_changed(&doc, zones);
        thread::yield_now();
    }
    done.store(true, Ordering::Release);
    let (calls, updates, seen, _, reader) = audio.join().unwrap();
    assert_eq!(calls, 0, "the reading side touched the heap {calls} times");
    assert!(published > 0 && updates > 0);
    assert_eq!(seen, last, "the reader ends on the newest snapshot");
    writer.collect();
    drop(reader);
}

#[test]
fn a_subtree_is_a_snapshot_the_audio_thread_can_read() {
    let mut doc = Document::new("instrument");
    doc.edit("Build", |tx| {
        let zone = tx.add_child(tx.root(), "zone")?;
        tx.set(zone, "key", 60)?;
        tx.set(zone, "gain", 0.5)?;
        tx.set(zone, "wave", vec![0u8; 256])
    })
    .unwrap();
    let (mut writer, mut reader) = snapshot::channel(NodeTree::new("empty"), 2);
    writer.publish(doc.subtree(doc.root()).unwrap()).unwrap();
    let calls = heap_calls(|| {
        let tree = reader.read();
        let zone = tree.children_of_kind("zone").next().unwrap();
        assert_eq!(zone.get("key"), Some(&Value::Int(60)));
        assert_eq!(
            zone.get("wave").and_then(Value::as_bytes).map(<[u8]>::len),
            Some(256)
        );
        assert!(zone.get("missing").is_none());
        assert!(!reader.update());
    });
    assert_eq!(calls, 0);

    // The writer frees what the reader let go of, on its own thread.
    writer.publish(NodeTree::new("next")).unwrap();
    assert_eq!(heap_calls(|| assert!(reader.update())), 0);
    assert_eq!(writer.collect(), 1);
}

#[test]
fn a_full_queue_hands_the_snapshot_back() {
    let (mut writer, mut reader) = snapshot::channel(0u32, 2);
    writer.publish(1).unwrap();
    writer.publish(2).unwrap();
    assert_eq!(writer.publish(3), Err(3));
    assert_eq!(*reader.read(), 2, "the newest wins");
    assert_eq!(writer.collect(), 2, "the first value and 1 come back");
    writer.publish(3).unwrap();
    assert_eq!(*reader.read(), 3);
}
