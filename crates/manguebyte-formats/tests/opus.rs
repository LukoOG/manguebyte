//! The Ogg Opus decoder, against the decodes of libopus.
//!
//! The files in `data/` were encoded by the libopus of FFmpeg 7.1, and
//! decoded by the same libopus to 32-bit float, as `data/README.md` says.
//! That libopus was built with `FLOAT_APPROX`, which works out the gains of
//! the CELT bands with a cubic approximation of `2^x`, within 1e-4 of it,
//! where this decoder, like a default build of libopus, takes the C
//! library's `exp`: that is what the CELT samples differ by, at about 86 dB
//! below the signal. With the same approximation, which a unit test of
//! `src/opus.rs` switches on, this decoder comes within 6e-7 of libopus
//! everywhere. SILK is decoded in fixed point: a file of SILK frames alone
//! decodes to the samples libopus gives, bit for bit.

mod common;

use std::io::Cursor;

use common::lossy::{
    check_seeks_are_exact, fix_page_checksums, poke, read_in_random_blocks, read_what_can_be_read,
    source,
};
use common::opus::{Reference, build, packets, tags};
use common::{Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::opus::{self, OpusError};
use manguebyte_formats::{AudioReader, FileFormat, decode};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// A test file, the decode libopus made of it, and whether it holds SILK
/// frames alone.
struct Case {
    name: &'static str,
    bytes: &'static [u8],
    reference: &'static [u8],
    silk_only: bool,
}

macro_rules! case {
    ($name:literal, $silk:expr) => {
        Case {
            name: $name,
            bytes: include_bytes!(concat!("data/", $name, ".opus")).as_slice(),
            reference: include_bytes!(concat!("data/", $name, ".ref")).as_slice(),
            silk_only: $silk,
        }
    };
}

fn cases() -> Vec<Case> {
    vec![
        case!("opus-celt-2ch-20ms-128k", false),
        case!("opus-celt-2ch-2.5ms-96k", false),
        case!("opus-celt-1ch-swb-10ms-40k", false),
        case!("opus-celt-1ch-nb-5ms-24k", false),
        case!("opus-celt-2ch-wb-40ms-cbr", false),
        case!("opus-celt-2ch-40ms-vbr", false),
        case!("opus-silk-1ch-nb-20ms", true),
        case!("opus-silk-1ch-mb-60ms", true),
        case!("opus-silk-1ch-wb-10ms", true),
        case!("opus-silk-2ch-wb-40ms", false),
        case!("opus-silk-1ch-bursts-45s", true),
        case!("opus-hybrid-1ch-swb-20ms", false),
        case!("opus-hybrid-2ch-fb-10ms", false),
        case!("opus-audio-2ch-120ms", false),
        case!("opus-audio-2ch-16k", false),
        case!("opus-2ch-mono-stereo-splice", false),
        case!("opus-6ch-family1", false),
        case!("opus-2ch-family255", false),
    ]
}

/// The signal-to-noise ratio of `decoded` against the frames of the
/// reference, in decibels, and the largest difference of a sample.
fn against(decoded: &AudioBuffer<f32>, reference: &Reference) -> (f64, f32) {
    let mut signal = 0.0f64;
    let mut noise = 0.0f64;
    let mut worst = 0.0f32;
    for (kept, frame) in (0..reference.frames).step_by(reference.step).enumerate() {
        for channel in 0..reference.channels {
            let theirs = reference.samples[kept * reference.channels + channel];
            let ours = decoded.channel(channel)[frame];
            signal += f64::from(theirs) * f64::from(theirs);
            noise += f64::from(ours - theirs) * f64::from(ours - theirs);
            worst = worst.max((ours - theirs).abs());
        }
    }
    (10.0 * (signal / noise.max(1e-30)).log10(), worst)
}

#[test]
fn every_file_decodes_to_the_length_and_the_samples_libopus_gives() {
    for case in cases() {
        let name = case.name;
        let reference = Reference::parse(case.reference);
        let file = opus::read(case.bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(file.buffer.num_channels(), reference.channels, "{name}");
        assert_eq!(file.buffer.num_frames(), reference.frames, "{name}: length");
        let (snr, worst) = against(&file.buffer, &reference);
        println!("{name}: {snr:.1} dB, largest difference {worst:.2e}");
        if case.silk_only {
            assert_eq!(worst, 0.0, "{name}: SILK alone is bit for bit");
        } else {
            assert!(snr >= 84.0, "{name}: {snr:.1} dB");
            assert!(worst <= 1e-4, "{name}: {worst}");
        }
    }
}

#[test]
fn every_way_of_reading_gives_the_same_samples() {
    for (seed, case) in cases().into_iter().enumerate() {
        let whole = decode(case.bytes).unwrap();
        assert_eq!(whole.format, FileFormat::OggOpus, "{}", case.name);
        assert_eq!(whole.sample_rate, 48_000);
        assert_eq!(whole.bits_per_sample, 32);
        let file = opus::read(case.bytes).unwrap();
        assert_eq!(whole.buffer, file.buffer, "{}", case.name);
        let blocks = read_in_random_blocks(case.bytes, seed as u64 + 1);
        assert_eq!(blocks, whole.buffer, "{}: in blocks", case.name);
    }
}

#[test]
fn seeks_in_short_files_land_exactly() {
    for (seed, case) in cases().into_iter().enumerate() {
        if case.name.contains("45s") {
            continue;
        }
        let whole = decode(case.bytes).unwrap().buffer;
        check_seeks_are_exact(case.name, case.bytes, &whole, 120, seed as u64 + 11);
    }
}

/// In 45 s of audio, a seek far from anything decoded starts 800 ms before
/// its target, and one near what was decoded from the start decodes on
/// from it, exactly.
#[test]
fn a_long_file_seeks_exactly_near_what_was_decoded_and_closely_elsewhere() {
    let bytes = data!("opus-silk-1ch-bursts-45s.opus");
    let whole = decode(bytes).unwrap().buffer;
    let len = whole.num_frames() as u64;
    let mut block = AudioBuffer::<f32>::new(1, 4_800);
    let compare =
        |reader: &mut AudioReader<Cursor<&[u8]>>, block: &mut AudioBuffer<f32>, at: u64| {
            assert_eq!(reader.seek(at).unwrap(), at);
            let got = reader.read(block).unwrap();
            assert_eq!(got as u64, (block.num_frames() as u64).min(len - at));
            let want = slice(&whole, at as usize..at as usize + got);
            block
                .channel(0)
                .iter()
                .zip(want.channel(0))
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max)
        };

    // A fresh reader, sent 40 s in: from 800 ms before.
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    let far = compare(&mut reader, &mut block, 1_920_000);
    println!("40 s in, from a fresh reader: largest difference {far:.2e}");
    assert!(far <= 1e-3, "{far}");
    // Back near the start: decoded from the start, exactly.
    assert_eq!(compare(&mut reader, &mut block, 400_000), 0.0);
    // On through the whole file, which keeps the state of the decoder
    // every 10 s; after that every seek is exact.
    reader.seek(0).unwrap();
    let mut rest = AudioBuffer::<f32>::new(1, 48_000);
    while reader.read(&mut rest).unwrap() > 0 {}
    let mut rng = Rng::new(45);
    for _ in 0..60 {
        let at = rng.below(len);
        assert_eq!(compare(&mut reader, &mut block, at), 0.0, "seek to {at}");
    }
}

#[test]
fn the_channels_of_mapping_family_1_come_in_the_order_of_wav() {
    let file = opus::read(data!("opus-6ch-family1.opus")).unwrap();
    let (source, _) = source("signal-6ch-48000");
    // Each channel has a pitch of its own: the decode of a channel is like
    // its source and unlike the others.
    let frames = source.num_frames() * 3 / 10;
    for channel in 0..6 {
        let decoded = file.buffer.channel(channel);
        for other in 0..6 {
            let want = source.channel(other);
            let (mut cross, mut a, mut b) = (0.0f64, 0.0f64, 0.0f64);
            for i in 0..frames {
                cross += f64::from(decoded[i]) * f64::from(want[i]);
                a += f64::from(decoded[i]) * f64::from(decoded[i]);
                b += f64::from(want[i]) * f64::from(want[i]);
            }
            let correlation = cross / (a * b).sqrt();
            if other == channel {
                assert!(correlation > 0.8, "{channel}: {correlation}");
            } else {
                assert!(
                    correlation.abs() < 0.3,
                    "{channel} and {other}: {correlation}"
                );
            }
        }
    }
}

#[test]
fn files_encoded_from_48_khz_are_near_their_source() {
    for (name, bytes, at_least) in [
        (
            "opus-celt-2ch-20ms-128k",
            data!("opus-celt-2ch-20ms-128k.opus"),
            20.0,
        ),
        (
            "opus-audio-2ch-120ms",
            data!("opus-audio-2ch-120ms.opus"),
            12.0,
        ),
        (
            "opus-celt-2ch-2.5ms-96k",
            data!("opus-celt-2ch-2.5ms-96k.opus"),
            12.0,
        ),
    ] {
        let file = opus::read(bytes).unwrap();
        let (source, _) = source("signal-2ch-48000");
        assert_eq!(file.buffer.num_frames(), source.num_frames(), "{name}");
        // The steady tones of the first three tenths.
        let tones = source.num_frames() * 3 / 10;
        let (snr, _) = common::lossy::closeness(&file.buffer, &source, 0..tones);
        println!("{name}: {snr:.1} dB on the tones");
        assert!(snr >= at_least, "{name}: {snr:.1} dB");
    }
}

#[test]
fn comments_are_read_into_the_metadata_and_the_gain_is_applied() {
    let bytes = data!("opus-celt-1ch-swb-10ms-40k.opus");
    let original = opus::read(bytes).unwrap();
    assert_eq!(original.vendor, "ffmpeg");
    let mut all = packets(bytes);
    let audio = all.split_off(2);
    let mut head = all[0].clone();
    // An output gain of -6 dB in Q7.8.
    let gain_q8: i16 = -6 * 256;
    head[16..18].copy_from_slice(&gain_q8.to_le_bytes());
    let comments = tags(
        "manguebyte",
        &["TITLE=Frevo", "ARTIST=Nação Zumbi", "BPM=120"],
    );
    let end = 57_601 + i64::from(u16::from_le_bytes([head[10], head[11]]));
    let rebuilt = build(&head, &comments, &audio, 13, Some(end));
    let file = opus::read(rebuilt.as_slice()).unwrap();
    assert_eq!(file.vendor, "manguebyte");
    assert_eq!(file.metadata.title.as_deref(), Some("Frevo"));
    assert_eq!(file.metadata.artist.as_deref(), Some("Nação Zumbi"));
    assert_eq!(file.metadata.bpm, Some(120.0));
    let reader = AudioReader::new(Cursor::new(&rebuilt)).unwrap();
    assert_eq!(reader.metadata().title.as_deref(), Some("Frevo"));
    assert_eq!(file.buffer.num_frames(), original.buffer.num_frames());
    let factor = 10f64.powf(-6.0 / 20.0);
    for (&got, &was) in file
        .buffer
        .channel(0)
        .iter()
        .zip(original.buffer.channel(0))
    {
        assert!(
            (f64::from(got) - f64::from(was) * factor).abs() <= 1e-6,
            "{got} {was}"
        );
    }
}

#[test]
fn the_pre_skip_and_the_last_granule_position_trim_the_audio() {
    let bytes = data!("opus-silk-1ch-nb-20ms.opus");
    let original = opus::read(bytes).unwrap().buffer;
    let mut all = packets(bytes);
    let audio = all.split_off(2);
    // A pre-skip 100 samples longer drops 100 more from the start.
    let mut head = all[0].clone();
    let pre_skip = u16::from_le_bytes([head[10], head[11]]);
    head[10..12].copy_from_slice(&(pre_skip + 100).to_le_bytes());
    let total = audio
        .iter()
        .map(|packet| common::opus::packet_samples(packet))
        .sum::<i64>();
    let rebuilt = build(&head, &all[1], &audio, 9, Some(total - 777));
    let file = opus::read(rebuilt.as_slice()).unwrap().buffer;
    let expected = (total - 777 - i64::from(pre_skip) - 100) as usize;
    assert_eq!(file.num_frames(), expected);
    assert_eq!(file.channel(0), &original.channel(0)[100..100 + expected]);
    // Every page in its own packet, and packets across pages, decode the
    // same.
    let paged = build(&all[0], &all[1], &audio, 1, None);
    let one_a_page = opus::read(paged.as_slice()).unwrap().buffer;
    assert_eq!(
        &one_a_page.channel(0)[..original.num_frames()],
        original.channel(0)
    );
}

#[test]
fn headers_that_break_the_rules_are_refused() {
    let bytes = data!("opus-celt-1ch-nb-5ms-24k.opus");
    let mut all = packets(bytes);
    let audio = all.split_off(2);
    let rebuild = |head: &[u8]| build(head, &all[1], &audio, 20, None);
    let refused = |head: Vec<u8>| -> OpusError {
        match opus::read(rebuild(&head).as_slice()) {
            Err(error) => error,
            Ok(_) => panic!("accepted {head:?}"),
        }
    };
    let head = all[0].clone();
    // A version of another major number.
    let mut changed = head.clone();
    changed[8] = 0x10;
    assert!(matches!(refused(changed), OpusError::Unsupported(_)));
    // No channels; three channels in family 0.
    let mut changed = head.clone();
    changed[9] = 0;
    assert!(matches!(refused(changed), OpusError::Malformed(_)));
    let mut changed = head.clone();
    changed[9] = 3;
    assert!(matches!(refused(changed), OpusError::Malformed(_)));
    // Family 1 with a channel mapped to a stream that does not exist, and
    // with too few bytes for its table.
    let mut changed = head.clone();
    changed[18] = 1;
    changed.extend_from_slice(&[1, 0, 2]);
    assert!(matches!(refused(changed), OpusError::Malformed(_)));
    let mut changed = head.clone();
    changed[18] = 1;
    assert!(matches!(refused(changed), OpusError::Malformed(_)));
    // Not Opus at all, and a header cut short.
    assert!(matches!(
        refused(b"OpusHeap\x01\x01".to_vec()),
        OpusError::NotOpus
    ));
    assert!(matches!(
        refused(head[..12].to_vec()),
        OpusError::Malformed(_)
    ));
}

#[test]
fn damaged_files_never_panic() {
    let mut rng = Rng::new(0x0D05);
    for case in cases() {
        if case.name.contains("45s") || case.name.contains("6ch") {
            continue;
        }
        let bytes = case.bytes;
        for _ in 0..12 {
            // Cut short anywhere.
            let cut = rng.below(bytes.len() as u64) as usize;
            poke(&bytes[..cut]);
            let _ = read_what_can_be_read(&bytes[..cut]);
            // Bytes changed in the audio, the pages' checksums put right so
            // that the damage reaches the decoder.
            let mut damaged = bytes.to_vec();
            for _ in 0..rng.between(1, 40) {
                let at = rng.between(120, damaged.len() as u64 - 1) as usize;
                damaged[at] = rng.next() as u8;
            }
            fix_page_checksums(&mut damaged);
            poke(&damaged);
            let _ = read_what_can_be_read(&damaged);
        }
    }
}

#[test]
fn packets_of_noise_never_panic() {
    let bytes = data!("opus-celt-2ch-20ms-128k.opus");
    let all = packets(bytes);
    let mut rng = Rng::new(77);
    for round in 0..40 {
        // Packets of random bytes, of every mode and framing, a few kept
        // whole so that the decoders carry state from noise into noise.
        let audio: Vec<Vec<u8>> = (0..30)
            .map(|_| {
                let len = rng.between(1, 1_500) as usize;
                (0..len).map(|_| rng.next() as u8).collect()
            })
            .collect();
        let mut head = all[0].clone();
        if round % 2 == 1 {
            head[9] = 1;
        }
        let stream = build(&head, &all[1], &audio, 7, None);
        poke(&stream);
        let _ = read_what_can_be_read(&stream);
    }
}

/// The same, for as long as `MANGUEBYTE_FUZZ_ROUNDS` says, with packets
/// that keep the table of contents of real ones and are noise after it.
#[test]
#[ignore = "runs for as long as it is told"]
fn noise_for_a_long_time() {
    let rounds: u64 = std::env::var("MANGUEBYTE_FUZZ_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);
    let mut rng = Rng::new(0xF0F0);
    let sources: Vec<Vec<Vec<u8>>> = cases().iter().map(|case| packets(case.bytes)).collect();
    for round in 0..rounds {
        let source = &sources[rng.below(sources.len() as u64) as usize];
        let audio: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                if rng.below(3) == 0 {
                    let len = rng.between(1, 1_300) as usize;
                    return (0..len).map(|_| rng.next() as u8).collect();
                }
                let real = &source[rng.between(2, source.len() as u64 - 1) as usize];
                let mut packet = real.clone();
                let from = rng.below(packet.len() as u64) as usize;
                for byte in &mut packet[from.max(1)..] {
                    if rng.below(4) == 0 {
                        *byte = rng.next() as u8;
                    }
                }
                if rng.below(5) == 0 {
                    packet.truncate(rng.between(1, packet.len() as u64) as usize);
                }
                packet
            })
            .collect();
        let stream = build(&source[0], &source[1], &audio, 5, None);
        poke(&stream);
        let _ = read_what_can_be_read(&stream);
        if round % 100 == 0 {
            println!("round {round}");
        }
    }
}

#[test]
fn opus_is_told_from_vorbis() {
    let bytes = data!("opus-celt-2ch-20ms-128k.opus");
    assert_eq!(
        FileFormat::detect(&bytes[..FileFormat::DETECT_LEN]),
        Some(FileFormat::OggOpus)
    );
    assert_eq!(
        FileFormat::detect(&data!("vorbis-2ch-44100-q3.ogg")[..FileFormat::DETECT_LEN]),
        Some(FileFormat::OggVorbis)
    );
    let reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.format(), FileFormat::OggOpus);
    assert_eq!(reader.sample_rate(), 48_000);
    assert_eq!(reader.len_frames(), Some(57_601));
    assert!(reader.is_float());
    // A Vorbis file is not Opus.
    assert!(matches!(
        opus::read(data!("vorbis-2ch-44100-q3.ogg")),
        Err(OpusError::NotOpus)
    ));
}

/// Compares the decodes of the `.opus` files in `MANGUEBYTE_OPUS_DIR` with
/// the `.libopus.raw` files beside them, which FFmpeg decoded with libopus
/// to 32-bit float, interleaved: how the files in `data/` were checked
/// before their references were cut down.
#[test]
#[ignore = "compares with decodes made by hand"]
fn compare_with_libopus_decodes_in_a_directory() {
    let directory = std::env::var("MANGUEBYTE_OPUS_DIR").expect("set MANGUEBYTE_OPUS_DIR");
    let mut paths: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "opus"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let reference: Vec<f32> = std::fs::read(path.with_extension("libopus.raw"))
            .unwrap()
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        let file = opus::read_file(&path).unwrap();
        let channels = file.buffer.num_channels();
        let frames = file.buffer.num_frames();
        let reference = Reference {
            step: 1,
            channels,
            frames: reference.len() / channels,
            samples: reference,
        };
        let (snr, worst) = against(&file.buffer, &reference);
        println!(
            "{name}: {frames} frames, libopus {}; {snr:.1} dB, largest difference {worst:.2e}",
            reference.frames
        );
    }
}
