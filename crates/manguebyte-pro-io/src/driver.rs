//! Loading a driver and asking it what it can do.

use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use manguebyte_core::AudioBuffer;
use manguebyte_devices::Direction;

use crate::fake::FakeDevice;
use crate::raw::RawDriver;
use crate::registry::DriverEntry;
use crate::stream::{BlockInfo, Notifications, Stream, StreamConfig};
use crate::thread::DriverThread;
use crate::{Error, SampleType, sys};

/// The sample rates [`Driver::sample_rates`] asks a driver about.
pub const COMMON_SAMPLE_RATES: [f64; 13] = [
    8_000.0, 11_025.0, 16_000.0, 22_050.0, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0,
    176_400.0, 192_000.0, 352_800.0, 384_000.0,
];

/// The one driver a process may have loaded.
static LOADED: Mutex<Option<String>> = Mutex::new(None);

/// Holds [`LOADED`] for one driver.
struct Claim;

impl Claim {
    fn take(name: &str) -> Result<Self, Error> {
        let mut loaded = LOADED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(loaded) = loaded.as_ref() {
            return Err(Error::AlreadyLoaded {
                loaded: loaded.clone(),
            });
        }
        *loaded = Some(name.to_owned());
        Ok(Self)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        *LOADED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// The name of the driver loaded in this process, if any.
pub fn loaded_driver() -> Option<String> {
    LOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// How to load a driver.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// The application's main window, which Windows drivers receive in
    /// `init` and may parent their dialogs to. `None` passes no window,
    /// which the drivers tried here accept; a driver that wants one fails
    /// to initialise without it.
    pub window: Option<isize>,
}

/// Where a driver comes from: the system, or the fake one.
#[derive(Clone)]
pub(crate) enum Origin {
    Registry(DriverEntry),
    Fake(FakeDevice),
}

impl Origin {
    fn name(&self) -> String {
        match self {
            Self::Registry(entry) => entry.name.clone(),
            Self::Fake(device) => device.name(),
        }
    }
}

pub(crate) struct Inner {
    pub(crate) thread: DriverThread,
    pub(crate) streaming: AtomicBool,
    key: String,
    name: String,
    version: i32,
    // Last, so that the driver is released before another may be loaded.
    _claim: Claim,
}

/// A driver, loaded and initialised: what `CoCreateInstance` made and
/// `init` accepted.
///
/// ASIO allows one loaded driver per process; loading another while this
/// one lives fails with [`Error::AlreadyLoaded`]. Dropping it releases the
/// driver, once its stream, if any, has stopped.
///
/// Every call is made on a thread the driver has to itself, a
/// single-threaded COM apartment, as drivers expect; `Driver` itself can be
/// used from any thread.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
///
/// ```no_run
/// use manguebyte_pro_io::{Driver, StreamConfig, drivers};
///
/// for entry in drivers()? {
///     let driver = Driver::load(&entry)?;
///     let info = driver.info()?;
///     println!("{}: {} in, {} out", info.name, info.channels.inputs, info.channels.outputs);
/// }
/// # Ok::<(), manguebyte_pro_io::Error>(())
/// ```
pub struct Driver {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver")
            .field("key", &self.inner.key)
            .field("name", &self.inner.name)
            .field("version", &self.inner.version)
            .finish_non_exhaustive()
    }
}

/// How many channels a driver has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Channels {
    /// Input channels.
    pub inputs: usize,
    /// Output channels.
    pub outputs: usize,
}

impl Channels {
    /// The channels of one side.
    pub fn of(&self, direction: Direction) -> usize {
        match direction {
            Direction::Input => self.inputs,
            Direction::Output => self.outputs,
        }
    }
}

/// A driver's latencies, in frames, as `getLatencies` reports them: from the
/// buffer switch to the moment the input's first frame was captured and the
/// output's first frame is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Latencies {
    /// Input latency, in frames.
    pub input: u32,
    /// Output latency, in frames.
    pub output: u32,
}

/// The buffer sizes a driver offers, as `getBufferSize` reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BufferSizes {
    /// The smallest, in frames.
    pub min: u32,
    /// The largest, in frames.
    pub max: u32,
    /// The one the driver prefers.
    pub preferred: u32,
    /// The step between sizes: `-1` for powers of two from `min` to `max`,
    /// `0` for one size only, and otherwise a step in frames.
    pub granularity: i32,
}

impl BufferSizes {
    /// Every size the driver offers, smallest first, at most 4,096 of
    /// them.
    pub fn sizes(&self) -> Vec<u32> {
        let mut sizes = Vec::new();
        match self.granularity {
            g if g < 0 => {
                let mut size = self.min.max(1);
                while size <= self.max && sizes.len() < 4_096 {
                    sizes.push(size);
                    size = match size.checked_mul(2) {
                        Some(size) => size,
                        None => break,
                    };
                }
            }
            0 => sizes.push(self.min),
            g => {
                let mut size = self.min;
                while size <= self.max && sizes.len() < 4_096 {
                    sizes.push(size);
                    size = match size.checked_add(g as u32) {
                        Some(size) => size,
                        None => break,
                    };
                }
            }
        }
        if self.preferred > 0 && !sizes.contains(&self.preferred) {
            sizes.push(self.preferred);
            sizes.sort_unstable();
        }
        sizes.retain(|&size| size > 0);
        sizes
    }

    /// Whether the driver offers `frames`.
    pub fn contains(&self, frames: u32) -> bool {
        frames == self.preferred || self.sizes().contains(&frames)
    }

    /// The size the driver offers that is closest to `frames` without
    /// being smaller, or the largest it offers. The preferred size when it
    /// reports none.
    pub fn nearest(&self, frames: u32) -> u32 {
        let sizes = self.sizes();
        sizes
            .iter()
            .copied()
            .find(|&size| size >= frames)
            .or(sizes.last().copied())
            .unwrap_or(self.preferred)
    }
}

/// One channel, as `getChannelInfo` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Which side it is on.
    pub direction: Direction,
    /// Its index on that side.
    pub index: usize,
    /// Its name, such as "Analog In 1".
    pub name: String,
    /// The group it belongs to (analog, S/PDIF, ADAT and so on).
    pub group: i32,
    /// How its samples are stored.
    pub sample_type: SampleType,
    /// Whether a stream uses it now.
    pub is_active: bool,
}

/// A clock the driver can follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSource {
    /// What [`Driver::set_clock_source`] takes.
    pub index: i32,
    /// Its name.
    pub name: String,
    /// The first input channel that carries it, or -1.
    pub channel: i32,
    /// The group of that channel, or -1.
    pub group: i32,
    /// Whether the driver follows it now.
    pub is_current: bool,
}

/// Everything [`Driver::info`] asks a driver.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DriverInfo {
    /// The name the driver gives itself.
    pub name: String,
    /// Its version, in a format of its own.
    pub version: i32,
    /// Its channels.
    pub channels: Channels,
    /// Its latencies at its preferred buffer size.
    pub latencies: Latencies,
    /// Its buffer sizes.
    pub buffer_sizes: BufferSizes,
    /// The rate it runs at now; zero when its clock is lost.
    pub sample_rate: f64,
    /// Which of [`COMMON_SAMPLE_RATES`] it accepts.
    pub sample_rates: Vec<f64>,
    /// Its input channels.
    pub inputs: Vec<ChannelInfo>,
    /// Its output channels.
    pub outputs: Vec<ChannelInfo>,
    /// The clocks it can follow.
    pub clock_sources: Vec<ClockSource>,
    /// Whether it says it calls `bufferSwitchTimeInfo`.
    pub can_time_info: bool,
    /// Whether it says it reports overloads.
    pub can_report_overload: bool,
    /// Frames it buffers beyond the double buffer, input and output, when
    /// it says.
    pub internal_buffering: Option<(u32, u32)>,
}

fn check<T>(call: &'static str, (code, value): (sys::AsioError, T)) -> Result<T, Error> {
    Error::check(call, code).map(|()| value)
}

pub(crate) fn query_channels(driver: &RawDriver) -> Result<Channels, Error> {
    let (code, inputs, outputs) = driver.channels();
    Error::check("getChannels", code)?;
    Ok(Channels {
        inputs: inputs.max(0) as usize,
        outputs: outputs.max(0) as usize,
    })
}

pub(crate) fn query_latencies(driver: &RawDriver) -> Result<Latencies, Error> {
    let (code, input, output) = driver.latencies();
    Error::check("getLatencies", code)?;
    Ok(Latencies {
        input: input.max(0) as u32,
        output: output.max(0) as u32,
    })
}

pub(crate) fn query_buffer_sizes(driver: &RawDriver) -> Result<BufferSizes, Error> {
    let [min, max, preferred, granularity] = check("getBufferSize", driver.buffer_size())?;
    Ok(BufferSizes {
        min: min.max(0) as u32,
        max: max.max(0) as u32,
        preferred: preferred.max(0) as u32,
        granularity,
    })
}

pub(crate) fn query_sample_rate(driver: &RawDriver) -> Result<f64, Error> {
    let (code, rate) = driver.sample_rate();
    match code {
        sys::ASE_OK => Ok(rate),
        // The clock is lost, for example external sync with nothing there.
        sys::ASE_NO_CLOCK => Ok(0.0),
        code => Err(Error::Driver {
            call: "getSampleRate",
            code,
        }),
    }
}

pub(crate) fn query_can_sample_rate(driver: &RawDriver, rate: f64) -> Result<bool, Error> {
    match driver.can_sample_rate(rate) {
        sys::ASE_OK => Ok(true),
        sys::ASE_NO_CLOCK => Ok(false),
        code => Err(Error::Driver {
            call: "canSampleRate",
            code,
        }),
    }
}

pub(crate) fn query_channel(
    driver: &RawDriver,
    direction: Direction,
    index: usize,
) -> Result<ChannelInfo, Error> {
    let (code, info) = driver.channel_info(index as i32, direction == Direction::Input);
    Error::check("getChannelInfo", code)?;
    Ok(ChannelInfo {
        direction,
        index,
        name: RawDriver::channel_name(&info),
        group: info.channel_group,
        sample_type: SampleType::from_raw(info.sample_type),
        is_active: info.is_active != sys::ASIO_FALSE,
    })
}

fn query_info(driver: &RawDriver, name: String, version: i32) -> Result<DriverInfo, Error> {
    let channels = query_channels(driver)?;
    let mut sample_rates = Vec::new();
    for rate in COMMON_SAMPLE_RATES {
        if query_can_sample_rate(driver, rate)? {
            sample_rates.push(rate);
        }
    }
    let side = |direction: Direction| -> Result<Vec<ChannelInfo>, Error> {
        (0..channels.of(direction))
            .map(|index| query_channel(driver, direction, index))
            .collect()
    };
    let (code, sources) = driver.clock_sources();
    let clock_sources = if code == sys::ASE_OK {
        sources
    } else {
        Vec::new()
    };
    let mut internal = sys::AsioInternalBufferInfo::default();
    let internal_buffering = (driver.future(
        sys::FUTURE_GET_INTERNAL_BUFFER_SAMPLES,
        (&mut internal as *mut sys::AsioInternalBufferInfo).cast::<c_void>(),
    ) == sys::ASE_SUCCESS)
        .then(|| {
            (
                internal.input_samples.max(0) as u32,
                internal.output_samples.max(0) as u32,
            )
        });
    Ok(DriverInfo {
        name,
        version,
        channels,
        latencies: query_latencies(driver)?,
        buffer_sizes: query_buffer_sizes(driver)?,
        sample_rate: query_sample_rate(driver)?,
        sample_rates,
        inputs: side(Direction::Input)?,
        outputs: side(Direction::Output)?,
        clock_sources,
        can_time_info: driver.future(sys::FUTURE_CAN_TIME_INFO, std::ptr::null_mut())
            == sys::ASE_SUCCESS,
        can_report_overload: driver.future(sys::FUTURE_CAN_REPORT_OVERLOAD, std::ptr::null_mut())
            == sys::ASE_SUCCESS,
        internal_buffering,
    })
}

impl Driver {
    /// Loads and initialises an installed driver.
    pub fn load(entry: &DriverEntry) -> Result<Self, Error> {
        Self::load_with(entry, LoadOptions::default())
    }

    /// Loads and initialises the installed driver with this name (its
    /// registry key or its description, in any case).
    pub fn load_by_name(name: &str) -> Result<Self, Error> {
        Self::load(&crate::find_driver(name)?)
    }

    /// Loads and initialises an installed driver, with options.
    pub fn load_with(entry: &DriverEntry, options: LoadOptions) -> Result<Self, Error> {
        Self::open(Origin::Registry(entry.clone()), options)
    }

    pub(crate) fn open(origin: Origin, options: LoadOptions) -> Result<Self, Error> {
        let key = origin.name();
        let claim = Claim::take(&key)?;
        let thread = DriverThread::spawn()?;
        let window = options.window.unwrap_or(0);
        let label = key.clone();
        let (name, version) = thread.run(move |apartment| -> Result<(String, i32), Error> {
            let object = match origin {
                #[cfg(windows)]
                Origin::Registry(entry) => crate::com::create_instance(&entry.clsid)?,
                #[cfg(not(windows))]
                Origin::Registry(_) => return Err(Error::Unsupported),
                Origin::Fake(device) => device.create_instance(),
            };
            // SAFETY: COM, or the fake, handed over a reference to a live
            // object.
            let driver = unsafe { RawDriver::new(object) };
            if !driver.init(window as *mut c_void) {
                return Err(Error::InitFailed {
                    driver: label,
                    message: driver.error_message(),
                });
            }
            let name = driver.name();
            let version = driver.version();
            apartment.driver = Some(driver);
            Ok((name, version))
        })??;
        Ok(Self {
            inner: Arc::new(Inner {
                thread,
                streaming: AtomicBool::new(false),
                key,
                name,
                version,
                _claim: claim,
            }),
        })
    }

    /// The name the driver was loaded by: its registry key, or the fake
    /// device's name.
    pub fn key(&self) -> &str {
        &self.inner.key
    }

    /// The name the driver gives itself.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The driver's version, in a format of its own.
    pub fn version(&self) -> i32 {
        self.inner.version
    }

    fn call<R, F>(&self, job: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&RawDriver) -> Result<R, Error> + Send + 'static,
    {
        self.inner.thread.with_driver(job)
    }

    /// Everything about the driver at once.
    pub fn info(&self) -> Result<DriverInfo, Error> {
        let (name, version) = (self.inner.name.clone(), self.inner.version);
        self.call(move |driver| query_info(driver, name, version))
    }

    /// How many channels the driver has.
    pub fn channels(&self) -> Result<Channels, Error> {
        self.call(query_channels)
    }

    /// The driver's latencies, in frames: at its preferred buffer size
    /// before a stream runs, and at the stream's while one does.
    pub fn latencies(&self) -> Result<Latencies, Error> {
        self.call(query_latencies)
    }

    /// The buffer sizes the driver offers.
    pub fn buffer_sizes(&self) -> Result<BufferSizes, Error> {
        self.call(query_buffer_sizes)
    }

    /// The rate the driver runs at, zero when its clock is lost.
    pub fn sample_rate(&self) -> Result<f64, Error> {
        self.call(query_sample_rate)
    }

    /// Whether the driver can run at `rate`.
    pub fn can_sample_rate(&self, rate: f64) -> Result<bool, Error> {
        self.call(move |driver| query_can_sample_rate(driver, rate))
    }

    /// Which of [`COMMON_SAMPLE_RATES`] the driver accepts.
    pub fn sample_rates(&self) -> Result<Vec<f64>, Error> {
        self.call(|driver| {
            let mut rates = Vec::new();
            for rate in COMMON_SAMPLE_RATES {
                if query_can_sample_rate(driver, rate)? {
                    rates.push(rate);
                }
            }
            Ok(rates)
        })
    }

    /// Sets the rate of the driver's clock. Drivers often ask for a reset
    /// afterwards, which a stream reports in its [`Notifications`].
    pub fn set_sample_rate(&self, rate: f64) -> Result<(), Error> {
        self.call(move |driver| Error::check("setSampleRate", driver.set_sample_rate(rate)))
    }

    /// Describes one channel.
    pub fn channel_info(&self, direction: Direction, index: usize) -> Result<ChannelInfo, Error> {
        self.call(move |driver| query_channel(driver, direction, index))
    }

    /// The clocks the driver can follow.
    pub fn clock_sources(&self) -> Result<Vec<ClockSource>, Error> {
        self.call(|driver| {
            let (code, sources) = driver.clock_sources();
            Error::check("getClockSources", code)?;
            Ok(sources)
        })
    }

    /// Makes the driver follow another clock.
    pub fn set_clock_source(&self, index: i32) -> Result<(), Error> {
        self.call(move |driver| Error::check("setClockSource", driver.set_clock_source(index)))
    }

    /// Opens the driver's own settings window. Settings changed there
    /// usually reach a running stream as a reset request.
    pub fn show_control_panel(&self) -> Result<(), Error> {
        self.call(|driver| Error::check("controlPanel", driver.control_panel()))
    }

    /// Creates the buffers of `config` and starts the driver, running
    /// `callback` on its audio thread for every block.
    ///
    /// The callback receives the captured audio, one planar channel per
    /// input of the configuration (it may change it: the buffer is the
    /// stream's own), a silent output buffer to fill, one channel per
    /// output, and the block's [`BlockInfo`]. Blocks are always the
    /// stream's buffer size. It must not allocate, lock or block.
    ///
    /// A driver runs one stream at a time; a second start fails with
    /// [`Error::Busy`]. The stream stops when the returned [`Stream`] is
    /// dropped.
    pub fn start<F>(&self, config: &StreamConfig, callback: F) -> Result<Stream, Error>
    where
        F: FnMut(&mut AudioBuffer<f32>, &mut AudioBuffer<f32>, &BlockInfo) + Send + 'static,
    {
        self.start_with_listener(config, callback, |_: &Notifications| {})
    }

    /// The same as [`start`](Self::start), and `listener` is told whenever
    /// the driver reports something through `asioMessage` or
    /// `sampleRateDidChange`: a reset request, a new buffer size or sample
    /// rate, changed latencies, an overload. It runs on the driver's
    /// thread, never the audio thread, within some milliseconds of the
    /// report, with everything reported since the stream started.
    ///
    /// When [`Notifications::needs_restart`] says so, drop the stream and
    /// the driver, and load and start them again: never from inside the
    /// listener or the callback, which run on the driver's threads.
    pub fn start_with_listener<F, L>(
        &self,
        config: &StreamConfig,
        callback: F,
        listener: L,
    ) -> Result<Stream, Error>
    where
        F: FnMut(&mut AudioBuffer<f32>, &mut AudioBuffer<f32>, &BlockInfo) + Send + 'static,
        L: FnMut(&Notifications) + Send + 'static,
    {
        Stream::start(
            &self.inner,
            config.clone(),
            Box::new(callback),
            Box::new(listener),
        )
    }
}
