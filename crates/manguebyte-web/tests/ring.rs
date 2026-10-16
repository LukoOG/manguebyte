//! The event ring: layout, order, wraparound of the counters, a full ring,
//! and the same bytes as the JavaScript ring.

use manguebyte_web::ring::{RING_HEADER_BYTES, RING_MAGIC, RING_MAX_CAPACITY};
use manguebyte_web::{EventRecord, ParamId, event_ring, event_ring_starting_at, ring_bytes};

/// The scenario `js/tests/ring.test.mjs` plays on the JavaScript ring.
/// Both must end with the bytes in `js/tests/fixtures/ring-golden.hex`.
fn golden_records() -> [EventRecord; 6] {
    [
        EventRecord::note_on(1, 2, 60, 0.5),
        EventRecord::param(
            0x0102_0304_0506_0708,
            513,
            ParamId::from_name("lowpass.cutoff"),
            1234.5,
        ),
        EventRecord::choke(77, 65_535),
        EventRecord::all_notes_off(u64::MAX >> 11, 7),
        EventRecord::note_on(99, 3, 127, 1.0)
            .with_channel(9)
            .with_accent(true),
        EventRecord {
            frame: 5,
            kind: 200,
            flags: 0xFE,
            part: 1,
            note: 255,
            channel: 255,
            velocity: -1.5,
            param: 0xDEAD_BEEF,
            value: -0.0,
        },
    ]
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn the_rust_ring_ends_with_the_golden_bytes() {
    let [a, b, c, d, e, f] = golden_records();
    let (mut producer, mut consumer) = event_ring_starting_at(4, 0xFFFF_FFFE);
    producer.push(a).unwrap();
    producer.push(b).unwrap();
    producer.push(c).unwrap();
    assert_eq!(consumer.pop(), Some(a));
    producer.push(d).unwrap();
    producer.push(e).unwrap();
    assert_eq!(producer.push(f), Err(f));
    assert_eq!(consumer.pop(), Some(b));
    let bytes = producer.to_bytes();
    let golden: String = include_str!("../js/tests/fixtures/ring-golden.hex")
        .split_whitespace()
        .collect();
    assert_eq!(hex(&bytes), golden);
    // Head 0 and tail 3, both wrapped past 2^32.
    assert_eq!(&bytes[0..8], &[0, 0, 0, 0, 3, 0, 0, 0]);
}

#[test]
fn the_layout_is_a_header_then_slots() {
    assert_eq!(RING_HEADER_BYTES, 16);
    assert_eq!(ring_bytes(1), 48);
    assert_eq!(ring_bytes(1024), 16 + 32 * 1024);
    assert_eq!(RING_MAGIC.to_le_bytes(), *b"MBR1");

    let (mut producer, mut consumer) = event_ring(4);
    let empty = producer.to_bytes();
    assert_eq!(empty.len(), ring_bytes(4));
    assert_eq!(
        &empty[..16],
        &[0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, b'M', b'B', b'R', b'1']
    );
    assert!(empty[16..].iter().all(|&byte| byte == 0));

    let records: Vec<EventRecord> = (0..6u8)
        .map(|n| EventRecord::note_on(u64::from(n) * 1000, u16::from(n), 60 + n, 0.25))
        .collect();
    for record in &records[..4] {
        producer.push(*record).unwrap();
    }
    consumer.pop();
    consumer.pop();
    producer.push(records[4]).unwrap();
    producer.push(records[5]).unwrap();
    let bytes = producer.to_bytes();
    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2); // head
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 6); // tail
    // Record n lives in slot n mod 4.
    for (slot, n) in [(0, 4), (1, 5), (2, 2), (3, 3)] {
        let start = 16 + 32 * slot;
        assert_eq!(
            &bytes[start..start + 32],
            &records[n].to_bytes(),
            "slot {slot}"
        );
    }
}

#[test]
fn records_come_out_in_order_across_the_counter_wraparound() {
    for start in [u32::MAX - 2, u32::MAX, 0, u32::MAX - 100] {
        let (mut producer, mut consumer) = event_ring_starting_at(8, start);
        let mut next_in = 0u64;
        let mut next_out = 0u64;
        // Uneven bursts so that every slot and every fill level is used.
        for burst in 0..200u64 {
            for _ in 0..(burst * 7 % 9) {
                if producer
                    .push(EventRecord::note_on(next_in, 0, 1, 1.0))
                    .is_ok()
                {
                    next_in += 1;
                }
            }
            assert_eq!(consumer.len() as u64, next_in - next_out);
            assert_eq!(producer.space() as u64, 8 - (next_in - next_out));
            for _ in 0..(burst * 5 % 7) {
                if let Some(record) = consumer.pop() {
                    assert_eq!(record.frame, next_out);
                    next_out += 1;
                }
            }
        }
        while let Some(record) = consumer.pop() {
            assert_eq!(record.frame, next_out);
            next_out += 1;
        }
        assert_eq!(next_out, next_in);
        assert!(next_in > 300, "only {next_in} records went through");
        assert!(consumer.is_empty());
    }
}

#[test]
fn a_full_ring_refuses_the_record_and_keeps_what_it_holds() {
    let (mut producer, mut consumer) = event_ring(5);
    assert_eq!(producer.capacity(), 8); // rounded up to a power of two
    assert_eq!(consumer.capacity(), 8);
    for frame in 0..8 {
        producer.push(EventRecord::choke(frame, 0)).unwrap();
    }
    assert_eq!(producer.space(), 0);
    let refused = EventRecord::choke(99, 0);
    assert_eq!(producer.push(refused), Err(refused));
    assert_eq!(producer.push(refused), Err(refused));
    assert_eq!(consumer.len(), 8);
    assert_eq!(consumer.pop().map(|record| record.frame), Some(0));
    producer.push(EventRecord::choke(8, 0)).unwrap();
    assert_eq!(producer.push(refused), Err(refused));
    let frames: Vec<u64> = std::iter::from_fn(|| consumer.pop())
        .map(|record| record.frame)
        .collect();
    assert_eq!(frames, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(consumer.pop(), None);
    assert_eq!(producer.space(), 8);

    let (mut producer, mut consumer) = event_ring(1);
    producer.push(EventRecord::choke(1, 0)).unwrap();
    assert!(producer.push(EventRecord::choke(2, 0)).is_err());
    assert_eq!(consumer.pop().map(|record| record.frame), Some(1));
    producer.push(EventRecord::choke(3, 0)).unwrap();
    assert_eq!(consumer.pop().map(|record| record.frame), Some(3));
}

#[test]
#[should_panic(expected = "a ring holds 1 to")]
fn a_ring_of_nothing_is_refused() {
    let _ = event_ring(0);
}

#[test]
#[should_panic(expected = "a ring holds 1 to")]
fn a_ring_too_large_is_refused() {
    let _ = event_ring(RING_MAX_CAPACITY + 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn two_threads_pass_every_record_in_order() {
    const COUNT: u64 = 200_000;
    let (mut producer, mut consumer) = event_ring_starting_at(64, u32::MAX - 1_000);
    let writer = std::thread::spawn(move || {
        let mut frame = 0;
        let mut refused = 0u64;
        while frame < COUNT {
            let record =
                EventRecord::param(frame, frame as u16, ParamId(frame as u32), frame as f64);
            match producer.push(record) {
                Ok(()) => frame += 1,
                Err(_) => {
                    refused += 1;
                    std::thread::yield_now();
                }
            }
        }
        refused
    });
    let mut expected = 0;
    while expected < COUNT {
        match consumer.pop() {
            Some(record) => {
                // Every field arrives whole: no torn slots.
                assert_eq!(
                    record,
                    EventRecord::param(
                        expected,
                        expected as u16,
                        ParamId(expected as u32),
                        expected as f64
                    )
                );
                expected += 1;
            }
            None => std::thread::yield_now(),
        }
    }
    writer.join().unwrap();
    assert_eq!(consumer.pop(), None);
}
