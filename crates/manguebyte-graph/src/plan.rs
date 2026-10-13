//! What travels between the editing thread and the audio thread.

use std::any::Any;

use manguebyte_core::{AudioBuffer, Sample};

use crate::{Node, NodeId};

/// A change to one node, applied on the audio thread.
///
/// It is an `FnMut` that is called once. An `FnOnce` in a box frees the box
/// when it is called, and the audio thread must not free anything.
pub(crate) type Update = Box<dyn FnMut(&mut dyn Any) + Send>;

/// An instruction for the engine.
pub(crate) enum Command<S: Sample> {
    /// Run this plan from now on.
    Swap(Box<Plan<S>>),
    /// Change one node.
    Update { node: NodeId, apply: Update },
}

/// Something the engine has finished with, on its way back to be freed.
///
/// Nothing ever reads the contents: they are carried to the editing thread
/// only so that dropping them happens there.
#[allow(dead_code)]
pub(crate) enum Garbage<S: Sample> {
    Plan(Box<Plan<S>>),
    Update(Update),
}

/// Where a signal comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The buffer the engine was given.
    GraphInput,
    /// The output of the node at this position in the plan.
    Node(usize),
}

/// A fixed delay of a whole number of frames, for one channel.
struct DelayChannel<S> {
    memory: Vec<S>,
    position: usize,
}

/// One connection into a node, with the delay that lines it up with the
/// node's other inputs.
pub(crate) struct Input<S: Sample> {
    pub(crate) source: Source,
    /// One delay per channel, or empty if this input needs none.
    delay: Vec<DelayChannel<S>>,
}

impl<S: Sample> Input<S> {
    pub(crate) fn new(source: Source, delay_frames: usize, channels: usize) -> Self {
        let delay = if delay_frames == 0 {
            Vec::new()
        } else {
            (0..channels)
                .map(|_| DelayChannel {
                    memory: vec![S::ZERO; delay_frames],
                    position: 0,
                })
                .collect()
        };
        Self { source, delay }
    }

    /// Adds `source` to `target`, delayed if this input needs it.
    pub(crate) fn mix_into(&mut self, source: &AudioBuffer<S>, target: &mut AudioBuffer<S>) {
        let channels = source.num_channels().min(target.num_channels());
        for channel in 0..channels {
            let from = source.channel(channel);
            let into = target.channel_mut(channel);
            match self.delay.get_mut(channel) {
                None => {
                    for (out, &sample) in into.iter_mut().zip(from) {
                        *out += sample;
                    }
                }
                Some(delay) => {
                    for (out, &sample) in into.iter_mut().zip(from) {
                        // Read the oldest sample, then overwrite it with the
                        // newest: a delay of exactly the memory's length.
                        *out += delay.memory[delay.position];
                        delay.memory[delay.position] = sample;
                        delay.position += 1;
                        if delay.position == delay.memory.len() {
                            delay.position = 0;
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        for delay in &mut self.delay {
            delay.memory.fill(S::ZERO);
        }
    }
}

/// One node in running order.
pub(crate) struct Slot<S: Sample> {
    pub(crate) id: NodeId,
    /// The processor. `None` in a new plan for a node the engine already
    /// has; the engine fills it in from the plan it is replacing.
    pub(crate) node: Option<Box<dyn Node<S>>>,
    pub(crate) inputs: Vec<Input<S>>,
}

/// Everything the engine needs to run the graph, with all memory already
/// allocated.
pub(crate) struct Plan<S: Sample> {
    /// The nodes, each after all of its sources.
    pub(crate) slots: Vec<Slot<S>>,
    /// The output of each node, in the same order.
    pub(crate) buffers: Vec<AudioBuffer<S>>,
    /// What is connected to the graph's output.
    pub(crate) outputs: Vec<Input<S>>,
    /// Where the output is assembled before it replaces the input.
    pub(crate) mix: AudioBuffer<S>,
    pub(crate) latency: usize,
}

impl<S: Sample> Plan<S> {
    pub(crate) fn empty(channels: usize, max_block_size: usize) -> Self {
        Self {
            slots: Vec::new(),
            buffers: Vec::new(),
            outputs: Vec::new(),
            mix: AudioBuffer::new(channels, max_block_size),
            latency: 0,
        }
    }

    /// Takes the processor of the node with this identifier, if the plan
    /// has it.
    pub(crate) fn take(&mut self, id: NodeId) -> Option<Box<dyn Node<S>>> {
        self.slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.node.take())
    }

    /// The node itself, not the box around it: a `Box<dyn Node>` is also a
    /// `Node`, and its `as_any_mut` would describe the box.
    pub(crate) fn node_mut(&mut self, id: NodeId) -> Option<&mut dyn Node<S>> {
        self.slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.node.as_deref_mut())
    }
}
