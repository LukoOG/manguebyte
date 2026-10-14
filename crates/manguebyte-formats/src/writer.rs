//! Writing an audio file block by block.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use crate::aiff::{self, AiffError, AiffFormat};
use crate::flac::{FlacOptions, FlacStream};
use crate::pcm;
use crate::wav::{self, WavError, WavFormat};
use crate::{FileFormat, FormatError};

/// Writes an audio file a block at a time.
///
/// Where [`wav::write`], [`aiff::write`] and [`flac::write`](crate::flac::write)
/// need the whole recording in a buffer, this takes it in pieces of any size
/// and keeps none of it beyond one block: the length is not known until
/// [`finish`](Self::finish), which goes back and completes the header. That
/// is why the destination must be able to seek.
///
/// A WAV file may grow past the 4 GiB its 32-bit sizes can state. It is then
/// finished as RF64 (EBU Tech 3306), the same layout with 64-bit sizes,
/// which this crate and most audio software read. AIFF has no such
/// extension: writing past its limit is an error.
///
/// A FLAC stream is compressed a block at a time as the samples arrive; see
/// [`flac`](Self::flac).
///
/// ```
/// use std::io::Cursor;
///
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_formats::wav::{self, WavFormat};
/// use manguebyte_formats::AudioWriter;
///
/// let mut writer = AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Int24)?;
/// let mut block = AudioBuffer::<f32>::new(2, 256);
/// for _ in 0..4 {
///     block.channel_mut(0).fill(0.5);
///     writer.write(&block)?;
/// }
/// assert_eq!(writer.frames_written(), 1_024);
/// let bytes = writer.finish()?.into_inner();
///
/// let file = wav::read(bytes.as_slice()).unwrap();
/// assert_eq!(file.buffer.num_frames(), 1_024);
/// assert_eq!(file.buffer.channel(0)[1_023], 0.5);
/// # Ok::<(), manguebyte_formats::FormatError>(())
/// ```
///
/// # Threads
///
/// Writing does I/O, so it does not belong on the audio thread. To record
/// from an audio callback, hand the writer to a
/// [`RecordingWriter`](crate::RecordingWriter).
///
/// # If writing fails
///
/// A WAV or AIFF writer hands every block to the destination as it comes,
/// in pieces of at most 32 KiB, and keeps nothing back between calls. So
/// when a write fails part of the way, as on a disk that has filled up, the
/// writer asks the destination how far it got, keeps the whole frames that
/// were taken and carries on from there. [`finish`](Self::finish) then
/// completes the header by writing over bytes that are already in the file,
/// which a full disk still allows.
///
/// A FLAC writer works the other way round. It keeps the block it is
/// filling in memory, and a compressed frame either reaches the destination
/// whole or is kept: when writing one fails, the next call, or `finish`,
/// goes back to where that frame began and writes it again whole, and the
/// samples it holds count as written meanwhile. See [`flac`](Self::flac).
///
/// Both rely on a destination without a buffer of its own. One with a
/// buffer, such as a [`BufWriter`](std::io::BufWriter), accepts frames
/// before they are written and cannot give them up again: once the disk
/// refuses them, every further call fails, `finish` included, and the header
/// stays unfinished. That is why [`create_wav`](Self::create_wav),
/// [`create_aiff`](Self::create_aiff) and [`create_flac`](Self::create_flac)
/// write to the [`File`] directly. Put a buffer in front of a file only if
/// saving system calls on very small blocks matters more than the header of
/// a recording that ran out of disk.
///
/// # If `finish` is never called
///
/// The header of a WAV or AIFF file then still holds the sizes it was
/// created with, which no finished file has. This crate takes them to mean
/// "up to the end of the file", whatever the length, so a recording
/// interrupted by a crash or a
/// power cut can be read back with [`AudioReader`](crate::AudioReader) or
/// [`read_file`](crate::read_file) and written out again. FFmpeg 7.1 also
/// decodes both to their end: an unfinished WAV with a warning, an
/// unfinished AIFF with a duration that is wrong, since it is worked out
/// from the stand-in frame count. Other software may refuse such a file.
/// Dropping a writer does not finish it, because an error while doing so
/// could not be reported.
///
/// An unfinished FLAC stream is a valid one that does not state its length,
/// its checksum or its seek points, and holds every frame that was complete.
/// The samples of the last block, which is encoded only when it is full or
/// at `finish`, are not in it.
pub struct AudioWriter<W: Write + Seek> {
    writer: W,
    kind: Kind,
    channels: u16,
    sample_rate: u32,
    /// Where in `writer` the file begins.
    origin: u64,
    /// Where in `writer` the samples begin.
    data_start: u64,
    frames: u64,
    /// Set when a write failed part of the way, so that `frames` no longer
    /// says where the writer is.
    torn: bool,
    /// A whole number of frames, encoded before each write.
    scratch: Vec<u8>,
}

enum Kind {
    Wav {
        format: WavFormat,
        always_rf64: bool,
    },
    Aiff {
        format: AiffFormat,
    },
    // Boxed because an encoder's state is much larger than the others'.
    Flac(Box<FlacStream>),
}

impl Kind {
    /// Bytes each sample takes in the file. FLAC samples take no fixed
    /// number, and none of the code that asks deals with FLAC.
    fn bytes_per_sample(&self) -> usize {
        match self {
            Self::Wav { format, .. } => format.bytes_per_sample(),
            Self::Aiff { format } => format.bytes_per_sample(),
            Self::Flac(_) => 0,
        }
    }
}

impl AudioWriter<File> {
    /// Creates a WAV file at `path`, replacing any existing file.
    ///
    /// The file is written to directly, with no buffer in front of it, so
    /// that a recording which runs out of disk still gets its header: see
    /// [If writing fails](Self#if-writing-fails).
    pub fn create_wav<P: AsRef<Path>>(
        path: P,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
    ) -> Result<Self, FormatError> {
        Self::wav(File::create(path)?, channels, sample_rate, format)
    }

    /// Creates an AIFF file at `path`, replacing any existing file.
    ///
    /// Like [`create_wav`](Self::create_wav), this writes to the file
    /// directly.
    pub fn create_aiff<P: AsRef<Path>>(
        path: P,
        channels: usize,
        sample_rate: u32,
        format: AiffFormat,
    ) -> Result<Self, FormatError> {
        Self::aiff(File::create(path)?, channels, sample_rate, format)
    }

    /// Creates a FLAC file at `path`, replacing any existing file.
    ///
    /// Like [`create_wav`](Self::create_wav), this writes to the file
    /// directly. FLAC is written a frame at a time, a few kilobytes per
    /// system call, so no buffer is needed in front of it.
    ///
    /// The options are checked before the file is created, so options that
    /// are refused leave a file already at `path` as it was. The errors are
    /// those of [`flac`](Self::flac), and any error from creating the file.
    pub fn create_flac<P: AsRef<Path>>(
        path: P,
        channels: usize,
        sample_rate: u32,
        options: &FlacOptions,
    ) -> Result<Self, FormatError> {
        options.check(channels, sample_rate)?;
        Self::flac(File::create(path)?, channels, sample_rate, options)
    }
}

impl<W: Write + Seek> AudioWriter<W> {
    /// Starts a WAV file in `writer`, at its current position.
    ///
    /// The file is finished as plain RIFF/WAVE if it fits in 4 GiB and as
    /// RF64 if it does not. Integer formats clip samples outside
    /// `-1.0..=1.0`.
    pub fn wav(
        writer: W,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
    ) -> Result<Self, FormatError> {
        Self::start_wav(writer, channels, sample_rate, format, false)
    }

    /// Starts a WAV file that is finished as RF64 whatever its length.
    ///
    /// For workflows that require RF64. [`wav`](Self::wav) changes to it on
    /// its own when a file needs it.
    pub fn wav_rf64(
        writer: W,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
    ) -> Result<Self, FormatError> {
        Self::start_wav(writer, channels, sample_rate, format, true)
    }

    fn start_wav(
        writer: W,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
        always_rf64: bool,
    ) -> Result<Self, FormatError> {
        let wav = FormatError::Wav;
        let channels = u16::try_from(channels).map_err(|_| wav(WavError::TooLarge))?;
        if channels == 0 {
            return Err(wav(WavError::Unsupported(
                "a WAV file needs at least one channel",
            )));
        }
        let header = wav::stream_header(channels, sample_rate, format, None, always_rf64);
        let kind = Kind::Wav {
            format,
            always_rf64,
        };
        Self::start(writer, kind, channels, sample_rate, &header)
    }

    /// Starts an AIFF file in `writer`, at its current position.
    ///
    /// 32-bit float is written as AIFF-C. Integer formats clip to full
    /// scale. The file cannot exceed 4 GiB: a [`write`](Self::write) that
    /// would take it past that fails and writes nothing.
    pub fn aiff(
        writer: W,
        channels: usize,
        sample_rate: u32,
        format: AiffFormat,
    ) -> Result<Self, FormatError> {
        let aiff = FormatError::Aiff;
        let channels = u16::try_from(channels).map_err(|_| aiff(AiffError::TooLarge))?;
        if channels == 0 {
            return Err(aiff(AiffError::Unsupported(
                "an AIFF file needs at least one channel",
            )));
        }
        let (header, header_len) = aiff::header(channels, sample_rate, format, None);
        let kind = Kind::Aiff { format };
        Self::start(writer, kind, channels, sample_rate, &header[..header_len])
    }

    /// Starts a FLAC stream in `writer`, at its current position.
    ///
    /// Samples are rounded to the bit depth of `options` with full scale at
    /// `±1.0`, as the decoder maps it, and clipped to it; NaN becomes zero.
    /// They are gathered into blocks, and each block is encoded and written
    /// when it is full; [`finish`](Self::finish) writes the last, shorter
    /// one and goes back to complete the stream information and the seek
    /// table. The bytes are exactly those [`flac::write`](crate::flac::write)
    /// produces for the same audio and options, however the audio is cut
    /// into calls.
    ///
    /// ```
    /// use std::io::Cursor;
    ///
    /// use manguebyte_core::AudioBuffer;
    /// use manguebyte_formats::flac::{self, FlacOptions};
    /// use manguebyte_formats::AudioWriter;
    ///
    /// let options = FlacOptions::new(24).with_level(8);
    /// let mut writer = AudioWriter::flac(Cursor::new(Vec::new()), 2, 96_000, &options)?;
    /// let mut block = AudioBuffer::<f32>::new(2, 512);
    /// for round in 0..20 {
    ///     block.channel_mut(1).fill(round as f32 / 32.0);
    ///     writer.write(&block)?;
    /// }
    /// let bytes = writer.finish()?.into_inner();
    ///
    /// let file = flac::read(bytes.as_slice())?;
    /// assert_eq!(file.buffer.num_frames(), 10_240);
    /// assert_eq!(file.buffer.channel(1)[10_239], 19.0 / 32.0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Memory is allocated here, for the largest block, and not again:
    /// writing takes no more memory however long the stream.
    ///
    /// If writing a frame fails, the frame stays in memory and the
    /// samples of the call that came after it are not taken:
    /// [`frames_written`](Self::frames_written) says how many were, the
    /// frame's own included. The next write, or `finish`, writes the frame
    /// again where it began. If `finish` itself cannot write the last frame,
    /// it still completes the stream information for the frames before it,
    /// and returns the error; the stray bytes of the frame that failed stay
    /// after them.
    ///
    /// A stream holds fewer than 2^31 frames, the most a frame header can
    /// number. Writing a block that would be frame 2^31 fails with
    /// [`FormatError::Io`] of kind
    /// [`InvalidInput`](std::io::ErrorKind::InvalidInput), every time it is
    /// tried, and `finish` still completes the stream before it.
    ///
    /// # Errors
    ///
    /// [`FormatError::Io`] of kind
    /// [`InvalidInput`](std::io::ErrorKind::InvalidInput), with nothing
    /// written, if [`FlacOptions::validate`](crate::flac::FlacOptions::validate)
    /// refuses `channels`, `sample_rate` and `options`: see the
    /// [limits](crate::flac::FlacOptions#limits). Any other
    /// [`FormatError::Io`] is an error from `writer` while finding its
    /// position or writing the metadata, which can be of any kind,
    /// [`InvalidInput`](std::io::ErrorKind::InvalidInput) included.
    pub fn flac(
        mut writer: W,
        channels: usize,
        sample_rate: u32,
        options: &FlacOptions,
    ) -> Result<Self, FormatError> {
        let stream = FlacStream::start(&mut writer, channels, sample_rate, options)?;
        Ok(Self {
            writer,
            kind: Kind::Flac(Box::new(stream)),
            channels: channels as u16,
            sample_rate,
            origin: 0,
            data_start: 0,
            frames: 0,
            torn: false,
            scratch: Vec::new(),
        })
    }

    fn start(
        mut writer: W,
        kind: Kind,
        channels: u16,
        sample_rate: u32,
        header: &[u8],
    ) -> Result<Self, FormatError> {
        let origin = writer.stream_position()?;
        writer.write_all(header)?;
        let frame_bytes = usize::from(channels) * kind.bytes_per_sample();
        Ok(Self {
            writer,
            kind,
            channels,
            sample_rate,
            origin,
            data_start: origin + header.len() as u64,
            frames: 0,
            torn: false,
            scratch: vec![0; pcm::scratch_len(frame_bytes, u64::MAX)],
        })
    }

    /// The format being written.
    pub fn format(&self) -> FileFormat {
        match self.kind {
            Kind::Wav { .. } => FileFormat::Wav,
            Kind::Aiff { .. } => FileFormat::Aiff,
            Kind::Flac(_) => FileFormat::Flac,
        }
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        usize::from(self.channels)
    }

    /// Sample rate in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames written so far.
    ///
    /// For WAV and AIFF, the frames of a write that failed are not counted
    /// until the next call has found out how many of them reached the
    /// destination: see
    /// [If writing fails](Self#if-writing-fails).
    ///
    /// For FLAC this counts the frames that wait in memory for their block
    /// to fill, which [`finish`](Self::finish) writes. After a write that
    /// failed it counts the frames the call took into the block, which stay
    /// in memory and are written again with it: see [`flac`](Self::flac).
    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    fn frame_bytes(&self) -> u64 {
        u64::from(self.channels) * self.kind.bytes_per_sample() as u64
    }

    /// After a write that failed part of the way, goes back to the end of
    /// the last whole frame that reached the writer and counts from there.
    ///
    /// A disk that fills up in the middle of a block has still taken the
    /// frames before that point. They are kept, and what follows them, be it
    /// more audio or the end of the file, is put where the next frame
    /// belongs.
    fn mend(&mut self) -> Result<(), FormatError> {
        // FLAC keeps its own account; `torn` is never set for it.
        if self.torn && !matches!(self.kind, Kind::Flac(_)) {
            let reached = self.writer.stream_position()?;
            let frames = reached.saturating_sub(self.data_start) / self.frame_bytes();
            self.writer.seek(SeekFrom::Start(
                self.data_start + frames * self.frame_bytes(),
            ))?;
            self.frames = frames;
            self.torn = false;
        }
        Ok(())
    }

    /// Checks that `more` further frames fit the format, before any of them
    /// is written.
    fn make_room(&mut self, more: u64) -> Result<u64, FormatError> {
        self.mend()?;
        let frames = self
            .frames
            .checked_add(more)
            .filter(|frames| frames.checked_mul(self.frame_bytes()).is_some());
        match (&self.kind, frames) {
            (Kind::Wav { .. }, Some(frames)) => Ok(frames),
            (Kind::Aiff { .. }, Some(frames))
                if aiff::fits(frames, frames * self.frame_bytes()) =>
            {
                Ok(frames)
            }
            (Kind::Wav { .. }, None) => Err(FormatError::Wav(WavError::TooLarge)),
            (Kind::Aiff { .. }, _) => Err(FormatError::Aiff(AiffError::TooLarge)),
            // FLAC is written by its own stream and never comes here.
            (Kind::Flac(_), _) => Ok(self.frames),
        }
    }

    /// Appends every frame of `buffer`.
    ///
    /// If the destination fails part of the way through, the frames it did
    /// take stay in the file and the rest of the block is lost. The writer
    /// can still be used: the next write, or [`finish`](Self::finish),
    /// carries on after the last whole frame that was taken. This needs a
    /// destination without a buffer of its own: see
    /// [If writing fails](Self#if-writing-fails).
    ///
    /// # Panics
    ///
    /// Panics if `buffer` does not have exactly
    /// [`num_channels`](Self::num_channels) channels.
    pub fn write(&mut self, buffer: &AudioBuffer<f32>) -> Result<(), FormatError> {
        assert_eq!(
            buffer.num_channels(),
            self.num_channels(),
            "the buffer must have as many channels as the file"
        );
        if let Kind::Flac(stream) = &mut self.kind {
            let written = stream.write_planar(&mut self.writer, buffer);
            self.frames = stream.frames();
            return Ok(written?);
        }
        let frames = self.make_room(buffer.num_frames() as u64)?;
        let (writer, scratch) = (&mut self.writer, self.scratch.as_mut_slice());
        let written = match self.kind {
            Kind::Wav { format, .. } => {
                pcm::write_planar(writer, format, format.bytes_per_sample(), buffer, scratch)
            }
            Kind::Aiff { format } => {
                pcm::write_planar(writer, format, format.bytes_per_sample(), buffer, scratch)
            }
            Kind::Flac(_) => Ok(()),
        };
        self.wrote(written, frames)
    }

    /// Takes note of how a write went.
    fn wrote(&mut self, written: std::io::Result<()>, frames: u64) -> Result<(), FormatError> {
        match written {
            Ok(()) => {
                self.frames = frames;
                Ok(())
            }
            Err(error) => {
                self.torn = true;
                Err(error.into())
            }
        }
    }

    /// Appends frames given as interleaved samples (`L R L R ...`), the way
    /// sound cards and queues carry them.
    ///
    /// # Panics
    ///
    /// Panics if the length of `samples` is not a multiple of
    /// [`num_channels`](Self::num_channels).
    pub fn write_interleaved(&mut self, samples: &[f32]) -> Result<(), FormatError> {
        assert!(
            samples.len() % self.num_channels() == 0,
            "interleaved length must be a multiple of the channel count"
        );
        if let Kind::Flac(stream) = &mut self.kind {
            let written = stream.write_interleaved(&mut self.writer, samples);
            self.frames = stream.frames();
            return Ok(written?);
        }
        let frames = self.make_room((samples.len() / self.num_channels()) as u64)?;
        let (writer, scratch) = (&mut self.writer, self.scratch.as_mut_slice());
        let written = match self.kind {
            Kind::Wav { format, .. } => {
                pcm::write_interleaved(writer, format, format.bytes_per_sample(), samples, scratch)
            }
            Kind::Aiff { format } => {
                pcm::write_interleaved(writer, format, format.bytes_per_sample(), samples, scratch)
            }
            Kind::Flac(_) => Ok(()),
        };
        self.wrote(written, frames)
    }

    /// Appends `frames` frames of silence, without the caller having to
    /// hold them in a buffer.
    pub fn write_silence(&mut self, frames: u64) -> Result<(), FormatError> {
        if let Kind::Flac(stream) = &mut self.kind {
            let written = stream.write_silence(&mut self.writer, frames);
            self.frames = stream.frames();
            return Ok(written?);
        }
        let total = self.make_room(frames)?;
        // Silence is zero bytes in every format written here.
        self.scratch.fill(0);
        let chunk = self.scratch.len() as u64;
        let mut left = frames * self.frame_bytes();
        let mut written = Ok(());
        while left > 0 && written.is_ok() {
            let count = left.min(chunk);
            written = self.writer.write_all(&self.scratch[..count as usize]);
            left -= count;
        }
        self.wrote(written, total)
    }

    /// Completes the header, now that the length is known, and gives back
    /// the underlying writer, flushed and positioned at the end of the file.
    ///
    /// The length is that of the frames the destination took, which after a
    /// failed write may be more than [`frames_written`](Self::frames_written)
    /// says.
    ///
    /// The header is completed before anything is added to the file, so it
    /// is still completed on a disk that has no room left. If this returns
    /// an error after a write has failed, look at the file all the same: the
    /// one thing that can then be missing is the padding byte that follows
    /// an odd number of sample bytes.
    pub fn finish(mut self) -> Result<W, FormatError> {
        if let Kind::Flac(stream) = &mut self.kind {
            stream.finish(&mut self.writer)?;
            return Ok(self.writer);
        }
        self.mend()?;
        let data_len = self.frames * self.frame_bytes();
        // Where the samples end, which is where the writer stands.
        let end = self.data_start + data_len;
        self.writer.seek(SeekFrom::Start(self.origin))?;
        match self.kind {
            Kind::Wav {
                format,
                always_rf64,
            } => {
                let header = wav::stream_header(
                    self.channels,
                    self.sample_rate,
                    format,
                    Some(data_len),
                    always_rf64,
                );
                self.writer.write_all(&header)?;
            }
            Kind::Flac(_) => {}
            Kind::Aiff { format } => {
                // `make_room` kept both within 32 bits.
                let sizes = (self.frames as u32, data_len as u32);
                let (header, header_len) =
                    aiff::header(self.channels, self.sample_rate, format, Some(sizes));
                self.writer.write_all(&header[..header_len])?;
            }
        }
        self.writer.seek(SeekFrom::Start(end))?;
        // Chunks are padded to an even length. This is the only byte added
        // here, and so the only one a full disk can refuse.
        if data_len % 2 == 1 {
            self.writer.write_all(&[0])?;
        }
        self.writer.flush()?;
        Ok(self.writer)
    }
}
