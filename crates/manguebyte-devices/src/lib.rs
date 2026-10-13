//! Audio device output.
//!
//! Opens the system's default output device and drives a callback that fills
//! planar [`AudioBuffer`]s, whatever sample format and layout the hardware
//! actually wants. The platform work is done by [`cpal`].
//!
//! ```no_run
//! use manguebyte_devices::OutputDevice;
//!
//! let device = OutputDevice::open_default()?;
//! let step = 440.0 * std::f32::consts::TAU / device.sample_rate() as f32;
//! let mut phase = 0.0f32;
//!
//! let _stream = device.start(512, move |buffer| {
//!     for frame in 0..buffer.num_frames() {
//!         let sample = 0.1 * phase.sin();
//!         phase = (phase + step) % std::f32::consts::TAU;
//!         for channel in 0..buffer.num_channels() {
//!             buffer.channel_mut(channel)[frame] = sample;
//!         }
//!     }
//! })?;
//!
//! std::thread::sleep(std::time::Duration::from_secs(1));
//! # Ok::<(), manguebyte_devices::DeviceError>(())
//! ```

use std::fmt;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, SampleFormat, SizedSample, StreamConfig};
use manguebyte_core::AudioBuffer;

/// What went wrong while talking to an audio device.
#[derive(Debug)]
pub enum DeviceError {
    /// The system has no default output device.
    NoOutputDevice,
    /// The device wants a sample format this crate cannot produce.
    UnsupportedSampleFormat(String),
    /// The platform audio backend reported an error.
    Backend(cpal::Error),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputDevice => f.write_str("no default audio output device"),
            Self::UnsupportedSampleFormat(format) => {
                write!(f, "unsupported device sample format: {format}")
            }
            Self::Backend(error) => write!(f, "audio backend error: {error}"),
        }
    }
}

impl std::error::Error for DeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

impl From<cpal::Error> for DeviceError {
    fn from(error: cpal::Error) -> Self {
        Self::Backend(error)
    }
}

/// An audio output device, opened with its preferred configuration.
pub struct OutputDevice {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    name: String,
}

impl OutputDevice {
    /// Opens the system's default output device.
    pub fn open_default() -> Result<Self, DeviceError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(DeviceError::NoOutputDevice)?;
        let supported = device.default_output_config()?;
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| String::from("unknown device"));
        Ok(Self {
            format: supported.sample_format(),
            config: supported.into(),
            device,
            name,
        })
    }

    /// Human-readable name of the device.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sample rate the device will run at, in hertz.
    pub fn sample_rate(&self) -> f64 {
        f64::from(self.config.sample_rate)
    }

    /// Number of output channels.
    pub fn num_channels(&self) -> usize {
        usize::from(self.config.channels)
    }

    /// Starts playback, reporting stream errors on standard error.
    ///
    /// See [`start_with_error_handler`](Self::start_with_error_handler).
    pub fn start<F>(&self, max_block_size: usize, callback: F) -> Result<OutputStream, DeviceError>
    where
        F: FnMut(&mut AudioBuffer<f32>) + Send + 'static,
    {
        self.start_with_error_handler(max_block_size, callback, |error| {
            eprintln!("manguebyte: audio stream error: {error}");
        })
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
        let stream = match self.format {
            SampleFormat::I8 => self.build::<i8, _, _>(renderer, on_error),
            SampleFormat::I16 => self.build::<i16, _, _>(renderer, on_error),
            SampleFormat::I24 => self.build::<I24, _, _>(renderer, on_error),
            SampleFormat::I32 => self.build::<i32, _, _>(renderer, on_error),
            SampleFormat::I64 => self.build::<i64, _, _>(renderer, on_error),
            SampleFormat::U8 => self.build::<u8, _, _>(renderer, on_error),
            SampleFormat::U16 => self.build::<u16, _, _>(renderer, on_error),
            SampleFormat::U32 => self.build::<u32, _, _>(renderer, on_error),
            SampleFormat::U64 => self.build::<u64, _, _>(renderer, on_error),
            SampleFormat::F32 => self.build::<f32, _, _>(renderer, on_error),
            SampleFormat::F64 => self.build::<f64, _, _>(renderer, on_error),
            other => return Err(DeviceError::UnsupportedSampleFormat(other.to_string())),
        }?;
        stream.play()?;
        Ok(OutputStream { stream })
    }

    fn build<T, F, E>(
        &self,
        mut renderer: BlockRenderer<F>,
        mut on_error: E,
    ) -> Result<cpal::Stream, cpal::Error>
    where
        T: SizedSample + FromSample<f32>,
        F: FnMut(&mut AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        self.device.build_output_stream(
            self.config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| renderer.render(data),
            move |error| on_error(DeviceError::Backend(error)),
            None,
        )
    }
}

/// A running output stream. Playback stops when it is dropped.
pub struct OutputStream {
    stream: cpal::Stream,
}

impl OutputStream {
    /// Pauses playback. The callback stops being called.
    pub fn pause(&self) -> Result<(), DeviceError> {
        Ok(self.stream.pause()?)
    }

    /// Resumes playback after [`pause`](Self::pause).
    pub fn resume(&self) -> Result<(), DeviceError> {
        Ok(self.stream.play()?)
    }
}

/// Adapts a planar `f32` callback to the interleaved, device-format slices the
/// backend hands out, splitting oversized requests into blocks.
struct BlockRenderer<F> {
    scratch: AudioBuffer<f32>,
    callback: F,
}

impl<F: FnMut(&mut AudioBuffer<f32>)> BlockRenderer<F> {
    fn new(channels: usize, max_block_size: usize, callback: F) -> Self {
        Self {
            scratch: AudioBuffer::new(channels, max_block_size),
            callback,
        }
    }

    fn render<T: cpal::Sample + FromSample<f32>>(&mut self, data: &mut [T]) {
        let channels = self.scratch.num_channels();
        if channels == 0 {
            return;
        }
        for chunk in data.chunks_mut(self.scratch.capacity() * channels) {
            self.scratch.set_num_frames(chunk.len() / channels);
            self.scratch.clear();
            (self.callback)(&mut self.scratch);
            for (index, channel) in self.scratch.iter_channels().enumerate() {
                for (frame, &sample) in channel.iter().enumerate() {
                    chunk[frame * channels + index] = T::from_sample(sample);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_and_splits_into_blocks() {
        let mut block_sizes = Vec::new();
        let mut renderer = BlockRenderer::new(2, 4, |buffer: &mut AudioBuffer<f32>| {
            block_sizes.push(buffer.num_frames());
            buffer.channel_mut(0).fill(0.25);
            buffer.channel_mut(1).fill(-0.5);
        });

        let mut data = [9.0f32; 20];
        renderer.render(&mut data);
        drop(renderer);

        assert_eq!(block_sizes, vec![4, 4, 2]);
        for frame in data.chunks_exact(2) {
            assert_eq!(frame, &[0.25, -0.5]);
        }
    }

    #[test]
    fn hands_the_callback_a_silent_buffer_every_time() {
        let mut renderer = BlockRenderer::new(1, 8, |buffer: &mut AudioBuffer<f32>| {
            assert_eq!(buffer.peak(), 0.0);
            buffer.channel_mut(0).fill(1.0);
        });
        let mut data = [0.0f32; 8];
        renderer.render(&mut data);
        renderer.render(&mut data);
        assert_eq!(data, [1.0; 8]);
    }

    #[test]
    fn converts_to_the_device_sample_format() {
        let mut renderer = BlockRenderer::new(1, 4, |buffer: &mut AudioBuffer<f32>| {
            buffer
                .channel_mut(0)
                .copy_from_slice(&[0.0, 0.5, -1.0, 1.0]);
        });

        let mut signed = [0i16; 4];
        renderer.render(&mut signed);
        assert_eq!(signed[0], 0);
        assert!((signed[1] - 16_384).abs() <= 1);
        assert_eq!(signed[2], i16::MIN);
        assert_eq!(signed[3], i16::MAX);

        let mut unsigned = [0u16; 4];
        renderer.render(&mut unsigned);
        assert_eq!(unsigned[0], 32_768);
        assert_eq!(unsigned[2], 0);
    }
}
