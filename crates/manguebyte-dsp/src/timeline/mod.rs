//! Musical time: a tempo map, and a transport an application owns and
//! drives.
//!
//! A host tells a plugin where its transport is at the start of each
//! block, as a [`Transport`](crate::Transport). This module is the other
//! side, for a program that keeps time itself (a standalone application,
//! a sequencer, a test):
//!
//! - [`TempoMap`] holds tempo changes (steps and linear ramps) and time
//!   signatures on a musical timeline, and converts between frames,
//!   seconds, quarter notes (PPQ positions) and bars, beats and ticks, in
//!   closed form in both directions. Its documentation gives the ramp
//!   arithmetic.
//! - [`TransportClock`] plays a map, block by block: play and stop, a
//!   position, a loop and seeking. Each block is a [`ClockBlock`]: where it
//!   starts, the [`Transport`](crate::Transport) for a
//!   [`ProcessContext`](crate::ProcessContext), and the [`Piece`]s it is
//!   cut into where the loop goes round. With its map, as a
//!   [`BlockTimeline`], it tells on which frame of the block any beat, tempo
//!   point or bar line lands.
//! - [`ClockControl`] works a clock from another thread and reads where it
//!   is.
//!
//! # Design
//!
//! **Where it lives.** `manguebyte-dsp` already owns `Transport` and
//! `ProcessContext`, and every crate that runs a processor depends on it,
//! so the timeline is here (ADR 0001): the plugin `Harness`, the
//! standalone runner, the graph and applications all reach it without a
//! new dependency. It needs nothing above `manguebyte-core` and
//! `manguebyte-midi`, from which [`TempoMap::from_midi_file`] reads a
//! Standard MIDI File's tempo map.
//!
//! **One clock, many blocks.** The clock's position is a whole frame, and
//! the beat, tempo and bar of a frame are worked out from the map each
//! time, never summed block by block, so they do not depend on how time is
//! cut into blocks; the tests check this bit for bit over several block
//! patterns. Changes inside a block are not new events: a processor that
//! cares asks the [`BlockTimeline`] in its context where they land, and
//! the one timestamped event stream of ADR 0005 is left as it is.
//!
//! **Threads.** Advancing a clock and every question to a map or a block
//! are real-time safe. A new map is built off the audio thread (that
//! allocates), sent to the clock through the wait-free queue of
//! `manguebyte-core`, and the map it replaces comes back through a second
//! queue to be freed where the control lives, as ADR 0002 asks. Positions
//! for a user interface are published through atomics.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::event::EventBuffer;
//! use manguebyte_dsp::timeline::{TempoCurve, TempoMap, TimeSignature, TransportClock};
//! use manguebyte_dsp::ProcessContext;
//!
//! // 90 bpm, ramping to 120 over the second bar.
//! let mut map = TempoMap::new(90.0, TimeSignature::FOUR_FOUR)?;
//! map.set_tempo(4.0, 90.0, TempoCurve::Linear)?;
//! map.set_tempo(8.0, 120.0, TempoCurve::Step)?;
//!
//! let (mut clock, control) = TransportClock::with_control(map, 48_000.0);
//! control.play();
//!
//! // On the audio thread, each block:
//! let events = EventBuffer::new();
//! let block = clock.next_block(512);
//! let transport = block.transport(clock.map());
//! let context = ProcessContext::<f32>::new(&events, &transport)
//!     .with_timeline(block.with_map(clock.map()));
//! assert_eq!(context.transport.tempo_bpm, Some(90.0));
//! assert_eq!(context.timeline.unwrap().beat_at(0), 0.0);
//! # Ok::<(), manguebyte_dsp::timeline::TempoMapError>(())
//! ```

mod clock;
mod map;

pub use clock::{
    BlockChange, BlockTimeline, ChangeKind, Changes, ClockBlock, ClockControl, Piece, Pieces,
    TransportClock,
};
pub use map::{
    BarBeatTick, BarPosition, MusicalPosition, SignaturePoint, TempoCurve, TempoMap, TempoMapError,
    TempoPoint, TimeSignature, beats_to_ticks, ticks_to_beats,
};

#[cfg(test)]
mod tests;
