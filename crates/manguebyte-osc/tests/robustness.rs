//! Damaged and hostile input: packets cut short at every byte, packets with
//! random damage, random bytes, and byte streams with broken framing. None
//! of it may panic, and what is accepted must be exactly what would have
//! been sent.

use manguebyte_osc::slip::{self, END, ESC, ESC_END, ESC_ESC, SlipDecoder, SlipError};
use manguebyte_osc::{
    DecodeError, EncodeError, OscArg, OscArgRef, OscBundle, OscColor, OscMessage, OscMidi,
    OscPacket, OscPacketRef, OscTime,
};

/// A small deterministic generator (xorshift64*), so that a failure can be
/// reproduced from the seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() >> 33) as usize % bound
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 56) as u8
    }
}

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

/// Valid packets of every shape, to be damaged.
fn corpus() -> Vec<Vec<u8>> {
    let example = OscMessage::new("/foo")
        .with(1_000)
        .with(-1)
        .with("hello")
        .with(1.234f32)
        .with(5.678f32);
    let nested = OscBundle::new(OscTime::new(3_900_000_000, 0))
        .with(OscMessage::new("/oscillator/4/frequency").with(440.0f32))
        .with(
            OscBundle::new(OscTime::new(3_900_000_001, 1 << 31))
                .with(example.clone())
                .with(OscBundle::new(OscTime::IMMEDIATELY))
                .with(everything()),
        )
        .with(OscMessage::new("/last"));
    let packets: Vec<OscPacket> = vec![
        OscMessage::new("/oscillator/4/frequency")
            .with(440.0f32)
            .into(),
        example.into(),
        everything().into(),
        OscMessage::new("/").into(),
        OscMessage::new("/strings")
            .with("")
            .with("a")
            .with("ab")
            .with("abc")
            .with("abcd")
            .into(),
        OscBundle::default().into(),
        OscBundle::default()
            .with(OscMessage::new("/a"))
            .with(OscMessage::new("/b").with(1))
            .into(),
        nested.into(),
    ];
    packets
        .iter()
        .map(|packet| packet.to_bytes().unwrap())
        .collect()
}

/// Walks everything a checked packet holds and returns how many messages
/// and arguments were seen. The point is that walking does not panic.
fn walk(packet: &OscPacketRef<'_>) -> usize {
    match packet {
        OscPacketRef::Message(message) => {
            assert!(message.address().starts_with('/'));
            let args = message.args().count();
            // One tag per argument.
            assert_eq!(args, message.type_tags().len());
            1 + args
        }
        OscPacketRef::Bundle(bundle) => bundle.elements().map(|element| walk(&element)).sum(),
    }
}

/// Decodes `bytes` both ways and checks what must hold for any input at
/// all. Returns whether the packet was accepted.
///
/// The two decoders are not independent of each other: `OscPacket::decode`
/// checks a packet with `OscPacketRef::parse` and then copies what it
/// found, so they accept and refuse the same packets, with the same error,
/// by construction, and comparing them checks only the copy. What is
/// checked here independently of the parser is that an accepted packet,
/// written out again by the encoder, which is separate code, gives back
/// exactly the bytes received. The parser itself is checked against the
/// specification with packets built by hand in `codec.rs`.
fn examine(bytes: &[u8]) -> bool {
    let view = OscPacketRef::parse(bytes);
    let owned = OscPacket::decode(bytes);
    match (view, owned) {
        (Ok(view), Ok(owned)) => {
            walk(&view);
            // What was accepted is a packet that can be sent again. If the
            // bytes it encodes to are as long as what came in, they are the
            // same bytes: strict decoding leaves one way to write a packet.
            // (They are longer only when a message came without its type
            // tag string, which is accepted but never written.)
            let again = owned.to_bytes().unwrap();
            assert_eq!(again.len(), owned.encoded_len());
            assert!(again.len() >= bytes.len());
            if again.len() == bytes.len() {
                assert_eq!(again, bytes);
            }
            // And decoding that is stable.
            assert_eq!(
                OscPacket::decode(&again).unwrap().to_bytes().unwrap(),
                again
            );
            true
        }
        (Err(view), Err(owned)) => {
            assert_eq!(view, owned);
            if bytes.len() % 4 != 0 {
                assert_eq!(view, DecodeError::Misaligned { len: bytes.len() });
            }
            false
        }
        (view, owned) => {
            panic!("the owned copy does not follow the parser: {view:?} and {owned:?}")
        }
    }
}

#[test]
fn a_packet_cut_short_at_any_byte_is_an_error_or_a_shorter_packet() {
    let (mut cuts, mut accepted) = (0, 0);
    for packet in corpus() {
        assert!(examine(&packet));
        for len in 0..packet.len() {
            cuts += 1;
            if examine(&packet[..len]) {
                accepted += 1;
                // Only a cut on a boundary of four can leave something
                // that is still a packet.
                assert_eq!(len % 4, 0);
            }
        }
    }
    println!("{cuts} cuts, {accepted} of them still packets");
    assert!(cuts > 500);
    // A bundle cut between two elements is a shorter bundle, and a message
    // cut after its address is a message without arguments.
    assert!(accepted > 0 && accepted < cuts / 10);
}

#[test]
fn a_packet_with_bytes_missing_from_its_middle_never_panics() {
    for packet in corpus() {
        for start in 0..packet.len() {
            for removed in [1, 2, 3, 4, 8] {
                let mut damaged = packet.clone();
                let end = (start + removed).min(damaged.len());
                damaged.drain(start..end);
                examine(&damaged);
            }
        }
    }
}

#[test]
fn random_damage_never_panics_and_never_decodes_to_something_else() {
    const INTERESTING: [u8; 12] = [
        0, 1, 4, 0x7f, 0x80, 0xff, b'/', b'#', b',', b'[', b']', b's',
    ];
    let corpus = corpus();
    let mut random = Random(0x05C0_FFEE_D00D_F00D);
    let (mut rounds, mut accepted) = (0u32, 0u32);
    for _ in 0..120_000 {
        let mut packet = corpus[random.below(corpus.len())].clone();
        for _ in 0..1 + random.below(3) {
            let at = random.below(packet.len());
            match random.below(6) {
                0 => packet[at] ^= 1 << random.below(8),
                1 => packet[at] = random.byte(),
                2 => packet[at] = INTERESTING[random.below(INTERESTING.len())],
                3 => {
                    // Swap two four-byte words.
                    let (a, b) = (at & !3, random.below(packet.len()) & !3);
                    for offset in 0..4 {
                        packet.swap(a + offset, b + offset);
                    }
                }
                4 => {
                    // Overwrite a word with a small or a huge number, where
                    // sizes are.
                    let word = at & !3;
                    let value: u32 = match random.below(4) {
                        0 => random.below(64) as u32,
                        1 => u32::MAX - random.below(8) as u32,
                        2 => 0x7fff_ffff,
                        _ => (random.below(64) as u32) * 4,
                    };
                    packet[word..word + 4].copy_from_slice(&value.to_be_bytes());
                }
                _ => {
                    let len = random.below(packet.len() + 1) & !3;
                    packet.truncate(len.max(4));
                }
            }
        }
        rounds += 1;
        accepted += u32::from(examine(&packet));
    }
    println!("{rounds} damaged packets, {accepted} still accepted");
    // Damage to a number or to a letter of an address leaves a valid
    // packet; damage to structure must not.
    assert!(accepted > 5_000 && accepted < rounds * 3 / 4);
}

#[test]
fn random_bytes_never_panic() {
    let mut random = Random(0x0DDB_A110_DDBA_1100);
    let mut accepted = 0u32;
    for _ in 0..200_000 {
        let len = random.below(24) * 4 + if random.below(8) == 0 { 2 } else { 0 };
        let mut bytes: Vec<u8> = (0..len)
            .map(|_| match random.below(4) {
                0 => 0,
                1 => b"/#,ifsbhdtScrmTFNI[]bundle"[random.below(26)],
                _ => random.byte(),
            })
            .collect();
        if let Some(first) = bytes.first_mut() {
            if random.below(3) != 0 {
                *first = if random.below(4) == 0 { b'#' } else { b'/' };
            }
        }
        if bytes.len() >= 8 && random.below(4) == 0 {
            bytes[..8].copy_from_slice(b"#bundle\0");
        }
        accepted += u32::from(examine(&bytes));
    }
    println!("{accepted} random byte strings happened to be packets");
    assert!(accepted > 100);
}

#[test]
fn a_view_of_a_checked_packet_yields_the_arguments_that_were_sent() {
    let message = everything();
    let bytes = message.to_bytes().unwrap();
    let OscPacketRef::Message(view) = OscPacketRef::parse(&bytes).unwrap() else {
        panic!("not a message");
    };
    let mut args = view.args();
    assert_eq!(args.size_hint(), (0, Some(24)));
    assert_eq!(args.next(), Some(OscArgRef::Int(-7)));
    assert_eq!(args.clone().count(), 23);
    assert_eq!(args.nth(22), Some(OscArgRef::ArrayEnd));
    // The end is the end, however often it is asked for.
    assert_eq!(args.next(), None);
    assert_eq!(args.next(), None);
}

fn framed(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    slip::encode_to_vec(packet, &mut out);
    out
}

/// What a decoder reports for a stream fed in pieces of `chunk` bytes.
fn decode_stream(stream: &[u8], chunk: usize, max: usize) -> Vec<Result<Vec<u8>, SlipError>> {
    let mut decoder = SlipDecoder::new(max);
    let mut seen = Vec::new();
    for piece in stream.chunks(chunk) {
        decoder.feed_all(piece, |frame| {
            if let Ok(frame) = frame {
                assert!(!frame.is_empty() && frame.len() <= max);
            }
            seen.push(frame.map(<[u8]>::to_vec));
        });
    }
    seen
}

#[test]
fn framing_follows_rfc_1055_with_an_end_on_both_sides() {
    assert_eq!((END, ESC, ESC_END, ESC_ESC), (0xC0, 0xDB, 0xDC, 0xDD));
    assert_eq!(framed(b"abc"), [END, b'a', b'b', b'c', END]);
    assert_eq!(framed(b""), [END, END]);
    assert_eq!(
        framed(&[1, END, 2, ESC, 3]),
        [END, 1, ESC, ESC_END, 2, ESC, ESC_ESC, 3, END]
    );
    // The escape bytes themselves need no escaping.
    assert_eq!(framed(&[ESC_END, ESC_ESC]), [END, ESC_END, ESC_ESC, END]);

    let packet = [END, ESC, 7, END, END, ESC];
    let expected = framed(&packet);
    assert_eq!(slip::encoded_len(&packet), expected.len());
    let mut buffer = [0u8; 32];
    assert_eq!(slip::encode(&packet, &mut buffer), Ok(expected.len()));
    assert_eq!(buffer[..expected.len()], expected);
    assert!(buffer[expected.len()..].iter().all(|&byte| byte == 0));
    for len in 0..expected.len() {
        let mut small = vec![0xAAu8; len];
        assert_eq!(
            slip::encode(&packet, &mut small),
            Err(EncodeError::BufferTooSmall {
                needed: expected.len()
            })
        );
        assert!(small.iter().all(|&byte| byte == 0xAA));
    }
}

#[test]
fn a_stream_decodes_the_same_however_it_is_cut() {
    let mut random = Random(0x0051_1B5E_ED00_0001);
    let mut packets = Vec::new();
    let mut stream = Vec::new();
    for round in 0..300 {
        let len = 1 + random.below(40);
        let packet: Vec<u8> = (0..len)
            .map(|_| match random.below(4) {
                0 => END,
                1 => ESC,
                2 => [ESC_END, ESC_ESC][random.below(2)],
                _ => random.byte(),
            })
            .collect();
        slip::encode_to_vec(&packet, &mut stream);
        assert_eq!(slip::encoded_len(&packet), framed(&packet).len());
        packets.push(Ok(packet));
        // Keep-alive ENDs between packets change nothing.
        if round % 7 == 0 {
            stream.extend_from_slice(&[END, END, END]);
        }
    }
    let whole = decode_stream(&stream, stream.len(), 64);
    assert_eq!(whole, packets);
    for chunk in [1, 2, 3, 5, 7, 64, 1_000] {
        assert_eq!(
            decode_stream(&stream, chunk, 64),
            whole,
            "pieces of {chunk}"
        );
    }
}

#[test]
fn damaged_framing_is_reported_and_the_next_packet_is_still_read() {
    // An escape followed by a byte that cannot follow one.
    let mut stream = vec![END, 1, 2, ESC, 9, 3, 4, END];
    stream.extend_from_slice(&framed(b"good"));
    assert_eq!(
        decode_stream(&stream, 3, 64),
        [
            Err(SlipError::InvalidEscape { byte: 9 }),
            Ok(b"good".to_vec())
        ]
    );
    // An escape followed by the end of the packet.
    let mut stream = vec![END, 1, ESC, END];
    stream.extend_from_slice(&framed(b"good"));
    assert_eq!(
        decode_stream(&stream, 1, 64),
        [
            Err(SlipError::InvalidEscape { byte: END }),
            Ok(b"good".to_vec())
        ]
    );
    // A packet longer than the decoder holds.
    let mut stream = framed(&[7; 65]);
    stream.extend_from_slice(&framed(&[8; 64]));
    assert_eq!(
        decode_stream(&stream, 5, 64),
        [Err(SlipError::FrameTooLong { max: 64 }), Ok(vec![8; 64])]
    );
    // Noise before the first END comes out as a frame of its own, which the
    // packet decoder then refuses; the packet after it is intact.
    let mut stream = b"noise".to_vec();
    stream.extend_from_slice(&framed(b"good"));
    assert_eq!(
        decode_stream(&stream, 4, 64),
        [Ok(b"noise".to_vec()), Ok(b"good".to_vec())]
    );
    assert!(!SlipError::InvalidEscape { byte: 9 }.to_string().is_empty());
    assert!(!SlipError::FrameTooLong { max: 64 }.to_string().is_empty());
}

#[test]
fn the_stream_decoder_can_be_fed_one_packet_at_a_time_and_reset() {
    let mut stream = framed(b"one");
    stream.extend_from_slice(&framed(b"two"));
    let mut decoder = SlipDecoder::new(16);
    assert_eq!(decoder.frame(), b"");

    let (read, outcome) = decoder.feed(&stream);
    assert_eq!((read, outcome), (5, Some(Ok(()))));
    assert_eq!(decoder.frame(), b"one");
    let (read, outcome) = decoder.feed(&stream[5..]);
    assert_eq!((read, outcome), (5, Some(Ok(()))));
    assert_eq!(decoder.frame(), b"two");
    assert_eq!(decoder.feed(&[]), (0, None));
    assert_eq!(decoder.frame(), b"");

    // Half a packet, then a new connection.
    assert_eq!(decoder.feed(&[END, b'h', b'a', ESC]), (4, None));
    decoder.reset();
    assert_eq!(decoder.feed(&framed(b"new")), (5, Some(Ok(()))));
    assert_eq!(decoder.frame(), b"new");
}

#[test]
fn random_streams_never_panic_and_never_exceed_the_limit() {
    let mut random = Random(0x0BAD_C0DE_0BAD_C0DE);
    let (mut frames, mut errors) = (0u32, 0u32);
    for max in [0, 1, 7, 64] {
        let stream: Vec<u8> = (0..60_000)
            .map(|_| match random.below(5) {
                0 => END,
                1 => ESC,
                2 => ESC_END,
                3 => ESC_ESC,
                _ => random.byte(),
            })
            .collect();
        let whole = decode_stream(&stream, stream.len(), max);
        assert_eq!(decode_stream(&stream, 13, max), whole);
        for frame in &whole {
            match frame {
                Ok(frame) => {
                    assert!(frame.len() <= max);
                    frames += 1;
                }
                Err(_) => errors += 1,
            }
        }
    }
    println!("{frames} frames and {errors} errors from random streams");
    assert!(frames > 1_000 && errors > 1_000);
}

#[test]
fn packets_survive_framing() {
    for packet in corpus() {
        let stream = framed(&packet);
        let seen = decode_stream(&stream, 3, 1_024);
        assert_eq!(seen, [Ok(packet.clone())]);
        assert!(examine(seen[0].as_ref().unwrap()));
    }
}
