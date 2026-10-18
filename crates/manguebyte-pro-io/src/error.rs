use std::fmt;

use manguebyte_devices::Direction;

use crate::SampleType;
use crate::sys;

/// What went wrong while talking to a driver.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// No installed driver has this name.
    DriverNotFound(String),
    /// A driver is loaded already. ASIO allows one loaded driver per
    /// process: drop the [`Driver`](crate::Driver) that holds it first.
    AlreadyLoaded {
        /// The driver that is loaded.
        loaded: String,
    },
    /// A call to the system failed: COM or the registry. `code` is the
    /// `HRESULT` or the registry's status.
    System {
        /// What was called.
        call: &'static str,
        /// What it returned.
        code: i32,
    },
    /// The driver would not initialise, with the message it gave.
    InitFailed {
        /// The driver.
        driver: String,
        /// Its error message, which may be empty.
        message: String,
    },
    /// A driver method returned an error.
    Driver {
        /// The method.
        call: &'static str,
        /// The `ASIOError` it returned.
        code: i32,
    },
    /// The driver cannot run with what was asked: a channel it does not
    /// have, a buffer size or sample rate it does not offer.
    InvalidConfig(String),
    /// A channel's samples are in a type this crate does not convert (DSD).
    UnsupportedSampleType {
        /// Which side the channel is on.
        direction: Direction,
        /// The channel.
        channel: usize,
        /// Its type.
        sample_type: SampleType,
    },
    /// A stream is running on the driver already: a driver runs one set of
    /// buffers at a time.
    Busy,
    /// The thread the driver lives on has stopped, because a call into the
    /// driver panicked.
    ThreadGone,
    /// Drivers are reached through COM, which exists only on Windows. The
    /// fake driver works everywhere.
    Unsupported,
}

impl Error {
    pub(crate) fn check(call: &'static str, code: sys::AsioError) -> Result<(), Self> {
        if code == sys::ASE_OK {
            Ok(())
        } else {
            Err(Self::Driver { call, code })
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DriverNotFound(name) => write!(f, "no driver called \"{name}\" is installed"),
            Self::AlreadyLoaded { loaded } => write!(
                f,
                "the driver \"{loaded}\" is loaded already, and a process loads one at a time"
            ),
            Self::System { call, code } => write!(f, "{call} failed with {code:#010x}"),
            Self::InitFailed { driver, message } if message.is_empty() => {
                write!(f, "the driver \"{driver}\" would not initialise")
            }
            Self::InitFailed { driver, message } => {
                write!(f, "the driver \"{driver}\" would not initialise: {message}")
            }
            Self::Driver { call, code } => {
                write!(f, "{call} returned {} ({code})", sys::error_name(*code))
            }
            Self::InvalidConfig(reason) => write!(f, "the driver cannot run so: {reason}"),
            Self::UnsupportedSampleType {
                direction,
                channel,
                sample_type,
            } => write!(
                f,
                "{} channel {channel} holds {sample_type} samples, which are not converted",
                direction.as_str()
            ),
            Self::Busy => f.write_str("a stream is running on the driver already"),
            Self::ThreadGone => f.write_str("the driver's thread has stopped"),
            Self::Unsupported => f.write_str("drivers are reached through COM, on Windows only"),
        }
    }
}

impl std::error::Error for Error {}
