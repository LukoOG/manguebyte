//! Thumbnails against brute force: the minimum, maximum and RMS level of
//! every range asked for are those of the samples of the buckets it
//! touches, worked out sample by sample; and what the cache keeps still
//! holds the audio between its levels.

mod common;

use std::io::Cursor;
use std::time::Instant;

use common::Rng;
use manguebyte_core::AudioBuffer;
use manguebyte_formats::AudioReader;
use manguebyte_formats::thumbnail::{Peak, Thumbnail, ThumbnailBuilder, ThumbnailError};
use manguebyte_formats::wav::{self, WavFormat};

/// Audio that changes its character from stretch to stretch: noise of
/// varying loudness, a tone, silence, and spikes, different on each
/// channel.
fn audio(channels: usize, frames: usize, seed: u64) -> AudioBuffer<f32> {
    let mut rng = Rng::new(seed);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (channel, samples) in buffer.iter_channels_mut().enumerate() {
        let mut level = 0.5f32;
        for (frame, sample) in samples.iter_mut().enumerate() {
            if frame % 997 == 0 {
                level = (rng.below(1_000) as f32 / 1_000.0).powi(2);
            }
            let noise = (rng.below(2_001) as f32 - 1_000.0) / 1_000.0;
            *sample = match (frame / 5_000 + channel) % 4 {
                0 => level * noise,
                1 => 0.7 * (frame as f32 * 0.013 * (channel + 1) as f32).sin(),
                2 => 0.0,
                _ if rng.below(500) == 0 => (rng.below(4_001) as f32 - 2_000.0) / 1_000.0,
                _ => 0.01 * noise,
            };
        }
    }
    buffer
}

/// The summary of the buckets `frames` touches, sample by sample.
fn brute_force(
    audio: &AudioBuffer<f32>,
    channel: usize,
    frames: std::ops::Range<u64>,
    bucket: u64,
) -> Option<Peak> {
    let len = audio.num_frames() as u64;
    let end = frames.end.min(len);
    if frames.start >= end {
        return None;
    }
    let from = (frames.start / bucket * bucket) as usize;
    let to = (end.div_ceil(bucket) * bucket).min(len) as usize;
    let samples = &audio.channel(channel)[from..to];
    let min = samples.iter().copied().fold(f32::INFINITY, f32::min);
    let max = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    Some(Peak {
        min,
        max,
        rms: (energy / samples.len() as f64).sqrt() as f32,
    })
}

fn assert_same(got: Option<Peak>, want: Option<Peak>, what: &str) {
    match (got, want) {
        (None, None) => {}
        (Some(got), Some(want)) => {
            // Exactly, but for the sign of a zero: the minimum of 0.0 and
            // -0.0 may be either.
            assert!(
                got.min == want.min,
                "{what}: min {} against {}",
                got.min,
                want.min
            );
            assert!(
                got.max == want.max,
                "{what}: max {} against {}",
                got.max,
                want.max
            );
            let error = (got.rms - want.rms).abs();
            assert!(
                error <= 2e-6 * want.rms.max(1e-3),
                "{what}: rms {} against {}",
                got.rms,
                want.rms
            );
        }
        _ => panic!("{what}: {got:?} against {want:?}"),
    }
}

/// A thumbnail of `audio` built from blocks of random sizes.
fn built(audio: &AudioBuffer<f32>, bucket: u32, seed: u64) -> Thumbnail {
    let mut thumbnail = Thumbnail::new(audio.num_channels(), 48_000, bucket);
    let mut rng = Rng::new(seed);
    let mut at = 0;
    while at < audio.num_frames() {
        let size = (rng.between(0, 3_000) as usize).min(audio.num_frames() - at);
        let mut block = AudioBuffer::new(audio.num_channels(), size);
        for (to, from) in block.iter_channels_mut().zip(audio.iter_channels()) {
            to.copy_from_slice(&from[at..at + size]);
        }
        if rng.below(2) == 0 {
            thumbnail.push(&block);
        } else {
            let mut interleaved = Vec::with_capacity(size * audio.num_channels());
            for frame in 0..size {
                for channel in block.iter_channels() {
                    interleaved.push(channel[frame]);
                }
            }
            thumbnail.push_interleaved(&interleaved);
        }
        at += size;
    }
    thumbnail
}

#[test]
fn every_range_is_what_brute_force_gives() {
    for (channels, frames, bucket) in [
        (2, 100_003, 256),
        (1, 70_000, 64),
        (3, 5_000, 1_000),
        (1, 1, 7),
    ] {
        let audio = audio(channels, frames, frames as u64);
        let thumbnail = built(&audio, bucket, 3);
        assert_eq!(thumbnail.len_frames(), frames as u64);
        let mut rng = Rng::new(41);
        let len = frames as u64;
        for round in 0..1_000 {
            let start = rng.below(len + 10);
            let end = match round % 4 {
                // Short ranges, long ones, the whole, and past the end.
                0 => start + rng.below(300),
                1 => start + rng.below(len),
                2 => len,
                _ => len + rng.below(1_000),
            };
            let (start, end) = if round % 4 == 2 {
                (0, end)
            } else {
                (start, end)
            };
            for channel in 0..channels {
                let what = format!("{channels} channels, {start}..{end}, channel {channel}");
                assert_same(
                    thumbnail.range(channel, start..end),
                    brute_force(&audio, channel, start..end, u64::from(bucket)),
                    &what,
                );
            }
        }
    }
}

#[test]
fn how_the_audio_is_cut_into_blocks_does_not_matter() {
    let audio = audio(2, 123_457, 9);
    let mut caches = Vec::new();
    for seed in 0..4 {
        let mut cache = Vec::new();
        built(&audio, 128, seed).write_cache(&mut cache, 0).unwrap();
        caches.push(cache);
    }
    assert!(caches.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn columns_cover_a_view_at_any_zoom() {
    let audio = audio(1, 100_000, 77);
    let thumbnail = built(&audio, 128, 1);
    for (start, end, width) in [
        (0u64, 100_000u64, 640usize),
        (0, 100_000, 1),
        (10_000, 10_100, 400),
        (99_000, 120_000, 300),
        (12_345, 67_890, 1_920),
    ] {
        let mut columns = vec![None; width];
        thumbnail.columns(0, start..end, &mut columns);
        let len = end - start;
        for (index, column) in columns.iter().enumerate() {
            let from = start + len * index as u64 / width as u64;
            let to = (start + len * (index as u64 + 1) / width as u64).max(from + 1);
            assert_same(
                *column,
                brute_force(&audio, 0, from..to, 128),
                &format!("{start}..{end} in {width}, column {index}"),
            );
        }
    }
}

#[test]
fn a_file_is_read_whole_or_on_a_thread_while_it_is_drawn() {
    let audio = audio(2, 300_001, 5);
    let mut bytes = Vec::new();
    wav::write(&mut bytes, &audio, 48_000, WavFormat::Float32).unwrap();
    let mut reader = AudioReader::new(Cursor::new(bytes.clone())).unwrap();
    let whole = Thumbnail::from_reader(&mut reader, 512).unwrap();
    assert_eq!(whole.len_frames(), 300_001);
    assert_eq!(whole.sample_rate(), 48_000);
    assert_same(
        whole.range(1, 0..300_001),
        brute_force(&audio, 1, 0..300_001, 512),
        "whole",
    );

    let builder = ThumbnailBuilder::spawn(AudioReader::new(Cursor::new(bytes)).unwrap(), 512);
    assert_eq!(builder.frames_total(), Some(300_001));
    // What can be drawn while it grows is what has been read.
    let mut seen = Vec::new();
    while !builder.is_finished() {
        let grown = builder.with(|thumbnail| {
            let len = thumbnail.len_frames();
            let mut columns = [None; 50];
            thumbnail.columns(0, 0..len.max(1), &mut columns);
            len
        });
        seen.push(grown);
        std::thread::yield_now();
    }
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(builder.frames_done(), 300_001);
    let built = builder.join().unwrap();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    built.write_cache(&mut a, 1).unwrap();
    whole.write_cache(&mut b, 1).unwrap();
    assert_eq!(a, b);

    // Cancelled, it stops early and gives what it has.
    let long = audio_wav(4_000_000);
    let builder = ThumbnailBuilder::spawn(AudioReader::new(Cursor::new(long)).unwrap(), 512);
    builder.cancel();
    let part = builder.join().unwrap();
    assert!(part.len_frames() < 4_000_000);
}

fn audio_wav(frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    wav::write(
        &mut bytes,
        &AudioBuffer::new(1, frames),
        48_000,
        WavFormat::Int16,
    )
    .unwrap();
    bytes
}

#[test]
fn the_cache_keeps_the_audio_between_its_levels() {
    let mut audio = audio(2, 150_001, 13);
    // Beyond full scale, which float files can hold.
    audio.channel_mut(1)[70_000] = 3.5;
    let thumbnail = built(&audio, 256, 2);
    let mut cache = Vec::new();
    thumbnail.write_cache(&mut cache, 0xFEED).unwrap();
    let buckets = 150_001usize.div_ceil(256);
    assert_eq!(cache.len(), 36 + buckets * 2 * 6 + 8);

    let read = Thumbnail::read_cache(cache.as_slice(), Some(0xFEED)).unwrap();
    assert_eq!(read.len_frames(), 150_001);
    assert_eq!(read.num_channels(), 2);
    assert_eq!(read.frames_per_bucket(), 256);
    // A step of the 16 bits the levels are kept to.
    let step = 3.5 / 32_767.0;
    let mut rng = Rng::new(8);
    for _ in 0..2_000 {
        let start = rng.below(150_001);
        let end = start + rng.below(20_000);
        for channel in 0..2 {
            let exact = thumbnail.range(channel, start..end).unwrap();
            let cached = read.range(channel, start..end).unwrap();
            assert!(cached.min <= exact.min && exact.min - cached.min <= step * 1.01);
            assert!(cached.max >= exact.max && cached.max - exact.max <= step * 1.01);
            assert!((cached.rms - exact.rms).abs() <= 3.5 / 65_535.0 + exact.rms * 1e-5);
        }
    }

    // Made from other audio, or not a cache at all.
    assert!(matches!(
        Thumbnail::read_cache(cache.as_slice(), Some(1)),
        Err(ThumbnailError::Stale)
    ));
    assert!(matches!(
        Thumbnail::read_cache(&b"RIFF and more"[..], None),
        Err(ThumbnailError::NotACache)
    ));
}

#[test]
fn a_damaged_or_hostile_cache_is_an_error() {
    let audio = audio(1, 3_001, 4);
    let mut cache = Vec::new();
    built(&audio, 100, 1).write_cache(&mut cache, 7).unwrap();
    assert!(Thumbnail::read_cache(cache.as_slice(), Some(7)).is_ok());
    for len in 0..cache.len() {
        assert!(
            Thumbnail::read_cache(&cache[..len], None).is_err(),
            "cut at {len}"
        );
    }
    let mut rng = Rng::new(3);
    for _ in 0..500 {
        let mut damaged = cache.clone();
        let at = rng.below(cache.len() as u64) as usize;
        damaged[at] ^= 1 << rng.below(8);
        assert!(
            Thumbnail::read_cache(damaged.as_slice(), None).is_err(),
            "byte {at}"
        );
    }
    // A header that claims 2^64 - 1 frames of 65535 channels is refused
    // without the memory it would take being asked for.
    let mut hostile = cache[..36].to_vec();
    hostile[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
    hostile[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        Thumbnail::read_cache(hostile.as_slice(), None),
        Err(ThumbnailError::Corrupt(_))
    ));
}

#[test]
fn queries_are_fast_on_a_long_file() {
    // Five minutes of mono at 48 kHz, in buckets of 256 frames: eight
    // levels. A file sixteen times longer adds two.
    let frames = 48_000 * 60 * 5;
    let mut thumbnail = Thumbnail::new(1, 48_000, 256);
    let mut block = AudioBuffer::new(1, 48_000);
    for (index, sample) in block.channel_mut(0).iter_mut().enumerate() {
        *sample = (index as f32 * 0.001).sin();
    }
    let started = Instant::now();
    for _ in 0..frames / 48_000 {
        thumbnail.push(&block);
    }
    let building = started.elapsed();
    let mut rng = Rng::new(1);
    let queries = 20_000;
    let started = Instant::now();
    let mut sum = 0.0f32;
    for _ in 0..queries {
        let start = rng.below(frames);
        let end = start + rng.below(frames - start) + 1;
        sum += thumbnail.range(0, start..end).unwrap().max;
    }
    let querying = started.elapsed();
    let mut columns = vec![None; 1_920];
    let started = Instant::now();
    thumbnail.columns(0, 0..frames, &mut columns);
    let view = started.elapsed();
    println!(
        "{frames} frames added in {building:?}; {queries} ranges in {querying:?} ({:.2} us each); \
         a view of 1920 columns of the whole in {view:?}",
        querying.as_secs_f64() * 1e6 / f64::from(queries)
    );
    assert!(sum > 0.0);
    // Unoptimised and on a busy machine, still far below a frame of video.
    assert!(view.as_secs_f64() < 0.1, "{view:?}");
    assert!(querying.as_secs_f64() / f64::from(queries) < 1e-4);
}
