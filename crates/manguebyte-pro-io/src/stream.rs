//! A running stream: the driver's buffers, and what turns them into planar
//! `f32` blocks for a callback.

use std::cell::UnsafeCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use manguebyte_core::{AudioBuffer, NoDenormals};
use manguebyte_devices::Direction;

use crate::callbacks::{self, ENGINE, IN_FLIGHT, NOTICES};
use crate::driver::{
    ChannelInfo, Inner, Latencies, query_buffer_sizes, query_can_sample_rate, query_channel,
    query_channels, query_latencies, query_sample_rate,
};
use crate::raw::{self, RawDriver};
use crate::sys::{self, AsioBufferInfo, IAsio};
use crate::{Error, SampleType};

/// What runs on every block.
pub(crate) type Callback =
    Box<dyn FnMut(&mut AudioBuffer<f32>, &mut AudioBuffer<f32>, &BlockInfo) + Send>;

/// What hears about the driver's notifications.
pub(crate) type Listener = Box<dyn FnMut(&Notifications) + Send>;

/// Which channels to open, and how.
///
/// ```
/// use manguebyte_pro_io::StreamConfig;
///
/// let config = StreamConfig::new()
///     .with_inputs(0..2)
///     .with_outputs([0, 1, 2, 3])
///     .with_buffer_size(256)
///     .with_sample_rate(48_000.0);
/// assert_eq!(config.inputs, [0, 1]);
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamConfig {
    /// The input channels to record, by index; the callback's input buffer
    /// has one channel per entry, in this order.
    pub inputs: Vec<usize>,
    /// The output channels to play, by index, in the order of the
    /// callback's output buffer.
    pub outputs: Vec<usize>,
    /// The buffer size, in frames; `None` is the driver's preferred size.
    /// It must be one the driver offers ([`BufferSizes::contains`]).
    ///
    /// [`BufferSizes::contains`]: crate::BufferSizes::contains
    pub buffer_size: Option<u32>,
    /// The sample rate, in hertz; `None` keeps the driver's.
    pub sample_rate: Option<f64>,
}

impl StreamConfig {
    /// No channels yet, the driver's buffer size and rate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records these input channels.
    #[must_use]
    pub fn with_inputs(mut self, channels: impl IntoIterator<Item = usize>) -> Self {
        self.inputs = channels.into_iter().collect();
        self
    }

    /// Plays these output channels.
    #[must_use]
    pub fn with_outputs(mut self, channels: impl IntoIterator<Item = usize>) -> Self {
        self.outputs = channels.into_iter().collect();
        self
    }

    /// Asks for a buffer size, in frames.
    #[must_use]
    pub fn with_buffer_size(mut self, frames: u32) -> Self {
        self.buffer_size = Some(frames);
        self
    }

    /// Asks for a sample rate, in hertz.
    #[must_use]
    pub fn with_sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = Some(rate);
        self
    }
}

/// What a callback is told about its block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub struct BlockInfo {
    /// Frames handed to the callback before this block, since the stream
    /// started.
    pub frame: u64,
    /// The stream's sample rate, in hertz.
    pub sample_rate: f64,
    /// The driver's sample position of the block, when it gives one. It is
    /// zero at the start and counts in whole blocks; the first few blocks
    /// may repeat a position while the hardware starts.
    pub sample_position: Option<u64>,
    /// The system time of that position, on the driver's clock (derived
    /// from `timeGetTime` on Windows), when it gives one.
    pub system_time: Option<Duration>,
    /// The driver's input latency, in frames.
    pub input_latency: u32,
    /// The driver's output latency, in frames.
    pub output_latency: u32,
    /// Which half of the driver's double buffer this block is, 0 or 1.
    pub buffer_index: usize,
    /// Whether the driver suggests processing at once (`directProcess`).
    pub direct_process: bool,
    /// Whether the block came through `bufferSwitchTimeInfo`, which carries
    /// the time, rather than `bufferSwitch`, after which the stream asks
    /// `getSamplePosition`.
    pub time_info: bool,
}

/// What the driver reported since the stream started, through
/// `asioMessage` and `sampleRateDidChange`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub struct Notifications {
    /// `kAsioResetRequest`: the driver must be closed and opened again.
    pub reset_requests: u32,
    /// `kAsioResyncRequest`: the driver lost sync, and the stream should
    /// start again.
    pub resync_requests: u32,
    /// `kAsioBufferSizeChange`: the driver prefers another buffer size.
    pub buffer_size_changes: u32,
    /// The size it asked for last.
    pub new_buffer_size: Option<u32>,
    /// `kAsioLatenciesChanged`: the stream reads the latencies again by
    /// itself.
    pub latency_changes: u32,
    /// `sampleRateDidChange`, or a `bufferSwitchTimeInfo` that says the
    /// rate changed.
    pub sample_rate_changes: u32,
    /// The rate reported last; zero when the driver lost its clock.
    pub new_sample_rate: Option<f64>,
    /// `kAsioOverload`: the driver noticed a dropout.
    pub overloads: u32,
}

impl Notifications {
    /// Whether the stream has to be opened again: after a reset request, a
    /// resync request, a new buffer size or a new sample rate. Open it from
    /// a thread of the application's, never from a callback.
    pub fn needs_restart(&self) -> bool {
        self.reset_requests > 0
            || self.resync_requests > 0
            || self.buffer_size_changes > 0
            || self.sample_rate_changes > 0
    }
}

/// The time `bufferSwitchTimeInfo` carries.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DriverTime {
    pub(crate) flags: u32,
    pub(crate) system_time: u64,
    pub(crate) sample_position: u64,
    pub(crate) sample_rate: f64,
}

/// One channel's half buffers, in the driver's memory.
struct Channel {
    halves: [*mut u8; 2],
    bytes: usize,
    sample_type: SampleType,
}

/// What a stream shares with whoever watches it.
#[derive(Default)]
struct Shared {
    callbacks: AtomicU64,
    frames: AtomicU64,
    time_info: AtomicBool,
    skipped: AtomicU64,
    panicked: AtomicBool,
    input_latency: AtomicU32,
    output_latency: AtomicU32,
}

/// What only the audio thread touches.
struct Audio {
    inputs: Vec<Channel>,
    outputs: Vec<Channel>,
    input: AudioBuffer<f32>,
    output: AudioBuffer<f32>,
    callback: Callback,
    frame: u64,
}

/// The running stream, as the callbacks find it through [`ENGINE`].
pub(crate) struct Engine {
    driver: *mut IAsio,
    output_ready: bool,
    sample_rate: f64,
    buffer_size: usize,
    shared: Arc<Shared>,
    /// Set while a block is being processed, so that a driver calling in
    /// again before the last call returned cannot reach `audio` twice.
    busy: AtomicBool,
    audio: UnsafeCell<Audio>,
}

impl Engine {
    /// Processes the half `index` of the buffers. Real-time safe.
    pub(crate) fn switch(&self, index: i32, direct: bool, time: Option<DriverTime>) {
        if self.busy.swap(true, Ordering::Acquire) {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let _quiet = NoDenormals::new();
        // SAFETY: `busy` gives this call the only access to `audio`.
        let audio = unsafe { &mut *self.audio.get() };
        let half = (index & 1) as usize;
        let frames = self.buffer_size;
        audio.input.set_num_frames(frames);
        audio.output.set_num_frames(frames);
        for (channel, source) in audio.inputs.iter().enumerate() {
            // SAFETY: the driver's half buffer of `bytes` bytes, valid
            // until the buffers are disposed, which only happens after the
            // driver stopped calling and the engine was retired.
            let bytes = unsafe { std::slice::from_raw_parts(source.halves[half], source.bytes) };
            let _ = source
                .sample_type
                .read(bytes, audio.input.channel_mut(channel));
        }
        audio.output.clear();

        let (sample_position, system_time, time_info) = match time {
            Some(time) => {
                let rate_changed = time.flags & sys::TIME_SAMPLE_RATE_VALID != 0
                    && time.flags & sys::TIME_SAMPLE_RATE_CHANGED != 0
                    && time.sample_rate > 0.0
                    && (time.sample_rate - self.sample_rate).abs() > 0.5;
                if rate_changed {
                    NOTICES.note_sample_rate(time.sample_rate);
                }
                (
                    (time.flags & sys::TIME_SAMPLE_POSITION_VALID != 0)
                        .then_some(time.sample_position),
                    (time.flags & sys::TIME_SYSTEM_TIME_VALID != 0).then_some(time.system_time),
                    true,
                )
            }
            None => match raw::sample_position(self.driver) {
                (sys::ASE_OK, position, stamp) => (Some(position), Some(stamp), false),
                _ => (None, None, false),
            },
        };
        if time_info {
            self.shared.time_info.store(true, Ordering::Relaxed);
        }
        let info = BlockInfo {
            frame: audio.frame,
            sample_rate: self.sample_rate,
            sample_position,
            system_time: system_time.map(Duration::from_nanos),
            input_latency: self.shared.input_latency.load(Ordering::Relaxed),
            output_latency: self.shared.output_latency.load(Ordering::Relaxed),
            buffer_index: half,
            direct_process: direct,
            time_info,
        };
        if !self.shared.panicked.load(Ordering::Relaxed) {
            // A panic must not unwind into the driver, which is C++ and
            // cannot take it: the callback is not called again, and the
            // stream plays silence. Catching costs nothing until it panics.
            let callback = &mut audio.callback;
            let (input, output) = (&mut audio.input, &mut audio.output);
            let ran = catch_unwind(AssertUnwindSafe(|| callback(input, output, &info)));
            if ran.is_err() {
                self.shared.panicked.store(true, Ordering::Relaxed);
            }
        }
        if self.shared.panicked.load(Ordering::Relaxed) {
            audio.output.set_num_frames(frames);
            audio.output.clear();
        }
        audio.output.set_num_frames(frames);
        for (channel, target) in audio.outputs.iter().enumerate() {
            // SAFETY: as for the inputs; the host owns the output half the
            // driver handed it until the call returns.
            let bytes =
                unsafe { std::slice::from_raw_parts_mut(target.halves[half], target.bytes) };
            let _ = target
                .sample_type
                .write(audio.output.channel(channel), bytes);
        }
        audio.frame += frames as u64;
        self.shared.frames.store(audio.frame, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        if self.output_ready {
            raw::output_ready(self.driver);
        }
        self.busy.store(false, Ordering::Release);
    }
}

/// Moves a value that holds raw pointers to the driver thread and back.
struct Unshared<T>(T);

// SAFETY: used only for buffer descriptions, whose pointers are into the
// driver's memory and are not dereferenced on the way.
unsafe impl<T> Send for Unshared<T> {}

/// What the driver thread found out before the buffers are made.
struct Plan {
    inputs: Vec<ChannelInfo>,
    outputs: Vec<ChannelInfo>,
    sample_rate: f64,
    buffer_size: u32,
}

fn plan(driver: &RawDriver, config: &StreamConfig) -> Result<Plan, Error> {
    let channels = query_channels(driver)?;
    let side = |direction: Direction, wanted: &[usize]| -> Result<Vec<ChannelInfo>, Error> {
        let mut infos: Vec<ChannelInfo> = Vec::with_capacity(wanted.len());
        for &index in wanted {
            if index >= channels.of(direction) {
                return Err(Error::InvalidConfig(format!(
                    "there is no {} channel {index}; the driver has {}",
                    direction.as_str(),
                    channels.of(direction)
                )));
            }
            if infos.iter().any(|info| info.index == index) {
                return Err(Error::InvalidConfig(format!(
                    "{} channel {index} is asked for twice",
                    direction.as_str()
                )));
            }
            let info = query_channel(driver, direction, index)?;
            if !info.sample_type.is_supported() {
                return Err(Error::UnsupportedSampleType {
                    direction,
                    channel: index,
                    sample_type: info.sample_type,
                });
            }
            infos.push(info);
        }
        Ok(infos)
    };
    let inputs = side(Direction::Input, &config.inputs)?;
    let outputs = side(Direction::Output, &config.outputs)?;
    if inputs.is_empty() && outputs.is_empty() {
        return Err(Error::InvalidConfig(String::from(
            "neither an input nor an output channel was asked for",
        )));
    }

    let mut sample_rate = query_sample_rate(driver)?;
    if let Some(wanted) = config.sample_rate {
        if (sample_rate - wanted).abs() > 0.5 {
            if !query_can_sample_rate(driver, wanted)? {
                return Err(Error::InvalidConfig(format!(
                    "the driver cannot run at {wanted} Hz"
                )));
            }
            Error::check("setSampleRate", driver.set_sample_rate(wanted))?;
            sample_rate = query_sample_rate(driver)?;
            if sample_rate <= 0.0 {
                sample_rate = wanted;
            }
        }
    }
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err(Error::InvalidConfig(String::from(
            "the driver reports no sample rate: its clock is lost",
        )));
    }

    let sizes = query_buffer_sizes(driver)?;
    let buffer_size = match config.buffer_size {
        Some(frames) if sizes.contains(frames) => frames,
        Some(frames) => {
            return Err(Error::InvalidConfig(format!(
                "the driver does not offer {frames}-frame buffers ({sizes:?})"
            )));
        }
        None => sizes.preferred,
    };
    if buffer_size == 0 {
        return Err(Error::InvalidConfig(String::from(
            "the driver prefers no buffer size",
        )));
    }
    Ok(Plan {
        inputs,
        outputs,
        sample_rate,
        buffer_size,
    })
}

/// How long a stopping stream waits for a callback to leave.
const RETIRE_TIMEOUT: Duration = Duration::from_secs(2);

/// Unpublishes `engine`, waits for the callbacks inside it to leave, and
/// frees it. A callback that never leaves (a driver that hangs inside one)
/// keeps the engine alive for good rather than freed under it.
fn retire(engine: NonNull<Engine>) {
    let _ = ENGINE.compare_exchange(
        engine.as_ptr(),
        ptr::null_mut(),
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    let start = Instant::now();
    while IN_FLIGHT.load(Ordering::SeqCst) > 0 {
        if start.elapsed() > RETIRE_TIMEOUT {
            return;
        }
        std::thread::yield_now();
    }
    // SAFETY: the engine came from `Box::into_raw`, nothing can reach it
    // any more, and it is freed once.
    drop(unsafe { Box::from_raw(engine.as_ptr()) });
}

/// A running stream on a [`Driver`](crate::Driver). Dropping it stops the
/// driver, disposes of the buffers and drops the callback.
pub struct Stream {
    inner: Arc<Inner>,
    engine: NonNull<Engine>,
    shared: Arc<Shared>,
    inputs: Vec<ChannelInfo>,
    outputs: Vec<ChannelInfo>,
    sample_rate: f64,
    buffer_size: u32,
    output_ready: bool,
}

// SAFETY: the engine pointer is only freed by `Drop`, after the driver has
// stopped calling into it; everything else is shared through atomics.
unsafe impl Send for Stream {}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("inputs", &self.inputs.len())
            .field("outputs", &self.outputs.len())
            .field("sample_rate", &self.sample_rate)
            .field("buffer_size", &self.buffer_size)
            .field("callbacks", &self.callbacks())
            .finish_non_exhaustive()
    }
}

impl Stream {
    pub(crate) fn start(
        inner: &Arc<Inner>,
        config: StreamConfig,
        callback: Callback,
        listener: Listener,
    ) -> Result<Self, Error> {
        if inner.streaming.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        let started = Self::open(inner, config, callback, listener);
        if started.is_err() {
            inner.streaming.store(false, Ordering::Release);
        }
        started
    }

    fn open(
        inner: &Arc<Inner>,
        config: StreamConfig,
        callback: Callback,
        listener: Listener,
    ) -> Result<Self, Error> {
        let thread = &inner.thread;
        let plan = thread.with_driver(move |driver| plan(driver, &config))?;
        let buffer_size = plan.buffer_size;

        // The buffers: inputs first, then outputs.
        let infos: Vec<AsioBufferInfo> = plan
            .inputs
            .iter()
            .map(|info| (true, info.index))
            .chain(plan.outputs.iter().map(|info| (false, info.index)))
            .map(|(input, channel)| AsioBufferInfo {
                is_input: i32::from(input),
                channel_num: channel as i32,
                buffers: [ptr::null_mut(); 2],
            })
            .collect();
        NOTICES.clear();
        let infos = Unshared(infos);
        let infos = thread.with_driver(move |driver| {
            let mut infos = infos;
            // SAFETY: the callback table is a static, valid for as long as
            // the driver keeps it.
            let code = unsafe {
                driver.create_buffers(&mut infos.0, buffer_size as i32, callbacks::table())
            };
            Error::check("createBuffers", code).map(|()| infos)
        })?;
        let dispose = || {
            let _ = thread.with_driver(|driver| {
                driver.dispose_buffers();
                Ok(())
            });
        };

        let frames = buffer_size as usize;
        let mut channels = Vec::with_capacity(infos.0.len());
        for (info, buffer) in plan.inputs.iter().chain(&plan.outputs).zip(&infos.0) {
            let bytes = info
                .sample_type
                .buffer_bytes(frames)
                .ok_or(Error::UnsupportedSampleType {
                    direction: info.direction,
                    channel: info.index,
                    sample_type: info.sample_type,
                });
            let halves = [
                buffer.buffers[0].cast::<u8>(),
                buffer.buffers[1].cast::<u8>(),
            ];
            match bytes {
                Ok(bytes) if !halves[0].is_null() && !halves[1].is_null() => {
                    channels.push(Channel {
                        halves,
                        bytes,
                        sample_type: info.sample_type,
                    })
                }
                Ok(_) => {
                    dispose();
                    return Err(Error::Driver {
                        call: "createBuffers",
                        code: sys::ASE_NO_MEMORY,
                    });
                }
                Err(error) => {
                    dispose();
                    return Err(error);
                }
            }
        }
        let outputs_start = plan.inputs.len();
        // The SDK has the host fill the output half that plays first before
        // it starts: both start silent, which is all zeros in every type.
        for channel in &channels[outputs_start..] {
            for half in channel.halves {
                // SAFETY: a half buffer of `bytes` bytes the driver just
                // made.
                unsafe { ptr::write_bytes(half, 0, channel.bytes) };
            }
        }

        let (latencies, output_ready) = match thread.with_driver(|driver| {
            let latencies = query_latencies(driver).unwrap_or_default();
            // The driver says whether it wants `outputReady`, which the
            // SDK has the host ask before streaming.
            Ok((latencies, driver.output_ready() == sys::ASE_OK))
        }) {
            Ok(answer) => answer,
            Err(error) => {
                dispose();
                return Err(error);
            }
        };
        let shared = Arc::new(Shared::default());
        shared
            .input_latency
            .store(latencies.input, Ordering::Relaxed);
        shared
            .output_latency
            .store(latencies.output, Ordering::Relaxed);

        let mut inputs = channels;
        let outputs = inputs.split_off(outputs_start);
        let driver = thread.run(|apartment| {
            apartment
                .driver
                .as_ref()
                .map(|driver| Unshared(driver.as_ptr()))
        });
        let Ok(Some(Unshared(driver))) = driver else {
            dispose();
            return Err(Error::ThreadGone);
        };
        let engine = Box::new(Engine {
            driver,
            output_ready,
            sample_rate: plan.sample_rate,
            buffer_size: frames,
            shared: Arc::clone(&shared),
            busy: AtomicBool::new(false),
            audio: UnsafeCell::new(Audio {
                input: AudioBuffer::new(inputs.len(), frames),
                output: AudioBuffer::new(outputs.len(), frames),
                inputs,
                outputs,
                callback,
                frame: 0,
            }),
        });
        let engine = NonNull::from(Box::leak(engine));
        if ENGINE
            .compare_exchange(
                ptr::null_mut(),
                engine.as_ptr(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            retire(engine);
            dispose();
            return Err(Error::Busy);
        }

        let watched = Arc::clone(&shared);
        let mut listener = listener;
        let mut last = Notifications::default();
        let housekeeping = Box::new(move |driver: &RawDriver| {
            let now = NOTICES.snapshot();
            if now == last {
                return;
            }
            if now.latency_changes != last.latency_changes {
                if let Ok(latencies) = query_latencies(driver) {
                    watched
                        .input_latency
                        .store(latencies.input, Ordering::Relaxed);
                    watched
                        .output_latency
                        .store(latencies.output, Ordering::Relaxed);
                }
            }
            listener(&now);
            last = now;
        });
        let started = thread.run(move |apartment| {
            let Some(driver) = &apartment.driver else {
                return Err(Error::ThreadGone);
            };
            let code = driver.start();
            if code == sys::ASE_OK {
                apartment.housekeeping = Some(housekeeping);
            }
            Error::check("start", code)
        });
        if let Err(error) = started.and_then(|started| started) {
            retire(engine);
            dispose();
            return Err(error);
        }
        Ok(Self {
            inner: Arc::clone(inner),
            engine,
            shared,
            inputs: plan.inputs,
            outputs: plan.outputs,
            sample_rate: plan.sample_rate,
            buffer_size,
            output_ready,
        })
    }

    /// The input channels, in the order of the callback's input buffer.
    pub fn inputs(&self) -> &[ChannelInfo] {
        &self.inputs
    }

    /// The output channels, in the order of the callback's output buffer.
    pub fn outputs(&self) -> &[ChannelInfo] {
        &self.outputs
    }

    /// The sample rate, in hertz.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The buffer size, in frames: the length of every block.
    pub fn buffer_size(&self) -> u32 {
        self.buffer_size
    }

    /// The driver's latencies, as it last reported them.
    pub fn latencies(&self) -> Latencies {
        Latencies {
            input: self.shared.input_latency.load(Ordering::Relaxed),
            output: self.shared.output_latency.load(Ordering::Relaxed),
        }
    }

    /// Whether the stream calls `outputReady` after every block, because
    /// the driver asked for it.
    pub fn uses_output_ready(&self) -> bool {
        self.output_ready
    }

    /// Whether the driver has called `bufferSwitchTimeInfo`, which carries
    /// the time, rather than `bufferSwitch`.
    pub fn uses_time_info(&self) -> bool {
        self.shared.time_info.load(Ordering::Relaxed)
    }

    /// How many blocks the callback has processed.
    pub fn callbacks(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Relaxed)
    }

    /// How many frames the callback has processed.
    pub fn frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    /// How many times the driver called in while the last block was still
    /// being processed, which the stream skips.
    pub fn skipped(&self) -> u64 {
        self.shared.skipped.load(Ordering::Relaxed)
    }

    /// Whether the callback panicked. It is not called again, and the
    /// stream plays silence.
    pub fn callback_panicked(&self) -> bool {
        self.shared.panicked.load(Ordering::Relaxed)
    }

    /// What the driver reported since the stream started.
    pub fn notifications(&self) -> Notifications {
        NOTICES.snapshot()
    }

    /// Whether the driver asked for the stream to be opened again (see
    /// [`Notifications::needs_restart`]).
    pub fn needs_restart(&self) -> bool {
        self.notifications().needs_restart()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let thread = &self.inner.thread;
        // Once `stop` returns, the SDK guarantees no more `bufferSwitch`.
        let _ = thread.run(|apartment| {
            apartment.housekeeping = None;
            if let Some(driver) = &apartment.driver {
                driver.stop();
            }
        });
        retire(self.engine);
        let _ = thread.with_driver(|driver| {
            driver.dispose_buffers();
            Ok(())
        });
        self.inner.streaming.store(false, Ordering::Release);
    }
}
