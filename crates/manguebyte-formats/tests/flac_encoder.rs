//! Proves the FLAC encoder through the crate's public interface.
//!
//! What is written must decode to what went in, whichever way it was
//! written: as a whole buffer, as integers, or a block at a time through
//! `AudioWriter` in pieces of any size, which must give the very same
//! bytes. Files the encoder wrote are kept in `data/` as fixtures, and were
//! decoded by FFmpeg as well (see `data/README.md`).

mod common;

use std::io::{self, Cursor, Seek, SeekFrom};

use common::{Failing, FullDisk, Rng, slice};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::flac::{self, FlacError, FlacOptions};
use manguebyte_formats::{
    AudioReader, AudioWriter, FileFormat, FormatError, RecorderConfig, RecordingWriter, decode,
};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// Rounds to a value that `bits` bits hold exactly, with full scale at 1.
fn quantize(value: f64, bits: u32) -> f32 {
    let full = (1u64 << (bits - 1)) as f64;
    ((value * full).round().clamp(-full, full - 1.0) / full) as f32
}

/// A few seconds of something like music, stereo, quantised to `bits`.
///
/// A plucked melody with eight harmonics, vibrato and decay, panned note by
/// note; a bass line; a sustained chord slightly detuned between the two
/// sides; a kick drum and a hi-hat of filtered noise; two early echoes that
/// cross between the channels; and a noise floor near the last bit. It is
/// meant to give an encoder what a recording would: tones it can predict,
/// noise it cannot, and two channels that are alike but not equal.
fn music(seconds: f64, rate: u32, bits: u32) -> AudioBuffer<f32> {
    use std::f64::consts::TAU;
    let frames = (seconds * f64::from(rate)) as usize;
    let rate = f64::from(rate);
    let mut sides = [vec![0.0f64; frames], vec![0.0f64; frames]];
    let mut rng = Rng::new(0x5EED);
    let mut uniform = move || (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
    let beat = 0.25;
    let pitch = |semitones: f64| 220.0 * 2f64.powf(semitones / 12.0);

    // The melody, on a pentatonic scale.
    let scale = [0.0, 2.0, 4.0, 7.0, 9.0, 12.0, 14.0, 16.0];
    let mut start = 0.0;
    while start < seconds {
        let note = scale[(uniform() * scale.len() as f64) as usize % scale.len()];
        let length = beat * (1.0 + (uniform() * 2.0).floor());
        let pan = 0.2 + 0.6 * uniform();
        let first = (start * rate) as usize;
        let last = (((start + length + 1.2) * rate) as usize).min(frames);
        let [left, right] = &mut sides;
        let span = left[first..last].iter_mut().zip(&mut right[first..last]);
        for (offset, (left, right)) in span.enumerate() {
            let t = offset as f64 / rate;
            let envelope = (1.0 - (-t / 0.004).exp()) * (-t / 0.35).exp();
            let wobble = 1.0 + 0.003 * (TAU * 5.5 * t).sin();
            let mut value = 0.0;
            for harmonic in 1..=8 {
                let k = f64::from(harmonic);
                value +=
                    (TAU * k * pitch(note) * wobble * t).sin() / k.powf(1.4) * (-t * k / 0.8).exp();
            }
            let value = 0.18 * envelope * value;
            *left += value * (1.0 - pan);
            *right += value * pan;
        }
        start += length;
    }

    let [left, right] = &mut sides;
    for (index, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = index as f64 / rate;
        // A bass note every two beats.
        let bar = (t / (2.0 * beat)).floor();
        let since = t - bar * 2.0 * beat;
        let root = pitch([-24.0, -19.0, -17.0, -22.0][bar as usize % 4]);
        let bass = 0.22
            * (-since / 0.6).exp()
            * ((TAU * root * t).sin() + 0.3 * (TAU * 2.0 * root * t).sin());
        // A chord that swells slowly, wider on one side than the other.
        let swell = 0.05 * (0.5 - 0.5 * (TAU * 0.25 * t).cos());
        let mut pad = [0.0; 2];
        for (side, detune) in [(0, 1.0), (1, 1.003)] {
            for semitones in [0.0, 4.0, 7.0] {
                let f = pitch(semitones - 12.0) * detune;
                pad[side] += swell * ((TAU * f * t).sin() + 0.4 * (TAU * 2.0 * f * t).sin());
            }
        }
        // A kick on every other beat, falling in pitch.
        let kick_time = t % (2.0 * beat);
        let kick = 0.35
            * (-kick_time / 0.12).exp()
            * (TAU * (50.0 * kick_time + 140.0 * 0.03 * (1.0 - (-kick_time / 0.03).exp()))).sin();
        *left += bass + pad[0] + kick;
        *right += bass + pad[1] + kick;
    }
    // The hi-hat: noise differentiated twice, so that it is bright, in short
    // bursts on every half beat.
    let mut history = [0.0; 2];
    let [left, right] = &mut sides;
    for (index, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = index as f64 / rate;
        let since = t % (beat / 2.0);
        let noise = uniform() - 0.5;
        let bright = noise - 2.0 * history[0] + history[1];
        history = [noise, history[0]];
        let hat = 0.05 * (-since / 0.03).exp() * bright;
        *left += 0.7 * hat;
        *right += hat;
    }
    // Two early reflections, each side hearing the other.
    let echoes = [
        ((0.023 * rate) as usize, 0.22),
        ((0.037 * rate) as usize, 0.15),
    ];
    let dry = sides.clone();
    for (delay, gain) in echoes {
        for index in delay..frames {
            sides[0][index] += gain * dry[1][index - delay];
            sides[1][index] += gain * dry[0][index - delay];
        }
    }

    let peak = sides
        .iter()
        .flatten()
        .fold(0.0f64, |peak, value| peak.max(value.abs()));
    let floor = 1.5 / (1u64 << (bits - 1)) as f64;
    let mut buffer = AudioBuffer::new(2, frames);
    for (channel, side) in sides.iter().enumerate() {
        for (out, &value) in buffer.channel_mut(channel).iter_mut().zip(side) {
            // A floor of noise, as any recording has.
            let dither = (uniform() + uniform() - 1.0) * floor;
            *out = quantize(0.7 * value / peak + dither, bits);
        }
    }
    buffer
}

/// Writes `audio` through an `AudioWriter` in pieces whose sizes `rng`
/// picks, through all three ways of giving it samples. Stretches of
/// silence go through `write_silence`.
fn write_in_pieces<W: io::Write + Seek>(
    writer: &mut AudioWriter<W>,
    audio: &AudioBuffer<f32>,
    rng: &mut Rng,
    largest: u64,
) {
    let channels = audio.num_channels();
    let mut done = 0;
    let mut interleaved = Vec::new();
    while done < audio.num_frames() {
        let count = (rng.between(0, largest) as usize).min(audio.num_frames() - done);
        let piece = slice(audio, done..done + count);
        if piece.peak() == 0.0 && rng.below(2) == 0 {
            writer.write_silence(count as u64).unwrap();
        } else if rng.below(2) == 0 {
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

fn whole(audio: &AudioBuffer<f32>, rate: u32, options: &FlacOptions) -> Vec<u8> {
    let mut bytes = Vec::new();
    flac::write(&mut bytes, audio, rate, options).unwrap();
    bytes
}

/// Where the first frame starts: after the marker and the metadata.
fn audio_start(bytes: &[u8]) -> usize {
    let mut position = 4;
    loop {
        let last = bytes[position] & 0x80 != 0;
        let len = u32::from_be_bytes([
            0,
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]);
        position += 4 + len as usize;
        if last {
            return position;
        }
    }
}

#[test]
fn every_way_of_writing_gives_the_same_bytes_however_the_audio_is_cut() {
    let mut audio = music(1.0, 44_100, 16);
    // A stretch of silence, which the pieces may give as `write_silence`.
    for channel in 0..2 {
        audio.channel_mut(channel)[20_000..26_000].fill(0.0);
    }
    for (level, largest) in [(0, 1), (0, 3_000), (5, 700), (5, 9_000), (8, 5_000)] {
        let options = FlacOptions::new(16).with_level(level).with_seek_points(3);
        let expected = whole(&audio, 44_100, &options);
        assert!(
            flac::read(expected.as_slice()).unwrap().buffer == audio,
            "level {level}"
        );

        let mut rng = Rng::new(level as u64 * 1000 + largest);
        let mut writer = AudioWriter::flac(Cursor::new(Vec::new()), 2, 44_100, &options).unwrap();
        assert_eq!(writer.format(), FileFormat::Flac);
        write_in_pieces(&mut writer, &audio, &mut rng, largest);
        let streamed = writer.finish().unwrap().into_inner();
        assert!(
            streamed == expected,
            "level {level}, pieces up to {largest}"
        );

        // And as integers.
        let integers: Vec<Vec<i32>> = audio
            .iter_channels()
            .map(|samples| {
                samples
                    .iter()
                    .map(|&value| (value * 32_768.0) as i32)
                    .collect()
            })
            .collect();
        let slices: Vec<&[i32]> = integers.iter().map(Vec::as_slice).collect();
        let mut bytes = Vec::new();
        flac::write_integers(&mut bytes, &slices, 44_100, &options).unwrap();
        assert!(bytes == expected, "level {level} from integers");
    }
}

#[test]
fn a_stream_can_start_partway_into_a_file() {
    let audio = music(0.3, 48_000, 24);
    let options = FlacOptions::new(24);
    let mut destination = Cursor::new(b"something before".to_vec());
    destination.seek(SeekFrom::End(0)).unwrap();
    let mut writer = AudioWriter::flac(destination, 2, 48_000, &options).unwrap();
    writer.write(&audio).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(&bytes[..16], b"something before");
    assert!(bytes[16..] == whole(&audio, 48_000, &options));
}

#[test]
fn what_is_written_reads_back_through_every_reader() {
    // Stated as 4 kHz, so that its 88200 frames last 22 seconds and the
    // seek table holds three points.
    let audio = music(2.0, 44_100, 24);
    let options = FlacOptions::new(24).with_level(3);
    let bytes = whole(&audio, 4_000, &options);

    let file = decode(&bytes).unwrap();
    assert_eq!(file.format, FileFormat::Flac);
    assert_eq!((file.sample_rate, file.bits_per_sample), (4_000, 24));
    assert!(file.buffer == audio);

    // Block by block and from anywhere.
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.len_frames(), Some(audio.num_frames() as u64));
    let mut block = AudioBuffer::<f32>::new(2, 1_000);
    let mut rng = Rng::new(99);
    for _ in 0..40 {
        let target = rng.below(audio.num_frames() as u64 - 1_000);
        assert_eq!(reader.seek(target).unwrap(), target);
        assert_eq!(reader.read(&mut block).unwrap(), 1_000);
        let expected = slice(&audio, target as usize..target as usize + 1_000);
        assert!(block == expected, "seek to {target}");
    }
}

#[test]
fn samples_are_rounded_and_clipped_as_the_decoder_maps_them() {
    for bits in [8, 12, 16, 20, 24] {
        let full = (1u64 << (bits - 1)) as f32;
        let mut audio = AudioBuffer::<f32>::new(1, 9);
        audio.channel_mut(0).copy_from_slice(&[
            0.0,
            1.0,
            -1.0,
            2.5,
            -7.0,
            f32::NAN,
            0.4 / full,
            0.6 / full,
            -1.0 + 1.0 / full,
        ]);
        let file = flac::read(whole(&audio, 8_000, &FlacOptions::new(bits)).as_slice()).unwrap();
        let top = (full - 1.0) / full;
        assert_eq!(
            file.buffer.channel(0),
            &[
                0.0,
                top,
                -1.0,
                top,
                -1.0,
                0.0,
                0.0,
                1.0 / full,
                -1.0 + 1.0 / full
            ],
            "{bits} bits"
        );
    }
}

#[test]
fn white_noise_takes_no_more_than_its_samples_and_the_headers() {
    for bits in [8, 16, 24] {
        let mut rng = Rng::new(u64::from(bits));
        let full = 1u64 << (bits - 1);
        let mut audio = AudioBuffer::<f32>::new(2, 20_000);
        for channel in 0..2 {
            for sample in audio.channel_mut(channel) {
                *sample = (rng.below(2 * full) as f64 - full as f64) as f32 / full as f32;
            }
        }
        for level in 0..=8 {
            let options = FlacOptions::new(bits)
                .with_level(level)
                .with_seek_points(0)
                .with_padding(0);
            let bytes = whole(&audio, 44_100, &options);
            assert!(flac::read(bytes.as_slice()).unwrap().buffer == audio);
            let frames = 20_000usize.div_ceil(options.block_size() as usize);
            let samples = 20_000 * 2 * bits as usize / 8;
            // A frame header of at most 16 bytes, a byte of subframe header
            // per channel, and the checksum.
            let bound = samples + frames * (16 + 2 + 2);
            let audio_bytes = bytes.len() - audio_start(&bytes);
            assert!(
                audio_bytes <= bound,
                "{bits} bits, level {level}: {audio_bytes} > {bound}"
            );
        }
    }
}

#[test]
fn an_unfinished_stream_holds_every_frame_that_was_complete() {
    let audio = music(1.0, 44_100, 16);
    let mut sink = Cursor::new(Vec::new());
    let mut writer = AudioWriter::flac(&mut sink, 2, 44_100, &FlacOptions::new(16)).unwrap();
    writer.write(&audio).unwrap();
    // The program dies here: no `finish`.
    drop(writer);
    let bytes = sink.into_inner();

    // Every full block was written; the last, partial one was not.
    let complete = audio.num_frames() / 4096 * 4096;
    let file = flac::read(bytes.as_slice()).unwrap();
    assert!(file.buffer == slice(&audio, 0..complete));
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.len_frames(), None);
    assert_eq!(reader.seek(u64::MAX).unwrap(), complete as u64);
}

#[test]
fn a_frame_that_could_not_be_written_is_written_again() {
    let audio = music(1.0, 44_100, 16);
    let options = FlacOptions::new(16).with_level(0);
    let expected = whole(&audio, 44_100, &options);
    // A disk with room for the metadata and a few frames, which refuses
    // every write once it is full, until room is made.
    let disk = Failing::new(
        Cursor::new(Vec::new()),
        audio_start(&expected) as u64 + 5_000,
    );
    let budget = std::sync::Arc::clone(&disk.budget);
    let mut writer = AudioWriter::flac(disk, 2, 44_100, &options).unwrap();
    let mut done = 0;
    let mut failures = 0;
    while done < audio.num_frames() {
        let count = 1_000.min(audio.num_frames() - done);
        let before = writer.frames_written();
        match writer.write(&slice(&audio, done..done + count)) {
            Ok(()) => done += count,
            Err(FormatError::Io(_)) => {
                failures += 1;
                // Part of the piece may have been taken; the rest is given
                // again once there is room.
                done += (writer.frames_written() - before) as usize;
                budget.fetch_add(4_000, std::sync::atomic::Ordering::Relaxed);
            }
            Err(other) => panic!("{other}"),
        }
    }
    assert!(failures >= 3, "{failures} failures");
    // Room for the last frame and for writing the metadata again.
    budget.fetch_add(1 << 20, std::sync::atomic::Ordering::Relaxed);
    let bytes = writer.finish().unwrap().into_inner().into_inner();
    assert!(bytes == expected);
}

#[test]
fn after_a_failed_write_the_count_includes_the_frame_kept_in_memory() {
    let audio = music(0.1, 44_100, 16);
    let options = FlacOptions::new(16).with_level(0);
    let expected = whole(&audio, 44_100, &options);
    // Room for the metadata and the first 100 bytes of the first frame.
    let disk = Failing::new(Cursor::new(Vec::new()), audio_start(&expected) as u64 + 100);
    let budget = std::sync::Arc::clone(&disk.budget);
    let mut writer = AudioWriter::flac(disk, 2, 44_100, &options).unwrap();
    // The first block fills and its frame cannot be written: the call fails
    // having taken the block's 1152 frames and no more.
    assert!(matches!(
        writer.write(&slice(&audio, 0..2_000)),
        Err(FormatError::Io(_))
    ));
    assert_eq!(writer.frames_written(), 1_152);
    // Still no room: the frame is tried again first, and nothing is taken.
    assert!(matches!(
        writer.write(&slice(&audio, 1_152..2_000)),
        Err(FormatError::Io(_))
    ));
    assert_eq!(writer.frames_written(), 1_152);
    // Room again: the frame is written where it began, then the rest.
    budget.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
    writer
        .write(&slice(&audio, 1_152..audio.num_frames()))
        .unwrap();
    assert_eq!(writer.frames_written(), audio.num_frames() as u64);
    let bytes = writer.finish().unwrap().into_inner().into_inner();
    assert!(bytes == expected);
}

#[test]
fn on_a_full_disk_finish_still_completes_the_stream_information() {
    let audio = music(1.0, 44_100, 16);
    let options = FlacOptions::new(16).with_level(0);
    let expected = whole(&audio, 44_100, &options);
    // Room for all but the last few frames.
    let disk = FullDisk::new(expected.len() as u64 - 3_000);
    let contents = disk.contents();
    let mut writer = AudioWriter::flac(disk, 2, 44_100, &options).unwrap();
    assert!(writer.write(&audio).is_err());
    assert!(writer.finish().is_err());
    let bytes = contents.lock().unwrap().clone();

    // The stream information describes the frames that fit, exactly as if
    // the audio had ended with them.
    let mut reader = AudioReader::new(Cursor::new(bytes.clone())).unwrap();
    let kept = reader.len_frames().unwrap() as usize;
    assert!(
        kept > 0 && kept < audio.num_frames() && kept % 1152 == 0,
        "{kept}"
    );
    let mut out = AudioBuffer::new(2, kept + 10);
    assert_eq!(reader.read(&mut out).unwrap(), kept);
    out.set_num_frames(kept);
    assert!(out == slice(&audio, 0..kept));
    let shorter = whole(&slice(&audio, 0..kept), 44_100, &options);
    assert!(bytes.starts_with(&shorter));
}

#[test]
fn a_recording_can_be_compressed_as_it_is_made() {
    let audio = music(1.0, 48_000, 24);
    let options = FlacOptions::new(24).with_level(5);
    let writer = AudioWriter::flac(Cursor::new(Vec::new()), 2, 48_000, &options).unwrap();
    let config = RecorderConfig {
        capacity_frames: 8_192,
    };
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    let mut rng = Rng::new(17);
    let mut done = 0;
    while done < audio.num_frames() {
        let count = (rng.between(1, 600) as usize).min(audio.num_frames() - done);
        // As an audio callback would, then the worker's turn.
        assert_eq!(recorder.push(&slice(&audio, done..done + count)), count);
        worker.drain().unwrap();
        done += count;
    }
    let bytes = worker.finish().unwrap().into_inner();
    assert!(bytes == whole(&audio, 48_000, &options));
}

#[test]
fn a_file_can_be_written_to_and_read_from_a_path() {
    let audio = music(0.5, 44_100, 16);
    let options = FlacOptions::new(16).with_tag("TITLE", "path test");
    let directory = std::env::temp_dir();
    let streamed = directory.join(format!(
        "manguebyte-flac-{}-streamed.flac",
        std::process::id()
    ));
    let whole_file = directory.join(format!("manguebyte-flac-{}-whole.flac", std::process::id()));

    let mut writer = AudioWriter::create_flac(&streamed, 2, 44_100, &options).unwrap();
    writer.write(&audio).unwrap();
    writer.finish().unwrap();
    flac::write_file(&whole_file, &audio, 44_100, &options).unwrap();

    let read = std::fs::read(&streamed).unwrap();
    assert!(read == std::fs::read(&whole_file).unwrap());
    let mut reader = AudioReader::open(&streamed).unwrap();
    let mut out = AudioBuffer::new(2, audio.num_frames());
    assert_eq!(reader.read(&mut out).unwrap(), audio.num_frames());
    assert!(out == audio);
    drop(reader);

    // Options that are refused leave the files as they were.
    let refused = FlacOptions::new(16).with_level(9);
    assert!(flac::write_file(&whole_file, &audio, 44_100, &refused).is_err());
    assert!(AudioWriter::create_flac(&streamed, 2, 44_100, &refused).is_err());
    assert!(std::fs::read(&streamed).unwrap() == read);
    assert!(std::fs::read(&whole_file).unwrap() == read);
    std::fs::remove_file(&streamed).unwrap();
    std::fs::remove_file(&whole_file).unwrap();
}

#[test]
fn errors_from_the_options_reach_the_caller() {
    let audio = AudioBuffer::<f32>::new(9, 10);
    let mut bytes = Vec::new();
    match flac::write(&mut bytes, &audio, 44_100, &FlacOptions::new(16)) {
        Err(FlacError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
        other => panic!("{other:?}"),
    }
    match AudioWriter::flac(
        Cursor::new(Vec::new()),
        2,
        44_100,
        &FlacOptions::new(16).with_level(12),
    ) {
        Err(FormatError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("level 12 accepted"),
    }

    // The same checks, on their own.
    for (channels, options) in [
        (9, FlacOptions::new(16)),
        (2, FlacOptions::new(16).with_level(12)),
    ] {
        match options.validate(channels, 44_100) {
            Err(FlacError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
            other => panic!("{other:?}"),
        }
    }
    assert!(FlacOptions::new(16).validate(8, 44_100).is_ok());
}

/// The length of the body of the last metadata block.
fn last_block_len(bytes: &[u8]) -> usize {
    let mut position = 4;
    loop {
        let len = u32::from_be_bytes([
            0,
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]) as usize;
        if bytes[position] & 0x80 != 0 {
            return len;
        }
        position += 4 + len;
    }
}

#[test]
fn the_largest_padding_the_format_allows_is_written() {
    // A metadata block's body holds up to 2^24 - 1 bytes (RFC 9639, section
    // 8). Without a seek table the padding may take all of it. With one,
    // seek points that are not used join the padding block, so the padding,
    // the header of the table it replaces and the whole table must fit.
    let most = (1u32 << 24) - 1;
    let audio = AudioBuffer::<f32>::new(1, 16);
    for (padding, points) in [
        (most - 3, 0),
        (most, 0),
        (most - 4 - 18, 1),
        (most - 4 - 18 * 932_067, 932_067),
    ] {
        let options = FlacOptions::new(16)
            .with_padding(padding)
            .with_seek_points(points);
        let mut bytes = Vec::new();
        let result = flac::write(&mut bytes, &audio, 44_100, &options);
        assert!(
            result.is_ok(),
            "padding of {padding} bytes, {points} points: {result:?}"
        );
        assert!(flac::read(bytes.as_slice()).unwrap().buffer == audio);
        // Sixteen samples need one seek point; the padding block holds the
        // padding and the room of the points left over.
        let unused = points.saturating_sub(1) as usize;
        assert_eq!(last_block_len(&bytes), padding as usize + 18 * unused);

        // The streaming writer reserves the same room, and so gives the
        // same bytes.
        let mut writer = AudioWriter::flac(Cursor::new(Vec::new()), 1, 44_100, &options).unwrap();
        writer.write(&audio).unwrap();
        assert!(writer.finish().unwrap().into_inner() == bytes);
    }
}

/// The samples of the six-channel fixture: a value every channel computes
/// from its position, different in kind for each.
fn six_channels(channel: usize, index: usize) -> i32 {
    let full = 1 << 19;
    let t = index as f64 / 22_050.0;
    let value = match channel {
        // A tone at three quarters of full scale.
        0 => 0.75 * (std::f64::consts::TAU * 440.0 * t).sin() * f64::from(full),
        // The same tone a little later and quieter.
        1 => 0.5 * (std::f64::consts::TAU * 440.0 * (t - 0.001)).sin() * f64::from(full),
        // Silence.
        2 => 0.0,
        // A square wave at full scale.
        3 => {
            if (index / 50) % 2 == 0 {
                f64::from(full - 1)
            } else {
                -f64::from(full)
            }
        }
        // A slow ramp.
        4 => (index as f64 * 97.0) % f64::from(2 * full) - f64::from(full),
        // Quiet numbers that look random: eleven bits of them.
        _ => ((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 53) as f64 - 1024.0,
    };
    value.round() as i32
}

fn six_channel_options() -> FlacOptions {
    FlacOptions::new(20)
        .with_level(2)
        .with_block_size(1_000)
        .with_padding(0)
        .with_seek_points(1)
        .with_tag("TITLE", "six channels")
}

/// The stream information's MD5 digest.
fn md5_of(bytes: &[u8]) -> &[u8] {
    &bytes[26..42]
}

#[test]
fn files_the_encoder_wrote_decode_to_their_source() {
    // Made by `dump_files_for_other_software`, decoded by FFmpeg too.
    let stereo = decode(data!("stereo16.wav")).unwrap().buffer;
    let file = flac::read(data!("encoder-stereo16-level0.flac")).unwrap();
    assert_eq!((file.sample_rate, file.bits_per_sample), (44_100, 16));
    assert!(file.buffer == stereo);

    let mono = decode(data!("mono24.wav")).unwrap().buffer;
    let file = flac::read(data!("encoder-mono24-level8.flac")).unwrap();
    assert_eq!((file.sample_rate, file.bits_per_sample), (96_000, 24));
    assert!(file.buffer == mono);

    let bytes = data!("encoder-6ch-20bit.flac");
    let file = flac::read(bytes).unwrap();
    assert_eq!((file.sample_rate, file.bits_per_sample), (22_050, 20));
    assert_eq!(file.buffer.num_channels(), 6);
    assert_eq!(file.buffer.num_frames(), 2_345);
    for channel in 0..6 {
        for (index, &value) in file.buffer.channel(channel).iter().enumerate() {
            let expected = six_channels(channel, index) as f32 / (1 << 19) as f32;
            assert_eq!(value, expected, "channel {channel}, sample {index}");
        }
    }

    // Each checksum is the one the encoder computes today for the same
    // audio, and FFmpeg found it to match the decoded audio.
    for (bytes, audio, rate, options) in [
        (
            data!("encoder-stereo16-level0.flac"),
            &stereo,
            44_100,
            fixture_options(16, 0),
        ),
        (
            data!("encoder-mono24-level8.flac"),
            &mono,
            96_000,
            fixture_options(24, 8),
        ),
    ] {
        assert_eq!(md5_of(bytes), md5_of(&whole(audio, rate, &options)));
    }
}

fn fixture_options(bits: u32, level: u8) -> FlacOptions {
    FlacOptions::new(bits)
        .with_level(level)
        .with_padding(0)
        .with_seek_points(1)
}

/// What FFmpeg 7.1's encoder makes of `music(5.0, 44_100, 16)`, in bytes of
/// audio frames, metadata left out. Measured by hand with
/// `ffmpeg -f s16le -ar 44100 -ac 2 -i music.raw -c:a flac -compression_level N`
/// on the file `dump_files_for_other_software` writes; see `data/README.md`.
const FFMPEG_MUSIC_BYTES: [(u8, usize); 3] = [(0, 568_369), (5, 492_876), (8, 490_211)];

#[test]
fn compression_is_within_a_few_percent_of_ffmpegs() {
    let audio = music(5.0, 44_100, 16);
    for (level, theirs) in FFMPEG_MUSIC_BYTES {
        let bytes = whole(&audio, 44_100, &FlacOptions::new(16).with_level(level));
        let ours = bytes.len() - audio_start(&bytes);
        let ratio = ours as f64 / theirs as f64;
        println!("level {level}: {ours} bytes, FFmpeg {theirs}, ratio {ratio:.4}");
        assert!(
            ratio < 1.03,
            "level {level}: {ours} bytes against FFmpeg's {theirs}"
        );
    }

    // Levels 1 and 2 differ only in how the stereo coding is chosen: from
    // an estimate, or by coding all four and keeping the smallest. So level
    // 2 is never larger; how much larger level 1 comes out is quoted in the
    // documentation of `flac`.
    let [one, two] = [1, 2].map(|level| {
        let bytes = whole(&audio, 44_100, &FlacOptions::new(16).with_level(level));
        bytes.len() - audio_start(&bytes)
    });
    println!("level 1: {one} bytes, level 2: {two}");
    assert!(two <= one, "level 1: {one} bytes, level 2: {two}");
}

/// Writes the encoder's output for many kinds of input to a directory, with
/// the samples each must decode to, so that another decoder can be checked
/// against them:
///
/// ```text
/// MANGUEBYTE_DUMP_DIR=/some/dir cargo test -p manguebyte-formats \
///     --test flac_encoder -- --ignored
/// ```
///
/// Each `NAME.flac` comes with `NAME.pcm`, its samples as interleaved
/// 32-bit little-endian integers, and a line in `manifest.txt`: name,
/// channels, bits and sample rate. It also writes the fixtures in `data/`
/// and `music.raw`, the material of the comparison with FFmpeg's encoder.
#[test]
#[ignore = "writes files for checking with other software"]
fn dump_files_for_other_software() {
    let directory = std::env::var("MANGUEBYTE_DUMP_DIR").expect("set MANGUEBYTE_DUMP_DIR");
    let directory = std::path::Path::new(&directory);
    let mut manifest = String::new();
    let mut dump = |name: &str, bytes: &[u8], channels: &[Vec<i32>], bits: u32, rate: u32| {
        std::fs::write(directory.join(format!("{name}.flac")), bytes).unwrap();
        let mut pcm = Vec::new();
        for index in 0..channels.first().map_or(0, Vec::len) {
            for channel in channels {
                pcm.extend_from_slice(&channel[index].to_le_bytes());
            }
        }
        std::fs::write(directory.join(format!("{name}.pcm")), pcm).unwrap();
        manifest.push_str(&format!("{name} {} {bits} {rate}\n", channels.len()));
    };
    let integers = |audio: &AudioBuffer<f32>, bits: u32| -> Vec<Vec<i32>> {
        let full = (1u64 << (bits - 1)) as f32;
        audio
            .iter_channels()
            .map(|samples| samples.iter().map(|&value| (value * full) as i32).collect())
            .collect()
    };
    let encode = |channels: &[Vec<i32>], rate: u32, options: &FlacOptions| {
        let slices: Vec<&[i32]> = channels.iter().map(Vec::as_slice).collect();
        let mut bytes = Vec::new();
        flac::write_integers(&mut bytes, &slices, rate, options).unwrap();
        bytes
    };

    // Signals of every kind at every level and the common depths.
    let kinds = ["silence", "square", "noise", "sweep"];
    let signal = |kind: usize, bits: u32, len: usize, seed: u64| -> Vec<i32> {
        let full = 1i64 << (bits - 1);
        let mut rng = Rng::new(seed);
        (0..len)
            .map(|index| {
                let value = match kind {
                    0 => 0,
                    1 => {
                        if (index / 7 + seed as usize) % 2 == 0 {
                            full - 1
                        } else {
                            -full
                        }
                    }
                    2 => rng.below(2 * full as u64) as i64 - full,
                    _ => {
                        let t = index as f64 / 44_100.0;
                        let span = len as f64 / 44_100.0;
                        let phase =
                            std::f64::consts::TAU * (20.0 * t + 19_980.0 * t * t / (2.0 * span));
                        (0.9 * (full - 1) as f64 * (phase + seed as f64).sin()).round() as i64
                    }
                };
                value as i32
            })
            .collect()
    };
    for level in 0..=8u8 {
        for bits in [8, 16, 24, 32] {
            for (kind, kind_name) in kinds.iter().enumerate() {
                let audio = vec![signal(kind, bits, 5_000, 2), signal(kind, bits, 5_000, 3)];
                let options = FlacOptions::new(bits).with_level(level);
                let name = format!("level{level}-{bits}bit-{kind_name}");
                dump(
                    &name,
                    &encode(&audio, 44_100, &options),
                    &audio,
                    bits,
                    44_100,
                );
            }
        }
    }
    // Every depth from 4 to 32.
    for bits in 4..=32 {
        let audio = vec![signal(3, bits, 6_000, 4), signal(2, bits, 6_000, 5)];
        let options = FlacOptions::new(bits).with_level(5);
        dump(
            &format!("depth{bits}"),
            &encode(&audio, 48_000, &options),
            &audio,
            bits,
            48_000,
        );
    }
    // One to eight channels.
    for channels in 1..=8 {
        let audio: Vec<Vec<i32>> = (0..channels)
            .map(|channel| signal(channel % 4, 16, 7_000, channel as u64 + 9))
            .collect();
        for level in [0, 8] {
            let options = FlacOptions::new(16).with_level(level);
            dump(
                &format!("channels{channels}-level{level}"),
                &encode(&audio, 48_000, &options),
                &audio,
                16,
                48_000,
            );
        }
    }
    // Block sizes with and without codes of their own.
    for block_size in [16, 17, 192, 256, 1_000, 4_608, 16_384, 65_535] {
        let audio = vec![
            signal(3, 16, 2 * block_size as usize + 33, 1),
            signal(2, 16, 2 * block_size as usize + 33, 2),
        ];
        let options = FlacOptions::new(16).with_block_size(block_size);
        dump(
            &format!("blocks{block_size}"),
            &encode(&audio, 44_100, &options),
            &audio,
            16,
            44_100,
        );
    }
    // Large frames: noise in the largest blocks.
    for (bits, block_size) in [
        (8, 65_535),
        (16, 32_768),
        (16, 49_152),
        (16, 52_000),
        (16, 54_000),
        (16, 56_000),
        (16, 60_000),
        (16, 65_535),
    ] {
        let audio = vec![
            signal(2, bits, 3 * block_size as usize, 1),
            signal(2, bits, 3 * block_size as usize, 2),
        ];
        let options = FlacOptions::new(bits).with_block_size(block_size);
        dump(
            &format!("bigframes{bits}-{block_size}"),
            &encode(&audio, 44_100, &options),
            &audio,
            bits,
            44_100,
        );
    }
    // Sample rates with each kind of code, and one with none.
    for rate in [
        1, 7, 8_000, 11_000, 11_025, 22_050, 88_200, 192_000, 352_800, 705_600, 1_048_575,
    ] {
        let audio = vec![signal(3, 24, 3_000, 1)];
        dump(
            &format!("rate{rate}"),
            &encode(&audio, rate, &FlacOptions::new(24)),
            &audio,
            24,
            rate,
        );
    }
    // Very short streams.
    for len in [0, 1, 2, 15, 16, 17] {
        let audio = vec![signal(2, 16, len, 1), signal(3, 16, len, 2)];
        dump(
            &format!("short{len}"),
            &encode(&audio, 44_100, &FlacOptions::new(16)),
            &audio,
            16,
            44_100,
        );
    }
    // Extremes of 32 bits: opposite channels and full-scale steps.
    let full = 1i64 << 31;
    let left: Vec<i32> = (0..9_000)
        .map(|index| {
            if index % 3 == 0 {
                (full - 1) as i32
            } else {
                (-full) as i32
            }
        })
        .collect();
    let right: Vec<i32> = left.iter().map(|&value| -1 - value).collect();
    for level in [0, 5, 8] {
        let audio = vec![left.clone(), right.clone()];
        dump(
            &format!("extreme32-level{level}"),
            &encode(&audio, 96_000, &FlacOptions::new(32).with_level(level)),
            &audio,
            32,
            96_000,
        );
    }
    // Wasted bits, and one stream long enough for a seek table of several
    // points, with tags and the default padding.
    let wasted: Vec<Vec<i32>> = (0..2)
        .map(|seed| {
            signal(2, 13, 9_000, seed + 7)
                .into_iter()
                .map(|value| value * 8)
                .collect()
        })
        .collect();
    dump(
        "wasted",
        &encode(&wasted, 44_100, &FlacOptions::new(16)),
        &wasted,
        16,
        44_100,
    );
    let long = vec![signal(3, 16, 65_000, 1)];
    let options = FlacOptions::new(16)
        .with_seek_points(4)
        .with_tag("TITLE", "seek table")
        .with_tag("ARTIST", "manguebyte");
    dump(
        "seektable",
        &encode(&long, 1_000, &options),
        &long,
        16,
        1_000,
    );
    // The largest metadata blocks the format allows: padding alone, and a
    // seek table of 932,067 points of which one is used.
    let short = vec![signal(3, 16, 3_000, 1)];
    for (name, padding, points) in [
        ("padding-largest", (1 << 24) - 1, 0),
        ("seektable-largest", 5, 932_067),
    ] {
        let options = FlacOptions::new(16)
            .with_padding(padding)
            .with_seek_points(points);
        dump(name, &encode(&short, 44_100, &options), &short, 16, 44_100);
    }

    // The streaming writer, finished and not.
    let audio = music(2.0, 44_100, 16);
    let options = FlacOptions::new(16);
    let mut writer = AudioWriter::flac(Cursor::new(Vec::new()), 2, 44_100, &options).unwrap();
    write_in_pieces(&mut writer, &audio, &mut Rng::new(1), 3_000);
    let bytes = writer.finish().unwrap().into_inner();
    dump("streamed", &bytes, &integers(&audio, 16), 16, 44_100);
    let mut sink = Cursor::new(Vec::new());
    let mut writer = AudioWriter::flac(&mut sink, 2, 44_100, &options).unwrap();
    writer.write(&audio).unwrap();
    drop(writer);
    let complete = audio.num_frames() / 4096 * 4096;
    dump(
        "unfinished",
        sink.get_ref(),
        &integers(&slice(&audio, 0..complete), 16),
        16,
        44_100,
    );

    // The material compared with FFmpeg's encoder, and this encoder's
    // output for it at every level.
    let audio = music(5.0, 44_100, 16);
    let samples = integers(&audio, 16);
    let mut raw = Vec::new();
    for index in 0..audio.num_frames() {
        for channel in &samples {
            raw.extend_from_slice(&(channel[index] as i16).to_le_bytes());
        }
    }
    std::fs::write(directory.join("music.raw"), raw).unwrap();
    for level in 0..=8 {
        let options = FlacOptions::new(16).with_level(level);
        dump(
            &format!("music-level{level}"),
            &whole(&audio, 44_100, &options),
            &samples,
            16,
            44_100,
        );
    }

    // The fixtures.
    let stereo = decode(include_bytes!("data/stereo16.wav")).unwrap().buffer;
    let bytes = whole(&stereo, 44_100, &fixture_options(16, 0));
    std::fs::write(directory.join("encoder-stereo16-level0.flac"), &bytes).unwrap();
    dump(
        "encoder-stereo16-level0",
        &bytes,
        &integers(&stereo, 16),
        16,
        44_100,
    );
    let mono = decode(include_bytes!("data/mono24.wav")).unwrap().buffer;
    let bytes = whole(&mono, 96_000, &fixture_options(24, 8));
    dump(
        "encoder-mono24-level8",
        &bytes,
        &integers(&mono, 24),
        24,
        96_000,
    );
    let six: Vec<Vec<i32>> = (0..6)
        .map(|channel| {
            (0..2_345)
                .map(|index| six_channels(channel, index))
                .collect()
        })
        .collect();
    dump(
        "encoder-6ch-20bit",
        &encode(&six, 22_050, &six_channel_options()),
        &six,
        20,
        22_050,
    );

    std::fs::write(directory.join("manifest.txt"), manifest).unwrap();
}
