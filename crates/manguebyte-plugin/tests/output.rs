//! What a plugin sends out, written as MIDI and read back.
//!
//! The framework reads MIDI 1.0, MPE and MIDI 2.0 in one place,
//! `manguebyte_dsp::note_input::NoteInput`. What `OutputTranslator`
//! writes is checked against it: notes and expressions sent through each
//! protocol come back as the same notes and expressions, within the
//! resolution of the protocol, and the MIDI 1.0 bytes are the ones the
//! specification's examples give.

use std::sync::Arc;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event, EventKind, Expression, NoteRef};
use manguebyte_dsp::note_input::{Mpe, NoteInput};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::Zone;
use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, Packet};
use manguebyte_params::param_set;
use manguebyte_plugin::output::{
    OutputBuffer, OutputKind, OutputProtocol, OutputTranslator, PER_NOTE_BEND_RANGE,
};
use manguebyte_plugin::{AudioLayout, Harness, Plugin, PluginInfo, PluginKind};

/// Translates `kinds` and reads the packets back through a `NoteInput`.
fn round_trip(protocol: OutputProtocol, mpe: Mpe, kinds: &[OutputKind]) -> Vec<EventKind> {
    let mut translator = OutputTranslator::new(protocol);
    let mut input = NoteInput::new(mpe);
    let mut read = Vec::new();
    for kind in kinds {
        translator.translate(kind, |packet| {
            input.packet(0, &packet, |event: Event| read.push(event.kind));
        });
    }
    read
}

fn packets(protocol: OutputProtocol, kinds: &[OutputKind]) -> Vec<Packet> {
    let mut translator = OutputTranslator::new(protocol);
    let mut out = Vec::new();
    for kind in kinds {
        translator.translate(kind, |packet| out.push(packet));
    }
    out
}

fn midi1(packets: &[Packet]) -> Vec<MidiMessage> {
    packets
        .iter()
        .map(|packet| packet.to_midi1().expect("a MIDI 1.0 packet"))
        .collect()
}

fn on(channel: u8, key: u8, velocity: f32) -> OutputKind {
    OutputKind::NoteOn {
        note: NoteRef::new(channel, key),
        velocity,
    }
}

fn off(channel: u8, key: u8) -> OutputKind {
    OutputKind::NoteOff {
        note: NoteRef::new(channel, key),
        velocity: 0.0,
    }
}

fn shape(channel: u8, key: u8, expression: Expression, value: f64) -> OutputKind {
    OutputKind::NoteExpression {
        note: NoteRef::new(channel, key),
        expression,
        value,
    }
}

/// The last value each expression of the note on `key` was read as.
fn read_value(events: &[EventKind], key: u8, wanted: Expression) -> Option<f64> {
    let mut id = None;
    let mut value = None;
    for event in events {
        match *event {
            EventKind::NoteOn { note, .. } if note.key == key => id = note.id,
            EventKind::NoteExpression {
                note,
                expression,
                value: read,
            } if expression == wanted && note.id == id && id.is_some() => value = Some(read),
            _ => {}
        }
    }
    value
}

fn velocity_of(events: &[EventKind], key: u8) -> f32 {
    events
        .iter()
        .find_map(|event| match *event {
            EventKind::NoteOn { note, velocity } if note.key == key => Some(velocity),
            _ => None,
        })
        .expect("the note was read back")
}

const SHAPED: [(Expression, f64); 6] = [
    (Expression::Tuning, -7.25),
    (Expression::Pressure, 0.75),
    (Expression::Brightness, 0.2),
    (Expression::Vibrato, 0.9),
    (Expression::Pan, -0.5),
    (Expression::Volume, 0.25),
];

#[test]
fn midi2_carries_every_expression_and_16_bit_velocity_back_unchanged() {
    let mut kinds = vec![on(3, 60, 0.123_45)];
    kinds.extend(SHAPED.map(|(expression, value)| shape(3, 60, expression, value)));
    kinds.push(off(3, 60));
    let read = round_trip(OutputProtocol::Midi2, Mpe::Off, &kinds);

    // Sixteen bits of velocity: within one step of 1/65535.
    assert!((velocity_of(&read, 60) - 0.123_45).abs() < 1.0 / 65_535.0);
    for (expression, value) in SHAPED {
        let back = read_value(&read, 60, expression).expect("the expression was read back");
        assert!(
            (back - value).abs() < 1e-6,
            "{expression:?}: sent {value}, read {back}"
        );
    }
    // The note keeps its channel and ends.
    let EventKind::NoteOn { note, .. } = read[0] else {
        panic!("{:?}", read[0])
    };
    assert_eq!((note.channel, note.key), (3, 60));
    assert!(matches!(read.last(), Some(EventKind::NoteOff { note: off, .. }) if off.id == note.id));
}

#[test]
fn tuning_is_announced_once_per_channel_and_clamped_to_its_range() {
    let sent = packets(
        OutputProtocol::Midi2,
        &[
            on(0, 60, 1.0),
            shape(0, 60, Expression::Tuning, 100.0),
            shape(0, 60, Expression::Tuning, -1.0),
        ],
    );
    let messages: Vec<ChannelVoice2> = sent
        .iter()
        .map(|packet| match packet.message() {
            Ok(Message::Midi2 { message, .. }) => message,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        messages[1],
        ChannelVoice2::RegisteredController {
            channel: 0,
            bank: 0,
            index: 7,
            value: u32::from(PER_NOTE_BEND_RANGE) << 25,
        }
    );
    assert_eq!(
        messages[2],
        ChannelVoice2::PerNotePitchBend {
            channel: 0,
            note: 60,
            value: u32::MAX,
        }
    );
    assert!(matches!(
        messages[3],
        ChannelVoice2::PerNotePitchBend { .. }
    ));
    assert_eq!(messages.len(), 4);
}

#[test]
fn a_shaped_note_number_is_reset_before_its_next_note() {
    let kinds = [
        on(0, 60, 1.0),
        shape(0, 60, Expression::Tuning, 3.0),
        off(0, 60),
        on(0, 60, 1.0),
    ];
    let sent = packets(OutputProtocol::Midi2, &kinds);
    assert!(sent.iter().any(|packet| matches!(
        packet.message(),
        Ok(Message::Midi2 {
            message: ChannelVoice2::PerNoteManagement {
                detach: true,
                reset: true,
                note: 60,
                ..
            },
            ..
        })
    )));
    // Read back, the second note starts in tune.
    let read = round_trip(OutputProtocol::Midi2, Mpe::Off, &kinds);
    let second = read
        .iter()
        .rposition(|event| matches!(event, EventKind::NoteOn { .. }))
        .unwrap();
    assert!(read[second..].iter().all(|event| !matches!(
        event,
        EventKind::NoteExpression {
            expression: Expression::Tuning,
            value,
            ..
        } if *value != 0.0
    )));
}

#[test]
fn notes_named_by_identifier_alone_are_found() {
    let id = 77;
    let kinds = [
        OutputKind::NoteOn {
            note: NoteRef::with_id(2, 64, id),
            velocity: 1.0,
        },
        // As CLAP and VST 3 hosts address expressions: by id, key unknown.
        OutputKind::NoteExpression {
            note: NoteRef::with_id(0, 0, id),
            expression: Expression::Pressure,
            value: 1.0,
        },
        OutputKind::NoteChoke {
            note: NoteRef::with_id(0, 0, id),
        },
    ];
    let messages: Vec<Message> = packets(OutputProtocol::Midi2, &kinds)
        .iter()
        .map(|packet| packet.message().unwrap())
        .collect();
    assert_eq!(
        messages[1],
        Message::Midi2 {
            group: 0,
            message: ChannelVoice2::PolyPressure {
                channel: 2,
                note: 64,
                pressure: u32::MAX,
            },
        }
    );
    assert_eq!(
        messages[2],
        Message::Midi2 {
            group: 0,
            message: ChannelVoice2::NoteOff {
                channel: 2,
                note: 64,
                velocity: 0,
                attribute: NoteAttribute::NONE,
            },
        }
    );
}

#[test]
fn midi1_follows_the_default_translation_and_its_examples() {
    let raw = |message| OutputKind::Midi2 { group: 4, message };
    let sent = packets(
        OutputProtocol::Midi1,
        &[
            // M2-104-UM, D.1.4: 0xAEBA narrows to 0x57.
            raw(ChannelVoice2::NoteOn {
                channel: 2,
                note: 60,
                velocity: 0xAEBA,
                attribute: NoteAttribute::pitch(60.5),
            }),
            // D.2.1: a velocity that would become zero is sent as 1.
            raw(ChannelVoice2::NoteOn {
                channel: 2,
                note: 61,
                velocity: 0x0100,
                attribute: NoteAttribute::NONE,
            }),
            // D.2.8: per-note pitch bend has no MIDI 1.0 form.
            raw(ChannelVoice2::PerNotePitchBend {
                channel: 2,
                note: 60,
                value: 0,
            }),
            raw(ChannelVoice2::PitchBend {
                channel: 2,
                value: 0x8000_0000,
            }),
        ],
    );
    let words: Vec<&[u32]> = sent.iter().map(Packet::words).collect();
    assert_eq!(words, [&[0x2492_3C57][..], &[0x2492_3D01], &[0x24E2_0040]]);

    // The framework's notes: velocity and pressure in 7 bits, tuning
    // dropped, on group 0.
    let sent = packets(
        OutputProtocol::Midi1,
        &[
            on(0, 64, 0xC000 as f32 / 65_535.0),
            shape(0, 64, Expression::Tuning, 2.0),
            shape(0, 64, Expression::Pressure, 0.5),
            off(0, 64),
        ],
    );
    assert_eq!(
        midi1(&sent),
        [
            MidiMessage::NoteOn {
                channel: 0,
                note: 64,
                velocity: 0x60,
            },
            MidiMessage::PolyPressure {
                channel: 0,
                note: 64,
                pressure: 64,
            },
            MidiMessage::NoteOff {
                channel: 0,
                note: 64,
                velocity: 0,
            },
        ]
    );
}

#[test]
fn midi1_from_the_plugin_is_translated_up_for_midi2_and_passed_on_for_midi1() {
    let kind = OutputKind::Midi1 {
        group: 3,
        message: MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    };
    let up = packets(OutputProtocol::Midi2, &[kind]);
    // As in the default translation's own example.
    assert_eq!(up[0].words(), &[0x4390_3C00, 0xC924_0000]);
    let same = packets(OutputProtocol::Midi1, &[kind]);
    assert_eq!(same[0].words(), &[0x2390_3C64]);
}

#[test]
fn mpe_gives_each_note_a_channel_and_reads_back_as_the_same_notes() {
    let zone = Zone::lower(15).unwrap();
    let kinds = [
        on(0, 60, 1.0),
        on(0, 64, 0.5),
        shape(0, 60, Expression::Tuning, 12.0),
        shape(0, 64, Expression::Tuning, -0.5),
        shape(0, 64, Expression::Pressure, 0.75),
        shape(0, 60, Expression::Brightness, 1.0),
        off(0, 60),
        off(0, 64),
    ];
    let sent = midi1(&packets(OutputProtocol::Mpe(zone), &kinds));
    // The configuration message first, then each note on its own member
    // channel, with its bend, pressure and timbre set before it starts.
    assert_eq!(&sent[..3], zone.configuration().messages().as_slice());
    assert_eq!(
        sent[6],
        MidiMessage::NoteOn {
            channel: 1,
            note: 60,
            velocity: 127,
        }
    );
    assert_eq!(
        sent[10],
        MidiMessage::NoteOn {
            channel: 2,
            note: 64,
            velocity: 64,
        }
    );

    // A receiver that follows the stream reads the notes on the master
    // channel, each with its own expressions, at MIDI 1.0 resolution.
    let read = round_trip(OutputProtocol::Mpe(zone), Mpe::Follow, &kinds);
    let notes: Vec<NoteRef> = read
        .iter()
        .filter_map(|event| match *event {
            EventKind::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 2);
    assert!(
        notes
            .iter()
            .all(|note| note.channel == 0 && note.id.is_some())
    );
    let tuning = read_value(&read, 60, Expression::Tuning).unwrap();
    assert!((tuning - 12.0).abs() < 0.01, "{tuning}");
    let tuning = read_value(&read, 64, Expression::Tuning).unwrap();
    assert!((tuning + 0.5).abs() < 0.01, "{tuning}");
    let pressure = read_value(&read, 64, Expression::Pressure).unwrap();
    assert!((pressure - 0.75).abs() < 1.0 / 127.0, "{pressure}");
    assert_eq!(read_value(&read, 60, Expression::Brightness), Some(1.0));
    // Both end.
    let offs = read
        .iter()
        .filter(|event| matches!(event, EventKind::NoteOff { .. }))
        .count();
    assert_eq!(offs, 2);
}

#[test]
fn ending_everything_stops_the_notes_still_sounding() {
    for protocol in [
        OutputProtocol::Midi2,
        OutputProtocol::Midi1,
        OutputProtocol::Mpe(Zone::lower(4).unwrap()),
    ] {
        let mut translator = OutputTranslator::new(protocol);
        for kind in [on(0, 60, 1.0), on(1, 62, 1.0), off(0, 60)] {
            translator.translate(&kind, |_| {});
        }
        let mut input = NoteInput::new(Mpe::Follow);
        let mut ended = Vec::new();
        translator.end_all(|packet| {
            input.packet(0, &packet, |event| ended.push(event.kind));
        });
        let keys: Vec<u8> = ended
            .iter()
            .filter_map(|event| match event {
                EventKind::NoteOff { note, .. } => Some(note.key),
                EventKind::Midi(MidiMessage::NoteOff { note, .. }) => Some(*note),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [62], "{protocol:?}");
        let mut again = Vec::new();
        translator.end_all(|packet| again.push(packet));
        assert!(again.is_empty(), "{protocol:?}");
    }
}

// --- What a plugin sends, through the harness ----------------------------

struct NoParams {}
param_set!(NoParams {});

/// Echoes every note an octave up with a bend that follows the frame, and
/// sends a controller on frame 5 of every block's worth of time.
struct Echo(Arc<NoParams>);

impl Plugin for Echo {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.echo",
        name: "Echo",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Sends its notes back out an octave up",
        kind: PluginKind::NoteEffect,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
    const NOTE_OUTPUT: bool = true;
    type Params = NoParams;

    fn new() -> Self {
        Self(Arc::new(NoParams {}))
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        self.process_with_output(buffer, context, &mut OutputBuffer::new());
    }
    fn process_with_output(
        &mut self,
        _: &mut AudioBuffer<f32>,
        context: &ProcessContext<'_, f32>,
        output: &mut OutputBuffer,
    ) {
        let up = |note: NoteRef| NoteRef::new(note.channel, note.key + 12);
        for event in context.events {
            match event.kind {
                EventKind::NoteOn { note, velocity } => {
                    output.note_on(event.offset, up(note), velocity);
                    let bend = f64::from(event.offset) / 100.0;
                    output.expression(event.offset, up(note), Expression::Tuning, bend);
                }
                EventKind::NoteOff { note, velocity } => {
                    output.note_off(event.offset, up(note), velocity);
                }
                EventKind::Midi(message) => {
                    output.midi1(event.offset, message);
                }
                _ => {}
            }
        }
    }
}

/// What the plugin sent, and the packets it became, each on its frame.
type Echoed = (Vec<(u64, OutputKind)>, Vec<(u64, Packet)>);

fn echoed(pattern: &[usize]) -> Echoed {
    let mut harness = Harness::<Echo>::new(48_000.0, 64).with_block_pattern(pattern);
    for (start, key) in [(3, 60), (40, 62), (64, 64), (130, 65)] {
        harness.note(start, 50, key, 0.75);
    }
    harness.schedule_midi(
        77,
        MidiMessage::ControlChange {
            channel: 0,
            controller: 1,
            value: 9,
        },
    );
    let input = AudioBuffer::<f32>::new(2, 256);
    harness.process(&input);
    let sent = harness.take_output();
    // Written as a host would, block by block, through one translator.
    let mut translator = OutputTranslator::new(OutputProtocol::Midi2);
    let mut packets = Vec::new();
    for (frame, kind) in &sent {
        translator.translate(kind, |packet| packets.push((*frame, packet)));
    }
    (sent, packets)
}

#[test]
fn what_a_plugin_sends_does_not_depend_on_the_block_size() {
    let (whole, whole_packets) = echoed(&[64]);
    assert_eq!(whole.len(), 13);
    assert_eq!(whole[0], (3, on(0, 72, 0.75)));
    for pattern in [&[1][..], &[7, 13, 1], &[33, 64, 2]] {
        let (cut, cut_packets) = echoed(pattern);
        assert_eq!(cut.len(), whole.len(), "{pattern:?}");
        for (a, b) in cut.iter().zip(&whole) {
            // The bend follows the frame within the block, which differs;
            // everything else is the same, on the same frame.
            assert_eq!(a.0, b.0, "{pattern:?}");
            if !matches!(a.1, OutputKind::NoteExpression { .. }) {
                assert_eq!(a.1, b.1, "{pattern:?}");
            }
        }
        assert_eq!(cut_packets.len(), whole_packets.len(), "{pattern:?}");
    }
    // Nothing is left for the next call.
    let mut harness = Harness::<Echo>::new(48_000.0, 64);
    harness.note(0, 1, 60, 1.0);
    harness.render(8);
    assert_eq!(harness.take_output().len(), 3);
    assert!(harness.take_output().is_empty());
}

/// A plugin that sends without setting `NOTE_OUTPUT` sends nothing.
struct Quiet(Arc<NoParams>);

impl Plugin for Quiet {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.quiet",
        kind: PluginKind::Effect,
        ..Echo::INFO
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
    type Params = NoParams;

    fn new() -> Self {
        Self(Arc::new(NoParams {}))
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
    fn process_with_output(
        &mut self,
        _: &mut AudioBuffer<f32>,
        _: &ProcessContext<'_, f32>,
        output: &mut OutputBuffer,
    ) {
        assert_eq!(output.capacity(), 0);
        assert!(!output.note_on(0, NoteRef::new(0, 1), 1.0));
    }
}

#[test]
fn a_plugin_without_note_output_has_no_room() {
    let mut harness = Harness::<Quiet>::new(48_000.0, 16);
    harness.render(32);
    assert!(harness.take_output().is_empty());
}
