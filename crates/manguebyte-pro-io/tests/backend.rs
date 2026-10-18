//! The drivers behind a `manguebyte-devices` device manager, with fake
//! drivers: duplex streams, restarts and fallbacks.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use manguebyte_devices::{
    AudioCallback, DeviceChoice, DeviceEvent, DeviceIo, DeviceManager, DeviceRef, DeviceSetup,
    DeviceSummary, Direction, Fallback, LossCause, StreamFormat,
};
use manguebyte_pro_io::{BufferSizes, FakeDevice, ID_PREFIX, ProIoBackend, SampleType};

fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn summary(name: &str) -> DeviceSummary {
    DeviceSummary {
        id: format!("{ID_PREFIX}{name}"),
        name: name.to_owned(),
    }
}

/// Passes the input through, and counts how often it was prepared and the
/// largest block it was handed.
struct Through {
    prepared: Arc<AtomicUsize>,
    longest: Arc<AtomicUsize>,
    rate: Arc<AtomicU32>,
}

impl Through {
    fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicU32>) {
        let prepared = Arc::new(AtomicUsize::new(0));
        let longest = Arc::new(AtomicUsize::new(0));
        let rate = Arc::new(AtomicU32::new(0));
        (
            Self {
                prepared: Arc::clone(&prepared),
                longest: Arc::clone(&longest),
                rate: Arc::clone(&rate),
            },
            prepared,
            longest,
            rate,
        )
    }
}

impl AudioCallback for Through {
    fn prepare(&mut self, format: &StreamFormat) {
        self.prepared.fetch_add(1, Ordering::Relaxed);
        self.rate
            .store(format.sample_rate as u32, Ordering::Relaxed);
    }

    fn process(&mut self, io: &mut DeviceIo<'_>) {
        self.longest.fetch_max(io.num_frames(), Ordering::Relaxed);
        let channels = io.output.num_channels().min(io.input.num_channels());
        for channel in 0..channels {
            io.output
                .channel_mut(channel)
                .copy_from_slice(io.input.channel(channel));
        }
    }
}

/// Polls until a stream is reported lost, for at most a second.
fn wait_for_loss(manager: &mut DeviceManager) -> Vec<DeviceEvent> {
    let start = Instant::now();
    loop {
        let events = manager.poll_devices();
        if events
            .iter()
            .any(|event| matches!(event, DeviceEvent::StreamLost { .. }))
            || start.elapsed() > Duration::from_secs(1)
        {
            return events;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ramp(frames: usize) -> Vec<u8> {
    let samples: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
    let mut bytes = vec![0u8; frames * 4];
    SampleType::Float32Lsb.write(&samples, &mut bytes).unwrap();
    bytes
}

#[test]
fn a_duplex_stream_runs_on_one_driver_through_the_manager() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Studio").with_buffer_sizes(BufferSizes {
        min: 64,
        max: 2_048,
        preferred: 1_024,
        granularity: -1,
    });
    let mut manager =
        DeviceManager::with_audio_backend(ProIoBackend::with_fake_drivers([device.clone()]));
    manager.set_max_block_size(256);
    assert_eq!(manager.host_name(), "ASIO");
    let devices = manager.devices().unwrap();
    assert_eq!(devices.outputs, [summary("Studio")]);
    assert_eq!(devices.default_input.as_deref(), Some("asio:Studio"));

    let info = manager
        .device_info(Direction::Input, "asio:Studio")
        .unwrap();
    assert_eq!(info.default_channels, Some(2));
    assert_eq!(info.default_sample_rate, Some(48_000));
    assert!(info.supports_sample_rate(96_000) && !info.supports_sample_rate(22_050));
    assert_eq!(info.configs[0].buffer_size, Some((64, 2_048)));
    // Describing it loaded the driver for a moment only.
    assert_eq!(device.instances_alive(), 0);

    let (callback, prepared, longest, _) = Through::new();
    let setup = DeviceSetup::default().with_input(DeviceChoice::Default);
    let report = manager.start(&setup, callback).unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert_eq!(prepared.load(Ordering::Relaxed), 1);
    let format = manager.format().unwrap();
    assert_eq!((format.input_channels, format.output_channels), (2, 2));
    assert_eq!(format.sample_rate, 48_000.0);
    assert_eq!(
        manager
            .running_settings(Direction::Output)
            .unwrap()
            .buffer_size,
        Some(1_024)
    );
    assert_eq!(
        manager.running_device(Direction::Input),
        Some(&summary("Studio"))
    );

    let block = ramp(1_024);
    assert!(device.write_input(0, &block));
    assert!(device.switch());
    assert_eq!(device.read_output(0).unwrap(), block);
    // The driver's 1,024 frames reach the callback in blocks of 256.
    assert_eq!(longest.load(Ordering::Relaxed), 256);

    manager.stop();
    assert!(!device.is_running());
    assert_eq!(device.instances_alive(), 0, "the driver is released");
}

#[test]
fn a_reset_request_reopens_the_driver_like_a_device_that_came_back() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Resetting");
    let mut manager =
        DeviceManager::with_audio_backend(ProIoBackend::with_fake_drivers([device.clone()]));
    let (callback, prepared, _, _) = Through::new();
    manager
        .start(
            &DeviceSetup::default().with_input(DeviceChoice::Default),
            callback,
        )
        .unwrap();
    manager.poll_devices();
    assert!(device.switch());

    // Inside the driver's call: only noted. The manager hears of it on its
    // next poll, once, and restarts on its own thread.
    assert_eq!(device.request_reset(), Some(1));
    let events = wait_for_loss(&mut manager);
    assert_eq!(
        events,
        [DeviceEvent::StreamLost {
            direction: Direction::Output,
            device: summary("Resetting"),
            cause: LossCause::RestartRequested,
        }]
    );
    let report = manager.follow(&events[0]).unwrap().unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert_eq!(prepared.load(Ordering::Relaxed), 2);
    // Closed and opened again, as the SDK asks of a reset: once to be
    // described, once for the first stream, once for the second.
    assert_eq!(
        (device.instances_created(), device.instances_alive()),
        (3, 1)
    );
    assert_eq!((device.stops(), device.buffer_disposals()), (1, 1));
    assert!(device.is_running());
    assert!(device.switch());
    assert_eq!(manager.poll_devices(), []);
}

#[test]
fn a_new_buffer_size_or_rate_from_the_driver_is_taken_on_the_restart() {
    let _turn = one_at_a_time();
    let device = FakeDevice::new("Changing");
    let mut manager =
        DeviceManager::with_audio_backend(ProIoBackend::with_fake_drivers([device.clone()]));
    let (callback, _, _, rate) = Through::new();
    // The setup asks for 128 frames; the driver's panel then asks for 512.
    let setup = DeviceSetup::default()
        .with_input(DeviceChoice::Default)
        .with_buffer_size(128);
    manager.start(&setup, callback).unwrap();
    assert_eq!(device.buffer_size(), Some(128));
    manager.poll_devices();

    assert_eq!(device.change_buffer_size(512), Some(1));
    let events = wait_for_loss(&mut manager);
    manager.follow(&events[0]).unwrap().unwrap();
    assert_eq!(device.buffer_size(), Some(512));
    assert_eq!(
        manager
            .running_settings(Direction::Input)
            .unwrap()
            .buffer_size,
        Some(512)
    );

    // An external clock moves the driver to 96 kHz: the callback is
    // prepared again at that rate.
    manager.poll_devices();
    assert!(device.change_sample_rate(96_000.0));
    let events = wait_for_loss(&mut manager);
    assert!(matches!(
        events.as_slice(),
        [DeviceEvent::StreamLost {
            cause: LossCause::RestartRequested,
            ..
        }]
    ));
    manager.follow(&events[0]).unwrap().unwrap();
    assert_eq!(rate.load(Ordering::Relaxed), 96_000);
    assert_eq!(manager.format().unwrap().sample_rate, 96_000.0);
}

#[test]
fn an_input_and_an_output_on_two_drivers_run_as_output_only() {
    let _turn = one_at_a_time();
    let first = FakeDevice::new("First");
    let second = FakeDevice::new("Second");
    let mut manager = DeviceManager::with_audio_backend(
        ProIoBackend::with_fake_drivers([first.clone(), second.clone()])
            .with_default_driver("Second"),
    );
    assert_eq!(
        manager.devices().unwrap().default_output.as_deref(),
        Some("asio:Second")
    );
    let setup = DeviceSetup::default()
        .with_output(DeviceChoice::Device(DeviceRef::new("asio:First", "First")))
        .with_input(DeviceChoice::Device(DeviceRef::new(
            "asio:Second",
            "Second",
        )));
    let (callback, _, _, _) = Through::new();
    let report = manager.start(&setup, callback).unwrap();
    assert!(
        matches!(
            report.fallbacks.as_slice(),
            [Fallback::InputDropped { device, .. }] if device.id == "asio:Second"
        ),
        "{:?}",
        report.fallbacks
    );
    assert!(manager.running_device(Direction::Input).is_none());
    assert!(first.is_running());
    assert_eq!(second.instances_alive(), 0);
}

#[test]
fn a_driver_that_will_not_initialise_is_no_device() {
    let _turn = one_at_a_time();
    let broken = FakeDevice::new("Unplugged").with_failing_init(true);
    let mut manager =
        DeviceManager::with_audio_backend(ProIoBackend::with_fake_drivers([broken.clone()]));
    let (callback, _, _, _) = Through::new();
    let report = manager.start(&DeviceSetup::default(), callback).unwrap();
    assert!(!manager.is_running());
    assert!(
        report
            .fallbacks
            .iter()
            .any(|fallback| matches!(fallback, Fallback::NoDevice { .. })),
        "{:?}",
        report.fallbacks
    );
    assert_eq!(broken.instances_alive(), 0);
}
