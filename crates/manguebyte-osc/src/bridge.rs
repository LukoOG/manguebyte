//! A bridge from the network to the audio thread.
//!
//! Packets arrive on a network thread, at any moment. The audio thread
//! runs in bursts, one block at a time, and must not wait for anything. The
//! bridge keeps the two apart:
//!
//! ```text
//!   network thread                                  audio thread
//!   (may block and allocate)                   (does neither)
//!
//!   receive a packet
//!   check it, find its handlers
//!   BridgeWriter::push_packet -- wait-free queue --> BridgeReader::read
//!                                                    frame, &OscEvent
//!                             <-- wait-free queue -- emptied buffers
//! ```
//!
//! ```
//! use manguebyte_osc::{BridgeConfig, Dispatcher, OscMessage, OscTime, bridge};
//!
//! const CUTOFF: u32 = 0;
//! let mut dispatcher = Dispatcher::new();
//! dispatcher.add("/filter/cutoff", CUTOFF)?;
//! let (mut writer, mut reader) = bridge::channel(dispatcher, BridgeConfig::default());
//!
//! // On a network thread. `BridgeThread::udp` does this for a socket.
//! let now = OscTime::now();
//! let packet = OscMessage::new("/filter/cutoff").with(1_200.0f32).to_bytes()?;
//! writer.push_packet(&packet, now)?;
//!
//! // On the audio thread, once per block.
//! let mut cutoff = 0.0;
//! reader.read_at(now, 512, 48_000.0, |_frame, event| {
//!     if event.handler() == CUTOFF {
//!         cutoff = event.args().next().and_then(|arg| arg.as_f32()).unwrap_or(cutoff);
//!     }
//! });
//! assert_eq!(cutoff, 1_200.0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # When an event happens
//!
//! Every event has a moment at which it is due:
//!
//! - A message in a bundle with a time tag is due at the time tag.
//! - A message outside a bundle, or in a bundle marked
//!   [`OscTime::IMMEDIATELY`], is due when its packet arrived, plus
//!   [`BridgeConfig::latency`].
//!
//! The reader is told the moment the first frame of the block stands for.
//! An event that is due inside the block is handed over with the frame it
//! falls on, to the nearest frame; one exactly half way between two frames
//! goes to the later. One due in a later block waits, on the audio thread,
//! in storage allocated up front. One that is already overdue, which means
//! nearer to a frame before the block than to the block's first, is handed
//! over on frame 0; if it carried a time tag it is counted as late.
//!
//! A bundle inside a bundle uses its own time tag, unless that is earlier
//! than the time of the bundle around it, which the specification forbids:
//! then it takes the outer time. A time tag more than
//! [`BridgeConfig::max_future`] ahead of the arrival is not honoured: the
//! bundle is treated as if it were marked immediately, and counted as out
//! of range. This also covers senders that write a small number or zero
//! where a time belongs.
//!
//! Events come out ordered by the moment they are due, and events due at
//! the same moment come out in the order they went in.
//!
//! Times are compared on the timeline described on [`OscTime`], so
//! scheduling keeps working across the 2036 rollover.
//!
//! # A packet is taken whole or not at all
//!
//! The specification says that the messages of one bundle take effect
//! together, as if nothing happened in between. The bridge keeps to that
//! in three places.
//!
//! - **Going in.** [`BridgeWriter::push_packet`] first works out every
//!   event a packet makes, and only then puts them on the queue. If they
//!   do not all fit, because the queue is short of room or a message needs
//!   a pool buffer that is not there, none of them is queued and all of
//!   them are counted as dropped.
//! - **Crossing.** The events of a packet become visible to the audio
//!   thread in one step. A block that reads while the network thread is in
//!   the middle of a packet sees none of that packet, and the next block
//!   sees all of it, so events that are due together are never split
//!   between two blocks by the timing of the threads.
//! - **Waiting.** When the audio thread has no room left, what it discards
//!   is always a whole group: all the events of one packet that are due at
//!   the same moment. See [`BridgeConfig::pending_capacity`].
//!
//! # What an event can hold
//!
//! An event is a fixed-size value that carries the type tags and the
//! argument bytes of its message as they were sent. Up to
//! [`OscEvent::INLINE_LEN`] bytes of them are stored in the event itself,
//! which is enough for a dozen numbers. A message that needs more, usually
//! because it has a long string or a blob, borrows a buffer from a pool
//! that is allocated when the bridge is built. The audio thread hands the
//! buffer back through a second queue when it is done; nothing is freed
//! there. A packet with a message too large for a pool buffer, or one that
//! arrives while the buffers it needs are in use, is dropped and counted.
//!
//! # What a packet may cost
//!
//! A sender chooses the address patterns in its packets, and a pattern
//! with wildcards is tried against every address the receiver has. Without
//! a limit, one datagram full of patterns built for the purpose would keep
//! the network thread busy for seconds. Two settings bound it:
//! [`BridgeConfig::max_pattern_len`] refuses a packet outright if one of
//! its patterns is absurdly long, and [`BridgeConfig::match_budget`] is
//! the most matching work the patterns of one packet may need between
//! them. A packet over either limit is refused whole and counted in
//! [`BridgeCounts::refused_patterns`]. Plain addresses are looked up
//! directly and are not affected.
//!
//! # What a block may cost
//!
//! One call to the reader takes in the events that are on the queue when
//! it begins, which is at most [`BridgeConfig::queue_capacity`], however
//! fast the network side adds more while it runs; those wait for the next
//! block. It hands over at most [`BridgeConfig::pending_capacity`] events,
//! and discards at most the events it held when it began and the ones it
//! took in. Making room for a group can take events out and put them back
//! when that turns out not to be enough, but fewer than the group has.
//! Each of these steps costs a number of comparisons that grows with the
//! logarithm of `pending_capacity`, so the work of one block is bounded by
//! the two capacities and by nothing a sender controls.
//!
//! Nothing is lost silently: every way a message can fail to reach the
//! audio thread has a counter in [`BridgeCounts`].

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use manguebyte_core::spsc::{self, Consumer, Producer};

use crate::agenda::{Agenda, Scheduled};
use crate::decode::{DecodeLimits, OscArgs, OscMessageRef, OscPacketRef};
use crate::dispatch::Dispatcher;
use crate::error::{DecodeError, OscError};
use crate::pattern;
use crate::tcp::TcpReceiver;
use crate::time::{OscTime, TICKS_PER_SECOND, duration_to_ticks};
use crate::udp::UdpReceiver;

/// How often a receiving thread stops waiting for the network to see
/// whether it has been asked to stop or given a new table of addresses.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Settings of a bridge.
///
/// All memory the bridge will ever use is allocated by [`channel`] from
/// these numbers.
///
/// ```
/// use std::time::Duration;
/// use manguebyte_osc::BridgeConfig;
///
/// // Keep the relative timing of messages without time tags, at the price
/// // of one 512-frame block of delay at 48 kHz.
/// let config = BridgeConfig::default().with_latency(Duration::from_secs_f64(512.0 / 48_000.0));
/// assert_eq!(config.queue_capacity, 1_024);
/// ```
///
/// More settings may be added later, so a value is made from
/// [`BridgeConfig::default`] and changed with the `with_` methods or by
/// assigning to its fields, never written out field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BridgeConfig {
    /// How many events can be on their way to the audio thread at once,
    /// which is how many may arrive between two blocks. Rounded up to a
    /// power of two. Default: 1,024.
    ///
    /// A packet is queued whole or not at all, so this is also the most
    /// events one packet can make, and the most the audio thread takes in
    /// during one block. The network side keeps one event's worth of
    /// memory for each, about 120 bytes, to put a packet together in.
    pub queue_capacity: usize,
    /// How many events the audio thread can hold: those of the block being
    /// rendered and those waiting for a later one. At least 1. Default:
    /// 1,024.
    ///
    /// When it is full, an event is never discarded in favour of one that
    /// is due later. New events that are due sooner than the last ones
    /// held push those out, one whole group at a time, a group being the
    /// events of one packet that are due at the same moment, until there
    /// is room for all of them. If pushing out every group that is due
    /// later than they are would still not make room, nothing is pushed
    /// out and the new events are discarded, as a whole group; so are new
    /// events that are due no sooner than everything held. Either way the
    /// loss is counted in [`BridgeCounts::dropped_pending_full`]. A sender
    /// that fills this store with events for the far future therefore
    /// cannot shut out events that are due now.
    pub pending_capacity: usize,
    /// How many buffers the pool has for messages too large to store in an
    /// event. Zero means such messages are always dropped. Default: 16.
    pub pool_buffers: usize,
    /// The size of each pool buffer in bytes, which is the largest sum of
    /// type tags and argument bytes a message may have. Default: 1,024.
    pub pool_buffer_len: usize,
    /// How long after its arrival a message without a time tag is due,
    /// to the nearest unit of a time tag.
    ///
    /// Zero, the default, puts every such message on frame 0 of the next
    /// block: the least delay, and timing no finer than a block. The
    /// duration of one block puts each message where it fell inside the
    /// block before, as `MidiReceiver` in `manguebyte-devices` does for
    /// MIDI: every message is late by the same amount, so the distances
    /// between them are kept.
    pub latency: Duration,
    /// How far ahead of its arrival a time tag may be. Default: 60 seconds.
    pub max_future: Duration,
    /// How deeply a packet may be nested.
    pub limits: DecodeLimits,
    /// The longest address pattern with wildcards that is looked at, in
    /// bytes. A packet that holds a longer one is refused whole. Default:
    /// 1,024, four times the longest address that can be registered.
    ///
    /// An address without wildcards is looked up directly whatever its
    /// length.
    pub max_pattern_len: usize,
    /// The most work that matching the address patterns of one packet may
    /// take. A packet that needs more is refused whole. Default:
    /// 1,000,000.
    ///
    /// The unit is the one of
    /// [`Dispatcher::dispatch_within`](crate::Dispatcher::dispatch_within):
    /// one place in one registered address looked at for one element of a
    /// pattern. A pattern as senders write them costs a few units for each
    /// registered address; [`Dispatcher::dispatch_within`] can be used to
    /// measure what a receiver's own traffic costs. A receiver with
    /// thousands of addresses that is sent bundles of hundreds of
    /// wildcard patterns may need more than the default, and
    /// [`BridgeCounts::refused_patterns`] says when. What this buys is a
    /// limit on how long one packet can keep the network side busy that
    /// does not depend on what the packet contains.
    pub match_budget: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1_024,
            pending_capacity: 1_024,
            pool_buffers: 16,
            pool_buffer_len: 1_024,
            latency: Duration::ZERO,
            max_future: Duration::from_secs(60),
            limits: DecodeLimits::default(),
            max_pattern_len: 1_024,
            match_budget: 1_000_000,
        }
    }
}

impl BridgeConfig {
    /// The same settings with [`queue_capacity`](Self::queue_capacity)
    /// changed.
    #[must_use]
    pub fn with_queue_capacity(mut self, events: usize) -> Self {
        self.queue_capacity = events;
        self
    }

    /// The same settings with [`pending_capacity`](Self::pending_capacity)
    /// changed.
    #[must_use]
    pub fn with_pending_capacity(mut self, events: usize) -> Self {
        self.pending_capacity = events;
        self
    }

    /// The same settings with [`pool_buffers`](Self::pool_buffers) changed.
    #[must_use]
    pub fn with_pool_buffers(mut self, buffers: usize) -> Self {
        self.pool_buffers = buffers;
        self
    }

    /// The same settings with [`pool_buffer_len`](Self::pool_buffer_len)
    /// changed.
    #[must_use]
    pub fn with_pool_buffer_len(mut self, bytes: usize) -> Self {
        self.pool_buffer_len = bytes;
        self
    }

    /// The same settings with [`latency`](Self::latency) changed.
    #[must_use]
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// The same settings with [`max_future`](Self::max_future) changed.
    #[must_use]
    pub fn with_max_future(mut self, max_future: Duration) -> Self {
        self.max_future = max_future;
        self
    }

    /// The same settings with [`limits`](Self::limits) changed.
    #[must_use]
    pub fn with_limits(mut self, limits: DecodeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The same settings with [`max_pattern_len`](Self::max_pattern_len)
    /// changed.
    #[must_use]
    pub fn with_max_pattern_len(mut self, bytes: usize) -> Self {
        self.max_pattern_len = bytes;
        self
    }

    /// The same settings with [`match_budget`](Self::match_budget) changed.
    #[must_use]
    pub fn with_match_budget(mut self, units: u64) -> Self {
        self.match_budget = units;
        self
    }
}

/// A duration in the units of a time tag, to the nearest unit. Durations
/// beyond 34 years are cut to that, which keeps the result positive.
fn duration_ticks(duration: Duration) -> i64 {
    duration_to_ticks(duration.min(Duration::from_secs(1 << 30))) as i64
}

#[derive(Debug, Default)]
struct Counters {
    packets: AtomicU64,
    malformed: AtomicU64,
    events: AtomicU64,
    unmatched: AtomicU64,
    out_of_range: AtomicU64,
    late: AtomicU64,
    refused_patterns: AtomicU64,
    dropped_queue_full: AtomicU64,
    dropped_too_large: AtomicU64,
    dropped_pool_empty: AtomicU64,
    dropped_pending_full: AtomicU64,
    io_errors: AtomicU64,
}

fn count(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// What a bridge has done so far, as read by [`BridgeStats::snapshot`].
///
/// The counters only grow. `late` and `dropped_pending_full` are written
/// by the audio thread, the others by the network side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BridgeCounts {
    /// Packets handed to [`BridgeWriter::push_packet`].
    pub packets: u64,
    /// Packets that were refused because they are not valid OSC, and
    /// damaged frames on a stream.
    pub malformed: u64,
    /// Events put on the queue to the audio thread. One message makes one
    /// event for every handler it reaches.
    pub events: u64,
    /// Messages whose address pattern matched no registered address.
    pub unmatched: u64,
    /// Bundles whose time tag was more than [`BridgeConfig::max_future`]
    /// ahead and was therefore not honoured.
    pub out_of_range: u64,
    /// Events with a time tag that had already passed when their block was
    /// rendered, by more than half a frame, and were handed over on frame
    /// 0.
    pub late: u64,
    /// Packets refused whole because of the cost of their address
    /// patterns: one was longer than [`BridgeConfig::max_pattern_len`], or
    /// matching them needed more than [`BridgeConfig::match_budget`]. How
    /// many events such a packet would have made is not known, so this
    /// counts packets. Nothing else about such a packet is counted: its
    /// messages are not in `unmatched` and its bundles not in
    /// `out_of_range`.
    pub refused_patterns: u64,
    /// Events not queued because the queue to the audio thread did not
    /// have room for all the events of their packet: the audio thread is
    /// not reading, [`BridgeConfig::queue_capacity`] is too small, or one
    /// packet makes more events than the queue holds.
    pub dropped_queue_full: u64,
    /// Events not queued because a message of their packet needs more room
    /// than [`BridgeConfig::pool_buffer_len`].
    pub dropped_too_large: u64,
    /// Events not queued because a message of their packet needs a pool
    /// buffer and all of them were in use.
    pub dropped_pool_empty: u64,
    /// Events discarded on the audio thread because it was already holding
    /// [`BridgeConfig::pending_capacity`] of them.
    pub dropped_pending_full: u64,
    /// Failures of the socket seen by a [`BridgeThread`], other than
    /// timeouts.
    pub io_errors: u64,
}

impl BridgeCounts {
    /// Events that were dropped, for any reason.
    ///
    /// A packet is dropped whole, so when one message of it cannot go, all
    /// the events of the packet are counted, under the reason that was met
    /// first. Packets counted in
    /// [`refused_patterns`](Self::refused_patterns) are not included.
    pub fn dropped(&self) -> u64 {
        self.dropped_queue_full
            + self.dropped_too_large
            + self.dropped_pool_empty
            + self.dropped_pending_full
    }
}

/// A handle for reading a bridge's counters from any thread.
#[derive(Debug, Clone)]
pub struct BridgeStats {
    counters: Arc<Counters>,
}

impl BridgeStats {
    /// The counters as they are now. Reads atomics and nothing else.
    pub fn snapshot(&self) -> BridgeCounts {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counters = &*self.counters;
        BridgeCounts {
            packets: read(&counters.packets),
            malformed: read(&counters.malformed),
            events: read(&counters.events),
            unmatched: read(&counters.unmatched),
            out_of_range: read(&counters.out_of_range),
            late: read(&counters.late),
            refused_patterns: read(&counters.refused_patterns),
            dropped_queue_full: read(&counters.dropped_queue_full),
            dropped_too_large: read(&counters.dropped_too_large),
            dropped_pool_empty: read(&counters.dropped_pool_empty),
            dropped_pending_full: read(&counters.dropped_pending_full),
            io_errors: read(&counters.io_errors),
        }
    }
}

/// An event as it crosses the queue. It is plain data, so that all the
/// events of a packet can be put on the queue in one step; a pool buffer,
/// which is owned memory, crosses on a second queue beside it.
#[derive(Clone, Copy)]
struct Wire {
    handler: u32,
    /// Whether the event came with a time tag that was honoured.
    timed: bool,
    /// Whether the type tags and the argument bytes are in a pool buffer
    /// instead of `inline`.
    pooled: bool,
    /// On the network side, while a packet is put together: which of the
    /// packet's pool buffers this event uses.
    slot: u32,
    /// How many more events of the same group follow this one on the
    /// queue. A group is the events of one packet that are due at the same
    /// moment.
    rest: u32,
    /// When the event is due, on the timeline of `OscTime::ticks`.
    due: i64,
    /// Position in the order events were made; settles ties between events
    /// due at the same moment.
    sequence: u64,
    /// The sequence number of the first event of the packet this one came
    /// in. With `due`, it names the group.
    packet: u64,
    tags_len: usize,
    data_len: usize,
    /// The type tags, then the argument bytes, unless a pool buffer holds
    /// them.
    inline: [u8; OscEvent::INLINE_LEN],
}

/// One message for one handler, as the audio thread receives it.
///
/// See the [module documentation](self) for what an event can hold.
pub struct OscEvent {
    wire: Wire,
    /// A pool buffer holding the type tags, then the argument bytes.
    buffer: Option<Box<[u8]>>,
}

impl OscEvent {
    /// How many bytes of type tags and argument data fit in an event
    /// without a pool buffer.
    pub const INLINE_LEN: usize = 64;

    /// The number the matching address was registered with.
    pub fn handler(&self) -> u32 {
        self.wire.handler
    }

    /// The type tags of the message, one character per argument.
    pub fn type_tags(&self) -> &str {
        let (tags, _) = self.parts();
        core::str::from_utf8(tags).unwrap_or("")
    }

    /// The arguments of the message. Decoding them does not allocate.
    pub fn args(&self) -> OscArgs<'_> {
        let (_, data) = self.parts();
        OscArgs::new(self.type_tags(), data, 0)
    }

    /// The time tag of the bundle the message was in, if it had one that
    /// was honoured.
    pub fn time_tag(&self) -> Option<OscTime> {
        self.wire
            .timed
            .then(|| OscTime::from_bits(self.wire.due as u64))
    }

    /// Whether the message was too large to store in the event and is held
    /// in a pool buffer.
    pub fn is_pooled(&self) -> bool {
        self.buffer.is_some()
    }

    /// The stored type tags and argument bytes.
    fn parts(&self) -> (&[u8], &[u8]) {
        let storage = match &self.buffer {
            Some(buffer) => &**buffer,
            None => &self.wire.inline,
        };
        let tags_len = self.wire.tags_len;
        let tags = storage.get(..tags_len).unwrap_or(&[]);
        let data = storage
            .get(tags_len..tags_len.saturating_add(self.wire.data_len))
            .unwrap_or(&[]);
        (tags, data)
    }
}

impl Scheduled for OscEvent {
    type Key = (i64, u64);

    /// Earliest first, and in the order they were made among those due at
    /// the same moment.
    fn key(&self) -> Self::Key {
        (self.wire.due, self.wire.sequence)
    }
}

impl fmt::Debug for OscEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Args<'a>(OscArgs<'a>);
        impl fmt::Debug for Args<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_list().entries(self.0.clone()).finish()
            }
        }
        f.debug_struct("OscEvent")
            .field("handler", &self.wire.handler)
            .field("time_tag", &self.time_tag())
            .field("args", &Args(self.args()))
            .finish()
    }
}

/// Builds a bridge: the half that takes packets in and the half the audio
/// thread reads.
///
/// `dispatcher` says which addresses are listened to and what number each
/// one is reported with. Allocates everything the bridge will use.
///
/// See the [module documentation](self) for an example.
pub fn channel(dispatcher: Dispatcher, config: BridgeConfig) -> (BridgeWriter, BridgeReader) {
    let (queue_in, queue_out) = spsc::channel(config.queue_capacity.max(1));
    let queue_capacity = queue_in.capacity();
    let pending_capacity = config.pending_capacity.max(1);
    // Each of these has room for every buffer at once, so that passing one
    // on cannot fail.
    let (carried_in, carried_out) = spsc::channel(config.pool_buffers.max(1));
    let (returns_in, returns_out) = spsc::channel(config.pool_buffers.max(1));
    let counters = Arc::new(Counters::default());

    let mut stock = Vec::with_capacity(config.pool_buffers);
    stock.resize_with(config.pool_buffers, || {
        vec![0u8; config.pool_buffer_len].into_boxed_slice()
    });

    let writer = BridgeWriter {
        dispatcher,
        outbox: Outbox {
            staged: Vec::with_capacity(queue_capacity),
            staged_buffers: Vec::with_capacity(config.pool_buffers),
            queue: queue_in,
            carried: carried_in,
            returns: returns_out,
            stock,
            buffer_len: config.pool_buffer_len,
            sequence: 0,
            room: 0,
            refusal: None,
            found: 0,
            unmatched: 0,
            out_of_range: 0,
            counters: Arc::clone(&counters),
        },
        limits: config.limits,
        latency: duration_ticks(config.latency),
        max_future: duration_ticks(config.max_future),
        max_pattern_len: config.max_pattern_len,
        match_budget: config.match_budget,
    };
    let reader = BridgeReader {
        queue: queue_out,
        carried: carried_out,
        returns: returns_in,
        agenda: Agenda::with_capacity(pending_capacity),
        set_aside: Vec::with_capacity(pending_capacity.min(queue_capacity)),
        counters,
    };
    (writer, reader)
}

/// When an event is due and whether a time tag said so.
#[derive(Debug, Clone, Copy)]
struct Schedule {
    due: i64,
    timed: bool,
}

/// Why the events of a packet cannot be queued.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    TooLarge,
    PoolEmpty,
    QueueFull,
}

/// A packet's address patterns cost more than the bridge allows.
struct PatternsRefused;

impl From<pattern::BudgetExceeded> for PatternsRefused {
    fn from(_: pattern::BudgetExceeded) -> Self {
        Self
    }
}

/// The network side's end of the queues, the pool, and the place where a
/// packet's events are put together before they are queued.
struct Outbox {
    queue: Producer<Wire>,
    /// Pool buffers on their way to the audio thread, in the order of the
    /// events that use them.
    carried: Producer<Box<[u8]>>,
    /// Buffers the audio thread has finished with.
    returns: Consumer<Box<[u8]>>,
    /// Buffers ready for use. Never holds more than it was built with, so
    /// pushing to it does not allocate.
    stock: Vec<Box<[u8]>>,
    buffer_len: usize,
    /// The sequence number the next event queued will have.
    sequence: u64,
    /// The events of the packet being taken in, held here until it is
    /// known that all of them can go. Built with room for as many as the
    /// queue holds.
    staged: Vec<Wire>,
    /// The pool buffers those events use, each at the position its event
    /// names in `slot`. Built with room for the whole pool.
    staged_buffers: Vec<Option<Box<[u8]>>>,
    /// How many events the queue had room for when the packet began. The
    /// audio thread can only have made more room since.
    room: usize,
    /// Why the packet cannot go, once that is known.
    refusal: Option<Refusal>,
    /// How many events the packet has made so far, staged or not.
    found: u64,
    /// Messages of the packet that reached no handler, and bundles whose
    /// time tag was not honoured. Added to the counters only if the packet
    /// is not refused for the cost of its patterns, which stops reading it
    /// part way.
    unmatched: u64,
    out_of_range: u64,
    counters: Arc<Counters>,
}

impl Outbox {
    /// Gets ready for the events of a new packet.
    fn begin(&mut self) {
        self.release_staged();
        // What has come back so far, and no more, so that this ends even
        // while the audio thread keeps returning buffers.
        for _ in 0..self.returns.len() {
            if let Some(buffer) = self.returns.pop() {
                self.stock.push(buffer);
            }
        }
        self.room = self.queue.free_len().min(self.staged.capacity());
        self.refusal = None;
        self.found = 0;
        self.unmatched = 0;
        self.out_of_range = 0;
    }

    /// Adds what was noted while reading the packet to the counters.
    fn count_what_was_read(&self) {
        let counters = &self.counters;
        counters
            .unmatched
            .fetch_add(self.unmatched, Ordering::Relaxed);
        counters
            .out_of_range
            .fetch_add(self.out_of_range, Ordering::Relaxed);
    }

    /// Makes the event for one handler of one message, if the packet can
    /// still go. Nothing reaches the queue here.
    fn stage(&mut self, handler: u32, message: &OscMessageRef<'_>, schedule: Schedule) {
        self.found += 1;
        if self.refusal.is_some() {
            // The packet is lost already; its events are only counted.
            return;
        }
        let tags = message.type_tags().as_bytes();
        let data = message.arg_data();
        let total = tags.len() + data.len();
        let pooled = total > OscEvent::INLINE_LEN;
        self.refusal = if pooled && total > self.buffer_len {
            Some(Refusal::TooLarge)
        } else if pooled && self.stock.is_empty() {
            Some(Refusal::PoolEmpty)
        } else if self.staged.len() >= self.room {
            Some(Refusal::QueueFull)
        } else {
            None
        };
        if self.refusal.is_some() {
            return;
        }

        let mut wire = Wire {
            handler,
            timed: schedule.timed,
            pooled,
            slot: 0,
            rest: 0,
            due: schedule.due,
            sequence: self.sequence.wrapping_add(self.staged.len() as u64),
            packet: self.sequence,
            tags_len: tags.len(),
            data_len: data.len(),
            inline: [0; OscEvent::INLINE_LEN],
        };
        if !pooled {
            store(&mut wire.inline, tags, data);
        } else if let Some(mut buffer) = self.stock.pop() {
            store(&mut buffer, tags, data);
            wire.slot = self.staged_buffers.len() as u32;
            // No more buffers exist than this vector has room for.
            self.staged_buffers.push(Some(buffer));
        }
        // Fewer than `room`, which is no more than the capacity.
        self.staged.push(wire);
    }

    /// Queues the packet's events, all in one step, or none of them.
    /// Returns how many were queued.
    fn finish(&mut self) -> usize {
        if let Some(refusal) = self.refusal {
            self.release_staged();
            let counters = &self.counters;
            let counter = match refusal {
                Refusal::TooLarge => &counters.dropped_too_large,
                Refusal::PoolEmpty => &counters.dropped_pool_empty,
                Refusal::QueueFull => &counters.dropped_queue_full,
            };
            counter.fetch_add(self.found, Ordering::Relaxed);
            return 0;
        }
        if self.staged.is_empty() {
            return 0;
        }

        // In the order the audio thread will hand them over, so that the
        // events of a group lie together and each can say how many follow.
        // Sorting in place does not allocate.
        self.staged
            .sort_unstable_by_key(|wire| (wire.due, wire.sequence));
        let mut after: Option<(i64, u32)> = None;
        for wire in self.staged.iter_mut().rev() {
            wire.rest = match after {
                Some((due, rest)) if due == wire.due => rest.saturating_add(1),
                _ => 0,
            };
            after = Some((wire.due, wire.rest));
        }

        // The buffers go first, so that the audio thread finds each one
        // waiting when it meets the event that uses it.
        for wire in &self.staged {
            if !wire.pooled {
                continue;
            }
            let buffer = self
                .staged_buffers
                .get_mut(wire.slot as usize)
                .and_then(Option::take);
            if let Some(buffer) = buffer {
                if let Err(buffer) = self.carried.push(buffer) {
                    // Not reachable: that queue has room for every buffer.
                    self.stock.push(buffer);
                }
            }
        }
        self.staged_buffers.clear();

        // One store of the queue's index makes all of them visible. There
        // is room: it was there when the packet began.
        let queued = self.queue.push_slice(&self.staged);
        self.sequence = self.sequence.wrapping_add(self.staged.len() as u64);
        self.staged.clear();
        self.counters
            .events
            .fetch_add(queued as u64, Ordering::Relaxed);
        queued
    }

    /// Forgets the events staged so far and puts their buffers back in
    /// stock.
    fn release_staged(&mut self) {
        self.staged.clear();
        for buffer in self.staged_buffers.drain(..).flatten() {
            self.stock.push(buffer);
        }
    }
}

/// Copies type tags and argument bytes, one after the other, to the front
/// of `target`, which the caller has checked is long enough.
fn store(target: &mut [u8], tags: &[u8], data: &[u8]) {
    if let Some(slot) = target.get_mut(..tags.len()) {
        slot.copy_from_slice(tags);
    }
    if let Some(slot) = target.get_mut(tags.len()..tags.len() + data.len()) {
        slot.copy_from_slice(data);
    }
}

/// The half of a bridge that takes packets in. It lives on a network
/// thread, or wherever packets come from.
///
/// [`BridgeThread`] owns one and feeds it from a socket. Code that gets
/// packets some other way calls [`push_packet`](Self::push_packet) itself.
pub struct BridgeWriter {
    dispatcher: Dispatcher,
    outbox: Outbox,
    limits: DecodeLimits,
    latency: i64,
    max_future: i64,
    max_pattern_len: usize,
    match_budget: u64,
}

impl BridgeWriter {
    /// Checks a packet and queues an event for every handler each of its
    /// messages reaches. Returns how many events were queued.
    ///
    /// The packet's events are queued together or not at all, and become
    /// visible to the audio thread in one step: see the
    /// [module documentation](self). A packet that cannot go is counted in
    /// [`BridgeCounts`], and the return value is zero.
    ///
    /// `arrival` is the moment the packet was received, normally
    /// [`OscTime::now`]. It is when messages without a time tag are due,
    /// and what time tags are measured against.
    ///
    /// Does not block, and does not allocate: a packet is read in place and
    /// its events are built in storage the bridge already has. The time it
    /// takes is bounded by the length of the packet and by
    /// [`BridgeConfig::match_budget`].
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] if the packet is not valid OSC. Nothing is queued
    /// in that case, and the packet is counted as malformed.
    pub fn push_packet(&mut self, bytes: &[u8], arrival: OscTime) -> Result<usize, DecodeError> {
        count(&self.outbox.counters.packets);
        let packet = match OscPacketRef::parse_with(bytes, &self.limits) {
            Ok(packet) => packet,
            Err(error) => {
                count(&self.outbox.counters.malformed);
                return Err(error);
            }
        };
        let now = arrival.ticks();
        let untimed = Schedule {
            due: now.saturating_add(self.latency),
            timed: false,
        };
        self.outbox.begin();
        let mut work = self.match_budget;
        match self.stage(&packet, untimed, now, &mut work) {
            Ok(()) => {
                self.outbox.count_what_was_read();
                Ok(self.outbox.finish())
            }
            Err(PatternsRefused) => {
                self.outbox.release_staged();
                count(&self.outbox.counters.refused_patterns);
                Ok(0)
            }
        }
    }

    /// The table of addresses this bridge listens to.
    pub fn dispatcher(&self) -> &Dispatcher {
        &self.dispatcher
    }

    /// Replaces the table of addresses, and returns the one that was in
    /// use. Packets pushed from now on are matched against the new table.
    ///
    /// Events already on their way keep the handler numbers they were
    /// given, so a number should not be given a new meaning while events
    /// for its old one may still be waiting. A [`Dispatcher`] can be
    /// cloned, changed and put back:
    ///
    /// ```
    /// use manguebyte_osc::{BridgeConfig, Dispatcher, bridge};
    ///
    /// let mut dispatcher = Dispatcher::new();
    /// dispatcher.add("/filter/cutoff", 0)?;
    /// let (mut writer, _reader) = bridge::channel(dispatcher, BridgeConfig::default());
    ///
    /// let mut table = writer.dispatcher().clone();
    /// table.add("/filter/resonance", 1)?;
    /// writer.set_dispatcher(table);
    /// assert_eq!(writer.dispatcher().len(), 2);
    /// # Ok::<(), manguebyte_osc::AddressError>(())
    /// ```
    ///
    /// For a bridge that a [`BridgeThread`] is feeding, use
    /// [`BridgeThread::set_dispatcher`].
    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) -> Dispatcher {
        core::mem::replace(&mut self.dispatcher, dispatcher)
    }

    /// A handle for reading the bridge's counters.
    pub fn stats(&self) -> BridgeStats {
        BridgeStats {
            counters: Arc::clone(&self.outbox.counters),
        }
    }

    /// Stages the events of a packet, without queueing any.
    fn stage(
        &mut self,
        packet: &OscPacketRef<'_>,
        schedule: Schedule,
        now: i64,
        work: &mut u64,
    ) -> Result<(), PatternsRefused> {
        match packet {
            OscPacketRef::Message(message) => {
                let address = message.address();
                if address.len() > self.max_pattern_len && pattern::has_wildcards(address) {
                    return Err(PatternsRefused);
                }
                let outbox = &mut self.outbox;
                let reached = self.dispatcher.dispatch_within(address, work, |handler| {
                    outbox.stage(handler, message, schedule);
                })?;
                if reached == 0 {
                    outbox.unmatched += 1;
                }
            }
            OscPacketRef::Bundle(bundle) => {
                // The depth of this recursion was bounded when the packet
                // was checked.
                let schedule = self.schedule(bundle.time(), schedule, now);
                for element in bundle.elements() {
                    self.stage(&element, schedule, now, work)?;
                }
            }
        }
        Ok(())
    }

    /// When the contents of a bundle with this time tag are due, given the
    /// schedule of whatever encloses the bundle.
    fn schedule(&mut self, time: OscTime, outer: Schedule, now: i64) -> Schedule {
        if time.is_immediate() {
            return outer;
        }
        let due = time.ticks();
        if due.saturating_sub(now) > self.max_future {
            self.outbox.out_of_range += 1;
            return outer;
        }
        if outer.timed && due < outer.due {
            return outer;
        }
        Schedule { due, timed: true }
    }
}

impl fmt::Debug for BridgeWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeWriter")
            .field("addresses", &self.dispatcher.len())
            .finish_non_exhaustive()
    }
}

/// The half of a bridge that the audio thread reads.
///
/// See the [module documentation](self) for how events are timed and for
/// what one call may cost.
pub struct BridgeReader {
    queue: Consumer<Wire>,
    /// Pool buffers arriving, in the order of the events that use them.
    carried: Consumer<Box<[u8]>>,
    /// Where pool buffers go back to the network side.
    returns: Producer<Box<[u8]>>,
    /// Events not yet handed over. Allocated once.
    agenda: Agenda<OscEvent>,
    /// Events taken out of the agenda to make room, while it is not yet
    /// known whether that will make enough. Allocated once, with room for
    /// as many as the queue or the agenda holds, whichever is fewer: see
    /// `make_room` for why that is enough.
    set_aside: Vec<OscEvent>,
    counters: Arc<Counters>,
}

impl BridgeReader {
    /// Hands over every event that is due in the coming block of `frames`
    /// frames, each with the frame it falls on. Call it once per block.
    ///
    /// The block is taken to begin now, by the system clock. That is
    /// simple, and as steady as the audio callbacks are. For timing that
    /// does not depend on when the callback happens to run, use
    /// [`read_stream`](Self::read_stream) with the position in the stream.
    ///
    /// Real-time safe: it reads wait-free queues and the clock, does not
    /// allocate, free, lock or block, and does a bounded amount of work.
    pub fn read(&mut self, frames: usize, sample_rate: f64, handle: impl FnMut(u32, &OscEvent)) {
        self.read_at(OscTime::now(), frames, sample_rate, handle);
    }

    /// [`read`](Self::read), with the moment the first frame of the block
    /// stands for passed in.
    ///
    /// An event due at time `t` is handed over on the frame nearest to
    /// `(t - block_start) * sample_rate`, a tie going to the later frame.
    /// If that is before frame 0 the event is overdue and gets frame 0; if
    /// it is `frames` or more the event stays for a later block.
    ///
    /// `block_start` must be on the same clock as the arrival times given
    /// to [`BridgeWriter::push_packet`] and as the senders' time tags.
    ///
    /// When each block is said to start where the one before it ended, an
    /// event lands on the same frame of the stream however the stream is
    /// cut into blocks, with one exception. A block start is a time tag,
    /// which cannot name most frames exactly: at 48 kHz it is off by up to
    /// six millionths of a frame. An event that close to the point half
    /// way between two frames can be put on either of them, depending on
    /// where the blocks begin. [`read_stream`](Self::read_stream) has no
    /// such exception.
    ///
    /// A block of zero frames takes events in but hands none over.
    ///
    /// Real-time safe, as [`read`](Self::read) is, and without reading the
    /// clock.
    pub fn read_at(
        &mut self,
        block_start: OscTime,
        frames: usize,
        sample_rate: f64,
        handle: impl FnMut(u32, &OscEvent),
    ) {
        self.hand_over(block_start, 0, frames, sample_rate, handle);
    }

    /// [`read`](Self::read) for a stream whose position is counted in
    /// frames: `origin` is the moment frame 0 of the stream stands for, and
    /// `position` is how many frames came before this block.
    ///
    /// An event due at time `t` belongs to the frame of the stream nearest
    /// to `(t - origin) * sample_rate`, a tie going to the later frame.
    /// That is worked out from `origin` alone, the same way in every
    /// block, so an event lands on the same frame of the stream however
    /// the stream is cut into blocks, without exception. It is handed over
    /// by the block that holds its frame, or on frame 0 of the first block
    /// read after its frame has passed.
    ///
    /// `origin` must be on the same clock as the arrival times given to
    /// [`BridgeWriter::push_packet`] and as the senders' time tags. If the
    /// stream stops and starts again, or the sound card's clock drifts
    /// away from that clock, choose a new origin and count from zero.
    ///
    /// ```
    /// use manguebyte_osc::{BridgeConfig, Dispatcher, OscBundle, OscMessage, OscTime, bridge};
    ///
    /// let mut dispatcher = Dispatcher::new();
    /// dispatcher.add("/note", 0)?;
    /// let (mut writer, mut reader) = bridge::channel(dispatcher, BridgeConfig::default());
    ///
    /// // The stream starts at `origin`. A note is sent for frame 1,000.
    /// let origin = OscTime::new(3_900_000_000, 0);
    /// let bundle = OscBundle::new(origin.add_frames(1_000, 48_000.0)).with(OscMessage::new("/note"));
    /// writer.push_packet(&bundle.to_bytes()?, origin)?;
    ///
    /// // It comes out in the fourth block of 256 frames, on frame 232.
    /// let mut landed = Vec::new();
    /// for block in 0..8u64 {
    ///     let position = block * 256;
    ///     reader.read_stream(origin, position, 256, 48_000.0, |frame, _event| {
    ///         landed.push(position + u64::from(frame));
    ///     });
    /// }
    /// assert_eq!(landed, [1_000]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Real-time safe, as [`read`](Self::read) is, and without reading the
    /// clock.
    pub fn read_stream(
        &mut self,
        origin: OscTime,
        position: u64,
        frames: usize,
        sample_rate: f64,
        handle: impl FnMut(u32, &OscEvent),
    ) {
        self.hand_over(origin, position, frames, sample_rate, handle);
    }

    /// How many events are held for a later block.
    pub fn pending(&self) -> usize {
        self.agenda.len()
    }

    /// Discards every event that has arrived and not been handed over, for
    /// use when playback stops or jumps. Real-time safe.
    pub fn clear(&mut self) {
        // What is on the queue now, and no more: see `take_in`.
        for _ in 0..self.queue.len() {
            let Some(wire) = self.queue.pop() else {
                break;
            };
            let mut event = self.receive(wire);
            self.give_back(&mut event);
        }
        while let Some(mut event) = self.agenda.pop_any() {
            self.give_back(&mut event);
        }
    }

    /// A handle for reading the bridge's counters.
    ///
    /// This shares ownership of the counters. Call it while setting up, not
    /// in the audio callback: dropping the last handle there would free
    /// memory.
    pub fn stats(&self) -> BridgeStats {
        BridgeStats {
            counters: Arc::clone(&self.counters),
        }
    }

    /// Takes in what has arrived, then hands over what is due in a block
    /// that begins `position` frames after `origin`.
    fn hand_over(
        &mut self,
        origin: OscTime,
        position: u64,
        frames: usize,
        sample_rate: f64,
        mut handle: impl FnMut(u32, &OscEvent),
    ) {
        self.take_in();
        if frames == 0 {
            return;
        }
        let origin = origin.ticks();
        let frames_per_tick = sample_rate.max(1.0) / TICKS_PER_SECOND;
        let position = position as f64;
        let block = frames as f64;
        while let Some(next) = self.agenda.first() {
            // The frame of the stream nearest to the moment the event is
            // due, half way going up. Adding a half and rounding down puts
            // the line between two frames in the same place whichever
            // block asks; rounding to the nearest would move it by a frame
            // for negative numbers.
            let since_origin = next.wire.due.saturating_sub(origin) as f64;
            let frame = (since_origin * frames_per_tick + 0.5).floor() - position;
            if frame >= block {
                break;
            }
            let Some(mut event) = self.agenda.pop_first() else {
                break;
            };
            let frame = if frame < 0.0 {
                if event.wire.timed {
                    count(&self.counters.late);
                }
                0
            } else {
                frame as u32
            };
            handle(frame, &event);
            self.give_back(&mut event);
        }
    }

    /// Moves what has arrived into the agenda.
    fn take_in(&mut self) {
        // What is on the queue now, and no more. The network side may add
        // events as fast as this loop takes them off; those are for the
        // next block. The events of a packet are queued in one step, so
        // this count never ends in the middle of one.
        let mut arrived = self.queue.len();
        while arrived > 0 {
            let Some(first) = self.queue.pop() else {
                break;
            };
            arrived -= 1;
            // The rest of its group follows it. All of them are kept, or
            // none.
            let rest = (first.rest as usize).min(arrived);
            arrived -= rest;
            let kept = self.make_room(rest + 1, first.due);
            self.settle(first, kept);
            for _ in 0..rest {
                let Some(wire) = self.queue.pop() else {
                    break;
                };
                self.settle(wire, kept);
            }
        }
    }

    /// Makes sure the agenda has room for a group of `events` events due
    /// at `due`, if it may: by discarding groups that are due later, last
    /// first, and only if discarding those makes enough room. Returns
    /// whether there is room.
    fn make_room(&mut self, events: usize, due: i64) -> bool {
        let capacity = self.agenda.capacity();
        if events > capacity {
            return false;
        }
        // An event taken out while there is still too little room is set
        // aside, to be put back if taking out everything due later turns
        // out not to be enough. Once there is room, they are discarded.
        //
        // Fewer than `events` are ever set aside. One is set aside only if,
        // with `held` events at the start and `taken` out so far, including
        // it, `held - taken + events > capacity`; and `held <= capacity`,
        // so `taken < events`. A group comes off the queue whole, so
        // `events` is no more than the queue holds, and it was checked
        // above to be no more than the agenda holds: `set_aside` was built
        // with room for that many.
        while self.agenda.len() + events > capacity {
            // Something has to give. It is what is due last, unless that
            // is no later than the newcomers: then they are the ones due
            // last, and they give.
            let Some((packet, last_due)) = self
                .agenda
                .last()
                .map(|event| (event.wire.packet, event.wire.due))
                .filter(|&(_, last_due)| last_due > due)
            else {
                // Even with everything due later taken out there is no
                // room, so that goes back.
                while let Some(event) = self.set_aside.pop() {
                    // Each of these was in the agenda a moment ago, so
                    // there is room for it.
                    if let Err(event) = self.agenda.push(event) {
                        self.discard(event);
                    }
                }
                return false;
            };
            // The whole group that event belongs to. Its members are next
            // to each other at the end of the order.
            while self
                .agenda
                .last()
                .is_some_and(|event| event.wire.packet == packet && event.wire.due == last_due)
            {
                let Some(event) = self.agenda.pop_last() else {
                    break;
                };
                let room = self.agenda.len() + events <= capacity;
                // `set_aside` cannot be full here (see above); if it were,
                // the event is discarded rather than the vector grown.
                if room || self.set_aside.len() == self.set_aside.capacity() {
                    self.discard(event);
                } else {
                    self.set_aside.push(event);
                }
            }
        }
        // There is room, so what was set aside goes for good.
        while let Some(event) = self.set_aside.pop() {
            self.discard(event);
        }
        true
    }

    /// Discards an event from the audio side for want of room, and counts
    /// it.
    fn discard(&mut self, mut event: OscEvent) {
        count(&self.counters.dropped_pending_full);
        self.give_back(&mut event);
    }

    /// Puts an event that has arrived into the agenda, or discards it.
    fn settle(&mut self, wire: Wire, kept: bool) {
        let event = self.receive(wire);
        // An event that should have a buffer and has none cannot be read.
        // That is not reachable: see `receive`.
        let whole = event.buffer.is_some() == wire.pooled;
        let discarded = if kept && whole {
            match self.agenda.push(event) {
                Ok(()) => return,
                Err(event) => event,
            }
        } else {
            event
        };
        self.discard(discarded);
    }

    /// An event taken off the queue, joined with its pool buffer if it has
    /// one. The network side queues the buffer before the event, so it is
    /// there.
    fn receive(&mut self, wire: Wire) -> OscEvent {
        let buffer = if wire.pooled {
            self.carried.pop()
        } else {
            None
        };
        OscEvent { wire, buffer }
    }

    /// Sends an event's pool buffer, if it has one, back to the network
    /// side, so that dropping the event afterwards frees nothing.
    fn give_back(&mut self, event: &mut OscEvent) {
        if let Some(buffer) = event.buffer.take() {
            if let Err(buffer) = self.returns.push(buffer) {
                // The return queue has room for every buffer that exists,
                // so this is not reachable. If it ever were, leaking the
                // buffer is the only thing that keeps the heap out of the
                // audio thread.
                core::mem::forget(buffer);
            }
        }
    }
}

impl fmt::Debug for BridgeReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeReader")
            .field("pending", &self.agenda.len())
            .finish_non_exhaustive()
    }
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// What a receiving thread is told from outside. The thread owns this.
struct Control {
    stop: Arc<AtomicBool>,
    /// New tables of addresses, to be put to use between two packets.
    tables: mpsc::Receiver<Dispatcher>,
}

impl Control {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Puts the newest table that has been sent to use.
    fn update(&self, writer: &mut BridgeWriter) {
        while let Ok(table) = self.tables.try_recv() {
            writer.set_dispatcher(table);
        }
    }
}

/// A thread that receives packets from a socket and feeds a
/// [`BridgeWriter`]. It stops when this value is dropped.
///
/// ```no_run
/// use manguebyte_osc::{BridgeConfig, BridgeThread, Dispatcher, UdpReceiver, bridge};
///
/// let mut dispatcher = Dispatcher::new();
/// dispatcher.add("/filter/cutoff", 0)?;
/// let (writer, mut reader) = bridge::channel(dispatcher.clone(), BridgeConfig::default());
/// let thread = BridgeThread::udp(UdpReceiver::bind("0.0.0.0:9000")?, writer)?;
///
/// // In the audio callback, once per block:
/// reader.read(512, 48_000.0, |frame, event| {
///     println!("frame {frame}: {event:?}");
/// });
///
/// // Later, from any thread that is not the audio thread: listen to one
/// // more address, without stopping anything.
/// dispatcher.add("/filter/resonance", 1)?;
/// thread.set_dispatcher(dispatcher);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct BridgeThread {
    stop: Arc<AtomicBool>,
    tables: mpsc::Sender<Dispatcher>,
    thread: Option<JoinHandle<()>>,
}

impl BridgeThread {
    /// Starts a thread that reads datagrams from `receiver`. Each one is
    /// stamped with the system clock's time as it arrives.
    ///
    /// The receiver's timeout is set, so that the thread can notice when it
    /// is asked to stop.
    pub fn udp(mut receiver: UdpReceiver, mut writer: BridgeWriter) -> io::Result<Self> {
        receiver.set_timeout(Some(POLL_INTERVAL))?;
        Self::spawn(move |control| {
            while !control.stopped() {
                control.update(&mut writer);
                match receiver.recv_bytes() {
                    Ok((bytes, _from)) => {
                        // A packet that is not valid is counted by the
                        // writer; there is nobody here to tell.
                        let _ = writer.push_packet(bytes, OscTime::now());
                    }
                    Err(error) if timed_out(&error) => {}
                    Err(_) => {
                        count(&writer.outbox.counters.io_errors);
                        // Whatever is wrong may stay wrong; do not spin.
                        thread::sleep(POLL_INTERVAL);
                    }
                }
            }
        })
    }

    /// Starts a thread that reads framed packets from one TCP connection.
    /// The thread ends when the sender closes the connection or the
    /// connection fails.
    ///
    /// The receiver's timeout is set, so that the thread can notice when it
    /// is asked to stop.
    pub fn tcp(mut receiver: TcpReceiver, mut writer: BridgeWriter) -> io::Result<Self> {
        receiver.set_timeout(Some(POLL_INTERVAL))?;
        Self::spawn(move |control| {
            while !control.stopped() {
                control.update(&mut writer);
                match receiver.recv_bytes() {
                    Ok(Some(bytes)) => {
                        let _ = writer.push_packet(bytes, OscTime::now());
                    }
                    Ok(None) => break,
                    Err(OscError::Io(error)) if timed_out(&error) => {}
                    Err(OscError::Io(_)) => {
                        count(&writer.outbox.counters.io_errors);
                        break;
                    }
                    // A damaged frame. The receiver has dropped it and
                    // carries on with the next.
                    Err(_) => count(&writer.outbox.counters.malformed),
                }
            }
        })
    }

    /// Whether the thread is still running. A TCP thread ends by itself
    /// when its connection does.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    /// Gives the thread a new table of addresses, to replace the one its
    /// bridge was built with. Nothing is stopped and nothing queued is
    /// lost. Returns `false` if the thread has ended.
    ///
    /// The thread puts the table to use before the next packet it reads,
    /// which for a thread that is waiting for the network is within about
    /// a twentieth of a second. Until then packets are matched against the
    /// old table. See [`BridgeWriter::set_dispatcher`] for what happens to
    /// events that are already on their way.
    ///
    /// This allocates and may wait briefly for a lock, so it is not for
    /// the audio thread.
    pub fn set_dispatcher(&self, dispatcher: Dispatcher) -> bool {
        self.tables.send(dispatcher).is_ok()
    }

    fn spawn(work: impl FnOnce(&Control) + Send + 'static) -> io::Result<Self> {
        let (tables, incoming) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let control = Control {
            stop: Arc::clone(&stop),
            tables: incoming,
        };
        let thread = thread::Builder::new()
            .name("manguebyte-osc".to_owned())
            .spawn(move || work(&control))?;
        Ok(Self {
            stop,
            tables,
            thread: Some(thread),
        })
    }
}

impl Drop for BridgeThread {
    /// Asks the thread to stop and waits for it.
    ///
    /// A thread that is waiting for the network notices when its wait
    /// times out, which is after about a twentieth of a second: the
    /// polling interval, plus whatever the granularity of the system's
    /// timer adds, which on Windows has been seen to be several
    /// milliseconds. A thread that is in the middle of a packet finishes
    /// the packet first; [`BridgeConfig::match_budget`] and the length of
    /// the packet bound how long that takes.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The thread only panics if a bug in this crate makes it; there
            // is nothing useful to do with that here.
            let _ = thread.join();
        }
    }
}
