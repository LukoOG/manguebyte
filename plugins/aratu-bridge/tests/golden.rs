//! The bytes of an events message, written by the Rust side, against the
//! same golden bytes the JavaScript client's test checks
//! (`crates/manguebyte-web/js/tests/fixtures/bridge-golden.hex`).

use aratu_bridge::protocol::{self, BeatRecord, page};
use manguebyte_web::{EventRecord, ParamId};

fn golden() -> Vec<u8> {
    let text = include_str!("../../../crates/manguebyte-web/js/tests/fixtures/bridge-golden.hex");
    let hex: String = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<String>()
        .split_whitespace()
        .collect();
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

#[test]
fn the_rust_side_writes_and_reads_the_golden_bytes() {
    let records = [
        BeatRecord {
            beat: 1.5,
            record: EventRecord::note_on(0, 3, 60, 0.75)
                .with_channel(2)
                .with_accent(true),
        },
        BeatRecord {
            beat: f64::NAN,
            record: EventRecord::param(0, 0, ParamId::from_name("lowpass.cutoff"), 1_234.5),
        },
    ];
    let bytes = protocol::message(page::EVENTS, 9, &protocol::events_body(7, &records));
    assert_eq!(bytes, golden());
    let golden = golden();
    let (header, body) = protocol::split(&golden).unwrap();
    assert_eq!((header.kind, header.request), (page::EVENTS, 9));
    let (epoch, read) = protocol::parse_events(body).unwrap();
    assert_eq!(epoch, 7);
    let read: Vec<BeatRecord> = read.collect();
    assert_eq!(read[0], records[0]);
    assert!(read[1].beat.is_nan());
    assert_eq!(read[1].record, records[1].record);
}
