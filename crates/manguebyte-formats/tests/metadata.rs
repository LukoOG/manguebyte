//! Metadata: what FFmpeg wrote into WAV, FLAC and MP3 files is read back as
//! it was written, and what this crate writes into WAV, AIFF and FLAC files
//! comes back unchanged, whole file or streamed, beside audio that is not
//! changed by it.

mod common;

use std::io::Cursor;

use common::{Rng, ramp};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::aiff::{self, AiffFormat};
use manguebyte_formats::flac::{self, FlacOptions};
use manguebyte_formats::metadata::{
    Broadcast, ChunkId, CuePoint, Instrument, Loop, LoopKind, Loudness, Metadata, Picture, RawChunk,
};
use manguebyte_formats::wav::{self, WavFormat};
use manguebyte_formats::{AudioReader, AudioWriter, FormatError, decode, mp3};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

fn picture() -> Picture {
    Picture {
        kind: 3,
        mime: "image/png".into(),
        description: "Capa".into(),
        width: 4,
        height: 2,
        depth: 24,
        colors: 0,
        // The PNG file FFmpeg made, as the picture block of the FLAC file
        // holds it.
        data: data!("meta-tags-picture.flac")[0x5B..0x5B + 94].to_vec(),
    }
}

/// Metadata with every field set that WAV keeps.
fn for_wav() -> Metadata {
    Metadata {
        title: Some("Maracatu Atômico".into()),
        artist: Some("Chico Science".into()),
        album: Some("Afrociberdelia".into()),
        date: Some("1996".into()),
        track: Some(5),
        track_total: Some(17),
        genre: Some("Manguebeat".into()),
        comment: Some("Odd".into()),
        copyright: Some("(C) 1996".into()),
        originator: Some("Manguebyte".into()),
        time_reference: Some(48_000 * 3_600 * 13),
        broadcast: Some(Broadcast {
            description: "Take 3".into(),
            originator_reference: "MB-0003".into(),
            origination_date: "2026-10-17".into(),
            origination_time: "13:00:00".into(),
            version: 2,
            umid: vec![7; 64],
            loudness: Some(Loudness {
                integrated: -23.0,
                range: 6.25,
                max_true_peak: -1.0,
                max_momentary: -14.5,
                max_short_term: -17.0,
            }),
            coding_history: "A=PCM,F=48000,W=24,M=stereo,T=Manguebyte\r\n".into(),
        }),
        ixml: Some("<?xml version=\"1.0\"?><BWFXML><SCENE>12</SCENE></BWFXML>".into()),
        instrument: Some(Instrument {
            root_note: 45,
            fine_tune: 0.0,
            gain_db: 2.0,
            notes: (40, 50),
            velocities: (1, 127),
        }),
        loops: vec![
            Loop::forward(10, 900),
            Loop {
                start: 100,
                end: 200,
                kind: LoopKind::Backward,
                play_count: 3,
            },
        ],
        cue_points: vec![
            CuePoint::new(1, 0).with_label("Start"),
            CuePoint {
                id: 2,
                position: 500,
                length: 100,
                label: Some("Verse".into()),
            },
        ],
        tags: vec![("IENG".into(), "Engineer".into())],
        chunks: vec![RawChunk {
            id: ChunkId::Riff(*b"xtra"),
            data: vec![1, 2, 3, 4, 5],
        }],
        ..Metadata::default()
    }
}

#[test]
fn what_ffmpeg_writes_into_wav_is_read() {
    let file = wav::read(data!("meta-info-bext.wav")).unwrap();
    let metadata = &file.metadata;
    assert_eq!(metadata.title.as_deref(), Some("Frevo"));
    assert_eq!(metadata.artist.as_deref(), Some("Manguebyte"));
    assert_eq!(metadata.album.as_deref(), Some("Tests"));
    assert_eq!(metadata.date.as_deref(), Some("2026"));
    assert_eq!(metadata.track, Some(4));
    assert_eq!(metadata.genre.as_deref(), Some("Frevo"));
    assert_eq!(metadata.comment.as_deref(), Some("A comment"));
    assert_eq!(metadata.copyright.as_deref(), Some("(C) 2026"));
    assert_eq!(metadata.originator.as_deref(), Some("FFmpeg"));
    assert_eq!(metadata.time_reference, Some(480_000));
    let broadcast = metadata.broadcast.as_ref().unwrap();
    assert_eq!(broadcast.description, "A test tone");
    assert_eq!(broadcast.originator_reference, "MB-1");
    assert_eq!(broadcast.origination_date, "2026-10-17");
    assert_eq!(broadcast.origination_time, "12:34:56");
    assert_eq!(broadcast.version, 1);
    assert_eq!(broadcast.loudness, None);
    assert_eq!(broadcast.coding_history, "A=PCM,F=8000,W=16,M=mono");
    assert!(metadata.chunks.is_empty(), "{:?}", metadata.chunks);
    assert_eq!(file.buffer.num_frames(), 400);

    // The same through the readers that work out the format.
    assert_eq!(
        decode(data!("meta-info-bext.wav")).unwrap().metadata,
        file.metadata
    );
    let reader = AudioReader::new(Cursor::new(data!("meta-info-bext.wav"))).unwrap();
    assert_eq!(reader.metadata(), &file.metadata);
}

#[test]
fn what_ffmpeg_writes_into_flac_is_read() {
    let file = flac::read(data!("meta-tags-picture.flac")).unwrap();
    let metadata = &file.metadata;
    assert_eq!(metadata.title.as_deref(), Some("Maracatu"));
    assert_eq!(metadata.artist.as_deref(), Some("Nação Zumbi"));
    assert_eq!(metadata.album.as_deref(), Some("Da Lama ao Caos"));
    assert_eq!(metadata.date.as_deref(), Some("1994"));
    assert_eq!((metadata.track, metadata.track_total), (Some(3), Some(12)));
    assert_eq!(metadata.genre.as_deref(), Some("Manguebeat"));
    assert_eq!(metadata.bpm, Some(96.0));
    assert_eq!(metadata.pictures, [picture()]);
    assert!(metadata.pictures[0].data.starts_with(b"\x89PNG\r\n"));
    assert!(metadata.tags.is_empty(), "{:?}", metadata.tags);
    assert!(metadata.chunks.is_empty());
    let reader = AudioReader::new(Cursor::new(data!("meta-tags-picture.flac"))).unwrap();
    assert_eq!(reader.metadata(), &file.metadata);
}

#[test]
fn what_ffmpeg_writes_into_id3_tags_is_read() {
    let file = mp3::read(data!("meta-id3v24.mp3")).unwrap();
    let metadata = &file.metadata;
    assert_eq!(metadata.title.as_deref(), Some("Ciranda"));
    assert_eq!(metadata.artist.as_deref(), Some("Lia de Itamaracá"));
    assert_eq!(metadata.album.as_deref(), Some("Eu Sou Lia"));
    assert_eq!(metadata.date.as_deref(), Some("2000"));
    assert_eq!((metadata.track, metadata.track_total), (Some(1), Some(10)));
    assert_eq!(metadata.genre.as_deref(), Some("Ciranda"));
    assert_eq!(metadata.bpm, Some(104.0));
    assert_eq!(metadata.key.as_deref(), Some("Am"));
    // FFmpeg writes a comment as a TXXX frame with that description.
    assert_eq!(metadata.tag("comment"), Some("Ciranda de praia"));
    let mut cover = picture();
    // ID3 says nothing of the size of a picture.
    (cover.width, cover.height, cover.depth) = (0, 0, 0);
    assert_eq!(metadata.pictures, [cover]);

    // ID3v2.3, with an ID3v1 tag at the end that says the same.
    let file = mp3::read(data!("meta-id3v23-v1.mp3")).unwrap();
    let metadata = &file.metadata;
    assert_eq!(metadata.title.as_deref(), Some("Coco"));
    assert_eq!(metadata.artist.as_deref(), Some("Selma do Coco"));
    assert_eq!(metadata.date.as_deref(), Some("1998"));
    assert_eq!(metadata.track, Some(7));
    assert_eq!(metadata.genre.as_deref(), Some("Folk"));
    assert_eq!(metadata.tag("comment"), Some("Old tags"));
    // The ID3v1 tag alone: the file without its ID3v2 tag.
    let bytes = data!("meta-id3v23-v1.mp3");
    let id3v2_len = 10
        + bytes[6..10]
            .iter()
            .fold(0usize, |size, &byte| size << 7 | usize::from(byte));
    let only = mp3::read(&bytes[id3v2_len..]).unwrap().metadata;
    assert_eq!(only.title.as_deref(), Some("Coco"));
    assert_eq!(only.artist.as_deref(), Some("Selma do Coco"));
    assert_eq!(only.date.as_deref(), Some("1998"));
    assert_eq!(only.comment.as_deref(), Some("Old tags"));
    assert_eq!(only.track, Some(7));
    // Genre 80, Folk, is one of Winamp's additions to the 80 of ID3v1.
    assert_eq!(only.genre, None);
    assert!(only.tags.is_empty());

    let reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.metadata(), &file.metadata);
}

#[test]
fn vorbis_comments_fill_the_fields() {
    let bytes = data!("vorbis-2ch-44100-q3.ogg");
    let file = manguebyte_formats::vorbis::read(bytes).unwrap();
    assert_eq!(file.metadata.title.as_deref(), Some("Test signal"));
    assert_eq!(file.metadata.artist.as_deref(), Some("Manguebyte"));
    assert_eq!(file.metadata.tag("encoder"), Some("Lavc libvorbis"));
    let reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.metadata(), &file.metadata);
}

#[test]
fn wav_metadata_round_trips_with_the_audio() {
    let audio = ramp(2, 1_001);
    let metadata = for_wav();
    for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
        let mut bytes = Vec::new();
        wav::write_with_metadata(&mut bytes, &audio, 48_000, format, &metadata).unwrap();
        let file = wav::read(bytes.as_slice()).unwrap();
        assert_eq!(file.metadata, metadata);
        let mut plain = Vec::new();
        wav::write(&mut plain, &audio, 48_000, format).unwrap();
        assert_eq!(file.buffer, wav::read(plain.as_slice()).unwrap().buffer);

        // Read and written again, nothing changes. (The integer formats
        // scale by 32767 when writing and 32768 when reading, so only float
        // samples come back the same.)
        if format == WavFormat::Float32 {
            let mut again = Vec::new();
            wav::write_with_metadata(&mut again, &file.buffer, 48_000, format, &file.metadata)
                .unwrap();
            assert_eq!(again, bytes);
        }
    }
}

#[test]
fn aiff_metadata_round_trips_with_the_audio() {
    let audio = ramp(1, 777);
    let metadata = Metadata {
        title: Some("Coco de Roda".into()),
        artist: Some("Selma".into()),
        comment: Some("An annotation".into()),
        copyright: Some("(C) 2026".into()),
        instrument: Some(Instrument {
            root_note: 72,
            fine_tune: 3.0,
            gain_db: -1.0,
            notes: (60, 84),
            velocities: (20, 127),
        }),
        loops: vec![Loop::forward(100, 700)],
        cue_points: vec![CuePoint::new(1, 50).with_label("Here")],
        chunks: vec![RawChunk {
            id: ChunkId::Aiff(*b"APPL"),
            data: b"MBYTsomething".to_vec(),
        }],
        ..Metadata::default()
    };
    for format in [AiffFormat::Int16, AiffFormat::Int24, AiffFormat::Float32] {
        let mut bytes = Vec::new();
        aiff::write_with_metadata(&mut bytes, &audio, 44_100, format, &metadata).unwrap();
        let file = aiff::read(bytes.as_slice()).unwrap();
        assert_eq!(file.metadata, metadata);
        let mut plain = Vec::new();
        aiff::write(&mut plain, &audio, 44_100, format).unwrap();
        assert_eq!(file.buffer, aiff::read(plain.as_slice()).unwrap().buffer);
        let mut again = Vec::new();
        aiff::write_with_metadata(&mut again, &file.buffer, 44_100, format, &file.metadata)
            .unwrap();
        assert_eq!(again, bytes);
    }
}

#[test]
fn flac_keeps_tags_pictures_and_blocks_it_does_not_know() {
    let audio = ramp(2, 5_000);
    let metadata = Metadata {
        title: Some("Frevo".into()),
        artist: Some("Spok".into()),
        track: Some(2),
        bpm: Some(150.0),
        key: Some("Bb".into()),
        isrc: Some("BRMBY2600002".into()),
        pictures: vec![picture()],
        tags: vec![("ALBUMARTIST".into(), "Orquestra".into())],
        chunks: vec![RawChunk {
            id: ChunkId::Flac(2),
            data: b"MBYTapplication data".to_vec(),
        }],
        ..Metadata::default()
    };
    let options = FlacOptions::new(16).with_metadata(&metadata);
    let mut bytes = Vec::new();
    flac::write(&mut bytes, &audio, 44_100, &options).unwrap();
    let file = flac::read(bytes.as_slice()).unwrap();
    assert_eq!(file.metadata, metadata);
    assert_eq!(
        file.buffer,
        flac::read(
            &{
                let mut plain = Vec::new();
                flac::write(&mut plain, &audio, 44_100, &FlacOptions::new(16)).unwrap();
                plain
            }[..]
        )
        .unwrap()
        .buffer
    );

    // The same streamed, and the file FFmpeg wrote, read and written again.
    let mut writer = AudioWriter::flac(Cursor::new(Vec::new()), 2, 44_100, &options).unwrap();
    writer.write(&audio).unwrap();
    assert!(matches!(
        writer.set_metadata(&metadata),
        Err(FormatError::Unsupported(_))
    ));
    let streamed = writer.finish().unwrap().into_inner();
    assert_eq!(streamed, bytes);
    let ffmpeg = flac::read(data!("meta-tags-picture.flac")).unwrap();
    let options = FlacOptions::new(16).with_metadata(&ffmpeg.metadata);
    let mut again = Vec::new();
    flac::write(&mut again, &ffmpeg.buffer, 8_000, &options).unwrap();
    assert_eq!(
        flac::read(again.as_slice()).unwrap().metadata,
        ffmpeg.metadata
    );

    // A raw block of a type the encoder writes itself is refused.
    let bad = FlacOptions::new(16).with_metadata(&Metadata {
        chunks: vec![RawChunk {
            id: ChunkId::Flac(4),
            data: vec![],
        }],
        ..Metadata::default()
    });
    assert!(bad.validate(2, 44_100).is_err());
}

#[test]
fn a_streamed_file_gets_its_metadata_after_the_samples() {
    let audio = ramp(2, 3_003);
    let metadata = for_wav();
    for rf64 in [false, true] {
        for format in [WavFormat::Int16, WavFormat::Int24] {
            let cursor = Cursor::new(Vec::new());
            let mut writer = if rf64 {
                AudioWriter::wav_rf64(cursor, 2, 48_000, format).unwrap()
            } else {
                AudioWriter::wav(cursor, 2, 48_000, format).unwrap()
            };
            writer.write(&audio).unwrap();
            // Set twice: the second replaces the first.
            writer.set_metadata(&Metadata::default()).unwrap();
            writer.set_metadata(&metadata).unwrap();
            let bytes = writer.finish().unwrap().into_inner();
            let file = wav::read(bytes.as_slice()).unwrap();
            assert_eq!(file.metadata, metadata, "rf64 {rf64}");
            assert_eq!(file.buffer.num_frames(), 3_003);
            // The RIFF size covers the whole file.
            if !rf64 {
                let size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                assert_eq!(size as usize + 8, bytes.len());
            }
            let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
            assert_eq!(reader.metadata(), &metadata);
            assert_eq!(reader.take_metadata(), metadata);
            assert!(reader.metadata().is_empty());
        }
    }

    // A loop needs an instrument chunk, which states a root note: middle C
    // when none is given.
    let aiff_metadata = Metadata {
        title: Some("Streamed".into()),
        loops: vec![Loop::forward(1, 3_000)],
        cue_points: vec![CuePoint::new(4, 2_000)],
        instrument: Some(Instrument::default()),
        ..Metadata::default()
    };
    let mut writer =
        AudioWriter::aiff(Cursor::new(Vec::new()), 2, 48_000, AiffFormat::Int24).unwrap();
    writer.set_metadata(&aiff_metadata).unwrap();
    writer.write(&audio).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    assert_eq!(size as usize + 8, bytes.len());
    let file = aiff::read(bytes.as_slice()).unwrap();
    assert_eq!(file.metadata, aiff_metadata);
    assert_eq!(file.buffer.num_frames(), 3_003);

    // Metadata that cannot be written is refused when it is set.
    let mut writer =
        AudioWriter::wav(Cursor::new(Vec::new()), 1, 48_000, WavFormat::Int16).unwrap();
    let bad = Metadata {
        loops: vec![Loop::forward(9, 3)],
        ..Metadata::default()
    };
    assert!(matches!(
        writer.set_metadata(&bad),
        Err(FormatError::Wav(wav::WavError::Io(_)))
    ));
}

#[test]
fn converting_between_formats_keeps_what_both_have() {
    let audio = ramp(1, 500);
    let mut bytes = Vec::new();
    wav::write_with_metadata(&mut bytes, &audio, 48_000, WavFormat::Int24, &for_wav()).unwrap();
    let file = decode(&bytes).unwrap();
    let mut aiff_bytes = Vec::new();
    aiff::write_with_metadata(
        &mut aiff_bytes,
        &file.buffer,
        48_000,
        AiffFormat::Int24,
        &file.metadata,
    )
    .unwrap();
    let back = aiff::read(aiff_bytes.as_slice()).unwrap().metadata;
    assert_eq!(back.title, file.metadata.title);
    assert_eq!(back.artist, file.metadata.artist);
    assert_eq!(back.copyright, file.metadata.copyright);
    assert_eq!(back.instrument, file.metadata.instrument);
    // AIFF has no backward loops, and keeps no lengths of cue points.
    assert_eq!(back.loops[0], file.metadata.loops[0]);
    assert_eq!(back.loops[1].kind, LoopKind::Forward);
    assert_eq!(back.cue_points[1].length, 0);
    assert_eq!(back.cue_points[1].label.as_deref(), Some("Verse"));
    // What AIFF has no place for is not there, and the WAV chunks are not
    // carried over.
    assert_eq!(back.album, None);
    assert_eq!(back.broadcast, None);
    assert!(back.chunks.is_empty());
}

#[test]
fn damaged_metadata_never_panics_and_never_costs_the_audio() {
    let audio = ramp(1, 64);
    let mut wav_bytes = Vec::new();
    wav::write_with_metadata(&mut wav_bytes, &audio, 8_000, WavFormat::Int16, &for_wav()).unwrap();
    let mut aiff_bytes = Vec::new();
    aiff::write_with_metadata(
        &mut aiff_bytes,
        &audio,
        8_000,
        AiffFormat::Int16,
        &Metadata {
            loops: vec![Loop::forward(1, 2)],
            cue_points: vec![CuePoint::new(1, 3).with_label("x")],
            title: Some("t".into()),
            ..Metadata::default()
        },
    )
    .unwrap();
    let files: [&[u8]; 6] = [
        &wav_bytes,
        &aiff_bytes,
        data!("meta-info-bext.wav"),
        data!("meta-tags-picture.flac"),
        data!("meta-id3v24.mp3"),
        data!("meta-id3v23-v1.mp3"),
    ];
    let mut rng = Rng::new(31);
    for bytes in files {
        for round in 0..400 {
            let mut damaged = bytes.to_vec();
            for _ in 0..1 + round % 4 {
                let at = rng.below(bytes.len().min(2_000) as u64) as usize;
                damaged[at] = rng.next() as u8;
            }
            let _ = decode(&damaged);
            if let Ok(mut reader) = AudioReader::new(Cursor::new(damaged.as_slice())) {
                let mut block = AudioBuffer::new(reader.num_channels(), 100);
                while let Ok(1..) = reader.read(&mut block) {}
            }
        }
        for len in (0..bytes.len()).step_by(7) {
            let _ = decode(&bytes[..len]);
        }
    }
}
