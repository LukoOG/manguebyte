//! Behaviour of the audio graph, including what it must not do on the audio
//! thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use manguebyte_core::AudioBuffer;
use manguebyte_core::source::BufferSource;
use manguebyte_dsp::event::{Event, EventBuffer, EventKind, NoteRef};
use manguebyte_dsp::sources::SourceProcessor;
use manguebyte_dsp::{Compressor, KeySource, ProcessContext, ProcessSpec, Processor, Transport};
use manguebyte_graph::{Bus, Graph, GraphEngine, GraphError, NodeId, Route};

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

#[test]
fn sources_play_into_the_graph_as_nodes() {
    use manguebyte_core::source::BufferSource;
    use manguebyte_dsp::sources::SourceProcessor;

    let spec = ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 8,
        num_channels: 1,
    };
    let (mut graph, mut engine) = Graph::<f32>::new(spec);
    let mut first = AudioBuffer::<f32>::new(1, 6);
    first
        .channel_mut(0)
        .copy_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    let mut second = AudioBuffer::<f32>::new(1, 3);
    second.channel_mut(0).fill(10.0);
    let a = graph.add(SourceProcessor::new(BufferSource::new(first)));
    let b = graph.add(SourceProcessor::new(
        BufferSource::new(second).looping(true),
    ));
    let half = graph.add(Scale(0.5));
    graph.connect(a, half).unwrap();
    graph.connect(b, half).unwrap();
    graph.connect(half, Graph::<f32>::OUTPUT).unwrap();
    graph.commit().unwrap();

    let mut block = AudioBuffer::<f32>::new(1, 4);
    let mut heard = Vec::with_capacity(8);
    let calls = heap_calls_during(|| {
        for _ in 0..2 {
            engine.process(&mut block);
            heard.extend_from_slice(block.channel(0));
        }
    });
    assert_eq!(calls, 0);
    assert_eq!(heard, [5.5, 6.0, 6.5, 7.0, 7.5, 8.0, 5.0, 5.0]);
}

// --- Sidechains, events and feedback --------------------------------------

/// Block patterns that cut the same audio in very different places,
/// including blocks longer than the graph was prepared for (8 frames).
const PATTERNS: [&[usize]; 5] = [&[8], &[1], &[3, 5, 2, 7, 1], &[20], &[13, 1, 4]];

fn note(offset: u32, key: u8) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOn {
            note: NoteRef::new(0, key),
            velocity: 1.0,
        },
    }
}

fn key_of(kind: &EventKind) -> u8 {
    match kind {
        EventKind::NoteOn { note, .. } => note.key,
        other => panic!("unexpected event {other:?}"),
    }
}

/// Plays a buffer, as a node.
fn player(samples: &[f32]) -> SourceProcessor<BufferSource<f32>> {
    let mut buffer = AudioBuffer::<f32>::new(1, samples.len());
    buffer.channel_mut(0).copy_from_slice(samples);
    SourceProcessor::new(BufferSource::new(buffer))
}

/// Runs `input` through the engine in blocks of `pattern`, with the events
/// of `schedule` (frame, key) and an optional sidechain, and returns the
/// output and the output events with their frames on the whole timeline.
fn render(
    engine: &mut GraphEngine,
    input: &[f32],
    pattern: &[usize],
    schedule: &[(usize, u8)],
    sidechain: Option<&[f32]>,
) -> (Vec<f32>, Vec<(usize, u8)>) {
    let mut output = Vec::with_capacity(input.len());
    let mut sent = Vec::new();
    let mut events = EventBuffer::with_capacity(64);
    let mut done = 0;
    let mut block = 0;
    while done < input.len() {
        let length = pattern[block % pattern.len()].min(input.len() - done);
        block += 1;
        let mut buffer = AudioBuffer::<f32>::new(1, length);
        buffer
            .channel_mut(0)
            .copy_from_slice(&input[done..done + length]);
        events.clear();
        for &(frame, key) in schedule {
            if (done..done + length).contains(&frame) {
                events.push(note((frame - done) as u32, key)).unwrap();
            }
        }
        let mut side = AudioBuffer::<f32>::new(1, length);
        if let Some(sidechain) = sidechain {
            side.channel_mut(0)
                .copy_from_slice(&sidechain[done..done + length]);
        }
        let transport = Transport::STOPPED;
        let mut context = ProcessContext::new(&events, &transport);
        if sidechain.is_some() {
            context = context.with_sidechain(&side);
        }
        engine.process_with(&mut buffer, &context);
        output.extend_from_slice(buffer.channel(0));
        for event in engine.output_events() {
            sent.push((done + event.offset as usize, key_of(&event.kind)));
        }
        done += length;
    }
    (output, sent)
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Something to compress: a tone that is not the same on any two frames.
fn programme(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| 0.6 * (frame as f32 * 0.37).sin())
        .collect()
}

/// A key that comes and goes.
fn key_signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| if (frame / 40) % 2 == 0 { 0.9 } else { 0.0 })
        .collect()
}

fn ducker() -> Compressor<f32> {
    Compressor::<f32>::new()
        .with_key_source(KeySource::Sidechain)
        .with_threshold_db(-30.0)
        .with_ratio(10.0)
        .with_attack_ms(0.05)
        .with_release_ms(0.5)
}

/// The compressor keyed directly, with no graph.
fn ducked_alone(input: &[f32], key: &[f32], pattern: &[usize]) -> Vec<f32> {
    let mut compressor = ducker();
    compressor.prepare(&MONO);
    let mut output = Vec::with_capacity(input.len());
    let mut done = 0;
    let mut block = 0;
    while done < input.len() {
        let length = pattern[block % pattern.len()].min(input.len() - done);
        block += 1;
        let mut buffer = AudioBuffer::<f32>::new(1, length);
        buffer
            .channel_mut(0)
            .copy_from_slice(&input[done..done + length]);
        let mut side = AudioBuffer::<f32>::new(1, length);
        side.channel_mut(0)
            .copy_from_slice(&key[done..done + length]);
        let context = ProcessContext::empty().with_sidechain(&side);
        compressor.process_with(&mut buffer, &context);
        output.extend_from_slice(buffer.channel(0));
        done += length;
    }
    output
}

#[test]
fn a_compressor_keyed_by_another_node_matches_one_keyed_directly() {
    let frames = if cfg!(miri) { 64 } else { 1_000 };
    let input = programme(frames);
    let key = key_signal(frames);
    let mut first: Option<Vec<f32>> = None;
    for pattern in PATTERNS {
        let expected = ducked_alone(&input, &key, pattern);
        assert!(!same_bits(&expected, &input), "nothing was ducked");

        // Keyed by a node that plays the key.
        let (mut graph, mut engine) = self::graph();
        let source = graph.add(player(&key));
        let duck = graph.add(ducker());
        graph.connect(INPUT, duck).unwrap();
        graph.connect_sidechain(source, duck).unwrap();
        graph.connect(duck, OUTPUT).unwrap();
        graph.commit().unwrap();
        let (output, _) = render(&mut engine, &input, pattern, &[], None);
        assert!(
            same_bits(&output, &expected),
            "{pattern:?}: keyed by a node"
        );

        // Keyed by the sidechain the engine is given, routed explicitly.
        let (mut graph, mut engine) = self::graph();
        let duck = graph.add(ducker());
        graph.connect(INPUT, duck).unwrap();
        graph
            .connect_sidechain(Graph::<f32>::SIDECHAIN, duck)
            .unwrap();
        graph.connect(duck, OUTPUT).unwrap();
        graph.commit().unwrap();
        let (output, _) = render(&mut engine, &input, pattern, &[], Some(&key));
        assert!(
            same_bits(&output, &expected),
            "{pattern:?}: routed sidechain"
        );

        // A node with nothing routed to its sidechain still receives the
        // engine's own, as it did before sidechains could be routed.
        let (mut graph, mut engine) = self::graph();
        let duck = graph.add(ducker());
        graph.connect(INPUT, duck).unwrap();
        graph.connect(duck, OUTPUT).unwrap();
        graph.commit().unwrap();
        let (output, _) = render(&mut engine, &input, pattern, &[], Some(&key));
        assert!(same_bits(&output, &expected), "{pattern:?}: passed through");

        match &first {
            Some(first) => assert!(same_bits(&output, first), "{pattern:?}: depends on blocks"),
            None => first = Some(output),
        }
    }
}

/// What a probe has heard: each event's frame on the timeline and key.
type Heard = Arc<Mutex<Vec<Timed>>>;

/// A note on the timeline: its frame and its key.
type Timed = (usize, u8);

/// Outputs its main input plus a hundred times its sidechain, and writes
/// down every event it receives on the frame of the timeline it lands on.
struct Probe {
    elapsed: usize,
    heard: Heard,
}
impl Probe {
    fn new() -> (Self, Heard) {
        let heard = Arc::new(Mutex::new(Vec::with_capacity(4_096)));
        let probe = Self {
            elapsed: 0,
            heard: Arc::clone(&heard),
        };
        (probe, heard)
    }
}
impl Processor for Probe {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        self.process_with(buffer, &ProcessContext::empty());
    }
    fn process_with(&mut self, buffer: &mut AudioBuffer, context: &ProcessContext<'_>) {
        for frame in 0..buffer.num_frames() {
            let side = context.sidechain_peak(frame).unwrap_or(0.0) as f32;
            buffer.channel_mut(0)[frame] += 100.0 * side;
        }
        let mut heard = self.heard.lock().unwrap();
        for event in context.events {
            heard.push((self.elapsed + event.offset as usize, key_of(&event.kind)));
        }
        self.elapsed += buffer.num_frames();
    }
}

/// Plays a note every `period` frames from frame `phase` on, the key
/// counting up from `first_key`, and sends it out as an event.
struct Arpeggiator {
    period: usize,
    phase: usize,
    next_key: u8,
    elapsed: usize,
    played: Vec<Event>,
}
impl Arpeggiator {
    fn new(period: usize, phase: usize, first_key: u8) -> Self {
        Self {
            period,
            phase,
            next_key: first_key,
            elapsed: 0,
            played: Vec::with_capacity(64),
        }
    }
    fn collect(&mut self, out: &mut EventBuffer) {
        for event in self.played.drain(..) {
            out.push(event).unwrap();
        }
    }
}
impl Processor for Arpeggiator {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer) {
        for frame in 0..buffer.num_frames() {
            let time = self.elapsed + frame;
            if time >= self.phase && (time - self.phase) % self.period == 0 {
                self.played.push(note(frame as u32, self.next_key));
                self.next_key = self.next_key.wrapping_add(1) % 128;
            }
        }
        self.elapsed += buffer.num_frames();
    }
}

fn arpeggiator(graph: &mut Graph, period: usize, phase: usize, first_key: u8) -> NodeId {
    graph.add_with_events(
        Arpeggiator::new(period, phase, first_key),
        |arp: &mut Arpeggiator, out: &mut EventBuffer| arp.collect(out),
    )
}

#[test]
fn events_reach_downstream_nodes_on_their_frames_and_merge_in_a_fixed_order() {
    let frames = 60;
    let schedule = [(4, 100), (13, 101), (40, 102)];
    let mut first: Option<(Vec<Timed>, Vec<Timed>)> = None;
    for pattern in PATTERNS {
        let (mut graph, mut engine) = graph();
        // Added first, so on a shared frame its notes come first, although
        // it is connected last.
        let low = arpeggiator(&mut graph, 9, 4, 0);
        let high = arpeggiator(&mut graph, 13, 0, 50);
        let (probe, heard) = Probe::new();
        let probe = graph.add(probe);
        graph.connect(INPUT, probe).unwrap();
        graph.connect_events(high, probe).unwrap();
        graph.connect_events(INPUT, probe).unwrap();
        graph.connect_events(low, probe).unwrap();
        graph.connect_events(low, OUTPUT).unwrap();
        graph.connect(probe, OUTPUT).unwrap();
        graph.commit().unwrap();

        let input = vec![0.0; frames];
        let (_, sent) = render(&mut engine, &input, pattern, &schedule, None);
        let heard = heard.lock().unwrap().clone();

        // Every note exactly where it was played.
        let low_notes: Vec<(usize, u8)> = (0..)
            .map(|n: usize| (4 + 9 * n, n as u8))
            .take_while(|&(frame, _)| frame < frames)
            .collect();
        assert_eq!(sent, low_notes, "{pattern:?}: the graph's output");
        let mut expected: Vec<(usize, u8)> = Vec::new();
        for frame in 0..frames {
            // The engine's events first, then the nodes in the order they
            // were added.
            for &(at, key) in &schedule {
                if at == frame {
                    expected.push((at, key));
                }
            }
            if frame >= 4 && (frame - 4) % 9 == 0 {
                expected.push((frame, ((frame - 4) / 9) as u8));
            }
            if frame % 13 == 0 {
                expected.push((frame, 50 + (frame / 13) as u8));
            }
        }
        assert_eq!(heard, expected, "{pattern:?}: what the node received");
        // Frame 13 really is shared by three sources, and 4 and 40 by two.
        assert_eq!(expected.iter().filter(|e| e.0 == 13).count(), 3);
        assert_eq!(engine.lost_events(), 0);

        match &first {
            Some(first) => assert_eq!(&(heard, sent), first, "{pattern:?}"),
            None => first = Some((heard, sent)),
        }
    }
}

#[test]
fn a_node_without_event_connections_hears_the_engines_events_on_their_frames() {
    // A block longer than the graph was prepared for is run in pieces;
    // each event still lands on its own frame.
    let (mut graph, mut engine) = graph();
    let (probe, heard) = Probe::new();
    let probe = graph.add(probe);
    graph.connect(probe, OUTPUT).unwrap();
    graph.commit().unwrap();
    let schedule = [(3, 1), (12, 2), (19, 3)];
    render(&mut engine, &[0.0; 20], &[20], &schedule, None);
    assert_eq!(*heard.lock().unwrap(), schedule);
}

#[test]
fn paths_of_different_latency_line_up_at_main_sidechain_events_and_output() {
    let (mut graph, mut engine) = graph();
    let late = graph.add(Late::new(5));
    let (probe, heard) = Probe::new();
    let probe = graph.add(probe);
    let arp = arpeggiator(&mut graph, 1_000, 2, 7);
    // Main input through five frames of latency; sidechain and events
    // straight from the input and from an arpeggiator.
    graph.connect(INPUT, late).unwrap();
    graph.connect(late, probe).unwrap();
    graph.connect_sidechain(INPUT, probe).unwrap();
    graph.connect_events(INPUT, probe).unwrap();
    graph.connect_events(arp, probe).unwrap();
    graph.connect_events(arp, OUTPUT).unwrap();
    graph.connect(probe, OUTPUT).unwrap();
    // A path with no latency straight to the output as well.
    graph.connect(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();
    assert_eq!(graph.latency_samples(), 5);

    let mut impulse = vec![0.0f32; 16];
    impulse[0] = 1.0;
    let (output, sent) = render(&mut engine, &impulse, &[3, 4, 9], &[(1, 60)], None);
    // The impulse arrives at the node's main input and sidechain on the
    // same frame, and the dry path is held back to meet it: 1 + 100 + 1.
    let mut expected = vec![0.0f32; 16];
    expected[5] = 102.0;
    assert_eq!(output, expected);
    // Events are held back by the same five frames.
    assert_eq!(*heard.lock().unwrap(), [(6, 60), (7, 7)]);
    assert_eq!(sent, [(7, 7)]);
    assert_eq!(engine.lost_events(), 0);
}

#[test]
fn routes_pick_buses_and_channels() {
    let stereo = ProcessSpec {
        num_channels: 2,
        ..MONO
    };
    let (mut graph, mut engine) = Graph::<f32>::new(stereo);
    let left = graph.add(Constant(0.25));
    let probe = graph.add(Probe::new().0);
    // Only channel 0 of the constant, into channel 1 of the sidechain and
    // of the main input. The probe adds the loudest sidechain channel to
    // its channel 0.
    graph
        .connect_with(left, probe, Route::SIDECHAIN.channels(0, 1, 1))
        .unwrap();
    graph
        .connect_with(left, probe, Route::MAIN.channels(0, 1, 1))
        .unwrap();
    graph.connect(probe, OUTPUT).unwrap();
    graph.commit().unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 4);
    engine.process(&mut block);
    assert_eq!(block.channel(0), &[25.0; 4]);
    assert_eq!(block.channel(1), &[0.25; 4]);

    assert_eq!(Route::SIDECHAIN.bus(), Bus::Sidechain);
    assert_eq!(Route::default(), Route::MAIN);
    assert_eq!(Route::MAIN.to_bus(Bus::Sidechain), Route::SIDECHAIN);
    // Disconnecting takes away exactly the route that was made.
    graph.disconnect_sidechain(left, probe).unwrap();
    graph.commit().unwrap();
    engine.process(&mut block);
    assert_eq!(block.channel(0), &[25.0; 4]);
    graph
        .disconnect_with(left, probe, Route::SIDECHAIN.channels(0, 1, 1))
        .unwrap();
    graph.commit().unwrap();
    engine.process(&mut block);
    assert_eq!(block.channel(0), &[0.0; 4]);
    assert_eq!(block.channel(1), &[0.25; 4]);
}

/// The comb filter `y[n] = x[n] + gain * y[n - delay]`, computed one frame
/// at a time in the same arithmetic as the graph.
fn comb(input: &[f32], gain: f32, delay: usize) -> Vec<f32> {
    let mut output: Vec<f32> = Vec::with_capacity(input.len());
    for (frame, &x) in input.iter().enumerate() {
        let back = frame
            .checked_sub(delay)
            .map_or(0.0, |earlier| gain * output[earlier]);
        output.push(x + back);
    }
    output
}

/// `INPUT -> sum -> OUTPUT`, with `sum -> gain` fed back into `sum`.
fn comb_graph(delay: usize) -> (Graph, GraphEngine, NodeId) {
    let (mut graph, engine) = graph();
    let sum = graph.add(Scale(1.0));
    let gain = graph.add(Scale(0.5));
    graph.connect(INPUT, sum).unwrap();
    graph.connect(sum, gain).unwrap();
    graph.connect_feedback(gain, sum, delay).unwrap();
    graph.connect(sum, OUTPUT).unwrap();
    graph.commit().unwrap();
    (graph, engine, sum)
}

#[test]
fn a_feedback_loop_is_a_comb_filter_whatever_the_blocks() {
    let frames = if cfg!(miri) { 40 } else { 200 };
    let mut input = vec![0.0f32; frames];
    input[0] = 1.0;
    input[17] = -0.75;
    input[18] = 0.3;
    // Shorter than a block, a block, and longer.
    for delay in [1, 3, 8, 13] {
        let expected = comb(&input, 0.5, delay);
        assert!(expected[delay] == 0.5 && expected[2 * delay] == 0.25);
        for pattern in PATTERNS {
            let (_graph, mut engine, _) = comb_graph(delay);
            let (output, _) = render(&mut engine, &input, pattern, &[], None);
            assert!(
                same_bits(&output, &expected),
                "delay {delay}, blocks {pattern:?}: {output:?}"
            );
        }
    }

    // A node may feed itself: y[n] = 0.5 * (x[n] + y[n - 4]).
    let (mut graph, mut engine) = graph();
    let node = graph.add(Scale(0.5));
    graph.connect(INPUT, node).unwrap();
    graph.connect_feedback(node, node, 4).unwrap();
    graph.connect(node, OUTPUT).unwrap();
    graph.commit().unwrap();
    let (output, _) = render(&mut engine, &input[..12], &[5], &[], None);
    assert_eq!(
        output[..9],
        [0.5, 0.0, 0.0, 0.0, 0.25, 0.0, 0.0, 0.0, 0.125]
    );
    // Feedback is not latency: nothing was compensated.
    assert_eq!(graph.latency_samples(), 0);
}

#[test]
fn a_loop_without_a_delay_is_refused() {
    let (mut graph, _engine) = graph();
    let a = graph.add(Scale(1.0));
    let b = graph.add(Scale(1.0));
    let c = arpeggiator(&mut graph, 4, 0, 0);
    graph.connect(a, b).unwrap();

    assert_eq!(graph.connect(b, a), Err(GraphError::Cycle));
    assert_eq!(graph.connect_sidechain(b, a), Err(GraphError::Cycle));
    // Events count too: a -> b -> c -> a.
    graph.connect_events(b, c).unwrap();
    assert_eq!(graph.connect_events(c, a), Err(GraphError::Cycle));
    assert_eq!(graph.connect(c, a), Err(GraphError::Cycle));
    let message = GraphError::Cycle.to_string();
    assert!(
        message.contains("loop") && message.contains("delay"),
        "{message}"
    );

    // Through a delay it is allowed, also into a sidechain or from a node
    // to itself; a delay of zero is not a delay.
    assert_eq!(
        graph.connect_feedback(b, a, 0),
        Err(GraphError::InvalidConnection)
    );
    graph.connect_feedback(c, a, 4).unwrap();
    graph
        .connect_with(b, a, Route::SIDECHAIN.delayed(16))
        .unwrap();
    graph.connect_feedback(a, a, 1).unwrap();
    assert_eq!(graph.connect(a, a), Err(GraphError::InvalidConnection));
    graph.commit().unwrap();

    // Other routes that make no sense.
    assert_eq!(
        graph.connect_sidechain(a, OUTPUT),
        Err(GraphError::InvalidConnection)
    );
    assert_eq!(
        graph.connect_with(a, b, Route::MAIN.channels(0, 0, 0)),
        Err(GraphError::InvalidConnection)
    );
    assert_eq!(
        graph.connect(a, Graph::<f32>::SIDECHAIN),
        Err(GraphError::InvalidConnection)
    );
    assert_eq!(
        graph.connect_events(Graph::<f32>::SIDECHAIN, a),
        Err(GraphError::InvalidConnection)
    );
    assert_eq!(
        graph.connect_events(a, a),
        Err(GraphError::InvalidConnection)
    );

    // Once the event link is gone, the loop it closed is no loop.
    graph.disconnect_feedback(c, a, 4).unwrap();
    graph
        .disconnect_with(b, a, Route::SIDECHAIN.delayed(16))
        .unwrap();
    graph.disconnect_feedback(a, a, 1).unwrap();
    graph.disconnect_events(b, c).unwrap();
    graph.connect(c, a).unwrap();
    graph.commit().unwrap();
}

#[test]
fn processing_and_swapping_a_routed_graph_uses_no_heap() {
    let (mut graph, mut engine) = graph();
    let key = graph.add(Counter(0.0));
    let duck = graph.add(ducker());
    let late = graph.add(Late::new(3));
    let arp = arpeggiator(&mut graph, 3, 1, 0);
    let (probe, _heard) = Probe::new();
    let probe = graph.add(probe);
    let gain = graph.add(Scale(0.25));
    graph.connect(INPUT, late).unwrap();
    graph.connect(late, duck).unwrap();
    graph.connect_sidechain(key, duck).unwrap();
    graph.connect(duck, probe).unwrap();
    graph
        .connect_sidechain(Graph::<f32>::SIDECHAIN, probe)
        .unwrap();
    graph.connect_events(INPUT, probe).unwrap();
    graph.connect_events(arp, probe).unwrap();
    graph.connect_events(arp, OUTPUT).unwrap();
    graph.connect(probe, gain).unwrap();
    graph.connect_feedback(gain, probe, 5).unwrap();
    graph.connect(probe, OUTPUT).unwrap();
    graph.connect(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();

    let mut buffer = AudioBuffer::<f32>::new(1, 21);
    let mut side = AudioBuffer::<f32>::new(1, 21);
    side.channel_mut(0).fill(0.5);
    let mut events = EventBuffer::with_capacity(8);
    events.push(note(2, 1)).unwrap();
    events.push(note(17, 2)).unwrap();
    let transport = Transport::playing_at(120.0);
    let context = ProcessContext::new(&events, &transport).with_sidechain(&side);
    // Once outside the guard, so that the swap's garbage is not counted.
    engine.process_with(&mut buffer, &context);

    // Blocks of every kind: in pieces, whole, and one frame.
    let calls = heap_calls_during(|| {
        for length in [21, 8, 1, 5] {
            buffer.set_num_frames(length);
            buffer.channel_mut(0).fill(0.1);
            engine.process_with(&mut buffer, &context);
        }
    });
    assert_eq!(calls, 0, "processing used the heap");
    assert!(buffer.peak().is_finite() && buffer.peak() > 0.0);
    assert!(!engine.output_events().is_empty());

    // An edit: the new plan takes over the delays and the engine sends the
    // old one back, without touching the heap on this side.
    let extra = graph.add(Scale(2.0));
    graph.connect(gain, extra).unwrap();
    graph.connect_events(INPUT, OUTPUT).unwrap();
    graph.commit().unwrap();
    let calls = heap_calls_during(|| {
        buffer.set_num_frames(8);
        engine.process_with(&mut buffer, &context);
    });
    assert_eq!(calls, 0, "applying an edit used the heap");
    assert_eq!(engine.num_nodes(), 7);
    assert_eq!(engine.lost_events(), 0);
}

#[test]
fn an_edit_while_a_loop_plays_does_not_interrupt_it() {
    let frames = if cfg!(miri) { 48 } else { 240 };
    let mut input = vec![0.0f32; frames];
    input[0] = 1.0;
    input[30] = 0.5;
    // A comb, and beside it a path through latency that compensation
    // holds the comb's output back to meet.
    let build = || {
        let (mut graph, engine, sum) = comb_graph(11);
        let late = graph.add(Late::new(6));
        graph.connect(INPUT, late).unwrap();
        graph.connect(late, OUTPUT).unwrap();
        graph.commit().unwrap();
        (graph, engine, sum)
    };
    let (_graph, mut engine, _) = build();
    let (expected, _) = render(&mut engine, &input, &[7], &[], None);

    let (mut graph, mut engine, sum) = build();
    let half = frames / 2;
    let (mut output, _) = render(&mut engine, &input[..half], &[7], &[], None);
    // While it plays: a new node fed from the loop but going nowhere
    // audible, an event connection, a change to a node that keeps its
    // value, and the same graph committed twice over.
    let tap = graph.add(Scale(3.0));
    graph.connect(sum, tap).unwrap();
    graph.connect_events(INPUT, tap).unwrap();
    assert!(graph.update(sum, |node: &mut Scale| node.0 = 1.0));
    graph.commit().unwrap();
    graph.commit().unwrap();
    let (rest, _) = render(&mut engine, &input[half..], &[7], &[], None);
    output.extend(rest);
    assert!(same_bits(&output, &expected), "the edit was heard");
    assert!(expected[half..].iter().any(|&sample| sample != 0.0));
}

#[test]
fn every_piece_of_a_split_block_knows_where_it_is() {
    /// Writes down, for each call, where its frame 0 is on the timeline as
    /// the transport and the offset tell it.
    struct Clock(Arc<Mutex<Vec<i64>>>);
    impl Processor for Clock {
        fn prepare(&mut self, _: &ProcessSpec) {}
        fn process(&mut self, _: &mut AudioBuffer) {}
        fn process_with(&mut self, _: &mut AudioBuffer, context: &ProcessContext<'_>) {
            let start = context.transport.position_samples.unwrap() + context.offset as i64;
            self.0.lock().unwrap().push(start);
        }
    }

    let (mut graph, mut engine) = graph();
    let plain = Arc::new(Mutex::new(Vec::new()));
    let keyed = Arc::new(Mutex::new(Vec::new()));
    let a = graph.add(Clock(Arc::clone(&plain)));
    let b = graph.add(Clock(Arc::clone(&keyed)));
    graph.connect(a, OUTPUT).unwrap();
    graph.connect_sidechain(a, b).unwrap();
    graph.connect(b, OUTPUT).unwrap();
    graph.commit().unwrap();

    // 20 frames, prepared for 8: pieces at 0, 8 and 16 of a block that
    // itself starts 3 frames into what the caller was given.
    let events = EventBuffer::new();
    let mut transport = Transport::playing_at(120.0);
    transport.position_samples = Some(1_000);
    let mut context = ProcessContext::new(&events, &transport);
    context.offset = 3;
    engine.process_with(&mut AudioBuffer::new(1, 20), &context);
    // A node that reads the caller's sidechain is told the offset; one
    // with a sidechain of its own gets a transport moved on to the piece.
    assert_eq!(*plain.lock().unwrap(), [1_003, 1_011, 1_019]);
    assert_eq!(*keyed.lock().unwrap(), [1_003, 1_011, 1_019]);
}
