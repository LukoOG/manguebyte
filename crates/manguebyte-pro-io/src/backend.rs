//! The drivers as a backend of `manguebyte-devices`, behind its
//! `DeviceManager`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use manguebyte_devices::{
    AudioBackend, BackendCallback, BackendEndpoint, BackendFormat, BackendRequest, BackendStream,
    CallbackInfo, ConfigRange, DeviceError, DeviceInfo, DeviceSnapshot, DeviceSummary, Direction,
    StreamSettings, StreamStatus,
};

use crate::driver::{LoadOptions, Origin};
use crate::{
    BufferSizes, Channels, Driver, Error, FakeDevice, Notifications, Stream, StreamConfig,
};

/// The host name the backend gives itself, which a
/// [`DeviceSetup`](manguebyte_devices::DeviceSetup) names.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub const HOST_NAME: &str = "ASIO";

/// What every device identifier of the backend starts with, so that a
/// saved setup never takes a device of another host for one of these.
pub const ID_PREFIX: &str = "asio:";

/// What a driver told the backend about itself.
#[derive(Debug, Clone)]
struct Description {
    channels: Channels,
    sample_rate: f64,
    sample_rates: Vec<f64>,
    buffer_sizes: BufferSizes,
}

#[derive(Default)]
struct State {
    described: HashMap<String, Description>,
    /// Buffer sizes drivers asked for while their stream ran, used when it
    /// opens again.
    buffer_sizes: HashMap<String, u32>,
}

enum Source {
    System,
    Fake(Vec<FakeDevice>),
}

/// The installed drivers as a backend of a
/// [`DeviceManager`](manguebyte_devices::DeviceManager).
///
/// Every driver is both an input and an output device, with the
/// identifier [`ID_PREFIX`] followed by its registry key, so a duplex
/// stream is the natural case: one driver, one callback, one clock. A
/// setup whose input and output are two different drivers cannot run,
/// since a process loads one driver at a time; the manager then drops the
/// input and says so in its report.
///
/// The default device is the one [`with_default_driver`] names, or else the
/// first driver the registry lists.
///
/// A driver loads when its stream starts, and is released when it stops.
/// To describe a driver the backend loads it for a moment, unless a stream
/// runs on another one, which keeps it from being loaded: then only a
/// driver described before can be described.
///
/// When the driver asks for a reset, or changes its buffer size or sample
/// rate, the stream asks the manager for a restart
/// ([`LossCause::RestartRequested`]), which the manager's next poll
/// reports and [`follow`] carries out: the stream stops, the driver is
/// released and loaded again, and the stream starts with the driver's new
/// settings, on the manager's thread.
///
/// [`with_default_driver`]: Self::with_default_driver
/// [`LossCause::RestartRequested`]: manguebyte_devices::LossCause::RestartRequested
/// [`follow`]: manguebyte_devices::DeviceManager::follow
///
/// ```no_run
/// use manguebyte_devices::{DeviceChoice, DeviceIo, DeviceManager, DeviceSetup};
/// use manguebyte_pro_io::ProIoBackend;
///
/// let mut manager = DeviceManager::with_audio_backend(ProIoBackend::new());
/// let setup = DeviceSetup::default().with_input(DeviceChoice::Default);
/// manager.start(&setup, |io: &mut DeviceIo<'_>| {
///     // Silence: the output starts silent, so leaving it alone plays none.
///     let _ = io.input.num_channels();
/// })?;
/// loop {
///     for event in manager.poll_devices() {
///         manager.follow(&event);
///     }
///     std::thread::sleep(std::time::Duration::from_secs(1));
/// #   break;
/// }
/// # Ok::<(), manguebyte_devices::DeviceError>(())
/// ```
pub struct ProIoBackend {
    source: Source,
    options: LoadOptions,
    default: Option<String>,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for ProIoBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProIoBackend")
            .field("fake", &matches!(self.source, Source::Fake(_)))
            .field("default", &self.default)
            .finish_non_exhaustive()
    }
}

impl Default for ProIoBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn other(error: Error) -> DeviceError {
    match error {
        Error::DriverNotFound(name) => DeviceError::DeviceNotFound(format!("{ID_PREFIX}{name}")),
        Error::InvalidConfig(reason) => DeviceError::UnsupportedSettings(reason),
        error => DeviceError::Other(Box::new(error)),
    }
}

fn frames_to_duration(frames: u32, rate: f64) -> Option<Duration> {
    (rate > 0.0).then(|| Duration::from_secs_f64(f64::from(frames) / rate))
}

impl ProIoBackend {
    /// The drivers installed on this system.
    pub fn new() -> Self {
        Self {
            source: Source::System,
            options: LoadOptions::default(),
            default: None,
            state: Arc::default(),
        }
    }

    /// Fake drivers instead of the system's, for tests: the same backend,
    /// loading each [`FakeDevice`] where it would create a driver through
    /// COM.
    pub fn with_fake_drivers(devices: impl IntoIterator<Item = FakeDevice>) -> Self {
        Self {
            source: Source::Fake(devices.into_iter().collect()),
            ..Self::new()
        }
    }

    /// Hands drivers the application's main window when they load (see
    /// [`LoadOptions::window`]).
    #[must_use]
    pub fn with_window(mut self, window: isize) -> Self {
        self.options.window = Some(window);
        self
    }

    /// Makes the driver with this registry key the default device.
    #[must_use]
    pub fn with_default_driver(mut self, name: &str) -> Self {
        self.default = Some(name.to_owned());
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every driver: identifier, name, and where it comes from.
    fn entries(&self) -> Result<Vec<(String, String, Origin)>, DeviceError> {
        Ok(match &self.source {
            Source::System => crate::drivers()
                .map_err(other)?
                .into_iter()
                .map(|entry| {
                    (
                        format!("{ID_PREFIX}{}", entry.name),
                        entry.description.clone(),
                        Origin::Registry(entry),
                    )
                })
                .collect(),
            Source::Fake(devices) => devices
                .iter()
                .map(|device| {
                    let name = device.name();
                    (
                        format!("{ID_PREFIX}{name}"),
                        name,
                        Origin::Fake(device.clone()),
                    )
                })
                .collect(),
        })
    }

    fn find(&self, id: &str) -> Result<(String, Origin), DeviceError> {
        self.entries()?
            .into_iter()
            .find(|(other, _, _)| other == id)
            .map(|(_, name, origin)| (name, origin))
            .ok_or_else(|| DeviceError::DeviceNotFound(id.to_owned()))
    }

    fn default_id(&self, entries: &[(String, String, Origin)]) -> Option<String> {
        let wanted = self
            .default
            .as_ref()
            .map(|name| format!("{ID_PREFIX}{name}"));
        entries
            .iter()
            .find(|(id, _, _)| Some(id) == wanted.as_ref())
            .or(entries.first())
            .map(|(id, _, _)| id.clone())
    }

    fn describe(driver: &Driver) -> Result<Description, Error> {
        let info = driver.info()?;
        Ok(Description {
            channels: info.channels,
            sample_rate: info.sample_rate,
            sample_rates: info.sample_rates,
            buffer_sizes: info.buffer_sizes,
        })
    }

    fn device_info_from(
        id: &str,
        name: &str,
        description: &Description,
        direction: Direction,
        is_default: bool,
    ) -> DeviceInfo {
        let channels = description
            .channels
            .of(direction)
            .min(usize::from(u16::MAX)) as u16;
        let sizes = &description.buffer_sizes;
        let buffer_size = (sizes.max > 0).then_some((sizes.min.min(sizes.max), sizes.max));
        let configs = if channels == 0 {
            Vec::new()
        } else {
            (1..=channels)
                .flat_map(|channels| {
                    description
                        .sample_rates
                        .iter()
                        .map(move |&rate| ConfigRange {
                            channels,
                            min_sample_rate: rate.round() as u32,
                            max_sample_rate: rate.round() as u32,
                            buffer_size,
                        })
                })
                .collect()
        };
        DeviceInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            is_default,
            default_sample_rate: (description.sample_rate > 0.0)
                .then(|| description.sample_rate.round() as u32),
            default_channels: (channels > 0).then_some(channels),
            configs,
        }
    }
}

/// Keeps a stream running for the device manager. Dropping it stops the
/// stream, then releases the driver.
struct Running {
    stream: Option<Stream>,
    driver: Option<Driver>,
    id: String,
    buffer_size: Arc<AtomicU32>,
    /// The bits of the rate the driver reported last, as an `f64`.
    sample_rate: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.stream.take());
        drop(self.driver.take());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let wanted = self.buffer_size.load(Ordering::Acquire);
        if wanted > 0 {
            state.buffer_sizes.insert(self.id.clone(), wanted);
        }
        // A driver on an external clock that changed rate runs at the new
        // one now: the next plan should take it as the driver's own.
        let rate = f64::from_bits(self.sample_rate.load(Ordering::Acquire));
        if rate > 0.0 {
            if let Some(description) = state.described.get_mut(&self.id) {
                description.sample_rate = rate;
            }
        }
    }
}

impl AudioBackend for ProIoBackend {
    fn host_name(&self) -> String {
        String::from(HOST_NAME)
    }

    fn snapshot(&self) -> Result<DeviceSnapshot, DeviceError> {
        let entries = self.entries()?;
        let default = self.default_id(&entries);
        let devices: Vec<DeviceSummary> = entries
            .into_iter()
            .map(|(id, name, _)| DeviceSummary { id, name })
            .collect();
        Ok(DeviceSnapshot {
            outputs: devices.clone(),
            inputs: devices,
            default_output: default.clone(),
            default_input: default,
            ..DeviceSnapshot::default()
        })
    }

    fn device_info(&self, direction: Direction, id: &str) -> Result<DeviceInfo, DeviceError> {
        let entries = self.entries()?;
        let is_default = self.default_id(&entries).as_deref() == Some(id);
        let (_, name, origin) = entries
            .into_iter()
            .find(|(other, _, _)| other == id)
            .ok_or_else(|| DeviceError::DeviceNotFound(id.to_owned()))?;
        let cached = self.state().described.get(id).cloned();
        let description = match cached {
            Some(description) => description,
            None => {
                // Loaded for a moment, and released at once.
                let driver = Driver::open(origin, self.options).map_err(other)?;
                let description = Self::describe(&driver).map_err(other)?;
                drop(driver);
                self.state()
                    .described
                    .insert(id.to_owned(), description.clone());
                description
            }
        };
        Ok(Self::device_info_from(
            id,
            &name,
            &description,
            direction,
            is_default,
        ))
    }

    fn start(
        &self,
        request: &BackendRequest,
        make: &mut dyn FnMut(BackendFormat) -> BackendCallback,
        status: StreamStatus,
    ) -> Result<BackendStream, DeviceError> {
        let id = match (&request.input, &request.output) {
            (Some(input), Some(output)) if input.id != output.id => {
                return Err(DeviceError::UnsupportedSettings(String::from(
                    "a process runs one driver at a time, so the input and the output must be \
                     the same driver",
                )));
            }
            (_, Some(endpoint)) | (Some(endpoint), None) => endpoint.id.clone(),
            (None, None) => {
                return Err(DeviceError::UnsupportedSettings(String::from(
                    "neither an input nor an output was asked for",
                )));
            }
        };
        let (name, origin) = self.find(&id)?;
        let driver = Driver::open(origin, self.options).map_err(other)?;
        let description = Self::describe(&driver).map_err(other)?;
        self.state()
            .described
            .insert(id.clone(), description.clone());

        let settings = |endpoint: &Option<BackendEndpoint>| endpoint.as_ref().map(|e| e.settings);
        let (input, output) = (settings(&request.input), settings(&request.output));
        let rate = match (
            input.and_then(|s| s.sample_rate),
            output.and_then(|s| s.sample_rate),
        ) {
            (Some(input), Some(output)) if input != output => {
                return Err(DeviceError::SampleRateMismatch { input, output });
            }
            (input, output) => output.or(input),
        };
        let channels = |side: Option<StreamSettings>, direction: Direction| match side {
            None => Ok(0),
            Some(settings) => {
                let available = description.channels.of(direction);
                match settings.channels.map(usize::from) {
                    None => Ok(available),
                    Some(wanted) if wanted <= available => Ok(wanted),
                    Some(wanted) => Err(DeviceError::UnsupportedSettings(format!(
                        "{name} has {available} {} channels, not {wanted}",
                        direction.as_str()
                    ))),
                }
            }
        };
        let input_channels = channels(input, Direction::Input)?;
        let output_channels = channels(output, Direction::Output)?;
        if input_channels + output_channels == 0 {
            return Err(DeviceError::UnsupportedSettings(format!(
                "{name} has no channels on the side asked for"
            )));
        }
        // A size the driver asked for while it last ran wins over the
        // setup's, which it replaced; anything else is rounded to a size
        // the driver offers.
        let asked = self.state().buffer_sizes.remove(&id);
        let buffer_size = asked
            .or(output.or(input).and_then(|s| s.buffer_size))
            .map(|frames| description.buffer_sizes.nearest(frames));
        let config = StreamConfig {
            inputs: (0..input_channels).collect(),
            outputs: (0..output_channels).collect(),
            buffer_size,
            sample_rate: rate.map(f64::from),
        };
        let sample_rate = rate.unwrap_or(description.sample_rate.round() as u32);
        let mut callback = make(BackendFormat::new(
            sample_rate,
            input_channels,
            output_channels,
        ));

        let asked_size = Arc::new(AtomicU32::new(0));
        let new_rate = Arc::new(AtomicU64::new(0));
        let listener = {
            let asked_size = Arc::clone(&asked_size);
            let new_rate = Arc::clone(&new_rate);
            let status = status.clone();
            move |notifications: &Notifications| {
                if let Some(frames) = notifications.new_buffer_size {
                    asked_size.store(frames, Ordering::Release);
                }
                if let Some(rate) = notifications.new_sample_rate {
                    new_rate.store(rate.to_bits(), Ordering::Release);
                }
                if notifications.needs_restart() {
                    status.request_restart();
                }
            }
        };
        let stream = driver
            .start_with_listener(
                &config,
                move |input, output, block| {
                    let rate = block.sample_rate;
                    let info = CallbackInfo {
                        frame: block.frame,
                        sample_rate: rate,
                        // The driver's clock when it gives one, else the
                        // frames so far.
                        stream_time: block.system_time.unwrap_or_else(|| {
                            if rate > 0.0 {
                                Duration::from_secs_f64(block.frame as f64 / rate)
                            } else {
                                Duration::ZERO
                            }
                        }),
                        output_latency: frames_to_duration(block.output_latency, rate),
                        input_latency: frames_to_duration(block.input_latency, rate),
                    };
                    callback.process(input, output, &info);
                },
                listener,
            )
            .map_err(other)?;

        let running_rate = stream.sample_rate().round() as u32;
        let side = |channels: usize| StreamSettings {
            sample_rate: Some(running_rate),
            channels: Some(channels.min(usize::from(u16::MAX)) as u16),
            buffer_size: Some(stream.buffer_size()),
        };
        let summary = DeviceSummary {
            id: id.clone(),
            name,
        };
        let (input_settings, output_settings) = (side(input_channels), side(output_channels));
        let mut started = BackendStream::new(
            Running {
                stream: Some(stream),
                driver: Some(driver),
                id,
                buffer_size: asked_size,
                sample_rate: new_rate,
                state: Arc::clone(&self.state),
            },
            running_rate,
        );
        if request.input.is_some() {
            started = started.with_input(summary.clone(), input_settings);
        }
        if request.output.is_some() {
            started = started.with_output(summary, output_settings);
        }
        Ok(started)
    }
}
