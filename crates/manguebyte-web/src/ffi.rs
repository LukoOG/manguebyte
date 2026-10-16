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
//! 2. [`mb_create_part`] for each part, and [`mb_set_chain`] as needed.
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
//! 3. Call [`mb_sfz_finish`], which hands the instrument to the part;
//!    later, [`mb_collect`] frees the instruments parts are done with.
//!
//! A load that fails writes why into the text buffer ([`mb_text_ptr`],
//! [`mb_text_len`]).
//!
//! The staging area and the output buffers never move, but any call that
//! allocates (making parts, describing parameters) may grow the memory, and
//! a grown memory has a new `ArrayBuffer`: views made on the old one must
//! be made again. [`mb_render`] and [`mb_push_staged`] never allocate.
//!
//! Every function that takes an engine accepts a null pointer and then
//! does nothing, returning 0, a null pointer, [`status::NO_ENGINE`] or NaN.

use crate::drums::drum_map_json;
use crate::engine::{Chain, Engine, EngineConfig, EngineError, InstrumentKind};
use crate::load::{SfzLoad, summary_json};
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
pub const ABI_VERSION: u32 = 2;

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
    loads: Vec<(u16, SfzLoad)>,
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
/// before the first. The engine has no look-ahead; the device's output
/// latency comes on top.
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
        Some(index) => web.loads[index].1 = load,
        None => web.loads.push((number, load)),
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
            web.text = web.loads[index].1.missing_json();
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
/// name, or one already given, is ignored. Returns a [`status`]: on
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
    match web.loads[index].1.add_file(&path, bytes) {
        Ok(_) => status::OK,
        Err(error) => {
            web.loads.swap_remove(index);
            web.fail(status::BAD_SAMPLE, error)
        }
    }
}

/// Finishes the load of `part` and gives the instrument to the part, which
/// plays new notes with it from the next frame rendered
/// ([`Engine::load_instrument`](crate::Engine::load_instrument)). On
/// success the text buffer holds a JSON summary,
/// `{"regions":10,"samples":10,"sample_bytes":1234,"warnings":["..."]}`.
/// Returns a [`status`]: [`status::BAD_SAMPLE`] if a file is still
/// missing, with the text buffer naming it. The load is over either way.
/// Allocates.
///
/// # Safety
///
/// `engine` is null or a live pointer from [`mb_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mb_sfz_finish(engine: *mut WebEngine, part: u32) -> i32 {
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
    let instrument = match load.finish() {
        Ok(instrument) => instrument,
        Err(error) => return web.fail(status::BAD_SAMPLE, error),
    };
    let summary = summary_json(&instrument);
    match web.engine.load_instrument(number, instrument) {
        Ok(()) => {
            web.text = summary;
            status::OK
        }
        Err(error) => web.fail(status_of(Err(error)), error),
    }
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
