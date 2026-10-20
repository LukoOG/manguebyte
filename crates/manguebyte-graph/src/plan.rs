//! What travels between the editing thread and the audio thread, and how
//! the audio thread runs it.

use std::any::Any;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::ProcessContext;
use manguebyte_dsp::event::{Event, EventBuffer};

use crate::{Node, NodeId, Route};

/// A change to one node, applied on the audio thread.
///
/// It is an `FnMut` that is called once. An `FnOnce` in a box frees the box
/// when it is called, and the audio thread must not free anything.
pub(crate) type Update = Box<dyn FnMut(&mut dyn Any) + Send>;

/// Collects the events a node sent while it processed a block. It receives
/// the node as `Any`, and casts it back to the type it was added as.
pub(crate) type Emitter = Box<dyn FnMut(&mut dyn Any, &mut EventBuffer) + Send>;

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
    /// The sidechain the engine was given, if any.
    GraphSidechain,
    /// The output of the node at this position in the plan.
    Node(usize),
}

/// Where events come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventSource {
    /// The events the engine was given.
    GraphInput,
    /// The events sent by the node at this position in the plan.
    Node(usize),
}

/// Which connection of the description an input stands for, so that a new
/// plan can take over the delay lines of the plan it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireKey {
    pub(crate) from: NodeId,
    pub(crate) to: NodeId,
    pub(crate) route: Route,
}

/// A fixed delay of a whole number of frames, for one channel.
pub(crate) struct DelayChannel<S> {
    memory: Vec<S>,
    position: usize,
}

impl<S: Sample> DelayChannel<S> {
    fn new(frames: usize) -> Self {
        Self {
            memory: vec![S::ZERO; frames],
            position: 0,
        }
    }
}

/// One audio connection into a bus.
pub(crate) struct Input<S: Sample> {
    pub(crate) key: WireKey,
    pub(crate) source: Source,
    source_channel: usize,
    target_channel: usize,
    channel_count: usize,
    /// Whether the delay is the connection's own, read before the source
    /// has run and written after, instead of one that lines paths up.
    feedback: bool,
    /// One delay per channel, or empty if this input needs none.
    lines: Vec<DelayChannel<S>>,
}

impl<S: Sample> Input<S> {
    /// An input that delays by `delay_frames`: its own delay if the route
    /// has one, otherwise the delay that lines it up with the node's other
    /// inputs.
    pub(crate) fn new(
        key: WireKey,
        source: Source,
        delay_frames: usize,
        bus_channels: usize,
    ) -> Self {
        let route = key.route;
        let feedback = route.delay() > 0;
        let delay = if feedback {
            route.delay()
        } else {
            delay_frames
        };
        let channels = route
            .channel_count()
            .min(bus_channels.saturating_sub(route.target_channel()));
        let lines = if delay == 0 {
            Vec::new()
        } else {
            (0..channels).map(|_| DelayChannel::new(delay)).collect()
        };
        Self {
            key,
            source,
            source_channel: route.source_channel(),
            target_channel: route.target_channel(),
            channel_count: route.channel_count(),
            feedback,
            lines,
        }
    }

    pub(crate) fn is_feedback(&self) -> bool {
        self.feedback
    }

    /// Adds this input's contribution to `target`: `source` from frame
    /// `start` on, delayed if this input lines paths up, or what a loop
    /// left in the delay if it is a feedback connection. `None` is silence.
    ///
    /// The first input of a bus overwrites instead of adding, which is the
    /// same number but keeps the sign of a zero, so that a node with a
    /// single input hears it bit for bit.
    pub(crate) fn mix_into(
        &mut self,
        source: Option<(&AudioBuffer<S>, usize)>,
        target: &mut AudioBuffer<S>,
        first: bool,
    ) {
        let frames = target.num_frames();
        let room = target.num_channels().saturating_sub(self.target_channel);
        if self.feedback {
            for (index, line) in self.lines.iter().enumerate().take(room) {
                let into = &mut target.channel_mut(self.target_channel + index)[..frames];
                let length = line.memory.len();
                let mut at = line.position;
                for out in into {
                    let sample = line.memory[at];
                    *out = if first { sample } else { *out + sample };
                    at += 1;
                    if at == length {
                        at = 0;
                    }
                }
            }
            return;
        }
        let from_channel = |channel: usize| -> &[S] {
            source
                .and_then(|(buffer, start)| {
                    let channel = self.source_channel.checked_add(channel)?;
                    (channel < buffer.num_channels())
                        .then(|| buffer.channel(channel).get(start..))
                        .flatten()
                })
                .unwrap_or(&[])
        };
        if self.lines.is_empty() {
            let available = source.map_or(0, |(buffer, _)| {
                buffer.num_channels().saturating_sub(self.source_channel)
            });
            for channel in 0..self.channel_count.min(room).min(available) {
                let from = from_channel(channel);
                let into = target.channel_mut(self.target_channel + channel);
                for (frame, out) in into.iter_mut().enumerate() {
                    let sample = from.get(frame).copied().unwrap_or(S::ZERO);
                    *out = if first { sample } else { *out + sample };
                }
            }
            return;
        }
        for channel in 0..self.lines.len().min(room) {
            // Read the oldest sample, then overwrite it with the newest: a
            // delay of exactly the memory's length. Missing source frames
            // are silence, so the line keeps time either way.
            let from: &[S] = from_channel(channel);
            let line = &mut self.lines[channel];
            let into = target.channel_mut(self.target_channel + channel);
            for (frame, out) in into.iter_mut().enumerate() {
                let sample = line.memory[line.position];
                *out = if first { sample } else { *out + sample };
                line.memory[line.position] = from.get(frame).copied().unwrap_or(S::ZERO);
                line.position += 1;
                if line.position == line.memory.len() {
                    line.position = 0;
                }
            }
        }
    }

    /// Stores what the source of a feedback connection produced during the
    /// piece just run, `frames` long, for the frames that read it later.
    pub(crate) fn record(&mut self, source: Option<(&AudioBuffer<S>, usize)>, frames: usize) {
        if !self.feedback {
            return;
        }
        for (index, line) in self.lines.iter_mut().enumerate() {
            let from = source
                .and_then(|(buffer, start)| {
                    let channel = self.source_channel.checked_add(index)?;
                    (channel < buffer.num_channels())
                        .then(|| buffer.channel(channel).get(start..))
                        .flatten()
                })
                .unwrap_or(&[]);
            let length = line.memory.len();
            for frame in 0..frames {
                line.memory[line.position] = from.get(frame).copied().unwrap_or(S::ZERO);
                line.position += 1;
                if line.position == length {
                    line.position = 0;
                }
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        for line in &mut self.lines {
            line.memory.fill(S::ZERO);
            line.position = 0;
        }
    }

    /// The shortest feedback delay of this input, if it is one.
    fn feedback_delay(&self) -> Option<usize> {
        self.feedback.then(|| self.key.route.delay())
    }

    /// Takes over the delay lines of `old`, if it stands for the same
    /// connection with the same delay, so that what is travelling through
    /// them is not lost. A swap of vectors: nothing is allocated or freed.
    fn adopt(&mut self, old: &mut Self) -> bool {
        let same_shape = self.lines.len() == old.lines.len()
            && self
                .lines
                .iter()
                .zip(&old.lines)
                .all(|(new, old)| new.memory.len() == old.memory.len());
        if self.key != old.key || self.feedback != old.feedback || !same_shape {
            return false;
        }
        std::mem::swap(&mut self.lines, &mut old.lines);
        true
    }
}

/// One event connection into a node, or into the graph's output.
pub(crate) struct EventInput {
    pub(crate) from: NodeId,
    pub(crate) to: NodeId,
    pub(crate) source: EventSource,
    /// How many frames the events are held back, to line them up with the
    /// node's audio.
    delay: u32,
    /// Events on their way, with offsets counted from the start of the
    /// next piece. Room for them is reserved when the plan is made.
    pending: Vec<Event>,
}

impl EventInput {
    pub(crate) fn new(from: NodeId, to: NodeId, source: EventSource, delay: usize) -> Self {
        let delay = u32::try_from(delay).unwrap_or(u32::MAX);
        Self {
            from,
            to,
            source,
            delay,
            pending: if delay == 0 {
                Vec::new()
            } else {
                Vec::with_capacity(crate::EVENT_CAPACITY)
            },
        }
    }

    /// Adds the events of `events` that fall in this piece of `frames`
    /// frames to `target`, each moved on by `shift` frames. Events that
    /// are held back wait for their piece. What finds no room is counted
    /// in `lost`.
    pub(crate) fn deliver(
        &mut self,
        events: &EventBuffer,
        frames: usize,
        target: &mut EventBuffer,
        shift: u32,
        lost: &mut u64,
    ) {
        fn put(target: &mut EventBuffer, event: Event, shift: u32, lost: &mut u64) {
            let event = Event {
                offset: event.offset.saturating_add(shift),
                ..event
            };
            if target.push(event).is_err() {
                *lost += 1;
            }
        }
        if self.delay == 0 {
            for &event in events {
                put(target, event, shift, lost);
            }
            return;
        }
        for &event in events {
            if self.pending.len() < self.pending.capacity() {
                self.pending.push(Event {
                    offset: event.offset.saturating_add(self.delay),
                    ..event
                });
            } else {
                *lost += 1;
            }
        }
        let frames = u32::try_from(frames).unwrap_or(u32::MAX);
        let mut kept = 0;
        for index in 0..self.pending.len() {
            let event = self.pending[index];
            if event.offset < frames {
                put(target, event, shift, lost);
            } else {
                self.pending[kept] = Event {
                    offset: event.offset - frames,
                    ..event
                };
                kept += 1;
            }
        }
        self.pending.truncate(kept);
    }

    fn adopt(&mut self, old: &mut Self) -> bool {
        if (self.from, self.to, self.delay) != (old.from, old.to, old.delay) || self.delay == 0 {
            return false;
        }
        std::mem::swap(&mut self.pending, &mut old.pending);
        true
    }
}

/// A node's sidechain: what is connected to it, and where it is mixed.
pub(crate) struct SidechainBus<S: Sample> {
    pub(crate) inputs: Vec<Input<S>>,
    pub(crate) buffer: AudioBuffer<S>,
}

/// A node's event input: what is connected to it, and where the events
/// are merged.
pub(crate) struct EventBus {
    pub(crate) inputs: Vec<EventInput>,
    pub(crate) buffer: EventBuffer,
}

/// One node in running order.
pub(crate) struct Slot<S: Sample> {
    pub(crate) id: NodeId,
    /// The processor. `None` in a new plan for a node the engine already
    /// has; the engine fills it in from the plan it is replacing.
    pub(crate) node: Option<Box<dyn Node<S>>>,
    /// Collects the events the node sends, for a node added with them.
    /// Travels with the node.
    pub(crate) emitter: Option<Emitter>,
    pub(crate) inputs: Vec<Input<S>>,
    /// `None` when nothing is connected to the sidechain: the node then
    /// receives the sidechain the engine was given, as it always has.
    pub(crate) sidechain: Option<SidechainBus<S>>,
    /// `None` when no events are connected to the node: it then receives
    /// the events the engine was given, as it always has.
    pub(crate) events: Option<EventBus>,
    /// What the node sent during the piece just run.
    pub(crate) emitted: EventBuffer,
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
    /// The events connected to the graph's output.
    pub(crate) output_events: Vec<EventInput>,
    /// Where the output is assembled before it replaces the input.
    pub(crate) mix: AudioBuffer<S>,
    pub(crate) latency: usize,
    /// The longest piece the plan may run at once: the shortest feedback
    /// delay in it, or `usize::MAX` if it has none.
    pub(crate) piece_limit: usize,
    /// For moving the transport on to a piece.
    pub(crate) sample_rate: f64,
}

impl<S: Sample> Plan<S> {
    pub(crate) fn empty(channels: usize, max_block_size: usize, sample_rate: f64) -> Self {
        Self {
            slots: Vec::new(),
            buffers: Vec::new(),
            outputs: Vec::new(),
            output_events: Vec::new(),
            mix: AudioBuffer::new(channels, max_block_size),
            latency: 0,
            piece_limit: usize::MAX,
            sample_rate,
        }
    }

    /// Works out [`piece_limit`](Self::piece_limit) from the inputs.
    pub(crate) fn find_piece_limit(&mut self) {
        let slots = self.slots.iter().flat_map(|slot| {
            slot.inputs
                .iter()
                .chain(slot.sidechain.iter().flat_map(|bus| bus.inputs.iter()))
        });
        self.piece_limit = slots
            .chain(self.outputs.iter())
            .filter_map(Input::feedback_delay)
            .min()
            .unwrap_or(usize::MAX);
    }

    /// Takes the processor of the node with this identifier, and what
    /// collects its events, if the plan has it.
    pub(crate) fn take(&mut self, id: NodeId) -> (Option<Box<dyn Node<S>>>, Option<Emitter>) {
        self.slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .map_or((None, None), |slot| (slot.node.take(), slot.emitter.take()))
    }

    /// The node itself, not the box around it: a `Box<dyn Node>` is also a
    /// `Node`, and its `as_any_mut` would describe the box.
    pub(crate) fn node_mut(&mut self, id: NodeId) -> Option<&mut dyn Node<S>> {
        self.slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.node.as_deref_mut())
    }

    /// Takes over from `old` everything still travelling through delays
    /// that both plans have: feedback, compensation and held-back events.
    /// An edit elsewhere in the graph then leaves them untouched.
    ///
    /// Runs on the audio thread. It only swaps vectors, so it neither
    /// allocates nor frees; the search is linear in the size of the plans.
    pub(crate) fn adopt_delays(&mut self, old: &mut Self) {
        fn adopt_input<S: Sample>(input: &mut Input<S>, old: &mut Plan<S>) {
            if input.lines.is_empty() {
                return;
            }
            for slot in &mut old.slots {
                let buses = slot.inputs.iter_mut().chain(
                    slot.sidechain
                        .iter_mut()
                        .flat_map(|bus| bus.inputs.iter_mut()),
                );
                for candidate in buses {
                    if input.adopt(candidate) {
                        return;
                    }
                }
            }
            for candidate in &mut old.outputs {
                if input.adopt(candidate) {
                    return;
                }
            }
        }
        fn adopt_events<S: Sample>(input: &mut EventInput, old: &mut Plan<S>) {
            if input.delay == 0 {
                return;
            }
            for slot in &mut old.slots {
                if let Some(bus) = &mut slot.events {
                    for candidate in &mut bus.inputs {
                        if input.adopt(candidate) {
                            return;
                        }
                    }
                }
            }
            for candidate in &mut old.output_events {
                if input.adopt(candidate) {
                    return;
                }
            }
        }

        for slot in &mut self.slots {
            for input in &mut slot.inputs {
                adopt_input(input, old);
            }
            if let Some(bus) = &mut slot.sidechain {
                for input in &mut bus.inputs {
                    adopt_input(input, old);
                }
            }
            if let Some(bus) = &mut slot.events {
                for input in &mut bus.inputs {
                    adopt_events(input, old);
                }
            }
        }
        for input in &mut self.outputs {
            adopt_input(input, old);
        }
        for input in &mut self.output_events {
            adopt_events(input, old);
        }
    }

    /// Silences every delay and drops every held-back event.
    pub(crate) fn clear_delays(&mut self) {
        for slot in &mut self.slots {
            for input in &mut slot.inputs {
                input.clear();
            }
            if let Some(bus) = &mut slot.sidechain {
                for input in &mut bus.inputs {
                    input.clear();
                }
            }
            if let Some(bus) = &mut slot.events {
                for input in &mut bus.inputs {
                    input.pending.clear();
                }
            }
        }
        for output in &mut self.outputs {
            output.clear();
        }
        for output in &mut self.output_events {
            output.pending.clear();
        }
    }

    /// Runs the plan over one piece that fits its buffers and is no longer
    /// than [`piece_limit`](Self::piece_limit).
    ///
    /// `buffer` is the piece, which begins `start` frames into the block the
    /// engine was given with `context`; `events` are the piece's own events,
    /// counted from its start. Events for the graph's output are added to
    /// `output_events`, counted from the start of the block.
    pub(crate) fn run(
        &mut self,
        buffer: &mut AudioBuffer<S>,
        context: &ProcessContext<'_, S>,
        start: usize,
        events: &EventBuffer,
        output_events: &mut EventBuffer,
        lost: &mut u64,
    ) {
        let frames = buffer.num_frames();
        // Where this piece is in the block the caller's sidechain and
        // transport describe.
        let offset = context.offset.saturating_add(start);
        let graph_sidechain = context.sidechain.map(|sidechain| (sidechain, offset));
        // A node with a sidechain of its own gets a context that begins
        // with the piece: its sidechain is the piece's, so its transport
        // has to be moved on to the piece as well.
        let mut piece_transport = *context.transport;
        if offset > 0 {
            piece_transport.advance(offset, self.sample_rate);
        }

        for index in 0..self.slots.len() {
            // A node's sources all come before it, so its buffer and theirs
            // can be borrowed apart; so can the events they sent.
            let (earlier, rest) = self.buffers.split_at_mut(index);
            let target = &mut rest[0];
            let (earlier_slots, rest_slots) = self.slots.split_at_mut(index);
            let Slot {
                node,
                emitter,
                inputs,
                sidechain,
                events: event_bus,
                emitted,
                ..
            } = &mut rest_slots[0];

            target.set_num_frames(frames);
            target.clear();
            let source = |source: Source| match source {
                Source::GraphInput => Some((&*buffer, 0)),
                Source::GraphSidechain => graph_sidechain,
                Source::Node(at) => earlier.get(at).map(|buffer| (buffer, 0)),
            };
            for (number, input) in inputs.iter_mut().enumerate() {
                input.mix_into(source(input.source), target, number == 0);
            }
            if let Some(bus) = sidechain.as_mut() {
                bus.buffer.set_num_frames(frames);
                bus.buffer.clear();
                for (number, input) in bus.inputs.iter_mut().enumerate() {
                    input.mix_into(source(input.source), &mut bus.buffer, number == 0);
                }
            }
            if let Some(bus) = event_bus.as_mut() {
                bus.buffer.clear();
                for input in &mut bus.inputs {
                    let from = match input.source {
                        EventSource::GraphInput => events,
                        EventSource::Node(at) => &earlier_slots[at].emitted,
                    };
                    input.deliver(from, frames, &mut bus.buffer, 0, lost);
                }
            }

            let Some(node) = node.as_deref_mut() else {
                continue;
            };
            let node_events = event_bus.as_ref().map_or(events, |bus| &bus.buffer);
            // Copied from the caller's, so that whatever else a context
            // carries reaches every node.
            let mut node_context = *context;
            node_context.events = node_events;
            match sidechain.as_ref() {
                Some(bus) => {
                    node_context.transport = &piece_transport;
                    node_context.sidechain = Some(&bus.buffer);
                    node_context.offset = 0;
                }
                None => node_context.offset = offset,
            }
            node.process_with(target, &node_context);
            if let Some(emit) = emitter {
                emitted.clear();
                emit(node.as_any_mut(), emitted);
            }
        }

        // The output is assembled apart, because the graph's input may be
        // one of the things connected to it.
        self.mix.set_num_frames(frames);
        self.mix.clear();
        let nodes = &self.buffers;
        let source = |source: Source| match source {
            Source::GraphInput => Some((&*buffer, 0)),
            Source::GraphSidechain => graph_sidechain,
            Source::Node(at) => nodes.get(at).map(|buffer| (buffer, 0)),
        };
        for (number, output) in self.outputs.iter_mut().enumerate() {
            output.mix_into(source(output.source), &mut self.mix, number == 0);
        }
        let shift = u32::try_from(start).unwrap_or(u32::MAX);
        for output in &mut self.output_events {
            let from = match output.source {
                EventSource::GraphInput => events,
                EventSource::Node(at) => &self.slots[at].emitted,
            };
            output.deliver(from, frames, output_events, shift, lost);
        }

        // Every source has run: keep what the feedback connections carry,
        // before the graph's input is overwritten by its output.
        if self.piece_limit != usize::MAX {
            for slot in &mut self.slots {
                let sidechain = slot
                    .sidechain
                    .iter_mut()
                    .flat_map(|bus| bus.inputs.iter_mut());
                for input in slot.inputs.iter_mut().chain(sidechain) {
                    if input.is_feedback() {
                        input.record(source(input.source), frames);
                    }
                }
            }
            for output in &mut self.outputs {
                if output.is_feedback() {
                    output.record(source(output.source), frames);
                }
            }
        }

        let channels = buffer.num_channels().min(self.mix.num_channels());
        for channel in 0..channels {
            buffer
                .channel_mut(channel)
                .copy_from_slice(self.mix.channel(channel));
        }
        for channel in channels..buffer.num_channels() {
            buffer.channel_mut(channel).fill(S::ZERO);
        }
    }
}
