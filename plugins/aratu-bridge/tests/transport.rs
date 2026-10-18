//! The host's transport, mapped onto the engine: a tempo change, a loop
//! going round, a jump of the play cursor and a stop, each with events
//! sent by beat over the bridge and heard where their beat falls.
//!
//! The plugin is driven block by block, as a host drives it, with a
//! transport made by hand. Each event is a kick of the drum kit through no
//! effects, and the output is exactly silent until a kick starts, so the
//! frame a kick lands on is the first frame that is not 0.

mod common;

use std::time::Duration;

use aratu_bridge::AratuBridge;
use aratu_bridge::client::Client;
use aratu_bridge::protocol::BeatRecord;
use common::*;
use manguebyte::prelude::*;
use manguebyte_web::{Chain, EventRecord, InstrumentKind, master_latency_frames};

const BLOCK: usize = 512;

struct Rig {
    plugin: AratuBridge,
    client: Client,
    output: Vec<f32>,
    buffer: AudioBuffer<f32>,
    events: EventBuffer,
}

impl Rig {
    fn new(parts: u16) -> Self {
        let mut plugin = AratuBridge::new();
        plugin.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: BLOCK,
            num_channels: 2,
        });
        plugin.set_port_override(Some(0));
        let port = plugin.wait_listening(Duration::from_secs(10)).unwrap();
        let mut client = Client::connect(port, ORIGIN).unwrap();
        client.hello().unwrap();
        for part in 0..parts {
            assert_eq!(
                client
                    .create_part(part, InstrumentKind::Drums, Chain::NONE)
                    .unwrap(),
                0
            );
        }
        Self {
            plugin,
            client,
            output: Vec::new(),
            buffer: AudioBuffer::new(2, BLOCK),
            events: EventBuffer::with_capacity(16),
        }
    }

    fn send(&mut self, epoch: u32, events: &[(f64, EventRecord)]) {
        let records: Vec<BeatRecord> = events
            .iter()
            .map(|&(beat, record)| BeatRecord { beat, record })
            .collect();
        self.client.events(epoch, &records).unwrap();
        self.client.sync().unwrap();
    }

    fn block(&mut self, transport: &Transport) {
        self.buffer.set_num_frames(BLOCK);
        self.buffer.clear();
        let context = ProcessContext::new(&self.events, transport);
        self.plugin.process(&mut self.buffer, &context);
        self.output.extend_from_slice(self.buffer.channel(0));
    }

    /// The first frame from `from` on that is not silent.
    fn onset(&self, from: usize) -> Option<usize> {
        self.output[from..]
            .iter()
            .position(|&sample| sample != 0.0)
            .map(|at| at + from)
    }
}

fn kick(part: u16) -> EventRecord {
    EventRecord::note_on(0, part, 36, 1.0)
}

fn latency() -> usize {
    master_latency_frames(RATE)
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn a_tempo_change_moves_the_beats_after_it() {
    let mut rig = Rig::new(1);
    rig.send(0, &[(2.0, kick(0))]);
    let mut transport = Transport::playing_at(120.0);
    let change = 47 * BLOCK; // 24 064 frames, a little past beat 1
    while rig.output.len() < 48_000 {
        if rig.output.len() == change {
            transport.tempo_bpm = Some(150.0);
        }
        rig.block(&transport);
        transport.advance(BLOCK, RATE);
    }
    let at_change = change as f64 / 24_000.0;
    let expected = change as f64 + (2.0 - at_change) * 60.0 * RATE / 150.0;
    assert_eq!(rig.onset(0), Some(expected.round() as usize + latency()));
    assert_eq!(rig.plugin.status().unwrap().late, 0);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn a_loop_goes_round_with_the_next_lap_sent_ahead() {
    // A loop of one beat, 24 000 frames, which the host goes round in the
    // middle of a block, as `Transport::advance` does.
    let mut rig = Rig::new(2);
    rig.send(
        0,
        &[
            (0.5, kick(0)),
            (0.9, EventRecord::choke(0, 0)),
            // Never reached: past the loop's end.
            (1.5, kick(1)),
        ],
    );
    // The next lap, sent before it starts, tagged with the next epoch.
    rig.send(1, &[(0.25, kick(1))]);
    let mut transport = Transport {
        loop_beats: Some((0.0, 1.0)),
        ..Transport::playing_at(120.0)
    };
    while rig.output.len() < 40_000 {
        rig.block(&transport);
        transport.advance(BLOCK, RATE);
    }
    assert_eq!(rig.onset(0), Some(12_000 + latency()));
    // The choke silences part 0 at beat 0.9; part 1 starts a quarter beat
    // into the second lap.
    let choked = 21_600 + latency();
    assert_eq!(rig.onset(choked + 10), Some(24_000 + 6_000 + latency()));
    let status = rig.plugin.status().unwrap();
    assert!(
        status.cleared >= 1,
        "the kick past the loop's end was dropped"
    );
    let transport = rig
        .client
        .wait_transport(Duration::from_secs(5), |t| t.epoch == 1)
        .unwrap();
    assert!(transport.playing);
    assert_eq!(transport.loop_start, 0.0);
    assert_eq!(transport.loop_end, 1.0);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn a_jump_drops_what_was_sent_and_takes_the_new_epoch() {
    let mut rig = Rig::new(1);
    rig.send(0, &[(0.75, kick(0))]);
    let mut transport = Transport::playing_at(120.0);
    let jump = 20 * BLOCK; // 10 240 frames, before beat 0.75
    while rig.output.len() < jump {
        rig.block(&transport);
        transport.advance(BLOCK, RATE);
    }
    transport.position_beats = Some(8.0);
    rig.block(&transport);
    transport.advance(BLOCK, RATE);
    let epoch = rig
        .client
        .wait_transport(Duration::from_secs(5), |t| t.beat >= 8.0)
        .unwrap()
        .epoch;
    assert_eq!(epoch, 2);
    // Sent for the old epoch after the jump: dropped. For the new: played.
    rig.send(0, &[(8.4, kick(0))]);
    rig.send(epoch, &[(8.5, kick(0))]);
    while rig.output.len() < 30_000 {
        rig.block(&transport);
        transport.advance(BLOCK, RATE);
    }
    assert_eq!(rig.onset(0), Some(jump + 12_000 + latency()));
    let status = rig.plugin.status().unwrap();
    assert_eq!(status.stale, 1);
    assert_eq!(status.cleared, 1, "the kick at beat 0.75");
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn stopped_events_wait_and_immediate_ones_play() {
    let mut rig = Rig::new(1);
    let mut transport = Transport {
        playing: false,
        ..Transport::playing_at(120.0)
    };
    rig.send(0, &[(0.25, kick(0))]);
    for _ in 0..20 {
        rig.block(&transport);
    }
    assert_eq!(rig.onset(0), None, "nothing is due while stopped");
    // An event for "now" plays while stopped.
    rig.send(0, &[(f64::NAN, kick(0))]);
    let now = rig.output.len();
    rig.block(&transport);
    rig.block(&transport);
    assert_eq!(rig.onset(0), Some(now + latency()));
    rig.send(0, &[(f64::NAN, EventRecord::choke(0, 0))]);
    let silent = rig.output.len() + latency() + 1;
    for _ in 0..4 {
        rig.block(&transport);
    }
    // Play from where it stopped: the waiting kick comes a quarter beat in.
    transport.playing = true;
    let start = rig.output.len();
    while rig.output.len() < start + 12_000 {
        rig.block(&transport);
        transport.advance(BLOCK, RATE);
    }
    assert_eq!(rig.onset(silent), Some(start + 6_000 + latency()));
    assert_eq!(rig.plugin.status().unwrap().immediate, 2);
}
