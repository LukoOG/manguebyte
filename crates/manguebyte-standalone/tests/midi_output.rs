//! The MIDI output port a standalone plugin's notes go to: chosen in the
//! settings, changed while the plugin plays, followed as ports come and
//! go, against the headless backend, so no MIDI device is opened.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use manguebyte_core::AudioBuffer;
use manguebyte_devices::{
    DeviceManager, DeviceRef, DeviceSummary, Direction, Fallback, MockBackend, SwapKind,
};
use manguebyte_dsp::event::EventKind;
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::Zone;
use manguebyte_params::{FloatParam, FloatRange, param_set};
use manguebyte_plugin::output::OutputBuffer;
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};
use manguebyte_standalone::{Standalone, StandaloneOptions, StandaloneSettings};

struct Params {
    level: FloatParam,
}
param_set!(Params { level });

fn params() -> Arc<Params> {
    Arc::new(Params {
        level: FloatParam::new("level", "Level", FloatRange::linear(0.0, 1.0), 0.5),
    })
}

/// Sends every note it is given back out, on the same frame.
struct Echo(Arc<Params>);

impl Plugin for Echo {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.standalone-echo",
        name: "Echo",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Sends its notes out",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    const NOTE_OUTPUT: bool = true;
    type Params = Params;

    fn new() -> Self {
        Self(params())
    }
    fn params(&self) -> Arc<Params> {
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
        for event in context.events {
            match event.kind {
                EventKind::NoteOn { note, velocity } => {
                    output.note_on(event.offset, note, velocity);
                }
                EventKind::NoteOff { note, velocity } => {
                    output.note_off(event.offset, note, velocity);
                }
                _ => {}
            }
        }
    }
}

/// A plugin that sends no notes.
struct Quiet(Arc<Params>);

impl Plugin for Quiet {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.standalone-quiet",
        name: "Quiet",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Sends nothing",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = Params;

    fn new() -> Self {
        Self(params())
    }
    fn params(&self) -> Arc<Params> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

/// Speakers, and two MIDI output ports.
fn studio() -> MockBackend {
    let mock = MockBackend::new();
    mock.add_device(
        Direction::Output,
        MockBackend::device("speakers", "Speakers", 2, 48_000),
    );
    mock.add_midi_port(Direction::Output, "synth", "Synth");
    mock.add_midi_port(Direction::Output, "drums", "Drums");
    mock
}

fn summary(id: &str, name: &str) -> DeviceSummary {
    DeviceSummary {
        id: id.into(),
        name: name.into(),
    }
}

fn start<P: Plugin>(mock: &MockBackend, settings: StandaloneSettings) -> Standalone<P> {
    let mut manager = DeviceManager::headless(mock);
    manager.set_crossfade(Duration::ZERO);
    let options = StandaloneOptions {
        settings,
        ..StandaloneOptions::default()
    };
    Standalone::<P>::start_on(manager, options).unwrap()
}

/// One block from the speakers.
fn play(mock: &MockBackend) {
    let mut block = [0.0f32; 2 * 64];
    assert!(mock.render(&mut block));
}

fn on(note: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: 0,
        note,
        velocity: 100,
    }
}

fn off(note: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: 0,
        note,
        velocity: 0,
    }
}

/// Waits until the mock's MIDI output ports have been sent `count`
/// note-ons and note-offs since the last call, and takes them, with
/// whatever else came with them a millisecond later.
fn sent_notes(mock: &MockBackend, count: usize) -> Vec<(String, u8, u8, bool)> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while notes(&sent).len() < count && Instant::now() < deadline {
        sent.extend(messages(mock));
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(1));
    sent.extend(messages(mock));
    notes(&sent)
}

/// What the mock's MIDI output ports have been sent since the last call,
/// as messages.
fn messages(mock: &MockBackend) -> Vec<(String, MidiMessage)> {
    mock.take_sent_midi()
        .into_iter()
        .filter_map(|(port, bytes)| Some((port, MidiMessage::from_bytes(&bytes)?)))
        .collect()
}

/// The note-ons and note-offs among `sent`, as (port, channel, key, on).
fn notes(sent: &[(String, MidiMessage)]) -> Vec<(String, u8, u8, bool)> {
    sent.iter()
        .filter_map(|(port, message)| match *message {
            MidiMessage::NoteOn { channel, note, .. } => Some((port.clone(), channel, note, true)),
            MidiMessage::NoteOff { channel, note, .. } => {
                Some((port.clone(), channel, note, false))
            }
            _ => None,
        })
        .collect()
}

fn note(port: &str, channel: u8, key: u8, on: bool) -> (String, u8, u8, bool) {
    (port.to_owned(), channel, key, on)
}

#[test]
fn the_settings_choose_the_midi_output_by_identifier() {
    let mock = studio();
    let settings = StandaloneSettings::default().with_midi_output(DeviceRef::new("synth", ""));
    let app = start::<Echo>(&mock, settings.clone());
    let report = app.report();
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.midi_output, Some(summary("synth", "Synth")));
    assert!(report.to_string().contains("sending notes to \"Synth\""));
    assert_eq!(app.midi_output_name(), Some("Synth"));
    assert_eq!(app.settings(), &settings);
    let choices = app.choices().unwrap();
    assert_eq!(
        choices
            .midi_outputs
            .iter()
            .map(|option| (option.port.id.as_str(), option.open))
            .collect::<Vec<_>>(),
        [("synth", true), ("drums", false)]
    );

    assert!(app.send(on(60)));
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("synth", 0, 60, true)]);
    assert!(app.send(off(60)));
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("synth", 0, 60, false)]);
    assert_eq!(app.midi_output_errors(), Some(0));

    // A plugin that sends no notes opens no port, and offers none.
    drop(app);
    let quiet = start::<Quiet>(&mock, settings);
    assert_eq!(quiet.report().midi_output, None);
    assert!(quiet.choices().unwrap().midi_outputs.is_empty());
}

#[test]
fn a_missing_midi_output_falls_back_to_none_and_is_opened_when_it_appears() {
    let mock = studio();
    let ghost = DeviceRef::new("ghost", "Ghost");
    let mut app = start::<Echo>(
        &mock,
        StandaloneSettings::default().with_midi_output(ghost.clone()),
    );
    let report = app.report().clone();
    assert_eq!(report.midi_output, None);
    assert!(
        matches!(
            &report.fallbacks[..],
            [Fallback::MidiPortMissing {
                direction: Direction::Output,
                wanted,
                ..
            }] if *wanted == ghost
        ),
        "{report}"
    );
    assert!(report.to_string().contains("\"Ghost\" is left out"));
    assert_eq!(app.midi_problems().len(), 1);
    // The settings still ask for it, and play on.
    assert_eq!(app.settings().midi_output, Some(ghost.clone()));
    assert!(app.send(on(60)));
    play(&mock);
    assert!(
        app.choices()
            .unwrap()
            .midi_outputs
            .iter()
            .all(|port| !port.open)
    );
    // Long past when the note was due: it went nowhere.
    std::thread::sleep(Duration::from_millis(50));

    // Plugged in: opened, without touching the audio.
    mock.add_midi_port(Direction::Output, "ghost", "Ghost");
    let report = app.poll_devices().expect("a port appeared").clone();
    assert_eq!(report.midi_output, Some(summary("ghost", "Ghost")));
    assert!(report.is_exact(), "{report}");
    assert!(app.midi_problems().is_empty());
    assert_eq!(report.swap.map(|swap| swap.kind), Some(SwapKind::Start));
    assert!(app.send(on(62)));
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("ghost", 0, 62, true)]);

    // Unplugged: reported, and the note it played cannot be ended.
    mock.remove_midi_port(Direction::Output, "ghost");
    let report = app.poll_devices().expect("a port went").clone();
    assert_eq!(report.midi_output, None);
    assert!(matches!(
        &report.fallbacks[..],
        [Fallback::MidiPortMissing {
            direction: Direction::Output,
            ..
        }]
    ));

    // An identifier that is not there at all.
    let report = app.set_midi_output(Some("nowhere")).clone();
    assert_eq!(report.midi_output, None);
    assert!(matches!(
        &report.fallbacks[..],
        [Fallback::MidiPortMissing {
            direction: Direction::Output,
            wanted,
            ..
        }] if wanted.id == "nowhere"
    ));
    assert_eq!(
        app.settings().midi_output,
        Some(DeviceRef::new("nowhere", ""))
    );
    assert!(app.set_midi_output(None).is_exact());
    assert_eq!(app.settings().midi_output, None);
}

#[test]
fn changing_the_midi_output_ends_the_notes_on_the_old_port() {
    let mock = studio();
    let mut app = start::<Echo>(
        &mock,
        StandaloneSettings::default().with_midi_output(DeviceRef::new("synth", "Synth")),
    );
    assert!(app.send(on(60)));
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("synth", 0, 60, true)]);

    // At once, from this thread: the old port ends the note it plays.
    let report = app.set_midi_output(Some("drums")).clone();
    assert_eq!(notes(&messages(&mock)), [note("synth", 0, 60, false)]);
    assert_eq!(report.midi_output, Some(summary("drums", "Drums")));
    assert!(report.is_exact(), "{report}");
    assert_eq!(
        app.settings().midi_output,
        Some(DeviceRef::new("drums", "Drums"))
    );
    // The audio was left alone.
    assert_eq!(report.swap.map(|swap| swap.kind), Some(SwapKind::Start));

    // The plugin's own note-off for it goes nowhere; the next note goes
    // to the new port.
    assert!(app.send(off(60)));
    assert!(app.send(on(62)));
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("drums", 0, 62, true)]);

    // MPE from the next block on: the note sounding is ended as it was
    // written, and the next one goes to a member channel.
    let report = app.set_midi_output_mpe(Zone::lower(15)).clone();
    assert!(report.is_exact(), "{report}");
    play(&mock);
    assert_eq!(sent_notes(&mock, 1), [note("drums", 0, 62, false)]);
    assert!(app.send(off(62)));
    assert!(app.send(on(64)));
    play(&mock);
    let started = sent_notes(&mock, 1);
    assert!(
        matches!(&started[..], [(port, channel, 64, true)] if port == "drums" && *channel != 0),
        "{started:?}"
    );
    let channel = started[0].1;

    // No port at all: the member channel's note is ended there.
    assert!(app.set_midi_output(None).is_exact());
    assert_eq!(notes(&messages(&mock)), [note("drums", channel, 64, false)]);
    assert_eq!(app.report().midi_output, None);

    // All of it is saved, and opens the same way next time.
    app.set_midi_output(Some("synth"));
    let saved = app.settings().to_string();
    drop(app);
    let settings: StandaloneSettings = saved.parse().unwrap();
    assert_eq!(settings.midi_output, Some(DeviceRef::new("synth", "Synth")));
    assert_eq!(settings.midi_output_mpe, Zone::lower(15));
    let app = start::<Echo>(&mock, settings);
    assert_eq!(app.report().midi_output, Some(summary("synth", "Synth")));
}

#[test]
fn every_note_reaches_one_port_and_none_hangs_while_the_output_changes() {
    let mock = studio();
    let mut app = start::<Echo>(
        &mock,
        StandaloneSettings::default().with_midi_output(DeviceRef::new("synth", "Synth")),
    );
    // The device's thread plays a block every millisecond, as a sound
    // card would, while this thread sends notes and changes the port.
    let stop = Arc::new(AtomicBool::new(false));
    let device = {
        let (mock, stop) = (mock.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                play(&mock);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let mut sent_notes = 0;
    for step in 0..120u32 {
        let key = 40 + (step % 40) as u8;
        assert!(app.send(on(key)));
        sent_notes += 1;
        std::thread::sleep(Duration::from_millis(1));
        match step % 5 {
            0 => {
                app.set_midi_output(Some("drums"));
            }
            2 => {
                app.set_midi_output(Some("synth"));
            }
            3 if step % 3 == 0 => {
                app.set_midi_output_mpe(Zone::lower(15));
            }
            4 if step % 3 == 0 => {
                app.set_midi_output_mpe(None);
            }
            _ => {}
        }
        // Some notes end before the port changes, some after.
        if step % 2 == 1 {
            assert!(app.send(off(key)));
        }
        std::thread::sleep(Duration::from_millis(1));
        if step % 2 == 0 {
            assert!(app.send(off(key)));
        }
    }
    // Long enough for the last notes to be due. Every note the plugin
    // started it has ended, so no port may be left playing one, before
    // anything is closed.
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::Relaxed);
    device.join().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let all = messages(&mock);
    drop(app);
    assert!(
        notes(&messages(&mock)).is_empty(),
        "nothing was left to end"
    );

    let mut started = 0;
    for port in ["synth", "drums"] {
        let mut held = BTreeSet::new();
        for (_, channel, key, on) in notes(&all).into_iter().filter(|(to, ..)| to == port) {
            if on {
                started += 1;
                held.insert((channel, key));
            } else {
                assert!(
                    held.remove(&(channel, key)),
                    "{port} was sent a note-off for {key} on {channel}, which it was not playing"
                );
            }
        }
        assert!(held.is_empty(), "{port} is left playing {held:?}");
    }
    assert_eq!(started, sent_notes, "every note-on reached one port");
}
