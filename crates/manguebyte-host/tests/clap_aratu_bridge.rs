//! Hosts Aratu Bridge through the CLAP library cargo builds for it, plays a
//! session into it over a real WebSocket, and holds what it renders, in a
//! CLAP host's blocks and with the host's transport, against the same
//! session through `Harness`.
//!
//! Not under Miri, which cannot load a shared library or open sockets.
#![cfg(all(feature = "clap", not(miri)))]

mod common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use aratu_bridge::AratuBridge;
use aratu_bridge::client::Client;
use aratu_bridge::protocol::BeatRecord;
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event, EventBuffer, EventKind};
use manguebyte_dsp::{ProcessContext, Processor, Transport};
use manguebyte_params::{ParamId, ParamSet};
use manguebyte_plugin::{Harness, Plugin};
use manguebyte_web::{Chain, EventRecord, InstrumentKind};

use common::{assert_same_bits, clap_from_plain};

const RATE: f64 = 48_000.0;
const ID: &str = "org.manguebyte.aratu-bridge";
const TEMPO: f64 = 112.5;
const FRAMES: usize = 40_000;
const ORIGIN: &str = "https://aratu.live";

/// A port nothing listens on now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn connect(port: u16, mut render_meanwhile: impl FnMut()) -> Client {
    let started = Instant::now();
    loop {
        render_meanwhile();
        if let Ok(mut client) = Client::connect(port, ORIGIN) {
            client.hello().unwrap();
            return client;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the plugin listens"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The session: two parts and a bar of notes, by beat.
fn play(client: &mut Client) {
    assert_eq!(
        client
            .create_part(0, InstrumentKind::Drums, Chain::NONE)
            .unwrap(),
        0
    );
    assert_eq!(
        client
            .create_part(1, InstrumentKind::Synth, Chain::FULL)
            .unwrap(),
        0
    );
    let mut records = Vec::new();
    for step in 0..8 {
        let beat = 0.25 + f64::from(step) / 4.0;
        records.push(BeatRecord {
            beat,
            record: EventRecord::note_on(0, 0, [36, 42, 38, 42][step as usize % 4], 0.9),
        });
        records.push(BeatRecord {
            beat: beat + 0.125,
            record: EventRecord::note_on(0, 1, 50 + step as u8, 0.7),
        });
        records.push(BeatRecord {
            beat: beat + 0.25,
            record: EventRecord::note_off(0, 1, 50 + step as u8),
        });
    }
    client.events(0, &records).unwrap();
    client.sync().unwrap();
}

#[test]
fn the_hosted_bridge_plays_a_session_as_the_plugin_does_alone() {
    let pattern = [256usize, 31, 512, 100];
    // Alone, through the harness.
    let mut harness = Harness::<AratuBridge>::new(RATE, 512)
        .with_block_pattern(&pattern)
        .with_tempo(TEMPO);
    harness.plugin_mut().set_port_override(Some(0));
    let port = harness
        .plugin_mut()
        .wait_listening(Duration::from_secs(10))
        .unwrap();
    let mut client = Client::connect(port, ORIGIN).unwrap();
    client.hello().unwrap();
    play(&mut client);
    let expected = harness.render(FRAMES);
    drop(client);

    // Hosted: the port is set through the parameter, as a user sets it.
    let library = common::load(common::built_library("aratu_bridge")).expect("it loads");
    let mut plugin = library.create(ID).expect("the plugin is created");
    let mut processor = plugin.activate(RATE, 512).expect("it activates");
    let port = free_port();
    let params = AratuBridge::new().params();
    let port_param = params.find("port").unwrap();
    let mut events = EventBuffer::with_capacity(16);
    events
        .push(Event {
            offset: 0,
            kind: EventKind::ParamValue {
                param: ParamId::from_name("port"),
                value: clap_from_plain(port_param, f64::from(port)),
            },
        })
        .unwrap();
    // Stopped while the page connects, so that the timeline starts at 0.
    let stopped = Transport {
        playing: false,
        ..Transport::playing_at(TEMPO)
    };
    let mut scratch = AudioBuffer::<f32>::new(2, 64);
    processor.process_with(&mut scratch, &ProcessContext::new(&events, &stopped));
    events.clear();
    let mut client = connect(port, || {
        processor.process_with(&mut scratch, &ProcessContext::new(&events, &stopped));
    });
    play(&mut client);

    let mut audio = AudioBuffer::<f32>::new(2, FRAMES);
    let mut transport = Transport::playing_at(TEMPO);
    let mut done = 0;
    let mut block = 0;
    while done < FRAMES {
        let length = pattern[block % pattern.len()].min(FRAMES - done);
        let context = ProcessContext::new(&events, &transport);
        audio.windowed(done..done + length, |piece| {
            processor.process_with(piece, &context)
        });
        transport.advance(length, RATE);
        done += length;
        block += 1;
    }
    assert!(
        audio.channel(0).iter().any(|&sample| sample.abs() > 0.01),
        "the session is heard"
    );
    assert_same_bits(&audio, &expected, "the hosted bridge");
    drop(client);
    drop(processor);
    plugin.deactivate().expect("it deactivates");
    assert_eq!(plugin.failed_blocks(), 0);
}
