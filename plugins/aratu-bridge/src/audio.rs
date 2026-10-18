//! The audio thread's side of the bridge: the engine, the events waiting
//! for their beat, and what arrives from the server thread.
//!
//! Everything reaches the audio thread through one lock-free queue, in
//! the order the page sent it: events with their beat, events to play at
//! once, and control calls made off the audio thread (`Detached*` slots of
//! `manguebyte-web`, a new engine), each in a box. A box goes back through
//! a second queue with what the engine let go of in it, for the server
//! thread to drop, as do the SFZ instruments sampler parts finish with.
//! Nothing here allocates, frees, locks or makes a system call.

use manguebyte::dsp::Transport;
use manguebyte::prelude::AudioBuffer;
use manguebyte::spsc::{Consumer, Producer};
use manguebyte_web::{
    Chain, DetachedInstrument, DetachedPart, DetachedSfz, DetachedStages, Engine, EngineError,
    EventRecord, HandoffError, ParamId, Pushed, SfzInstrument, ffi,
};

use crate::protocol::{self, Status};
use crate::shared::{Shared, Snapshot};
use crate::store::{Store, Waiting};
use crate::timeline::{Block, Edge, Span, Timeline};

/// How late an event may arrive and still be played, at the start of the
/// block it arrives in: later than this after its beat, it is dropped.
pub const LATE_LIMIT_SECONDS: f64 = 0.1;

/// How many events each of the two stores (this lap, the next) holds.
pub const STORE_CAPACITY: usize = 1 << 15;

/// The parameter the host's tempo is sent to.
pub const MASTER_TEMPO: ParamId = ParamId::from_name("master.tempo");
/// The parameter the host's beat is sent to.
pub const MASTER_BEAT: ParamId = ParamId::from_name("master.beat");

/// A control call made off the audio thread, to be handed over on it.
pub enum Command {
    /// A fresh engine for a new session; the old one comes back in it.
    NewEngine(Box<Engine>),
    /// A part to make, or make again.
    Attach(DetachedPart),
    /// A part to take out.
    Detach(u16, DetachedPart),
    /// An instrument for a part.
    Instrument(u16, DetachedInstrument),
    /// A chain for a part, with the stages it may lack.
    Chain(u16, Chain, DetachedStages),
    /// An SFZ instrument for a sampler part, and the request to answer.
    Sfz(u16, DetachedSfz, u32),
    /// Every note let go, every waiting event dropped.
    Panic,
}

/// What reaches the audio thread, in order.
pub enum ToAudio {
    /// An event for a beat of an epoch.
    Event {
        /// The beat.
        beat: f64,
        /// The epoch it was sent for.
        epoch: u32,
        /// The record.
        record: EventRecord,
    },
    /// An event to play as soon as possible.
    Now(EventRecord),
    /// A control call.
    Command(Box<Command>),
}

/// What goes back.
pub enum FromAudio {
    /// A command handed over, with what the engine let go of in it, and
    /// its status (`ffi::status`).
    Done(Box<Command>, i32),
    /// An SFZ instrument a part has finished with.
    Finished(Box<SfzInstrument>),
}

/// The status of a refused hand-over.
pub fn status_of(error: HandoffError) -> i32 {
    match error {
        HandoffError::Engine(EngineError::TooManyParts { .. }) => ffi::status::TOO_MANY_PARTS,
        HandoffError::Engine(EngineError::NoSuchPart(_)) => ffi::status::NO_SUCH_PART,
        HandoffError::Engine(EngineError::NotASampler(_)) => ffi::status::NOT_A_SAMPLER,
        HandoffError::Engine(EngineError::LoaderFull(_)) => ffi::status::LOADER_FULL,
        HandoffError::Engine(EngineError::InvalidConfig(_)) => ffi::status::INVALID_CONFIG,
        _ => protocol::status::STALE,
    }
}

/// The audio thread's state.
pub struct Audio {
    engine: Box<Engine>,
    incoming: Consumer<ToAudio>,
    outgoing: Producer<FromAudio>,
    timeline: Timeline,
    current: Store,
    next: Store,
    seq: u64,
    /// The engine has not been told the tempo and the beat yet.
    resync: bool,
    parts: Vec<u16>,
    status: Status,
    sample_rate: f64,
    late_limit: i64,
    blocks: u64,
}

impl Audio {
    /// The audio side of a bridge, with `engine` and the two queues.
    /// Allocates.
    pub fn new(
        engine: Box<Engine>,
        incoming: Consumer<ToAudio>,
        outgoing: Producer<FromAudio>,
    ) -> Self {
        let sample_rate = engine.config().sample_rate;
        let max_parts = engine.config().max_parts;
        Self {
            engine,
            incoming,
            outgoing,
            timeline: Timeline::new(),
            current: Store::new(STORE_CAPACITY),
            next: Store::new(STORE_CAPACITY),
            seq: 0,
            resync: true,
            parts: Vec::with_capacity(max_parts),
            status: Status::default(),
            sample_rate,
            late_limit: (LATE_LIMIT_SECONDS * sample_rate).round() as i64,
            blocks: 0,
        }
    }

    /// The engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The counters so far.
    pub fn status(&self) -> Status {
        self.status
    }

    /// The timeline's epoch.
    pub fn epoch(&self) -> u32 {
        self.timeline.epoch()
    }

    /// Renders `buffer`, two channels, for the block `transport` describes,
    /// and publishes the snapshot to `shared`. Real-time safe.
    pub fn process(
        &mut self,
        buffer: &mut AudioBuffer<f32>,
        transport: &Transport,
        shared: &Shared,
    ) {
        let frames = buffer.num_frames();
        self.receive();
        let block = self.timeline.block(transport, frames, self.sample_rate);
        self.schedule(&block);
        if buffer.num_channels() >= 2 {
            let (left, right) = buffer.channel_pair_mut(0, 1);
            self.engine.render_into(left, right);
        } else {
            buffer.clear();
        }
        self.hand_back();
        self.blocks += 1;
        self.publish(&block, transport, shared);
    }

    /// The frame events for the first frame of this block's timeline go
    /// to: the engine's output runs its master bus's look-ahead late, which
    /// the plugin reports to the host as its latency.
    fn base(&self) -> u64 {
        self.engine.frame() + self.engine.master_latency_frames() as u64
    }

    fn push(&mut self, record: EventRecord) {
        if self.engine.push(record) == Pushed::Full {
            self.status.overflow += 1;
        }
    }

    /// Takes in what the server sent, while there is room to send back.
    fn receive(&mut self) {
        while self.outgoing.free_len() >= 2 {
            let Some(item) = self.incoming.pop() else {
                break;
            };
            match item {
                ToAudio::Event {
                    beat,
                    epoch,
                    record,
                } => {
                    let current = self.timeline.epoch();
                    let store = if epoch == current {
                        &mut self.current
                    } else if epoch == current.wrapping_add(1) {
                        &mut self.next
                    } else {
                        self.status.stale += 1;
                        continue;
                    };
                    self.seq += 1;
                    let waiting = Waiting {
                        beat,
                        seq: self.seq,
                        record,
                    };
                    if !store.insert(waiting) {
                        self.status.overflow += 1;
                    }
                }
                ToAudio::Now(mut record) => {
                    record.frame = self.base();
                    self.status.immediate += 1;
                    self.push(record);
                }
                ToAudio::Command(mut command) => {
                    let status = self.apply(&mut command);
                    // There is room: checked before the pop.
                    let _ = self.outgoing.push(FromAudio::Done(command, status));
                }
            }
        }
    }

    fn apply(&mut self, command: &mut Command) -> i32 {
        let result = match command {
            Command::NewEngine(engine) => {
                core::mem::swap(&mut self.engine, engine);
                self.clear_stores();
                self.resync = true;
                Ok(())
            }
            Command::Attach(slot) => self.engine.attach_part(slot),
            Command::Detach(part, slot) => self.engine.detach_part(*part, slot),
            Command::Instrument(part, slot) => self.engine.swap_instrument(*part, slot),
            Command::Chain(part, chain, stages) => {
                self.engine.set_chain_from(*part, *chain, stages)
            }
            Command::Sfz(part, slot, _) => self.engine.give_sfz(*part, slot),
            Command::Panic => {
                self.all_notes_off(self.base());
                self.clear_stores();
                Ok(())
            }
        };
        match result {
            Ok(()) => ffi::status::OK,
            Err(error) => status_of(error),
        }
    }

    fn clear_stores(&mut self) {
        self.status.cleared += (self.current.clear() + self.next.clear()) as u64;
    }

    /// Lets every note of every part go at `frame`.
    fn all_notes_off(&mut self, frame: u64) {
        self.parts.clear();
        for part in self.engine.part_ids() {
            if self.parts.len() < self.parts.capacity() {
                self.parts.push(part);
            }
        }
        for index in 0..self.parts.len() {
            let part = self.parts[index];
            self.push(EventRecord::all_notes_off(frame, part));
        }
    }

    /// The next lap's events become this lap's.
    fn wrap(&mut self) {
        self.status.cleared += self.current.clear() as u64;
        core::mem::swap(&mut self.current, &mut self.next);
    }

    /// Pushes this block's events into the engine on their frames.
    fn schedule(&mut self, block: &Block) {
        let base = self.base();
        match block.edge {
            Edge::Jump => {
                self.all_notes_off(base);
                self.clear_stores();
            }
            Edge::Wrap => {
                self.all_notes_off(base);
                self.wrap();
            }
            Edge::None => {
                if block.stopped {
                    self.all_notes_off(base);
                }
            }
        }
        if block.playing {
            if self.resync || block.started || block.edge != Edge::None || block.tempo_changed {
                self.push(EventRecord::param(base, 0, MASTER_TEMPO, block.tempo));
            }
            if self.resync || block.started || block.edge != Edge::None {
                self.push(EventRecord::param(base, 0, MASTER_BEAT, block.beat));
            }
            self.resync = false;
        }
        if let Some(span) = block.first {
            self.play(span, block.frames_per_beat, base);
        }
        if let Some(span) = block.wrapped {
            let at = base + span.offset as u64;
            self.all_notes_off(at);
            self.wrap();
            self.push(EventRecord::param(at, 0, MASTER_BEAT, span.beat));
            self.play(span, block.frames_per_beat, base);
        }
    }

    /// Pushes the events of this lap that fall in `span`.
    fn play(&mut self, span: Span, frames_per_beat: f64, base: u64) {
        while let Some(waiting) = self.current.peek() {
            let Some(at) = span.frame_of(waiting.beat, frames_per_beat) else {
                break;
            };
            let mut record = waiting.record;
            self.current.pop();
            let start = span.offset as i64;
            let at = if at < start {
                if start - at > self.late_limit {
                    self.status.too_late += 1;
                    continue;
                }
                self.status.late += 1;
                start
            } else {
                at
            };
            record.frame = base + at as u64;
            self.push(record);
        }
    }

    /// Sends back the instruments parts have finished with.
    fn hand_back(&mut self) {
        while self.outgoing.free_len() >= 1 {
            match self.engine.take_finished_instrument() {
                Some(instrument) => {
                    let _ = self.outgoing.push(FromAudio::Finished(instrument));
                }
                None => break,
            }
        }
    }

    fn publish(&mut self, block: &Block, transport: &Transport, shared: &Shared) {
        let stats = self.engine.stats();
        self.status.engine = [
            stats.queued,
            stats.ignored,
            stats.dropped,
            stats.late,
            stats.applied,
            stats.unrouted,
            stats.displaced,
            stats.stalled,
            stats.recovered,
            stats.limited,
            stats.cancelled,
        ];
        self.status.limiter_db = self.engine.limiter_reduction_db();
        let (loop_start, loop_end) = block.loop_beats.unwrap_or((f64::NAN, f64::NAN));
        let (numerator, denominator) = transport.time_signature.unwrap_or((0, 0));
        let snapshot = Snapshot {
            transport: protocol::Transport {
                beat: block.beat,
                tempo: block.tempo,
                bar_start: transport.bar_start_beats.unwrap_or(f64::NAN),
                loop_start,
                loop_end,
                age: 0.0,
                block: self.blocks,
                epoch: block.epoch,
                numerator,
                denominator,
                playing: block.playing,
                recording: transport.recording,
                from_host: block.from_host,
            },
            status: self.status,
        };
        shared.publish(&snapshot);
    }
}
