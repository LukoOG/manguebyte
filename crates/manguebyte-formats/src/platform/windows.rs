//! Media Foundation: the source reader of `mfreadwrite.dll`, which finds
//! the container and the decoder for a file and hands out its audio, here
//! as 32-bit floats.
//!
//! windows-sys does not declare Media Foundation, which is COM. Its
//! functions are declared here from the Windows SDK's `mfapi.h`, `mfidl.h`
//! and `mfreadwrite.h`, its GUIDs from the same headers, and the methods of
//! its interfaces are called by their place in the tables `mfreadwrite.h`
//! and `mfobjects.h` lay out, which COM keeps fixed for ever.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

use manguebyte_core::AudioBuffer;
use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::Shell::SHCreateMemStream;
use windows_sys::core::{GUID, HRESULT};

use super::{Pending, PlatformError, mp4};
use crate::{FileFormat, FormatError};

/// The version of Media Foundation asked for: that of Windows 7 and later.
const MF_VERSION: u32 = 0x0002_0070;
/// Start Media Foundation without its network sockets.
const MFSTARTUP_LITE: u32 = 1;

const FIRST_AUDIO_STREAM: u32 = 0xFFFF_FFFD;
const ALL_STREAMS: u32 = 0xFFFF_FFFE;
const MEDIA_SOURCE: u32 = 0xFFFF_FFFF;

const READ_ERROR: u32 = 0x1;
const READ_END_OF_STREAM: u32 = 0x2;
const READ_TYPE_CHANGED: u32 = 0x20;

/// `E_FAIL`, for a failure the source reader reports by a flag.
const E_FAIL: HRESULT = 0x8000_4005_u32 as HRESULT;

const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;

const GUID_NULL: GUID = GUID::from_u128(0);
const MF_MT_MAJOR_TYPE: GUID = GUID::from_u128(0x48eb_a18e_f8c9_4687_bf11_0a74_c9f9_6a8f);
const MF_MT_SUBTYPE: GUID = GUID::from_u128(0xf7e3_4c9a_42e8_4714_b74b_cb29_d72c_35e5);
const MF_MT_AUDIO_NUM_CHANNELS: GUID = GUID::from_u128(0x37e4_8bf5_645e_4c5b_89de_ada9_e29b_696a);
const MF_MT_AUDIO_SAMPLES_PER_SECOND: GUID =
    GUID::from_u128(0x5fae_eae7_0290_4c31_9e8a_c534_f68d_9dba);
const MF_PD_DURATION: GUID = GUID::from_u128(0x6c99_0d33_bb8e_477a_8598_0d5d_96fc_d88a);
const MF_SD_AUDIO_ENCODER_DELAY: GUID = GUID::from_u128(0x8e85_422c_73de_403f_9a35_550a_d6e8_b951);
const MF_SD_AUDIO_ENCODER_PADDING: GUID =
    GUID::from_u128(0x529c_7f2c_ac4b_4e3f_bfc3_0902_1949_82cb);
const MF_MEDIA_TYPE_AUDIO: GUID = GUID::from_u128(0x7364_7561_0000_0010_8000_00aa_0038_9b71);
const MF_AUDIO_FORMAT_FLOAT: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
const MF_BYTESTREAM_CONTENT_TYPE: GUID = GUID::from_u128(0xfc35_8289_3cb6_460c_a424_b668_1260_375a);
const IID_IMF_ATTRIBUTES: GUID = GUID::from_u128(0x2cd2_d921_c447_44a7_a13c_4ada_bfc2_47e3);

/// The places of the methods called, in their interfaces' tables. Every
/// interface starts with `IUnknown`: `QueryInterface`, `AddRef`, `Release`.
mod method {
    pub(super) const QUERY_INTERFACE: usize = 0;
    pub(super) const RELEASE: usize = 2;
    // IMFAttributes, which IMFMediaType extends.
    pub(super) const GET_UINT32: usize = 7;
    pub(super) const SET_GUID: usize = 24;
    pub(super) const SET_STRING: usize = 25;
    // IMFSourceReader.
    pub(super) const SET_STREAM_SELECTION: usize = 4;
    pub(super) const GET_CURRENT_MEDIA_TYPE: usize = 6;
    pub(super) const SET_CURRENT_MEDIA_TYPE: usize = 7;
    pub(super) const SET_CURRENT_POSITION: usize = 8;
    pub(super) const READ_SAMPLE: usize = 9;
    pub(super) const GET_PRESENTATION_ATTRIBUTE: usize = 12;
    // IMFSample, after the 33 methods of IMFAttributes.
    pub(super) const CONVERT_TO_CONTIGUOUS_BUFFER: usize = 41;
    // IMFMediaBuffer.
    pub(super) const LOCK: usize = 3;
    pub(super) const UNLOCK: usize = 4;
}

/// `PROPVARIANT`, as far as this module uses it: a type tag and a 64-bit
/// value. The real union is 16 bytes on 64-bit systems and 8 on 32-bit
/// ones, so this is never smaller.
#[repr(C)]
struct PropVariant {
    vt: u16,
    reserved: [u16; 3],
    value: [u64; 2],
}

#[cfg_attr(
    target_arch = "x86",
    link(name = "mfplat", kind = "raw-dylib", import_name_type = "undecorated")
)]
#[cfg_attr(not(target_arch = "x86"), link(name = "mfplat", kind = "raw-dylib"))]
unsafe extern "system" {
    fn MFStartup(version: u32, flags: u32) -> HRESULT;
    fn MFShutdown() -> HRESULT;
    fn MFCreateMediaType(media_type: *mut *mut c_void) -> HRESULT;
    fn MFCreateMFByteStreamOnStream(stream: *mut c_void, byte_stream: *mut *mut c_void) -> HRESULT;
}

#[cfg_attr(
    target_arch = "x86",
    link(
        name = "mfreadwrite",
        kind = "raw-dylib",
        import_name_type = "undecorated"
    )
)]
#[cfg_attr(
    not(target_arch = "x86"),
    link(name = "mfreadwrite", kind = "raw-dylib")
)]
unsafe extern "system" {
    fn MFCreateSourceReaderFromURL(
        url: *const u16,
        attributes: *mut c_void,
        reader: *mut *mut c_void,
    ) -> HRESULT;
    fn MFCreateSourceReaderFromByteStream(
        stream: *mut c_void,
        attributes: *mut c_void,
        reader: *mut *mut c_void,
    ) -> HRESULT;
}

#[cfg_attr(
    target_arch = "x86",
    link(name = "ole32", kind = "raw-dylib", import_name_type = "undecorated")
)]
#[cfg_attr(not(target_arch = "x86"), link(name = "ole32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn PropVariantClear(value: *mut PropVariant) -> HRESULT;
}

/// Fails with the code the system gave, when it is a failure.
fn check(result: HRESULT, what: &'static str) -> Result<(), FormatError> {
    if result < 0 {
        Err(FormatError::Platform(PlatformError { what, code: result }))
    } else {
        Ok(())
    }
}

/// The calling thread's membership of COM's multithreaded apartment, which
/// the thread leaves when it ends.
struct Apartment {
    joined: bool,
}

impl Apartment {
    fn join() -> Self {
        // SAFETY: no argument but the flag, which is a valid one. A thread
        // already in a single-threaded apartment is refused, and stays
        // there: Media Foundation's objects work from either kind.
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_MULTITHREADED as u32) };
        Self {
            joined: result >= 0,
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.joined {
            // SAFETY: balances the CoInitializeEx that succeeded on this
            // thread, at the thread's end.
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static APARTMENT: Apartment = Apartment::join();
}

/// Makes sure the calling thread may call COM.
fn enter_apartment() {
    APARTMENT.with(|_| {});
}

/// An owned reference to a COM object.
struct Com(NonNull<c_void>);

impl Com {
    /// Takes over a reference the system handed out, if it handed one out.
    fn new(pointer: *mut c_void) -> Option<Self> {
        NonNull::new(pointer).map(Self)
    }

    fn as_raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The method at `index` of the object's table.
    ///
    /// # Safety
    ///
    /// `F` must be the type of that method of the object's interface: an
    /// `unsafe extern "system" fn` whose first argument is the object.
    unsafe fn method<F: Copy>(&self, index: usize) -> F {
        assert_eq!(size_of::<F>(), size_of::<*const c_void>());
        // SAFETY: a COM object starts with a pointer to its table of
        // methods, which holds the method at `index` for the interfaces
        // this is called with, as the caller promises; a function pointer
        // is the size of a pointer, which the assertion checks.
        unsafe {
            let table = *self.0.as_ptr().cast::<*const *const c_void>();
            let entry = *table.add(index);
            std::mem::transmute_copy::<*const c_void, F>(&entry)
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;
        // SAFETY: `Release` is the third method of every COM interface,
        // and this owns the reference it gives up.
        unsafe {
            let release: Release = self.method(method::RELEASE);
            release(self.as_raw());
        }
    }
}

/// A reader of one file through Media Foundation.
pub(crate) struct Reader {
    /// The source reader. An `Option` only so that `drop` can release it
    /// before shutting Media Foundation down.
    reader: Option<Com>,
    channels: usize,
    sample_rate: u32,
    frames: Option<u64>,
    /// The frames the encoder put before the audio, which are decoded and
    /// left out. `pending` counts frames as the decoder gives them, these
    /// included; `position` counts the audio.
    skip: u64,
    /// The time stamp of the first block of the stream, in units of 100 ns.
    origin: i64,
    pending: Pending,
    position: u64,
    /// Whether the decoder has given its last block.
    ended: bool,
    /// Set when a failure left the reader somewhere unknown; the next read
    /// finds its place again.
    lost: bool,
    /// Whether the format may still change, before the first block.
    adapt: bool,
    /// Whether the system's seeks land where its time stamps say.
    exact_seeks: bool,
}

// SAFETY: the reader holds Media Foundation objects, which are free
// threaded, and is used by one thread at a time, through `&mut self`. Every
// call into the system first joins the calling thread to COM.
unsafe impl Send for Reader {}

impl Reader {
    pub(crate) fn open_path(path: &Path, format: FileFormat) -> Result<Self, FormatError> {
        enter_apartment();
        let started = Started::new()?;
        let url: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut reader = ptr::null_mut();
        // SAFETY: a NUL-terminated wide string, no attributes, and a place
        // for the reader.
        let result =
            unsafe { MFCreateSourceReaderFromURL(url.as_ptr(), ptr::null_mut(), &mut reader) };
        check(result, "open the file")?;
        let reader = Com::new(reader).ok_or(FormatError::Unsupported("no source reader"))?;
        let mut this = Self::start(reader, started)?;
        let edit = match format {
            FileFormat::Mp4 => std::fs::File::open(path)
                .ok()
                .and_then(|mut file| mp4::sound_edit(&mut file, this.sample_rate)),
            _ => None,
        };
        this.settle(format, edit);
        Ok(this)
    }

    /// Takes in what the file says beside what Media Foundation does: the
    /// encoder's delay and padding that an MPEG-4 file's edit list states,
    /// which Media Foundation does not apply, unless the system said what
    /// they are itself; and that ADTS states no length, which the system
    /// only guesses from the bitrate, as it guesses where to seek.
    fn settle(&mut self, format: FileFormat, edit: Option<mp4::Edit>) {
        if let Some(edit) = edit {
            if self.skip == 0 {
                self.skip = edit.delay;
                self.frames = Some(edit.length);
            }
        }
        if format == FileFormat::Aac {
            self.frames = None;
        }
        // After a seek in WMA, the decoder's first block does not start
        // where its time stamp says (two frames out, measured on Windows
        // 11), so there too the decoder walks or starts again.
        self.exact_seeks = !matches!(format, FileFormat::Aac | FileFormat::Wma);
    }

    pub(crate) fn open_bytes(bytes: Vec<u8>, format: FileFormat) -> Result<Self, FormatError> {
        enter_apartment();
        let started = Started::new()?;
        let len = u32::try_from(bytes.len())
            .map_err(|_| FormatError::Unsupported("a file held in memory past 4 GiB"))?;
        // SAFETY: the bytes and their length; the stream copies them.
        let stream = unsafe { SHCreateMemStream(bytes.as_ptr(), len) };
        let stream = Com::new(stream).ok_or(FormatError::Platform(PlatformError {
            what: "hold the file in memory",
            code: E_FAIL,
        }))?;
        let mut byte_stream = ptr::null_mut();
        // SAFETY: an IStream, and a place for the byte stream.
        let result = unsafe { MFCreateMFByteStreamOnStream(stream.as_raw(), &mut byte_stream) };
        check(result, "read the file from memory")?;
        let byte_stream =
            Com::new(byte_stream).ok_or(FormatError::Unsupported("no byte stream"))?;
        // A stream has no name to tell its kind by: say what it holds.
        set_content_type(&byte_stream, format);
        let mut reader = ptr::null_mut();
        // SAFETY: a byte stream, no attributes, and a place for the reader.
        let result = unsafe {
            MFCreateSourceReaderFromByteStream(byte_stream.as_raw(), ptr::null_mut(), &mut reader)
        };
        check(result, "open the file")?;
        let reader = Com::new(reader).ok_or(FormatError::Unsupported("no source reader"))?;
        let mut this = Self::start(reader, started)?;
        let edit = match format {
            FileFormat::Mp4 => mp4::sound_edit(&mut std::io::Cursor::new(&bytes), this.sample_rate),
            _ => None,
        };
        this.settle(format, edit);
        Ok(this)
    }

    /// Selects the first audio stream, asks for floats, and decodes the
    /// first block.
    fn start(reader: Com, started: Started) -> Result<Self, FormatError> {
        type SetSelection = unsafe extern "system" fn(*mut c_void, u32, i32) -> HRESULT;
        type SetType =
            unsafe extern "system" fn(*mut c_void, u32, *mut u32, *mut c_void) -> HRESULT;
        // SAFETY: the methods of IMFSourceReader at their places, called
        // with the reader, stream numbers the interface defines, and a
        // media type made here.
        unsafe {
            let select: SetSelection = reader.method(method::SET_STREAM_SELECTION);
            check(select(reader.as_raw(), ALL_STREAMS, 0), "select the audio")?;
            check(
                select(reader.as_raw(), FIRST_AUDIO_STREAM, 1),
                "find an audio stream",
            )?;

            let mut wanted = ptr::null_mut();
            check(MFCreateMediaType(&mut wanted), "make a media type")?;
            let wanted = Com::new(wanted).ok_or(FormatError::Unsupported("no media type"))?;
            set_guid(&wanted, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_AUDIO)?;
            set_guid(&wanted, &MF_MT_SUBTYPE, &MF_AUDIO_FORMAT_FLOAT)?;
            let set: SetType = reader.method(method::SET_CURRENT_MEDIA_TYPE);
            check(
                set(
                    reader.as_raw(),
                    FIRST_AUDIO_STREAM,
                    ptr::null_mut(),
                    wanted.as_raw(),
                ),
                "decode to floating point",
            )?;
        }
        let (channels, sample_rate) = current_format(&reader)?;
        let duration = presentation_attribute(&reader, MEDIA_SOURCE, &MF_PD_DURATION, VT_UI8);
        // What the encoder put before and after the audio, where the
        // container says (Windows 10 and later).
        let delay = presentation_attribute(
            &reader,
            FIRST_AUDIO_STREAM,
            &MF_SD_AUDIO_ENCODER_DELAY,
            VT_UI4,
        );
        let padding = presentation_attribute(
            &reader,
            FIRST_AUDIO_STREAM,
            &MF_SD_AUDIO_ENCODER_PADDING,
            VT_UI4,
        );

        let mut this = Self {
            reader: Some(reader),
            channels,
            sample_rate,
            frames: None,
            skip: delay.unwrap_or(0),
            origin: 0,
            pending: Pending::default(),
            position: 0,
            ended: false,
            lost: false,
            adapt: true,
            exact_seeks: true,
        };
        // Media Foundation is shut down when the reader is dropped.
        std::mem::forget(started);
        // The first block may bring the real format with it, as ADTS does:
        // the reader's first guess is taken back before any audio is out.
        match this.decode()? {
            Some(time) => this.origin = time,
            None => this.ended = true,
        }
        this.adapt = false;
        this.pending.first = 0;
        let rate = u128::from(this.sample_rate);
        this.frames = duration.map(|time| {
            let frames = ((u128::from(time) * rate + 5_000_000) / 10_000_000) as u64;
            frames.saturating_sub(this.skip + padding.unwrap_or(0))
        });
        Ok(this)
    }

    fn reader(&self) -> &Com {
        self.reader.as_ref().expect("a reader until dropped")
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

    /// Decodes the next block into `pending`, after what it holds, and
    /// returns its time stamp; `None` at the end of the stream.
    fn decode(&mut self) -> Result<Option<i64>, FormatError> {
        type ReadSample = unsafe extern "system" fn(
            *mut c_void,
            u32,
            u32,
            *mut u32,
            *mut u32,
            *mut i64,
            *mut *mut c_void,
        ) -> HRESULT;
        enter_apartment();
        // A decoder that gives nothing but empty blocks is not trusted for
        // ever.
        for _ in 0..10_000 {
            let (mut flags, mut time, mut sample) = (0u32, 0i64, ptr::null_mut());
            // SAFETY: ReadSample at its place, synchronous, with places for
            // the flags, the time stamp and the sample, which this then
            // owns.
            let result = unsafe {
                let read: ReadSample = self.reader().method(method::READ_SAMPLE);
                read(
                    self.reader().as_raw(),
                    FIRST_AUDIO_STREAM,
                    0,
                    ptr::null_mut(),
                    &mut flags,
                    &mut time,
                    &mut sample,
                )
            };
            let sample = Com::new(sample);
            check(result, "decode a block")?;
            if flags & READ_ERROR != 0 {
                return Err(FormatError::Platform(PlatformError {
                    what: "decode a block",
                    code: E_FAIL,
                }));
            }
            if flags & READ_TYPE_CHANGED != 0 {
                let format = current_format(self.reader())?;
                if format != (self.channels, self.sample_rate) {
                    if !self.adapt {
                        return Err(FormatError::Unsupported(
                            "a stream that changes its rate or channels",
                        ));
                    }
                    (self.channels, self.sample_rate) = format;
                }
            }
            if let Some(sample) = sample {
                self.append(&sample)?;
                if flags & READ_END_OF_STREAM != 0 {
                    self.ended = true;
                }
                return Ok(Some(time));
            }
            if flags & READ_END_OF_STREAM != 0 {
                self.ended = true;
                return Ok(None);
            }
        }
        Err(FormatError::Unsupported("a decoder that gives no audio"))
    }

    /// Adds the samples of a block to `pending`.
    fn append(&mut self, sample: &Com) -> Result<(), FormatError> {
        type Contiguous = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
        type Lock =
            unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32) -> HRESULT;
        type Unlock = unsafe extern "system" fn(*mut c_void) -> HRESULT;
        let mut buffer = ptr::null_mut();
        // SAFETY: ConvertToContiguousBuffer of IMFSample at its place, with
        // a place for the buffer, which this then owns.
        let result = unsafe {
            let contiguous: Contiguous = sample.method(method::CONVERT_TO_CONTIGUOUS_BUFFER);
            contiguous(sample.as_raw(), &mut buffer)
        };
        let buffer = Com::new(buffer);
        check(result, "reach a decoded block")?;
        let Some(buffer) = buffer else {
            return Ok(());
        };
        let (mut data, mut len) = (ptr::null_mut(), 0u32);
        // SAFETY: Lock and Unlock of IMFMediaBuffer at their places. While
        // locked, `data` points at `len` bytes that the buffer owns; they
        // are copied before it is unlocked.
        unsafe {
            let lock: Lock = buffer.method(method::LOCK);
            check(
                lock(buffer.as_raw(), &mut data, ptr::null_mut(), &mut len),
                "lock a block",
            )?;
            if !data.is_null() {
                let bytes = std::slice::from_raw_parts(data, len as usize);
                let whole = bytes.len() / (4 * self.channels) * (4 * self.channels);
                self.pending.samples.extend(
                    bytes[..whole]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
            }
            let unlock: Unlock = buffer.method(method::UNLOCK);
            unlock(buffer.as_raw());
        }
        Ok(())
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
            // The encoder's delay, at the start, is decoded and dropped.
            self.pending.skip_to(self.position + self.skip, channels);
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
        let channels = self.channels;
        let held = (self.pending.samples.len() / channels) as u64;
        // Counted as the decoder counts, the encoder's delay included.
        let wanted = target + self.skip;
        if !self.lost && (self.pending.first..self.pending.first + held).contains(&wanted) {
            self.pending.taken = (wanted - self.pending.first) as usize;
            self.position = target;
            return Ok(target);
        }
        // ADTS has no index: the system seeks in it by the bitrate, and the
        // time stamps it gives after are guesses. There the decoder walks
        // on to the frame, or starts again from the first.
        let landed = if !self.exact_seeks {
            if !self.lost && wanted >= self.pending.next_frame() {
                self.walk_to(wanted)
            } else {
                self.seek_from_before(wanted, u64::MAX)
            }
        } else {
            self.seek_from_before(wanted, 8_192)
        };
        match landed {
            Ok(landed) => {
                let landed = landed.saturating_sub(self.skip);
                self.lost = false;
                self.position = landed;
                Ok(landed)
            }
            Err(error) => {
                self.lost = true;
                Err(error)
            }
        }
    }

    /// Starts the decoder `preroll` frames before frame `target` of what it
    /// decodes, or at the start, and decodes up to it: AAC needs the block
    /// before for its overlap, and a decoder may land a block early or
    /// late. Returns the frame it stands at, counted the same way.
    fn seek_from_before(&mut self, target: u64, preroll: u64) -> Result<u64, FormatError> {
        type SetPosition =
            unsafe extern "system" fn(*mut c_void, *const GUID, *const PropVariant) -> HRESULT;
        let rate = u128::from(self.sample_rate);
        let mut preroll = preroll;
        loop {
            let start = target.saturating_sub(preroll);
            let time = (u128::from(start) * 10_000_000 / rate) as i64 + self.origin;
            let position = PropVariant {
                vt: VT_I8,
                reserved: [0; 3],
                value: [time as u64, 0],
            };
            // SAFETY: SetCurrentPosition at its place, in units of 100 ns,
            // which GUID_NULL names.
            let result = unsafe {
                let set: SetPosition = self.reader().method(method::SET_CURRENT_POSITION);
                set(self.reader().as_raw(), &GUID_NULL, &position)
            };
            check(result, "seek")?;
            self.ended = false;
            self.pending.clear(0);
            let Some(stamp) = self.decode()? else {
                // Nothing after that place: the end.
                return Ok(start);
            };
            let since = i128::from(stamp - self.origin).max(0) as u128;
            let first = ((since * rate + 5_000_000) / 10_000_000) as u64;
            if first > target && start > 0 {
                // Landed past the frame wanted: start further back.
                preroll = preroll.saturating_mul(4);
                continue;
            }
            self.pending.first = if start == 0 { 0 } else { first };
            return self.walk_to(target);
        }
    }

    /// Decodes on to the block that holds frame `target` of what the
    /// decoder gives, and returns the frame it stands at.
    fn walk_to(&mut self, target: u64) -> Result<u64, FormatError> {
        let channels = self.channels;
        loop {
            let held = (self.pending.samples.len() / channels) as u64;
            if self.pending.first + held > target || self.ended {
                break;
            }
            let next = self.pending.first + held;
            self.pending.clear(next);
            self.decode()?;
        }
        self.pending.skip_to(target, channels);
        Ok(self.pending.next_frame().min(target))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        enter_apartment();
        self.reader = None;
        // SAFETY: balances the MFStartup of `open_*`, after the last object
        // was released.
        unsafe { MFShutdown() };
    }
}

/// Media Foundation, started; shut down again on drop unless a reader
/// takes it over.
struct Started;

impl Started {
    fn new() -> Result<Self, FormatError> {
        // SAFETY: the version this was written for, and a valid flag.
        let result = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) };
        check(result, "start Media Foundation")?;
        Ok(Self)
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup of `new`.
        unsafe { MFShutdown() };
    }
}

/// A whole number the reader says of a stream, or of the file with
/// `MEDIA_SOURCE`, if it says one of type `vt`.
fn presentation_attribute(reader: &Com, stream: u32, key: &GUID, vt: u16) -> Option<u64> {
    type GetAttribute =
        unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut PropVariant) -> HRESULT;
    let mut value = PropVariant {
        vt: 0,
        reserved: [0; 3],
        value: [0; 2],
    };
    // SAFETY: GetPresentationAttribute of IMFSourceReader at its place,
    // with a GUID and a PROPVARIANT to fill, which is cleared after.
    unsafe {
        let get: GetAttribute = reader.method(method::GET_PRESENTATION_ATTRIBUTE);
        let result = get(reader.as_raw(), stream, key, &mut value);
        let number = match value.vt {
            VT_UI4 if vt == VT_UI4 => Some(value.value[0] & 0xFFFF_FFFF),
            VT_UI8 if vt == VT_UI8 => Some(value.value[0]),
            _ => None,
        };
        PropVariantClear(&mut value);
        number.filter(|_| result >= 0)
    }
}

/// Sets a GUID of a media type.
fn set_guid(attributes: &Com, key: &GUID, value: &GUID) -> Result<(), FormatError> {
    type SetGuid = unsafe extern "system" fn(*mut c_void, *const GUID, *const GUID) -> HRESULT;
    // SAFETY: SetGUID of IMFAttributes at its place, with two GUIDs.
    let result = unsafe {
        let set: SetGuid = attributes.method(method::SET_GUID);
        set(attributes.as_raw(), key, value)
    };
    check(result, "describe the samples wanted")
}

/// The channels and sample rate the reader decodes the audio stream to.
fn current_format(reader: &Com) -> Result<(usize, u32), FormatError> {
    type GetType = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT;
    type GetU32 = unsafe extern "system" fn(*mut c_void, *const GUID, *mut u32) -> HRESULT;
    let mut current = ptr::null_mut();
    // SAFETY: GetCurrentMediaType of IMFSourceReader and GetUINT32 of
    // IMFAttributes at their places, with places for what they give.
    unsafe {
        let get: GetType = reader.method(method::GET_CURRENT_MEDIA_TYPE);
        check(
            get(reader.as_raw(), FIRST_AUDIO_STREAM, &mut current),
            "read the format",
        )?;
        let current = Com::new(current).ok_or(FormatError::Unsupported("no media type"))?;
        let get: GetU32 = current.method(method::GET_UINT32);
        let (mut channels, mut rate) = (0u32, 0u32);
        check(
            get(current.as_raw(), &MF_MT_AUDIO_NUM_CHANNELS, &mut channels),
            "read the number of channels",
        )?;
        check(
            get(current.as_raw(), &MF_MT_AUDIO_SAMPLES_PER_SECOND, &mut rate),
            "read the sample rate",
        )?;
        if channels == 0 || channels > 255 || rate == 0 {
            return Err(FormatError::Unsupported(
                "a stream of no channels or no rate",
            ));
        }
        Ok((channels as usize, rate))
    }
}

/// Tells a byte stream the MIME type of what it holds, which is how Media
/// Foundation chooses its reader for a stream without a name. A failure is
/// left to the reader, which can still tell many formats by their bytes.
fn set_content_type(byte_stream: &Com, format: FileFormat) {
    type QueryInterface =
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
    type SetString = unsafe extern "system" fn(*mut c_void, *const GUID, *const u16) -> HRESULT;
    let mime = match format {
        FileFormat::Mp4 => "audio/mp4",
        FileFormat::Aac => "audio/aac",
        FileFormat::Wma => "audio/x-ms-wma",
        _ => return,
    };
    let mime: Vec<u16> = mime.encode_utf16().chain([0]).collect();
    let mut attributes = ptr::null_mut();
    // SAFETY: QueryInterface of IUnknown for IMFAttributes, and SetString
    // of IMFAttributes at its place with a NUL-terminated wide string.
    unsafe {
        let query: QueryInterface = byte_stream.method(method::QUERY_INTERFACE);
        if query(byte_stream.as_raw(), &IID_IMF_ATTRIBUTES, &mut attributes) < 0 {
            return;
        }
        let Some(attributes) = Com::new(attributes) else {
            return;
        };
        let set: SetString = attributes.method(method::SET_STRING);
        set(
            attributes.as_raw(),
            &MF_BYTESTREAM_CONTENT_TYPE,
            mime.as_ptr(),
        );
    }
}
