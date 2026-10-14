//! The FLAC encoder: options, stream state, metadata, and the functions
//! that write a whole buffer.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use manguebyte_core::AudioBuffer;

use super::FlacError;
use super::bits::BitWriter;
use super::frame::{FrameEncoder, Settings};
use super::md5::Md5;

/// The vendor string written into every file, naming the encoder.
const VENDOR: &str = concat!("manguebyte-formats ", env!("CARGO_PKG_VERSION"));

/// Seconds between seek points until the table fills.
const SEEK_SPACING_SECONDS: u64 = 10;

/// Bytes in one seek point.
const SEEK_POINT_BYTES: usize = 18;

/// The largest body a metadata block can have: its length has 24 bits.
const MAX_BLOCK_BODY: usize = (1 << 24) - 1;

/// How to write a FLAC stream.
///
/// Made with [`FlacOptions::new`] and adjusted with the methods that follow
/// it, each of which returns the changed options:
///
/// ```
/// use manguebyte_formats::flac::FlacOptions;
///
/// let options = FlacOptions::new(24).with_level(8).with_tag("TITLE", "Maracatu");
/// assert_eq!(options.block_size(), 4_096);
/// ```
///
/// # Limits
///
/// Nothing is checked until the options are used, or until
/// [`validate`](Self::validate) is called. A writer given values outside
/// these limits fails with [`FlacError::Io`] of kind
/// [`InvalidInput`](io::ErrorKind::InvalidInput) before it writes anything:
///
/// - 4 to 32 bits per sample;
/// - compression levels 0 to 8, which are this encoder's, not the format's;
/// - blocks of 16 to 65535 samples;
/// - tag names that are not empty and hold only printable ASCII other than
///   `=`;
/// - metadata blocks of at most 16,777,215 bytes each, the most the format
///   allows: the Vorbis comment, which holds the encoder's name and each
///   tag with 4 bytes of length; the padding; and the seek table, 18 bytes
///   a point, so at most 932,067 points. Seek points that are not used join
///   the padding block, so when both are asked for, the padding, 4 bytes
///   and the seek table must fit one block together;
/// - given to the writer beside the options: 1 to 8 channels, and sample
///   rates of 1 to 1,048,575 Hz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlacOptions {
    bits_per_sample: u32,
    level: u8,
    block_size: Option<u32>,
    padding: u32,
    seek_points: u32,
    tags: Vec<(String, String)>,
}

impl Default for FlacOptions {
    /// 16 bits per sample at level 5.
    fn default() -> Self {
        Self::new(16)
    }
}

impl FlacOptions {
    /// Options for samples of `bits_per_sample` bits, from 4 to 32, at
    /// compression level 5, with 8192 bytes of padding and room for 100
    /// seek points.
    pub fn new(bits_per_sample: u32) -> Self {
        Self {
            bits_per_sample,
            level: 5,
            block_size: None,
            padding: 8192,
            seek_points: 100,
            tags: Vec::new(),
        }
    }

    /// Sets the compression level, from 0, the fastest, to 8, which
    /// searches the most and usually, not always, comes out smallest.
    ///
    /// Every level is lossless and every level decodes at the same speed;
    /// the level only decides how hard the encoder searches. Levels 0 to 2
    /// use fixed predictors in blocks of 1152 samples, levels 3 and up
    /// linear prediction in blocks of 4096, with more predictor orders,
    /// windows, coefficient precisions and residual partitions tried the
    /// higher the level. At levels 2 and 5 to 8, each block of two channels
    /// is coded in all four ways they can be combined and the smallest is
    /// kept; at the others the combination is chosen from an estimate.
    pub fn with_level(mut self, level: u8) -> Self {
        self.level = level;
        self
    }

    /// Sets the number of samples in each block, from 16 to 65535, instead
    /// of the level's choice.
    ///
    /// Streams that some players must be able to start in the middle of,
    /// such as broadcasts, keep to the "streamable subset" of the format
    /// (RFC 9639, section 7), which allows at most 4608 samples a block at
    /// 48 kHz or less and 16384 above. Every level stays within it unless
    /// this is set beyond, or the bit depth is not 8, 12, 16, 20, 24 or 32,
    /// or the sample rate is one a frame header cannot state, such as
    /// 100,001 Hz.
    ///
    /// Very long blocks of audio that does not compress make frames of
    /// hundreds of kilobytes. They are valid, and this crate reads them,
    /// but FFmpeg 7.1 returns nothing from a frame of 216,012 bytes, without
    /// an error, while it reads one of 208,012 (16-bit stereo noise in
    /// blocks of 54,000 and 52,000 samples).
    pub fn with_block_size(mut self, samples: u32) -> Self {
        self.block_size = Some(samples);
        self
    }

    /// Sets the bytes of padding left after the metadata, so that tags can
    /// be added later without rewriting the file. Zero leaves none, apart
    /// from seek points that were reserved and not used.
    ///
    /// The most is 16,777,215 bytes, less room for the seek table: see
    /// [Limits](Self#limits).
    pub fn with_padding(mut self, bytes: u32) -> Self {
        self.padding = bytes;
        self
    }

    /// Sets how many seek points the file may hold. Zero writes no seek
    /// table.
    ///
    /// A point is placed every ten seconds. When a recording runs longer
    /// than the points can cover, every other point is dropped and the
    /// spacing doubles, so the table always spans the whole stream. Points
    /// that are not used become padding. Each point takes 18 bytes, and the
    /// table at most 16,777,215 bytes with the padding: see
    /// [Limits](Self#limits).
    pub fn with_seek_points(mut self, points: u32) -> Self {
        self.seek_points = points;
        self
    }

    /// Adds a tag to the file's Vorbis comment, such as `TITLE` or `ARTIST`.
    ///
    /// The name is printable ASCII without `=` (RFC 9639, section 8.6), and
    /// is compared without regard to case by readers. A name may be given
    /// more than once.
    pub fn with_tag(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.push((name.into(), value.into()));
        self
    }

    /// Bits per sample.
    pub fn bits_per_sample(&self) -> u32 {
        self.bits_per_sample
    }

    /// The compression level.
    pub fn level(&self) -> u8 {
        self.level
    }

    /// Bytes of padding asked for.
    pub fn padding(&self) -> u32 {
        self.padding
    }

    /// Seek points the file may hold.
    pub fn seek_points(&self) -> u32 {
        self.seek_points
    }

    /// The tags, in the order they were added.
    pub fn tags(&self) -> &[(String, String)] {
        &self.tags
    }

    /// Samples in each block: the one set, or the level's choice.
    pub fn block_size(&self) -> u32 {
        self.block_size
            .unwrap_or_else(|| Settings::level(self.level.min(8)).block_size)
    }

    /// Checks, without writing anything, that these options can write a
    /// stream of `channels` channels at `sample_rate` hertz.
    ///
    /// Every writer makes the same checks before it writes a byte, and
    /// fails with the same error. Checking first tells a mistake in the
    /// options apart from a destination that fails, since a destination can
    /// report [`InvalidInput`](io::ErrorKind::InvalidInput) too.
    ///
    /// ```
    /// use manguebyte_formats::flac::FlacOptions;
    ///
    /// let options = FlacOptions::new(24).with_level(8);
    /// assert!(options.validate(2, 96_000).is_ok());
    /// assert!(options.validate(9, 96_000).is_err());
    /// assert!(options.clone().with_level(9).validate(2, 96_000).is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// [`FlacError::Io`] of kind [`InvalidInput`](io::ErrorKind::InvalidInput)
    /// whose message names the first of the [limits](Self#limits) broken.
    pub fn validate(&self, channels: usize, sample_rate: u32) -> Result<(), FlacError> {
        self.check(channels, sample_rate)?;
        Ok(())
    }

    /// The checks behind [`validate`](Self::validate), with the error the
    /// writers return.
    pub(crate) fn check(&self, channels: usize, sample_rate: u32) -> io::Result<()> {
        if !(1..=8).contains(&channels) {
            return Err(invalid("FLAC holds 1 to 8 channels"));
        }
        if !(1..1 << 20).contains(&sample_rate) {
            return Err(invalid("FLAC holds sample rates of 1 to 1048575 Hz"));
        }
        if !(4..=32).contains(&self.bits_per_sample) {
            return Err(invalid("FLAC holds 4 to 32 bits per sample here"));
        }
        if self.level > 8 {
            return Err(invalid("this encoder's compression levels go from 0 to 8"));
        }
        if !(16..=65_535).contains(&self.block_size()) {
            return Err(invalid("FLAC blocks hold 16 to 65535 samples"));
        }
        comment_len(&self.tags)?;
        // A metadata block's body has a 24-bit length (RFC 9639, section
        // 8). Seek points that are not used become padding, so when both
        // are asked for, one padding block may have to hold the padding,
        // the whole table and the header of the seek table it replaces.
        let table = u64::from(self.seek_points) * SEEK_POINT_BYTES as u64;
        let padding = u64::from(self.padding);
        let largest = match (table, padding) {
            (0, _) => padding,
            (_, 0) => table,
            _ => padding + 4 + table,
        };
        if largest > MAX_BLOCK_BODY as u64 {
            return Err(invalid("the padding and seek table are too large"));
        }
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Checks the tag names and returns the length of the Vorbis comment block
/// the tags make, without building it.
fn comment_len(tags: &[(String, String)]) -> io::Result<usize> {
    let valid = |byte: u8| (0x20..=0x7E).contains(&byte) && byte != b'=';
    // The vendor string after its 32-bit length, then the 32-bit count of
    // tags.
    let mut len = 4 + VENDOR.len() + 4;
    for (name, value) in tags {
        if name.is_empty() || !name.bytes().all(valid) {
            return Err(invalid(
                "a tag name must be printable ASCII without '=' and not empty",
            ));
        }
        len = len
            .saturating_add(4 + name.len() + 1)
            .saturating_add(value.len());
        if len > MAX_BLOCK_BODY {
            return Err(invalid("the tags are too long for a FLAC file"));
        }
    }
    Ok(len)
}

/// Builds the body of a Vorbis comment block (RFC 9639, section 8.6), from
/// tags that [`comment_len`] accepted. Its lengths are little-endian, unlike
/// everything else in FLAC.
fn vorbis_comment(tags: &[(String, String)], len: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(len);
    body.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    body.extend_from_slice(VENDOR.as_bytes());
    body.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (name, value) in tags {
        // No longer than the block, which `comment_len` checked.
        let len = (name.len() + 1 + value.len()) as u32;
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.push(b'=');
        body.extend_from_slice(value.as_bytes());
    }
    debug_assert_eq!(body.len(), len);
    body
}

/// A frame the seek table points at.
#[derive(Debug, Clone, Copy)]
struct SeekPoint {
    sample: u64,
    offset: u64,
    samples: u16,
}

/// Keeps a seek table of fixed capacity spread over a stream of unknown
/// length.
struct SeekTable {
    points: Vec<SeekPoint>,
    capacity: usize,
    spacing: u64,
    /// The first sample at which the next point is due.
    next: u64,
}

impl SeekTable {
    fn new(capacity: usize, sample_rate: u32) -> Self {
        Self {
            points: Vec::with_capacity(capacity),
            capacity,
            spacing: (u64::from(sample_rate) * SEEK_SPACING_SECONDS).max(1),
            next: 0,
        }
    }

    /// Takes note of a frame that has been written, and points at it if a
    /// point is due.
    fn note(&mut self, point: SeekPoint) {
        if self.capacity == 0 || point.sample < self.next {
            return;
        }
        if self.points.len() == self.capacity {
            if self.capacity == 1 {
                // The one point stays at the start.
                self.next = u64::MAX;
                return;
            }
            // Halve the table: keep the points on the grid of twice the
            // spacing, which are the even ones.
            let mut kept = 0;
            for index in (0..self.points.len()).step_by(2) {
                self.points[kept] = self.points[index];
                kept += 1;
            }
            self.points.truncate(kept);
            self.spacing = self.spacing.saturating_mul(2);
            let last = self.points.last().map_or(0, |point| point.sample);
            self.next = (last / self.spacing + 1).saturating_mul(self.spacing);
            if point.sample < self.next {
                return;
            }
        }
        self.points.push(point);
        self.next = (point.sample / self.spacing + 1).saturating_mul(self.spacing);
    }
}

/// Everything about a stream being encoded except where its bytes go.
///
/// Samples are taken into a block; when the block is full the caller has it
/// encoded, writes the frame, and commits it, which is when the frame
/// counts: its samples enter the checksum, the seek table and the totals.
/// A frame that could not be written stays in the block to be tried again.
///
/// All storage is allocated here, so taking samples and encoding frames
/// never touch the heap.
pub(crate) struct Encoder {
    channels: usize,
    sample_rate: u32,
    bits: u32,
    block_size: usize,
    /// Integer full scale, for converting `f32` samples.
    scale: f64,
    lowest: f64,
    highest: f64,
    frames: FrameEncoder,
    block: Vec<Vec<i32>>,
    filled: usize,
    frame: BitWriter,
    /// Whether `frame` holds the current block, encoded.
    encoded: bool,
    frame_number: u64,
    /// Samples in each channel of the frames committed.
    committed: u64,
    /// Bytes of the frames committed.
    audio_bytes: u64,
    min_frame: u32,
    max_frame: u32,
    md5: Md5,
    seek_table: SeekTable,
    comment: Vec<u8>,
    padding: usize,
}

impl Encoder {
    /// Checks the options and gets ready to encode.
    pub(crate) fn new(
        channels: usize,
        sample_rate: u32,
        options: &FlacOptions,
    ) -> io::Result<Self> {
        options.check(channels, sample_rate)?;
        let bits = options.bits_per_sample;
        let block_size = options.block_size() as usize;
        let comment = vorbis_comment(&options.tags, comment_len(&options.tags)?);
        let seek_points = options.seek_points as usize;
        let padding = options.padding as usize;

        let settings = Settings::level(options.level);
        let full = (1u64 << (bits - 1)) as f64;
        Ok(Self {
            channels,
            sample_rate,
            bits,
            block_size,
            scale: full,
            lowest: -full,
            highest: full - 1.0,
            frames: FrameEncoder::new(settings, channels, bits, sample_rate, block_size),
            block: vec![vec![0; block_size]; channels],
            filled: 0,
            frame: BitWriter::with_capacity(FrameEncoder::max_frame_bytes(
                channels, bits, block_size,
            )),
            encoded: false,
            frame_number: 0,
            committed: 0,
            audio_bytes: 0,
            min_frame: 0,
            max_frame: 0,
            md5: Md5::new(),
            seek_table: SeekTable::new(seek_points, sample_rate),
            comment,
            padding,
        })
    }

    /// Samples per channel taken so far, written or waiting in the block.
    pub(crate) fn samples(&self) -> u64 {
        self.committed + self.filled as u64
    }

    /// Bytes of the frames committed so far.
    pub(crate) fn audio_bytes(&self) -> u64 {
        self.audio_bytes
    }

    pub(crate) fn block_is_full(&self) -> bool {
        self.filled == self.block_size
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.filled > 0
    }

    fn to_integer(&self, sample: f32) -> i32 {
        // NaN survives the clamp and becomes 0 in the conversion.
        (f64::from(sample) * self.scale)
            .round()
            .clamp(self.lowest, self.highest) as i32
    }

    /// Takes frames of `buffer` from frame `from` on, as many as the block
    /// has room for, and returns how many it took.
    pub(crate) fn take_planar(&mut self, buffer: &AudioBuffer<f32>, from: usize) -> usize {
        let count = (buffer.num_frames() - from).min(self.block_size - self.filled);
        for channel in 0..self.channels {
            let source = &buffer.channel(channel)[from..from + count];
            for (index, &sample) in source.iter().enumerate() {
                let value = self.to_integer(sample);
                self.block[channel][self.filled + index] = value;
            }
        }
        self.filled += count;
        self.encoded = false;
        count
    }

    /// Takes whole frames of interleaved `samples`, as many as the block has
    /// room for, and returns how many frames it took.
    pub(crate) fn take_interleaved(&mut self, samples: &[f32]) -> usize {
        let count = (samples.len() / self.channels).min(self.block_size - self.filled);
        for (index, frame) in samples.chunks_exact(self.channels).take(count).enumerate() {
            for (channel, &sample) in frame.iter().enumerate() {
                let value = self.to_integer(sample);
                self.block[channel][self.filled + index] = value;
            }
        }
        self.filled += count;
        self.encoded = false;
        count
    }

    /// Takes samples given as integers, which the caller has checked fit the
    /// bit depth, from index `from` on.
    fn take_integers(&mut self, channels: &[&[i32]], from: usize) -> usize {
        let len = channels.first().map_or(0, |samples| samples.len());
        let count = (len - from).min(self.block_size - self.filled);
        for (block, samples) in self.block.iter_mut().zip(channels) {
            block[self.filled..self.filled + count].copy_from_slice(&samples[from..from + count]);
        }
        self.filled += count;
        self.encoded = false;
        count
    }

    /// Takes up to `frames` frames of silence and returns how many it took.
    pub(crate) fn take_silence(&mut self, frames: u64) -> usize {
        let count = usize::try_from(frames)
            .unwrap_or(usize::MAX)
            .min(self.block_size - self.filled);
        for block in &mut self.block {
            block[self.filled..self.filled + count].fill(0);
        }
        self.filled += count;
        self.encoded = false;
        count
    }

    /// Encodes the samples in the block as the next frame and returns its
    /// bytes. Encoding again without taking more samples gives the same
    /// bytes without redoing the work.
    pub(crate) fn encode(&mut self) -> io::Result<&[u8]> {
        if !self.encoded {
            // Frames of a stream with blocks of fixed size are numbered in
            // 31 bits (RFC 9639, section 9.1.5).
            if self.frame_number >= 1 << 31 {
                return Err(invalid("the stream has more frames than FLAC can number"));
            }
            self.frames
                .encode(&self.block, self.filled, self.frame_number, &mut self.frame);
            self.encoded = true;
        }
        Ok(self.frame.bytes())
    }

    /// Records that the frame [`encode`](Self::encode) gave has been
    /// written, and empties the block.
    pub(crate) fn commit(&mut self) {
        debug_assert!(self.encoded);
        let len = self.frame.bytes().len() as u32;
        self.seek_table.note(SeekPoint {
            sample: self.committed,
            offset: self.audio_bytes,
            samples: self.filled as u16,
        });
        self.min_frame = if self.frame_number == 0 {
            len
        } else {
            self.min_frame.min(len)
        };
        self.max_frame = self.max_frame.max(len);
        self.add_to_checksum();
        self.frame_number += 1;
        self.committed += self.filled as u64;
        self.audio_bytes += u64::from(len);
        self.filled = 0;
        self.encoded = false;
    }

    /// Adds the samples of the block to the MD5 digest: interleaved, each
    /// in as many little-endian bytes as the bit depth needs, sign extended
    /// (RFC 9639, section 8.2).
    fn add_to_checksum(&mut self) {
        let width = self.bits.div_ceil(8) as usize;
        let mut bytes = [0u8; 4096];
        let mut used = 0;
        for index in 0..self.filled {
            for channel in &self.block {
                bytes[used..used + width].copy_from_slice(&channel[index].to_le_bytes()[..width]);
                used += width;
                if used + 4 > bytes.len() {
                    self.md5.update(&bytes[..used]);
                    used = 0;
                }
            }
        }
        self.md5.update(&bytes[..used]);
    }

    /// The length of everything before the first frame. It does not change
    /// between the start of a stream and its end.
    pub(crate) fn metadata_len(&self) -> usize {
        let table = self.seek_table.capacity;
        4 + (4 + 34)
            + (4 + self.comment.len())
            + if table > 0 {
                4 + table * SEEK_POINT_BYTES
            } else {
                0
            }
            + if self.padding > 0 {
                4 + self.padding
            } else {
                0
            }
    }

    /// Everything before the first frame: the marker and the metadata
    /// blocks.
    ///
    /// Before the stream is `finished`, the totals, the frame sizes and the
    /// checksum are written as unknown and every seek point as a
    /// placeholder, which is a valid stream that decoders read to its end.
    pub(crate) fn metadata(&self, finished: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.metadata_len());
        out.extend_from_slice(b"fLaC");

        let table = self.seek_table.capacity;
        let reserved = if table > 0 {
            4 + table * SEEK_POINT_BYTES
        } else {
            0
        } + if self.padding > 0 {
            4 + self.padding
        } else {
            0
        };
        let points = if finished {
            self.seek_table.points.len()
        } else {
            0
        };
        // What the seek table takes when it is written now.
        let seek_block = if !finished && table > 0 {
            4 + table * SEEK_POINT_BYTES
        } else if points > 0 {
            4 + points * SEEK_POINT_BYTES
        } else {
            0
        };
        let padding_block = reserved - seek_block;

        // Stream information (RFC 9639, section 8.2).
        block_header(&mut out, 0, false, 34);
        let block = self.block_size as u16;
        out.extend_from_slice(&block.to_be_bytes());
        out.extend_from_slice(&block.to_be_bytes());
        let (min_frame, max_frame) = if finished {
            (self.min_frame, self.max_frame)
        } else {
            (0, 0)
        };
        out.extend_from_slice(&min_frame.to_be_bytes()[1..]);
        out.extend_from_slice(&max_frame.to_be_bytes()[1..]);
        // Zero means "unknown", which is also what a total too large for its
        // 36 bits has to be written as.
        let total = if finished && self.committed < 1 << 36 {
            self.committed
        } else {
            0
        };
        let packed = (u64::from(self.sample_rate) << 44)
            | ((self.channels as u64 - 1) << 41)
            | (u64::from(self.bits - 1) << 36)
            | total;
        out.extend_from_slice(&packed.to_be_bytes());
        let digest = if finished {
            self.md5.clone().finish()
        } else {
            [0; 16]
        };
        out.extend_from_slice(&digest);

        // The vendor string and the tags.
        let last = seek_block == 0 && padding_block == 0;
        block_header(&mut out, 4, last, self.comment.len());
        out.extend_from_slice(&self.comment);

        if seek_block > 0 {
            block_header(&mut out, 3, padding_block == 0, seek_block - 4);
            if finished {
                for point in &self.seek_table.points {
                    out.extend_from_slice(&point.sample.to_be_bytes());
                    out.extend_from_slice(&point.offset.to_be_bytes());
                    out.extend_from_slice(&point.samples.to_be_bytes());
                }
            } else {
                for _ in 0..table {
                    out.extend_from_slice(&u64::MAX.to_be_bytes());
                    out.extend_from_slice(&[0; 10]);
                }
            }
        }
        if padding_block > 0 {
            block_header(&mut out, 1, true, padding_block - 4);
            out.resize(out.len() + padding_block - 4, 0);
        }
        debug_assert_eq!(out.len(), self.metadata_len());
        out
    }
}

/// Appends the four-byte header of a metadata block.
fn block_header(out: &mut Vec<u8>, kind: u8, last: bool, len: usize) {
    out.push(kind | if last { 0x80 } else { 0 });
    out.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
}

/// A FLAC stream being written block by block to a destination that can
/// seek, for [`AudioWriter`](crate::AudioWriter).
pub(crate) struct FlacStream {
    encoder: Encoder,
    /// Where the stream starts in the destination.
    origin: u64,
    /// Where its first frame starts.
    audio_start: u64,
    /// Set when writing a frame failed. The frame is still in the encoder's
    /// block, and the destination may hold part of it.
    torn: bool,
}

impl FlacStream {
    /// Writes the metadata of a stream whose length is not known yet at the
    /// current position of `writer`.
    pub(crate) fn start<W: Write + Seek>(
        writer: &mut W,
        channels: usize,
        sample_rate: u32,
        options: &FlacOptions,
    ) -> io::Result<Self> {
        let encoder = Encoder::new(channels, sample_rate, options)?;
        let origin = writer.stream_position()?;
        writer.write_all(&encoder.metadata(false))?;
        Ok(Self {
            audio_start: origin + encoder.metadata_len() as u64,
            encoder,
            origin,
            torn: false,
        })
    }

    /// Samples per channel taken so far.
    pub(crate) fn frames(&self) -> u64 {
        self.encoder.samples()
    }

    /// Writes the frame in the encoder's block.
    fn write_block<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        let bytes = self.encoder.encode()?;
        match writer.write_all(bytes) {
            Ok(()) => {
                self.encoder.commit();
                self.torn = false;
                Ok(())
            }
            Err(error) => {
                self.torn = true;
                Err(error)
            }
        }
    }

    /// After a frame failed to be written, goes back to where it started
    /// and writes it again.
    fn mend<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.torn {
            writer.seek(SeekFrom::Start(
                self.audio_start + self.encoder.audio_bytes(),
            ))?;
            self.write_block(writer)?;
        }
        Ok(())
    }

    /// Writes each block the encoder fills while `take` gives it samples.
    /// `take` returns how many it gave, zero once it has none left.
    fn feed<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        mut take: impl FnMut(&mut Encoder) -> usize,
    ) -> io::Result<()> {
        self.mend(writer)?;
        loop {
            let taken = take(&mut self.encoder);
            if self.encoder.block_is_full() {
                self.write_block(writer)?;
            } else if taken == 0 {
                return Ok(());
            }
        }
    }

    pub(crate) fn write_planar<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        buffer: &AudioBuffer<f32>,
    ) -> io::Result<()> {
        let mut done = 0;
        self.feed(writer, |encoder| {
            let taken = encoder.take_planar(buffer, done);
            done += taken;
            taken
        })
    }

    pub(crate) fn write_interleaved<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        samples: &[f32],
    ) -> io::Result<()> {
        let channels = self.encoder.channels;
        let mut done = 0;
        self.feed(writer, |encoder| {
            let taken = encoder.take_interleaved(&samples[done * channels..]);
            done += taken;
            taken
        })
    }

    pub(crate) fn write_silence<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        frames: u64,
    ) -> io::Result<()> {
        let mut left = frames;
        self.feed(writer, |encoder| {
            let taken = encoder.take_silence(left);
            left -= taken as u64;
            taken
        })
    }

    /// Writes the last, short block and completes the metadata. The writer
    /// is left at the end of the stream.
    ///
    /// The metadata is completed even if the last frame cannot be written,
    /// so that the frames before it can be read; the error is returned
    /// afterwards.
    pub(crate) fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        let mut written = self.mend(writer);
        if written.is_ok() && self.encoder.has_pending() {
            written = self.write_block(writer);
        }
        let completed = writer
            .seek(SeekFrom::Start(self.origin))
            .and_then(|_| writer.write_all(&self.encoder.metadata(true)))
            .and_then(|()| {
                let end = self.audio_start + self.encoder.audio_bytes();
                writer.seek(SeekFrom::Start(end))
            })
            .and_then(|_| writer.flush());
        written.and(completed)
    }
}

/// Encodes everything `take` gives the encoder and writes the whole stream
/// to `writer`.
fn encode_all<W: Write>(
    mut writer: W,
    mut encoder: Encoder,
    mut take: impl FnMut(&mut Encoder) -> usize,
) -> Result<(), FlacError> {
    // The metadata comes first and depends on every frame, so the frames
    // are held until the end. They are no larger than the samples were.
    let mut audio = Vec::new();
    loop {
        let taken = take(&mut encoder);
        if encoder.block_is_full() || (taken == 0 && encoder.has_pending()) {
            audio.extend_from_slice(encoder.encode()?);
            encoder.commit();
        } else if taken == 0 {
            break;
        }
    }
    writer.write_all(&encoder.metadata(true))?;
    writer.write_all(&audio)?;
    writer.flush()?;
    Ok(())
}

/// Writes `buffer` to `writer` as a FLAC stream.
///
/// Samples are rounded to the bit depth of `options`, with full scale at
/// `±1.0` as the decoder maps it, and clipped to it; NaN becomes zero. The
/// stream is encoded in memory and then written in one go, so `writer` need
/// not seek. To write a long recording a piece at a time, use
/// [`AudioWriter::flac`](crate::AudioWriter::flac), which produces exactly
/// the same bytes.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_formats::flac::{self, FlacOptions};
///
/// let mut buffer = AudioBuffer::<f32>::new(2, 1_000);
/// for (index, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
///     *sample = ((index % 100) as f32 - 50.0) / 64.0;
/// }
///
/// let mut bytes = Vec::new();
/// flac::write(&mut bytes, &buffer, 44_100, &FlacOptions::new(16))?;
///
/// let file = flac::read(bytes.as_slice())?;
/// assert_eq!(file.buffer, buffer);
/// # Ok::<(), flac::FlacError>(())
/// ```
///
/// # Errors
///
/// [`FlacError::Io`] of kind [`InvalidInput`](io::ErrorKind::InvalidInput),
/// with nothing written:
///
/// - if [`FlacOptions::validate`] refuses the options with the buffer's
///   number of channels and `sample_rate`: see the
///   [limits](FlacOptions#limits);
/// - if the stream would have 2^31 frames or more, more than a frame
///   header can number; a frame holds one block.
///
/// Any other [`FlacError::Io`] is an error from `writer`, which can be of
/// any kind, [`InvalidInput`](io::ErrorKind::InvalidInput) included.
pub fn write<W: Write>(
    writer: W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &FlacOptions,
) -> Result<(), FlacError> {
    let encoder = Encoder::new(buffer.num_channels(), sample_rate, options)?;
    let mut done = 0;
    encode_all(writer, encoder, |encoder| {
        let taken = encoder.take_planar(buffer, done);
        done += taken;
        taken
    })
}

/// Writes `buffer` to a new FLAC file at `path`, replacing any existing
/// file.
///
/// The options are checked before the file is created, so options that are
/// refused leave a file already at `path` as it was.
///
/// # Errors
///
/// As [`write()`], and any error from creating the file.
pub fn write_file<P: AsRef<Path>>(
    path: P,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &FlacOptions,
) -> Result<(), FlacError> {
    options.check(buffer.num_channels(), sample_rate)?;
    write(
        BufWriter::new(File::create(path)?),
        buffer,
        sample_rate,
        options,
    )
}

/// Writes integer samples to `writer` as a FLAC stream, one slice per
/// channel.
///
/// This is the lossless path for any bit depth, 32 bits included, which an
/// `f32` buffer cannot carry exactly. Every sample must fit the bit depth
/// of `options`: from `-2^(bits - 1)` to `2^(bits - 1) - 1`.
///
/// ```
/// use manguebyte_formats::flac::{self, FlacOptions};
///
/// let left: Vec<i32> = (0..500).map(|n| n * 4_000_000).collect();
/// let right: Vec<i32> = left.iter().map(|&sample| -sample).collect();
/// let mut bytes = Vec::new();
/// flac::write_integers(&mut bytes, &[&left, &right], 96_000, &FlacOptions::new(32))?;
/// assert_eq!(flac::read(bytes.as_slice())?.bits_per_sample, 32);
/// # Ok::<(), flac::FlacError>(())
/// ```
///
/// # Errors
///
/// As [`write()`], with `channels.len()` as the number of channels, and
/// also with kind [`InvalidInput`](io::ErrorKind::InvalidInput), with
/// nothing written, if the channels differ in length or a sample does not
/// fit the bit depth.
pub fn write_integers<W: Write>(
    writer: W,
    channels: &[&[i32]],
    sample_rate: u32,
    options: &FlacOptions,
) -> Result<(), FlacError> {
    let encoder = Encoder::new(channels.len(), sample_rate, options)?;
    let len = channels.first().map_or(0, |samples| samples.len());
    if channels.iter().any(|samples| samples.len() != len) {
        return Err(invalid("every channel must have the same number of samples").into());
    }
    let bits = options.bits_per_sample;
    let range = -(1i64 << (bits - 1))..1i64 << (bits - 1);
    let fits = channels.iter().all(|samples| {
        samples
            .iter()
            .all(|&sample| range.contains(&i64::from(sample)))
    });
    if !fits {
        return Err(invalid("a sample does not fit the bit depth").into());
    }
    let mut done = 0;
    encode_all(writer, encoder, |encoder| {
        let taken = encoder.take_integers(channels, done);
        done += taken;
        taken
    })
}

#[cfg(test)]
mod tests;
