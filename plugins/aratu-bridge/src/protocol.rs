//! The bridge protocol: the binary messages a page and the plugin exchange
//! over the WebSocket. `docs/ARATU_BRIDGE.md` is the specification; this
//! module is its byte layout, used by the plugin's server and by
//! [`Client`](crate::client::Client), and pinned by the JavaScript client's
//! tests.
//!
//! Every message is one binary WebSocket message that starts with an
//! eight-byte header, little endian like everything after it:
//!
//! | Bytes | Field     | Meaning |
//! |-------|-----------|---------|
//! | 0     | `type`    | one of [`page`] or [`plugin`] |
//! | 1     | `flags`   | 0 |
//! | 2..4  | reserved  | 0 |
//! | 4..8  | `request` | an id the page chooses; a reply carries it back |

use manguebyte_web::{EventRecord, RECORD_SIZE};

/// The version of this protocol. A page and a plugin of different
/// versions refuse each other in the hello.
pub const PROTOCOL_VERSION: u32 = 1;

/// The WebSocket subprotocol a page must offer.
pub const SUBPROTOCOL: &str = "aratu-bridge.1";

/// The port the plugin listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47_870;

/// The size of the header in front of every message.
pub const HEADER_SIZE: usize = 8;

/// The largest message either side sends, in bytes: 64 MiB, room for one
/// large sample file. A larger one closes the connection with code 1009.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// The most records one events message carries.
pub const MAX_RECORDS_PER_MESSAGE: usize = 65_536;

/// The largest SFZ text, in bytes.
pub const MAX_SFZ_TEXT_BYTES: usize = 4 << 20;

/// Message types from the page.
pub mod page {
    /// Opens a session: `u32` protocol version, `u32` engine interface
    /// version ([`manguebyte_web::ffi::ABI_VERSION`]), then the page's name
    /// in UTF-8. Answered by [`plugin::HELLO`](super::plugin::HELLO).
    pub const HELLO: u8 = 0x01;
    /// Events: `u32` epoch, `u32` 0, then 32-byte records whose first
    /// eight bytes are an `f64` beat instead of a frame. Not answered.
    pub const EVENTS: u8 = 0x02;
    /// `u16` part, `u16` 0, `u32` instrument code, `u32` chain bits.
    pub const CREATE_PART: u8 = 0x10;
    /// `u16` part, `u16` 0, `u32` instrument code.
    pub const SET_INSTRUMENT: u8 = 0x11;
    /// `u16` part.
    pub const REMOVE_PART: u8 = 0x12;
    /// `u16` part, `u16` 0, `u32` chain bits.
    pub const SET_CHAIN: u8 = 0x13;
    /// `u16` part, `u16` 0, then the SFZ text in UTF-8. Answered with the
    /// files it names, a JSON array.
    pub const SFZ_BEGIN: u8 = 0x20;
    /// `u16` part, `u16` length of the path, the path in UTF-8, then the
    /// file's bytes.
    pub const SFZ_FILE: u8 = 0x21;
    /// `u16` part. Answered once the part plays the instrument, with a
    /// JSON summary.
    pub const SFZ_FINISH: u8 = 0x22;
    /// `u16` part: drops a load under way.
    pub const SFZ_ABANDON: u8 = 0x23;
    /// `u32` what ([`query`](super::query)), `u32` argument (a part).
    pub const QUERY: u8 = 0x30;
    /// Answered once every message before it has been handed to the
    /// audio thread.
    pub const SYNC: u8 = 0x31;
    /// Lets every note go and drops every event waiting for its beat.
    pub const PANIC: u8 = 0x32;
}

/// Message types from the plugin.
pub mod plugin {
    /// The answer to a hello: `u32` protocol version, `u32` engine
    /// interface version, `f64` sample rate, `u32` latency in frames,
    /// `u32` largest block, `u32` most parts, `u32` largest message, `u32`
    /// epoch, then the plugin's version in UTF-8.
    pub const HELLO: u8 = 0x81;
    /// The host's transport; see [`Transport`](super::Transport).
    pub const TRANSPORT: u8 = 0x82;
    /// The answer to a request: `i32` status, then what was asked for.
    pub const REPLY: u8 = 0x83;
    /// Counters; see [`Status`](super::Status).
    pub const STATUS: u8 = 0x84;
    /// Something the page should know: `u32` code ([`notice`](super::notice)),
    /// then text in UTF-8.
    pub const NOTICE: u8 = 0x85;
}

/// What a query asks for.
pub mod query {
    /// The drum map, JSON.
    pub const DRUM_MAP_JSON: u32 = 1;
    /// A part's parameters, JSON, at their defaults.
    pub const PARAMS_JSON: u32 = 2;
    /// The master bus's parameters, JSON, at their defaults.
    pub const MASTER_PARAMS_JSON: u32 = 3;
    /// A part's chain bits, as the status (negative if there is no such
    /// part).
    pub const PART_CHAIN: u32 = 4;
    /// The counters, laid out as a status message's body.
    pub const STATS: u32 = 5;
    /// `u32` latency in frames, `u32` largest block, `f64` how far ahead,
    /// in seconds, a page should send events.
    pub const LATENCY: u32 = 6;
}

/// Notice codes.
pub mod notice {
    /// The host reset the plugin (a new sample rate): every part is gone.
    pub const ENGINE_RESET: u32 = 1;
    /// The audio thread's queue was full and messages were dropped.
    pub const OVERFLOW: u32 = 2;
}

/// Statuses of replies beyond those of `manguebyte_web::ffi::status`.
pub mod status {
    /// Done.
    pub const OK: i32 = 0;
    /// The message is not one the plugin understands, or is malformed.
    pub const BAD_MESSAGE: i32 = -200;
    /// The bridge cannot answer this over the network.
    pub const UNSUPPORTED: i32 = -201;
    /// The audio thread's queue is full; try again.
    pub const BUSY: i32 = -202;
    /// No session: the page has not said hello, or the plugin has not been
    /// activated by its host.
    pub const NOT_READY: i32 = -203;
    /// A later load of the same part took over, or the part changed.
    pub const STALE: i32 = -204;
}

/// The header of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The message type.
    pub kind: u8,
    /// The request id.
    pub request: u32,
}

/// Splits a message into its header and body; `None` if it is shorter
/// than a header.
pub fn split(message: &[u8]) -> Option<(Header, &[u8])> {
    if message.len() < HEADER_SIZE {
        return None;
    }
    let header = Header {
        kind: message[0],
        request: u32_at(message, 4)?,
    };
    Some((header, &message[HEADER_SIZE..]))
}

/// A message of type `kind` answering or making request `request`, with
/// `body` after the header.
pub fn message(kind: u8, request: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + body.len());
    out.extend_from_slice(&[kind, 0, 0, 0]);
    out.extend_from_slice(&request.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// A reply to `request` with `status` and `payload`.
pub fn reply(request: u32, status: i32, payload: &[u8]) -> Vec<u8> {
    let mut body = status.to_le_bytes().to_vec();
    body.extend_from_slice(payload);
    message(plugin::REPLY, request, &body)
}

/// The `u16` at `at`, little endian.
pub fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The `u32` at `at`, little endian.
pub fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The `i32` at `at`, little endian.
pub fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The `u64` at `at`, little endian.
pub fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The `f64` at `at`, little endian.
pub fn f64_at(bytes: &[u8], at: usize) -> Option<f64> {
    u64_at(bytes, at).map(f64::from_bits)
}

/// A record and the beat it is for, as an events message carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatRecord {
    /// The beat on the host's timeline, in quarter notes; NaN for "as
    /// soon as possible".
    pub beat: f64,
    /// The record; its frame is not used.
    pub record: EventRecord,
}

impl BeatRecord {
    /// The record's 32 bytes, its first eight the beat.
    pub fn to_bytes(&self) -> [u8; RECORD_SIZE] {
        let mut bytes = self.record.to_bytes();
        bytes[..8].copy_from_slice(&self.beat.to_le_bytes());
        bytes
    }

    /// Reads one from its 32 bytes.
    pub fn from_bytes(bytes: &[u8; RECORD_SIZE]) -> Self {
        let mut record = EventRecord::from_bytes(bytes);
        let beat = f64::from_bits(record.frame);
        record.frame = 0;
        Self { beat, record }
    }
}

/// The body of an events message for `epoch`.
pub fn events_body(epoch: u32, records: &[BeatRecord]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + RECORD_SIZE * records.len());
    body.extend_from_slice(&epoch.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    for record in records {
        body.extend_from_slice(&record.to_bytes());
    }
    body
}

/// The epoch and the records of an events message's body; `None` if it
/// is malformed.
pub fn parse_events(body: &[u8]) -> Option<(u32, impl Iterator<Item = BeatRecord> + '_)> {
    let epoch = u32_at(body, 0)?;
    let records = body.get(8..)?;
    if records.len() % RECORD_SIZE != 0 || records.len() / RECORD_SIZE > MAX_RECORDS_PER_MESSAGE {
        return None;
    }
    Some((
        epoch,
        records.chunks_exact(RECORD_SIZE).map(|chunk| {
            let mut bytes = [0u8; RECORD_SIZE];
            bytes.copy_from_slice(chunk);
            BeatRecord::from_bytes(&bytes)
        }),
    ))
}

/// The host's transport as the plugin sends it.
///
/// | Bytes  | Field        | Type |
/// |--------|--------------|------|
/// | 0..8   | `beat`       | f64, the position at the start of the latest block, quarter notes |
/// | 8..16  | `tempo`      | f64, quarter notes per minute |
/// | 16..24 | `bar_start`  | f64, NaN if unknown |
/// | 24..32 | `loop_start` | f64, NaN if no loop |
/// | 32..40 | `loop_end`   | f64, NaN if no loop |
/// | 40..48 | `age`        | f64, seconds between that block and the message |
/// | 48..56 | `block`      | u64, blocks the plugin has processed |
/// | 56..60 | `epoch`      | u32 |
/// | 60..62 | `numerator`  | u16, 0 if unknown |
/// | 62..64 | `denominator`| u16, 0 if unknown |
/// | 64     | `flags`      | u8: 1 playing, 2 recording, 4 looping, 8 the position is the host's own |
/// | 65..68 | padding      | 0 |
///
/// 68 bytes in all ([`Transport::SIZE`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transport {
    /// The beat at the start of the latest block.
    pub beat: f64,
    /// The tempo.
    pub tempo: f64,
    /// Where the bar the beat is in started, or NaN.
    pub bar_start: f64,
    /// The loop, or NaN and NaN.
    pub loop_start: f64,
    /// The end of the loop, or NaN.
    pub loop_end: f64,
    /// Seconds between the latest block and the message.
    pub age: f64,
    /// Blocks processed.
    pub block: u64,
    /// The epoch.
    pub epoch: u32,
    /// Beats per bar, 0 if unknown.
    pub numerator: u16,
    /// The note value of a beat, 0 if unknown.
    pub denominator: u16,
    /// Whether the host plays.
    pub playing: bool,
    /// Whether it records.
    pub recording: bool,
    /// Whether the host gave the position (rather than the plugin counting
    /// it).
    pub from_host: bool,
}

impl Transport {
    /// The size of a transport message's body.
    pub const SIZE: usize = 68;

    /// The body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::SIZE);
        for value in [
            self.beat,
            self.tempo,
            self.bar_start,
            self.loop_start,
            self.loop_end,
            self.age,
        ] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&self.block.to_le_bytes());
        body.extend_from_slice(&self.epoch.to_le_bytes());
        body.extend_from_slice(&self.numerator.to_le_bytes());
        body.extend_from_slice(&self.denominator.to_le_bytes());
        let flags = u8::from(self.playing)
            | (u8::from(self.recording) << 1)
            | (u8::from(self.loop_start.is_finite()) << 2)
            | (u8::from(self.from_host) << 3);
        body.extend_from_slice(&[flags, 0, 0, 0]);
        body
    }

    /// Reads one; `None` if `body` is too short.
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        let flags = *body.get(64)?;
        Some(Self {
            beat: f64_at(body, 0)?,
            tempo: f64_at(body, 8)?,
            bar_start: f64_at(body, 16)?,
            loop_start: f64_at(body, 24)?,
            loop_end: f64_at(body, 32)?,
            age: f64_at(body, 40)?,
            block: u64_at(body, 48)?,
            epoch: u32_at(body, 56)?,
            numerator: u16_at(body, 60)?,
            denominator: u16_at(body, 62)?,
            playing: flags & 1 != 0,
            recording: flags & 2 != 0,
            from_host: flags & 8 != 0,
        })
    }
}

/// The counters the plugin reports: the engine's
/// ([`EngineStats`](manguebyte_web::EngineStats), in its order) and the
/// bridge's own.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    /// The engine's eleven counters: queued, ignored, dropped, late,
    /// applied, unrouted, displaced, stalled, recovered, limited,
    /// cancelled.
    pub engine: [u64; 11],
    /// Events that arrived after their beat and played at the start of a
    /// block.
    pub late: u64,
    /// Events that arrived too late to play, and were dropped.
    pub too_late: u64,
    /// Events of an epoch that had passed, dropped.
    pub stale: u64,
    /// Events still waiting at a jump, a loop's end or a panic, dropped.
    pub cleared: u64,
    /// Events dropped because the plugin's store or queue was full.
    pub overflow: u64,
    /// Events played as soon as possible.
    pub immediate: u64,
    /// Records for `master.tempo` and `master.beat`, which the host's
    /// transport sets: dropped.
    pub overridden: u64,
    /// How far the master limiter turns the level down, dB.
    pub limiter_db: f64,
}

impl Status {
    /// The size of a status message's body.
    pub const SIZE: usize = 8 * 19;

    /// The body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::SIZE);
        for value in self.engine.iter().chain(&[
            self.late,
            self.too_late,
            self.stale,
            self.cleared,
            self.overflow,
            self.immediate,
            self.overridden,
        ]) {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&self.limiter_db.to_le_bytes());
        body
    }

    /// Reads one; `None` if `body` is too short.
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        let at = |index: usize| u64_at(body, index * 8);
        let mut engine = [0; 11];
        for (index, value) in engine.iter_mut().enumerate() {
            *value = at(index)?;
        }
        Some(Self {
            engine,
            late: at(11)?,
            too_late: at(12)?,
            stale: at(13)?,
            cleared: at(14)?,
            overflow: at(15)?,
            immediate: at(16)?,
            overridden: at(17)?,
            limiter_db: f64_at(body, 18 * 8)?,
        })
    }
}

/// What the plugin says in its hello.
#[derive(Debug, Clone, PartialEq)]
pub struct Welcome {
    /// The protocol version.
    pub protocol: u32,
    /// The engine's interface version.
    pub abi: u32,
    /// The host's sample rate.
    pub sample_rate: f64,
    /// The plugin's latency in frames, which the host compensates.
    pub latency: u32,
    /// The largest block the host renders.
    pub max_block: u32,
    /// The most parts.
    pub max_parts: u32,
    /// The largest message.
    pub max_message: u32,
    /// The transport's epoch.
    pub epoch: u32,
    /// The plugin's version.
    pub version: String,
}

impl Welcome {
    /// The body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&self.protocol.to_le_bytes());
        body.extend_from_slice(&self.abi.to_le_bytes());
        body.extend_from_slice(&self.sample_rate.to_le_bytes());
        for value in [
            self.latency,
            self.max_block,
            self.max_parts,
            self.max_message,
            self.epoch,
        ] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(self.version.as_bytes());
        body
    }

    /// Reads one; `None` if `body` is too short.
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        Some(Self {
            protocol: u32_at(body, 0)?,
            abi: u32_at(body, 4)?,
            sample_rate: f64_at(body, 8)?,
            latency: u32_at(body, 16)?,
            max_block: u32_at(body, 20)?,
            max_parts: u32_at(body, 24)?,
            max_message: u32_at(body, 28)?,
            epoch: u32_at(body, 32)?,
            version: String::from_utf8_lossy(body.get(36..)?).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beat_record_is_a_record_with_a_beat_for_a_frame() {
        let record = EventRecord::note_on(0, 3, 60, 0.75).with_accent(true);
        let beat = BeatRecord {
            beat: 12.375,
            record,
        };
        let bytes = beat.to_bytes();
        assert_eq!(&bytes[..8], &12.375f64.to_le_bytes());
        assert_eq!(&bytes[8..], &record.to_bytes()[8..]);
        assert_eq!(BeatRecord::from_bytes(&bytes), beat);
        let now = BeatRecord::from_bytes(
            &BeatRecord {
                beat: f64::NAN,
                record,
            }
            .to_bytes(),
        );
        assert!(now.beat.is_nan());
    }

    #[test]
    fn messages_round_trip() {
        let records = [
            BeatRecord {
                beat: 1.0,
                record: EventRecord::note_off(0, 1, 40),
            },
            BeatRecord {
                beat: 2.5,
                record: EventRecord::cancel(0, 1),
            },
        ];
        let bytes = message(page::EVENTS, 9, &events_body(7, &records));
        let (header, body) = split(&bytes).unwrap();
        assert_eq!(
            header,
            Header {
                kind: page::EVENTS,
                request: 9
            }
        );
        let (epoch, parsed) = parse_events(body).unwrap();
        assert_eq!(epoch, 7);
        assert_eq!(parsed.collect::<Vec<_>>(), records);
        assert!(parse_events(&body[..body.len() - 1]).is_none());
        assert!(split(&bytes[..7]).is_none());

        let transport = Transport {
            beat: 33.25,
            tempo: 97.0,
            bar_start: 32.0,
            loop_start: 16.0,
            loop_end: 48.0,
            age: 0.002,
            block: 1234,
            epoch: 5,
            numerator: 7,
            denominator: 8,
            playing: true,
            recording: false,
            from_host: true,
        };
        let body = transport.to_bytes();
        assert_eq!(body.len(), Transport::SIZE);
        assert_eq!(Transport::from_bytes(&body), Some(transport));
        assert_eq!(body[64], 1 | 4 | 8);

        let status = Status {
            engine: core::array::from_fn(|index| index as u64 * 3),
            late: 1,
            too_late: 2,
            stale: 3,
            cleared: 4,
            overflow: 5,
            immediate: 6,
            overridden: 7,
            limiter_db: -1.5,
        };
        assert_eq!(status.to_bytes().len(), Status::SIZE);
        assert_eq!(Status::from_bytes(&status.to_bytes()), Some(status));

        let welcome = Welcome {
            protocol: 1,
            abi: 2,
            sample_rate: 44_100.0,
            latency: 103,
            max_block: 512,
            max_parts: 64,
            max_message: 1 << 26,
            epoch: 0,
            version: "0.1.0".into(),
        };
        assert_eq!(Welcome::from_bytes(&welcome.to_bytes()), Some(welcome));
        let reply = reply(4, -6, b"x");
        let (header, body) = split(&reply).unwrap();
        assert_eq!(header.kind, plugin::REPLY);
        assert_eq!(i32_at(body, 0), Some(-6));
        assert_eq!(&body[4..], b"x");
    }
}
