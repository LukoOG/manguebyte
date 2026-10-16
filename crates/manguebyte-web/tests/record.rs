//! The 32-byte event record, byte by byte.

use manguebyte_web::record::{flag, offset};
use manguebyte_web::{Event, EventRecord, ParamId, RECORD_SIZE, RecordKind};

/// A record with every field set to a value whose bytes are all different.
fn every_field() -> EventRecord {
    EventRecord {
        frame: 0x0807_0605_0403_0201,
        kind: 3,
        flags: 0x09,
        part: 0x0B0A,
        note: 0x0C,
        channel: 0x0D,
        velocity: f32::from_bits(0x1312_1110),
        param: 0x1716_1514,
        value: f64::from_bits(0x1F1E_1D1C_1B1A_1918),
    }
}

#[test]
fn the_layout_is_the_agreed_one_byte_by_byte() {
    assert_eq!(RECORD_SIZE, 32);
    #[rustfmt::skip]
    let expected: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // frame, u64 little endian
        0x03,                                           // kind
        0x09,                                           // flags
        0x0A, 0x0B,                                     // part, u16
        0x0C,                                           // note
        0x0D,                                           // channel
        0x00, 0x00,                                     // padding
        0x10, 0x11, 0x12, 0x13,                         // velocity, f32
        0x14, 0x15, 0x16, 0x17,                         // param, u32
        0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, // value, f64
    ];
    assert_eq!(every_field().to_bytes(), expected);
    assert_eq!(
        (
            offset::FRAME,
            offset::KIND,
            offset::FLAGS,
            offset::PART,
            offset::NOTE,
            offset::CHANNEL,
            offset::VELOCITY,
            offset::PARAM,
            offset::VALUE
        ),
        (0, 8, 9, 10, 12, 13, 16, 20, 24)
    );
    assert_eq!(offset::PADDING, [14, 15]);
}

#[test]
fn typed_values_land_where_the_table_says() {
    let record = EventRecord::param(48_000, 513, ParamId::from_name("gain"), -6.5);
    let bytes = record.to_bytes();
    assert_eq!(&bytes[0..8], &48_000u64.to_le_bytes());
    assert_eq!(bytes[8], 3);
    assert_eq!(&bytes[10..12], &[0x01, 0x02]);
    assert_eq!(&bytes[20..24], &ParamId::from_name("gain").0.to_le_bytes());
    assert_eq!(&bytes[24..32], &(-6.5f64).to_le_bytes());

    let note = EventRecord::note_on(1, 7, 60, 0.5)
        .with_channel(9)
        .to_bytes();
    assert_eq!(
        &note[..],
        &[
            1, 0, 0, 0, 0, 0, 0, 0, // frame 1
            1, 0, // note on, no flags
            7, 0, // part 7
            60, 9, 0, 0, // note 60, channel 9, padding
            0x00, 0x00, 0x00, 0x3F, // 0.5f32
            0, 0, 0, 0, // param
            0, 0, 0, 0, 0, 0, 0, 0, // value
        ][..]
    );
}

#[test]
fn the_kind_codes_are_one_to_six() {
    let kinds = [
        (RecordKind::NoteOn, 1),
        (RecordKind::NoteOff, 2),
        (RecordKind::Param, 3),
        (RecordKind::Choke, 4),
        (RecordKind::AllNotesOff, 5),
        (RecordKind::Cancel, 6),
    ];
    for (kind, code) in kinds {
        assert_eq!(kind.code(), code);
        assert_eq!(RecordKind::from_code(code), Some(kind));
    }
    let known: Vec<u8> = (0..=255u8)
        .filter(|&code| RecordKind::from_code(code).is_some())
        .collect();
    assert_eq!(known, [1, 2, 3, 4, 5, 6]);
    assert_eq!(EventRecord::choke(5, 6).to_bytes()[8], 4);
    assert_eq!(EventRecord::all_notes_off(5, 6).to_bytes()[8], 5);
    assert_eq!(EventRecord::cancel(5, 6).to_bytes()[8], 6);
    assert_eq!(
        EventRecord::cancel(5, 6).decode(),
        Some(Event::Cancel { part: 6 })
    );
    assert_eq!(EventRecord::note_off(5, 6, 1).to_bytes()[8], 2);
}

#[test]
fn padding_is_written_as_zero_and_never_read() {
    let record = every_field();
    let mut bytes = record.to_bytes();
    for at in offset::PADDING {
        assert_eq!(bytes[at], 0);
        bytes[at] = 0xFF;
    }
    let read = EventRecord::from_bytes(&bytes);
    assert_eq!(read.to_bytes(), record.to_bytes());
    assert_eq!(read.decode(), record.decode());
}

#[test]
fn the_flags_byte_carries_the_accent_and_reserved_bits_are_ignored() {
    assert_eq!((flag::ACCENT, flag::KNOWN), (1, 1));
    let plain = EventRecord::note_on(10, 2, 38, 0.75);
    let accented = plain.with_accent(true);
    assert_eq!(plain.to_bytes()[offset::FLAGS], 0);
    assert_eq!(accented.to_bytes()[offset::FLAGS], 1);
    assert!(accented.accent() && !plain.accent());
    assert_eq!(accented.with_accent(false), plain);
    // Every other byte is the same: the accent is only bit 0 of byte 9.
    let (a, b) = (plain.to_bytes(), accented.to_bytes());
    let differing: Vec<usize> = (0..RECORD_SIZE).filter(|&i| a[i] != b[i]).collect();
    assert_eq!(differing, [offset::FLAGS]);

    let event = |flags: u8| EventRecord { flags, ..plain }.decode();
    let note = |accent| {
        Some(Event::NoteOn {
            part: 2,
            channel: 0,
            note: 38,
            velocity: 0.75,
            accent,
        })
    };
    // A record from a writer that knew nothing of flags has a 0 there.
    assert_eq!(event(0), note(false));
    assert_eq!(event(1), note(true));
    // Reserved bits change nothing, with or without the accent.
    for flags in 0..=255u8 {
        assert_eq!(event(flags), note(flags & 1 == 1), "flags {flags:#04x}");
    }
    // Only a note on reads the accent; a silent note on is a note off.
    let silent = EventRecord::note_on(10, 2, 38, 0.0).with_accent(true);
    assert_eq!(
        silent.decode(),
        Some(Event::NoteOff {
            part: 2,
            channel: 0,
            note: 38
        })
    );
    for record in [
        EventRecord::note_off(1, 2, 38),
        EventRecord::choke(1, 2),
        EventRecord::all_notes_off(1, 2),
        EventRecord::param(1, 2, ParamId(7), 0.5),
    ] {
        assert_eq!(
            EventRecord {
                flags: 0xFF,
                ..record
            }
            .decode(),
            record.decode()
        );
    }
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn any_32_bytes_read_back_unchanged_and_decode_without_panicking() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut decoded = [0usize; 7];
    for round in 0..200_000 {
        let mut bytes = [0u8; 32];
        for chunk in bytes.chunks_exact_mut(8) {
            chunk.copy_from_slice(&xorshift(&mut state).to_le_bytes());
        }
        // Make valid kinds and notes common enough to reach every branch.
        if round % 2 == 0 {
            bytes[8] %= 8;
            bytes[12] %= 160;
            bytes[13] %= 20;
        }
        if round % 4 == 0 {
            bytes[16..20].copy_from_slice(&((round % 11) as f32 / 10.0).to_le_bytes());
        }
        let record = EventRecord::from_bytes(&bytes);
        let mut expected = bytes;
        for at in offset::PADDING {
            expected[at] = 0;
        }
        assert_eq!(record.to_bytes(), expected);
        let index = match record.decode() {
            None => 0,
            Some(Event::NoteOn {
                note,
                channel,
                velocity,
                ..
            }) => {
                assert!(note <= 127 && channel <= 15);
                assert!(velocity > 0.0 && velocity <= 1.0);
                1
            }
            Some(Event::NoteOff { note, channel, .. }) => {
                assert!(note <= 127 && channel <= 15);
                2
            }
            Some(Event::Param { value, .. }) => {
                assert!(value.is_finite());
                3
            }
            Some(Event::Choke { .. }) => 4,
            Some(Event::AllNotesOff { .. }) => 5,
            Some(Event::Cancel { .. }) => 6,
        };
        decoded[index] += 1;
    }
    // Every outcome was reached many times.
    assert!(decoded.iter().all(|&count| count > 1_000), "{decoded:?}");
}

#[test]
fn decoding_follows_the_rules() {
    let on = EventRecord::note_on(3, 4, 127, 1.0).with_channel(15);
    assert_eq!(
        on.decode(),
        Some(Event::NoteOn {
            part: 4,
            channel: 15,
            note: 127,
            velocity: 1.0,
            accent: false,
        })
    );
    assert_eq!(on.decode().map(|event| event.part()), Some(4));
    assert_eq!(EventRecord { note: 128, ..on }.decode(), None);
    assert_eq!(EventRecord { channel: 16, ..on }.decode(), None);
    assert_eq!(
        EventRecord {
            velocity: 1.000_001,
            ..on
        }
        .decode(),
        None
    );
    assert_eq!(
        EventRecord {
            velocity: -0.0,
            ..on
        }
        .decode(),
        Some(Event::NoteOff {
            part: 4,
            channel: 15,
            note: 127
        })
    );
    let param = EventRecord::param(0, 65_535, ParamId(0xFFFF_FFFF), f64::MAX);
    assert_eq!(
        param.decode(),
        Some(Event::Param {
            part: 65_535,
            param: ParamId(0xFFFF_FFFF),
            value: f64::MAX
        })
    );
    assert_eq!(
        EventRecord::param(0, 0, ParamId(1), f64::NAN).decode(),
        None
    );
    assert_eq!(
        EventRecord::choke(0, 2).decode(),
        Some(Event::Choke { part: 2 })
    );
    assert_eq!(
        EventRecord::all_notes_off(0, 2).decode(),
        Some(Event::AllNotesOff { part: 2 })
    );
}
