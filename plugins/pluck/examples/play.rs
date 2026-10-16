//! Plays Mangue Pluck on the sound card, with no host.
//!
//! ```text
//! cargo run -p mangue-pluck --features standalone --example play
//! cargo run -p mangue-pluck --features standalone --example play -- -20 hold
//! ```
//!
//! The first argument is the output level in decibels, which is otherwise
//! the plugin's default, `mangue_pluck::DEFAULT_LEVEL_DB`. With `hold`, the
//! program keeps running after its own phrase so that a connected MIDI
//! keyboard can be played, until Enter is pressed.

use std::thread::sleep;
use std::time::Duration;

use mangue_pluck::{DEFAULT_LEVEL_DB, Pluck};
use manguebyte::midi::MidiMessage;
use manguebyte::standalone::{Standalone, StandaloneOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let level: f32 = match arguments.next() {
        Some(text) => text.parse()?,
        None => DEFAULT_LEVEL_DB,
    };
    let hold = arguments.next().as_deref() == Some("hold");

    let app = Standalone::<Pluck>::start(StandaloneOptions::default())?;
    app.params().level.set(level);
    println!(
        "Playing on {} at {} Hz",
        app.device_name(),
        app.sample_rate()
    );
    match app.midi_input_names().as_slice() {
        [] => println!("No MIDI keyboards connected."),
        names => println!("Listening to MIDI from: {}", names.join(", ")),
    }
    for problem in app.midi_problems() {
        println!("MIDI: {problem}");
    }

    // An A minor arpeggio, up and back.
    for key in [45, 52, 57, 60, 64, 69, 64, 60, 57, 52, 45] {
        app.send(MidiMessage::NoteOn {
            channel: 0,
            note: key,
            velocity: 100,
        });
        sleep(Duration::from_millis(140));
        app.send(MidiMessage::NoteOff {
            channel: 0,
            note: key,
            velocity: 0,
        });
    }

    if hold {
        println!("Play your keyboard. Press Enter to quit.");
        std::io::stdin().read_line(&mut String::new())?;
    } else {
        // Let the last note and the reverb fade.
        sleep(Duration::from_millis(1_500));
    }
    Ok(())
}
