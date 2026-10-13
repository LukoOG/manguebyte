//! Behaviour of the audio graph, including what it must not do on the audio
//! thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::{ProcessSpec, Processor};
use manguebyte_graph::{Graph, GraphEngine, GraphError, NodeId};

// --- Watching the heap ---------------------------------------------------

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
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

/// Runs `work` and returns how many times it used the heap.
fn heap_calls_during(work: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

// --- Nodes for testing ---------------------------------------------------

/// Multiplies its input.
struct Scale(f32);
impl Processor for Scale {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        buffer.apply_gain(self.0);
    }
}

/// Ignores its input and outputs a constant.
struct Constant(f32);
impl Processor for Constant {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        for channel in buffer.iter_channels_mut() {
            channel.fill(self.0);
        }
    }
}

/// Outputs 1, 2, 3, ... and so shows whether its state was kept.
struct Counter(f32);
impl Processor for Counter {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        for frame in 0..buffer.num_frames() {
            self.0 += 1.0;
            for channel in 0..buffer.num_channels() {
                buffer.channel_mut(channel)[frame] = self.0;
            }
        }
    }
}

/// Delays its input by a number of frames and says so.
struct Late {
    frames: usize,
    memory: Vec<f32>,
    position: usize,
}
impl Late {
    fn new(frames: usize) -> Self {
        Self {
            frames,
            memory: vec![0.0; frames],
            position: 0,
        }
    }
}
impl Processor for Late {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        // Mono is enough for these tests.
        for sample in buffer.channel_mut(0) {
            std::mem::swap(sample, &mut self.memory[self.position]);
            self.position = (self.position + 1) % self.frames;
        }
    }
    fn latency_samples(&self) -> usize {
        self.frames
    }
}

/// Records when and where it is dropped.
struct Witness {
    dropped: Arc<AtomicBool>,
    dropped_while_watching: Arc<AtomicBool>,
}
impl Processor for Witness {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer) {}
}
impl Drop for Witness {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
        if WATCHING.with(Cell::get) {
            self.dropped_while_watching.store(true, Ordering::Relaxed);
        }
    }
}

const MONO: ProcessSpec = ProcessSpec {
    sample_rate: 48_000.0,
    max_block_size: 8,
    num_channels: 1,
};

fn graph() -> (Graph, GraphEngine) {
    Graph::new(MONO)
}

const INPUT: NodeId = Graph::<f32>::INPUT;
const OUTPUT: NodeId = Graph::<f32>::OUTPUT;

/// Runs one block of ones through the engine and returns the result.
fn run(engine: &mut GraphEngine) -> Vec<f32> {
    run_block(engine, &[1.0; 4])
}

fn run_block(engine: &mut GraphEngine, input: &[f32]) -> Vec<f32> {
    let mut buffer = AudioBuffer::<f32>::new(1, input.len());
    buffer.channel_mut(0).copy_from_slice(input);
    engine.process(&mut buffer);
    buffer.channel(0).to_vec()
}

// --- The tests -----------------------------------------------------------

#[test]
fn an_empty_graph_is_silent_and_a_wire_passes_audio_through() {
    let (mut graph, mut engine) = graph();
    assert_eq!(run(&mut engine), [0.0; 4]);
    assert!(graph.is_empty());

    graph.connect(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [1.0; 4]);
}

#[test]
fn nodes_run_in_series_and_inputs_are_summed() {
    let (mut graph, mut engine) = graph();
    let double = graph.add(Scale(2.0));
    let triple = graph.add(Scale(3.0));
    graph.connect(INPUT, double).unwrap();
    graph.connect(double, triple).unwrap();
    graph.connect(triple, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [6.0; 4]);
    assert_eq!(engine.num_nodes(), 2);

    // Fan out to both and sum at the output: 2 + 6, plus the dry input.
    graph.connect(double, OUTPUT).unwrap();
    graph.connect(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [9.0; 4]);

    // A node with two inputs receives their sum: (1 + 2) * 3.
    graph.disconnect(double, OUTPUT).unwrap();
    graph.disconnect(INPUT, OUTPUT).unwrap();
    graph.connect(INPUT, triple).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [9.0; 4]);
}

#[test]
fn running_order_follows_the_connections_not_the_order_of_adding() {
    let (mut graph, mut engine) = graph();
    let last = graph.add(Scale(10.0));
    let middle = graph.add(Constant(0.5));
    let first = graph.add(Scale(100.0));
    graph.connect(INPUT, first).unwrap();
    graph.connect(first, middle).unwrap();
    graph.connect(middle, last).unwrap();
    graph.connect(last, OUTPUT).unwrap();
    graph.commit().unwrap();
    // The constant replaces whatever came before it.
    assert_eq!(run(&mut engine), [5.0; 4]);
}

#[test]
fn impossible_connections_are_refused_and_change_nothing() {
    let (mut graph, mut engine) = graph();
    let a = graph.add(Scale(2.0));
    let b = graph.add(Scale(3.0));
    graph.connect(INPUT, a).unwrap();
    graph.connect(a, b).unwrap();
    graph.connect(b, OUTPUT).unwrap();

    assert_eq!(graph.connect(b, a), Err(GraphError::Cycle));
    assert_eq!(graph.connect(a, a), Err(GraphError::InvalidConnection));
    assert_eq!(graph.connect(OUTPUT, a), Err(GraphError::InvalidConnection));
    assert_eq!(graph.connect(a, INPUT), Err(GraphError::InvalidConnection));

    graph.remove(b).unwrap();
    assert_eq!(graph.connect(a, b), Err(GraphError::UnknownNode(b)));
    assert_eq!(graph.remove(b), Err(GraphError::UnknownNode(b)));
    assert!(!graph.contains(b) && graph.contains(a));
    assert_eq!(graph.len(), 1);

    // Removing a node removed its connections; the rest is intact.
    graph.connect(a, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [2.0; 4]);
}

#[test]
fn nothing_changes_until_commit_and_then_all_at_once() {
    let (mut graph, mut engine) = graph();
    let double = graph.add(Scale(2.0));
    graph.connect(INPUT, double).unwrap();
    graph.connect(double, OUTPUT).unwrap();
    assert_eq!(run(&mut engine), [0.0; 4], "ran an uncommitted graph");

    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [2.0; 4]);

    let triple = graph.add(Scale(3.0));
    graph.disconnect(double, OUTPUT).unwrap();
    graph.connect(double, triple).unwrap();
    graph.connect(triple, OUTPUT).unwrap();
    assert_eq!(run(&mut engine), [2.0; 4], "half an edit was heard");

    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [6.0; 4]);
}

#[test]
fn a_node_keeps_its_state_when_the_graph_is_rewired_around_it() {
    let (mut graph, mut engine) = graph();
    let counter = graph.add(Counter(0.0));
    graph.connect(counter, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [1.0, 2.0, 3.0, 4.0]);

    let double = graph.add(Scale(2.0));
    graph.disconnect(counter, OUTPUT).unwrap();
    graph.connect(counter, double).unwrap();
    graph.connect(double, OUTPUT).unwrap();
    graph.commit().unwrap();
    // The counter carried on from 4 instead of starting again.
    assert_eq!(run(&mut engine), [10.0, 12.0, 14.0, 16.0]);

    // Two commits before the engine runs are applied in order.
    graph.remove(double).unwrap();
    graph.commit().unwrap();
    graph.connect(counter, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [9.0, 10.0, 11.0, 12.0]);
}

#[test]
fn a_removed_node_is_freed_by_the_editing_thread_never_while_processing() {
    let dropped = Arc::new(AtomicBool::new(false));
    let dropped_while_watching = Arc::new(AtomicBool::new(false));
    let (mut graph, mut engine) = graph();
    let witness = graph.add(Witness {
        dropped: Arc::clone(&dropped),
        dropped_while_watching: Arc::clone(&dropped_while_watching),
    });
    graph.connect(INPUT, witness).unwrap();
    graph.connect(witness, OUTPUT).unwrap();
    graph.commit().unwrap();
    run(&mut engine);

    graph.remove(witness).unwrap();
    graph.commit().unwrap();
    assert!(!dropped.load(Ordering::Relaxed), "dropped before the swap");

    let mut buffer = AudioBuffer::<f32>::new(1, 4);
    let calls = heap_calls_during(|| engine.process(&mut buffer));
    assert_eq!(calls, 0, "the swap used the heap");
    assert!(
        !dropped.load(Ordering::Relaxed),
        "dropped on the audio thread"
    );

    graph.collect_garbage();
    assert!(dropped.load(Ordering::Relaxed));
    assert!(!dropped_while_watching.load(Ordering::Relaxed));
}

#[test]
fn update_reaches_a_node_before_and_after_it_starts_running() {
    let (mut graph, mut engine) = graph();
    let scale = graph.add(Scale(2.0));
    graph.connect(INPUT, scale).unwrap();
    graph.connect(scale, OUTPUT).unwrap();

    // Still on this side: applied at once.
    assert!(graph.update(scale, |node: &mut Scale| node.0 = 5.0));
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [5.0; 4]);

    // Now on the audio side: applied at the start of the next block.
    assert!(graph.update(scale, |node: &mut Scale| node.0 = 7.0));
    let mut buffer = AudioBuffer::<f32>::new(1, 4);
    buffer.channel_mut(0).fill(1.0);
    let calls = heap_calls_during(|| engine.process(&mut buffer));
    assert_eq!(buffer.channel(0), &[7.0; 4]);
    assert_eq!(calls, 0, "applying an update used the heap");

    // The wrong type changes nothing, and a missing node is reported.
    assert!(graph.update(scale, |node: &mut Constant| node.0 = 99.0));
    assert_eq!(run(&mut engine), [7.0; 4]);
    graph.remove(scale).unwrap();
    assert!(!graph.update(scale, |node: &mut Scale| node.0 = 1.0));
}

#[test]
fn paths_of_different_latency_are_lined_up() {
    let (mut graph, mut engine) = graph();
    let late = graph.add(Late::new(5));
    graph.connect(INPUT, late).unwrap();
    graph.connect(late, OUTPUT).unwrap();
    graph.connect(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(graph.latency_samples(), 5);

    let mut impulse = [0.0f32; 8];
    impulse[0] = 1.0;
    let output = run_block(&mut engine, &impulse);
    assert_eq!(engine.latency_samples(), 5);
    // Without compensation there would be a 1 on frame 0 and another on
    // frame 5. With it, both copies arrive together.
    assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0]);

    // Latency adds up along a path and the longest path sets the total.
    let later = graph.add(Late::new(2));
    graph.disconnect(late, OUTPUT).unwrap();
    graph.connect(late, later).unwrap();
    graph.connect(later, OUTPUT).unwrap();
    graph.commit().unwrap();
    engine.reset();
    assert_eq!(graph.latency_samples(), 7);
    let output = run_block(&mut engine, &impulse);
    assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0]);

    // A node can be told its latency changed.
    graph.set_latency(later, 0).unwrap();
    graph.commit().unwrap();
    assert_eq!(graph.latency_samples(), 5);
}

#[test]
fn a_block_longer_than_prepared_is_run_in_pieces() {
    let (mut graph, mut engine) = graph();
    let counter = graph.add(Counter(0.0));
    graph.connect(counter, OUTPUT).unwrap();
    graph.commit().unwrap();
    let output = run_block(&mut engine, &[0.0; 20]);
    let expected: Vec<f32> = (1..=20).map(|n| n as f32).collect();
    assert_eq!(output, expected);
}

#[test]
fn preparing_again_reaches_every_node_and_resizes_the_buffers() {
    struct Remembers(Arc<AtomicUsize>);
    impl Processor for Remembers {
        fn prepare(&mut self, spec: &ProcessSpec) {
            self.0.store(spec.max_block_size, Ordering::Relaxed);
        }
        fn process(&mut self, buffer: &mut AudioBuffer) {
            buffer.channel_mut(0).fill(1.0);
        }
    }

    let seen = Arc::new(AtomicUsize::new(0));
    let (mut graph, mut engine) = graph();
    let node = graph.add(Remembers(Arc::clone(&seen)));
    assert_eq!(seen.load(Ordering::Relaxed), 8, "prepared when added");
    graph.connect(node, OUTPUT).unwrap();
    graph.commit().unwrap();

    engine.prepare(&ProcessSpec {
        max_block_size: 64,
        ..MONO
    });
    assert_eq!(seen.load(Ordering::Relaxed), 64);
    let mut buffer = AudioBuffer::<f32>::new(1, 64);
    let calls = heap_calls_during(|| engine.process(&mut buffer));
    assert_eq!(buffer.channel(0), &[1.0; 64]);
    assert_eq!(calls, 0, "a 64-frame block was split or reallocated");

    // Nodes added afterwards are prepared with the new specification.
    let newer = Arc::new(AtomicUsize::new(0));
    graph.add(Remembers(Arc::clone(&newer)));
    assert_eq!(newer.load(Ordering::Relaxed), 64);
}

#[test]
fn too_many_pending_edits_are_refused_without_losing_nodes() {
    let (mut graph, mut engine) = graph();
    let scale = graph.add(Scale(4.0));
    graph.connect(INPUT, scale).unwrap();
    graph.connect(scale, OUTPUT).unwrap();

    // Nobody is processing, so the queue fills up.
    let mut refused = 0;
    for _ in 0..1_000 {
        if graph.commit() == Err(GraphError::Busy) {
            refused += 1;
        }
    }
    assert!(refused > 0, "the queue never filled");

    // Once the engine runs, everything queued is applied and edits resume.
    assert_eq!(run(&mut engine), [4.0; 4]);
    let extra = graph.add(Scale(0.5));
    graph.disconnect(scale, OUTPUT).unwrap();
    graph.connect(scale, extra).unwrap();
    graph.connect(extra, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(run(&mut engine), [2.0; 4]);
}

#[test]
fn processing_a_large_graph_uses_no_heap() {
    let stereo = ProcessSpec {
        num_channels: 2,
        max_block_size: 64,
        ..MONO
    };
    let (mut graph, mut engine) = Graph::<f32>::new(stereo);
    let mut previous = INPUT;
    for index in 0..40 {
        let node = graph.add(Scale(1.01));
        graph.connect(previous, node).unwrap();
        if index % 5 == 0 {
            graph.connect(node, OUTPUT).unwrap();
        }
        previous = node;
    }
    let late = graph.add(Late::new(16));
    graph.connect(INPUT, late).unwrap();
    graph.connect(late, OUTPUT).unwrap();
    graph.commit().unwrap();

    let mut buffer = AudioBuffer::<f32>::new(2, 64);
    let calls = heap_calls_during(|| {
        for _ in 0..8 {
            buffer.channel_mut(0).fill(0.5);
            engine.process(&mut buffer);
        }
    });
    assert_eq!(calls, 0);
    assert!(buffer.peak() > 0.0);
}

#[test]
fn the_graph_can_be_edited_from_one_thread_while_another_runs_it() {
    let (mut graph, mut engine) = graph();
    let stop = Arc::new(AtomicBool::new(false));
    let blocks = Arc::new(AtomicUsize::new(0));

    let audio = std::thread::spawn({
        let stop = Arc::clone(&stop);
        let blocks = Arc::clone(&blocks);
        move || {
            let mut buffer = AudioBuffer::<f32>::new(1, 8);
            let mut last = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                buffer.channel_mut(0).fill(1.0);
                engine.process(&mut buffer);
                assert!(buffer.peak().is_finite());
                blocks.fetch_add(1, Ordering::Relaxed);
                last = buffer.channel(0).to_vec();
                std::thread::yield_now();
            }
            // One more block, so the last commit has certainly been applied.
            buffer.channel_mut(0).fill(1.0);
            engine.process(&mut buffer);
            last.clear();
            last.extend_from_slice(buffer.channel(0));
            last
        }
    });

    // Keep rebuilding a chain of varying length while the engine runs.
    let mut chain: Vec<NodeId> = Vec::new();
    for round in 0..300 {
        for &node in &chain {
            graph.remove(node).unwrap();
        }
        chain.clear();
        graph.disconnect(INPUT, OUTPUT).unwrap();
        let mut previous = INPUT;
        for _ in 0..(round % 5) {
            let node = graph.add(Scale(2.0));
            graph.connect(previous, node).unwrap();
            chain.push(node);
            previous = node;
        }
        graph.connect(previous, OUTPUT).unwrap();
        while graph.commit() == Err(GraphError::Busy) {
            std::thread::yield_now();
        }
    }
    while blocks.load(Ordering::Relaxed) == 0 {
        std::thread::yield_now();
    }
    stop.store(true, Ordering::Relaxed);
    let last = audio.join().unwrap();
    // Round 299 built a chain of 299 % 5 = 4 doublers.
    assert_eq!(last, vec![16.0; 8]);
}
