//! A dynamic audio processing graph.
//!
//! A fixed chain of processors is a tuple, resolved by the compiler. A graph
//! is for everything that cannot be known in advance: a mixer whose channels
//! come and go, a modular patch, a plugin host, a live-coding engine. Nodes
//! are added, connected and removed **while audio is running**, and the
//! audio thread never allocates, locks or frees to make that happen.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::{Gain, Oscillator, ProcessSpec, Processor, Waveform};
//! use manguebyte_graph::Graph;
//!
//! let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 };
//! let (mut graph, mut engine) = Graph::<f32>::new(spec);
//!
//! // Two oscillators into one gain, into the output.
//! let low = graph.add(Oscillator::new(Waveform::Sine, 110.0));
//! let high = graph.add(Oscillator::new(Waveform::Sine, 440.0));
//! let level = graph.add(Gain::<f32>::new(0.25));
//! graph.connect(low, level)?;
//! graph.connect(high, level)?;
//! graph.connect(level, Graph::<f32>::OUTPUT)?;
//! graph.commit()?;
//!
//! // On the audio thread:
//! let mut block = AudioBuffer::<f32>::new(2, 256);
//! engine.process(&mut block);
//! assert!(block.peak() > 0.0);
//!
//! // Later, while it plays: turn the level down and drop one oscillator.
//! graph.update(level, |gain: &mut Gain<f32>| gain.set_gain(0.1));
//! graph.remove(high)?;
//! graph.commit()?;
//! # Ok::<(), manguebyte_graph::GraphError>(())
//! ```
//!
//! # How editing stays off the audio thread
//!
//! The graph has two halves. [`Graph`] is the description: it lives on
//! whichever thread edits it and may allocate freely. [`GraphEngine`] is a
//! [`Processor`] and lives on the audio thread.
//!
//! [`Graph::commit`] compiles the description into a plan: the order to run
//! the nodes in, a buffer for each, and delay lines where paths need
//! aligning. The plan travels to the engine through a wait-free queue. At the
//! start of its next block the engine moves the processors it already has
//! into the new plan, which is a pointer move each, and sends the old plan
//! back through a second queue. The old plan, and any removed node inside
//! it, is freed on the editing thread the next time the graph is touched.
//!
//! So a node keeps its state across any number of edits: a reverb's tail
//! and an oscillator's phase survive rewiring around them. So does whatever
//! is travelling through a delay that the old and the new plan share: a
//! feedback loop, a compensated path or events held back keep going through
//! an edit elsewhere in the graph.
//!
//! # Sidechains
//!
//! Every node has two audio inputs, as a plugin has buses: the main one,
//! which it processes, and a sidechain ([`Bus`]), which it receives as
//! [`ProcessContext::sidechain`](manguebyte_dsp::ProcessContext::sidechain).
//! [`Graph::connect_sidechain`] feeds one, and [`Graph::connect_with`]
//! takes a [`Route`] that names the bus and the channels:
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::{Compressor, KeySource, Oscillator, ProcessSpec, Processor, Waveform};
//! use manguebyte_graph::{Graph, Route};
//!
//! let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 2 };
//! let (mut graph, mut engine) = Graph::<f32>::new(spec);
//! let bass = graph.add(Oscillator::new(Waveform::Saw, 55.0));
//! let kick = graph.add(Oscillator::new(Waveform::Sine, 2.0));
//! let duck = graph.add(Compressor::<f32>::new().with_key_source(KeySource::Sidechain));
//! graph.connect(bass, duck)?;
//! // Only the kick's left channel keys the compressor.
//! graph.connect_with(kick, duck, Route::SIDECHAIN.channels(0, 0, 1))?;
//! graph.connect(duck, Graph::<f32>::OUTPUT)?;
//! graph.commit()?;
//!
//! let mut block = AudioBuffer::<f32>::new(2, 64);
//! engine.process(&mut block);
//! # Ok::<(), manguebyte_graph::GraphError>(())
//! ```
//!
//! A node with nothing connected to its sidechain receives the sidechain
//! the engine itself was given, as before sidechains could be routed;
//! [`Graph::SIDECHAIN`] is that sidechain as a source, to route it
//! explicitly.
//!
//! # Events
//!
//! Notes, parameter changes and MIDI travel as
//! [`Event`](manguebyte_dsp::event::Event)s, each on its frame.
//! [`Graph::connect_events`] sends the events of one node to another; a
//! node made with [`Graph::add_with_events`] can send them, as an
//! arpeggiator or a hosted plugin with a note output does. The events the
//! engine is given come from [`Graph::INPUT`], and the ones connected to
//! [`Graph::OUTPUT`] are left in [`GraphEngine::output_events`].
//!
//! - A node with no event connection receives the events the engine was
//!   given, as every node did before events could be routed. Once anything
//!   is connected to it, it receives exactly what is connected.
//! - Events reach a node on the frame they were sent on, held back by the
//!   same amount as audio when latency compensation delays a path, so
//!   they stay with the audio they belong to.
//! - Events from several connections on the same frame arrive in a fixed
//!   order: the engine's own first, then each node's in the order the
//!   nodes were added; each source's own events keep their order. It does
//!   not depend on the order the connections were made in, nor on the
//!   size of the blocks.
//!
//! Each event input holds [`EVENT_CAPACITY`] events a block; what does not
//! fit is dropped and counted by [`GraphEngine::lost_events`].
//!
//! # Feedback
//!
//! A loop is allowed only through a delayed connection
//! ([`Graph::connect_feedback`], or a [`Route`] with a delay): the plan
//! stays a running order in which every other connection goes forwards,
//! and a delayed connection reads what its source produced at least that
//! many frames earlier. Any other loop is refused with
//! [`GraphError::Cycle`].
//!
//! The delay is a number of frames, not of blocks, so that a loop sounds
//! the same whatever size the host's blocks are: a "one block" delay would
//! change with the block size, and so would the sound. A delay as long as
//! the largest block costs nothing; a shorter one, down to one frame, makes
//! the engine run the graph in pieces no longer than that delay, which
//! costs more calls but gives the same output. See
//! [`Route`](Route#delayed-connections-and-feedback).
//!
//! # Latency compensation
//!
//! A node may report that its output is late, for example because of
//! look-ahead ([`Processor::latency_samples`]). When paths of different
//! latency meet, the graph delays the earlier ones so they line up again,
//! whether they meet at a main input, a sidechain, an event input or the
//! graph's output, and reports the total through
//! [`GraphEngine::latency_samples`]. Feedback connections are left as they
//! are: their delay is exactly the one they were given.

#![forbid(unsafe_code)]

mod engine;
mod plan;
mod route;

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

use manguebyte_core::spsc::{self, Consumer, Producer};
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::event::EventBuffer;
use manguebyte_dsp::{ProcessSpec, Processor};

pub use engine::GraphEngine;
use plan::{
    Command, Emitter, EventBus, EventInput, EventSource, Garbage, Input, Plan, SidechainBus, Slot,
    Source, Update, WireKey,
};
pub use route::{Bus, Route};

/// How many edits can wait for the audio thread at once.
const QUEUE_CAPACITY: usize = 256;

/// How many events one event input of a node, a node's own output, or the
/// graph's output holds in one block. Room for them is reserved when the
/// graph is committed.
pub const EVENT_CAPACITY: usize = 1_024;

/// Identifies a node in a [`Graph`]. Identifiers are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

/// A processor that can be a node: any [`Processor`] that may be sent to the
/// audio thread. Implemented automatically.
pub trait Node<S: Sample>: Processor<S> + Send + 'static {
    /// The node as [`Any`], so that [`Graph::update`] can reach its real
    /// type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<S: Sample, T: Processor<S> + Send + 'static> Node<S> for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Why an edit to a [`Graph`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// No node has this identifier, or it has been removed.
    UnknownNode(NodeId),
    /// The connection makes no sense: into the graph's input, out of its
    /// output, from a node to itself without a delay, into the output's
    /// sidechain, events out of the graph's sidechain, or no channels.
    InvalidConnection,
    /// The connections form a loop with no delay in it. Feedback needs an
    /// explicit delay: [`Graph::connect_feedback`].
    Cycle,
    /// Too many edits are waiting for the audio thread, which is not
    /// running or cannot keep up. Nothing was changed; try again.
    Busy,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "the graph has no node {}", id.0),
            Self::InvalidConnection => f.write_str("that connection is not possible"),
            Self::Cycle => f.write_str(
                "the connections form a loop with no delay in it; \
                 feedback has to go through a delayed connection",
            ),
            Self::Busy => f.write_str("too many edits are waiting for the audio thread"),
        }
    }
}

impl std::error::Error for GraphError {}

/// What the description knows about one node.
struct NodeEntry<S: Sample> {
    id: NodeId,
    /// Latency the node reported when it was added.
    latency: usize,
    /// Whether the node sends events.
    emits: bool,
    /// The processor itself, until the first commit hands it to the engine.
    waiting: Option<Box<dyn Node<S>>>,
    /// What collects its events, travelling with it.
    emitter: Option<Emitter>,
}

/// One audio connection of the description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wire {
    from: NodeId,
    to: NodeId,
    route: Route,
}

/// The editable description of a processing graph.
///
/// See the [crate documentation](crate) for how it works with
/// [`GraphEngine`].
pub struct Graph<S: Sample = f32> {
    spec: Arc<Mutex<ProcessSpec>>,
    nodes: Vec<NodeEntry<S>>,
    wires: Vec<Wire>,
    event_wires: Vec<(NodeId, NodeId)>,
    next_id: u32,
    commands: Producer<Command<S>>,
    garbage: Consumer<Garbage<S>>,
    latency: usize,
}

impl<S: Sample> Graph<S> {
    /// The graph's own input: what the engine is given to process, and the
    /// events it is given with it.
    pub const INPUT: NodeId = NodeId(0);
    /// The graph's own output: what the engine leaves in the buffer, and
    /// the events it leaves in [`GraphEngine::output_events`].
    pub const OUTPUT: NodeId = NodeId(1);
    /// The sidechain the engine is given
    /// ([`ProcessContext::sidechain`](manguebyte_dsp::ProcessContext::sidechain)),
    /// as a source of audio. Silence when it is given none.
    pub const SIDECHAIN: NodeId = NodeId(2);

    /// Creates an empty graph and the engine that runs it.
    ///
    /// The engine is ready to process under `spec` at once. An empty graph
    /// outputs silence.
    pub fn new(spec: ProcessSpec) -> (Self, GraphEngine<S>) {
        let (commands, command_receiver) = spsc::channel(QUEUE_CAPACITY);
        let (garbage_sender, garbage) = spsc::channel(QUEUE_CAPACITY);
        let spec = Arc::new(Mutex::new(spec));
        let graph = Self {
            spec: Arc::clone(&spec),
            nodes: Vec::new(),
            wires: Vec::new(),
            event_wires: Vec::new(),
            next_id: 3,
            commands,
            garbage,
            latency: 0,
        };
        let engine = GraphEngine::new(spec, command_receiver, garbage_sender);
        (graph, engine)
    }

    fn spec(&self) -> ProcessSpec {
        *self
            .spec
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Adds a node and returns its identifier.
    ///
    /// The node is prepared here, on the calling thread, so it is ready to
    /// run the moment the engine receives it. It does nothing until it is
    /// connected and the graph is [committed](Self::commit).
    pub fn add(&mut self, node: impl Node<S>) -> NodeId {
        self.insert(Box::new(node), None)
    }

    /// Adds a node that sends events, and returns its identifier.
    ///
    /// After each block the node processes, `collect` is called on the
    /// audio thread with the node and an empty [`EventBuffer`], and puts
    /// into it what the node sent during that block, each event on its
    /// frame of the block. Connect the node with
    /// [`connect_events`](Self::connect_events) to deliver them.
    /// `collect` must not allocate, lock or block; events beyond the
    /// buffer's capacity ([`EVENT_CAPACITY`]) are refused by it.
    ///
    /// A hosted plugin keeps what it sent in its processor, so this is
    /// where it is read, for example with
    /// `|plugin: &mut ClapProcessor, out| ...` over its `output_events()`.
    ///
    /// ```
    /// use manguebyte_core::AudioBuffer;
    /// use manguebyte_dsp::event::{Event, EventBuffer, EventKind, NoteRef};
    /// use manguebyte_dsp::{ProcessSpec, Processor};
    /// use manguebyte_graph::Graph;
    ///
    /// /// Plays middle C on the first frame of every block it processes.
    /// struct Pulse(bool);
    /// impl Processor for Pulse {
    ///     fn prepare(&mut self, _: &ProcessSpec) {}
    ///     fn process(&mut self, _: &mut AudioBuffer) {
    ///         self.0 = true;
    ///     }
    /// }
    ///
    /// let spec = ProcessSpec { sample_rate: 48_000.0, max_block_size: 64, num_channels: 1 };
    /// let (mut graph, mut engine) = Graph::<f32>::new(spec);
    /// let pulse = graph.add_with_events(Pulse(false), |pulse: &mut Pulse, out: &mut EventBuffer| {
    ///     if std::mem::take(&mut pulse.0) {
    ///         let note = NoteRef::new(0, 60);
    ///         let _ = out.push(Event { offset: 0, kind: EventKind::NoteOn { note, velocity: 1.0 } });
    ///     }
    /// });
    /// graph.connect_events(pulse, Graph::<f32>::OUTPUT)?;
    /// graph.commit()?;
    ///
    /// engine.process(&mut AudioBuffer::new(1, 64));
    /// assert_eq!(engine.output_events().len(), 1);
    /// # Ok::<(), manguebyte_graph::GraphError>(())
    /// ```
    pub fn add_with_events<T: Node<S>>(
        &mut self,
        node: T,
        mut collect: impl FnMut(&mut T, &mut EventBuffer) + Send + 'static,
    ) -> NodeId {
        let emitter: Emitter = Box::new(move |node: &mut dyn Any, out: &mut EventBuffer| {
            if let Some(node) = node.downcast_mut::<T>() {
                collect(node, out);
            }
        });
        self.insert(Box::new(node), Some(emitter))
    }

    fn insert(&mut self, mut node: Box<dyn Node<S>>, emitter: Option<Emitter>) -> NodeId {
        self.collect_garbage();
        node.prepare(&self.spec());
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.push(NodeEntry {
            id,
            latency: node.latency_samples(),
            emits: emitter.is_some(),
            waiting: Some(node),
            emitter,
        });
        id
    }

    /// Removes a node and every connection to or from it.
    ///
    /// The node keeps running until the next [`commit`](Self::commit). After
    /// that the engine hands it back, and it is dropped on the thread that
    /// next touches the graph.
    pub fn remove(&mut self, id: NodeId) -> Result<(), GraphError> {
        self.collect_garbage();
        let index = self
            .nodes
            .iter()
            .position(|node| node.id == id)
            .ok_or(GraphError::UnknownNode(id))?;
        self.nodes.remove(index);
        self.wires.retain(|wire| wire.from != id && wire.to != id);
        self.event_wires
            .retain(|&(from, to)| from != id && to != id);
        Ok(())
    }

    /// Feeds the output of `from` into the main input of `to`.
    ///
    /// A node with several inputs receives their sum. Connecting the same
    /// pair twice has no further effect. Use [`INPUT`](Self::INPUT) and
    /// [`OUTPUT`](Self::OUTPUT) for the ends of the graph. The same as
    /// [`connect_with`](Self::connect_with) and [`Route::MAIN`].
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.connect_with(from, to, Route::MAIN)
    }

    /// Feeds the output of `from` into the sidechain of `to`, every channel
    /// into the same channel. The same as [`connect_with`](Self::connect_with)
    /// and [`Route::SIDECHAIN`].
    pub fn connect_sidechain(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.connect_with(from, to, Route::SIDECHAIN)
    }

    /// Feeds the output of `from` into the main input of `to`, `delay`
    /// frames later. Unlike any other connection it may close a loop,
    /// including one from a node to itself. A `delay` of zero is refused
    /// with [`GraphError::InvalidConnection`].
    ///
    /// The same as [`connect_with`](Self::connect_with) and
    /// [`Route::MAIN`]`.delayed(delay)`; see
    /// [`Route`](Route#delayed-connections-and-feedback) for how the delay
    /// keeps the output independent of the block size.
    pub fn connect_feedback(
        &mut self,
        from: NodeId,
        to: NodeId,
        delay: usize,
    ) -> Result<(), GraphError> {
        if delay == 0 {
            return Err(GraphError::InvalidConnection);
        }
        self.connect_with(from, to, Route::MAIN.delayed(delay))
    }

    /// Connects the output of `from` to `to` along `route`: into its main
    /// input or its sidechain, a range of channels, delayed or not.
    ///
    /// Making the same connection twice has no further effect; two routes
    /// between the same nodes that differ in anything are two connections.
    /// A connection without a delay that would close a loop is refused with
    /// [`GraphError::Cycle`], and nothing changes.
    pub fn connect_with(
        &mut self,
        from: NodeId,
        to: NodeId,
        route: Route,
    ) -> Result<(), GraphError> {
        self.check_audio(from, to, route)?;
        let wire = Wire { from, to, route };
        if !self.wires.contains(&wire) {
            self.wires.push(wire);
            if route.delay() == 0 && self.order().is_err() {
                self.wires.pop();
                return Err(GraphError::Cycle);
            }
        }
        Ok(())
    }

    /// Removes the connection [`connect`](Self::connect) makes from `from`
    /// to `to`, if there is one.
    pub fn disconnect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.disconnect_with(from, to, Route::MAIN)
    }

    /// Removes the connection [`connect_sidechain`](Self::connect_sidechain)
    /// makes from `from` to `to`, if there is one.
    pub fn disconnect_sidechain(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.disconnect_with(from, to, Route::SIDECHAIN)
    }

    /// Removes the connection
    /// [`connect_feedback`](Self::connect_feedback) makes from `from` to
    /// `to` with this delay, if there is one.
    pub fn disconnect_feedback(
        &mut self,
        from: NodeId,
        to: NodeId,
        delay: usize,
    ) -> Result<(), GraphError> {
        if delay == 0 {
            return Err(GraphError::InvalidConnection);
        }
        self.disconnect_with(from, to, Route::MAIN.delayed(delay))
    }

    /// Removes the connection from `from` to `to` along exactly `route`, if
    /// there is one.
    pub fn disconnect_with(
        &mut self,
        from: NodeId,
        to: NodeId,
        route: Route,
    ) -> Result<(), GraphError> {
        self.check_audio(from, to, route)?;
        self.wires.retain(|wire| *wire != Wire { from, to, route });
        Ok(())
    }

    /// Sends the events of `from` to `to`.
    ///
    /// `from` is a node made with [`add_with_events`](Self::add_with_events)
    /// (any other sends nothing) or [`INPUT`](Self::INPUT), for the events
    /// the engine is given. `to` is a node or [`OUTPUT`](Self::OUTPUT). A
    /// node with any event connection receives exactly what is connected to
    /// it, instead of the engine's events; see the
    /// [crate documentation](crate#events) for timing and order.
    ///
    /// Event connections take part in the running order like audio ones,
    /// so one that would close a loop is refused with
    /// [`GraphError::Cycle`].
    pub fn connect_events(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check_events(from, to)?;
        if !self.event_wires.contains(&(from, to)) {
            self.event_wires.push((from, to));
            if self.order().is_err() {
                self.event_wires.pop();
                return Err(GraphError::Cycle);
            }
        }
        Ok(())
    }

    /// Stops sending the events of `from` to `to`, if they were sent.
    pub fn disconnect_events(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check_events(from, to)?;
        self.event_wires.retain(|&pair| pair != (from, to));
        Ok(())
    }

    fn is_end(id: NodeId) -> bool {
        id == Self::INPUT || id == Self::OUTPUT || id == Self::SIDECHAIN
    }

    fn check_known(&self, id: NodeId) -> Result<(), GraphError> {
        if Self::is_end(id) || self.contains(id) {
            Ok(())
        } else {
            Err(GraphError::UnknownNode(id))
        }
    }

    fn check_audio(&self, from: NodeId, to: NodeId, route: Route) -> Result<(), GraphError> {
        let into_output_sidechain = to == Self::OUTPUT && route.bus() == Bus::Sidechain;
        if from == Self::OUTPUT
            || to == Self::INPUT
            || to == Self::SIDECHAIN
            || (from == to && route.delay() == 0)
            || into_output_sidechain
            || route.channel_count() == 0
        {
            return Err(GraphError::InvalidConnection);
        }
        self.check_known(from)?;
        self.check_known(to)
    }

    fn check_events(&self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        if from == Self::OUTPUT
            || from == Self::SIDECHAIN
            || to == Self::INPUT
            || to == Self::SIDECHAIN
            || from == to
        {
            return Err(GraphError::InvalidConnection);
        }
        self.check_known(from)?;
        self.check_known(to)
    }

    /// Whether the graph has a node with this identifier.
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.iter().any(|node| node.id == id)
    }

    /// How many nodes the graph has, not counting its input and output.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Tells the graph that a node's latency has changed since it was added,
    /// so the next [`commit`](Self::commit) compensates for the new value.
    pub fn set_latency(&mut self, id: NodeId, samples: usize) -> Result<(), GraphError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.id == id)
            .ok_or(GraphError::UnknownNode(id))?;
        node.latency = samples;
        Ok(())
    }

    /// The latency of the whole graph as last committed, in frames.
    pub fn latency_samples(&self) -> usize {
        self.latency
    }

    /// Changes a node, wherever it currently is.
    ///
    /// `change` receives the node as the type it was added as. If the node
    /// is already running, the change is carried to the audio thread and
    /// applied there at the start of the next block, so it must not
    /// allocate, lock or block. If `T` is not the node's type, nothing
    /// happens.
    ///
    /// Returns `false` if the node does not exist or too many edits are
    /// already waiting for the audio thread.
    pub fn update<T: 'static>(
        &mut self,
        id: NodeId,
        change: impl FnOnce(&mut T) + Send + 'static,
    ) -> bool {
        self.collect_garbage();
        let Some(entry) = self.nodes.iter_mut().find(|node| node.id == id) else {
            return false;
        };
        // `as_deref_mut` matters: a `Box<dyn Node>` is itself a `Node`, so
        // asking the box for `Any` would describe the box, not what is in it.
        if let Some(node) = entry.waiting.as_deref_mut() {
            if let Some(node) = node.as_any_mut().downcast_mut::<T>() {
                change(node);
            }
            return true;
        }
        let mut change = Some(change);
        let apply: Update = Box::new(move |node: &mut dyn Any| {
            if let (Some(node), Some(change)) = (node.downcast_mut::<T>(), change.take()) {
                change(node);
            }
        });
        self.commands
            .push(Command::Update { node: id, apply })
            .is_ok()
    }

    /// Makes the engine run the graph as it is now described.
    ///
    /// Until this is called, edits change only the description. The engine
    /// switches to the new arrangement at the start of its next block, all
    /// at once.
    pub fn commit(&mut self) -> Result<(), GraphError> {
        self.collect_garbage();
        let (plan, latency) = self.compile()?;
        match self.commands.push(Command::Swap(plan)) {
            Ok(()) => {
                self.latency = latency;
                Ok(())
            }
            Err(Command::Swap(mut plan)) => {
                // The engine never saw the plan. Take the new nodes back so
                // a later commit can send them.
                for slot in &mut plan.slots {
                    if let Some(entry) = self.nodes.iter_mut().find(|n| n.id == slot.id) {
                        if let Some(node) = slot.node.take() {
                            entry.waiting = Some(node);
                        }
                        if let Some(emitter) = slot.emitter.take() {
                            entry.emitter = Some(emitter);
                        }
                    }
                }
                Err(GraphError::Busy)
            }
            Err(_) => Err(GraphError::Busy),
        }
    }

    /// Frees what the engine has finished with: plans it has replaced, and
    /// the removed nodes inside them. Every editing method does this, so
    /// calling it by hand is only needed to release memory promptly.
    pub fn collect_garbage(&mut self) {
        while self.garbage.pop().is_some() {}
    }

    /// Every connection that makes `to` wait for `from`: audio ones without
    /// a delay, and event ones, between two nodes.
    fn dependencies(&self) -> Vec<(NodeId, NodeId)> {
        let audio = self
            .wires
            .iter()
            .filter(|wire| wire.route.delay() == 0)
            .map(|wire| (wire.from, wire.to));
        audio
            .chain(self.event_wires.iter().copied())
            .filter(|&(from, to)| !Self::is_end(from) && !Self::is_end(to))
            .collect()
    }

    /// The nodes in an order in which each one comes after all of its
    /// sources, or an error if the connections form a loop.
    fn order(&self) -> Result<Vec<NodeId>, GraphError> {
        let dependencies = self.dependencies();
        let mut waiting_for: Vec<(NodeId, usize)> = self
            .nodes
            .iter()
            .map(|node| {
                let sources = dependencies
                    .iter()
                    .filter(|&&(_, to)| to == node.id)
                    .count();
                (node.id, sources)
            })
            .collect();

        let mut order = Vec::with_capacity(self.nodes.len());
        while order.len() < self.nodes.len() {
            // Taking the first ready node each time keeps the order stable:
            // nodes that do not depend on each other run in the order they
            // were added.
            let ready = waiting_for
                .iter()
                .position(|&(id, sources)| sources == 0 && !order.contains(&id))
                .ok_or(GraphError::Cycle)?;
            let id = waiting_for[ready].0;
            order.push(id);
            for &(from, to) in &dependencies {
                if from == id {
                    if let Some(entry) = waiting_for.iter_mut().find(|(node, _)| *node == to) {
                        entry.1 -= 1;
                    }
                }
            }
        }
        Ok(order)
    }

    /// Turns the description into a plan the engine can run, and works out
    /// the graph's total latency.
    fn compile(&mut self) -> Result<(Box<Plan<S>>, usize), GraphError> {
        let spec = self.spec();
        let channels = spec.num_channels;
        let order = self.order()?;
        let position = |id: NodeId| order.iter().position(|&other| other == id);
        let audio_source = |id: NodeId| match id {
            id if id == Self::INPUT => Some(Source::GraphInput),
            id if id == Self::SIDECHAIN => Some(Source::GraphSidechain),
            id => position(id).map(Source::Node),
        };
        let event_source = |id: NodeId| match id {
            id if id == Self::INPUT => Some(EventSource::GraphInput),
            id => position(id).map(EventSource::Node),
        };
        // How late each node's output is, counting everything before it.
        let mut lateness = vec![0usize; order.len()];
        let late = |source: Source, lateness: &[usize]| match source {
            Source::Node(at) => lateness[at],
            Source::GraphInput | Source::GraphSidechain => 0,
        };
        let late_events = |source: EventSource, lateness: &[usize]| match source {
            EventSource::Node(at) => lateness[at],
            EventSource::GraphInput => 0,
        };

        // What feeds `to`: its audio wires with their sources, and its
        // event sources in the order their events are merged in.
        let wires_into = |to: NodeId| -> Vec<(Wire, Source)> {
            self.wires
                .iter()
                .filter(|wire| wire.to == to)
                .filter_map(|&wire| audio_source(wire.from).map(|source| (wire, source)))
                .collect()
        };
        let events_into = |to: NodeId| -> Vec<(NodeId, EventSource)> {
            let mut sources: Vec<(NodeId, EventSource)> = self
                .event_wires
                .iter()
                .filter(|&&(_, target)| target == to)
                .filter_map(|&(from, _)| event_source(from).map(|source| (from, source)))
                .collect();
            sources.sort_by_key(|&(from, _)| from);
            sources
        };
        // The latest of the sources that are not delayed by a loop.
        let latest_of =
            |wires: &[(Wire, Source)], events: &[(NodeId, EventSource)], lateness: &[usize]| {
                let audio = wires
                    .iter()
                    .filter(|(wire, _)| wire.route.delay() == 0)
                    .map(|&(_, source)| late(source, lateness));
                let events = events
                    .iter()
                    .map(|&(_, source)| late_events(source, lateness));
                audio.chain(events).max().unwrap_or(0)
            };
        let inputs_on = |wires: &[(Wire, Source)], bus: Bus, latest: usize, lateness: &[usize]| {
            wires
                .iter()
                .filter(|(wire, _)| wire.route.bus() == bus)
                .map(|&(wire, source)| {
                    let key = WireKey {
                        from: wire.from,
                        to: wire.to,
                        route: wire.route,
                    };
                    let align = latest - late(source, lateness);
                    Input::new(key, source, align, channels)
                })
                .collect::<Vec<_>>()
        };
        let event_inputs =
            |to: NodeId, events: &[(NodeId, EventSource)], latest, lateness: &[usize]| {
                events
                    .iter()
                    .map(|&(from, source)| {
                        EventInput::new(from, to, source, latest - late_events(source, lateness))
                    })
                    .collect::<Vec<_>>()
            };

        let mut slots = Vec::with_capacity(order.len());
        for (index, &id) in order.iter().enumerate() {
            let wires = wires_into(id);
            let events = events_into(id);
            let latest = latest_of(&wires, &events, &lateness);
            let entry = self
                .nodes
                .iter_mut()
                .find(|node| node.id == id)
                .expect("the order only contains existing nodes");
            lateness[index] = latest + entry.latency;

            let sidechain = inputs_on(&wires, Bus::Sidechain, latest, &lateness);
            slots.push(Slot {
                id,
                node: entry.waiting.take(),
                emitter: entry.emitter.take(),
                inputs: inputs_on(&wires, Bus::Main, latest, &lateness),
                sidechain: (!sidechain.is_empty()).then(|| SidechainBus {
                    inputs: sidechain,
                    buffer: AudioBuffer::new(channels, spec.max_block_size),
                }),
                events: (!events.is_empty()).then(|| EventBus {
                    inputs: event_inputs(id, &events, latest, &lateness),
                    buffer: EventBuffer::with_capacity(EVENT_CAPACITY),
                }),
                emitted: if entry.emits {
                    EventBuffer::with_capacity(EVENT_CAPACITY)
                } else {
                    EventBuffer::new()
                },
            });
        }

        let wires = wires_into(Self::OUTPUT);
        let events = events_into(Self::OUTPUT);
        let latency = latest_of(&wires, &events, &lateness);
        let mut plan = Box::new(Plan {
            buffers: (0..slots.len())
                .map(|_| AudioBuffer::new(channels, spec.max_block_size))
                .collect(),
            slots,
            outputs: inputs_on(&wires, Bus::Main, latency, &lateness),
            output_events: event_inputs(Self::OUTPUT, &events, latency, &lateness),
            mix: AudioBuffer::new(channels, spec.max_block_size),
            latency,
            piece_limit: usize::MAX,
            sample_rate: spec.sample_rate,
        });
        plan.find_piece_limit();
        Ok((plan, latency))
    }
}
