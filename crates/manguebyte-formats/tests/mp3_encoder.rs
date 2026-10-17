//! The MP3 encoder, through this crate's decoder and FFmpeg's.
//!
//! The `encoder-*.mp3` files in `data/` are what the encoder wrote from
//! three of the test sources, and the `.mp3float.ref` and `.mp3fixed.ref`
//! files beside them what FFmpeg 7.1's floating-point and fixed-point MP3
//! decoders made of them (see `data/README.md`). How near the audio comes
//! to its source is measured with the noise-to-mask ratio of
//! `common/nmr.rs`, whose model is apart from the encoder's.

mod common;

use std::io::Cursor;

use common::lossy::{closeness, fitted_closeness, read_in_random_blocks, source};
use common::nmr::nmr;
use common::opus::Reference;
use common::{Failing, Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::metadata::{Metadata, Picture};
use manguebyte_formats::mp3::{self, Mp3Error, Mp3Options};
use manguebyte_formats::{AudioWriter, FileFormat, FormatError, decode};

/// Encodes `buffer` whole.
fn encode(buffer: &AudioBuffer<f32>, rate: u32, options: &Mp3Options) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    mp3::write(&mut bytes, buffer, rate, options).unwrap();
    bytes.into_inner()
}

/// The files in `data/`: the name, the source, the options.
fn fixtures() -> [(&'static str, &'static str, Mp3Options); 3] {
    [
        (
            "encoder-signal-2ch-32000-cbr96",
            "signal-2ch-32000",
            Mp3Options::constant(96),
        ),
        (
            "encoder-signal-1ch-48000-vbr5",
            "signal-1ch-48000",
            Mp3Options::variable(5.0),
        ),
        (
            "encoder-tones-2ch-44100-vbr8",
            "tones-2ch-44100",
            Mp3Options::variable(8.0).with_tag("TIT2", "Tones"),
        ),
    ]
}

macro_rules! fixture {
    ($name:literal) => {
        (
            include_bytes!(concat!("data/", $name, ".mp3")).as_slice(),
            include_bytes!(concat!("data/", $name, ".mp3float.ref")).as_slice(),
            include_bytes!(concat!("data/", $name, ".mp3fixed.ref")).as_slice(),
        )
    };
}

fn fixture_bytes(name: &str) -> (&'static [u8], &'static [u8], &'static [u8]) {
    match name {
        "encoder-signal-2ch-32000-cbr96" => fixture!("encoder-signal-2ch-32000-cbr96"),
        "encoder-signal-1ch-48000-vbr5" => fixture!("encoder-signal-1ch-48000-vbr5"),
        _ => fixture!("encoder-tones-2ch-44100-vbr8"),
    }
}

/// The largest difference between `decoded`, clipped to `-1.0..=clip`,
/// and the frames of `reference`.
fn largest_difference(decoded: &AudioBuffer<f32>, reference: &Reference, clip: f32) -> f32 {
    let mut worst = 0.0f32;
    for (kept, frame) in (0..reference.frames).step_by(reference.step).enumerate() {
        for channel in 0..reference.channels {
            let theirs = reference.samples[kept * reference.channels + channel];
            let ours = decoded.channel(channel)[frame].clamp(-1.0, clip);
            worst = worst.max((ours - theirs).abs());
        }
    }
    worst
}

#[test]
fn ffmpeg_decodes_what_the_encoder_writes_as_this_crate_does() {
    for (name, source_name, options) in fixtures() {
        let (bytes, float, fixed) = fixture_bytes(name);
        let (source, rate) = source(source_name);
        let file = mp3::read(bytes).unwrap();
        assert_eq!(file.sample_rate, rate);
        assert_eq!(file.buffer.num_channels(), source.num_channels());
        assert_eq!(
            file.buffer.num_frames(),
            source.num_frames(),
            "{name}: length"
        );
        for (decoder, reference, clip, tolerance) in [
            ("mp3float", float, f32::MAX, 1e-4),
            (
                "mp3 (fixed point)",
                fixed,
                32_767.0 / 32_768.0,
                4.0 / 32_768.0,
            ),
        ] {
            let reference = Reference::parse(reference);
            assert_eq!(
                reference.frames,
                source.num_frames(),
                "{name}: {decoder}'s length"
            );
            assert_eq!(reference.channels, source.num_channels());
            let worst = largest_difference(&file.buffer, &reference, clip);
            println!("{name}: {decoder} differs by {worst:.1e} at most");
            assert!(worst <= tolerance, "{name}: {decoder}: {worst}");
        }
        // The encoder still writes these bytes: when it changes, the files
        // are made again with `dump_files_for_other_software`.
        let again = encode(&source, rate, &options);
        assert!(
            again == bytes,
            "{name}: the encoder no longer writes the file in data/"
        );
    }
}

#[test]
fn every_length_comes_back_exactly() {
    for channels in [1usize, 2] {
        for (rate, frames) in [
            (44_100, 0usize),
            (44_100, 1),
            (44_100, 575),
            (44_100, 576),
            (44_100, 1_151),
            (44_100, 1_152),
            (44_100, 1_153),
            (48_000, 4_000),
            (32_000, 3_333),
            (44_100, 44_101),
        ] {
            let buffer = common::ramp(channels, frames);
            for options in [Mp3Options::constant(64), Mp3Options::variable(3.0)] {
                let bytes = encode(&buffer, rate, &options);
                let file = mp3::read(bytes.as_slice()).unwrap();
                assert_eq!(
                    file.buffer.num_frames(),
                    frames,
                    "{channels} channels at {rate} Hz"
                );
                assert_eq!(file.sample_rate, rate);
            }
        }
    }
}

#[test]
fn constant_bitrates_are_held_and_buy_less_noise() {
    let (buffer, rate) = source("signal-2ch-44100");
    let frames = buffer.num_frames();
    let seconds = frames as f64 / f64::from(rate);
    let mut last = f64::MAX;
    for (kbps, nmr_at_most) in [(64u32, 6.0), (128, -1.5), (192, -6.0), (320, -12.0)] {
        let bytes = encode(&buffer, rate, &Mp3Options::constant(kbps));
        // Every frame but the first, which holds the Xing header, is at
        // the bitrate; the stream is a frame longer than the audio and its
        // delay need.
        let audio_frames = (frames + 1_057).div_ceil(1_152) as f64;
        let expected = audio_frames * 144.0 * f64::from(kbps) * 1_000.0 / f64::from(rate);
        let first = bytes.len() as f64 - expected;
        assert!(
            (0.0..1_100.0).contains(&first),
            "{kbps}: {} bytes, {expected:.0} expected",
            bytes.len()
        );
        let decoded = mp3::read(bytes.as_slice()).unwrap().buffer;
        let masked = nmr(&decoded, &buffer, rate, 0..frames);
        let (_, tones) = fitted_closeness(&decoded, &buffer, 0..frames * 3 / 10);
        println!(
            "{kbps} kbit/s: {:.1} kbit/s with the Xing frame, noise-to-mask {:.1} dB ({:.1} % above the mask), {tones:.1} dB on the tones",
            bytes.len() as f64 * 8.0 / seconds / 1_000.0,
            masked.mean_db,
            100.0 * masked.audible
        );
        assert!(
            masked.mean_db <= nmr_at_most,
            "{kbps}: {:.1} dB",
            masked.mean_db
        );
        assert!(masked.mean_db < last);
        last = masked.mean_db;
    }
}

#[test]
fn quality_moves_the_noise_and_the_rate_together() {
    let (buffer, rate) = source("signal-2ch-44100");
    let frames = buffer.num_frames();
    let seconds = frames as f64 / f64::from(rate);
    let mut last: Option<(f64, f64)> = None;
    for (quality, nmr_at_most, kbps_within) in [
        (0.0f32, 3.0, 40.0..80.0),
        (5.0, -6.0, 70.0..130.0),
        (10.0, -14.0, 100.0..200.0),
    ] {
        let bytes = encode(&buffer, rate, &Mp3Options::variable(quality));
        let kbps = bytes.len() as f64 * 8.0 / seconds / 1_000.0;
        let decoded = mp3::read(bytes.as_slice()).unwrap().buffer;
        let masked = nmr(&decoded, &buffer, rate, 0..frames);
        let (snr, _) = closeness(&decoded, &buffer, 0..frames);
        println!(
            "quality {quality}: {kbps:.1} kbit/s, noise-to-mask {:.1} dB ({:.1} % above the mask), {snr:.1} dB",
            masked.mean_db,
            100.0 * masked.audible
        );
        assert!(
            masked.mean_db <= nmr_at_most,
            "quality {quality}: {:.1} dB",
            masked.mean_db
        );
        assert!(
            kbps_within.contains(&kbps),
            "quality {quality}: {kbps:.1} kbit/s"
        );
        if let Some((last_kbps, last_nmr)) = last {
            assert!(
                kbps > last_kbps && masked.mean_db < last_nmr,
                "quality {quality}"
            );
        }
        last = Some((kbps, masked.mean_db));
    }
}

#[test]
fn every_way_of_writing_gives_the_same_bytes_however_the_audio_is_cut() {
    let (buffer, rate) = source("signal-2ch-48000");
    for options in [
        Mp3Options::variable(4.0),
        Mp3Options::constant(160).with_joint_stereo(false),
    ] {
        let expected = encode(&buffer, rate, &options);
        assert!(encode(&buffer, rate, &options) == expected);
        let frames = buffer.num_frames();
        for seed in 1..3u64 {
            let mut rng = Rng::new(seed);
            let mut writer = AudioWriter::mp3(Cursor::new(Vec::new()), 2, rate, &options).unwrap();
            assert_eq!(writer.format(), FileFormat::Mp3);
            let mut done = 0;
            while done < frames {
                let count = (rng.between(0, 3_000) as usize).min(frames - done);
                let piece = slice(&buffer, done..done + count);
                if rng.below(2) == 0 {
                    writer.write(&piece).unwrap();
                } else {
                    let mut interleaved = Vec::with_capacity(count * 2);
                    for i in 0..count {
                        interleaved.push(piece.channel(0)[i]);
                        interleaved.push(piece.channel(1)[i]);
                    }
                    writer.write_interleaved(&interleaved).unwrap();
                }
                done += count;
                assert_eq!(writer.frames_written(), done as u64);
            }
            assert!(
                writer.finish().unwrap().into_inner() == expected,
                "seed {seed}"
            );
        }
    }
    let zeros = AudioBuffer::<f32>::new(1, 9_000);
    let options = Mp3Options::constant(96);
    let mut writer = AudioWriter::mp3(Cursor::new(Vec::new()), 1, 32_000, &options).unwrap();
    writer.write_silence(5_000).unwrap();
    writer.write(&slice(&zeros, 0..4_000)).unwrap();
    assert!(writer.finish().unwrap().into_inner() == encode(&zeros, 32_000, &options));
}

#[test]
fn what_is_written_reads_back_through_every_reader() {
    let (buffer, rate) = source("signal-2ch-44100");
    let bytes = encode(&buffer, rate, &Mp3Options::variable(6.0));
    let whole = decode(&bytes).unwrap();
    assert_eq!(whole.format, FileFormat::Mp3);
    assert_eq!(whole.buffer.num_frames(), buffer.num_frames());
    assert_eq!(read_in_random_blocks(&bytes, 5), whole.buffer);
    common::lossy::check_seeks_are_exact("encoder", &bytes, &whole.buffer, 40, 9);
}

#[test]
fn tags_and_pictures_come_back() {
    let metadata = Metadata {
        title: Some("Maracatu atômico".to_owned()),
        artist: Some("Chico Science & Nação Zumbi".to_owned()),
        track: Some(2),
        pictures: vec![Picture {
            kind: 3,
            mime: "image/png".to_owned(),
            description: "capa".to_owned(),
            data: vec![0x89, b'P', b'N', b'G', 9, 9],
            ..Picture::default()
        }],
        ..Metadata::default()
    };
    let options = Mp3Options::constant(128)
        .with_metadata(&metadata)
        .with_tag("MOOD", "frevo");
    let buffer = common::ramp(2, 3_000);
    let file = mp3::read(encode(&buffer, 48_000, &options).as_slice()).unwrap();
    assert_eq!(file.buffer.num_frames(), 3_000);
    assert_eq!(file.metadata.title, metadata.title);
    assert_eq!(file.metadata.artist, metadata.artist);
    assert_eq!(file.metadata.track, Some(2));
    assert_eq!(file.metadata.pictures, metadata.pictures);
    assert_eq!(file.metadata.tag("MOOD"), Some("frevo"));
    let mut writer = AudioWriter::mp3(Cursor::new(Vec::new()), 2, 48_000, &options).unwrap();
    assert!(matches!(
        writer.set_metadata(&metadata),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn options_are_checked_before_anything_is_written() {
    let buffer = AudioBuffer::<f32>::new(1, 100);
    for (options, channels, rate) in [
        (Mp3Options::constant(100), 1usize, 44_100u32),
        (Mp3Options::constant(8), 1, 44_100),
        (Mp3Options::variable(10.5), 1, 44_100),
        (Mp3Options::variable(f32::NAN), 1, 44_100),
        (Mp3Options::default(), 0, 44_100),
        (Mp3Options::default(), 3, 44_100),
        (Mp3Options::default(), 1, 22_050),
        (Mp3Options::default(), 1, 96_000),
    ] {
        let mut out = Cursor::new(Vec::new());
        let result = AudioWriter::mp3(&mut out, channels, rate, &options);
        assert!(
            matches!(result, Err(FormatError::Mp3(Mp3Error::Unsupported(_)))),
            "{options:?} {channels} {rate}"
        );
        assert!(out.into_inner().is_empty());
        if channels == 1 {
            let mut out = Cursor::new(Vec::new());
            assert!(matches!(
                mp3::write(&mut out, &buffer, rate, &options),
                Err(Mp3Error::Unsupported(_))
            ));
        }
    }
}

#[test]
fn hostile_samples_are_written_and_read_back_finite() {
    let frames = 12_000;
    let mut buffer = AudioBuffer::<f32>::new(2, frames);
    let mut rng = Rng::new(7);
    for channel in buffer.iter_channels_mut() {
        for (i, sample) in channel.iter_mut().enumerate() {
            *sample = match i % 7 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                3 => f32::MAX,
                4 => 1e-40,
                _ => (rng.below(2_000) as f32 - 1_000.0) * 1e3,
            };
        }
    }
    for options in [
        Mp3Options::constant(32),
        Mp3Options::constant(320),
        Mp3Options::variable(10.0),
    ] {
        let decoded = mp3::read(encode(&buffer, 44_100, &options).as_slice())
            .unwrap()
            .buffer;
        assert_eq!(decoded.num_frames(), frames);
        assert!(
            decoded
                .iter_channels()
                .all(|channel| channel.iter().all(|s| s.is_finite()))
        );
    }
}

#[test]
fn a_frame_that_could_not_be_written_is_written_again() {
    let (buffer, rate) = source("signal-2ch-32000");
    let options = Mp3Options::constant(128);
    let expected = encode(&buffer, rate, &options);
    let disk = Failing::new(Cursor::new(Vec::new()), 2_000);
    let budget = std::sync::Arc::clone(&disk.budget);
    let mut writer = AudioWriter::mp3(disk, 2, rate, &options).unwrap();
    let mut done = 0;
    let mut failures = 0;
    while done < buffer.num_frames() {
        let count = 1_500.min(buffer.num_frames() - done);
        let before = writer.frames_written();
        match writer.write(&slice(&buffer, done..done + count)) {
            Ok(()) => done += count,
            Err(FormatError::Io(_)) => {
                failures += 1;
                done += (writer.frames_written() - before) as usize;
                // Room for the frames kept, which are written again whole.
                budget.fetch_add(3_000, std::sync::atomic::Ordering::Relaxed);
            }
            Err(other) => panic!("{other}"),
        }
    }
    assert!(failures >= 3, "{failures} failures");
    budget.fetch_add(1 << 20, std::sync::atomic::Ordering::Relaxed);
    let bytes = writer.finish().unwrap().into_inner().into_inner();
    assert!(bytes == expected);
}

#[test]
fn a_file_can_be_written_to_and_read_from_a_path() {
    let directory = std::env::temp_dir().join(format!("manguebyte-mp3-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("tone.mp3");
    let buffer = common::ramp(1, 4_000);
    mp3::write_file(&path, &buffer, 32_000, &Mp3Options::constant(48)).unwrap();
    let file = mp3::read_file(&path).unwrap();
    assert_eq!(file.buffer.num_frames(), 4_000);
    let mut writer = AudioWriter::create_mp3(&path, 1, 32_000, &Mp3Options::constant(48)).unwrap();
    writer.write(&buffer).unwrap();
    writer.finish().unwrap();
    assert_eq!(mp3::read_file(&path).unwrap().buffer, file.buffer);
    assert!(AudioWriter::create_mp3(&path, 3, 32_000, &Mp3Options::constant(48)).is_err());
    assert_eq!(mp3::read_file(&path).unwrap().buffer, file.buffer);
    std::fs::remove_dir_all(&directory).unwrap();
}

/// Encodes the test sources at constant and variable bitrates and prints
/// what comes out, with the speed; with `MANGUEBYTE_DUMP_DIR` set, writes
/// the files there too.
#[test]
#[ignore = "prints measurements"]
fn measure_the_bitrates() {
    let dump = std::env::var("MANGUEBYTE_DUMP_DIR").ok();
    let settings = [
        ("cbr64", Mp3Options::constant(64)),
        ("cbr128", Mp3Options::constant(128)),
        ("cbr192", Mp3Options::constant(192)),
        ("cbr320", Mp3Options::constant(320)),
        ("vbr0", Mp3Options::variable(0.0)),
        ("vbr3", Mp3Options::variable(3.0)),
        ("vbr5", Mp3Options::variable(5.0)),
        ("vbr8", Mp3Options::variable(8.0)),
        ("vbr10", Mp3Options::variable(10.0)),
    ];
    for name in [
        "signal-2ch-44100",
        "tones-2ch-48000",
        "signal-1ch-48000",
        "signal-2ch-32000",
    ] {
        let (buffer, rate) = source(name);
        for (label, options) in &settings {
            let started = std::time::Instant::now();
            let bytes = encode(&buffer, rate, options);
            let elapsed = started.elapsed().as_secs_f64();
            let seconds = buffer.num_frames() as f64 / f64::from(rate);
            let kbps = bytes.len() as f64 * 8.0 / seconds / 1_000.0;
            let decoded = mp3::read(bytes.as_slice()).unwrap();
            let frames = buffer.num_frames();
            assert_eq!(
                decoded.buffer.num_frames(),
                frames,
                "{name} {label}: length"
            );
            let (snr, worst) = closeness(&decoded.buffer, &buffer, 0..frames);
            let (_, tones) = fitted_closeness(&decoded.buffer, &buffer, 0..frames * 3 / 10);
            let masked = nmr(&decoded.buffer, &buffer, rate, 0..frames);
            println!(
                "{name} {label}: {kbps:.1} kbit/s, {snr:.1} dB ({tones:.1} dB on the tones), NMR {:.1} dB ({:.1}% audible), worst {worst:.3}, {:.0}x real time",
                masked.mean_db,
                100.0 * masked.audible,
                seconds / elapsed
            );
            if let Some(directory) = &dump {
                let path = std::path::Path::new(directory).join(format!("{name}-{label}.mp3"));
                std::fs::write(path, &bytes).unwrap();
            }
        }
    }
}

/// Writes the files of [`fixtures`] into `MANGUEBYTE_DUMP_DIR`, for FFmpeg
/// to decode: see `data/README.md`.
#[test]
#[ignore = "writes files for other software"]
fn dump_files_for_other_software() {
    let directory = std::env::var("MANGUEBYTE_DUMP_DIR").expect("MANGUEBYTE_DUMP_DIR");
    for (name, source_name, options) in fixtures() {
        let (buffer, rate) = source(source_name);
        let bytes = encode(&buffer, rate, &options);
        std::fs::write(
            std::path::Path::new(&directory).join(format!("{name}.mp3")),
            bytes,
        )
        .unwrap();
    }
}
