//! The four host callbacks a driver calls, and what they reach.
//!
//! The callbacks are plain C functions with no context pointer, so what they
//! work on is found through statics. ASIO allows one loaded driver per
//! process, and a driver runs one set of buffers at a time, so there is at
//! most one stream to find: [`ENGINE`] points at it while it runs.
//!
//! Everything here is real-time safe: atomics only, no allocation, no lock,
//! no system call. `asioMessage` may arrive on any thread at any time, even
//! inside `createBuffers` or `start`, so it touches nothing but [`NOTICES`];
//! a reset or a change of buffer size it reports is acted on later, by the
//! driver thread and whoever owns the stream, never from inside the
//! callback, as the SDK requires (the callback has to return into the
//! driver).

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::stream::{DriverTime, Engine, Notifications};
use crate::sys::{self, AsioBool, AsioCallbacks, AsioSampleRate, AsioTime};

/// The running stream, or null.
pub(crate) static ENGINE: AtomicPtr<Engine> = AtomicPtr::new(ptr::null_mut());

/// How many `bufferSwitch` calls are inside [`ENGINE`] right now, so that
/// a stopping stream can wait for them to leave before it is freed.
pub(crate) static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// What the driver reported through `asioMessage` and
/// `sampleRateDidChange`, counted since the stream started.
pub(crate) static NOTICES: Notices = Notices::new();

pub(crate) struct Notices {
    resets: AtomicU32,
    resyncs: AtomicU32,
    buffer_size_changes: AtomicU32,
    buffer_size: AtomicI32,
    latency_changes: AtomicU32,
    rate_changes: AtomicU32,
    /// The bits of the last rate reported, as an `f64`.
    rate: AtomicU64,
    overloads: AtomicU32,
}

impl Notices {
    const fn new() -> Self {
        Self {
            resets: AtomicU32::new(0),
            resyncs: AtomicU32::new(0),
            buffer_size_changes: AtomicU32::new(0),
            buffer_size: AtomicI32::new(0),
            latency_changes: AtomicU32::new(0),
            rate_changes: AtomicU32::new(0),
            rate: AtomicU64::new(0),
            overloads: AtomicU32::new(0),
        }
    }

    /// Forgets everything, as a stream starts.
    pub(crate) fn clear(&self) {
        for counter in [
            &self.resets,
            &self.resyncs,
            &self.buffer_size_changes,
            &self.latency_changes,
            &self.rate_changes,
            &self.overloads,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
        self.buffer_size.store(0, Ordering::Relaxed);
        self.rate.store(0, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> Notifications {
        let size = self.buffer_size.load(Ordering::Acquire);
        let rate = f64::from_bits(self.rate.load(Ordering::Acquire));
        let rate_changes = self.rate_changes.load(Ordering::Acquire);
        Notifications {
            reset_requests: self.resets.load(Ordering::Acquire),
            resync_requests: self.resyncs.load(Ordering::Acquire),
            buffer_size_changes: self.buffer_size_changes.load(Ordering::Acquire),
            new_buffer_size: u32::try_from(size).ok().filter(|&size| size > 0),
            latency_changes: self.latency_changes.load(Ordering::Acquire),
            sample_rate_changes: rate_changes,
            new_sample_rate: (rate_changes > 0).then_some(rate),
            overloads: self.overloads.load(Ordering::Acquire),
        }
    }

    /// A sample rate the driver now runs at, from `sampleRateDidChange` or
    /// the flags of `bufferSwitchTimeInfo`. Zero means the clock is lost.
    pub(crate) fn note_sample_rate(&self, rate: f64) {
        let rate = if rate.is_finite() && rate >= 0.0 {
            rate
        } else {
            0.0
        };
        self.rate.store(rate.to_bits(), Ordering::Release);
        self.rate_changes.fetch_add(1, Ordering::AcqRel);
    }
}

/// The callbacks handed to every `createBuffers`. The driver keeps the
/// pointer until the buffers are disposed, so it lives in a static.
struct CallbackTable(UnsafeCell<AsioCallbacks>);

// SAFETY: the table is never written after it is built; the driver only
// reads it.
unsafe impl Sync for CallbackTable {}

static CALLBACKS: CallbackTable = CallbackTable(UnsafeCell::new(AsioCallbacks {
    buffer_switch,
    sample_rate_did_change,
    asio_message,
    buffer_switch_time_info,
}));

pub(crate) fn table() -> *mut AsioCallbacks {
    CALLBACKS.0.get()
}

/// Hands one block to the running stream, if any.
fn dispatch(index: i32, direct: bool, time: Option<DriverTime>) {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let engine = ENGINE.load(Ordering::SeqCst);
    if !engine.is_null() {
        // SAFETY: a published engine stays alive until it is unpublished
        // and every call counted in `IN_FLIGHT` has left; this one was
        // counted before it looked.
        unsafe { (*engine).switch(index, direct, time) };
    }
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
}

/// `bufferSwitch`: process the half `index` of the buffers.
unsafe extern "C" fn buffer_switch(index: i32, direct_process: AsioBool) {
    dispatch(index, direct_process != sys::ASIO_FALSE, None);
}

/// `bufferSwitchTimeInfo`: the same, with the time of the block.
unsafe extern "C" fn buffer_switch_time_info(
    params: *mut AsioTime,
    index: i32,
    direct_process: AsioBool,
) -> *mut AsioTime {
    let time = if params.is_null() {
        None
    } else {
        // SAFETY: the driver hands a valid structure for the length of the
        // call. It is packed, so it is copied out unaligned.
        let info = unsafe { ptr::addr_of!((*params).time_info).read_unaligned() };
        Some(DriverTime {
            flags: info.flags,
            system_time: info.system_time.get(),
            sample_position: info.sample_position.get(),
            sample_rate: info.sample_rate,
        })
    };
    dispatch(index, direct_process != sys::ASIO_FALSE, time);
    params
}

/// `sampleRateDidChange`: the clock changed, usually an external one. The
/// stream asks to be restarted, so the callback is prepared for the new
/// rate.
unsafe extern "C" fn sample_rate_did_change(rate: AsioSampleRate) {
    NOTICES.note_sample_rate(rate);
}

/// `asioMessage`: the driver asks the host something, or tells it.
unsafe extern "C" fn asio_message(
    selector: i32,
    value: i32,
    _message: *mut c_void,
    _opt: *mut f64,
) -> i32 {
    answer(selector, value)
}

/// What the host answers to `asioMessage`. Real-time safe.
pub(crate) fn answer(selector: i32, value: i32) -> i32 {
    match selector {
        sys::SELECTOR_SUPPORTED => i32::from(matches!(
            value,
            sys::ENGINE_VERSION
                | sys::RESET_REQUEST
                | sys::BUFFER_SIZE_CHANGE
                | sys::RESYNC_REQUEST
                | sys::LATENCIES_CHANGED
                | sys::SUPPORTS_TIME_INFO
                | sys::OVERLOAD
        )),
        // This host implements version 2 of the interface.
        sys::ENGINE_VERSION => 2,
        sys::RESET_REQUEST => {
            NOTICES.resets.fetch_add(1, Ordering::AcqRel);
            1
        }
        sys::BUFFER_SIZE_CHANGE => {
            // Accepted: the stream is opened again with the new size, off
            // the audio thread.
            NOTICES.buffer_size.store(value, Ordering::Release);
            NOTICES.buffer_size_changes.fetch_add(1, Ordering::AcqRel);
            1
        }
        sys::RESYNC_REQUEST => {
            NOTICES.resyncs.fetch_add(1, Ordering::AcqRel);
            1
        }
        sys::LATENCIES_CHANGED => {
            NOTICES.latency_changes.fetch_add(1, Ordering::AcqRel);
            1
        }
        sys::SUPPORTS_TIME_INFO => 1,
        sys::OVERLOAD => {
            NOTICES.overloads.fetch_add(1, Ordering::AcqRel);
            0
        }
        // Time code, MMC, input monitoring, gains and meters are not
        // supported; neither is anything newer.
        _ => 0,
    }
}
