//! The WebAssembly exports: plain `extern "C"` functions over the module's
//! memory, with no binding generator.
//!
//! Built with `cargo build -p manguebyte-web --target wasm32-unknown-unknown`,
//! the crate is a module that imports nothing and exports these functions
//! and its `memory`. Pointers are byte offsets into that memory; `u32` and
//! `i32` are JavaScript numbers, `f64` too.
//!
//! The usual sequence, which `js/worklet.js` follows:
//!
//! 1. [`mb_engine_new`] once, keeping the returned engine.
//! 2. [`mb_create_part`] for each part, [`mb_set_instrument`] to give one
//!    another instrument, and [`mb_set_chain`] as needed.
//! 3. For every quantum: copy records into the staging area at
//!    [`mb_staging_ptr`] and call [`mb_admit_staged`], keeping the records
//!    it did not take for the next quantum (or call [`mb_push_staged`],
//!    which takes them all and drops what does not fit); call
//!    [`mb_render`]; read the frames from [`mb_output_ptr`] for channels 0
//!    and 1.
//!
//! Loading an SFZ instrument into a sampler part, which `js/worklet.js`
//! does a step at a time between render quanta:
//!
//! 1. Write the SFZ text at [`mb_input_ptr`] and call [`mb_sfz_begin`];
//!    [`mb_sfz_files_json`] lists the sample files it names.
//! 2. For each file, write its path and then its bytes at [`mb_input_ptr`]
//!    and call [`mb_sfz_add_file`], which decodes it.
//! 3. Call [`mb_sfz_install`], which hands the instrument to the part at
//!    once: from the next frame rendered, it plays at a provisional
//!    make-up, an estimate of what its loudness normalisation will find.
//! 4. Call [`mb_sampler_measure`] until it returns [`status::OK`] rather
//!    than [`status::MEASURING`]: each call renders a few thousand frames of
//!    the phrase the normalisation measures a clone of the instrument on,
//!    and the last glides the part to the make-up measured
//!    ([`mb_sampler_make_up_db`]). Later, [`mb_collect`] frees the
//!    instruments parts are done with.
//!
//! The older way, which still works, measures before the part plays the
//! instrument: after step 2, call [`mb_sfz_measure`] until it returns
//! [`status::OK`], then [`mb_sfz_finish`], which hands the instrument over
//! at the make-up measured (and measures what was left, all at once).
//!
//! A load that fails writes why into the text buffer ([`mb_text_ptr`],
//! [`mb_text_len`]).
//!
//! Effects: [`mb_set_chain`] (or [`mb_create_part`]) puts them in a part's
//! chain, which is where their memory is made; their switches and settings
//! are parameters, sent in records like any other. [`mb_part_chain`] reads
//! a chain back, [`mb_effect_curve`] and [`mb_curve_ptr`] give the points
//! an editor draws for an effect, [`mb_effect_running`] whether it is doing
//! work. The master bus's parameters are records too, whatever part they
//! name; [`mb_master_params_json`] describes them and
//! [`mb_master_param_value`] reads them, and [`mb_limiter_reduction_db`]
//! feeds a meter.
//!
//! Scopes and levels, for an editor to draw each part: after a render,
//! [`mb_part_scope`] copies a part's latest frames to [`mb_scope_ptr`] and
//! [`mb_part_scope_end`] says which frame follows them; [`mb_part_levels`]
//! writes its peak and RMS levels to [`mb_levels_ptr`]. None of them
//! allocates, so `js/worklet.js` calls them on the audio thread after
//! every quantum, to copy what changed into shared memory.
//!
//! The staging area, the output buffers, the scope area and the levels
//! never move, but any call that
//! allocates (making parts, describing parameters) may grow the memory, and
//! a grown memory has a new `ArrayBuffer`: views made on the old one must
//! be made again. [`mb_render`] and [`mb_push_staged`] never allocate.
//!
//! Every function that takes an engine accepts a null pointer and then
//! does nothing, returning 0, a null pointer, [`status::NO_ENGINE`] or NaN.

use crate::drums::drum_map_json;
use crate::effect::Effect;
use crate::engine::{Chain, Engine, EngineConfig, EngineError, InstrumentKind};
use crate::load::{SfzInstrument, SfzLoad, summary_json};
use crate::meter::SCOPE_FRAMES;
use crate::probe::LoudnessProbe;
use crate::record::RECORD_SIZE;
use manguebyte_params::ParamId;

/// The version of this interface, from [`mb_abi_version`]. It changes
/// whenever an export changes meaning or one that `js/worklet.js` cannot do
/// without is added. Version 2 added [`mb_admit_staged`],
/// [`mb_min_sample_rate`], [`mb_max_sample_rate`] and counters 6 to 9 of
/// [`mb_stat`]. The flags byte of the record
/// ([`record::flag`](crate::record::flag)) did not change it: the byte was
/// padding written as 0, a 0 still means no flag, and every export means
/// what it did. Nor did the exports that load SFZ instruments
/// ([`mb_text_ptr`], [`mb_input_ptr`], [`mb_sfz_begin`],
/// [`mb_sfz_files_json`], [`mb_sfz_add_file`], [`mb_sfz_finish`],
/// [`mb_sfz_abandon`], [`mb_collect`]), [`mb_latency_frames`], counter 10
/// of [`mb_stat`], instrument codes 2 and 3 and record kind 6 (cancel):
/// they are only added, code that predates them never calls or sends them,
/// and `js/worklet.js` looks for the exports by name, so it runs a module
/// without them and says, when asked to load, that it has no sampler parts.
/// Kind 6 was a code an engine ignored and counted; a writer that sends it
/// to an older engine gets that, and no writer sent it before.
///
/// The effects and the master bus did not change it either. What they add
/// is added beside what was there, and nothing that was there means
/// anything else: chain bits 2 to 12 (an older engine refuses them with
/// [`status::BAD_CHAIN`], so a page knows), parameter ids it routes where
/// an older engine counts them as unrouted, the exports
/// [`mb_master_params_json`], [`mb_master_param_value`],
/// [`mb_effect_curve`], [`mb_curve_ptr`], [`mb_part_chain`],
/// [`mb_effect_running`] and [`mb_limiter_reduction_db`], which
/// `js/engine.js` only calls for what the new API asks, and status
/// [`status::NO_SUCH_EFFECT`]. The record, the ring and the drum map are
/// untouched. Two things are different and are not changes of meaning: a
/// part's description has more entries (each effect's switch, the send,
/// and `"effect"` and `"visual"` fields), which a reader that looks
/// parameters up by id reads as before; and [`mb_latency_frames`] is
/// larger by the master bus's look-ahead, which is what it is for: an
/// event scheduled that far ahead lands on its frame, exactly as before.
///
/// Nor did the instruments of `manguebyte-dsp`: instrument codes 4 to 9
/// are only added (an older engine refuses them with
/// [`status::BAD_INSTRUMENT`]), with parameter ids of their own and the
/// export [`mb_set_instrument`], which `js/worklet.js` looks for by name.
/// A note on before the last note's note off was two notes and still is on
/// every kind that was there; on the two monophonic kinds it is a slide.
/// Nor did the newer voices: codes 10 to 15 (the drum kits, the
/// polyphonic synthesiser, the mallets, the bowed strings, the winds and
/// the choir) are only added, with parameter ids of their own; a drum kit
/// answers the drum map as the other kits do.
///
/// Nor did loudness normalisation: [`mb_sfz_measure`] and its status
/// [`status::MEASURING`] are added, and `js/worklet.js` looks for the export
/// by name; [`mb_sfz_finish`] still finishes a load in one call, measuring
/// it there if no step did; and `sampler.normalize` is a parameter id of
/// its own. Every kind playing at the loudness target changes levels, not
/// what any export or record means.
/// The synth takes its accent as an argument of the note now, and sounds
/// the same.
///
/// Nor did playing an instrument before it is measured: [`mb_sfz_install`],
/// [`mb_sampler_measure`] and [`mb_sampler_make_up_db`] are added, and
/// `js/worklet.js` looks for them by name, loading the older way without
/// them. [`mb_sfz_measure`] still measures before the part plays the
/// instrument, and [`mb_sfz_finish`] still hands it over measured. The one
/// difference those two show is in round robins and random regions: the
/// part now plays the instrument from where it was loaded, and a clone of
/// it is measured, where before the part went on from where the
/// measurement had left them.
///
/// Nor did the scopes and levels of the parts: [`mb_scope_frames`],
/// [`mb_part_scope`], [`mb_scope_ptr`], [`mb_part_scope_end`],
/// [`mb_part_levels`] and [`mb_levels_ptr`] are only added, and
/// `js/worklet.js` looks for them by name, answering that the module has
/// no scopes without them. What the parts sound like, and every other
/// export, is unchanged.
pub const ABI_VERSION: u32 = 2;

/// The part number [`mb_effect_curve`] and [`mb_effect_running`] take for
/// the master bus: above every part, -1 from JavaScript.
pub const MASTER: u32 = u32::MAX;

/// The most records the staging area can hold.
pub const MAX_STAGING_RECORDS: u32 = 65_536;

/// What the control exports return.
pub mod status {
    /// Done.
    pub const OK: i32 = 0;
    /// The engine pointer was null.
    pub const NO_ENGINE: i32 = -1;
    /// The part number is above 65 535.
    pub const BAD_PART: i32 = -2;
    /// The instrument code is not one of
    /// [`InstrumentKind::code`](crate::InstrumentKind::code).
    pub const BAD_INSTRUMENT: i32 = -3;
    /// The chain has a bit that is not a stage.
    pub const BAD_CHAIN: i32 = -4;
    /// Every part slot is taken.
    pub const TOO_MANY_PARTS: i32 = -5;
    /// There is no such part.
    pub const NO_SUCH_PART: i32 = -6;
    /// A configuration value was out of range.
    pub const INVALID_CONFIG: i32 = -7;
    /// The part does not play SFZ instruments.
    pub const NOT_A_SAMPLER: i32 = -8;
    /// No SFZ instrument is being loaded for the part.
    pub const NO_LOAD: i32 = -9;
    /// The SFZ text could not be read; the text buffer says why.
    pub const BAD_SFZ: i32 = -10;
    /// A sample could not be decoded, or is missing; the text buffer says
    /// which and why.
    pub const BAD_SAMPLE: i32 = -11;
    /// The lengths given run past the end of the input area.
    pub const BAD_INPUT: i32 = -12;
    /// The part has been given instruments it has not taken yet: render,
    /// and try again.
    pub const LOADER_FULL: i32 = -13;
    /// The effect code is not one of
    /// [`Effect::code`](crate::Effect::code), or the part or the master bus
    /// cannot have that effect.
    pub const NO_SUCH_EFFECT: i32 = -14;
    /// Not an error: [`mb_sfz_measure`](super::mb_sfz_measure) did a step
    /// and there are more to do.
    pub const MEASURING: i32 = 1;
}

/// The largest input area [`mb_input_ptr`] makes, in bytes: 256 MiB.
pub const MAX_INPUT_BYTES: u32 = 1 << 28;

/// An engine with the memory the exports hand to JavaScript.
pub struct WebEngine {
    engine: Engine,
    staging: Vec<u8>,
    text: String,
    /// Bytes JavaScript hands over: SFZ text, sample files.
    input: Vec<u8>,
    /// The SFZ instruments being loaded, by part.
    loads: Vec<(u16, Load)>,
    /// The points of the last curve, `x` and `y` in turn.
    curve: Vec<f32>,
    /// The frames of the last [`mb_part_scope`], left then right: room for
    /// [`SCOPE_FRAMES`] of each, made with the engine.
    scope: Vec<f32>,
    /// The levels of the last [`mb_part_levels`].
    levels: [f32; 4],
}

/// An SFZ instrument on its way to a part.
enum Load {
    /// Its sample files arriving and being decoded.
    Decoding(SfzLoad),
    /// Made, and a clone of it being measured for the part's loudness
    /// normalisation before the part plays it.
    Measuring {
        instrument: SfzInstrument,
        probe: LoudnessProbe,
    },
}

impl WebEngine {
    /// The engine inside.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The engine inside, mutably.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }
}

/// # Safety
///
/// `engine` is null or a pointer from [`mb_engine_new`] that has not been
/// freed, and nothing else uses it for the lifetime `'a`.
unsafe fn web<'a>(engine: *mut WebEngine) -> Option<&'a mut WebEngine> {
    // SAFETY: the caller promises the pointer is null or live and unshared.
    unsafe { engine.as_mut() }
}

fn status_of(result: Result<(), EngineError>) -> i32 {
    match result {
        Ok(()) => status::OK,
        Err(EngineError::TooManyParts { .. }) => status::TOO_MANY_PARTS,
        Err(EngineError::NoSuchPart(_)) => status::NO_SUCH_PART,
        Err(EngineError::InvalidConfig(_)) => status::INVALID_CONFIG,
        Err(EngineError::NotASampler(_)) => status::NOT_A_SAMPLER,
        Err(EngineError::LoaderFull(_)) => status::LOADER_FULL,
    }
}

/// The version of this interface: [`ABI_VERSION`].
#[unsafe(no_mangle)]
pub extern "C" fn mb_abi_version() -> u32 {
    ABI_VERSION
}

/// The size of one event record: 32.
#[unsafe(no_mangle)]
pub extern "C" fn mb_record_size() -> u32 {
    RECORD_SIZE as u32
}

/// The lowest sample rate [`mb_engine_new`] accepts, in hertz:
/// [`EngineConfig::MIN_SAMPLE_RATE`].
#[unsafe(no_mangle)]
pub extern "C" fn mb_min_sample_rate() -> f64 {
    EngineConfig::MIN_SAMPLE_RATE
}

/// The highest sample rate [`mb_engine_new`] accepts, in hertz:
/// [`EngineConfig::MAX_SAMPLE_RATE`].
#[unsafe(no_mangle)]
pub extern "C" fn mb_max_sample_rate() -> f64 {
    EngineConfig::MAX_SAMPLE_RATE
}

/// Makes an engine, or returns null if an argument is out of range (see
/// [`EngineConfig`]: the sample rate is [`mb_min_sample_rate`] to
/// [`mb_max_sample_rate`]; `staging_records` is 1 to
/// [`MAX_STAGING_RECORDS`]). Free it with [`mb_engine_free`].
#[unsafe(no_mangle)]
pub extern "C" fn mb_engine_new(
    sample_rate: f64,
    max_quantum: u32,
    max_parts: u32,
    pending_capacity: u32,
    staging_records: u32,
) -> *mut WebEngine {
    if !(1..=MAX_STAGING_RECORDS).contains(&staging_records) {
        return core::ptr::null_mut();
    }
    let config = EngineConfig {
        sample_rate,
        max_quantum: max_quantum as usize,
        max_parts: max_parts as usize,
        pending_capacity: pending_capacity as usize,
    };
    match Engine::new(config) {
        Ok(engine) => Box::into_raw(Box::new(WebEngine {
            engine,
            staging: vec![0; staging_records as usize * RECORD_SIZE],
            text: String::new(),
            input: Vec::new(),
            loads: Vec::new(),
            curve: Vec::new(),
            scope: vec![0.0; 2 * SCOPE_FRAMES],
            levels: [0.0; 4],
        })),
        Err(_) => core::ptr::null_mut(),
    }
}

/// Frees an engine.
///
/// # Safety
///
/// `engine` is null or a pointer from [`mb_engine_new`] that has not been
/// freed yet; it is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_engine_free(engine: *mut WebEngine) {
    if !engine.is_null() {
        // SAFETY: the pointer came from `Box::into_raw` in `mb_engine_new`
        // and the caller promises it is freed only once.
        drop(unsafe { Box::from_raw(engine) });
    }
}

/// Where the staging area starts: room for [`mb_staging_capacity`]
/// records, written by JavaScript and read by [`mb_push_staged`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_staging_ptr(engine: *mut WebEngine) -> *mut u8 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) => web.staging.as_mut_ptr(),
        None => core::ptr::null_mut(),
    }
}

/// How many records the staging area holds.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_staging_capacity(engine: *mut WebEngine) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| (web.staging.len() / RECORD_SIZE) as u32)
}

/// Pushes the first `count` records of the staging area, at most its
/// capacity, and returns how many were queued (see
/// [`Engine::push`](crate::Engine::push)). Never allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_push_staged(engine: *mut WebEngine, count: u32) -> u32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return 0;
    };
    let bytes = (count as usize)
        .saturating_mul(RECORD_SIZE)
        .min(web.staging.len());
    web.engine.push_bytes(&web.staging[..bytes]) as u32
}

/// Takes records from the start of the staging area, the first `count` at
/// most, and returns how many it took, as
/// [`Engine::admit_bytes`](crate::Engine::admit_bytes) does: it stops
/// before the first record that the full queue of waiting events would
/// drop, so the caller keeps that record and those after it for the next
/// quantum. Never allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_admit_staged(engine: *mut WebEngine, count: u32) -> u32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return 0;
    };
    let bytes = (count as usize)
        .saturating_mul(RECORD_SIZE)
        .min(web.staging.len());
    web.engine.admit_bytes(&web.staging[..bytes]) as u32
}

/// How many more events can wait in the engine.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_pending_room(engine: *mut WebEngine) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| web.engine.pending_room() as u32)
}

/// Forgets every waiting event.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_clear_pending(engine: *mut WebEngine) {
    // SAFETY: as the caller promises.
    if let Some(web) = unsafe { web(engine) } {
        web.engine.clear_pending();
    }
}

/// Renders `frames` frames, at most the engine's maximum quantum, and
/// returns how many were rendered. Never allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_render(engine: *mut WebEngine, frames: u32) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| web.engine.render(frames as usize) as u32)
}

/// Where output channel `channel` (0 left, 1 right) starts, as `f32`s;
/// null for any other channel. Holds the frames of the last
/// [`mb_render`] from its start.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_output_ptr(engine: *mut WebEngine, channel: u32) -> *const f32 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) if channel < 2 => web.engine.output_ptr(channel as usize),
        _ => core::ptr::null(),
    }
}

/// The frame the next [`mb_render`] starts on. Exact up to 2⁵³.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_frame(engine: *mut WebEngine) -> f64 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(f64::NAN, |web| web.engine.frame() as f64)
}

fn part_number(part: u32) -> Result<u16, i32> {
    u16::try_from(part).map_err(|_| status::BAD_PART)
}

/// Makes part `part` play instrument `kind`
/// ([`InstrumentKind::code`](crate::InstrumentKind::code)) through the
/// stages in `chain` ([`Chain::bits`](crate::Chain::bits)), replacing any
/// part already there. Returns a [`status`]. Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_create_part(
    engine: *mut WebEngine,
    part: u32,
    kind: u32,
    chain: u32,
) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let part = match part_number(part) {
        Ok(part) => part,
        Err(status) => return status,
    };
    let Some(kind) = InstrumentKind::from_code(kind) else {
        return status::BAD_INSTRUMENT;
    };
    let Some(chain) = Chain::from_bits(chain) else {
        return status::BAD_CHAIN;
    };
    status_of(web.engine.create_part_with(part, kind, chain))
}

/// Makes part `part` play instrument `kind`
/// ([`InstrumentKind::code`](crate::InstrumentKind::code)), keeping its
/// chain, the values of the chain's parameters, gain, pan and the send
/// ([`Engine::set_instrument`](crate::Engine::set_instrument)). Returns a
/// [`status`]. Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_set_instrument(engine: *mut WebEngine, part: u32, kind: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let part = match part_number(part) {
        Ok(part) => part,
        Err(status) => return status,
    };
    let Some(kind) = InstrumentKind::from_code(kind) else {
        return status::BAD_INSTRUMENT;
    };
    status_of(web.engine.set_instrument(part, kind))
}

/// Removes a part. Returns a [`status`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_remove_part(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    match part_number(part) {
        Ok(part) => status_of(web.engine.remove_part(part)),
        Err(status) => status,
    }
}

/// Sets which stages a part's chain has
/// ([`Chain::bits`](crate::Chain::bits)). Returns a [`status`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_set_chain(engine: *mut WebEngine, part: u32, chain: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let part = match part_number(part) {
        Ok(part) => part,
        Err(status) => return status,
    };
    let Some(chain) = Chain::from_bits(chain) else {
        return status::BAD_CHAIN;
    };
    status_of(web.engine.set_chain(part, chain))
}

/// Writes the parameters of a part as JSON
/// ([`Engine::params_json`](crate::Engine::params_json)) into the engine's
/// text buffer and returns where it starts, or null if there is no such
/// part. [`mb_text_len`] gives its length in bytes, UTF-8. The text stays
/// until the next call that writes text. Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_params_json(engine: *mut WebEngine, part: u32) -> *const u8 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return core::ptr::null();
    };
    let json = part_number(part)
        .ok()
        .and_then(|part| web.engine.params_json(part));
    match json {
        Some(json) => {
            web.text = json;
            web.text.as_ptr()
        }
        None => {
            web.text.clear();
            core::ptr::null()
        }
    }
}

/// Writes the drum map ([`drum_map_json`]) into the
/// engine's text buffer and returns where it starts; see
/// [`mb_params_json`]. Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_drum_map_json(engine: *mut WebEngine) -> *const u8 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return core::ptr::null();
    };
    web.text = drum_map_json();
    web.text.as_ptr()
}

/// The length in bytes of the text the last text call wrote.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_text_len(engine: *mut WebEngine) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| web.text.len() as u32)
}

/// The value a parameter of a part holds, or NaN if there is no such part
/// or parameter. `param` is a [`ParamId`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_param_value(engine: *mut WebEngine, part: u32, param: u32) -> f64 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return f64::NAN;
    };
    part_number(part)
        .ok()
        .and_then(|part| web.engine.param_value_by_id(part, ParamId(param)))
        .unwrap_or(f64::NAN)
}

/// How many voices of a part are sounding, or -1 if there is no such part.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_active_voices(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    part_number(part)
        .ok()
        .and_then(|part| web.engine.active_voices(part))
        .map_or(-1, |voices| voices as i32)
}

/// One of the engine's counters
/// ([`EngineStats`](crate::EngineStats)): 0 queued, 1 ignored, 2 dropped,
/// 3 late, 4 applied, 5 unrouted, 6 displaced, 7 stalled, 8 recovered,
/// 9 limited, 10 cancelled. NaN for any other index.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_stat(engine: *mut WebEngine, index: u32) -> f64 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return f64::NAN;
    };
    let stats = web.engine.stats();
    let value = match index {
        0 => stats.queued,
        1 => stats.ignored,
        2 => stats.dropped,
        3 => stats.late,
        4 => stats.applied,
        5 => stats.unrouted,
        6 => stats.displaced,
        7 => stats.stalled,
        8 => stats.recovered,
        9 => stats.limited,
        10 => stats.cancelled,
        _ => return f64::NAN,
    };
    value as f64
}

/// The engine's latency in frames
/// ([`Engine::latency_frames`](crate::Engine::latency_frames)): one render
/// quantum, the size of the last [`mb_render`], or the maximum quantum
/// before the first, and the master bus's look-ahead
/// ([`master_latency_frames`](crate::master_latency_frames), 111 frames at
/// 48 kHz). The device's output latency comes on top.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_latency_frames(engine: *mut WebEngine) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| web.engine.latency_frames() as u32)
}

/// Where the text buffer starts: the text the last text call wrote, which
/// [`mb_text_len`] measures. The SFZ exports write their error messages
/// there, and [`mb_sfz_finish`] a summary of the instrument it loaded.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_text_ptr(engine: *mut WebEngine) -> *const u8 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) => web.text.as_ptr(),
        None => core::ptr::null(),
    }
}

/// Makes the input area `bytes` long and returns where it starts, for
/// JavaScript to write SFZ text or a sample file into before the call that
/// reads it. Null if `bytes` is above [`MAX_INPUT_BYTES`]. Allocates, so
/// the memory may grow: views of it must be made again after this call.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_input_ptr(engine: *mut WebEngine, bytes: u32) -> *mut u8 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return core::ptr::null_mut();
    };
    if bytes > MAX_INPUT_BYTES {
        return core::ptr::null_mut();
    }
    web.input.resize(bytes as usize, 0);
    web.input.as_mut_ptr()
}

/// Checks that `part` is a sampler part.
fn sampler_part(engine: &Engine, part: u32) -> Result<u16, i32> {
    let part = part_number(part)?;
    match engine.part_kind(part) {
        None => Err(status::NO_SUCH_PART),
        Some(kind) if !kind.is_sampler() => Err(status::NOT_A_SAMPLER),
        Some(_) => Ok(part),
    }
}

impl WebEngine {
    /// Writes `message` into the text buffer and returns `status`.
    fn fail(&mut self, status: i32, message: impl core::fmt::Display) -> i32 {
        self.text = message.to_string();
        status
    }

    fn load_index(&self, part: u32) -> Option<usize> {
        let part = part_number(part).ok()?;
        self.loads
            .iter()
            .position(|(load_part, _)| *load_part == part)
    }
}

/// Starts loading an SFZ instrument for sampler part `part`, from the
/// first `bytes` bytes of the input area, which hold its text in UTF-8.
/// Replaces a load of the part not finished yet. [`mb_sfz_files_json`]
/// then lists the sample files it needs. Returns a [`status`]: on
/// [`status::BAD_SFZ`] the text buffer says what is wrong with the text.
/// Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_begin(engine: *mut WebEngine, part: u32, bytes: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let number = match sampler_part(&web.engine, part) {
        Ok(number) => number,
        Err(status) => return status,
    };
    let Some(text) = web.input.get(..bytes as usize) else {
        return web.fail(status::BAD_INPUT, "the text runs past the input area");
    };
    let load = match SfzLoad::new(&String::from_utf8_lossy(text)) {
        Ok(load) => load,
        Err(error) => return web.fail(status::BAD_SFZ, error),
    };
    match web.load_index(part) {
        Some(index) => web.loads[index].1 = Load::Decoding(load),
        None => web.loads.push((number, Load::Decoding(load))),
    }
    status::OK
}

/// Writes the sample files the load of `part` still needs into the text
/// buffer, as a JSON array of the paths its text gives them (after
/// `default_path`, normalised; see [`SfzLoad::paths`]), and returns where
/// it starts; null if the part has no load. Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_files_json(engine: *mut WebEngine, part: u32) -> *const u8 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return core::ptr::null();
    };
    match web.load_index(part) {
        Some(index) => {
            web.text = match &web.loads[index].1 {
                Load::Decoding(load) => load.missing_json(),
                Load::Measuring { .. } => "[]".to_owned(),
            };
            web.text.as_ptr()
        }
        None => {
            web.text.clear();
            core::ptr::null()
        }
    }
}

/// Decodes one sample file for the load of `part`. The input area holds
/// the file's path, `path_bytes` bytes of UTF-8 as [`mb_sfz_files_json`]
/// gave it, then the file's `file_bytes` bytes. A path the text does not
/// name, or one already given, is ignored, as is every file once
/// [`mb_sfz_measure`] has begun. Returns a [`status`]: on
/// [`status::BAD_SAMPLE`] the text buffer says why, and the load is over.
/// Allocates; the work is bounded by the size of the file.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_add_file(
    engine: *mut WebEngine,
    part: u32,
    path_bytes: u32,
    file_bytes: u32,
) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let Some(index) = web.load_index(part) else {
        return web.fail(
            status::NO_LOAD,
            "no SFZ instrument is being loaded for the part",
        );
    };
    let path_end = path_bytes as usize;
    let file_end = path_end + file_bytes as usize;
    if file_end > web.input.len() {
        return web.fail(status::BAD_INPUT, "the file runs past the input area");
    }
    let path = String::from_utf8_lossy(&web.input[..path_end]).into_owned();
    let bytes = web.input[path_end..file_end].to_vec();
    let Load::Decoding(load) = &mut web.loads[index].1 else {
        return status::OK;
    };
    match load.add_file(&path, bytes) {
        Ok(_) => status::OK,
        Err(error) => {
            web.loads.swap_remove(index);
            web.fail(status::BAD_SAMPLE, error)
        }
    }
}

/// Measures the instrument of the load of `part` for the part's loudness
/// normalisation, up to `frames` frames of the phrase it plays at a time
/// ([`PROBE_STEP_FRAMES`](crate::PROBE_STEP_FRAMES) is about what decoding
/// a 64 KiB file costs), before the part plays it. The first call makes
/// the instrument, every file being in, and the measurement of a clone of
/// it, and renders nothing. Returns a [`status`]: [`status::MEASURING`]
/// while there is more to measure, [`status::OK`] once it is done (at
/// once, for a part that does not normalise), and then [`mb_sfz_finish`]
/// gives the instrument to the part (or [`mb_sfz_install`], to go on
/// measuring while the part plays it). On an error the text buffer says
/// why, and the load is over: [`status::BAD_SAMPLE`] if a file is missing,
/// or the part's own if it is gone or plays something else now. Allocates
/// on the first call; the work of each is bounded by `frames`.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_measure(engine: *mut WebEngine, part: u32, frames: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let Some(index) = web.load_index(part) else {
        return web.fail(
            status::NO_LOAD,
            "no SFZ instrument is being loaded for the part",
        );
    };
    let (number, load) = web.loads.swap_remove(index);
    let (instrument, mut probe, made) = match load {
        Load::Measuring { instrument, probe } => (instrument, probe, false),
        Load::Decoding(load) => {
            let instrument = match load.finish() {
                Ok(instrument) => instrument,
                Err(error) => return web.fail(status::BAD_SAMPLE, error),
            };
            match web.engine.probe_instrument(number, &instrument) {
                Ok(probe) => (instrument, probe, true),
                Err(error) => return web.fail(status_of(Err(error)), error),
            }
        }
    };
    // Making the instrument and the measurement is a step of its own.
    let done = if made {
        probe.is_done()
    } else {
        probe.step(frames as usize)
    };
    web.loads
        .push((number, Load::Measuring { instrument, probe }));
    if done { status::OK } else { status::MEASURING }
}

/// Finishes the load of `part` and gives the instrument to the part, which
/// plays new notes with it from the next frame rendered
/// ([`Engine::load_instrument`](crate::Engine::load_instrument)), at the
/// make-up gain [`mb_sfz_measure`] measured, measuring what it left, or
/// everything if it was never called, all at once. On
/// success the text buffer holds a JSON summary,
/// `{"regions":10,"samples":10,"sample_bytes":1234,"warnings":["..."]}`.
/// Returns a [`status`]: [`status::BAD_SAMPLE`] if a file is still
/// missing, with the text buffer naming it. The load is over either way.
/// Allocates. [`mb_sfz_install`] gives the part the instrument before it
/// is measured instead.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_finish(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    unsafe { give(engine, part, false) }
}

/// Finishes the load of `part` and gives the instrument to the part at
/// once, before it is measured
/// ([`Engine::install_instrument`](crate::Engine::install_instrument)): it
/// plays new notes with it from the next frame rendered, at a provisional
/// make-up while [`mb_sampler_measure`] measures it, a step at a time, and
/// then at the make-up measured. A measurement [`mb_sfz_measure`] began
/// goes on where it was. On success the text buffer holds the summary
/// [`mb_sfz_finish`] gives. Returns a [`status`]: [`status::BAD_SAMPLE`]
/// if a file is still missing, with the text buffer naming it. The load is
/// over either way. Allocates; nothing is rendered, and the estimate of
/// the make-up reads about as many frames as one step of
/// [`mb_sampler_measure`] renders.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_install(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    unsafe { give(engine, part, true) }
}

/// [`mb_sfz_finish`], or with `now` [`mb_sfz_install`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
unsafe fn give(engine: *mut WebEngine, part: u32, now: bool) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let Some(index) = web.load_index(part) else {
        return web.fail(
            status::NO_LOAD,
            "no SFZ instrument is being loaded for the part",
        );
    };
    let (number, load) = web.loads.swap_remove(index);
    let (summary, given) = match load {
        Load::Decoding(load) => {
            let instrument = match load.finish() {
                Ok(instrument) => instrument,
                Err(error) => return web.fail(status::BAD_SAMPLE, error),
            };
            let summary = summary_json(&instrument);
            let given = if now {
                web.engine.install_instrument(number, instrument)
            } else {
                web.engine.load_instrument(number, instrument)
            };
            (summary, given)
        }
        Load::Measuring { instrument, probe } => {
            let summary = summary_json(&instrument);
            let given = if now {
                web.engine.install_probed(number, instrument, probe)
            } else {
                web.engine.load_probed(number, instrument, probe)
            };
            (summary, given)
        }
    };
    match given {
        Ok(()) => {
            web.text = summary;
            status::OK
        }
        Err(error) => web.fail(status_of(Err(error)), error),
    }
}

/// Measures the instrument sampler part `part` plays, if it is still being
/// measured after [`mb_sfz_install`], up to `frames` frames of its phrase
/// ([`PROBE_STEP_FRAMES`](crate::PROBE_STEP_FRAMES) is about what decoding
/// a 64 KiB file costs; with 0 it only asks), and once that is done has
/// the part play at the make-up measured, gliding there over 20 ms
/// ([`Engine::measure_instrument`](crate::Engine::measure_instrument)).
/// Returns a [`status`]: [`status::MEASURING`] while there is more to
/// measure, [`status::OK`] once it is done or if there was nothing to
/// measure, or [`status::NO_SUCH_PART`], [`status::NOT_A_SAMPLER`],
/// [`status::BAD_PART`]. Allocates; the work is bounded by `frames`.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sampler_measure(engine: *mut WebEngine, part: u32, frames: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let part = match part_number(part) {
        Ok(part) => part,
        Err(status) => return status,
    };
    match web.engine.measure_instrument(part, frames as usize) {
        Ok(true) => status::OK,
        Ok(false) => status::MEASURING,
        Err(error) => status_of(Err(error)),
    }
}

/// The make-up gain, in decibels, the loudness normalisation of sampler
/// part `part` measured for the instrument it plays
/// ([`Engine::sampler_make_up_db`](crate::Engine::sampler_make_up_db)), or
/// NaN: while it is still being measured, if it was not measured, or if
/// there is no such sampler part.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sampler_make_up_db(engine: *mut WebEngine, part: u32) -> f64 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return f64::NAN;
    };
    part_number(part)
        .ok()
        .and_then(|part| web.engine.sampler_make_up_db(part))
        .unwrap_or(f64::NAN)
}

/// Gives up the load of `part`, if there is one. Returns a [`status`]:
/// [`status::NO_LOAD`] if there was none.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_abandon(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    match web.load_index(part) {
        Some(index) => {
            web.loads.swap_remove(index);
            status::OK
        }
        None => status::NO_LOAD,
    }
}

/// Frees the SFZ instruments sampler parts have finished with
/// ([`Engine::collect_instruments`](crate::Engine::collect_instruments))
/// and returns how many. It frees, so it belongs between render calls.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_collect(engine: *mut WebEngine) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(0, |web| web.engine.collect_instruments() as u32)
}

/// The stages in a part's chain ([`Chain::bits`](crate::Chain::bits)), or
/// a [`status`]: [`status::NO_SUCH_PART`], [`status::BAD_PART`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_part_chain(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    match part_number(part) {
        Ok(part) => web
            .engine
            .part_chain(part)
            .map_or(status::NO_SUCH_PART, |chain| chain.bits() as i32),
        Err(status) => status,
    }
}

/// Writes the master bus's parameters as JSON
/// ([`Engine::master_params_json`](crate::Engine::master_params_json)) into
/// the text buffer and returns where it starts; see [`mb_params_json`].
/// Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_master_params_json(engine: *mut WebEngine) -> *const u8 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return core::ptr::null();
    };
    web.text = web.engine.master_params_json();
    web.text.as_ptr()
}

/// The value a parameter of the master bus holds, or NaN if there is no
/// such parameter. `param` is a [`ParamId`].
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_master_param_value(engine: *mut WebEngine, param: u32) -> f64 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(f64::NAN, |web| {
        web.engine
            .master_param_value_by_id(ParamId(param))
            .unwrap_or(f64::NAN)
    })
}

/// Works out the curve an editor draws for effect `effect`
/// ([`Effect::code`](crate::Effect::code)) of part `part`, or of the master
/// bus when `part` is [`MASTER`], with `points` points (2 to
/// [`MAX_CURVE_POINTS`](crate::MAX_CURVE_POINTS)), from the values its
/// parameters hold ([`Engine::effect_curve`](crate::Engine::effect_curve)).
/// Returns how many points it wrote, at [`mb_curve_ptr`], each an `x` and
/// a `y` as `f32`s in the axes of the effect's
/// [`Visual`](crate::Visual), or a [`status`]: [`status::NO_SUCH_PART`],
/// [`status::BAD_PART`], [`status::NO_SUCH_EFFECT`]. It reads the
/// parameters only, never what the render call works on, and allocates,
/// so it belongs between render calls, which is where the worklet's
/// messages are handled.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_effect_curve(
    engine: *mut WebEngine,
    part: u32,
    effect: u32,
    points: u32,
) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let Some(effect) = Effect::from_code(effect) else {
        return status::NO_SUCH_EFFECT;
    };
    let curve = if part == MASTER {
        web.engine.master_curve(effect, points as usize)
    } else {
        let number = match part_number(part) {
            Ok(number) => number,
            Err(status) => return status,
        };
        if web.engine.part_kind(number).is_none() {
            return status::NO_SUCH_PART;
        }
        web.engine.effect_curve(number, effect, points as usize)
    };
    let Some(curve) = curve else {
        return status::NO_SUCH_EFFECT;
    };
    web.curve.clear();
    web.curve.extend(curve.points.iter().flatten());
    curve.points.len() as i32
}

/// Where the points of the last [`mb_effect_curve`] start: `x` and `y` in
/// turn, as `f32`s. They stay until the next curve.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_curve_ptr(engine: *mut WebEngine) -> *const f32 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) => web.curve.as_ptr(),
        None => core::ptr::null(),
    }
}

/// Whether effect `effect` of part `part` has work to do
/// ([`Engine::effect_running`](crate::Engine::effect_running)): 1 if it
/// is on, fading, or ringing, 0 if it is skipped, or a [`status`]. With
/// `part` [`MASTER`], whether the glue compressor ([`Effect::Glue`]) is in
/// or the shared reverb ([`Effect::Reverb`]) is awake.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_effect_running(engine: *mut WebEngine, part: u32, effect: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let Some(effect) = Effect::from_code(effect) else {
        return status::NO_SUCH_EFFECT;
    };
    if part == MASTER {
        return match effect {
            Effect::Glue => i32::from(web.engine.glue_running()),
            Effect::Reverb => i32::from(!web.engine.master_reverb_asleep()),
            Effect::Limiter => 1,
            _ => status::NO_SUCH_EFFECT,
        };
    }
    if !effect.is_part_effect() {
        return status::NO_SUCH_EFFECT;
    }
    match part_number(part) {
        Ok(number) => web
            .engine
            .effect_running(number, effect)
            .map_or(status::NO_SUCH_PART, i32::from),
        Err(status) => status,
    }
}

/// How far the master limiter turned the level down on the last frame
/// rendered, in decibels, as a positive number; NaN without an engine.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_limiter_reduction_db(engine: *mut WebEngine) -> f64 {
    // SAFETY: as the caller promises.
    unsafe { web(engine) }.map_or(f64::NAN, |web| web.engine.limiter_reduction_db())
}

/// How many frames of each channel a part's scope holds:
/// [`SCOPE_FRAMES`], 4 096.
#[unsafe(no_mangle)]
pub extern "C" fn mb_scope_frames() -> u32 {
    SCOPE_FRAMES as u32
}

/// Copies the latest `frames` frames of part `part`, at most
/// [`mb_scope_frames`] and at most the frames since frame 0, to the scope
/// area ([`mb_scope_ptr`]): the left channel, then the right right after
/// it, as many `f32`s each ([`Engine::part_scope`](crate::Engine::part_scope)).
/// Returns how many frames, or a [`status`]: [`status::NO_SUCH_PART`],
/// [`status::BAD_PART`]. The first is frame [`mb_part_scope_end`] less that
/// many. Never allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_part_scope(engine: *mut WebEngine, part: u32, frames: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let number = match part_number(part) {
        Ok(number) => number,
        Err(status) => return status,
    };
    let Some(end) = web.engine.part_scope_end(number) else {
        return status::NO_SUCH_PART;
    };
    // Laid end to end: the right channel starts where the left one ends.
    let count = (frames as usize)
        .min(SCOPE_FRAMES)
        .min(usize::try_from(end).unwrap_or(usize::MAX));
    let (left, right) = web.scope.split_at_mut(count);
    match web
        .engine
        .part_scope_into(number, left, &mut right[..count])
    {
        Some((copied, _)) => copied as i32,
        None => status::NO_SUCH_PART,
    }
}

/// Where the scope area starts: the frames of the last [`mb_part_scope`],
/// left then right. It never moves.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_scope_ptr(engine: *mut WebEngine) -> *const f32 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) => web.scope.as_ptr(),
        None => core::ptr::null(),
    }
}

/// The frame after the newest one of part `part`'s scope: the next frame
/// the part renders ([`Engine::part_scope_end`](crate::Engine::part_scope_end)),
/// counted as events count them. Exact up to 2⁵³; NaN if there is no such
/// part.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_part_scope_end(engine: *mut WebEngine, part: u32) -> f64 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return f64::NAN;
    };
    part_number(part)
        .ok()
        .and_then(|part| web.engine.part_scope_end(part))
        .map_or(f64::NAN, |end| end as f64)
}

/// Writes the levels of part `part` on the newest frame of its scope
/// ([`Engine::part_levels`](crate::Engine::part_levels)) to [`mb_levels_ptr`]:
/// four `f32`s, the peak of the left and of the right channel, then their
/// RMS, linear. Returns a [`status`]: [`status::OK`],
/// [`status::NO_SUCH_PART`], [`status::BAD_PART`]. Never allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_part_levels(engine: *mut WebEngine, part: u32) -> i32 {
    // SAFETY: as the caller promises.
    let Some(web) = (unsafe { web(engine) }) else {
        return status::NO_ENGINE;
    };
    let number = match part_number(part) {
        Ok(number) => number,
        Err(status) => return status,
    };
    match web.engine.part_levels(number) {
        Some(levels) => {
            let [peak_left, peak_right] = levels.peak;
            let [rms_left, rms_right] = levels.rms;
            web.levels = [peak_left, peak_right, rms_left, rms_right];
            status::OK
        }
        None => status::NO_SUCH_PART,
    }
}

/// Where the levels of the last [`mb_part_levels`] are: four `f32`s. They
/// never move.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_levels_ptr(engine: *mut WebEngine) -> *const f32 {
    // SAFETY: as the caller promises.
    match unsafe { web(engine) } {
        Some(web) => web.levels.as_ptr(),
        None => core::ptr::null(),
    }
}
