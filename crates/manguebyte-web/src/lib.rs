//! A Manguebyte engine for the browser, that builds and runs natively too.
//!
//! The [`Engine`] holds numbered parts. Each part plays an instrument, the
//! polyphonic synth or the drum kit, through a small chain: a resonant
//! low-pass filter, a reverb, then gain and pan. Parts are made and
//! configured with control calls, outside the audio callback; sound is
//! driven by events, each one landing on the exact frame it names.
//!
//! # Time and events
//!
//! Time is a frame counter, a `u64` that starts at 0 when the engine is made
//! and moves on with every frame rendered. An event travels as a 32-byte
//! [`EventRecord`], laid out as [`record::offset`] describes, little endian:
//! `frame` u64, `kind` u8, `flags` u8 ([`record::flag`]), `part` u16,
//! `note` u8, `channel` u8, two padding bytes, `velocity` f32 (0 to 1),
//! `param` u32 (a [`ParamId`]) and `value` f64 in the parameter's own
//! units. The kinds are
//! listed in [`RecordKind`] with their codes: note on 1, note off 2, param 3,
//! choke 4 (the part's notes stop at once) and all notes off 5 (the part
//! releases every note). A record with an unknown kind or a field out of
//! range is ignored, never a panic; [`EventRecord::decode`] gives the rules.
//!
//! When a quantum of frames is rendered, an event whose frame is inside it
//! lands on that frame; an event whose frame has already passed lands on the
//! first frame of the quantum; an event further ahead waits. Because every
//! part works one frame at a time, the output does not depend on the size
//! of the quanta: one frame at a time or 512 at once gives the same samples,
//! bit for bit.
//!
//! # Accents
//!
//! Bit 0 of a note on's `flags` byte, [`record::flag::ACCENT`]
//! ([`EventRecord::with_accent`]), accents the note, as Aratu marks a note
//! written in capitals. An accent is part of the note it is on and of no
//! other, and it is applied on the same frame, so the output stays the
//! same whatever the quanta.
//!
//! - A synth note is [`SYNTH_ACCENT_DB`] (3.5 dB) louder; its filter
//!   envelope opens the filter [`SYNTH_ACCENT_OCTAVES`] (1 octave) further,
//!   which falls away with the envelope; and its attack takes
//!   [`SYNTH_ACCENT_ATTACK`] (half) of the time set.
//! - A drum is [`DRUM_ACCENT_DB`] (3 dB) louder, and for its first
//!   [`DRUM_ACCENT_SNAP_SECONDS`] (30 ms) its high frequencies are lifted,
//!   most at the transient ([`DRUM_ACCENT_SNAP`], [`CYMBAL_ACCENT_SNAP`]
//!   for the hi-hats and cymbals). An accented kick also starts higher in
//!   pitch, with a louder click.
//!
//! At full velocity the peak rises by 3 to 3.5 dB on the synth and by 3 to
//! 4.6 dB on the drums, and an accented note or drum alone still stays
//! below [`OUTPUT_CEILING`]; `tests/accent.rs` measures it.
//!
//! # Parts and parameters
//!
//! A part is a `u16` the caller chooses, made with
//! [`Engine::create_part`] as an [`InstrumentKind::Synth`] or
//! [`InstrumentKind::Drums`] and given its stages with
//! [`Engine::set_chain`]. Its parameters have stable text identifiers,
//! listed in [`ids`] (`"lowpass.cutoff"`, `"reverb.mix"`, `"gain"`, `"pan"`,
//! `"synth.cutoff"`, `"kick.level"` and so on); an event names one by its
//! [`ParamId`], the FNV-1a hash of the identifier. [`Engine::params_json`]
//! describes a part's parameters as JSON: range, default, unit, the curve a
//! knob follows, smoothing, in the format of
//! [`ParamSet::descriptions_json`](manguebyte_params::ParamSet::descriptions_json).
//!
//! The drum kit follows General MIDI: kick 36, snare 38, closed hat 42,
//! open hat 46, crash 49, ride 51, which Aratu writes `b`, `s`, `c`, `o`,
//! `x` and `r` ([`DRUM_MAP`], [`drum_map_json`]). Striking the closed hat
//! chokes the open hat.
//!
//! # Between threads
//!
//! [`event_ring`] is a single-producer single-consumer ring of records with
//! a fixed byte layout (see [`ring`]), and [`Engine::drain`] moves its
//! records into the engine before each quantum. The Rust ring keeps its
//! records in ordinary memory, for native programs; in a browser a ring
//! with the same layout lives in a `SharedArrayBuffer` shared by the page's
//! main thread and its audio worklet, and `js/ring.js` and `js/worklet.js`
//! drain it in JavaScript, through the WebAssembly exports. When shared
//! memory is not available, records can be copied in a message and handed
//! over with [`Engine::push_bytes`].
//!
//! The engine holds at most
//! [`pending_capacity`](EngineConfig::pending_capacity) waiting events.
//! When that is full it keeps those due first: an earlier event pushes out
//! the latest one, and a drain leaves later records in the ring.
//!
//! A page gets `SharedArrayBuffer` only when it is cross-origin isolated:
//! it must be served with the headers `Cross-Origin-Opener-Policy:
//! same-origin` and `Cross-Origin-Embedder-Policy: require-corp`.
//! Without them the JavaScript helper falls back to messages.
//!
//! # In a browser
//!
//! Built for `wasm32-unknown-unknown`, the crate is a WebAssembly module
//! with the exports in [`ffi`] and no imports. Next to the crate,
//! `js/worklet.js` is an `AudioWorkletProcessor` that instantiates the
//! module and renders through it, `js/engine.js` is the main-thread helper
//! that creates the node, owns the ring and maps `AudioContext` time to
//! frames, and `js/ring.js` is the ring both use. `examples/web/` is a page
//! that plays a drum pattern and a bass line, with a small server that
//! sends the two headers above; its README says how to run it.
//!
//! ```text
//! cargo build -p manguebyte-web --target wasm32-unknown-unknown
//! node --test "crates/manguebyte-web/js/tests/*.test.mjs"
//! ```
//!
//! runs the JavaScript tests: the ring against the same golden bytes as
//! [`ring`], the module driven through its exports, and the worklet with
//! the helper, in Node.js with stand-ins for the browser's worklet globals.
//!
//! # Output level
//!
//! The parts are added up and pass through a limiter that keeps every
//! sample within [`OUTPUT_CEILING`] (about -0.3 dB below full scale): a
//! frame that would go beyond it is turned down just enough, the level is
//! held for [`LIMITER_HOLD_SECONDS`] and comes back over
//! [`LIMITER_RELEASE_SECONDS`]. One note or one drum at full velocity and
//! default settings stays below the ceiling (a single note of the synth
//! peaks around -9 dB, a drum at -4.5 dB or lower); loud chords and
//! several loud parts together are held under it rather than clipped,
//! though the limiter does not look ahead, so under heavy overload the
//! tops of rising edges are flattened briefly (see [`Engine::render`]).
//! [`EngineStats::limited`] counts the frames the limiter turned down.
//!
//! # The contract with Aratu
//!
//! [Aratu](https://github.com/alessonqueirozdev-hub/aratu) writes records
//! and reads the drum map, so these are fixed: a change to any of them that
//! an existing reader or writer would get wrong is a change to
//! [`ffi::ABI_VERSION`] (now 2) and must be agreed with Aratu. The flags
//! byte was such a compatible change: it was padding written as 0, which
//! still means no flag, and an engine from before it ignores it.
//! `tests/record.rs`, `tests/ring.rs` and `js/tests/ring.test.mjs` pin them,
//! the last two against the same golden bytes.
//!
//! A record is 32 bytes, every multi-byte field little endian
//! ([`record::offset`]):
//!
//! | Bytes  | Field      | Type | Meaning |
//! |--------|------------|------|---------|
//! | 0..8   | `frame`    | u64  | the frame the event lands on, counted from the engine's first |
//! | 8      | `kind`     | u8   | 1 note on, 2 note off, 3 param, 4 choke, 5 all notes off |
//! | 9      | `flags`    | u8   | bit 0 accent (note on); bits 1 to 7 reserved: written as 0, ignored |
//! | 10..12 | `part`     | u16  | the part the event is for |
//! | 12     | `note`     | u8   | MIDI note, 0 to 127 (note on and note off) |
//! | 13     | `channel`  | u8   | MIDI channel, 0 to 15 (note on and note off) |
//! | 14..16 | padding    |      | written as 0, never read |
//! | 16..20 | `velocity` | f32  | 0 to 1 (note on); 0 is a note off |
//! | 20..24 | `param`    | u32  | a [`ParamId`]: the 32-bit FNV-1a hash of the parameter's identifier (param) |
//! | 24..32 | `value`    | f64  | the parameter's new value in its own units, finite (param) |
//!
//! Code 0 and codes above 5 are not kinds. A record with an unknown kind,
//! or with a field its kind uses out of range, is ignored and counted
//! ([`EventRecord::decode`]); fields a kind does not use may hold anything.
//!
//! A shared ring of capacity `C` (a power of two, at most
//! [`ring::RING_MAX_CAPACITY`]) is `16 + 32·C` bytes: `head` (u32, records
//! ever taken out, written by the consumer) at 0, `tail` (u32, records ever
//! put in, written by the producer) at 4, the capacity (u32) at 8, the
//! ASCII text `MBR1` at 12, then the slots; see [`ring`].
//!
//! The drum map ([`DRUM_MAP`], published as JSON by [`drum_map_json`] and
//! the `mb_drum_map_json` export):
//!
//! | Drum | Name in the JSON | General MIDI note | Aratu symbol |
//! |------|------------------|-------------------|--------------|
//! | kick | `kick` | 36 | `b` |
//! | snare | `snare` | 38 | `s` |
//! | closed hi-hat | `closed_hat` | 42 | `c` |
//! | open hi-hat | `open_hat` | 46 | `o` |
//! | crash | `crash` | 49 | `x` |
//! | ride | `ride` | 51 | `r` |
//!
//! A closed hat chokes a sounding open hat, with a fade of
//! [`HAT_CHOKE_SECONDS`].
//!
//! # Real time
//!
//! [`Engine::push`], [`Engine::push_bytes`], [`Engine::admit_bytes`],
//! [`Engine::drain`] and [`Engine::render`] never allocate, lock or panic,
//! and rendering holds
//! [`NoDenormals`](manguebyte_core::NoDenormals) (which does nothing in
//! WebAssembly, where the reverb is put to sleep in silence instead).
//! `tests/realtime.rs` checks the first part of that with a counting
//! allocator.

#![deny(unsafe_code)]

mod chain;
mod drums;
mod engine;
#[allow(unsafe_code)]
pub mod ffi;
mod params;
mod queue;
pub mod record;
pub mod ring;
mod synth;

pub use drums::{
    CYMBAL_ACCENT_SNAP, DAMP_SECONDS, DRUM_ACCENT_DB, DRUM_ACCENT_SNAP, DRUM_ACCENT_SNAP_SECONDS,
    DRUM_MAP, Drum, DrumNote, HAT_CHOKE_SECONDS, drum_map_json,
};
pub use engine::{
    Chain, Engine, EngineConfig, EngineError, EngineStats, InstrumentKind, LIMITER_HOLD_SECONDS,
    LIMITER_RELEASE_SECONDS, OUTPUT_CEILING, Pushed,
};
pub use manguebyte_params::ParamId;
pub use params::ids;
pub use record::{Event, EventRecord, RECORD_SIZE, RecordKind};
pub use ring::{RingConsumer, RingProducer, event_ring, event_ring_starting_at, ring_bytes};
pub use synth::{
    RETRIGGER_GLIDE_SECONDS, STEAL_FADE_SECONDS, SYNTH_ACCENT_ATTACK, SYNTH_ACCENT_DB,
    SYNTH_ACCENT_OCTAVES, SYNTH_VOICES,
};
