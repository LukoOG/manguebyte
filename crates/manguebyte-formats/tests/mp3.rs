//! Decodes MP3 files that LAME wrote and compares them with the audio they
//! were made from; and files taken apart and put together again, for what
//! LAME does not write.
//!
//! The files in `data/` were encoded by LAME through FFmpeg 7.1 from audio
//! that `common::lossy` makes (see `data/README.md`). A lossy codec does not
//! give back what it was given, so what is checked is the length, exactly,
//! and how near the audio is, against bounds set a decibel or so short of
//! what was measured. The measurements are printed, so that `--nocapture`
//! shows how far each file is from its bounds.
//!
//! Seeking is held to more than nearness: what is read after a seek must
//! be, bit for bit, what a decode of the whole file gives at that place.

mod common;

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::lossy::{
    Bounds, check_seeks_are_exact, poke, read_in_random_blocks, read_what_can_be_read,
};
use common::{Counting, Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::mp3::{self, Mp3Error};
use manguebyte_formats::{AudioReader, FileFormat, FormatError, decode};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// The samples LAME puts before the audio, as the LAME tag of every file
/// here says, and the 529 the filter banks of any decoder take to give
/// their first sample: where the audio of a file without a LAME tag starts.
const DELAY: usize = 576 + 529;

/// The files, with their bounds.
///
/// The whole-file ratios are low because of the stretch of white noise: a
/// perceptual coder keeps the spectrum of noise, not its waveform. The
/// tones show what the coding of a steady sound keeps. In constant bitrate
/// up to 160 kbit/s, LAME scales what it encodes by 0.95, as the level of
/// the tones shows to four places; the ratio of the tones is taken once the
/// level is matched.
///
/// The LAME tag FFmpeg wrote into the 12 kHz file says the audio is 15023
/// samples long, ten more than were encoded, and the decoder gives what the
/// tag says.
fn files() -> Vec<Bounds> {
    let file = |name, bytes, source| Bounds::new(name, bytes, source).level(0.945, 0.955);
    vec![
        file(
            "mp3-2ch-44100-cbr128.mp3",
            data!("mp3-2ch-44100-cbr128.mp3"),
            "signal-2ch-44100",
        )
        .snr(11.5, 55.0)
        .max_error(0.44),
        file(
            "mp3-2ch-44100-vbr2.mp3",
            data!("mp3-2ch-44100-vbr2.mp3"),
            "signal-2ch-44100",
        )
        .snr(14.0, 44.5)
        .level(0.995, 1.005)
        .max_error(0.37),
        file(
            "mp3-tones-2ch-48000-320.mp3",
            data!("mp3-tones-2ch-48000-320.mp3"),
            "tones-2ch-48000",
        )
        .snr(59.0, 59.0)
        .level(0.998, 1.002)
        .max_error(0.025),
        file(
            "mp3-1ch-22050-64.mp3",
            data!("mp3-1ch-22050-64.mp3"),
            "signal-1ch-22050",
        )
        .snr(15.5, 53.0)
        .max_error(0.41),
        file(
            "mp3-2ch-24000-vbr4.mp3",
            data!("mp3-2ch-24000-vbr4.mp3"),
            "signal-2ch-24000",
        )
        .snr(14.0, 31.5)
        .level(0.99, 1.005)
        .max_error(0.34),
        file(
            "mp3-1ch-16000-32.mp3",
            data!("mp3-1ch-16000-32.mp3"),
            "signal-1ch-16000",
        )
        .snr(14.5, 45.0)
        .max_error(0.25),
        file(
            "mp3-2ch-11025-32.mp3",
            data!("mp3-2ch-11025-32.mp3"),
            "signal-2ch-11025",
        )
        .snr(12.0, 31.0)
        .level(0.94, 0.955)
        .max_error(0.38),
        file(
            "mp3-1ch-12000-24.mp3",
            data!("mp3-1ch-12000-24.mp3"),
            "signal-1ch-12000",
        )
        .at(0, 15_023)
        .snr(13.5, 41.0)
        .max_error(0.29),
        file(
            "mp3-1ch-8000-16.mp3",
            data!("mp3-1ch-8000-16.mp3"),
            "signal-1ch-8000",
        )
        .snr(13.5, 32.0)
        .max_error(0.33),
        file(
            "mp3-2ch-32000-stereo96.mp3",
            data!("mp3-2ch-32000-stereo96.mp3"),
            "signal-2ch-32000",
        )
        .snr(12.0, 42.5)
        .max_error(0.40),
        file(
            "mp3-2ch-44100-100frames.mp3",
            data!("mp3-2ch-44100-100frames.mp3"),
            "tones-2ch-44100-short",
        )
        .snr(23.5, 25.0)
        .level(0.93, 0.97)
        .max_error(0.051),
        // No Xing header and so no LAME tag: every decoded sample is given,
        // and the audio starts where the delays end.
        file(
            "mp3-2ch-44100-plain160.mp3",
            data!("mp3-2ch-44100-plain160.mp3"),
            "signal-2ch-44100",
        )
        .at(DELAY, 97 * 1_152)
        .snr(10.0, 40.0)
        .level(0.94, 1.01)
        .max_error(0.5),
    ]
}

#[test]
fn files_decode_to_the_length_encoded_and_near_the_audio() {
    for bounds in files() {
        let name = bounds.name;
        let file = mp3::read(bounds.bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        bounds.check(&file.buffer, file.sample_rate);

        // The same through the readers that work out the format.
        let any = decode(bounds.bytes).unwrap();
        assert_eq!(any.format, FileFormat::Mp3);
        assert_eq!(any.bits_per_sample, 32);
        assert!(any.buffer == file.buffer, "{name}");
        let reader = AudioReader::new(Cursor::new(bounds.bytes)).unwrap();
        assert_eq!(reader.format(), FileFormat::Mp3);
        assert!(reader.is_float());
        // Only a file with a Xing header says how long it is.
        let stated = (bounds.offset == 0).then_some(file.buffer.num_frames() as u64);
        assert_eq!(reader.len_frames(), stated, "{name}");
    }
}

#[test]
fn blocks_of_any_size_and_seeks_give_exactly_the_whole_decode() {
    for bounds in files() {
        let name = bounds.name;
        let whole = mp3::read(bounds.bytes).unwrap().buffer;
        let streamed = read_in_random_blocks(bounds.bytes, bounds.bytes.len() as u64);
        assert!(streamed == whole, "{name}: read in blocks");
        check_seeks_are_exact(name, bounds.bytes, &whole, 40, 7);
    }
}

#[test]
fn a_long_stream_is_counted_once_and_seeks_are_exact() {
    // The frames of a file of 2.5 seconds six times over, with no header
    // to say how long that is: the length is known once the frames have
    // been counted, and after that a seek reads a few frames.
    let bytes = common::mp3::repeat_frames(data!("mp3-2ch-44100-cbr128.mp3"), 6);
    let whole = mp3::read(bytes.as_slice()).unwrap().buffer;
    let total = whole.num_frames() as u64;
    assert_eq!(total, 6 * 97 * 1_152);

    let counting = Counting::new(Cursor::new(bytes.as_slice()));
    let bytes_read = Arc::clone(&counting.bytes_read);
    let mut reader = AudioReader::new(counting).unwrap();
    assert_eq!(reader.len_frames(), None);
    assert_eq!(reader.seek(u64::MAX).unwrap(), total);
    let mut block = AudioBuffer::<f32>::new(2, 100);
    assert_eq!(reader.read(&mut block).unwrap(), 0);

    let mut rng = Rng::new(5);
    let mut most = 0;
    for _ in 0..25 {
        let target = rng.below(total - 100);
        let before = bytes_read.load(Ordering::Relaxed);
        assert_eq!(reader.seek(target).unwrap(), target);
        assert_eq!(reader.read(&mut block).unwrap(), 100);
        most = most.max(bytes_read.load(Ordering::Relaxed) - before);
        let from = target as usize;
        assert!(block == slice(&whole, from..from + 100), "seek to {target}");
    }
    println!("{} bytes: a seek read at most {most}", bytes.len());
    assert!(most < bytes.len() as u64 / 8, "{most} bytes");
    check_seeks_are_exact("repeated frames", &bytes, &whole, 32, 9);
}

#[test]
fn a_xing_header_gives_the_length_of_a_long_stream() {
    // The same with the first frame of silence put back in front, its
    // count of frames made six times larger: the LAME tag's delay and
    // padding apply at the start and the end of the whole.
    let original = data!("mp3-2ch-44100-cbr128.mp3");
    let frames = common::mp3::frames(original);
    let mut info = original[frames[0].clone()].to_vec();
    assert!(common::mp3::is_info_frame(&info));
    info[44..48].copy_from_slice(&(6 * 97u32).to_be_bytes());
    let mut bytes = info;
    bytes.extend_from_slice(&common::mp3::repeat_frames(original, 6));
    let plain = mp3::read(common::mp3::repeat_frames(original, 6).as_slice())
        .unwrap()
        .buffer;
    let tagged = mp3::read(bytes.as_slice()).unwrap().buffer;
    let len = 6 * 97 * 1_152 - 576 - 911;
    assert_eq!(tagged.num_frames(), len);
    assert!(tagged == slice(&plain, DELAY..DELAY + len));
    check_seeks_are_exact("repeated, tagged", &bytes, &tagged, 24, 10);
}

#[test]
fn a_vbri_header_gives_the_length_and_is_not_played() {
    // The Info frame of a LAME file turned into one of the VBRI header that
    // encoders from Fraunhofer write: 32 bytes after the frame header, its
    // name, version, delay, quality, bytes and frames. It says nothing of
    // what was put around the audio, so every decoded sample is given.
    let original = data!("mp3-2ch-44100-cbr128.mp3");
    let frames = common::mp3::frames(original);
    let mut bytes = original.to_vec();
    let info = frames[0].clone();
    bytes[info.start + 4..info.end].fill(0);
    let mut vbri = b"VBRI".to_vec();
    vbri.extend_from_slice(&1u16.to_be_bytes());
    vbri.extend_from_slice(&0u16.to_be_bytes());
    vbri.extend_from_slice(&75u16.to_be_bytes());
    vbri.extend_from_slice(&(original.len() as u32).to_be_bytes());
    vbri.extend_from_slice(&97u32.to_be_bytes());
    bytes[info.start + 36..info.start + 36 + vbri.len()].copy_from_slice(&vbri);

    let lame = mp3::read(original).unwrap().buffer;
    let file = mp3::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file.num_frames(), 97 * 1_152);
    assert!(lame == slice(&file, DELAY..DELAY + lame.num_frames()));
    let reader = AudioReader::new(Cursor::new(bytes.as_slice())).unwrap();
    assert_eq!(reader.len_frames(), Some(97 * 1_152));
}

#[test]
fn frames_with_a_crc_are_checked_against_it() {
    // A stream with no bit reservoir, so that frames have bytes to spare at
    // their end, given a CRC in each frame that has. It decodes to the same
    // samples; with a bit of side information changed, to an error.
    let original = data!("mp3-2ch-44100-plain160.mp3");
    let (protected, count) = common::mp3::protect_frames(original);
    let frames = common::mp3::frames(&protected);
    println!("{count} of {} frames carry a CRC", frames.len());
    assert!(count > frames.len() / 2);
    let reference = mp3::read(original).unwrap().buffer;
    assert!(mp3::read(protected.as_slice()).unwrap().buffer == reference);

    // The side information of a frame with a CRC, a third of the way in.
    let hit = frames
        .iter()
        .skip(frames.len() / 3)
        .find(|frame| protected[frame.start + 1] & 1 == 0)
        .unwrap()
        .clone();
    let mut damaged = protected.clone();
    damaged[hit.start + 10] ^= 0x08;
    match mp3::read(damaged.as_slice()) {
        Err(Mp3Error::Corrupt) => {}
        other => panic!("{other:?}"),
    }
    // Reading stops there, and a seek beyond it carries on.
    let (audio, failed) = read_what_can_be_read(&damaged).unwrap();
    assert!(failed);
    assert!(audio == slice(&reference, 0..audio.num_frames()));
    let mut reader = AudioReader::new(Cursor::new(damaged.as_slice())).unwrap();
    let target = reference.num_frames() as u64 - 3_000;
    assert_eq!(reader.seek(target).unwrap(), target);
    let mut block = AudioBuffer::<f32>::new(2, 1_000);
    assert_eq!(reader.read(&mut block).unwrap(), 1_000);
    let from = target as usize;
    assert!(block == slice(&reference, from..from + 1_000));
}

#[test]
fn what_is_not_a_frame_is_stepped_over() {
    // Bytes that are no frame between two frames, and an ID3v1 tag at the
    // end: the frames around them decode as before, bit for bit, with the
    // bit reservoir reaching across the gap.
    for bytes in [
        data!("mp3-2ch-44100-cbr128.mp3"),
        data!("mp3-2ch-44100-plain160.mp3"),
    ] {
        let reference = mp3::read(bytes).unwrap().buffer;
        let frames = common::mp3::frames(bytes);
        let gap = frames[frames.len() / 2].start;
        let mut changed = bytes[..gap].to_vec();
        changed.extend(std::iter::repeat_n(0x55, 301));
        changed.extend_from_slice(&bytes[gap..]);
        changed.extend_from_slice(b"TAG");
        changed.extend(std::iter::repeat_n(b' ', 125));
        let decoded = mp3::read(changed.as_slice()).unwrap().buffer;
        assert!(decoded == reference);
        check_seeks_are_exact("a gap and a tag", &changed, &reference, 16, 4);
    }
}

#[test]
fn free_format_is_refused() {
    // Layers I and II, which this once refused too, are read; see
    // `tests/mp2.rs`. Every frame's bitrate index set to 0 is free format.
    let mut free = data!("mp3-2ch-44100-plain160.mp3").to_vec();
    for frame in common::mp3::frames(data!("mp3-2ch-44100-plain160.mp3")) {
        free[frame.start + 2] &= 0x0F;
    }
    match AudioReader::new(Cursor::new(free.as_slice())) {
        Err(FormatError::Mp3(Mp3Error::Unsupported(what))) => assert!(what.contains("free")),
        other => panic!("{:?}", other.map(|_| ())),
    }
    // An ID3v2 tag and nothing after it.
    let tag = b"ID3\x04\x00\x00\x00\x00\x00\x02\x00\x00";
    assert!(matches!(
        decode(tag),
        Err(FormatError::Mp3(Mp3Error::NotMp3))
    ));
}

#[test]
fn cut_files_give_the_start_of_the_audio() {
    let bytes = data!("mp3-2ch-24000-vbr4.mp3");
    let whole = mp3::read(bytes).unwrap().buffer;
    for len in (0..600).step_by(11).chain((600..bytes.len()).step_by(293)) {
        let cut = &bytes[..len];
        poke(cut);
        if let Some((audio, _)) = read_what_can_be_read(cut) {
            assert!(audio.num_frames() <= whole.num_frames());
            assert!(
                audio == slice(&whole, 0..audio.num_frames()),
                "cut at {len}"
            );
        }
    }
}

#[test]
fn damage_never_panics() {
    // Most frames carry no CRC, and damage to their audio decodes to what
    // it decodes to. Damage to headers and side information can make
    // anything of a frame; none of it may panic, hang or read past the end.
    for bytes in [
        data!("mp3-2ch-24000-vbr4.mp3"),
        data!("mp3-1ch-8000-16.mp3"),
    ] {
        let frames = common::mp3::frames(bytes);
        let mut rng = Rng::new(77);
        for round in 0..60 {
            let mut damaged = bytes.to_vec();
            // Mostly the first 40 bytes of a frame, where the header and
            // the side information are.
            let frame = &frames[rng.below(frames.len() as u64) as usize];
            let index = if round % 3 == 0 {
                rng.below(bytes.len() as u64) as usize
            } else {
                frame.start + rng.below(40.min(frame.len() as u64)) as usize
            };
            damaged[index] = match round % 4 {
                0 => 0x00,
                1 => 0xFF,
                _ => damaged[index] ^ (1 << rng.below(8)),
            };
            poke(&damaged);
            let _ = read_what_can_be_read(&damaged);
        }
    }
}
