//! The Ogg Opus encoder, through this crate's decoder.
//!
//! Neither FFmpeg nor opus-tools is on the machine these tests were
//! written on, so no file the encoder writes has been decoded by libopus
//! yet. What holds the encoder to the format instead: every frame is read
//! back by this crate's decoder, which decodes the files libopus writes to
//! within the rounding of libopus's own decode (`tests/opus.rs`), and a
//! unit test (`src/opus/celt/encode.rs`) checks that, frame after frame,
//! the decoder ends each frame with the range and the band energies the
//! encoder meant, which it cannot unless it read every symbol as it was
//! written. How near the audio comes to its source is measured with a
//! noise-to-mask ratio of a model of its own (`common/nmr.rs`), apart from
//! the encoder's, and with the signal-to-noise ratio.

mod common;

use std::io::Cursor;

use common::lossy::{
    check_seeks_are_exact, closeness, fitted_closeness, read_in_random_blocks, source,
};
use common::nmr::nmr;
use common::{Failing, Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::metadata::{Metadata, Picture};
use manguebyte_formats::opus::{self, FrameDuration, OpusError, OpusOptions};
use manguebyte_formats::{AudioWriter, FileFormat, FormatError, decode};

/// Encodes `buffer` whole.
fn encode(buffer: &AudioBuffer<f32>, rate: u32, options: &OpusOptions) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    opus::write(&mut bytes, buffer, rate, options).unwrap();
    bytes.into_inner()
}

/// The length a stream of `frames` frames at `rate` decodes to at 48 kHz.
fn length_at_48k(frames: usize, rate: u32) -> usize {
    (frames as f64 * 48_000.0 / f64::from(rate)).round() as usize
}

#[test]
fn every_length_comes_back_exactly() {
    for channels in [1usize, 2, 3, 6] {
        for (rate, frames) in [
            (48_000, 0usize),
            (48_000, 1),
            (48_000, 119),
            (48_000, 120),
            (48_000, 121),
            (48_000, 839),
            (48_000, 840),
            (48_000, 841),
            (48_000, 960),
            (48_000, 4_321),
            (44_100, 1),
            (44_100, 2_049),
            (22_050, 3_333),
            (8_000, 777),
            (96_000, 12_345),
            (11_025, 1_000),
        ] {
            let buffer = common::ramp(channels, frames);
            for options in [
                OpusOptions::variable(64 * channels as u32),
                OpusOptions::constant(48 * channels as u32),
            ] {
                let bytes = encode(&buffer, rate, &options);
                let file = opus::read(bytes.as_slice()).unwrap();
                assert_eq!(
                    file.buffer.num_frames(),
                    length_at_48k(frames, rate),
                    "{channels} channels, {frames} frames at {rate} Hz"
                );
                assert_eq!(file.buffer.num_channels(), channels);
                assert_eq!(file.input_rate, rate);
            }
        }
    }
}

#[test]
fn every_frame_duration_comes_back_in_time() {
    let (buffer, rate) = source("signal-1ch-48000");
    let frames = buffer.num_frames();
    for (duration, at_least) in [
        (FrameDuration::Ms2_5, 9.0),
        (FrameDuration::Ms5, 10.0),
        (FrameDuration::Ms10, 11.0),
        (FrameDuration::Ms20, 12.0),
    ] {
        let options = OpusOptions::constant(96).with_frame_duration(duration);
        let bytes = encode(&buffer, rate, &options);
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        assert_eq!(decoded.num_frames(), frames, "{duration:?}");
        let (snr, _) = closeness(&decoded, &buffer, 0..frames);
        println!("{duration:?}: {snr:.1} dB");
        assert!(snr >= at_least, "{duration:?}: {snr:.1} dB");
    }
}

/// The bitrate sets the noise: more bits, less noise against the masking
/// threshold, and a constant bitrate comes out at the rate asked, the
/// pages' own few bytes over it.
#[test]
fn the_bitrate_moves_the_noise_and_the_size_together() {
    let (buffer, rate) = source("signal-2ch-48000");
    let frames = buffer.num_frames();
    let seconds = frames as f64 / f64::from(rate);
    let mut last: Option<(f64, f64)> = None;
    for (kbps, nmr_at_most, tones_at_least) in [
        (64u32, 1.0, 28.0),
        (96, -3.0, 32.0),
        (128, -6.5, 37.0),
        (192, -10.5, 42.0),
        (256, -13.5, 43.0),
    ] {
        let bytes = encode(&buffer, rate, &OpusOptions::constant(kbps));
        let measured = bytes.len() as f64 * 8.0 / seconds / 1_000.0;
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        let masked = nmr(&decoded, &buffer, rate, 0..frames);
        let (snr, _) = closeness(&decoded, &buffer, 0..frames);
        let (_, tones) = fitted_closeness(&decoded, &buffer, 0..frames * 3 / 10);
        println!(
            "{kbps} kbit/s: {measured:.1} kbit/s, noise-to-mask {:.1} dB ({:.1} % of bands above the mask), {snr:.1} dB, {tones:.1} dB on the tones",
            masked.mean_db,
            100.0 * masked.audible
        );
        assert!(
            (f64::from(kbps)..f64::from(kbps) * 1.05).contains(&measured),
            "{kbps}: {measured:.1} kbit/s"
        );
        assert!(
            masked.mean_db <= nmr_at_most,
            "{kbps}: {:.1} dB",
            masked.mean_db
        );
        assert!(tones >= tones_at_least, "{kbps}: {tones:.1} dB");
        if let Some((last_kbps, last_nmr)) = last {
            assert!(measured > last_kbps && masked.mean_db < last_nmr, "{kbps}");
        }
        last = Some((measured, masked.mean_db));
    }
}

/// A variable bitrate spends what each frame needs: the frames of short
/// blocks more, those of audio that stops short of the top bands fewer,
/// silence almost nothing. On the test signal, whose tones are narrow and
/// a tenth of which is silence, it comes out under the rate asked, and
/// quieter for the bits than a constant one.
#[test]
fn a_variable_bitrate_spends_less_on_simpler_audio() {
    let (buffer, rate) = source("signal-2ch-48000");
    let frames = buffer.num_frames();
    let seconds = frames as f64 / f64::from(rate);
    let measure = |options: &OpusOptions| {
        let bytes = encode(&buffer, rate, options);
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        assert_eq!(decoded.num_frames(), frames);
        let kbps = bytes.len() as f64 * 8.0 / seconds / 1_000.0;
        (kbps, nmr(&decoded, &buffer, rate, 0..frames).mean_db)
    };
    let (variable_kbps, variable_nmr) = measure(&OpusOptions::variable(128));
    let (constant_kbps, constant_nmr) = measure(&OpusOptions::constant(96));
    println!(
        "variable 128: {variable_kbps:.1} kbit/s, {variable_nmr:.1} dB; constant 96: {constant_kbps:.1} kbit/s, {constant_nmr:.1} dB"
    );
    assert!((75.0..115.0).contains(&variable_kbps), "{variable_kbps:.1}");
    assert!(variable_kbps < constant_kbps && variable_nmr < constant_nmr - 2.0);
    // Silence costs two bytes a frame and a byte of framing.
    // Silence costs two bytes a frame, a byte of framing and a byte of
    // lacing: a second of it takes about 200 bytes past the headers.
    let silence = AudioBuffer::<f32>::new(2, 48_000);
    let headers = encode(&AudioBuffer::new(2, 0), 48_000, &OpusOptions::variable(128)).len();
    let bytes = encode(&silence, 48_000, &OpusOptions::variable(128));
    assert!(
        bytes.len() - headers < 4 * 51,
        "{} past {headers}",
        bytes.len()
    );
}

/// A steady chord comes back close, from 64 kbit/s on as close as the caps
/// on each band's bits allow (RFC 6716, section 4.3.3).
#[test]
fn tones_come_back_close() {
    let (buffer, rate) = source("tones-2ch-48000");
    let frames = buffer.num_frames();
    for (kbps, at_least) in [(64u32, 44.0), (128, 44.0), (256, 44.0)] {
        let bytes = encode(&buffer, rate, &OpusOptions::constant(kbps));
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        let (snr, _) = closeness(&decoded, &buffer, 2_000..frames - 2_000);
        println!("{kbps} kbit/s: {snr:.1} dB");
        assert!(snr >= at_least, "{kbps}: {snr:.1} dB");
    }
}

#[test]
fn every_way_of_writing_gives_the_same_bytes_however_the_audio_is_cut() {
    for (name, options) in [
        (
            "signal-2ch-24000",
            OpusOptions::variable(96).with_tag("ARTIST", "Chico Science"),
        ),
        (
            "signal-2ch-48000",
            OpusOptions::constant(64).with_frame_duration(FrameDuration::Ms5),
        ),
    ] {
        let (buffer, rate) = source(name);
        let expected = encode(&buffer, rate, &options);
        // Twice the same: the encoder is deterministic.
        assert!(encode(&buffer, rate, &options) == expected);
        let frames = buffer.num_frames();
        for seed in 1..3u64 {
            let mut rng = Rng::new(seed);
            let mut writer = AudioWriter::opus(Cursor::new(Vec::new()), 2, rate, &options).unwrap();
            assert_eq!(writer.format(), FileFormat::OggOpus);
            let mut done = 0;
            while done < frames {
                let count = (rng.between(0, 3_000) as usize).min(frames - done);
                let piece = slice(&buffer, done..done + count);
                match rng.below(3) {
                    0 => writer.write(&piece).unwrap(),
                    1 => {
                        let mut interleaved = Vec::with_capacity(count * 2);
                        for i in 0..count {
                            interleaved.push(piece.channel(0)[i]);
                            interleaved.push(piece.channel(1)[i]);
                        }
                        writer.write_interleaved(&interleaved).unwrap();
                    }
                    _ if piece
                        .iter_channels()
                        .all(|channel| channel.iter().all(|&s| s == 0.0)) =>
                    {
                        writer.write_silence(count as u64).unwrap();
                    }
                    _ => writer.write(&piece).unwrap(),
                }
                done += count;
                assert_eq!(writer.frames_written(), done as u64);
            }
            let bytes = writer.finish().unwrap().into_inner();
            assert!(bytes == expected, "{name}, seed {seed}");
        }
    }
    // Silence written as silence is the same as zeros.
    let options = OpusOptions::default();
    let zeros = AudioBuffer::<f32>::new(2, 10_000);
    let mut writer = AudioWriter::opus(Cursor::new(Vec::new()), 2, 44_100, &options).unwrap();
    writer.write_silence(4_000).unwrap();
    writer.write(&slice(&zeros, 0..6_000)).unwrap();
    assert!(writer.finish().unwrap().into_inner() == encode(&zeros, 44_100, &options));
}

#[test]
fn what_is_written_reads_back_through_every_reader() {
    let (buffer, rate) = source("signal-1ch-48000");
    let bytes = encode(&buffer, rate, &OpusOptions::variable(64));
    let whole = decode(&bytes).unwrap();
    assert_eq!(whole.format, FileFormat::OggOpus);
    assert_eq!(whole.sample_rate, 48_000);
    assert_eq!(whole.buffer.num_frames(), buffer.num_frames());
    assert_eq!(read_in_random_blocks(&bytes, 7), whole.buffer);
    check_seeks_are_exact("encoder", &bytes, &whole.buffer, 40, 3);
}

/// Each channel comes back as itself, in the order WAV and FLAC use, for
/// one to eight channels in the layouts of mapping family 1 (coupled pairs
/// and channels alone) and beyond, one stream a channel (family 255).
#[test]
fn every_channel_comes_back_in_its_place() {
    for channels in [1usize, 2, 3, 4, 5, 6, 7, 8, 11] {
        let frames = 9_600;
        let mut buffer = AudioBuffer::<f32>::new(channels, frames);
        for (channel, samples) in buffer.iter_channels_mut().enumerate() {
            let hz = 300.0 + 170.0 * channel as f32;
            for (i, sample) in samples.iter_mut().enumerate() {
                *sample = 0.3 * (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin();
            }
        }
        let bytes = encode(
            &buffer,
            48_000,
            &OpusOptions::constant(64 * channels as u32),
        );
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        assert_eq!(decoded.num_channels(), channels);
        assert_eq!(decoded.num_frames(), frames);
        for got in 0..channels {
            for want in 0..channels {
                let (_, snr) = fitted_closeness(
                    &channel_of(&decoded, got),
                    &channel_of(&buffer, want),
                    1_000..frames - 1_000,
                );
                if got == want {
                    assert!(
                        snr > 20.0,
                        "{channels} channels: {got} is {snr:.1} dB from itself"
                    );
                } else {
                    assert!(snr < 1.0, "{channels} channels: {got} looks like {want}");
                }
            }
        }
    }
}

#[test]
fn comments_and_pictures_come_back() {
    let metadata = Metadata {
        title: Some("Manguetown".to_owned()),
        artist: Some("Nação Zumbi".to_owned()),
        pictures: vec![Picture {
            kind: 3,
            mime: "image/png".to_owned(),
            description: "capa".to_owned(),
            width: 1,
            height: 1,
            depth: 24,
            colors: 0,
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
        }],
        ..Metadata::default()
    };
    let options = OpusOptions::variable(64)
        .with_metadata(&metadata)
        .with_tag("comment", "a=b");
    let buffer = common::ramp(2, 3_000);
    let bytes = encode(&buffer, 32_000, &options);
    let file = opus::read(bytes.as_slice()).unwrap();
    assert!(file.vendor.starts_with("manguebyte-formats "));
    assert_eq!(file.metadata.title, metadata.title);
    assert_eq!(file.metadata.artist, metadata.artist);
    assert_eq!(file.metadata.pictures, metadata.pictures);
    assert!(
        file.comments
            .contains(&("comment".to_owned(), "a=b".to_owned()))
    );
    // Through the generic reader too.
    assert_eq!(decode(&bytes).unwrap().metadata.title, metadata.title);
    // A writer takes them in its options, not afterwards.
    let mut writer = AudioWriter::opus(Cursor::new(Vec::new()), 2, 32_000, &options).unwrap();
    assert!(matches!(
        writer.set_metadata(&metadata),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn options_are_checked_before_anything_is_written() {
    let buffer = AudioBuffer::<f32>::new(1, 100);
    for (options, channels, rate) in [
        (OpusOptions::constant(5), 1usize, 48_000u32),
        (OpusOptions::variable(511), 1, 48_000),
        (OpusOptions::constant(64), 2, 48_000),
        (OpusOptions::constant(64).with_tag("A=B", "c"), 1, 48_000),
        (OpusOptions::constant(64).with_tag("", "c"), 1, 48_000),
        (OpusOptions::constant(64).with_tag("TÍTULO", "c"), 1, 48_000),
        (OpusOptions::constant(64), 0, 48_000),
        (OpusOptions::constant(64), 256, 48_000),
        (OpusOptions::constant(64), 1, 0),
    ] {
        if channels == 2 {
            // 32 kbit/s a channel is fine.
            assert!(options.validate(channels, rate).is_ok());
            continue;
        }
        let mut out = Cursor::new(Vec::new());
        let result = AudioWriter::opus(&mut out, channels, rate, &options);
        assert!(
            matches!(result, Err(FormatError::Opus(OpusError::Unsupported(_)))),
            "{options:?} {channels} {rate}"
        );
        assert!(out.into_inner().is_empty());
        if channels == 1 && rate > 0 {
            let mut out = Cursor::new(Vec::new());
            assert!(matches!(
                opus::write(&mut out, &buffer, rate, &options),
                Err(OpusError::Unsupported(_))
            ));
        }
    }
}

#[test]
fn hostile_samples_are_written_and_read_back_finite() {
    let frames = 20_000;
    let mut buffer = AudioBuffer::<f32>::new(2, frames);
    let mut rng = Rng::new(99);
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
    for options in [OpusOptions::constant(12), OpusOptions::variable(1_000)] {
        let bytes = encode(&buffer, 44_100, &options);
        let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
        assert_eq!(decoded.num_frames(), length_at_48k(frames, 44_100));
        assert!(
            decoded
                .iter_channels()
                .all(|channel| channel.iter().all(|s| s.is_finite()))
        );
    }
    // Square waves and clicks at full scale, at the extremes of rate and
    // channels.
    for (channels, rate, frames) in [
        (1usize, 1u32, 2usize),
        (8, 192_000, 3_000),
        (255, 8_000, 3_000),
    ] {
        let mut buffer = AudioBuffer::<f32>::new(channels, frames);
        for (index, channel) in buffer.iter_channels_mut().enumerate() {
            for (i, sample) in channel.iter_mut().enumerate() {
                *sample = if (i / (index + 3)) % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
            }
        }
        let bytes = encode(&buffer, rate, &OpusOptions::constant(16 * channels as u32));
        let file = opus::read(bytes.as_slice()).unwrap();
        assert_eq!(
            (file.buffer.num_channels(), file.buffer.num_frames()),
            (channels, length_at_48k(frames, rate))
        );
    }
}

#[test]
fn a_page_that_could_not_be_written_is_written_again() {
    let (buffer, rate) = source("signal-2ch-32000");
    let options = OpusOptions::default();
    let expected = encode(&buffer, rate, &options);
    let disk = Failing::new(Cursor::new(Vec::new()), 3_000);
    let budget = std::sync::Arc::clone(&disk.budget);
    let mut writer = AudioWriter::opus(disk, 2, rate, &options).unwrap();
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
                // Room for the pages kept, which are written again whole.
                budget.fetch_add(5_000, std::sync::atomic::Ordering::Relaxed);
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
    let directory = std::env::temp_dir().join(format!("manguebyte-opus-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("tone.opus");
    let buffer = common::ramp(1, 4_000);
    opus::write_file(&path, &buffer, 16_000, &OpusOptions::constant(32)).unwrap();
    let file = opus::read_file(&path).unwrap();
    assert_eq!(file.buffer.num_frames(), 12_000);
    assert_eq!(file.input_rate, 16_000);
    let mut writer =
        AudioWriter::create_opus(&path, 1, 16_000, &OpusOptions::constant(32)).unwrap();
    writer.write(&buffer).unwrap();
    writer.finish().unwrap();
    assert_eq!(opus::read_file(&path).unwrap().buffer, file.buffer);
    // Options refused leave the file as it was.
    assert!(AudioWriter::create_opus(&path, 0, 16_000, &OpusOptions::constant(32)).is_err());
    assert_eq!(opus::read_file(&path).unwrap().buffer, file.buffer);
    std::fs::remove_dir_all(&directory).unwrap();
}

/// How fast the encoder runs against real time, printed: an unoptimised
/// build is many times slower than a release build.
#[test]
#[ignore = "prints measurements"]
fn measure_the_speed() {
    for (name, kbps) in [
        ("signal-2ch-48000", 128u32),
        ("signal-2ch-44100", 128),
        ("signal-6ch-48000", 384),
    ] {
        let (buffer, rate) = source(name);
        let seconds = buffer.num_frames() as f64 / f64::from(rate);
        let options = OpusOptions::constant(kbps);
        let started = std::time::Instant::now();
        let rounds = 5;
        for _ in 0..rounds {
            std::hint::black_box(encode(&buffer, rate, &options));
        }
        let elapsed = started.elapsed().as_secs_f64() / f64::from(rounds);
        println!(
            "{name} at {kbps} kbit/s: {:.0}x real time",
            seconds / elapsed
        );
    }
}

/// One channel of `buffer`.
fn channel_of(buffer: &AudioBuffer<f32>, channel: usize) -> AudioBuffer<f32> {
    let mut out = AudioBuffer::new(1, buffer.num_frames());
    out.channel_mut(0).copy_from_slice(buffer.channel(channel));
    out
}

#[test]
#[ignore = "prints measurements"]
fn measure_the_channels_of_a_surround_stream() {
    let (buffer, rate) = source("signal-6ch-48000");
    let frames = buffer.num_frames();
    let bytes = encode(&buffer, rate, &OpusOptions::constant(384));
    let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
    for channel in 0..6 {
        let (got, want) = (channel_of(&decoded, channel), channel_of(&buffer, channel));
        let (snr, worst) = closeness(&got, &want, 0..frames);
        let masked = nmr(&got, &want, rate, 0..frames);
        println!(
            "channel {channel}: {snr:.1} dB, NMR {:.1} dB, worst {worst:.3}",
            masked.mean_db
        );
    }
    let (buffer, rate) = source("signal-1ch-48000");
    let bytes = encode(&buffer, rate, &OpusOptions::constant(24));
    let decoded = opus::read(bytes.as_slice()).unwrap().buffer;
    let (mut worst, mut at) = (0.0f32, 0);
    for (i, (&got, &want)) in decoded.channel(0).iter().zip(buffer.channel(0)).enumerate() {
        if (got - want).abs() > worst {
            worst = (got - want).abs();
            at = i;
        }
    }
    println!(
        "mono at 24 kbit/s: worst {worst:.3} at {at} ({:.3} of the way), {:?} against {:?}",
        at as f64 / buffer.num_frames() as f64,
        &decoded.channel(0)[at - 3..at + 3],
        &buffer.channel(0)[at - 3..at + 3]
    );
}

/// Encodes the test sources at several bitrates and prints what comes out,
/// with the speed; with `MANGUEBYTE_DUMP_DIR` set, writes the files there
/// too.
#[test]
#[ignore = "prints measurements"]
fn measure_the_bitrates() {
    let dump = std::env::var("MANGUEBYTE_DUMP_DIR").ok();
    for name in [
        "signal-2ch-48000",
        "tones-2ch-48000",
        "signal-1ch-48000",
        "signal-6ch-48000",
        "signal-2ch-44100",
    ] {
        let (buffer, rate) = source(name);
        let channels = buffer.num_channels() as u32;
        for (kbps, variable) in [
            (24 * channels, false),
            (32 * channels, false),
            (48 * channels, false),
            (64 * channels, false),
            (96 * channels, false),
            (128 * channels, false),
            (32 * channels, true),
            (48 * channels, true),
            (64 * channels, true),
            (96 * channels, true),
        ] {
            let options = if variable {
                OpusOptions::variable(kbps)
            } else {
                OpusOptions::constant(kbps)
            };
            let started = std::time::Instant::now();
            let bytes = encode(&buffer, rate, &options);
            let elapsed = started.elapsed().as_secs_f64();
            let seconds = buffer.num_frames() as f64 / f64::from(rate);
            let measured = bytes.len() as f64 * 8.0 / seconds / 1_000.0;
            let decoded = opus::read(bytes.as_slice()).unwrap();
            let kind = if variable { "vbr" } else { "cbr" };
            if rate == 48_000 {
                let frames = buffer.num_frames();
                let (snr, worst) = closeness(&decoded.buffer, &buffer, 0..frames);
                let (_, tones) = fitted_closeness(&decoded.buffer, &buffer, 0..frames * 3 / 10);
                let masked = nmr(&decoded.buffer, &buffer, rate, 0..frames);
                // By stretch: tones, sweep, noise, clicks.
                let parts: Vec<String> = [(0.0, 0.3), (0.3, 0.5), (0.6, 0.8), (0.8, 1.0)]
                    .iter()
                    .map(|&(from, to)| {
                        let range = (from * frames as f64) as usize..(to * frames as f64) as usize;
                        format!("{:.1}", nmr(&decoded.buffer, &buffer, rate, range).mean_db)
                    })
                    .collect();
                println!(
                    "{name} {kind} {kbps}: {measured:.1} kbit/s, {snr:.1} dB ({tones:.1} dB on the tones), NMR {:.1} dB ({:.1}% audible; by stretch {}), worst {worst:.3}, {:.0}x real time",
                    masked.mean_db,
                    100.0 * masked.audible,
                    parts.join(" "),
                    seconds / elapsed
                );
            } else {
                println!(
                    "{name} {kind} {kbps}: {measured:.1} kbit/s, {:.0}x real time",
                    seconds / elapsed
                );
            }
            if let Some(directory) = &dump {
                let path =
                    std::path::Path::new(directory).join(format!("{name}-{kind}{kbps}.opus"));
                std::fs::write(path, &bytes).unwrap();
            }
        }
    }
}
