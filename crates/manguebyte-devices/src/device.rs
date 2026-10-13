use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample, StreamConfig};
use manguebyte_core::AudioBuffer;

use crate::blocks::{BlockCapturer, BlockRenderer};
use crate::settings::{ConfigRange, Resolved, resolve};
use crate::{DeviceError, StreamSettings};

/// Which side of a device we are talking about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Input,
    Output,
}

/// Runs `$build!(T)` with `T` set to the Rust type of a device sample format.
macro_rules! with_sample_type {
    ($format:expr, $build:ident) => {
        match $format {
            cpal::SampleFormat::I8 => $build!(i8),
            cpal::SampleFormat::I16 => $build!(i16),
            cpal::SampleFormat::I24 => $build!(cpal::I24),
            cpal::SampleFormat::I32 => $build!(i32),
            cpal::SampleFormat::I64 => $build!(i64),
            cpal::SampleFormat::U8 => $build!(u8),
            cpal::SampleFormat::U16 => $build!(u16),
            cpal::SampleFormat::U24 => $build!(cpal::U24),
            cpal::SampleFormat::U32 => $build!(u32),
            cpal::SampleFormat::U64 => $build!(u64),
            cpal::SampleFormat::F32 => $build!(f32),
            cpal::SampleFormat::F64 => $build!(f64),
            other => Err($crate::DeviceError::UnsupportedSampleFormat(
                other.to_string(),
            )),
        }
    };
}
pub(crate) use with_sample_type;

/// A description of an audio device, as returned by [`output_devices`] and
/// [`input_devices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable identifier. Store it to reopen the same device in a later run
    /// with [`OutputDevice::open_by_id`] or [`InputDevice::open_by_id`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether this is the system's default device for its direction.
    pub is_default: bool,
    /// Sample rate the device prefers, in hertz.
    pub default_sample_rate: Option<u32>,
    /// Channel count the device prefers.
    pub default_channels: Option<u16>,
    /// Every configuration the device reports. Empty when the device cannot
    /// be queried, for example because another program holds it exclusively.
    pub configs: Vec<ConfigRange>,
}

impl DeviceInfo {
    /// Whether any configuration of the device runs at `sample_rate`.
    pub fn supports_sample_rate(&self, sample_rate: u32) -> bool {
        self.configs
            .iter()
            .any(|config| config.supports_sample_rate(sample_rate))
    }

    /// The largest channel count the device offers.
    pub fn max_channels(&self) -> u16 {
        self.configs
            .iter()
            .map(|config| config.channels)
            .max()
            .or(self.default_channels)
            .unwrap_or(0)
    }
}

/// Lists the audio output devices of the system's default audio host.
pub fn output_devices() -> Result<Vec<DeviceInfo>, DeviceError> {
    list(Direction::Output)
}

/// Lists the audio input devices of the system's default audio host.
pub fn input_devices() -> Result<Vec<DeviceInfo>, DeviceError> {
    list(Direction::Input)
}

fn list(direction: Direction) -> Result<Vec<DeviceInfo>, DeviceError> {
    let host = cpal::default_host();
    let default_id = match direction {
        Direction::Output => host.default_output_device(),
        Direction::Input => host.default_input_device(),
    }
    .and_then(|device| device_id(&device));

    let devices = match direction {
        Direction::Output => host.output_devices()?,
        Direction::Input => host.input_devices()?,
    };
    Ok(devices
        .map(|device| {
            let id = device_id(&device);
            let default = default_config(&device, direction);
            let mut configs: Vec<ConfigRange> = supported_ranges(&device, direction)
                .unwrap_or_default()
                .iter()
                .map(ConfigRange::from_backend)
                .collect();
            configs.sort_by_key(|c| (c.channels, c.min_sample_rate, c.max_sample_rate));
            configs.dedup();
            DeviceInfo {
                is_default: id.is_some() && id == default_id,
                id: id.unwrap_or_default(),
                name: device_name(&device),
                default_sample_rate: default.map(|config| config.sample_rate()),
                default_channels: default.map(|config| config.channels()),
                configs,
            }
        })
        .collect())
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| String::from("unknown device"))
}

fn default_config(
    device: &cpal::Device,
    direction: Direction,
) -> Option<cpal::SupportedStreamConfig> {
    match direction {
        Direction::Output => device.default_output_config().ok(),
        Direction::Input => device.default_input_config().ok(),
    }
}

fn supported_ranges(
    device: &cpal::Device,
    direction: Direction,
) -> Result<Vec<cpal::SupportedStreamConfigRange>, cpal::Error> {
    Ok(match direction {
        Direction::Output => device.supported_output_configs()?.collect(),
        Direction::Input => device.supported_input_configs()?.collect(),
    })
}

/// A device that has been found and given a configuration, in either
/// direction.
pub(crate) struct Opened {
    pub(crate) device: cpal::Device,
    pub(crate) resolved: Resolved,
    pub(crate) direction: Direction,
    name: String,
    id: String,
}

impl Opened {
    fn default_device(direction: Direction) -> Result<Self, DeviceError> {
        let host = cpal::default_host();
        let device = match direction {
            Direction::Output => host
                .default_output_device()
                .ok_or(DeviceError::NoOutputDevice)?,
            Direction::Input => host
                .default_input_device()
                .ok_or(DeviceError::NoInputDevice)?,
        };
        Self::from_device(device, direction)
    }

    fn by_id(id: &str, direction: Direction) -> Result<Self, DeviceError> {
        let not_found = || DeviceError::DeviceNotFound(id.to_owned());
        let parsed: cpal::DeviceId = id.parse().map_err(|_| not_found())?;
        let host = cpal::host_from_id(parsed.host()).map_err(|_| not_found())?;
        let device = host.device_by_id(&parsed).ok_or_else(not_found)?;
        let usable = match direction {
            Direction::Output => device.supports_output(),
            Direction::Input => device.supports_input(),
        };
        if !usable {
            return Err(not_found());
        }
        Self::from_device(device, direction)
    }

    fn from_device(device: cpal::Device, direction: Direction) -> Result<Self, DeviceError> {
        let default = match direction {
            Direction::Output => device.default_output_config()?,
            Direction::Input => device.default_input_config()?,
        };
        Ok(Self {
            resolved: Resolved::from_default(default),
            name: device_name(&device),
            id: device_id(&device).unwrap_or_default(),
            device,
            direction,
        })
    }

    fn apply(&mut self, settings: &StreamSettings) -> Result<(), DeviceError> {
        let ranges = supported_ranges(&self.device, self.direction)?;
        let default = default_config(&self.device, self.direction);
        self.resolved = resolve(&ranges, default, settings)?;
        Ok(())
    }

    pub(crate) fn config(&self) -> StreamConfig {
        self.resolved.config
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.resolved.config.sample_rate
    }

    pub(crate) fn num_channels(&self) -> usize {
        usize::from(self.resolved.config.channels)
    }
}

macro_rules! device_accessors {
    () => {
        /// Human-readable name of the device.
        pub fn name(&self) -> &str {
            &self.opened.name
        }

        /// Stable identifier of the device, for reopening it in a later run.
        pub fn id(&self) -> &str {
            &self.opened.id
        }

        /// Sample rate the device will run at, in hertz.
        pub fn sample_rate(&self) -> f64 {
            f64::from(self.opened.sample_rate())
        }

        /// Number of channels the device will run with.
        pub fn num_channels(&self) -> usize {
            self.opened.num_channels()
        }

        /// Hardware buffer size that was requested, in frames. `None` leaves
        /// the choice to the device.
        pub fn requested_buffer_size(&self) -> Option<u32> {
            match self.opened.resolved.config.buffer_size {
                cpal::BufferSize::Fixed(frames) => Some(frames),
                cpal::BufferSize::Default => None,
            }
        }

        /// Reconfigures the device.
        ///
        /// Fails with [`DeviceError::UnsupportedSettings`] when the device
        /// cannot do what is asked, leaving nothing half-applied.
        pub fn with_settings(mut self, settings: StreamSettings) -> Result<Self, DeviceError> {
            self.opened.apply(&settings)?;
            Ok(self)
        }
    };
}

fn report_to_stderr(error: DeviceError) {
    eprintln!("manguebyte: audio stream error: {error}");
}

/// An audio output device, opened and configured but not yet playing.
///
/// ```no_run
/// use manguebyte_devices::{OutputDevice, StreamSettings};
///
/// let device = OutputDevice::open_default()?
///     .with_settings(StreamSettings::default().with_buffer_size(256))?;
/// println!("{} at {} Hz", device.name(), device.sample_rate());
/// # Ok::<(), manguebyte_devices::DeviceError>(())
/// ```
pub struct OutputDevice {
    pub(crate) opened: Opened,
}

impl OutputDevice {
    /// Opens the system's default output device with its preferred
    /// configuration.
    pub fn open_default() -> Result<Self, DeviceError> {
        Ok(Self {
            opened: Opened::default_device(Direction::Output)?,
        })
    }

    /// Opens the output device with this identifier, as found in
    /// [`DeviceInfo::id`] or returned by [`id`](Self::id).
    pub fn open_by_id(id: &str) -> Result<Self, DeviceError> {
        Ok(Self {
            opened: Opened::by_id(id, Direction::Output)?,
        })
    }

    /// Opens a device found by [`output_devices`].
    pub fn open(info: &DeviceInfo) -> Result<Self, DeviceError> {
        Self::open_by_id(&info.id)
    }

    device_accessors!();

    /// Starts playback, reporting stream errors on standard error.
    ///
    /// See [`start_with_error_handler`](Self::start_with_error_handler).
    pub fn start<F>(&self, max_block_size: usize, callback: F) -> Result<OutputStream, DeviceError>
    where
        F: FnMut(&mut AudioBuffer<f32>) + Send + 'static,
    {
        self.start_with_error_handler(max_block_size, callback, report_to_stderr)
    }

    /// Starts playback.
    ///
    /// `callback` runs on the audio thread. Each call receives a silent buffer
    /// with [`num_channels`](Self::num_channels) channels and at most
    /// `max_block_size` frames, and must fill it without allocating, locking
    /// or blocking. `on_error` is told about problems such as underruns or
    /// the device going away.
    ///
    /// Playback stops when the returned stream is dropped.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_size` is zero.
    pub fn start_with_error_handler<F, E>(
        &self,
        max_block_size: usize,
        callback: F,
        on_error: E,
    ) -> Result<OutputStream, DeviceError>
    where
        F: FnMut(&mut AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        assert!(max_block_size > 0, "max_block_size must be non-zero");
        let renderer = BlockRenderer::new(self.num_channels(), max_block_size, callback);
        macro_rules! build {
            ($sample:ty) => {
                self.build::<$sample, _, _>(renderer, on_error)
            };
        }
        let stream = with_sample_type!(self.opened.resolved.format, build)?;
        stream.play()?;
        Ok(OutputStream { stream })
    }

    fn build<T, F, E>(
        &self,
        mut renderer: BlockRenderer<F>,
        mut on_error: E,
    ) -> Result<cpal::Stream, DeviceError>
    where
        T: SizedSample + FromSample<f32>,
        F: FnMut(&mut AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        Ok(self.opened.device.build_output_stream(
            self.opened.config(),
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| renderer.render(data),
            move |error| on_error(DeviceError::Backend(error)),
            None,
        )?)
    }
}

/// An audio input device, opened and configured but not yet recording.
///
/// ```no_run
/// use manguebyte_devices::InputDevice;
///
/// let device = InputDevice::open_default()?;
/// let _stream = device.start(512, |buffer| {
///     // Runs on the audio thread: no allocation, locks or I/O in here.
///     let _level = buffer.peak();
/// })?;
/// std::thread::sleep(std::time::Duration::from_secs(1));
/// # Ok::<(), manguebyte_devices::DeviceError>(())
/// ```
pub struct InputDevice {
    pub(crate) opened: Opened,
}

impl InputDevice {
    /// Opens the system's default input device with its preferred
    /// configuration.
    pub fn open_default() -> Result<Self, DeviceError> {
        Ok(Self {
            opened: Opened::default_device(Direction::Input)?,
        })
    }

    /// Opens the input device with this identifier, as found in
    /// [`DeviceInfo::id`] or returned by [`id`](Self::id).
    pub fn open_by_id(id: &str) -> Result<Self, DeviceError> {
        Ok(Self {
            opened: Opened::by_id(id, Direction::Input)?,
        })
    }

    /// Opens a device found by [`input_devices`].
    pub fn open(info: &DeviceInfo) -> Result<Self, DeviceError> {
        Self::open_by_id(&info.id)
    }

    device_accessors!();

    /// Starts recording, reporting stream errors on standard error.
    ///
    /// See [`start_with_error_handler`](Self::start_with_error_handler).
    pub fn start<F>(&self, max_block_size: usize, callback: F) -> Result<InputStream, DeviceError>
    where
        F: FnMut(&AudioBuffer<f32>) + Send + 'static,
    {
        self.start_with_error_handler(max_block_size, callback, report_to_stderr)
    }

    /// Starts recording.
    ///
    /// `callback` runs on the audio thread. Each call receives a buffer with
    /// [`num_channels`](Self::num_channels) channels and at most
    /// `max_block_size` frames of captured audio, and must not allocate, lock
    /// or block. To get the audio to another thread, push it into an
    /// [`spsc`](manguebyte_core::spsc) queue.
    ///
    /// Recording stops when the returned stream is dropped.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_size` is zero.
    pub fn start_with_error_handler<F, E>(
        &self,
        max_block_size: usize,
        callback: F,
        on_error: E,
    ) -> Result<InputStream, DeviceError>
    where
        F: FnMut(&AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        assert!(max_block_size > 0, "max_block_size must be non-zero");
        let capturer = BlockCapturer::new(self.num_channels(), max_block_size, callback);
        macro_rules! build {
            ($sample:ty) => {
                self.build::<$sample, _, _>(capturer, on_error)
            };
        }
        let stream = with_sample_type!(self.opened.resolved.format, build)?;
        stream.play()?;
        Ok(InputStream { stream })
    }

    fn build<T, F, E>(
        &self,
        mut capturer: BlockCapturer<F>,
        mut on_error: E,
    ) -> Result<cpal::Stream, DeviceError>
    where
        T: SizedSample,
        f32: FromSample<T>,
        F: FnMut(&AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        Ok(self.opened.device.build_input_stream(
            self.opened.config(),
            move |data: &[T], _: &cpal::InputCallbackInfo| capturer.capture(data),
            move |error| on_error(DeviceError::Backend(error)),
            None,
        )?)
    }
}

macro_rules! stream_controls {
    ($verb:literal) => {
        #[doc = concat!("Pauses ", $verb, ". The callback stops being called.")]
        pub fn pause(&self) -> Result<(), DeviceError> {
            Ok(self.stream.pause()?)
        }

        #[doc = concat!("Resumes ", $verb, " after [`pause`](Self::pause).")]
        pub fn resume(&self) -> Result<(), DeviceError> {
            Ok(self.stream.play()?)
        }

        /// Hardware buffer size the stream is actually running with, in
        /// frames, when the platform can tell.
        pub fn buffer_size(&self) -> Option<u32> {
            self.stream.buffer_size().ok()
        }
    };
}

/// A running output stream. Playback stops when it is dropped.
pub struct OutputStream {
    stream: cpal::Stream,
}

impl OutputStream {
    stream_controls!("playback");
}

/// A running input stream. Recording stops when it is dropped.
pub struct InputStream {
    stream: cpal::Stream,
}

impl InputStream {
    stream_controls!("recording");
}
