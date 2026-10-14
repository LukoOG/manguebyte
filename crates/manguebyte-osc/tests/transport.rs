//! Packets over real sockets on the loopback interface.
//!
//! Every socket binds port 0, so the system picks a free port and two runs
//! of the tests cannot collide. The UDP tests send one datagram and wait
//! for it before sending the next: nothing is in flight but the datagram
//! being waited for, so there is no order to lose and no buffer to
//! overflow, and the only timeout is far longer than the loopback
//! interface needs.

use std::io::{ErrorKind, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

use manguebyte_osc::slip::{self, SlipError};
use manguebyte_osc::{
    DecodeError, OscArgRef, OscBundle, OscError, OscMessage, OscPacket, OscPacketRef, OscTime,
    TcpReceiver, TcpSender, UdpReceiver, UdpSender,
};

/// How long a receive may wait. A packet crosses the loopback interface in
/// well under a millisecond; this only ends a test that is broken.
const PATIENCE: Duration = Duration::from_secs(10);

/// The second example message of the OSC 1.0 specification.
const SPEC_FOO: [u8; 40] = [
    0x2f, 0x66, 0x6f, 0x6f, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x69, 0x69, 0x73, 0x66, 0x66, 0x00, 0x00,
    0x00, 0x00, 0x03, 0xe8, 0xff, 0xff, 0xff, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x00, 0x00, 0x00,
    0x3f, 0x9d, 0xf3, 0xb6, 0x40, 0xb5, 0xb2, 0x2d,
];

const FOO_ARGS: [OscArgRef<'static>; 5] = [
    OscArgRef::Int(1_000),
    OscArgRef::Int(-1),
    OscArgRef::String("hello"),
    OscArgRef::Float(1.234),
    OscArgRef::Float(5.678),
];

fn foo() -> OscMessage {
    OscMessage::new("/foo")
        .with(1_000)
        .with(-1)
        .with("hello")
        .with(1.234f32)
        .with(5.678f32)
}

fn packets() -> Vec<OscPacket> {
    // A blob of every byte value, the framing bytes of SLIP among them,
    // and one large enough to span several reads from a stream. It stays
    // under the 9,216 bytes that macOS allows in a UDP datagram by default.
    let every_byte: Vec<u8> = (0..=255).collect();
    let large: Vec<u8> = (0..8_000u32).map(|n| (n * 7) as u8).collect();
    vec![
        OscMessage::new("/oscillator/4/frequency")
            .with(440.0f32)
            .into(),
        foo().into(),
        OscMessage::new("/").into(),
        OscMessage::new("/blob").with(every_byte).into(),
        OscMessage::new("/large").with(large).with("end").into(),
        OscBundle::new(OscTime::new(3_900_000_000, 1 << 31))
            .with(foo())
            .with(OscBundle::default().with(OscMessage::new("/inner").with(true)))
            .into(),
    ]
}

#[test]
fn udp_on_the_loopback_interface_delivers_packets_unchanged() {
    let mut receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    receiver.set_timeout(Some(PATIENCE)).unwrap();
    let address = receiver.local_addr().unwrap();
    assert_ne!(address.port(), 0);

    let mut sender = UdpSender::new(address).unwrap();
    assert_eq!(sender.target(), address);
    let sender_port = sender.socket().local_addr().unwrap().port();

    for packet in packets() {
        sender.send(&packet).unwrap();
        let (received, from) = receiver.recv().unwrap();
        assert_eq!(received, packet);
        assert!(from.ip().is_loopback());
        assert_eq!(from.port(), sender_port);
    }

    // A message sent from borrowed arguments arrives as the bytes the
    // specification gives for it.
    sender.send_message("/foo", &FOO_ARGS).unwrap();
    let (bytes, _) = receiver.recv_bytes().unwrap();
    assert_eq!(bytes, SPEC_FOO);

    // The receiver can hand out a view instead of owned values.
    sender.send_bytes(&SPEC_FOO).unwrap();
    let (view, _) = receiver.recv_ref().unwrap();
    let OscPacketRef::Message(message) = view else {
        panic!("not a message");
    };
    assert_eq!(message.address(), "/foo");
    assert_eq!(message.args().collect::<Vec<_>>(), FOO_ARGS);

    // A datagram that is not a packet is an error, and the next one is
    // received as usual.
    sender.send_bytes(b"not osc!").unwrap();
    assert!(matches!(
        receiver.recv(),
        Err(OscError::Decode(DecodeError::UnknownPacket {
            first: b'n',
            offset: 0
        }))
    ));
    sender.send(&foo().into()).unwrap();
    assert_eq!(receiver.recv().unwrap().0, OscPacket::Message(foo()));
}

#[test]
fn sockets_the_caller_has_set_up_can_be_used() {
    let mut receiver = UdpReceiver::from_socket(UdpSocket::bind("127.0.0.1:0").unwrap());
    receiver.set_timeout(Some(PATIENCE)).unwrap();
    let outgoing = UdpSocket::bind("127.0.0.1:0").unwrap();
    let source = outgoing.local_addr().unwrap();
    let mut sender = UdpSender::from_socket(outgoing, receiver.local_addr().unwrap());

    sender.send(&foo().into()).unwrap();
    let (received, from) = receiver.recv().unwrap();
    assert_eq!(received, OscPacket::Message(foo()));
    assert_eq!(from, source);
    assert_eq!(receiver.socket().local_addr().unwrap(), sender.target());
}

#[test]
fn a_udp_receive_gives_up_after_its_timeout() {
    let mut receiver = UdpReceiver::bind("127.0.0.1:0").unwrap();
    receiver
        .set_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    match receiver.recv() {
        Err(OscError::Io(error)) => assert!(
            matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "{error}"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[test]
fn a_broadcast_sender_asks_the_system_for_permission() {
    // Nothing is sent: this checks the socket option alone.
    let plain = UdpSender::new("127.0.0.1:9").unwrap();
    assert!(!plain.socket().broadcast().unwrap());
    let broadcast = UdpSender::broadcast("255.255.255.255:9").unwrap();
    assert!(broadcast.socket().broadcast().unwrap());
    assert_eq!(broadcast.target(), (Ipv4Addr::BROADCAST, 9).into());
}

#[test]
#[ignore = "sends one broadcast datagram on the local network; run with --ignored"]
fn a_broadcast_reaches_a_receiver_on_this_machine() {
    let mut receiver = UdpReceiver::bind("0.0.0.0:0").unwrap();
    receiver.set_timeout(Some(Duration::from_secs(2))).unwrap();
    let port = receiver.local_addr().unwrap().port();
    let mut sender = UdpSender::broadcast((Ipv4Addr::BROADCAST, port)).unwrap();
    sender.send(&foo().into()).unwrap();
    assert_eq!(receiver.recv().unwrap().0, OscPacket::Message(foo()));
}

#[test]
#[ignore = "needs a network interface that carries multicast; run with --ignored"]
fn multicast_reaches_a_receiver_that_joined_the_group() {
    // An address from the block set aside for use inside one organisation.
    let group = Ipv4Addr::new(239, 255, 77, 77);
    let mut receiver = UdpReceiver::multicast_v4(group, 0, Ipv4Addr::UNSPECIFIED).unwrap();
    receiver.set_timeout(Some(Duration::from_secs(2))).unwrap();
    let port = receiver.local_addr().unwrap().port();
    let mut sender = UdpSender::new((group, port)).unwrap();
    sender.socket().set_multicast_loop_v4(true).unwrap();
    sender.send(&foo().into()).unwrap();
    assert_eq!(receiver.recv().unwrap().0, OscPacket::Message(foo()));
}

/// A sender and a receiver on the two ends of one loopback connection.
fn connection() -> (TcpSender, TcpReceiver) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let sender = TcpSender::connect(listener.local_addr().unwrap()).unwrap();
    let (receiver, from) = TcpReceiver::accept(&listener).unwrap();
    assert!(from.ip().is_loopback());
    receiver.set_timeout(Some(PATIENCE)).unwrap();
    (sender, receiver)
}

/// A plain connection to write arbitrary bytes into, and a receiver that
/// accepts packets of up to `max_packet_len` bytes on its other end.
fn raw_connection(max_packet_len: usize) -> (TcpStream, TcpReceiver) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let raw = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    raw.set_nodelay(true).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let receiver = TcpReceiver::with_max_packet_len(stream, max_packet_len);
    receiver.set_timeout(Some(PATIENCE)).unwrap();
    (raw, receiver)
}

fn framed(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    slip::encode_to_vec(packet, &mut out);
    out
}

#[test]
fn tcp_delivers_every_packet_whole_and_in_order() {
    let (mut sender, mut receiver) = connection();
    assert!(sender.stream().nodelay().unwrap());

    // All of them are sent before any is read: the stream has to keep them
    // apart. Together they are about nine kilobytes, which a socket's
    // buffer takes without the sender having to wait.
    let packets = packets();
    for packet in &packets {
        sender.send(packet).unwrap();
    }
    for packet in &packets {
        assert_eq!(receiver.recv().unwrap().as_ref(), Some(packet));
    }

    sender.send_message("/foo", &FOO_ARGS).unwrap();
    assert_eq!(receiver.recv_bytes().unwrap(), Some(&SPEC_FOO[..]));
    sender.send_bytes(&SPEC_FOO).unwrap();
    let view = receiver.recv_ref().unwrap().unwrap();
    assert_eq!(view.to_packet(), OscPacket::Message(foo()));

    // Once the sender hangs up, the receiver says so, and keeps saying so.
    drop(sender);
    assert_eq!(receiver.recv().unwrap(), None);
    assert_eq!(receiver.recv().unwrap(), None);
}

#[test]
fn a_stream_that_arrives_a_byte_at_a_time_still_decodes() {
    let (mut raw, mut receiver) = raw_connection(TcpReceiver::DEFAULT_MAX_PACKET_LEN);
    let first = OscPacket::from(foo());
    let second = OscPacket::from(OscMessage::new("/blob").with(vec![0xC0u8, 0xDB, 0xDC, 0xDD]));
    let mut stream = framed(&first.to_bytes().unwrap());
    // Extra ENDs between packets are allowed.
    stream.extend_from_slice(&[slip::END, slip::END]);
    stream.extend_from_slice(&framed(&second.to_bytes().unwrap()));
    for byte in &stream {
        raw.write_all(&[*byte]).unwrap();
    }
    assert_eq!(receiver.recv().unwrap(), Some(first));
    assert_eq!(receiver.recv().unwrap(), Some(second));
}

#[test]
fn a_damaged_stream_reports_the_damage_and_carries_on() {
    let (mut raw, mut receiver) = raw_connection(64);
    // An escape followed by a byte that cannot follow one.
    raw.write_all(&[slip::END, b'/', slip::ESC, 0, b'x', slip::END])
        .unwrap();
    // A packet longer than this receiver accepts.
    raw.write_all(&framed(&[b'a'; 100])).unwrap();
    // Framed correctly, but not a packet.
    raw.write_all(&framed(b"junk")).unwrap();
    // And a good one.
    raw.write_all(&framed(&SPEC_FOO)).unwrap();

    assert!(matches!(
        receiver.recv(),
        Err(OscError::Frame(SlipError::InvalidEscape { byte: 0 }))
    ));
    assert!(matches!(
        receiver.recv(),
        Err(OscError::Frame(SlipError::FrameTooLong { max: 64 }))
    ));
    assert!(matches!(
        receiver.recv(),
        Err(OscError::Decode(DecodeError::UnknownPacket {
            first: b'j',
            ..
        }))
    ));
    assert_eq!(receiver.recv().unwrap(), Some(OscPacket::Message(foo())));
}

#[test]
fn a_timeout_keeps_the_part_of_a_packet_that_has_arrived() {
    let (mut raw, mut receiver) = raw_connection(1_024);
    let stream = framed(&SPEC_FOO);
    let (head, tail) = stream.split_at(17);

    raw.write_all(head).unwrap();
    receiver
        .set_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    match receiver.recv() {
        Err(OscError::Io(error)) => assert!(
            matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "{error}"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }

    raw.write_all(tail).unwrap();
    receiver.set_timeout(Some(PATIENCE)).unwrap();
    assert_eq!(receiver.recv().unwrap(), Some(OscPacket::Message(foo())));
}

#[test]
fn transport_errors_say_what_happened_and_keep_their_cause() {
    use std::error::Error;

    let errors = [
        OscError::from(std::io::Error::new(ErrorKind::TimedOut, "too slow")),
        OscError::from(DecodeError::Empty { offset: 0 }),
        OscError::from(manguebyte_osc::EncodeError::InvalidAddress),
        OscError::from(SlipError::FrameTooLong { max: 64 }),
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_some());
    }
}
