//! The driver interface: the `IASIO` COM interface, its structures,
//! constants and host callbacks.
//!
//! Declared by hand from the Steinberg ASIO SDK 2.3.4 interface definitions
//! (`common/asio.h`, `common/iasiodrv.h` and the SDK's specification), used
//! under the GPLv3. Nothing of the SDK is copied into this crate and no
//! bindings are generated.
//!
//! On Windows the SDK's `long` is 32 bits on every architecture, so it is
//! `i32` here. The SDK packs its structures to four bytes with MSVC; only
//! the time structures, which hold doubles, change layout because of it,
//! and they are declared `packed(4)`. The sizes and offsets are checked at
//! compile time.
//!
//! ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.

#![allow(missing_docs)]

use std::ffi::{c_char, c_void};
use std::mem::{offset_of, size_of};

/// `ASIOBool`: zero is false.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub type AsioBool = i32;
/// `ASIOError`: one of the `ASE_` codes.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub type AsioError = i32;
/// `ASIOSampleRate`: an IEEE 754 double on every platform the SDK lists.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub type AsioSampleRate = f64;
/// `ASIOSampleType`: one of the `ASIOST` codes.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub type AsioSampleType = i32;

/// `ASIOFalse`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub const ASIO_FALSE: AsioBool = 0;
/// `ASIOTrue`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
pub const ASIO_TRUE: AsioBool = 1;

pub const ASE_OK: AsioError = 0;
/// The success value of `future`, where `ASE_OK` is not enough.
pub const ASE_SUCCESS: AsioError = 0x3f48_47a0;
pub const ASE_NOT_PRESENT: AsioError = -1000;
pub const ASE_HW_MALFUNCTION: AsioError = -999;
pub const ASE_INVALID_PARAMETER: AsioError = -998;
pub const ASE_INVALID_MODE: AsioError = -997;
pub const ASE_SP_NOT_ADVANCING: AsioError = -996;
pub const ASE_NO_CLOCK: AsioError = -995;
pub const ASE_NO_MEMORY: AsioError = -994;

/// The SDK's name for an error code.
pub fn error_name(code: AsioError) -> &'static str {
    match code {
        ASE_OK => "ASE_OK",
        ASE_SUCCESS => "ASE_SUCCESS",
        ASE_NOT_PRESENT => "ASE_NotPresent",
        ASE_HW_MALFUNCTION => "ASE_HWMalfunction",
        ASE_INVALID_PARAMETER => "ASE_InvalidParameter",
        ASE_INVALID_MODE => "ASE_InvalidMode",
        ASE_SP_NOT_ADVANCING => "ASE_SPNotAdvancing",
        ASE_NO_CLOCK => "ASE_NoClock",
        ASE_NO_MEMORY => "ASE_NoMemory",
        _ => "an unknown error",
    }
}

// Sample types.
pub const ST_INT16_MSB: AsioSampleType = 0;
pub const ST_INT24_MSB: AsioSampleType = 1;
pub const ST_INT32_MSB: AsioSampleType = 2;
pub const ST_FLOAT32_MSB: AsioSampleType = 3;
pub const ST_FLOAT64_MSB: AsioSampleType = 4;
pub const ST_INT32_MSB16: AsioSampleType = 8;
pub const ST_INT32_MSB18: AsioSampleType = 9;
pub const ST_INT32_MSB20: AsioSampleType = 10;
pub const ST_INT32_MSB24: AsioSampleType = 11;
pub const ST_INT16_LSB: AsioSampleType = 16;
pub const ST_INT24_LSB: AsioSampleType = 17;
pub const ST_INT32_LSB: AsioSampleType = 18;
pub const ST_FLOAT32_LSB: AsioSampleType = 19;
pub const ST_FLOAT64_LSB: AsioSampleType = 20;
pub const ST_INT32_LSB16: AsioSampleType = 24;
pub const ST_INT32_LSB18: AsioSampleType = 25;
pub const ST_INT32_LSB20: AsioSampleType = 26;
pub const ST_INT32_LSB24: AsioSampleType = 27;
pub const ST_DSD_INT8_LSB1: AsioSampleType = 32;
pub const ST_DSD_INT8_MSB1: AsioSampleType = 33;
pub const ST_DSD_INT8_NER8: AsioSampleType = 40;

// Selectors of the host's `asioMessage` callback.
pub const SELECTOR_SUPPORTED: i32 = 1;
pub const ENGINE_VERSION: i32 = 2;
pub const RESET_REQUEST: i32 = 3;
pub const BUFFER_SIZE_CHANGE: i32 = 4;
pub const RESYNC_REQUEST: i32 = 5;
pub const LATENCIES_CHANGED: i32 = 6;
pub const SUPPORTS_TIME_INFO: i32 = 7;
pub const SUPPORTS_TIME_CODE: i32 = 8;
pub const MMC_COMMAND: i32 = 9;
pub const SUPPORTS_INPUT_MONITOR: i32 = 10;
pub const SUPPORTS_INPUT_GAIN: i32 = 11;
pub const SUPPORTS_INPUT_METER: i32 = 12;
pub const SUPPORTS_OUTPUT_GAIN: i32 = 13;
pub const SUPPORTS_OUTPUT_METER: i32 = 14;
pub const OVERLOAD: i32 = 15;

// Selectors of the driver's `future` method.
pub const FUTURE_ENABLE_TIME_CODE_READ: i32 = 1;
pub const FUTURE_DISABLE_TIME_CODE_READ: i32 = 2;
pub const FUTURE_SET_INPUT_MONITOR: i32 = 3;
pub const FUTURE_TRANSPORT: i32 = 4;
pub const FUTURE_SET_INPUT_GAIN: i32 = 5;
pub const FUTURE_GET_INPUT_METER: i32 = 6;
pub const FUTURE_SET_OUTPUT_GAIN: i32 = 7;
pub const FUTURE_GET_OUTPUT_METER: i32 = 8;
pub const FUTURE_CAN_INPUT_MONITOR: i32 = 9;
pub const FUTURE_CAN_TIME_INFO: i32 = 10;
pub const FUTURE_CAN_TIME_CODE: i32 = 11;
pub const FUTURE_CAN_TRANSPORT: i32 = 12;
pub const FUTURE_CAN_INPUT_GAIN: i32 = 13;
pub const FUTURE_CAN_INPUT_METER: i32 = 14;
pub const FUTURE_CAN_OUTPUT_GAIN: i32 = 15;
pub const FUTURE_CAN_OUTPUT_METER: i32 = 16;
pub const FUTURE_OPTIONAL_ONE: i32 = 17;
pub const FUTURE_SET_IO_FORMAT: i32 = 0x2311_1961;
pub const FUTURE_GET_IO_FORMAT: i32 = 0x2311_1983;
pub const FUTURE_CAN_DO_IO_FORMAT: i32 = 0x2311_2004;
pub const FUTURE_CAN_REPORT_OVERLOAD: i32 = 0x2404_2012;
pub const FUTURE_GET_INTERNAL_BUFFER_SAMPLES: i32 = 0x2504_2012;

// Flags of `AsioTimeInfo::flags`.
pub const TIME_SYSTEM_TIME_VALID: u32 = 1;
pub const TIME_SAMPLE_POSITION_VALID: u32 = 1 << 1;
pub const TIME_SAMPLE_RATE_VALID: u32 = 1 << 2;
pub const TIME_SPEED_VALID: u32 = 1 << 3;
pub const TIME_SAMPLE_RATE_CHANGED: u32 = 1 << 4;
pub const TIME_CLOCK_SOURCE_CHANGED: u32 = 1 << 5;

// Flags of `AsioTimeCode::flags`.
pub const TC_VALID: u32 = 1;
pub const TC_RUNNING: u32 = 1 << 1;
pub const TC_REVERSE: u32 = 1 << 2;
pub const TC_ONSPEED: u32 = 1 << 3;
pub const TC_STILL: u32 = 1 << 4;
pub const TC_SPEED_VALID: u32 = 1 << 8;

/// `ASIOSamples`: a 64-bit sample count as two 32-bit halves.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AsioSamples {
    pub hi: u32,
    pub lo: u32,
}

impl AsioSamples {
    pub fn get(self) -> u64 {
        (u64::from(self.hi) << 32) | u64::from(self.lo)
    }

    pub fn new(value: u64) -> Self {
        Self {
            hi: (value >> 32) as u32,
            lo: value as u32,
        }
    }
}

/// `ASIOTimeStamp`: nanoseconds as two 32-bit halves.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AsioTimeStamp {
    pub hi: u32,
    pub lo: u32,
}

impl AsioTimeStamp {
    pub fn get(self) -> u64 {
        (u64::from(self.hi) << 32) | u64::from(self.lo)
    }

    pub fn new(value: u64) -> Self {
        Self {
            hi: (value >> 32) as u32,
            lo: value as u32,
        }
    }
}

/// `ASIOTimeCode`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioTimeCode {
    pub speed: f64,
    pub time_code_samples: AsioSamples,
    pub flags: u32,
    pub future: [c_char; 64],
}

/// `AsioTimeInfo`.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioTimeInfo {
    pub speed: f64,
    pub system_time: AsioTimeStamp,
    pub sample_position: AsioSamples,
    pub sample_rate: AsioSampleRate,
    pub flags: u32,
    pub reserved: [c_char; 12],
}

/// `ASIOTime`, which the driver hands to `bufferSwitchTimeInfo`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioTime {
    pub reserved: [i32; 4],
    pub time_info: AsioTimeInfo,
    pub time_code: AsioTimeCode,
}

impl AsioTime {
    /// All zeros, as the SDK's example driver starts it.
    pub const fn zeroed() -> Self {
        Self {
            reserved: [0; 4],
            time_info: AsioTimeInfo {
                speed: 0.0,
                system_time: AsioTimeStamp { hi: 0, lo: 0 },
                sample_position: AsioSamples { hi: 0, lo: 0 },
                sample_rate: 0.0,
                flags: 0,
                reserved: [0; 12],
            },
            time_code: AsioTimeCode {
                speed: 0.0,
                time_code_samples: AsioSamples { hi: 0, lo: 0 },
                flags: 0,
                future: [0; 64],
            },
        }
    }
}

/// `ASIOClockSource`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioClockSource {
    pub index: i32,
    pub associated_channel: i32,
    pub associated_group: i32,
    pub is_current_source: AsioBool,
    pub name: [c_char; 32],
}

/// `ASIOChannelInfo`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioChannelInfo {
    pub channel: i32,
    pub is_input: AsioBool,
    pub is_active: AsioBool,
    pub channel_group: i32,
    pub sample_type: AsioSampleType,
    pub name: [c_char; 32],
}

/// `ASIOBufferInfo`: the host fills in the first two fields, the driver
/// the two halves of the channel's double buffer.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioBufferInfo {
    pub is_input: AsioBool,
    pub channel_num: i32,
    pub buffers: [*mut c_void; 2],
}

/// `ASIOInternalBufferInfo`, for `FUTURE_GET_INTERNAL_BUFFER_SAMPLES`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AsioInternalBufferInfo {
    pub input_samples: i32,
    pub output_samples: i32,
}

/// `ASIOCallbacks`: what the host hands the driver in `createBuffers`.
///
/// These are plain C function pointers in a C structure, with no context
/// pointer: the C calling convention, which on x86-64 and AArch64 is the
/// system's own.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioCallbacks {
    pub buffer_switch: unsafe extern "C" fn(double_buffer_index: i32, direct_process: AsioBool),
    pub sample_rate_did_change: unsafe extern "C" fn(rate: AsioSampleRate),
    pub asio_message:
        unsafe extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    pub buffer_switch_time_info: unsafe extern "C" fn(
        params: *mut AsioTime,
        double_buffer_index: i32,
        direct_process: AsioBool,
    ) -> *mut AsioTime,
}

/// A COM class or interface identifier.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// The type of an `IASIO` method. The interface is a C++ class whose
/// methods carry no calling convention of their own, so on 32-bit x86
/// MSVC calls them with `thiscall`; everywhere else that is the system's
/// convention.
#[cfg(all(windows, target_arch = "x86"))]
macro_rules! method {
    (fn($($arg:ty),* $(,)?) $(-> $ret:ty)?) => {
        unsafe extern "thiscall" fn($($arg),*) $(-> $ret)?
    };
}
#[cfg(not(all(windows, target_arch = "x86")))]
macro_rules! method {
    (fn($($arg:ty),* $(,)?) $(-> $ret:ty)?) => {
        unsafe extern "system" fn($($arg),*) $(-> $ret)?
    };
}

/// Defines a function with the calling convention of an `IASIO` method,
/// for the fake driver.
#[cfg(all(windows, target_arch = "x86"))]
macro_rules! define_method {
    ($(#[$attr:meta])* fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block) => {
        $(#[$attr])*
        unsafe extern "thiscall" fn $name($($arg: $ty),*) $(-> $ret)? $body
    };
}
#[cfg(not(all(windows, target_arch = "x86")))]
macro_rules! define_method {
    ($(#[$attr:meta])* fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block) => {
        $(#[$attr])*
        unsafe extern "system" fn $name($($arg: $ty),*) $(-> $ret)? $body
    };
}
pub(crate) use define_method;

/// The `IASIO` vtable: `IUnknown`'s three methods, then the driver's, in
/// the order `iasiodrv.h` declares them.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
pub struct IAsioVtbl {
    pub query_interface: unsafe extern "system" fn(
        this: *mut IAsio,
        iid: *const Guid,
        object: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut IAsio) -> u32,
    pub release: unsafe extern "system" fn(this: *mut IAsio) -> u32,
    pub init: method!(fn(*mut IAsio, *mut c_void) -> AsioBool),
    pub get_driver_name: method!(fn(*mut IAsio, *mut c_char)),
    pub get_driver_version: method!(fn(*mut IAsio) -> i32),
    pub get_error_message: method!(fn(*mut IAsio, *mut c_char)),
    pub start: method!(fn(*mut IAsio) -> AsioError),
    pub stop: method!(fn(*mut IAsio) -> AsioError),
    pub get_channels: method!(fn(*mut IAsio, *mut i32, *mut i32) -> AsioError),
    pub get_latencies: method!(fn(*mut IAsio, *mut i32, *mut i32) -> AsioError),
    pub get_buffer_size:
        method!(fn(*mut IAsio, *mut i32, *mut i32, *mut i32, *mut i32) -> AsioError),
    pub can_sample_rate: method!(fn(*mut IAsio, AsioSampleRate) -> AsioError),
    pub get_sample_rate: method!(fn(*mut IAsio, *mut AsioSampleRate) -> AsioError),
    pub set_sample_rate: method!(fn(*mut IAsio, AsioSampleRate) -> AsioError),
    pub get_clock_sources: method!(fn(*mut IAsio, *mut AsioClockSource, *mut i32) -> AsioError),
    pub set_clock_source: method!(fn(*mut IAsio, i32) -> AsioError),
    pub get_sample_position:
        method!(fn(*mut IAsio, *mut AsioSamples, *mut AsioTimeStamp) -> AsioError),
    pub get_channel_info: method!(fn(*mut IAsio, *mut AsioChannelInfo) -> AsioError),
    pub create_buffers:
        method!(fn(*mut IAsio, *mut AsioBufferInfo, i32, i32, *mut AsioCallbacks) -> AsioError),
    pub dispose_buffers: method!(fn(*mut IAsio) -> AsioError),
    pub control_panel: method!(fn(*mut IAsio) -> AsioError),
    pub future: method!(fn(*mut IAsio, i32, *mut c_void) -> AsioError),
    pub output_ready: method!(fn(*mut IAsio) -> AsioError),
}

/// An `IASIO` object: a pointer to its vtable, then whatever the driver
/// keeps.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[repr(C)]
pub struct IAsio {
    pub vtbl: *const IAsioVtbl,
}

// The layouts MSVC gives the SDK's structures under `pack(4)`.
const _: () = {
    assert!(size_of::<AsioSamples>() == 8);
    assert!(size_of::<AsioTimeStamp>() == 8);
    assert!(size_of::<AsioTimeCode>() == 84);
    assert!(offset_of!(AsioTimeCode, flags) == 16);
    assert!(size_of::<AsioTimeInfo>() == 48);
    assert!(offset_of!(AsioTimeInfo, sample_rate) == 24);
    assert!(offset_of!(AsioTimeInfo, flags) == 32);
    assert!(size_of::<AsioTime>() == 148);
    assert!(offset_of!(AsioTime, time_info) == 16);
    assert!(offset_of!(AsioTime, time_code) == 64);
    assert!(size_of::<AsioClockSource>() == 48);
    assert!(size_of::<AsioChannelInfo>() == 52);
    assert!(offset_of!(AsioBufferInfo, buffers) == 8);
    assert!(size_of::<AsioBufferInfo>() == 8 + 2 * size_of::<*mut c_void>());
    assert!(size_of::<AsioCallbacks>() == 4 * size_of::<*mut c_void>());
    assert!(size_of::<Guid>() == 16);
    assert!(size_of::<IAsioVtbl>() == 24 * size_of::<*mut c_void>());
};
