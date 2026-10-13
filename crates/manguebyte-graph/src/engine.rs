use std::sync::{Arc, Mutex};

use manguebyte_core::spsc::{Consumer, Producer};
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::{ProcessContext, ProcessSpec, Processor};

use crate::QUEUE_CAPACITY;
use crate::plan::{Command, Garbage, Plan, Source};

/// The half of a graph that runs on the audio thread.
///
/// It is a [`Processor`]: put it in a device callback, a plugin or a chain
/// like any other. Everything it does while processing is free of
/// allocation, locks and frees, including switching to a rewired graph.
///
/// Created together with its [`Graph`](crate::Graph) by
/// [`Graph::new`](crate::Graph::new).
pub struct GraphEngine<S: Sample = f32> {
    spec: Arc<Mutex<ProcessSpec>>,
    plan: Box<Plan<S>>,
    commands: Consumer<Command<S>>,
    garbage: Producer<Garbage<S>>,
    /// Things that could not be sent back because the queue was full. They
    /// wait here, in memory reserved up front, and are sent when there is
    /// room.
    held: Vec<Garbage<S>>,
}

impl<S: Sample> GraphEngine<S> {
    pub(crate) fn new(
        spec: Arc<Mutex<ProcessSpec>>,
        commands: Consumer<Command<S>>,
        garbage: Producer<Garbage<S>>,
    ) -> Self {
        let current = *spec.lock().unwrap_or_else(|poison| poison.into_inner());
        Self {
            spec,
            plan: Box::new(Plan::empty(current.num_channels, current.max_block_size)),
            commands,
            garbage,
            held: Vec::with_capacity(QUEUE_CAPACITY),
        }
    }

    /// How many nodes the engine is currently running.
    pub fn num_nodes(&self) -> usize {
        self.plan.slots.len()
    }

    /// Applies every edit that has arrived since the last block.
    fn apply_commands(&mut self) {
        self.release_held();
        while let Some(command) = self.commands.pop() {
            match command {
                Command::Swap(mut next) => {
                    // Move the processors we already have into the new plan.
                    // Each is a pointer; nothing is allocated or freed.
                    for slot in &mut next.slots {
                        if slot.node.is_none() {
                            slot.node = self.plan.take(slot.id);
                        }
                    }
                    let previous = std::mem::replace(&mut self.plan, next);
                    self.send_back(Garbage::Plan(previous));
                }
                Command::Update { node, mut apply } => {
                    if let Some(target) = self.plan.node_mut(node) {
                        apply(target.as_any_mut());
                    }
                    self.send_back(Garbage::Update(apply));
                }
            }
        }
    }

    /// Hands something to the editing thread to be freed there.
    fn send_back(&mut self, garbage: Garbage<S>) {
        if let Err(garbage) = self.garbage.push(garbage) {
            if self.held.len() < self.held.capacity() {
                self.held.push(garbage);
            } else {
                // The editing thread has stopped collecting and every
                // reserve is full. Freeing here could stall the audio, so
                // the memory is abandoned instead.
                std::mem::forget(garbage);
            }
        }
    }

    fn release_held(&mut self) {
        while let Some(garbage) = self.held.pop() {
            if let Err(garbage) = self.garbage.push(garbage) {
                // Still no room. The capacity is unchanged, so this push
                // cannot allocate.
                self.held.push(garbage);
                break;
            }
        }
    }

    /// Runs the plan over one block that fits the buffers.
    fn run(&mut self, buffer: &mut AudioBuffer<S>, context: &ProcessContext<'_, S>) {
        let plan = &mut *self.plan;
        let frames = buffer.num_frames();

        for (index, slot) in plan.slots.iter_mut().enumerate() {
            // A node's sources all come before it, so its buffer and theirs
            // can be borrowed apart.
            let (earlier, rest) = plan.buffers.split_at_mut(index);
            let target = &mut rest[0];
            target.set_num_frames(frames);
            target.clear();
            for input in &mut slot.inputs {
                let source = match input.source {
                    Source::GraphInput => &*buffer,
                    Source::Node(at) => &earlier[at],
                };
                input.mix_into(source, target);
            }
            if let Some(node) = &mut slot.node {
                node.process_with(target, context);
            }
        }

        // The output is assembled apart, because the graph's input may be
        // one of the things connected to it.
        plan.mix.set_num_frames(frames);
        plan.mix.clear();
        for output in &mut plan.outputs {
            let source = match output.source {
                Source::GraphInput => &*buffer,
                Source::Node(at) => &plan.buffers[at],
            };
            output.mix_into(source, &mut plan.mix);
        }
        let channels = buffer.num_channels().min(plan.mix.num_channels());
        for channel in 0..channels {
            buffer
                .channel_mut(channel)
                .copy_from_slice(plan.mix.channel(channel));
        }
        for channel in channels..buffer.num_channels() {
            buffer.channel_mut(channel).fill(S::ZERO);
        }
    }
}

impl<S: Sample> Processor<S> for GraphEngine<S> {
    /// Gets every node ready for `spec`, and makes it the specification new
    /// nodes are prepared with.
    ///
    /// The engine is already prepared for the specification its graph was
    /// created with, so this is only needed when that changes. It allocates,
    /// and belongs off the audio thread like any `prepare`.
    fn prepare(&mut self, spec: &ProcessSpec) {
        *self
            .spec
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = *spec;
        self.apply_commands();
        let plan = &mut *self.plan;
        for slot in &mut plan.slots {
            if let Some(node) = &mut slot.node {
                node.prepare(spec);
            }
        }
        for buffer in &mut plan.buffers {
            buffer.resize(spec.num_channels, spec.max_block_size);
        }
        plan.mix.resize(spec.num_channels, spec.max_block_size);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.process_with(buffer, &ProcessContext::empty());
    }

    fn process_with(&mut self, buffer: &mut AudioBuffer<S>, context: &ProcessContext<'_, S>) {
        self.apply_commands();
        let limit = self.plan.mix.capacity();
        let frames = buffer.num_frames();
        if frames <= limit {
            self.run(buffer, context);
            return;
        }
        // A block longer than the graph was prepared for is run in pieces.
        // Its events are delivered with the first piece.
        let quiet = context.without_events();
        let mut start = 0;
        while start < frames {
            let end = (start + limit).min(frames);
            let piece_context = if start == 0 { context } else { &quiet };
            buffer.windowed(start..end, |piece| self.run(piece, piece_context));
            start = end;
        }
    }

    fn reset(&mut self) {
        let plan = &mut *self.plan;
        for slot in &mut plan.slots {
            if let Some(node) = &mut slot.node {
                node.reset();
            }
            for input in &mut slot.inputs {
                input.clear();
            }
        }
        for output in &mut plan.outputs {
            output.clear();
        }
    }

    fn latency_samples(&self) -> usize {
        self.plan.latency
    }
}
