//! Proves that the parts of this crate meant for the audio thread never
//! touch the heap.
//!
//! Packets, translation, parameter numbers, MPE and the keyboard state are
//! all documented as real-time safe. As in `manguebyte-dsp`, the promise
//! is checked rather than trusted: this test replaces the global allocator
//! with one that counts, and fails if any of them allocates, reallocates
//! or frees while it works.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use manguebyte_midi::keyboard::KeyboardState;
use manguebyte_midi::mpe::{ChannelAllocator, Configuration, NoteTracker, Zone, ZoneKind};
use manguebyte_midi::rpn::{Parameter, RpnDecoder, RpnEncoder};
use manguebyte_midi::translate::{
    Midi1ToMidi2, UmpTranslator, midi2_to_midi1, packet_to_midi1, scale_up,
};
use manguebyte_midi::ump::{
    Assembler, FlexAddress, Message, Packet, TextKind, endpoint_name_packets, packets,
    sysex7_packets, sysex8_packets, text_packets,
};
use manguebyte_midi::{MidiMessage, StreamEvent, StreamParser};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and panics if it used the heap in any way.
fn assert_no_heap_use(what: &str, work: impl FnOnce()) {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    let calls = HEAP_CALLS.with(Cell::get);
    assert_eq!(calls, 0, "{what} used the heap {calls} time(s)");
}

/// xorshift64*, for input that covers more than a few hand-picked cases.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
}

/// A stream of channel voice messages that exercises every kind, built
/// before the allocator is watched.
fn performance(len: usize) -> Vec<MidiMessage> {
    let mut random = Random(0x1234_5678_9ABC_DEF1);
    (0..len)
        .map(|_| {
            let bits = random.next();
            let channel = (bits & 0x0F) as u8;
            let a = ((bits >> 8) & 0x7F) as u8;
            let b = ((bits >> 16) & 0x7F) as u8;
            match (bits >> 4) & 0x0F {
                0..=2 => MidiMessage::NoteOn {
                    channel,
                    note: a,
                    velocity: b,
                },
                3..=4 => MidiMessage::NoteOff {
                    channel,
                    note: a,
                    velocity: b,
                },
                5 => MidiMessage::PolyPressure {
                    channel,
                    note: a,
                    pressure: b,
                },
                6..=7 => MidiMessage::ControlChange {
                    channel,
                    controller: a,
                    value: b,
                },
                // Controllers that drive the state machines.
                8..=10 => MidiMessage::ControlChange {
                    channel,
                    controller: [
                        0, 6, 32, 38, 64, 74, 96, 97, 98, 99, 100, 101, 120, 121, 123, 6,
                    ][usize::from(a & 0x0F)],
                    value: if b & 1 == 0 { b } else { b & 0x0F },
                },
                11 => MidiMessage::ProgramChange {
                    channel,
                    program: a,
                },
                12 => MidiMessage::ChannelPressure {
                    channel,
                    pressure: a,
                },
                _ => MidiMessage::PitchBend {
                    channel,
                    value: ((bits >> 8) & 0x3FFF) as i16 - 8192,
                },
            }
        })
        .collect()
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");
}

#[test]
fn packets_are_read_written_and_interpreted_without_the_heap() {
    // Random words: every message type, valid and not.
    let mut random = Random(0x0DDB_1A5E_5BAD_5EED);
    let words: Vec<u32> = (0..4_000).map(|_| random.next()).collect();
    let mut out = vec![0u32; 4];
    let mut decoded = 0usize;
    assert_no_heap_use("reading packets", || {
        for item in packets(&words) {
            let Ok(packet) = item else { continue };
            let _ = packet.write(&mut out);
            let _ = packet.group();
            let _ = packet.to_midi1();
            if let Ok(message) = packet.message() {
                decoded += 1;
                let again = message.to_packet();
                assert!(again.message().is_ok());
            }
        }
    });
    assert!(decoded > 100, "only {decoded} random packets were valid");
}

#[test]
fn long_payloads_are_cut_up_and_put_together_without_the_heap() {
    let payload: Vec<u8> = (0..200u8).map(|byte| byte & 0x7F).collect();
    let name = "An endpoint name that needs more than one packet";
    let lyric = "a syllable longer than twelve bytes";
    let mut assembler = Assembler::new(256);
    let mut received = 0usize;
    assert_no_heap_use("splitting and assembling", || {
        for packet in sysex7_packets(3, &payload).unwrap() {
            if let Ok(Message::SysEx7(piece)) = packet.message() {
                if let Some(whole) = assembler.push(piece.part, piece.data()) {
                    received += whole.len();
                }
            }
        }
        for packet in sysex8_packets(3, 1, &payload) {
            if let Ok(Message::SysEx8(piece)) = packet.message() {
                if let Some(whole) = assembler.push(piece.part, piece.data()) {
                    received += whole.len();
                }
            }
        }
        for packet in text_packets(0, FlexAddress::Group, TextKind::Lyrics, lyric).unwrap() {
            assert!(packet.message().is_ok());
        }
        for packet in endpoint_name_packets(name).unwrap() {
            assert!(packet.message().is_ok());
        }
        // A message too long for the assembler is dropped, not grown into.
        let mut small = [0u8; 13];
        for round in 0..100u8 {
            small.fill(round);
            let _ = assembler.push(manguebyte_midi::ump::Part::Continue, &small);
        }
        assembler.cancel();
    });
    assert_eq!(received, 400);
}

#[test]
fn translation_does_not_use_the_heap() {
    let messages = performance(20_000);
    let mut to_midi2 = Midi1ToMidi2::new();
    let mut packet_translator = UmpTranslator::new();
    let mut produced = 0usize;
    assert_no_heap_use("translation", || {
        for (index, message) in messages.iter().enumerate() {
            for midi2 in to_midi2.translate(*message) {
                produced += midi2_to_midi1(midi2).len();
            }
            if index % 64 == 0 {
                let _ = to_midi2.held();
                let _ = to_midi2.flush();
            }
            let group = (index % 16) as u8;
            let packet = Packet::from_midi1(group, *message);
            for translated in packet_translator.to_midi2(packet) {
                produced += packet_to_midi1(translated).len();
            }
            // The end of a block: everything held is sent.
            if index % 256 == 0 && packet_translator.is_holding() {
                packet_translator.flush_all(|held| produced += packet_to_midi1(held).len());
            }
            if index % 5 == 0 {
                let _ = packet_translator.flush(group);
            }
        }
        for value in 0..=0x3FFFu32 {
            let _ = std::hint::black_box(scale_up(value, 14, 32));
        }
    });
    assert!(produced > 20_000);
}

#[test]
fn parameter_numbers_do_not_use_the_heap() {
    let messages = performance(20_000);
    let mut decoder = RpnDecoder::new();
    let mut encoder = RpnEncoder::new();
    let mut events = 0usize;
    assert_no_heap_use("parameter numbers", || {
        for (index, message) in messages.iter().enumerate() {
            if decoder.feed(*message).is_some() {
                events += 1;
            }
            let channel = message.channel();
            let parameter = if index % 3 == 0 {
                Parameter::PITCH_BEND_SENSITIVITY
            } else {
                Parameter::non_registered((index % 128) as u8, 1)
            };
            for sent in &encoder.set(channel, parameter, (index & 0x3FFF) as u16) {
                if decoder.feed(*sent).is_some() {
                    events += 1;
                }
            }
            let _ = encoder.set_coarse(channel, parameter, 1);
            let _ = encoder.increment(channel, parameter);
            let _ = encoder.deselect(channel);
        }
    });
    assert!(events > 40_000);
}

#[test]
fn mpe_does_not_use_the_heap() {
    let messages = performance(20_000);
    let mut tracker = NoteTracker::<24>::new();
    let mut allocator = ChannelAllocator::new(Zone::lower(5).unwrap());
    let mut events = 0usize;
    assert_no_heap_use("MPE", || {
        // Zones come and go while notes play, with more notes than the
        // tracker has room for.
        for (index, message) in messages.iter().enumerate() {
            if index % 997 == 0 {
                let configuration = Configuration {
                    zone: if index % 2 == 0 {
                        ZoneKind::Lower
                    } else {
                        ZoneKind::Upper
                    },
                    members: (index % 16) as u8,
                };
                for part in &configuration.messages() {
                    tracker.feed(*part, |_| events += 1);
                }
            }
            tracker.feed(*message, |event| {
                std::hint::black_box(event);
                events += 1;
            });
            if let MidiMessage::NoteOn { note, .. } = *message {
                let channel = allocator.note_on(note);
                if index % 3 == 0 {
                    allocator.note_off(channel, note);
                } else if index % 7 == 0 {
                    let _ = allocator.release(note);
                }
            }
        }
        tracker.reset();
        allocator.reset();
    });
    assert!(events > 5_000, "only {events} note events");
}

#[test]
fn the_keyboard_state_does_not_use_the_heap() {
    let messages = performance(20_000);
    let keyboard = KeyboardState::new();
    let mut seen = 0usize;
    assert_no_heap_use("the keyboard state", || {
        for message in &messages {
            keyboard.process(*message);
            let channel = message.channel();
            seen += keyboard.sounding(channel).len();
            seen += keyboard.keys_down_on_any_channel().iter().count();
            let _ = keyboard.is_sustain_down(channel);
            let _ = keyboard.velocity(channel, 60);
        }
        keyboard.reset();
    });
    assert!(seen > 0);
}

#[test]
fn the_stream_parser_still_does_not_use_the_heap_alongside_packets() {
    // A byte stream turned into packets as it arrives: the path from a
    // MIDI 1.0 cable to a MIDI 2.0 consumer.
    let mut bytes = Vec::new();
    for message in performance(5_000) {
        bytes.extend_from_slice(message.to_bytes().as_slice());
    }
    bytes.extend_from_slice(&[0xF0, 0x7E, 0x7F, 0x09, 0x03, 0xF7, 0xF8]);
    let mut parser = StreamParser::new(64);
    let mut translator = UmpTranslator::new();
    let mut words = 0usize;
    assert_no_heap_use("bytes to packets", || {
        parser.feed_slice(&bytes, |event| match event {
            StreamEvent::Channel(message) => {
                for packet in translator.to_midi2(Packet::from_midi1(0, message)) {
                    words += packet.word_count();
                }
            }
            StreamEvent::System(message) => {
                let packet = Message::System { group: 0, message }.to_packet();
                for packet in translator.to_midi2(packet) {
                    words += packet.word_count();
                }
            }
            StreamEvent::SysEx(payload) => {
                if let Ok(pieces) = sysex7_packets(0, payload) {
                    words += pieces.map(|packet| packet.word_count()).sum::<usize>();
                }
            }
        });
    });
    assert!(words > 5_000);
}
