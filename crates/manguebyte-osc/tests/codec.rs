//! The codec against the specification: its examples byte for byte, every
//! argument type, round trips, and each way a packet can be wrong.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use manguebyte_osc::{
    BundleEncoder, DecodeError, DecodeLimits, EncodeError, OscArg, OscArgRef, OscBundle, OscColor,
    OscMessage, OscMidi, OscPacket, OscPacketRef, OscTime, encode_message, encoded_message_len,
};

/// The first example message of the OSC 1.0 specification:
/// `/oscillator/4/frequency` with the float 440.0.
const SPEC_OSCILLATOR: [u8; 32] = [
    0x2f, 0x6f, 0x73, 0x63, 0x69, 0x6c, 0x6c, 0x61, 0x74, 0x6f, 0x72, 0x2f, 0x34, 0x2f, 0x66, 0x72,
    0x65, 0x71, 0x75, 0x65, 0x6e, 0x63, 0x79, 0x00, 0x2c, 0x66, 0x00, 0x00, 0x43, 0xdc, 0x00, 0x00,
];

/// The second example message of the OSC 1.0 specification: `/foo` with the
/// arguments 1000, -1, "hello", 1.234 and 5.678.
const SPEC_FOO: [u8; 40] = [
    0x2f, 0x66, 0x6f, 0x6f, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x69, 0x69, 0x73, 0x66, 0x66, 0x00, 0x00,
    0x00, 0x00, 0x03, 0xe8, 0xff, 0xff, 0xff, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x00, 0x00, 0x00,
    0x3f, 0x9d, 0xf3, 0xb6, 0x40, 0xb5, 0xb2, 0x2d,
];

fn oscillator() -> OscMessage {
    OscMessage::new("/oscillator/4/frequency").with(440.0f32)
}

fn foo() -> OscMessage {
    OscMessage::new("/foo")
        .with(1_000)
        .with(-1)
        .with("hello")
        .with(1.234f32)
        .with(5.678f32)
}

/// A message holding one argument of every type, and arrays inside arrays.
fn everything() -> OscMessage {
    OscMessage::new("/every/type")
        .with(-7)
        .with(0.5f32)
        .with("text")
        .with(vec![1u8, 2, 3, 4, 5])
        .with(-7_000_000_000i64)
        .with(-0.25f64)
        .with(OscTime::new(3_900_000_000, 0x8000_0000))
        .with(OscArg::Symbol("symbol".into()))
        .with('A')
        .with(OscColor::new(255, 128, 0, 64))
        .with(OscMidi::new(1, 0x90, 60, 100))
        .with(true)
        .with(false)
        .with(OscArg::Nil)
        .with(OscArg::Impulse)
        .with(vec![
            OscArg::Int(1),
            OscArg::Array(vec![OscArg::String("in".into()), OscArg::Blob(vec![9])]),
            OscArg::Array(Vec::new()),
        ])
}

fn nested_bundle() -> OscBundle {
    OscBundle::new(OscTime::new(3_900_000_000, 0))
        .with(oscillator())
        .with(
            OscBundle::new(OscTime::new(3_900_000_001, 1 << 31))
                .with(foo())
                .with(OscBundle::new(OscTime::IMMEDIATELY))
                .with(everything()),
        )
        .with(OscMessage::new("/last"))
}

fn packets() -> Vec<OscPacket> {
    vec![
        oscillator().into(),
        foo().into(),
        everything().into(),
        OscMessage::new("/").into(),
        OscBundle::default().into(),
        nested_bundle().into(),
    ]
}

#[test]
fn the_examples_in_the_specification_encode_byte_for_byte() {
    assert_eq!(oscillator().to_bytes().unwrap(), SPEC_OSCILLATOR);
    assert_eq!(foo().to_bytes().unwrap(), SPEC_FOO);

    // The same bytes come out of the encoder that takes borrowed arguments.
    let mut buffer = [0xAAu8; 64];
    let len = encode_message(
        &mut buffer,
        "/oscillator/4/frequency",
        &[OscArgRef::Float(440.0)],
    )
    .unwrap();
    assert_eq!(buffer[..len], SPEC_OSCILLATOR);
    let args = [
        OscArgRef::Int(1_000),
        OscArgRef::Int(-1),
        OscArgRef::String("hello"),
        OscArgRef::Float(1.234),
        OscArgRef::Float(5.678),
    ];
    let len = encode_message(&mut buffer, "/foo", &args).unwrap();
    assert_eq!(buffer[..len], SPEC_FOO);
    assert_eq!(encoded_message_len("/foo", &args), SPEC_FOO.len());
}

#[test]
fn the_examples_in_the_specification_decode_to_what_they_say() {
    assert_eq!(
        OscPacket::decode(&SPEC_OSCILLATOR),
        Ok(OscPacket::Message(oscillator()))
    );
    assert_eq!(OscPacket::decode(&SPEC_FOO), Ok(OscPacket::Message(foo())));

    // The decoder that does not allocate sees the same message.
    let OscPacketRef::Message(message) = OscPacketRef::parse(&SPEC_FOO).unwrap() else {
        panic!("not a message");
    };
    assert_eq!(message.address(), "/foo");
    assert_eq!(message.type_tags(), "iisff");
    assert_eq!(
        message.args().collect::<Vec<_>>(),
        [
            OscArgRef::Int(1_000),
            OscArgRef::Int(-1),
            OscArgRef::String("hello"),
            OscArgRef::Float(1.234),
            OscArgRef::Float(5.678),
        ]
    );
    assert_eq!(message.to_message(), foo());
}

/// Encodes `/x` with one argument and returns what follows the address.
fn tags_and_data(arg: OscArgRef<'_>) -> Vec<u8> {
    let mut buffer = [0u8; 64];
    let len = encode_message(&mut buffer, "/x", &[arg]).unwrap();
    assert_eq!(&buffer[..4], b"/x\0\0");
    buffer[4..len].to_vec()
}

#[test]
fn strings_are_padded_as_the_specification_shows() {
    // The two string examples of the specification: "OSC" takes four bytes
    // and "data" takes eight.
    assert_eq!(
        tags_and_data(OscArgRef::String("OSC")),
        [0x2c, 0x73, 0, 0, 0x4f, 0x53, 0x43, 0]
    );
    assert_eq!(
        tags_and_data(OscArgRef::String("data")),
        [0x2c, 0x73, 0, 0, 0x64, 0x61, 0x74, 0x61, 0, 0, 0, 0]
    );
    assert_eq!(tags_and_data(OscArgRef::String("")), b",s\0\0\0\0\0\0");
}

#[test]
fn every_argument_type_has_its_tag_and_its_bytes() {
    let cases: [(OscArgRef<'_>, char, &[u8]); 16] = [
        (OscArgRef::Int(-2), 'i', &[0xff, 0xff, 0xff, 0xfe]),
        (OscArgRef::Float(1.0), 'f', &[0x3f, 0x80, 0, 0]),
        (OscArgRef::String("ab"), 's', b"ab\0\0"),
        (
            OscArgRef::Blob(&[1, 2, 3, 4, 5]),
            'b',
            &[0, 0, 0, 5, 1, 2, 3, 4, 5, 0, 0, 0],
        ),
        (
            OscArgRef::Long(-2),
            'h',
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe],
        ),
        (OscArgRef::Double(1.0), 'd', &[0x3f, 0xf0, 0, 0, 0, 0, 0, 0]),
        (
            OscArgRef::Time(OscTime::new(1, 2)),
            't',
            &[0, 0, 0, 1, 0, 0, 0, 2],
        ),
        (OscArgRef::Symbol("abc"), 'S', b"abc\0"),
        (OscArgRef::Char('A'), 'c', &[0, 0, 0, 0x41]),
        (
            OscArgRef::Color(OscColor::new(1, 2, 3, 4)),
            'r',
            &[1, 2, 3, 4],
        ),
        (
            OscArgRef::Midi(OscMidi::new(1, 0x90, 60, 100)),
            'm',
            &[1, 0x90, 60, 100],
        ),
        (OscArgRef::Bool(true), 'T', &[]),
        (OscArgRef::Bool(false), 'F', &[]),
        (OscArgRef::Nil, 'N', &[]),
        (OscArgRef::Impulse, 'I', &[]),
        (OscArgRef::Char('\u{1F3B9}'), 'c', &[0, 0x01, 0xf3, 0xb9]),
    ];
    for (arg, tag, data) in cases {
        assert_eq!(arg.tag(), tag);
        let mut expected = vec![b',', tag as u8, 0, 0];
        expected.extend_from_slice(data);
        assert_eq!(tags_and_data(arg), expected, "{arg:?}");

        // And back.
        let mut packet = b"/x\0\0".to_vec();
        packet.extend_from_slice(&expected);
        let OscPacketRef::Message(message) = OscPacketRef::parse(&packet).unwrap() else {
            panic!("not a message");
        };
        assert_eq!(message.args().collect::<Vec<_>>(), [arg], "{arg:?}");
    }
}

#[test]
fn a_message_with_every_type_and_nested_arrays_round_trips() {
    let message = everything();
    let bytes = message.to_bytes().unwrap();
    assert_eq!(bytes.len() % 4, 0);
    assert_eq!(bytes.len(), message.encoded_len());
    assert_eq!(
        OscPacket::decode(&bytes),
        Ok(OscPacket::Message(message.clone()))
    );

    let OscPacketRef::Message(view) = OscPacketRef::parse(&bytes).unwrap() else {
        panic!("not a message");
    };
    assert_eq!(view.type_tags(), "ifsbhdtScrmTFNI[i[sb][]]");
    // Borrowed, the arrays are flat, with markers where they open and
    // close.
    let flat: Vec<_> = view.args().collect();
    assert_eq!(flat.len(), 24);
    assert_eq!(
        flat[15..],
        [
            OscArgRef::ArrayStart,
            OscArgRef::Int(1),
            OscArgRef::ArrayStart,
            OscArgRef::String("in"),
            OscArgRef::Blob(&[9]),
            OscArgRef::ArrayEnd,
            OscArgRef::ArrayStart,
            OscArgRef::ArrayEnd,
            OscArgRef::ArrayEnd,
        ]
    );

    // Encoding the flat arguments gives the bytes the owned message gave.
    let mut buffer = vec![0u8; bytes.len()];
    assert_eq!(
        encode_message(&mut buffer, "/every/type", &flat),
        Ok(bytes.len())
    );
    assert_eq!(buffer, bytes);
    assert_eq!(encoded_message_len("/every/type", &flat), bytes.len());
}

#[test]
fn a_bundle_is_laid_out_as_the_specification_says() {
    let bundle =
        OscBundle::new(OscTime::new(1, 0x8000_0000)).with(OscMessage::new("/a").with(true));
    #[rustfmt::skip]
    let expected = [
        b'#', b'b', b'u', b'n', b'd', b'l', b'e', 0,
        0, 0, 0, 1, 0x80, 0, 0, 0,
        0, 0, 0, 8,
        b'/', b'a', 0, 0, b',', b'T', 0, 0,
    ];
    assert_eq!(bundle.to_bytes().unwrap(), expected);
    assert_eq!(OscPacket::decode(&expected), Ok(OscPacket::Bundle(bundle)));
    assert_eq!(
        OscBundle::default().to_bytes().unwrap(),
        b"#bundle\0\0\0\0\0\0\0\0\x01"
    );
}

#[test]
fn every_packet_round_trips_and_takes_the_length_it_says() {
    for packet in packets() {
        let bytes = packet.to_bytes().unwrap();
        assert_eq!(bytes.len(), packet.encoded_len(), "{packet:?}");
        assert_eq!(bytes.len() % 4, 0);
        assert_eq!(OscPacket::decode(&bytes).as_ref(), Ok(&packet));
        assert_eq!(OscPacketRef::parse(&bytes).unwrap().to_packet(), packet);

        // A buffer of exactly the right size is enough, and every smaller
        // one is refused with the size that is needed.
        let mut exact = vec![0u8; bytes.len()];
        assert_eq!(packet.encode(&mut exact), Ok(bytes.len()));
        assert_eq!(exact, bytes);
        for len in 0..bytes.len() {
            assert_eq!(
                packet.encode(&mut exact[..len]),
                Err(EncodeError::BufferTooSmall {
                    needed: bytes.len()
                })
            );
        }
    }
}

#[test]
fn nested_bundles_keep_their_shape_and_their_times() {
    let bundle = nested_bundle();
    let bytes = bundle.to_bytes().unwrap();
    assert_eq!(bytes.len(), bundle.encoded_len());

    let OscPacketRef::Bundle(outer) = OscPacketRef::parse(&bytes).unwrap() else {
        panic!("not a bundle");
    };
    assert_eq!(outer.time(), OscTime::new(3_900_000_000, 0));
    let elements: Vec<_> = outer.elements().collect();
    assert_eq!(elements.len(), 3);
    let OscPacketRef::Bundle(inner) = elements[1] else {
        panic!("not a bundle");
    };
    assert_eq!(inner.time(), OscTime::new(3_900_000_001, 1 << 31));
    let inner_elements: Vec<_> = inner.elements().collect();
    assert_eq!(inner_elements.len(), 3);
    let OscPacketRef::Bundle(innermost) = inner_elements[1] else {
        panic!("not a bundle");
    };
    assert!(innermost.time().is_immediate());
    assert_eq!(innermost.elements().count(), 0);
    assert_eq!(outer.to_bundle(), bundle);
}

#[test]
fn the_bundle_encoder_writes_what_the_owned_types_write() {
    let owned = OscBundle::new(OscTime::new(10, 20))
        .with(OscMessage::new("/a").with(1).with("one"))
        .with(
            OscBundle::new(OscTime::new(30, 40))
                .with(OscMessage::new("/b").with(2.0f32))
                .with(OscBundle::new(OscTime::new(50, 60)).with(OscMessage::new("/c"))),
        )
        .with(OscMessage::new("/d").with(vec![OscArg::Int(3)]));
    let expected = owned.to_bytes().unwrap();

    let build = |buffer: &mut [u8]| {
        let mut bundle = BundleEncoder::new(buffer, OscTime::new(10, 20));
        bundle.message("/a", &[OscArgRef::Int(1), OscArgRef::String("one")])?;
        bundle.begin_bundle(OscTime::new(30, 40))?;
        bundle.message("/b", &[OscArgRef::Float(2.0)])?;
        bundle.begin_bundle(OscTime::new(50, 60))?;
        bundle.message("/c", &[])?;
        bundle.end_bundle()?;
        bundle.end_bundle()?;
        bundle.message(
            "/d",
            &[
                OscArgRef::ArrayStart,
                OscArgRef::Int(3),
                OscArgRef::ArrayEnd,
            ],
        )?;
        bundle.finish()
    };
    let mut buffer = vec![0u8; expected.len()];
    assert_eq!(build(&mut buffer), Ok(expected.len()));
    assert_eq!(buffer, expected);
    for len in 0..expected.len() {
        assert_eq!(
            build(&mut buffer[..len]),
            Err(EncodeError::BufferTooSmall {
                needed: expected.len()
            })
        );
    }
}

#[test]
fn the_bundle_encoder_reports_misuse_and_survives_a_bad_message() {
    let mut buffer = [0u8; 256];

    let mut bundle = BundleEncoder::new(&mut buffer, OscTime::IMMEDIATELY);
    assert_eq!(bundle.end_bundle(), Err(EncodeError::UnbalancedBundle));
    for _ in 0..BundleEncoder::MAX_DEPTH {
        assert_eq!(bundle.begin_bundle(OscTime::IMMEDIATELY), Ok(()));
    }
    assert_eq!(
        bundle.begin_bundle(OscTime::IMMEDIATELY),
        Err(EncodeError::TooDeep)
    );
    assert_eq!(bundle.finish(), Err(EncodeError::UnbalancedBundle));

    // A message that cannot be encoded leaves no trace in the bundle.
    let mut bundle = BundleEncoder::new(&mut buffer, OscTime::IMMEDIATELY);
    bundle.message("/first", &[OscArgRef::Int(1)]).unwrap();
    assert_eq!(
        bundle.message("no-slash", &[]),
        Err(EncodeError::InvalidAddress)
    );
    assert_eq!(
        bundle.message("/s", &[OscArgRef::Int(7), OscArgRef::String("a\0b")]),
        Err(EncodeError::NulInString)
    );
    bundle.message("/second", &[OscArgRef::Int(2)]).unwrap();
    let len = bundle.finish().unwrap();
    assert_eq!(
        OscPacket::decode(&buffer[..len]),
        Ok(OscPacket::Bundle(
            OscBundle::default()
                .with(OscMessage::new("/first").with(1))
                .with(OscMessage::new("/second").with(2))
        ))
    );
}

#[test]
fn blobs_and_strings_of_every_length_are_padded_to_four() {
    for len in 0..=17usize {
        let blob: Vec<u8> = (0..len as u8)
            .map(|byte| byte.wrapping_mul(37) | 1)
            .collect();
        let text: String = "abcdefghijklmnopqrstuvwxyz".chars().take(len).collect();
        let message = OscMessage::new("/pad")
            .with(blob.clone())
            .with(text.clone())
            .with(7);
        let bytes = message.to_bytes().unwrap();
        // Address 8, type tags 8, the blob's size 4, the integer 4.
        let expected = 24 + len.div_ceil(4) * 4 + (len / 4 + 1) * 4;
        assert_eq!(bytes.len(), expected, "length {len}");
        assert_eq!(bytes.len(), message.encoded_len());
        assert_eq!(OscPacket::decode(&bytes), Ok(OscPacket::Message(message)));
    }
}

#[test]
fn floats_keep_every_bit() {
    let singles = [
        0x7fc0_0001u32,
        0xffc1_2345,
        0x7f80_0000,
        0xff80_0000,
        0x8000_0000,
        0x0000_0001,
        0x7f7f_ffff,
    ];
    let doubles = [
        0x7ff8_0000_0000_0001u64,
        0xfff0_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x0000_0000_0000_0001,
        0x400921fb54442d18,
    ];
    let mut message = OscMessage::new("/bits");
    for bits in singles {
        message = message.with(f32::from_bits(bits));
    }
    for bits in doubles {
        message = message.with(f64::from_bits(bits));
    }
    let bytes = message.to_bytes().unwrap();
    let OscPacketRef::Message(view) = OscPacketRef::parse(&bytes).unwrap() else {
        panic!("not a message");
    };
    let mut args = view.args();
    for bits in singles {
        assert_eq!(args.next().unwrap().as_f32().unwrap().to_bits(), bits);
    }
    for bits in doubles {
        assert_eq!(args.next().unwrap().as_f64().unwrap().to_bits(), bits);
    }
    assert_eq!(args.next(), None);
    // Owned values carry the bits too: encoding what was decoded gives the
    // bytes that were decoded.
    assert_eq!(
        OscPacket::decode(&bytes).unwrap().to_bytes().unwrap(),
        bytes
    );
}

#[test]
fn extreme_integers_and_characters_round_trip() {
    let message = OscMessage::new("/limits")
        .with(i32::MIN)
        .with(i32::MAX)
        .with(i64::MIN)
        .with(i64::MAX)
        .with('\0')
        .with('\u{10FFFF}')
        .with(OscTime::from_bits(u64::MAX))
        .with(OscTime::IMMEDIATELY)
        .with("caf\u{e9} \u{1F3B9}");
    let bytes = message.to_bytes().unwrap();
    assert_eq!(OscPacket::decode(&bytes), Ok(OscPacket::Message(message)));
}

#[test]
fn argument_accessors_return_only_their_own_type() {
    assert_eq!(OscArgRef::Int(3).as_i32(), Some(3));
    assert_eq!(OscArgRef::Float(3.0).as_i32(), None);
    assert_eq!(OscArgRef::Long(3).as_i64(), Some(3));
    assert_eq!(OscArgRef::Double(0.5).as_f64(), Some(0.5));
    assert_eq!(OscArgRef::String("s").as_str(), Some("s"));
    assert_eq!(OscArgRef::Symbol("s").as_str(), Some("s"));
    assert_eq!(OscArgRef::Blob(&[1]).as_blob(), Some(&[1u8][..]));
    assert_eq!(OscArgRef::Bool(true).as_bool(), Some(true));
    assert_eq!(OscArgRef::Nil.as_bool(), None);
    // `number` takes whichever numeric type a sender chose.
    for arg in [
        OscArgRef::Int(3),
        OscArgRef::Float(3.0),
        OscArgRef::Long(3),
        OscArgRef::Double(3.0),
    ] {
        assert_eq!(arg.number(), Some(3.0));
    }
    assert_eq!(OscArgRef::String("3").number(), None);
    assert_eq!(OscArgRef::from(1.5f32), OscArgRef::Float(1.5));
    assert_eq!(OscArgRef::from("x"), OscArgRef::String("x"));
}

#[test]
fn a_message_without_a_type_tag_string_has_no_arguments() {
    // The specification asks receivers to accept this from old senders.
    assert_eq!(
        OscPacket::decode(b"/ping\0\0\0"),
        Ok(OscPacket::Message(OscMessage::new("/ping")))
    );
    // Arguments without type tags cannot be read.
    assert_eq!(
        OscPacket::decode(b"/ping\0\0\0\0\0\0\x01"),
        Err(DecodeError::MissingTypeTags { offset: 8 })
    );
}

#[test]
fn each_thing_wrong_with_a_packet_has_its_own_error() {
    let cases: &[(&[u8], DecodeError)] = &[
        (b"/a\0", DecodeError::Misaligned { len: 3 }),
        (b"", DecodeError::Empty { offset: 0 }),
        (
            b"abcd",
            DecodeError::UnknownPacket {
                first: b'a',
                offset: 0,
            },
        ),
        (
            b"\0\0\0\0",
            DecodeError::UnknownPacket {
                first: 0,
                offset: 0,
            },
        ),
        (
            b"#bundlX\0\0\0\0\0\0\0\0\x01",
            DecodeError::InvalidBundleHeader { offset: 0 },
        ),
        (
            b"#bundle!\0\0\0\0\0\0\0\x01",
            DecodeError::InvalidBundleHeader { offset: 0 },
        ),
        (b"#bun", DecodeError::Truncated { offset: 4 }),
        (b"#bundle\0\0\0\0\0", DecodeError::Truncated { offset: 12 }),
        (b"/a\0\0,i\0\0", DecodeError::Truncated { offset: 8 }),
        (
            b"/a\0\0,h\0\0\0\0\0\0",
            DecodeError::Truncated { offset: 12 },
        ),
        (b"/abc", DecodeError::UnterminatedString { offset: 0 }),
        (b"/a\0\0,sss", DecodeError::UnterminatedString { offset: 4 }),
        (
            b"/a\0\0,s\0\0abcd",
            DecodeError::UnterminatedString { offset: 8 },
        ),
        (b"/a\0x,\0\0\0", DecodeError::NonZeroPadding { offset: 2 }),
        (
            b"/a\0\0,\0\0\x01",
            DecodeError::NonZeroPadding { offset: 5 },
        ),
        (
            b"/a\0\0,s\0\0ab\0c",
            DecodeError::NonZeroPadding { offset: 10 },
        ),
        (
            b"/a\0\0,b\0\0\0\0\0\x01A\x01\0\0",
            DecodeError::NonZeroPadding { offset: 13 },
        ),
        (b"/\xff\0\0", DecodeError::InvalidString { offset: 0 }),
        (
            b"/a\0\0,s\0\0\xc3\x28\0\0",
            DecodeError::InvalidString { offset: 8 },
        ),
        (b"/a\0\0abc\0", DecodeError::MissingTypeTags { offset: 4 }),
        (
            b"/a\0\0,ix\0\0\0\0\x01",
            DecodeError::UnknownTypeTag {
                tag: b'x',
                index: 1,
            },
        ),
        (
            b"/a\0\0,\xc3\xa9\0",
            DecodeError::UnknownTypeTag {
                tag: 0xc3,
                index: 0,
            },
        ),
        (b"/a\0\0,[\0\0", DecodeError::UnbalancedArray),
        (b"/a\0\0,]\0\0", DecodeError::UnbalancedArray),
        (b"/a\0\0,[]]\0\0\0\0", DecodeError::UnbalancedArray),
        (
            b"/a\0\0,b\0\0\xff\xff\xff\xff",
            DecodeError::InvalidBlobSize {
                size: -1,
                offset: 8,
            },
        ),
        (
            b"/a\0\0,b\0\0\0\0\0\x08abcd",
            DecodeError::InvalidBlobSize { size: 8, offset: 8 },
        ),
        (
            b"/a\0\0,b\0\0\x7f\xff\xff\xffabcd",
            DecodeError::InvalidBlobSize {
                size: i32::MAX,
                offset: 8,
            },
        ),
        (
            b"/a\0\0,c\0\0\0\0\xd8\0",
            DecodeError::InvalidChar {
                value: 0xd800,
                offset: 8,
            },
        ),
        (
            b"/a\0\0,c\0\0\0\x11\0\0",
            DecodeError::InvalidChar {
                value: 0x11_0000,
                offset: 8,
            },
        ),
        (
            b"/a\0\0,i\0\0\0\0\0\x01\0\0\0\x02",
            DecodeError::TrailingData { offset: 12 },
        ),
        (
            b"/a\0\0,\0\0\0\0\0\0\0",
            DecodeError::TrailingData { offset: 8 },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\x05/a\0\0,\0\0\0",
            DecodeError::InvalidElementSize {
                size: 5,
                offset: 16,
            },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\xff\xff\xff\xfc/a\0\0",
            DecodeError::InvalidElementSize {
                size: -4,
                offset: 16,
            },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\x10/a\0\0,\0\0\0",
            DecodeError::InvalidElementSize {
                size: 16,
                offset: 16,
            },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\0",
            DecodeError::Empty { offset: 20 },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0",
            DecodeError::Misaligned { len: 18 },
        ),
        // An error inside an element is reported where it is in the whole
        // packet: the second element begins at byte 32.
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\x08/a\0\0,\0\0\0\0\0\0\x08/b\0\0,i\0\0",
            DecodeError::Truncated { offset: 40 },
        ),
        (
            b"#bundle\0\0\0\0\0\0\0\0\x01\0\0\0\x08/a\0\0,\0\0\0\0\0\0\x0c/b\0\0,s\0\0ab\0c",
            DecodeError::NonZeroPadding { offset: 42 },
        ),
    ];
    for (bytes, expected) in cases {
        assert_eq!(
            OscPacket::decode(bytes),
            Err(*expected),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
        assert_eq!(OscPacketRef::parse(bytes), Err(*expected));
        // Every error can be shown to a person.
        assert!(!expected.to_string().is_empty());
    }
}

/// `depth` bundles, each inside the one before.
fn bundles_nested(depth: usize) -> Vec<u8> {
    let mut packet = Vec::new();
    for _ in 0..depth {
        let mut outer = b"#bundle\0\0\0\0\0\0\0\0\x01".to_vec();
        if !packet.is_empty() {
            outer.extend_from_slice(&(packet.len() as u32).to_be_bytes());
            outer.extend_from_slice(&packet);
        }
        packet = outer;
    }
    packet
}

/// A message whose one argument is `depth` arrays, each inside the one
/// before.
fn arrays_nested(depth: usize) -> Vec<u8> {
    let mut tags = String::from(",");
    tags.push_str(&"[".repeat(depth));
    tags.push_str(&"]".repeat(depth));
    let mut packet = b"/a\0\0".to_vec();
    packet.extend_from_slice(tags.as_bytes());
    packet.resize(packet.len() + 4 - tags.len() % 4, 0);
    packet
}

#[test]
fn nesting_is_accepted_up_to_the_limit_and_refused_beyond_it() {
    let defaults = DecodeLimits::default();
    assert_eq!(
        (defaults.max_bundle_depth, defaults.max_array_depth),
        (16, 16)
    );
    assert!(OscPacket::decode(&bundles_nested(16)).is_ok());
    assert_eq!(
        OscPacket::decode(&bundles_nested(17)),
        Err(DecodeError::BundleTooDeep { max: 16 })
    );
    assert!(OscPacket::decode(&arrays_nested(16)).is_ok());
    assert_eq!(
        OscPacket::decode(&arrays_nested(17)),
        Err(DecodeError::ArrayTooDeep { max: 16 })
    );

    let strict = DecodeLimits::default()
        .with_max_bundle_depth(0)
        .with_max_array_depth(0);
    assert_eq!(
        OscPacket::decode_with(&bundles_nested(1), &strict),
        Err(DecodeError::BundleTooDeep { max: 0 })
    );
    assert_eq!(
        OscPacket::decode_with(&arrays_nested(1), &strict),
        Err(DecodeError::ArrayTooDeep { max: 0 })
    );
    assert!(OscPacket::decode_with(&SPEC_FOO, &strict).is_ok());

    // Whatever is asked for, no more than 64 levels are followed, so that
    // a packet cannot run the decoder out of stack.
    let unlimited = DecodeLimits::default()
        .with_max_bundle_depth(usize::MAX)
        .with_max_array_depth(usize::MAX);
    assert!(OscPacket::decode_with(&bundles_nested(64), &unlimited).is_ok());
    assert_eq!(
        OscPacket::decode_with(&bundles_nested(3_000), &unlimited),
        Err(DecodeError::BundleTooDeep { max: 64 })
    );
    assert!(OscPacket::decode_with(&arrays_nested(64), &unlimited).is_ok());
    assert_eq!(
        OscPacket::decode_with(&arrays_nested(30_000), &unlimited),
        Err(DecodeError::ArrayTooDeep { max: 64 })
    );
}

#[test]
fn what_cannot_be_sent_is_refused_when_encoding() {
    let mut buffer = [0u8; 64];
    assert_eq!(
        OscMessage::new("no/slash").encode(&mut buffer),
        Err(EncodeError::InvalidAddress)
    );
    assert_eq!(
        OscMessage::new("").encode(&mut buffer),
        Err(EncodeError::InvalidAddress)
    );
    assert_eq!(
        OscMessage::new("/a\0b").encode(&mut buffer),
        Err(EncodeError::NulInString)
    );
    assert_eq!(
        OscMessage::new("/a").with("x\0").encode(&mut buffer),
        Err(EncodeError::NulInString)
    );
    assert_eq!(
        OscMessage::new("/a")
            .with(OscArg::Symbol("\0".into()))
            .encode(&mut buffer),
        Err(EncodeError::NulInString)
    );
    assert_eq!(
        OscBundle::default()
            .with(OscMessage::new("oops"))
            .encode(&mut buffer),
        Err(EncodeError::InvalidAddress)
    );
    for unbalanced in [
        &[OscArgRef::ArrayStart][..],
        &[OscArgRef::ArrayEnd],
        &[OscArgRef::ArrayEnd, OscArgRef::ArrayStart],
        &[
            OscArgRef::ArrayStart,
            OscArgRef::ArrayEnd,
            OscArgRef::ArrayEnd,
        ],
    ] {
        assert_eq!(
            encode_message(&mut buffer, "/a", unbalanced),
            Err(EncodeError::UnbalancedArray)
        );
    }
    assert!(!EncodeError::TooLarge.to_string().is_empty());
}

#[test]
fn encoding_to_a_vector_appends_and_uses_the_room_it_is_given() {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"head");
    let before = (out.as_ptr(), out.capacity());

    oscillator().encode_to_vec(&mut out).unwrap();
    OscPacket::from(foo()).encode_to_vec(&mut out).unwrap();
    OscBundle::default().encode_to_vec(&mut out).unwrap();
    // The vector had room, so it was not moved or grown.
    assert_eq!((out.as_ptr(), out.capacity()), before);
    assert_eq!(&out[..4], b"head");
    assert_eq!(out[4..36], SPEC_OSCILLATOR);
    assert_eq!(out[36..76], SPEC_FOO);
    assert_eq!(out.len(), 76 + 16);

    // A failure leaves the vector as it was.
    let len = out.len();
    assert_eq!(
        OscMessage::new("/a").with("\0").encode_to_vec(&mut out),
        Err(EncodeError::NulInString)
    );
    assert_eq!(out.len(), len);
}

#[test]
fn the_time_tag_format_is_the_one_ntp_uses() {
    assert_eq!(OscTime::IMMEDIATELY.to_bits(), 1);
    assert_eq!(OscTime::default(), OscTime::IMMEDIATELY);
    assert!(OscTime::IMMEDIATELY.is_immediate());
    assert_eq!(OscTime::IMMEDIATELY.to_system_time(), None);

    let time = OscTime::new(0x1234_5678, 0x9abc_def0);
    assert_eq!(time.to_bits(), 0x1234_5678_9abc_def0);
    assert_eq!(
        (time.seconds(), time.fraction()),
        (0x1234_5678, 0x9abc_def0)
    );
    assert_eq!(OscTime::from_bits(time.to_bits()), time);

    // 1 January 1970 is 2,208,988,800 seconds after 1 January 1900.
    let epoch = OscTime::new(2_208_988_800, 0);
    assert_eq!(epoch.to_system_time(), Some(UNIX_EPOCH));
    assert_eq!(OscTime::from_system_time(UNIX_EPOCH), Some(epoch));
    // 1 January 2000, 00:00 UTC: 946,684,800 seconds of Unix time.
    assert_eq!(
        OscTime::new(3_155_673_600, 1 << 30).to_system_time(),
        Some(UNIX_EPOCH + Duration::from_millis(946_684_800_250))
    );

    assert!(OscTime::new(1, 0) < OscTime::new(1, 1));
    assert!(OscTime::new(1, u32::MAX) < OscTime::new(2, 0));
}

#[test]
fn system_times_round_trip_and_the_range_has_edges() {
    for (seconds, nanos) in [
        (0u64, 0u32),
        (1, 1_000),
        (1_700_000_000, 123_456_700),
        (1_700_000_000, 999_999_900),
        (2_085_978_495, 500_000_000),
        (2_085_978_496, 0),
        (2_085_978_496, 100),
        (4_000_000_000, 250_000_000),
    ] {
        let time = UNIX_EPOCH + Duration::new(seconds, nanos);
        let tag = OscTime::from_system_time(time).unwrap();
        assert_eq!(tag.to_system_time(), Some(time), "{seconds}.{nanos:09}");
    }
    // Before 1970 as well.
    let before = UNIX_EPOCH - Duration::new(1_000, 250_000_000);
    let tag = OscTime::from_system_time(before).unwrap();
    assert_eq!(tag, OscTime::new(2_208_987_799, 3 << 30));
    assert_eq!(tag.to_system_time(), Some(before));

    // The seconds wrap on 7 February 2036 at 06:28:16 UTC, which is
    // 2,085,978,496 seconds of Unix time. Values on both sides of it are
    // read as the dates they stand for.
    let rollover = UNIX_EPOCH + Duration::from_secs(2_085_978_496);
    assert_eq!(
        OscTime::from_system_time(rollover),
        Some(OscTime::new(0, 0))
    );
    assert_eq!(OscTime::new(0, 0).to_system_time(), Some(rollover));
    assert_eq!(
        OscTime::new(u32::MAX, 0).to_system_time(),
        Some(rollover - Duration::from_secs(1))
    );

    // The range a time tag can name is 2^32 seconds long and begins 2^31
    // seconds after 1900.
    let first = UNIX_EPOCH - Duration::from_secs(61_505_152);
    let step = Duration::from_micros(1);
    assert_eq!(
        OscTime::from_system_time(first),
        Some(OscTime::new(1 << 31, 0))
    );
    assert_eq!(OscTime::from_system_time(first - step), None);
    let end = UNIX_EPOCH + Duration::from_secs(4_233_462_144);
    assert_eq!(OscTime::from_system_time(end), None);
    let last = OscTime::from_system_time(end - step).unwrap();
    assert_eq!(last.seconds(), (1 << 31) - 1);
    assert_eq!(last.to_system_time(), Some(end - step));
}

#[test]
fn differences_and_sums_work_across_the_rollover() {
    let before = OscTime::new(u32::MAX, 1 << 31);
    let after = OscTime::new(0, 1 << 31);
    assert_eq!(after.seconds_since(before), 1.0);
    assert_eq!(before.seconds_since(after), -1.0);
    assert_eq!(before + Duration::from_secs(1), after);
    assert_eq!(after - Duration::from_secs(1), before);

    let time = OscTime::new(3_900_000_000, 0);
    assert_eq!(
        time + Duration::from_millis(1_500),
        OscTime::new(3_900_000_001, 1 << 31)
    );
    assert_eq!(
        time - Duration::from_millis(1_500),
        OscTime::new(3_899_999_998, 1 << 31)
    );
    for nanos in [1u64, 20_833, 999_999_999, 1_000_000_001, 86_400_000_000_000] {
        let duration = Duration::from_nanos(nanos);
        let seconds = duration.as_secs_f64();
        // One unit of a time tag is under 233 picoseconds.
        assert!(((time + duration).seconds_since(time) - seconds).abs() < 2.4e-10);
        assert!(((time - duration).seconds_since(time) + seconds).abs() < 2.4e-10);
    }
}

/// Adding a duration and subtracting it again gives back the time it
/// started from: both round to the nearest unit of a time tag, the same
/// way. Before they did, adding rounded up and subtracting rounded down,
/// and 1, 2, 3, 10, 100 and 999 milliseconds all came back one unit late.
#[test]
fn subtracting_a_duration_undoes_adding_it() {
    let times = [
        OscTime::new(3_900_000_000, 0),
        OscTime::new(3_900_000_000, 0x8765_4321),
        // Just before the 2036 rollover, and just after it.
        OscTime::new(u32::MAX, 0xF000_0000),
        OscTime::new(0, 5),
        // Where the 64 bits wrap around.
        OscTime::new(u32::MAX, u32::MAX),
    ];
    let durations = [
        Duration::from_nanos(1),
        Duration::from_nanos(3),
        Duration::from_nanos(20_833),
        Duration::from_millis(1),
        Duration::from_millis(2),
        Duration::from_millis(3),
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(999),
        Duration::new(1, 500_000_000),
        Duration::new(86_400, 999_999_999),
        Duration::from_secs_f64(512.0 / 48_000.0),
    ];
    for time in times {
        for duration in durations {
            assert_eq!((time + duration) - duration, time, "{time:?} {duration:?}");
            assert_eq!((time - duration) + duration, time, "{time:?} {duration:?}");
            // The two directions move by the same number of units, and that
            // number is the nearest to the exact one: within half a unit.
            let up = (time + duration).to_bits().wrapping_sub(time.to_bits());
            let down = time.to_bits().wrapping_sub((time - duration).to_bits());
            assert_eq!(up, down);
            let exact = duration.as_nanos() << 32;
            let off = (u128::from(up) * 1_000_000_000).abs_diff(exact);
            assert!(off <= 500_000_000, "{duration:?}: {up} units");
        }
    }
}

/// `add_frames` names the start of a block exactly: with a whole number as
/// the sample rate it is the nearest unit to frames / rate seconds, however
/// far into the stream, and it never drifts the way adding one block's
/// duration after another does.
#[test]
fn adding_frames_is_exact_to_the_unit() {
    let start = OscTime::new(3_900_000_000, 0x0123_4567);
    for rate in [
        1u64, 8_000, 44_100, 48_000, 65_536, 96_000, 192_000, 384_000,
    ] {
        for frames in [
            0u64,
            1,
            479,
            480,
            rate - 1,
            rate,
            rate + 1,
            1 << 31,
            // A day, and a hundred years.
            rate * 86_400 + 7,
            rate * 3_155_760_000,
        ] {
            let ticks = start
                .add_frames(frames, rate as f64)
                .to_bits()
                .wrapping_sub(start.to_bits());
            let exact = u128::from(frames) << 32;
            let nearest = (exact + u128::from(rate) / 2) / u128::from(rate);
            assert_eq!(
                u128::from(ticks),
                nearest % (1 << 64),
                "{rate} Hz, {frames}"
            );
        }
        // One block after another, counted in frames, lands where one jump
        // does; counted in durations it drifts.
        let block = 441u64;
        let mut by_duration = start;
        for _ in 0..rate {
            by_duration = by_duration + Duration::from_nanos(block * 1_000_000_000 / rate);
        }
        let by_frames = start.add_frames(block * rate, rate as f64);
        assert_eq!(by_frames, start + Duration::from_secs(block));
        if (block * 1_000_000_000) % rate != 0 {
            assert_ne!(by_duration, by_frames, "{rate} Hz");
        }
    }
    // A rate that is not a whole number is as close as a double allows.
    let rate = 44_100.0 * 1_001.0 / 1_000.0;
    let ticks = start.add_frames(1_000_000, rate).seconds_since(start);
    assert!((ticks - 1_000_000.0 / rate).abs() < 1e-9);
    // Rates that make no sense count as one frame a second.
    for rate in [0.0, -48_000.0, f64::NAN, 0.5] {
        assert_eq!(
            start.add_frames(3, rate),
            start + Duration::from_secs(3),
            "{rate}"
        );
    }
}

#[test]
fn the_clock_reads_the_present() {
    let before = SystemTime::now();
    let now = OscTime::now();
    let after = SystemTime::now();
    assert!(!now.is_immediate());
    let read = now.to_system_time().unwrap();
    // Allow for a clock that is being adjusted while the test runs.
    let slack = Duration::from_secs(5);
    assert!(read + slack >= before && read <= after + slack);
}
