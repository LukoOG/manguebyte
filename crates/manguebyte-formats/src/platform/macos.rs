//! AudioToolbox's Extended Audio File Services (`ExtAudioFile`), which
//! read a file through Core Audio's container parsers and decoders and
//! hand out its audio, here as interleaved 32-bit floats at the file's own
//! rate.
//!
//! Declared from Apple's documentation of `ExtendedAudioFile.h`,
//! `AudioFile.h`, `CoreAudioTypes.h` and `CFURL.h`. `ExtAudioFileSeek` is
//! exact to the frame: it handles the decoder's priming itself.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use manguebyte_core::AudioBuffer;

use super::{Pending, PlatformError};
use crate::{FileFormat, FormatError};

type OsStatus = i32;
type ExtAudioFileRef = *mut c_void;
type AudioFileId = *mut c_void;
type CfUrlRef = *const c_void;

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const FILE_DATA_FORMAT: u32 = four_cc(b"ffmt");
const CLIENT_DATA_FORMAT: u32 = four_cc(b"cfmt");
const FILE_LENGTH_FRAMES: u32 = four_cc(b"#frm");
const LINEAR_PCM: u32 = four_cc(b"lpcm");
const FORMAT_IS_FLOAT: u32 = 1 << 0;
const FORMAT_IS_PACKED: u32 = 1 << 3;
const M4A_FILE: u32 = four_cc(b"m4af");
const ADTS_FILE: u32 = four_cc(b"adts");
const CAF_FILE: u32 = four_cc(b"caff");

/// Frames asked of the decoder at a time.
const BLOCK: usize = 4_096;

/// `AudioStreamBasicDescription`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct StreamDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

/// `AudioBuffer`.
#[repr(C)]
struct RawBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

/// `AudioBufferList` of one buffer.
#[repr(C)]
struct BufferList {
    number_buffers: u32,
    buffers: [RawBuffer; 1],
}

type ReadProc = unsafe extern "C" fn(*mut c_void, i64, u32, *mut c_void, *mut u32) -> OsStatus;
type WriteProc = unsafe extern "C" fn(*mut c_void, i64, u32, *const c_void, *mut u32) -> OsStatus;
type GetSizeProc = unsafe extern "C" fn(*mut c_void) -> i64;
type SetSizeProc = unsafe extern "C" fn(*mut c_void, i64) -> OsStatus;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn ExtAudioFileOpenURL(url: CfUrlRef, file: *mut ExtAudioFileRef) -> OsStatus;
    fn ExtAudioFileWrapAudioFileID(
        file: AudioFileId,
        for_writing: u8,
        out: *mut ExtAudioFileRef,
    ) -> OsStatus;
    fn ExtAudioFileDispose(file: ExtAudioFileRef) -> OsStatus;
    fn ExtAudioFileGetProperty(
        file: ExtAudioFileRef,
        id: u32,
        size: *mut u32,
        data: *mut c_void,
    ) -> OsStatus;
    fn ExtAudioFileSetProperty(
        file: ExtAudioFileRef,
        id: u32,
        size: u32,
        data: *const c_void,
    ) -> OsStatus;
    fn ExtAudioFileRead(file: ExtAudioFileRef, frames: *mut u32, data: *mut BufferList)
    -> OsStatus;
    fn ExtAudioFileSeek(file: ExtAudioFileRef, frame: i64) -> OsStatus;
    fn AudioFileOpenWithCallbacks(
        client: *mut c_void,
        read: ReadProc,
        write: Option<WriteProc>,
        get_size: GetSizeProc,
        set_size: Option<SetSizeProc>,
        type_hint: u32,
        file: *mut AudioFileId,
    ) -> OsStatus;
    fn AudioFileClose(file: AudioFileId) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> CfUrlRef;
    fn CFRelease(object: *const c_void);
}

fn check(status: OsStatus, what: &'static str) -> Result<(), FormatError> {
    if status == 0 {
        Ok(())
    } else {
        Err(FormatError::Platform(PlatformError { what, code: status }))
    }
}

/// Reads for Core Audio from a file held in memory.
///
/// # Safety
///
/// `client` is the `Vec<u8>` the reader keeps in a box for as long as the
/// file is open; `buffer` has room for `request` bytes.
unsafe extern "C" fn read_memory(
    client: *mut c_void,
    position: i64,
    request: u32,
    buffer: *mut c_void,
    actual: *mut u32,
) -> OsStatus {
    // SAFETY: as the caller promises.
    let bytes = unsafe { &*client.cast::<Vec<u8>>() };
    let start = usize::try_from(position)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let count = (request as usize).min(bytes.len() - start);
    // SAFETY: `count` bytes from within `bytes` into a buffer of at least
    // that many, and the count into the place Core Audio gave.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr().add(start), buffer.cast::<u8>(), count);
        *actual = count as u32;
    }
    0
}

/// The length for Core Audio of a file held in memory.
///
/// # Safety
///
/// As for [`read_memory`].
unsafe extern "C" fn memory_size(client: *mut c_void) -> i64 {
    // SAFETY: as the caller promises.
    let bytes = unsafe { &*client.cast::<Vec<u8>>() };
    bytes.len() as i64
}

/// A reader of one file through `ExtAudioFile`.
pub(crate) struct Reader {
    file: ExtAudioFileRef,
    /// The file opened with callbacks, when it is held in memory.
    audio_file: AudioFileId,
    /// The bytes the callbacks read, at an address that does not move.
    bytes: Option<Box<Vec<u8>>>,
    channels: usize,
    sample_rate: u32,
    frames: Option<u64>,
    pending: Pending,
    position: u64,
    ended: bool,
    lost: bool,
}

// SAFETY: an ExtAudioFile may be used from any thread, one at a time,
// which `&mut self` ensures; the bytes it reads from move with it.
unsafe impl Send for Reader {}

impl Reader {
    pub(crate) fn open_path(path: &Path, format: FileFormat) -> Result<Self, FormatError> {
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: the path's bytes and their length, with the default
        // allocator; the URL is released once the file is open.
        let url = unsafe {
            CFURLCreateFromFileSystemRepresentation(
                ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                0,
            )
        };
        if url.is_null() {
            return Err(FormatError::Unsupported(
                "a path Core Foundation cannot hold",
            ));
        }
        let mut file = ptr::null_mut();
        // SAFETY: a URL, and a place for the file.
        let status = unsafe { ExtAudioFileOpenURL(url, &mut file) };
        // SAFETY: the URL made above, not used again.
        unsafe { CFRelease(url) };
        check(status, "open the file")?;
        Self::start(file, ptr::null_mut(), None, format)
    }

    pub(crate) fn open_bytes(bytes: Vec<u8>, format: FileFormat) -> Result<Self, FormatError> {
        let hint = match format {
            FileFormat::Mp4 => M4A_FILE,
            FileFormat::Aac => ADTS_FILE,
            FileFormat::Caf => CAF_FILE,
            _ => 0,
        };
        let bytes = Box::new(bytes);
        let client = (&*bytes as *const Vec<u8>).cast_mut().cast::<c_void>();
        let mut audio_file = ptr::null_mut();
        // SAFETY: the callbacks read from the boxed bytes, which the reader
        // keeps until after the file is closed; there is no writing.
        let status = unsafe {
            AudioFileOpenWithCallbacks(
                client,
                read_memory,
                None,
                memory_size,
                None,
                hint,
                &mut audio_file,
            )
        };
        check(status, "open the file")?;
        let mut file = ptr::null_mut();
        // SAFETY: the file just opened, for reading, and a place for the
        // extended file.
        let status = unsafe { ExtAudioFileWrapAudioFileID(audio_file, 0, &mut file) };
        if status != 0 {
            // SAFETY: the file opened above, not used again.
            unsafe { AudioFileClose(audio_file) };
            return Err(FormatError::Platform(PlatformError {
                what: "open the file",
                code: status,
            }));
        }
        Self::start(file, audio_file, Some(bytes), format)
    }

    /// Asks for floats at the file's rate, and reads the length.
    fn start(
        file: ExtAudioFileRef,
        audio_file: AudioFileId,
        bytes: Option<Box<Vec<u8>>>,
        format: FileFormat,
    ) -> Result<Self, FormatError> {
        // From here the reader owns the file and closes it when dropped.
        let mut this = Self {
            file,
            audio_file,
            bytes,
            channels: 1,
            sample_rate: 1,
            frames: None,
            pending: Pending::default(),
            position: 0,
            ended: false,
            lost: false,
        };
        let mut format = StreamDescription::default();
        let mut size = size_of::<StreamDescription>() as u32;
        // SAFETY: the property that is a stream description, with one to
        // fill and its size.
        let status = unsafe {
            ExtAudioFileGetProperty(
                file,
                FILE_DATA_FORMAT,
                &mut size,
                (&mut format as *mut StreamDescription).cast(),
            )
        };
        check(status, "read the format")?;
        let channels = format.channels_per_frame;
        let rate = format.sample_rate.round();
        if channels == 0 || channels > 255 || !(1.0..=4_294_967_295.0).contains(&rate) {
            return Err(FormatError::Unsupported(
                "a stream of no channels or no rate",
            ));
        }
        this.channels = channels as usize;
        this.sample_rate = rate as u32;

        let client = StreamDescription {
            sample_rate: format.sample_rate,
            format_id: LINEAR_PCM,
            format_flags: FORMAT_IS_FLOAT | FORMAT_IS_PACKED,
            bytes_per_packet: 4 * channels,
            frames_per_packet: 1,
            bytes_per_frame: 4 * channels,
            channels_per_frame: channels,
            bits_per_channel: 32,
            reserved: 0,
        };
        // SAFETY: the property that is the format handed out, with a
        // stream description and its size.
        let status = unsafe {
            ExtAudioFileSetProperty(
                file,
                CLIENT_DATA_FORMAT,
                size_of::<StreamDescription>() as u32,
                (&client as *const StreamDescription).cast(),
            )
        };
        check(status, "decode to floating point")?;

        let mut length = 0i64;
        let mut size = size_of::<i64>() as u32;
        // SAFETY: the property that is a 64-bit length, with a place for it.
        let status = unsafe {
            ExtAudioFileGetProperty(
                file,
                FILE_LENGTH_FRAMES,
                &mut size,
                (&mut length as *mut i64).cast(),
            )
        };
        // ADTS states no length; what the system gives is a guess from the
        // bitrate.
        if status == 0 && length >= 0 && format != FileFormat::Aac {
            this.frames = Some(length as u64);
        }
        Ok(this)
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.channels
    }

    pub(crate) fn frames(&self) -> Option<u64> {
        self.frames
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    /// Decodes the next block into `pending`, after what it holds. Returns
    /// `false` at the end of the stream.
    fn decode(&mut self) -> Result<bool, FormatError> {
        let channels = self.channels;
        let start = self.pending.samples.len();
        self.pending.samples.resize(start + BLOCK * channels, 0.0);
        let mut list = BufferList {
            number_buffers: 1,
            buffers: [RawBuffer {
                number_channels: channels as u32,
                data_byte_size: (BLOCK * channels * 4) as u32,
                data: self.pending.samples[start..].as_mut_ptr().cast(),
            }],
        };
        let mut count = BLOCK as u32;
        // SAFETY: room for `count` frames of interleaved floats, as the
        // client format says, in the one buffer of the list.
        let status = unsafe { ExtAudioFileRead(self.file, &mut count, &mut list) };
        let got = (count as usize).min(BLOCK);
        self.pending
            .samples
            .truncate(start + if status == 0 { got * channels } else { 0 });
        check(status, "decode a block")?;
        if got == 0 {
            self.ended = true;
            return Ok(false);
        }
        Ok(true)
    }

    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, FormatError> {
        if self.lost {
            self.seek(self.position)?;
        }
        let channels = self.channels;
        let want = out.num_frames();
        let mut done = 0;
        while done < want {
            let left = self
                .frames
                .map_or(u64::MAX, |frames| frames.saturating_sub(self.position));
            if left == 0 {
                break;
            }
            if self.pending.left(channels) == 0 {
                if self.ended {
                    break;
                }
                let next = self.pending.next_frame();
                self.pending.clear(next);
                if let Err(error) = self.decode() {
                    self.lost = true;
                    return Err(error);
                }
                continue;
            }
            let count = (want - done).min(usize::try_from(left).unwrap_or(usize::MAX));
            let got = self.pending.take(out, done, count, channels);
            done += got;
            self.position += got as u64;
        }
        Ok(done)
    }

    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, FormatError> {
        let target = self.frames.map_or(frame, |frames| frame.min(frames));
        let held = (self.pending.samples.len() / self.channels) as u64;
        if !self.lost && (self.pending.first..self.pending.first + held).contains(&target) {
            self.pending.taken = (target - self.pending.first) as usize;
            self.position = target;
            return Ok(target);
        }
        let offset = i64::try_from(target).unwrap_or(i64::MAX);
        // SAFETY: the open file, and a frame of it.
        let status = unsafe { ExtAudioFileSeek(self.file, offset) };
        if status != 0 {
            self.lost = true;
            return Err(FormatError::Platform(PlatformError {
                what: "seek",
                code: status,
            }));
        }
        self.pending.clear(target);
        self.ended = false;
        self.lost = false;
        self.position = target;
        Ok(target)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: the files this owns, the extended one first, as it reads
        // through the other; the bytes they read are dropped after.
        unsafe {
            ExtAudioFileDispose(self.file);
            if !self.audio_file.is_null() {
                AudioFileClose(self.audio_file);
            }
        }
        drop(self.bytes.take());
    }
}
