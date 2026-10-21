//! The Ogg Vorbis encoder.
//!
//! Each block goes through the forward MDCT of section 1.3.2 of the Vorbis
//! I specification with the window the decoder will put on it, long or
//! short, the short ones where a transient would smear across a long block
//! (pre-echo). A masking threshold (the model of the crate's `psy` module) sets the floor of
//! each channel: a floor of type 1, fitted at its posts, which the
//! residue, the spectrum divided by the floor and rounded, is coded
//! against, so that the quantisation noise lies at the floor, under the
//! threshold. Two channels are coupled, after rounding, by the square
//! polar mapping of section 1.3.3, which the decoder undoes exactly. The
//! residue is of type 2, both channels interleaved, or of type 1 for more
//! than two channels (see [`interleaved`]), cut into partitions of
//! 16 values, each classified by its largest value and coded with the
//! books of its class, in up to three passes for large values. Every
//! codebook is designed here ([`books`]), from models of what it codes, and
//! sent in the setup header, which the encoder reads back with the
//! decoder's own parser before it starts: what it writes, the decoder
//! reads.
//!
//! The quality setting moves the floor against the threshold, by 2.5 dB a
//! step, and the bitrate with it: see [`VorbisOptions::with_quality`].

mod books;
mod trained;

use std::io::{self, Seek, Write};

use manguebyte_core::AudioBuffer;

use self::books::{Book, Lattice};
use super::VorbisError;
use super::decoder::{inverse_db_table, render_floor1, slope};
use super::mdct::Mdct;
use super::reader::channel_order;
use super::setup::{self, Floor1, Identification};
use crate::metadata::{Metadata, picture_comment};
use crate::ogg::Pager;
use crate::psy::{HOP, Model, Transients};

/// The vendor string written into every file, naming the encoder.
const VENDOR: &str = concat!("manguebyte-formats ", env!("CARGO_PKG_VERSION"));

/// Values in a partition of the residue.
const PARTITION: usize = 16;

/// The largest value each class of partition holds, and the books of its
/// passes, as indices into the book list of [`design_books`].
const CLASS_REACH: [i32; 8] = [0, 1, 2, 4, 8, 16, 136, 2_184];
const CLASS_BOOKS: [[Option<u8>; 3]; 8] = [
    [None, None, None],
    [Some(BOOK_C1), None, None],
    [Some(BOOK_C2), None, None],
    [Some(BOOK_C4), None, None],
    [Some(BOOK_C8), None, None],
    [Some(BOOK_C16), None, None],
    [Some(BOOK_X16), Some(BOOK_C8), None],
    [Some(BOOK_X256), Some(BOOK_X16), Some(BOOK_C8)],
];
const BOOK_FLOOR: u8 = 0;
const BOOK_CLASS: u8 = 1;
const BOOK_C1: u8 = 2;
const BOOK_C2: u8 = 3;
const BOOK_C4: u8 = 4;
const BOOK_C8: u8 = 5;
const BOOK_C16: u8 = 6;
const BOOK_X16: u8 = 7;
const BOOK_X256: u8 = 8;

/// The largest value the floor is fitted to leave in the residue near a
/// post, well within what the books reach, for the floor between posts may
/// dip under the posts' gain.
const LARGEST_VALUE: f32 = 1_000.0;

/// The floor's multiplier: steps of 1.1 dB, 128 values.
const FLOOR_MULTIPLIER: u32 = 2;
const FLOOR_RANGE: i32 = 128;
/// Posts per partition of the floor.
const FLOOR_DIMENSIONS: usize = 4;

/// How to write an Ogg Vorbis stream.
///
/// ```
/// use manguebyte_formats::vorbis::VorbisOptions;
///
/// let options = VorbisOptions::new(6.0).with_tag("TITLE", "Frevo");
/// assert_eq!(options.quality(), 6.0);
/// assert_eq!(options.nominal_bitrate(2, 44_100), 134_000);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct VorbisOptions {
    quality: f32,
    tags: Vec<(String, String)>,
    /// Set when a picture was too large to carry.
    oversized: bool,
}

impl Default for VorbisOptions {
    /// Quality 5.
    fn default() -> Self {
        Self::new(5.0)
    }
}

impl VorbisOptions {
    /// Options of quality `quality`, from 0 to 10.
    pub fn new(quality: f32) -> Self {
        Self {
            quality,
            tags: Vec::new(),
            oversized: false,
        }
    }

    /// Sets the quality, from 0 to 10, whole or not.
    ///
    /// Each step lowers the floor, and with it the noise of the
    /// quantisation, by 2.5 dB against the masking threshold of the
    /// encoder's model: at 0 the noise lies 5.5 dB above it, at 5 7 dB
    /// under it and at 10 19.5 dB under it. The bitrate follows the audio,
    /// as in every variable-bitrate encoder;
    /// [`nominal_bitrate`](Self::nominal_bitrate) gives the rate the header
    /// states, about what the test signal of this crate takes: stereo at
    /// 44.1 kHz, about 77 kbit/s at quality 0, 124 kbit/s at 5 and
    /// 173 kbit/s at 10.
    pub fn with_quality(mut self, quality: f32) -> Self {
        self.quality = quality;
        self
    }

    /// Adds a comment, such as `TITLE` or `ARTIST` (section 5 of the
    /// specification): a name of printable ASCII without `=`, compared
    /// without regard to case, given as often as wanted.
    pub fn with_tag(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.push((name.into(), value.into()));
        self
    }

    /// Adds what `metadata` holds that a Vorbis comment can: its fields and
    /// tags ([`Metadata::to_vorbis_comments`]) and its pictures, as
    /// `METADATA_BLOCK_PICTURE` comments, the FLAC picture block in base64,
    /// as Ogg files carry them. Loops, cue points and the Broadcast Wave
    /// fields have no place in Ogg Vorbis.
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

    /// The quality.
    pub fn quality(&self) -> f32 {
        self.quality
    }

    /// The comments.
    pub fn tags(&self) -> &[(String, String)] {
        &self.tags
    }

    /// The bitrate the identification header states, in bits per second:
    /// what the quality gives music of `channels` channels at
    /// `sample_rate`, roughly. The stream says it for information; players
    /// take the real rate from the file.
    pub fn nominal_bitrate(&self, channels: usize, sample_rate: u32) -> u32 {
        // 38 kbit/s a channel at 44.1 kHz and quality 0, and 4.8 kbit/s
        // more for each step: what the test signal takes.
        let per_channel = 38_000.0 + 4_800.0 * f64::from(self.quality);
        let rate = (f64::from(sample_rate) / 44_100.0).clamp(0.25, 2.0).sqrt();
        let bits = per_channel
            * channels.min(2) as f64
            * rate
            * if channels > 2 {
                1.0 + 0.5 * (channels - 2) as f64
            } else {
                1.0
            };
        (bits / 1_000.0).round() as u32 * 1_000
    }

    /// Checks the options with the channels and rate they are for: 1 to 255
    /// channels, a rate of at least 1 Hz, a quality from 0 to 10, comment
    /// names that are printable ASCII without `=`, and pictures small
    /// enough for a FLAC picture block.
    pub fn validate(&self, channels: usize, sample_rate: u32) -> Result<(), VorbisError> {
        if !(1..=255).contains(&channels) {
            return Err(VorbisError::Unsupported("Vorbis holds 1 to 255 channels"));
        }
        if sample_rate == 0 {
            return Err(VorbisError::Unsupported("a sample rate of zero"));
        }
        if !(0.0..=10.0).contains(&self.quality) {
            return Err(VorbisError::Unsupported("a quality outside 0 to 10"));
        }
        if self.oversized {
            return Err(VorbisError::Unsupported("a picture too large to carry"));
        }
        if self.tags.iter().any(|(name, _)| {
            name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| (0x20..0x7E).contains(&byte) && byte != b'=')
        }) {
            return Err(VorbisError::Unsupported(
                "a comment name that is not printable ASCII without '='",
            ));
        }
        Ok(())
    }
}

/// Packs bits least significant first, as Vorbis does (section 2).
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used of the last byte, 0 meaning a new byte is needed.
    used: u32,
}

impl BitWriter {
    pub(crate) fn write(&mut self, value: u32, count: u32) {
        let mut value = u64::from(value) & ((1u64 << count) - 1);
        let mut left = count;
        while left > 0 {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let room = 8 - self.used;
            let take = room.min(left);
            let last = self.bytes.last_mut().expect("a byte was pushed");
            *last |= ((value & ((1 << take) - 1)) as u8) << self.used;
            value >>= take;
            left -= take;
            self.used = (self.used + take) % 8;
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.used = 0;
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// `ilog` of section 9.2.1.
fn ilog(value: u32) -> u32 {
    32 - value.leading_zeros()
}

/// The configuration of one block size.
struct BlockSize {
    /// The floor as the decoder reads it, from the setup header.
    floor: Floor1,
    mdct: Mdct,
    model: Model,
}

/// The posts of a floor of `half` bins, besides the two ends, in the order
/// they are coded: spread on a logarithmic scale from bin 2 to nine tenths
/// of the band, then ordered by halving, so that each post is predicted
/// from the two posts around it coded before it.
fn floor_posts(half: usize, count: usize) -> Vec<u16> {
    let low = 2.0f64;
    let high = 0.9 * half as f64;
    let mut sorted: Vec<u16> = Vec::with_capacity(count);
    for i in 0..count {
        let x = low * (high / low).powf(i as f64 / (count - 1) as f64);
        let mut x = x.round() as u16;
        if let Some(&last) = sorted.last() {
            x = x.max(last + 1);
        }
        sorted.push(x);
    }
    // Breadth-first over the halvings of the sorted list.
    let mut order = Vec::with_capacity(count);
    let mut spans = vec![(0usize, count)];
    while !spans.is_empty() {
        let mut next = Vec::new();
        for (start, end) in spans {
            if start >= end {
                continue;
            }
            let middle = (start + end) / 2;
            order.push(sorted[middle]);
            next.push((start, middle));
            next.push((middle + 1, end));
        }
        spans = next;
    }
    order
}

/// The floor of `half` bins as the setup header states it (section
/// 7.2.2): one class of [`FLOOR_DIMENSIONS`] posts coded with
/// [`BOOK_FLOOR`].
fn write_floor(bits: &mut BitWriter, half: usize, posts: &[u16]) {
    let partitions = posts.len() / FLOOR_DIMENSIONS;
    bits.write(partitions as u32, 5);
    for _ in 0..partitions {
        bits.write(0, 4);
    }
    // Class 0: its dimensions, no subclasses, its one book.
    bits.write(FLOOR_DIMENSIONS as u32 - 1, 3);
    bits.write(0, 2);
    bits.write(u32::from(BOOK_FLOOR) + 1, 8);
    bits.write(FLOOR_MULTIPLIER - 1, 2);
    let range_bits = ilog(half as u32 - 1);
    bits.write(range_bits, 4);
    for &x in posts {
        bits.write(u32::from(x), range_bits);
    }
}

/// Whether the residue of a stream of `channels` channels is of type 2,
/// its channels interleaved, or of type 1, each channel apart.
///
/// Type 2 suits one channel, where the two are the same, and two coupled
/// channels, whose values go together. With more, type 1: the
/// specification allows type 2 for any number of channels, but libvorbis,
/// which most players decode with, reads a partition of type 2 as if it
/// started on the first channel, and so misplaces every value when the
/// channels do not divide the partition (three, five, six or seven
/// channels with partitions of 16 values).
fn interleaved(channels: usize) -> bool {
    channels <= 2
}

/// The residue of a block of `half` bins per channel: partitions of
/// [`PARTITION`] values, eight classes (section 8.6.1).
fn write_residue(bits: &mut BitWriter, half: usize, channels: usize) {
    let (kind, end) = if interleaved(channels) {
        (2, half * channels)
    } else {
        (1, half)
    };
    bits.write(kind, 16);
    bits.write(0, 24);
    bits.write(end as u32, 24);
    bits.write(PARTITION as u32 - 1, 24);
    bits.write(CLASS_BOOKS.len() as u32 - 1, 6);
    bits.write(u32::from(BOOK_CLASS), 8);
    for passes in &CLASS_BOOKS {
        let mut cascade = 0u32;
        for (pass, book) in passes.iter().enumerate() {
            if book.is_some() {
                cascade |= 1 << pass;
            }
        }
        bits.write(cascade & 7, 3);
        bits.write(0, 1);
    }
    for passes in &CLASS_BOOKS {
        for &book in passes.iter().flatten() {
            bits.write(u32::from(book), 8);
        }
    }
}

/// The books every stream uses, in the order the setup header lists them.
fn design_books() -> Vec<Book> {
    let weights = |counts: &[u16]| {
        counts
            .iter()
            .map(|&count| f64::from(count))
            .collect::<Vec<f64>>()
    };
    let classes: Vec<f64> = trained::CLASS_PAIRS
        .iter()
        .map(|&count| f64::from(count))
        .collect();
    // A value of a vector book weighs what its magnitude was counted.
    let magnitudes = |counts: &'static [u16], step: i32| {
        move |value: i32| f64::from(counts[(value / step).unsigned_abs() as usize])
    };
    vec![
        Book::scalar(&weights(&trained::FLOOR_CODES)),
        Book::scalar(&classes).with_dimensions(2),
        Book::lattice(
            4,
            Lattice { values: 3, step: 1 },
            &weights(&trained::CLASS_1),
        ),
        Book::vector(
            2,
            Lattice { values: 5, step: 1 },
            magnitudes(&trained::CLASS_2, 1),
        ),
        Book::vector(
            2,
            Lattice { values: 9, step: 1 },
            magnitudes(&trained::CLASS_4, 1),
        ),
        Book::vector(
            2,
            Lattice {
                values: 17,
                step: 1,
            },
            magnitudes(&trained::CLASS_8, 1),
        ),
        Book::vector(
            2,
            Lattice {
                values: 33,
                step: 1,
            },
            magnitudes(&trained::CLASS_16, 1),
        ),
        Book::vector(
            2,
            Lattice {
                values: 17,
                step: 16,
            },
            magnitudes(&trained::COARSE_16, 16),
        ),
        Book::vector(
            2,
            Lattice {
                values: 17,
                step: 256,
            },
            magnitudes(&trained::COARSE_256, 256),
        ),
    ]
}

/// Encodes an Ogg Vorbis stream as samples arrive.
pub(crate) struct VorbisStream {
    channels: usize,
    /// For each channel of the stream, the channel of the input it takes.
    input_of: Vec<usize>,
    sizes: [usize; 2],
    blocks: [BlockSize; 2],
    books: Vec<Book>,
    slopes: [Vec<f32>; 2],
    inverse_db: [f32; 256],
    /// How far the floor sits under the threshold, as a power factor.
    floor_factor: f32,
    /// The samples of each stream channel from `buffer_start` on.
    buffer: Vec<Vec<f32>>,
    buffer_start: i64,
    /// Samples taken so far.
    received: u64,
    transients: Transients,
    /// Whether each hop is a transient, from hop `hop_base` on.
    hops: Vec<bool>,
    hop_base: i64,
    /// Samples of the hop being filled.
    hop_fill: usize,
    /// The block to encode next: its centre and whether it is long; the
    /// size of the block before it, if any.
    center: i64,
    long: bool,
    previous_long: Option<bool>,
    packets: u64,
    ended: bool,
    pager: Pager,
    // Work space.
    windowed: Vec<f32>,
    window: Vec<f32>,
    spectra: Vec<Vec<f32>>,
    threshold: Vec<f32>,
    curve: Vec<f32>,
    quantized: Vec<Vec<i32>>,
    interleaved: Vec<i32>,
    /// The class of each partition of each vector the residue codes.
    classes: Vec<Vec<u8>>,
    floor_codes: Vec<Vec<i32>>,
    floor_used: Vec<bool>,
    final_y: Vec<i64>,
    step2: Vec<bool>,
    bits: BitWriter,
}

impl VorbisStream {
    /// Starts a stream: checks the options and writes the three headers.
    pub(crate) fn start<W: Write + Seek>(
        writer: &mut W,
        channels: usize,
        sample_rate: u32,
        options: &VorbisOptions,
    ) -> Result<Self, VorbisError> {
        options.validate(channels, sample_rate)?;
        let mut stream = Self::new(channels, sample_rate, options)?;
        stream.flush(writer)?;
        Ok(stream)
    }

    fn new(
        channels: usize,
        sample_rate: u32,
        options: &VorbisOptions,
    ) -> Result<Self, VorbisError> {
        let long = if sample_rate >= 24_000 {
            2_048
        } else if sample_rate >= 12_000 {
            1_024
        } else {
            512
        };
        let sizes = [long / 8, long];
        let books = design_books();
        let posts = [
            floor_posts(sizes[0] / 2, 3 * FLOOR_DIMENSIONS),
            floor_posts(sizes[1] / 2, 8 * FLOOR_DIMENSIONS),
        ];

        // The headers.
        let mut ident = BitWriter::default();
        for &byte in b"\x01vorbis" {
            ident.write(u32::from(byte), 8);
        }
        ident.write(0, 32);
        ident.write(channels as u32, 8);
        ident.write(sample_rate, 32);
        ident.write(0, 32);
        ident.write(options.nominal_bitrate(channels, sample_rate), 32);
        ident.write(0, 32);
        ident.write(sizes[0].trailing_zeros(), 4);
        ident.write(sizes[1].trailing_zeros(), 4);
        ident.write(1, 1);
        let ident = ident.into_bytes();

        let mut comments = BitWriter::default();
        for &byte in b"\x03vorbis" {
            comments.write(u32::from(byte), 8);
        }
        let put_text = |bits: &mut BitWriter, text: &[u8]| {
            bits.write(text.len() as u32, 32);
            for &byte in text {
                bits.write(u32::from(byte), 8);
            }
        };
        put_text(&mut comments, VENDOR.as_bytes());
        comments.write(options.tags.len() as u32, 32);
        for (name, value) in &options.tags {
            put_text(&mut comments, format!("{name}={value}").as_bytes());
        }
        comments.write(1, 1);
        let comments = comments.into_bytes();

        let mut setup_bits = BitWriter::default();
        for &byte in b"\x05vorbis" {
            setup_bits.write(u32::from(byte), 8);
        }
        setup_bits.write(books.len() as u32 - 1, 8);
        for book in &books {
            book.write_header(&mut setup_bits);
        }
        // One time domain transform, a placeholder.
        setup_bits.write(0, 6);
        setup_bits.write(0, 16);
        // Two floors of type 1.
        setup_bits.write(1, 6);
        for (size, posts) in sizes.iter().zip(&posts) {
            setup_bits.write(1, 16);
            write_floor(&mut setup_bits, size / 2, posts);
        }
        // Two residues.
        setup_bits.write(1, 6);
        for size in sizes {
            write_residue(&mut setup_bits, size / 2, channels);
        }
        // Two mappings, each of one submap, coupling two channels.
        setup_bits.write(1, 6);
        for index in 0..2 {
            setup_bits.write(0, 16);
            setup_bits.write(0, 1);
            if channels == 2 {
                setup_bits.write(1, 1);
                setup_bits.write(0, 8);
                setup_bits.write(0, 1);
                setup_bits.write(1, 1);
            } else {
                setup_bits.write(0, 1);
            }
            setup_bits.write(0, 2);
            setup_bits.write(0, 8);
            setup_bits.write(index, 8);
            setup_bits.write(index, 8);
        }
        // Two modes: short and long.
        setup_bits.write(1, 6);
        for index in 0..2 {
            setup_bits.write(index, 1);
            setup_bits.write(0, 16);
            setup_bits.write(0, 16);
            setup_bits.write(index, 8);
        }
        setup_bits.write(1, 1);
        let setup_packet = setup_bits.into_bytes();

        // Read back with the decoder's parser: the floors the encoder
        // renders are those the decoder will.
        let identification = Identification {
            channels,
            sample_rate,
            block_sizes: sizes,
        };
        let parsed = setup::setup(&setup_packet, &identification)?;
        let mut floors = parsed.floors.into_iter();
        let mut take_floor = || -> Result<Floor1, VorbisError> {
            floors
                .next()
                .flatten()
                .ok_or(VorbisError::Malformed("the encoder's own floor"))
        };
        let full_scale = |n: usize| {
            // A full-scale sine in the middle of the band, through the
            // window and the transform: the power of its strongest bin.
            let mut mdct = Mdct::new(n);
            let window = full_window(n, &slope(n / 2));
            let input: Vec<f32> = (0..n)
                .map(|i| {
                    let phase =
                        std::f64::consts::TAU * (n as f64 / 8.0 + 0.5) * i as f64 / n as f64;
                    phase.sin() as f32 * window[i]
                })
                .collect();
            let mut out = vec![0.0f32; n / 2];
            mdct.forward(&input, &mut out);
            out.iter()
                .map(|&value| f64::from(value * value))
                .fold(0.0, f64::max)
        };
        let blocks = [
            BlockSize {
                floor: take_floor()?,
                mdct: Mdct::new(sizes[0]),
                model: Model::new(sizes[0], sample_rate, full_scale(sizes[0])),
            },
            BlockSize {
                floor: take_floor()?,
                mdct: Mdct::new(sizes[1]),
                model: Model::new(sizes[1], sample_rate, full_scale(sizes[1])),
            },
        ];

        let order = channel_order(channels);
        let mut input_of = vec![0usize; channels];
        for (input, &stream_channel) in order.iter().enumerate() {
            input_of[stream_channel] = input;
        }

        // The stream's serial number: fixed, so that the same audio and
        // options give the same bytes.
        let mut pager = Pager::new(0x4D42_5642 ^ (channels as u32) << 8 ^ sample_rate);
        pager.packet(&ident, 0);
        pager.page(false);
        pager.packet(&comments, 0);
        pager.packet(&setup_packet, 0);
        pager.page(false);

        // Quality 5 puts the noise 7 dB under the threshold; the noise of
        // rounding to a floor of gain g has power g^2 / 12.
        let below_db = 2.5 * f64::from(options.quality) - 5.5;
        let floor_factor = (12.0 * 10f64.powf(-below_db / 10.0)) as f32;

        let long_half = sizes[1] / 2;
        let max_points = blocks[1].floor.x.len();
        Ok(Self {
            channels,
            input_of,
            sizes,
            blocks,
            books,
            slopes: [slope(sizes[0] / 2), slope(sizes[1] / 2)],
            inverse_db: inverse_db_table(),
            floor_factor,
            buffer: vec![Vec::with_capacity(4 * sizes[1]); channels],
            buffer_start: -(sizes[1] as i64),
            received: 0,
            transients: Transients::new(channels),
            hops: Vec::new(),
            hop_base: 0,
            hop_fill: 0,
            center: 0,
            long: true,
            previous_long: None,
            packets: 0,
            ended: false,
            pager,
            windowed: vec![0.0; sizes[1]],
            window: vec![0.0; sizes[1]],
            spectra: vec![vec![0.0; long_half]; channels],
            threshold: vec![0.0; long_half],
            curve: vec![0.0; long_half],
            quantized: vec![vec![0; long_half]; channels],
            interleaved: vec![0; long_half * channels],
            classes: vec![vec![0; long_half * channels / PARTITION + 1]; channels],
            floor_codes: vec![vec![0; max_points]; channels],
            floor_used: vec![false; channels],
            final_y: vec![0; max_points],
            step2: vec![false; max_points],
            bits: BitWriter::default(),
        })
        .map(|mut stream| {
            // The zeros before the first sample, which the first block
            // starts in.
            for channel in &mut stream.buffer {
                channel.resize(sizes[1], 0.0);
            }
            stream
        })
    }

    /// Frames taken so far.
    pub(crate) fn frames(&self) -> u64 {
        self.received
    }

    /// Takes the frames of `buffer` and writes the pages they complete.
    pub(crate) fn write_planar<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        buffer: &AudioBuffer<f32>,
    ) -> io::Result<()> {
        self.flush(writer)?;
        let frames = buffer.num_frames();
        let mut from = 0;
        while from < frames {
            let count = (frames - from).min(self.sizes[1]);
            for (stream_channel, &input) in self.input_of.iter().enumerate() {
                let samples = &buffer.channel(input)[from..from + count];
                self.buffer[stream_channel].extend(samples.iter().map(|&sample| clean(sample)));
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
        for chunk in samples.chunks(self.sizes[1] * channels) {
            for (stream_channel, &input) in self.input_of.iter().enumerate() {
                self.buffer[stream_channel].extend(
                    chunk
                        .iter()
                        .skip(input)
                        .step_by(channels)
                        .map(|&sample| clean(sample)),
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
            let count = left.min(self.sizes[1] as u64) as usize;
            for channel in &mut self.buffer {
                channel.resize(channel.len() + count, 0.0);
            }
            self.took(count);
            left -= count as u64;
            self.pump(false);
            self.flush(writer)?;
        }
        Ok(())
    }

    /// Encodes what is left and ends the stream.
    pub(crate) fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        self.flush(writer)?;
        if !self.ended {
            self.pump(true);
        }
        self.flush(writer)?;
        writer.flush()
    }

    /// Counts the samples just put into the buffer, and runs the transient
    /// detector over them.
    fn took(&mut self, count: usize) {
        let end = self.buffer[0].len();
        for index in end - count..end {
            for channel in 0..self.channels {
                self.transients.add(channel, self.buffer[channel][index]);
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

    /// Whether any hop in the stretch `from..to` of the stream is a
    /// transient. Hops not yet seen are not.
    fn transient_in(&self, from: i64, to: i64) -> bool {
        let first = (from.max(0) / HOP as i64 - self.hop_base).max(0) as usize;
        let last = ((to.max(0) + HOP as i64 - 1) / HOP as i64 - self.hop_base).max(0) as usize;
        self.hops
            .get(first..last.min(self.hops.len()))
            .is_some_and(|hops| hops.iter().any(|&flag| flag))
    }

    /// Encodes every block the samples taken allow, or, when `ending`, every
    /// block up to the end of the stream.
    fn pump(&mut self, ending: bool) {
        let [short, long] = self.sizes;
        loop {
            if self.ended {
                return;
            }
            let n = self.sizes[usize::from(self.long)];
            let total = self.received as i64;
            // Look far enough ahead to choose the next block's size.
            let next_long_center = self.center + (n / 4 + long / 4) as i64;
            let horizon = next_long_center + (long / 2 + short) as i64;
            if !ending && total < horizon.max(self.center + n as i64 / 2) {
                return;
            }
            let last = ending && self.center >= total;
            let next_long = if last {
                self.long
            } else {
                // A long block spans its centre plus or minus a quarter of a
                // long and of a short block, between short ones; a
                // transient there takes short blocks.
                let reach = (long / 4 + short / 4) as i64;
                !self.transient_in(
                    next_long_center - reach,
                    next_long_center + reach + short as i64,
                )
            };
            self.encode_block(next_long);
            let granule = if self.packets == 0 {
                0
            } else {
                self.center as u64
            };
            let granule = if last { self.received } else { granule };
            let packet = std::mem::take(&mut self.bits);
            self.pager.packet(packet.bytes(), granule);
            self.bits = packet;
            self.bits.clear();
            self.packets += 1;
            if last {
                self.pager.page(true);
                self.ended = true;
                return;
            }
            let next = self.sizes[usize::from(next_long)];
            self.previous_long = Some(self.long);
            self.center += (n / 4 + next / 4) as i64;
            self.long = next_long;
            self.drop_old();
        }
    }

    /// Forgets the samples no block will read again.
    fn drop_old(&mut self) {
        let keep_from = self.center - self.sizes[1] as i64;
        if keep_from > self.buffer_start + 2 * self.sizes[1] as i64 {
            let drop = (keep_from - self.buffer_start) as usize;
            for channel in &mut self.buffer {
                channel.drain(..drop);
            }
            self.buffer_start += drop as i64;
            // And the transient flags before it.
            let hop = keep_from / HOP as i64 - self.hop_base;
            if hop > 64 {
                self.hops.drain(..hop as usize);
                self.hop_base += hop;
            }
        }
    }

    /// The sample of stream channel `channel` at `index`, zero outside
    /// what was taken.
    fn sample(&self, channel: usize, index: i64) -> f32 {
        let at = index - self.buffer_start;
        if at < 0 || index >= self.received as i64 {
            return 0.0;
        }
        self.buffer[channel]
            .get(at as usize)
            .copied()
            .unwrap_or(0.0)
    }

    /// Encodes the block at `self.center` into `self.bits`.
    fn encode_block(&mut self, next_long: bool) {
        let long = self.long;
        let size = usize::from(long);
        let n = self.sizes[size];
        let half = n / 2;
        let previous_long = self.previous_long.unwrap_or(long);
        // The window the decoder will put on this block.
        self.window[..n].copy_from_slice(&window(
            n,
            self.sizes[0],
            long,
            previous_long,
            next_long,
            &self.slopes,
        ));

        let start = self.center - half as i64;
        for channel in 0..self.channels {
            for i in 0..n {
                self.windowed[i] = self.sample(channel, start + i as i64) * self.window[i];
            }
            let block = &mut self.blocks[size];
            block
                .mdct
                .forward(&self.windowed[..n], &mut self.spectra[channel][..half]);
        }

        // Floors and residues, channel by channel.
        for channel in 0..self.channels {
            let block = &mut self.blocks[size];
            block
                .model
                .threshold(&self.spectra[channel][..half], &mut self.threshold[..half]);
            let used = fit_floor(
                &block.floor,
                &self.threshold[..half],
                &self.spectra[channel][..half],
                self.floor_factor,
                &mut self.floor_codes[channel],
                &mut self.final_y,
            );
            self.floor_used[channel] = used;
            let quantized = &mut self.quantized[channel][..half];
            if used {
                render_floor1(
                    &block.floor,
                    &self.floor_codes[channel],
                    &mut self.final_y,
                    &mut self.step2,
                    &self.inverse_db,
                    &mut self.curve[..half],
                );
                let reach = CLASS_REACH[CLASS_REACH.len() - 1];
                for ((q, &x), &gain) in quantized
                    .iter_mut()
                    .zip(&self.spectra[channel][..half])
                    .zip(&self.curve[..half])
                {
                    *q = ((x / gain).round() as i32).clamp(-reach, reach);
                }
                if quantized.iter().all(|&q| q == 0) {
                    self.floor_used[channel] = false;
                }
            } else {
                quantized.fill(0);
            }
        }

        // The packet: type, mode, window flags.
        let bits = &mut self.bits;
        bits.write(0, 1);
        bits.write(u32::from(long), 1);
        if long {
            bits.write(u32::from(previous_long), 1);
            bits.write(u32::from(next_long), 1);
        }
        let floor = &self.blocks[size].floor;
        let book = &self.books[usize::from(BOOK_FLOOR)];
        for channel in 0..self.channels {
            if !self.floor_used[channel] {
                bits.write(0, 1);
                continue;
            }
            bits.write(1, 1);
            let codes = &self.floor_codes[channel];
            let width = ilog(FLOOR_RANGE as u32 - 1);
            bits.write(codes[0] as u32, width);
            bits.write(codes[1] as u32, width);
            for &code in &codes[2..floor.x.len()] {
                book.write(bits, code as usize);
            }
        }

        // Coupling: the square polar mapping, on the rounded values, which
        // the decoder undoes exactly.
        if self.channels == 2 {
            let (left, right) = self.quantized.split_at_mut(1);
            for (l, r) in left[0][..half].iter_mut().zip(&mut right[0][..half]) {
                let (magnitude, angle) = couple(*l, *r);
                *l = magnitude;
                *r = angle;
            }
        }

        // The residue, unless every channel is silent: of type 2, one
        // vector of the channels interleaved; of type 1, the vectors of
        // the channels whose floor is used.
        if self.floor_used.iter().any(|&used| used) {
            let channels = self.channels;
            if interleaved(channels) {
                for i in 0..half {
                    for channel in 0..channels {
                        self.interleaved[i * channels + channel] = self.quantized[channel][i];
                    }
                }
                let vectors = [&self.interleaved[..half * channels]];
                encode_residue(&vectors, &self.books, &mut self.classes, &mut self.bits);
            } else {
                let mut vectors: [&[i32]; 255] = [&[]; 255];
                let mut count = 0;
                for (channel, quantized) in self.quantized.iter().enumerate() {
                    if self.floor_used[channel] {
                        vectors[count] = &quantized[..half];
                        count += 1;
                    }
                }
                encode_residue(
                    &vectors[..count],
                    &self.books,
                    &mut self.classes,
                    &mut self.bits,
                );
            }
        }
    }

    /// Writes the pages finished, or what is left of them after a write
    /// that failed, which is written again from where it began.
    fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        self.pager.flush(writer)
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

/// The square polar mapping of section 1.3.3 run backwards: the magnitude
/// and angle that the decoder turns into `left` and `right`.
pub(super) fn couple(left: i32, right: i32) -> (i32, i32) {
    if left.abs() >= right.abs() {
        if left > 0 {
            (left, left - right)
        } else {
            (left, right - left)
        }
    } else if right > 0 {
        (right, left - right)
    } else {
        (right, right - left)
    }
}

/// The window of a block of `n` samples, long or short, between blocks of
/// the sizes given (section 4.3.1), as the decoder applies it.
fn window(
    n: usize,
    short: usize,
    long: bool,
    previous_long: bool,
    next_long: bool,
    slopes: &[Vec<f32>; 2],
) -> Vec<f32> {
    let (left_start, left_slope) = if long && !previous_long {
        (n / 4 - short / 4, 0)
    } else {
        (0, usize::from(long))
    };
    let (right_start, right_slope) = if long && !next_long {
        (n * 3 / 4 - short / 4, 0)
    } else {
        (n / 2, usize::from(long))
    };
    let mut out = vec![1.0f32; n];
    out[..left_start].fill(0.0);
    let rise = &slopes[left_slope];
    out[left_start..left_start + rise.len()].copy_from_slice(rise);
    let fall = &slopes[right_slope];
    for (value, &gain) in out[right_start..right_start + fall.len()]
        .iter_mut()
        .zip(fall.iter().rev())
    {
        *value = gain;
    }
    out[right_start + fall.len()..].fill(0.0);
    out
}

/// The window of a long block between long blocks.
fn full_window(n: usize, slope: &[f32]) -> Vec<f32> {
    let half = n / 2;
    (0..n)
        .map(|i| if i < half { slope[i] } else { slope[n - 1 - i] })
        .collect()
}

/// The floor value, in steps of the multiplier, of a gain.
fn floor_value(gain: f32) -> i32 {
    let index = 255.0 + 256.0 / 7.0 * f64::from(gain.max(1e-30)).log10();
    ((index / f64::from(FLOOR_MULTIPLIER)).round() as i32).clamp(0, FLOOR_RANGE - 1)
}

/// The Y of the line from `(x0, y0)` to `(x1, y1)` at `x` (section 9.2.6),
/// as the decoder predicts posts.
fn render_point(x0: i64, y0: i64, x1: i64, y1: i64, x: i64) -> i64 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    let offset = dy.abs() * (x - x0) / adx;
    if dy < 0 { y0 - offset } else { y0 + offset }
}

/// Fits the floor of a channel to its threshold: the value each post must
/// have for the noise to lie at the threshold times `factor`, coded against
/// the prediction from the posts around it (section 7.2.4 run backwards)
/// into `codes`. Returns whether the floor is used: not when the whole
/// spectrum lies under it.
fn fit_floor(
    floor: &Floor1,
    threshold: &[f32],
    spectrum: &[f32],
    factor: f32,
    codes: &mut [i32],
    final_y: &mut [i64],
) -> bool {
    let half = threshold.len();
    let points = floor.x.len();
    // The gain wanted at each bin, and whether anything rises above it.
    let mut audible = false;
    let gain = |bin: usize| (threshold[bin] * factor).sqrt();
    for (bin, &x) in spectrum.iter().enumerate() {
        if x.abs() >= 0.5 * gain(bin) {
            audible = true;
            break;
        }
    }
    if !audible {
        return false;
    }
    // Each post takes the lowest gain wanted between it and the posts on
    // either side of it, halfway.
    let mut target = vec![0i32; points];
    for (rank, &index) in floor.sorted.iter().enumerate() {
        let index = usize::from(index);
        let x = usize::from(floor.x[index]).min(half - 1);
        let below = if rank == 0 {
            0
        } else {
            usize::from(floor.x[usize::from(floor.sorted[rank - 1])])
        };
        let above = floor
            .sorted
            .get(rank + 1)
            .map_or(half, |&next| usize::from(floor.x[usize::from(next)]));
        let from = ((below + x) / 2).min(half - 1);
        let to = ((x + above) / 2).clamp(from + 1, half);
        let lowest = (from..to).map(gain).fold(f32::MAX, f32::min);
        // Never so low that the values around the post outgrow the books.
        let largest = spectrum[from..to]
            .iter()
            .fold(0.0f32, |most, &x| most.max(x.abs()));
        target[index] = floor_value(lowest.max(largest / LARGEST_VALUE));
    }

    // The codes, in the order the decoder reads the posts.
    let range = i64::from(FLOOR_RANGE);
    codes[0] = target[0];
    codes[1] = target[1];
    final_y[0] = i64::from(target[0]);
    final_y[1] = i64::from(target[1]);
    for point in 2..points {
        let low = usize::from(floor.low_neighbour[point]);
        let high = usize::from(floor.high_neighbour[point]);
        let x = |index: usize| i64::from(floor.x[index]);
        let predicted = render_point(x(low), final_y[low], x(high), final_y[high], x(point));
        let mut wanted = i64::from(target[point]);
        // A post near enough its prediction takes it, which costs the
        // fewest bits: a little lower than wanted, when the residue pays
        // for it, or a step higher, a little more noise.
        if (-1..=2).contains(&(wanted - predicted)) {
            wanted = predicted;
        }
        let high_room = range - predicted;
        let low_room = predicted;
        let room = 2 * high_room.min(low_room);
        let delta = wanted - predicted;
        let code = if delta == 0 {
            0
        } else if delta > 0 && 2 * delta < room {
            2 * delta
        } else if delta < 0 && -2 * delta - 1 < room {
            -2 * delta - 1
        } else if high_room > low_room {
            wanted
        } else {
            range - 1 - wanted
        };
        codes[point] = code as i32;
        final_y[point] = wanted;
    }
    true
}

/// Codes a residue (section 8.6.2 run backwards) of one vector, for type
/// 2, or of one vector a channel, for type 1, all the same length: the
/// classes of their partitions, two to a codeword, a codeword of each
/// vector in turn, then each partition's values pass by pass, partition by
/// partition and vector by vector within each.
fn encode_residue(
    vectors: &[&[i32]],
    books: &[Book],
    classes: &mut [Vec<u8>],
    bits: &mut BitWriter,
) {
    let Some(first) = vectors.first() else {
        return;
    };
    let partitions = first.len() / PARTITION;
    for (values, classes) in vectors.iter().zip(classes.iter_mut()) {
        for (partition, class) in classes.iter_mut().enumerate().take(partitions) {
            let part = &values[partition * PARTITION..(partition + 1) * PARTITION];
            let largest = part.iter().map(|value| value.abs()).max().unwrap_or(0);
            *class = CLASS_REACH
                .iter()
                .position(|&reach| largest <= reach)
                .unwrap_or(CLASS_REACH.len() - 1) as u8;
        }
    }
    let class_count = CLASS_REACH.len();
    let words = books[usize::from(BOOK_CLASS)].dimensions;
    let mut scratch = [0i32; PARTITION];
    // The pass picks a book of each class, not an element of one list.
    #[allow(clippy::needless_range_loop)]
    for pass in 0..3 {
        let mut partition = 0;
        while partition < partitions {
            if pass == 0 {
                for classes in classes.iter().take(vectors.len()) {
                    let mut entry = 0usize;
                    for digit in 0..words {
                        let class = if partition + digit < partitions {
                            classes[partition + digit]
                        } else {
                            0
                        };
                        entry = entry * class_count + usize::from(class);
                    }
                    books[usize::from(BOOK_CLASS)].write(bits, entry);
                }
            }
            for _ in 0..words {
                if partition >= partitions {
                    break;
                }
                for (values, classes) in vectors.iter().zip(classes.iter()) {
                    let class = usize::from(classes[partition]);
                    let Some(book) = CLASS_BOOKS[class][pass] else {
                        continue;
                    };
                    let part = &values[partition * PARTITION..(partition + 1) * PARTITION];
                    // What this pass codes of each value: for a cascade,
                    // the coarse part first, then what is left.
                    for (slot, &value) in scratch.iter_mut().zip(part) {
                        *slot = pass_value(value, class, pass);
                    }
                    let book = &books[usize::from(book)];
                    for vector in scratch.chunks(book.dimensions) {
                        book.write(bits, book.entry_of(vector));
                    }
                }
                partition += 1;
            }
        }
    }
}

/// The part of `value` that pass `pass` of class `class` codes.
fn pass_value(value: i32, class: usize, pass: usize) -> i32 {
    let passes = CLASS_BOOKS[class];
    let mut rest = value;
    for (index, book) in passes.iter().enumerate() {
        let Some(book) = book else {
            break;
        };
        let step = match *book {
            BOOK_X256 => 256,
            BOOK_X16 => 16,
            _ => 1,
        };
        let reach = if step == 1 {
            CLASS_REACH[class].min(16)
        } else {
            8 * step
        };
        let coarse = if step == 1 {
            rest.clamp(-reach, reach)
        } else {
            ((f64::from(rest) / f64::from(step)).round() as i32 * step).clamp(-reach, reach)
        };
        if index == pass {
            return coarse;
        }
        rest -= coarse;
    }
    0
}

/// Writes `buffer` to `writer` as Ogg Vorbis.
pub(crate) fn write_buffer<W: Write + Seek>(
    writer: &mut W,
    buffer: &AudioBuffer<f32>,
    sample_rate: u32,
    options: &VorbisOptions,
) -> Result<(), VorbisError> {
    let mut stream = VorbisStream::start(writer, buffer.num_channels(), sample_rate, options)?;
    stream.write_planar(writer, buffer)?;
    stream.finish(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coupling_is_undone_by_the_decoder() {
        // The decoder's inverse, as section 1.3.3 states it.
        let decouple = |m: i32, a: i32| -> (i32, i32) {
            if m > 0 {
                if a > 0 { (m, m - a) } else { (m + a, m) }
            } else if a > 0 {
                (m, m + a)
            } else {
                (m - a, m)
            }
        };
        for left in -20..=20 {
            for right in -20..=20 {
                let (m, a) = couple(left, right);
                assert_eq!(decouple(m, a), (left, right), "{left} {right}");
            }
        }
    }

    #[test]
    fn the_posts_are_distinct_and_within_the_band() {
        for (half, count) in [(1_024, 32), (128, 12), (64, 12)] {
            let posts = floor_posts(half, count);
            assert_eq!(posts.len(), count);
            let mut sorted = posts.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), count);
            assert!(sorted[0] > 0 && usize::from(*sorted.last().unwrap()) < half);
        }
    }

    /// With the floor far under the threshold, what comes back is what went
    /// in, to the precision of the rounding.
    #[test]
    fn a_floor_far_down_gives_the_samples_back() {
        for (channels, frequency) in [(1usize, 1_000.0f64), (1, 437.0), (2, 437.0), (2, 1_000.0)] {
            let rate = 48_000;
            let frames = 24_000;
            let mut buffer = AudioBuffer::<f32>::new(channels, frames);
            for (channel, samples) in buffer.iter_channels_mut().enumerate() {
                for (i, sample) in samples.iter_mut().enumerate() {
                    *sample = (0.5
                        * (std::f64::consts::TAU
                            * frequency
                            * (1.0 + 0.1 * channel as f64)
                            * i as f64
                            / f64::from(rate))
                        .sin()) as f32;
                }
            }
            let options = VorbisOptions::new(10.0);
            let mut out = std::io::Cursor::new(Vec::new());
            let mut stream = VorbisStream::start(&mut out, channels, rate, &options).unwrap();
            stream.floor_factor = 1e-3;
            stream.write_planar(&mut out, &buffer).unwrap();
            stream.finish(&mut out).unwrap();
            let decoded = crate::vorbis::read(out.into_inner().as_slice())
                .unwrap()
                .buffer;
            assert_eq!(decoded.num_frames(), frames);
            let (mut signal, mut noise) = (0.0f64, 0.0f64);
            for channel in 0..channels {
                for i in 2_000..frames - 2_000 {
                    let a = f64::from(buffer.channel(channel)[i]);
                    let b = f64::from(decoded.channel(channel)[i]);
                    signal += a * a;
                    noise += (a - b) * (a - b);
                }
            }
            let snr = 10.0 * (signal / noise).log10();
            assert!(snr > 50.0, "{channels} channels: {snr:.1} dB");
        }
    }

    #[test]
    fn cascades_add_up_to_their_values() {
        for class in 0..CLASS_REACH.len() {
            let reach = CLASS_REACH[class];
            for value in -reach..=reach {
                let sum: i32 = (0..3).map(|pass| pass_value(value, class, pass)).sum();
                assert_eq!(sum, value, "class {class}, value {value}");
                for (pass, book) in CLASS_BOOKS[class].iter().enumerate() {
                    if let Some(book) = *book {
                        let lattice = design_books()[usize::from(book)].lattice.unwrap();
                        let part = pass_value(value, class, pass);
                        assert!(
                            part.abs() <= lattice.reach() && part % lattice.step == 0,
                            "{class} {value} {pass}"
                        );
                    }
                }
            }
        }
    }
}
