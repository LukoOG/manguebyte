//! The plugin's server over a real socket: who may connect, what it does
//! with frames that break the protocol, pings, closes, fragments, versions
//! and the statuses of control calls.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use aratu_bridge::client::{Client, ClientError};
use aratu_bridge::protocol::{
    self, BeatRecord, PROTOCOL_VERSION, SUBPROTOCOL, Status, page, query, status,
};
use aratu_bridge::ws::{self, Message, close, opcode};
use aratu_bridge::{AratuBridge, DEFAULT_PORT};
use common::*;
use manguebyte::prelude::*;
use manguebyte_web::{Chain, EventRecord, InstrumentKind, ffi};

fn harness() -> Harness<AratuBridge> {
    Harness::<AratuBridge>::new(RATE, 256)
}

fn refused(result: Result<Client, ClientError>) -> u16 {
    match result {
        Err(ClientError::Refused(status, _)) => status,
        other => panic!("not refused: {other:?}"),
    }
}

/// What the server answers to a frame, until it closes.
fn close_code(client: &mut Client) -> Option<u16> {
    for _ in 0..200 {
        match client.next_message(Duration::from_millis(20)) {
            Ok(Some(Message::Close(code, _))) => return code,
            Ok(_) => {}
            Err(ClientError::Closed(code)) => return code,
            Err(other) => panic!("{other}"),
        }
    }
    None
}

#[test]
fn the_default_port_is_the_parameter() {
    let harness = harness();
    assert_eq!(harness.params().port.value(), i32::from(DEFAULT_PORT));
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn pages_from_elsewhere_are_refused() {
    let mut harness = harness();
    let port = listen(&mut harness);
    let host = format!("127.0.0.1:{port}");
    assert_eq!(refused(Client::connect(port, "https://evil.example")), 403);
    assert_eq!(refused(Client::connect(port, "null")), 403);
    assert_eq!(
        refused(Client::connect(port, "https://aratu.live.evil.example")),
        403
    );
    assert_eq!(
        refused(Client::connect_offering(port, ORIGIN, "chat", &host)),
        400,
        "without the subprotocol"
    );
    assert_eq!(
        refused(Client::connect_offering(
            port,
            ORIGIN,
            SUBPROTOCOL,
            &format!("rebound.example:{port}")
        )),
        403,
        "a Host that is not the loopback"
    );
    // A request with no Origin at all.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!(
                "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {SUBPROTOCOL}\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    // Aratu itself is welcome, and only one page at a time.
    let mut first = Client::connect(port, "https://aratu.live").unwrap();
    assert_eq!(refused(Client::connect(port, ORIGIN)), 409);
    first.hello().unwrap();
    assert!(harness.plugin_mut().connected());
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn pings_are_answered_and_a_close_is_returned() {
    let mut harness = harness();
    let mut client = connect(&mut harness);
    client
        .send_frame(true, opcode::PING, b"are you there")
        .unwrap();
    let pong = loop {
        match client.next_message(Duration::from_secs(5)).unwrap() {
            Some(Message::Pong(data)) => break data,
            Some(_) => {}
            None => panic!("no pong"),
        }
    };
    assert_eq!(pong, b"are you there");
    client
        .send_frame(
            true,
            opcode::CLOSE,
            &ws::close_payload(close::NORMAL, "done"),
        )
        .unwrap();
    assert_eq!(close_code(&mut client), Some(close::NORMAL));
    // The plugin is free for the next page.
    std::thread::sleep(Duration::from_millis(50));
    let port = harness.plugin_mut().local_port().unwrap();
    Client::connect(port, ORIGIN).unwrap().hello().unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn a_message_in_fragments_around_a_ping_is_put_together() {
    let mut harness = harness();
    let port = listen(&mut harness);
    let mut client = Client::connect(port, ORIGIN).unwrap();
    let mut body = PROTOCOL_VERSION.to_le_bytes().to_vec();
    body.extend_from_slice(&ffi::ABI_VERSION.to_le_bytes());
    body.extend_from_slice(b"fragmented");
    let hello = protocol::message(page::HELLO, 77, &body);
    client
        .send_frame(false, opcode::BINARY, &hello[..3])
        .unwrap();
    client.send_frame(true, opcode::PING, b"x").unwrap();
    client
        .send_frame(false, opcode::CONTINUATION, &hello[3..9])
        .unwrap();
    client
        .send_frame(true, opcode::CONTINUATION, &hello[9..])
        .unwrap();
    let mut welcomed = false;
    for _ in 0..100 {
        if let Some(Message::Binary(bytes)) =
            client.next_message(Duration::from_millis(50)).unwrap()
        {
            let (header, _) = protocol::split(&bytes).unwrap();
            if header.kind == protocol::plugin::HELLO && header.request == 77 {
                welcomed = true;
                break;
            }
        }
    }
    assert!(welcomed);
}

/// Sends something that breaks the rules.
type Breaker = Box<dyn Fn(&mut Client)>;

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn frames_that_break_the_rules_close_the_connection() {
    let cases: Vec<(&str, Breaker, u16)> = vec![
        (
            "unmasked",
            Box::new(|client| {
                let mut frame = Vec::new();
                ws::encode_frame(&mut frame, true, opcode::BINARY, b"12345678", None);
                client.send_raw(&frame).unwrap();
            }),
            close::PROTOCOL,
        ),
        (
            "oversized",
            Box::new(|client| {
                // Only the header of a frame larger than any message.
                let mut frame = vec![0x82, 0xff];
                frame.extend_from_slice(&((protocol::MAX_MESSAGE_BYTES as u64) + 1).to_be_bytes());
                frame.extend_from_slice(&[1, 2, 3, 4]);
                client.send_raw(&frame).unwrap();
            }),
            close::TOO_BIG,
        ),
        (
            "text",
            Box::new(|client| client.send_frame(true, opcode::TEXT, b"hello").unwrap()),
            close::UNSUPPORTED,
        ),
        (
            "no header",
            Box::new(|client| client.send_frame(true, opcode::BINARY, b"abc").unwrap()),
            close::POLICY,
        ),
        (
            "malformed events",
            Box::new(|client| {
                client.send(page::EVENTS, &[0; 8 + 31]).unwrap();
            }),
            close::POLICY,
        ),
    ];
    for (name, send, code) in cases {
        let mut harness = harness();
        let mut client = connect(&mut harness);
        send(&mut client);
        assert_eq!(close_code(&mut client), Some(code), "{name}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn another_version_is_refused_in_the_hello() {
    let mut harness = harness();
    let port = listen(&mut harness);
    let mut client = Client::connect(port, ORIGIN).unwrap();
    match client.hello_as(PROTOCOL_VERSION + 1) {
        Err(ClientError::Protocol(text)) => assert!(text.contains("protocol 1"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(close_code(&mut client), Some(4000));
    // Nothing but a hello before the hello.
    let mut client = Client::connect(port, ORIGIN).unwrap();
    let (status, _) = client.query(query::DRUM_MAP_JSON, 0).unwrap();
    assert_eq!(status, status::NOT_READY);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn control_calls_answer_as_the_engine_would() {
    let mut harness = harness();
    let mut client = connect(&mut harness);
    assert_eq!(
        client
            .create_part(0, InstrumentKind::Synth, Chain::FULL)
            .unwrap(),
        0
    );
    let mut bad_kind = 1u16.to_le_bytes().to_vec();
    bad_kind.extend_from_slice(&[0, 0]);
    bad_kind.extend_from_slice(&999u32.to_le_bytes());
    bad_kind.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        client.request(page::CREATE_PART, &bad_kind).unwrap().0,
        ffi::status::BAD_INSTRUMENT
    );
    let mut bad_chain = 0u16.to_le_bytes().to_vec();
    bad_chain.extend_from_slice(&[0, 0]);
    bad_chain.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        client.request(page::SET_CHAIN, &bad_chain).unwrap().0,
        ffi::status::BAD_CHAIN
    );
    assert_eq!(client.remove_part(9).unwrap(), ffi::status::NO_SUCH_PART);
    assert_eq!(
        client.set_instrument(9, InstrumentKind::Fm).unwrap(),
        ffi::status::NO_SUCH_PART
    );
    match client.load_sfz(0, "<region> sample=x.wav", &[]).unwrap() {
        Err((code, _)) => assert_eq!(code, ffi::status::NOT_A_SAMPLER),
        Ok(summary) => panic!("{summary}"),
    }
    assert_eq!(
        client
            .create_part(1, InstrumentKind::Sampler, Chain::NONE)
            .unwrap(),
        0
    );
    match client
        .load_sfz(1, "<region> sample=x.wav <<<", &[])
        .unwrap()
    {
        Err((code, _)) => assert_eq!(code, ffi::status::BAD_SFZ),
        Ok(summary) => panic!("{summary}"),
    }
    match client
        .load_sfz(
            1,
            "<region> sample=x.wav",
            &[("x.wav".into(), b"not a wav".to_vec())],
        )
        .unwrap()
    {
        Err((code, _)) => assert_eq!(code, ffi::status::BAD_SAMPLE),
        Ok(summary) => panic!("{summary}"),
    }
    // Queries.
    let (code, chain) = client.query(query::PART_CHAIN, 0).unwrap();
    assert_eq!((code, chain.len()), (Chain::FULL.bits() as i32, 0));
    let (code, json) = client.query(query::PARAMS_JSON, 0).unwrap();
    assert_eq!(code, 0);
    assert!(String::from_utf8(json).unwrap().contains("synth.cutoff"));
    let (code, json) = client.query(query::DRUM_MAP_JSON, 0).unwrap();
    assert_eq!(code, 0);
    assert!(String::from_utf8(json).unwrap().contains("\"kick\""));
    let (code, json) = client.query(query::MASTER_PARAMS_JSON, 0).unwrap();
    assert_eq!(code, 0);
    assert!(String::from_utf8(json).unwrap().contains("master.tempo"));
    let (code, latency) = client.query(query::LATENCY, 0).unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        protocol::u32_at(&latency, 0),
        Some(manguebyte_web::master_latency_frames(RATE) as u32)
    );
    assert_eq!(client.query(999, 0).unwrap().0, status::UNSUPPORTED);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn the_host_owns_the_tempo_and_the_page_hears_the_transport() {
    let mut harness = Harness::<AratuBridge>::new(RATE, 256).with_tempo(133.0);
    let mut client = connect(&mut harness);
    assert_eq!(
        client
            .create_part(0, InstrumentKind::Drums, Chain::NONE)
            .unwrap(),
        0
    );
    let tempo = BeatRecord {
        beat: 0.5,
        record: EventRecord::param(0, 0, ParamId::from_name("master.tempo"), 60.0),
    };
    client.events(0, &[tempo]).unwrap();
    client.sync().unwrap();
    harness.render(48_000);
    let transport = client
        .wait_transport(Duration::from_secs(5), |t| t.beat > 1.0)
        .unwrap();
    assert!(transport.playing && transport.from_host);
    assert_eq!(transport.tempo, 133.0);
    assert_eq!((transport.numerator, transport.denominator), (4, 4));
    let engine = harness.plugin_mut().engine().unwrap();
    assert_eq!(engine.master_param_value("master.tempo"), Some(133.0));
    let (code, body) = client.query(query::STATS, 0).unwrap();
    assert_eq!(code, 0);
    let status = Status::from_bytes(&body).unwrap();
    assert_eq!(status.overridden, 1);
}
