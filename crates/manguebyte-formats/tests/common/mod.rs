//! Helpers shared by the streaming tests.
//!
//! Each test file is its own program and uses only some of these.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use manguebyte_core::AudioBuffer;

pub mod lossy;
pub mod vorbis;

/// A small deterministic random number generator (xorshift64*), so that
/// "random" block sizes and seeks are the same on every run.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    /// A number in `low..=high`.
    pub fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

/// A buffer in which every sample is different and says where it is: the
/// value at `(channel, frame)` is `sample_at(channel, frame)`.
pub fn ramp(channels: usize, frames: usize) -> AudioBuffer<f32> {
    let mut buffer = AudioBuffer::new(channels, frames);
    for (channel, samples) in buffer.iter_channels_mut().enumerate() {
        for (frame, sample) in samples.iter_mut().enumerate() {
            *sample = sample_at(channel, frame as u64);
        }
    }
    buffer
}

/// A value that 16 bits hold exactly, different for neighbouring frames and
/// for each channel, and repeating only every 65521 frames.
pub fn sample_at(channel: usize, frame: u64) -> f32 {
    let step = (frame * 7 + channel as u64 * 12_345) % 65_521;
    (step as f32 - 32_760.0) / 32_768.0
}

/// The frames `range` of `buffer`, as a buffer of their own.
pub fn slice(buffer: &AudioBuffer<f32>, range: std::ops::Range<usize>) -> AudioBuffer<f32> {
    let mut out = AudioBuffer::new(buffer.num_channels(), range.len());
    for (to, from) in out.iter_channels_mut().zip(buffer.iter_channels()) {
        to.copy_from_slice(&from[range.clone()]);
    }
    out
}

/// A file that exists only as its interesting parts.
///
/// It remembers its length and the bytes written inside the stretches named
/// when it was created. Everything else reads back as zero. That is enough
/// to write and read a file of many gigabytes without holding it, as long as
/// the test knows where it will look.
pub struct SparseFile {
    kept: Vec<std::ops::Range<u64>>,
    /// Bytes inside the kept stretches, by offset.
    bytes: BTreeMap<u64, u8>,
    len: u64,
    position: u64,
    /// How many bytes went through `write`.
    pub written: u64,
}

impl SparseFile {
    pub fn new(kept: &[std::ops::Range<u64>]) -> Self {
        Self {
            kept: kept.to_vec(),
            bytes: BTreeMap::new(),
            len: 0,
            position: 0,
            written: 0,
        }
    }

    pub fn total_len(&self) -> u64 {
        self.len
    }

    /// The bytes of `range`, with zero wherever nothing was kept.
    pub fn bytes(&self, range: std::ops::Range<u64>) -> Vec<u8> {
        range
            .map(|offset| self.bytes.get(&offset).copied().unwrap_or(0))
            .collect()
    }
}

impl Write for SparseFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let start = self.position;
        let end = start + data.len() as u64;
        for kept in &self.kept {
            let from = kept.start.max(start);
            let to = kept.end.min(end);
            for offset in from..to {
                self.bytes.insert(offset, data[(offset - start) as usize]);
            }
        }
        self.position = end;
        self.len = self.len.max(end);
        self.written += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for SparseFile {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = (self.len.saturating_sub(self.position)).min(out.len() as u64) as usize;
        out[..count].fill(0);
        let start = self.position;
        for (&offset, &byte) in self.bytes.range(start..start + count as u64) {
            out[(offset - start) as usize] = byte;
        }
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for SparseFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position = target.ok_or_else(|| io::Error::other("seek before the start"))?;
        Ok(self.position)
    }
}

/// A reader that counts what is asked of it, to show that seeking reads a
/// small part of a file and not all of it.
pub struct Counting<R> {
    inner: R,
    /// Bytes read so far. Shared, so that the count can be looked at while
    /// something else owns the reader.
    pub bytes_read: Arc<AtomicU64>,
}

impl<R> Counting<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(out)?;
        self.bytes_read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A reader that fails after a given number of bytes, as a disk or a
/// network share that goes away would.
pub struct Failing<R> {
    inner: R,
    /// Bytes that can still be read before every read fails.
    pub budget: Arc<AtomicU64>,
}

impl<R> Failing<R> {
    pub fn new(inner: R, budget: u64) -> Self {
        Self {
            inner,
            budget: Arc::new(AtomicU64::new(budget)),
        }
    }
}

impl<R: Read> Read for Failing<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let budget = self.budget.load(Ordering::Relaxed);
        if budget == 0 {
            return Err(io::Error::other("the disk went away"));
        }
        let allowed = out.len().min(usize::try_from(budget).unwrap_or(usize::MAX));
        let count = self.inner.read(&mut out[..allowed])?;
        self.budget.store(budget - count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

impl<R: Seek> Seek for Failing<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// As a writer it fails the same way: a disk that fills up.
impl<W: Write> Write for Failing<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let budget = self.budget.load(Ordering::Relaxed);
        if budget == 0 {
            return Err(io::Error::other("the disk is full"));
        }
        let allowed = data
            .len()
            .min(usize::try_from(budget).unwrap_or(usize::MAX));
        let count = self.inner.write(&data[..allowed])?;
        self.budget.store(budget - count as u64, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T> Failing<T> {
    pub fn into_inner(self) -> T {
        self.inner
    }
}

/// A disk with room for a fixed number of bytes. Up to there it takes
/// whatever is written; once it is full, what is on it can still be written
/// over and nothing can be added. That is how a full disk behaves, and it is
/// what makes completing a header possible on one.
///
/// [`Failing`] is the harsher case, a disk that refuses every write.
///
/// The bytes are shared, so that a test can look at them after the disk
/// itself is gone, as it is when a recording thread has ended.
pub struct FullDisk {
    bytes: Arc<Mutex<Vec<u8>>>,
    position: u64,
    room: u64,
}

impl FullDisk {
    pub fn new(room: u64) -> Self {
        Self {
            bytes: Arc::default(),
            position: 0,
            room,
        }
    }

    /// What is on the disk, now and later.
    pub fn contents(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.bytes)
    }
}

impl Write for FullDisk {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let fits = self
            .room
            .saturating_sub(self.position)
            .min(data.len() as u64) as usize;
        if fits == 0 && !data.is_empty() {
            return Err(io::Error::other("no space left on device"));
        }
        let mut bytes = self.bytes.lock().unwrap();
        let start = self.position as usize;
        if bytes.len() < start + fits {
            bytes.resize(start + fits, 0);
        }
        bytes[start..start + fits].copy_from_slice(&data[..fits]);
        self.position += fits as u64;
        Ok(fits)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for FullDisk {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.bytes.lock().unwrap().len() as u64;
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position = target.ok_or_else(|| io::Error::other("seek before the start"))?;
        Ok(self.position)
    }
}

/// Writes bits most significant first, as FLAC packs them.
#[derive(Default)]
pub struct BitWriter {
    pub bytes: Vec<u8>,
    /// Bits used in the last byte, 0 when it is full or there is none.
    used: u32,
}

impl BitWriter {
    /// Appends the lowest `count` bits of `value`.
    pub fn bits(&mut self, value: u64, count: u32) {
        // Whole bytes on a byte boundary, which is where the samples of the
        // usual depths fall, go in at once.
        if self.used == 0 && count % 8 == 0 {
            let bytes = value.to_be_bytes();
            self.bytes
                .extend_from_slice(&bytes[8 - count as usize / 8..]);
            return;
        }
        for index in (0..count).rev() {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> index) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= bit << (7 - self.used);
            self.used = (self.used + 1) % 8;
        }
    }

    /// Moves on to the next whole byte, leaving zeros in between.
    pub fn align(&mut self) {
        self.used = 0;
    }
}

/// CRC-8 with polynomial 0x07, bit by bit.
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16 with polynomial 0x8005, bit by bit.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The checksum of an Ogg page, bit by bit: CRC-32 with polynomial
/// 0x04C11DB7, from zero, as RFC 3533 has it. The page's own checksum field
/// must be zero.
pub fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The pages of an Ogg stream: where each starts and ends.
pub fn ogg_pages(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut pages = Vec::new();
    let mut at = 0;
    while at + 27 <= bytes.len() && &bytes[at..at + 4] == b"OggS" {
        let segments = usize::from(bytes[at + 26]);
        let body: usize = bytes[at + 27..at + 27 + segments]
            .iter()
            .map(|&lacing| usize::from(lacing))
            .sum();
        let end = at + 27 + segments + body;
        pages.push(at..end);
        at = end;
    }
    pages
}

/// Repeats the audio of an Ogg Vorbis stream that libvorbis wrote `times`
/// over, as one longer stream: its headers, the pages of audio before the
/// last `times` times, and the last page once.
///
/// Each page gets its number in the new stream, its checksum worked out
/// again, and a granule position carried on from the round before. Where
/// one round meets the next, the first packet of a round no longer starts
/// the stream, so it finishes samples it did not finish before: a quarter
/// of each of its block and the block before it. The granule positions are
/// worked out from the block size of every packet, which libvorbis gives
/// as its first mode bit, mode 0 being the short block and mode 1 the long;
/// that this reproduces the granule positions of the stream it is given
/// is checked.
pub fn repeat_vorbis(bytes: &[u8], times: usize) -> Vec<u8> {
    let pages = ogg_pages(bytes);
    assert_eq!(pages.last().unwrap().end, bytes.len());
    let lacing = |page: &std::ops::Range<usize>| {
        let segments = usize::from(bytes[page.start + 26]);
        &bytes[page.start + 27..page.start + 27 + segments]
    };
    let granule = |page: &std::ops::Range<usize>| {
        i64::from_le_bytes(bytes[page.start + 6..page.start + 14].try_into().unwrap())
    };

    // The packets of the stream, each with the page it ends on and its
    // first byte.
    let mut packets: Vec<(usize, u8)> = Vec::new();
    let mut first_byte = None;
    for (index, page) in pages.iter().enumerate() {
        let mut offset = page.start + 27 + lacing(page).len();
        for &value in lacing(page) {
            if first_byte.is_none() && value > 0 {
                first_byte = Some(bytes[offset]);
            }
            offset += usize::from(value);
            if value < 255 {
                packets.push((index, first_byte.take().unwrap_or(0)));
            }
        }
    }
    let identification = &bytes[pages[0].start + 27 + lacing(&pages[0]).len()..];
    let sizes = [
        1i64 << (identification[28] & 0x0F),
        1i64 << (identification[28] >> 4),
    ];
    let audio_start = packets[2].0 + 1;
    let audio: Vec<(usize, i64)> = packets[3..]
        .iter()
        .map(|&(page, first)| (page, sizes[usize::from((first >> 1) & 1)]))
        .collect();

    // The granule position after each packet, from the start.
    let mut after = vec![0i64; audio.len()];
    for index in 1..audio.len() {
        after[index] = after[index - 1] + audio[index - 1].1 / 4 + audio[index].1 / 4;
    }
    let last = pages.len() - 1;
    for (index, &(page, _)) in audio.iter().enumerate() {
        let is_last_on_page = audio.get(index + 1).is_none_or(|next| next.0 != page);
        if is_last_on_page && page != last {
            assert_eq!(after[index], granule(&pages[page]), "page {page}");
        }
    }
    // The rounds end with the page before the last, which must end with a
    // packet.
    assert!(*lacing(&pages[last - 1]).last().unwrap() < 255);
    let round_packets = audio.iter().filter(|(page, _)| *page < last).count();
    let junction = audio[round_packets - 1].1 / 4 + audio[0].1 / 4;
    let round = after[round_packets - 1] + junction;

    let mut out = Vec::new();
    let mut sequence = 0u32;
    let mut put = |page: &std::ops::Range<usize>, granule: i64, out: &mut Vec<u8>| {
        let mut page = bytes[page.clone()].to_vec();
        // A page on which no packet ends says so with -1, whatever round.
        if i64::from_le_bytes(page[6..14].try_into().unwrap()) != -1 {
            page[6..14].copy_from_slice(&granule.to_le_bytes());
        }
        page[18..22].copy_from_slice(&sequence.to_le_bytes());
        page[22..26].fill(0);
        let crc = ogg_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&page);
        sequence += 1;
    };
    for page in &pages[..audio_start] {
        put(page, granule(page), &mut out);
    }
    for time in 0..times as i64 {
        for page in &pages[audio_start..last] {
            put(page, granule(page) + time * round, &mut out);
        }
    }
    put(
        &pages[last],
        granule(&pages[last]) + (times as i64 - 1) * round,
        &mut out,
    );
    out
}

/// The start of a FLAC stream of 48 kHz audio with blocks of `block`
/// samples: the marker and a stream information block that is the last of
/// the metadata. A test that writes its own frames puts them after this.
/// A `total` of zero leaves the length undeclared.
pub fn flac_head(channels: usize, bits: u32, block: usize, total: u64) -> Vec<u8> {
    let mut bytes = b"fLaC".to_vec();
    let mut info = BitWriter::default();
    info.bits(block as u64, 16);
    info.bits(block as u64, 16);
    info.bits(0, 24);
    info.bits(0, 24);
    info.bits(48_000, 20);
    info.bits(channels as u64 - 1, 3);
    info.bits(u64::from(bits) - 1, 5);
    info.bits(total, 36);
    info.bytes.extend_from_slice(&[0; 16]);
    write_block_header(&mut bytes, 0, true, info.bytes.len());
    bytes.extend_from_slice(&info.bytes);
    bytes
}

/// What kind of FLAC stream [`write_flac`] makes.
#[derive(Clone)]
pub struct FlacSpec {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u32,
    /// Samples in each frame. A stream whose frames all have the size of the
    /// first entry is written with frame numbers; any other with sample
    /// numbers, the sizes being used in turn.
    pub block_sizes: Vec<usize>,
    /// Put a seek point at every this many frames.
    pub seek_every: Option<usize>,
    /// Whether the header says how long the stream is.
    pub declare_total: bool,
    /// Whether the header says how large the largest frame is.
    pub declare_frame_size: bool,
    /// Number carried by the first frame: zero unless the stream is the
    /// tail of a longer one.
    pub first_number: u64,
}

impl FlacSpec {
    pub fn new(channels: usize, bits: u32, block_size: usize) -> Self {
        Self {
            sample_rate: 48_000,
            channels,
            bits,
            block_sizes: vec![block_size],
            seek_every: None,
            declare_total: true,
            declare_frame_size: true,
            first_number: 0,
        }
    }

    fn is_variable(&self) -> bool {
        self.block_sizes
            .iter()
            .any(|&size| size != self.block_sizes[0])
    }
}

/// A FLAC stream made by [`write_flac`], and where its frames are.
pub struct FlacStream {
    pub bytes: Vec<u8>,
    /// Offset of the first frame.
    pub audio_start: usize,
    /// For each frame: its offset from `audio_start`, the number of its
    /// first sample, and how many samples it holds.
    pub frames: Vec<(usize, u64, usize)>,
}

/// Writes a FLAC stream of `total` samples per channel, the sample at
/// `(channel, index)` being `sample(channel, index)`.
///
/// This is not a compressor. Each channel of each frame is stored either as
/// one value, when all its samples are equal, or sample by sample: the two
/// simplest kinds of subframe the format has. That is all a test of
/// framing, seeking and memory use needs, and it lets a test build streams
/// that FFmpeg does not write: blocks of varying size, seek tables, headers
/// that leave the length out.
pub fn write_flac(spec: &FlacSpec, total: u64, sample: impl Fn(usize, u64) -> i32) -> FlacStream {
    let variable = spec.is_variable();
    let mut audio = Vec::new();
    let mut frames = Vec::new();
    let mut block = vec![Vec::<i32>::new(); spec.channels];
    let mut position = 0u64;
    let mut index = 0usize;
    let mut largest_frame = 0usize;
    while position < total {
        let size = spec.block_sizes[index % spec.block_sizes.len()];
        let size = size.min((total - position) as usize);
        for (channel, samples) in block.iter_mut().enumerate() {
            samples.clear();
            samples.extend((0..size as u64).map(|offset| sample(channel, position + offset)));
        }

        let mut frame = BitWriter::default();
        // Sync code (thirteen ones and a zero), a reserved bit, and how
        // frames are numbered.
        frame.bits(0x3FFE, 14);
        frame.bits(0, 1);
        frame.bits(u64::from(variable), 1);
        // Block size written out in 16 bits (code 7); sample rate, and
        // further down the sample size, as the stream information has them
        // (code 0).
        frame.bits(7, 4);
        frame.bits(0, 4);
        frame.bits(spec.channels as u64 - 1, 4);
        frame.bits(0, 3);
        frame.bits(0, 1);
        let number = spec.first_number + if variable { position } else { index as u64 };
        write_number(&mut frame, number);
        frame.bits(size as u64 - 1, 16);
        let header_crc = crc8(&frame.bytes);
        frame.bits(u64::from(header_crc), 8);

        for samples in &block {
            let mask = (1u64 << spec.bits) - 1;
            // A subframe starts with a zero bit, six bits of type and a bit
            // for wasted bits: type 0 is one value, type 1 every sample.
            if samples.iter().all(|&value| value == samples[0]) {
                frame.bits(0x00, 8);
                frame.bits(samples[0] as i64 as u64 & mask, spec.bits);
            } else {
                frame.bits(0x02, 8);
                for &value in samples {
                    frame.bits(value as i64 as u64 & mask, spec.bits);
                }
            }
        }
        frame.align();
        let frame_crc = crc16(&frame.bytes);
        frame.bits(u64::from(frame_crc), 16);

        frames.push((audio.len(), position, size));
        largest_frame = largest_frame.max(frame.bytes.len());
        audio.extend_from_slice(&frame.bytes);
        position += size as u64;
        index += 1;
    }

    let mut bytes = b"fLaC".to_vec();
    let mut info = BitWriter::default();
    let smallest = spec.block_sizes.iter().copied().min().unwrap();
    let largest = spec.block_sizes.iter().copied().max().unwrap();
    info.bits(smallest as u64, 16);
    info.bits(largest as u64, 16);
    info.bits(0, 24);
    info.bits(
        if spec.declare_frame_size {
            largest_frame as u64
        } else {
            0
        },
        24,
    );
    info.bits(u64::from(spec.sample_rate), 20);
    info.bits(spec.channels as u64 - 1, 3);
    info.bits(u64::from(spec.bits) - 1, 5);
    info.bits(if spec.declare_total { total } else { 0 }, 36);
    // The MD5 of the audio, left as zeros: "not computed".
    info.bytes.extend_from_slice(&[0; 16]);
    write_block_header(&mut bytes, 0, false, info.bytes.len());
    bytes.extend_from_slice(&info.bytes);

    if let Some(every) = spec.seek_every {
        let mut table = Vec::new();
        for &(offset, first, size) in frames.iter().step_by(every.max(1)) {
            table.extend_from_slice(&(spec.first_number_of_sample(first, variable)).to_be_bytes());
            table.extend_from_slice(&(offset as u64).to_be_bytes());
            table.extend_from_slice(&(size as u16).to_be_bytes());
        }
        // Two placeholders, as encoders leave for points to be added later.
        for _ in 0..2 {
            table.extend_from_slice(&u64::MAX.to_be_bytes());
            table.extend_from_slice(&[0; 10]);
        }
        write_block_header(&mut bytes, 3, false, table.len());
        bytes.extend_from_slice(&table);
    }

    // Padding, which every reader must step over, ends the metadata.
    write_block_header(&mut bytes, 1, true, 37);
    bytes.extend_from_slice(&[0; 37]);

    let audio_start = bytes.len();
    bytes.extend_from_slice(&audio);
    FlacStream {
        bytes,
        audio_start,
        frames,
    }
}

impl FlacSpec {
    /// The sample number a seek point carries for the frame starting at
    /// `first`. Seek points always count samples.
    fn first_number_of_sample(&self, first: u64, variable: bool) -> u64 {
        if variable {
            self.first_number + first
        } else {
            self.first_number * self.block_sizes[0] as u64 + first
        }
    }
}

fn write_block_header(bytes: &mut Vec<u8>, kind: u8, last: bool, len: usize) {
    bytes.push(kind | if last { 0x80 } else { 0 });
    bytes.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
}

/// Writes a frame or sample number in the UTF-8-like coding of frame
/// headers, which runs on to seven bytes for 36 bits.
fn write_number(frame: &mut BitWriter, number: u64) {
    if number < 0x80 {
        frame.bits(number, 8);
        return;
    }
    // How many bytes carry six bits each after the first.
    let mut extra = 1;
    while extra < 6 && number >> (6 * extra + 6 - extra) != 0 {
        extra += 1;
    }
    let lead_bits = 6 - extra;
    frame.bits((1u64 << (extra + 1)) - 1, extra + 1);
    frame.bits(0, 1);
    frame.bits(number >> (6 * extra), lead_bits);
    for index in (0..extra).rev() {
        frame.bits(0b10, 2);
        frame.bits(number >> (6 * index), 6);
    }
}

/// A longer FLAC stream made from the frames of `bytes`, `times` over.
///
/// Frames of FLAC do not depend on one another, so a stream can be built by
/// copying them: here each copy gets the next frame number, and its two
/// checksums are worked out again. The last frame of `bytes` is left out,
/// being the only one that may be shorter than the rest. The result is as
/// long as a test needs while every sample in it was still compressed by
/// whoever wrote `bytes`.
///
/// Returns the stream and the number of samples each round adds.
pub fn repeat_flac(bytes: &[u8], times: usize, declare_total: bool) -> (Vec<u8>, usize) {
    let (audio_start, block_size, offsets) = flac_frame_offsets(bytes);
    let whole = offsets.len() - 1;
    let round = whole * block_size;

    let mut out = bytes[..audio_start].to_vec();
    // The length is the last 36 bits of the first 18 bytes of the stream
    // information, which starts at byte 8.
    let total = if declare_total {
        (round * times) as u64
    } else {
        0
    };
    out[21] = (out[21] & 0xF0) | (total >> 32) as u8;
    out[22..26].copy_from_slice(&(total as u32).to_be_bytes());

    for number in 0..(whole * times) as u64 {
        let index = number as usize % whole;
        let frame = &bytes[audio_start + offsets[index]..audio_start + offsets[index + 1]];
        // After four bytes of codes come the number, then a block size or a
        // sample rate if they are written out, then the header's checksum.
        let number_len = (frame[4].leading_ones() as usize).max(1);
        let block_bytes = match frame[2] >> 4 {
            6 => 1,
            7 => 2,
            _ => 0,
        };
        let rate_bytes = match frame[2] & 0x0F {
            12 => 1,
            13 | 14 => 2,
            _ => 0,
        };
        let rest = 4 + number_len;
        let header_end = rest + block_bytes + rate_bytes;

        let mut coded = BitWriter::default();
        write_number(&mut coded, number);
        let mut copy = frame[..4].to_vec();
        copy.extend_from_slice(&coded.bytes);
        copy.extend_from_slice(&frame[rest..header_end]);
        copy.push(crc8(&copy));
        copy.extend_from_slice(&frame[header_end + 1..frame.len() - 2]);
        let frame_crc = crc16(&copy);
        copy.extend_from_slice(&frame_crc.to_be_bytes());
        out.extend_from_slice(&copy);
    }
    (out, round)
}

/// Converts an integer sample of `bits` bits the way the decoder does.
pub fn flac_scale(sample: i32, bits: u32) -> f32 {
    (f64::from(sample) / (1u64 << (bits - 1)) as f64) as f32
}

/// A sample of `bits` bits for `(channel, index)`: noise, broken up by
/// stretches where a channel holds one value, so that a stream made from it
/// has both kinds of subframe and frames of very different sizes.
pub fn flac_sample(bits: u32, channel: usize, index: u64) -> i32 {
    // Every third stretch of 700 samples holds one value.
    let stretch = index / 700;
    let seed = if stretch % 3 == 0 { stretch } else { index };
    let mut hash = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((channel as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
    hash ^= hash >> 31;
    hash = hash.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash ^= hash >> 29;
    // Keep the top `bits` bits, with their sign.
    ((hash as i64) >> (64 - bits)) as i32
}

/// Where the frames of a FLAC stream with blocks of one size are: the
/// offset of the first frame, the block size, and each frame's offset from
/// the first.
///
/// A frame is taken to start where the two sync bytes are followed by a
/// header whose checksum matches and whose number is the next in line. This
/// is the test's own reading of the format, independent of the decoder.
pub fn flac_frame_offsets(bytes: &[u8]) -> (usize, usize, Vec<usize>) {
    assert_eq!(&bytes[..4], b"fLaC");
    let block_size = usize::from(u16::from_be_bytes([bytes[8], bytes[9]]));
    let mut position = 4;
    loop {
        let last = bytes[position] & 0x80 != 0;
        let len = u32::from_be_bytes([
            0,
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]);
        position += 4 + len as usize;
        if last {
            break;
        }
    }
    let audio_start = position;

    let mut offsets = Vec::new();
    while position + 6 < bytes.len() {
        if bytes[position] == 0xFF && bytes[position + 1] == 0xF8 {
            if let Some(number) = frame_number(&bytes[position..]) {
                if number == offsets.len() as u64 {
                    offsets.push(position - audio_start);
                }
            }
        }
        position += 1;
    }
    (audio_start, block_size, offsets)
}

/// The number in the frame header at the start of `bytes`, if the header's
/// checksum matches.
fn frame_number(bytes: &[u8]) -> Option<u64> {
    let first = *bytes.get(4)?;
    let extra = match first.leading_ones() {
        0 => 0,
        ones @ 2..=6 => ones as usize - 1,
        _ => return None,
    };
    let mut number = u64::from(first) & (0x7F >> (extra + usize::from(extra > 0)));
    for &byte in bytes.get(5..5 + extra)? {
        number = (number << 6) | u64::from(byte & 0x3F);
    }
    // A block size or a sample rate written out follows the number.
    let block_bytes = match bytes[2] >> 4 {
        6 => 1,
        7 => 2,
        _ => 0,
    };
    let rate_bytes = match bytes[2] & 0x0F {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    let end = 5 + extra + block_bytes + rate_bytes;
    (crc8(bytes.get(..end)?) == *bytes.get(end)?).then_some(number)
}

/// Inserts a seek table after the stream information of a FLAC stream.
/// Each point is the number of a frame's first sample, the frame's offset
/// from the first frame, and its length in samples.
pub fn with_seek_table(bytes: &[u8], points: &[(u64, u64, u16)]) -> Vec<u8> {
    // The marker, a block header and the 34 bytes of stream information.
    let info_end = 4 + 4 + 34;
    let info_was_last = bytes[4] & 0x80 != 0;
    let mut out = bytes[..info_end].to_vec();
    out[4] &= 0x7F;
    write_block_header(&mut out, 3, info_was_last, points.len() * 18);
    for &(sample, offset, len) in points {
        out.extend_from_slice(&sample.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(&bytes[info_end..]);
    out
}
