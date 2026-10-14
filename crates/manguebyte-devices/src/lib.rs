//! Audio device input and output.
//!
//! Finds the system's audio devices, opens them with the sample rate, channel
//! count and buffer size you ask for, and drives callbacks that work on planar
//! [`AudioBuffer`](manguebyte_core::AudioBuffer)s whatever sample format and
//! layout the hardware actually uses. The platform work is done by [`cpal`].
//!
//! - [`output_devices`] and [`input_devices`] list what is connected.
//! - [`OutputDevice`] plays audio.
//! - [`InputDevice`] records it.
//! - [`DuplexStream`] does both at once, for effects and monitoring.
//! - [`midi`] opens MIDI ports and keeps a player's timing on the way to
//!   the audio thread (feature `midi`, on by default).
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
//!
//! # Callbacks run on the audio thread
//!
//! Every callback handed to this crate is called by the operating system's
//! audio thread and has a few milliseconds to finish, every time. It must not
//! allocate, lock, block or do I/O. Share values with it through atomics and
//! move data in and out through an [`spsc`](manguebyte_core::spsc) queue.

#![forbid(unsafe_code)]

mod blocks;
mod device;
mod duplex;
mod error;
#[cfg(feature = "midi")]
pub mod midi;
mod settings;

pub use device::{
    DeviceInfo, InputDevice, InputStream, OutputDevice, OutputStream, input_devices, output_devices,
};
pub use duplex::{DuplexSettings, DuplexStream};
pub use error::DeviceError;
pub use settings::{ConfigRange, StreamSettings};
