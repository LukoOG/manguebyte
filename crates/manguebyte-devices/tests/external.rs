//! A backend from outside the crate, behind the device manager: the seam a
//! crate of its own implements to bring another driver model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use manguebyte_core::AudioBuffer;
use manguebyte_devices::{
    AudioBackend, AudioCallback, BackendCallback, BackendFormat, BackendRequest, BackendStream,
    CallbackInfo, ConfigRange, DeviceChoice, DeviceError, DeviceEvent, DeviceInfo, DeviceIo,
    DeviceManager, DeviceSetup, DeviceSnapshot, DeviceSummary, Direction, Fallback, LossCause,
    StreamFormat, StreamSettings, StreamStatus,
};

#[derive(Default)]
struct Shared {
    callback: Option<BackendCallback>,
    status: Option<StreamStatus>,
    requests: Vec<BackendRequest>,
}

/// One device that is both an input and an output, run by hand.
#[derive(Clone, Default)]
struct Loopback {
    shared: Arc<Mutex<Shared>>,
}

fn device() -> DeviceSummary {
    DeviceSummary {
        id: String::from("loop"),
        name: String::from("Loop"),
    }
}

/// Drops the callback when the manager drops the stream.
struct Running(Arc<Mutex<Shared>>);

impl Drop for Running {
    fn drop(&mut self) {
        let callback = self.0.lock().unwrap().callback.take();
        // Outside the lock: dropping it hands the application's callback
        // back to the manager.
        drop(callback);
    }
}

impl Loopback {
    /// Runs one block through the stream, as a driver's thread would.
    fn run(&self, input: &mut AudioBuffer<f32>, output: &mut AudioBuffer<f32>) -> bool {
        let mut shared = self.shared.lock().unwrap();
        let Some(callback) = shared.callback.as_mut() else {
            return false;
        };
        let info = CallbackInfo {
            sample_rate: f64::from(callback.format().sample_rate),
            ..CallbackInfo::default()
        };
        callback.process(input, output, &info);
        true
    }

    fn status(&self) -> StreamStatus {
        self.shared.lock().unwrap().status.clone().unwrap()
    }
}

impl AudioBackend for Loopback {
    fn host_name(&self) -> String {
        String::from("Loopback")
    }

    fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        Ok(DeviceSnapshot {
            outputs: vec![device()],
            inputs: vec![device()],
            default_output: Some(device().id),
            default_input: Some(device().id),
            ..DeviceSnapshot::default()
        })
    }

    fn device_info(&self, _: Direction, id: &str) -> Result<DeviceInfo, DeviceError> {
        if id != "loop" {
            return Err(DeviceError::DeviceNotFound(id.to_owned()));
        }
        Ok(DeviceInfo {
            id: device().id,
            name: device().name,
            is_default: true,
            default_sample_rate: Some(48_000),
            default_channels: Some(2),
            configs: (1..=2)
                .map(|channels| ConfigRange {
                    channels,
                    min_sample_rate: 44_100,
                    max_sample_rate: 48_000,
                    buffer_size: Some((16, 4_096)),
                })
                .collect(),
        })
    }

    fn start(
        &self,
        request: &BackendRequest,
        make: &mut dyn FnMut(BackendFormat) -> BackendCallback,
        status: StreamStatus,
    ) -> Result<BackendStream, DeviceError> {
        let settings = |side: &Option<manguebyte_devices::BackendEndpoint>| {
            side.as_ref().map(|endpoint| StreamSettings {
                sample_rate: Some(endpoint.settings.sample_rate.unwrap_or(48_000)),
                channels: Some(endpoint.settings.channels.unwrap_or(2)),
                buffer_size: endpoint.settings.buffer_size,
            })
        };
        let (input, output) = (settings(&request.input), settings(&request.output));
        let channels =
            |side: &Option<StreamSettings>| side.and_then(|s| s.channels).map_or(0, usize::from);
        let rate = output.or(input).and_then(|s| s.sample_rate).unwrap();
        let callback = make(BackendFormat::new(
            rate,
            channels(&input),
            channels(&output),
        ));
        let mut shared = self.shared.lock().unwrap();
        shared.requests.push(request.clone());
        shared.callback = Some(callback);
        shared.status = Some(status);
        let mut stream = BackendStream::new(Running(Arc::clone(&self.shared)), rate);
        if let Some(settings) = input {
            stream = stream.with_input(device(), settings);
        }
        if let Some(settings) = output {
            stream = stream.with_output(device(), settings);
        }
        Ok(stream)
    }
}

/// Passes the input through, and counts how often it was prepared and how
/// long the blocks it was handed were.
struct Through {
    prepared: Arc<AtomicUsize>,
    longest: Arc<AtomicUsize>,
}

impl AudioCallback for Through {
    fn prepare(&mut self, format: &StreamFormat) {
        assert_eq!(format.input_channels, 2);
        self.prepared.fetch_add(1, Ordering::Relaxed);
    }

    fn process(&mut self, io: &mut DeviceIo<'_>) {
        self.longest.fetch_max(io.num_frames(), Ordering::Relaxed);
        for channel in 0..io.output.num_channels() {
            io.output
                .channel_mut(channel)
                .copy_from_slice(io.input.channel(channel));
        }
    }
}

#[test]
fn a_duplex_stream_runs_on_an_outside_backend_and_restarts_when_asked() {
    let backend = Loopback::default();
    let mut manager = DeviceManager::with_audio_backend(backend.clone());
    manager.set_max_block_size(256);
    let prepared = Arc::new(AtomicUsize::new(0));
    let longest = Arc::new(AtomicUsize::new(0));
    let setup = DeviceSetup::default().with_input(DeviceChoice::Default);
    let report = manager
        .start(
            &setup,
            Through {
                prepared: Arc::clone(&prepared),
                longest: Arc::clone(&longest),
            },
        )
        .unwrap();
    assert!(report.is_exact(), "{:?}", report.fallbacks);
    assert_eq!(manager.host_name(), "Loopback");
    assert_eq!(manager.running_device(Direction::Input), Some(&device()));
    assert_eq!(manager.format().unwrap().sample_rate, 48_000.0);
    {
        let shared = backend.shared.lock().unwrap();
        assert_eq!(shared.requests[0].max_block_size, 256);
        assert_eq!(shared.requests[0].input.as_ref().unwrap().id, "loop");
    }

    // A block four times the largest the callback takes.
    let mut input = AudioBuffer::new(2, 1_024);
    for (index, sample) in input.channel_mut(1).iter_mut().enumerate() {
        *sample = index as f32;
    }
    let mut output = AudioBuffer::new(2, 1_024);
    assert!(backend.run(&mut input, &mut output));
    assert_eq!(output.channel(1), input.channel(1));
    assert_eq!(longest.load(Ordering::Relaxed), 256);

    // The driver's settings change: the backend asks for a restart, and
    // the manager hears of it once, and restarts on the next follow.
    manager.poll_devices();
    backend.status().request_restart();
    let events = manager.poll_devices();
    assert_eq!(
        events,
        [DeviceEvent::StreamLost {
            direction: Direction::Output,
            device: device(),
            cause: LossCause::RestartRequested,
        }]
    );
    assert!(manager.follow(&events[0]).unwrap().unwrap().is_exact());
    assert_eq!(prepared.load(Ordering::Relaxed), 2);
    assert_eq!(backend.shared.lock().unwrap().requests.len(), 2);
    assert_eq!(manager.poll_devices(), []);
    assert!(backend.run(&mut input, &mut output));

    // Stopping drops the backend's stream and gives the callback back.
    manager.stop();
    assert!(!backend.run(&mut input, &mut output));
    assert!(manager.take_callback().is_some());
}

#[test]
fn the_backend_is_the_only_host_and_reports_its_errors() {
    let backend = Loopback::default();
    let mut manager = DeviceManager::with_audio_backend(backend.clone());
    let errors = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&errors);
    manager.set_error_handler(move |error| {
        assert!(matches!(error, DeviceError::Other(_)));
        assert_eq!(
            error.to_string(),
            "audio backend error: the driver hiccupped"
        );
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let hosts = manager.hosts();
    assert_eq!(hosts.len(), 1);
    assert!(hosts[0].is_default);

    let report = manager
        .start(
            &DeviceSetup::default().with_host("WASAPI"),
            |_: &mut DeviceIo<'_>| {},
        )
        .unwrap();
    assert!(matches!(
        report.fallbacks.as_slice(),
        [Fallback::HostUnavailable { used, .. }] if used == "Loopback"
    ));
    let report = manager
        .apply(&DeviceSetup::default().with_host("loopback"))
        .unwrap();
    assert!(report.is_exact());

    let failure: Box<dyn std::error::Error + Send + Sync> = "the driver hiccupped".into();
    backend.status().report_error(DeviceError::Other(failure));
    assert_eq!(errors.load(Ordering::Relaxed), 1);
    backend.status().report_lost();
    assert!(matches!(
        manager.poll_devices().as_slice(),
        [DeviceEvent::StreamLost {
            cause: LossCause::ReportedByBackend,
            ..
        }]
    ));
}
