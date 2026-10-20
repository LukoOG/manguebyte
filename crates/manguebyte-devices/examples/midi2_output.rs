//! Lists the MIDI output ports with the protocol each would speak, and,
//! only when asked, sends one of them a short, quiet note in MIDI 2.0.
//!
//! ```text
//! cargo run -p manguebyte-devices --example midi2_output
//! cargo run -p manguebyte-devices --example midi2_output -- --send <port id>
//! ```
//!
//! Without `--send` nothing is sent, so nothing sounds. With it, the port
//! is opened preferring MIDI 2.0 and sent, through the queue and thread
//! the audio thread would use: a note-on of middle C at a low velocity
//! (about 6 % of full scale) with a per-note brightness controller, and
//! the note-off a quarter of a second later. A port that speaks MIDI 1.0
//! gets the default translation of the same packets (velocity 8 of 127;
//! the per-note controller has no MIDI 1.0 form and is dropped).

use std::time::Duration;

use manguebyte_devices::midi::{
    MidiOutput, MidiProtocol, midi_output_ports_with_protocols, midi_output_queue,
    ump_output_backend,
};
use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, Packet};

fn note(on: bool) -> Packet {
    let (channel, note, velocity, attribute) = (0, 60, 0x1000, NoteAttribute::NONE);
    let message = if on {
        ChannelVoice2::NoteOn {
            channel,
            note,
            velocity,
            attribute,
        }
    } else {
        ChannelVoice2::NoteOff {
            channel,
            note,
            velocity,
            attribute,
        }
    };
    Message::Midi2 { group: 0, message }.to_packet()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match ump_output_backend() {
        Ok(backend) => println!("UMP output: {backend}"),
        Err(why) => println!("UMP output: none ({why})"),
    }
    for info in midi_output_ports_with_protocols()? {
        let why = info
            .fallback
            .as_ref()
            .map(|why| format!(" ({why})"))
            .unwrap_or_default();
        println!(
            "{:?}  {}  [{}]{why}",
            info.protocol, info.port.name, info.port.id
        );
    }

    let mut args = std::env::args().skip(1);
    let Some(flag) = args.next() else {
        println!("Nothing sent. Pass --send <port id> to send a quiet note.");
        return Ok(());
    };
    let id = match (flag.as_str(), args.next()) {
        ("--send", Some(id)) => id,
        _ => return Err("usage: midi2_output [--send <port id>]".into()),
    };

    let output = MidiOutput::open_with_protocol(&id, MidiProtocol::Midi2)?;
    match output.fallback() {
        None => println!("{}: sending MIDI 2.0 packets unchanged", output.name()),
        Some(why) => println!("{}: sending MIDI 1.0, because {why}", output.name()),
    }
    let (mut transmitter, queue) = midi_output_queue(16);
    let thread = output.spawn(queue);
    // What an audio callback does: one block of 480 frames at 48 kHz.
    transmitter.begin_block(480, 48_000.0);
    let brightness = Message::Midi2 {
        group: 0,
        message: ChannelVoice2::RegisteredPerNoteController {
            channel: 0,
            note: 60,
            controller: 74,
            value: 0x8000_0000,
        },
    }
    .to_packet();
    for packet in [note(true), brightness] {
        transmitter.send(0, packet);
    }
    std::thread::sleep(Duration::from_millis(250));
    transmitter.begin_block(480, 48_000.0);
    transmitter.send(0, note(false));
    std::thread::sleep(Duration::from_millis(50));
    drop(thread);
    println!("Sent: note-on, per-note controller 74, note-off.");
    Ok(())
}
