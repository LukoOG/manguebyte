use std::sync::{Arc, Mutex};

use manguebyte_core::spsc::{Consumer, Producer};
use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::event::{Event, EventBuffer};
use manguebyte_dsp::{ProcessContext, ProcessSpec, Processor};

use crate::plan::{Command, Garbage, Plan};
use crate::{EVENT_CAPACITY, QUEUE_CAPACITY};

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
    /// The caller's events for one piece of a block that is run in pieces.
    piece_events: EventBuffer,
    /// The events connected to the graph's output, from the last block.
    output_events: EventBuffer,
    /// Events dropped for want of room since the engine was made.
    lost_events: u64,
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
            plan: Box::new(Plan::empty(
                current.num_channels,
                current.max_block_size,
                current.sample_rate,
            )),
            commands,
            garbage,
            held: Vec::with_capacity(QUEUE_CAPACITY),
            piece_events: EventBuffer::with_capacity(EVENT_CAPACITY),
            output_events: EventBuffer::with_capacity(EVENT_CAPACITY),
            lost_events: 0,
        }
    }

    /// How many nodes the engine is currently running.
    pub fn num_nodes(&self) -> usize {
        self.plan.slots.len()
    }

    /// The events connected to the graph's
    /// [output](crate::Graph::OUTPUT) during the last block, each on the
    /// frame of that block it belongs to, lined up with the audio output as
    /// latency compensation lines up the audio.
    ///
    /// Read it after each call to
    /// [`process_with`](Processor::process_with); the next call replaces
    /// it. Real-time safe.
    pub fn output_events(&self) -> &EventBuffer {
        &self.output_events
    }

    /// How many events have been dropped since the engine was made, because
    /// more than [`EVENT_CAPACITY`] arrived at one input, or were held back
    /// at once by latency compensation, in one block. Real-time safe.
    pub fn lost_events(&self) -> u64 {
        self.lost_events
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
                            let (node, emitter) = self.plan.take(slot.id);
                            slot.node = node;
                            slot.emitter = emitter;
                        }
                    }
                    // What is on its way through delays the two plans share
                    // carries on, so an edit elsewhere is not heard here.
                    next.adopt_delays(&mut self.plan);
                    next.sample_rate = self.plan.sample_rate;
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
}

impl<S: Sample> Processor<S> for GraphEngine<S> {
    /// Gets every node ready for `spec`, and makes it the specification new
    /// nodes are prepared with.
    ///
    /// The engine is already prepared for the specification its graph was
    /// created with, so this is only needed when that changes. It allocates,
    /// and belongs off the audio thread like any `prepare`. After a change
    /// of channel count, commit the graph again so that its delays have the
    /// new count too.
    fn prepare(&mut self, spec: &ProcessSpec) {
        *self
            .spec
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = *spec;
        self.apply_commands();
        let plan = &mut *self.plan;
        plan.sample_rate = spec.sample_rate;
        for slot in &mut plan.slots {
            if let Some(node) = &mut slot.node {
                node.prepare(spec);
            }
            if let Some(bus) = &mut slot.sidechain {
                bus.buffer.resize(spec.num_channels, spec.max_block_size);
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

    /// Runs the graph over one block.
    ///
    /// A block longer than the graph was prepared for, or than the
    /// shortest feedback delay in it, is run in pieces. Each piece gets the
    /// events that fall in it, on their frames, and a context whose
    /// [`offset`](ProcessContext::offset) says where it begins, so the
    /// output does not depend on where the pieces fall.
    fn process_with(&mut self, buffer: &mut AudioBuffer<S>, context: &ProcessContext<'_, S>) {
        self.apply_commands();
        self.output_events.clear();
        let limit = self.plan.mix.capacity().min(self.plan.piece_limit).max(1);
        let frames = buffer.num_frames();
        let Self {
            plan,
            piece_events,
            output_events,
            lost_events,
            ..
        } = self;
        if frames <= limit {
            plan.run(
                buffer,
                context,
                0,
                context.events,
                output_events,
                lost_events,
            );
            return;
        }
        let events = context.events.as_slice();
        let mut next = 0;
        let mut start = 0;
        while start < frames {
            let end = (start + limit).min(frames);
            piece_events.clear();
            // The last piece also takes whatever is placed past the end of
            // the block, so that it is never lost.
            while let Some(event) = events.get(next) {
                let at = event.offset as usize;
                if at >= end && end < frames {
                    break;
                }
                let moved = Event {
                    offset: u32::try_from(at.saturating_sub(start)).unwrap_or(u32::MAX),
                    ..*event
                };
                if piece_events.push(moved).is_err() {
                    *lost_events += 1;
                }
                next += 1;
            }
            buffer.windowed(start..end, |piece| {
                plan.run(
                    piece,
                    context,
                    start,
                    piece_events,
                    output_events,
                    lost_events,
                );
            });
            start = end;
        }
    }

    fn reset(&mut self) {
        let plan = &mut *self.plan;
        for slot in &mut plan.slots {
            if let Some(node) = &mut slot.node {
                node.reset();
            }
        }
        plan.clear_delays();
    }

    fn latency_samples(&self) -> usize {
        self.plan.latency
    }
}
