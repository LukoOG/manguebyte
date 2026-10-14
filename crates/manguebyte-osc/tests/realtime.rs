//! Proves what this crate says about the heap.
//!
//! The global allocator is replaced with one that counts calls and keeps
//! track of how many bytes are in use. Two kinds of claims are checked:
//!
//! - what is documented as free of allocation really makes no call to the
//!   allocator at all: the audio thread's side of the bridge above all, and
//!   also the network side, the decoder that works in place, the encoders
//!   and the matcher;
//! - decoding into owned values takes memory in proportion to the packet,
//!   whatever the packet is built to do, and a packet that is refused takes
//!   none.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

use manguebyte_osc::slip::{self, SlipDecoder};
use manguebyte_osc::{
    BridgeConfig, BundleEncoder, Dispatcher, OscArg, OscArgRef, OscBundle, OscMessage, OscPacket,
    OscPacketRef, OscTime, bridge, encode_message, encoded_message_len, pattern,
};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
    static BYTES_IN_USE: Cell<isize> = const { Cell::new(0) };
    static MOST_BYTES_IN_USE: Cell<isize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    /// Notes one call that changed the bytes in use by `change`.
    fn record(change: isize) {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
                let _ = BYTES_IN_USE.try_with(|bytes| {
                    bytes.set(bytes.get() + change);
                    let _ =
                        MOST_BYTES_IN_USE.try_with(|most| most.set(most.get().max(bytes.get())));
                });
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is updating thread-local counters, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size() as isize);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record(-(layout.size() as isize));
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size as isize - layout.size() as isize);
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and returns how many calls it made to the allocator and the
/// most bytes it had in use at one time, beyond what was in use before.
fn measure(work: impl FnOnce()) -> (usize, usize) {
    HEAP_CALLS.with(|calls| calls.set(0));
    BYTES_IN_USE.with(|bytes| bytes.set(0));
    MOST_BYTES_IN_USE.with(|most| most.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    (
        HEAP_CALLS.with(Cell::get),
        MOST_BYTES_IN_USE.with(Cell::get).max(0) as usize,
    )
}

/// Runs `work` and panics if it used the heap in any way.
fn assert_no_heap_use(what: &str, work: impl FnOnce()) {
    let (calls, _) = measure(work);
    assert_eq!(calls, 0, "{what} used the heap {calls} time(s)");
}

const RATE: f64 = 48_000.0;
const BLOCK: usize = 480;

fn origin() -> OscTime {
    OscTime::new(3_900_000_000, 0)
}

fn at_frame(frame: u32) -> OscTime {
    origin() + Duration::from_secs_f64(f64::from(frame) / RATE)
}

fn bytes(packet: impl Into<OscPacket>) -> Vec<u8> {
    packet.into().to_bytes().unwrap()
}

fn dispatcher() -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    dispatcher.add("/n", 1).unwrap();
    for voice in 0..8 {
        dispatcher
            .add(&format!("/voice/{voice}/gain"), 10 + voice)
            .unwrap();
        dispatcher
            .add(&format!("/voice/{voice}/pan"), 20 + voice)
            .unwrap();
    }
    dispatcher
}

/// A message too large to be stored in an event: it needs a pool buffer.
fn large(id: i32) -> OscMessage {
    OscMessage::new("/n")
        .with(id)
        .with("a string that is much too long to be stored in the event itself, so it goes to the pool")
        .with(vec![7u8; 40])
}

/// A dozen numbers, which still fit in an event.
fn dozen() -> OscMessage {
    let mut message = OscMessage::new("/n");
    for value in 0..12 {
        message = message.with(value as f32);
    }
    message
}

/// Every kind of packet the bridge has a path for.
fn traffic() -> Vec<Vec<u8>> {
    vec![
        bytes(OscMessage::new("/n").with(1)),
        bytes(dozen()),
        bytes(large(2)),
        bytes(OscMessage::new("/voice/*/gain").with(0.5f32)),
        bytes(OscMessage::new("/voice/[0-3]/{gain,pan}").with(0.25f32)),
        bytes(OscMessage::new("//pan").with(-1.0f32)),
        bytes(OscMessage::new("/nobody/home").with(1)),
        bytes(OscMessage::new("/voice/[").with(1)),
        // Due in the first block, in the second, and already overdue.
        bytes(OscBundle::new(at_frame(100)).with(OscMessage::new("/n").with(3))),
        bytes(OscBundle::new(at_frame(500)).with(large(4))),
        bytes(
            OscBundle::new(origin() - Duration::from_secs(1))
                .with(OscMessage::new("/n").with(5))
                .with(OscBundle::new(at_frame(700)).with(OscMessage::new("/n").with(6))),
        ),
        // Too far ahead to be honoured.
        bytes(OscBundle::new(origin() + Duration::from_secs(3_600)).with(OscMessage::new("/n"))),
        // Not packets at all.
        b"junk".to_vec(),
        b"/n\0\0,i\0\0".to_vec(),
        b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\x09".to_vec(),
        Vec::new(),
    ]
}

/// What a handler on the audio thread would do with an event: look at
/// everything in it.
#[derive(Default)]
struct Tally {
    events: usize,
    numbers: f64,
    text: usize,
    pooled: usize,
    timed: usize,
}

impl Tally {
    fn take(&mut self, frame: u32, event: &manguebyte_osc::OscEvent) {
        self.events += 1;
        self.numbers += f64::from(frame) + f64::from(event.handler());
        self.text += event.type_tags().len();
        self.pooled += usize::from(event.is_pooled());
        self.timed += usize::from(event.time_tag().is_some());
        for arg in event.args() {
            match arg {
                OscArgRef::String(text) | OscArgRef::Symbol(text) => self.text += text.len(),
                OscArgRef::Blob(data) => self.text += data.len(),
                other => self.numbers += other.number().unwrap_or(0.0),
            }
        }
    }
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");

    let (calls, most) = measure(|| drop(std::hint::black_box(vec![0u8; 1_000])));
    assert_eq!((calls, most), (2, 1_000));
}

#[test]
fn reading_events_on_the_audio_thread_does_not_use_the_heap() {
    let config = BridgeConfig::default()
        .with_pending_capacity(40)
        .with_pool_buffers(3);
    let (mut writer, mut reader) = bridge::channel(dispatcher(), config);
    let stats = writer.stats();
    let arrival = origin() - Duration::from_millis(5);
    for packet in traffic() {
        let _ = writer.push_packet(&packet, arrival);
    }
    // More events for later blocks than the audio side has room for, one
    // of them with a pool buffer, so that the paths that drop are run too.
    for id in 0..20 {
        let message = if id == 15 {
            large(id)
        } else {
            OscMessage::new("/n").with(id)
        };
        let packet = bytes(OscBundle::new(at_frame(1_000 + id as u32)).with(message));
        let _ = writer.push_packet(&packet, arrival);
    }

    let mut tally = Tally::default();
    let mut held = 0;
    assert_no_heap_use("BridgeReader::read_at", || {
        for block in 0..4u32 {
            reader.read_at(
                at_frame(block * BLOCK as u32),
                BLOCK,
                RATE,
                |frame, event| {
                    tally.take(frame, event);
                },
            );
            held += reader.pending();
        }
        // A block of no frames, and one timed by the system clock.
        reader.read_at(at_frame(4 * BLOCK as u32), 0, RATE, |_, _| {});
        reader.read(BLOCK, RATE, |frame, event| tally.take(frame, event));
        reader.clear();
    });
    println!(
        "{} events read: {} from the pool, {} with a time tag",
        tally.events, tally.pooled, tally.timed
    );
    let counts = stats.snapshot();
    assert!(tally.events >= 40, "{}", tally.events);
    assert!(tally.pooled >= 2 && tally.timed >= 5 && tally.text > 100);
    assert!(held > 0);
    // The paths that count something were all taken.
    assert!(counts.late >= 1 && counts.dropped_pending_full >= 1);

    // And in the steady state, with buffers going round between the two
    // sides for a long time.
    let packets = traffic();
    for round in 0..200u32 {
        let now = at_frame(round * BLOCK as u32);
        for packet in &packets {
            let _ = writer.push_packet(packet, now);
        }
        assert_no_heap_use("BridgeReader::read_at", || {
            reader.read_at(now, BLOCK, RATE, |frame, event| tally.take(frame, event));
        });
    }
    assert_eq!(stats.snapshot().dropped_pool_empty, 0);
    assert!(tally.pooled > 200);
}

#[test]
fn taking_packets_in_does_not_use_the_heap_either() {
    let config = BridgeConfig::default().with_pool_buffers(4);
    let (mut writer, mut reader) = bridge::channel(dispatcher(), config);
    let stats = writer.stats();
    let packets = traffic();
    let mut tally = Tally::default();
    let mut refused = 0;
    assert_no_heap_use("BridgeWriter::push_packet", || {
        for round in 0..300u32 {
            let now = at_frame(round * BLOCK as u32);
            for packet in &packets {
                refused += usize::from(writer.push_packet(packet, now).is_err());
            }
            // Read every third round only, so that the pool runs dry and
            // the paths that drop are run as well.
            if round % 3 == 0 {
                reader.read_at(now, BLOCK, RATE, |frame, event| tally.take(frame, event));
            }
        }
    });
    let counts = stats.snapshot();
    println!("{counts:?}");
    assert_eq!(counts.packets, 300 * packets.len() as u64);
    assert_eq!(counts.malformed, refused as u64);
    assert_eq!(refused, 300 * 4);
    assert_eq!((counts.unmatched, counts.out_of_range), (600, 300));
    // Each round's packets reach 32 handlers between them. Every one of
    // those is either an event or counted as dropped.
    assert!(counts.dropped_pool_empty > 0);
    assert_eq!(
        counts.events + counts.dropped_pool_empty + counts.dropped_queue_full,
        300 * 32
    );
    assert_eq!(counts.dropped_too_large, 0);
    assert!(tally.events > 5_000);
}

/// The paths the bridge takes when it is asked for more than it has: a
/// group of events pushed out by events due sooner, a group refused whole
/// on the audio side, a block read by its position in the stream, and
/// packets refused for the cost of their address patterns or for want of
/// room in the queue or the pool. None of them may touch the heap.
#[test]
fn making_way_and_refusing_do_not_use_the_heap() {
    let config = BridgeConfig::default()
        .with_queue_capacity(16)
        .with_pending_capacity(6)
        .with_pool_buffers(3)
        .with_max_pattern_len(64)
        .with_match_budget(5_000);
    let (mut writer, mut reader) = bridge::channel(dispatcher(), config);
    let stats = writer.stats();
    let far = |id: i32| {
        bytes(
            OscBundle::new(at_frame(40_000))
                .with(OscMessage::new("/n").with(id))
                .with(large(id))
                .with(OscMessage::new("/n").with(id)),
        )
    };
    let (far_first, far_second) = (far(1), far(2));
    let now = bytes(
        OscBundle::default()
            .with(OscMessage::new("/n").with(0))
            .with(large(0)),
    );
    let five_between = bytes((0..5).fold(OscBundle::new(at_frame(30_000)), |bundle, id| {
        bundle.with(OscMessage::new("/n").with(id))
    }));
    let too_many = bytes((0..20).fold(OscBundle::default(), |bundle, id| {
        bundle.with(OscMessage::new("/n").with(id))
    }));
    let too_long = bytes(OscMessage::new(format!("/voice/*/{}", "?".repeat(80))));
    let too_costly = bytes((0..40).fold(OscBundle::default(), |bundle, _| {
        bundle.with(OscMessage::new("//*{,}{,}{,}{,}{,}{,}{,}x"))
    }));
    let mut tally = Tally::default();
    let mut held = 0;

    for round in 0..50u32 {
        let start = at_frame(round * BLOCK as u32);
        assert_no_heap_use("making way and refusing", || {
            // Two groups for later fill the audio side; then a group due now
            // pushes one of them out. Then five due between those two
            // times take the other out, find that is not enough, and put
            // it back.
            assert_eq!(writer.push_packet(&far_first, start), Ok(3));
            assert_eq!(writer.push_packet(&far_second, start), Ok(3));
            reader.read_at(start, BLOCK, RATE, |frame, event| tally.take(frame, event));
            assert_eq!(writer.push_packet(&now, start), Ok(2));
            assert_eq!(writer.push_packet(&five_between, start), Ok(5));
            assert_eq!(writer.push_packet(&too_many, start), Ok(0));
            assert_eq!(writer.push_packet(&too_long, start), Ok(0));
            assert_eq!(writer.push_packet(&too_costly, start), Ok(0));
            reader.read_stream(
                origin(),
                u64::from(round) * BLOCK as u64,
                BLOCK,
                RATE,
                |frame, event| tally.take(frame, event),
            );
            held += reader.pending();
            reader.clear();
        });
    }
    let counts = stats.snapshot();
    println!("{counts:?}");
    // Each path was taken in every round: the group pushed out and the five
    // that did not fit are 8 events, and the group that was put back is
    // still held when the block has been read.
    assert_eq!(counts.refused_patterns, 100);
    assert_eq!(counts.dropped_queue_full, 50 * 20);
    assert_eq!(counts.dropped_pending_full, 50 * 8);
    assert_eq!(held, 50 * 3);
    assert_eq!(counts.dropped_pool_empty, 0);
    assert_eq!(tally.events, 50 * 2);
    assert_eq!(tally.pooled, 50);
}

/// Adds up everything in a packet, to make sure all of it is visited.
fn weigh(packet: &OscPacketRef<'_>) -> usize {
    match packet {
        OscPacketRef::Message(message) => {
            let args: usize = message
                .args()
                .map(|arg| match arg {
                    OscArgRef::String(text) | OscArgRef::Symbol(text) => text.len(),
                    OscArgRef::Blob(data) => data.len(),
                    _ => 1,
                })
                .sum();
            message.address().len() + message.type_tags().len() + args
        }
        OscPacketRef::Bundle(bundle) => {
            usize::from(bundle.time().is_immediate())
                + bundle
                    .elements()
                    .map(|element| weigh(&element))
                    .sum::<usize>()
        }
    }
}

fn everything() -> OscMessage {
    OscMessage::new("/every/type")
        .with(-7)
        .with(0.5f32)
        .with("text")
        .with(vec![1u8, 2, 3, 4, 5])
        .with(-7_000_000_000i64)
        .with(-0.25f64)
        .with(OscTime::new(3_900_000_000, 0x8000_0000))
        .with(OscArg::Symbol("symbol".into()))
        .with('A')
        .with(true)
        .with(OscArg::Nil)
        .with(vec![
            OscArg::Int(1),
            OscArg::Array(vec![OscArg::String("in".into()), OscArg::Blob(vec![9])]),
        ])
}

fn nested() -> OscPacket {
    OscBundle::new(OscTime::new(3_900_000_000, 0))
        .with(everything())
        .with(
            OscBundle::new(OscTime::new(3_900_000_001, 0))
                .with(OscMessage::new("/a").with(1).with("two"))
                .with(OscBundle::default().with(everything())),
        )
        .with(OscMessage::new("/last"))
        .into()
}

#[test]
fn reading_a_packet_in_place_does_not_use_the_heap() {
    let packet = bytes(nested());
    let mut weight = 0;
    let mut refused = 0;
    assert_no_heap_use("OscPacketRef::parse", || {
        let view = OscPacketRef::parse(&packet).unwrap();
        weight = weigh(&view);
        // Refusing a packet does not allocate either, wherever it is cut.
        for len in 0..packet.len() {
            match OscPacketRef::parse(&packet[..len]) {
                Ok(view) => weight += weigh(&view),
                Err(_) => refused += 1,
            }
        }
    });
    assert!(weight > 100, "{weight}");
    assert!(refused > packet.len() * 9 / 10);
}

#[test]
fn a_packet_that_is_refused_costs_no_memory() {
    let packet = bytes(nested());
    let mut refused = 0;
    for len in 0..packet.len() {
        let cut = &packet[..len];
        if OscPacketRef::parse(cut).is_ok() {
            continue;
        }
        assert_no_heap_use("refusing a packet", || {
            assert!(OscPacket::decode(cut).is_err());
        });
        refused += 1;
    }
    // Damage in the last argument of the last message is found only after
    // everything before it has been read; still nothing was allocated.
    let mut damaged = bytes(OscBundle::default().with(everything()).with(everything()));
    let last = damaged.len() - 1;
    damaged[last] = 0xFF;
    assert_no_heap_use("refusing a packet", || {
        assert!(OscPacket::decode(&damaged).is_err());
    });
    assert!(refused > 200);
}

#[test]
fn encoding_into_a_buffer_does_not_use_the_heap() {
    let packets = [
        nested(),
        everything().into(),
        OscMessage::new("/foo").with(1_000).with("hello").into(),
    ];
    let mut buffer = [0u8; 1_024];
    let mut framed = [0u8; 2_100];
    let mut vector = Vec::with_capacity(4_096);
    let mut decoder = SlipDecoder::new(1_024);
    let mut checksum = 0usize;

    assert_no_heap_use("encoding", || {
        for packet in &packets {
            let len = packet.encode(&mut buffer).unwrap();
            assert_eq!(len, packet.encoded_len());
            // Too small a buffer is an error, not an allocation.
            assert!(packet.encode(&mut buffer[..len - 1]).is_err());
            // A vector with room to spare is not grown.
            packet.encode_to_vec(&mut vector).unwrap();

            // Framing and unframing work in buffers of their own.
            let framed_len = slip::encode(&buffer[..len], &mut framed).unwrap();
            decoder.feed_all(&framed[..framed_len], |frame| {
                checksum += frame.map_or(0, <[u8]>::len);
            });
        }

        let args = [
            OscArgRef::Int(1),
            OscArgRef::Float(2.0),
            OscArgRef::String("three"),
            OscArgRef::Blob(&[4, 5, 6]),
            OscArgRef::ArrayStart,
            OscArgRef::Double(7.0),
            OscArgRef::ArrayEnd,
        ];
        let len = encode_message(&mut buffer, "/borrowed", &args).unwrap();
        assert_eq!(len, encoded_message_len("/borrowed", &args));
        checksum += len;

        let mut bundle = BundleEncoder::new(&mut buffer, OscTime::IMMEDIATELY);
        bundle.message("/a", &args).unwrap();
        bundle.begin_bundle(OscTime::new(1, 2)).unwrap();
        bundle.message("/b", &[OscArgRef::Bool(true)]).unwrap();
        assert!(bundle.message("bad", &[]).is_err());
        bundle.end_bundle().unwrap();
        checksum += bundle.finish().unwrap();
    });
    assert_eq!(
        vector.len(),
        packets.iter().map(OscPacket::encoded_len).sum::<usize>()
    );
    assert!(checksum > vector.len());
}

#[test]
fn matching_dispatching_and_the_clock_do_not_use_the_heap() {
    let dispatcher = dispatcher();
    let patterns = [
        "/n",
        "/voice/3/gain",
        "/voice/*/gain",
        "/voice/[0-3]/{gain,pan}",
        "/voice/?/*",
        "//pan",
        "//*",
        "/voice/[",
        "/nobody",
        "",
    ];
    let longest = format!("/{}", "x".repeat(254));
    let mut reached = 0;
    let mut odd_handlers = 0;
    let mut now = OscTime::IMMEDIATELY;
    assert_no_heap_use("matching", || {
        for text in patterns {
            reached += dispatcher.dispatch(text, |handler| odd_handlers += handler % 2);
            reached += usize::from(pattern::matches(text, "/voice/3/gain"));
            reached += usize::from(pattern::validate_pattern(text).is_ok());
            reached += usize::from(pattern::validate_address(text).is_ok());
        }
        // The longest address that is matched without borrowing memory.
        reached += usize::from(pattern::matches("/*x", &longest));
        now = OscTime::now();
    });
    assert!(
        reached > 40 && odd_handlers > 10,
        "{reached} {odd_handlers}"
    );
    assert!(!now.is_immediate());
}

/// A message to `/a` with these type tags and this argument data.
fn message_with(tags: &str, data: &[u8]) -> Vec<u8> {
    let mut packet = b"/a\0\0,".to_vec();
    packet.extend_from_slice(tags.as_bytes());
    packet.resize(packet.len() + 4 - (1 + tags.len()) % 4, 0);
    packet.extend_from_slice(data);
    packet
}

/// A bundle holding `count` copies of `element`.
fn bundle_of(count: usize, element: &[u8]) -> Vec<u8> {
    let mut packet = b"#bundle\0\0\0\0\0\0\0\0\x01".to_vec();
    for _ in 0..count {
        packet.extend_from_slice(&(element.len() as u32).to_be_bytes());
        packet.extend_from_slice(element);
    }
    packet
}

#[test]
fn decoding_takes_memory_in_proportion_to_the_packet() {
    // Packets of about sixty kilobytes, near the most a UDP datagram
    // carries, each built to make the decoder allocate as much as it can
    // for every byte it is sent.
    let hostile: Vec<(&str, Vec<u8>)> = vec![
        (
            "arguments that take no data",
            message_with(&"T".repeat(60_000), &[]),
        ),
        ("empty arrays", message_with(&"[]".repeat(30_000), &[])),
        (
            "arrays sixteen deep",
            message_with(
                &format!("{}{}", "[".repeat(16), "]".repeat(16)).repeat(1_800),
                &[],
            ),
        ),
        (
            "one-letter strings",
            message_with(&"s".repeat(12_000), &b"x\0\0\0".repeat(12_000)),
        ),
        (
            "empty blobs",
            message_with(&"b".repeat(12_000), &[0u8; 48_000]),
        ),
        ("the smallest messages", bundle_of(7_500, b"/\0\0\0")),
        (
            "empty bundles",
            bundle_of(3_000, b"#bundle\0\0\0\0\0\0\0\0\x01"),
        ),
        ("one large blob", {
            let mut data = 60_000u32.to_be_bytes().to_vec();
            data.resize(60_004, 0xEE);
            message_with("b", &data)
        }),
    ];

    let mut worst = 0.0f64;
    for (what, packet) in &hostile {
        let mut decoded = None;
        let (calls, most) = measure(|| decoded = Some(OscPacket::decode(packet)));
        let decoded = decoded.unwrap().unwrap();
        // All of the packet was decoded: writing it again takes at least
        // as many bytes.
        assert!(decoded.encoded_len() >= packet.len(), "{what}");
        let per_byte = most as f64 / packet.len() as f64;
        println!(
            "{what}: {} bytes in, {most} bytes of heap at most ({per_byte:.1} per byte), {calls} calls",
            packet.len()
        );
        worst = worst.max(per_byte);
        // The most a sender can get for one byte is one argument: a type
        // tag that needs no data. Vectors are allocated at their final
        // size, so nothing is taken beyond that.
        assert!(most <= size_of::<OscArg>() * packet.len(), "{what}");
    }
    println!(
        "an argument takes {} bytes and a packet {}; worst case {worst:.1} bytes of heap per byte",
        size_of::<OscArg>(),
        size_of::<OscPacket>()
    );
}
