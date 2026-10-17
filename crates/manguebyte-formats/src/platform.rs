//! Formats the system decodes: AAC and ALAC in MPEG-4 files (`.m4a`,
//! `.mp4`), AAC in ADTS (`.aac`), Windows Media Audio (`.wma`) and the Core
//! Audio Format (`.caf`), through the codecs of the operating system.
//!
//! These formats are recognised whatever the build, so that
//! [`read_file`](crate::read_file) and [`AudioReader`](crate::AudioReader)
//! say what a file is even where they cannot decode it. Decoding them
//! needs the `platform-codecs` feature, and a system that has a codec:
//!
//! | System | Through | Decodes |
//! |---|---|---|
//! | Windows 10 and 11 | Media Foundation's source reader | AAC (MPEG-4 and ADTS), ALAC, WMA, and the other formats it has a codec for |
//! | macOS | AudioToolbox's `ExtAudioFile` | AAC (MPEG-4 and ADTS), ALAC, CAF; not WMA |
//! | Linux and others | nothing | none: there is no codec layer every system has, so these formats are refused with [`FormatError::Unsupported`] |
//!
//! Without the feature they are refused the same way on every system.
//!
//! ```no_run
//! // With `platform-codecs`, on Windows or macOS:
//! let file = manguebyte_formats::read_file("song.m4a")?;
//! assert_eq!(file.format, manguebyte_formats::FileFormat::Mp4);
//! # Ok::<(), manguebyte_formats::FormatError>(())
//! ```
//!
//! # What comes out
//!
//! - Samples are 32-bit floats, as the system's decoder gives them, at the
//!   file's own rate and number of channels; nothing is resampled. Channels
//!   come in the order the system gives them, which for these formats is
//!   the WAV order.
//! - The length is the one the file states, as the system reads it. Where
//!   the decoder gives more, the rest is left out; where it gives less, the
//!   audio ends early. ADTS states none, and its length is known at its
//!   end.
//! - The encoder's delay is left out where the file states it: macOS does
//!   this itself; on Windows, Media Foundation does not apply the edit
//!   list of an MPEG-4 file, so this crate reads it (ISO/IEC 14496-12) and
//!   leaves out the priming it states, 1024 frames for FFmpeg's AAC
//!   encoder. ADTS states no delay: its decode starts with the priming.
//! - A seek starts the system's decoder a little before the frame asked
//!   for and decodes its way there, so that what follows matches what a
//!   decode from the start gives. Measured on Windows 11: ALAC matches it
//!   bit for bit; AAC in MPEG-4 within 1.6e-4, and within 4.2e-2 where the
//!   encoder used noise substitution (PNS), whose random noise a seek
//!   starts afresh. In ADTS and WMA, where Media Foundation's seeks do not
//!   land where its time stamps say, a seek decodes on from where the
//!   reader stands, or from the start, and matches bit for bit.
//! - The tags these files carry are not read: [`AudioReader::metadata`](crate::AudioReader::metadata)
//!   is empty for them.
//!
//! # Threads
//!
//! A reader may be moved to another thread, as a
//! [`StreamingPlayer`](crate::StreamingPlayer) does. On Windows each
//! thread that uses one is joined to COM's multithreaded apartment, if it
//! is in no apartment yet, for as long as the thread lives.

use std::fmt;
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::{FileFormat, FormatError};

#[cfg(all(feature = "platform-codecs", windows))]
#[allow(unsafe_code)]
mod windows;
#[cfg(all(feature = "platform-codecs", windows))]
use windows as system;
#[cfg(all(feature = "platform-codecs", windows))]
mod mp4;

#[cfg(all(feature = "platform-codecs", target_os = "macos"))]
#[allow(unsafe_code)]
mod macos;
#[cfg(all(feature = "platform-codecs", target_os = "macos"))]
use macos as system;

/// A call into the system's codecs that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformError {
    /// What was being done.
    pub what: &'static str,
    /// The code the system gave: an `HRESULT` on Windows, an `OSStatus` on
    /// macOS.
    pub code: i32,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the system's codec failed to {} (code 0x{:08X})",
            self.what, self.code as u32
        )
    }
}

impl std::error::Error for PlatformError {}

/// Whether this build decodes the formats of this module.
pub(crate) const AVAILABLE: bool = cfg!(all(
    feature = "platform-codecs",
    any(windows, target_os = "macos")
));

/// What the formats of this module are refused with where they cannot be
/// decoded.
pub(crate) fn unavailable() -> FormatError {
    FormatError::Unsupported(if cfg!(feature = "platform-codecs") {
        "this system has no codec layer for AAC, ALAC, WMA or CAF"
    } else {
        "AAC, ALAC, WMA and CAF need the `platform-codecs` feature"
    })
}

/// Decodes a file through the system's codecs.
pub(crate) struct PlatformReader {
    #[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
    inner: system::Reader,
    #[cfg(not(all(feature = "platform-codecs", any(windows, target_os = "macos"))))]
    _never: std::convert::Infallible,
}

impl PlatformReader {
    /// Opens the file at `path`, which `format` says it is.
    pub(crate) fn open_path(path: &Path, format: FileFormat) -> Result<Self, FormatError> {
        #[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
        {
            system::Reader::open_path(path, format).map(|inner| Self { inner })
        }
        #[cfg(not(all(feature = "platform-codecs", any(windows, target_os = "macos"))))]
        {
            let _ = (path, format);
            Err(unavailable())
        }
    }

    /// Opens a file held in memory.
    pub(crate) fn open_bytes(bytes: Vec<u8>, format: FileFormat) -> Result<Self, FormatError> {
        #[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
        {
            system::Reader::open_bytes(bytes, format).map(|inner| Self { inner })
        }
        #[cfg(not(all(feature = "platform-codecs", any(windows, target_os = "macos"))))]
        {
            let _ = (bytes, format);
            Err(unavailable())
        }
    }
}

#[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
impl PlatformReader {
    pub(crate) fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.inner.num_channels()
    }

    pub(crate) fn frames(&self) -> Option<u64> {
        self.inner.frames()
    }

    pub(crate) fn position(&self) -> u64 {
        self.inner.position()
    }

    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, FormatError> {
        self.inner.read(out)
    }

    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, FormatError> {
        self.inner.seek(frame)
    }
}

#[cfg(not(all(feature = "platform-codecs", any(windows, target_os = "macos"))))]
impl PlatformReader {
    pub(crate) fn sample_rate(&self) -> u32 {
        match self._never {}
    }

    pub(crate) fn num_channels(&self) -> usize {
        match self._never {}
    }

    pub(crate) fn frames(&self) -> Option<u64> {
        match self._never {}
    }

    pub(crate) fn position(&self) -> u64 {
        match self._never {}
    }

    pub(crate) fn read(&mut self, _: &mut AudioBuffer<f32>) -> Result<usize, FormatError> {
        match self._never {}
    }

    pub(crate) fn seek(&mut self, _: u64) -> Result<u64, FormatError> {
        match self._never {}
    }
}

/// Decodes the whole of what `reader` holds.
pub(crate) fn read_all(reader: &mut PlatformReader) -> Result<AudioBuffer<f32>, FormatError> {
    let channels = reader.num_channels();
    // The stated length is trusted only to size the first allocation.
    let expected = reader.frames().unwrap_or(0).min(1 << 24) as usize;
    let mut samples: Vec<Vec<f32>> = vec![Vec::with_capacity(expected); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 8_192);
    loop {
        let got = reader.read(&mut block)?;
        if got == 0 {
            break;
        }
        for (out, channel) in samples.iter_mut().zip(block.iter_channels()) {
            out.extend_from_slice(&channel[..got]);
        }
    }
    let frames = samples.first().map_or(0, Vec::len);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (out, channel) in buffer.iter_channels_mut().zip(&samples) {
        out.copy_from_slice(channel);
    }
    Ok(buffer)
}

/// Where a reader that has decoded `pending` frames from frame `first` on
/// stands, shared by the systems: the frames decoded and not yet handed
/// out, interleaved.
#[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
#[derive(Debug, Default)]
pub(crate) struct Pending {
    /// Interleaved samples.
    pub(crate) samples: Vec<f32>,
    /// The frame `samples` starts at.
    pub(crate) first: u64,
    /// Frames of `samples` already handed out.
    pub(crate) taken: usize,
}

#[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
impl Pending {
    /// Frames held and not handed out.
    pub(crate) fn left(&self, channels: usize) -> usize {
        self.samples.len() / channels - self.taken
    }

    /// The frame the next one handed out is.
    pub(crate) fn next_frame(&self) -> u64 {
        self.first + self.taken as u64
    }

    /// Copies up to `count` frames into `out` from frame `at` of `out` on,
    /// and returns how many.
    pub(crate) fn take(
        &mut self,
        out: &mut AudioBuffer<f32>,
        at: usize,
        count: usize,
        channels: usize,
    ) -> usize {
        let count = count.min(self.left(channels));
        for (channel, samples) in out.iter_channels_mut().enumerate() {
            for (index, sample) in samples[at..at + count].iter_mut().enumerate() {
                *sample = self.samples[(self.taken + index) * channels + channel];
            }
        }
        self.taken += count;
        count
    }

    /// Drops the frames before `frame`, if they are held.
    pub(crate) fn skip_to(&mut self, frame: u64, channels: usize) {
        let held = (self.samples.len() / channels) as u64;
        let skip = frame.saturating_sub(self.first).min(held);
        self.taken = self.taken.max(skip as usize);
    }

    pub(crate) fn clear(&mut self, first: u64) {
        self.samples.clear();
        self.first = first;
        self.taken = 0;
    }
}
