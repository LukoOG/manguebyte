//! The triple buffer under load from two threads.
//!
//! A writer thread fills a large value one element at a time and publishes
//! it, as fast as it can; a reader thread fetches whatever is newest, as
//! fast as it can. A value is torn if the reader can see one that holds
//! elements of two publications, which these tests would catch: every
//! element of a publication holds the same number.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use manguebyte_analysis::triple_buffer;

/// Elements per value: large enough that writing one takes a while.
const LEN: usize = 512;

/// Publications per test. Fewer under Miri, which is slow.
const ROUNDS: u64 = if cfg!(miri) { 300 } else { 200_000 };

/// Checks that `value` is one whole publication and returns its number.
fn whole(value: &[u64]) -> u64 {
    let first = value[0];
    assert!(
        value.iter().all(|&element| element == first),
        "a torn value: starts with {first}, also holds {:?}",
        value.iter().find(|&&element| element != first)
    );
    first
}

#[test]
fn the_reader_only_ever_sees_whole_values_in_order() {
    let (mut writer, mut reader) = triple_buffer::channel(vec![0u64; LEN]);
    let writing = thread::spawn(move || {
        for round in 1..=ROUNDS {
            for element in writer.pending_mut().iter_mut() {
                *element = round;
            }
            writer.publish();
        }
    });

    let mut last = 0;
    let mut fetched = 0u64;
    let mut fetch = |reader: &mut triple_buffer::Reader<Vec<u64>>| {
        if reader.update() {
            let number = whole(reader.get());
            assert!(number > last, "went back from {last} to {number}");
            last = number;
            fetched += 1;
        } else {
            // What it holds stays whole while nothing new arrives.
            assert_eq!(whole(reader.get()), last);
        }
    };
    // Read until the writer has ended, whether it finished or panicked; a
    // reader that waited for the last value instead would wait for ever
    // after a panic.
    while !writing.is_finished() {
        fetch(&mut reader);
    }
    // The join passes on a panic of the writer, and after it the last
    // value published is there to fetch.
    writing.join().unwrap();
    fetch(&mut reader);
    assert_eq!(last, ROUNDS);
    // Nothing is left to fetch once the last value has been seen.
    assert!(!reader.update());
    assert_eq!(whole(reader.latest()), ROUNDS);
    println!("the reader fetched {fetched} of {ROUNDS} values");
    assert!(fetched > 0);
}

#[test]
fn a_reader_that_reads_slowly_still_sees_whole_values() {
    // The reader spends a long time on each value, so the writer laps it
    // many times and the middle copy changes hands constantly.
    let (mut writer, mut reader) = triple_buffer::channel(vec![0u64; LEN]);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let writing = thread::spawn(move || {
        let mut round = 0u64;
        while !stopped.load(Ordering::Relaxed) {
            round += 1;
            writer.pending_mut().fill(round);
            writer.publish();
        }
        round
    });

    let mut last = 0;
    for _ in 0..if cfg!(miri) { 20 } else { 2_000 } {
        let value = reader.latest();
        // Read every element several times while the writer keeps going.
        for _ in 0..4 {
            let number = whole(value);
            assert!(number >= last);
            last = number;
        }
        thread::yield_now();
    }
    stop.store(true, Ordering::Relaxed);
    let published = writing.join().unwrap();
    assert!(last <= published);
    // After the writer has stopped, the last value it published is there.
    assert_eq!(whole(reader.latest()), published);
}

#[test]
fn either_end_may_be_dropped_while_the_other_is_busy() {
    let (mut writer, reader) = triple_buffer::channel(vec![1u64; 64]);
    let writing = thread::spawn(move || {
        for round in 0..1_000 {
            writer.pending_mut().fill(round);
            writer.publish();
        }
    });
    drop(reader);
    writing.join().unwrap();

    let (writer, mut reader) = triple_buffer::channel(vec![1u64; 64]);
    let reading = thread::spawn(move || {
        for _ in 0..1_000 {
            assert_eq!(whole(reader.latest()), 1);
        }
    });
    drop(writer);
    reading.join().unwrap();
}
