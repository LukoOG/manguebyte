//! What the translator, the parameter decoder, the MPE note tracker and
//! the keyboard state make of the same MIDI streams.
//!
//! Each of them is a state machine that reads controller and note
//! messages, and where two of them read the same message they have to
//! give it the same meaning. The tests in the first part pin down one
//! case each, found in review, where a specification or one of the other
//! state machines said otherwise. The tests in the second part compare
//! the state machines with each other and with plain models over long
//! random streams.
//!
//! Sources named in the comments: "Universal MIDI Packet (UMP) Format and
//! MIDI 2.0 Protocol" v1.1.1 (M2-104-UM), the MIDI 1.0 Detailed
//! Specification, recommended practice RP-015 (response to reset all
//! controllers) and "MIDI Polyphonic Expression" (RP-053 / M1-100-UM).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use manguebyte_midi::MidiMessage;
use manguebyte_midi::keyboard::KeyboardState;
use manguebyte_midi::mpe::{
    ChannelAllocator, Configuration, Note, NoteEvent, NoteTracker, Role, Zone, ZoneKind, ZoneLayout,
};
use manguebyte_midi::rpn::{Change, Parameter, RpnDecoder};
use manguebyte_midi::translate::{Midi1ToMidi2, midi2_to_midi1};
use manguebyte_midi::ump::ChannelVoice2;

fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
    MidiMessage::ControlChange {
        channel,
        controller,
        value,
    }
}

fn on(channel: u8, note: u8, velocity: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel,
        note,
        velocity,
    }
}

fn off(channel: u8, note: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel,
        note,
        velocity: 0,
    }
}

fn lower_zone() -> ZoneLayout {
    let mut layout = ZoneLayout::new();
    layout.configure(Configuration {
        zone: ZoneKind::Lower,
        members: 15,
    });
    layout
}

// ---------------------------------------------------------------------
// Part one: single cases.
// ---------------------------------------------------------------------

/// `KeyboardState` once kept "down" and "sustained" in separate atomic
/// words, written one after the other and read one after the other. A key
/// that moved from one set to the other while a reader was between its
/// two loads was in neither: a key that had been sounding without a break
/// read as silent, in a sixth to a third of the reads of this test. Both
/// states of a key now share a word.
#[test]
fn a_key_held_by_the_pedal_never_reads_as_silent() {
    let keyboard = Arc::new(KeyboardState::new());
    let done = Arc::new(AtomicBool::new(false));
    let reading = Arc::new(AtomicBool::new(false));
    // Pedal down, key struck: from here on the key sounds without a
    // break, whether it is held or released, until the pedal lifts.
    keyboard.set_sustain(0, true);
    keyboard.note_on(0, 60, 100);
    let writer = {
        let (keyboard, done) = (Arc::clone(&keyboard), Arc::clone(&done));
        let reading = Arc::clone(&reading);
        std::thread::spawn(move || {
            // Not before the reader is looking.
            while !reading.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            for _ in 0..200_000u32 {
                keyboard.note_off(0, 60);
                keyboard.note_on(0, 60, 100);
                // The pedal lifts and comes down again under the held
                // key, so that the next release starts from a key that is
                // held and nothing more.
                keyboard.set_sustain(0, false);
                keyboard.set_sustain(0, true);
            }
            done.store(true, Ordering::Release);
        })
    };
    let mut reads = 0u64;
    let mut silent = 0u64;
    reading.store(true, Ordering::Release);
    while !done.load(Ordering::Acquire) {
        reads += 2;
        silent += u64::from(!keyboard.is_sounding(0, 60));
        silent += u64::from(!keyboard.sounding(0).contains(60));
    }
    writer.join().unwrap();
    assert!(reads > 0);
    assert_eq!(
        silent, 0,
        "a key that never stopped sounding read as silent in {silent} of {reads} reads"
    );
}

/// A parameter set with a coarse data byte only is held by `Midi1ToMidi2`
/// in case a fine byte follows. Nothing sent after it may overtake it.
/// The MPE configuration message is such a parameter (its fine byte has
/// no function), and so is the usual pitch bend sensitivity message: the
/// notes must not arrive before the zone they belong to.
#[test]
fn a_coarse_only_parameter_is_translated_before_what_follows_it() {
    let mut translator = Midi1ToMidi2::new();
    let mut out = Vec::new();
    let configuration = Configuration {
        zone: ZoneKind::Lower,
        members: 15,
    };
    for message in configuration
        .messages()
        .iter()
        .copied()
        .chain([on(1, 60, 100)])
    {
        out.extend(translator.translate(message));
    }
    let kinds: Vec<&str> = out
        .iter()
        .map(|message| match message {
            ChannelVoice2::RegisteredController {
                bank: 0, index: 6, ..
            } => "configuration",
            ChannelVoice2::NoteOn { .. } => "note",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["configuration", "note"]);
}

/// MIDI 1.0: "If both the MSB and LSB are sent initially, a subsequent
/// fine adjustment only requires the sending of the LSB." `RpnDecoder`
/// keeps the coarse byte when the same parameter is selected again, and
/// never reports a fine byte joined to a coarse byte of zero that nobody
/// sent: 24 semitones and 50 cents must not become 50 cents.
#[test]
fn a_fine_bend_range_byte_keeps_the_semitones() {
    let mut tracker = NoteTracker::<4>::with_layout(lower_zone());
    for message in [
        on(1, 60, 100),
        cc(1, 101, 0),
        cc(1, 100, 0),
        cc(1, 6, 24),
        cc(1, 38, 0),
        // Later: the same parameter selected again, cents adjusted alone.
        cc(1, 101, 0),
        cc(1, 100, 0),
        cc(1, 38, 50),
    ] {
        tracker.feed(message, |_| {});
    }
    assert_eq!(tracker.notes()[0].bend_range_cents, 2450);
}

/// The same stream through `Midi1ToMidi2`: section D.3.3 has the
/// translator keep the latest coarse byte and send when controller 38
/// arrives. A MIDI 1.0 receiver ends at 24 semitones and 50 cents, and so
/// must the MIDI 2.0 receiver behind the translator.
#[test]
fn a_fine_byte_after_selecting_the_same_parameter_again_is_translated() {
    let mut translator = Midi1ToMidi2::new();
    let values: Vec<u32> = [
        cc(1, 101, 0),
        cc(1, 100, 0),
        cc(1, 6, 24),
        cc(1, 38, 0),
        cc(1, 101, 0),
        cc(1, 100, 0),
        cc(1, 38, 50),
    ]
    .into_iter()
    .flat_map(|message| translator.translate(message))
    .filter_map(|message| match message {
        ChannelVoice2::RegisteredController {
            bank: 0,
            index: 0,
            value,
            ..
        } => Some(value >> 18),
        _ => None,
    })
    .collect();
    assert_eq!(values.last(), Some(&((24 << 7) | 50)), "sent {values:?}");
}

/// MIDI 1.0 Detailed Specification, channel mode messages: 124 (omni
/// off), 125 (omni on), 126 (mono on) and 127 (poly on) also act as all
/// notes off.
#[test]
fn mode_messages_124_to_127_release_the_keys_of_the_keyboard_state() {
    for controller in [124u8, 125, 126, 127] {
        let keyboard = KeyboardState::new();
        for message in [on(2, 60, 100), on(2, 64, 100), cc(2, controller, 0)] {
            keyboard.process(message);
        }
        assert!(
            keyboard.keys_down(2).is_empty(),
            "controller {controller}: still down {:?}",
            keyboard.keys_down(2).iter().collect::<Vec<_>>()
        );
    }
}

/// The same for the note tracker on a channel outside every zone, which
/// it documents as "tracked as ordinary MIDI".
#[test]
fn mode_messages_124_to_127_end_the_notes_of_an_ordinary_channel() {
    for controller in [124u8, 125, 126, 127] {
        let mut tracker = NoteTracker::<8>::new();
        for message in [on(5, 60, 100), on(5, 64, 100), cc(5, controller, 0)] {
            tracker.feed(message, |_| {});
        }
        assert!(
            tracker.notes().is_empty(),
            "controller {controller}: {} notes still sounding",
            tracker.notes().len()
        );
    }
}

/// All notes off is a note-off for every note, so under the pedal the
/// notes ring on. `KeyboardState` and `NoteTracker` are two models of one
/// stream and must agree on that.
#[test]
fn all_notes_off_under_the_pedal_means_the_same_to_tracker_and_keyboard() {
    let keyboard = KeyboardState::new();
    let mut tracker = NoteTracker::<8>::new();
    let mut events = Vec::new();
    for message in [cc(3, 64, 127), on(3, 60, 100), cc(3, 123, 0)] {
        keyboard.process(message);
        tracker.feed(message, |event| events.push(event));
    }
    assert!(keyboard.is_sounding(3, 60) && !keyboard.is_down(3, 60));
    assert!(
        tracker.note(3, 60).is_some_and(|note| !note.key_down),
        "the keyboard state keeps the note for the pedal, the tracker reported {events:?}"
    );
    assert!(matches!(
        events[..],
        [NoteEvent::Started(_), NoteEvent::Released(_)]
    ));
    // The pedal lifting ends it in both.
    keyboard.process(cc(3, 64, 0));
    tracker.feed(cc(3, 64, 0), |_| {});
    assert!(!keyboard.is_sounding(3, 60));
    assert!(tracker.notes().is_empty());
}

/// Section 7.4.6 of the UMP specification: control change 0, 6, 32, 38,
/// 98, 99, 100 and 101 are not to be sent in the MIDI 2.0 protocol and
/// receivers of it should ignore them. Passed on to MIDI 1.0, index 6 or
/// 38 would rewrite whatever parameter the device has selected and 98 to
/// 101 would change the selection.
#[test]
fn midi2_controllers_that_stand_for_sequences_do_not_reach_midi1() {
    let passed: Vec<u8> = [0u8, 6, 32, 38, 98, 99, 100, 101]
        .into_iter()
        .filter(|controller| {
            !midi2_to_midi1(ChannelVoice2::ControlChange {
                channel: 0,
                controller: *controller,
                value: 0x8000_0000,
            })
            .is_empty()
        })
        .collect();
    assert!(passed.is_empty(), "passed through to MIDI 1.0: {passed:?}");
}

/// RP-015: reset all controllers sets the parameter number to null, so
/// the data entry after it changes nothing. `RpnDecoder` and
/// `Midi1ToMidi2` must read the stream the same way.
#[test]
fn translator_and_decoder_agree_on_what_reset_all_controllers_does() {
    let stream = [
        cc(0, 101, 0),
        cc(0, 100, 0),
        cc(0, 121, 0),
        cc(0, 6, 12),
        cc(0, 38, 0),
    ];
    let mut decoder = RpnDecoder::new();
    let decoded = stream
        .iter()
        .filter_map(|message| decoder.feed(*message))
        .count();
    let mut translator = Midi1ToMidi2::new();
    let mut translated: Vec<ChannelVoice2> = stream
        .iter()
        .flat_map(|message| translator.translate(*message))
        .collect();
    translated.extend(translator.flush());
    // The reset itself is still a controller in MIDI 2.0.
    assert!(matches!(
        translated[..],
        [ChannelVoice2::ControlChange {
            controller: 121,
            ..
        }]
    ));
    assert_eq!(decoded, 0);
}

/// RP-015 lists the sound controllers 70 to 79 among what reset all
/// controllers does not touch. Controller 74 is one of them, on the
/// member and on the master channel.
#[test]
fn reset_all_controllers_leaves_controller_74_alone() {
    let mut tracker = NoteTracker::<4>::with_layout(lower_zone());
    for message in [on(1, 60, 100), cc(1, 74, 10), cc(0, 74, 20), cc(0, 121, 0)] {
        tracker.feed(message, |_| {});
    }
    let note = tracker.notes()[0];
    assert_eq!((note.timbre, note.master_timbre), (10, 20));
}

/// `Note::id` is documented as "different for every note the tracker has
/// started, until it wraps after four thousand million notes". `reset`
/// must not start the count again.
#[test]
fn note_ids_are_not_reused_after_reset() {
    let mut tracker = NoteTracker::<4>::new();
    let mut ids = Vec::new();
    let mut started = |tracker: &mut NoteTracker<4>, key| {
        tracker.feed(on(0, key, 100), |event| {
            if let NoteEvent::Started(note) = event {
                ids.push(note.id);
            }
        });
    };
    started(&mut tracker, 60);
    tracker.reset();
    started(&mut tracker, 61);
    assert_ne!(ids[0], ids[1], "two different notes were given the same id");
}

/// Two trackers that hold the same notes, zones, channel values and next
/// id are equal, whatever notes each has seen end.
#[test]
fn trackers_in_the_same_state_are_equal() {
    let mut first = NoteTracker::<4>::new();
    let mut second = NoteTracker::<4>::new();
    for message in [on(0, 60, 100), off(0, 60)] {
        first.feed(message, |_| {});
    }
    for message in [on(0, 61, 100), off(0, 61)] {
        second.feed(message, |_| {});
    }
    assert_eq!(first.notes(), second.notes());
    assert_eq!(first.layout(), second.layout());
    assert!(first == second, "equality depends on notes that have ended");
}

// ---------------------------------------------------------------------
// Part two: the state machines against each other and against models.
// ---------------------------------------------------------------------

struct Random(u64);

impl Random {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
}

/// A random message biased towards what drives the state machines: few
/// keys, every channel, pedals, mode messages and parameter numbers.
fn random_message(random: &mut Random) -> MidiMessage {
    let bits = random.next();
    let channel = (bits & 0x0F) as u8;
    let a = ((bits >> 8) & 0x7F) as u8;
    let b = ((bits >> 16) & 0x7F) as u8;
    let key = 60 + (a % 4);
    match (bits >> 4) & 0x0F {
        0..=3 => on(channel, key, b),
        4..=6 => MidiMessage::NoteOff {
            channel,
            note: key,
            velocity: b,
        },
        7 => MidiMessage::PolyPressure {
            channel,
            note: key,
            pressure: b,
        },
        8..=10 => cc(
            channel,
            [
                0,
                6,
                32,
                38,
                64,
                74,
                96,
                97,
                98,
                99,
                100,
                101,
                120,
                121,
                123,
                // One of the four mode messages.
                124 + ((bits >> 24) & 3) as u8,
            ][usize::from(a & 0x0F)],
            if b & 1 == 0 { b } else { b & 0x0F },
        ),
        11 => cc(channel, 64, b),
        12 => MidiMessage::ChannelPressure {
            channel,
            pressure: a,
        },
        13 => cc(channel, 74, a),
        _ => MidiMessage::PitchBend {
            channel,
            value: ((bits >> 8) & 0x3FFF) as i16 - 8192,
        },
    }
}

/// Feeds a random stream, with zones coming and going, and checks after
/// every message that a listener who only follows the events holds
/// exactly the notes the tracker reports.
fn events_and_notes_stay_in_step<const N: usize>(seed: u64, steps: u32) {
    let mut random = Random(0xD1B5_4A32_D192_ED03 ^ (seed * 0x9E37_79B9));
    let mut tracker = NoteTracker::<N>::new();
    let mut live: Vec<Note> = Vec::new();
    for step in 0..steps {
        let message = if random.next() % 211 == 0 {
            let configuration = Configuration {
                zone: if random.next() % 2 == 0 {
                    ZoneKind::Lower
                } else {
                    ZoneKind::Upper
                },
                members: (random.next() % 16) as u8,
            };
            let parts = configuration.messages();
            for part in &parts.as_slice()[..2] {
                tracker.feed(*part, |event| panic!("a selection reported {event:?}"));
            }
            parts.as_slice()[2]
        } else {
            random_message(&mut random)
        };
        tracker.feed(message, |event| match event {
            NoteEvent::Started(note) => live.push(note),
            NoteEvent::Changed(note) | NoteEvent::Released(note) => {
                let slot = live
                    .iter_mut()
                    .find(|slot| slot.id == note.id)
                    .unwrap_or_else(|| panic!("N {N} seed {seed} step {step}: {note:?}"));
                assert_ne!(*slot, note, "reported with nothing changed");
                *slot = note;
            }
            NoteEvent::Ended(note) => {
                let before = live.len();
                live.retain(|slot| slot.id != note.id);
                assert_eq!(live.len() + 1, before, "N {N} seed {seed} step {step}");
            }
            NoteEvent::LayoutChanged(_) => {}
            other => panic!("an event this test does not know: {other:?}"),
        });
        assert_eq!(live, tracker.notes(), "N {N} seed {seed} step {step}");
        assert!(tracker.notes().len() <= N);
        for note in tracker.notes() {
            let zone = match tracker.layout().role(note.channel) {
                Role::Master(kind) | Role::Member(kind) => Some(kind),
                Role::Outside => None,
            };
            assert_eq!(note.zone, zone, "N {N} seed {seed} step {step}");
        }
    }
}

#[test]
fn tracker_events_and_notes_stay_in_step_for_any_capacity() {
    for seed in 1..=8 {
        events_and_notes_stay_in_step::<0>(seed, 1_000);
        events_and_notes_stay_in_step::<1>(seed, 4_000);
        events_and_notes_stay_in_step::<2>(seed, 4_000);
        events_and_notes_stay_in_step::<6>(seed, 4_000);
        events_and_notes_stay_in_step::<17>(seed, 4_000);
    }
}

#[test]
fn the_allocator_never_shares_a_channel_while_one_is_free() {
    // MPE, MIDI mode 3: every new note gets its own channel until there
    // are no unoccupied channels left.
    for members in 1..=15u8 {
        for zone in [Zone::lower(members).unwrap(), Zone::upper(members).unwrap()] {
            let mut allocator = ChannelAllocator::new(zone);
            let mut random = Random(0xABCD_EF01_2345_6789 ^ u64::from(members));
            let mut sounding: Vec<(u8, u8)> = Vec::new();
            for _ in 0..4_000 {
                let key = 40 + (random.next() % 12) as u8;
                if random.next() % 5 < 3 {
                    let free = zone
                        .member_channels()
                        .filter(|channel| allocator.note_count(*channel) == 0)
                        .count();
                    let channel = allocator.note_on(key);
                    assert!(zone.is_member(channel));
                    assert!(free == 0 || !allocator.is_shared(channel));
                    if !sounding.contains(&(channel, key)) {
                        sounding.push((channel, key));
                    }
                } else if let Some(position) = sounding.iter().position(|(_, k)| *k == key) {
                    let (channel, key) = sounding.remove(position);
                    assert!(allocator.note_off(channel, key));
                }
                let total: usize = zone
                    .member_channels()
                    .map(|channel| allocator.note_count(channel))
                    .sum();
                assert_eq!(total, sounding.len());
            }
        }
    }
}

#[test]
fn the_keyboard_state_follows_a_plain_model_on_one_thread() {
    let keyboard = KeyboardState::new();
    let mut down = [[false; 128]; 16];
    let mut held = [[false; 128]; 16];
    let mut velocity = [[0u8; 128]; 16];
    let mut pedal = [false; 16];
    let mut random = Random(0x0123_4567_89AB_CDEF);
    for step in 0..60_000u32 {
        let message = random_message(&mut random);
        keyboard.process(message);
        match message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity: struck,
            } if struck > 0 => {
                let (c, n) = (usize::from(channel), usize::from(note));
                down[c][n] = true;
                held[c][n] = false;
                velocity[c][n] = struck;
            }
            MidiMessage::NoteOn { channel, note, .. }
            | MidiMessage::NoteOff { channel, note, .. } => {
                let (c, n) = (usize::from(channel), usize::from(note));
                held[c][n] |= down[c][n] && pedal[c];
                down[c][n] = false;
            }
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => {
                let c = usize::from(channel);
                match controller {
                    64 | 121 => {
                        pedal[c] = controller == 64 && value >= 64;
                        if !pedal[c] {
                            held[c] = [false; 128];
                        }
                    }
                    120 => {
                        down[c] = [false; 128];
                        held[c] = [false; 128];
                    }
                    // All notes off, alone or as part of a mode message.
                    123..=127 => {
                        for n in 0..128 {
                            held[c][n] |= down[c][n] && pedal[c];
                            down[c][n] = false;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if step % 53 != 0 {
            continue;
        }
        for c in 0..16u8 {
            let ci = usize::from(c);
            assert_eq!(keyboard.is_sustain_down(c), pedal[ci]);
            for n in 0..128u8 {
                let ni = usize::from(n);
                let sounding = down[ci][ni] || held[ci][ni];
                assert_eq!(keyboard.is_down(c, n), down[ci][ni], "step {step}");
                assert_eq!(keyboard.is_sounding(c, n), sounding, "step {step}");
                assert_eq!(keyboard.keys_down(c).contains(n), down[ci][ni]);
                assert_eq!(keyboard.sounding(c).contains(n), sounding);
                let expected = if sounding { velocity[ci][ni] } else { 0 };
                assert_eq!(keyboard.velocity(c, n), expected, "step {step}");
            }
        }
    }
}

/// Outside MPE zones the note tracker and the keyboard state are two
/// readings of the same thing: which keys are held, which the pedal
/// keeps, and how hard each was struck. Over a long random stream of
/// notes, pedals, mode messages and resets they must never differ.
#[test]
fn the_tracker_and_the_keyboard_state_agree_on_ordinary_channels() {
    let keyboard = KeyboardState::new();
    // Four keys on sixteen channels: room for every note there can be.
    let mut tracker = NoteTracker::<64>::new();
    let mut random = Random(0x0005_EED0_FAC0_FFEE);
    let (mut held_by_pedal, mut compared) = (0u32, 0u32);
    for step in 0..80_000u32 {
        let message = random_message(&mut random);
        // No parameter numbers, so that no zone is ever configured: the
        // keyboard state does not know zones.
        if matches!(
            message,
            MidiMessage::ControlChange {
                controller: 98..=101,
                ..
            }
        ) {
            continue;
        }
        keyboard.process(message);
        tracker.feed(message, |_| {});
        if step % 7 != 0 {
            continue;
        }
        for channel in 0..16u8 {
            for key in 60..64u8 {
                let note = tracker.note(channel, key);
                assert_eq!(
                    note.is_some(),
                    keyboard.is_sounding(channel, key),
                    "step {step}, after {message:?}: channel {channel} key {key}"
                );
                assert_eq!(
                    note.is_some_and(|note| note.key_down),
                    keyboard.is_down(channel, key),
                    "step {step}, after {message:?}: channel {channel} key {key}"
                );
                assert_eq!(
                    note.map_or(0, |note| note.velocity),
                    keyboard.velocity(channel, key),
                    "step {step}"
                );
                held_by_pedal += u32::from(note.is_some_and(|note| !note.key_down));
                compared += 1;
            }
            assert_eq!(keyboard.sounding(channel).len(), {
                tracker
                    .notes()
                    .iter()
                    .filter(|note| note.channel == channel)
                    .count()
            });
        }
    }
    assert!(!tracker.layout().is_active());
    // The comparison saw notes held by the pedal, not only held keys.
    assert!(held_by_pedal > 1_000, "{held_by_pedal} of {compared}");
}

/// The parameter decoder and the translator read the same controller
/// stream. Here a MIDI 1.0 receiver is modelled from what the decoder
/// reports, a MIDI 2.0 receiver from what the translator sends, and the
/// two must hold the same value for every parameter after every message.
///
/// Also checked: the translator holds something only directly after a
/// coarse data byte, so that nothing can be sent ahead of a parameter
/// that came before it.
#[test]
fn the_translator_and_the_decoder_tell_the_same_story() {
    let mut random = Random(0x0BAD_5EED_0DDB_A110);
    let mut decoder = RpnDecoder::new();
    let mut translator = Midi1ToMidi2::new();
    // A reset on every channel first: a new decoder takes the parameter
    // numbers as null, a new translator as not yet known, and after the
    // reset both know them to be null.
    for channel in 0..16 {
        assert!(decoder.feed(cc(channel, 121, 0)).is_none());
        assert_eq!(translator.translate(cc(channel, 121, 0)).len(), 1);
    }

    type Values = HashMap<(u8, Parameter), u16>;
    let mut midi1 = Values::new();
    let mut midi2 = Values::new();
    let receive = |values: &mut Values, message: ChannelVoice2| match message {
        ChannelVoice2::RegisteredController {
            channel,
            bank,
            index,
            value,
        } => {
            values.insert(
                (channel, Parameter::registered(bank, index)),
                (value >> 18) as u16,
            );
        }
        ChannelVoice2::AssignableController {
            channel,
            bank,
            index,
            value,
        } => {
            let parameter = Parameter::non_registered(bank, index);
            values.insert((channel, parameter), (value >> 18) as u16);
        }
        _ => {}
    };
    let (mut whole, mut coarse, mut fine_only, mut steps_taken, mut two) = (0, 0, 0, 0, 0);
    for step in 0..60_000u32 {
        let bits = random.next();
        let channel = (bits & 1) as u8;
        let data = ((bits >> 8) & 0x7F) as u8;
        // Few parameter numbers, so that the same ones come round again
        // and the null parameter is selected now and then.
        let half = [0u8, 0, 1, 127][((bits >> 16) & 3) as usize];
        let message = match (bits >> 20) % 40 {
            0..=11 => cc(channel, 98 + ((bits >> 26) & 3) as u8, half),
            12..=21 => cc(channel, 6, data),
            22..=31 => cc(channel, 38, data),
            32..=33 => cc(channel, 96 + ((bits >> 26) & 1) as u8, data),
            34 => cc(channel, 121, 0),
            35..=36 => cc(channel, 7, data),
            37 => cc(channel, 0, data),
            _ => on(channel, 60, 100),
        };
        let is_coarse = matches!(message, MidiMessage::ControlChange { controller: 6, .. });

        let sent = translator.translate(message);
        two += u32::from(sent.len() == 2);
        for message in sent {
            receive(&mut midi2, message);
        }
        if let Some(event) = decoder.feed(message) {
            let key = (event.channel, event.parameter);
            match event.change {
                Change::Value { value, fine } => {
                    midi1.insert(key, value);
                    whole += u32::from(fine);
                    coarse += u32::from(!fine);
                }
                // Half a value, or a step of the receiver's own choosing:
                // the stream no longer says what the value is, and the
                // translator sends nothing for it.
                Change::FineOnly(_) | Change::Increment(_) | Change::Decrement(_) => {
                    midi1.remove(&key);
                    midi2.remove(&key);
                    fine_only += u32::from(matches!(event.change, Change::FineOnly(_)));
                    steps_taken += u32::from(!matches!(event.change, Change::FineOnly(_)));
                }
            }
        }

        assert!(
            translator.held().is_none() || is_coarse,
            "step {step}: something is held after {message:?}"
        );
        // What the translator still holds will be sent before anything
        // else, so it counts as received.
        let mut behind_the_translator = midi2.clone();
        if let Some(held) = translator.held() {
            receive(&mut behind_the_translator, held);
        }
        assert_eq!(
            midi1, behind_the_translator,
            "step {step}, after {message:?}"
        );
    }
    // The stream exercised every path.
    assert!(
        whole > 1_000 && coarse > 1_000,
        "{whole} whole, {coarse} coarse"
    );
    assert!(
        fine_only > 100 && steps_taken > 100,
        "{fine_only} fine only, {steps_taken} steps"
    );
    assert!(
        two > 1_000,
        "{two} messages sent a held byte ahead of themselves"
    );
    assert!(midi1.len() > 4, "{} parameters", midi1.len());
}
