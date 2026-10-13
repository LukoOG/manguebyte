//! Decodes files written by another program and compares them with their
//! source.
//!
//! A decoder tested only against an encoder by the same author can share
//! that author's misreading of the format. These files were written by
//! FFmpeg 7.1 from the two WAV files next to them (see `data/README.md`),
//! so what is checked here is agreement with an independent implementation.
//! FLAC and PCM AIFF are lossless: the decoded audio must equal the source
//! exactly, sample for sample.

use manguebyte_formats::{FileFormat, FormatError, aiff, decode, flac, wav};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

#[test]
fn flac_decodes_to_exactly_what_was_encoded() {
    let source = wav::read(data!("stereo16.wav")).unwrap();
    assert_eq!(source.buffer.num_frames(), 8_820);

    for (name, bytes) in [
        // Fixed predictors only, and independent channels.
        ("level 0", data!("stereo16-level0.flac")),
        // Linear prediction and stereo decorrelation.
        ("level 5", data!("stereo16-level5.flac")),
        // High-order prediction, short blocks, exhaustive search.
        ("level 12", data!("stereo16-level12.flac")),
    ] {
        let decoded = flac::read(bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(decoded.sample_rate, 44_100, "{name}");
        assert_eq!(decoded.bits_per_sample, 16, "{name}");
        assert!(decoded.buffer == source.buffer, "{name}: audio differs");
    }
}

#[test]
fn flac_decodes_24_bit_audio() {
    let source = wav::read(data!("mono24.wav")).unwrap();
    let decoded = flac::read(data!("mono24.flac")).unwrap();
    assert_eq!(decoded.sample_rate, 96_000);
    assert_eq!(decoded.bits_per_sample, 24);
    assert_eq!(decoded.buffer.num_channels(), 1);
    assert!(decoded.buffer == source.buffer);
}

#[test]
fn aiff_decodes_to_exactly_what_was_encoded() {
    let stereo = wav::read(data!("stereo16.wav")).unwrap();
    let mono = wav::read(data!("mono24.wav")).unwrap();

    let big_endian = aiff::read(data!("stereo16.aiff")).unwrap();
    assert_eq!(
        (big_endian.sample_rate, big_endian.bits_per_sample),
        (44_100, 16)
    );
    assert!(big_endian.buffer == stereo.buffer);

    let little_endian = aiff::read(data!("stereo16-sowt.aifc")).unwrap();
    assert!(little_endian.buffer == stereo.buffer);

    let deep = aiff::read(data!("mono24.aiff")).unwrap();
    assert_eq!((deep.sample_rate, deep.bits_per_sample), (96_000, 24));
    assert!(deep.buffer == mono.buffer);
}

#[test]
fn the_format_is_recognised_from_the_contents() {
    for (bytes, format, rate) in [
        (data!("stereo16.wav"), FileFormat::Wav, 44_100),
        (data!("stereo16.aiff"), FileFormat::Aiff, 44_100),
        (data!("stereo16-sowt.aifc"), FileFormat::Aiff, 44_100),
        (data!("mono24.flac"), FileFormat::Flac, 96_000),
    ] {
        let file = decode(bytes).unwrap();
        assert_eq!(file.format, format);
        assert_eq!(file.sample_rate, rate);
        assert!(file.buffer.peak() > 0.1);
    }
    assert!(matches!(
        decode(b"ID3\x04 not audio we know"),
        Err(FormatError::Unrecognized)
    ));
    assert!(matches!(decode(b""), Err(FormatError::Unrecognized)));
}

/// Where the first audio frame begins: after the marker and every metadata
/// block.
fn first_frame(bytes: &[u8]) -> usize {
    let mut position = 4;
    loop {
        let is_last = bytes[position] & 0x80 != 0;
        let length = u32::from_be_bytes([
            0,
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]);
        position += 4 + length as usize;
        if is_last {
            return position;
        }
    }
}

#[test]
fn damaged_flac_is_reported_and_never_panics() {
    let valid = data!("stereo16-level5.flac");

    // Cut short anywhere: an error, or a shorter file if the cut falls
    // exactly between two frames.
    for len in (0..valid.len()).step_by(211) {
        if let Ok(file) = flac::read(&valid[..len]) {
            assert!(
                file.buffer.num_frames() < 8_820,
                "cut at {len} lost nothing"
            );
        }
    }

    // Flip one byte anywhere in the audio: every frame carries a checksum,
    // so the damage is always noticed. The audio starts after the metadata
    // blocks, which here include several kilobytes of padding that may be
    // changed freely.
    let audio_start = first_frame(valid);
    assert!(audio_start > 42 && audio_start < valid.len() / 2);
    for index in (audio_start..valid.len()).step_by(97) {
        let mut damaged = valid.to_vec();
        damaged[index] ^= 0x55;
        assert!(
            flac::read(damaged.as_slice()).is_err(),
            "damage at {index} went unnoticed"
        );
    }

    // And in the headers, where anything may happen except a panic.
    // The marker and the stream information are in the first 42 bytes.
    for index in 0..64 {
        for value in [0x00, 0x7F, 0x80, 0xFF] {
            let mut damaged = valid.to_vec();
            damaged[index] = value;
            let _ = flac::read(damaged.as_slice());
        }
    }
}
