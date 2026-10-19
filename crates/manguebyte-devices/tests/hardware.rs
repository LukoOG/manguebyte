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
