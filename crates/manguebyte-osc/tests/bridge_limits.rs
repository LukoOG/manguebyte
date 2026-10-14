//! What the bridge does when it is asked for more than it has.
//!
//! - A packet is taken whole or not at all, on the way in, while crossing
//!   to the audio thread, and while waiting there.
//! - One block takes in no more than the queue holds, however fast the
//!   network side fills it.
//! - Events held for later make way for events that are due sooner.
//! - A packet whose address patterns are built to be expensive is refused,
//!   after a bounded amount of work.
//!
//! The tests whose names end in `_threaded` run a sender and a reader at
//! the same time; the rest pass their own times in and do not depend on
//! how fast they run, except for the ones that say they measure time.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use manguebyte_osc::bridge;
use manguebyte_osc::pattern::BudgetExceeded;
use manguebyte_osc::{
    BridgeConfig, BridgeReader, BridgeWriter, Dispatcher, OscBundle, OscMessage, OscPacket, OscTime,
};

const RATE: f64 = 48_000.0;
const BLOCK: usize = 480;
const NOTE: u32 = 1;

fn origin() -> OscTime {
    OscTime::new(3_900_000_000, 0)
}

fn setup(config: BridgeConfig) -> (BridgeWriter, BridgeReader) {
    let mut dispatcher = Dispatcher::new();
    dispatcher.add("/n", NOTE).unwrap();
    bridge::channel(dispatcher, config)
}

fn note(id: i32) -> OscMessage {
    OscMessage::new("/n").with(id)
}

/// A note too large to be stored in an event: with its 100-byte blob it
/// needs a pool buffer of at least 112 bytes.
fn large_note(id: i32) -> OscMessage {
    note(id).with(vec![0xABu8; 100])
}

fn bytes(packet: impl Into<OscPacket>) -> Vec<u8> {
    packet.into().to_bytes().unwrap()
}

/// A bundle for `time` holding `count` notes that all carry `id`.
fn bundle_of(time: OscTime, id: i32, count: usize) -> Vec<u8> {
    let mut bundle = OscBundle::new(time);
    for _ in 0..count {
        bundle = bundle.with(note(id));
    }
    bytes(bundle)
}

/// Reads one block and returns the number every event carries.
fn read_ids(reader: &mut BridgeReader, block_start: OscTime) -> Vec<i32> {
    let mut ids = Vec::new();
    reader.read_at(block_start, BLOCK, RATE, |_, event| {
        ids.push(event.args().next().and_then(|arg| arg.as_i32()).unwrap());
    });
    ids
}

/// A small deterministic generator (xorshift64*).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() >> 33) as usize % bound
    }
}

// ---------------------------------------------------------------------
// A packet is taken whole or not at all.
// ---------------------------------------------------------------------

/// OSC 1.0, "Temporal Semantics and OSC Time Tags": the messages of one
/// bundle are atomic, their methods invoked in immediate succession as if
/// nothing happened in between.
#[test]
fn a_bundle_that_does_not_fit_in_the_queue_is_not_applied_in_part() {
    let config = BridgeConfig::default().with_queue_capacity(4);
    let (mut writer, mut reader) = setup(config);
    let queued = writer
        .push_packet(&bundle_of(OscTime::IMMEDIATELY, 7, 6), origin())
        .unwrap();
    let seen = read_ids(&mut reader, origin()).len();
    assert_eq!(queued, seen);
    assert!(
        seen == 0 || seen == 6,
        "{seen} of the 6 messages of one bundle reached the audio thread; \
         a bundle is all or nothing"
    );
    // A queue of four can never take six, so it is none, and all six are
    // counted.
    assert_eq!(seen, 0);
    let counts = writer.stats().snapshot();
    assert_eq!((counts.events, counts.dropped_queue_full), (0, 6));

    // Room is counted for the whole packet: three are waiting, so a bundle
    // of two does not go, and one of one does.
    assert_eq!(
        writer.push_packet(&bundle_of(OscTime::IMMEDIATELY, 1, 3), origin()),
        Ok(3)
    );
    assert_eq!(
        writer.push_packet(&bundle_of(OscTime::IMMEDIATELY, 2, 2), origin()),
        Ok(0)
    );
    assert_eq!(
        writer.push_packet(&bundle_of(OscTime::IMMEDIATELY, 3, 1), origin()),
        Ok(1)
    );
    assert_eq!(read_ids(&mut reader, origin()), [1, 1, 1, 3]);
    assert_eq!(writer.stats().snapshot().dropped_queue_full, 8);
}

#[test]
fn a_packet_short_of_anything_else_is_refused_whole_as_well() {
    // Pool buffers: two exist, and the bundle needs three.
    let config = BridgeConfig::default()
        .with_pool_buffers(2)
        .with_pool_buffer_len(128);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let short_of_buffers = OscBundle::default()
        .with(note(1))
        .with(large_note(2))
        .with(large_note(3))
        .with(large_note(4))
        .with(note(5));
    assert_eq!(
        writer.push_packet(&bytes(short_of_buffers), origin()),
        Ok(0)
    );
    assert_eq!(read_ids(&mut reader, origin()), []);
    let counts = stats.snapshot();
    // All five events of the packet are counted, under the reason that was
    // met first.
    assert_eq!((counts.events, counts.dropped_pool_empty), (0, 5));
    assert_eq!(counts.dropped(), 5);

    // The two buffers it had taken before it failed are back in stock.
    let fits = OscBundle::default()
        .with(large_note(6))
        .with(note(7))
        .with(large_note(8));
    assert_eq!(writer.push_packet(&bytes(fits), origin()), Ok(3));
    let mut seen = Vec::new();
    reader.read_at(origin(), BLOCK, RATE, |_, event| {
        seen.push((
            event.args().next().unwrap().as_i32().unwrap(),
            event.is_pooled(),
        ));
    });
    assert_eq!(seen, [(6, true), (7, false), (8, true)]);

    // One message too large for any buffer takes its bundle with it.
    let too_large = OscBundle::default()
        .with(note(1))
        .with(note(2).with(vec![0u8; 200]))
        .with(large_note(3))
        .with(note(4));
    assert_eq!(writer.push_packet(&bytes(too_large), origin()), Ok(0));
    assert_eq!(read_ids(&mut reader, origin()), []);
    let counts = stats.snapshot();
    assert_eq!(
        (counts.dropped_too_large, counts.dropped_pool_empty),
        (4, 5)
    );
    // And the buffer the third message would have used was not lost.
    assert_eq!(
        writer.push_packet(
            &bytes(
                OscBundle::default()
                    .with(large_note(9))
                    .with(large_note(10))
            ),
            origin()
        ),
        Ok(2)
    );
    assert_eq!(read_ids(&mut reader, origin()), [9, 10]);

    // One message for several handlers is a packet like any other.
    let mut dispatcher = Dispatcher::new();
    for voice in 0..6 {
        dispatcher.add(&format!("/v/{voice}"), voice).unwrap();
    }
    let config = BridgeConfig::default().with_queue_capacity(4);
    let (mut writer, mut reader) = bridge::channel(dispatcher, config);
    assert_eq!(
        writer.push_packet(&bytes(OscMessage::new("/v/*").with(1)), origin()),
        Ok(0)
    );
    assert_eq!(writer.stats().snapshot().dropped_queue_full, 6);
    assert_eq!(
        writer.push_packet(&bytes(OscMessage::new("/v/[0-3]").with(2)), origin()),
        Ok(4)
    );
    let mut handlers = Vec::new();
    reader.read_at(origin(), BLOCK, RATE, |_, event| {
        handlers.push(event.handler())
    });
    assert_eq!(handlers, [0, 1, 2, 3]);
}

/// The same property under the conditions the bridge is built for: a network
/// thread pushing while the audio thread reads. Nothing is dropped in this
/// test; the queue never fills. A block that reads while the network thread
/// is half way through a bundle must see none of it, not half.
#[test]
fn the_messages_of_one_bundle_are_handed_over_in_the_same_block_threaded() {
    const PER_BUNDLE: usize = 64;
    const BUNDLES: usize = 4_000;
    let config = BridgeConfig::default()
        .with_queue_capacity(4_096)
        .with_pending_capacity(4_096);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let consumed = Arc::new(AtomicU64::new(0));
    let seen_by_reader = Arc::clone(&consumed);
    let arrival = origin();

    // A sender of 2,000 bundles a second, at its own pace.
    let network = std::thread::spawn(move || {
        let started = Instant::now();
        for id in 0..BUNDLES {
            let due = started + Duration::from_micros(500 * id as u64);
            while Instant::now() < due {
                std::hint::spin_loop();
            }
            // A safety net for a machine so loaded that the reader stalls:
            // never more than three quarters of the queue in flight, so
            // that nothing is ever dropped.
            let sent = (id * PER_BUNDLE) as u64;
            while sent - seen_by_reader.load(Ordering::Acquire) > 3_000 {
                std::thread::yield_now();
            }
            let packet = bundle_of(OscTime::IMMEDIATELY, id as i32, PER_BUNDLE);
            assert_eq!(writer.push_packet(&packet, arrival), Ok(PER_BUNDLE));
        }
    });

    // An audio callback about once a millisecond.
    let total = (BUNDLES * PER_BUNDLE) as u64;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut blocks_seen_in = vec![0u32; BUNDLES];
    let mut blocks = 0u32;
    while consumed.load(Ordering::Acquire) < total {
        assert!(Instant::now() < deadline, "the test did not finish");
        // Every event is due at `arrival`, so every block hands over all
        // that has come in.
        let ids = read_ids(&mut reader, arrival);
        consumed.fetch_add(ids.len() as u64, Ordering::AcqRel);
        blocks += 1;
        let mut previous = None;
        for id in ids {
            if previous != Some(id) {
                blocks_seen_in[id as usize] += 1;
                previous = Some(id);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    network.join().unwrap();
    assert_eq!(stats.snapshot().dropped(), 0);
    assert!(blocks_seen_in.iter().all(|&count| count >= 1));
    let split: Vec<usize> = (0..BUNDLES).filter(|&id| blocks_seen_in[id] > 1).collect();
    assert!(
        split.is_empty(),
        "{} of {BUNDLES} bundles were split between two blocks ({blocks} blocks were read); \
         the first few: {:?}",
        split.len(),
        &split[..split.len().min(8)]
    );
}

#[test]
fn a_group_that_does_not_fit_on_the_audio_side_is_dropped_whole() {
    let config = BridgeConfig::default().with_pending_capacity(4);
    let later = origin() + Duration::from_secs(1);
    let much_later = origin() + Duration::from_secs(2);

    // Six events for one moment, where four can be held.
    let (mut writer, mut reader) = setup(config.clone());
    assert_eq!(writer.push_packet(&bundle_of(later, 1, 6), origin()), Ok(6));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(reader.pending(), 0);
    assert_eq!(writer.stats().snapshot().dropped_pending_full, 6);
    assert_eq!(read_ids(&mut reader, later), []);

    // Three and three for the same moment: the first three stay, and the
    // second three go together, not one of them in and two out.
    let (mut writer, mut reader) = setup(config.clone());
    assert_eq!(writer.push_packet(&bundle_of(later, 1, 3), origin()), Ok(3));
    assert_eq!(writer.push_packet(&bundle_of(later, 2, 3), origin()), Ok(3));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(reader.pending(), 3);
    assert_eq!(writer.stats().snapshot().dropped_pending_full, 3);
    assert_eq!(read_ids(&mut reader, later), [1, 1, 1]);

    // A bundle inside a bundle, with a later time of its own, is a group
    // of its own: the outer two fit, the inner three do not.
    let (mut writer, mut reader) = setup(config);
    let mut inner = OscBundle::new(much_later);
    for _ in 0..3 {
        inner = inner.with(note(2));
    }
    let outer = OscBundle::new(later)
        .with(note(1))
        .with(inner)
        .with(note(1));
    assert_eq!(writer.push_packet(&bytes(outer), origin()), Ok(5));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(reader.pending(), 2);
    assert_eq!(writer.stats().snapshot().dropped_pending_full, 3);
    assert_eq!(read_ids(&mut reader, later), [1, 1]);
    assert_eq!(read_ids(&mut reader, much_later), []);
}

// ---------------------------------------------------------------------
// One block does a bounded amount of work.
// ---------------------------------------------------------------------

/// The audio side takes in what the queue holds when a block begins. If it
/// popped until it found the queue empty, a sender that refills the queue
/// as fast as it is emptied would decide how long the audio thread stays
/// in the call.
#[test]
fn one_block_takes_in_no_more_than_the_queue_holds_threaded() {
    const CAPACITY: u64 = 64;
    let config = BridgeConfig::default()
        .with_queue_capacity(CAPACITY as usize)
        .with_pending_capacity(CAPACITY as usize);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let stop = Arc::new(AtomicU64::new(0));
    let flag = Arc::clone(&stop);
    // A sender that does not pause, as a flood of datagrams would be. Each
    // bundle is small enough to fit in the queue whole.
    let network = std::thread::spawn(move || {
        let packet = bundle_of(OscTime::IMMEDIATELY, 1, 16);
        while flag.load(Ordering::Relaxed) == 0 {
            let _ = writer.push_packet(&packet, origin());
        }
    });

    let started = Instant::now();
    let mut most = 0i64;
    let mut blocks = 0u32;
    while started.elapsed() < Duration::from_secs(3) && most <= CAPACITY as i64 {
        let dropped_before = stats.snapshot().dropped_pending_full as i64;
        let held_before = reader.pending() as i64;
        let mut handed_over = 0i64;
        reader.read_at(origin(), BLOCK, RATE, |_, _| handed_over += 1);
        // What a block takes off the queue is handed over, kept for later,
        // or dropped for want of room.
        let kept = reader.pending() as i64 - held_before;
        let dropped = stats.snapshot().dropped_pending_full as i64 - dropped_before;
        most = most.max(handed_over + kept + dropped);
        blocks += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    let most = most as u64;
    stop.store(1, Ordering::Relaxed);
    network.join().unwrap();
    assert!(
        most <= CAPACITY,
        "one call to read_at went through {most} events with a queue of {CAPACITY} \
         (seen within {blocks} blocks)"
    );
    // The sender did get through: the bound above was not kept by nothing
    // arriving.
    assert!(stats.snapshot().events > 1_000);
}

// ---------------------------------------------------------------------
// Events held for later make way for events that are due sooner.
// ---------------------------------------------------------------------

/// One datagram of 16 kilobytes holds a bundle of 1,024 small messages.
/// Tagged 59 seconds ahead, inside the default `max_future`, they fill the
/// default `pending_capacity`. Events that are due now must still get
/// through.
#[test]
fn events_held_for_later_do_not_shut_out_events_that_are_due_now() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let now = origin();
    let later = now + Duration::from_secs(59);
    let flood = bundle_of(later, -1, 1_024);
    // Well inside what one UDP datagram carries (65,507 bytes).
    assert_eq!(flood.len(), 16 + 1_024 * 16);
    assert_eq!(writer.push_packet(&flood, now), Ok(1_024));
    assert_eq!(read_ids(&mut reader, now), []);
    assert_eq!(reader.pending(), 1_024);

    // Ordinary control traffic, one message per block for a second.
    let mut delivered = 0;
    for block in 0..100u32 {
        let block_start = now + Duration::from_millis(10 * u64::from(block));
        assert_eq!(writer.push_packet(&bytes(note(1)), block_start), Ok(1));
        delivered += read_ids(&mut reader, block_start).len();
    }
    let counts = writer.stats().snapshot();
    assert_eq!(
        delivered, 100,
        "{delivered} of 100 messages due at once were handed over; {} were dropped \
         because the audio side was full of events due 59 s later",
        counts.dropped_pending_full
    );
    // What made room was the bundle due last, and it went whole: none of
    // its messages is left to take effect without the others.
    assert_eq!(counts.dropped_pending_full, 1_024);
    assert_eq!(reader.pending(), 0);
    assert_eq!(read_ids(&mut reader, later), []);
}

#[test]
fn when_the_audio_side_is_full_the_events_due_last_are_the_ones_that_go() {
    let config = BridgeConfig::default().with_pending_capacity(4);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let now = origin();
    let at = |millis: u64| now + Duration::from_millis(millis);
    let mut push = |id: i32, millis: u64| {
        let packet = bytes(OscBundle::new(at(millis)).with(note(id)));
        assert_eq!(writer.push_packet(&packet, now), Ok(1));
    };

    // Four are held, due in 4, 3, 2 and 1 seconds.
    for (id, millis) in [(4, 4_000), (3, 3_000), (2, 2_000), (1, 1_000)] {
        push(id, millis);
    }
    assert_eq!(read_ids(&mut reader, now), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 0)
    );

    // One due in 2.5 seconds arrives: the one due in 4 makes room for it.
    push(25, 2_500);
    assert_eq!(read_ids(&mut reader, now), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 1)
    );

    // One due in 10 seconds arrives: everything held is due sooner, so it
    // is the one that goes.
    push(100, 10_000);
    assert_eq!(read_ids(&mut reader, now), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 2)
    );

    // One due at the same moment as the last one held: the one that came
    // first stays.
    push(30, 3_000);
    assert_eq!(read_ids(&mut reader, now), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 3)
    );

    // Two due now, in one bundle, push out the two due last, which are
    // separate packets and so go one at a time, as far as needed.
    assert_eq!(
        writer.push_packet(&bundle_of(OscTime::IMMEDIATELY, 0, 2), now),
        Ok(2)
    );
    assert_eq!(read_ids(&mut reader, now), [0, 0]);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (2, 5)
    );

    // What is left comes out when it is due.
    assert_eq!(read_ids(&mut reader, at(1_000)), [1]);
    assert_eq!(read_ids(&mut reader, at(2_000)), [2]);
    assert_eq!(read_ids(&mut reader, at(20_000)), []);
    assert_eq!(stats.snapshot().late, 0);
}

/// Groups due later are pushed out only if that makes room. A group that
/// would not fit even with every later group gone pushes nothing out: it
/// is discarded alone, rather than taking the later groups with it.
#[test]
fn a_group_that_cannot_fit_pushes_nothing_out() {
    let config = BridgeConfig::default().with_pending_capacity(4);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let at = |seconds: u64| origin() + Duration::from_secs(seconds);
    // Held: two due in 5 seconds, then one due in 10 and one in 11.
    assert_eq!(writer.push_packet(&bundle_of(at(5), 5, 2), origin()), Ok(2));
    assert_eq!(
        writer.push_packet(&bundle_of(at(10), 10, 1), origin()),
        Ok(1)
    );
    assert_eq!(
        writer.push_packet(&bundle_of(at(11), 11, 1), origin()),
        Ok(1)
    );
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(reader.pending(), 4);

    // Three due in 7 seconds. Even with both later ones gone, two are held
    // that are due sooner, and 2 + 3 is more than 4.
    assert_eq!(writer.push_packet(&bundle_of(at(7), 7, 3), origin()), Ok(3));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 3)
    );

    // Two due in 7 seconds do fit once both later ones are gone.
    assert_eq!(writer.push_packet(&bundle_of(at(7), 8, 2), origin()), Ok(2));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 5)
    );
    assert_eq!(read_ids(&mut reader, at(5)), [5, 5]);
    assert_eq!(read_ids(&mut reader, at(7)), [8, 8]);
    assert_eq!(read_ids(&mut reader, at(20)), []);

    // The same, with the later events in one group of two.
    let (mut writer, mut reader) = setup(BridgeConfig::default().with_pending_capacity(4));
    let stats = writer.stats();
    assert_eq!(writer.push_packet(&bundle_of(at(5), 5, 2), origin()), Ok(2));
    assert_eq!(
        writer.push_packet(&bundle_of(at(10), 10, 2), origin()),
        Ok(2)
    );
    assert_eq!(writer.push_packet(&bundle_of(at(7), 7, 3), origin()), Ok(3));
    assert_eq!(read_ids(&mut reader, origin()), []);
    assert_eq!(
        (reader.pending(), stats.snapshot().dropped_pending_full),
        (4, 3)
    );
    assert_eq!(read_ids(&mut reader, at(10)), [5, 5, 10, 10]);
}

#[test]
fn events_that_make_way_give_their_pool_buffers_back() {
    // Four buffers: two for the events held, two for a round's arrivals.
    let config = BridgeConfig::default()
        .with_pending_capacity(2)
        .with_pool_buffers(4)
        .with_pool_buffer_len(128);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let mut delivered = 0;
    for round in 0..300u32 {
        let now = origin() + Duration::from_millis(10 * u64::from(round));
        // One for a second from now and one for two seconds from now, of
        // which at most one is still held when the block has been read:
        // the other is pushed out by a later arrival or dropped as it
        // arrives. And one for now, which is handed over.
        for (id, seconds) in [(-1, 1), (-2, 2)] {
            let held = OscBundle::new(now + Duration::from_secs(seconds)).with(large_note(id));
            assert_eq!(
                writer.push_packet(&bytes(held), now),
                Ok(1),
                "round {round}"
            );
        }
        assert_eq!(
            writer.push_packet(&bytes(large_note(1)), now),
            Ok(1),
            "round {round}"
        );
        let ids = read_ids(&mut reader, now);
        delivered += ids.iter().filter(|&&id| id == 1).count();
        assert!(reader.pending() <= 1);
    }
    assert_eq!(delivered, 300);
    let counts = stats.snapshot();
    // Both ways of discarding an event were taken hundreds of times. With
    // every buffer in use each round, one that went missing would have
    // shown as an event refused for want of a buffer.
    assert_eq!(counts.dropped_pool_empty, 0);
    assert!(counts.dropped_pending_full > 500, "{counts:?}");
    assert_eq!(counts.events, 900);
}

/// One event as the model below keeps it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Held {
    /// The frame of the stream it is due on.
    due: i64,
    /// Where it came in the order of everything sent.
    order: u64,
    packet: u64,
    timed: bool,
    id: i32,
}

/// What `BridgeConfig::pending_capacity` documents, written down as
/// plainly as possible: a list, searched from end to end.
struct Model {
    capacity: usize,
    held: Vec<Held>,
    dropped: u64,
    late: u64,
    /// Groups no larger than the capacity that were discarded although
    /// events due later were held, which were left alone because pushing
    /// them out would not have made room.
    spared: u64,
}

impl Model {
    /// Takes in the events of one packet.
    fn arrive(&mut self, mut events: Vec<Held>) {
        events.sort_by_key(|event| (event.due, event.order));
        // One group at a time: the events that are due at the same moment.
        for group in events.chunk_by(|a, b| a.due == b.due) {
            let due = group[0].due;
            // Only what is due later can be pushed out, and nothing is
            // unless that makes room for the whole group.
            let staying = self.held.iter().filter(|event| event.due <= due).count();
            let room = staying + group.len() <= self.capacity;
            while room && self.held.len() + group.len() > self.capacity {
                let last = *self
                    .held
                    .iter()
                    .max_by_key(|event| (event.due, event.order))
                    .unwrap();
                assert!(last.due > due);
                // The group due last goes, whole.
                let before = self.held.len();
                self.held
                    .retain(|event| (event.packet, event.due) != (last.packet, last.due));
                self.dropped += (before - self.held.len()) as u64;
            }
            if room {
                self.held.extend_from_slice(group);
            } else {
                self.dropped += group.len() as u64;
                if group.len() <= self.capacity && self.held.iter().any(|event| event.due > due) {
                    self.spared += 1;
                }
            }
        }
    }

    /// Hands over what is due before `position + frames`.
    fn read(&mut self, position: i64, frames: i64) -> Vec<(u32, i32)> {
        self.held.sort_by_key(|event| (event.due, event.order));
        let due_now = self
            .held
            .partition_point(|event| event.due < position + frames);
        self.held
            .drain(..due_now)
            .map(|event| {
                if event.due < position && event.timed {
                    self.late += 1;
                }
                ((event.due - position).max(0) as u32, event.id)
            })
            .collect()
    }
}

/// Random traffic, more than the audio side has room for, through the
/// bridge and through the model above. At 65,536 frames a second a frame
/// is a whole number of time tag units, so the model's frames are exact.
#[test]
fn what_is_kept_and_what_is_handed_over_is_what_the_documented_rule_says() {
    const POWER_OF_TWO_RATE: f64 = 65_536.0;
    const FRAMES: i64 = 256;
    let mut random = Random(0x00A6_E17D_A5EE_D001);
    for capacity in [1usize, 3, 8, 20] {
        let config = BridgeConfig::default()
            .with_pending_capacity(capacity)
            .with_queue_capacity(256);
        let (mut writer, mut reader) = setup(config);
        let stats = writer.stats();
        let mut model = Model {
            capacity,
            held: Vec::new(),
            dropped: 0,
            late: 0,
            spared: 0,
        };
        let frame = |frame: i64| origin().add_frames(frame as u64, POWER_OF_TWO_RATE);
        let (mut order, mut packet, mut id) = (0u64, 0u64, 0i32);
        let (mut handed_over, mut blocks_with_some) = (0usize, 0u32);

        for block in 0..3_000i64 {
            let position = block * FRAMES;
            for _ in 0..random.below(4) {
                // A bundle that is due at once or at some frame, sometimes
                // already past, holding one to five notes and sometimes a
                // bundle of its own.
                let pick_due = |random: &mut Random| match random.below(6) {
                    0 => None,
                    1 => Some(position - 1 - random.below(300) as i64),
                    2 | 3 => Some(position + random.below(600) as i64),
                    _ => Some(position + 300 * (1 + random.below(8) as i64)),
                };
                let outer_due = pick_due(&mut random).filter(|&due| due >= 0);
                let outer_schedule = match outer_due {
                    Some(due) => (due, true),
                    None => (position, false),
                };
                let mut events = Vec::new();
                let mut bundle = OscBundle::new(outer_due.map_or(OscTime::IMMEDIATELY, frame));
                let mut add = |events: &mut Vec<Held>, schedule: (i64, bool)| {
                    id += 1;
                    order += 1;
                    events.push(Held {
                        due: schedule.0,
                        order,
                        packet,
                        timed: schedule.1,
                        id,
                    });
                    note(id)
                };
                for _ in 0..1 + random.below(3) {
                    bundle = bundle.with(add(&mut events, outer_schedule));
                }
                if random.below(3) == 0 {
                    let inner_due = pick_due(&mut random).filter(|&due| due >= 0);
                    // A bundle inside takes its own time, unless it has
                    // none or the time is before the outer bundle's.
                    let inner_schedule = match inner_due {
                        Some(due) if !(outer_schedule.1 && due < outer_schedule.0) => (due, true),
                        _ => outer_schedule,
                    };
                    let mut inner = OscBundle::new(inner_due.map_or(OscTime::IMMEDIATELY, frame));
                    for _ in 0..1 + random.below(3) {
                        inner = inner.with(add(&mut events, inner_schedule));
                    }
                    bundle = bundle.with(inner);
                    if random.below(2) == 0 {
                        bundle = bundle.with(add(&mut events, outer_schedule));
                    }
                }
                let sent = events.len();
                assert_eq!(
                    writer.push_packet(&bytes(bundle), frame(position)),
                    Ok(sent)
                );
                model.arrive(events);
                packet += 1;
            }

            let mut seen = Vec::new();
            reader.read_stream(
                origin(),
                position as u64,
                FRAMES as usize,
                POWER_OF_TWO_RATE,
                |offset, event| {
                    seen.push((offset, event.args().next().unwrap().as_i32().unwrap()));
                },
            );
            let expected = model.read(position, FRAMES);
            assert_eq!(seen, expected, "capacity {capacity}, block {block}");
            assert_eq!(reader.pending(), model.held.len(), "block {block}");
            let counts = stats.snapshot();
            assert_eq!(
                (counts.dropped_pending_full, counts.late),
                (model.dropped, model.late),
                "capacity {capacity}, block {block}"
            );
            handed_over += seen.len();
            blocks_with_some += u32::from(!seen.is_empty());
        }
        println!(
            "room for {capacity}: {id} events sent, {handed_over} handed over in \
             {blocks_with_some} blocks, {} dropped, {} late, {} groups that did not fit \
             left later ones alone",
            model.dropped, model.late, model.spared
        );
        // Enough of everything for the agreement to mean something.
        assert!(handed_over > 500 && model.late > 50);
        assert!(model.dropped > 50);
        // A group that has to leave later ones alone needs more than one
        // place and a store that is often nearly full.
        if (2..=8).contains(&capacity) {
            assert!(model.spared > 20, "{}", model.spared);
        }
        assert_eq!(stats.snapshot().dropped(), model.dropped);
    }
}

// ---------------------------------------------------------------------
// Address patterns built to be expensive.
// ---------------------------------------------------------------------

fn voices(count: u32) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    for voice in 0..count {
        let address = format!("/synth/voice/{voice}/filter/cutoff/frequency");
        dispatcher.add(&address, voice).unwrap();
    }
    dispatcher
}

/// A pattern that keeps the matcher at work for as long as it is: `//*`
/// reaches every place in an address, and each `{,}` after it has to be
/// tried at every one of them. The final `x` makes sure it matches nothing.
fn hostile(optional_groups: usize) -> String {
    format!("//*{}x", "{,}".repeat(optional_groups))
}

/// This test measures time. A datagram can carry a pattern of 60 kilobytes,
/// and the dispatcher tries a pattern against every registered address. No
/// address longer than 255 bytes can be registered, so a pattern of twenty
/// kilobytes is refused without being looked at.
#[test]
fn a_long_pattern_does_not_stall_the_network_side() {
    let (mut writer, mut reader) = bridge::channel(voices(16), BridgeConfig::default());

    // One third of what a UDP datagram can carry. `//*` reaches every
    // place in an address, and each of the 20,000 empty alternatives is
    // then tried at every one of them.
    let pattern = format!("//*{{{}}}x", ",".repeat(20_000));
    let packet = bytes(OscMessage::new(pattern).with(1));
    assert!(packet.len() < 21_000);

    // The best of three, so that a pause that is the machine's and not the
    // bridge's does not count.
    let mut took = Duration::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        assert_eq!(writer.push_packet(&packet, origin()), Ok(0));
        took = took.min(started.elapsed());
    }
    assert!(
        took < Duration::from_millis(100),
        "one packet of {} bytes kept the network side busy for {took:?} with 16 addresses \
         registered",
        packet.len()
    );
    let counts = writer.stats().snapshot();
    assert_eq!(
        (counts.refused_patterns, counts.unmatched, counts.events),
        (3, 0, 0)
    );
    assert_eq!(reader.pending(), 0);
    reader.read_at(origin(), BLOCK, RATE, |_, _| {
        panic!("nothing was to be delivered")
    });
}

#[test]
fn only_patterns_with_wildcards_have_a_length_limit() {
    let config = BridgeConfig::default().with_max_pattern_len(64);
    assert_eq!(BridgeConfig::default().max_pattern_len, 1_024);
    let mut dispatcher = voices(4);
    dispatcher.add("/n", 100).unwrap();
    let (mut writer, mut reader) = bridge::channel(dispatcher, config);
    let stats = writer.stats();
    let mut push = |address: String| {
        let length = address.len();
        let queued = writer
            .push_packet(&bytes(OscMessage::new(address).with(1)), origin())
            .unwrap();
        (length, queued)
    };

    // A pattern of a chosen length that matches all four voices: the last
    // letter is one of a list of alternatives, most of them empty.
    let padded = |len: usize| {
        let commas = len - "/synth/voice/*/filter/cutoff/frequenc{y}".len();
        format!(
            "/synth/voice/*/filter/cutoff/frequenc{{{}y}}",
            ",".repeat(commas)
        )
    };
    // Exactly at the limit it is looked at, and matches.
    assert_eq!(push(padded(64)), (64, 4));
    assert_eq!(stats.snapshot().refused_patterns, 0);
    // One byte more and the packet is refused.
    assert_eq!(push(padded(65)), (65, 0));
    assert_eq!(stats.snapshot().refused_patterns, 1);
    // An address without wildcards is looked up whatever its length: it
    // costs one comparison, and here it simply is not registered.
    assert_eq!(push(format!("/{}", "x".repeat(5_000))), (5_001, 0));
    let counts = stats.snapshot();
    assert_eq!((counts.refused_patterns, counts.unmatched), (1, 1));

    // One long pattern anywhere in a bundle refuses the bundle.
    let bundle = OscBundle::default()
        .with(note(1))
        .with(OscBundle::default().with(OscMessage::new(format!("/n{}", "*".repeat(63)))))
        .with(note(2));
    assert_eq!(writer.push_packet(&bytes(bundle), origin()), Ok(0));
    assert_eq!(stats.snapshot().refused_patterns, 2);
    let mut seen = 0;
    reader.read_at(origin(), BLOCK, RATE, |_, _| seen += 1);
    assert_eq!(seen, 4);
}

/// Sixty patterns of a kilobyte, each under the length limit, in one
/// datagram cost as much as one pattern of sixty kilobytes. What bounds
/// that is the budget for the whole packet.
#[test]
fn the_patterns_of_one_packet_cannot_cost_more_than_the_budget() {
    let config = BridgeConfig::default().with_pool_buffers(1);
    assert_eq!(config.match_budget, 1_000_000);
    let dispatcher = {
        let mut dispatcher = voices(32);
        dispatcher.add("/n", 100).unwrap();
        dispatcher
    };
    let pattern = hostile(330);
    assert!(pattern.len() < config.max_pattern_len);

    // What the pattern costs, counted by the dispatcher itself: one fits
    // in the budget, and three do not.
    let mut work = config.match_budget;
    assert_eq!(
        dispatcher.dispatch_within(&pattern, &mut work, |_| {}),
        Ok(0)
    );
    let one = config.match_budget - work;
    println!("one such pattern against 33 addresses costs {one} units");
    assert!(3 * one > config.match_budget);
    let mut work = 2 * one;
    assert_eq!(
        dispatcher.dispatch_within(&pattern, &mut work, |_| {}),
        Ok(0)
    );
    assert_eq!(
        dispatcher.dispatch_within(&pattern, &mut work, |_| {}),
        Ok(0)
    );
    assert_eq!(work, 0);
    assert_eq!(
        dispatcher.dispatch_within(&pattern, &mut work, |_| {}),
        Err(BudgetExceeded)
    );
    // The count depends on the pattern and the table and on nothing else.
    let mut again = config.match_budget;
    assert_eq!(
        dispatcher.dispatch_within(&pattern, &mut again, |_| {}),
        Ok(0)
    );
    assert_eq!(config.match_budget - again, one);

    let (mut writer, mut reader) = bridge::channel(dispatcher.clone(), config);
    let stats = writer.stats();
    let single = bytes(OscMessage::new(pattern.as_str()).with(1));
    assert_eq!(writer.push_packet(&single, origin()), Ok(0));
    let counts = stats.snapshot();
    assert_eq!((counts.refused_patterns, counts.unmatched), (0, 1));

    // A datagram's worth, with messages before the patterns that would
    // have been delivered, one of them holding the only pool buffer.
    let with_patterns = |patterns: usize| {
        let mut bundle = OscBundle::default().with(note(1)).with(large_note(2));
        for _ in 0..patterns {
            bundle = bundle.with(OscMessage::new(pattern.as_str()).with(1));
        }
        bytes(bundle.with(note(3)))
    };
    let packet = with_patterns(60);
    assert!(packet.len() > 60_000 && packet.len() < 65_507);
    let started = Instant::now();
    assert_eq!(writer.push_packet(&packet, origin()), Ok(0));
    let took = started.elapsed();
    println!("refusing {} bytes of patterns took {took:?}", packet.len());
    let counts = stats.snapshot();
    assert_eq!(
        (counts.refused_patterns, counts.events, counts.dropped()),
        (1, 0, 0)
    );
    assert_eq!(read_ids(&mut reader, origin()), []);

    // The refused packet left nothing behind: the buffer is free again,
    // and the next packet is taken as if nothing had happened.
    let next = OscBundle::default().with(large_note(4)).with(note(5));
    assert_eq!(writer.push_packet(&bytes(next), origin()), Ok(2));
    assert_eq!(read_ids(&mut reader, origin()), [4, 5]);

    // The budget is what decides. Three of the patterns are refused with
    // the default, and with a budget of four times what one costs they
    // are worked through: the notes are delivered and the patterns reach
    // nobody.
    let three = with_patterns(3);
    assert_eq!(writer.push_packet(&three, origin()), Ok(0));
    assert_eq!(stats.snapshot().refused_patterns, 2);
    let config = BridgeConfig::default().with_match_budget(4 * one);
    let (mut writer, mut reader) = bridge::channel(dispatcher, config);
    assert_eq!(writer.push_packet(&three, origin()), Ok(3));
    assert_eq!(read_ids(&mut reader, origin()), [1, 2, 3]);
    let counts = writer.stats().snapshot();
    assert_eq!((counts.refused_patterns, counts.unmatched), (0, 3));
}

/// A packet refused for its patterns is counted once, in
/// `refused_patterns`, however far reading it had got: the messages before
/// the one that ran out of budget are not also counted as unmatched, and
/// its bundles not as out of range.
#[test]
fn a_packet_refused_for_its_patterns_is_counted_once() {
    let (mut writer, _reader) = bridge::channel(voices(32), BridgeConfig::default());
    let stats = writer.stats();
    let pattern = hostile(330);
    let too_far = origin() + Duration::from_secs(3_600);
    // Two patterns fit in the budget and match nothing; the third does not
    // fit. Before them, a message to nobody and a bundle too far ahead.
    let mut bundle = OscBundle::default()
        .with(OscMessage::new("/nobody"))
        .with(OscBundle::new(too_far).with(OscMessage::new("/nobody/either")));
    for _ in 0..3 {
        bundle = bundle.with(OscMessage::new(pattern.as_str()));
    }
    assert_eq!(writer.push_packet(&bytes(bundle), origin()), Ok(0));
    let counts = stats.snapshot();
    assert_eq!(
        (
            counts.refused_patterns,
            counts.unmatched,
            counts.out_of_range
        ),
        (1, 0, 0)
    );

    // The same without the third pattern is read to the end, and all of it
    // is counted.
    let mut bundle = OscBundle::default()
        .with(OscMessage::new("/nobody"))
        .with(OscBundle::new(too_far).with(OscMessage::new("/nobody/either")));
    for _ in 0..2 {
        bundle = bundle.with(OscMessage::new(pattern.as_str()));
    }
    assert_eq!(writer.push_packet(&bytes(bundle), origin()), Ok(0));
    let counts = stats.snapshot();
    assert_eq!(
        (
            counts.refused_patterns,
            counts.unmatched,
            counts.out_of_range
        ),
        (1, 4, 1)
    );
}

/// This test measures time. The budget is counted in units of work, not
/// in time; this spends exactly the default budget on the most expensive
/// kind of work there is and reports how long that takes, which is the
/// longest that matching can keep the network side busy for one packet.
/// The limit asserted is far above what was seen (a quarter of a second in
/// an unoptimised build on a loaded machine), so that the test tells a
/// bound from no bound without depending on the machine.
#[test]
fn spending_the_whole_budget_takes_a_bounded_time() {
    let budget = BridgeConfig::default().match_budget;
    let dispatcher = voices(200);
    let patterns = [
        hostile(330),
        format!("//*[{}]x", "a".repeat(1_000)),
        format!("//*{{{}}}x", "a,".repeat(500)),
        format!("//*{}", "{,e}".repeat(250)),
        format!("//*{{{}}}x", ",".repeat(1_000)),
    ];
    for pattern in &patterns {
        let mut work = budget;
        let started = Instant::now();
        let mut rounds = 0;
        while dispatcher
            .dispatch_within(pattern, &mut work, |_| {})
            .is_ok()
        {
            rounds += 1;
            assert!(rounds < 1_000_000, "the budget is not being spent");
        }
        let took = started.elapsed();
        println!(
            "{budget} units spent on {:.40}... in {took:?} ({rounds} whole rounds)",
            pattern
        );
        assert_eq!(work, 0);
        assert!(took < Duration::from_secs(20), "{took:?}");
    }
}

/// The other side of the budget: what senders really write has to fit in
/// it many times over, or the limit would get in the way.
#[test]
fn ordinary_patterns_cost_a_small_part_of_the_budget() {
    let budget = BridgeConfig::default().match_budget;
    let dispatcher = voices(200);
    let cost = |pattern: &str| {
        let mut work = budget;
        let reached = dispatcher
            .dispatch_within(pattern, &mut work, |_| {})
            .unwrap();
        (reached, budget - work)
    };

    // A plain address is looked up, not matched: a dozen units, whatever
    // the size of the table.
    let (reached, plain) = cost("/synth/voice/7/filter/cutoff/frequency");
    assert_eq!(reached, 1);
    assert!(plain < 20, "{plain}");

    let ordinary = [
        ("/synth/voice/7?/filter/cutoff/frequency", 10),
        ("/synth/voice/[0-9]/filter/{cutoff,q}/*", 10),
        ("/mixer/*/gain", 0),
        ("/synth/voice/*/filter/cutoff/frequency", 200),
        ("//frequency", 200),
        ("//gain", 0),
        ("/*/*/*/*/*/*", 200),
    ];
    for (pattern, expected) in ordinary {
        let (reached, units) = cost(pattern);
        println!("{pattern}: {reached} of 200 addresses, {units} units");
        assert_eq!(reached, expected, "{pattern}");
        // Against two hundred addresses, each of these could be sent ten
        // times in one packet with room to spare.
        assert!(units < budget / 10, "{pattern} costs {units}");
    }
    // One that matches nothing from its first part on costs a dozen units
    // for each address.
    let (_, elsewhere) = cost("/mixer/*/gain");
    assert!(elsewhere < 200 * 15, "{elsewhere}");

    // Through the bridge: a bundle of forty messages with wildcards, ten
    // events each, is taken.
    let mut bundle = OscBundle::default();
    for _ in 0..40 {
        let message = OscMessage::new("/synth/voice/7?/filter/cutoff/frequency");
        bundle = bundle.with(message.with(0.5f32));
    }
    let config = BridgeConfig::default();
    let (mut writer, _reader) = bridge::channel(voices(200), config);
    assert_eq!(writer.push_packet(&bytes(bundle), origin()), Ok(400));
    assert_eq!(writer.stats().snapshot().refused_patterns, 0);
}
