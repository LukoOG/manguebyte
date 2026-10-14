//! Messages from any thread to the main thread, without locks.
//!
//! A plugin may call the host's log function from any thread, the audio
//! thread included, so the message cannot be put in a `String` or behind a
//! mutex. The wait-free queue in `manguebyte-core` has one producer; a log
//! has as many as the plugin has threads.
//!
//! So messages go into a fixed set of slots. A writer claims an empty slot
//! with one compare-and-swap, fills it and marks it full; the main thread
//! claims full slots the same way, reads them and marks them empty. The
//! text itself is stored in atomics, eight bytes to a word, so there is no
//! `unsafe` code here at all: the worst a bug could do is garble a message.
//!
//! Messages come out in the order their slots were claimed, across drains:
//! one still being written holds back the ones after it until a later
//! drain. `manguebyte_core::log::LogQueue` is the same design for plugins'
//! own use; this one keeps CLAP's seven severities and text that may not be
//! UTF-8 until it is read.

use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Longest message kept, in bytes. Longer ones are cut.
pub(crate) const MESSAGE_BYTES: usize = 240;

const WORDS: usize = MESSAGE_BYTES / 8;

/// How many messages can wait for the main thread at once.
const SLOTS: usize = 64;

const EMPTY: u8 = 0;
const WRITING: u8 = 1;
const FULL: u8 = 2;
const READING: u8 = 3;

struct Slot {
    state: AtomicU8,
    severity: AtomicI32,
    length: AtomicU32,
    /// Position of the message among all messages ever posted.
    order: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            severity: AtomicI32::new(0),
            length: AtomicU32::new(0),
            order: AtomicU64::new(0),
            words: [const { AtomicU64::new(0) }; WORDS],
        }
    }
}

/// Messages taken from their slots and numbered after one that was still
/// being written, and the number of the next message to hand out.
type Held = (u64, Vec<(u64, i32, String)>);

/// A fixed set of message slots that any thread may write to.
pub(crate) struct Mailbox {
    slots: [Slot; SLOTS],
    next: AtomicU64,
    lost: AtomicU32,
    /// Kept by the reading side only; writers never take it.
    held: Mutex<Held>,
}

impl Mailbox {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const { Slot::new() }; SLOTS],
            next: AtomicU64::new(0),
            lost: AtomicU32::new(0),
            held: Mutex::new((0, Vec::new())),
        }
    }

    /// Leaves a message for the main thread.
    ///
    /// Wait-free and free of allocation: at most one pass over the slots.
    /// If every slot is taken the message is counted as lost.
    pub(crate) fn post(&self, severity: i32, text: &[u8]) {
        let text = &text[..text.len().min(MESSAGE_BYTES)];
        for slot in &self.slots {
            if slot
                .state
                .compare_exchange(EMPTY, WRITING, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            slot.severity.store(severity, Ordering::Relaxed);
            slot.length.store(text.len() as u32, Ordering::Relaxed);
            slot.order
                .store(self.next.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
            for (word, chunk) in slot.words.iter().zip(text.chunks(8)) {
                let mut bytes = [0u8; 8];
                bytes[..chunk.len()].copy_from_slice(chunk);
                word.store(u64::from_le_bytes(bytes), Ordering::Relaxed);
            }
            // Publishes everything stored above to whoever claims the slot.
            slot.state.store(FULL, Ordering::Release);
            return;
        }
        self.lost.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes the waiting messages, oldest first, as `(severity, text)`, and
    /// the number of messages lost since the last call. Allocates.
    ///
    /// A message still being written holds back the ones after it, which
    /// come out with it in a later call.
    pub(crate) fn drain(&self) -> (Vec<(i32, String)>, u32) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let (next, waiting) = &mut *held;
        let mut found = std::mem::take(waiting);
        for slot in &self.slots {
            if slot
                .state
                .compare_exchange(FULL, READING, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let length = (slot.length.load(Ordering::Relaxed) as usize).min(MESSAGE_BYTES);
            let mut bytes = [0u8; MESSAGE_BYTES];
            for (chunk, word) in bytes.chunks_mut(8).zip(&slot.words) {
                chunk.copy_from_slice(&word.load(Ordering::Relaxed).to_le_bytes());
            }
            found.push((
                slot.order.load(Ordering::Relaxed),
                slot.severity.load(Ordering::Relaxed),
                String::from_utf8_lossy(&bytes[..length]).into_owned(),
            ));
            slot.state.store(EMPTY, Ordering::Release);
        }
        found.sort_by_key(|&(order, _, _)| order);
        // Every number handed out belongs to a message that will be
        // published, so the run without gaps can go and the rest waits.
        let mut ready = 0;
        while found
            .get(ready)
            .is_some_and(|&(order, _, _)| order == *next)
        {
            *next += 1;
            ready += 1;
        }
        *waiting = found.split_off(ready);
        let messages = found
            .into_iter()
            .map(|(_, severity, text)| (severity, text))
            .collect();
        (messages, self.lost.swap(0, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_out_whole_and_in_order() {
        let mailbox = Mailbox::new();
        mailbox.post(1, b"first");
        mailbox.post(3, b"");
        mailbox.post(2, "third, with an accent: \u{e7}".as_bytes());
        let (messages, lost) = mailbox.drain();
        assert_eq!(
            messages,
            [
                (1, "first".to_owned()),
                (3, String::new()),
                (2, "third, with an accent: \u{e7}".to_owned()),
            ]
        );
        assert_eq!(lost, 0);
        assert!(mailbox.drain().0.is_empty(), "a message was read twice");
    }

    #[test]
    fn a_long_message_is_cut_and_a_full_box_counts_what_it_drops() {
        let mailbox = Mailbox::new();
        let long = vec![b'x'; MESSAGE_BYTES + 100];
        mailbox.post(0, &long);
        let (messages, _) = mailbox.drain();
        assert_eq!(messages[0].1.len(), MESSAGE_BYTES);
        assert!(messages[0].1.bytes().all(|byte| byte == b'x'));

        for index in 0..SLOTS + 5 {
            mailbox.post(index as i32, b"flood");
        }
        let (messages, lost) = mailbox.drain();
        assert_eq!(messages.len(), SLOTS);
        assert_eq!(lost, 5);
        // The ones that got in are the first ones, in order.
        assert!(
            messages
                .iter()
                .enumerate()
                .all(|(index, message)| message.0 == index as i32)
        );
        // And the slots are free again.
        mailbox.post(7, b"after");
        assert_eq!(mailbox.drain(), (vec![(7, "after".to_owned())], 0));
    }

    #[test]
    fn a_message_still_being_written_holds_back_the_ones_after_it() {
        let mailbox = Mailbox::new();
        // A writer has claimed the first slot and taken the first number,
        // and has not finished.
        let slot = &mailbox.slots[0];
        slot.state.store(WRITING, Ordering::Relaxed);
        let order = mailbox.next.fetch_add(1, Ordering::Relaxed);
        mailbox.post(2, b"later");
        assert_eq!(mailbox.drain(), (Vec::new(), 0), "held back, not lost");
        // It finishes.
        slot.severity.store(1, Ordering::Relaxed);
        slot.length.store(7, Ordering::Relaxed);
        slot.order.store(order, Ordering::Relaxed);
        slot.words[0].store(u64::from_le_bytes(*b"earlier_"), Ordering::Relaxed);
        slot.state.store(FULL, Ordering::Release);
        assert_eq!(
            mailbox.drain(),
            (vec![(1, "earlier".to_owned()), (2, "later".to_owned())], 0)
        );
    }

    #[test]
    fn several_threads_can_post_at_once() {
        const THREADS: usize = 4;
        const EACH: usize = if cfg!(miri) { 20 } else { 2_000 };
        let mailbox = std::sync::Arc::new(Mailbox::new());
        let writers: Vec<_> = (0..THREADS)
            .map(|thread| {
                let mailbox = std::sync::Arc::clone(&mailbox);
                std::thread::spawn(move || {
                    for index in 0..EACH {
                        let text = format!("thread {thread} message {index} padding padding");
                        mailbox.post(thread as i32, text.as_bytes());
                    }
                })
            })
            .collect();

        let mut seen = 0usize;
        let mut lost = 0usize;
        let mut check = |mailbox: &Mailbox| {
            let (messages, dropped) = mailbox.drain();
            lost += dropped as usize;
            for (severity, text) in messages {
                // A message is never a mixture of two.
                let prefix = format!("thread {severity} message ");
                assert!(text.starts_with(&prefix), "garbled message {text:?}");
                assert!(text.ends_with(" padding padding"), "cut message {text:?}");
                seen += 1;
            }
        };
        while writers.iter().any(|writer| !writer.is_finished()) {
            check(&mailbox);
        }
        for writer in writers {
            writer.join().unwrap();
        }
        check(&mailbox);
        assert_eq!(seen + lost, THREADS * EACH);
        assert!(seen > 0);
    }
}
