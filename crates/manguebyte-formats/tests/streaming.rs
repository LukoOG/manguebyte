//! Reading and writing block by block.
//!
//! The streaming reader must deliver exactly what the whole-file readers
//! deliver, whatever the block sizes and wherever it is told to seek. The
//! whole-file readers are in turn checked against files written by FFmpeg
//! (`vectors.rs`), and the files crafted here are also checked against
//! values worked out in this file, sample by sample.

mod common;

use std::fs::File;
use std::io::{BufWriter, Cursor, Seek, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{
    BitWriter, Counting, FlacSpec, FlacStream, FullDisk, Rng, SparseFile, crc8, crc16,
    flac_frame_offsets, flac_head, flac_sample, flac_scale, ramp, repeat_flac, slice,
    with_seek_table, write_flac,
};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::aiff::{self, AiffError, AiffFormat};
use manguebyte_formats::flac::{self, FlacError};
use manguebyte_formats::wav::{self, WavError, WavFormat};
use manguebyte_formats::{AudioReader, AudioWriter, FileFormat, FormatError, decode};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// A value no sample of a test file has, to see what a read left alone.
const UNTOUCHED: f32 = 1234.5;

/// The largest block any test asks for.
const MAX_BLOCK: usize = 60_000;

/// Two buffers are the same when every sample has the same bits: stricter
/// than `==`, which takes `-0.0` for `0.0` and no NaN for itself.
#[track_caller]
fn assert_same(what: &str, actual: &AudioBuffer<f32>, expected: &AudioBuffer<f32>) {
    assert_eq!(actual.num_channels(), expected.num_channels(), "{what}");
    assert_eq!(actual.num_frames(), expected.num_frames(), "{what}");
    for (channel, (a, b)) in actual
        .iter_channels()
        .zip(expected.iter_channels())
        .enumerate()
    {
        for (frame, (x, y)) in a.iter().zip(b).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "{what}: channel {channel}, frame {frame}: {x} is not {y}"
            );
        }
    }
}

/// A WAV file with a chunk to step over, a format chunk of either kind, and
/// `data` as its samples.
fn wav_file(tag: u16, channels: u16, bits: u16, extensible: bool, data: &[u8]) -> Vec<u8> {
    let block_align = channels * (bits / 8);
    let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
    // An odd-sized chunk, which is followed by a padding byte.
    bytes.extend_from_slice(b"LIST\x03\0\0\0abc\0");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&(if extensible { 40u32 } else { 16 }).to_le_bytes());
    bytes.extend_from_slice(&(if extensible { 0xFFFE } else { tag }).to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&(44_100 * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        // Size of the extension, valid bits, channel mask, and the format
        // as the first two bytes of a GUID.
        bytes.extend_from_slice(&22u16.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 == 1 {
        bytes.push(0);
    }
    let riff_size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
    bytes
}

/// What one sample of a WAV file is worth, worked out apart from the crate.
fn wav_value(tag: u16, sample: &[u8]) -> f32 {
    match (tag, sample) {
        (1, &[a]) => (f32::from(a) - 128.0) / 128.0,
        (1, &[a, b]) => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
        (1, &[a, b, c]) => {
            let sign = if c & 0x80 != 0 { 0xFF } else { 0x00 };
            i32::from_le_bytes([a, b, c, sign]) as f32 / 8_388_608.0
        }
        (1, &[a, b, c, d]) => {
            (f64::from(i32::from_le_bytes([a, b, c, d])) / 2_147_483_648.0) as f32
        }
        (3, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
        (3, &[a, b, c, d, e, f, g, h]) => f64::from_le_bytes([a, b, c, d, e, f, g, h]) as f32,
        _ => panic!("no such WAV sample"),
    }
}

/// An AIFF or AIFF-C file with a chunk to step over and `data` as its
/// samples.
fn aiff_file(compression: Option<&[u8; 4]>, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
    let width = match compression {
        Some(b"fl32") => 4,
        Some(b"fl64") => 8,
        _ => usize::from(bits).div_ceil(8),
    };
    let frames = (data.len() / (width * usize::from(channels))) as u32;
    let mut bytes = b"FORM\0\0\0\0".to_vec();
    bytes.extend_from_slice(if compression.is_some() {
        b"AIFC"
    } else {
        b"AIFF"
    });
    // An odd-sized chunk, which is followed by a padding byte.
    bytes.extend_from_slice(b"NAME\0\0\0\x05takes\0");
    bytes.extend_from_slice(b"COMM");
    bytes.extend_from_slice(&(if compression.is_some() { 24u32 } else { 18 }).to_be_bytes());
    bytes.extend_from_slice(&channels.to_be_bytes());
    bytes.extend_from_slice(&frames.to_be_bytes());
    bytes.extend_from_slice(&bits.to_be_bytes());
    // 48000 as an 80-bit float: 0xBB80 times two to the power 0.
    bytes.extend_from_slice(&[0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]);
    if let Some(compression) = compression {
        bytes.extend_from_slice(compression);
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes.extend_from_slice(b"SSND");
    bytes.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(data);
    if data.len() % 2 == 1 {
        bytes.push(0);
    }
    let form_size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&form_size.to_be_bytes());
    bytes
}

/// What one sample of an AIFF file is worth, worked out apart from the
/// crate: an integer is the top of its bytes, most significant first unless
/// the file is `sowt`.
fn aiff_value(compression: Option<&[u8; 4]>, sample: &[u8]) -> f32 {
    let mut big_endian = sample.to_vec();
    if compression == Some(b"sowt") {
        big_endian.reverse();
    }
    match compression {
        Some(b"fl32") => f32::from_be_bytes(big_endian.try_into().unwrap()),
        Some(b"fl64") => f64::from_be_bytes(big_endian.try_into().unwrap()) as f32,
        _ => {
            let mut value = i64::from(big_endian[0] as i8);
            for &byte in &big_endian[1..] {
                value = (value << 8) | i64::from(byte);
            }
            (value as f64 / (1u64 << (8 * big_endian.len() - 1)) as f64) as f32
        }
    }
}

/// Bytes with no pattern, the same on every run.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len).map(|_| rng.next() as u8).collect()
}

/// Sample bytes for a float format: ordinary values, and the odd ones a
/// file may hold.
fn float_bytes(frames: usize, wide: bool, little_endian: bool) -> Vec<u8> {
    let mut rng = Rng::new(77);
    let mut bytes = Vec::new();
    for index in 0..frames {
        let value = match index % 9 {
            0 => 0.0,
            1 => -0.0,
            2 => f64::INFINITY,
            3 => f64::NAN,
            4 => 1.0e-40,
            5 => -3.5,
            _ => (rng.below(2_000_001) as f64 - 1_000_000.0) / 1_000_000.0,
        };
        let mut sample = if wide {
            value.to_be_bytes().to_vec()
        } else {
            (value as f32).to_be_bytes().to_vec()
        };
        if little_endian {
            sample.reverse();
        }
        bytes.extend_from_slice(&sample);
    }
    bytes
}

/// A file to read, and what its audio is if that was worked out here.
struct Fixture {
    name: String,
    bytes: Vec<u8>,
    expected: Option<AudioBuffer<f32>>,
}

impl Fixture {
    fn new(name: &str, bytes: Vec<u8>) -> Self {
        Self {
            name: name.to_string(),
            bytes,
            expected: None,
        }
    }
}

/// The audio of `data`, each sample of `width` bytes valued by `value`.
fn expected_from(
    data: &[u8],
    channels: usize,
    width: usize,
    value: impl Fn(&[u8]) -> f32,
) -> AudioBuffer<f32> {
    let frames = data.len() / (channels * width);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (index, sample) in data.chunks_exact(width).take(frames * channels).enumerate() {
        buffer.channel_mut(index % channels)[index / channels] = value(sample);
    }
    buffer
}

/// The FLAC streams made here, by name.
fn made_flac() -> Vec<(String, FlacSpec, u64)> {
    let mut streams = Vec::new();
    let mut add = |name: &str, spec: FlacSpec, total: u64| {
        streams.push((name.to_string(), spec, total));
    };

    add("mono 16, plain", FlacSpec::new(1, 16, 256), 40_000);

    let mut spec = FlacSpec::new(1, 16, 256);
    spec.seek_every = Some(8);
    add("mono 16, seek table", spec, 40_000);

    let mut spec = FlacSpec::new(2, 24, 1_024);
    spec.block_sizes = vec![1_024, 17, 300, 4_096, 1];
    spec.declare_total = false;
    spec.declare_frame_size = false;
    add(
        "stereo 24, blocks of varying size, nothing declared",
        spec,
        30_011,
    );

    let mut spec = FlacSpec::new(2, 20, 1_024);
    spec.block_sizes = vec![2_000, 16, 513];
    spec.seek_every = Some(5);
    add(
        "stereo 20, blocks of varying size, seek table",
        spec,
        25_000,
    );

    let mut spec = FlacSpec::new(8, 8, 192);
    spec.seek_every = Some(1);
    add(
        "eight channels of 8 bits, a seek point per frame",
        spec,
        5_000,
    );

    add("mono 32, one short frame", FlacSpec::new(1, 32, 4_096), 100);

    let mut spec = FlacSpec::new(1, 16, 512);
    spec.first_number = 1_000;
    spec.seek_every = Some(4);
    spec.declare_total = false;
    add("the tail of a longer stream", spec, 20_000);

    let mut spec = FlacSpec::new(2, 12, 4_608);
    spec.declare_total = false;
    add("stereo 12, length not declared", spec, 50_000);

    add("no frames at all", FlacSpec::new(2, 16, 4_096), 0);
    streams
}

fn made_flac_fixture(name: &str, spec: &FlacSpec, total: u64) -> Fixture {
    let bits = spec.bits;
    let stream = write_flac(spec, total, |channel, index| {
        flac_sample(bits, channel, index)
    });
    let mut expected = AudioBuffer::new(spec.channels, total as usize);
    for (channel, samples) in expected.iter_channels_mut().enumerate() {
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = flac_scale(flac_sample(bits, channel, index as u64), bits);
        }
    }
    Fixture {
        name: format!("made FLAC: {name}"),
        bytes: stream.bytes,
        expected: Some(expected),
    }
}

/// Every file the streaming reader is checked with.
fn fixtures() -> Vec<Fixture> {
    let mut fixtures = vec![
        Fixture::new("stereo16.wav", data!("stereo16.wav").to_vec()),
        Fixture::new("stereo16-rf64.wav", data!("stereo16-rf64.wav").to_vec()),
        Fixture::new("mono24.wav", data!("mono24.wav").to_vec()),
        Fixture::new("stereo16.aiff", data!("stereo16.aiff").to_vec()),
        Fixture::new("stereo16-sowt.aifc", data!("stereo16-sowt.aifc").to_vec()),
        Fixture::new("mono24.aiff", data!("mono24.aiff").to_vec()),
        Fixture::new(
            "stereo16-level0.flac",
            data!("stereo16-level0.flac").to_vec(),
        ),
        Fixture::new(
            "stereo16-level5.flac",
            data!("stereo16-level5.flac").to_vec(),
        ),
        Fixture::new(
            "stereo16-level12.flac",
            data!("stereo16-level12.flac").to_vec(),
        ),
        Fixture::new(
            "stereo16-blocks128.flac",
            data!("stereo16-blocks128.flac").to_vec(),
        ),
        Fixture::new("mono24.flac", data!("mono24.flac").to_vec()),
    ];

    // WAV in every depth it is read in, with and without the extensible
    // header. 1001 frames, so that the data of the narrow ones is odd.
    for (tag, bits, channels) in [
        (1u16, 8u16, 1u16),
        (1, 8, 3),
        (1, 16, 2),
        (1, 24, 1),
        (1, 24, 5),
        (1, 32, 2),
        (3, 32, 2),
        (3, 64, 3),
    ] {
        let width = usize::from(bits / 8);
        let len = 1_001 * usize::from(channels) * width;
        let data = if tag == 3 {
            float_bytes(1_001 * usize::from(channels), bits == 64, true)
        } else {
            noise(len, u64::from(bits) + u64::from(channels))
        };
        for extensible in [false, true] {
            fixtures.push(Fixture {
                name: format!("made WAV: tag {tag}, {bits} bits, {channels} ch, {extensible}"),
                bytes: wav_file(tag, channels, bits, extensible, &data),
                expected: Some(expected_from(
                    &data,
                    usize::from(channels),
                    width,
                    |sample| wav_value(tag, sample),
                )),
            });
        }
    }

    // AIFF in every depth from 1 to 32 bits worth telling apart, both byte
    // orders, and both floats.
    for (compression, bits, channels) in [
        (None, 1u16, 1u16),
        (None, 8, 2),
        (None, 12, 1),
        (None, 16, 3),
        (None, 20, 2),
        (None, 24, 1),
        (None, 32, 2),
        (Some(b"NONE"), 16, 2),
        (Some(b"twos"), 24, 2),
        (Some(b"sowt"), 16, 1),
        (Some(b"sowt"), 24, 2),
        (Some(b"sowt"), 32, 1),
        (Some(b"fl32"), 32, 2),
        (Some(b"FL32"), 32, 1),
        (Some(b"fl64"), 64, 2),
    ] {
        let float = matches!(compression, Some(b"fl32" | b"FL32" | b"fl64"));
        let width = if float {
            usize::from(bits / 8)
        } else {
            usize::from(bits).div_ceil(8)
        };
        let data = if float {
            float_bytes(999 * usize::from(channels), bits == 64, false)
        } else {
            noise(999 * usize::from(channels) * width, u64::from(bits) + 100)
        };
        // The value of a sample does not depend on the case of the type.
        let kind: Option<&[u8; 4]> = match compression {
            Some(b"FL32") => Some(b"fl32"),
            other => other,
        };
        fixtures.push(Fixture {
            name: format!(
                "made AIFF: {:?}, {bits} bits, {channels} ch",
                compression.map(|name| String::from_utf8_lossy(name).into_owned())
            ),
            bytes: aiff_file(compression, channels, bits, &data),
            expected: Some(expected_from(
                &data,
                usize::from(channels),
                width,
                |sample| aiff_value(kind, sample),
            )),
        });
    }

    for (name, spec, total) in made_flac() {
        fixtures.push(made_flac_fixture(&name, &spec, total));
    }
    fixtures
}

/// Reads all of `bytes` in blocks whose sizes `next_block` gives.
fn read_in_blocks(
    name: &str,
    bytes: &[u8],
    mut next_block: impl FnMut() -> usize,
) -> AudioBuffer<f32> {
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap_or_else(|error| {
        panic!("{name}: {error}");
    });
    let channels = reader.num_channels();
    let mut collected = vec![Vec::new(); channels];
    let mut block = AudioBuffer::<f32>::new(channels, MAX_BLOCK);
    let mut total = 0u64;
    loop {
        let size = next_block();
        block.set_num_frames(size);
        for channel in block.iter_channels_mut() {
            channel.fill(UNTOUCHED);
        }
        let got = reader
            .read(&mut block)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(got <= size, "{name}: read more than was asked for");
        for (channel, samples) in collected.iter_mut().enumerate() {
            samples.extend_from_slice(&block.channel(channel)[..got]);
            assert!(
                block.channel(channel)[got..]
                    .iter()
                    .all(|&sample| sample == UNTOUCHED),
                "{name}: a read touched frames past its count"
            );
        }
        total += got as u64;
        assert_eq!(reader.position(), total, "{name}");
        if got < size {
            // The end: nothing more comes, however often it is asked for.
            assert_eq!(reader.read(&mut block).unwrap(), 0, "{name}");
            break;
        }
    }

    let mut buffer = AudioBuffer::new(channels, total as usize);
    for (channel, samples) in collected.iter().enumerate() {
        buffer.channel_mut(channel).copy_from_slice(samples);
    }
    buffer
}

#[test]
fn blocks_of_any_size_give_what_the_whole_file_readers_give() {
    for fixture in fixtures() {
        let name = &fixture.name;
        let whole = decode(&fixture.bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        if let Some(expected) = &fixture.expected {
            // The whole-file reader agrees with the values worked out here.
            assert_same(name, &whole.buffer, expected);
        }

        let reader = AudioReader::new(Cursor::new(&fixture.bytes)).unwrap();
        assert_eq!(reader.format(), whole.format, "{name}");
        assert_eq!(reader.sample_rate(), whole.sample_rate, "{name}");
        assert_eq!(reader.bits_per_sample(), whole.bits_per_sample, "{name}");
        assert_eq!(reader.num_channels(), whole.buffer.num_channels(), "{name}");
        if let Some(len) = reader.len_frames() {
            assert_eq!(len, whole.buffer.num_frames() as u64, "{name}");
        } else {
            assert_eq!(reader.format(), FileFormat::Flac, "{name}");
        }
        assert_eq!(reader.position(), 0, "{name}");

        // Fixed sizes, from one frame to more than the whole file.
        for size in [1usize, 2, 7, 64, 1_000, 4_096, MAX_BLOCK] {
            // One frame at a time is slow; the short files are enough for it.
            if size < 7 && whole.buffer.num_frames() > 10_000 {
                continue;
            }
            let streamed = read_in_blocks(name, &fixture.bytes, || size);
            assert_same(
                &format!("{name}, blocks of {size}"),
                &streamed,
                &whole.buffer,
            );
        }

        // The irregular pattern the rest of the framework is tested with.
        let pattern = [1usize, 511, 7, 300, 64, 2, 129];
        let mut turn = 0;
        let streamed = read_in_blocks(name, &fixture.bytes, || {
            turn += 1;
            pattern[turn % pattern.len()]
        });
        assert_same(
            &format!("{name}, irregular blocks"),
            &streamed,
            &whole.buffer,
        );

        // Random sizes, empty blocks among them.
        for seed in 1..=4 {
            let mut rng = Rng::new(seed);
            let streamed = read_in_blocks(name, &fixture.bytes, || {
                let size = rng.between(0, 3_000) as usize;
                // A block of no frames reads nothing and is not the end.
                if rng.below(8) == 0 { 0 } else { size.max(1) }
            });
            assert_same(
                &format!("{name}, random blocks {seed}"),
                &streamed,
                &whole.buffer,
            );
        }
    }
}

/// Seeks all over a file and checks what is read after each one.
fn check_seeks(name: &str, bytes: &[u8], reference: &AudioBuffer<f32>, rounds: usize, seed: u64) {
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    let len = reference.num_frames() as u64;
    let mut rng = Rng::new(seed);
    let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 700);
    for round in 0..rounds {
        let here = reader.position();
        let target = match round % 10 {
            0 => 0,
            1 => len,
            2 => len.saturating_sub(1),
            3 => len + 1 + rng.below(1_000),
            4 => u64::MAX,
            // A short hop either way, inside what was just decoded or
            // right next to it.
            5 => here + rng.below(64),
            6 => here.saturating_sub(rng.below(64)),
            // Some way ahead, where decoding onwards beats searching.
            7 => here + rng.below(10_000),
            _ => rng.below(len + 1),
        };
        let landed = reader
            .seek(target)
            .unwrap_or_else(|error| panic!("{name}: seek to {target}: {error}"));
        assert_eq!(landed, target.min(len), "{name}: seek to {target}");
        assert_eq!(reader.position(), landed, "{name}: seek to {target}");

        let size = rng.between(0, 700) as usize;
        block.set_num_frames(size);
        let got = reader
            .read(&mut block)
            .unwrap_or_else(|error| panic!("{name}: read at {landed}: {error}"));
        let from = landed as usize;
        assert_eq!(
            got,
            size.min(reference.num_frames() - from),
            "{name}: at {landed}"
        );
        block.set_num_frames(got);
        assert_same(
            &format!("{name}: {got} frames at {landed}"),
            &block,
            &slice(reference, from..from + got),
        );
        assert_eq!(reader.position(), landed + got as u64, "{name}");
    }
}

#[test]
fn seeking_lands_on_the_exact_frame() {
    for fixture in fixtures() {
        let whole = decode(&fixture.bytes).unwrap();
        for seed in [11, 12] {
            check_seeks(&fixture.name, &fixture.bytes, &whole.buffer, 70, seed);
        }
    }
}

/// A seek table for a FLAC file written by FFmpeg, which writes none: a
/// point at every `every` frames, each changed by `bend` before it is
/// written.
fn table_for(
    bytes: &[u8],
    every: usize,
    bend: impl Fn((u64, u64, u16)) -> (u64, u64, u16),
) -> Vec<u8> {
    let (_, block_size, offsets) = flac_frame_offsets(bytes);
    assert!(!offsets.is_empty(), "no frame was found");
    let points: Vec<_> = offsets
        .iter()
        .enumerate()
        .step_by(every)
        .map(|(index, &offset)| {
            bend((
                (index * block_size) as u64,
                offset as u64,
                block_size as u16,
            ))
        })
        .collect();
    with_seek_table(bytes, &points)
}

#[test]
fn a_seek_table_is_used_when_right_and_harmless_when_wrong() {
    for (name, bytes, every) in [
        ("level 0", data!("stereo16-level0.flac"), 1),
        ("level 5", data!("stereo16-level5.flac"), 1),
        ("level 12", data!("stereo16-level12.flac"), 2),
        ("blocks of 128", data!("stereo16-blocks128.flac"), 7),
        ("24 bits", data!("mono24.flac"), 1),
    ] {
        let reference = flac::read(bytes).unwrap().buffer;

        // A table that tells the truth.
        let honest = table_for(bytes, every, |point| point);
        assert_eq!(flac::read(honest.as_slice()).unwrap().buffer, reference);
        check_seeks(
            &format!("{name}, honest table"),
            &honest,
            &reference,
            100,
            21,
        );

        // Tables that lie, each in its own way. The audio found must still
        // be the right audio.
        let liars: [(&str, Vec<u8>); 5] = [
            (
                "offsets that lead into the middle of frames",
                table_for(bytes, 1, |(sample, offset, len)| (sample, offset + 3, len)),
            ),
            (
                "sample numbers half of what they should be",
                table_for(bytes, 1, |(sample, offset, len)| (sample / 2, offset, len)),
            ),
            (
                "sample numbers twice what they should be",
                table_for(bytes, 1, |(sample, offset, len)| {
                    (sample * 2 + 1, offset, len)
                }),
            ),
            (
                "offsets past the end of the file",
                table_for(bytes, 1, |(sample, offset, len)| {
                    (sample, offset + (1 << 40), len)
                }),
            ),
            (
                "every point at the first frame",
                table_for(bytes, 1, |(sample, _, len)| (sample, 0, len)),
            ),
        ];
        for (lie, bytes) in &liars {
            check_seeks(&format!("{name}, {lie}"), bytes, &reference, 50, 22);
        }
    }
}

/// A FLAC stream in whose audio every frame holds the bytes of a frame
/// header, checksum and all, carrying a number chosen to mislead.
fn stream_with_decoys() -> (FlacStream, AudioBuffer<f32>) {
    let block = 512u64;
    let total = 60_000u64;
    let sample = |_channel: usize, index: u64| {
        let frame = index / block;
        let within = index % block;
        if (100..104).contains(&within) {
            // Sync, a 16-bit block size, one channel, a frame number, the
            // block size and the header's checksum: eight bytes, which sit
            // on byte boundaries as four 16-bit samples.
            let mut header = vec![0xFF, 0xF8, 0x70, 0x00];
            header.push(((frame * 37 + 5) % 120) as u8);
            header.extend_from_slice(&511u16.to_be_bytes());
            header.push(crc8(&header));
            let at = (within - 100) as usize * 2;
            i32::from(i16::from_be_bytes([header[at], header[at + 1]]))
        } else {
            flac_sample(16, 0, index) | 1
        }
    };
    let stream = write_flac(&FlacSpec::new(1, 16, block as usize), total, sample);
    let mut expected = AudioBuffer::new(1, total as usize);
    for (index, value) in expected.channel_mut(0).iter_mut().enumerate() {
        *value = flac_scale(sample(0, index as u64), 16);
    }
    (stream, expected)
}

#[test]
fn a_frame_header_inside_the_audio_does_not_mislead_a_seek() {
    let (stream, expected) = stream_with_decoys();
    // The decoys are there: far more sync codes than frames.
    let syncs = stream
        .bytes
        .windows(2)
        .filter(|pair| pair == &[0xFF, 0xF8])
        .count();
    assert!(syncs >= 2 * stream.frames.len(), "{syncs} sync codes");

    assert_same(
        "decoys, whole",
        &flac::read(stream.bytes.as_slice()).unwrap().buffer,
        &expected,
    );
    for seed in [31, 32] {
        check_seeks("decoys", &stream.bytes, &expected, 200, seed);
    }
}

/// A mono 16-bit stream of 4096-sample frames of noise in which every frame
/// holds, as six of its samples and half of a seventh, the bytes of a whole
/// frame: sync code, a header carrying the number of the frame it sits in,
/// one constant subframe and both checksums.
fn stream_with_whole_frames_inside() -> (Vec<u8>, AudioBuffer<f32>) {
    let block = 4_096u64;
    let frames = 120u64;
    let total = block * frames;
    let sample = |_channel: usize, index: u64| {
        let frame = index / block;
        let within = index % block;
        if (3_000..3_007).contains(&within) {
            let mut inner = vec![0xFF, 0xF8, 0x70, 0x00, frame as u8];
            inner.extend_from_slice(&(block as u16 - 1).to_be_bytes());
            inner.push(crc8(&inner));
            // A subframe that holds the one value 0x1234.
            inner.extend_from_slice(&[0x00, 0x12, 0x34]);
            let frame_crc = crc16(&inner);
            inner.extend_from_slice(&frame_crc.to_be_bytes());
            inner.push(0x55);
            let at = (within - 3_000) as usize * 2;
            i32::from(i16::from_be_bytes([inner[at], inner[at + 1]]))
        } else {
            flac_sample(16, 0, index) | 1
        }
    };
    let stream = write_flac(&FlacSpec::new(1, 16, block as usize), total, sample);
    let mut expected = AudioBuffer::new(1, total as usize);
    for (index, value) in expected.channel_mut(0).iter_mut().enumerate() {
        *value = flac_scale(sample(0, index as u64), 16);
    }
    (stream.bytes, expected)
}

/// A header alone is turned down by the checksum of the frame it claims to
/// start. A whole frame is not: sync code, both checksums and a number that
/// fits the stream are all there. Audio stored sample by sample can hold
/// exactly that, in a file that is valid FLAC and reads correctly from its
/// start. What gives it away is what follows it, which is the rest of the
/// audio and not the next frame.
#[test]
fn a_whole_frame_inside_the_audio_does_not_mislead_a_seek() {
    let (bytes, expected) = stream_with_whole_frames_inside();
    // The planted frames are there, one in every frame of the stream.
    let planted = bytes
        .windows(4)
        .filter(|start| start == &[0xFF, 0xF8, 0x70, 0x00])
        .count();
    assert!(planted >= 240, "{planted} frame starts");
    assert_same(
        "whole frames inside, whole",
        &flac::read(bytes.as_slice()).unwrap().buffer,
        &expected,
    );

    let total = expected.num_frames() as u64;
    let mut reader = AudioReader::new(Cursor::new(bytes.as_slice())).unwrap();
    let mut block = AudioBuffer::<f32>::new(1, 64);
    let mut rng = Rng::new(71);
    // Seeks after which the audio read was not the audio of the place asked
    // for, and seeks after which reading failed although nothing in the
    // file is damaged.
    let mut wrong = Vec::new();
    let mut failed = Vec::new();
    for _ in 0..300 {
        let target = rng.below(total - 64);
        if reader.seek(target).ok() != Some(target) {
            failed.push(target);
            continue;
        }
        block.set_num_frames(64);
        match reader.read(&mut block) {
            Ok(64) => {
                let from = target as usize;
                if block.channel(0) != &expected.channel(0)[from..from + 64] {
                    wrong.push(target);
                }
            }
            _ => failed.push(target),
        }
    }
    assert!(
        wrong.is_empty() && failed.is_empty(),
        "of 300 seeks in a valid file, {} played audio from somewhere else (first at {:?}) \
         and {} ended in an error (first at {:?})",
        wrong.len(),
        wrong.first(),
        failed.len(),
        failed.first()
    );

    // And the planted frames themselves can be seeked into and read across.
    for frame in [0u64, 1, 57, 119] {
        let target = frame * 4_096 + 2_990;
        assert_eq!(reader.seek(target).unwrap(), target);
        block.set_num_frames(64);
        assert_eq!(reader.read(&mut block).unwrap(), 64);
        let from = target as usize;
        assert!(block.channel(0) == &expected.channel(0)[from..from + 64]);
    }
    for seed in [72, 73] {
        check_seeks("whole frames inside", &bytes, &expected, 150, seed);
    }
}

/// A seek accepts a frame for what is found after it: the next frame, or
/// the end of the file. The last frame of a file that has something else
/// appended to it has neither, and must still be reached.
#[test]
fn what_is_appended_to_a_file_does_not_keep_a_seek_from_its_last_frames() {
    let spec = FlacSpec::new(2, 16, 512);
    let total = 30_000u64;
    let stream = write_flac(&spec, total, |channel, index| {
        flac_sample(16, channel, index)
    });
    let reference = flac::read(stream.bytes.as_slice()).unwrap().buffer;

    // A tag of 128 bytes after the audio, as ID3v1 puts one there.
    let mut tagged = stream.bytes.clone();
    tagged.extend_from_slice(b"TAG");
    tagged.resize(stream.bytes.len() + 128, b' ');

    // The stream says how long it is, so reading stops before the tag.
    let streamed = read_in_blocks("tagged", &tagged, || 1_000);
    assert_same("tagged", &streamed, &reference);
    for seed in [51, 52] {
        check_seeks("tagged", &tagged, &reference, 150, seed);
    }
    let mut reader = AudioReader::new(Cursor::new(tagged.as_slice())).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 64);
    for target in [
        total - 1,
        total - 64,
        total - 300,
        total - 513,
        total - 2_000,
    ] {
        assert_eq!(reader.seek(0).unwrap(), 0);
        assert_eq!(reader.seek(target).unwrap(), target);
        let got = reader.read(&mut block).unwrap();
        assert_eq!(got as u64, 64.min(total - target));
        block.set_num_frames(got);
        let from = target as usize;
        assert_same("tagged", &block, &slice(&reference, from..from + got));
        block.set_num_frames(64);
    }
}

#[test]
fn a_seek_reads_a_small_part_of_the_file() {
    // 256 frames of 4096 samples of noise: two megabytes.
    let noise = |_: usize, index: u64| flac_sample(16, 0, index * 3 + 1) | 1;
    let total = 256 * 4_096;
    let mut spec = FlacSpec::new(1, 16, 4_096);
    let plain = write_flac(&spec, total, noise);
    spec.seek_every = Some(16);
    let tabled = write_flac(&spec, total, noise);
    let file_len = plain.bytes.len() as u64;
    assert!(file_len > 2_000_000);

    let mut worst = [0u64; 2];
    for (which, stream) in [&plain, &tabled].into_iter().enumerate() {
        let counting = Counting::new(Cursor::new(stream.bytes.as_slice()));
        let bytes_read = Arc::clone(&counting.bytes_read);
        let mut reader = AudioReader::new(counting).unwrap();
        let mut rng = Rng::new(5);
        let mut block = AudioBuffer::<f32>::new(1, 64);
        for _ in 0..60 {
            let target = rng.below(total);
            let before = bytes_read.load(Ordering::Relaxed);
            reader.seek(target).unwrap();
            let got = reader.read(&mut block).unwrap();
            assert_eq!(got, 64.min((total - target) as usize));
            for (offset, &value) in block.channel(0)[..got].iter().enumerate() {
                assert_eq!(value, flac_scale(noise(0, target + offset as u64), 16));
            }
            let spent = bytes_read.load(Ordering::Relaxed) - before;
            worst[which] = worst[which].max(spent);
        }
    }
    // Bisection reads a few windows of 32 KiB, not the two megabytes; a
    // seek table spares most of those.
    println!(
        "file of {file_len} bytes: a seek read at most {} bytes without a seek table, {} with",
        worst[0], worst[1]
    );
    assert!(worst[0] < file_len / 5, "{} of {file_len}", worst[0]);
    assert!(worst[1] < file_len / 8, "{} of {file_len}", worst[1]);
    assert!(worst[1] < worst[0]);
}

#[test]
fn bisection_finds_frames_compressed_by_another_encoder() {
    // FFmpeg's frames, sixteen times over: 1088 frames in a third of a
    // megabyte, enough for the search to halve its way down several times.
    let source = data!("stereo16-blocks128.flac");
    let reference = wav::read(data!("stereo16.wav")).unwrap().buffer;
    assert_eq!(flac::read(source).unwrap().buffer, reference);

    for declare_total in [true, false] {
        let (bytes, round) = repeat_flac(source, 16, declare_total);
        assert_eq!(round, 68 * 128);
        assert!(bytes.len() > 300_000, "{} bytes", bytes.len());
        let mut expected = AudioBuffer::new(2, round * 16);
        for (channel, samples) in expected.iter_channels_mut().enumerate() {
            for (frame, sample) in samples.iter_mut().enumerate() {
                *sample = reference.channel(channel)[frame % round];
            }
        }

        let name = format!("FFmpeg's frames repeated, length declared: {declare_total}");
        let whole = flac::read(bytes.as_slice()).unwrap();
        assert_same(&name, &whole.buffer, &expected);
        let streamed = read_in_blocks(&name, &bytes, || 3_333);
        assert_same(&name, &streamed, &expected);
        for seed in [41, 42] {
            check_seeks(&name, &bytes, &expected, 150, seed);
        }

        // And with a seek table on top.
        let tabled = table_for(&bytes, 50, |point| point);
        check_seeks(&format!("{name}, seek table"), &tabled, &expected, 150, 44);
    }
}

/// A block to read data of unknown worth into.
fn block_for<R: std::io::Read + std::io::Seek>(reader: &AudioReader<R>) -> AudioBuffer<f32> {
    let channels = reader.num_channels();
    assert!(channels > 0);
    // A damaged header may claim tens of thousands of channels.
    AudioBuffer::new(channels, if channels > 16 { 8 } else { 512 })
}

/// Opens, seeks and reads data that may be anything. Nothing here may
/// panic, and a seek may not land past its target.
fn poke(bytes: &[u8]) {
    let Ok(mut reader) = AudioReader::new(Cursor::new(bytes)) else {
        return;
    };
    let mut block = block_for(&reader);
    for target in [3, 5_000, u64::MAX, 1] {
        if let Ok(landed) = reader.seek(target) {
            assert!(landed <= target);
            assert_eq!(reader.position(), landed);
        }
        let _ = reader.read(&mut block);
    }
}

/// Reads data that may be anything from its start to its end or to the
/// first error, and says which it was. `None` if it cannot be opened.
fn read_what_can_be_read(bytes: &[u8]) -> Option<(AudioBuffer<f32>, bool)> {
    let mut reader = AudioReader::new(Cursor::new(bytes)).ok()?;
    let mut block = block_for(&reader);
    let channels = reader.num_channels();
    let mut collected = vec![Vec::new(); channels];
    let mut failed = false;
    // No file tested here is longer than this many blocks.
    for _ in 0..100_000 {
        match reader.read(&mut block) {
            Ok(0) => break,
            Ok(got) => {
                for (channel, samples) in collected.iter_mut().enumerate() {
                    samples.extend_from_slice(&block.channel(channel)[..got]);
                }
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let mut buffer = AudioBuffer::new(channels, collected[0].len());
    for (channel, samples) in collected.iter().enumerate() {
        buffer.channel_mut(channel).copy_from_slice(samples);
    }
    Some((buffer, failed))
}

#[test]
fn damaged_and_cut_files_are_errors_and_never_panics() {
    // Each with how thoroughly to go through it: decoding FFmpeg's FLAC
    // hundreds of times in a debug build is slow, and the FLAC made here
    // has the same structure at a fraction of the cost.
    let mut files = vec![
        ("wav", data!("stereo16.wav").to_vec(), 1),
        ("rf64", data!("stereo16-rf64.wav").to_vec(), 1),
        ("aiff", data!("mono24.aiff").to_vec(), 1),
        ("aifc", data!("stereo16-sowt.aifc").to_vec(), 1),
        ("flac", data!("stereo16-blocks128.flac").to_vec(), 6),
    ];
    let mut spec = FlacSpec::new(2, 16, 192);
    spec.seek_every = Some(3);
    spec.block_sizes = vec![192, 31, 700];
    let made = write_flac(&spec, 6_000, |channel, index| {
        flac_sample(16, channel, index)
    });
    files.push(("made flac", made.bytes, 2));

    for (name, valid, sparing) in &files {
        let (whole, failed) = read_what_can_be_read(valid).unwrap();
        assert!(!failed, "{name}");
        assert!(whole.num_frames() >= 6_000, "{name}");

        // Cut short anywhere. The headers are cut at every byte or so, the
        // audio at steps.
        let cuts = (0..valid.len().min(200))
            .step_by(*sparing)
            .chain((200..valid.len()).step_by(1_499 * sparing));
        for len in cuts {
            poke(&valid[..len]);
            if let Some((audio, _)) = read_what_can_be_read(&valid[..len]) {
                // What a cut file still gives is the start of the audio.
                assert!(
                    audio.num_frames() <= whole.num_frames(),
                    "{name} cut at {len}"
                );
                assert_same(
                    &format!("{name} cut at {len}"),
                    &audio,
                    &slice(&whole, 0..audio.num_frames()),
                );
            }
        }

        // Any byte of the headers set to all zeros, to all ones, or with
        // its top bit turned over.
        for index in (0..valid.len().min(128)).step_by(*sparing) {
            for value in [0x00, 0xFF, valid[index] ^ 0x80] {
                let mut damaged = valid.clone();
                damaged[index] = value;
                poke(&damaged);
                let _ = read_what_can_be_read(&damaged);
            }
        }

        // Bits flipped all over the file.
        let mut rng = Rng::new(9);
        for _ in 0..90 / sparing {
            let mut damaged = valid.clone();
            let index = rng.below(valid.len() as u64) as usize;
            damaged[index] ^= 1 << rng.below(8);
            poke(&damaged);
            let _ = read_what_can_be_read(&damaged);
        }
    }
}

#[test]
fn damage_to_flac_audio_is_reported_and_never_played() {
    for (name, valid) in [
        ("FFmpeg", data!("stereo16-blocks128.flac").to_vec()),
        (
            "FFmpeg, repeated",
            repeat_flac(data!("stereo16-blocks128.flac"), 4, true).0,
        ),
    ] {
        let reference = flac::read(valid.as_slice()).unwrap().buffer;
        let (audio_start, _, _) = flac_frame_offsets(&valid);
        let frames = reference.num_frames() as u64;

        let mut seeks = 0;
        let mut seeks_that_worked = 0;
        for index in (audio_start..valid.len()).step_by(valid.len() / 40) {
            let mut damaged = valid.clone();
            damaged[index] ^= 0x10;

            // Read from the start: every frame carries a checksum, so the
            // damage is met and reported, and all that came before it is
            // the true audio.
            let Some((audio, failed)) = read_what_can_be_read(&damaged) else {
                // The header of the first frame says how the stream's
                // frames are numbered. Without it the file is not opened.
                assert!(index < audio_start + 16, "{name}: damage at {index}");
                assert!(flac::read(damaged.as_slice()).is_err());
                continue;
            };
            assert!(failed, "{name}: damage at {index} went unnoticed");
            assert!(audio.num_frames() < reference.num_frames());
            assert_same(
                &format!("{name}, damage at {index}"),
                &audio,
                &slice(&reference, 0..audio.num_frames()),
            );

            // Seek anywhere: either an error, or the true audio of the
            // place asked for. Never the audio of somewhere else.
            let mut reader = AudioReader::new(Cursor::new(&damaged)).unwrap();
            let mut block = AudioBuffer::<f32>::new(2, 100);
            let mut rng = Rng::new(index as u64);
            for _ in 0..12 {
                let target = rng.below(frames);
                seeks += 1;
                let Ok(landed) = reader.seek(target) else {
                    continue;
                };
                assert_eq!(landed, target, "{name}, damage at {index}");
                block.set_num_frames(100);
                let Ok(got) = reader.read(&mut block) else {
                    continue;
                };
                block.set_num_frames(got);
                let from = landed as usize;
                assert_same(
                    &format!("{name}, damage at {index}, seek to {target}"),
                    &block,
                    &slice(&reference, from..from + got),
                );
                seeks_that_worked += 1;
            }
        }
        // One frame in 69 or more is damaged, and a seek fails only when it
        // is aimed at that frame or reads on into it: damage early in a
        // file does not put the rest of it out of reach.
        println!("{name}: {seeks_that_worked} of {seeks} seeks into damaged files worked");
        assert!(seeks_that_worked * 10 >= seeks * 9, "{name}");
    }
}

/// A FLAC stream of one stereo frame whose two checksums are right and whose
/// predictor is unstable.
///
/// Both channels are linear-prediction subframes of order 1 with the largest
/// coefficient 15 bits hold and no shift, so each sample is 16383 times the
/// one before it. No encoder writes this, but a file can hold it, and a
/// checksum that matches says nothing against it. `channel_code` is 1 for
/// two channels coded apart, or picks one of the three ways of coding them
/// together (8 left/side, 9 side/right, 10 mid/side), after which the
/// decoder goes on to add or subtract the two.
fn unstable_flac(channel_code: u64) -> Vec<u8> {
    let block = 32usize;
    let mut bytes = flac_head(2, 16, block, block as u64);

    let mut frame = BitWriter::default();
    frame.bits(0xFFF8, 16);
    // Block size written out in 16 bits; sample rate from the stream.
    frame.bits(7, 4);
    frame.bits(0, 4);
    frame.bits(channel_code, 4);
    frame.bits(0, 3);
    frame.bits(0, 1);
    frame.bits(0, 8);
    frame.bits(block as u64 - 1, 16);
    let header_crc = crc8(&frame.bytes);
    frame.bits(u64::from(header_crc), 8);

    for (index, warm_up) in [0x7FFFu64, 0x1_0000].into_iter().enumerate() {
        // The side channel carries one bit more.
        let is_side = match channel_code {
            8 | 10 => index == 1,
            9 => index == 0,
            _ => false,
        };
        let width = if is_side { 17 } else { 16 };
        // Padding bit, type 32 (prediction of order 1), no wasted bits.
        frame.bits(0, 1);
        frame.bits(32, 6);
        frame.bits(0, 1);
        frame.bits(warm_up & ((1 << width) - 1), width);
        // Precision of 15 bits, shift of 0, the coefficient 16383.
        frame.bits(14, 4);
        frame.bits(0, 5);
        frame.bits(0x3FFF, 15);
        // Rice coding with 4-bit parameters, one partition, parameter 0,
        // and a residual of zero for every sample after the first.
        frame.bits(0, 2);
        frame.bits(0, 4);
        frame.bits(0, 4);
        for _ in 1..block {
            frame.bits(1, 1);
        }
    }
    frame.align();
    let frame_crc = crc16(&frame.bytes);
    frame.bits(u64::from(frame_crc), 16);

    bytes.extend_from_slice(&frame.bytes);
    bytes
}

/// The damage tests above flip bits, which the checksums catch. This is
/// what a checksum cannot catch: a frame that was written this way. The
/// numbers it decodes to are far outside 16 bits. They must not overflow
/// anything on the way, which in a debug build is a panic on the thread
/// that reads the file, and they must not be cut down to 16 bits and
/// played, which is what an unchecked build would do.
#[test]
fn a_flac_frame_with_an_unstable_predictor_is_an_error_and_not_a_panic() {
    for (name, channel_code) in [
        ("two channels", 1u64),
        ("left/side", 8),
        ("side/right", 9),
        ("mid/side", 10),
    ] {
        let bytes = unstable_flac(channel_code);

        let mut reader = AudioReader::new(Cursor::new(bytes.as_slice())).unwrap();
        assert_eq!(reader.len_frames(), Some(32), "{name}");
        let mut block = AudioBuffer::<f32>::new(2, 32);
        // And the same again when asked again: the reader stays usable.
        for _ in 0..2 {
            assert!(
                matches!(
                    reader.read(&mut block),
                    Err(FormatError::Flac(FlacError::Malformed(_)))
                ),
                "{name}: the streaming reader"
            );
            assert_eq!(reader.position(), 0, "{name}");
        }
        assert!(
            matches!(flac::read(bytes.as_slice()), Err(FlacError::Malformed(_))),
            "{name}: the whole-file reader"
        );
        assert!(
            matches!(decode(&bytes), Err(FormatError::Flac(_))),
            "{name}"
        );
    }
}

/// Appends one subframe of `block` samples of `width` bits, of a kind and
/// with contents drawn from everything the format's syntax allows.
///
/// `tame` keeps the numbers small and the predictors stable, so that the
/// frame decodes to samples that fit; otherwise anything goes.
fn crafted_subframe(rng: &mut Rng, frame: &mut BitWriter, block: usize, width: u32, tame: bool) {
    // A number of `count` bits: anything, or one from -4 to 3.
    let value = |rng: &mut Rng, count: u32| -> u64 {
        let mask = (1u64 << count) - 1;
        if tame {
            (rng.below(8) as i64 - 4) as u64 & mask
        } else {
            rng.next() & mask
        }
    };

    let wasted = if rng.below(4) == 0 && width > 1 {
        rng.between(1, u64::from((width - 1).min(6))) as u32
    } else {
        0
    };
    let depth = width - wasted;
    let kind = rng.below(4);
    let order = match kind {
        0 | 1 => 0,
        2 => rng.below(5).min(block as u64),
        _ => rng.between(1, 32).min(block as u64),
    } as usize;

    // A zero bit, six bits of type, and whether bits are wasted: if so, how
    // many, as that many zeros less one and a one.
    frame.bits(0, 1);
    let type_code = match kind {
        0 => 0,
        1 => 1,
        2 => 8 + order,
        _ => 31 + order,
    };
    frame.bits(type_code as u64, 6);
    frame.bits(u64::from(wasted > 0), 1);
    if wasted > 0 {
        frame.bits(1, wasted);
    }

    match kind {
        // One value for the whole block.
        0 => frame.bits(value(rng, depth), depth),
        // Every sample as it is.
        1 => {
            for _ in 0..block {
                frame.bits(value(rng, depth), depth);
            }
        }
        // A predictor: its first samples, then for the linear kind its
        // precision, shift and coefficients, then the residuals.
        _ => {
            for _ in 0..order {
                frame.bits(value(rng, depth), depth);
            }
            if kind == 3 {
                // Tame: half the sample before, which dies away.
                let precision = if tame { 2 } else { rng.between(1, 15) as u32 };
                frame.bits(u64::from(precision) - 1, 4);
                frame.bits(if tame { 1 } else { rng.below(16) }, 5);
                for index in 0..order {
                    let coefficient = if tame {
                        u64::from(index == 0)
                    } else {
                        rng.next() & ((1 << precision) - 1)
                    };
                    frame.bits(coefficient, precision);
                }
            }

            let method = rng.below(2);
            frame.bits(method, 2);
            let parameter_bits = 4 + method as u32;
            let escape = (1u64 << parameter_bits) - 1;
            // The block must divide into the partitions, and the first one
            // must have room for the predictor's first samples.
            let mut partition_order = rng.below(4) as u32;
            while block % (1 << partition_order) != 0 || (block >> partition_order) < order {
                partition_order -= 1;
            }
            frame.bits(u64::from(partition_order), 4);
            for partition in 0..1usize << partition_order {
                let count = (block >> partition_order) - if partition == 0 { order } else { 0 };
                if !tame && rng.below(4) == 0 {
                    // Residuals written out, in any number of bits.
                    let raw_bits = rng.below(32) as u32;
                    frame.bits(escape, parameter_bits);
                    frame.bits(u64::from(raw_bits), 5);
                    for _ in 0..count {
                        frame.bits(value(rng, raw_bits), raw_bits);
                    }
                } else {
                    let parameter = if tame {
                        rng.below(2)
                    } else {
                        rng.below(escape)
                    } as u32;
                    frame.bits(u64::from(parameter), parameter_bits);
                    for _ in 0..count {
                        // A quotient as zeros and a one, then the low bits.
                        let quotient = rng.below(if tame { 2 } else { 6 }) as u32;
                        frame.bits(1, quotient + 1);
                        frame.bits(rng.next() & ((1 << parameter) - 1), parameter);
                    }
                }
            }
        }
    }
}

/// A FLAC stream of one frame that is well formed and has both checksums
/// right, and whose subframes hold whatever [`crafted_subframe`] drew.
fn crafted_flac(rng: &mut Rng, channels: usize, bits: u32, tame: bool) -> (Vec<u8>, usize) {
    let block = rng.between(1, 48) as usize;
    let channel_code = if channels == 2 {
        [1u64, 8, 9, 10][rng.below(4) as usize]
    } else {
        channels as u64 - 1
    };

    let mut frame = BitWriter::default();
    frame.bits(0xFFF8, 16);
    // Block size written out in 8 bits; sample rate and sample size from
    // the stream; frame number zero.
    frame.bits(6, 4);
    frame.bits(0, 4);
    frame.bits(channel_code, 4);
    frame.bits(0, 3);
    frame.bits(0, 1);
    frame.bits(0, 8);
    frame.bits(block as u64 - 1, 8);
    let header_crc = crc8(&frame.bytes);
    frame.bits(u64::from(header_crc), 8);
    for channel in 0..channels {
        let is_side = matches!((channel_code, channel), (8, 1) | (9, 0) | (10, 1));
        crafted_subframe(rng, &mut frame, block, bits + u32::from(is_side), tame);
    }
    frame.align();
    let frame_crc = crc16(&frame.bytes);
    frame.bits(u64::from(frame_crc), 16);

    let mut bytes = flac_head(channels, bits, block, 0);
    bytes.extend_from_slice(&frame.bytes);
    (bytes, block)
}

/// Frames that pass every checksum and hold anything at all: every kind of
/// subframe, wasted bits, both residual codings with escapes, predictors of
/// every order with any coefficients, all four channel assignments, 4 to 32
/// bits. Whatever they decode to, both readers must agree on it: either the
/// same samples, every one of them within full scale, or an error.
#[test]
fn frames_with_valid_checksums_and_any_contents_are_decoded_or_refused() {
    let mut rng = Rng::new(2024);
    let mut decoded = 0;
    let mut refused = 0;
    for round in 0..4_000 {
        let channels = rng.between(1, 8) as usize;
        let bits = rng.between(4, 32) as u32;
        let tame = round % 2 == 0;
        let (bytes, block_size) = crafted_flac(&mut rng, channels, bits, tame);
        let name = format!("round {round}: {channels} channels of {bits} bits");

        let whole = flac::read(bytes.as_slice());
        let mut reader = AudioReader::new(Cursor::new(bytes.as_slice()))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(reader.num_channels(), channels, "{name}");
        let mut block = AudioBuffer::<f32>::new(channels, 64);
        match (reader.read(&mut block), whole) {
            (Ok(got), Ok(whole)) => {
                assert_eq!(got, block_size, "{name}");
                block.set_num_frames(got);
                assert_same(&name, &block, &whole.buffer);
                assert!(block.peak() <= 1.0, "{name}: beyond full scale");
                decoded += 1;
            }
            (Err(FormatError::Flac(streamed)), Err(whole)) => {
                // The same refusal from both.
                assert_eq!(streamed.to_string(), whole.to_string(), "{name}");
                assert!(matches!(whole, FlacError::Malformed(_)), "{name}: {whole}");
                refused += 1;
            }
            (streamed, whole) => panic!(
                "{name}: the readers disagree: {:?} and {:?}",
                streamed,
                whole.map(|file| file.buffer.num_frames())
            ),
        }
        // Seeking in it does not panic either, wherever it is told to go.
        for target in [1, 31, u64::MAX, 0] {
            let _ = reader.seek(target);
            let _ = reader.read(&mut block);
        }
    }
    // Both outcomes are well represented, so both paths were walked.
    println!("{decoded} crafted frames were decoded, {refused} refused");
    assert!(decoded >= 400, "only {decoded} frames decoded");
    assert!(refused >= 400, "only {refused} frames refused");
}

#[test]
fn a_failed_seek_leaves_the_reader_where_it_was() {
    // Damage in the last third of the file, then a seek into it.
    let valid = repeat_flac(data!("stereo16-blocks128.flac"), 4, true).0;
    let reference = flac::read(valid.as_slice()).unwrap().buffer;
    let mut damaged = valid.clone();
    let hit = valid.len() * 5 / 6;
    for byte in &mut damaged[hit..hit + 2_000] {
        *byte = 0x55;
    }

    let mut reader = AudioReader::new(Cursor::new(&damaged)).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 300);
    reader.seek(1_000).unwrap();
    assert_eq!(reader.read(&mut block).unwrap(), 300);

    // Somewhere inside the damage there is a frame the reader cannot reach.
    let frames = reference.num_frames() as u64;
    let failed: Vec<u64> = (frames * 4 / 6..frames)
        .step_by(97)
        .filter(|&target| {
            reader.seek(1_300).unwrap();
            reader.seek(target).is_err()
        })
        .collect();
    assert!(!failed.is_empty(), "no seek met the damage");
    for target in failed {
        reader.seek(1_300).unwrap();
        match reader.seek(target) {
            Err(FormatError::Flac(error)) => assert!(!matches!(error, FlacError::Io(_))),
            other => panic!("seek to {target}: {other:?}"),
        }
        assert_eq!(reader.position(), 1_300);
        // And reading carries on from there as if nothing had happened.
        assert_eq!(reader.read(&mut block).unwrap(), 300);
        assert_same(
            "after a failed seek",
            &block,
            &slice(&reference, 1_300..1_600),
        );
    }
}

#[test]
fn a_disk_that_fails_now_and_then_never_puts_a_flac_reader_out_of_place() {
    // Frames of very different sizes, with and without a seek table. The
    // disk lets a number of bytes through and then fails, over and over,
    // in the middle of reads and of seeks. Whatever the reader says it has
    // read must be the audio of the place it says it is at, and a seek that
    // fails must leave it where it was.
    let mut spec = FlacSpec::new(2, 16, 1_024);
    spec.block_sizes = vec![1_024, 40, 4_096, 300];
    let total = 90_000u64;
    let sample = |channel: usize, index: u64| flac_sample(16, channel, index);
    let plain = write_flac(&spec, total, sample).bytes;
    spec.seek_every = Some(6);
    let tabled = write_flac(&spec, total, sample).bytes;
    let reference = flac::read(plain.as_slice()).unwrap().buffer;

    let mut failures = 0;
    for (which, bytes) in [&plain, &tabled].into_iter().enumerate() {
        for seed in 1..=12 {
            let failing = common::Failing::new(Cursor::new(bytes.as_slice()), u64::MAX);
            let budget = Arc::clone(&failing.budget);
            let mut reader = AudioReader::new(failing).unwrap();
            let mut rng = Rng::new(seed + 100 * which as u64);
            let mut block = AudioBuffer::<f32>::new(2, 700);
            let mut position = 0u64;
            for step in 0..400 {
                let what = format!("file {which}, seed {seed}, step {step}");
                // Nothing at all, a little, a few frames' worth, or plenty.
                budget.store(
                    match rng.below(4) {
                        0 => 0,
                        1 => rng.below(600),
                        2 => rng.below(40_000),
                        _ => u64::MAX,
                    },
                    Ordering::Relaxed,
                );
                if rng.below(2) == 0 {
                    let target = rng.below(total + 500);
                    match reader.seek(target) {
                        Ok(landed) => {
                            assert_eq!(landed, target.min(total), "{what}");
                            position = landed;
                        }
                        Err(FormatError::Io(_)) | Err(FormatError::Flac(FlacError::Io(_))) => {
                            failures += 1;
                        }
                        Err(error) => panic!("{what}: seek to {target}: {error}"),
                    }
                    assert_eq!(reader.position(), position, "{what}");
                } else {
                    let size = rng.between(0, 700) as usize;
                    block.set_num_frames(size);
                    for channel in block.iter_channels_mut() {
                        channel.fill(UNTOUCHED);
                    }
                    let got = match reader.read(&mut block) {
                        Ok(got) => {
                            assert_eq!(got as u64, (size as u64).min(total - position), "{what}");
                            got
                        }
                        // Some frames may have been read before the disk
                        // failed: the position says how many.
                        Err(FormatError::Io(_)) | Err(FormatError::Flac(FlacError::Io(_))) => {
                            failures += 1;
                            let got = reader.position() - position;
                            assert!(got <= size as u64, "{what}");
                            got as usize
                        }
                        Err(error) => panic!("{what}: read at {position}: {error}"),
                    };
                    let from = position as usize;
                    block.set_num_frames(got);
                    assert_same(&what, &block, &slice(&reference, from..from + got));
                    position += got as u64;
                    assert_eq!(reader.position(), position, "{what}");
                }
            }
        }
    }
    // The disk did fail, often.
    assert!(failures > 1_000, "{failures} failures");
}

#[test]
fn what_is_not_audio_is_refused() {
    for bytes in [
        &b""[..],
        b"fLa",
        b"ID3\x04 not audio we know",
        b"OggS\0\x02",
    ] {
        assert!(matches!(
            AudioReader::new(Cursor::new(bytes)),
            Err(FormatError::Unrecognized)
        ));
    }
    assert!(matches!(
        AudioReader::new(Cursor::new(&b"RIFF\0\0\0\0AVI "[..])),
        Err(FormatError::Wav(WavError::NotWav))
    ));
    assert!(matches!(
        AudioReader::new(Cursor::new(&b"FORM\0\0\0\0ILBM"[..])),
        Err(FormatError::Aiff(AiffError::NotAiff))
    ));
    assert!(matches!(
        AudioReader::new(Cursor::new(&b"fLaC"[..])),
        Err(FormatError::Flac(FlacError::Truncated))
    ));
    assert!(matches!(
        AudioReader::open("no such file anywhere.wav"),
        Err(FormatError::Io(_))
    ));
}

#[test]
#[should_panic(expected = "as many channels as the file")]
fn reading_into_a_buffer_of_the_wrong_shape_panics() {
    let mut reader = AudioReader::new(Cursor::new(data!("stereo16.wav"))).unwrap();
    let _ = reader.read(&mut AudioBuffer::<f32>::new(1, 16));
}

/// The three ways of handing audio to a writer, taken in turn.
fn write_in_pieces<W: std::io::Write + std::io::Seek>(
    writer: &mut AudioWriter<W>,
    audio: &AudioBuffer<f32>,
    seed: u64,
) {
    let channels = audio.num_channels();
    let mut rng = Rng::new(seed);
    let mut done = 0;
    let mut interleaved = Vec::new();
    while done < audio.num_frames() {
        let count = (rng.between(0, 2_500) as usize).min(audio.num_frames() - done);
        let piece = slice(audio, done..done + count);
        if rng.below(2) == 0 {
            writer.write(&piece).unwrap();
        } else {
            interleaved.resize(count * channels, 0.0);
            piece.copy_to_interleaved(&mut interleaved);
            writer.write_interleaved(&interleaved).unwrap();
        }
        done += count;
        assert_eq!(writer.frames_written(), done as u64);
    }
}

/// Audio that fills the range of every format and steps outside it.
fn loud_ramp(channels: usize, frames: usize) -> AudioBuffer<f32> {
    let mut buffer = ramp(channels, frames);
    for channel in buffer.iter_channels_mut() {
        for (frame, sample) in channel.iter_mut().enumerate() {
            match frame % 41 {
                0 => *sample = 1.0,
                1 => *sample = -1.0,
                2 => *sample = 1.5,
                3 => *sample = -7.0,
                4 => *sample = 1.0e-7,
                _ => *sample *= 1.9,
            }
        }
    }
    buffer
}

#[test]
fn a_file_written_in_blocks_holds_what_the_whole_file_writers_write() {
    for (channels, frames) in [(1usize, 0usize), (1, 1), (1, 4_999), (2, 5_000), (5, 3_333)] {
        let audio = loud_ramp(channels, frames);

        for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
            let name = format!("WAV {format:?}, {channels} x {frames}");
            let mut whole = Vec::new();
            wav::write(&mut whole, &audio, 44_100, format).unwrap();

            let sink = Cursor::new(Vec::new());
            let mut writer = AudioWriter::wav(sink, channels, 44_100, format).unwrap();
            assert_eq!(writer.format(), FileFormat::Wav);
            assert_eq!(writer.num_channels(), channels);
            assert_eq!(writer.sample_rate(), 44_100);
            write_in_pieces(&mut writer, &audio, 3);
            let streamed = writer.finish().unwrap().into_inner();

            // The same format chunk and the same sample bytes, behind a
            // header that is 36 bytes longer for the room it reserves.
            assert_eq!(streamed.len(), whole.len() + 36, "{name}");
            assert_eq!(&streamed[..4], b"RIFF", "{name}");
            let riff_size = u32::from_le_bytes(streamed[4..8].try_into().unwrap());
            assert_eq!(riff_size as usize, streamed.len() - 8, "{name}");
            assert_eq!(&streamed[12..20], b"JUNK\x1C\0\0\0", "{name}");
            assert_eq!(&streamed[48..72], &whole[12..36], "{name}");
            assert_eq!(&streamed[72..], &whole[36..], "{name}");

            let expected = wav::read(whole.as_slice()).unwrap();
            assert_eq!(wav::read(streamed.as_slice()).unwrap(), expected, "{name}");
            let again = read_in_blocks(&name, &streamed, || 777);
            assert_same(&name, &again, &expected.buffer);
        }

        for format in [AiffFormat::Int16, AiffFormat::Int24, AiffFormat::Float32] {
            let name = format!("AIFF {format:?}, {channels} x {frames}");
            let mut whole = Vec::new();
            aiff::write(&mut whole, &audio, 44_100, format).unwrap();

            let sink = Cursor::new(Vec::new());
            let mut writer = AudioWriter::aiff(sink, channels, 44_100, format).unwrap();
            assert_eq!(writer.format(), FileFormat::Aiff);
            write_in_pieces(&mut writer, &audio, 4);
            let streamed = writer.finish().unwrap().into_inner();

            // AIFF has one layout: the two files are the same byte for byte.
            assert!(streamed == whole, "{name}: the files differ");
            let expected = aiff::read(whole.as_slice()).unwrap();
            let again = read_in_blocks(&name, &streamed, || 777);
            assert_same(&name, &again, &expected.buffer);
        }
    }
}

#[test]
fn files_are_laid_out_as_ffmpeg_lays_out_the_same_audio() {
    let audio = wav::read(data!("stereo16.wav")).unwrap().buffer;

    // RF64: the 80 bytes before the samples are FFmpeg's, field for field.
    let sink = Cursor::new(Vec::new());
    let mut writer = AudioWriter::wav_rf64(sink, 2, 44_100, WavFormat::Int16).unwrap();
    write_in_pieces(&mut writer, &audio, 8);
    let ours = writer.finish().unwrap().into_inner();
    let theirs = data!("stereo16-rf64.wav");
    assert_eq!(ours.len(), theirs.len());
    assert!(ours[..80] == theirs[..80], "the RF64 headers differ");

    // AIFF: the whole file is FFmpeg's, the samples being scaled by a power
    // of two and so the same in both.
    let sink = Cursor::new(Vec::new());
    let mut writer = AudioWriter::aiff(sink, 2, 44_100, AiffFormat::Int16).unwrap();
    write_in_pieces(&mut writer, &audio, 9);
    let ours = writer.finish().unwrap().into_inner();
    assert!(ours == data!("stereo16.aiff"), "the AIFF files differ");
}

#[test]
fn a_write_that_fails_part_of_the_way_keeps_the_frames_that_were_taken() {
    let audio = ramp(2, 1_000);
    for aiff in [false, true] {
        // The header (80 bytes of WAV, 54 of AIFF), then room for 300
        // frames of six bytes and two bytes more.
        let header = if aiff { 54 } else { 80 };
        let sink = common::Failing::new(Cursor::new(Vec::new()), header + 300 * 6 + 2);
        let budget = sink.budget.clone();
        let mut writer = if aiff {
            AudioWriter::aiff(sink, 2, 48_000, AiffFormat::Int24).unwrap()
        } else {
            AudioWriter::wav(sink, 2, 48_000, WavFormat::Int24).unwrap()
        };
        writer.write(&slice(&audio, 0..200)).unwrap();
        assert!(matches!(
            writer.write(&slice(&audio, 200..500)),
            Err(FormatError::Io(_))
        ));
        // The count stands at the last write that went through.
        assert_eq!(writer.frames_written(), 200);

        // The disk has room again. Writing goes on after the 300th frame,
        // the last whole one the disk took.
        budget.store(u64::MAX, Ordering::Relaxed);
        writer.write_interleaved(&[0.25; 2 * 10]).unwrap();
        assert_eq!(writer.frames_written(), 310);
        writer.write(&slice(&audio, 900..1_000)).unwrap();
        let bytes = writer.finish().unwrap().into_inner().into_inner();

        let file = decode(&bytes).unwrap().buffer;
        let expected = if aiff {
            slice(&audio, 0..300)
        } else {
            slice(&through_24_bits(&audio), 0..300)
        };
        assert_eq!(file.num_frames(), 410, "aiff: {aiff}");
        assert_same("before the failure", &slice(&file, 0..300), &expected);
        assert_eq!(slice(&file, 300..310).channel(1), &[0.25; 10]);

        // And if nothing more is written, finishing alone keeps them.
        let sink = common::Failing::new(Cursor::new(Vec::new()), header + 300 * 6 + 2);
        let budget = sink.budget.clone();
        let mut writer = if aiff {
            AudioWriter::aiff(sink, 2, 48_000, AiffFormat::Int24).unwrap()
        } else {
            AudioWriter::wav(sink, 2, 48_000, WavFormat::Int24).unwrap()
        };
        assert!(writer.write(&audio).is_err());
        budget.store(u64::MAX, Ordering::Relaxed);
        let bytes = writer.finish().unwrap().into_inner().into_inner();
        let file = decode(&bytes).unwrap().buffer;
        assert_same("only what was taken", &file, &expected);
    }
}

/// Writes `audio` in blocks of 100 frames, as a recorder's worker hands
/// them over, up to the first block that is refused. Returns how many
/// frames went into blocks that were accepted.
fn write_until_refused<W: Write + Seek>(
    writer: &mut AudioWriter<W>,
    audio: &AudioBuffer<f32>,
) -> usize {
    for start in (0..audio.num_frames()).step_by(100) {
        let end = (start + 100).min(audio.num_frames());
        if writer.write(&slice(audio, start..end)).is_err() {
            return start;
        }
    }
    audio.num_frames()
}

#[test]
fn a_disk_that_stays_full_still_gets_its_header() {
    // In the test above the disk has room again by the time the file is
    // finished. Here it never has: what is on it can be written over, and
    // that is all. Completing a header is writing over, so it must work.
    let audio = ramp(2, 2_000);
    for (name, header, frame, aiff) in [
        ("WAV, 32-bit float", 80u64, 8u64, None),
        ("AIFF, 24 bits", 54, 6, Some(AiffFormat::Int24)),
        ("AIFF-C, 32-bit float", 60, 8, Some(AiffFormat::Float32)),
    ] {
        // Room for the header, 600 frames and half of another.
        let room = header + 600 * frame + frame / 2;
        let disk = FullDisk::new(room);
        let contents = disk.contents();
        let mut writer = match aiff {
            Some(format) => AudioWriter::aiff(disk, 2, 48_000, format).unwrap(),
            None => AudioWriter::wav(disk, 2, 48_000, WavFormat::Float32).unwrap(),
        };
        // The block that crosses the 600th frame is the one refused.
        assert_eq!(write_until_refused(&mut writer, &audio), 600, "{name}");
        assert_eq!(writer.frames_written(), 600, "{name}");
        // Still full: every further write fails and changes nothing.
        assert!(writer.write(&slice(&audio, 0..100)).is_err(), "{name}");
        assert!(writer.write_silence(1).is_err(), "{name}");
        writer
            .finish()
            .unwrap_or_else(|error| panic!("{name}: the header was not completed: {error}"));

        let bytes = contents.lock().unwrap().clone();
        assert_eq!(bytes.len() as u64, room, "{name}");
        // The sizes in the header are those of 600 frames, and no longer
        // the ones the file was started with.
        let at = header as usize - if aiff.is_some() { 12 } else { 4 };
        let size = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        let (data_size, form_size) = if aiff.is_some() {
            let form = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
            (u32::from_be_bytes(size) - 8, form)
        } else {
            let form = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            (u32::from_le_bytes(size), form)
        };
        assert_eq!(u64::from(data_size), 600 * frame, "{name}");
        assert_eq!(u64::from(form_size), header - 8 + 600 * frame, "{name}");
        let file = decode(&bytes).unwrap().buffer;
        assert_same(name, &file, &slice(&audio, 0..600));
    }
}

#[test]
fn on_a_full_disk_the_header_is_completed_before_the_padding_is_tried() {
    // One channel of 24 bits and room for exactly 601 frames: an odd number
    // of sample bytes, which is followed by a padding byte the disk has no
    // room for. That byte is all that may be missing.
    let audio = ramp(1, 2_000);
    for aiff in [false, true] {
        let header = if aiff { 54 } else { 80 };
        let disk = FullDisk::new(header + 601 * 3);
        let contents = disk.contents();
        let mut writer = if aiff {
            AudioWriter::aiff(disk, 1, 48_000, AiffFormat::Int24).unwrap()
        } else {
            AudioWriter::wav(disk, 1, 48_000, WavFormat::Int24).unwrap()
        };
        assert_eq!(write_until_refused(&mut writer, &audio), 600);
        // The failure is reported, since the file is a byte short.
        assert!(matches!(writer.finish(), Err(FormatError::Io(_))));

        let bytes = contents.lock().unwrap().clone();
        assert_eq!(bytes.len() as u64, header + 601 * 3);
        // The header says 601 frames, the one the disk took whole included.
        if aiff {
            assert_eq!(bytes[22..26], 601u32.to_be_bytes());
            assert_eq!(bytes[42..46], (8 + 601u32 * 3).to_be_bytes());
        } else {
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(bytes[76..80], (601u32 * 3).to_le_bytes());
        }
        let file = decode(&bytes).unwrap().buffer;
        let expected = if aiff {
            slice(&audio, 0..601)
        } else {
            slice(&through_24_bits(&audio), 0..601)
        };
        assert_same("601 frames", &file, &expected);
    }
}

#[test]
fn files_are_created_without_a_buffer_because_a_buffer_cannot_be_mended() {
    // The path constructors hand the writer the file itself.
    type Create<F> = fn(&'static str, usize, u32, F) -> Result<AudioWriter<File>, FormatError>;
    let _: Create<WavFormat> = AudioWriter::create_wav;
    let _: Create<AiffFormat> = AudioWriter::create_aiff;

    // What that spares them. The same full disk as above, behind a buffer:
    // the buffer accepts frames the disk then refuses, and from there on it
    // can neither be flushed nor moved, so the header cannot be reached.
    let audio = ramp(2, 2_000);
    let disk = FullDisk::new(80 + 600 * 8 + 4);
    let contents = disk.contents();
    let mut writer = AudioWriter::wav(BufWriter::new(disk), 2, 48_000, WavFormat::Float32).unwrap();
    let accepted = write_until_refused(&mut writer, &audio);
    assert!(accepted > 600, "{accepted} frames were accepted");
    assert!(accepted < 2_000);
    // This is the limit the documentation states: an error, not a file
    // that claims to be finished.
    assert!(matches!(writer.finish(), Err(FormatError::Io(_))));

    let bytes = contents.lock().unwrap().clone();
    assert_eq!(bytes[76..80], [0xFF; 4]);
    // What reached the disk is still read, as any unfinished file is.
    let file = decode(&bytes).unwrap().buffer;
    assert_same("behind a buffer", &file, &slice(&audio, 0..600));
}

#[test]
fn silence_can_be_written_without_a_buffer() {
    let audio = ramp(2, 100);
    for rf64 in [false, true] {
        let sink = Cursor::new(Vec::new());
        let mut writer = if rf64 {
            AudioWriter::wav_rf64(sink, 2, 8_000, WavFormat::Float32).unwrap()
        } else {
            AudioWriter::wav(sink, 2, 8_000, WavFormat::Float32).unwrap()
        };
        writer.write(&audio).unwrap();
        writer.write_silence(70_000).unwrap();
        writer.write(&audio).unwrap();
        assert_eq!(writer.frames_written(), 70_200);
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(&bytes[..4], if rf64 { b"RF64" } else { b"RIFF" });

        let file = wav::read(bytes.as_slice()).unwrap();
        assert_eq!(file.buffer.num_frames(), 70_200);
        assert_eq!(slice(&file.buffer, 0..100), audio);
        assert_eq!(slice(&file.buffer, 100..70_100).peak(), 0.0);
        assert_eq!(slice(&file.buffer, 70_100..70_200), audio);
    }
}

#[test]
fn a_file_can_start_anywhere_in_its_writer() {
    for aiff in [false, true] {
        let mut sink = Cursor::new(b"something else".to_vec());
        sink.set_position(14);
        let mut writer = if aiff {
            AudioWriter::aiff(sink, 1, 22_050, AiffFormat::Int24).unwrap()
        } else {
            AudioWriter::wav(sink, 1, 22_050, WavFormat::Int24).unwrap()
        };
        // An odd number of bytes, so that a padding byte is due.
        let audio = ramp(1, 333);
        writer.write(&audio).unwrap();
        let sink = writer.finish().unwrap();
        // The writer is left at the end, ready for whatever comes next.
        assert_eq!(sink.position(), sink.get_ref().len() as u64);
        let bytes = sink.into_inner();
        assert_eq!(&bytes[..14], b"something else");
        assert_eq!((bytes.len() - 14) % 2, 0);

        let mut whole = Vec::new();
        if aiff {
            aiff::write(&mut whole, &audio, 22_050, AiffFormat::Int24).unwrap();
        } else {
            wav::write(&mut whole, &audio, 22_050, WavFormat::Int24).unwrap();
        }
        assert_eq!(decode(&bytes[14..]).unwrap(), decode(&whole).unwrap());
    }
}

#[test]
fn a_recording_that_was_never_finished_can_still_be_read() {
    let audio = ramp(2, 3_000);
    for kind in 0..4 {
        let mut sink = Cursor::new(Vec::new());
        let mut whole = Vec::new();
        let mut writer = match kind {
            0 => {
                wav::write(&mut whole, &audio, 48_000, WavFormat::Int16).unwrap();
                AudioWriter::wav(&mut sink, 2, 48_000, WavFormat::Int16).unwrap()
            }
            1 => {
                wav::write(&mut whole, &audio, 48_000, WavFormat::Float32).unwrap();
                AudioWriter::wav(&mut sink, 2, 48_000, WavFormat::Float32).unwrap()
            }
            2 => {
                aiff::write(&mut whole, &audio, 48_000, AiffFormat::Int16).unwrap();
                AudioWriter::aiff(&mut sink, 2, 48_000, AiffFormat::Int16).unwrap()
            }
            _ => {
                aiff::write(&mut whole, &audio, 48_000, AiffFormat::Float32).unwrap();
                AudioWriter::aiff(&mut sink, 2, 48_000, AiffFormat::Float32).unwrap()
            }
        };
        writer.write(&audio).unwrap();
        // The program dies here: no `finish`.
        drop(writer);

        let mut bytes = sink.into_inner();
        // The disk kept all but the last three bytes: part of a frame.
        bytes.truncate(bytes.len() - 3);
        let expected = decode(&whole).unwrap();
        let file = decode(&bytes).unwrap();
        assert_eq!(file.sample_rate, 48_000, "kind {kind}");
        assert_eq!(file.bits_per_sample, expected.bits_per_sample);
        assert_same(
            &format!("kind {kind}"),
            &file.buffer,
            &slice(&expected.buffer, 0..2_999),
        );

        let mut reader = AudioReader::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(reader.len_frames(), Some(2_999));
        assert_eq!(reader.seek(2_990).unwrap(), 2_990);
        let mut block = AudioBuffer::<f32>::new(2, 64);
        assert_eq!(reader.read(&mut block).unwrap(), 9);
    }
}

/// The sizes an unfinished WAV header holds are all ones, which read as
/// numbers stop a little short of 4 GiB. The recordings that outgrow that
/// are the ones RF64 exists for, and the ones it hurts most to lose: an
/// unfinished one must be read to its end too, not to where 32 bits end.
#[test]
fn an_unfinished_wav_past_four_gigabytes_is_read_to_its_end() {
    const FRAME: u64 = 18;
    let head = ramp(6, 1_000);
    let mut tail = ramp(6, 1_000);
    tail.apply_gain(-0.5);
    let silent_frames = 300_000_000u64;
    let total_frames = 1_000 + silent_frames + 1_000;
    let tail_start = 80 + (1_000 + silent_frames) * FRAME;
    let kept = [
        0..80 + 1_000 * FRAME,
        tail_start..tail_start + 1_000 * FRAME,
    ];

    let mut sink = SparseFile::new(&kept);
    let mut writer = AudioWriter::wav(&mut sink, 6, 96_000, WavFormat::Int24).unwrap();
    writer.write(&head).unwrap();
    writer.write_silence(silent_frames).unwrap();
    writer.write(&tail).unwrap();
    // The program dies here: no `finish`.
    drop(writer);
    assert_eq!(sink.total_len(), 80 + total_frames * FRAME);
    assert!(sink.total_len() > 5 * (1 << 30));
    // The header is the one the file was started with.
    let header = sink.bytes(0..80);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[4..8], &[0xFF; 4]);
    assert_eq!(&header[12..16], b"JUNK");
    assert_eq!(&header[72..80], b"data\xFF\xFF\xFF\xFF");

    let mut reader = AudioReader::new(sink).unwrap();
    assert_eq!(
        reader.len_frames(),
        Some(total_frames),
        "the audio past 4 GiB of an unfinished recording is not read"
    );
    let mut block = AudioBuffer::<f32>::new(6, 1_000);
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    assert_same("the start", &block, &through_24_bits(&head));
    assert_eq!(
        reader.seek(total_frames - 1_000).unwrap(),
        total_frames - 1_000
    );
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    assert_same("the end", &block, &through_24_bits(&tail));
    assert_eq!(reader.read(&mut block).unwrap(), 0);
    assert_eq!(reader.seek(u64::MAX).unwrap(), total_frames);
}

#[test]
fn a_writer_refuses_what_its_format_cannot_hold() {
    let sink = || Cursor::new(Vec::new());
    assert!(matches!(
        AudioWriter::wav(sink(), 0, 48_000, WavFormat::Int16),
        Err(FormatError::Wav(WavError::Unsupported(_)))
    ));
    assert!(matches!(
        AudioWriter::wav(sink(), 65_536, 48_000, WavFormat::Int16),
        Err(FormatError::Wav(WavError::TooLarge))
    ));
    assert!(matches!(
        AudioWriter::aiff(sink(), 0, 48_000, AiffFormat::Int16),
        Err(FormatError::Aiff(AiffError::Unsupported(_)))
    ));
    assert!(matches!(
        AudioWriter::aiff(sink(), 65_536, 48_000, AiffFormat::Int16),
        Err(FormatError::Aiff(AiffError::TooLarge))
    ));
    assert!(AudioWriter::wav(sink(), 65_535, 48_000, WavFormat::Float32).is_ok());
}

#[test]
#[should_panic(expected = "as many channels as the file")]
fn writing_a_buffer_of_the_wrong_shape_panics() {
    let mut writer =
        AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Int16).unwrap();
    let _ = writer.write(&AudioBuffer::<f32>::new(3, 16));
}

/// What a 24-bit WAV file makes of `audio`.
fn through_24_bits(audio: &AudioBuffer<f32>) -> AudioBuffer<f32> {
    let mut bytes = Vec::new();
    wav::write(&mut bytes, audio, 96_000, WavFormat::Int24).unwrap();
    wav::read(bytes.as_slice()).unwrap().buffer
}

#[test]
fn a_wav_past_four_gigabytes_becomes_rf64_and_reads_back() {
    // Six channels of 24 bits: 18 bytes a frame. A stretch of audio, more
    // than five gigabytes of silence, and a stretch of audio at the end.
    // Only the two stretches and the header are kept; the silence is
    // counted and thrown away.
    const FRAME: u64 = 18;
    let head = ramp(6, 1_000);
    let mut tail = ramp(6, 1_000);
    tail.apply_gain(-0.5);
    let silent_frames = 300_000_000u64;
    let total_frames = 1_000 + silent_frames + 1_000;
    let data_len = total_frames * FRAME;
    assert!(data_len > 5 * (1 << 30));
    let tail_start = 80 + (1_000 + silent_frames) * FRAME;
    let kept = [
        0..80 + 1_000 * FRAME,
        tail_start - 90..tail_start + 1_000 * FRAME,
    ];

    let sink = SparseFile::new(&kept);
    let mut writer = AudioWriter::wav(sink, 6, 96_000, WavFormat::Int24).unwrap();
    writer.write(&head).unwrap();
    writer.write_silence(silent_frames).unwrap();
    writer.write(&tail).unwrap();
    assert_eq!(writer.frames_written(), total_frames);
    let sink = writer.finish().unwrap();
    assert_eq!(sink.total_len(), 80 + data_len);

    // The header, field by field (EBU Tech 3306).
    let header = sink.bytes(0..80);
    let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
    assert_eq!(&header[0..4], b"RF64");
    assert_eq!(u32_at(4), 0xFFFF_FFFF);
    assert_eq!(&header[8..16], b"WAVEds64");
    assert_eq!(u32_at(16), 28);
    assert_eq!(u64_at(20), 80 + data_len - 8, "RIFF size");
    assert_eq!(u64_at(28), data_len, "data size");
    assert_eq!(u64_at(36), total_frames, "sample count");
    assert_eq!(u32_at(44), 0, "table length");
    assert_eq!(&header[48..52], b"fmt ");
    assert_eq!(&header[72..76], b"data");
    assert_eq!(u32_at(76), 0xFFFF_FFFF);

    // The whole of it went through the writer, and none of it stayed.
    assert!(sink.written > data_len);

    // Reading it back finds both ends, the far one behind an offset that
    // does not fit in 32 bits.
    let mut reader = AudioReader::new(sink).unwrap();
    assert_eq!(reader.format(), FileFormat::Wav);
    assert_eq!(reader.num_channels(), 6);
    assert_eq!(reader.sample_rate(), 96_000);
    assert_eq!(reader.bits_per_sample(), 24);
    assert_eq!(reader.len_frames(), Some(total_frames));

    let mut block = AudioBuffer::<f32>::new(6, 1_000);
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    assert_same("the start", &block, &through_24_bits(&head));

    // Five frames of silence, then the tail.
    let tail_frame = 1_000 + silent_frames;
    assert_eq!(reader.seek(tail_frame - 5).unwrap(), tail_frame - 5);
    block.set_num_frames(5);
    assert_eq!(reader.read(&mut block).unwrap(), 5);
    assert_eq!(block.peak(), 0.0);
    block.set_num_frames(1_000);
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    assert_same("the end", &block, &through_24_bits(&tail));
    assert_eq!(reader.position(), total_frames);
    assert_eq!(reader.read(&mut block).unwrap(), 0);
    assert_eq!(reader.seek(u64::MAX).unwrap(), total_frames);
}

#[test]
fn an_aiff_cannot_grow_past_its_limit_and_stays_valid() {
    // Mono 16-bit: the limit is a little under 2^31 frames.
    let sink = SparseFile::new(std::slice::from_ref(&(0..64)));
    let mut writer = AudioWriter::aiff(sink, 1, 48_000, AiffFormat::Int16).unwrap();
    let frames = 2_147_483_000u64;
    writer.write_silence(frames).unwrap();

    let more = AudioBuffer::<f32>::new(1, 1_000);
    for _ in 0..2 {
        assert!(matches!(
            writer.write(&more),
            Err(FormatError::Aiff(AiffError::TooLarge))
        ));
        assert!(matches!(
            writer.write_interleaved(&[0.0; 1_000]),
            Err(FormatError::Aiff(AiffError::TooLarge))
        ));
        assert!(matches!(
            writer.write_silence(1_000),
            Err(FormatError::Aiff(AiffError::TooLarge))
        ));
        // Nothing was written by the calls that failed.
        assert_eq!(writer.frames_written(), frames);
    }
    // What still fits is still taken.
    writer.write(&AudioBuffer::<f32>::new(1, 100)).unwrap();
    let frames = frames + 100;

    let sink = writer.finish().unwrap();
    assert_eq!(sink.total_len(), 54 + frames * 2);
    let header = sink.bytes(0..54);
    let u32_at = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
    assert_eq!(&header[0..4], b"FORM");
    assert_eq!(u64::from(u32_at(4)), sink.total_len() - 8);
    assert_eq!(u64::from(u32_at(22)), frames);
    assert_eq!(&header[38..42], b"SSND");
    assert_eq!(u64::from(u32_at(42)), 8 + frames * 2);

    let reader = AudioReader::new(sink).unwrap();
    assert_eq!(reader.len_frames(), Some(frames));
}

/// A file in the system's directory for temporary files, removed when the
/// test is done with it, however the test ends.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        // The process number keeps two runs of the tests apart.
        let name = format!("manguebyte-formats-{}-{name}", std::process::id());
        Self(std::env::temp_dir().join(name))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Everything from where `reader` stands to the end, in blocks of 777.
fn read_to_end<R: std::io::Read + Seek>(reader: &mut AudioReader<R>) -> AudioBuffer<f32> {
    let channels = reader.num_channels();
    let mut collected = vec![Vec::new(); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 777);
    loop {
        let got = reader.read(&mut block).unwrap();
        for (channel, samples) in collected.iter_mut().enumerate() {
            samples.extend_from_slice(&block.channel(channel)[..got]);
        }
        if got < 777 {
            break;
        }
    }
    let mut buffer = AudioBuffer::new(channels, collected[0].len());
    for (channel, samples) in collected.iter().enumerate() {
        buffer.channel_mut(channel).copy_from_slice(samples);
    }
    buffer
}

/// Checks the file at `file`, which was written through its path, against
/// the bytes and the audio it should hold, reading it through its path.
fn check_file_at(
    name: &str,
    file: &TempFile,
    bytes: &[u8],
    expected: &AudioBuffer<f32>,
    format: FileFormat,
) {
    assert!(
        std::fs::read(&file.0).unwrap() == bytes,
        "{name}: the file on disk differs"
    );
    // The format is told from the contents: the name says nothing.
    let mut reader = AudioReader::open(&file.0).unwrap_or_else(|error| {
        panic!("{name}: {error}");
    });
    assert_eq!(reader.format(), format, "{name}");
    assert_eq!(reader.sample_rate(), 44_100, "{name}");
    assert_eq!(reader.num_channels(), 2, "{name}");
    assert_eq!(reader.len_frames(), Some(expected.num_frames() as u64));
    assert_same(name, &read_to_end(&mut reader), expected);
    assert_eq!(reader.seek(9_500).unwrap(), 9_500, "{name}");
    assert_same(
        name,
        &read_to_end(&mut reader),
        &slice(expected, 9_500..expected.num_frames()),
    );

    let read = manguebyte_formats::read_file(&file.0).unwrap();
    assert_eq!(read.format, format, "{name}");
    assert_eq!(read.sample_rate, 44_100, "{name}");
    assert_same(name, &read.buffer, expected);
}

#[test]
fn files_are_written_and_read_by_their_path() {
    let audio = loud_ramp(2, 10_000);

    for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
        let name = format!("WAV {format:?}");
        let file = TempFile::new(&format!("wav-{format:?}.audio"));
        let mut writer = AudioWriter::create_wav(&file.0, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 5);
        drop(writer.finish().unwrap());

        // The same audio written the same way into memory.
        let sink = Cursor::new(Vec::new());
        let mut writer = AudioWriter::wav(sink, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 5);
        let bytes = writer.finish().unwrap().into_inner();
        let expected = wav::read(bytes.as_slice()).unwrap();
        assert_eq!(wav::read_file(&file.0).unwrap(), expected, "{name}");
        check_file_at(&name, &file, &bytes, &expected.buffer, FileFormat::Wav);
    }

    for format in [AiffFormat::Int16, AiffFormat::Int24, AiffFormat::Float32] {
        let name = format!("AIFF {format:?}");
        let file = TempFile::new(&format!("aiff-{format:?}.audio"));
        let mut writer = AudioWriter::create_aiff(&file.0, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 6);
        drop(writer.finish().unwrap());

        // AIFF has one layout: the whole-file writer gives the same bytes.
        let mut bytes = Vec::new();
        aiff::write(&mut bytes, &audio, 44_100, format).unwrap();
        let expected = aiff::read(bytes.as_slice()).unwrap();
        assert_eq!(aiff::read_file(&file.0).unwrap(), expected, "{name}");
        check_file_at(&name, &file, &bytes, &expected.buffer, FileFormat::Aiff);
    }

    // Creating a file where there is one replaces it. And a file that was
    // created and never finished is on disk all the same, with every frame
    // that was written: nothing waits in a buffer for a `finish` that does
    // not come.
    for aiff in [false, true] {
        let file = TempFile::new("replaced.audio");
        std::fs::write(&file.0, vec![0x55; 100_000]).unwrap();
        let (mut writer, header) = if aiff {
            let format = AiffFormat::Float32;
            (
                AudioWriter::create_aiff(&file.0, 2, 44_100, format).unwrap(),
                60,
            )
        } else {
            let format = WavFormat::Float32;
            (
                AudioWriter::create_wav(&file.0, 2, 44_100, format).unwrap(),
                80,
            )
        };
        writer.write(&slice(&audio, 0..1_234)).unwrap();
        drop(writer);
        assert_eq!(
            std::fs::metadata(&file.0).unwrap().len(),
            header + 1_234 * 8
        );
        let mut reader = AudioReader::open(&file.0).unwrap();
        assert_eq!(reader.len_frames(), Some(1_234));
        assert_same(
            "never finished",
            &read_to_end(&mut reader),
            &slice(&audio, 0..1_234),
        );
    }
}

/// Writes files for other software to read. Not part of a normal run:
///
/// ```text
/// MANGUEBYTE_DUMP_DIR=/some/dir cargo test -p manguebyte-formats \
///     --test streaming -- --ignored
/// ```
///
/// `data/README.md` says what was checked with them.
#[test]
#[ignore = "writes files for checking with other software"]
fn dump_files_for_other_software() {
    let directory = std::env::var("MANGUEBYTE_DUMP_DIR").expect("set MANGUEBYTE_DUMP_DIR");
    let directory = std::path::Path::new(&directory);
    let audio = wav::read(data!("stereo16.wav")).unwrap().buffer;

    for (name, format) in [
        ("int16", WavFormat::Int16),
        ("int24", WavFormat::Int24),
        ("float32", WavFormat::Float32),
    ] {
        let path = directory.join(format!("streamed-{name}.wav"));
        let mut writer = AudioWriter::create_wav(path, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 1);
        writer.finish().unwrap();

        let path = directory.join(format!("streamed-{name}-rf64.wav"));
        let file = std::fs::File::create(path).unwrap();
        let mut writer = AudioWriter::wav_rf64(file, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 2);
        writer.finish().unwrap();
    }
    for (name, format) in [
        ("int16", AiffFormat::Int16),
        ("int24", AiffFormat::Int24),
        ("float32", AiffFormat::Float32),
    ] {
        let path = directory.join(format!("streamed-{name}.aiff"));
        let mut writer = AudioWriter::create_aiff(path, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 3);
        writer.finish().unwrap();

        // The same recording never finished, as a crash would leave it.
        let path = directory.join(format!("unfinished-{name}.aiff"));
        let mut writer = AudioWriter::create_aiff(path, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 3);
        drop(writer);
    }
    for (name, format) in [
        ("int16", WavFormat::Int16),
        ("int24", WavFormat::Int24),
        ("float32", WavFormat::Float32),
    ] {
        let path = directory.join(format!("unfinished-{name}.wav"));
        let mut writer = AudioWriter::create_wav(path, 2, 44_100, format).unwrap();
        write_in_pieces(&mut writer, &audio, 1);
        drop(writer);
    }

    // The FLAC streams the tests make for themselves, to see that another
    // decoder reads them as the tests expect.
    for (index, (name, spec, total)) in made_flac().into_iter().enumerate() {
        let fixture = made_flac_fixture(&name, &spec, total);
        std::fs::write(directory.join(format!("made-{index}.flac")), &fixture.bytes).unwrap();
        // What it should decode to, as 32-bit float.
        let expected = fixture.expected.unwrap();
        if expected.num_frames() > 0 {
            wav::write_file(
                directory.join(format!("made-{index}.wav")),
                &expected,
                spec.sample_rate,
                WavFormat::Float32,
            )
            .unwrap();
        }
    }
    let (repeated, _) = repeat_flac(data!("stereo16-blocks128.flac"), 10, true);
    std::fs::write(directory.join("repeated.flac"), repeated).unwrap();
    let tabled = table_for(data!("stereo16-level12.flac"), 2, |point| point);
    std::fs::write(directory.join("tabled.flac"), tabled).unwrap();
    let (decoys, _) = stream_with_decoys();
    std::fs::write(directory.join("decoys.flac"), decoys.bytes).unwrap();
}
