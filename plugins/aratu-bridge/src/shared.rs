//! What the audio thread, the server thread and the plugin share, through
//! atomics only: the port, the engine's configuration, and a snapshot of
//! the transport and the counters that the audio thread writes after every
//! block and the server reads, as a sequence lock that never makes the
//! writer wait.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::protocol::{Status, Transport};

/// No port override.
const NO_PORT: u32 = u32::MAX;

/// The words of a snapshot.
const WORDS: usize = 8 + 19;

/// Shared between the plugin, its audio thread and its server thread.
#[derive(Debug)]
pub struct Shared {
    /// Set when the plugin goes away: the server stops.
    pub shutdown: AtomicBool,
    port_override: AtomicU32,
    bound_port: AtomicU64,
    /// Whether a page is connected.
    pub connected: AtomicBool,
    /// Moves on each time the plugin makes a new engine for a new sample
    /// rate or block size, which drops every part.
    pub generation: AtomicU64,
    sample_rate: AtomicU64,
    /// The engine's largest quantum.
    pub max_quantum: AtomicU32,
    /// The host's largest block.
    pub max_block: AtomicU32,
    /// The plugin's latency, frames.
    pub latency: AtomicU32,
    seq: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            port_override: AtomicU32::new(NO_PORT),
            bound_port: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            sample_rate: AtomicU64::new(0),
            max_quantum: AtomicU32::new(0),
            max_block: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            seq: AtomicU64::new(0),
            words: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// The transport and counters after a block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Snapshot {
    /// The transport; its `age` is not kept.
    pub transport: Transport,
    /// The counters.
    pub status: Status,
}

impl Shared {
    /// The port to listen on instead of the parameter's, if any; 0 asks
    /// the system for a free one.
    pub fn port_override(&self) -> Option<u16> {
        u16::try_from(self.port_override.load(Ordering::Acquire)).ok()
    }

    /// Sets or clears the override.
    pub fn set_port_override(&self, port: Option<u16>) {
        self.port_override
            .store(port.map_or(NO_PORT, u32::from), Ordering::Release);
    }

    /// The port the server listens on, if it listens.
    pub fn bound_port(&self) -> Option<u16> {
        self.bound().map(|(_, port)| port)
    }

    /// The port the server was asked for and the one it listens on, if it
    /// listens (they differ when 0 asked the system for a free one).
    pub fn bound(&self) -> Option<(u16, u16)> {
        let word = self.bound_port.load(Ordering::Acquire);
        (word >> 32 != 0).then_some(((word >> 16) as u16, word as u16))
    }

    /// Says which port the server listens on, asked for as `requested`.
    pub fn set_bound_port(&self, bound: Option<(u16, u16)>) {
        let word = bound.map_or(0, |(requested, port)| {
            1 << 32 | u64::from(requested) << 16 | u64::from(port)
        });
        self.bound_port.store(word, Ordering::Release);
    }

    /// The sample rate, 0 before the host has activated the plugin.
    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.sample_rate.load(Ordering::Acquire))
    }

    /// Sets it.
    pub fn set_sample_rate(&self, rate: f64) {
        self.sample_rate.store(rate.to_bits(), Ordering::Release);
    }

    /// Writes a snapshot. Only the audio thread calls it. Real-time safe.
    pub fn publish(&self, snapshot: &Snapshot) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (word, value) in self.words.iter().zip(encode(snapshot)) {
            word.store(value, Ordering::Relaxed);
        }
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// The latest snapshot, read whole.
    pub fn snapshot(&self) -> Snapshot {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let mut values = [0u64; WORDS];
            for (value, word) in values.iter_mut().zip(&self.words) {
                *value = word.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return decode(&values);
            }
        }
    }
}

fn encode(snapshot: &Snapshot) -> [u64; WORDS] {
    let t = &snapshot.transport;
    let s = &snapshot.status;
    let mut words = [0u64; WORDS];
    words[0] = t.beat.to_bits();
    words[1] = t.tempo.to_bits();
    words[2] = t.bar_start.to_bits();
    words[3] = t.loop_start.to_bits();
    words[4] = t.loop_end.to_bits();
    words[5] = t.block;
    words[6] = u64::from(t.epoch);
    words[7] = u64::from(t.numerator)
        | (u64::from(t.denominator) << 16)
        | (u64::from(t.playing) << 32)
        | (u64::from(t.recording) << 33)
        | (u64::from(t.from_host) << 34);
    words[8..19].copy_from_slice(&s.engine);
    words[19] = s.late;
    words[20] = s.too_late;
    words[21] = s.stale;
    words[22] = s.cleared;
    words[23] = s.overflow;
    words[24] = s.immediate;
    words[25] = s.overridden;
    words[26] = s.limiter_db.to_bits();
    words
}

fn decode(words: &[u64; WORDS]) -> Snapshot {
    let mut engine = [0; 11];
    engine.copy_from_slice(&words[8..19]);
    Snapshot {
        transport: Transport {
            beat: f64::from_bits(words[0]),
            tempo: f64::from_bits(words[1]),
            bar_start: f64::from_bits(words[2]),
            loop_start: f64::from_bits(words[3]),
            loop_end: f64::from_bits(words[4]),
            age: 0.0,
            block: words[5],
            epoch: words[6] as u32,
            numerator: words[7] as u16,
            denominator: (words[7] >> 16) as u16,
            playing: words[7] >> 32 & 1 != 0,
            recording: words[7] >> 33 & 1 != 0,
            from_host: words[7] >> 34 & 1 != 0,
        },
        status: Status {
            engine,
            late: words[19],
            too_late: words[20],
            stale: words[21],
            cleared: words[22],
            overflow: words[23],
            immediate: words[24],
            overridden: words[25],
            limiter_db: f64::from_bits(words[26]),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reads_back_as_written() {
        let shared = Shared::default();
        let mut snapshot = Snapshot::default();
        snapshot.transport.beat = 12.5;
        snapshot.transport.tempo = 133.0;
        snapshot.transport.numerator = 6;
        snapshot.transport.denominator = 8;
        snapshot.transport.playing = true;
        snapshot.transport.from_host = true;
        snapshot.transport.loop_start = f64::NAN;
        snapshot.status.engine[4] = 99;
        snapshot.status.overridden = 3;
        snapshot.status.limiter_db = -0.25;
        shared.publish(&snapshot);
        let read = shared.snapshot();
        assert_eq!(read.transport.beat, 12.5);
        assert!(read.transport.loop_start.is_nan());
        assert_eq!(read.transport.denominator, 8);
        assert!(read.transport.playing && !read.transport.recording && read.transport.from_host);
        assert_eq!(read.status, snapshot.status);
        assert_eq!(shared.port_override(), None);
        shared.set_port_override(Some(0));
        assert_eq!(shared.port_override(), Some(0));
    }

    #[test]
    fn a_reader_never_sees_half_a_snapshot() {
        let shared = std::sync::Arc::new(Shared::default());
        let writer = {
            let shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut snapshot = Snapshot::default();
                for n in 0..if cfg!(miri) { 50 } else { 20_000 } {
                    snapshot.transport.block = n;
                    snapshot.transport.beat = n as f64;
                    snapshot.status.late = n;
                    shared.publish(&snapshot);
                }
            })
        };
        for _ in 0..if cfg!(miri) { 50 } else { 20_000 } {
            let read = shared.snapshot();
            assert_eq!(read.transport.beat, read.transport.block as f64);
            assert_eq!(read.status.late, read.transport.block);
        }
        writer.join().unwrap();
    }
}
