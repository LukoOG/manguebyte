//! Where each block of the host's timeline is in beats, and what happened
//! to the transport since the last block: a start, a stop, a jump or a
//! loop going round.
//!
//! The page schedules events by beat, on the host's timeline, in quarter
//! notes. A host tells a plugin, at the start of each block, the beat the
//! block starts on and the tempo ([`Transport`]). [`Timeline::block`] turns
//! that into the stretch of beats each frame of the block covers, so that
//! an event lands on the frame its beat rounds to:
//!
//! - **Tempo.** A block is read at the tempo the host gives for it. A
//!   change of tempo takes effect from the block it is reported in, and is
//!   not taken for a jump, whichever of the two tempos the host advanced
//!   the last block at.
//! - **Loops.** When the host loops and a block runs past the loop's end,
//!   the block is cut there: its first frames cover the end of the lap and
//!   the rest the start of the next. A host that cuts its blocks at the
//!   loop's end itself gives a block that starts at the loop's start, and
//!   that is a wrap too. A wrap moves the [epoch](Timeline::epoch) on by
//!   one.
//! - **Jumps.** A block that does not start where the last one ended (at
//!   either tempo, give or take a frame) is a jump: the user moved the
//!   play cursor, or the host started from elsewhere. A jump moves the
//!   epoch on by two, so that events the page sent for the next lap of a
//!   loop, tagged with the epoch after the current one, are dropped too.
//! - **Stopped**, the timeline does not move and nothing is due; a start
//!   where the transport stopped is not a jump.
//!
//! A host that does not give a position is counted for: the beat moves on
//! by the frames rendered at the tempo, from 0.

use manguebyte::prelude::Transport;

/// The tempo assumed until a host gives one, in quarter notes per minute.
pub const DEFAULT_TEMPO: f64 = 120.0;

/// What happened to the transport at the start of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// It went on from where it was.
    None,
    /// It went round its loop between the last block and this one.
    Wrap,
    /// It moved somewhere else.
    Jump,
}

/// A stretch of a block that covers beats in a straight line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// Its first frame in the block.
    pub offset: usize,
    /// How many frames it has.
    pub frames: usize,
    /// The beat of its first frame.
    pub beat: f64,
}

impl Span {
    /// The frame of the block `beat` rounds to, if it falls in the span or
    /// before it: `Some(offset)` with `offset < self.offset + self.frames`,
    /// which is before `self.offset` (late) when `beat` came before the
    /// span. `None` if it is due after the span.
    pub fn frame_of(&self, beat: f64, frames_per_beat: f64) -> Option<i64> {
        let from_start = ((beat - self.beat) * frames_per_beat).round();
        if from_start.is_nan() || from_start >= self.frames as f64 {
            return None;
        }
        // Far in the past saturates; the caller drops it as too late.
        Some(self.offset as i64 + from_start.max(-1e15) as i64)
    }
}

/// One block of the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    /// What happened between the last block and this one.
    pub edge: Edge,
    /// Whether the transport started with this block.
    pub started: bool,
    /// Whether it stopped with this block.
    pub stopped: bool,
    /// Whether it plays.
    pub playing: bool,
    /// The tempo, quarter notes per minute.
    pub tempo: f64,
    /// Whether the tempo differs from the last block's.
    pub tempo_changed: bool,
    /// Frames per beat at the tempo.
    pub frames_per_beat: f64,
    /// The beat the block starts on.
    pub beat: f64,
    /// The beats the block covers, while it plays: from its first frame,
    /// up to the loop's end if it wraps inside.
    pub first: Option<Span>,
    /// The beats it covers after wrapping round its loop inside, from the
    /// loop's start.
    pub wrapped: Option<Span>,
    /// The epoch once this block's edges are counted.
    pub epoch: u32,
    /// Whether the host gave the position.
    pub from_host: bool,
    /// The host's loop, if it loops.
    pub loop_beats: Option<(f64, f64)>,
}

#[derive(Debug, Clone, Copy)]
struct Last {
    playing: bool,
    /// The beat and frames of the block's last span.
    span_beat: f64,
    span_frames: usize,
    frames_per_beat: f64,
    loop_beats: Option<(f64, f64)>,
    wrapped_inside: bool,
    /// Where the next block starts if it goes on.
    next: f64,
}

/// Follows the host's transport from block to block.
#[derive(Debug, Clone)]
pub struct Timeline {
    epoch: u32,
    tempo: f64,
    last: Option<Last>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    /// A timeline that has seen no block, at epoch 0.
    pub const fn new() -> Self {
        Self {
            epoch: 0,
            tempo: DEFAULT_TEMPO,
            last: None,
        }
    }

    /// The epoch: how many times the timeline has wrapped, plus twice how
    /// many times it has jumped. Events are sent for an epoch, and those of
    /// another are dropped.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Reads the block of `frames` frames that `transport` describes, at
    /// `sample_rate`. Real-time safe.
    pub fn block(&mut self, transport: &Transport, frames: usize, sample_rate: f64) -> Block {
        let tempo = transport
            .tempo_bpm
            .filter(|tempo| tempo.is_finite() && *tempo > 0.0)
            .unwrap_or(self.tempo);
        let tempo_changed = tempo.to_bits() != self.tempo.to_bits();
        self.tempo = tempo;
        let frames_per_beat = 60.0 * sample_rate / tempo;
        let tolerance = 1.0 / frames_per_beat + 1e-9;
        let host_beat = transport.position_beats.filter(|beat| beat.is_finite());
        let from_host = host_beat.is_some();
        let beat = host_beat.unwrap_or_else(|| self.last.map_or(0.0, |last| last.next));
        let playing = transport.playing;
        let loop_beats = transport
            .loop_beats
            .filter(|(start, end)| start.is_finite() && end.is_finite() && end - start > tolerance);

        let mut edge = Edge::None;
        let (mut started, mut stopped) = (playing, false);
        if let Some(last) = self.last {
            started = playing && !last.playing;
            stopped = !playing && last.playing;
            edge = Self::edge(&last, beat, frames_per_beat, tolerance);
        }
        match edge {
            Edge::Jump => self.epoch = self.epoch.wrapping_add(2),
            Edge::Wrap => self.epoch = self.epoch.wrapping_add(1),
            Edge::None => {}
        }

        let (mut first, mut wrapped) = (None, None);
        let mut last_span = Span {
            offset: 0,
            frames,
            beat,
        };
        if playing {
            first = Some(last_span);
            if let Some((start, end)) = loop_beats {
                let crosses = beat < end && beat + frames as f64 / frames_per_beat > end;
                if crosses {
                    let at = ((end - beat) * frames_per_beat).round().max(0.0) as usize;
                    if at < frames {
                        first = (at > 0).then_some(Span {
                            offset: 0,
                            frames: at,
                            beat,
                        });
                        last_span = Span {
                            offset: at,
                            frames: frames - at,
                            beat: start,
                        };
                        wrapped = Some(last_span);
                        self.epoch = self.epoch.wrapping_add(1);
                    }
                }
            }
        }
        let next = if playing {
            last_span.beat + last_span.frames as f64 / frames_per_beat
        } else {
            beat
        };
        self.last = Some(Last {
            playing,
            span_beat: last_span.beat,
            span_frames: last_span.frames,
            frames_per_beat,
            loop_beats,
            wrapped_inside: wrapped.is_some(),
            next,
        });
        Block {
            edge,
            started,
            stopped,
            playing,
            tempo,
            tempo_changed,
            frames_per_beat,
            beat,
            first,
            wrapped,
            epoch: self.epoch,
            from_host,
            loop_beats,
        }
    }

    /// Whether `beat`, where this block starts, follows on from `last`.
    fn edge(last: &Last, beat: f64, frames_per_beat: f64, tolerance: f64) -> Edge {
        let near = |low: f64, high: f64| beat >= low - tolerance && beat <= high + tolerance;
        if !last.playing {
            return if near(last.next, last.next) {
                Edge::None
            } else {
                Edge::Jump
            };
        }
        // The last block went on at its tempo or at this one's.
        let (a, b) = (
            last.span_frames as f64 / last.frames_per_beat,
            last.span_frames as f64 / frames_per_beat,
        );
        let (low, high) = (last.span_beat + a.min(b), last.span_beat + a.max(b));
        if near(low, high) {
            return Edge::None;
        }
        // Or it ended at or past the loop's end, and the host went round.
        if let Some((start, end)) = last.loop_beats {
            if !last.wrapped_inside && high >= end - tolerance {
                let (low, high) = (start + (low - end).max(0.0), start + (high - end).max(0.0));
                if near(low, high) {
                    return Edge::Wrap;
                }
            }
        }
        Edge::Jump
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn playing(beat: f64, tempo: f64) -> Transport {
        Transport {
            position_beats: Some(beat),
            tempo_bpm: Some(tempo),
            ..Transport::playing_at(tempo)
        }
    }

    #[test]
    fn steady_play_has_no_edges_whatever_the_blocks() {
        let mut timeline = Timeline::new();
        let mut transport = playing(0.0, 97.0);
        let mut covered = 0.0;
        for (index, frames) in [1, 512, 77, 1_024, 13, 300]
            .iter()
            .cycle()
            .take(60)
            .enumerate()
        {
            let block = timeline.block(&transport, *frames, RATE);
            assert_eq!(block.edge, Edge::None, "block {index}");
            assert_eq!(block.started, index == 0);
            let span = block.first.unwrap();
            assert!((span.beat - covered).abs() < 1e-9);
            assert_eq!(span.frames, *frames);
            covered += *frames as f64 / block.frames_per_beat;
            transport.advance(*frames, RATE);
        }
        assert_eq!(timeline.epoch(), 0);
    }

    #[test]
    fn a_beat_lands_on_the_frame_it_rounds_to() {
        let span = Span {
            offset: 100,
            frames: 400,
            beat: 2.0,
        };
        let frames_per_beat = 24_000.0; // 120 bpm at 48 kHz
        assert_eq!(span.frame_of(2.0, frames_per_beat), Some(100));
        assert_eq!(
            span.frame_of(2.0 + 399.4 / 24_000.0, frames_per_beat),
            Some(499)
        );
        assert_eq!(span.frame_of(2.0 + 399.6 / 24_000.0, frames_per_beat), None);
        assert_eq!(
            span.frame_of(2.0 - 10.0 / 24_000.0, frames_per_beat),
            Some(90)
        );
        assert_eq!(span.frame_of(f64::NAN, frames_per_beat), None);
    }

    #[test]
    fn a_tempo_change_is_not_a_jump() {
        let mut timeline = Timeline::new();
        let mut transport = playing(0.0, 120.0);
        timeline.block(&transport, 512, RATE);
        transport.advance(512, RATE);
        // The host advanced the last block at the old tempo and reports
        // the new one now.
        transport.tempo_bpm = Some(150.0);
        let block = timeline.block(&transport, 512, RATE);
        assert_eq!(block.edge, Edge::None);
        assert!(block.tempo_changed);
        assert_eq!(block.frames_per_beat, 60.0 * RATE / 150.0);
        // Or at the new tempo.
        transport.advance(512, RATE);
        transport.tempo_bpm = Some(90.0);
        transport.position_beats = transport
            .position_beats
            .map(|beat| beat + 512.0 / (60.0 * RATE / 90.0) - 512.0 / (60.0 * RATE / 150.0));
        let block = timeline.block(&transport, 512, RATE);
        assert_eq!(block.edge, Edge::None);
        assert_eq!(timeline.epoch(), 0);
        let block = timeline.block(
            &playing(block.beat + 512.0 / block.frames_per_beat, 90.0),
            512,
            RATE,
        );
        assert!(!block.tempo_changed);
    }

    #[test]
    fn a_loop_wraps_inside_a_block_and_at_its_edge() {
        // Two beats from 4 to 6 at 120 bpm: 48 000 frames a lap.
        let mut timeline = Timeline::new();
        let mut transport = Transport {
            loop_beats: Some((4.0, 6.0)),
            ..playing(5.9, 120.0)
        };
        timeline.block(&transport, 1, RATE);
        transport.advance(1, RATE);
        // 0.1 beat minus a frame is 2 399 frames to the end.
        let block = timeline.block(&transport, 4_000, RATE);
        let first = block.first.unwrap();
        let wrapped = block.wrapped.unwrap();
        assert_eq!(first.frames, 2_399);
        assert_eq!(
            wrapped,
            Span {
                offset: 2_399,
                frames: 1_601,
                beat: 4.0
            }
        );
        assert_eq!(block.epoch, 1);
        // The host went round too: the next block follows on.
        transport.advance(4_000, RATE);
        let block = timeline.block(&transport, 512, RATE);
        assert_eq!(block.edge, Edge::None);
        assert!((block.beat - (4.0 + 1_601.0 / 24_000.0)).abs() < 1e-9);
        assert_eq!(block.epoch, 1);

        // A host that cuts its blocks at the loop's end.
        let mut timeline = Timeline::new();
        let looping = |beat| Transport {
            loop_beats: Some((4.0, 6.0)),
            ..playing(beat, 120.0)
        };
        let block = timeline.block(&looping(6.0 - 512.0 / 24_000.0), 512, RATE);
        assert!(block.wrapped.is_none());
        let block = timeline.block(&looping(4.0), 512, RATE);
        assert_eq!(block.edge, Edge::Wrap);
        assert_eq!(block.epoch, 1);
    }

    #[test]
    fn a_move_of_the_cursor_is_a_jump() {
        let mut timeline = Timeline::new();
        timeline.block(&playing(8.0, 120.0), 512, RATE);
        let block = timeline.block(&playing(0.0, 120.0), 512, RATE);
        assert_eq!(block.edge, Edge::Jump);
        assert_eq!(block.epoch, 2);
        // Forwards, by more than a frame.
        let next = block.beat + 512.0 / 24_000.0 + 2.0 / 24_000.0;
        assert_eq!(
            timeline.block(&playing(next, 120.0), 512, RATE).edge,
            Edge::Jump
        );
        // Within a frame is not.
        let next = next + 512.0 / 24_000.0 + 0.5 / 24_000.0;
        assert_eq!(
            timeline.block(&playing(next, 120.0), 512, RATE).edge,
            Edge::None
        );
        assert_eq!(timeline.epoch(), 4);
    }

    #[test]
    fn stopping_and_starting_again_where_it_stopped_is_not_a_jump() {
        let mut timeline = Timeline::new();
        let mut transport = playing(2.0, 120.0);
        timeline.block(&transport, 512, RATE);
        transport.advance(512, RATE);
        transport.playing = false;
        let block = timeline.block(&transport, 512, RATE);
        assert!(block.stopped && !block.playing);
        assert_eq!(block.first, None);
        assert_eq!(block.edge, Edge::None);
        transport.advance(512, RATE); // nothing moves while stopped
        assert_eq!(timeline.block(&transport, 512, RATE).edge, Edge::None);
        transport.playing = true;
        let block = timeline.block(&transport, 512, RATE);
        assert!(block.started);
        assert_eq!(block.edge, Edge::None);
        // The cursor moved while stopped, then play.
        transport.advance(512, RATE);
        transport.playing = false;
        timeline.block(&transport, 512, RATE);
        transport.position_beats = Some(32.0);
        let block = timeline.block(&transport, 512, RATE);
        assert_eq!(block.edge, Edge::Jump);
        assert_eq!(timeline.epoch(), 2);
    }

    #[test]
    fn a_host_without_a_position_is_counted_for() {
        let mut timeline = Timeline::new();
        let transport = Transport {
            position_beats: None,
            ..Transport::playing_at(120.0)
        };
        let block = timeline.block(&transport, 24_000, RATE);
        assert!(!block.from_host);
        assert_eq!(block.beat, 0.0);
        let block = timeline.block(&transport, 512, RATE);
        assert_eq!(block.beat, 1.0);
        assert_eq!(block.edge, Edge::None);
    }
}
