//! A Manguebyte engine for the browser, that builds and runs natively too.
//!
//! The [`Engine`] holds numbered parts. Each part plays an instrument (the
//! polyphonic synth, the drum kit, an SFZ instrument, or one of
//! `manguebyte-dsp`'s, see [Instruments](#instruments)) through a chain of
//! effects (see [Effects](#effects)), then gain and pan, into a master bus
//! with a shared reverb, a glue compressor and a true-peak limiter (see
//! [The master bus](#the-master-bus)). Parts are made and configured with
//! control calls, outside the audio callback; sound is driven by events,
//! each one landing on the exact frame it names.
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
//! choke 4 (the part's notes stop at once), all notes off 5 (the part
//! releases every note) and cancel 6 (the part's events from the record's
//! frame on are dropped, and its notes fade out; see
//! [Playing live](#playing-live)). A record with an unknown kind or a field
//! out of range is ignored, never a panic; [`EventRecord::decode`] gives the
//! rules.
//!
//! The engine renders its parts [`Engine::master_latency_frames`] ahead of
//! its output, the time the master bus's limiter looks ahead, and the bus
//! delays them by as much, so an event is heard on the frame it names.
//! When a quantum is rendered, an event whose frame the parts reach in it
//! lands on that frame; an event whose frame they have already passed
//! lands on the first frame they render, and is heard that many frames
//! into the quantum; an event further ahead waits. Every event pushed
//! before the first render lands on its frame. Because every part and the
//! bus work one frame at a time, the output does not depend on the size of
//! the quanta: one frame at a time or 512 at once gives the same samples,
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
//! - The instruments of `manguebyte-dsp` take the accent as an argument of
//!   the note ([`Synth::note_on_accented`](manguebyte_dsp::Synth::note_on_accented)),
//!   and each does what its voice's settings say: louder, and brighter on
//!   the strings, the FM voice and the acid bass.
//!
//! At full velocity the peak rises by 3 to 3.5 dB on the synth and by 3 to
//! 4.6 dB on the drums, and an accented note or drum alone still stays
//! below [`OUTPUT_CEILING`]; `tests/accent.rs` measures it, and
//! `tests/voices.rs` for the other instruments.
//!
//! # Instruments
//!
//! Besides the synth, the drum kit and the two samplers, a part can play
//! the voices of [`manguebyte_dsp::instruments`], each a
//! [`Synth`](manguebyte_dsp::Synth) of them made when the part's
//! instrument is set ([`Engine::create_part`], [`Engine::set_instrument`]):
//!
//! - [`InstrumentKind::Plucked`]: plucked strings, [`PLUCKED_VOICES`]
//!   voices, nylon and steel guitar, finger and slap bass;
//! - [`InstrumentKind::Fm`]: four-operator FM, [`FM_VOICES`] voices,
//!   electric piano, tubular bell, FM bass;
//! - [`InstrumentKind::Organ`]: the tonewheel organ, [`ORGAN_VOICES`]
//!   voices on one manual, through a rotating speaker that is part of the
//!   instrument (`organ.rotary`, `organ.speaker`) and sleeps in silence;
//! - [`InstrumentKind::AnalogKit`]: the 808-style drum machine,
//!   [`ANALOG_KIT_VOICES`] voices sharing a choke group, on the drum map;
//!   the ride and the guitar tap, which the machine does not have, are the
//!   synthesized drums of [`InstrumentKind::Drums`];
//! - [`InstrumentKind::KickBass`] and [`InstrumentKind::Acid`]: one voice
//!   with legato on, so that a note struck while the last one is held (its
//!   note on before the last note's note off) slides to its pitch.
//!
//! Each has its parameters in its part's JSON: a preset where it has
//! several (`plucked.preset`, `fm.preset`, `organ.preset`, `acid.preset`),
//! which moves the knobs it covers on its frame, and the main knobs. Each
//! part keeps two synths of its voices, so that a cancel fades out the
//! notes sounding while the notes after it start on clean voices. The
//! crate's README lists every parameter.
//!
//! # Parts and parameters
//!
//! A part is a `u16` the caller chooses, made with
//! [`Engine::create_part`] as one of the [`InstrumentKind`]s, given another
//! with [`Engine::set_instrument`], and given its stages with
//! [`Engine::set_chain`]. Its parameters have stable text
//! identifiers, listed in [`ids`] (`"lowpass.cutoff"`, `"reverb.mix"`,
//! `"gain"`, `"pan"`, `"send"`, `"delay.time"`,
//! `"synth.cutoff"`, `"kick.level"` and so on); an event names one by its
//! [`ParamId`], the FNV-1a hash of the identifier. [`Engine::params_json`]
//! describes a part's parameters as JSON: range, default, unit, the curve a
//! knob follows, smoothing, in the format of
//! [`ParamSet::descriptions_json`](manguebyte_params::ParamSet::descriptions_json).
//!
//! The drum kit follows General MIDI: kick 36, snare 38, closed hat 42,
//! open hat 46, crash 49, ride 51, high tom 50, mid tom 47, floor tom 43
//! and side stick 37, which Aratu writes `b`, `s`, `c`, `o`, `x`, `r`,
//! `t`, `m`, `f` and `k`, and a guitar body tap on note 88, which has no
//! symbol ([`DRUM_MAP`], [`drum_map_json`]). Striking the closed hat
//! chokes the open hat.
//!
//! # Playing live
//!
//! Three things for a performer changing the music while it plays:
//!
//! - **Cancel** ([`RecordKind::Cancel`], code 6, [`EventRecord::cancel`]).
//!   When it is pushed, every event of its part due on its frame or later
//!   that was pushed before it is dropped ([`EngineStats::cancelled`]
//!   counts them); events pushed after it are kept. On its frame, every
//!   note of the part still sounding fades out in a straight line over
//!   [`CANCEL_FADE_SECONDS`] (8 ms), on every kind of part; a note started
//!   on the same frame, after it, plays in full. To replace what is left of
//!   an edited bar, send a cancel, then the new events. A full queue takes
//!   a cancel all the same, since it only makes room.
//! - **All notes off without a click.** All notes off lets every note of a
//!   part go with a fade: the synth's release (5 ms at the shortest), the
//!   drums' [`DAMP_SECONDS`] (50 ms), a sampler's own releases but never
//!   less than [`PANIC_FADE_SECONDS`] (8 ms). `tests/stage.rs` stops a loud
//!   sound of each kind of part at its loudest frame and checks that no
//!   step from one sample to the next afterwards is larger than the largest
//!   step before by more than 2% of its peak (none is larger at all; a cut
//!   adds 73 to 92%).
//! - **Latency.** [`Engine::latency_frames`] is how far ahead of
//!   [`Engine::frame`] an event pushed from another thread must be due to
//!   be sure of landing on its frame: one render quantum and the master
//!   bus's look-ahead ([`Engine::master_latency_frames`], 111 frames at
//!   48 kHz), which the engine renders its parts ahead by. In a browser
//!   that is 239 frames; `js/engine.js` adds the context's output latency.
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
//! # Effects
//!
//! A part's [`Chain`] says which [`Effect`]s it has: high-pass, low-pass,
//! EQ, drive, bitcrush, compressor, chorus, flanger, phaser, tremolo,
//! auto-pan, delay and reverb, which always run in that order
//! ([`Effect::PART_ORDER`]). An effect in the chain has its memory and its
//! parameters (`"delay.time"`, listed in [`ids`]); its switch, a parameter
//! like any other (`"delay.on"`), switches it on and off on an exact
//! frame. An effect not in the chain, or switched off and done fading,
//! is skipped and costs nothing. Switching fades over
//! [`EFFECT_FADE_SECONDS`] (the delay and the reverb let their echoes and
//! tails ring on), every setting glides, and nothing clicks. Each effect
//! names in its parameters' JSON what an editor draws for it, a
//! [`Visual`], and [`Engine::effect_curve`] gives the points, worked out
//! from the parameters outside the render call. The crate's README lists
//! every parameter with its unit, range and default.
//!
//! # The master bus
//!
//! The parts and the return of a shared reverb, which each part feeds by
//! its `send`, go through a glue compressor (off until `master.glue.on`),
//! `master.gain` and a look-ahead limiter, `manguebyte-dsp`'s
//! [`Limiter`](manguebyte_dsp::Limiter), that is true-peak aware: it is fed
//! the waveform between the samples, read by interpolating by eight, and
//! keeps it under `master.limiter.ceiling`, -0.3 dBTP by default, and every
//! sample within [`OUTPUT_CEILING`] (about -0.3 dB below full scale).
//! Below the ceiling it leaves the sound exactly as it is: one note or one
//! drum at full velocity and default settings stays below it (a single note
//! of the synth peaks around -9 dB, a drum at -4.5 dB or lower); loud
//! chords and several loud parts together are turned down in a ramp before
//! their peaks and let back up over [`LIMITER_RELEASE_SECONDS`], never
//! clipped. [`EngineStats::limited`] counts the frames it turned down. The
//! look-ahead, [`MASTER_LOOKAHEAD_SECONDS`], and the reading between
//! samples, [`TRUE_PEAK_DELAY`] frames, make the bus
//! [`master_latency_frames`] late, which the engine renders its parts ahead
//! by, so events stay on their frames. `master.tempo` and `master.beat` are
//! what synced effects follow. A master parameter's record may name any
//! part; it belongs to none ([`Engine::master_params_json`]).
//!
//! # Output level
//!
//! See [the master bus](#the-master-bus): every sample stays within
//! [`OUTPUT_CEILING`], whatever the parts do.
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
//! | 8      | `kind`     | u8   | 1 note on, 2 note off, 3 param, 4 choke, 5 all notes off, 6 cancel |
//! | 9      | `flags`    | u8   | bit 0 accent (note on); bits 1 to 7 reserved: written as 0, ignored |
//! | 10..12 | `part`     | u16  | the part the event is for |
//! | 12     | `note`     | u8   | MIDI note, 0 to 127 (note on and note off) |
//! | 13     | `channel`  | u8   | MIDI channel, 0 to 15 (note on and note off) |
//! | 14..16 | padding    |      | written as 0, never read |
//! | 16..20 | `velocity` | f32  | 0 to 1 (note on); 0 is a note off |
//! | 20..24 | `param`    | u32  | a [`ParamId`]: the 32-bit FNV-1a hash of the parameter's identifier (param) |
//! | 24..32 | `value`    | f64  | the parameter's new value in its own units, finite (param) |
//!
//! Code 0 and codes above 6 are not kinds. A record with an unknown kind,
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
//! | high tom | `high_tom` | 50 | `t` |
//! | mid tom | `mid_tom` | 47 | `m` |
//! | floor tom | `floor_tom` | 43 | `f` |
//! | side stick | `side_stick` | 37 | `k` |
//! | guitar body tap | `guitar_tap` | 88 | none (`null`) |
//!
//! The JSON's `symbol` is a one-letter string, or `null` for a drum
//! Aratu does not write in a drum line. A closed hat chokes a sounding
//! open hat, with a fade of [`HAT_CHOKE_SECONDS`].
//!
//! The toms are tuned membranes, at 165, 123 and 84 Hz before
//! [`ids::KIT_TUNE`], that start about a third sharp and settle within a
//! quarter of a second, with the stick's noise; they ring for about 0.5,
//! 0.6 and 0.8 s (to -60 dB, times [`ids::KIT_DECAY`]). The side stick is a
//! woody knock of a few resonances between 330 Hz and 3 kHz on a sharp
//! click, gone within 80 ms. The guitar tap is the thump of a guitar's
//! body, its air resonance at 100 Hz and its top at 205 and 240 Hz, for
//! about 0.3 s, under the click of a nail. Note 88 is not a General MIDI
//! drum and has no symbol in a drum line, so the JSON gives `null` for it;
//! a part strikes it by its note. The first six rows keep their places;
//! new drums are added after them. `tests/kit.rs` measures each drum, and
//! the `drum_wavs` example writes every one to a WAV file.
//!
//! # Sampler parts
//!
//! A part made as an [`InstrumentKind::Sampler`] plays an SFZ instrument
//! through [`manguebyte_sampler`], and is silent until it has one.
//! [`SfzLoad`] parses the text, lists the sample files it names, decodes them
//! one at a time and makes the instrument, and [`Engine::load_instrument`]
//! gives it to the part, whose sounding notes ring on with the instrument
//! they started with. Samples play at their own rate, read at the ratio of
//! that rate to the engine's. A sample peaking at full scale peaks at
//! [`SAMPLER_LEVEL_DB`] (-6 dB) at full velocity; an accent adds
//! [`SAMPLER_ACCENT_DB`] (3 dB); choke stops every voice after one more
//! frame, and all notes off releases every voice in no less than
//! [`PANIC_FADE_SECONDS`] (8 ms). A part made as an
//! [`InstrumentKind::SamplerKit`] is a sampler marked as a drum kit: for a
//! note of the drum map its SFZ text has no region for, it plays the drum
//! of [`InstrumentKind::Drums`], at the note's velocity and with its accent,
//! and it plays every drum that way until it has an instrument. In a browser, `js/worklet.js` decodes the
//! files between render quanta, a file or so at a time, so that loading
//! never holds up the sound; the crate's README explains that choice.
//!
//! # Real time
//!
//! [`Engine::push`], [`Engine::push_bytes`], [`Engine::admit_bytes`],
//! [`Engine::drain`] and [`Engine::render`] never allocate, lock or panic,
//! every kind of instrument and every effect included (an instrument's
//! memory is made when a part is given it, and an effect's when it joins a
//! part's chain, by control calls), and rendering holds
//! [`NoDenormals`](manguebyte_core::NoDenormals) (which does nothing in
//! WebAssembly, where the reverbs, the delay and the organ's speaker are
//! put to sleep in silence instead, and the master bus comes to rest).
//! `tests/realtime.rs` checks the first part of that with a counting
//! allocator, with every kind of instrument playing and every effect on.

#![deny(unsafe_code)]

mod chain;
mod curve;
mod drums;
mod effect;
mod engine;
#[allow(unsafe_code)]
pub mod ffi;
mod fx;
mod load;
mod master;
mod params;
mod queue;
pub mod record;
pub mod ring;
mod sampled;
mod synth;
mod voiced;

pub use curve::{Curve, MAX_CURVE_POINTS};
pub use drums::{
    CYMBAL_ACCENT_SNAP, DAMP_SECONDS, DRUM_ACCENT_DB, DRUM_ACCENT_SNAP, DRUM_ACCENT_SNAP_SECONDS,
    DRUM_MAP, Drum, DrumNote, HAT_CHOKE_SECONDS, drum_map_json,
};
pub use effect::{Effect, Visual};
pub use engine::{
    CANCEL_FADE_SECONDS, Chain, Engine, EngineConfig, EngineError, EngineStats, InstrumentKind,
    LIMITER_RELEASE_SECONDS, OUTPUT_CEILING, Pushed,
};
pub use fx::{DELAY_GLIDE_SECONDS, DELAY_MAX_SECONDS, EFFECT_FADE_SECONDS, WAVE_SMOOTHING_SECONDS};
pub use load::{SfzInstrument, SfzLoad};
pub use manguebyte_params::ParamId;
pub use manguebyte_sampler;
pub use master::{MASTER_LOOKAHEAD_SECONDS, TRUE_PEAK_DELAY, master_latency_frames};
pub use params::ids;
pub use record::{Event, EventRecord, RECORD_SIZE, RecordKind};
pub use ring::{RingConsumer, RingProducer, event_ring, event_ring_starting_at, ring_bytes};
pub use sampled::{PANIC_FADE_SECONDS, SAMPLER_ACCENT_DB, SAMPLER_LEVEL_DB, SAMPLER_VOICES};
pub use synth::{
    RETRIGGER_GLIDE_SECONDS, STEAL_FADE_SECONDS, SYNTH_ACCENT_ATTACK, SYNTH_ACCENT_DB,
    SYNTH_ACCENT_OCTAVES, SYNTH_VOICES,
};
pub use voiced::{
    ANALOG_KIT_LEVEL_DB, ANALOG_KIT_VOICES, FM_LEVEL_DB, FM_VOICES, ORGAN_VOICES,
    PLUCKED_LEVEL_DB, PLUCKED_VOICES, SPEAKER_SLEEP_SECONDS,
};
