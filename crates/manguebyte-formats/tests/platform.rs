//! The formats the system decodes: AAC and ALAC in MPEG-4, AAC in ADTS and
//! WMA, through Media Foundation on Windows and AudioToolbox on macOS with
//! the `platform-codecs` feature, and refused clearly everywhere else.
//!
//! The files in `data/` were encoded by FFmpeg 7.1 from audio
//! `common::lossy` makes (see `data/README.md`). ALAC is lossless, so its
//! decode is held to the 16-bit samples it was made of, bit for bit; the
//! lossy ones to bounds a little below what was measured on Windows 11.

mod common;

use std::path::PathBuf;

use manguebyte_formats::{FileFormat, decode, read_file};

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

const FILES: [(&str, FileFormat); 4] = [
    ("aac-tones-2ch-44100.m4a", FileFormat::Mp4),
    ("alac-tones-2ch-44100.m4a", FileFormat::Mp4),
    ("aac-1ch-22050.aac", FileFormat::Aac),
    ("wma-tones-2ch-44100.wma", FileFormat::Wma),
];

#[test]
fn the_formats_are_recognised_on_every_system() {
    for (name, format) in FILES {
        let bytes = std::fs::read(path(name)).unwrap();
        assert_eq!(FileFormat::detect(&bytes), Some(format), "{name}");
        assert!(format.is_platform());
    }
    assert_eq!(FileFormat::detect(b"caff\0\x01\0\0"), Some(FileFormat::Caf));
    // ADTS shares its sync word with MPEG audio, but not its layer field.
    assert_eq!(
        FileFormat::detect(&[0xFF, 0xF1, 0x50, 0x80]),
        Some(FileFormat::Aac)
    );
    assert_eq!(
        FileFormat::detect(&[0xFF, 0xFB, 0x90, 0x64]),
        Some(FileFormat::Mp3)
    );
}

#[cfg(not(all(feature = "platform-codecs", any(windows, target_os = "macos"))))]
#[test]
fn without_a_system_codec_they_are_refused_by_name() {
    use std::io::Cursor;

    use manguebyte_formats::AudioReader;

    for (name, _) in FILES {
        let bytes = std::fs::read(path(name)).unwrap();
        for result in [
            read_file(path(name)).map(|_| ()),
            decode(&bytes).map(|_| ()),
            AudioReader::open(path(name)).map(|_| ()),
            AudioReader::new(Cursor::new(bytes)).map(|_| ()),
        ] {
            match result {
                Err(manguebyte_formats::FormatError::Unsupported(what)) => {
                    assert!(what.contains("AAC"), "{name}: {what}")
                }
                other => panic!("{name}: {other:?}"),
            }
        }
    }
}

#[cfg(all(feature = "platform-codecs", any(windows, target_os = "macos")))]
mod with_the_system {
    use std::io::Cursor;

    use manguebyte_core::AudioBuffer;
    use manguebyte_formats::AudioReader;

    use super::common::lossy::{closeness, source};
    use super::common::{Rng, slice};
    use super::*;

    /// Whether the system has the codecs, which a Windows Server may not
    /// (Media Foundation is an optional feature there). Only where
    /// `MANGUEBYTE_SYSTEM_CODECS_OPTIONAL` is set may they be missing; anywhere
    /// else, a system that cannot start them fails the tests.
    fn system_has_codecs() -> bool {
        match read_file(path("alac-tones-2ch-44100.m4a")) {
            Err(manguebyte_formats::FormatError::Platform(error))
                if std::env::var("MANGUEBYTE_SYSTEM_CODECS_OPTIONAL")
                    .is_ok_and(|value| !value.is_empty()) =>
            {
                eprintln!("skipped: {error}");
                false
            }
            _ => true,
        }
    }

    /// Reads a whole file through [`AudioReader`] in blocks of random
    /// sizes.
    fn read_in_blocks(
        mut reader: AudioReader<impl std::io::Read + std::io::Seek>,
    ) -> AudioBuffer<f32> {
        let channels = reader.num_channels();
        let mut collected = vec![Vec::new(); channels];
        let mut block = AudioBuffer::<f32>::new(channels, 3_000);
        let mut rng = Rng::new(5);
        loop {
            let size = rng.between(1, 3_000) as usize;
            block.set_num_frames(size);
            let got = reader.read(&mut block).unwrap();
            for (out, channel) in collected.iter_mut().zip(block.iter_channels()) {
                out.extend_from_slice(&channel[..got]);
            }
            if got < size {
                break;
            }
        }
        let mut buffer = AudioBuffer::new(channels, collected[0].len());
        for (out, samples) in buffer.iter_channels_mut().zip(&collected) {
            out.copy_from_slice(samples);
        }
        buffer
    }

    /// The largest difference between what a seek reads and what a decode
    /// from the start has there, over seeks all over the file, read from
    /// the file and from memory.
    fn seek_error(name: &str, whole: &AudioBuffer<f32>) -> f32 {
        let bytes = std::fs::read(path(name)).unwrap();
        let from_file = seek_error_of(name, whole, AudioReader::open(path(name)).unwrap());
        let from_memory = seek_error_of(name, whole, AudioReader::new(Cursor::new(bytes)).unwrap());
        from_file.max(from_memory)
    }

    fn seek_error_of(
        name: &str,
        whole: &AudioBuffer<f32>,
        mut reader: AudioReader<impl std::io::Read + std::io::Seek>,
    ) -> f32 {
        let len = whole.num_frames() as u64;
        let mut rng = Rng::new(17);
        let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 700);
        let mut worst = 0.0f32;
        for round in 0..24 {
            let target = match round % 4 {
                0 => rng.below(3_000),
                1 => len.saturating_sub(rng.below(3_000)),
                _ => rng.below(len),
            };
            let landed = reader.seek(target).unwrap();
            assert_eq!(landed, target.min(len), "{name}: seek to {target}");
            let got = reader.read(&mut block).unwrap();
            assert_eq!(got as u64, 700.min(len - landed), "{name}: at {landed}");
            let want = slice(whole, landed as usize..landed as usize + got);
            for (read, expected) in block.iter_channels().zip(want.iter_channels()) {
                for (a, b) in read[..got].iter().zip(expected) {
                    worst = worst.max((a - b).abs());
                }
            }
        }
        worst
    }

    #[test]
    fn alac_gives_back_the_samples_it_was_made_of() {
        if !system_has_codecs() {
            return;
        }
        let name = "alac-tones-2ch-44100.m4a";
        let file = read_file(path(name)).unwrap();
        assert_eq!(file.format, FileFormat::Mp4);
        assert_eq!((file.sample_rate, file.buffer.num_channels()), (44_100, 2));
        // FFmpeg cut a quarter of a second, and rounded to 16 bits.
        let (audio, _) = source("tones-2ch-44100");
        let audio = slice(&audio, 0..11_025);
        assert_eq!(file.buffer.num_frames(), 11_025);
        for (got, want) in file.buffer.iter_channels().zip(audio.iter_channels()) {
            for (frame, (&got, &want)) in got.iter().zip(want).enumerate() {
                let sixteen = (want * 32_768.0)
                    .round_ties_even()
                    .clamp(-32_768.0, 32_767.0);
                assert_eq!(got, sixteen / 32_768.0, "{name}: frame {frame}");
            }
        }
        assert_eq!(seek_error(name, &file.buffer), 0.0);
    }

    #[test]
    fn lossy_files_decode_near_their_source() {
        if !system_has_codecs() {
            return;
        }
        // (file, source, where the audio starts, least SNR in dB, length)
        let mut files = vec![
            (
                "aac-tones-2ch-44100.m4a",
                "tones-2ch-44100",
                0..1,
                27.0,
                Some(44_100),
            ),
            // ADTS says nothing of the encoder's delay: the decoder's
            // priming is in the audio.
            (
                "aac-1ch-22050.aac",
                "signal-1ch-22050",
                0..3_000,
                22.0,
                None,
            ),
        ];
        if cfg!(windows) {
            files.push((
                "wma-tones-2ch-44100.wma",
                "tones-2ch-44100",
                0..1,
                47.0,
                Some(44_100),
            ));
        }
        for (name, from, starts, least, stated) in files {
            let file = read_file(path(name)).unwrap();
            let (audio, rate) = source(from);
            assert_eq!(file.sample_rate, rate, "{name}");
            assert_eq!(file.buffer.num_channels(), audio.num_channels(), "{name}");
            let mut best = (f64::MIN, 0, 0.0);
            for offset in starts {
                let len = (file.buffer.num_frames() - offset).min(audio.num_frames());
                let aligned = slice(&file.buffer, offset..offset + len);
                let (snr, worst) = closeness(&aligned, &slice(&audio, 0..len), 0..len);
                if snr > best.0 {
                    best = (snr, offset, worst);
                }
            }
            println!(
                "{name}: {} frames from {}, {:.2} dB, largest error {:.4}",
                file.buffer.num_frames(),
                best.1,
                best.0,
                best.2
            );
            assert!(best.0 >= least, "{name}: {:.2} dB", best.0);

            let reader = AudioReader::open(path(name)).unwrap();
            assert_eq!(reader.len_frames(), stated, "{name}");
            assert!(reader.is_float());
            assert!(reader.metadata().is_empty());
            // From the file and from memory, in any blocks, the same.
            assert!(read_in_blocks(reader) == file.buffer, "{name}: in blocks");
            let bytes = std::fs::read(path(name)).unwrap();
            let memory = AudioReader::new(Cursor::new(bytes.clone())).unwrap();
            assert!(read_in_blocks(memory) == file.buffer, "{name}: from memory");
            assert!(
                decode(&bytes).unwrap().buffer == file.buffer,
                "{name}: decode"
            );

            let error = seek_error(name, &file.buffer);
            println!("{name}: seeks read within {error:.2e} of the whole decode");
            assert!(error < 0.05, "{name}: {error}");
        }
    }

    #[test]
    fn a_reader_moves_to_another_thread() {
        if !system_has_codecs() {
            return;
        }
        let reader = AudioReader::open(path("alac-tones-2ch-44100.m4a")).unwrap();
        let whole = read_file(path("alac-tones-2ch-44100.m4a")).unwrap().buffer;
        let decoded = std::thread::spawn(move || read_in_blocks(reader))
            .join()
            .unwrap();
        assert!(decoded == whole);
    }

    #[test]
    fn damaged_files_are_errors_or_short_never_a_crash() {
        if !system_has_codecs() {
            return;
        }
        let bytes = std::fs::read(path("aac-tones-2ch-44100.m4a")).unwrap();
        let mut rng = Rng::new(9);
        for round in 0..12 {
            let mut damaged = bytes.clone();
            if round % 2 == 0 {
                damaged.truncate(rng.below(bytes.len() as u64) as usize);
            } else {
                for _ in 0..20 {
                    let at = rng.below(bytes.len() as u64) as usize;
                    damaged[at] = rng.next() as u8;
                }
            }
            let _ = decode(&damaged);
        }
    }
}
