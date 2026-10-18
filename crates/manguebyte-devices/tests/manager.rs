//! The device manager, hot-plug and saved setups, against the headless
//! backend: no sound card needed, so these run on every system.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use manguebyte_devices::{
    AudioCallback, DeviceChoice, DeviceError, DeviceEvent, DeviceIo, DeviceManager, DeviceRef,
    DeviceSetup, DeviceSummary, Direction, Fallback, LossCause, MIN_POLL_INTERVAL, MockBackend,
    StreamFormat,
};

/// A mock with built-in speakers and microphone, and a USB interface.
fn studio() -> MockBackend {
    let mock = MockBackend::new();
    mock.add_device(
        Direction::Output,
        MockBackend::device("speakers", "Speakers", 2, 48_000),
    );
    mock.add_device(
        Direction::Output,
        MockBackend::device("usb-out", "USB Interface", 4, 96_000),
    );
    mock.add_device(
        Direction::Input,
        MockBackend::device("mic", "Microphone", 1, 48_000),
    );
    mock.add_device(
        Direction::Input,
        MockBackend::device("usb-in", "USB Interface", 2, 96_000),
    );
    mock
}

fn summary(id: &str, name: &str) -> DeviceSummary {
    DeviceSummary {
        id: id.into(),
        name: name.into(),
    }
}

fn usb_setup() -> DeviceSetup {
    DeviceSetup::default()
        .with_output(DeviceChoice::Device(DeviceRef::new(
            "usb-out",
            "USB Interface",
        )))
        .with_input(DeviceChoice::Device(DeviceRef::new(
            "usb-in",
            "USB Interface",
        )))
        .with_sample_rate(96_000)
        .with_buffer_size(128)
}

/// Writes a level that tells which run of the callback produced a block.
fn level(value: f32) -> impl FnMut(&mut DeviceIo<'_>) + Send + 'static {
    move |io: &mut DeviceIo<'_>| {
        for channel in io.output.iter_channels_mut() {
            channel.fill(value);
        }
    }
}

fn render(mock: &MockBackend, channels: usize) -> Option<Vec<f32>> {
    let mut block = vec![0.0f32; 64 * channels];
    mock.render(&mut block).then_some(block)
}

#[test]
fn the_default_setup_plays_on_the_default_output() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    let report = manager.start(&DeviceSetup::default(), level(0.25)).unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert!(manager.is_running());
    assert_eq!(
        manager.running_device(Direction::Output),
        Some(&summary("speakers", "Speakers"))
    );
    assert_eq!(manager.running_device(Direction::Input), None);
    let format = manager.format().unwrap();
    assert_eq!(format.sample_rate, 48_000.0);
    assert_eq!((format.input_channels, format.output_channels), (0, 2));

    let block = render(&mock, 2).unwrap();
    assert!(block.iter().all(|&sample| sample == 0.25));
    // Nothing captures without an input.
    assert!(!mock.capture(&[0.0; 64]));

    manager.stop();
    assert!(!mock.is_running());
    assert!(render(&mock, 2).is_none());
    // The callback was kept, and runs again.
    manager.restart().unwrap();
    assert_eq!(render(&mock, 2).unwrap()[0], 0.25);
}

#[test]
fn a_saved_setup_reopens_the_same_devices() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    let report = manager.start(&usb_setup(), level(0.5)).unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert_eq!(
        mock.running(Direction::Output).unwrap().1.sample_rate,
        Some(96_000)
    );
    assert_eq!(mock.running(Direction::Input).unwrap().0, "usb-in");
    assert_eq!(
        manager
            .running_settings(Direction::Output)
            .unwrap()
            .buffer_size,
        Some(128)
    );
    let saved = manager.setup().to_string();
    drop(manager);
    assert!(!mock.is_running());

    // A later run.
    let restored: DeviceSetup = saved.parse().unwrap();
    assert_eq!(restored, usb_setup());
    let mut manager = DeviceManager::headless(&mock);
    let report = manager.start(&restored, level(0.5)).unwrap();
    assert!(report.is_exact());
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "usb-out"
    );
}

#[test]
fn a_missing_device_falls_back_and_the_setup_still_wants_it() {
    let mock = studio();
    mock.remove_device(Direction::Output, "usb-out");
    mock.remove_device(Direction::Input, "usb-in");
    let mut manager = DeviceManager::headless(&mock);
    let report = manager.start(&usb_setup(), level(0.5)).unwrap();
    assert!(matches!(
        &report.fallbacks[..],
        [
            Fallback::DeviceReplaced {
                direction: Direction::Output,
                by_name: false,
                ..
            },
            Fallback::DeviceReplaced {
                direction: Direction::Input,
                by_name: false,
                ..
            },
        ]
    ));
    // The built-in devices can run at the saved rate, so they do.
    assert_eq!(manager.format().unwrap().sample_rate, 96_000.0);
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "speakers"
    );
    assert_eq!(manager.running_device(Direction::Input).unwrap().id, "mic");
    // What gets saved is still the interface, so that it is used again
    // once it is back.
    assert_eq!(manager.setup(), &usb_setup());

    // It comes back under another identifier, as ALSA devices do when
    // plugged into another port: found by its name.
    mock.add_device(
        Direction::Output,
        MockBackend::device("usb-out-2", "USB Interface", 4, 96_000),
    );
    let report = manager
        .apply(&usb_setup().with_input(DeviceChoice::None))
        .unwrap();
    assert!(matches!(
        &report.fallbacks[..],
        [Fallback::DeviceReplaced { by_name: true, .. }]
    ));
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "usb-out-2"
    );
}

#[test]
fn an_unknown_host_falls_back_to_the_default_one() {
    let mock = studio();
    mock.add_host("Other");
    let mut manager = DeviceManager::headless(&mock);
    let report = manager
        .start(&DeviceSetup::default().with_host("Nope"), level(0.1))
        .unwrap();
    assert_eq!(
        report.fallbacks,
        [Fallback::HostUnavailable {
            wanted: "Nope".into(),
            used: "Mock".into()
        }]
    );
    let report = manager
        .apply(&DeviceSetup::default().with_host("other"))
        .unwrap();
    assert!(report.is_exact());
    assert_eq!(manager.host_name(), "Other");
    assert_eq!(manager.hosts().len(), 2);
}

#[test]
fn unplugging_the_device_in_use_is_reported_and_followed() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    manager.start(&usb_setup(), level(0.5)).unwrap();
    // The first poll learns what is there.
    assert_eq!(manager.poll_devices(), []);

    assert!(mock.remove_device(Direction::Output, "usb-out"));
    // The stream stops being called, as a real one does.
    assert!(render(&mock, 4).is_none());
    let events = manager.poll_devices();
    assert_eq!(
        events,
        [
            DeviceEvent::DeviceRemoved {
                direction: Direction::Output,
                device: summary("usb-out", "USB Interface"),
            },
            DeviceEvent::StreamLost {
                direction: Direction::Output,
                device: summary("usb-out", "USB Interface"),
                cause: LossCause::Disconnected,
            },
        ]
    );
    // Either event moves the stream to the default; the other one then
    // changes nothing.
    let report = manager.follow(&events[1]).unwrap().unwrap();
    assert!(!report.is_exact());
    assert!(manager.follow(&events[0]).is_none());
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "speakers"
    );
    assert!(render(&mock, 2).is_some());
    // Reported once, not at every poll.
    assert_eq!(manager.poll_devices(), []);

    // Plugged in again: the setup wants it, so the manager goes back.
    mock.add_device(
        Direction::Output,
        MockBackend::device("usb-out", "USB Interface", 4, 96_000),
    );
    let events = manager.poll_devices();
    assert_eq!(
        events,
        [DeviceEvent::DeviceAdded {
            direction: Direction::Output,
            device: summary("usb-out", "USB Interface"),
        }]
    );
    let report = manager.follow(&events[0]).unwrap().unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "usb-out"
    );
    assert_eq!(render(&mock, 4).unwrap()[0], 0.5);
}

#[test]
fn a_device_the_backend_reports_lost_is_reported_once() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    let errors = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&errors);
    manager.set_error_handler(move |error| {
        assert!(error.is_device_lost());
        counter.fetch_add(1, Ordering::Relaxed);
    });
    manager.start(&DeviceSetup::default(), level(0.5)).unwrap();
    manager.poll_devices();
    assert!(mock.report_device_lost(Direction::Output));
    assert_eq!(errors.load(Ordering::Relaxed), 1);
    assert_eq!(
        manager.poll_devices(),
        [DeviceEvent::StreamLost {
            direction: Direction::Output,
            device: summary("speakers", "Speakers"),
            cause: LossCause::ReportedByBackend,
        }]
    );
    assert_eq!(manager.poll_devices(), []);
}

#[test]
fn the_default_changing_moves_a_stream_that_follows_the_default() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    manager.start(&DeviceSetup::default(), level(0.5)).unwrap();
    manager.poll_devices();
    mock.set_default_device(Direction::Output, Some("usb-out"));
    let events = manager.poll_devices();
    assert_eq!(
        events,
        [DeviceEvent::DefaultChanged {
            direction: Direction::Output,
            device: Some(summary("usb-out", "USB Interface")),
        }]
    );
    manager.follow(&events[0]).unwrap().unwrap();
    assert_eq!(
        manager.running_device(Direction::Output).unwrap().id,
        "usb-out"
    );

    // A setup that names a device stays on it.
    manager
        .apply(
            &DeviceSetup::default()
                .with_output(DeviceChoice::Device(DeviceRef::new("speakers", "Speakers"))),
        )
        .unwrap();
    mock.set_default_device(Direction::Output, Some("speakers"));
    manager.poll_devices();
    mock.set_default_device(Direction::Output, Some("usb-out"));
    let events = manager.poll_devices();
    assert!(manager.follow(&events[0]).is_none());
}

#[test]
fn with_nothing_connected_the_manager_waits_for_a_device() {
    let mock = MockBackend::new();
    let mut manager = DeviceManager::headless(&mock);
    let report = manager.start(&DeviceSetup::default(), level(0.5)).unwrap();
    assert_eq!(
        report.fallbacks,
        [Fallback::NoDevice {
            direction: Direction::Output,
            wanted: None
        }]
    );
    assert!(!manager.is_running());
    manager.poll_devices();

    mock.add_device(
        Direction::Output,
        MockBackend::device("speakers", "Speakers", 2, 44_100),
    );
    let events = manager.poll_devices();
    assert_eq!(events.len(), 2, "{events:?}");
    let report = manager.follow(&events[0]).unwrap().unwrap();
    assert!(report.is_exact());
    assert!(manager.is_running());
    assert_eq!(manager.format().unwrap().sample_rate, 44_100.0);
    // Already on it: the second event changes nothing.
    assert!(manager.follow(&events[1]).is_none());
}

#[test]
fn listing_failures_are_reported_and_recovered_from() {
    let mock = studio();
    let mut manager = DeviceManager::headless(&mock);
    manager.poll_devices();
    mock.fail_listing(true);
    assert!(matches!(
        &manager.poll_devices()[..],
        [DeviceEvent::ListingFailed(_)]
    ));
    assert!(manager.start(&DeviceSetup::default(), level(0.5)).is_err());
    mock.fail_listing(false);
    assert_eq!(manager.poll_devices(), []);
    // The callback was kept through the failure.
    manager.restart().unwrap();
    assert!(manager.is_running());
}

#[test]
fn restarting_before_starting_is_an_error() {
    let mut manager = DeviceManager::headless(&studio());
    assert!(matches!(manager.restart(), Err(DeviceError::NoCallback)));
    assert!(manager.take_callback().is_none());
}

/// Counts what the manager does with its callback.
#[derive(Default)]
struct Counting {
    prepared: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
    formats: Option<mpsc::Sender<StreamFormat>>,
}

impl AudioCallback for Counting {
    fn prepare(&mut self, format: &StreamFormat) {
        self.prepared.fetch_add(1, Ordering::Relaxed);
        if let Some(formats) = &self.formats {
            formats.send(*format).unwrap();
        }
    }

    fn process(&mut self, io: &mut DeviceIo<'_>) {
        let frames = io.num_frames();
        io.output.channel_mut(0)[..frames].fill(1.0);
    }

    fn release(&mut self) {
        self.released.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn the_callback_is_prepared_before_and_released_after_every_run() {
    let mock = studio();
    let (formats, received) = mpsc::channel();
    let callback = Counting {
        formats: Some(formats),
        ..Counting::default()
    };
    let (prepared, released) = (
        Arc::clone(&callback.prepared),
        Arc::clone(&callback.released),
    );
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(256);
    manager.start(&DeviceSetup::default(), callback).unwrap();
    manager.apply(&usb_setup()).unwrap();
    manager.stop();
    assert_eq!(prepared.load(Ordering::Relaxed), 2);
    assert_eq!(released.load(Ordering::Relaxed), 2);
    let formats: Vec<StreamFormat> = received.try_iter().collect();
    assert_eq!(
        formats,
        [
            StreamFormat {
                sample_rate: 48_000.0,
                max_block_size: 256,
                input_channels: 0,
                output_channels: 2,
            },
            StreamFormat {
                sample_rate: 96_000.0,
                max_block_size: 256,
                input_channels: 2,
                output_channels: 4,
            },
        ]
    );
    assert!(manager.take_callback().is_some());
}

#[test]
fn a_duplex_run_carries_input_to_output_with_its_latency() {
    let mock = studio();
    mock.set_latency(
        Some(Duration::from_millis(2)),
        Some(Duration::from_millis(3)),
    );
    let mut manager = DeviceManager::headless(&mock);
    manager.set_max_block_size(64);
    let (infos, received) = mpsc::channel();
    let setup = DeviceSetup::default()
        .with_output(DeviceChoice::Device(DeviceRef::new("speakers", "Speakers")))
        .with_input(DeviceChoice::Device(DeviceRef::new("mic", "Microphone")));
    manager
        .start(&setup, move |io: &mut DeviceIo<'_>| {
            for channel in io.output.iter_channels_mut() {
                channel.copy_from_slice(io.input.channel(0));
            }
            let _ = infos.send(io.info);
        })
        .unwrap();

    // Ten blocks of a ramp in, ten blocks out.
    let mut played: Vec<f32> = Vec::new();
    for block in 0..10 {
        let ramp: Vec<f32> = (0..64).map(|n| (block * 64 + n) as f32 / 1_000.0).collect();
        assert!(mock.capture(&ramp));
        let mut out = [0.0f32; 128];
        assert!(mock.render(&mut out));
        played.extend(out.iter().step_by(2));
    }
    // The input arrives, in order and smoothly, after the reserve.
    let first = played.iter().position(|&sample| sample > 0.01).unwrap();
    assert!(first < 640 - 64, "nothing came through");
    assert!(
        played[first..]
            .windows(2)
            .all(|pair| (pair[1] - pair[0] - 0.001).abs() < 1e-4),
        "the ramp was broken"
    );
    let info = received.try_iter().last().unwrap();
    assert_eq!(info.output_latency, Some(Duration::from_millis(3)));
    let input = info.input_latency.unwrap();
    assert!(input > Duration::from_millis(2), "{input:?}");
    assert_eq!(info.frame, 9 * 64);
}

#[test]
fn the_watcher_thread_reports_changes() {
    let mock = studio();
    let manager = DeviceManager::headless(&mock);
    let (events, received) = mpsc::channel();
    let watcher = manager
        .watch(MIN_POLL_INTERVAL, move |event| {
            let _ = events.send(event);
        })
        .unwrap();
    assert_eq!(watcher.interval(), MIN_POLL_INTERVAL);
    // Let the first poll learn what is there, then change something.
    std::thread::sleep(MIN_POLL_INTERVAL * 3);
    mock.add_device(
        Direction::Input,
        MockBackend::device("headset", "Headset", 1, 16_000),
    );
    watcher.refresh();
    let event = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        event,
        DeviceEvent::DeviceAdded {
            direction: Direction::Input,
            device: summary("headset", "Headset"),
        }
    );
    drop(watcher);
}

#[cfg(feature = "midi")]
mod midi {
    use super::*;
    use manguebyte_devices::midi::{MidiError, MidiInputMessage};
    use manguebyte_midi::MidiMessage;

    #[test]
    fn midi_from_every_open_port_reaches_the_callback() {
        let mock = studio();
        mock.add_midi_port(Direction::Input, "keys", "Keys");
        mock.add_midi_port(Direction::Input, "pads", "Pads");
        mock.add_midi_port(Direction::Output, "synth", "Synth");
        let setup = DeviceSetup::default()
            .with_midi_input(DeviceRef::new("keys", "Keys"))
            .with_midi_input(DeviceRef::new("gone", "Gone"))
            .with_midi_output(DeviceRef::new("synth", "Synth"));

        let (notes, received) = mpsc::channel();
        let mut manager = DeviceManager::headless(&mock);
        let report = manager
            .start(&setup, move |io: &mut DeviceIo<'_>| {
                io.read_midi(|_, message| {
                    let _ = notes.send(message);
                });
            })
            .unwrap();
        assert!(matches!(
            &report.fallbacks[..],
            [Fallback::MidiPortMissing { .. }]
        ));
        assert_eq!(manager.midi_inputs(), [summary("keys", "Keys")]);

        assert!(mock.send_midi("keys", &[0x90, 60, 100]));
        assert!(!mock.send_midi("pads", &[0x90, 61, 100]), "not open");
        manager
            .enable_midi_input(DeviceRef::new("pads", "Pads"))
            .unwrap();
        assert!(mock.send_midi("pads", &[0x91, 62, 90]));
        render(&mock, 2).unwrap();
        let seen: Vec<MidiInputMessage> = received.try_iter().collect();
        assert_eq!(
            seen,
            [
                MidiInputMessage::Channel(MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100
                }),
                MidiInputMessage::Channel(MidiMessage::NoteOn {
                    channel: 1,
                    note: 62,
                    velocity: 90
                }),
            ]
        );

        // MIDI keeps flowing through a restart of the audio.
        manager.restart().unwrap();
        assert!(mock.send_midi("keys", &[0x80, 60, 0]));
        render(&mock, 2).unwrap();
        assert_eq!(received.try_iter().count(), 1);

        manager.send_midi(&[0xB0, 7, 100]).unwrap();
        assert_eq!(
            mock.take_sent_midi(),
            [("synth".to_owned(), vec![0xB0, 7, 100])]
        );
        // A MIDI 2.0 packet goes out by the default translation.
        let note = manguebyte_midi::ump::Packet::from_words(&[0x4091_3C00, 0xC000_0000]).unwrap();
        manager.send_midi_packet(&note).unwrap();
        assert_eq!(
            mock.take_sent_midi(),
            [("synth".to_owned(), vec![0x91, 60, 0x60])]
        );
        manager.disable_midi_input("keys");
        assert!(!mock.send_midi("keys", &[0x90, 60, 100]));
        manager.set_midi_output(None).unwrap();
        assert!(matches!(
            manager.send_midi(&[0xF8]),
            Err(MidiError::PortNotFound(_))
        ));
    }

    #[test]
    fn a_midi_port_of_the_setup_is_opened_when_it_appears() {
        let mock = studio();
        let mut manager = DeviceManager::headless(&mock);
        let setup = DeviceSetup::default().with_midi_input(DeviceRef::new("keys", "Keys"));
        manager.start(&setup, level(0.0)).unwrap();
        manager.poll_devices();
        assert!(manager.midi_inputs().is_empty());

        // Plugged in under a new identifier: found by its name.
        mock.add_midi_port(Direction::Input, "keys-2", "Keys");
        let events = manager.poll_devices();
        assert_eq!(
            events,
            [DeviceEvent::MidiPortAdded {
                direction: Direction::Input,
                port: summary("keys-2", "Keys"),
            }]
        );
        assert!(manager.follow(&events[0]).unwrap().is_ok());
        assert_eq!(manager.midi_inputs(), [summary("keys-2", "Keys")]);
        assert!(manager.is_running(), "the audio was left alone");

        mock.remove_midi_port(Direction::Input, "keys-2");
        let events = manager.poll_devices();
        assert!(manager.follow(&events[0]).is_some());
        assert!(manager.midi_inputs().is_empty());
    }
}
