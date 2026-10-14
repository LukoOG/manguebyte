//! The bridge to the audio thread: which frame an event lands on, in what
//! order events come out, and what happens to what does not fit.
//!
//! Every test but the ones at the end that run a `BridgeThread` passes its
//! own times in, so nothing else here depends on how fast the test runs.

use std::io::Write;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use manguebyte_osc::bridge;
use manguebyte_osc::{
    BridgeConfig, BridgeReader, BridgeThread, BridgeWriter, Dispatcher, OscArg, OscArgRef,
    OscBundle, OscEvent, OscMessage, OscPacket, OscTime, TcpReceiver, TcpSender, UdpReceiver,
    UdpSender,
};

const RATE: f64 = 48_000.0;
/// Units of a time tag in one second.
const TICKS_PER_SECOND: f64 = 4_294_967_296.0;
/// Ten milliseconds.
const BLOCK: usize = 480;
const NOTE: u32 = 1;

/// The moment frame 0 of the stream stands for: some time in 2023.
fn origin() -> OscTime {
    OscTime::new(3_900_000_000, 0)
}

/// The moment a frame of the stream stands for. Fractions of a frame and
/// frames before the origin are allowed.
fn at_frame(frame: f64) -> OscTime {
    let distance = Duration::from_secs_f64(frame.abs() / RATE);
    if frame >= 0.0 {
        origin() + distance
    } else {
        origin() - distance
    }
}

fn setup(config: BridgeConfig) -> (BridgeWriter, BridgeReader) {
    let mut dispatcher = Dispatcher::new();
    dispatcher.add("/n", NOTE).unwrap();
    bridge::channel(dispatcher, config)
}

/// A message to the one registered address, told apart by a number.
fn note(id: i32) -> OscMessage {
    OscMessage::new("/n").with(id)
}

fn bytes(packet: impl Into<OscPacket>) -> Vec<u8> {
    packet.into().to_bytes().unwrap()
}

/// A bundle for `time` holding one note per number.
fn tagged(time: OscTime, ids: &[i32]) -> Vec<u8> {
    let mut bundle = OscBundle::new(time);
    for &id in ids {
        bundle = bundle.with(note(id));
    }
    bytes(bundle)
}

fn id_of(event: &OscEvent) -> i32 {
    event.args().next().and_then(|arg| arg.as_i32()).unwrap()
}

/// Reads one block and returns the frame and the number of every event.
fn read(reader: &mut BridgeReader, block_start: OscTime, frames: usize) -> Vec<(u32, i32)> {
    let mut seen = Vec::new();
    reader.read_at(block_start, frames, RATE, |frame, event| {
        assert_eq!(event.handler(), NOTE);
        assert!((frame as usize) < frames);
        seen.push((frame, id_of(event)));
    });
    seen
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

#[test]
fn a_time_tag_lands_on_its_exact_frame() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    // Everything arrives five milliseconds before the first block.
    let arrival = at_frame(-240.0);
    for (frame, id) in [(100.0, 1), (0.0, 2), (479.0, 3), (480.0, 4), (1_000.0, 5)] {
        let packet = tagged(at_frame(frame), &[id]);
        assert_eq!(writer.push_packet(&packet, arrival), Ok(1));
    }

    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [(0, 2), (100, 1), (479, 3)]
    );
    // The other two belong to later blocks and wait for them.
    assert_eq!(reader.pending(), 2);
    assert_eq!(read(&mut reader, at_frame(480.0), BLOCK), [(0, 4)]);
    assert_eq!(read(&mut reader, at_frame(960.0), BLOCK), [(40, 5)]);
    assert_eq!(reader.pending(), 0);

    let counts = writer.stats().snapshot();
    assert_eq!((counts.packets, counts.events), (5, 5));
    assert_eq!(
        (counts.late, counts.out_of_range, counts.dropped()),
        (0, 0, 0)
    );
}

#[test]
fn every_frame_of_a_block_can_be_hit() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    // In an order that is not the order of the frames.
    for step in 0..BLOCK {
        let frame = (step * 193) % BLOCK;
        let packet = tagged(at_frame(frame as f64), &[frame as i32]);
        assert_eq!(writer.push_packet(&packet, at_frame(-1.0)), Ok(1));
    }
    let expected: Vec<(u32, i32)> = (0..BLOCK)
        .map(|frame| (frame as u32, frame as i32))
        .collect();
    assert_eq!(read(&mut reader, at_frame(0.0), BLOCK), expected);
}

/// Plays a list of time-tagged events through blocks of the given sizes,
/// over and over, and returns the frame of the stream each one landed on.
fn render(block_sizes: &[usize], events: &[(u64, i32)], total: u64) -> Vec<(u64, i32)> {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    for &(frame, id) in events {
        let packet = tagged(at_frame(frame as f64), &[id]);
        assert_eq!(writer.push_packet(&packet, at_frame(-48.0)), Ok(1));
    }
    let mut landed = Vec::new();
    let mut position = 0u64;
    let mut sizes = block_sizes.iter().cycle();
    while position < total {
        let frames = *sizes.next().unwrap();
        for (offset, id) in read(&mut reader, at_frame(position as f64), frames) {
            landed.push((position + u64::from(offset), id));
        }
        position += frames as u64;
    }
    assert_eq!(reader.pending(), 0);
    assert_eq!(writer.stats().snapshot().late, 0);
    landed
}

#[test]
fn where_an_event_lands_does_not_depend_on_the_block_size() {
    const TOTAL: u64 = 20_000;
    let mut random = Random(0x0B10_C512_E5ED_0001);
    let mut events = Vec::new();
    for id in 0..400 {
        let frame = match events.last() {
            // Every fifth event shares its frame with the one before, so
            // that order within a frame is tested too.
            Some(&(previous, _)) if id % 5 == 0 => previous,
            _ => random.below(TOTAL as usize) as u64,
        };
        events.push((frame, id));
    }
    // Earliest first, and in the order they were sent within a frame.
    let mut expected = events.clone();
    expected.sort_by_key(|&(frame, _)| frame);

    for block_sizes in [
        &[64][..],
        &[256],
        &[997],
        &[1_024],
        &[1, 511, 7, 300, 64, 2, 129],
        &[20_000],
        &[1],
    ] {
        assert_eq!(
            render(block_sizes, &events, TOTAL),
            expected,
            "blocks of {block_sizes:?}"
        );
    }
}

#[test]
fn late_bundles_land_on_frame_zero_and_are_counted() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let arrival = at_frame(-240.0);
    // Ten milliseconds overdue, one frame overdue, and overdue by more
    // than half a frame, which is nearer to the frame before the block.
    writer
        .push_packet(&tagged(at_frame(-480.0), &[1]), arrival)
        .unwrap();
    writer
        .push_packet(&tagged(at_frame(-1.0), &[2]), arrival)
        .unwrap();
    writer
        .push_packet(&tagged(at_frame(-0.6), &[3]), arrival)
        .unwrap();
    // Less than half a frame early: frame 0 is its frame, so it is on time.
    writer
        .push_packet(&tagged(at_frame(-0.4), &[4]), arrival)
        .unwrap();
    // A message without a time tag cannot be late.
    writer.push_packet(&bytes(note(5)), arrival).unwrap();

    // All on frame 0, in the order of the moments they were due.
    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [(0, 1), (0, 5), (0, 2), (0, 3), (0, 4)]
    );
    let counts = writer.stats().snapshot();
    assert_eq!(counts.late, 3);
    assert_eq!((counts.events, counts.dropped()), (5, 0));
}

/// Reads an event through blocks of the given sizes, each said to start
/// where the one before it ended, and returns the frames of the stream it
/// landed on and the late count. `block_start` gives the time of a frame.
fn land_through_blocks(
    event: OscTime,
    blocks: &[usize],
    sample_rate: f64,
    block_start: impl Fn(u64) -> OscTime,
    stream: bool,
) -> (Vec<u64>, u64) {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let arrival = block_start(0) - Duration::from_millis(10);
    assert_eq!(writer.push_packet(&tagged(event, &[1]), arrival), Ok(1));
    let mut landed = Vec::new();
    let mut position = 0u64;
    for &frames in blocks {
        let mut take = |offset: u32, _: &OscEvent| landed.push(position + u64::from(offset));
        if stream {
            reader.read_stream(block_start(0), position, frames, sample_rate, &mut take);
        } else {
            reader.read_at(block_start(position), frames, sample_rate, &mut take);
        }
        position += frames as u64;
    }
    assert_eq!(reader.pending(), 0);
    (landed, writer.stats().snapshot().late)
}

/// The boundary between two frames is in the same place whichever block
/// asks. An event exactly half way between the last frame of one block and
/// the first of the next belongs to the first frame of the next block: the
/// block before must not keep it back and the block after must not call
/// it late. At 65,536 frames a second a frame is exactly 65,536 units of a
/// time tag, so the half frame and every block start are exact.
#[test]
fn an_event_half_a_frame_before_a_block_is_not_counted_as_late() {
    const EXACT_RATE: f64 = 65_536.0;
    let frame_time = |frame: u64| origin().add_frames(frame, EXACT_RATE);
    // Frame 479.5.
    let half = OscTime::from_bits(frame_time(479).to_bits() + (1 << 15));
    assert_eq!(half.to_bits() - origin().to_bits(), 479 * 65_536 + 32_768);
    let ones = [1usize; 960];
    for blocks in [
        &[960][..],
        &[481, 479],
        &[480, 480],
        &[479, 1, 480],
        &[64; 15],
        &ones,
    ] {
        for stream in [false, true] {
            let (landed, late) = land_through_blocks(half, blocks, EXACT_RATE, frame_time, stream);
            assert_eq!(
                (landed, late),
                (vec![480], 0),
                "blocks of {:?}, read_stream: {stream}",
                &blocks[..blocks.len().min(4)]
            );
        }
    }
    // A hair either side of the half goes to the nearer frame.
    for (offset, frame) in [(-1i64, 479u64), (1, 480)] {
        let event = OscTime::from_bits(half.to_bits().wrapping_add_signed(offset));
        for blocks in [&[960][..], &[480, 480], &ones] {
            let (landed, late) = land_through_blocks(event, blocks, EXACT_RATE, frame_time, false);
            assert_eq!((landed, late), (vec![frame], 0));
        }
    }
}

/// With rates at which a frame is not a whole number of time tag units,
/// `read_stream` still puts every event on one frame of the stream however
/// the stream is cut, including events placed as near as a time tag can
/// get to half way between two frames, and none of them is late.
#[test]
fn half_frames_land_on_the_same_frame_of_the_stream_at_any_rate() {
    for rate in [8_000.0, 44_100.0, 48_000.0, 96_000.0, 384_000.0] {
        let frame_time = |frame: u64| origin().add_frames(frame, rate);
        // Half a frame after frame k, as near as a time tag can say it.
        let half_after = |k: u64| {
            let half = (TICKS_PER_SECOND / rate / 2.0).round() as u64;
            OscTime::from_bits(frame_time(k).to_bits() + half)
        };
        let mut random = Random(0x0BAD_C0DE_0000_0001 ^ rate as u64);
        for _ in 0..60 {
            let k = random.below(2_000) as u64;
            let event = half_after(k);
            let ones = [1usize; 2_100];
            let (first, late) = land_through_blocks(event, &[2_100], rate, frame_time, true);
            assert_eq!(late, 0);
            assert_eq!(first.len(), 1);
            // One of the two frames it lies between.
            assert!(
                first[0] == k || first[0] == k + 1,
                "{rate}: {k} -> {first:?}"
            );
            for blocks in [
                [64].repeat(33),
                [1, 511, 7, 300, 64, 2, 129].repeat(3),
                vec![k as usize + 1, 2_100],
                vec![k as usize, 2_100],
                ones.to_vec(),
            ] {
                let (landed, late) = land_through_blocks(event, &blocks, rate, frame_time, true);
                assert_eq!((&landed, late), (&first, 0), "{rate}: frame {k}");
            }
        }
    }
}

#[test]
fn a_message_without_a_time_tag_is_due_on_arrival() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    writer
        .push_packet(&bytes(note(1)), at_frame(-360.0))
        .unwrap();
    writer
        .push_packet(&bytes(note(2)), at_frame(-120.0))
        .unwrap();
    // A bundle marked "immediately" is the same thing.
    writer
        .push_packet(&tagged(OscTime::IMMEDIATELY, &[3, 4]), at_frame(-60.0))
        .unwrap();
    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [(0, 1), (0, 2), (0, 3), (0, 4)]
    );
    assert_eq!(writer.stats().snapshot().late, 0);
}

#[test]
fn latency_puts_untagged_messages_where_they_fell_in_the_block_before() {
    // One block of latency: what `MidiReceiver` does for MIDI.
    let config = BridgeConfig::default().with_latency(Duration::from_secs_f64(BLOCK as f64 / RATE));
    let (mut writer, mut reader) = setup(config);

    // 7.5 ms and 2.5 ms before the block: a quarter and three quarters of
    // the way through the block before it.
    writer
        .push_packet(&bytes(note(1)), at_frame(-360.0))
        .unwrap();
    writer
        .push_packet(&bytes(note(2)), at_frame(-120.0))
        .unwrap();
    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [(120, 1), (360, 2)]
    );

    // The distance between two messages is kept across a block boundary:
    // the second and the third arrive 240 frames apart and land 240 frames
    // apart, on frames 360 and 480 + 120.
    writer
        .push_packet(&bytes(note(3)), at_frame(120.0))
        .unwrap();
    assert_eq!(read(&mut reader, at_frame(480.0), BLOCK), [(120, 3)]);

    // A bundle marked "immediately" is treated the same.
    writer
        .push_packet(&tagged(OscTime::IMMEDIATELY, &[4]), at_frame(600.0))
        .unwrap();
    assert_eq!(read(&mut reader, at_frame(960.0), BLOCK), [(120, 4)]);

    // One that the audio thread gets to too late goes on frame 0, and is
    // not counted: it had no time tag to be late for.
    writer.push_packet(&bytes(note(5)), at_frame(0.0)).unwrap();
    assert_eq!(read(&mut reader, at_frame(1_440.0), BLOCK), [(0, 5)]);
    assert_eq!(writer.stats().snapshot().late, 0);
}

/// What `read` returns, with the time tag each event reports.
fn read_with_time_tags(
    reader: &mut BridgeReader,
    block_start: OscTime,
) -> Vec<(u32, i32, Option<OscTime>)> {
    let mut seen = Vec::new();
    reader.read_at(block_start, BLOCK, RATE, |frame, event| {
        seen.push((frame, id_of(event), event.time_tag()));
    });
    seen
}

#[test]
fn a_time_tag_too_far_ahead_is_not_honoured() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let arrival = origin();
    let minute = Duration::from_secs(60);
    let in_range = arrival + minute;
    let from_1990 = OscTime::new(2_840_000_000, 0);

    // A hair more than a minute ahead, which is the default limit.
    let beyond = tagged(arrival + minute + Duration::from_micros(1), &[1]);
    writer.push_packet(&beyond, arrival).unwrap();
    writer
        .push_packet(&tagged(in_range, &[2]), arrival)
        .unwrap();
    // What senders write when they do not mean a time at all. Read as
    // times, these are in 2036.
    for (id, bits) in [(3, 0u64), (4, 2), (5, 10 << 32)] {
        let packet = tagged(OscTime::from_bits(bits), &[id]);
        writer.push_packet(&packet, arrival).unwrap();
    }
    // Decades overdue is overdue: late, not out of range.
    writer
        .push_packet(&tagged(from_1990, &[6]), arrival)
        .unwrap();

    assert_eq!(
        read_with_time_tags(&mut reader, arrival),
        [
            (0, 6, Some(from_1990)),
            (0, 1, None),
            (0, 3, None),
            (0, 4, None),
            (0, 5, None),
        ]
    );
    assert_eq!(reader.pending(), 1);
    let counts = writer.stats().snapshot();
    assert_eq!((counts.out_of_range, counts.late), (4, 1));
    assert_eq!((counts.events, counts.dropped()), (6, 0));

    // The one inside the limit is kept for its moment.
    assert_eq!(
        read_with_time_tags(&mut reader, in_range),
        [(0, 2, Some(in_range))]
    );
}

#[test]
fn a_bundle_inside_a_bundle_follows_its_own_time_but_not_an_earlier_one() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let arrival = at_frame(-240.0);
    let outer = OscBundle::new(at_frame(100.0))
        .with(note(1))
        .with(OscBundle::new(at_frame(200.0)).with(note(2)))
        // Earlier than the bundle around it, which is not allowed: it
        // takes the outer time.
        .with(OscBundle::new(at_frame(50.0)).with(note(3)))
        .with(OscBundle::new(OscTime::IMMEDIATELY).with(note(4)))
        .with(note(5));
    assert_eq!(writer.push_packet(&bytes(outer), arrival), Ok(5));
    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [(100, 1), (100, 3), (100, 4), (100, 5), (200, 2)]
    );

    // A bundle with a time inside one without.
    let outer = OscBundle::default()
        .with(note(6))
        .with(OscBundle::new(at_frame(780.0)).with(note(7)));
    assert_eq!(writer.push_packet(&bytes(outer), at_frame(300.0)), Ok(2));
    assert_eq!(
        read(&mut reader, at_frame(480.0), BLOCK),
        [(0, 6), (300, 7)]
    );
    assert_eq!(writer.stats().snapshot().late, 0);
}

#[test]
fn events_come_out_in_time_order_and_ties_in_the_order_sent() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let arrival = at_frame(-240.0);
    writer
        .push_packet(&tagged(at_frame(50.0), &[1, 2, 3]), arrival)
        .unwrap();
    writer
        .push_packet(&tagged(at_frame(50.0), &[4, 5]), arrival)
        .unwrap();
    writer
        .push_packet(&tagged(at_frame(20.0), &[6]), arrival)
        .unwrap();
    writer.push_packet(&bytes(note(7)), arrival).unwrap();
    writer
        .push_packet(&tagged(at_frame(50.0), &[8]), arrival)
        .unwrap();
    assert_eq!(
        read(&mut reader, at_frame(0.0), BLOCK),
        [
            (0, 7),
            (20, 6),
            (50, 1),
            (50, 2),
            (50, 3),
            (50, 4),
            (50, 5),
            (50, 8)
        ]
    );
}

#[test]
fn a_message_reaches_every_handler_its_pattern_matches() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.add("/voice/1/gain", 10).unwrap();
    dispatcher.add("/voice/2/gain", 20).unwrap();
    dispatcher.add("/voice/2/pan", 21).unwrap();
    dispatcher.add("/voice/1/gain", 11).unwrap();
    let (mut writer, mut reader) = bridge::channel(dispatcher, BridgeConfig::default());
    assert_eq!(writer.dispatcher().len(), 4);

    let arrival = origin();
    let push = |writer: &mut BridgeWriter, message: OscMessage| {
        writer.push_packet(&bytes(message), arrival)
    };
    assert_eq!(
        push(&mut writer, OscMessage::new("/voice/*/gain").with(0.5f32)),
        Ok(3)
    );
    assert_eq!(
        push(&mut writer, OscMessage::new("/voice/2/pan").with(-1.0f32)),
        Ok(1)
    );
    assert_eq!(
        push(&mut writer, OscMessage::new("//pan").with(0.25f32)),
        Ok(1)
    );
    // Nobody listens to these: an address that is not registered, and a
    // pattern that is not well formed.
    assert_eq!(
        push(&mut writer, OscMessage::new("/voice/3/gain").with(1.0f32)),
        Ok(0)
    );
    assert_eq!(
        push(&mut writer, OscMessage::new("/voice/[").with(1.0f32)),
        Ok(0)
    );

    let mut seen = Vec::new();
    reader.read_at(arrival, BLOCK, RATE, |_, event| {
        seen.push((
            event.handler(),
            event.args().next().unwrap().as_f32().unwrap(),
        ));
    });
    assert_eq!(
        seen,
        [(10, 0.5), (20, 0.5), (11, 0.5), (21, -1.0), (21, 0.25)]
    );

    let counts = writer.stats().snapshot();
    assert_eq!((counts.packets, counts.events, counts.unmatched), (5, 5, 2));
    assert_eq!(counts.malformed, 0);
}

#[test]
fn arguments_arrive_as_sent_whether_stored_in_the_event_or_in_the_pool() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let long_text = "x".repeat(200);
    let small = OscMessage::new("/n")
        .with(7)
        .with(0.5f32)
        .with("hi")
        .with(true)
        .with(vec![OscArg::Double(2.0)]);
    let large = OscMessage::new("/n")
        .with(8)
        .with(long_text.as_str())
        .with(vec![9u8; 100]);
    writer.push_packet(&bytes(small), origin()).unwrap();
    writer.push_packet(&bytes(large), origin()).unwrap();

    let mut seen = 0;
    reader.read_at(origin(), BLOCK, RATE, |_, event| {
        let args: Vec<OscArgRef<'_>> = event.args().collect();
        match id_of(event) {
            7 => {
                assert!(!event.is_pooled());
                assert_eq!(event.type_tags(), "ifsT[d]");
                assert_eq!(
                    args,
                    [
                        OscArgRef::Int(7),
                        OscArgRef::Float(0.5),
                        OscArgRef::String("hi"),
                        OscArgRef::Bool(true),
                        OscArgRef::ArrayStart,
                        OscArgRef::Double(2.0),
                        OscArgRef::ArrayEnd,
                    ]
                );
            }
            8 => {
                assert!(event.is_pooled());
                assert_eq!(event.type_tags(), "isb");
                assert_eq!(
                    args,
                    [
                        OscArgRef::Int(8),
                        OscArgRef::String(&long_text),
                        OscArgRef::Blob(&[9; 100]),
                    ]
                );
            }
            other => panic!("unexpected event {other}"),
        }
        assert!(format!("{event:?}").contains("handler: 1"));
        seen += 1;
    });
    assert_eq!(seen, 2);
}

/// A note with a blob that makes its type tags and argument bytes add up
/// to `4 + 12 + 4 + blob_len` rounded up to four.
fn note_with_blob(id: i32, blob_len: usize) -> OscMessage {
    OscMessage::new("/n")
        .with(id)
        .with(0)
        .with(0)
        .with(vec![0xABu8; blob_len])
}

#[test]
fn sixty_four_bytes_fit_in_an_event_and_one_more_needs_the_pool() {
    assert_eq!(OscEvent::INLINE_LEN, 64);
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    // Four tags, three integers, a size and 44 bytes: 64. One byte more
    // is padded to 48, which makes 68.
    writer
        .push_packet(&bytes(note_with_blob(1, 44)), origin())
        .unwrap();
    writer
        .push_packet(&bytes(note_with_blob(2, 45)), origin())
        .unwrap();
    let mut pooled = Vec::new();
    reader.read_at(origin(), BLOCK, RATE, |_, event| {
        let blob = event.args().last().unwrap().as_blob().unwrap();
        assert!(blob.iter().all(|&byte| byte == 0xAB));
        pooled.push((id_of(event), blob.len(), event.is_pooled()));
    });
    assert_eq!(pooled, [(1, 44, false), (2, 45, true)]);
}

#[test]
fn a_message_too_large_for_the_pool_is_dropped_and_counted() {
    let config = BridgeConfig::default()
        .with_pool_buffers(2)
        .with_pool_buffer_len(128);
    let (mut writer, mut reader) = setup(config);
    let stats = writer.stats();
    let arrival = origin();
    // 4 + 12 + 4 + 108 is exactly the 128 bytes a buffer holds.
    let fits = |id| bytes(note_with_blob(id, 108));
    let too_large = bytes(note_with_blob(0, 109));

    assert_eq!(writer.push_packet(&too_large, arrival), Ok(0));
    assert_eq!(stats.snapshot().dropped_too_large, 1);

    // Two buffers, three messages that need one.
    assert_eq!(writer.push_packet(&fits(1), arrival), Ok(1));
    assert_eq!(writer.push_packet(&fits(2), arrival), Ok(1));
    assert_eq!(writer.push_packet(&fits(3), arrival), Ok(0));
    assert_eq!(stats.snapshot().dropped_pool_empty, 1);
    // A message that needs no buffer is not held up by that.
    assert_eq!(writer.push_packet(&bytes(note(4)), arrival), Ok(1));
    assert_eq!(read(&mut reader, arrival, BLOCK), [(0, 1), (0, 2), (0, 4)]);

    // The audio side hands each buffer back when it is done with it, so the
    // pool does not run dry however long this goes on.
    for round in 0..1_000 {
        assert_eq!(writer.push_packet(&fits(round), arrival), Ok(1));
        assert_eq!(writer.push_packet(&fits(-round), arrival), Ok(1));
        assert_eq!(read(&mut reader, arrival, BLOCK), [(0, round), (0, -round)]);
    }
    let counts = stats.snapshot();
    assert_eq!(counts.events, 3 + 2_000);
    assert_eq!(
        (counts.dropped_too_large, counts.dropped_pool_empty),
        (1, 1)
    );
    assert_eq!(counts.dropped(), 2);
}

#[test]
fn without_a_pool_only_what_fits_in_an_event_gets_through() {
    let config = BridgeConfig::default().with_pool_buffers(0);
    let (mut writer, mut reader) = setup(config);
    assert_eq!(
        writer.push_packet(&bytes(note_with_blob(1, 44)), origin()),
        Ok(1)
    );
    assert_eq!(
        writer.push_packet(&bytes(note_with_blob(2, 45)), origin()),
        Ok(0)
    );
    assert_eq!(read(&mut reader, origin(), BLOCK), [(0, 1)]);
    assert_eq!(writer.stats().snapshot().dropped_pool_empty, 1);
}

#[test]
fn a_full_queue_drops_and_counts_and_keeps_its_buffers() {
    let config = BridgeConfig::default().with_queue_capacity(4);
    let (mut writer, mut reader) = setup(config);
    let results: Vec<_> = (0..6)
        .map(|id| writer.push_packet(&bytes(note(id)), origin()))
        .collect();
    assert_eq!(results, [Ok(1), Ok(1), Ok(1), Ok(1), Ok(0), Ok(0)]);
    assert_eq!(writer.stats().snapshot().dropped_queue_full, 2);
    assert_eq!(
        read(&mut reader, origin(), BLOCK),
        [(0, 0), (0, 1), (0, 2), (0, 3)]
    );

    // An event that needed a pool buffer and did not get into the queue
    // gives the buffer back at once.
    let config = BridgeConfig::default()
        .with_queue_capacity(1)
        .with_pool_buffers(2);
    let (mut writer, mut reader) = setup(config);
    let large = |id| bytes(note_with_blob(id, 100));
    for round in 0..50 {
        assert_eq!(writer.push_packet(&large(round), origin()), Ok(1));
        assert_eq!(writer.push_packet(&large(-1), origin()), Ok(0));
        assert_eq!(read(&mut reader, origin(), BLOCK), [(0, round)]);
    }
    let counts = writer.stats().snapshot();
    assert_eq!(
        (counts.dropped_queue_full, counts.dropped_pool_empty),
        (50, 0)
    );
}

#[test]
fn the_audio_side_holds_no_more_than_it_was_built_for() {
    let config = BridgeConfig::default()
        .with_pending_capacity(4)
        .with_pool_buffers(2);
    let (mut writer, mut reader) = setup(config);
    let arrival = origin();
    for id in 0..6 {
        let packet = tagged(at_frame(10_000.0 + f64::from(id)), &[id]);
        assert_eq!(writer.push_packet(&packet, arrival), Ok(1));
    }
    assert_eq!(read(&mut reader, at_frame(0.0), BLOCK), []);
    assert_eq!(reader.pending(), 4);
    assert_eq!(writer.stats().snapshot().dropped_pending_full, 2);
    assert_eq!(
        read(&mut reader, at_frame(9_600.0), BLOCK),
        [(400, 0), (401, 1), (402, 2), (403, 3)]
    );

    // A dropped event that held a pool buffer gives it back.
    for id in 0..4 {
        let packet = tagged(at_frame(20_000.0), &[id]);
        assert_eq!(writer.push_packet(&packet, arrival), Ok(1));
    }
    for round in 0..50 {
        let large = OscBundle::new(at_frame(20_000.0)).with(note_with_blob(round, 100));
        assert_eq!(writer.push_packet(&bytes(large), arrival), Ok(1), "{round}");
        assert_eq!(read(&mut reader, at_frame(0.0), BLOCK), []);
    }
    let counts = writer.stats().snapshot();
    assert_eq!(
        (counts.dropped_pending_full, counts.dropped_pool_empty),
        (52, 0)
    );
}

#[test]
fn a_block_of_zero_frames_hands_nothing_over() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    writer.push_packet(&bytes(note(1)), origin()).unwrap();
    assert_eq!(read(&mut reader, origin(), 0), []);
    assert_eq!(reader.pending(), 1);
    assert_eq!(read(&mut reader, origin(), 1), [(0, 1)]);
}

#[test]
fn clearing_discards_what_has_not_been_handed_over() {
    let config = BridgeConfig::default().with_pool_buffers(1);
    let (mut writer, mut reader) = setup(config);
    let arrival = origin();
    // One waiting for a later block, one still in the queue, one with the
    // only pool buffer.
    writer
        .push_packet(&tagged(at_frame(5_000.0), &[1]), arrival)
        .unwrap();
    assert_eq!(read(&mut reader, at_frame(0.0), BLOCK), []);
    writer.push_packet(&bytes(note(2)), arrival).unwrap();
    writer
        .push_packet(&bytes(note_with_blob(3, 100)), arrival)
        .unwrap();

    reader.clear();
    assert_eq!(reader.pending(), 0);
    assert_eq!(read(&mut reader, at_frame(4_800.0), BLOCK), []);
    // The buffer came back.
    assert_eq!(
        writer.push_packet(&bytes(note_with_blob(4, 100)), arrival),
        Ok(1)
    );
    assert_eq!(read(&mut reader, at_frame(4_800.0), BLOCK), [(0, 4)]);
}

#[test]
fn scheduling_carries_on_across_the_2036_rollover() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    // A sixteenth of a second before the seconds of a time tag wrap to 0.
    let block_start = OscTime::new(u32::MAX, 0xF000_0000);
    let arrival = block_start - Duration::from_millis(1);
    let after = block_start + Duration::from_millis(250);
    // As plain numbers, the later time is the smaller one.
    assert!(after < block_start);
    assert_eq!(after.seconds(), 0);

    writer.push_packet(&tagged(after, &[1]), arrival).unwrap();
    writer
        .push_packet(&tagged(block_start - Duration::from_secs(1), &[2]), arrival)
        .unwrap();
    // One block of a second, which contains the rollover.
    assert_eq!(
        read(&mut reader, block_start, 48_000),
        [(0, 2), (12_000, 1)]
    );
    let counts = writer.stats().snapshot();
    assert_eq!((counts.late, counts.out_of_range), (1, 0));
}

#[test]
fn packets_that_are_not_osc_are_counted_and_change_nothing() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    let mut random = Random(0x00BA_D0BA_D0BA_D001);
    let mut refused = 0u64;
    for _ in 0..20_000 {
        let len = random.below(17) * 4 + usize::from(random.below(6) == 0);
        let mut garbage: Vec<u8> = (0..len)
            .map(|_| b"/#,n\0ifsb[]\x01\xffbundle"[random.below(19)])
            .collect();
        if len >= 8 && random.below(3) == 0 {
            garbage[..8].copy_from_slice(b"#bundle\0");
        }
        if writer.push_packet(&garbage, origin()).is_err() {
            refused += 1;
        }
    }
    let counts = writer.stats().snapshot();
    assert_eq!(counts.packets, 20_000);
    assert_eq!(counts.malformed, refused);
    assert!(refused > 10_000);

    // Whatever got through by chance is drained, and the bridge still
    // works.
    reader.clear();
    writer.push_packet(&bytes(note(1)), origin()).unwrap();
    assert_eq!(read(&mut reader, origin(), BLOCK), [(0, 1)]);
}

/// Reads blocks until `wanted` events have come out, and returns their
/// numbers. Gives up after ten seconds, which is thousands of times longer
/// than a packet takes to cross the loopback interface.
fn collect(reader: &mut BridgeReader, wanted: usize) -> Vec<i32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ids = Vec::new();
    while ids.len() < wanted && Instant::now() < deadline {
        // A block that starts an hour from now: whatever has arrived is
        // overdue, so that nothing here depends on the clock's small print.
        let block_start = OscTime::now() + Duration::from_secs(3_600);
        reader.read_at(block_start, BLOCK, RATE, |frame, event| {
            assert_eq!(frame, 0);
            ids.push(id_of(event));
        });
        std::thread::sleep(Duration::from_millis(1));
    }
    ids
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_thread_on_a_udp_socket_feeds_the_audio_side() {
    let receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    let (writer, mut reader) = setup(BridgeConfig::default());
    let stats = writer.stats();
    let thread = BridgeThread::udp(receiver, writer).unwrap();
    assert!(thread.is_running());

    let mut sender = UdpSender::new(address).unwrap();
    // One at a time, each awaited before the next is sent, so that the
    // test does not lean on datagrams keeping their order.
    for id in 1..=3 {
        sender.send(&note(id).into()).unwrap();
        assert_eq!(collect(&mut reader, 1), [id]);
    }
    sender.send_bytes(b"junk").unwrap();
    wait_for("the bad packet to be counted", || {
        stats.snapshot().malformed == 1
    });
    sender.send_message("/n", &[OscArgRef::Int(4)]).unwrap();
    assert_eq!(collect(&mut reader, 1), [4]);
    sender
        .send(&OscBundle::default().with(note(5)).with(note(6)).into())
        .unwrap();
    assert_eq!(collect(&mut reader, 2), [5, 6]);

    // Dropping the handle stops the thread and waits for it.
    drop(thread);
    let counts = stats.snapshot();
    assert_eq!((counts.packets, counts.events, counts.malformed), (6, 6, 1));
    assert_eq!((counts.io_errors, counts.dropped()), (0, 0));
}

#[test]
fn a_thread_on_a_tcp_connection_feeds_the_audio_side() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut sender = TcpSender::connect(listener.local_addr().unwrap()).unwrap();
    let (receiver, _) = TcpReceiver::accept(&listener).unwrap();
    let (writer, mut reader) = setup(BridgeConfig::default());
    let stats = writer.stats();
    let thread = BridgeThread::tcp(receiver, writer).unwrap();

    sender
        .send(&OscBundle::default().with(note(1)).with(note(2)).into())
        .unwrap();
    // Framed correctly, but not a packet.
    sender.send_bytes(b"junk").unwrap();
    sender.send(&note(3).into()).unwrap();
    // Not framed correctly: an escape followed by a byte that cannot
    // follow one.
    let mut raw = sender.stream();
    raw.write_all(&[0xC0, 1, 0xDB, 9, 2, 0xC0]).unwrap();
    sender.send_message("/n", &[OscArgRef::Int(4)]).unwrap();

    assert_eq!(collect(&mut reader, 4), [1, 2, 3, 4]);
    let counts = stats.snapshot();
    assert_eq!((counts.events, counts.malformed), (4, 2));

    // When the sender hangs up, the thread ends by itself.
    assert!(thread.is_running());
    drop(sender);
    wait_for("the thread to end", || !thread.is_running());
    assert_eq!(stats.snapshot().io_errors, 0);
}

/// A receiver that gains or loses an address while it runs replaces the
/// table of its bridge without building a new one: events already queued
/// keep the handler numbers they were given, and packets pushed afterwards
/// are matched against the new table.
#[test]
fn the_table_of_a_bridge_can_be_replaced_while_it_runs() {
    let (mut writer, mut reader) = setup(BridgeConfig::default());
    assert_eq!(writer.push_packet(&bytes(note(1)), origin()), Ok(1));

    let mut table = writer.dispatcher().clone();
    table.add("/m", 2).unwrap();
    assert_eq!(table.remove("/n"), 1);
    let old = writer.set_dispatcher(table);
    assert!(old.iter().eq([("/n", NOTE)]));
    assert!(writer.dispatcher().iter().eq([("/m", 2)]));

    assert_eq!(writer.push_packet(&bytes(note(3)), origin()), Ok(0));
    let to_m = OscMessage::new("/m").with(4);
    assert_eq!(writer.push_packet(&bytes(to_m), origin()), Ok(1));
    let mut seen = Vec::new();
    reader.read_at(origin(), BLOCK, RATE, |_, event| {
        seen.push((event.handler(), id_of(event)));
    });
    assert_eq!(seen, [(NOTE, 1), (2, 4)]);
    assert_eq!(writer.stats().snapshot().unmatched, 1);
}

#[test]
fn a_running_bridge_thread_takes_a_new_table_without_stopping() {
    let receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    let (writer, mut reader) = setup(BridgeConfig::default());
    let stats = writer.stats();
    let thread = BridgeThread::udp(receiver, writer).unwrap();
    let mut sender = UdpSender::new(address).unwrap();
    sender.send(&note(1).into()).unwrap();
    assert_eq!(collect(&mut reader, 1), [1]);

    // A table in which "/n" is gone and "/m" has come.
    let mut table = Dispatcher::new();
    table.add("/m", 2).unwrap();
    assert!(thread.set_dispatcher(table));

    // The thread puts the table to use before it reads its next packet. A
    // packet it was already waiting for when the table came may still be
    // matched against the old table, so "/m" is sent, one at a time, until
    // it reaches a handler: that takes one packet or two.
    let mut sent = 0;
    loop {
        let before = stats.snapshot();
        sender.send(&OscMessage::new("/m").with(5).into()).unwrap();
        sent += 1;
        wait_for("the packet to be taken in", || {
            let now = stats.snapshot();
            now.events + now.unmatched > before.events + before.unmatched
        });
        if stats.snapshot().events > before.events {
            break;
        }
        assert!(sent < 2, "the new table was not put to use");
    }
    assert_eq!(collect(&mut reader, 1), [5]);

    // From now on "/n" reaches nobody.
    let before = stats.snapshot();
    sender.send(&note(6).into()).unwrap();
    wait_for("the packet to be counted as unmatched", || {
        stats.snapshot().unmatched > before.unmatched
    });
    assert_eq!(stats.snapshot().events, before.events);
    drop(thread);

    // A thread that has ended cannot take a table.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let sender = TcpSender::connect(listener.local_addr().unwrap()).unwrap();
    let (receiver, _) = TcpReceiver::accept(&listener).unwrap();
    let (writer, _reader) = setup(BridgeConfig::default());
    let thread = BridgeThread::tcp(receiver, writer).unwrap();
    drop(sender);
    wait_for("the thread to end", || !thread.is_running());
    assert!(!thread.set_dispatcher(Dispatcher::new()));
}

/// Dropping a `BridgeThread` asks it to stop and waits for it. An idle
/// thread notices within its polling interval, about a twentieth of a
/// second. A busy one finishes the packet in hand first, and for packets
/// built to make matching expensive the match budget bounds that. The
/// limits asserted are far above what either takes, so that the test only
/// tells a bounded wait from an unbounded one and does not depend on the
/// machine.
#[test]
fn dropping_a_bridge_thread_waits_a_bounded_time_even_while_it_is_busy() {
    let receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    let (writer, _reader) = setup(BridgeConfig::default());
    let idle = BridgeThread::udp(receiver, writer).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    drop(idle);
    let idle_took = started.elapsed();

    let mut dispatcher = Dispatcher::new();
    for voice in 0..200 {
        let address = format!("/synth/voice/{voice}/filter/cutoff/frequency");
        dispatcher.add(&address, voice).unwrap();
    }
    let receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    let (writer, _reader) = bridge::channel(dispatcher, BridgeConfig::default());
    let stats = writer.stats();
    let busy = BridgeThread::udp(receiver, writer).unwrap();
    // Seven patterns under the length limit, each of which costs millions
    // of units against these addresses. Kept under the 9,216 bytes some
    // systems allow in one datagram.
    let pattern = format!("//*{}x", "{,}".repeat(330));
    let mut bundle = OscBundle::default();
    for _ in 0..7 {
        bundle = bundle.with(OscMessage::new(pattern.as_str()));
    }
    let packet = bytes(bundle);
    assert!(packet.len() < 8_000);
    let sender = UdpSender::new(address).unwrap();
    sender.send_bytes(&packet).unwrap();
    for _ in 0..20 {
        // The system may refuse some of a burst; one is enough.
        let _ = sender.send_bytes(&packet);
    }
    wait_for("a packet to be refused", || {
        stats.snapshot().refused_patterns >= 1
    });
    let started = Instant::now();
    drop(busy);
    let busy_took = started.elapsed();
    println!(
        "dropping took {idle_took:?} when idle and {busy_took:?} when busy, after {} \
         packets were refused",
        stats.snapshot().refused_patterns
    );
    assert!(idle_took < Duration::from_secs(2), "{idle_took:?}");
    assert!(busy_took < Duration::from_secs(5), "{busy_took:?}");
}
