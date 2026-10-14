//! Tests of the encoder that look inside what it writes.
//!
//! The stream is decoded with this crate's decoder, which gives back
//! integers here so that 32-bit audio is compared exactly, and is also
//! taken apart by a small inspector written for these tests, which reports
//! how every subframe was coded. The inspector only reads; whether a stream
//! decodes to the right samples is the decoder's word, and FFmpeg's (see
//! `tests/data/README.md`).

use std::io::Cursor;

use super::*;
use crate::flac::{BitReader, FrameDecoder, Stereo as Decorrelation, frame_header, read_metadata};

/// A small deterministic random number generator (xorshift64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn full(bits: u32) -> i64 {
    1 << (bits - 1)
}

/// The kinds of signal every combination is tried with.
#[derive(Debug, Clone, Copy)]
enum Signal {
    Silence,
    /// Full-scale square wave: the largest steps the format can be given.
    Square,
    /// Uniform white noise over the whole range.
    Noise,
    /// A sine sweeping from 20 Hz to nearly half the sample rate.
    Sweep,
}

const SIGNALS: [Signal; 4] = [
    Signal::Silence,
    Signal::Square,
    Signal::Noise,
    Signal::Sweep,
];

fn signal(kind: Signal, bits: u32, len: usize, seed: u64) -> Vec<i32> {
    let top = full(bits);
    let mut rng = Rng(seed | 1);
    (0..len)
        .map(|index| {
            let value = match kind {
                Signal::Silence => 0,
                Signal::Square => {
                    if (index / 7 + seed as usize) % 2 == 0 {
                        top - 1
                    } else {
                        -top
                    }
                }
                Signal::Noise => (rng.next() % (2 * top as u64)) as i64 - top,
                Signal::Sweep => {
                    let t = index as f64 / 44_100.0;
                    let span = len as f64 / 44_100.0;
                    // Linear chirp: the phase is the integral of the
                    // frequency.
                    let (start, end) = (20.0, 20_000.0);
                    let phase = 2.0
                        * std::f64::consts::PI
                        * (start * t + (end - start) * t * t / (2.0 * span));
                    (0.9 * (top - 1) as f64 * (phase + seed as f64).sin()).round() as i64
                }
            };
            value as i32
        })
        .collect()
}

fn encode(channels: &[Vec<i32>], sample_rate: u32, options: &FlacOptions) -> Vec<u8> {
    let slices: Vec<&[i32]> = channels.iter().map(Vec::as_slice).collect();
    let mut bytes = Vec::new();
    write_integers(&mut bytes, &slices, sample_rate, options).unwrap();
    bytes
}

/// What the stream information says.
struct Info {
    min_block: u16,
    max_block: u16,
    min_frame: u32,
    max_frame: u32,
    sample_rate: u32,
    channels: usize,
    bits: u32,
    total: u64,
    md5: [u8; 16],
}

fn stream_info(bytes: &[u8]) -> Info {
    assert_eq!(&bytes[..4], b"fLaC");
    assert_eq!(
        bytes[4] & 0x7F,
        0,
        "the first block is the stream information"
    );
    let body = &bytes[8..42];
    let packed = u64::from_be_bytes(body[10..18].try_into().unwrap());
    Info {
        min_block: u16::from_be_bytes([body[0], body[1]]),
        max_block: u16::from_be_bytes([body[2], body[3]]),
        min_frame: u32::from_be_bytes([0, body[4], body[5], body[6]]),
        max_frame: u32::from_be_bytes([0, body[7], body[8], body[9]]),
        sample_rate: (packed >> 44) as u32,
        channels: ((packed >> 41) & 7) as usize + 1,
        bits: ((packed >> 36) & 31) as u32 + 1,
        total: packed & 0xF_FFFF_FFFF,
        md5: body[18..34].try_into().unwrap(),
    }
}

/// The metadata blocks: kind, whether marked last, and body.
fn metadata_blocks(bytes: &[u8]) -> Vec<(u8, bool, &[u8])> {
    let mut blocks = Vec::new();
    let mut position = 4;
    loop {
        let last = bytes[position] & 0x80 != 0;
        let kind = bytes[position] & 0x7F;
        let len = u32::from_be_bytes([
            0,
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]) as usize;
        blocks.push((kind, last, &bytes[position + 4..position + 4 + len]));
        position += 4 + len;
        if last {
            return blocks;
        }
    }
}

/// Decodes a stream with this crate's decoder, keeping the integers.
fn decode_integers(bytes: &[u8]) -> Vec<Vec<i64>> {
    let metadata = read_metadata(&mut Cursor::new(bytes), false).unwrap();
    let mut channels = vec![Vec::new(); metadata.info.channels];
    let mut reader = BitReader::new(&bytes[metadata.audio_start as usize..]);
    let mut decoder = FrameDecoder::new();
    while !reader.at_end() {
        let header = decoder.decode(&mut reader, &metadata.info).unwrap();
        for (out, samples) in channels.iter_mut().zip(&decoder.block) {
            out.extend_from_slice(&samples[..header.block_size]);
        }
    }
    channels
}

/// How one subframe was coded, as the inspector read it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Coding {
    Constant,
    Verbatim,
    Fixed(usize),
    Lpc { order: usize, precision: u32 },
}

#[derive(Debug, Clone)]
struct SubframeFacts {
    /// Bits the subframe takes in the stream, its header included.
    bits: usize,
    coding: Coding,
    wasted: u32,
    partition_order: u32,
    wide: bool,
    escapes: usize,
    zero_escapes: usize,
    largest_residual: u64,
}

#[derive(Debug, Clone)]
struct FrameFacts {
    number: u64,
    block_size: usize,
    stereo: Decorrelation,
    subframes: Vec<SubframeFacts>,
    bytes: usize,
}

/// Takes apart every frame of a stream.
fn inspect(bytes: &[u8]) -> Vec<FrameFacts> {
    let metadata = read_metadata(&mut Cursor::new(bytes), false).unwrap();
    let info = metadata.info;
    let audio = &bytes[metadata.audio_start as usize..];
    let mut reader = BitReader::new(audio);
    let mut frames = Vec::new();
    while !reader.at_end() {
        let start = reader.byte_position();
        let header = frame_header(&mut reader, &info).unwrap();
        let mut subframes = Vec::new();
        for channel in 0..header.channels {
            let side = matches!(
                (header.stereo, channel),
                (Decorrelation::LeftSide, 1)
                    | (Decorrelation::SideRight, 0)
                    | (Decorrelation::MidSide, 1)
            );
            let depth = header.bits_per_sample + u32::from(side);
            let start = reader.position;
            let mut facts = inspect_subframe(&mut reader, header.block_size, depth);
            facts.bits = reader.position - start;
            subframes.push(facts);
        }
        reader.align();
        reader.bits(16).unwrap();
        frames.push(FrameFacts {
            number: header.number,
            block_size: header.block_size,
            stereo: header.stereo,
            subframes,
            bytes: reader.byte_position() - start,
        });
    }
    frames
}

fn inspect_subframe(reader: &mut BitReader<'_>, len: usize, depth: u32) -> SubframeFacts {
    assert_eq!(reader.bits(1).unwrap(), 0);
    let kind = reader.bits(6).unwrap() as usize;
    let wasted = if reader.bits(1).unwrap() == 1 {
        reader.unary().unwrap() as u32 + 1
    } else {
        0
    };
    let bits = depth - wasted;
    let mut facts = SubframeFacts {
        bits: 0,
        coding: Coding::Verbatim,
        wasted,
        partition_order: 0,
        wide: false,
        escapes: 0,
        zero_escapes: 0,
        largest_residual: 0,
    };
    let order = match kind {
        0 => {
            reader.signed(bits).unwrap();
            facts.coding = Coding::Constant;
            return facts;
        }
        1 => {
            for _ in 0..len {
                reader.signed(bits).unwrap();
            }
            return facts;
        }
        8..=12 => {
            let order = kind - 8;
            facts.coding = Coding::Fixed(order);
            for _ in 0..order {
                reader.signed(bits).unwrap();
            }
            order
        }
        32..=63 => {
            let order = kind - 31;
            for _ in 0..order {
                reader.signed(bits).unwrap();
            }
            let precision = reader.bits(4).unwrap() as u32 + 1;
            assert!(precision < 16);
            let shift = reader.signed(5).unwrap();
            assert!((0..=15).contains(&shift), "shift {shift}");
            for _ in 0..order {
                reader.signed(precision).unwrap();
            }
            facts.coding = Coding::Lpc { order, precision };
            order
        }
        _ => panic!("reserved subframe type {kind}"),
    };

    let wide = reader.bits(2).unwrap();
    assert!(wide < 2);
    facts.wide = wide == 1;
    let width = if facts.wide { 5 } else { 4 };
    facts.partition_order = reader.bits(4).unwrap() as u32;
    let partitions = 1 << facts.partition_order;
    assert_eq!(len % partitions, 0);
    assert!(
        len / partitions > order,
        "the first partition holds a residual"
    );
    for partition in 0..partitions {
        let count = len / partitions - if partition == 0 { order } else { 0 };
        let parameter = reader.bits(width).unwrap();
        if parameter == (1 << width) - 1 {
            facts.escapes += 1;
            let raw = reader.bits(5).unwrap() as u32;
            facts.zero_escapes += usize::from(raw == 0);
            for _ in 0..count {
                let value = reader.signed(raw).unwrap();
                facts.largest_residual = facts.largest_residual.max(value.unsigned_abs());
            }
        } else {
            for _ in 0..count {
                let folded =
                    (reader.unary().unwrap() << parameter) | reader.bits(parameter as u32).unwrap();
                let value = (folded >> 1) as i64 ^ -((folded & 1) as i64);
                facts.largest_residual = facts.largest_residual.max(value.unsigned_abs());
            }
        }
    }
    facts
}

/// The MD5 digest FLAC defines for these samples, packed here without the
/// encoder's code.
fn reference_md5(channels: &[Vec<i32>], bits: u32) -> [u8; 16] {
    let width = bits.div_ceil(8) as usize;
    let mut bytes = Vec::new();
    for index in 0..channels[0].len() {
        for channel in channels {
            let value = i64::from(channel[index]);
            for byte in 0..width {
                bytes.push((value >> (8 * byte)) as u8);
            }
        }
    }
    let mut hash = Md5::new();
    hash.update(&bytes);
    hash.finish()
}

/// Checks everything that must hold of any stream the encoder writes.
fn check_stream(
    bytes: &[u8],
    channels: &[Vec<i32>],
    bits: u32,
    sample_rate: u32,
    options: &FlacOptions,
    what: &str,
) {
    let len = channels[0].len();
    let decoded = decode_integers(bytes);
    assert_eq!(decoded.len(), channels.len(), "{what}");
    for (channel, (got, want)) in decoded.iter().zip(channels).enumerate() {
        assert_eq!(got.len(), len, "{what}: channel {channel} length");
        let first_difference = got
            .iter()
            .zip(want)
            .position(|(&got, &want)| got != i64::from(want));
        assert_eq!(first_difference, None, "{what}: channel {channel} differs");
    }

    let info = stream_info(bytes);
    let block_size = options.block_size();
    assert_eq!(u32::from(info.min_block), block_size, "{what}");
    assert_eq!(u32::from(info.max_block), block_size, "{what}");
    assert_eq!(info.sample_rate, sample_rate, "{what}");
    assert_eq!(info.channels, channels.len(), "{what}");
    assert_eq!(info.bits, bits, "{what}");
    assert_eq!(info.total, len as u64, "{what}");
    assert_eq!(info.md5, reference_md5(channels, bits), "{what}: checksum");

    let frames = inspect(bytes);
    assert_eq!(frames.len(), len.div_ceil(block_size as usize), "{what}");
    let sizes = frames.iter().map(|frame| frame.bytes as u32);
    assert_eq!(info.min_frame, sizes.clone().min().unwrap_or(0), "{what}");
    assert_eq!(info.max_frame, sizes.max().unwrap_or(0), "{what}");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.number, index as u64, "{what}");
        let expected = (len - index * block_size as usize).min(block_size as usize);
        assert_eq!(frame.block_size, expected, "{what}");
        // Never larger than the samples stored plainly with the frame
        // header, the subframe headers and the checksum.
        let plain = (channels.len() * (8 + frame.block_size * bits as usize)).div_ceil(8);
        assert!(
            frame.bytes <= 16 + plain + 2,
            "{what}: frame {index} of {} bytes",
            frame.bytes
        );
        for subframe in &frame.subframes {
            assert!(
                subframe.largest_residual < 1 << 31,
                "{what}: a residual of {} in frame {index}",
                subframe.largest_residual
            );
        }
    }
}

#[test]
fn every_level_depth_and_signal_decodes_to_what_went_in() {
    // Lengths that are not a multiple of either block size the levels use.
    let len = 5_000;
    for level in 0..=8 {
        for bits in [4, 8, 12, 16, 20, 24, 32] {
            let options = FlacOptions::new(bits).with_level(level).with_padding(0);
            for kind in SIGNALS {
                let mono = [signal(kind, bits, len, 1)];
                let what = format!("level {level}, {bits} bits, {kind:?}, mono");
                check_stream(
                    &encode(&mono, 44_100, &options),
                    &mono,
                    bits,
                    44_100,
                    &options,
                    &what,
                );

                // The second channel is a different signal of the same kind,
                // so that all four stereo codings are in play.
                let stereo = [signal(kind, bits, len, 2), signal(kind, bits, len, 3)];
                let what = format!("level {level}, {bits} bits, {kind:?}, stereo");
                check_stream(
                    &encode(&stereo, 44_100, &options),
                    &stereo,
                    bits,
                    44_100,
                    &options,
                    &what,
                );
            }
        }
    }
}

#[test]
fn odd_bit_depths_are_carried_by_the_stream_information() {
    for bits in [5, 7, 10, 15, 17, 18, 23, 31] {
        let channels = [
            signal(Signal::Sweep, bits, 3_000, 4),
            signal(Signal::Noise, bits, 3_000, 5),
        ];
        let options = FlacOptions::new(bits).with_level(5);
        let bytes = encode(&channels, 48_000, &options);
        check_stream(
            &bytes,
            &channels,
            bits,
            48_000,
            &options,
            &format!("{bits} bits"),
        );
    }
}

#[test]
fn one_to_eight_channels_decode_to_what_went_in() {
    for channels in 1..=8 {
        let audio: Vec<Vec<i32>> = (0..channels)
            .map(|channel| signal(SIGNALS[channel % 4], 16, 6_000, channel as u64 + 10))
            .collect();
        for level in [0, 5, 8] {
            let options = FlacOptions::new(16).with_level(level);
            let bytes = encode(&audio, 48_000, &options);
            let what = format!("{channels} channels at level {level}");
            check_stream(&bytes, &audio, 16, 48_000, &options, &what);
        }
    }
}

#[test]
fn any_block_size_decodes_to_what_went_in() {
    for block_size in [
        16, 17, 100, 192, 255, 256, 257, 576, 1000, 1152, 4096, 4608, 8192, 16_384, 65_535,
    ] {
        let len = block_size as usize * 2 + 77;
        let audio = [
            signal(Signal::Sweep, 16, len, 1),
            signal(Signal::Noise, 16, len, 2),
        ];
        for level in [0, 5] {
            let options = FlacOptions::new(16)
                .with_level(level)
                .with_block_size(block_size);
            let bytes = encode(&audio, 44_100, &options);
            check_stream(
                &bytes,
                &audio,
                16,
                44_100,
                &options,
                &format!("blocks of {block_size}"),
            );
        }
    }
}

#[test]
fn short_inputs_decode_to_what_went_in() {
    for len in (0..=40).chain([1151, 1152, 1153, 4095, 4097]) {
        for level in [0, 5, 8] {
            let audio = [
                signal(Signal::Sweep, 24, len, 1),
                signal(Signal::Noise, 24, len, 2),
            ];
            let options = FlacOptions::new(24).with_level(level);
            let bytes = encode(&audio, 96_000, &options);
            let what = format!("{len} samples at level {level}");
            if len == 0 {
                assert!(inspect(&bytes).is_empty());
                assert_eq!(stream_info(&bytes).md5, reference_md5(&audio, 24));
                continue;
            }
            check_stream(&bytes, &audio, 24, 96_000, &options, &what);
        }
    }
}

#[test]
fn extremes_of_24_and_32_bits_do_not_overflow() {
    // Channels at opposite ends of the range make a side channel of 25 or
    // 33 bits at its limits, and steps that no fixed predictor above order
    // 0 can take without a residual of 2^31 or more at 32 bits. Built with
    // overflow checks, as the tests are, any overflow would stop the test.
    for bits in [24, 32] {
        let top = full(bits);
        let mut rng = Rng(77);
        let pattern = |rng: &mut Rng| match rng.next() % 4 {
            0 => top - 1,
            1 => -top,
            2 => 0,
            _ => (rng.next() % (2 * top as u64)) as i64 - top,
        };
        let left: Vec<i32> = (0..9_000).map(|_| pattern(&mut rng) as i32).collect();
        let right: Vec<i32> = left
            .iter()
            .map(|&value| (-1 - i64::from(value)) as i32)
            .collect();
        let alternating: Vec<i32> = (0..9_000)
            .map(|index| if index % 2 == 0 { top - 1 } else { -top } as i32)
            .collect();
        for level in 0..=8 {
            let options = FlacOptions::new(bits).with_level(level);
            for audio in [
                vec![left.clone(), right.clone()],
                vec![alternating.clone(), alternating.clone()],
                vec![alternating.clone(), right.clone()],
            ] {
                let bytes = encode(&audio, 192_000, &options);
                check_stream(
                    &bytes,
                    &audio,
                    bits,
                    192_000,
                    &options,
                    &format!("{bits} bits at level {level}"),
                );
            }
        }
    }
}

#[test]
fn the_checksum_matches_one_computed_by_another_implementation() {
    // The digests were computed with Python's hashlib over the same samples
    // packed as RFC 9639 says: interleaved, little-endian, sign-extended to
    // whole bytes.
    for (bits, digest) in [
        (8, "94f2467390ec1146d944d30ffbf6f06a"),
        (12, "41fba3f3da37d28771fa6bf68ad8d50c"),
        (20, "0832ff471c8a38fb894cd5f078e4767a"),
        (24, "e6bca43a0bea28895f6b5c221e3f2b26"),
        (32, "e6ca50170c66db2a0bcfb4cc223ddf3a"),
    ] {
        let top = full(bits);
        let left: Vec<i32> = (0..50i64)
            .map(|k| ((k * 7_919 * (top / 100 + 1)) % (2 * top) - top) as i32)
            .collect();
        let right: Vec<i32> = left
            .iter()
            .map(|&value| (-1 - i64::from(value)) as i32)
            .collect();
        let bytes = encode(&[left, right], 44_100, &FlacOptions::new(bits));
        let hex: String = stream_info(&bytes)
            .md5
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hex, digest, "{bits} bits");
    }
}

/// Counts the frames coded with each stereo decorrelation.
fn stereo_counts(bytes: &[u8]) -> [usize; 4] {
    let mut counts = [0; 4];
    for frame in inspect(bytes) {
        counts[match frame.stereo {
            Decorrelation::Independent => 0,
            Decorrelation::LeftSide => 1,
            Decorrelation::SideRight => 2,
            Decorrelation::MidSide => 3,
        }] += 1;
    }
    counts
}

#[test]
fn each_stereo_decorrelation_is_chosen_where_it_is_smallest() {
    // A and C are loud and unrelated, B is quieter and unrelated to both.
    let len = 4096 * 6;
    let mut rng = Rng(5);
    let mut noise = |amplitude: i64| -> Vec<i64> {
        (0..len)
            .map(|_| (rng.next() % (2 * amplitude as u64)) as i64 - amplitude)
            .collect()
    };
    let (a, b, c) = (noise(4_000), noise(1_000), noise(4_000));
    let pair = |left: &dyn Fn(usize) -> i64, right: &dyn Fn(usize) -> i64| {
        [
            (0..len).map(|i| left(i) as i32).collect::<Vec<_>>(),
            (0..len).map(|i| right(i) as i32).collect(),
        ]
    };
    let cases = [
        // Unrelated channels: any combination is louder than they are.
        (pair(&|i| a[i], &|i| c[i]), 0, "independent"),
        // Left is the quietest channel and side is quieter still.
        (pair(&|i| a[i], &|i| a[i] + b[i]), 1, "left and side"),
        // Likewise with right.
        (pair(&|i| a[i] + b[i], &|i| a[i]), 2, "side and right"),
        // The mid channel is A itself.
        (pair(&|i| a[i] + b[i], &|i| a[i] - b[i]), 3, "mid and side"),
    ];
    for (audio, expected, name) in cases {
        for level in 0..=8 {
            let options = FlacOptions::new(16).with_level(level);
            let bytes = encode(&audio, 44_100, &options);
            check_stream(&bytes, &audio, 16, 44_100, &options, name);
            let counts = stereo_counts(&bytes);
            let frames: usize = counts.iter().sum();
            // Where all four are coded and compared, every frame takes the
            // right one; where they are estimated, nearly every frame.
            let needed = if Settings::level(level).exact_stereo() {
                frames
            } else {
                frames - 1
            };
            assert!(
                counts[expected] >= needed,
                "{name} at level {level}: {counts:?}"
            );
        }
    }
}

/// Two random walks that share part of their steps, the share changing
/// slowly, so that which stereo coding is smallest changes from block to
/// block and is often a close call.
fn correlated_walks(len: usize) -> [Vec<i32>; 2] {
    let mut rng = Rng(0x5EED);
    let (mut left, mut right) = (Vec::with_capacity(len), Vec::with_capacity(len));
    let (mut a, mut b) = (0.0f64, 0.0f64);
    for index in 0..len {
        let share = 0.5 + 0.5 * (index as f64 / 9_000.0).sin();
        let common = (rng.next() % 2_001) as f64 - 1_000.0;
        let own_a = (rng.next() % 2_001) as f64 - 1_000.0;
        let own_b = (rng.next() % 2_001) as f64 - 1_000.0;
        a = 0.995 * a + share * common + (1.0 - share) * own_a;
        b = 0.995 * b + share * common + (1.0 - share) * own_b;
        left.push(a.round().clamp(-32_768.0, 32_767.0) as i32);
        right.push(b.round().clamp(-32_768.0, 32_767.0) as i32);
    }
    [left, right]
}

#[test]
fn where_all_four_stereo_codings_are_compared_each_frame_takes_the_smallest() {
    // Each of the four channels a pair can be coded as (left, right, side
    // one bit wider, mid) is also encoded alone, at the same level. Planning
    // a channel does not depend on the rest of the frame, so the subframes
    // of those mono streams are the ones a stereo frame can hold, and their
    // sizes, read from the streams, give what each pairing costs.
    let [left, right] = correlated_walks(40 * 1152);
    let side: Vec<i32> = left.iter().zip(&right).map(|(&l, &r)| l - r).collect();
    let mid: Vec<i32> = left
        .iter()
        .zip(&right)
        .map(|(&l, &r)| (l + r) >> 1)
        .collect();
    for level in 0..=8 {
        let alone = |channel: &Vec<i32>, bits: u32| -> Vec<usize> {
            let options = FlacOptions::new(bits).with_level(level);
            inspect(&encode(std::slice::from_ref(channel), 44_100, &options))
                .iter()
                .map(|frame| frame.subframes[0].bits)
                .collect()
        };
        let (l, r, s, m) = (
            alone(&left, 16),
            alone(&right, 16),
            alone(&side, 17),
            alone(&mid, 16),
        );
        let options = FlacOptions::new(16).with_level(level);
        let stereo = inspect(&encode(&[left.clone(), right.clone()], 44_100, &options));
        let mut missed = 0;
        for (index, frame) in stereo.iter().enumerate() {
            let pairs = [
                l[index] + r[index],
                l[index] + s[index],
                s[index] + r[index],
                m[index] + s[index],
            ];
            let coded: usize = frame.subframes.iter().map(|subframe| subframe.bits).sum();
            let chosen = match frame.stereo {
                Decorrelation::Independent => 0,
                Decorrelation::LeftSide => 1,
                Decorrelation::SideRight => 2,
                Decorrelation::MidSide => 3,
            };
            assert_eq!(coded, pairs[chosen], "level {level}, frame {index}");
            if coded > *pairs.iter().min().unwrap() {
                missed += 1;
            }
        }
        println!(
            "level {level}: the smallest coding missed in {missed} of {} frames, {} bytes",
            stereo.len(),
            stereo.iter().map(|frame| frame.bytes).sum::<usize>()
        );
        // The levels that estimate may miss, as the documentation says;
        // the others never do.
        if Settings::level(level).exact_stereo() {
            assert_eq!(missed, 0, "level {level}");
        }
    }
}

#[test]
fn constant_blocks_take_one_value_and_noise_is_stored_plainly() {
    let len = 3 * 4096;
    let mut audio = vec![
        signal(Signal::Silence, 16, len, 0),
        signal(Signal::Noise, 16, len, 9),
    ];
    // A constant that is not zero, in the first channel.
    audio[0].iter_mut().for_each(|value| *value = -1234);
    let options = FlacOptions::new(16);
    let bytes = encode(&audio, 44_100, &options);
    check_stream(&bytes, &audio, 16, 44_100, &options, "constant and noise");
    for frame in inspect(&bytes) {
        assert_eq!(frame.stereo, Decorrelation::Independent);
        assert_eq!(frame.subframes[0].coding, Coding::Constant);
        assert_eq!(frame.subframes[1].coding, Coding::Verbatim);
    }
    // Header, one constant, the noise and the checksum: nothing else.
    let audio_bytes: usize = inspect(&bytes).iter().map(|frame| frame.bytes).sum();
    assert!(
        audio_bytes <= len * 2 + 3 * (16 + 3 + 1 + 2),
        "{audio_bytes}"
    );
}

#[test]
fn low_bits_that_are_always_zero_are_not_stored() {
    // 16-bit samples whose lowest three bits are zero: 13 bits of noise.
    let len = 2 * 4096;
    let audio: Vec<Vec<i32>> = [2, 4]
        .into_iter()
        .map(|seed| {
            signal(Signal::Noise, 13, len, seed)
                .into_iter()
                .map(|value| value * 8)
                .collect()
        })
        .collect();
    let options = FlacOptions::new(16);
    let bytes = encode(&audio, 44_100, &options);
    check_stream(&bytes, &audio, 16, 44_100, &options, "wasted bits");
    for frame in inspect(&bytes) {
        for subframe in &frame.subframes {
            assert_eq!(subframe.wasted, 3, "{frame:?}");
        }
    }
    let audio_bytes: usize = inspect(&bytes).iter().map(|frame| frame.bytes).sum();
    assert!(audio_bytes < len * 2 * 13 / 8 + 200, "{audio_bytes}");
}

#[test]
fn the_levels_use_the_predictors_they_promise() {
    let len = 4 * 4096;
    let audio = [
        signal(Signal::Sweep, 16, len, 1),
        signal(Signal::Sweep, 16, len, 2),
    ];
    let mut sizes = Vec::new();
    for level in 0..=8 {
        let options = FlacOptions::new(16).with_level(level);
        let bytes = encode(&audio, 44_100, &options);
        let frames = inspect(&bytes);
        let mut lpc = 0;
        for subframe in frames.iter().flat_map(|frame| &frame.subframes) {
            match subframe.coding {
                Coding::Lpc { order, precision } => {
                    lpc += 1;
                    assert!(level >= 3, "linear prediction at level {level}");
                    let max = Settings::level(level).max_lpc_order();
                    assert!(order <= max, "order {order} at level {level}");
                    assert!(precision <= 15);
                }
                Coding::Fixed(order) => assert!(order <= 4),
                _ => {}
            }
            assert!(subframe.partition_order <= Settings::level(level).max_partition_order());
        }
        // A sweep is what linear prediction is for.
        if level >= 3 {
            assert!(lpc > 0, "no linear prediction at level {level}");
        }
        sizes.push(bytes.len());
    }
    // More search never makes the result larger, from one family of levels
    // to the next.
    assert!(sizes[3] < sizes[0], "{sizes:?}");
    assert!(sizes[8] <= sizes[5], "{sizes:?}");
}

#[test]
fn silent_stretches_inside_a_block_become_empty_escaped_partitions() {
    // A tone with exact digital silence in the middle of every block.
    let len = 4 * 4096;
    let mut tone = signal(Signal::Sweep, 16, len, 3);
    for block in tone.chunks_mut(4096) {
        block[1024..3072].fill(0);
    }
    let options = FlacOptions::new(16).with_level(8);
    let bytes = encode(&[tone.clone()], 44_100, &options);
    check_stream(&bytes, &[tone], 16, 44_100, &options, "silent stretches");
    let empty: usize = inspect(&bytes)
        .iter()
        .flat_map(|frame| &frame.subframes)
        .map(|subframe| subframe.zero_escapes)
        .sum();
    assert!(empty >= 4, "{empty} empty partitions");
}

#[test]
fn loud_residuals_take_wide_rice_parameters() {
    // 24-bit noise that wanders, so that prediction leaves residuals of
    // about 2^16 with the two-sided exponential spread that Rice codes suit.
    let len = 2 * 4096;
    let mut rng = Rng(3);
    let mut previous = 0.0f64;
    let audio: Vec<i32> = (0..len)
        .map(|_| {
            let draw = rng.next();
            let uniform = ((draw >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
            let step = -65_536.0 * uniform.ln() * if draw & 1 == 0 { 1.0 } else { -1.0 };
            previous = (0.99 * previous + step).clamp(-8_388_608.0, 8_388_607.0);
            previous.round() as i32
        })
        .collect();
    let options = FlacOptions::new(24);
    let bytes = encode(std::slice::from_ref(&audio), 96_000, &options);
    check_stream(&bytes, &[audio], 24, 96_000, &options, "loud residuals");
    assert!(
        inspect(&bytes)
            .iter()
            .flat_map(|frame| &frame.subframes)
            .any(|subframe| subframe.wide)
    );
}

#[test]
fn the_seek_table_spans_the_stream_and_points_at_real_frames() {
    // At 1000 Hz a point is due every 10000 samples. 150000 samples need
    // 15 points; with room for 4, the spacing doubles twice, to 40000.
    let len = 150_000;
    let audio = [signal(Signal::Sweep, 16, len, 1)];
    let options = FlacOptions::new(16)
        .with_level(0)
        .with_seek_points(4)
        .with_padding(100);
    let bytes = encode(&audio, 1_000, &options);
    check_stream(&bytes, &audio, 16, 1_000, &options, "seek table");

    let blocks = metadata_blocks(&bytes);
    let kinds: Vec<u8> = blocks.iter().map(|block| block.0).collect();
    assert_eq!(kinds, [0, 4, 3, 1]);
    let table = blocks[2].2;
    assert_eq!(table.len() % 18, 0);
    let points: Vec<(u64, u64, u16)> = table
        .chunks_exact(18)
        .map(|point| {
            (
                u64::from_be_bytes(point[..8].try_into().unwrap()),
                u64::from_be_bytes(point[8..16].try_into().unwrap()),
                u16::from_be_bytes([point[16], point[17]]),
            )
        })
        .collect();
    let samples: Vec<u64> = points.iter().map(|point| point.0).collect();
    // Frames hold 1152 samples, so each point is the first frame at or
    // after a multiple of 40000.
    assert_eq!(samples, [0, 40_320, 80_640, 120_960]);
    // Every point is in use, so the padding is what was asked for.
    assert_eq!(blocks[3].2.len(), 100);

    // Each point leads to a frame that starts with the sample it names.
    let first_frame = bytes.len()
        - inspect(&bytes)
            .iter()
            .map(|frame| frame.bytes)
            .sum::<usize>();
    let mut offset = 0;
    let mut sample = 0;
    let mut starts = Vec::new();
    for frame in inspect(&bytes) {
        starts.push((sample, offset, frame.block_size as u16));
        sample += frame.block_size as u64;
        offset += frame.bytes as u64;
    }
    for point in &points {
        assert!(starts.contains(point), "{point:?}");
    }
    assert!(first_frame > 0);
}

#[test]
fn unused_seek_points_become_padding() {
    // At 1000 Hz, 12000 samples need two points; eight are reserved.
    let audio = [signal(Signal::Sweep, 16, 12_000, 1)];
    let options = FlacOptions::new(16)
        .with_level(0)
        .with_seek_points(8)
        .with_padding(0);
    let bytes = encode(&audio, 1_000, &options);
    let blocks = metadata_blocks(&bytes);
    let kinds: Vec<(u8, bool)> = blocks.iter().map(|block| (block.0, block.1)).collect();
    assert_eq!(kinds, [(0, false), (4, false), (3, false), (1, true)]);
    assert_eq!(blocks[2].2.len(), 2 * 18);
    // The padding block's own header takes four of the bytes freed.
    assert_eq!(blocks[3].2.len(), 6 * 18 - 4);
    assert_eq!(bytes.len(), {
        let all = encode(&audio, 1_000, &options.clone().with_seek_points(2));
        all.len() + 6 * 18
    });

    // No points needed at all, and no padding asked for: the table's room
    // is all padding.
    let options = FlacOptions::new(16).with_seek_points(3).with_padding(0);
    let bytes = encode(&[vec![]], 44_100, &options);
    let kinds: Vec<(u8, usize)> = metadata_blocks(&bytes)
        .iter()
        .map(|block| (block.0, block.2.len()))
        .collect();
    assert_eq!(kinds, [(0, 34), (4, kinds[1].1), (1, 3 * 18)]);

    // Neither: the comment is the last block.
    let options = FlacOptions::new(16).with_seek_points(0).with_padding(0);
    let bytes = encode(&audio, 44_100, &options);
    let blocks = metadata_blocks(&bytes);
    assert_eq!(blocks.len(), 2);
    assert!(blocks[1].1);
}

#[test]
fn the_vorbis_comment_names_the_encoder_and_holds_the_tags() {
    let options = FlacOptions::new(16)
        .with_tag("TITLE", "Maracatu atômico")
        .with_tag("artist", "Chico Science = Nação Zumbi");
    let bytes = encode(&[vec![0; 10]], 44_100, &options);
    let blocks = metadata_blocks(&bytes);
    let comment = blocks.iter().find(|block| block.0 == 4).unwrap().2;
    let number = |position: &mut usize| {
        let value = u32::from_le_bytes(comment[*position..*position + 4].try_into().unwrap());
        *position += 4;
        value as usize
    };
    let field = |position: &mut usize| {
        let len = number(position);
        let text = std::str::from_utf8(&comment[*position..*position + len]).unwrap();
        *position += len;
        text.to_string()
    };
    let mut position = 0;
    assert_eq!(field(&mut position), VENDOR);
    assert!(VENDOR.starts_with("manguebyte-formats "));
    assert_eq!(number(&mut position), 2);
    assert_eq!(field(&mut position), "TITLE=Maracatu atômico");
    assert_eq!(field(&mut position), "artist=Chico Science = Nação Zumbi");
    assert_eq!(position, comment.len());
}

#[test]
fn what_flac_cannot_hold_is_refused_before_anything_is_written() {
    let audio = |channels: usize| vec![vec![0i32; 10]; channels];
    let refused = |channels: &[Vec<i32>], rate: u32, options: &FlacOptions| {
        let slices: Vec<&[i32]> = channels.iter().map(Vec::as_slice).collect();
        let mut bytes = Vec::new();
        match write_integers(&mut bytes, &slices, rate, options) {
            Err(FlacError::Io(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert!(bytes.is_empty());
                error.to_string()
            }
            other => panic!("not refused: {other:?}"),
        }
    };
    let good = FlacOptions::new(16);
    // A tag as long as the Vorbis comment block can hold, given the vendor
    // string, the count of tags and the tag's own length: 4 bytes each.
    let longest = MAX_BLOCK_BODY - (4 + VENDOR.len() + 4) - 4 - "A=".len();
    for (channels, rate, options) in [
        (0, 44_100, good.clone()),
        (9, 44_100, good.clone()),
        (1, 0, good.clone()),
        (1, 1 << 20, good.clone()),
        (1, 44_100, FlacOptions::new(3)),
        (1, 44_100, FlacOptions::new(33)),
        (1, 44_100, good.clone().with_level(9)),
        (1, 44_100, good.clone().with_block_size(15)),
        (1, 44_100, good.clone().with_block_size(65_536)),
        (1, 44_100, good.clone().with_tag("A=B", "c")),
        (1, 44_100, good.clone().with_tag("", "c")),
        (1, 44_100, good.clone().with_tag("TÍTULO", "c")),
        (
            1,
            44_100,
            good.clone().with_tag("A", "b".repeat(longest + 1)),
        ),
        (1, 44_100, good.clone().with_padding(1 << 24)),
        (1, 44_100, good.clone().with_seek_points(1 << 20)),
    ] {
        let message = refused(&audio(channels), rate, &options);
        // `validate` refuses them too, with the same error.
        match options.validate(channels, rate) {
            Err(FlacError::Io(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert_eq!(error.to_string(), message);
            }
            other => panic!("{message}: {other:?}"),
        }
    }
    // What only the samples get wrong; the options are good.
    let message = refused(&[vec![0; 10], vec![0; 9]], 44_100, &good);
    assert!(message.contains("same number"), "{message}");
    let message = refused(&[vec![32_768]], 44_100, &good);
    assert!(message.contains("does not fit"), "{message}");
    assert!(good.validate(2, 44_100).is_ok());

    // The largest metadata blocks: 2^24 - 1 bytes of body, and with seek
    // points and padding both, the padding, the padding block's header
    // and the whole table must fit one block, since unused points join it.
    let max = MAX_BLOCK_BODY as u32;
    let most_points = max / 18;
    assert_eq!(most_points, 932_067);
    for (padding, points, accepted) in [
        (max, 0, true),
        (max + 1, 0, false),
        (0, most_points, true),
        (0, most_points + 1, false),
        (max - 4 - 18 * most_points, most_points, true),
        (max - 4 - 18 * most_points + 1, most_points, false),
        (max - 4 - 18, 1, true),
        (max - 4 - 18 + 1, 1, false),
    ] {
        let options = good.clone().with_padding(padding).with_seek_points(points);
        assert_eq!(
            options.validate(1, 44_100).is_ok(),
            accepted,
            "padding {padding}, {points} seek points"
        );
    }
    // Written, the longest tag fills its block exactly.
    let bytes = encode(
        &audio(1),
        44_100,
        &good.clone().with_tag("A", "b".repeat(longest)),
    );
    let comment = metadata_blocks(&bytes)[1];
    assert_eq!((comment.0, comment.2.len()), (4, MAX_BLOCK_BODY));

    // The limits themselves are accepted.
    let samples = [-32_768, 32_767];
    let slices: Vec<&[i32]> = vec![&samples[..]; 8];
    let mut bytes = Vec::new();
    write_integers(
        &mut bytes,
        &slices,
        (1 << 20) - 1,
        &good.clone().with_block_size(16),
    )
    .unwrap();
    let info = stream_info(&bytes);
    assert_eq!((info.channels, info.sample_rate), (8, (1 << 20) - 1));
}

#[test]
fn a_stream_cannot_have_more_frames_than_a_header_can_number() {
    // Reaching frame 2^31 for real takes 2^35 samples or more, so the count
    // is set just below it.
    let last = (1u64 << 31) - 1;
    let options = FlacOptions::new(16)
        .with_block_size(16)
        .with_seek_points(0)
        .with_padding(0);
    let samples = vec![100i32; 40];

    // The whole-buffer writer writes nothing.
    let mut encoder = Encoder::new(1, 44_100, &options).unwrap();
    encoder.frame_number = last;
    let mut bytes = Vec::new();
    let mut done = 0;
    let result = encode_all(&mut bytes, encoder, |encoder| {
        let taken = encoder.take_integers(&[samples.as_slice()], done);
        done += taken;
        taken
    });
    match result {
        Err(FlacError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
        other => panic!("{other:?}"),
    }
    assert!(bytes.is_empty());

    // The streaming writer writes the last frame there can be, refuses the
    // next every time it is tried, and still completes the stream before
    // it.
    let mut out = Cursor::new(Vec::new());
    let mut stream = FlacStream::start(&mut out, 1, 44_100, &options).unwrap();
    stream.encoder.frame_number = last;
    let mut buffer = AudioBuffer::<f32>::new(1, 40);
    buffer.channel_mut(0).fill(0.25);
    for attempt in 0..3 {
        let error = if attempt == 0 {
            stream.write_planar(&mut out, &buffer)
        } else {
            stream.write_silence(&mut out, 0)
        };
        assert_eq!(
            error.unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "attempt {attempt}"
        );
        // One frame written, one waiting, the rest not taken.
        assert_eq!(stream.frames(), 32);
    }
    assert!(stream.finish(&mut out).is_err());
    let bytes = out.into_inner();
    assert_eq!(stream_info(&bytes).total, 16);
    let frames = inspect(&bytes);
    assert_eq!(frames.len(), 1);
    assert_eq!((frames[0].number, frames[0].block_size), (last, 16));
}

#[test]
fn levels_have_the_documented_block_sizes() {
    for level in 0..=8 {
        let expected = if level < 3 { 1152 } else { 4096 };
        assert_eq!(
            FlacOptions::new(16).with_level(level).block_size(),
            expected
        );
    }
    assert_eq!(FlacOptions::new(16).with_block_size(100).block_size(), 100);
}
