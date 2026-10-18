//! A driver written in Rust, for tests and for running without hardware.
//!
//! [`FakeDevice`] is an object with the `IASIO` vtable, laid out as COM
//! lays one out, which the crate loads exactly as it loads a real driver,
//! except that it is created in Rust instead of by `CoCreateInstance`.
//! Everything above that point, from `init` to the host callbacks, runs the
//! same code as with hardware. Nothing happens by itself: a test calls
//! [`switch`](FakeDevice::switch) where a sound card's interrupt would
//! call `bufferSwitch`, and [`request_reset`](FakeDevice::request_reset)
//! and its siblings where a driver would call `asioMessage`, so everything
//! is deterministic and runs on any system. [`FakeDevice::run_clock`]
//! switches on a thread of its own instead, for tests that want a driver
//! that runs.

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::driver::{LoadOptions, Origin};
use crate::sys::{
    self, AsioBool, AsioBufferInfo, AsioCallbacks, AsioChannelInfo, AsioClockSource, AsioError,
    AsioSampleRate, AsioSamples, AsioTime, AsioTimeStamp, Guid, IAsio, IAsioVtbl, define_method,
};
use crate::{BufferSizes, Driver, Error, SampleType};

const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;

struct FakeBuffer {
    input: bool,
    channel: usize,
    halves: [Vec<u8>; 2],
}

struct FakeState {
    name: String,
    inputs: Vec<SampleType>,
    outputs: Vec<SampleType>,
    sizes: BufferSizes,
    rates: Vec<f64>,
    rate: f64,
    latencies: Option<(i32, i32)>,
    output_ready: bool,
    asks_for_time_info: bool,
    init_fails: bool,
    buffers: Vec<FakeBuffer>,
    callbacks: Option<AsioCallbacks>,
    running: bool,
    time_info: bool,
    engine_version: Option<i32>,
    buffer_size: usize,
    index: usize,
    last_index: Option<usize>,
    position: u64,
    rate_changed: bool,
    time: Box<AsioTime>,
}

#[derive(Default)]
struct Counters {
    created: AtomicUsize,
    alive: AtomicUsize,
    inits: AtomicUsize,
    starts: AtomicUsize,
    stops: AtomicUsize,
    creations: AtomicUsize,
    disposals: AtomicUsize,
    output_ready: AtomicUsize,
    switches: AtomicUsize,
}

struct FakeShared {
    state: Mutex<FakeState>,
    counters: Counters,
    switching: AtomicBool,
    epoch: Instant,
}

/// A fake driver: its configuration, its state, and what it saw the host
/// do. Clones are the same device.
///
/// ```
/// use manguebyte_pro_io::{FakeDevice, SampleType, StreamConfig};
///
/// let device = FakeDevice::new("Fake").with_channels(2, 2, SampleType::Int24Lsb);
/// let driver = device.load()?;
/// let stream = driver.start(
///     &StreamConfig::new().with_inputs(0..2).with_outputs(0..2),
///     |input, output, _info| {
///         for channel in 0..output.num_channels() {
///             output.channel_mut(channel).copy_from_slice(input.channel(channel));
///         }
///     },
/// )?;
/// // Half scale on input 1, as a 24-bit little-endian driver holds it.
/// let block: Vec<u8> = [0x00, 0x00, 0x40].repeat(stream.buffer_size() as usize);
/// device.write_input(1, &block);
/// assert!(device.switch());
/// assert_eq!(device.read_output(1).unwrap(), block);
/// # Ok::<(), manguebyte_pro_io::Error>(())
/// ```
#[derive(Clone)]
pub struct FakeDevice {
    shared: Arc<FakeShared>,
}

impl std::fmt::Debug for FakeDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDevice")
            .field("name", &self.name())
            .field("running", &self.is_running())
            .finish_non_exhaustive()
    }
}

/// Switches a fake device on a thread of its own until dropped.
pub struct FakeClock {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for FakeClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for FakeClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeClock").finish_non_exhaustive()
    }
}

fn lock(shared: &FakeShared) -> MutexGuard<'_, FakeState> {
    shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_text(target: *mut c_char, room: usize, text: &str) {
    let bytes = text.as_bytes();
    let length = bytes.len().min(room - 1);
    // SAFETY: the host hands at least `room` bytes, as the SDK sizes them.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), target, length);
        *target.add(length) = 0;
    }
}

impl FakeDevice {
    /// A device with two inputs and two outputs of 32-bit floats, buffers
    /// of 64 to 2,048 frames in powers of two (256 preferred), 44.1, 48 and
    /// 96 kHz (48 kHz now), that calls `bufferSwitchTimeInfo` when the host
    /// supports it and does not want `outputReady`.
    pub fn new(name: &str) -> Self {
        Self {
            shared: Arc::new(FakeShared {
                state: Mutex::new(FakeState {
                    name: name.to_owned(),
                    inputs: vec![SampleType::Float32Lsb; 2],
                    outputs: vec![SampleType::Float32Lsb; 2],
                    sizes: BufferSizes {
                        min: 64,
                        max: 2_048,
                        preferred: 256,
                        granularity: -1,
                    },
                    rates: vec![44_100.0, 48_000.0, 96_000.0],
                    rate: 48_000.0,
                    latencies: None,
                    output_ready: false,
                    asks_for_time_info: true,
                    init_fails: false,
                    buffers: Vec::new(),
                    callbacks: None,
                    running: false,
                    time_info: false,
                    engine_version: None,
                    buffer_size: 0,
                    index: 0,
                    last_index: None,
                    position: 0,
                    rate_changed: false,
                    time: Box::new(AsioTime::zeroed()),
                }),
                counters: Counters::default(),
                switching: AtomicBool::new(false),
                epoch: Instant::now(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        lock(&self.shared)
    }

    /// Sets the sample type of every input and output channel.
    #[must_use]
    pub fn with_channels(self, inputs: usize, outputs: usize, sample_type: SampleType) -> Self {
        self.with_sample_types(vec![sample_type; inputs], vec![sample_type; outputs])
    }

    /// Sets the channels, one sample type each.
    #[must_use]
    pub fn with_sample_types(self, inputs: Vec<SampleType>, outputs: Vec<SampleType>) -> Self {
        {
            let mut state = self.state();
            state.inputs = inputs;
            state.outputs = outputs;
        }
        self
    }

    /// Sets the buffer sizes.
    #[must_use]
    pub fn with_buffer_sizes(self, sizes: BufferSizes) -> Self {
        self.state().sizes = sizes;
        self
    }

    /// Sets the sample rates, and the one the device runs at.
    #[must_use]
    pub fn with_sample_rates(self, rates: &[f64], current: f64) -> Self {
        {
            let mut state = self.state();
            state.rates = rates.to_vec();
            state.rate = current;
        }
        self
    }

    /// Sets the latencies the device reports, in frames. By default they
    /// are the buffer size plus 15 frames in, plus 31 out.
    #[must_use]
    pub fn with_latencies(self, input: i32, output: i32) -> Self {
        self.state().latencies = Some((input, output));
        self
    }

    /// Whether the device asks for `outputReady` after every block.
    #[must_use]
    pub fn with_output_ready(self, wanted: bool) -> Self {
        self.state().output_ready = wanted;
        self
    }

    /// Whether the device offers `bufferSwitchTimeInfo` to the host.
    #[must_use]
    pub fn with_time_info(self, offered: bool) -> Self {
        self.state().asks_for_time_info = offered;
        self
    }

    /// Makes `init` fail, as a driver whose hardware is unplugged does.
    #[must_use]
    pub fn with_failing_init(self, fails: bool) -> Self {
        self.state().init_fails = fails;
        self
    }

    /// The device's name, which is also its driver's.
    pub fn name(&self) -> String {
        self.state().name.clone()
    }

    /// Loads the device as a driver, as [`Driver::load`] loads an installed
    /// one: one per process.
    pub fn load(&self) -> Result<Driver, Error> {
        Driver::open(Origin::Fake(self.clone()), LoadOptions::default())
    }

    /// A new COM object for the device, with one reference: what
    /// `CoCreateInstance` would hand over.
    pub(crate) fn create_instance(&self) -> *mut IAsio {
        self.shared.counters.created.fetch_add(1, Ordering::Relaxed);
        self.shared.counters.alive.fetch_add(1, Ordering::Relaxed);
        let instance = Box::new(Instance {
            iasio: IAsio { vtbl: &VTBL },
            references: AtomicU32::new(1),
            shared: Arc::clone(&self.shared),
        });
        Box::into_raw(instance).cast::<IAsio>()
    }

    /// Writes raw samples, in the channel's sample type, into the half of
    /// input `channel` that the next [`switch`](Self::switch) hands over.
    /// Returns whether the channel has a buffer.
    pub fn write_input(&self, channel: usize, bytes: &[u8]) -> bool {
        let mut state = self.state();
        let index = state.index;
        let Some(buffer) = state
            .buffers
            .iter_mut()
            .find(|buffer| buffer.input && buffer.channel == channel)
        else {
            return false;
        };
        let half = &mut buffer.halves[index];
        let length = bytes.len().min(half.len());
        half[..length].copy_from_slice(&bytes[..length]);
        true
    }

    /// The raw samples the host wrote into output `channel` during the last
    /// [`switch`](Self::switch).
    pub fn read_output(&self, channel: usize) -> Option<Vec<u8>> {
        let state = self.state();
        let index = state.last_index?;
        state
            .buffers
            .iter()
            .find(|buffer| !buffer.input && buffer.channel == channel)
            .map(|buffer| buffer.halves[index].clone())
    }

    /// Calls the host's `bufferSwitchTimeInfo`, or `bufferSwitch` when the
    /// host did not take time info, with the next half of the buffers, as
    /// the hardware would. Returns `false`, calling nothing, when the
    /// device is not running.
    ///
    /// Allocates nothing, so a test can count allocations around it. Call
    /// it from one thread at a time.
    pub fn switch(&self) -> bool {
        if self.shared.switching.swap(true, Ordering::Acquire) {
            return false;
        }
        let call = {
            let mut state = self.state();
            match (state.running, state.callbacks) {
                (true, Some(callbacks)) => {
                    let index = state.index;
                    let nanos = self.shared.epoch.elapsed().as_nanos() as u64;
                    let mut flags = sys::TIME_SYSTEM_TIME_VALID
                        | sys::TIME_SAMPLE_POSITION_VALID
                        | sys::TIME_SAMPLE_RATE_VALID
                        | sys::TIME_SPEED_VALID;
                    if std::mem::take(&mut state.rate_changed) {
                        flags |= sys::TIME_SAMPLE_RATE_CHANGED;
                    }
                    let position = state.position;
                    let rate = state.rate;
                    let time = &mut *state.time;
                    time.time_info.speed = 1.0;
                    time.time_info.system_time = AsioTimeStamp::new(nanos);
                    time.time_info.sample_position = AsioSamples::new(position);
                    time.time_info.sample_rate = rate;
                    time.time_info.flags = flags;
                    let time: *mut AsioTime = time;
                    Some((callbacks, index, state.time_info, time))
                }
                _ => None,
            }
        };
        let Some((callbacks, index, time_info, time)) = call else {
            self.shared.switching.store(false, Ordering::Release);
            return false;
        };
        // Outside the lock: the host may call back into the device.
        if time_info {
            // SAFETY: the host's callback; `time` stays valid and untouched
            // by the device until the call returns, as only `switch` writes
            // it and `switching` keeps a second one out.
            unsafe { (callbacks.buffer_switch_time_info)(time, index as i32, sys::ASIO_TRUE) };
        } else {
            // SAFETY: the host's callback, which takes plain values.
            unsafe { (callbacks.buffer_switch)(index as i32, sys::ASIO_TRUE) };
        }
        {
            let mut state = self.state();
            state.position += state.buffer_size as u64;
            state.last_index = Some(index);
            state.index = index ^ 1;
        }
        self.shared
            .counters
            .switches
            .fetch_add(1, Ordering::Relaxed);
        self.shared.switching.store(false, Ordering::Release);
        true
    }

    /// Switches every `interval` on a thread of its own until the returned
    /// clock is dropped, as a driver's thread would.
    pub fn run_clock(&self, interval: Duration) -> FakeClock {
        let stop = Arc::new(AtomicBool::new(false));
        let device = self.clone();
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(String::from("manguebyte-pro-io fake clock"))
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    device.switch();
                    std::thread::sleep(interval);
                }
            })
            .ok();
        FakeClock { stop, thread }
    }

    /// Calls the host's `asioMessage` with `selector` and `value`, as a
    /// driver does. `None` when the host gave the device no callbacks.
    pub fn ask_host(&self, selector: i32, value: i32) -> Option<i32> {
        let callbacks = self.state().callbacks?;
        // SAFETY: the host's callback; the message and option are unused.
        Some(unsafe { (callbacks.asio_message)(selector, value, ptr::null_mut(), ptr::null_mut()) })
    }

    /// Asks the host whether it supports `selector`, and if it does sends
    /// it, as the SDK has a driver do. Returns the host's answer.
    fn notify(&self, selector: i32, value: i32) -> Option<i32> {
        match self.ask_host(sys::SELECTOR_SUPPORTED, selector)? {
            0 => Some(0),
            _ => self.ask_host(selector, value),
        }
    }

    /// Asks the host for a reset (`kAsioResetRequest`).
    pub fn request_reset(&self) -> Option<i32> {
        self.notify(sys::RESET_REQUEST, 0)
    }

    /// Prefers another buffer size, and tells the host
    /// (`kAsioBufferSizeChange`).
    pub fn change_buffer_size(&self, frames: u32) -> Option<i32> {
        self.state().sizes.preferred = frames;
        self.notify(sys::BUFFER_SIZE_CHANGE, frames as i32)
    }

    /// Asks the host to start again after losing sync
    /// (`kAsioResyncRequest`).
    pub fn request_resync(&self) -> Option<i32> {
        self.notify(sys::RESYNC_REQUEST, 0)
    }

    /// Reports other latencies, and tells the host
    /// (`kAsioLatenciesChanged`).
    pub fn change_latencies(&self, input: i32, output: i32) -> Option<i32> {
        self.state().latencies = Some((input, output));
        self.notify(sys::LATENCIES_CHANGED, 0)
    }

    /// Reports a dropout (`kAsioOverload`).
    pub fn report_overload(&self) -> Option<i32> {
        self.ask_host(sys::OVERLOAD, 0)
    }

    /// Runs at another rate, as an external clock would make it, and tells
    /// the host through `sampleRateDidChange`; the next time info says so
    /// too. Returns whether the host had given callbacks.
    pub fn change_sample_rate(&self, rate: f64) -> bool {
        let callbacks = {
            let mut state = self.state();
            state.rate = rate;
            state.rate_changed = true;
            state.callbacks
        };
        let Some(callbacks) = callbacks else {
            return false;
        };
        // SAFETY: the host's callback, which takes a plain value.
        unsafe { (callbacks.sample_rate_did_change)(rate) };
        true
    }

    /// Whether the host started the device and has not stopped it.
    pub fn is_running(&self) -> bool {
        self.state().running
    }

    /// The buffer size of the buffers the host created, if any.
    pub fn buffer_size(&self) -> Option<usize> {
        let state = self.state();
        (!state.buffers.is_empty()).then_some(state.buffer_size)
    }

    /// The rate the device runs at.
    pub fn sample_rate(&self) -> f64 {
        self.state().rate
    }

    /// Whether the host took `bufferSwitchTimeInfo` when the device
    /// offered it.
    pub fn host_takes_time_info(&self) -> bool {
        self.state().time_info
    }

    /// The engine version the host gave when the device asked, during
    /// `createBuffers`.
    pub fn host_engine_version(&self) -> Option<i32> {
        self.state().engine_version
    }

    /// How many objects were created for the device: one per load.
    pub fn instances_created(&self) -> usize {
        self.shared.counters.created.load(Ordering::Relaxed)
    }

    /// How many of them have not been released.
    pub fn instances_alive(&self) -> usize {
        self.shared.counters.alive.load(Ordering::Relaxed)
    }

    /// How often `init` was called.
    pub fn inits(&self) -> usize {
        self.shared.counters.inits.load(Ordering::Relaxed)
    }

    /// How often `start` was called.
    pub fn starts(&self) -> usize {
        self.shared.counters.starts.load(Ordering::Relaxed)
    }

    /// How often `stop` was called.
    pub fn stops(&self) -> usize {
        self.shared.counters.stops.load(Ordering::Relaxed)
    }

    /// How often `createBuffers` succeeded.
    pub fn buffer_creations(&self) -> usize {
        self.shared.counters.creations.load(Ordering::Relaxed)
    }

    /// How often `disposeBuffers` succeeded.
    pub fn buffer_disposals(&self) -> usize {
        self.shared.counters.disposals.load(Ordering::Relaxed)
    }

    /// How often the host called `outputReady` while the device ran.
    pub fn output_ready_calls(&self) -> usize {
        self.shared.counters.output_ready.load(Ordering::Relaxed)
    }

    /// How many blocks [`switch`](Self::switch) handed to the host.
    pub fn switches(&self) -> usize {
        self.shared.counters.switches.load(Ordering::Relaxed)
    }
}

/// The COM object: a vtable pointer first, as every `IASIO` object starts.
#[repr(C)]
struct Instance {
    iasio: IAsio,
    references: AtomicU32,
    shared: Arc<FakeShared>,
}

/// The device behind `this`.
///
/// # Safety
///
/// `this` must be an object [`FakeDevice::create_instance`] made, still
/// referenced.
unsafe fn device<'a>(this: *mut IAsio) -> &'a FakeShared {
    // SAFETY: the object is an `Instance`, whose first field is the
    // interface, and the caller keeps it alive.
    unsafe { &(*this.cast::<Instance>()).shared }
}

unsafe extern "system" fn query_interface(
    _this: *mut IAsio,
    _iid: *const Guid,
    object: *mut *mut c_void,
) -> i32 {
    if !object.is_null() {
        // SAFETY: COM callers hand a pointer to fill in.
        unsafe { *object = ptr::null_mut() };
    }
    E_NOINTERFACE
}

unsafe extern "system" fn add_ref(this: *mut IAsio) -> u32 {
    // SAFETY: a live object.
    let instance = unsafe { &*this.cast::<Instance>() };
    instance.references.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn release(this: *mut IAsio) -> u32 {
    // SAFETY: a live object.
    let left = unsafe { &*this.cast::<Instance>() }
        .references
        .fetch_sub(1, Ordering::AcqRel)
        - 1;
    if left == 0 {
        // SAFETY: the last reference is gone: the object came from
        // `Box::into_raw` and is freed once.
        let instance = unsafe { Box::from_raw(this.cast::<Instance>()) };
        {
            // As a driver does when it is released: nothing runs any more.
            let mut state = lock(&instance.shared);
            state.running = false;
            state.callbacks = None;
            state.buffers.clear();
        }
        instance
            .shared
            .counters
            .alive
            .fetch_sub(1, Ordering::Relaxed);
    }
    left
}

define_method! {
    fn init(this: *mut IAsio, _window: *mut c_void) -> AsioBool {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        shared.counters.inits.fetch_add(1, Ordering::Relaxed);
        if lock(shared).init_fails { sys::ASIO_FALSE } else { sys::ASIO_TRUE }
    }
}

define_method! {
    fn get_driver_name(this: *mut IAsio, name: *mut c_char) {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        write_text(name, 32, &lock(shared).name);
    }
}

define_method! {
    fn get_driver_version(_this: *mut IAsio) -> i32 {
        1
    }
}

define_method! {
    fn get_error_message(_this: *mut IAsio, text: *mut c_char) {
        write_text(text, 124, "the fake device was told to fail");
    }
}

define_method! {
    fn start(this: *mut IAsio) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        let mut state = lock(shared);
        if state.callbacks.is_none() {
            return sys::ASE_INVALID_MODE;
        }
        state.running = true;
        state.position = 0;
        shared.counters.starts.fetch_add(1, Ordering::Relaxed);
        sys::ASE_OK
    }
}

define_method! {
    fn stop(this: *mut IAsio) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        lock(shared).running = false;
        shared.counters.stops.fetch_add(1, Ordering::Relaxed);
        sys::ASE_OK
    }
}

define_method! {
    fn get_channels(this: *mut IAsio, inputs: *mut i32, outputs: *mut i32) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        // SAFETY: the host hands two integers to fill in.
        unsafe {
            *inputs = state.inputs.len() as i32;
            *outputs = state.outputs.len() as i32;
        }
        sys::ASE_OK
    }
}

define_method! {
    fn get_latencies(this: *mut IAsio, input: *mut i32, output: *mut i32) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        let size = if state.buffers.is_empty() { state.sizes.preferred as i32 } else { state.buffer_size as i32 };
        let (i, o) = state.latencies.unwrap_or((size + 15, size + 31));
        // SAFETY: the host hands two integers to fill in.
        unsafe {
            *input = i;
            *output = o;
        }
        sys::ASE_OK
    }
}

define_method! {
    fn get_buffer_size(
        this: *mut IAsio,
        min: *mut i32,
        max: *mut i32,
        preferred: *mut i32,
        granularity: *mut i32,
    ) -> AsioError {
        // SAFETY: the host calls a live object.
        let sizes = lock(unsafe { device(this) }).sizes;
        // SAFETY: the host hands four integers to fill in.
        unsafe {
            *min = sizes.min as i32;
            *max = sizes.max as i32;
            *preferred = sizes.preferred as i32;
            *granularity = sizes.granularity;
        }
        sys::ASE_OK
    }
}

define_method! {
    fn can_sample_rate(this: *mut IAsio, rate: AsioSampleRate) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        if state.rates.contains(&rate) { sys::ASE_OK } else { sys::ASE_NO_CLOCK }
    }
}

define_method! {
    fn get_sample_rate(this: *mut IAsio, rate: *mut AsioSampleRate) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        // SAFETY: the host hands a double to fill in.
        unsafe { *rate = state.rate };
        sys::ASE_OK
    }
}

define_method! {
    fn set_sample_rate(this: *mut IAsio, rate: AsioSampleRate) -> AsioError {
        // SAFETY: the host calls a live object.
        let mut state = lock(unsafe { device(this) });
        if !state.rates.contains(&rate) {
            return sys::ASE_NO_CLOCK;
        }
        state.rate = rate;
        sys::ASE_OK
    }
}

define_method! {
    fn get_clock_sources(this: *mut IAsio, clocks: *mut AsioClockSource, count: *mut i32) -> AsioError {
        let _ = this;
        // SAFETY: the host hands room for `*count` sources.
        unsafe {
            if *count >= 1 {
                let mut name = [0 as c_char; 32];
                write_text(name.as_mut_ptr(), 32, "Internal");
                *clocks = AsioClockSource {
                    index: 0,
                    associated_channel: -1,
                    associated_group: -1,
                    is_current_source: sys::ASIO_TRUE,
                    name,
                };
            }
            *count = 1;
        }
        sys::ASE_OK
    }
}

define_method! {
    fn set_clock_source(_this: *mut IAsio, index: i32) -> AsioError {
        if index == 0 { sys::ASE_OK } else { sys::ASE_INVALID_PARAMETER }
    }
}

define_method! {
    fn get_sample_position(this: *mut IAsio, position: *mut AsioSamples, stamp: *mut AsioTimeStamp) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        let state = lock(shared);
        if !state.running {
            return sys::ASE_SP_NOT_ADVANCING;
        }
        // SAFETY: the host hands two structures to fill in.
        unsafe {
            *position = AsioSamples::new(state.position);
            *stamp = AsioTimeStamp::new(shared.epoch.elapsed().as_nanos() as u64);
        }
        sys::ASE_OK
    }
}

define_method! {
    fn get_channel_info(this: *mut IAsio, info: *mut AsioChannelInfo) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        // SAFETY: the host hands a structure with the channel filled in.
        let info = unsafe { &mut *info };
        let input = info.is_input != sys::ASIO_FALSE;
        let types = if input { &state.inputs } else { &state.outputs };
        let Some(sample_type) = usize::try_from(info.channel).ok().and_then(|index| types.get(index)) else {
            return sys::ASE_INVALID_PARAMETER;
        };
        info.sample_type = sample_type.to_raw();
        info.channel_group = 0;
        info.is_active = i32::from(state.buffers.iter().any(|buffer| {
            buffer.input == input && buffer.channel as i32 == info.channel
        }));
        let name = format!("Fake {} {}", if input { "In" } else { "Out" }, info.channel + 1);
        write_text(info.name.as_mut_ptr(), 32, &name);
        sys::ASE_OK
    }
}

define_method! {
    fn create_buffers(
        this: *mut IAsio,
        infos: *mut AsioBufferInfo,
        count: i32,
        frames: i32,
        callbacks: *mut AsioCallbacks,
    ) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        let table = {
            let mut state = lock(shared);
            if !state.buffers.is_empty() || callbacks.is_null() || count < 0 || frames <= 0 {
                return sys::ASE_INVALID_MODE;
            }
            if !state.sizes.contains(frames as u32) {
                return sys::ASE_INVALID_MODE;
            }
            // SAFETY: the host hands `count` structures.
            let infos = unsafe { std::slice::from_raw_parts_mut(infos, count as usize) };
            let mut buffers = Vec::with_capacity(infos.len());
            for info in infos.iter() {
                let input = info.is_input != sys::ASIO_FALSE;
                let types = if input { &state.inputs } else { &state.outputs };
                let Some(bytes) = usize::try_from(info.channel_num)
                    .ok()
                    .and_then(|index| types.get(index))
                    .and_then(|ty| ty.buffer_bytes(frames as usize))
                else {
                    return sys::ASE_INVALID_MODE;
                };
                buffers.push(FakeBuffer {
                    input,
                    channel: info.channel_num as usize,
                    halves: [vec![0; bytes], vec![0; bytes]],
                });
            }
            for (info, buffer) in infos.iter_mut().zip(&mut buffers) {
                info.buffers = [
                    buffer.halves[0].as_mut_ptr().cast(),
                    buffer.halves[1].as_mut_ptr().cast(),
                ];
            }
            state.buffers = buffers;
            state.buffer_size = frames as usize;
            state.index = 0;
            state.last_index = None;
            state.position = 0;
            // SAFETY: the host's table, valid until the buffers are
            // disposed.
            let table = unsafe { *callbacks };
            state.callbacks = Some(table);
            table
        };
        shared.counters.creations.fetch_add(1, Ordering::Relaxed);
        // What the SDK's example driver asks the host here, outside the
        // lock: the host may call back in.
        let ask = |selector: i32, value: i32| {
            // SAFETY: the host's callback.
            unsafe { (table.asio_message)(selector, value, ptr::null_mut(), ptr::null_mut()) }
        };
        let version = (ask(sys::SELECTOR_SUPPORTED, sys::ENGINE_VERSION) == 1)
            .then(|| ask(sys::ENGINE_VERSION, 0));
        let offers = lock(shared).asks_for_time_info;
        let time_info = offers && ask(sys::SUPPORTS_TIME_INFO, 0) == 1;
        let mut state = lock(shared);
        state.engine_version = version;
        state.time_info = time_info;
        sys::ASE_OK
    }
}

define_method! {
    fn dispose_buffers(this: *mut IAsio) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        let mut state = lock(shared);
        if state.buffers.is_empty() {
            return sys::ASE_INVALID_MODE;
        }
        state.running = false;
        state.buffers.clear();
        state.callbacks = None;
        shared.counters.disposals.fetch_add(1, Ordering::Relaxed);
        sys::ASE_OK
    }
}

define_method! {
    fn control_panel(_this: *mut IAsio) -> AsioError {
        sys::ASE_NOT_PRESENT
    }
}

define_method! {
    fn future(this: *mut IAsio, selector: i32, parameter: *mut c_void) -> AsioError {
        // SAFETY: the host calls a live object.
        let state = lock(unsafe { device(this) });
        match selector {
            sys::FUTURE_CAN_TIME_INFO if state.asks_for_time_info => sys::ASE_SUCCESS,
            sys::FUTURE_CAN_REPORT_OVERLOAD => sys::ASE_SUCCESS,
            sys::FUTURE_GET_INTERNAL_BUFFER_SAMPLES if !parameter.is_null() => {
                // SAFETY: the selector's parameter is this structure.
                unsafe {
                    *parameter.cast::<sys::AsioInternalBufferInfo>() = sys::AsioInternalBufferInfo {
                        input_samples: 16,
                        output_samples: 32,
                    };
                }
                sys::ASE_SUCCESS
            }
            sys::FUTURE_CAN_TIME_INFO | sys::FUTURE_CAN_TIME_CODE => sys::ASE_NOT_PRESENT,
            _ => sys::ASE_INVALID_PARAMETER,
        }
    }
}

define_method! {
    fn output_ready(this: *mut IAsio) -> AsioError {
        // SAFETY: the host calls a live object.
        let shared = unsafe { device(this) };
        let state = lock(shared);
        if !state.output_ready {
            return sys::ASE_NOT_PRESENT;
        }
        if state.running {
            shared.counters.output_ready.fetch_add(1, Ordering::Relaxed);
        }
        sys::ASE_OK
    }
}

static VTBL: IAsioVtbl = IAsioVtbl {
    query_interface,
    add_ref,
    release,
    init,
    get_driver_name,
    get_driver_version,
    get_error_message,
    start,
    stop,
    get_channels,
    get_latencies,
    get_buffer_size,
    can_sample_rate,
    get_sample_rate,
    set_sample_rate,
    get_clock_sources,
    set_clock_source,
    get_sample_position,
    get_channel_info,
    create_buffers,
    dispose_buffers,
    control_panel,
    future,
    output_ready,
};
