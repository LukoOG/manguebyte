//! MPEG audio layers I and II.
//!
//! The layer II files in `data/` were encoded by FFmpeg 7.1's own encoder
//! from audio `common::lossy` makes (see `data/README.md`), one for each of
//! the allocation tables of 11172-3 and the one of 13818-3; their decodes
//! are compared with that audio. FFmpeg writes neither layer I, nor joint
//! stereo, nor a CRC, so `common::mpeg12` writes streams of chosen subband
//! samples, and the decodes of the same samples written in different ways
//! are compared bit for bit: layer I against layer II, joint stereo against
//! stereo, with a CRC against without.

mod common;

use std::io::Cursor;

use common::lossy::{
    check_seeks_are_exact, closeness, fitted_closeness, poke, read_in_random_blocks,
    read_what_can_be_read, source,
};
use common::mpeg12::{self, Mode, Subbands};
use common::{Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::mp3::{self, Mp3Error};
use manguebyte_formats::{AudioReader, FileFormat, FormatError, decode};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// Samples between the input of an encoder's analysis filter bank and the
/// output of the decoder's synthesis filter bank: 512 taps each, less 31
/// (11172-3, Annex C).
const DELAY: usize = 481;

struct File {
    name: &'static str,
    bytes: &'static [u8],
    source: &'static str,
    frames: usize,
    /// The least signal-to-noise ratio over the whole source, and over its
    /// steady tones once their level is matched, in decibels.
    snr: (f64, f64),
    max_error: f32,
}

fn files() -> [File; 6] {
    [
        // Table 3-B.2b: 96 kbit/s a channel at 44.1 kHz.
        File {
            name: "mp2-2ch-44100-192.mp2",
            bytes: data!("mp2-2ch-44100-192.mp2"),
            source: "signal-2ch-44100",
            frames: 96,
            snr: (11.4, 39.0),
            max_error: 0.6,
        },
        // Table 3-B.2c: 48 kbit/s a channel at 48 kHz.
        File {
            name: "mp2-tones-2ch-48000-96.mp2",
            bytes: data!("mp2-tones-2ch-48000-96.mp2"),
            source: "tones-2ch-48000",
            frames: 42,
            snr: (32.4, 32.4),
            max_error: 0.1,
        },
        // Table 3-B.2d: 32 kbit/s a channel at 32 kHz.
        File {
            name: "mp2-2ch-32000-64.mp2",
            bytes: data!("mp2-2ch-32000-64.mp2"),
            source: "signal-2ch-32000",
            frames: 35,
            snr: (7.6, 26.0),
            max_error: 0.6,
        },
        // Table 3-B.2a: 80 kbit/s a channel at 32 kHz.
        File {
            name: "mp2-2ch-32000-160.mp2",
            bytes: data!("mp2-2ch-32000-160.mp2"),
            source: "signal-2ch-32000",
            frames: 35,
            snr: (11.8, 42.5),
            max_error: 0.55,
        },
        // Table B.1 of 13818-3: the lower sample frequencies.
        File {
            name: "mp2-1ch-22050-64.mp2",
            bytes: data!("mp2-1ch-22050-64.mp2"),
            source: "signal-1ch-22050",
            frames: 39,
            snr: (12.0, 47.0),
            max_error: 0.6,
        },
        // Table 3-B.2a again, 64 kbit/s a channel at 44.1 kHz, in a file of
        // 100 frames of audio.
        File {
            name: "mp2-2ch-44100.mp2",
            bytes: data!("mp2-2ch-44100.mp2"),
            source: "tones-2ch-44100-short",
            frames: 1,
            snr: (23.4, 23.4),
            max_error: 0.1,
        },
    ]
}

#[test]
fn layer_ii_files_decode_near_the_audio_they_were_made_from() {
    for file in files() {
        let name = file.name;
        let decoded = mp3::read(file.bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let (audio, rate) = source(file.source);
        assert_eq!(decoded.sample_rate, rate, "{name}");
        assert_eq!(
            decoded.buffer.num_channels(),
            audio.num_channels(),
            "{name}"
        );
        // Every sample of every frame, delay included: no header says
        // otherwise.
        assert_eq!(decoded.buffer.num_frames(), file.frames * 1_152, "{name}");
        // FFmpeg stops a little short of the end of what it was given.
        let len = audio.num_frames().min(file.frames * 1_152 - DELAY);
        let aligned = slice(&decoded.buffer, DELAY..DELAY + len);
        let (snr, worst) = closeness(&aligned, &audio, 0..len);
        let tones = if file.source.starts_with("signal") {
            0..len * 3 / 10
        } else {
            0..len
        };
        let (level, tones_snr) = fitted_closeness(&aligned, &audio, tones);
        println!(
            "{name}: {snr:.2} dB, largest error {worst:.4}; tones at level {level:.4}, \
             {tones_snr:.2} dB once matched"
        );
        assert!(snr >= file.snr.0, "{name}: {snr:.2} dB");
        assert!(tones_snr >= file.snr.1, "{name}: {tones_snr:.2} dB");
        assert!(worst <= file.max_error, "{name}: {worst}");
        // FFmpeg's encoder keeps the level, unlike LAME at low rates.
        assert!((0.99..=1.01).contains(&level), "{name}: level {level}");

        let any = decode(file.bytes).unwrap();
        assert_eq!(any.format, FileFormat::Mp3);
        assert!(any.buffer == decoded.buffer, "{name}");
        let reader = AudioReader::new(Cursor::new(file.bytes)).unwrap();
        assert_eq!(reader.len_frames(), None, "{name}");
    }
}

#[test]
fn blocks_of_any_size_and_seeks_give_exactly_the_whole_decode() {
    for file in files() {
        let whole = mp3::read(file.bytes).unwrap().buffer;
        let streamed = read_in_random_blocks(file.bytes, file.bytes.len() as u64);
        assert!(streamed == whole, "{}: read in blocks", file.name);
        check_seeks_are_exact(file.name, file.bytes, &whole, 40, 11);
    }
    // Layer I, with seeks across a stream long enough to be searched.
    let bytes = mpeg12::layer1(&Subbands::random(2, 60, 5), Mode::Joint(1), true);
    let whole = mp3::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(whole.num_frames(), 180 * 384);
    assert!(read_in_random_blocks(&bytes, 9) == whole);
    check_seeks_are_exact("layer I", &bytes, &whole, 60, 13);
}

#[test]
fn layer_i_and_layer_ii_decode_the_same_subband_samples_alike() {
    for channels in [1, 2] {
        let subbands = Subbands::random(channels, 12, 21 + channels as u64);
        let mode = if channels == 1 {
            Mode::Mono
        } else {
            Mode::Stereo
        };
        let one = mp3::read(mpeg12::layer1(&subbands, mode, false).as_slice()).unwrap();
        let two = mp3::read(mpeg12::layer2(&subbands, mode, false).as_slice()).unwrap();
        assert_eq!(one.sample_rate, 48_000);
        assert_eq!(one.buffer.num_frames(), 12 * 1_152);
        assert!(one.buffer == two.buffer, "{channels} channels");
        // Not silence, nor noise beyond full scale.
        let peak = one
            .buffer
            .iter_channels()
            .flatten()
            .fold(0.0f32, |peak, &sample| peak.max(sample.abs()));
        assert!((0.05..2.0).contains(&peak), "{peak}");
    }
}

#[test]
fn joint_stereo_decodes_as_the_same_samples_in_stereo() {
    let subbands = Subbands::random(2, 6, 99);
    let stereo = (
        mp3::read(mpeg12::layer1(&subbands, Mode::Stereo, false).as_slice()).unwrap(),
        mp3::read(mpeg12::layer2(&subbands, Mode::Stereo, false).as_slice()).unwrap(),
    );
    for extension in 0..4 {
        let mode = Mode::Joint(extension);
        let one = mp3::read(mpeg12::layer1(&subbands, mode, false).as_slice()).unwrap();
        let two = mp3::read(mpeg12::layer2(&subbands, mode, false).as_slice()).unwrap();
        assert!(
            one.buffer == stereo.0.buffer,
            "layer I, extension {extension}"
        );
        assert!(
            two.buffer == stereo.1.buffer,
            "layer II, extension {extension}"
        );
    }
    // The two channels differ, by their scale factors and below the bound.
    assert!(stereo.0.buffer.channel(0) != stereo.0.buffer.channel(1));
}

#[test]
fn frames_with_a_crc_are_checked_against_it() {
    let subbands = Subbands::random(2, 4, 3);
    for layer in [1, 2] {
        let write = |protected| match layer {
            1 => mpeg12::layer1(&subbands, Mode::Joint(2), protected),
            _ => mpeg12::layer2(&subbands, Mode::Joint(2), protected),
        };
        let plain = mp3::read(write(false).as_slice()).unwrap();
        let protected = write(true);
        assert!(mp3::read(protected.as_slice()).unwrap().buffer == plain.buffer);

        // A bit of the allocation of the second frame, which the CRC
        // covers.
        let frame_len = if layer == 1 { 448 } else { 1_152 };
        let mut damaged = protected.clone();
        damaged[frame_len + 7] ^= 0x10;
        match mp3::read(damaged.as_slice()) {
            Err(Mp3Error::Corrupt) => {}
            other => panic!("layer {layer}: {:?}", other.map(|_| ())),
        }
        // Through the reader, the frame before the damage is read, and the
        // damaged one is reported.
        let samples = if layer == 1 { 384 } else { 1_152 };
        let mut reader = AudioReader::new(Cursor::new(damaged.as_slice())).unwrap();
        let mut block = AudioBuffer::new(2, samples);
        assert_eq!(reader.read(&mut block).unwrap(), samples);
        assert!(block == slice(&plain.buffer, 0..samples), "layer {layer}");
        assert!(matches!(
            reader.read(&mut block),
            Err(FormatError::Mp3(Mp3Error::Corrupt))
        ));
    }
}

#[test]
fn free_format_is_refused_and_so_is_a_tag_alone() {
    let mut free = data!("mp2-2ch-32000-64.mp2").to_vec();
    // Every frame's bitrate index set to 0, which is free format.
    for frame in 0..35 {
        free[frame * 288 + 2] &= 0x0F;
    }
    match AudioReader::new(Cursor::new(free.as_slice())) {
        Err(FormatError::Mp3(Mp3Error::Unsupported(what))) => assert!(what.contains("free")),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert_eq!(
        FileFormat::detect(data!("mp2-2ch-44100.mp2")),
        Some(FileFormat::Mp3)
    );
}

#[test]
fn damage_never_panics() {
    let layer1 = mpeg12::layer1(&Subbands::random(2, 2, 8), Mode::Joint(0), true);
    for bytes in [
        data!("mp2-2ch-32000-64.mp2"),
        data!("mp2-1ch-22050-64.mp2"),
        layer1.as_slice(),
    ] {
        let mut rng = Rng::new(17);
        for round in 0..80 {
            let mut damaged = bytes.to_vec();
            let at = if round % 2 == 0 {
                rng.below(bytes.len() as u64) as usize
            } else {
                // The header and the allocation of some frame.
                let frame = rng.below(bytes.len() as u64 / 288) as usize * 288;
                frame + rng.below(40) as usize
            };
            damaged[at] = match round % 3 {
                0 => 0xFF,
                1 => 0x00,
                _ => damaged[at] ^ (1 << rng.below(8)),
            };
            poke(&damaged);
            let _ = read_what_can_be_read(&damaged);
        }
        for len in (0..bytes.len()).step_by(97) {
            poke(&bytes[..len]);
        }
    }
}
