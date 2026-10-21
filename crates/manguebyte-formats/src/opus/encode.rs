//! The Ogg Opus encoder: CELT frames in Ogg pages (RFC 6716 and RFC 7845).
//!
//! The audio is taken to 48 kHz if it is at another rate ([`Resampler`]),
//! cut into frames of 2.5 to 20 ms, and each frame of each stream coded by
//! a [`CeltEncoder`]: one stream for one or two channels (mapping family 0),
//! the coupled pairs and single channels of the Vorbis layouts for three to
//! eight (family 1), and a stream per channel beyond (family 255). Every
//! packet holds one frame of every stream, all but the last self-delimited
//! (RFC 6716, appendix B).
//!
//! The decoder gives back each sample `PRE_SKIP` samples late, the overlap
//! of the first block, which the identification header tells it to drop;
//! the granule position of the last page cuts the end where the audio
//! ended, so that a stream decodes to exactly its length at 48 kHz.

use std::io::{self, Seek, Write};

use manguebyte_core::AudioBuffer;

use super::OpusError;
use super::celt::tables::{BANDS, OVERLAP};
use super::celt::{Budget, CeltEncoder, FrameChoices};
use super::resample::Resampler;
use crate::metadata::{Metadata, picture_comment};
use crate::ogg::Pager;
use crate::vorbis::channel_order;

/// The vendor string written into every file, naming the encoder.
const VENDOR: &str = concat!("manguebyte-formats ", env!("CARGO_PKG_VERSION"));

/// Samples the decoder drops from the start: the overlap of the window,
/// which the first block needs before the first sample.
const PRE_SKIP: usize = OVERLAP;

/// The longest frame of one stream.
const MAX_FRAME_BYTES: usize = 1_275;

/// How long each frame is, the delay and the time resolution of the
/// stream: shorter frames cost more bits for the same quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameDuration {
    /// 2.5 ms, 120 samples at 48 kHz.
    Ms2_5,
    /// 5 ms.
    Ms5,
    /// 10 ms.
    Ms10,
    /// 20 ms, the default.
    Ms20,
}

impl FrameDuration {
    /// Samples of a frame at 48 kHz.
    pub fn samples(self) -> usize {
        match self {
            Self::Ms2_5 => 120,
            Self::Ms5 => 240,
            Self::Ms10 => 480,
            Self::Ms20 => 960,
        }
    }
}

/// How to write an Ogg Opus stream.
///
/// ```
/// use manguebyte_formats::opus::{FrameDuration, OpusOptions};
///
/// let options = OpusOptions::variable(128)
///     .with_frame_duration(FrameDuration::Ms10)
///     .with_tag("TITLE", "Frevo");
/// assert_eq!(options.kbps(), 128);
/// assert!(options.is_variable());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct OpusOptions {
    kbps: u32,
    variable: bool,
    frame: FrameDuration,
    tags: Vec<(String, String)>,
    /// Set when a picture was too large to carry.
    oversized: bool,
}

impl Default for OpusOptions {
    /// A variable bitrate of 128 kbit/s in frames of 20 ms.
    fn default() -> Self {
        Self::variable(128)
    }
}

impl OpusOptions {
    /// A constant bitrate of `kbps` kbit/s for the whole stream, all its
    /// channels together: every packet is the same size.
    pub fn constant(kbps: u32) -> Self {
        Self {
            kbps,
            variable: false,
            frame: FrameDuration::Ms20,
            tags: Vec::new(),
            oversized: false,
        }
    }

    /// A variable bitrate of about `kbps` kbit/s on average for the whole
    /// stream: frames of short blocks take more, frames whose audio stops
    /// short of the top bands fewer, silence almost none, and the average
    /// is held over about half a second.
    pub fn variable(kbps: u32) -> Self {
        Self {
            variable: true,
            ..Self::constant(kbps)
        }
    }

    /// Sets the length of each frame; 20 ms by default.
    pub fn with_frame_duration(mut self, frame: FrameDuration) -> Self {
        self.frame = frame;
        self
    }

    /// Adds a comment, such as `TITLE` or `ARTIST`, as Vorbis comments are
    /// (RFC 7845, section 5.2): a name of printable ASCII without `=`,
    /// compared without regard to case, given as often as wanted.
    pub fn with_tag(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.push((name.into(), value.into()));
        self
    }

    /// Adds what `metadata` holds that a comment can: its fields and tags
    /// ([`Metadata::to_vorbis_comments`]) and its pictures, as
    /// `METADATA_BLOCK_PICTURE` comments, the FLAC picture block in base64,
    /// as Ogg files carry them. Loops, cue points and the Broadcast Wave
    /// fields have no place in Ogg Opus.
    pub fn with_metadata(mut self, metadata: &Metadata) -> Self {
        self.tags.extend(metadata.to_vorbis_comments());
        for picture in &metadata.pictures {
            match picture_comment(picture) {
                Some(text) => self.tags.push(("METADATA_BLOCK_PICTURE".to_owned(), text)),
                None => self.oversized = true,
            }
        }
        self
    }

    /// The bitrate, in kbit/s.
    pub fn kbps(&self) -> u32 {
        self.kbps
    }

    /// Whether the bitrate is variable.
    pub fn is_variable(&self) -> bool {
        self.variable
    }

    /// The length of each frame.
    pub fn frame_duration(&self) -> FrameDuration {
        self.frame
    }

    /// The comments.
    pub fn tags(&self) -> &[(String, String)] {
        &self.tags
    }

    /// Checks the options with the channels and rate they are for: 1 to 255
    /// channels, a rate of at least 1 Hz, a bitrate of 6 to 510 kbit/s a
    /// channel, comment names that are printable ASCII without `=`, and
    /// pictures small enough for a FLAC picture block.
    pub fn validate(&self, channels: usize, sample_rate: u32) -> Result<(), OpusError> {
        if !(1..=255).contains(&channels) {
            return Err(OpusError::Unsupported("Ogg Opus holds 1 to 255 channels"));
        }
        if sample_rate == 0 {
            return Err(OpusError::Unsupported("a sample rate of zero"));
        }
        let per_channel = f64::from(self.kbps) / channels as f64;
        if !(6.0..=510.0).contains(&per_channel) {
            return Err(OpusError::Unsupported(
                "a bitrate outside 6 to 510 kbit/s a channel",
            ));
        }
        if self.oversized {
            return Err(OpusError::Unsupported("a picture too large to carry"));
        }
        if self.tags.iter().any(|(name, _)| {
            name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| (0x20..0x7E).contains(&byte) && byte != b'=')
        }) {
            return Err(OpusError::Unsupported(
                "a comment name that is not printable ASCII without '='",
            ));
        }
        Ok(())
    }
}

/// The first band an encoder of two channels codes as intensity stereo, by
/// the bitrate of its stream: none at a high rate, more of the top bands
/// as the rate falls.
fn intensity_for(kbps: f64) -> usize {
    const STEPS: [(f64, usize); 7] = [
        (128.0, BANDS),
        (112.0, 20),
        (96.0, 19),
        (80.0, 18),
        (64.0, 16),
        (48.0, 14),
        (32.0, 11),
    ];
    STEPS
        .iter()
        .find(|&&(at_least, _)| kbps >= at_least)
        .map_or(8, |&(_, band)| band)
}

/// One stream of a packet.
struct Stream {
    /// The input channels it codes, one or two.
    inputs: Vec<usize>,
    encoder: CeltEncoder,
    /// The bytes of an average frame.
    average: f64,
    /// Bytes written over the average so far.
    debt: f64,
    choices: FrameChoices,
}

/// How the channels go into streams, and the identification header's
/// mapping table that says so (RFC 7845, section 5.1.1).
struct Layout {
    family: u8,
    /// Each stream's input channels, the coupled ones first, and its share
    /// of the bitrate.
    streams: Vec<(Vec<usize>, f64)>,
    coupled: usize,
    mapping: Vec<u8>,
}

fn layout(channels: usize) -> Layout {
    match channels {
        1 => Layout {
            family: 0,
            streams: vec![(vec![0], 1.0)],
            coupled: 0,
            mapping: Vec::new(),
        },
        2 => Layout {
            family: 0,
            streams: vec![(vec![0, 1], 2.0)],
            coupled: 1,
            mapping: Vec::new(),
        },
        3..=8 => {
            // In the Vorbis order of section 5.1.1.2: the pairs, then the
            // channels alone; the last of six to eight is the LFE.
            let (pairs, singles): (&[(usize, usize)], &[usize]) = match channels {
                3 => (&[(0, 2)], &[1]),
                4 => (&[(0, 1), (2, 3)], &[]),
                5 => (&[(0, 2), (3, 4)], &[1]),
                6 => (&[(0, 2), (3, 4)], &[1, 5]),
                7 => (&[(0, 2), (3, 4)], &[1, 5, 6]),
                _ => (&[(0, 2), (3, 4), (5, 6)], &[1, 7]),
            };
            let order = channel_order(channels);
            let mut input_of = vec![0usize; channels];
            for (input, &vorbis) in order.iter().enumerate() {
                input_of[vorbis] = input;
            }
            let mut streams = Vec::new();
            let mut mapping = vec![0u8; channels];
            for (index, &(a, b)) in pairs.iter().enumerate() {
                streams.push((vec![input_of[a], input_of[b]], 2.0));
                mapping[a] = (2 * index) as u8;
                mapping[b] = (2 * index + 1) as u8;
            }
            for (index, &single) in singles.iter().enumerate() {
                let lfe = channels >= 6 && single == channels - 1;
                streams.push((vec![input_of[single]], if lfe { 0.25 } else { 1.0 }));
                mapping[single] = (2 * pairs.len() + index) as u8;
            }
            Layout {
                family: 1,
                streams,
                coupled: pairs.len(),
                mapping,
            }
        }
        _ => Layout {
            family: 255,
            streams: (0..channels).map(|channel| (vec![channel], 1.0)).collect(),
            coupled: 0,
            mapping: (0..channels as u8).collect(),
        },
    }
}

/// Encodes an Ogg Opus stream as samples arrive.
pub(crate) struct OpusStream {
    channels: usize,
    frame_size: usize,
    variable: bool,
    streams: Vec<Stream>,
    resampler: Option<Resampler>,
    /// The samples of each channel at 48 kHz not yet coded.
    pending: Vec<Vec<f32>>,
    /// Frames taken, at the input's rate, and at 48 kHz.
    received: u64,
    received_48k: u64,
    /// The granule position of the last packet made.
    granule: u64,
    ended: bool,
    pager: Pager,
    // Work space.
    block: Vec<f32>,
    packet: Vec<u8>,
}

impl OpusStream {
    /// Starts a stream: checks the options and writes the two headers.
    pub(crate) fn start<W: Write + Seek>(
        writer: &mut W,
        channels: usize,
        sample_rate: u32,
        options: &OpusOptions,
    ) -> Result<Self, OpusError> {
        options.validate(channels, sample_rate)?;
        let mut stream = Self::new(channels, sample_rate, options);
        stream.pager.flush(writer)?;
        Ok(stream)
    }

    fn new(channels: usize, sample_rate: u32, options: &OpusOptions) -> Self {
        let layout = layout(channels);
        let frame_size = options.frame.samples();
        let total_weight: f64 = layout.streams.iter().map(|(_, weight)| weight).sum();
        let count = layout.streams.len();
        let streams = layout
            .streams
            .iter()
            .enumerate()
            .map(|(index, (inputs, weight))| {
                let kbps = f64::from(options.kbps) * weight / total_weight;
                // The table of contents, and the length of a frame that
                // is not the last.
                let overhead = if index + 1 < count { 3.0 } else { 1.0 };
                let bytes = kbps * 1_000.0 * frame_size as f64 / 48_000.0 / 8.0 - overhead;
                let average = bytes.clamp(2.0, MAX_FRAME_BYTES as f64);
                Stream {
                    inputs: inputs.clone(),
                    encoder: CeltEncoder::new(inputs.len()),
                    average: if options.variable {
                        average
                    } else {
                        average.floor()
                    },
                    debt: 0.0,
                    choices: FrameChoices {
                        intensity: if inputs.len() == 2 {
                            intensity_for(kbps)
                        } else {
                            BANDS
                        },
                        allow_dual: true,
                        depth: 8,
                    },
                }
            })
            .collect();

        // The identification header (RFC 7845, section 5.1).
        let mut head = b"OpusHead".to_vec();
        head.push(1);
        head.push(channels as u8);
        head.extend_from_slice(&(PRE_SKIP as u16).to_le_bytes());
        head.extend_from_slice(&sample_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(layout.family);
        if layout.family != 0 {
            head.push(count as u8);
            head.push(layout.coupled as u8);
            head.extend_from_slice(&layout.mapping);
        }
        // The comment header (section 5.2).
        let mut tags = b"OpusTags".to_vec();
        let put_text = |out: &mut Vec<u8>, text: &[u8]| {
            out.extend_from_slice(&(text.len() as u32).to_le_bytes());
            out.extend_from_slice(text);
        };
        put_text(&mut tags, VENDOR.as_bytes());
        tags.extend_from_slice(&(options.tags.len() as u32).to_le_bytes());
        for (name, value) in &options.tags {
            put_text(&mut tags, format!("{name}={value}").as_bytes());
        }

        // A fixed serial number, so that the same audio and options give
        // the same bytes.
        let mut pager = Pager::new(0x4D42_4F50 ^ (channels as u32) << 8 ^ sample_rate);
        pager.packet(&head, 0);
        pager.page(false);
        pager.packet(&tags, 0);
        pager.page(false);

        Self {
            channels,
            frame_size,
            variable: options.variable,
            streams,
            resampler: (sample_rate != 48_000)
                .then(|| Resampler::new(f64::from(sample_rate), 48_000.0, channels)),
            pending: vec![Vec::with_capacity(4 * frame_size); channels],
            received: 0,
            received_48k: 0,
            granule: 0,
            ended: false,
            pager,
            block: vec![0.0; 2 * frame_size],
            packet: Vec::with_capacity(count * (MAX_FRAME_BYTES + 3)),
        }
    }

    /// Frames taken so far, at the rate given.
    pub(crate) fn frames(&self) -> u64 {
        self.received
    }

    /// Takes `frames` frames, channel `c` of frame `i` at `input(c, i)`,
    /// and writes the pages they complete.
    fn take<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        frames: usize,
        input: impl Fn(usize, usize) -> f32,
    ) -> io::Result<()> {
        self.pager.flush(writer)?;
        let before: usize = self.pending[0].len();
        match &mut self.resampler {
            Some(resampler) => {
                resampler.process(frames, |c, i| clean(input(c, i)), &mut self.pending);
            }
            None => {
                for (channel, pending) in self.pending.iter_mut().enumerate() {
                    pending.extend((0..frames).map(|i| clean(input(channel, i))));
                }
            }
        }
        self.received += frames as u64;
        self.received_48k += (self.pending[0].len() - before) as u64;
        while self.pending[0].len() >= self.frame_size {
            self.encode_packet(None);
        }
        self.pager.flush(writer)
    }

    /// Takes the frames of `buffer` and writes the pages they complete.
    pub(crate) fn write_planar<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        buffer: &AudioBuffer<f32>,
    ) -> io::Result<()> {
        let frames = buffer.num_frames();
        let mut from = 0;
        while from < frames {
            let count = (frames - from).min(4_096);
            self.take(writer, count, |c, i| buffer.channel(c)[from + i])?;
            from += count;
        }
        Ok(())
    }

    /// The same for interleaved samples.
    pub(crate) fn write_interleaved<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        samples: &[f32],
    ) -> io::Result<()> {
        let channels = self.channels;
        for chunk in samples.chunks(4_096 * channels) {
            self.take(writer, chunk.len() / channels, |c, i| {
                chunk[i * channels + c]
            })?;
        }
        Ok(())
    }

    /// The same for silence.
    pub(crate) fn write_silence<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        frames: u64,
    ) -> io::Result<()> {
        let mut left = frames;
        while left > 0 {
            let count = left.min(4_096) as usize;
            self.take(writer, count, |_, _| 0.0)?;
            left -= count as u64;
        }
        self.pager.flush(writer)
    }

    /// Encodes what is left, padded with silence to the end of the last
    /// frame, and ends the stream.
    pub(crate) fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        self.pager.flush(writer)?;
        if !self.ended {
            if let Some(resampler) = &mut self.resampler {
                let before = self.pending[0].len();
                resampler.finish(&mut self.pending);
                self.received_48k += (self.pending[0].len() - before) as u64;
            }
            let end = PRE_SKIP as u64 + self.received_48k;
            loop {
                for pending in &mut self.pending {
                    if pending.len() < self.frame_size {
                        pending.resize(self.frame_size, 0.0);
                    }
                }
                let last = self.granule + self.frame_size as u64 >= end;
                self.encode_packet(last.then_some(end));
                if last {
                    break;
                }
            }
            self.pager.page(true);
            self.ended = true;
        }
        self.pager.flush(writer)?;
        writer.flush()
    }

    /// Codes a frame of every stream into a packet, from the first
    /// `frame_size` samples of each channel waiting, and hands it to the
    /// pager, ending at `end` if it is the last.
    fn encode_packet(&mut self, end: Option<u64>) {
        let n = self.frame_size;
        let count = self.streams.len();
        self.packet.clear();
        let config = 28 + n.trailing_zeros() as u8 - 120usize.trailing_zeros() as u8;
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let channels = stream.inputs.len();
            for (slot, &input) in stream.inputs.iter().enumerate() {
                self.block[slot * n..(slot + 1) * n].copy_from_slice(&self.pending[input][..n]);
            }
            // Frames in two seconds, over which the variable bitrate pays
            // back what it spent over the average.
            let horizon = (96_000 / n) as f64;
            let budget = if self.variable {
                let average = (stream.average - stream.debt / horizon)
                    .clamp(stream.average / 2.0, 2.0 * stream.average);
                Budget::Variable {
                    average: average as f32,
                    max: ((3.0 * stream.average) as usize).clamp(2, MAX_FRAME_BYTES),
                }
            } else {
                Budget::Fixed(stream.average as usize)
            };
            let frame =
                stream
                    .encoder
                    .encode(&self.block[..channels * n], n, budget, &stream.choices);
            // What a frame spends over the average is paid back; what one
            // saves, being simpler, is not spent on others.
            let bound = horizon * stream.average;
            stream.debt = (stream.debt + frame.len() as f64 - stream.average).clamp(0.0, bound);
            self.packet.push(config << 3 | u8::from(channels == 2) << 2);
            if index + 1 < count {
                // Self-delimited: the length of the frame (section 3.2.1).
                let len = frame.len();
                if len < 252 {
                    self.packet.push(len as u8);
                } else {
                    let first = 252 + ((len - 252) & 3);
                    self.packet.push(first as u8);
                    self.packet.push(((len - first) / 4) as u8);
                }
            }
            self.packet.extend_from_slice(&frame);
        }
        for pending in &mut self.pending {
            pending.drain(..n);
        }
        self.granule += n as u64;
        let granule = end.unwrap_or(self.granule);
        self.pager.packet(&self.packet, granule);
    }
}

/// A sample as the encoder takes it: NaN as silence, everything held to
/// the range a float of the format can carry through.
#[inline]
fn clean(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        sample.clamp(-1e4, 1e4)
    }
}

/// Writes `buffer` to `writer` as Ogg Opus.
pub(crate) fn write_buffer<W: Write + Seek>(
    writer: &mut W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &OpusOptions,
) -> Result<(), OpusError> {
    let mut stream = OpusStream::start(writer, buffer.num_channels(), sample_rate, options)?;
    stream.write_planar(writer, buffer)?;
    stream.finish(writer)?;
    Ok(())
}
