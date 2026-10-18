//! The claim of the bridge: the music a page sends over the network sounds
//! in the DAW exactly as the same engine renders it.
//!
//! A session of control calls (parts of four kinds, an SFZ instrument sent
//! as bytes, a chain and an instrument changed while it plays) and events
//! scheduled in beats travels over a real WebSocket on 127.0.0.1 to the
//! plugin, which renders it through `Harness`, as a host would, in blocks
//! that keep changing size, at a tempo. The same session, the beats turned
//! into frames, is rendered by `manguebyte-web`'s engine directly. The two
//! must be the same, bit for bit.

mod common;

use aratu_bridge::AratuBridge;
use aratu_bridge::protocol::BeatRecord;
use aratu_bridge::server::engine_config;
use common::*;
use manguebyte::prelude::*;
use manguebyte_web::{
    Chain, Effect, Engine, EventRecord, InstrumentKind, Pushed, SfzLoad, master_latency_frames,
};

/// The largest block the harness renders, and so the engine's quantum.
const MAX_BLOCK: usize = 1_024;

const FRAMES: usize = 72_000;

#[derive(Clone, Copy, Debug)]
enum Call {
    Create(u16, InstrumentKind, Chain),
    Instrument(u16, InstrumentKind),
    SetChain(u16, Chain),
    Sfz(u16),
}

fn calls() -> Vec<(usize, Call)> {
    vec![
        (0, Call::Create(0, InstrumentKind::Synth, Chain::FULL)),
        (0, Call::Create(1, InstrumentKind::Drums, Chain::NONE)),
        (0, Call::Create(2, InstrumentKind::Sampler, Chain::FULL)),
        (0, Call::Sfz(2)),
        (
            0,
            Call::Create(3, InstrumentKind::Plucked, Chain::FULL.with(Effect::Delay)),
        ),
        // While it plays: an effect part 0 has never had, and a new kit.
        (30_000, Call::SetChain(0, Chain::FULL.with(Effect::Chorus))),
        (48_000, Call::Instrument(1, InstrumentKind::AnalogKit)),
    ]
}

/// The events, by beat.
fn events() -> Vec<BeatRecord> {
    let id = ParamId::from_name;
    let at = |beat: f64, record: EventRecord| BeatRecord { beat, record };
    let mut events = Vec::new();
    for step in 0..24 {
        let beat = f64::from(step) / 8.0;
        let note = 52 + (step % 7) as u8;
        events.push(at(
            beat,
            EventRecord::note_on(0, 0, note, 0.8).with_accent(step % 3 == 0),
        ));
        events.push(at(beat + 1.0 / 16.0, EventRecord::note_off(0, 0, note)));
        let drum = [36, 42, 38, 42][step as usize % 4];
        events.push(at(beat + 1.0 / 32.0, EventRecord::note_on(0, 1, drum, 0.9)));
        if step % 4 == 1 {
            events.push(at(beat, EventRecord::note_on(0, 3, 40 + step as u8, 1.0)));
            events.push(at(
                beat + 0.25,
                EventRecord::note_off(0, 3, 40 + step as u8),
            ));
        }
    }
    events.push(at(0.25, EventRecord::note_on(0, 2, 69, 0.8)));
    events.push(at(1.25, EventRecord::note_off(0, 2, 69)));
    events.push(at(1.5, EventRecord::note_on(0, 2, 64, 0.7)));
    events.push(at(2.5, EventRecord::note_off(0, 2, 64)));
    events.push(at(0.0, EventRecord::param(0, 3, id("delay.on"), 1.0)));
    events.push(at(0.0, EventRecord::param(0, 3, id("delay.sync"), 1.0)));
    events.push(at(
        0.75,
        EventRecord::param(0, 0, id("lowpass.cutoff"), 1_500.0),
    ));
    events.push(at(0.5, EventRecord::param(0, 1, id("send"), 0.4)));
    events.push(at(1.5, EventRecord::param(0, 0, id("chorus.on"), 1.0)));
    events.push(at(1.5, EventRecord::param(0, 0, id("chorus.mix"), 0.6)));
    // Cancel what is left of part 0 and play something else.
    events.push(at(2.0, EventRecord::cancel(0, 0)));
    events.push(at(2.0, EventRecord::note_on(0, 0, 76, 1.0)));
    events.push(at(2.75, EventRecord::note_off(0, 0, 76)));
    events
}

/// The events whose frame at `tempo` is not within a hair of half a
/// frame, where rounding could go either way.
fn clear_of_ties(events: Vec<BeatRecord>, tempo: f64) -> Vec<BeatRecord> {
    let frames_per_beat = 60.0 * RATE / tempo;
    events
        .into_iter()
        .filter(|event| ((event.beat * frames_per_beat).fract() - 0.5).abs() > 1e-4)
        .collect()
}

/// The session rendered by the engine directly, in quanta of 128: the
/// beats made frames, late by the master bus's look-ahead, as the plugin
/// reports its latency, with the host's tempo and beat 0 sent first.
fn reference(tempo: f64, events: &[BeatRecord]) -> (Vec<f32>, Vec<f32>) {
    let mut engine = Engine::new(manguebyte_web::EngineConfig {
        pending_capacity: 1 << 14,
        ..engine_config(RATE, MAX_BLOCK)
    })
    .unwrap();
    let latency = master_latency_frames(RATE) as u64;
    let frames_per_beat = 60.0 * RATE / tempo;
    let mut sorted: Vec<(usize, &BeatRecord)> = events.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.beat.total_cmp(&b.1.beat).then(a.0.cmp(&b.0)));
    let tempo_id = ParamId::from_name("master.tempo");
    let beat_id = ParamId::from_name("master.beat");
    assert_eq!(
        engine.push(EventRecord::param(latency, 0, tempo_id, tempo)),
        Pushed::Queued
    );
    assert_eq!(
        engine.push(EventRecord::param(latency, 0, beat_id, 0.0)),
        Pushed::Queued
    );
    for (_, event) in sorted {
        let mut record = event.record;
        record.frame = (event.beat * frames_per_beat).round() as u64 + latency;
        assert_eq!(engine.push(record), Pushed::Queued);
    }
    let (text, files) = tone_instrument();
    let mut left = Vec::with_capacity(FRAMES);
    let mut right = Vec::with_capacity(FRAMES);
    let mut pending = calls().into_iter().peekable();
    while left.len() < FRAMES {
        while let Some((_, call)) = pending.next_if(|&(at, _)| at <= left.len()) {
            match call {
                Call::Create(part, kind, chain) => {
                    engine.create_part_with(part, kind, chain).unwrap()
                }
                Call::Instrument(part, kind) => engine.set_instrument(part, kind).unwrap(),
                Call::SetChain(part, chain) => engine.set_chain(part, chain).unwrap(),
                Call::Sfz(part) => {
                    let mut load = SfzLoad::new(&text).unwrap();
                    for (path, bytes) in &files {
                        load.add_file(path, bytes.clone()).unwrap();
                    }
                    engine
                        .load_instrument(part, load.finish().unwrap())
                        .unwrap();
                }
            }
        }
        let next = pending.peek().map_or(FRAMES, |&(at, _)| at);
        let count = 128.min(next - left.len());
        engine.render(count);
        let (l, r) = engine.output();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
    }
    (left, right)
}

/// The session sent over the bridge and rendered by the plugin through the
/// harness in blocks of `pattern`.
fn bridged(tempo: f64, events: &[BeatRecord], pattern: &[usize]) -> (Vec<f32>, Vec<f32>) {
    let mut harness = Harness::<AratuBridge>::new(RATE, MAX_BLOCK)
        .with_block_pattern(pattern)
        .with_tempo(tempo);
    let mut client = connect(&mut harness);
    let (text, files) = tone_instrument();
    let mut left = Vec::with_capacity(FRAMES);
    let mut right = Vec::with_capacity(FRAMES);
    let mut sent_events = false;
    let mut pending = calls().into_iter().peekable();
    while left.len() < FRAMES {
        while let Some((_, call)) = pending.next_if(|&(at, _)| at <= left.len()) {
            let status = match call {
                Call::Create(part, kind, chain) => client.create_part(part, kind, chain).unwrap(),
                Call::Instrument(part, kind) => client.set_instrument(part, kind).unwrap(),
                Call::SetChain(part, chain) => client.set_chain(part, chain).unwrap(),
                Call::Sfz(part) => {
                    let summary = client.load_sfz(part, &text, &files).unwrap();
                    let summary = summary.unwrap_or_else(|(status, text)| {
                        panic!("{status}: {}", String::from_utf8_lossy(&text))
                    });
                    assert!(summary.contains("\"regions\":1"), "{summary}");
                    assert!(summary.contains("\"make_up\":") && !summary.contains("null"));
                    0
                }
            };
            assert_eq!(status, 0, "{call:?}");
        }
        if !sent_events {
            client.events(0, events).unwrap();
            sent_events = true;
        }
        // Everything sent is in the audio thread's queue before the next
        // block, as it would be well ahead of its beat in a host.
        client.sync().unwrap();
        let next = pending.peek().map_or(FRAMES, |&(at, _)| at);
        let block = harness.render(next - left.len());
        left.extend_from_slice(block.channel(0));
        right.extend_from_slice(block.channel(1));
    }
    let status = harness.plugin_mut().status().unwrap();
    assert_eq!(
        (status.late, status.too_late, status.stale, status.overflow),
        (0, 0, 0, 0),
        "{status:?}"
    );
    assert_eq!(status.engine[3], 0, "no event landed late in the engine");
    client.close(1000).unwrap();
    (left, right)
}

fn same_bits_at(tempo: f64) {
    let events = clear_of_ties(events(), tempo);
    let (left, right) = reference(tempo, &events);
    let loudest = left
        .iter()
        .chain(&right)
        .fold(0.0f32, |peak, s| peak.max(s.abs()));
    assert!(loudest > 0.05, "the session is heard: {loudest}");
    for pattern in [&[1_024][..], &[1, 999, 13, 1_024, 511], &[64, 300, 7, 129]] {
        let (bridge_left, bridge_right) = bridged(tempo, &events, pattern);
        for (channel, (a, b)) in [(&left, &bridge_left), (&right, &bridge_right)]
            .into_iter()
            .enumerate()
        {
            if let Some(frame) = first_difference(a, b) {
                panic!(
                    "at {tempo} bpm in blocks of {pattern:?}, channel {channel} differs from frame {frame}: {} against {}",
                    a[frame], b[frame]
                );
            }
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn a_session_sent_over_the_bridge_sounds_as_the_engine_renders_it() {
    // 25 600 frames a beat: every event on a whole frame.
    same_bits_at(112.5);
}

#[test]
#[cfg_attr(miri, ignore = "sockets")]
fn the_same_at_a_tempo_whose_beats_fall_between_frames() {
    same_bits_at(97.0);
}
