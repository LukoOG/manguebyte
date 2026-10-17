//! The MP3 encoder: MPEG-1 layer III (ISO/IEC 11172-3) at 32, 44.1 and
//! 48 kHz, one or two channels.
//!
//! Each granule of 576 samples goes through the analysis filter bank of
//! [`analysis`] as a long block, or as three short ones where the
//! transient detector of the crate's `psy` module finds an attack that a
//! long block would smear into pre-echo, with the start and stop blocks
//! that join them. The masking model of `psy` gives the noise each
//! scalefactor band may hold, and [`quantize`] finds the coarsest steps
//! that keep the noise there, then the Huffman tables that code the
//! values in the fewest bits.
//!
//! At a variable bitrate the noise is what the quality sets, and each
//! frame takes the lowest bitrate its bits fit, with the bit reservoir. At
//! a constant bitrate the noise is the least that fits the frame's share
//! of the bits, found by bisection, the reservoir lending what one frame
//! needs more than another. Two channels are coded as mid and side when
//! that takes fewer bits, frame by frame.
//!
//! A first frame of silence carries a Xing (or, at a constant bitrate,
//! Info) header and a LAME tag: the number of frames, the bytes, a table
//! for seeking, and the samples of delay and padding that a decoder
//! removes to give back exactly the samples encoded, as this crate's
//! decoder and FFmpeg do.

mod analysis;
mod quantize;

use std::collections::VecDeque;
use std::io::{self, Seek, SeekFrom, Write};

use manguebyte_core::AudioBuffer;

use self::analysis::{Analysis, BlockType, DELAY, to_band_order};
use self::quantize::{
    Coded, Partition, Prepared, choose_scalefac_compress, partitions, quantize, write_main_data,
};
use super::Mp3Error;
use super::tables::{BITRATES, SHORT_BANDS};
use crate::metadata::{Metadata, write_id3v2};
use crate::psy::{HOP, Model, Transients};

/// The bitrates of MPEG-1 layer III in kbit/s, by index.
const KBPS: [u16; 15] = BITRATES[0];

/// The samples the decoder gives before the first one encoded, beyond the
/// delay the LAME tag states: what decoders add to that delay.
const DECODER_DELAY: usize = 529;

/// The delay the LAME tag states, so that with the decoder's own the
/// first sample given is the first encoded.
const STATED_DELAY: usize = DELAY - DECODER_DELAY;

/// The most bytes of main data a frame can start before itself.
const RESERVOIR: u64 = 511;

/// The most bits one granule of one channel can hold.
const MAX_PART2_3: u32 = 4_095;

/// The vendor name in the LAME tag. Decoders read the delay and padding of
/// a LAME tag only when its name starts with `LAME`; the five letters after
/// name this encoder.
const TAG_NAME: &[u8; 9] = b"LAMEmbyte";

/// The bitrate of an MP3 stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mp3Bitrate {
    /// Every frame at this many kbit/s: 32, 40, 48, 56, 64, 80, 96, 112,
    /// 128, 160, 192, 224, 256 or 320.
    Constant(u32),
    /// Each frame at the bitrate the quality needs, from 0 to 10, whole or
    /// not: higher is better and larger.
    Variable(f32),
}

/// How to write an MP3 stream.
///
/// ```
/// use manguebyte_formats::mp3::{Mp3Bitrate, Mp3Options};
///
/// let options = Mp3Options::constant(192).with_tag("TPE2", "Mestre AmbrÃ³sio");
/// assert_eq!(options.bitrate(), Mp3Bitrate::Constant(192));
/// assert_eq!(Mp3Options::default().bitrate(), Mp3Bitrate::Variable(5.0));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Mp3Options {
    bitrate: Mp3Bitrate,
    joint_stereo: bool,
    metadata: Metadata,
}

impl Default for Mp3Options {
    /// A variable bitrate at quality 5, joint stereo.
    fn default() -> Self {
        Self::variable(5.0)
    }
}

impl Mp3Options {
    /// A constant bitrate of `kbps` kbit/s.
    pub fn constant(kbps: u32) -> Self {
        Self {
            bitrate: Mp3Bitrate::Constant(kbps),
            joint_stereo: true,
            metadata: Metadata::default(),
        }
    }

    /// A variable bitrate at `quality`, from 0 to 10.
    ///
    /// The quality sets the noise against the masking threshold of the
    /// encoder's model, by 3 dB a step: 6 dB above it at 0, 9 dB under it
    /// at 5, 24 dB under it at 10, as far as 320 kbit/s and the reservoir
    /// allow. On the stereo test signal of this crate at 44.1 kHz that
    /// comes to about 56 kbit/s at 0, 98 at 5 and 120 at 10, whose white
    /// noise no bitrate codes 24 dB under the threshold.
    pub fn variable(quality: f32) -> Self {
        Self {
            bitrate: Mp3Bitrate::Variable(quality),
            ..Self::constant(128)
        }
    }

    /// Whether two channels may be coded as mid and side, which a frame is
    /// when that takes fewer bits; on by default. Off, every frame is plain
    /// stereo.
    pub fn with_joint_stereo(mut self, joint: bool) -> Self {
        self.joint_stereo = joint;
        self
    }

    /// Writes `metadata` into an ID3v2.4 tag before the audio: what of it
    /// ID3 has frames for (see the [`metadata`](crate::metadata) module).
    pub fn with_metadata(mut self, metadata: &Metadata) -> Self {
        self.metadata = metadata.clone();
        self
    }

    /// Adds a tag: the text frame `name` names when it is one, such as
    /// `TPE2`, and a `TXXX` frame described by `name` otherwise.
    pub fn with_tag(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.tags.push((name.into(), value.into()));
        self
    }

    /// The bitrate.
    pub fn bitrate(&self) -> Mp3Bitrate {
        self.bitrate
    }

    /// Whether mid and side coding is allowed.
    pub fn joint_stereo(&self) -> bool {
        self.joint_stereo
    }

    /// The metadata of the ID3v2 tag.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Checks the options with the channels and rate they are for: one or
    /// two channels at 32, 44.1 or 48 kHz, a constant bitrate MPEG-1 layer
    /// III has or a quality from 0 to 10, and a tag ID3v2 can hold.
    pub fn validate(&self, channels: usize, sample_rate: u32) -> Result<(), Mp3Error> {
        if !(1..=2).contains(&channels) {
            return Err(Mp3Error::Unsupported("MP3 holds one or two channels"));
        }
        if rate_index(sample_rate).is_none() {
            return Err(Mp3Error::Unsupported(
                "this encoder writes MPEG-1 at 32, 44.1 or 48 kHz",
            ));
        }
        match self.bitrate {
            Mp3Bitrate::Constant(kbps) => {
                if bitrate_index(kbps).is_none() {
                    return Err(Mp3Error::Unsupported(
                        "a bitrate MPEG-1 layer III does not have",
                    ));
                }
            }
            Mp3Bitrate::Variable(quality) => {
                if !(0.0..=10.0).contains(&quality) {
                    return Err(Mp3Error::Unsupported("a quality outside 0 to 10"));
                }
            }
        }
        write_id3v2(&self.metadata).map_err(Mp3Error::Unsupported)?;
        Ok(())
    }
}

fn rate_index(sample_rate: u32) -> Option<usize> {
    [44_100, 48_000, 32_000]
        .iter()
        .position(|&rate| rate == sample_rate)
}

fn bitrate_index(kbps: u32) -> Option<usize> {
    (1..15).find(|&index| u32::from(KBPS[index]) == kbps)
}

/// Packs bits most significant first, as MPEG audio does.
#[derive(Debug, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used of the last byte, 0 meaning a new byte is needed.
    used: u32,
}

impl BitWriter {
    pub(super) fn write(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let last = self.bytes.last_mut().expect("a byte was pushed");
            *last |= (((value >> bit) & 1) as u8) << (7 - self.used);
            self.used = (self.used + 1) % 8;
        }
    }

    fn bit_len(&self) -> u64 {
        let whole = self.bytes.len() as u64 * 8;
        if self.used == 0 {
            whole
        } else {
            whole - 8 + u64::from(self.used)
        }
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.used = 0;
    }
}

/// CRC-16 with the polynomial 0x8005, reflected, from zero (CRC-16/ARC):
/// the checksums of the LAME tag.
fn crc16_arc(mut crc: u16, bytes: &[u8]) -> u16 {
    for &byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// What one granule brings to its frame.
struct Granule {
    block_type: BlockType,
    /// By channel: the lines in the order they are coded, and the noise
    /// each partition may hold.
    lines: Vec<[f32; 576]>,
    allowed: Vec<Vec<f32>>,
}

/// A frame whose main data may not all be in it yet.
struct PendingFrame {
    bytes: Vec<u8>,
    /// Where its main data begins in it, and where that is in the stream of
    /// main data of every frame.
    main_offset: usize,
    main_start: u64,
}

/// Encodes an MP3 stream as samples arrive.
pub(crate) struct Mp3Stream {
    channels: usize,
    sample_rate: u32,
    rate_index: usize,
    bitrate: Mp3Bitrate,
    joint_stereo: bool,
    /// The noise allowed against the model's threshold, as a power factor,
    /// at a variable bitrate; at a constant one, what the last frame took,
    /// in tens of decibels.
    allowance: f32,
    last_log: f32,
    analysis: Vec<Analysis>,
    long_model: Model,
    short_model: Model,
    long_partitions: Vec<Partition>,
    short_partitions: Vec<Partition>,
    /// The samples of each channel from `input_start` on.
    input: Vec<Vec<f32>>,
    input_start: u64,
    received: u64,
    transients: Transients,
    hops: Vec<bool>,
    hop_base: u64,
    hop_fill: usize,
    /// Granules analysed, and the block type of the last.
    granules: u64,
    last_type: BlockType,
    /// Frames of audio made.
    coded_frames: u64,
    ended: bool,
    /// The bytes of main data all frames so far have room for, and the
    /// bytes of it used, stuffing included.
    main_capacity: u64,
    main_written: u64,
    pending: VecDeque<PendingFrame>,
    /// The fraction of a byte the frames so far are short of the bitrate.
    lag: u32,
    /// Where the frame of the Xing header starts in the destination, its
    /// length and bitrate index; the bytes of the stream from it on.
    info_at: Option<u64>,
    info_len: usize,
    info_bitrate: usize,
    stream_bytes: u64,
    music_crc: u16,
    /// Frames of audio handed to the output.
    emitted: u64,
    /// Where every `toc_stride`-th frame starts, from the Xing frame on.
    toc: Vec<u64>,
    toc_stride: u64,
    out: Vec<u8>,
    retry_at: Option<u64>,
    bits: BitWriter,
}

/// The most frame positions kept for the table of contents.
const TOC_POINTS: usize = 1_024;

impl Mp3Stream {
    /// Starts a stream: checks the options and writes the ID3v2 tag and the
    /// frame of the Xing header, to be completed by `finish`.
    pub(crate) fn start<W: Write + Seek>(
        writer: &mut W,
        channels: usize,
        sample_rate: u32,
        options: &Mp3Options,
    ) -> Result<Self, Mp3Error> {
        options.validate(channels, sample_rate)?;
        let rate_index = rate_index(sample_rate).expect("validated");
        let allowance = match options.bitrate {
            Mp3Bitrate::Variable(quality) => 10f32.powf((6.0 - 3.0 * quality) / 10.0),
            Mp3Bitrate::Constant(_) => 1.0,
        };
        let full_scale_long = full_scale(sample_rate, 0);
        let full_scale_short = full_scale(sample_rate, 2);
        let mut stream = Self {
            channels,
            sample_rate,
            rate_index,
            bitrate: options.bitrate,
            joint_stereo: options.joint_stereo && channels == 2,
            allowance,
            last_log: 0.0,
            analysis: (0..channels).map(|_| Analysis::new()).collect(),
            long_model: Model::new(1_152, sample_rate, full_scale_long),
            short_model: Model::new(384, sample_rate, full_scale_short),
            long_partitions: partitions(rate_index, false),
            short_partitions: partitions(rate_index, true),
            input: vec![Vec::with_capacity(4 * 576); channels],
            input_start: 0,
            received: 0,
            transients: Transients::new(channels),
            hops: Vec::new(),
            hop_base: 0,
            hop_fill: 0,
            granules: 0,
            last_type: 0,
            coded_frames: 0,
            ended: false,
            main_capacity: 0,
            main_written: 0,
            pending: VecDeque::new(),
            lag: 0,
            info_at: None,
            info_len: 0,
            info_bitrate: 0,
            stream_bytes: 0,
            music_crc: 0,
            emitted: 0,
            toc: Vec::with_capacity(TOC_POINTS),
            toc_stride: 1,
            out: Vec::new(),
            retry_at: None,
            bits: BitWriter::default(),
        };
        if let Some(tag) = write_id3v2(&options.metadata).map_err(Mp3Error::Unsupported)? {
            stream.out.extend_from_slice(&tag);
        }
        let start = writer.stream_position()?;
        stream.info_at = Some(start + stream.out.len() as u64);
        let info = stream.info_frame(0);
        stream.info_len = info.len();
        stream.stream_bytes = info.len() as u64;
        stream.out.extend_from_slice(&info);
        stream.flush(writer)?;
        Ok(stream)
    }

    /// Frames taken so far.
    pub(crate) fn frames(&self) -> u64 {
        self.received
    }

    /// Takes the frames of `buffer` and writes the frames they complete.
    pub(crate) fn write_planar<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        buffer: &AudioBuffer<f32>,
    ) -> io::Result<()> {
        self.flush(writer)?;
        let frames = buffer.num_frames();
        let mut from = 0;
        while from < frames {
            let count = (frames - from).min(1_152);
            for (channel, input) in self.input.iter_mut().enumerate() {
                input.extend(
                    buffer.channel(channel)[from..from + count]
                        .iter()
                        .map(|&s| clean(s)),
                );
            }
            self.took(count);
            from += count;
            self.pump(false);
            self.flush(writer)?;
        }
        Ok(())
    }

    /// The same for interleaved samples.
    pub(crate) fn write_interleaved<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        samples: &[f32],
    ) -> io::Result<()> {
        self.flush(writer)?;
        let channels = self.channels;
        for chunk in samples.chunks(1_152 * channels) {
            for (channel, input) in self.input.iter_mut().enumerate() {
                input.extend(
                    chunk
                        .iter()
                        .skip(channel)
                        .step_by(channels)
                        .map(|&s| clean(s)),
                );
            }
            self.took(chunk.len() / channels);
            self.pump(false);
            self.flush(writer)?;
        }
        Ok(())
    }

    /// The same for silence.
    pub(crate) fn write_silence<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        frames: u64,
    ) -> io::Result<()> {
        self.flush(writer)?;
        let mut left = frames;
        while left > 0 {
            let count = left.min(1_152) as usize;
            for input in &mut self.input {
                input.resize(input.len() + count, 0.0);
            }
            self.took(count);
            left -= count as u64;
            self.pump(false);
            self.flush(writer)?;
        }
        Ok(())
    }

    /// Encodes what is left, writes the last frames and completes the
    /// Xing header.
    pub(crate) fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        self.flush(writer)?;
        if !self.ended {
            self.pump(true);
            while let Some(frame) = self.pending.pop_front() {
                self.emit(frame.bytes);
            }
            self.ended = true;
        }
        self.flush(writer)?;
        if let Some(at) = self.info_at {
            let end = writer.stream_position()?;
            let info = self.info_frame(self.coded_frames);
            writer.seek(SeekFrom::Start(at))?;
            writer.write_all(&info)?;
            writer.seek(SeekFrom::Start(end))?;
        }
        writer.flush()
    }

    /// Counts the samples just put into the input, and runs the transient
    /// detector over them.
    fn took(&mut self, count: usize) {
        let end = self.input[0].len();
        for index in end - count..end {
            for channel in 0..self.channels {
                self.transients.add(channel, self.input[channel][index]);
            }
            self.hop_fill += 1;
            if self.hop_fill == HOP {
                self.hop_fill = 0;
                let flag = self.transients.end_hop();
                self.hops.push(flag);
            }
        }
        self.received += count as u64;
    }

    /// Whether a hop of the stretch `from..to` of the stream is a transient.
    fn transient_in(&self, from: i64, to: i64) -> bool {
        let hop = |at: i64| (at.max(0) as u64 / HOP as u64).saturating_sub(self.hop_base) as usize;
        let (first, last) = (hop(from), hop(to + HOP as i64 - 1));
        self.hops
            .get(first..last.min(self.hops.len()))
            .is_some_and(|hops| hops.iter().any(|&flag| flag))
    }

    /// Whether granule `g` is to be coded as short blocks: an attack in
    /// what its long block would spread over, where the decoder gives the
    /// samples from `576 g - 1057` on.
    fn wants_short(&self, granule: u64) -> bool {
        let start = 576 * granule as i64 - DELAY as i64;
        self.transient_in(start + 200, start + 1_152 - 200)
    }

    /// The frames the stream has once it ends: enough for the decoder to
    /// give every sample, after its delay.
    fn total_frames(&self) -> u64 {
        (self.received + DELAY as u64).div_ceil(1_152)
    }

    /// Encodes every frame the samples taken allow, or, when `ending`, every
    /// frame up to the end of the stream.
    fn pump(&mut self, ending: bool) {
        loop {
            if ending {
                if self.coded_frames >= self.total_frames() {
                    return;
                }
            } else if self.received < 576 * (self.granules + 3) {
                // Two granules and the one after, whose block type the
                // second's depends on.
                return;
            }
            let first = self.granule();
            let second = self.granule();
            self.encode_frame([first, second]);
            self.coded_frames += 1;
        }
    }

    /// The sample of `channel` at `index`, zero past what was taken.
    fn sample(&self, channel: usize, index: u64) -> f32 {
        if index >= self.received || index < self.input_start {
            return 0.0;
        }
        self.input[channel]
            .get((index - self.input_start) as usize)
            .copied()
            .unwrap_or(0.0)
    }

    /// Analyses the next granule.
    fn granule(&mut self) -> Granule {
        let g = self.granules;
        let short = self.wants_short(g);
        let next_short = self.wants_short(g + 1);
        let block_type = match (short, next_short, self.last_type) {
            (true, _, _) => 2,
            // Between short blocks, a long one could not both stop and start
            // them.
            (false, true, 2) => 2,
            (false, true, _) => 1,
            (false, false, 2) => 3,
            _ => 0,
        };
        self.last_type = block_type;
        let mut lines = Vec::with_capacity(self.channels);
        let mut allowed = Vec::with_capacity(self.channels);
        for channel in 0..self.channels {
            let mut samples = [0.0f32; 576];
            for (i, sample) in samples.iter_mut().enumerate() {
                *sample = self.sample(channel, 576 * g + i as u64);
            }
            let analysis = &mut self.analysis[channel];
            analysis.filter(&samples);
            let mut spectrum = [0.0f32; 576];
            analysis.transform(block_type, &mut spectrum);
            let (partitions, allowance) = if block_type == 2 {
                // The model, window by window, on the lines in the order of
                // their frequencies.
                let mut threshold = [0.0f32; 576];
                for window in 0..3 {
                    let mut x = [0.0f32; 192];
                    for (f, value) in x.iter_mut().enumerate() {
                        *value = spectrum[(f / 6) * 18 + window * 6 + f % 6];
                    }
                    let mut out = [0.0f32; 192];
                    self.short_model.threshold(&x, &mut out);
                    for (f, &value) in out.iter().enumerate() {
                        threshold[(f / 6) * 18 + window * 6 + f % 6] = value;
                    }
                }
                to_band_order(&mut spectrum, &SHORT_BANDS[self.rate_index]);
                to_band_order(&mut threshold, &SHORT_BANDS[self.rate_index]);
                (&self.short_partitions, threshold)
            } else {
                let mut threshold = [0.0f32; 576];
                self.long_model.threshold(&spectrum, &mut threshold);
                (&self.long_partitions, threshold)
            };
            allowed.push(
                partitions
                    .iter()
                    .map(|p| allowance[p.start..p.end].iter().sum())
                    .collect(),
            );
            lines.push(spectrum);
        }
        self.granules += 1;
        // The samples no granule will read again, and their transient flags.
        let keep = 576 * self.granules;
        if keep > self.input_start + 4 * 576 {
            let drop = (keep - self.input_start).min(self.input[0].len() as u64) as usize;
            for input in &mut self.input {
                input.drain(..drop);
            }
            self.input_start += drop as u64;
            let hop = (self.input_start.saturating_sub(2 * 1_152)) / HOP as u64;
            if hop > self.hop_base + 64 {
                let drop = ((hop - self.hop_base) as usize).min(self.hops.len());
                self.hops.drain(..drop);
                self.hop_base += drop as u64;
            }
        }
        Granule {
            block_type,
            lines,
            allowed,
        }
    }

    /// Codes a frame of two granules.
    fn encode_frame(&mut self, granules: [Granule; 2]) {
        let channels = self.channels;
        let partitions_of = |block_type: BlockType| {
            if block_type == 2 {
                &self.short_partitions
            } else {
                &self.long_partitions
            }
        };
        let prepare = |mid_side: bool| -> Vec<Vec<Prepared>> {
            granules
                .iter()
                .map(|granule| {
                    let partitions = partitions_of(granule.block_type);
                    if mid_side {
                        let half = std::f32::consts::FRAC_1_SQRT_2;
                        let (left, right) = (&granule.lines[0], &granule.lines[1]);
                        let mut mid = [0.0f32; 576];
                        let mut side = [0.0f32; 576];
                        for i in 0..576 {
                            mid[i] = (left[i] + right[i]) * half;
                            side[i] = (left[i] - right[i]) * half;
                        }
                        let allowed: Vec<f32> = granule.allowed[0]
                            .iter()
                            .zip(&granule.allowed[1])
                            .map(|(&l, &r)| l.min(r))
                            .collect();
                        vec![
                            Prepared::new(&mid, allowed.clone(), granule.block_type, partitions),
                            Prepared::new(&side, allowed, granule.block_type, partitions),
                        ]
                    } else {
                        (0..channels)
                            .map(|channel| {
                                Prepared::new(
                                    &granule.lines[channel],
                                    granule.allowed[channel].clone(),
                                    granule.block_type,
                                    partitions,
                                )
                            })
                            .collect()
                    }
                })
                .collect()
        };
        let code = |prepared: &[Vec<Prepared>], allowance: f32| -> (Vec<Vec<Coded>>, u64, bool) {
            let mut total = 0u64;
            let mut fits = true;
            let coded: Vec<Vec<Coded>> = prepared
                .iter()
                .map(|channels| {
                    channels
                        .iter()
                        .map(|p| {
                            let coded = quantize(p, partitions_of(p.block_type), allowance);
                            total += u64::from(coded.part2_3_length());
                            fits &= coded.part2_3_length() <= MAX_PART2_3;
                            coded
                        })
                        .collect()
                })
                .collect();
            (coded, total, fits)
        };

        let reservoir = (self.main_capacity - self.main_written).min(RESERVOIR);
        let side_len = if channels == 1 { 17 } else { 32 };
        let slots = |index: usize, lag: u32| -> u64 {
            let (len, _) = frame_len(index, self.sample_rate, lag);
            (len - 4 - side_len) as u64
        };
        let lr = prepare(false);
        let ms = self.joint_stereo.then(|| prepare(true));
        // At a variable bitrate the noise starts at what the quality allows;
        // at a constant one, at what the frame before took.
        let start_log = match self.bitrate {
            Mp3Bitrate::Variable(_) => self.allowance.log10(),
            Mp3Bitrate::Constant(_) => self.last_log,
        };
        let start = 10f32.powf(start_log);
        // Mid and side when that is the cheaper, at the noise to start from.
        let lr_first = code(&lr, start);
        let (prepared, mid_side, first) = match ms {
            Some(ms) => {
                let ms_first = code(&ms, start);
                if ms_first.1 < lr_first.1 {
                    (ms, true, ms_first)
                } else {
                    (lr, false, lr_first)
                }
            }
            None => (lr, false, lr_first),
        };

        // The noise, and the bitrate.
        let (budget, target, fixed) = match self.bitrate {
            Mp3Bitrate::Constant(kbps) => {
                let index = bitrate_index(kbps).expect("validated");
                let room = slots(index, self.lag);
                (
                    8 * (room + reservoir),
                    8 * (room + reservoir / 2),
                    Some(index),
                )
            }
            Mp3Bitrate::Variable(_) => (8 * (slots(14, self.lag) + reservoir), u64::MAX, None),
        };
        // The least noise that fits, in decibels against the threshold: at
        // a variable bitrate from what the quality allows up, at a constant
        // one anywhere from 40 dB under to 60 dB over. From the start, steps
        // that double from 5 dB, down while the frame fits and up while it
        // does not, then bisection between the last two.
        let limit = target.min(budget);
        let fits = |result: &(Vec<Vec<Coded>>, u64, bool)| result.2 && result.1 <= limit;
        let (floor, ceiling) = match self.bitrate {
            Mp3Bitrate::Variable(_) => (start_log, 6.0f32),
            Mp3Bitrate::Constant(_) => (-4.0f32, 6.0),
        };
        let mut fit: Option<(f32, Vec<Vec<Coded>>)> = None;
        let mut miss: Option<f32> = None;
        if fits(&first) {
            fit = Some((start_log, first.0));
        } else {
            miss = Some(start_log);
        }
        let mut step = 0.5f32;
        loop {
            let next = match (&fit, miss) {
                (Some((at, _)), None) if *at > floor => (*at - step).max(floor),
                (None, Some(at)) if at < ceiling => (at + step).min(ceiling),
                _ => break,
            };
            let result = code(&prepared, 10f32.powf(next));
            if fits(&result) {
                fit = Some((next, result.0));
            } else {
                miss = Some(next);
            }
            step *= 2.0;
        }
        if let (Some(_), Some(_)) = (&fit, miss) {
            for _ in 0..6 {
                let (good, bad) = (fit.as_ref().map_or(ceiling, |f| f.0), miss.unwrap_or(floor));
                let middle = 0.5 * (good + bad);
                let result = code(&prepared, 10f32.powf(middle));
                if fits(&result) {
                    fit = Some((middle, result.0));
                } else {
                    miss = Some(middle);
                }
            }
        }
        let mut coded = match fit {
            Some((at, coded)) => {
                if matches!(self.bitrate, Mp3Bitrate::Constant(_)) {
                    self.last_log = at;
                }
                coded
            }
            // Nothing fits: silence does.
            None => code(&prepared, f32::INFINITY).0,
        };

        // Scalefactors the second granule shares with the first.
        let mut scfsi = [[false; 4]; 2];
        for channel in 0..channels {
            let (first, second) = (&coded[0][channel], &coded[1][channel]);
            if first.block_type == 2 || second.block_type == 2 {
                continue;
            }
            for (group, bands) in [0..6, 6..11, 11..16, 16..21].into_iter().enumerate() {
                scfsi[channel][group] =
                    first.scalefactors[bands.clone()] == second.scalefactors[bands];
            }
            let sent = scfsi[channel].map(|shared| !shared);
            choose_scalefac_compress(&mut coded[1][channel], sent);
        }
        let bits: u64 = coded
            .iter()
            .flatten()
            .map(|c| u64::from(c.part2_3_length()))
            .sum();
        let bytes = bits.div_ceil(8);

        let index = fixed.unwrap_or_else(|| {
            (1..15)
                .find(|&index| slots(index, self.lag) + reservoir >= bytes)
                .unwrap_or(14)
        });
        let (len, padding) = frame_len(index, self.sample_rate, self.lag);
        self.lag = next_lag(index, self.sample_rate, self.lag);

        // The main data, and the frame.
        self.bits.clear();
        for (granule, channels_coded) in coded.iter().enumerate() {
            for (channel, c) in channels_coded.iter().enumerate() {
                let sent = if granule == 1 {
                    scfsi[channel].map(|shared| !shared)
                } else {
                    [true; 4]
                };
                let before = self.bits.bit_len();
                write_main_data(&mut self.bits, c, sent, self.rate_index);
                debug_assert_eq!(self.bits.bit_len() - before, u64::from(c.part2_3_length()));
            }
        }
        let mut frame = vec![0u8; len];
        let mode_extension = if mid_side { 2 } else { 0 };
        write_header(
            &mut frame,
            index,
            self.rate_index,
            padding,
            channels,
            self.joint_stereo,
            mode_extension,
        );
        let mut side = BitWriter::default();
        side.write(reservoir as u32, 9);
        side.write(0, if channels == 1 { 5 } else { 3 });
        for shared in scfsi.iter().take(channels) {
            for &flag in shared {
                side.write(u32::from(flag), 1);
            }
        }
        for channels_coded in &coded {
            for c in channels_coded {
                write_side_info(&mut side, c);
            }
        }
        frame[4..4 + side_len].copy_from_slice(&side.bytes);

        // Main data goes where the reservoir left room, maybe in frames
        // before this one; the room past 511 bytes back is stuffing.
        let main_start = self.main_capacity;
        self.main_written = self.main_written.max(self.main_capacity - reservoir);
        self.main_capacity += (len - 4 - side_len) as u64;
        self.pending.push_back(PendingFrame {
            bytes: frame,
            main_offset: 4 + side_len,
            main_start,
        });
        let data = std::mem::take(&mut self.bits.bytes);
        let mut at = self.main_written;
        for pending in self.pending.iter_mut() {
            let room = pending.bytes.len() - pending.main_offset;
            let (from, to) = (pending.main_start, pending.main_start + room as u64);
            if at >= to || at - self.main_written >= data.len() as u64 {
                continue;
            }
            let start = at.max(from);
            let done = (start - self.main_written) as usize;
            let count = ((to - start) as usize).min(data.len() - done);
            let offset = pending.main_offset + (start - from) as usize;
            pending.bytes[offset..offset + count].copy_from_slice(&data[done..done + count]);
            at = start + count as u64;
        }
        debug_assert_eq!(at, self.main_written + data.len() as u64);
        self.main_written += data.len() as u64;
        self.bits.bytes = data;
        // Frames no main data will go into again.
        while let Some(front) = self.pending.front() {
            let room = (front.bytes.len() - front.main_offset) as u64;
            if front.main_start + room > self.main_written {
                break;
            }
            let frame = self.pending.pop_front().expect("a frame");
            self.emit(frame.bytes);
        }
    }

    /// Hands a finished frame to the output, counting it.
    fn emit(&mut self, bytes: Vec<u8>) {
        let number = self.emitted;
        if number % self.toc_stride == 0 {
            if self.toc.len() == TOC_POINTS {
                // Every other point, twice as far apart.
                let kept: Vec<u64> = self.toc.iter().copied().step_by(2).collect();
                self.toc = kept;
                self.toc_stride *= 2;
            }
            if number % self.toc_stride == 0 {
                self.toc.push(self.stream_bytes);
            }
        }
        self.music_crc = crc16_arc(self.music_crc, &bytes);
        self.stream_bytes += bytes.len() as u64;
        self.out.extend_from_slice(&bytes);
        self.emitted += 1;
    }

    /// The frame of the Xing or Info header and the LAME tag, for a stream
    /// of `frames` frames of audio.
    fn info_frame(&mut self, frames: u64) -> Vec<u8> {
        let side_len = if self.channels == 1 { 17 } else { 32 };
        let at = 4 + side_len;
        // The headers, and the 190 bytes the checksum of the LAME tag
        // covers.
        let needed = (at + 120 + 36).max(192);
        if self.info_bitrate == 0 {
            let lowest = match self.bitrate {
                Mp3Bitrate::Constant(kbps) => bitrate_index(kbps).expect("validated"),
                Mp3Bitrate::Variable(_) => 1,
            };
            self.info_bitrate = (lowest..15)
                .find(|&index| frame_len(index, self.sample_rate, u32::MAX).0 >= needed)
                .unwrap_or(14);
        }
        let (len, _) = frame_len(self.info_bitrate, self.sample_rate, u32::MAX);
        let mut frame = vec![0u8; len];
        write_header(
            &mut frame,
            self.info_bitrate,
            self.rate_index,
            false,
            self.channels,
            self.joint_stereo,
            0,
        );
        let tag = &mut frame[at..];
        tag[..4].copy_from_slice(match self.bitrate {
            Mp3Bitrate::Constant(_) => b"Info",
            Mp3Bitrate::Variable(_) => b"Xing",
        });
        tag[4..8].copy_from_slice(&0x0Fu32.to_be_bytes());
        tag[8..12].copy_from_slice(&(frames.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        let bytes = self.stream_bytes.min(u64::from(u32::MAX)) as u32;
        tag[12..16].copy_from_slice(&bytes.to_be_bytes());
        // The table of contents: where each hundredth of the stream starts,
        // in 256ths of its bytes.
        for (i, entry) in tag[16..116].iter_mut().enumerate() {
            let frame_number = i as u64 * frames / 100;
            let point =
                ((frame_number / self.toc_stride) as usize).min(self.toc.len().saturating_sub(1));
            let position = self.toc.get(point).copied().unwrap_or(0);
            *entry = if bytes == 0 {
                0
            } else {
                (position * 256 / u64::from(bytes)).min(255) as u8
            };
        }
        let quality = match self.bitrate {
            Mp3Bitrate::Constant(_) => 100,
            Mp3Bitrate::Variable(quality) => (10.0 * quality).round() as u32,
        };
        tag[116..120].copy_from_slice(&quality.to_be_bytes());
        // The LAME tag.
        let lame = &mut tag[120..156];
        lame[..9].copy_from_slice(TAG_NAME);
        lame[9] = match self.bitrate {
            Mp3Bitrate::Constant(_) => 1,
            Mp3Bitrate::Variable(_) => 4,
        };
        lame[20] = match self.bitrate {
            Mp3Bitrate::Constant(kbps) => kbps.min(255) as u8,
            Mp3Bitrate::Variable(_) => 0,
        };
        let total = frames * 1_152;
        let samples = self.received;
        let padding = total
            .saturating_sub(STATED_DELAY as u64 + samples)
            .min(4_095) as u32;
        let delay = STATED_DELAY as u32;
        lame[21] = (delay >> 4) as u8;
        lame[22] = ((delay & 0x0F) << 4) as u8 | (padding >> 8) as u8;
        lame[23] = padding as u8;
        // The source rate.
        lame[24] = match self.sample_rate {
            44_100 => 1 << 6,
            48_000 => 2 << 6,
            _ => 0,
        };
        lame[28..32].copy_from_slice(&bytes.to_be_bytes());
        lame[32..34].copy_from_slice(&self.music_crc.to_be_bytes());
        let crc = crc16_arc(0, &frame[..190]);
        let tag_crc = at + 120 + 34;
        frame[tag_crc..tag_crc + 2].copy_from_slice(&crc.to_be_bytes());
        frame
    }

    /// Writes the bytes finished, or what is left of them after a write
    /// that failed, which is written again from where it began.
    fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let start = match self.retry_at {
            Some(at) => {
                writer.seek(SeekFrom::Start(at))?;
                at
            }
            None => writer.stream_position()?,
        };
        match writer.write_all(&self.out) {
            Ok(()) => {
                self.out.clear();
                self.retry_at = None;
                Ok(())
            }
            Err(error) => {
                self.retry_at = Some(start);
                Err(error)
            }
        }
    }
}

/// A sample as the encoder takes it: NaN as silence, the rest held to a
/// range the filter bank carries through.
#[inline]
fn clean(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        sample.clamp(-1e4, 1e4)
    }
}

/// The length of a frame at bitrate `index` and whether it is padded,
/// given the fraction of a byte the frames before are short, `lag`
/// (in units of the sample rate): 144 bitrate / rate bytes, and a byte
/// more when the fractions add up to one (2.4.3.1). With `lag` at its
/// largest, the frame is not padded.
fn frame_len(index: usize, sample_rate: u32, lag: u32) -> (usize, bool) {
    let numerator = 144 * u32::from(KBPS[index]) * 1_000;
    let whole = (numerator / sample_rate) as usize;
    let rest = numerator % sample_rate;
    let padded = lag != u32::MAX && lag + rest >= sample_rate;
    (whole + usize::from(padded), padded)
}

/// The lag after a frame of bitrate `index`.
fn next_lag(index: usize, sample_rate: u32, lag: u32) -> u32 {
    let rest = (144 * u32::from(KBPS[index]) * 1_000) % sample_rate;
    (lag + rest) % sample_rate
}

/// Writes a frame header: MPEG-1 layer III without a CRC.
fn write_header(
    frame: &mut [u8],
    bitrate: usize,
    rate_index: usize,
    padding: bool,
    channels: usize,
    joint: bool,
    mode_extension: u8,
) {
    let mode = match (channels, joint) {
        (1, _) => 3u8,
        (_, true) => 1,
        _ => 0,
    };
    frame[0] = 0xFF;
    frame[1] = 0xFB;
    frame[2] = (bitrate as u8) << 4 | (rate_index as u8) << 2 | u8::from(padding) << 1;
    frame[3] = mode << 6 | mode_extension << 4;
}

/// Writes the side information of one granule of one channel (2.4.1.7).
fn write_side_info(bits: &mut BitWriter, coded: &Coded) {
    bits.write(coded.part2_3_length(), 12);
    bits.write(coded.big_values as u32, 9);
    bits.write(coded.global_gain as u32, 8);
    bits.write(coded.scalefac_compress as u32, 4);
    let switching = coded.block_type != 0;
    bits.write(u32::from(switching), 1);
    if switching {
        bits.write(u32::from(coded.block_type), 2);
        bits.write(0, 1);
        bits.write(u32::from(coded.table_select[0]), 5);
        bits.write(u32::from(coded.table_select[1]), 5);
        for _ in 0..3 {
            bits.write(0, 3);
        }
    } else {
        for &table in &coded.table_select {
            bits.write(u32::from(table), 5);
        }
        bits.write(u32::from(coded.region0_count), 4);
        bits.write(u32::from(coded.region1_count), 3);
    }
    bits.write(0, 1);
    bits.write(u32::from(coded.scalefac_scale), 1);
    bits.write(u32::from(coded.count1_table_b), 1);
}

/// The power of the strongest line a full-scale sine gives, through the
/// analysis and the transform of blocks of `block_type`, in the middle of
/// the band: the scale the masking model takes its threshold in quiet in.
fn full_scale(sample_rate: u32, block_type: BlockType) -> f64 {
    // Line 64 of a long block, line 21 of a short one, at their centres.
    let frequency = 64.5 / 1_152.0 * f64::from(sample_rate);
    let mut analysis = Analysis::new();
    let mut strongest = 0.0f64;
    for granule in 0..6u64 {
        let mut samples = [0.0f32; 576];
        for (i, sample) in samples.iter_mut().enumerate() {
            let t = (granule * 576 + i as u64) as f64 / f64::from(sample_rate);
            *sample = (std::f64::consts::TAU * frequency * t).sin() as f32;
        }
        analysis.filter(&samples);
        let mut spectrum = [0.0f32; 576];
        analysis.transform(block_type, &mut spectrum);
        if granule >= 3 {
            for &line in &spectrum {
                strongest = strongest.max(f64::from(line * line));
            }
        }
    }
    strongest
}

/// Writes `buffer` to `writer` as MP3.
pub(crate) fn write_buffer<W: Write + Seek>(
    writer: &mut W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &Mp3Options,
) -> Result<(), Mp3Error> {
    let mut stream = Mp3Stream::start(writer, buffer.num_channels(), sample_rate, options)?;
    stream.write_planar(writer, buffer)?;
    stream.finish(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::header::FrameHeader;
    use super::super::layer3::side_info;
    use super::*;

    /// The block types of every granule of the first channel of a stream
    /// this encoder wrote, the Xing frame left out.
    fn block_types(bytes: &[u8]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut at = 0;
        let mut first = true;
        while let Some(header) = bytes
            .get(at..at + 4)
            .and_then(|b| FrameHeader::parse([b[0], b[1], b[2], b[3]]))
        {
            if !first {
                let side = side_info(&header, &bytes[at + 4..]).unwrap();
                types.extend(side.block_types(0));
            }
            first = false;
            at += header.len();
        }
        types
    }

    #[test]
    fn an_attack_after_quiet_is_coded_with_short_blocks() {
        let rate = 44_100;
        let frames = 20_000;
        let click = 11_000;
        let mut buffer = AudioBuffer::<f32>::new(1, frames);
        for (i, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            let tone = 0.01 * (i as f32 * 0.05).sin();
            *sample = if (click..click + 400).contains(&i) {
                0.8 * ((i - click) as f32 * 0.9).sin()
            } else {
                tone
            };
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        write_buffer(&mut bytes, &buffer, rate, &Mp3Options::constant(128)).unwrap();
        let types = block_types(&bytes.into_inner());
        // The granule whose long block would hold the attack, at 576 g -
        // 1057 for the first of its 1152 samples, is short, joined to the
        // long ones by a start and a stop block; the others are long.
        let short: Vec<usize> = (0..types.len()).filter(|&g| types[g] == 2).collect();
        assert!(!short.is_empty(), "{types:?}");
        for &g in &short {
            let start = 576 * g as i64 - DELAY as i64;
            assert!(
                start < click as i64 + 400 && start + 1_152 > click as i64,
                "{g}: {types:?}"
            );
        }
        let (first, last) = (short[0], short[short.len() - 1]);
        assert_eq!(types[first - 1], 1, "{types:?}");
        assert_eq!(types[last + 1], 3, "{types:?}");
        assert!(
            types
                .iter()
                .enumerate()
                .all(|(g, &t)| t == 0 || (first - 1..=last + 1).contains(&g))
        );
    }

    #[test]
    fn the_lame_tag_checksum_is_crc16_arc() {
        // The check value of CRC-16/ARC.
        assert_eq!(crc16_arc(0, b"123456789"), 0xBB3D);
    }
}
