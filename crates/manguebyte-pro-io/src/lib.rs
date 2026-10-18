//! ASIO driver support for the Manguebyte Audio Framework, under the GPLv3.
//!
//! # Licence
//!
//! Unlike the rest of the framework, which is under MIT or Apache-2.0,
//! **this crate is under the GNU General Public License, version 3 only**.
//! Its driver interface ([`sys`]) is declared by hand from the Steinberg
//! ASIO SDK 2.3.4 interface definitions, used under the GPLv3, the option
//! Steinberg's dual licence offers besides its own. No file of the SDK is
//! copied into the crate and no bindings are generated from it.
//!
//! A program that uses this crate is therefore under the GPLv3 as a whole.
//! A GPLv3 or AGPLv3 program, such as Aratu, can use it. A closed-source
//! program must not: it takes the other path, which keeps the rest of the
//! framework's licences, and turns on `cpal`'s own `asio` feature with an
//! SDK it downloads and licenses from Steinberg itself, as the
//! documentation of `manguebyte_devices::audio_hosts` explains. No other
//! crate of the framework depends on this one.
//!
//! # What is here
//!
//! - [`drivers`] lists the installed drivers, from the registry
//!   (`HKEY_LOCAL_MACHINE\SOFTWARE\ASIO`).
//! - [`Driver`] loads one through COM, initialises it and asks it what it
//!   can do: channels and their sample types, latencies, buffer sizes,
//!   sample rates, clocks. A process loads one driver at a time.
//! - [`Driver::start`] creates its buffers and runs a callback on planar
//!   `f32` blocks, input and output together in one callback on the
//!   driver's clock, whatever [`SampleType`] the driver stores.
//! - [`ProIoBackend`] puts the drivers behind a `manguebyte-devices`
//!   [`DeviceManager`](manguebyte_devices::DeviceManager), with saved
//!   setups, fallbacks, device events and restarts.
//! - [`FakeDevice`] is a driver written in Rust, which runs everything above
//!   on any system with no hardware, for tests.
//!
//! ```no_run
//! use manguebyte_pro_io::{Driver, StreamConfig, drivers};
//!
//! let Some(entry) = drivers()?.into_iter().next() else {
//!     return Ok(());
//! };
//! let driver = Driver::load(&entry)?;
//! let channels = driver.channels()?;
//! let config = StreamConfig::new()
//!     .with_inputs(0..channels.inputs)
//!     .with_outputs(0..channels.outputs);
//! let stream = driver.start(&config, |input, output, info| {
//!     // Runs on the driver's audio thread: no allocation, locks or I/O.
//!     // The output starts silent; leave it so, or fill it.
//!     let _ = (input.num_channels(), output.num_frames(), info.frame);
//! })?;
//! std::thread::sleep(std::time::Duration::from_secs(1));
//! if stream.needs_restart() {
//!     // The driver asked for a reset: drop the stream and the driver, and
//!     // load them again.
//! }
//! # Ok::<(), manguebyte_pro_io::Error>(())
//! ```
//!
//! # Threads
//!
//! Drivers are single-threaded COM objects. Each loaded driver gets a
//! thread of its own, a single-threaded apartment that hands on its window
//! messages, and every call into the driver runs there, whichever thread
//! made it, so [`Driver`] and [`Stream`] can be used from any thread. The
//! callback runs on the thread the driver calls `bufferSwitch` from, its
//! own high-priority audio thread; the path from there to the callback
//! allocates nothing, takes no lock and makes no system call (the only
//! calls are the SDK's `getSamplePosition`, when the driver gives no time
//! info, and `outputReady`, when it asks for it).
//!
//! The driver's callbacks are plain C functions with no context pointer,
//! so the stream they reach is found through a static; since a process
//! loads one driver, and a driver runs one stream, there is never more than
//! one.
//!
//! # Resets
//!
//! A driver asks the host to close and reopen it when its settings change
//! (`kAsioResetRequest`), and reports a new buffer size, a new sample rate
//! or a loss of sync. None of that is acted on inside the callback that
//! reports it, which has to return into the driver: the report is counted
//! in [`Notifications`], a listener hears of it on the driver's thread,
//! and the owner of the stream drops and reopens it from a thread of its
//! own. [`ProIoBackend`] does that through the device manager, which sees a
//! restart request as a stream lost and restarts it.
//!
//! # Platforms
//!
//! Drivers exist on Windows only, where [`drivers`] reads the registry and
//! [`Driver::load`] creates them through COM. Elsewhere the list is empty,
//! and [`FakeDevice`] still runs everything else, which is how CI tests it.
//!
//! ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.

mod backend;
mod callbacks;
#[cfg(windows)]
mod com;
mod driver;
mod error;
mod fake;
mod raw;
mod registry;
mod sample;
mod stream;
pub mod sys;
mod thread;

pub use backend::{HOST_NAME, ID_PREFIX, ProIoBackend};
pub use driver::{
    BufferSizes, COMMON_SAMPLE_RATES, ChannelInfo, Channels, ClockSource, Driver, DriverInfo,
    Latencies, LoadOptions, loaded_driver,
};
pub use error::Error;
pub use fake::{FakeClock, FakeDevice};
pub use registry::{Clsid, DriverEntry, ParseClsidError, drivers, find_driver};
pub use sample::{SampleType, UnsupportedSampleType};
pub use stream::{BlockInfo, Notifications, Stream, StreamConfig};
