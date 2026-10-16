//! Decodes Ogg Vorbis files that libvorbis wrote and compares them with the
//! audio they were made from.
//!
//! The files in `data/` were encoded by libvorbis through FFmpeg 7.1 from
//! audio that `common::lossy` makes (see `data/README.md`). A lossy codec
//! does not give back what it was given, so what is checked is the length,
//! exactly, and how near the audio is, against bounds set a decibel or so
//! short of what was measured. The measurements are printed, so that
//! `--nocapture` shows how far each file is from its bounds.
//!
//! Seeking is held to more than nearness: what is read after a seek must
//! be, bit for bit, what a decode of the whole file gives at that place.

mod common;

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::lossy::{
    Bounds, check_seeks_are_exact, fix_page_checksums, poke, read_in_random_blocks,
    read_what_can_be_read, source,
};
use common::{Counting, Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::vorbis::{self, VorbisError};
use manguebyte_formats::{AudioReader, FileFormat, FormatError, decode};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// The files, with their bounds.
///
/// The whole-file ratios are low because of the stretch of white noise: a
/// perceptual coder keeps the spectrum of noise, not its waveform, and at
/// these rates gives back noise as loud but different, a ratio near 0 dB.
/// The tones show what the coding of a steady sound keeps. libvorbis gives
/// them back up to 1 % louder than they were, more at its highest quality,
/// which is why their ratio is taken once the level is matched. In the 5.1
/// file libvorbis codes the LFE channel only up to about 280 Hz, which
/// takes the upper partials out of that channel's tones.
fn files() -> Vec<Bounds> {
    vec![
        Bounds::new(
            "vorbis-2ch-44100-q3.ogg",
            data!("vorbis-2ch-44100-q3.ogg"),
            "signal-2ch-44100",
        )
        .snr(9.5, 32.5)
        .level(0.995, 1.015)
        .max_error(0.56),
        Bounds::new(
            "vorbis-1ch-22050-q0.ogg",
            data!("vorbis-1ch-22050-q0.ogg"),
            "signal-1ch-22050",
        )
        .snr(9.5, 24.5)
        .level(0.99, 1.015)
        .max_error(0.43),
        Bounds::new(
            "vorbis-6ch-48000-q2.ogg",
            data!("vorbis-6ch-48000-q2.ogg"),
            "signal-6ch-48000",
        )
        .snr(6.5, 13.0)
        .level(1.0, 1.05)
        .max_error(0.69),
        Bounds::new(
            "vorbis-1ch-8000-q5.ogg",
            data!("vorbis-1ch-8000-q5.ogg"),
            "signal-1ch-8000",
        )
        .snr(21.5, 32.0)
        .level(0.99, 1.01)
        .max_error(0.12),
        Bounds::new(
            "vorbis-2ch-96000-q4.ogg",
            data!("vorbis-2ch-96000-q4.ogg"),
            "signal-2ch-96000",
        )
        .snr(8.5, 31.0)
        .level(0.995, 1.015)
        .max_error(0.5),
        Bounds::new(
            "vorbis-tones-2ch-48000-q10.ogg",
            data!("vorbis-tones-2ch-48000-q10.ogg"),
            "tones-2ch-48000",
        )
        .snr(37.5, 44.0)
        .level(1.005, 1.015)
        .max_error(0.011),
        Bounds::new(
            "vorbis-2ch-44100-100frames.ogg",
            data!("vorbis-2ch-44100-100frames.ogg"),
            "tones-2ch-44100-short",
        )
        .snr(36.0, 36.5)
        .level(0.995, 1.015)
        .max_error(0.008),
        Bounds::new(
            "vorbis-1ch-8000-bursts.ogg",
            data!("vorbis-1ch-8000-bursts.ogg"),
            "bursts-1ch-8000",
        )
        .snr(22.0, 22.0)
        .level(0.99, 1.02)
        .max_error(0.086),
    ]
}

#[test]
fn files_decode_to_the_length_encoded_and_near_the_audio() {
    for bounds in files() {
        let name = bounds.name;
        let file = vorbis::read(bounds.bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        bounds.check(&file.buffer, file.sample_rate);

        // The same through the readers that work out the format.
        let any = decode(bounds.bytes).unwrap();
        assert_eq!(any.format, FileFormat::OggVorbis);
        assert_eq!(any.bits_per_sample, 32);
        assert!(any.buffer == file.buffer, "{name}");
        let reader = AudioReader::new(Cursor::new(bounds.bytes)).unwrap();
        assert_eq!(reader.format(), FileFormat::OggVorbis);
        assert!(reader.is_float());
        assert_eq!(reader.len_frames(), Some(file.buffer.num_frames() as u64));
    }
}

#[test]
fn comments_and_vendor_are_read() {
    let file = vorbis::read(data!("vorbis-2ch-44100-q3.ogg")).unwrap();
    // FFmpeg names itself as the vendor when asked to be exact, and adds an
    // encoder tag of its own.
    assert_eq!(file.vendor, "ffmpeg");
    assert_eq!(
        file.comments,
        [
            ("encoder".to_string(), "Lavc libvorbis".to_string()),
            ("TITLE".to_string(), "Test signal".to_string()),
            ("ARTIST".to_string(), "Manguebyte".to_string()),
        ]
    );
}

#[test]
fn channels_come_in_the_order_of_wav() {
    // Every channel of the source has its own pitch. Each decoded channel
    // must be nearer to the source channel of the same place than to any
    // other, which the order Vorbis stores them in would break: there the
    // centre is second and the LFE last.
    let file = vorbis::read(data!("vorbis-6ch-48000-q2.ogg")).unwrap();
    let (expected, _) = source("signal-6ch-48000");
    let tones = 0..expected.num_frames() * 3 / 10;
    for channel in 0..6 {
        let distance = |other: usize| -> f64 {
            file.buffer.channel(channel)[tones.clone()]
                .iter()
                .zip(&expected.channel(other)[tones.clone()])
                .map(|(&a, &b)| f64::from(a - b).powi(2))
                .sum()
        };
        let nearest = (0..6)
            .min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
            .unwrap();
        assert_eq!(nearest, channel, "decoded channel {channel}");
    }
}

#[test]
fn blocks_of_any_size_and_seeks_give_exactly_the_whole_decode() {
    for bounds in files() {
        let name = bounds.name;
        let whole = vorbis::read(bounds.bytes).unwrap().buffer;
        // Reading the long file in blocks costs another decode of it, which
        // seeking does not need.
        if whole.num_frames() < 200_000 {
            let streamed = read_in_random_blocks(bounds.bytes, 3);
            assert!(streamed == whole, "{name}: read in blocks");
        }
        check_seeks_are_exact(name, bounds.bytes, &whole, 120, 7);
    }
}

#[test]
fn a_seek_reads_a_small_part_of_a_long_file() {
    // The 150 seconds of bursts twice over: five minutes in 118 kilobytes
    // and 300 pages. A seek reads the pages a search looks at and walks a
    // short way, not the whole file, and lands exactly.
    let bytes = common::repeat_vorbis(data!("vorbis-1ch-8000-bursts.ogg"), 2);
    let whole = vorbis::read(bytes.as_slice()).unwrap().buffer;
    // The last page of the file comes once, after both rounds.
    assert!(whole.num_frames() > 2 * 1_190_000);
    let counting = Counting::new(Cursor::new(bytes.as_slice()));
    let bytes_read = Arc::clone(&counting.bytes_read);
    let mut reader = AudioReader::new(counting).unwrap();
    assert_eq!(reader.len_frames(), Some(whole.num_frames() as u64));
    let opened = bytes_read.load(Ordering::Relaxed);
    let mut block = AudioBuffer::<f32>::new(1, 100);
    let mut rng = Rng::new(17);
    let mut most = 0;
    for _ in 0..40 {
        let target = rng.below(whole.num_frames() as u64 - 100);
        let before = bytes_read.load(Ordering::Relaxed);
        assert_eq!(reader.seek(target).unwrap(), target);
        assert_eq!(reader.read(&mut block).unwrap(), 100);
        most = most.max(bytes_read.load(Ordering::Relaxed) - before);
        let from = target as usize;
        assert!(block == slice(&whole, from..from + 100), "seek to {target}");
    }
    println!(
        "{} bytes: opening read {opened}, a seek at most {most}",
        bytes.len()
    );
    assert!(opened < bytes.len() as u64 / 3, "{opened} bytes to open");
    assert!(most < bytes.len() as u64 * 2 / 3, "{most} bytes");
    check_seeks_are_exact("bursts, repeated", &bytes, &whole, 60, 21);
}

#[test]
fn cut_files_give_the_start_of_the_audio() {
    let bytes = data!("vorbis-1ch-22050-q0.ogg");
    let whole = vorbis::read(bytes).unwrap().buffer;
    for len in (0..400).step_by(3).chain((400..bytes.len()).step_by(97)) {
        let cut = &bytes[..len];
        poke(cut);
        if let Some((audio, _)) = read_what_can_be_read(cut) {
            // What a cut file gives is the start of the audio, exactly.
            assert!(audio.num_frames() <= whole.num_frames());
            assert!(
                audio == slice(&whole, 0..audio.num_frames()),
                "cut at {len}"
            );
        }
    }
}

#[test]
fn damage_is_reported_and_never_played() {
    let bytes = data!("vorbis-1ch-22050-q0.ogg");
    let whole = vorbis::read(bytes).unwrap().buffer;
    let mut noticed = 0;
    for index in (0..bytes.len()).step_by(53) {
        let mut damaged = bytes.to_vec();
        damaged[index] ^= 0x24;
        poke(&damaged);
        // Every page carries a checksum: a change anywhere is noticed, and
        // what comes before it is the true audio. Damage to the last page
        // can also show as a file that ends a page early, since the length
        // is read from that page.
        match read_what_can_be_read(&damaged) {
            None => noticed += 1,
            Some((audio, failed)) => {
                let shorter = audio.num_frames() < whole.num_frames();
                assert!(failed || shorter, "damage at {index} went unnoticed");
                noticed += usize::from(failed);
                let start = slice(&whole, 0..audio.num_frames());
                assert!(audio == start, "damage at {index}");
            }
        }
    }
    assert!(noticed > 0);

    // Any byte of the headers set to all zeros or all ones, with the page
    // checksums put right, so that the decoder itself meets the damage.
    for index in (0..400.min(bytes.len())).step_by(3) {
        for value in [0x00, 0xFF] {
            let mut damaged = bytes.to_vec();
            damaged[index] = value;
            fix_page_checksums(&mut damaged);
            poke(&damaged);
            let _ = read_what_can_be_read(&damaged);
        }
    }

    // And bits flipped anywhere, with the checksums put right.
    let mut rng = Rng::new(31);
    for _ in 0..150 {
        let mut damaged = bytes.to_vec();
        let index = rng.below(bytes.len() as u64) as usize;
        damaged[index] ^= 1 << rng.below(8);
        fix_page_checksums(&mut damaged);
        poke(&damaged);
        let _ = read_what_can_be_read(&damaged);
    }
}

#[test]
fn a_seek_past_damage_lands_on_the_true_audio() {
    // A page in the middle is damaged. Reading from the start stops there
    // with an error, and stays stopped; a seek beyond it reads on as if
    // nothing had happened.
    let bytes = common::repeat_vorbis(data!("vorbis-1ch-8000-q5.ogg"), 3);
    let whole = vorbis::read(bytes.as_slice()).unwrap().buffer;
    let pages = common::ogg_pages(&bytes);
    let hit = pages[pages.len() / 2].start + 40;
    let mut damaged = bytes.clone();
    damaged[hit] ^= 0x01;

    let mut reader = AudioReader::new(Cursor::new(damaged.as_slice())).unwrap();
    let mut block = AudioBuffer::<f32>::new(1, 1_000);
    let mut read = 0usize;
    let error = loop {
        match reader.read(&mut block) {
            Ok(got) => {
                assert!(got > 0, "the damage was not met");
                assert!(block == slice(&whole, read..read + got));
                read += got;
            }
            Err(error) => break error,
        }
    };
    assert!(
        matches!(error, FormatError::Vorbis(VorbisError::Corrupt)),
        "{error}"
    );
    assert!(
        reader.read(&mut block).is_err(),
        "trying again tries the same page"
    );

    let total = whole.num_frames() as u64;
    let target = total - 5_000;
    println!(
        "{} pages, damage in page {}, {total} frames",
        pages.len(),
        pages.len() / 2
    );
    assert_eq!(
        reader
            .seek(target)
            .unwrap_or_else(|error| panic!("{error}")),
        target
    );
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    let from = target as usize;
    assert!(block == slice(&whole, from..from + 1_000));
}

#[test]
fn what_is_not_vorbis_in_an_ogg_file_is_refused() {
    // An Ogg page that starts a stream of something else, with a valid
    // checksum.
    let mut page = b"OggS\x00\x02".to_vec();
    page.extend_from_slice(&[0; 8]);
    page.extend_from_slice(&7u32.to_le_bytes());
    page.extend_from_slice(&0u32.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(1);
    page.push(8);
    page.extend_from_slice(b"OpusHead");
    fix_page_checksums(&mut page);
    assert!(matches!(
        decode(&page),
        Err(FormatError::Vorbis(VorbisError::NotVorbis))
    ));
    assert!(matches!(
        AudioReader::new(Cursor::new(&page)),
        Err(FormatError::Vorbis(VorbisError::NotVorbis))
    ));
    // Only the start of a page.
    assert!(matches!(
        AudioReader::new(Cursor::new(&page[..20])),
        Err(FormatError::Vorbis(VorbisError::Truncated))
    ));
}
