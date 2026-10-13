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
//! and an oscillator's phase survive rewiring around them.
//!
//! # Latency compensation
//!
//! A node may report that its output is late, for example because of
//! look-ahead ([`Processor::latency_samples`]). When paths of different
//! latency meet, the graph delays the earlier ones so they line up again,
//! and reports the total through [`GraphEngine::latency_samples`].

mod engine;
mod plan;

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

use manguebyte_core::spsc::{self, Consumer, Producer};
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

pub use engine::GraphEngine;
use plan::{Command, Garbage, Input, Plan, Slot, Source, Update};

/// How many edits can wait for the audio thread at once.
const QUEUE_CAPACITY: usize = 256;

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
    /// output, or from a node to itself.
    InvalidConnection,
    /// The connections form a loop. Feedback needs an explicit delay.
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
            Self::Cycle => f.write_str("the connections form a loop"),
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
    /// The processor itself, until the first commit hands it to the engine.
    waiting: Option<Box<dyn Node<S>>>,
}

/// The editable description of a processing graph.
///
/// See the [crate documentation](crate) for how it works with
/// [`GraphEngine`].
pub struct Graph<S: Sample = f32> {
    spec: Arc<Mutex<ProcessSpec>>,
    nodes: Vec<NodeEntry<S>>,
    connections: Vec<(NodeId, NodeId)>,
    next_id: u32,
    commands: Producer<Command<S>>,
    garbage: Consumer<Garbage<S>>,
    latency: usize,
}

impl<S: Sample> Graph<S> {
    /// The graph's own input: what the engine is given to process.
    pub const INPUT: NodeId = NodeId(0);
    /// The graph's own output: what the engine leaves in the buffer.
    pub const OUTPUT: NodeId = NodeId(1);

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
            connections: Vec::new(),
            next_id: 2,
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
        self.collect_garbage();
        let mut node: Box<dyn Node<S>> = Box::new(node);
        node.prepare(&self.spec());
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.push(NodeEntry {
            id,
            latency: node.latency_samples(),
            waiting: Some(node),
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
        self.connections
            .retain(|&(from, to)| from != id && to != id);
        Ok(())
    }

    /// Feeds the output of `from` into the input of `to`.
    ///
    /// A node with several inputs receives their sum. Connecting the same
    /// pair twice has no further effect. Use [`INPUT`](Self::INPUT) and
    /// [`OUTPUT`](Self::OUTPUT) for the ends of the graph.
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check_ends(from, to)?;
        if !self.connections.contains(&(from, to)) {
            self.connections.push((from, to));
            if self.order().is_err() {
                self.connections.pop();
                return Err(GraphError::Cycle);
            }
        }
        Ok(())
    }

    /// Removes the connection from `from` to `to`, if there is one.
    pub fn disconnect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.check_ends(from, to)?;
        self.connections.retain(|&pair| pair != (from, to));
        Ok(())
    }

    fn check_ends(&self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        if from == Self::OUTPUT || to == Self::INPUT || from == to {
            return Err(GraphError::InvalidConnection);
        }
        for id in [from, to] {
            let is_end = id == Self::INPUT || id == Self::OUTPUT;
            if !is_end && !self.nodes.iter().any(|node| node.id == id) {
                return Err(GraphError::UnknownNode(id));
            }
        }
        Ok(())
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
                    if let Some(node) = slot.node.take() {
                        if let Some(entry) = self.nodes.iter_mut().find(|n| n.id == slot.id) {
                            entry.waiting = Some(node);
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

    /// The nodes in an order in which each one comes after all of its
    /// sources, or an error if the connections form a loop.
    fn order(&self) -> Result<Vec<NodeId>, GraphError> {
        let mut waiting_for: Vec<(NodeId, usize)> = self
            .nodes
            .iter()
            .map(|node| {
                let sources = self
                    .connections
                    .iter()
                    .filter(|&&(from, to)| to == node.id && from != Self::INPUT)
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
            for &(from, to) in &self.connections {
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
        let order = self.order()?;
        let position = |id: NodeId| order.iter().position(|&other| other == id);

        // How late each node's output is, counting everything before it.
        let mut lateness = vec![0usize; order.len()];
        let mut slots = Vec::with_capacity(order.len());

        for (index, &id) in order.iter().enumerate() {
            let sources: Vec<(Source, usize)> = self
                .connections
                .iter()
                .filter(|&&(_, to)| to == id)
                .filter_map(|&(from, _)| {
                    if from == Self::INPUT {
                        Some((Source::GraphInput, 0))
                    } else {
                        position(from).map(|at| (Source::Node(at), lateness[at]))
                    }
                })
                .collect();
            let latest = sources.iter().map(|&(_, late)| late).max().unwrap_or(0);
            let entry = self
                .nodes
                .iter_mut()
                .find(|node| node.id == id)
                .expect("the order only contains existing nodes");
            lateness[index] = latest + entry.latency;

            slots.push(Slot {
                id,
                node: entry.waiting.take(),
                inputs: sources
                    .into_iter()
                    .map(|(source, late)| Input::new(source, latest - late, spec.num_channels))
                    .collect(),
            });
        }

        let outputs: Vec<(Source, usize)> = self
            .connections
            .iter()
            .filter(|&&(_, to)| to == Self::OUTPUT)
            .filter_map(|&(from, _)| {
                if from == Self::INPUT {
                    Some((Source::GraphInput, 0))
                } else {
                    position(from).map(|at| (Source::Node(at), lateness[at]))
                }
            })
            .collect();
        let latency = outputs.iter().map(|&(_, late)| late).max().unwrap_or(0);

        let plan = Box::new(Plan {
            buffers: (0..slots.len())
                .map(|_| AudioBuffer::new(spec.num_channels, spec.max_block_size))
                .collect(),
            slots,
            outputs: outputs
                .into_iter()
                .map(|(source, late)| Input::new(source, latency - late, spec.num_channels))
                .collect(),
            mix: AudioBuffer::new(spec.num_channels, spec.max_block_size),
            latency,
        });
        Ok((plan, latency))
    }
}
