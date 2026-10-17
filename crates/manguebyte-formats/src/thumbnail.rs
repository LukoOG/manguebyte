//! Waveform overviews: the lowest, highest and RMS level of every stretch of
//! a file, at every zoom, for drawing it.
//!
//! A [`Thumbnail`] keeps, for every channel, the minimum, the maximum and
//! the energy of each *bucket* of a few hundred frames, and the same for
//! groups of four buckets, of sixteen, and so on up to the whole file. A
//! range at any zoom is then summed from at most six cells of each level,
//! a few dozen in all, however long the range:
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_formats::thumbnail::Thumbnail;
//!
//! let mut audio = AudioBuffer::<f32>::new(1, 48_000);
//! for (frame, sample) in audio.channel_mut(0).iter_mut().enumerate() {
//!     *sample = (frame as f32 * 0.3).sin() * 0.5;
//! }
//! let mut thumbnail = Thumbnail::new(1, 48_000, 256);
//! thumbnail.push(&audio);
//!
//! // A view 100 pixels wide: a column for each pixel.
//! let mut columns = [None; 100];
//! thumbnail.columns(0, 0..48_000, &mut columns);
//! let peak = columns[50].unwrap();
//! assert!(peak.min < -0.49 && peak.max > 0.49);
//! assert!((peak.rms - 0.5 / 2f32.sqrt()).abs() < 0.01);
//! ```
//!
//! # Precision
//!
//! A range is widened to the buckets it touches: what [`range`](Thumbnail::range)
//! gives for frames 1000 to 2000 in buckets of 256 is the summary of frames
//! 768 to 2048, exactly, minimum and maximum to the bit. At a zoom where a
//! column spans less than a bucket, neighbouring columns can share one;
//! read the samples themselves at that zoom. The RMS level is summed in
//! 32-bit floats and is good to about one part in a million.
//!
//! # Building one
//!
//! [`push`](Thumbnail::push) takes audio a block at a time, so a thumbnail
//! grows while a file is read or recorded, and can be drawn at any moment
//! from what it holds. [`Thumbnail::from_reader`] reads a whole file;
//! [`ThumbnailBuilder`] does so on a thread of its own, sharing the
//! thumbnail as it grows. A thumbnail takes memory and its locks block,
//! so none of this belongs on the audio thread.
//!
//! # Memory
//!
//! Twelve bytes a bucket and channel, a third more for the coarser levels:
//! an hour of stereo at 48 kHz in buckets of 512 frames takes about 11 MB.
//!
//! # Cache
//!
//! [`write_cache`](Thumbnail::write_cache) saves a thumbnail in six bytes a
//! bucket and channel, about 4 MB for that hour, with a number that says
//! which file it was made from, and [`read_cache`](Thumbnail::read_cache)
//! reads it back in a fraction of the time decoding the file takes.
//! Levels are kept to 16 bits: a minimum is rounded down and a maximum up,
//! so that a cached thumbnail still holds the audio between them. A cache
//! file is checked against a checksum, and a damaged or hostile one is an
//! error, never a panic or an allocation beyond what its own length backs.

use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread::JoinHandle;

use manguebyte_core::AudioBuffer;

use crate::{AudioReader, FormatError};

/// How many buckets of one level make a bucket of the next.
const FAN_OUT: u64 = 4;

/// What a stretch of one channel holds: its lowest and highest sample and
/// its RMS level, in the samples' own units (full scale at `±1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Peak {
    /// The lowest sample.
    pub min: f32,
    /// The highest sample.
    pub max: f32,
    /// The root of the mean of the squares of the samples.
    pub rms: f32,
}

/// One bucket of one channel at one level.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cell {
    min: f32,
    max: f32,
    /// The sum of the squares of the samples.
    energy: f32,
}

impl Cell {
    const EMPTY: Self = Self {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
        energy: 0.0,
    };

    fn join(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
            energy: self.energy + other.energy,
        }
    }
}

/// The bucket being filled, for one channel.
#[derive(Debug, Clone, Copy)]
struct Partial {
    min: f32,
    max: f32,
    energy: f64,
}

impl Partial {
    const EMPTY: Self = Self {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
        energy: 0.0,
    };

    fn add(&mut self, sample: f32) {
        // NaN is left out of the minimum and maximum, and of the energy:
        // one bad sample must not blank a whole overview.
        if sample.is_nan() {
            return;
        }
        self.min = self.min.min(sample);
        self.max = self.max.max(sample);
        self.energy += f64::from(sample) * f64::from(sample);
    }

    fn cell(&self) -> Cell {
        Cell {
            min: self.min,
            max: self.max,
            energy: self.energy as f32,
        }
    }
}

/// A waveform overview of audio of any length. See the
/// [module documentation](self).
#[derive(Debug, Clone)]
pub struct Thumbnail {
    channels: usize,
    sample_rate: u32,
    bucket: u64,
    frames: u64,
    /// `levels[k]` holds the complete buckets of `bucket * 4^k` frames, the
    /// channels of each bucket side by side.
    levels: Vec<Vec<Cell>>,
    /// The bucket of level 0 being filled, and how many frames it has.
    partial: Vec<Partial>,
    filled: u64,
}

impl Thumbnail {
    /// An empty thumbnail of `channels` channels, with buckets of
    /// `frames_per_bucket` frames at its finest level.
    ///
    /// A few hundred frames make a good bucket: fine enough to draw a
    /// waveform a few hundred pixels per second wide, coarse enough to keep
    /// an hour in a few megabytes.
    ///
    /// # Panics
    ///
    /// Panics if `channels` or `frames_per_bucket` is zero.
    pub fn new(channels: usize, sample_rate: u32, frames_per_bucket: u32) -> Self {
        assert!(channels > 0, "a thumbnail needs at least one channel");
        assert!(frames_per_bucket > 0, "a bucket holds at least one frame");
        Self {
            channels,
            sample_rate,
            bucket: u64::from(frames_per_bucket),
            frames: 0,
            levels: vec![Vec::new()],
            partial: vec![Partial::EMPTY; channels],
            filled: 0,
        }
    }

    /// Reads all of `reader`, from where it stands to its end, into a new
    /// thumbnail.
    pub fn from_reader<R: Read + Seek>(
        reader: &mut AudioReader<R>,
        frames_per_bucket: u32,
    ) -> Result<Self, FormatError> {
        let mut thumbnail = Self::new(
            reader.num_channels(),
            reader.sample_rate(),
            frames_per_bucket,
        );
        let mut block = AudioBuffer::new(reader.num_channels(), 8_192);
        loop {
            let got = reader.read(&mut block)?;
            if got == 0 {
                return Ok(thumbnail);
            }
            thumbnail.push_frames(&block, got);
        }
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels
    }

    /// The sample rate of the audio, in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames in each bucket of the finest level.
    pub fn frames_per_bucket(&self) -> u32 {
        self.bucket as u32
    }

    /// How many frames have been added.
    pub fn len_frames(&self) -> u64 {
        self.frames
    }

    /// Adds the frames of `block` after those added before.
    ///
    /// # Panics
    ///
    /// Panics if `block` does not have [`num_channels`](Self::num_channels)
    /// channels.
    pub fn push(&mut self, block: &AudioBuffer<f32>) {
        self.push_frames(block, block.num_frames());
    }

    /// Adds frames given interleaved, `L R L R ...`.
    ///
    /// # Panics
    ///
    /// Panics if the length of `samples` is not a multiple of the number of
    /// channels.
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        assert!(
            samples.len() % self.channels == 0,
            "interleaved length must be a multiple of the channel count"
        );
        for frame in samples.chunks_exact(self.channels) {
            for (partial, &sample) in self.partial.iter_mut().zip(frame) {
                partial.add(sample);
            }
            self.filled += 1;
            self.frames += 1;
            if self.filled == self.bucket {
                self.close_bucket();
            }
        }
    }

    fn push_frames(&mut self, block: &AudioBuffer<f32>, count: usize) {
        assert_eq!(
            block.num_channels(),
            self.channels,
            "the block must have as many channels as the thumbnail"
        );
        let mut from = 0;
        while from < count {
            let take = ((self.bucket - self.filled) as usize).min(count - from);
            for (partial, samples) in self.partial.iter_mut().zip(block.iter_channels()) {
                for &sample in &samples[from..from + take] {
                    partial.add(sample);
                }
            }
            from += take;
            self.filled += take as u64;
            self.frames += take as u64;
            if self.filled == self.bucket {
                self.close_bucket();
            }
        }
    }

    /// Moves the bucket being filled into the finest level, and builds the
    /// buckets of the coarser levels it completes.
    fn close_bucket(&mut self) {
        let partial = &self.partial;
        self.levels[0].extend(partial.iter().map(Partial::cell));
        self.partial.fill(Partial::EMPTY);
        self.filled = 0;
        let mut level = 0;
        loop {
            let count = (self.levels[level].len() / self.channels) as u64;
            if count % FAN_OUT != 0 {
                break;
            }
            if level + 1 == self.levels.len() {
                self.levels.push(Vec::new());
            }
            let first = (count - FAN_OUT) as usize;
            for channel in 0..self.channels {
                let cell = (first..first + FAN_OUT as usize)
                    .map(|bucket| self.levels[level][bucket * self.channels + channel])
                    .fold(Cell::EMPTY, Cell::join);
                self.levels[level + 1].push(cell);
            }
            level += 1;
        }
    }

    /// Sums the cells of `channel` that cover the buckets `buckets` of the
    /// finest level, the one being filled included, handing each to
    /// `visit`.
    fn visit(&self, channel: usize, buckets: Range<u64>, mut visit: impl FnMut(Cell)) {
        let complete = (self.levels[0].len() / self.channels) as u64;
        let (mut low, mut high) = (buckets.start, buckets.end.min(complete));
        if buckets.end > complete && self.filled > 0 && buckets.start <= complete {
            visit(self.partial[channel].cell());
        }
        let mut level = 0;
        while low < high {
            let cell = |bucket: u64| self.levels[level][bucket as usize * self.channels + channel];
            while low < high && low % FAN_OUT != 0 {
                visit(cell(low));
                low += 1;
            }
            while low < high && high % FAN_OUT != 0 {
                high -= 1;
                visit(cell(high));
            }
            if low >= high {
                break;
            }
            // Whole groups of four, which the next level holds.
            if level + 1 == self.levels.len() {
                for bucket in low..high {
                    visit(cell(bucket));
                }
                break;
            }
            low /= FAN_OUT;
            high /= FAN_OUT;
            level += 1;
        }
    }

    /// The summary of the frames `frames` of `channel`, widened to the
    /// buckets they touch, or `None` if the range holds no frame added so
    /// far. A range past the end is cut to the end.
    ///
    /// # Panics
    ///
    /// Panics if `channel` is not one of the thumbnail's.
    pub fn range(&self, channel: usize, frames: Range<u64>) -> Option<Peak> {
        assert!(channel < self.channels, "no such channel");
        let end = frames.end.min(self.frames);
        if frames.start >= end {
            return None;
        }
        let buckets = frames.start / self.bucket..end.div_ceil(self.bucket);
        let mut cell = Cell::EMPTY;
        let mut energy = 0.0f64;
        self.visit(channel, buckets.clone(), |part| {
            cell = cell.join(part);
            energy += f64::from(part.energy);
        });
        // The frames those buckets hold.
        let first = buckets.start * self.bucket;
        let last = (buckets.end * self.bucket).min(self.frames);
        let count = (last - first).max(1) as f64;
        if cell.min > cell.max {
            // Nothing but NaN.
            return Some(Peak::default());
        }
        Some(Peak {
            min: cell.min,
            max: cell.max,
            rms: (energy / count).sqrt() as f32,
        })
    }

    /// Splits the frames `frames` of `channel` into `columns.len()` columns
    /// of equal width and puts the summary of each in its column, as
    /// [`range`](Self::range) gives it: one column for each pixel of a
    /// view, at any zoom. Columns past the frames added so far are `None`.
    pub fn columns(&self, channel: usize, frames: Range<u64>, columns: &mut [Option<Peak>]) {
        let count = columns.len() as u128;
        let start = frames.start;
        let len = u128::from(frames.end.saturating_sub(start));
        for (index, column) in columns.iter_mut().enumerate() {
            let from = start + (len * index as u128 / count) as u64;
            let to = start + (len * (index as u128 + 1) / count) as u64;
            // A column narrower than a frame shows the frame it falls in.
            *column = self.range(channel, from..to.max(from + 1));
        }
    }

    /// Writes the thumbnail as a cache file. `source` is a number that
    /// identifies the audio it was made from, such as
    /// [`file_fingerprint`] of the file; [`read_cache`](Self::read_cache)
    /// can check it.
    pub fn write_cache<W: Write>(&self, mut writer: W, source: u64) -> io::Result<()> {
        let buckets = self.frames.div_ceil(self.bucket);
        // The largest magnitude, so that 16 bits span the levels there are,
        // even beyond full scale.
        let mut scale = 0.0f32;
        for channel in 0..self.channels {
            if let Some(peak) = self.range(channel, 0..self.frames) {
                scale = scale.max(peak.min.abs()).max(peak.max.abs());
            }
        }
        if !(scale.is_finite() && scale > 0.0) {
            scale = 1.0;
        }
        let mut out = Vec::with_capacity(HEADER_LEN + buckets as usize * self.channels * 6 + 8);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.channels as u16).to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.bucket as u32).to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&source.to_le_bytes());
        out.extend_from_slice(&scale.to_le_bytes());
        let quantise = |value: f32, round: fn(f32) -> f32| -> i16 {
            round(value / scale * 32_767.0).clamp(-32_767.0, 32_767.0) as i16
        };
        for bucket in 0..buckets {
            for channel in 0..self.channels {
                let peak = self
                    .range(channel, bucket * self.bucket..(bucket + 1) * self.bucket)
                    .unwrap_or_default();
                out.extend_from_slice(&quantise(peak.min, f32::floor).to_le_bytes());
                out.extend_from_slice(&quantise(peak.max, f32::ceil).to_le_bytes());
                let rms = (peak.rms / scale * 65_535.0).round().clamp(0.0, 65_535.0) as u16;
                out.extend_from_slice(&rms.to_le_bytes());
            }
        }
        let checksum = fnv1a(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        writer.write_all(&out)
    }

    /// Reads a thumbnail that [`write_cache`](Self::write_cache) wrote. With
    /// `source` given, a cache made from other audio is
    /// [`ThumbnailError::Stale`].
    ///
    /// The thumbnail read can be pushed to, to carry on where the cached
    /// one stopped; its last bucket then joins levels kept to 16 bits with
    /// new ones that are not.
    pub fn read_cache<R: Read>(reader: R, source: Option<u64>) -> Result<Self, ThumbnailError> {
        let mut reader = reader;
        let mut header = [0u8; HEADER_LEN];
        reader
            .read_exact(&mut header)
            .map_err(|error| match error.kind() {
                io::ErrorKind::UnexpectedEof => ThumbnailError::NotACache,
                _ => ThumbnailError::Io(error),
            })?;
        if &header[..4] != MAGIC {
            return Err(ThumbnailError::NotACache);
        }
        let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
        let u32_at = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let u64_at = |at: usize| u64::from(u32_at(at)) | u64::from(u32_at(at + 4)) << 32;
        if u16_at(4) != VERSION {
            return Err(ThumbnailError::Unsupported);
        }
        let channels = usize::from(u16_at(6));
        let sample_rate = u32_at(8);
        let bucket = u32_at(12);
        let frames = u64_at(16);
        let stored_source = u64_at(24);
        let scale = f32::from_le_bytes([header[32], header[33], header[34], header[35]]);
        if channels == 0 || bucket == 0 || !(scale.is_finite() && scale > 0.0) {
            return Err(ThumbnailError::Corrupt("impossible header"));
        }
        if source.is_some_and(|source| source != stored_source) {
            return Err(ThumbnailError::Stale);
        }
        let body_len = frames
            .div_ceil(u64::from(bucket))
            .checked_mul(channels as u64 * 6)
            .and_then(|len| len.checked_add(8))
            .ok_or(ThumbnailError::Corrupt("impossible length"))?;
        // Read no more than the header asks for, and allocate no more than
        // arrives: a header can claim anything.
        let mut body = Vec::new();
        reader.take(body_len).read_to_end(&mut body)?;
        if body.len() as u64 != body_len {
            return Err(ThumbnailError::Corrupt("the file is cut short"));
        }
        let (cells, checksum) = body.split_at(body.len() - 8);
        let mut hasher = Fnv::new();
        hasher.update(&header);
        hasher.update(cells);
        let stored = u64::from_le_bytes(checksum.try_into().expect("eight bytes"));
        if hasher.finish() != stored {
            return Err(ThumbnailError::Corrupt("the checksum does not match"));
        }

        let mut thumbnail = Self::new(channels, sample_rate, bucket);
        let level = |value: i16| f32::from(value) / 32_767.0 * scale;
        let buckets = cells.len() / (channels * 6);
        for (index, record) in cells.chunks_exact(channels * 6).enumerate() {
            let in_bucket = if index + 1 == buckets {
                frames - index as u64 * u64::from(bucket)
            } else {
                u64::from(bucket)
            };
            for (channel, fields) in record.chunks_exact(6).enumerate() {
                let min = level(i16::from_le_bytes([fields[0], fields[1]]));
                let max = level(i16::from_le_bytes([fields[2], fields[3]]));
                let rms = f32::from(u16::from_le_bytes([fields[4], fields[5]])) / 65_535.0 * scale;
                thumbnail.partial[channel] = Partial {
                    min,
                    max,
                    energy: f64::from(rms) * f64::from(rms) * in_bucket as f64,
                };
            }
            thumbnail.frames += in_bucket;
            thumbnail.filled = in_bucket;
            if in_bucket == u64::from(bucket) {
                thumbnail.close_bucket();
            }
        }
        Ok(thumbnail)
    }
}

/// The start of a cache file.
const MAGIC: &[u8; 4] = b"MBTN";
/// The layout of a cache file this writes.
const VERSION: u16 = 1;
/// Bytes before the buckets: magic, version, channels, rate, bucket size,
/// frames, source and scale.
const HEADER_LEN: usize = 36;

/// FNV-1a, 64 bits (Fowler, Noll and Vo), to tell a damaged cache file.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xCBF2_9CE4_8422_2325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01B3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hasher = Fnv::new();
    hasher.update(bytes);
    hasher.finish()
}

/// A number that changes when the file at `path` does: from its length
/// and the time it was last changed. Good for telling whether a cached
/// thumbnail still belongs to its file; not a hash of its contents.
pub fn file_fingerprint<P: AsRef<Path>>(path: P) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as u64);
    let mut hasher = Fnv::new();
    hasher.update(&metadata.len().to_le_bytes());
    hasher.update(&modified.to_le_bytes());
    Ok(hasher.finish())
}

/// What went wrong while reading a thumbnail cache.
#[derive(Debug)]
pub enum ThumbnailError {
    /// The reader failed.
    Io(io::Error),
    /// The data is not a thumbnail cache.
    NotACache,
    /// The cache is of a version this crate does not read.
    Unsupported,
    /// The cache was made from other audio than the one asked about.
    Stale,
    /// The cache is damaged.
    Corrupt(&'static str),
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::NotACache => f.write_str("not a thumbnail cache"),
            Self::Unsupported => f.write_str("a thumbnail cache of an unknown version"),
            Self::Stale => f.write_str("the thumbnail cache was made from other audio"),
            Self::Corrupt(what) => write!(f, "damaged thumbnail cache: {what}"),
        }
    }
}

impl std::error::Error for ThumbnailError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ThumbnailError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Builds a thumbnail of a file on a thread of its own, sharing it while it
/// grows.
///
/// ```no_run
/// use manguebyte_formats::AudioReader;
/// use manguebyte_formats::thumbnail::ThumbnailBuilder;
///
/// let reader = AudioReader::open("session.wav")?;
/// let builder = ThumbnailBuilder::spawn(reader, 512);
/// // From the interface thread, at every frame drawn:
/// let mut columns = vec![None; 800];
/// builder.with(|thumbnail| {
///     let end = thumbnail.len_frames();
///     thumbnail.columns(0, 0..end, &mut columns);
/// });
/// // And once it is done:
/// let thumbnail = builder.join()?;
/// # Ok::<(), manguebyte_formats::FormatError>(())
/// ```
///
/// The thumbnail sits behind a lock that the builder takes once for each
/// block of 8192 frames it adds, for as long as adding them takes, and that
/// [`with`](Self::with) takes for as long as its closure runs. It is a lock
/// all the same: never call [`with`](Self::with) from the audio thread.
pub struct ThumbnailBuilder {
    shared: Arc<RwLock<Thumbnail>>,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    total: Option<u64>,
    thread: Option<JoinHandle<Result<(), FormatError>>>,
}

impl ThumbnailBuilder {
    /// Starts reading `reader` from where it stands into a thumbnail with
    /// buckets of `frames_per_bucket` frames.
    ///
    /// # Panics
    ///
    /// Panics if `frames_per_bucket` is zero, or if the system cannot start
    /// a thread.
    pub fn spawn<R>(mut reader: AudioReader<R>, frames_per_bucket: u32) -> Self
    where
        R: Read + Seek + Send + 'static,
    {
        let thumbnail = Thumbnail::new(
            reader.num_channels(),
            reader.sample_rate(),
            frames_per_bucket,
        );
        let shared = Arc::new(RwLock::new(thumbnail));
        let done = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let total = reader
            .len_frames()
            .map(|len| len.saturating_sub(reader.position()));
        let thread = {
            let (shared, done, cancel, finished) = (
                shared.clone(),
                done.clone(),
                cancel.clone(),
                finished.clone(),
            );
            std::thread::Builder::new()
                .name("manguebyte-thumbnail".into())
                .spawn(move || {
                    let mut block = AudioBuffer::new(reader.num_channels(), 8_192);
                    let result = loop {
                        if cancel.load(Ordering::Relaxed) {
                            break Ok(());
                        }
                        match reader.read(&mut block) {
                            Ok(0) => break Ok(()),
                            Ok(got) => {
                                write_lock(&shared).push_frames(&block, got);
                                done.fetch_add(got as u64, Ordering::Relaxed);
                            }
                            Err(error) => break Err(error),
                        }
                    };
                    finished.store(true, Ordering::Release);
                    result
                })
                .expect("a thread for the thumbnail")
        };
        Self {
            shared,
            done,
            cancel,
            finished,
            total,
            thread: Some(thread),
        }
    }

    /// Runs `f` on the thumbnail as it stands, holding its lock meanwhile.
    pub fn with<T>(&self, f: impl FnOnce(&Thumbnail) -> T) -> T {
        f(&read_lock(&self.shared))
    }

    /// Frames read so far.
    pub fn frames_done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Frames there are to read, when the file says.
    pub fn frames_total(&self) -> Option<u64> {
        self.total
    }

    /// Whether the thread has stopped: at the end of the file, at an error,
    /// or because it was cancelled.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Asks the thread to stop after the block it is reading.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Waits for the thread and hands over the thumbnail: of the whole
    /// file, or, after [`cancel`](Self::cancel), of what was read.
    ///
    /// # Errors
    ///
    /// The error that stopped the reading. The thumbnail of what was read
    /// before it is lost with it; [`with`](Self::with) beforehand can still
    /// see it.
    pub fn join(mut self) -> Result<Thumbnail, FormatError> {
        let thread = self.thread.take().expect("joined once");
        match thread.join() {
            Ok(result) => result?,
            Err(panic) => std::panic::resume_unwind(panic),
        }
        let thumbnail = match Arc::try_unwrap(std::mem::replace(
            &mut self.shared,
            Arc::new(RwLock::new(Thumbnail::new(1, 0, 1))),
        )) {
            Ok(lock) => lock
                .into_inner()
                .unwrap_or_else(|poison| poison.into_inner()),
            Err(shared) => read_lock(&shared).clone(),
        };
        Ok(thumbnail)
    }
}

impl Drop for ThumbnailBuilder {
    /// Stops the thread, and waits for it.
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_lock(lock: &RwLock<Thumbnail>) -> RwLockReadGuard<'_, Thumbnail> {
    lock.read().unwrap_or_else(|poison| poison.into_inner())
}

fn write_lock(lock: &RwLock<Thumbnail>) -> std::sync::RwLockWriteGuard<'_, Thumbnail> {
    lock.write().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_visits_a_few_cells_a_level_whatever_its_length() {
        let mut thumbnail = Thumbnail::new(1, 48_000, 16);
        let samples: Vec<f32> = (0..1_000_000u64)
            .map(|i| ((i * 7_919) % 1_000) as f32)
            .collect();
        thumbnail.push_interleaved(&samples);
        let levels = thumbnail.levels.len();
        assert!(levels >= 8, "{levels}");
        let mut most = 0;
        for (start, end) in [(0, 1_000_000), (17, 999_999), (123_456, 654_321), (5, 6)] {
            let mut visited = 0;
            let buckets = start / 16..(end as u64).div_ceil(16);
            thumbnail.visit(0, buckets, |_| visited += 1);
            most = most.max(visited);
        }
        assert!(most <= 6 * levels, "{most} cells for {levels} levels");
    }
}
