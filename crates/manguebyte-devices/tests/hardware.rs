//! Tests that need real audio hardware.
//!
//! They are ignored by default because CI machines have no sound card. Run
//! them on a machine that does with:
//!
//! ```text
//! cargo test -p manguebyte-devices --test hardware -- --ignored --test-threads 1
//! ```
//!
//! Nothing audible is played: every output callback leaves its buffer silent.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use manguebyte_devices::{
    DuplexStream, InputDevice, OutputDevice, StreamSettings, input_devices, output_devices,
};

#[cfg(feature = "midi")]
#[test]
#[ignore = "needs the system MIDI service"]
fn lists_midi_ports_and_reports_a_missing_one() {
    use manguebyte_devices::midi::{
        MidiError, MidiInput, MidiOutput, midi_input_ports, midi_output_ports,
    };

    let inputs = midi_input_ports().unwrap();
    let outputs = midi_output_ports().unwrap();
    println!("MIDI inputs: {inputs:?}");
    println!("MIDI outputs: {outputs:?}");
    assert!(
        inputs
            .iter()
            .chain(&outputs)
            .all(|port| !port.id.is_empty())
    );

    assert!(matches!(
        MidiInput::open("no-such-port"),
        Err(MidiError::PortNotFound(_))
    ));
    assert!(matches!(
        MidiOutput::open("no-such-port"),
        Err(MidiError::PortNotFound(_))
    ));

    // Opening is enough to check the connection; nothing is sent, so no
    // synthesiser makes a sound.
    if let Some(port) = outputs.first() {
        let output = MidiOutput::open(&port.id).unwrap();
        assert_eq!(output.name(), port.name);
    }
    if let Some(port) = inputs.first() {
        let (input, mut receiver) = MidiInput::open(&port.id).unwrap();
        assert_eq!(input.name(), port.name);
        receiver.read(256, 48_000.0, |_, _| {});
    }
}

#[test]
#[ignore = "needs an audio output device"]
fn lists_output_devices_and_marks_one_as_default() {
    let devices = output_devices().unwrap();
    for device in &devices {
        println!("{device:?}");
    }
    assert_eq!(devices.iter().filter(|d| d.is_default).count(), 1);
    assert!(devices.iter().all(|d| !d.id.is_empty()));
}

#[test]
#[ignore = "needs an audio output device"]
fn output_runs_and_can_be_reopened_by_id() {
    let default = OutputDevice::open_default().unwrap();
    let device = OutputDevice::open_by_id(default.id()).unwrap();
    assert_eq!(device.name(), default.name());

    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = device
        .start(256, move |buffer| {
            assert!(buffer.num_frames() <= 256);
            counter.fetch_add(buffer.num_frames(), Ordering::Relaxed);
        })
        .unwrap();
    sleep(Duration::from_millis(400));
    drop(stream);

    let rendered = frames.load(Ordering::Relaxed) as f64;
    assert!(
        rendered > device.sample_rate() * 0.2,
        "only {rendered} frames in 400 ms"
    );
}

#[test]
#[ignore = "needs an audio output device"]
fn impossible_settings_are_refused() {
    let device = OutputDevice::open_default().unwrap();
    let result = device.with_settings(StreamSettings::default().with_sample_rate(1));
    assert!(result.is_err());
}

#[test]
#[ignore = "needs an audio input device"]
fn input_delivers_blocks() {
    println!("{:?}", input_devices().unwrap());
    let device = InputDevice::open_default().unwrap();
    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = device
        .start(256, move |buffer| {
            counter.fetch_add(buffer.num_frames(), Ordering::Relaxed);
        })
        .unwrap();
    sleep(Duration::from_millis(400));
    drop(stream);
    assert!(frames.load(Ordering::Relaxed) as f64 > device.sample_rate() * 0.2);
}

#[test]
#[ignore = "needs audio input and output devices"]
fn duplex_carries_input_to_the_output_callback() {
    let output = OutputDevice::open_default().unwrap();
    let input = InputDevice::open_default()
        .unwrap()
        .with_settings(StreamSettings::default().with_sample_rate(output.sample_rate() as u32))
        .unwrap();

    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = DuplexStream::start(&input, &output, 256, move |input, output| {
        assert_eq!(input.num_frames(), output.num_frames());
        counter.fetch_add(input.num_frames(), Ordering::Relaxed);
    })
    .unwrap();
    sleep(Duration::from_millis(1_000));
    let (underruns, overruns) = (stream.underruns(), stream.overruns());
    drop(stream);

    println!("underruns: {underruns}, overruns: {overruns}");
    assert!(frames.load(Ordering::Relaxed) as f64 > output.sample_rate() * 0.5);
    assert!(
        underruns < 20,
        "the reserve never settled: {underruns} underruns"
    );
}

#[test]
#[ignore = "needs an audio output device"]
fn the_device_manager_runs_and_restores_the_system_default() {
    use manguebyte_devices::{
        AudioHost, DeviceEvent, DeviceIo, DeviceManager, DeviceSetup, Direction, audio_hosts,
    };

    println!("hosts: {:?}", audio_hosts());
    for host in audio_hosts() {
        let host = AudioHost::by_name(&host.name).unwrap();
        println!(
            "{}: {} outputs",
            host.name(),
            host.output_devices().unwrap().len()
        );
    }

    let mut manager = DeviceManager::new();
    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let report = manager
        .start(&DeviceSetup::default(), move |io: &mut DeviceIo<'_>| {
            counter.fetch_add(io.num_frames(), Ordering::Relaxed);
        })
        .unwrap();
    println!("report: {report:?}, format: {:?}", manager.format());
    assert!(manager.is_running());
    assert!(
        manager.poll_devices().is_empty(),
        "the first poll only learns"
    );
    sleep(Duration::from_millis(400));
    let events = manager.poll_devices();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, DeviceEvent::StreamLost { .. })),
        "{events:?}"
    );

    // Saved and restored, it opens the same device.
    let saved = manager.setup().to_string();
    let device = manager.running_device(Direction::Output).cloned();
    manager.apply(&saved.parse().unwrap()).unwrap();
    assert_eq!(manager.running_device(Direction::Output).cloned(), device);
    sleep(Duration::from_millis(200));
    drop(manager);
    let rendered = frames.load(Ordering::Relaxed);
    assert!(rendered > 10_000, "only {rendered} frames");
}

/// Runs an engine of silence and changes its output device under it: to
/// another output when there is one (a crossfade, both playing), then the
/// same device opened again (a gap). Reports how each change went.
#[test]
#[ignore = "needs an audio output device"]
fn the_engine_runs_on_while_the_output_device_changes() {
    use std::sync::atomic::AtomicU64;

    use manguebyte_devices::{
        AudioCallback, DeviceChoice, DeviceIo, DeviceManager, DeviceRef, DeviceSetup, Direction,
        EngineFormat, StreamFormat,
    };

    /// Leaves every block silent, and checks that the frames follow on.
    struct Silence {
        prepared: Arc<AtomicUsize>,
        frames: Arc<AtomicU64>,
        wrong: Arc<AtomicUsize>,
    }

    impl AudioCallback for Silence {
        fn prepare(&mut self, _: &StreamFormat) {
            self.prepared.fetch_add(1, Ordering::Relaxed);
        }

        fn process(&mut self, io: &mut DeviceIo<'_>) {
            let frames = io.num_frames() as u64;
            let expected = self.frames.fetch_add(frames, Ordering::Relaxed);
            if io.info.frame != expected || io.output.peak() != 0.0 {
                self.wrong.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let prepared = Arc::new(AtomicUsize::new(0));
    let frames = Arc::new(AtomicU64::new(0));
    let wrong = Arc::new(AtomicUsize::new(0));
    let mut manager = DeviceManager::new();
    let outputs = manager.devices().unwrap().outputs;
    println!("{}: {outputs:?}", manager.host_name());
    let format = EngineFormat::new(48_000, 512, 0, 2);
    let silence = Silence {
        prepared: Arc::clone(&prepared),
        frames: Arc::clone(&frames),
        wrong: Arc::clone(&wrong),
    };
    manager
        .start_engine(&DeviceSetup::default(), format, silence)
        .unwrap();
    let run = |manager: &mut DeviceManager, what: &str| {
        let before = frames.load(Ordering::Relaxed);
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_millis(800) {
            for event in manager.poll_devices() {
                println!("{what}: {event}");
                if let Some(result) = manager.follow(&event) {
                    result.unwrap();
                }
            }
            sleep(Duration::from_millis(20));
        }
        let swap = manager.last_swap().unwrap();
        println!(
            "{what}: {:?} on {:?} at {} Hz, {:?} of silence, reopened in {:?}, {} underruns, \
             {} engine frames since",
            swap.kind,
            manager.running_device(Direction::Output).map(|d| &d.name),
            manager.format().unwrap().sample_rate,
            swap.silence,
            swap.reopen,
            swap.underruns,
            frames.load(Ordering::Relaxed) - before
        );
        assert!(frames.load(Ordering::Relaxed) > before + 10_000, "{what}");
    };
    run(&mut manager, "start");
    let first = manager.running_device(Direction::Output).cloned().unwrap();
    if let Some(other) = outputs.iter().find(|device| device.id != first.id) {
        let choice = DeviceChoice::Device(DeviceRef::from(other));
        manager
            .apply(&DeviceSetup::default().with_output(choice))
            .unwrap();
        run(&mut manager, "to another output");
    }
    manager.restart().unwrap();
    run(&mut manager, "the same output again");
    assert_eq!(prepared.load(Ordering::Relaxed), 1);
    assert_eq!(wrong.load(Ordering::Relaxed), 0);
}

#[cfg(all(feature = "midi", unix, not(target_os = "android")))]
#[test]
#[ignore = "needs the system MIDI service"]
fn a_virtual_port_is_seen_by_other_programs() {
    use manguebyte_devices::midi::{MidiInput, MidiOutput, midi_input_ports};

    let name = "Manguebyte virtual test";
    let mut output = MidiOutput::create_virtual(name).unwrap();
    sleep(Duration::from_millis(200));
    let port = midi_input_ports()
        .unwrap()
        .into_iter()
        .find(|port| port.name.contains(name))
        .expect("the virtual port is listed");
    let (_input, mut receiver) = MidiInput::open(&port.id).unwrap();
    output.send_raw(&[0x90, 60, 100]).unwrap();
    sleep(Duration::from_millis(200));
    let mut received = 0;
    receiver.read(256, 48_000.0, |_, _| received += 1);
    assert_eq!(received, 1);
}

#[cfg(feature = "midi")]
#[test]
#[ignore = "needs the system MIDI service"]
fn lists_output_ports_with_the_protocol_each_would_speak() {
    use manguebyte_devices::midi::{
        MidiOutput, MidiProtocol, ProtocolFallback, midi_output_ports_with_protocols,
        ump_output_backend,
    };

    match ump_output_backend() {
        Ok(backend) => println!("UMP output: {backend}"),
        Err(why) => println!("UMP output: none ({why})"),
    }
    let ports = midi_output_ports_with_protocols().unwrap();
    for info in &ports {
        println!("{:?} {:?} {:?}", info.protocol, info.port, info.fallback);
        assert_eq!(
            info.protocol == MidiProtocol::Midi2,
            info.fallback.is_none()
        );
    }
    // Opening is enough to see the protocol chosen; nothing is sent, so no
    // synthesiser makes a sound.
    if let Some(info) = ports.first() {
        let output = MidiOutput::open_with_protocol(&info.port.id, MidiProtocol::Midi2).unwrap();
        assert_eq!(output.protocol(), info.protocol);
        drop(output);
        let default = MidiOutput::open(&info.port.id).unwrap();
        assert_eq!(default.protocol(), MidiProtocol::Midi1);
        assert_eq!(default.fallback(), Some(&ProtocolFallback::NotRequested));
    }
}

/// A MIDI 2.0 note with what MIDI 1.0 cannot carry (a 16-bit velocity, a
/// pitch attribute, registered per-note controller 74), its note-off, and
/// a system exclusive message long enough for three packets.
#[cfg(all(feature = "midi", any(target_os = "macos", target_os = "linux")))]
fn midi2_packets() -> Vec<manguebyte_midi::ump::Packet> {
    use manguebyte_midi::ump::{ChannelVoice2, Message, NoteAttribute, sysex7_packets};

    let packet = |message| Message::Midi2 { group: 2, message }.to_packet();
    let mut packets = vec![
        packet(ChannelVoice2::NoteOn {
            channel: 9,
            note: 60,
            velocity: 0x1234,
            attribute: NoteAttribute::pitch(60.25),
        }),
        packet(ChannelVoice2::RegisteredPerNoteController {
            channel: 9,
            note: 60,
            controller: 74,
            value: 0x8765_4321,
        }),
        packet(ChannelVoice2::NoteOff {
            channel: 9,
            note: 60,
            velocity: 0x0100,
            attribute: NoteAttribute::NONE,
        }),
    ];
    packets.extend(sysex7_packets(2, &[0x7D; 15]).unwrap());
    packets
}

/// Sends `packets` to the port `id`, opened preferring MIDI 2.0, through
/// the queue and the thread the audio thread uses, and checks that it
/// speaks MIDI 2.0.
#[cfg(all(feature = "midi", any(target_os = "macos", target_os = "linux")))]
fn send_through_the_output_thread(id: &str, packets: &[manguebyte_midi::ump::Packet]) {
    use manguebyte_devices::midi::{MidiOutput, MidiProtocol, midi_output_queue};

    let output = MidiOutput::open_with_protocol(id, MidiProtocol::Midi2).unwrap();
    assert_eq!(
        (output.protocol(), output.fallback()),
        (MidiProtocol::Midi2, None),
        "the loopback endpoint should be opened for MIDI 2.0"
    );
    let (mut transmitter, queue) = midi_output_queue(64);
    let thread = output.spawn(queue);
    transmitter.begin_block(48, 48_000.0);
    for (offset, packet) in packets.iter().enumerate() {
        assert!(transmitter.send(offset as u32, *packet));
    }
    sleep(Duration::from_millis(100));
    assert_eq!(thread.errors(), 0);
    drop(thread);
}

/// The packets a UMP endpoint receives are the ones sent, word for word:
/// CoreMIDI's event lists reach a virtual destination of this process in
/// the MIDI 2.0 protocol. Nothing is sent to any other program or device.
#[cfg(all(feature = "midi", target_os = "macos"))]
#[test]
#[ignore = "needs the system MIDI service"]
fn midi_2_0_reaches_a_coremidi_ump_endpoint_unchanged() {
    use std::sync::Mutex;

    use coremidi::{Client, EventList, Properties, Protocol};

    let received = Arc::new(Mutex::new(Vec::<u32>::new()));
    let protocols = Arc::new(Mutex::new(Vec::new()));
    let client = Client::new("Manguebyte UMP loopback").unwrap();
    let destination = {
        let (received, protocols) = (Arc::clone(&received), Arc::clone(&protocols));
        client
            .virtual_destination_with_protocol(
                "Manguebyte UMP loopback",
                Protocol::Midi20,
                move |list: &EventList| {
                    protocols.lock().unwrap().push(list.protocol());
                    for packet in list.iter() {
                        received.lock().unwrap().extend_from_slice(packet.data());
                    }
                },
            )
            .unwrap()
    };
    let protocol: Result<i32, _> = destination.get_property(&Properties::protocol_id());
    println!("the virtual destination's kMIDIPropertyProtocolID: {protocol:?}");
    if protocol != Ok(2) {
        // Say what the destination speaks, as a driver says it of its own.
        destination
            .set_property(&Properties::protocol_id(), 2_i32)
            .unwrap();
    }
    let id = destination.unique_id().unwrap().to_string();
    sleep(Duration::from_millis(200));

    let packets = midi2_packets();
    send_through_the_output_thread(&id, &packets);
    sleep(Duration::from_millis(200));

    let expected: Vec<u32> = packets.iter().flat_map(|p| p.words().to_vec()).collect();
    assert_eq!(*received.lock().unwrap(), expected);
    assert!(
        protocols
            .lock()
            .unwrap()
            .iter()
            .all(|protocol| *protocol == Protocol::Midi20)
    );
}

/// The packets a UMP endpoint receives are the ones sent, word for word:
/// the ALSA sequencer carries them from this process's MIDI 2.0 client to
/// another MIDI 2.0 client of this process. Nothing is sent to any other
/// program or device. Needs alsa-lib 1.2.10, Linux 6.5 and the sequencer
/// (`/dev/snd/seq`).
#[cfg(all(feature = "midi", target_os = "linux"))]
#[test]
#[ignore = "needs the ALSA sequencer with UMP (Linux 6.5, alsa-lib 1.2.10)"]
fn midi_2_0_reaches_an_alsa_ump_endpoint_unchanged() {
    use manguebyte_ump_io::alsa::Input;

    let mut input = Input::open("Manguebyte UMP loopback", "in").unwrap();
    let (client, port) = input.address();
    let packets = midi2_packets();
    send_through_the_output_thread(&format!("{client}:{port}"), &packets);

    let expected: Vec<u32> = packets.iter().flat_map(|p| p.words().to_vec()).collect();
    let mut received = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while received.len() < expected.len() && std::time::Instant::now() < deadline {
        match input.read().unwrap() {
            Some(event) if event.is_ump() => received.extend_from_slice(event.words()),
            Some(_) => {}
            None => sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(received, expected);
}
