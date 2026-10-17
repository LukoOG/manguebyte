//! `NoteInput`: MPE and MIDI 2.0 reach a processor as the same per-note
//! events.

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event, EventBuffer, EventKind, Expression, NoteRef};
use manguebyte_dsp::note_input::{Mpe, NoteInput};
use manguebyte_dsp::{ProcessContext, ProcessSpec, Processor, Synth, Transport, Voice};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::{Configuration, ZoneKind};
use manguebyte_midi::translate::{scale_7_to_16, scale_7_to_32, scale_14_to_32, scale_up};
use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, Packet};

type Stream = Vec<(u32, In)>;

/// A message of either protocol, on a frame.
#[derive(Debug, Clone, Copy)]
enum In {
    One(MidiMessage),
    Two(ChannelVoice2),
}

fn run(input: &mut NoteInput, stream: &[(u32, In)]) -> Vec<Event> {
    let mut out = Vec::new();
    for &(offset, message) in stream {
        match message {
            In::One(message) => input.midi1(offset, message, |event| out.push(event)),
            In::Two(message) => input.midi2(offset, message, |event| out.push(event)),
        }
    }
    out
}

fn cc(channel: u8, controller: u8, value: u8) -> In {
    In::One(MidiMessage::ControlChange {
        channel,
        controller,
        value,
    })
}

fn on1(channel: u8, note: u8, velocity: u8) -> In {
    In::One(MidiMessage::NoteOn {
        channel,
        note,
        velocity,
    })
}

fn off1(channel: u8, note: u8, velocity: u8) -> In {
    In::One(MidiMessage::NoteOff {
        channel,
        note,
        velocity,
    })
}

fn bend1(channel: u8, value: i16) -> In {
    In::One(MidiMessage::PitchBend { channel, value })
}

fn pressure1(channel: u8, pressure: u8) -> In {
    In::One(MidiMessage::ChannelPressure { channel, pressure })
}

fn on2(channel: u8, note: u8, velocity: u16) -> In {
    In::Two(ChannelVoice2::NoteOn {
        channel,
        note,
        velocity,
        attribute: NoteAttribute::NONE,
    })
}

fn off2(channel: u8, note: u8, velocity: u16) -> In {
    In::Two(ChannelVoice2::NoteOff {
        channel,
        note,
        velocity,
        attribute: NoteAttribute::NONE,
    })
}

fn note_bend2(channel: u8, note: u8, value: u32) -> In {
    In::Two(ChannelVoice2::PerNotePitchBend {
        channel,
        note,
        value,
    })
}

fn poly2(channel: u8, note: u8, pressure: u32) -> In {
    In::Two(ChannelVoice2::PolyPressure {
        channel,
        note,
        pressure,
    })
}

fn per_note2(channel: u8, note: u8, controller: u8, value: u32) -> In {
    In::Two(ChannelVoice2::RegisteredPerNoteController {
        channel,
        note,
        controller,
        value,
    })
}

fn cc2(channel: u8, controller: u8, value: u32) -> In {
    In::Two(ChannelVoice2::ControlChange {
        channel,
        controller,
        value,
    })
}

fn rc2(channel: u8, bank: u8, index: u8, value: u32) -> In {
    In::Two(ChannelVoice2::RegisteredController {
        channel,
        bank,
        index,
        value,
    })
}

fn raw(bend: i16) -> u32 {
    scale_14_to_32((i32::from(bend) + 8192) as u16)
}

/// The per-note events, without the channel-wide MIDI passed alongside.
fn per_note(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .copied()
        .filter(|event| !matches!(event.kind, EventKind::Midi(_)))
        .collect()
}

fn expression_of(events: &[Event], wanted: Expression) -> Vec<(u32, f64)> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::NoteExpression {
                note,
                expression,
                value,
            } if expression == wanted => Some((note.id.unwrap(), value)),
            _ => None,
        })
        .collect()
}

fn starts(events: &[Event]) -> Vec<NoteRef> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect()
}

fn ends(events: &[Event]) -> Vec<NoteRef> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::NoteOff { note, .. } => Some(note),
            _ => None,
        })
        .collect()
}

/// A phrase of two overlapping notes on member channels 2 and 3 of a
/// lower zone, bent, pressed and brightened, with the zone's own wheel,
/// modulation and pedal on the master channel; and the same phrase said
/// in MIDI 2.0, with per-note messages on channel 1 and the values the
/// specification's default translation gives.
fn the_same_phrase_twice() -> (Stream, Stream) {
    let mpe = vec![
        (0, bend1(1, 1000)),
        (0, pressure1(1, 20)),
        (0, cc(1, 74, 90)),
        (0, on1(1, 60, 100)),
        (5, cc(0, 1, 64)),
        (7, bend1(0, -300)),
        (10, bend1(1, 8191)),
        (12, pressure1(1, 127)),
        (20, bend1(2, -8192)),
        (20, on1(2, 64, 7)),
        (21, cc(2, 74, 0)),
        (30, cc(0, 64, 127)),
        (33, off1(1, 60, 40)),
        // The released note no longer follows its channel.
        (34, bend1(1, 0)),
        (40, cc(0, 64, 0)),
        (41, pressure1(2, 64)),
        (50, off1(2, 64, 127)),
    ];
    let two = vec![
        (0, note_bend2(0, 60, raw(1000))),
        (0, poly2(0, 60, scale_7_to_32(20))),
        (0, per_note2(0, 60, 74, scale_7_to_32(90))),
        (0, on2(0, 60, scale_7_to_16(100))),
        (5, cc2(0, 1, scale_7_to_32(64))),
        (
            7,
            In::Two(ChannelVoice2::PitchBend {
                channel: 0,
                value: raw(-300),
            }),
        ),
        (10, note_bend2(0, 60, raw(8191))),
        (12, poly2(0, 60, scale_7_to_32(127))),
        (20, note_bend2(0, 64, raw(-8192))),
        (20, on2(0, 64, scale_7_to_16(7))),
        (21, per_note2(0, 64, 74, scale_7_to_32(0))),
        (30, cc2(0, 64, scale_7_to_32(127))),
        (33, off2(0, 60, scale_7_to_16(40))),
        (40, cc2(0, 64, 0)),
        (41, poly2(0, 64, scale_7_to_32(64))),
        (50, off2(0, 64, scale_7_to_16(127))),
    ];
    (mpe, two)
}

#[test]
fn an_mpe_stream_and_the_same_midi_2_stream_give_identical_events() {
    let (mpe, two) = the_same_phrase_twice();
    let from_mpe = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &mpe);
    let from_two = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &two);
    assert_eq!(from_mpe, from_two);
    // And neither is empty or trivial: two notes, each started, shaped
    // and ended, every event on its frame.
    assert_eq!(starts(&from_mpe).len(), 2);
    assert_eq!(ends(&from_mpe).len(), 2);
    assert!(
        from_mpe
            .windows(2)
            .all(|pair| pair[0].offset <= pair[1].offset)
    );
    // The same with no zones at all on the MIDI 2.0 side: per-note
    // messages on channel 1 are per note whatever MPE is doing.
    let from_two_alone = run(&mut NoteInput::new(Mpe::Off), &two);
    assert_eq!(from_mpe, from_two_alone);
}

#[test]
fn values_reach_the_note_in_its_own_units() {
    let (mpe, _) = the_same_phrase_twice();
    let events = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &mpe);
    let notes = starts(&events);
    let (first, second) = (notes[0].id.unwrap(), notes[1].id.unwrap());
    assert_ne!(first, second);
    // Both on the master channel of the zone.
    assert!(notes.iter().all(|note| note.channel == 0));

    let tuning = expression_of(&events, Expression::Tuning);
    // 1000/8192 of 48 semitones, then the whole range up, then for the
    // second note the whole range down. The master channel's wheel is not
    // in here: it is passed on as the channel-wide bend it is.
    assert_eq!(tuning.len(), 3);
    assert_eq!(tuning[0].0, first);
    assert!((tuning[0].1 - 1000.0 / 8192.0 * 48.0).abs() < 1e-3);
    assert_eq!(tuning[1], (first, 48.0));
    assert_eq!(tuning[2], (second, -48.0));

    let pressure = expression_of(&events, Expression::Pressure);
    assert_eq!(pressure[0].0, first);
    assert!((pressure[0].1 - 20.0 / 127.0).abs() < 2e-3);
    assert_eq!(pressure[1], (first, 1.0));
    // The second note's pressure moves after the pedal lifted; the first
    // note had been released before.
    assert_eq!(pressure[2].0, second);
    assert!((pressure[2].1 - 0.5).abs() < 1e-9);

    let brightness = expression_of(&events, Expression::Brightness);
    assert_eq!(brightness[0].0, first);
    assert!((brightness[0].1 - 90.0 / 127.0).abs() < 3e-3);
    assert_eq!(brightness[1], (second, 0.0));

    // The velocities, widened as MIDI 2.0 widens them.
    let velocities: Vec<f32> = events
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::NoteOn { velocity, .. } => Some(velocity),
            _ => None,
        })
        .collect();
    assert!((velocities[0] - 100.0 / 127.0).abs() < 3e-3);
    assert!((velocities[1] - 7.0 / 127.0).abs() < 3e-3);

    // The zone's own messages pass on as MIDI on the master channel.
    let midi: Vec<MidiMessage> = events
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::Midi(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        midi,
        [
            MidiMessage::ControlChange {
                channel: 0,
                controller: 1,
                value: 64
            },
            MidiMessage::PitchBend {
                channel: 0,
                value: -300
            },
            MidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 127
            },
            MidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 0
            },
        ]
    );
}

#[test]
fn a_note_keeps_its_identifier_from_start_to_end() {
    let (mpe, _) = the_same_phrase_twice();
    let events = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &mpe);
    for start in starts(&events) {
        let about_it = events.iter().filter(|event| match event.kind {
            EventKind::NoteOn { note, .. }
            | EventKind::NoteOff { note, .. }
            | EventKind::NoteExpression { note, .. } => note.id == start.id,
            _ => false,
        });
        let kinds: Vec<_> = about_it.map(|event| event.kind).collect();
        assert!(matches!(kinds.first(), Some(EventKind::NoteOn { .. })));
        assert!(matches!(kinds.last(), Some(EventKind::NoteOff { .. })));
        assert!(kinds.iter().all(|kind| match kind {
            EventKind::NoteOn { note, .. }
            | EventKind::NoteOff { note, .. }
            | EventKind::NoteExpression { note, .. } => *note == start,
            _ => false,
        }));
    }
    // Identifiers made here have the top bit set, so they never equal a
    // host's.
    assert!(
        starts(&events)
            .iter()
            .all(|note| note.id.unwrap() >= 0x8000_0000)
    );
}

#[test]
fn a_configuration_message_and_its_midi_2_form_give_the_same_notes() {
    let configure = Configuration {
        zone: ZoneKind::Lower,
        members: 15,
    };
    let mut mpe: Vec<(u32, In)> = configure
        .messages()
        .iter()
        .map(|&message| (0, In::One(message)))
        .collect();
    let (phrase, two) = the_same_phrase_twice();
    mpe.extend(phrase);
    let mut midi2 = vec![(0, rc2(0, 0, 6, 15 << 25))];
    midi2.extend(two);

    let from_mpe = run(&mut NoteInput::new(Mpe::Follow), &mpe);
    let from_two = run(&mut NoteInput::new(Mpe::Follow), &midi2);
    assert_eq!(per_note(&from_mpe), per_note(&from_two));
    assert_eq!(starts(&from_mpe).len(), 2);

    // With MPE off the same MIDI 1.0 stream is plain MIDI.
    let plain = run(&mut NoteInput::new(Mpe::Off), &mpe);
    assert!(starts(&plain).iter().all(|note| note.id.is_none()));
    assert_eq!(starts(&plain)[0].channel, 1);
}

#[test]
fn plain_midi_is_read_exactly_as_before() {
    let stream = [
        on1(3, 60, 100),
        bend1(3, 1234),
        pressure1(3, 9),
        cc(3, 74, 5),
        In::One(MidiMessage::PolyPressure {
            channel: 3,
            note: 60,
            pressure: 70,
        }),
        In::One(MidiMessage::ProgramChange {
            channel: 3,
            program: 4,
        }),
        off1(3, 60, 3),
        on1(9, 36, 127),
    ];
    let stream: Vec<(u32, In)> = stream
        .iter()
        .enumerate()
        .map(|(at, message)| (at as u32 * 3, *message))
        .collect();
    let events = run(&mut NoteInput::new(Mpe::Follow), &stream);
    let expected: Vec<Event> = stream
        .iter()
        .map(|&(offset, message)| {
            let In::One(message) = message else {
                unreachable!()
            };
            Event::from_midi(offset, message)
        })
        .collect();
    assert_eq!(events, expected);
}

#[test]
fn a_velocity_prefix_gives_fourteen_bits() {
    let stream = [
        (0, cc(0, 88, 64)),
        (0, on1(0, 60, 100)),
        (1, on1(0, 61, 100)),
    ];
    let events = run(&mut NoteInput::new(Mpe::Follow), &stream);
    // The prefix is passed on too, as the controller it is.
    assert_eq!(events.len(), 3);
    let EventKind::NoteOn { velocity, .. } = events[1].kind else {
        panic!()
    };
    assert_eq!(velocity, ((100 << 7) | 64) as f32 / 16_383.0);
    // It applies to the next note only.
    let EventKind::NoteOn { velocity, .. } = events[2].kind else {
        panic!()
    };
    assert_eq!(velocity, 100.0 / 127.0);

    // In a zone it is MIDI 2.0's sixteen bits, so the two agree.
    let zone = [(0, cc(1, 88, 64)), (0, on1(1, 60, 100))];
    let two = [(0, on2(0, 60, scale_up((100 << 7) | 64, 14, 16) as u16))];
    let events = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &zone);
    assert_eq!(events, run(&mut NoteInput::new(Mpe::LOWER_ZONE), &two));
}

#[test]
fn bend_ranges_follow_registered_parameter_zero_and_controller_zero_seven() {
    // Member channels share one range: sent on channel 4, it applies on
    // channel 2 too. Coarse 12 semitones, then 50 cents alone.
    let stream = [
        (0, cc(3, 101, 0)),
        (0, cc(3, 100, 0)),
        (0, cc(3, 6, 12)),
        (1, bend1(1, 8191)),
        (1, on1(1, 60, 100)),
        (2, cc(3, 38, 50)),
        (3, bend1(1, -8192)),
    ];
    let events = run(&mut NoteInput::new(Mpe::LOWER_ZONE), &stream);
    let tuning = expression_of(&events, Expression::Tuning);
    let values: Vec<f64> = tuning.iter().map(|(_, value)| *value).collect();
    assert_eq!(values, [12.0, 12.5, -12.5]);

    // A configuration message puts the range back to 48.
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    run(&mut input, &stream[..3]);
    let configure: Vec<(u32, In)> = Configuration {
        zone: ZoneKind::Lower,
        members: 15,
    }
    .messages()
    .iter()
    .map(|&message| (0, In::One(message)))
    .collect();
    run(&mut input, &configure);
    let events = run(&mut input, &[(0, bend1(1, 8191)), (0, on1(1, 60, 1))]);
    assert_eq!(expression_of(&events, Expression::Tuning)[0].1, 48.0);

    // Per-note pitch bend: 48 semitones by default, and registered
    // controller 0/7 changes it, here to 2 semitones and 50 cents.
    let mut input = NoteInput::new(Mpe::Off);
    let events = run(
        &mut input,
        &[
            (0, on2(5, 60, 1000)),
            (1, note_bend2(5, 60, u32::MAX)),
            (2, rc2(5, 0, 7, (2 << 25) | (50 << 18))),
            (3, note_bend2(5, 60, 0)),
        ],
    );
    let values: Vec<f64> = expression_of(&events, Expression::Tuning)
        .iter()
        .map(|(_, value)| *value)
        .collect();
    assert_eq!(values, [48.0, 2.5, -2.5]);
}

#[test]
fn zone_changes_release_the_notes_of_channels_that_change_role() {
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let events = run(
        &mut input,
        &[
            (0, on1(1, 60, 100)),
            (0, on1(12, 62, 100)),
            (0, on1(14, 64, 100)),
        ],
    );
    let notes = starts(&events);
    assert_eq!(notes.len(), 3);

    // An upper zone of four takes channels 11 to 16 from the lower zone:
    // the notes on channels 13 and 15 end, the one on channel 2 plays on.
    let upper: Vec<(u32, In)> = Configuration {
        zone: ZoneKind::Upper,
        members: 4,
    }
    .messages()
    .iter()
    .map(|&message| (9, In::One(message)))
    .collect();
    let events = run(&mut input, &upper);
    assert_eq!(ends(&events), [notes[1], notes[2]]);
    assert!(events.iter().all(|event| event.offset == 9));
    assert_eq!(input.layout().lower().unwrap().member_count(), 10);

    // Channel 15 is now a member of the upper zone: its notes are
    // reported on channel 16.
    let events = run(&mut input, &[(10, on1(14, 64, 100))]);
    assert_eq!(starts(&events)[0].channel, 15);

    // Switching both zones off: what was left ends, and every channel is
    // plain MIDI again.
    let off: Vec<(u32, In)> = [
        Configuration {
            zone: ZoneKind::Lower,
            members: 0,
        },
        Configuration {
            zone: ZoneKind::Upper,
            members: 0,
        },
    ]
    .iter()
    .flat_map(|configuration| configuration.messages().as_slice().to_vec())
    .map(|message| (11, In::One(message)))
    .collect();
    let events = run(&mut input, &off);
    assert_eq!(ends(&events).len(), 2);
    assert_eq!(input.held_notes(), 0);
    let events = run(&mut input, &[(12, on1(1, 60, 100))]);
    assert_eq!(starts(&events), [NoteRef::new(1, 60)]);
}

#[test]
fn a_note_that_started_before_its_zone_still_ends() {
    let mut input = NoteInput::new(Mpe::Follow);
    let mut events = run(&mut input, &[(0, on1(4, 60, 100))]);
    let configure: Vec<(u32, In)> = Configuration {
        zone: ZoneKind::Lower,
        members: 15,
    }
    .messages()
    .iter()
    .map(|&message| (1, In::One(message)))
    .collect();
    events.extend(run(&mut input, &configure));
    events.extend(run(&mut input, &[(2, off1(4, 60, 0))]));
    // The note-off finds the plain note by its channel and key.
    assert_eq!(ends(&events), [NoteRef::new(4, 60)]);
}

#[test]
fn the_same_key_twice_on_one_channel_ends_the_first_but_not_on_two() {
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let events = run(
        &mut input,
        &[
            (0, on1(1, 60, 100)),
            (1, on1(2, 60, 100)),
            (2, on1(1, 60, 90)),
        ],
    );
    let notes = starts(&events);
    assert_eq!(notes.len(), 3);
    assert!(notes.iter().all(|note| note.key == 60 && note.channel == 0));
    // Only the first note, struck again on its own channel, ends.
    assert_eq!(ends(&events), [notes[0]]);
    assert_eq!(input.held_notes(), 2);
}

#[test]
fn midi_2_pitch_attributes_and_per_note_controllers() {
    let mut input = NoteInput::new(Mpe::Off);
    let events = run(
        &mut input,
        &[
            // A note that is 60.25 semitones high, sent on number 7.
            (
                0,
                In::Two(ChannelVoice2::NoteOn {
                    channel: 2,
                    note: 7,
                    velocity: 0xFFFF,
                    attribute: NoteAttribute::pitch(60.25),
                }),
            ),
            // Its pitch moved to 61.5 with per-note controller #3.
            (1, per_note2(2, 7, 3, (61 << 25) | (1 << 24))),
            (2, per_note2(2, 7, 10, u32::MAX)),
            (3, per_note2(2, 7, 10, 0)),
            (4, per_note2(2, 7, 7, u32::MAX / 2)),
            (5, per_note2(2, 7, 11, u32::MAX / 2)),
            (6, per_note2(2, 7, 77, u32::MAX)),
            // Modulation has no expression and is dropped.
            (7, per_note2(2, 7, 1, 123)),
        ],
    );
    let tuning = expression_of(&events, Expression::Tuning);
    assert_eq!(tuning[0].1, 60.25 - 7.0);
    assert_eq!(tuning[1].1, 61.5 - 7.0);
    let pan: Vec<f64> = expression_of(&events, Expression::Pan)
        .iter()
        .map(|(_, value)| *value)
        .collect();
    assert_eq!(pan, [1.0, -1.0]);
    let volume = expression_of(&events, Expression::Volume);
    // A quarter for half the volume, and a quarter of that for half the
    // expression.
    assert!((volume[0].1 - 0.25).abs() < 1e-9);
    assert!((volume[1].1 - 0.0625).abs() < 1e-9);
    assert_eq!(expression_of(&events, Expression::Vibrato)[0].1, 1.0);
    assert_eq!(events.len(), 8);
}

#[test]
fn per_note_values_belong_to_the_number_until_reset() {
    let mut input = NoteInput::new(Mpe::Off);
    let manage = |detach, reset| {
        In::Two(ChannelVoice2::PerNoteManagement {
            channel: 0,
            note: 60,
            detach,
            reset,
        })
    };
    // A bend sent before the note is its starting bend.
    let events = run(
        &mut input,
        &[(0, note_bend2(0, 60, u32::MAX)), (0, on2(0, 60, 100))],
    );
    let first = starts(&events)[0];
    assert_eq!(
        expression_of(&events, Expression::Tuning),
        [(first.id.unwrap(), 48.0)]
    );

    // Detached, the first note plays on and no longer follows its number;
    // a second note on the number starts with the number's bend.
    let events = run(
        &mut input,
        &[
            (1, manage(true, false)),
            (2, on2(0, 60, 100)),
            (3, note_bend2(0, 60, CENTRE_PLUS_QUARTER)),
        ],
    );
    let second = starts(&events)[0];
    assert!(ends(&events).is_empty());
    let tuning = expression_of(&events, Expression::Tuning);
    assert_eq!(tuning.len(), 2);
    assert!(tuning.iter().all(|(id, _)| *id == second.id.unwrap()));

    // A reset puts the attached note's per-note values back to rest.
    let events = run(&mut input, &[(4, manage(false, true))]);
    assert_eq!(
        expression_of(&events, Expression::Tuning),
        [(second.id.unwrap(), 0.0)]
    );
    // The two note-offs on the number end the notes oldest first.
    let events = run(&mut input, &[(5, off2(0, 60, 0)), (6, off2(0, 60, 0))]);
    assert_eq!(ends(&events), [first, second]);
}

const CENTRE_PLUS_QUARTER: u32 = 0x8000_0000 + 0x2000_0000;

#[test]
fn a_released_mpe_note_stops_following_its_channel_and_a_reset_rests_the_zone() {
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let events = run(
        &mut input,
        &[
            (0, on1(1, 60, 100)),
            (1, bend1(1, 4096)),
            (2, pressure1(1, 100)),
            // Reset all controllers on the master channel brings the
            // member's bend and pressure back to rest.
            (3, cc(0, 121, 0)),
        ],
    );
    let id = starts(&events)[0].id.unwrap();
    assert_eq!(
        expression_of(&events, Expression::Tuning).last(),
        Some(&(id, 0.0))
    );
    assert_eq!(
        expression_of(&events, Expression::Pressure).last(),
        Some(&(id, 0.0))
    );
    let events = run(&mut input, &[(4, off1(1, 60, 0)), (5, bend1(1, 8191))]);
    assert_eq!(per_note(&events).len(), 1);
}

#[test]
fn native_notes_keep_the_hosts_identifiers() {
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let mut events = Vec::new();
    input.note_on(0, NoteRef::with_id(3, 60, 77), 0.5, |event| {
        events.push(event)
    });
    input.midi1(
        1,
        MidiMessage::PitchBend {
            channel: 3,
            value: 8191,
        },
        |event| events.push(event),
    );
    input.note_off(2, NoteRef::with_id(3, 60, 77), 0.25, |event| {
        events.push(event)
    });
    // Outside a zone the note passes through untouched.
    input.note_on(3, NoteRef::new(0, 1), 1.0, |_| {});
    let note = NoteRef::with_id(0, 60, 77);
    assert_eq!(
        events.iter().map(|event| event.kind).collect::<Vec<_>>(),
        [
            EventKind::NoteOn {
                note,
                velocity: 0.5
            },
            EventKind::NoteExpression {
                note,
                expression: Expression::Tuning,
                value: 48.0
            },
            EventKind::NoteOff {
                note,
                velocity: 0.25
            },
        ]
    );
    let mut plain = Vec::new();
    let mut input = NoteInput::new(Mpe::Follow);
    input.note_on(3, NoteRef::with_id(4, 1, 9), 1.0, |event| plain.push(event));
    assert_eq!(
        plain[0].kind,
        EventKind::NoteOn {
            note: NoteRef::with_id(4, 1, 9),
            velocity: 1.0
        }
    );
}

#[test]
fn packets_of_either_protocol_are_read() {
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let mut events = Vec::new();
    let one = Message::Midi1 {
        group: 3,
        message: MidiMessage::NoteOn {
            channel: 1,
            note: 60,
            velocity: 100,
        },
    }
    .to_packet();
    let two = Message::Midi2 {
        group: 0,
        message: ChannelVoice2::PerNotePitchBend {
            channel: 1,
            note: 60,
            value: u32::MAX,
        },
    }
    .to_packet();
    input.packet(0, &one, |event| events.push(event));
    input.packet(1, &two, |event| events.push(event));
    // Utility, stream and system packets are not notes.
    input.packet(2, &Packet::from_words(&[0x10F8_0000]).unwrap(), |event| {
        events.push(event)
    });
    assert_eq!(events.len(), 2);
    assert_eq!(expression_of(&events, Expression::Tuning)[0].1, 48.0);
}

/// Records the pitch offset it is told and plays it as a constant.
#[derive(Debug, Default)]
struct Tuned {
    key: u8,
    tuning: f64,
    held: bool,
}

impl Voice for Tuned {
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn note_on(&mut self, note: NoteRef, _: f32) {
        self.key = note.key;
        self.held = true;
    }
    fn note_off(&mut self, _: f32) {
        self.held = false;
    }
    fn choke(&mut self) {
        *self = Self::default();
    }
    fn is_active(&self) -> bool {
        self.held
    }
    fn render(&mut self, buffer: &mut AudioBuffer) {
        let value = f32::from(self.key) + self.tuning as f32;
        for sample in buffer.channel_mut(0) {
            *sample += value;
        }
    }
    fn expression(&mut self, expression: Expression, value: f64) {
        if expression == Expression::Tuning {
            self.tuning = value;
        }
    }
}

#[test]
fn a_stolen_voice_does_not_hear_the_expressions_of_the_note_it_lost() {
    let mut synth = Synth::new([Tuned::default(), Tuned::default()]);
    synth.prepare(&ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 64,
        num_channels: 1,
    });
    let mut input = NoteInput::new(Mpe::LOWER_ZONE);
    let mut events = EventBuffer::with_capacity(64);
    let stream = [
        (0, on1(1, 60, 100)),
        (1, on1(2, 62, 100)),
        // A third note takes over the voice of the first.
        (2, on1(3, 64, 100)),
        // The first note's channel bends: nothing may move.
        (3, bend1(1, 8191)),
        // The third note's channel bends by a quarter of 48.
        (4, bend1(3, 2048)),
    ];
    for &(offset, message) in &stream {
        let In::One(message) = message else {
            unreachable!()
        };
        input.midi1(offset, message, |event| {
            let _ = events.push(event);
        });
    }
    let mut buffer = AudioBuffer::<f32>::new(1, 8);
    let transport = Transport::STOPPED;
    synth.process_with(&mut buffer, &ProcessContext::new(&events, &transport));
    let out = buffer.channel(0);
    // Frame 5 holds the second and third notes: 62 and 64 + 12.
    assert!((out[5] - (62.0 + 64.0 + 12.0)).abs() < 1e-2, "{out:?}");
    // Frame 3: the first note's bend reached nobody.
    assert!((out[3] - (62.0 + 64.0)).abs() < 1e-3, "{out:?}");
}

#[test]
fn past_its_capacity_the_oldest_note_is_released() {
    let mut input = NoteInput::with_capacity(Mpe::LOWER_ZONE, 2);
    let events = run(
        &mut input,
        &[
            (0, on1(1, 60, 100)),
            (1, on1(2, 62, 100)),
            (2, on1(3, 64, 100)),
        ],
    );
    let notes = starts(&events);
    assert_eq!(ends(&events), [notes[0]]);
    assert_eq!(input.held_notes(), 2);
}

#[test]
fn hostile_input_never_panics_and_stays_bounded() {
    // A deterministic stream of every kind of message with every kind of
    // value, out-of-range fields included.
    let mut state = 0x1234_5678_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut input = NoteInput::with_capacity(Mpe::Follow, 16);
    let mut count = 0usize;
    for round in 0..200_000u32 {
        let (a, b) = (next(), next());
        let channel = (a >> 8) as u8;
        let data = (a >> 16) as u8;
        let data2 = (a >> 24) as u8;
        let mut emit = |_: Event| count += 1;
        if round % 2 == 0 {
            let message = match a % 7 {
                0 => MidiMessage::NoteOn {
                    channel,
                    note: data,
                    velocity: data2,
                },
                1 => MidiMessage::NoteOff {
                    channel,
                    note: data,
                    velocity: data2,
                },
                2 => MidiMessage::PolyPressure {
                    channel,
                    note: data,
                    pressure: data2,
                },
                // Bias towards the parameter controllers, so that zones
                // come and go.
                3 => MidiMessage::ControlChange {
                    channel: channel % 2 * 15,
                    controller: [6, 38, 100, 101, 74, 88, 121, 123][(data % 8) as usize],
                    value: data2 % 16,
                },
                4 => MidiMessage::ChannelPressure {
                    channel,
                    pressure: data,
                },
                5 => MidiMessage::PitchBend {
                    channel,
                    value: b as i16,
                },
                _ => MidiMessage::ControlChange {
                    channel,
                    controller: data,
                    value: data2,
                },
            };
            input.midi1(round, message, &mut emit);
        } else if let Ok(packet) = Packet::from_words(&[0x4000_0000 | (a & 0x0FFF_FFFF), b]) {
            input.packet(round, &packet, &mut emit);
        }
        assert!(input.held_notes() <= 16);
    }
    assert!(count > 0);
}
