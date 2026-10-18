//! Safe calls on a live `IASIO` object.

use std::ffi::{c_char, c_void};
use std::ptr;

use crate::driver::ClockSource;
use crate::sys::{
    self, AsioBufferInfo, AsioCallbacks, AsioChannelInfo, AsioClockSource, AsioError, AsioSamples,
    AsioTimeStamp, IAsio, IAsioVtbl,
};

/// One reference to a driver object, released when dropped.
///
/// The object is a single-threaded COM object: every call but
/// [`output_ready`](Self::output_ready) and [`sample_position`], which the
/// SDK has the host
/// make from inside the driver's own `bufferSwitch`, belongs on the thread
/// that created it, and the driver thread keeps it there.
pub(crate) struct RawDriver(*mut IAsio);

// SAFETY: the pointer is only used on the driver thread, which owns the
// `RawDriver`, apart from the two calls the SDK makes from the driver's
// own audio thread; `Send` is what lets the driver thread take it over.
unsafe impl Send for RawDriver {}

fn text(buffer: &[c_char]) -> String {
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl RawDriver {
    /// Takes over one reference to `object`.
    ///
    /// # Safety
    ///
    /// `object` must point at a live `IASIO` object with a reference the
    /// caller hands over.
    pub(crate) unsafe fn new(object: *mut IAsio) -> Self {
        Self(object)
    }

    pub(crate) fn as_ptr(&self) -> *mut IAsio {
        self.0
    }

    fn vtbl(&self) -> &IAsioVtbl {
        // SAFETY: a live object starts with a pointer to its vtable, which
        // lives as long as the object.
        unsafe { &*(*self.0).vtbl }
    }

    pub(crate) fn init(&self, window: *mut c_void) -> bool {
        // SAFETY: a live object; the window handle may be null.
        unsafe { (self.vtbl().init)(self.0, window) != sys::ASIO_FALSE }
    }

    pub(crate) fn name(&self) -> String {
        // The SDK gives the name 32 characters; more room costs nothing.
        let mut buffer = [0 as c_char; 128];
        // SAFETY: a live object and room for the name.
        unsafe { (self.vtbl().get_driver_name)(self.0, buffer.as_mut_ptr()) };
        text(&buffer)
    }

    pub(crate) fn version(&self) -> i32 {
        // SAFETY: a live object.
        unsafe { (self.vtbl().get_driver_version)(self.0) }
    }

    pub(crate) fn error_message(&self) -> String {
        // The SDK gives the message 124 characters.
        let mut buffer = [0 as c_char; 256];
        // SAFETY: a live object and room for the message.
        unsafe { (self.vtbl().get_error_message)(self.0, buffer.as_mut_ptr()) };
        text(&buffer)
    }

    pub(crate) fn start(&self) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().start)(self.0) }
    }

    pub(crate) fn stop(&self) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().stop)(self.0) }
    }

    pub(crate) fn channels(&self) -> (AsioError, i32, i32) {
        let (mut inputs, mut outputs) = (0, 0);
        // SAFETY: a live object and two integers to fill in.
        let code = unsafe { (self.vtbl().get_channels)(self.0, &mut inputs, &mut outputs) };
        (code, inputs, outputs)
    }

    pub(crate) fn latencies(&self) -> (AsioError, i32, i32) {
        let (mut input, mut output) = (0, 0);
        // SAFETY: a live object and two integers to fill in.
        let code = unsafe { (self.vtbl().get_latencies)(self.0, &mut input, &mut output) };
        (code, input, output)
    }

    pub(crate) fn buffer_size(&self) -> (AsioError, [i32; 4]) {
        let mut sizes = [0i32; 4];
        let [min, max, preferred, granularity] = &mut sizes;
        // SAFETY: a live object and four integers to fill in.
        let code =
            unsafe { (self.vtbl().get_buffer_size)(self.0, min, max, preferred, granularity) };
        (code, sizes)
    }

    pub(crate) fn can_sample_rate(&self, rate: f64) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().can_sample_rate)(self.0, rate) }
    }

    pub(crate) fn sample_rate(&self) -> (AsioError, f64) {
        let mut rate = 0.0;
        // SAFETY: a live object and a double to fill in.
        let code = unsafe { (self.vtbl().get_sample_rate)(self.0, &mut rate) };
        (code, rate)
    }

    pub(crate) fn set_sample_rate(&self, rate: f64) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().set_sample_rate)(self.0, rate) }
    }

    pub(crate) fn clock_sources(&self) -> (AsioError, Vec<ClockSource>) {
        let empty = AsioClockSource {
            index: 0,
            associated_channel: 0,
            associated_group: 0,
            is_current_source: 0,
            name: [0; 32],
        };
        let mut sources = [empty; 32];
        let mut count = sources.len() as i32;
        // SAFETY: a live object, room for `count` sources, and the count.
        let code =
            unsafe { (self.vtbl().get_clock_sources)(self.0, sources.as_mut_ptr(), &mut count) };
        let count = count.clamp(0, sources.len() as i32) as usize;
        let list = sources[..count]
            .iter()
            .map(|source| ClockSource {
                index: source.index,
                name: text(&source.name),
                channel: source.associated_channel,
                group: source.associated_group,
                is_current: source.is_current_source != sys::ASIO_FALSE,
            })
            .collect();
        (code, list)
    }

    pub(crate) fn set_clock_source(&self, index: i32) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().set_clock_source)(self.0, index) }
    }

    pub(crate) fn channel_info(&self, channel: i32, input: bool) -> (AsioError, AsioChannelInfo) {
        let mut info = AsioChannelInfo {
            channel,
            is_input: i32::from(input),
            is_active: 0,
            channel_group: 0,
            sample_type: 0,
            name: [0; 32],
        };
        // SAFETY: a live object and a structure to fill in.
        let code = unsafe { (self.vtbl().get_channel_info)(self.0, &mut info) };
        (code, info)
    }

    pub(crate) fn channel_name(info: &AsioChannelInfo) -> String {
        text(&info.name)
    }

    /// # Safety
    ///
    /// `callbacks` must stay valid until the buffers are disposed.
    pub(crate) unsafe fn create_buffers(
        &self,
        infos: &mut [AsioBufferInfo],
        frames: i32,
        callbacks: *mut AsioCallbacks,
    ) -> AsioError {
        // SAFETY: a live object, `infos.len()` structures, and callbacks the
        // caller keeps alive.
        unsafe {
            (self.vtbl().create_buffers)(
                self.0,
                infos.as_mut_ptr(),
                infos.len() as i32,
                frames,
                callbacks,
            )
        }
    }

    pub(crate) fn dispose_buffers(&self) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().dispose_buffers)(self.0) }
    }

    pub(crate) fn control_panel(&self) -> AsioError {
        // SAFETY: a live object.
        unsafe { (self.vtbl().control_panel)(self.0) }
    }

    pub(crate) fn future(&self, selector: i32, parameter: *mut c_void) -> AsioError {
        // SAFETY: a live object; the parameter is what the selector wants,
        // which callers pass.
        unsafe { (self.vtbl().future)(self.0, selector, parameter) }
    }

    pub(crate) fn output_ready(&self) -> AsioError {
        output_ready(self.0)
    }
}

/// `outputReady` on the driver's audio thread.
pub(crate) fn output_ready(driver: *mut IAsio) -> AsioError {
    // SAFETY: callers pass a live object; the method takes nothing else.
    unsafe { ((*(*driver).vtbl).output_ready)(driver) }
}

/// `getSamplePosition` on the driver's audio thread: the sample position
/// and the system time in nanoseconds.
pub(crate) fn sample_position(driver: *mut IAsio) -> (AsioError, u64, u64) {
    let mut position = AsioSamples::default();
    let mut stamp = AsioTimeStamp::default();
    // SAFETY: callers pass a live object; two structures to fill in.
    let code =
        unsafe { ((*(*driver).vtbl).get_sample_position)(driver, &mut position, &mut stamp) };
    (code, position.get(), stamp.get())
}

impl Drop for RawDriver {
    fn drop(&mut self) {
        // SAFETY: the reference this value holds is released once.
        unsafe { (self.vtbl().release)(self.0) };
        self.0 = ptr::null_mut();
    }
}
