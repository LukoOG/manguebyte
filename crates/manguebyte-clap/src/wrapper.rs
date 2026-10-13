//! The glue between the CLAP C ABI and a [`Plugin`].
//!
//! # Threads
//!
//! CLAP says which thread may call each function, and this file leans on
//! those rules instead of on locks:
//!
//! - `process`, `reset`, `start_processing` and `stop_processing` are called
//!   on the audio thread.
//! - `activate` and `deactivate` are called on the main thread, and only
//!   while the plugin is not processing.
//! - `params.flush` is called on the audio thread while the plugin is
//!   active, in place of `process`, and on the main thread otherwise.
//! - Everything else is called on the main thread.
//!
//! So the audio-side state ([`Audio`]) is only ever touched by one thread at
//! a time, and lives in an `UnsafeCell`. Everything the main thread needs
//! while audio runs goes through the parameters, which are atomics, or
//! through the few atomics on [`Instance`].

use std::any::TypeId;
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::host::clap_host;
use clap_sys::id::{CLAP_INVALID_ID, clap_id};
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::plugin_features::*;
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::event::{Event, EventBuffer, EventKind, Expression, NoteRef};
use manguebyte_dsp::{ProcessContext, ProcessSpec, Transport};
use manguebyte_midi::MidiMessage;
use manguebyte_params::{Param, ParamSet};
use manguebyte_plugin::{
    ParamWatcher, Plugin, PluginKind, apply_param_events, load_state, save_state,
};

/// Most events delivered to the plugin in one block. Extra events are
/// dropped rather than allocated for.
const EVENTS_PER_BLOCK: usize = 4_096;

/// Largest state a host may hand back, so a corrupt stream cannot make the
/// plugin allocate without bound.
const MAX_STATE_BYTES: usize = 64 << 20;

/// Runs `work`, turning a panic into `fallback`.
///
/// A panic must never cross into the host: unwinding through C is not
/// defined, and aborting would take the user's session with it.
fn guard<R>(fallback: R, work: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or(fallback)
}

// --- Parameter values as CLAP sees them ---------------------------------
//
// A continuous parameter is exposed as its position in 0..=1, so a host's
// automation lane has the same feel as the plugin's own control. A stepped
// parameter is exposed as a whole number of steps, which is what CLAP's
// "stepped" flag means.

fn step_count(param: &dyn Param) -> Option<f64> {
    param.step_count().filter(|&steps| steps > 0).map(f64::from)
}

fn to_clap(param: &dyn Param, plain: f64) -> f64 {
    let position = param.plain_to_normalized(plain);
    match step_count(param) {
        Some(steps) => (position * steps).round(),
        None => position,
    }
}

fn from_clap(param: &dyn Param, value: f64) -> f64 {
    let position = match step_count(param) {
        Some(steps) => value / steps,
        None => value,
    };
    param.normalized_to_plain(position)
}

/// Copies `text` into a fixed C buffer, cutting it at a character boundary
/// if it does not fit, and always ending it with a NUL.
fn copy_text(text: &str, out: &mut [c_char]) {
    let Some(room) = out.len().checked_sub(1) else {
        return;
    };
    let mut len = text.len().min(room);
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    for (slot, &byte) in out.iter_mut().zip(&text.as_bytes()[..len]) {
        *slot = byte as c_char;
    }
    out[len] = 0;
}

// --- Descriptor ---------------------------------------------------------

static DESCRIPTORS: Mutex<Vec<(TypeId, &'static clap_plugin_descriptor)>> = Mutex::new(Vec::new());

/// A C string that lives for the rest of the program.
fn leak_c_string(text: &str) -> *const c_char {
    let cleaned: String = text.chars().filter(|&c| c != '\0').collect();
    CString::new(cleaned)
        .expect("interior NULs were removed")
        .into_raw()
}

/// The CLAP descriptor of `P`, built once and kept for the rest of the
/// program, as CLAP requires descriptors to outlive every plugin instance.
fn descriptor<P: Plugin>() -> &'static clap_plugin_descriptor {
    let mut all = DESCRIPTORS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let key = TypeId::of::<P>();
    if let Some(&(_, found)) = all.iter().find(|(id, _)| *id == key) {
        return found;
    }

    let info = P::INFO;
    let mut features: Vec<*const c_char> = match info.kind {
        PluginKind::Instrument => vec![
            CLAP_PLUGIN_FEATURE_INSTRUMENT.as_ptr(),
            CLAP_PLUGIN_FEATURE_SYNTHESIZER.as_ptr(),
        ],
        PluginKind::Effect => vec![CLAP_PLUGIN_FEATURE_AUDIO_EFFECT.as_ptr()],
        PluginKind::NoteEffect => vec![CLAP_PLUGIN_FEATURE_NOTE_EFFECT.as_ptr()],
        PluginKind::Analyzer => vec![CLAP_PLUGIN_FEATURE_ANALYZER.as_ptr()],
    };
    features.push(ptr::null());

    let empty = leak_c_string("");
    let built: &'static clap_plugin_descriptor = Box::leak(Box::new(clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: leak_c_string(info.id),
        name: leak_c_string(info.name),
        vendor: leak_c_string(info.vendor),
        url: empty,
        manual_url: empty,
        support_url: empty,
        version: leak_c_string(info.version),
        description: leak_c_string(info.description),
        features: Box::leak(features.into_boxed_slice()).as_ptr(),
    }));
    all.push((key, built));
    built
}

// --- Entry point and factory --------------------------------------------

/// The CLAP entry point for a library that exports the single plugin `P`.
///
/// [`export_clap!`](crate::export_clap) puts the result in the `clap_entry`
/// symbol a host looks for. Tests can call it directly to drive a plugin
/// through the real C interface.
pub const fn plugin_entry<P: Plugin>() -> clap_plugin_entry {
    clap_plugin_entry {
        clap_version: CLAP_VERSION,
        init: Some(entry_init),
        deinit: Some(entry_deinit),
        get_factory: Some(entry_get_factory::<P>),
    }
}

unsafe extern "C" fn entry_init(_plugin_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory<P: Plugin>(factory_id: *const c_char) -> *const c_void {
    if factory_id.is_null() {
        return ptr::null();
    }
    // SAFETY: the host passes a valid NUL-terminated string.
    let id = unsafe { CStr::from_ptr(factory_id) };
    if id == CLAP_PLUGIN_FACTORY_ID {
        ptr::from_ref(Vtables::<P>::FACTORY).cast()
    } else {
        ptr::null()
    }
}

/// The tables of function pointers handed to the host, one set per plugin
/// type. Associated constants of reference type live for the whole program.
struct Vtables<P>(PhantomData<P>);

impl<P: Plugin> Vtables<P> {
    const FACTORY: &'static clap_plugin_factory = &clap_plugin_factory {
        get_plugin_count: Some(factory_count),
        get_plugin_descriptor: Some(factory_descriptor::<P>),
        create_plugin: Some(factory_create::<P>),
    };
    const PARAMS: &'static clap_plugin_params = &clap_plugin_params {
        count: Some(params_count::<P>),
        get_info: Some(params_get_info::<P>),
        get_value: Some(params_get_value::<P>),
        value_to_text: Some(params_value_to_text::<P>),
        text_to_value: Some(params_text_to_value::<P>),
        flush: Some(params_flush::<P>),
    };
    const STATE: &'static clap_plugin_state = &clap_plugin_state {
        save: Some(state_save::<P>),
        load: Some(state_load::<P>),
    };
    const AUDIO_PORTS: &'static clap_plugin_audio_ports = &clap_plugin_audio_ports {
        count: Some(audio_ports_count::<P>),
        get: Some(audio_ports_get::<P>),
    };
    const NOTE_PORTS: &'static clap_plugin_note_ports = &clap_plugin_note_ports {
        count: Some(note_ports_count::<P>),
        get: Some(note_ports_get::<P>),
    };
    const LATENCY: &'static clap_plugin_latency = &clap_plugin_latency {
        get: Some(latency_get::<P>),
    };
}

unsafe extern "C" fn factory_count(_factory: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn factory_descriptor<P: Plugin>(
    _factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    guard(ptr::null(), || match index {
        0 => ptr::from_ref(descriptor::<P>()),
        _ => ptr::null(),
    })
}

unsafe extern "C" fn factory_create<P: Plugin>(
    _factory: *const clap_plugin_factory,
    host: *const clap_host,
    plugin_id: *const c_char,
) -> *const clap_plugin {
    guard(ptr::null(), || {
        if host.is_null() || plugin_id.is_null() {
            return ptr::null();
        }
        // SAFETY: the host passes a valid `clap_host` and a NUL-terminated id.
        let (host_version, wanted) = unsafe { ((*host).clap_version, CStr::from_ptr(plugin_id)) };
        if !clap_version_is_compatible(host_version) || wanted.to_bytes() != P::INFO.id.as_bytes() {
            return ptr::null();
        }

        let plugin = P::new();
        let params = plugin.params();
        if params.validate().is_err() {
            return ptr::null();
        }
        let mut ids = Vec::with_capacity(params.len());
        params.visit(&mut |param| ids.push(param.param_id()));

        let instance = Box::into_raw(Box::new(Instance::<P> {
            clap: clap_plugin {
                desc: descriptor::<P>(),
                plugin_data: ptr::null_mut(),
                init: Some(plugin_init),
                destroy: Some(plugin_destroy::<P>),
                activate: Some(plugin_activate::<P>),
                deactivate: Some(plugin_deactivate::<P>),
                start_processing: Some(plugin_start_processing),
                stop_processing: Some(plugin_stop_processing),
                reset: Some(plugin_reset::<P>),
                process: Some(plugin_process::<P>),
                get_extension: Some(plugin_get_extension::<P>),
                on_main_thread: Some(plugin_on_main_thread),
            },
            host,
            ids,
            latency: AtomicU32::new(0),
            host_knows_values: AtomicBool::new(true),
            audio: UnsafeCell::new(Audio {
                watcher: ParamWatcher::new(&*params),
                known_to_host: current_values(&*params),
                plugin,
                sample_rate: 0.0,
                buffer: AudioBuffer::new(P::LAYOUT.outputs, 0),
                sidechain: AudioBuffer::new(P::LAYOUT.sidechain, 0),
                events: EventBuffer::new(),
            }),
            params,
        }));
        // SAFETY: `instance` was just produced by `Box::into_raw`, so it is
        // valid and nothing else refers to it yet.
        unsafe {
            (*instance).clap.plugin_data = instance.cast();
            &raw const (*instance).clap
        }
    })
}

// --- Instance -----------------------------------------------------------

/// One plugin as the host sees it.
struct Instance<P: Plugin> {
    /// What the host holds a pointer to. Its `plugin_data` points back here.
    clap: clap_plugin,
    /// The host that created this plugin. CLAP keeps it valid for as long
    /// as the plugin exists.
    host: *const clap_host,
    params: Arc<P::Params>,
    /// The id of each parameter, in the order the host indexes them.
    ids: Vec<ParamId>,
    /// Latency reported by the plugin when it was last activated.
    latency: AtomicU32,
    /// Cleared when the host itself changed every value at once (by loading
    /// state), so that the audio thread does not report those changes back.
    host_knows_values: AtomicBool,
    /// See the module documentation for who may touch this and when.
    audio: UnsafeCell<Audio<P>>,
}

/// The state used while processing.
struct Audio<P> {
    plugin: P,
    sample_rate: f64,
    buffer: AudioBuffer<f32>,
    sidechain: AudioBuffer<f32>,
    events: EventBuffer,
    /// Parameter values as the plugin last saw them.
    watcher: ParamWatcher,
    /// Parameter values as the host last knew them, in index order.
    known_to_host: Vec<f64>,
}

fn current_values(params: &dyn ParamSet) -> Vec<f64> {
    let mut values = Vec::with_capacity(params.len());
    params.visit(&mut |param| values.push(param.plain()));
    values
}

/// The instance behind a pointer the host passed back.
///
/// # Safety
///
/// `plugin` must be null or a pointer returned by `factory_create::<P>` that
/// has not been destroyed.
unsafe fn instance<'a, P: Plugin>(plugin: *const clap_plugin) -> Option<&'a Instance<P>> {
    if plugin.is_null() {
        return None;
    }
    // SAFETY: per this function's contract `plugin` is live, and its
    // `plugin_data` was set by `factory_create` to the instance that owns it.
    unsafe { (*plugin).plugin_data.cast::<Instance<P>>().as_ref() }
}

impl<P: Plugin> Instance<P> {
    fn index_of(&self, id: clap_id) -> Option<usize> {
        self.ids.iter().position(|known| known.0 == id)
    }

    fn param(&self, id: clap_id) -> Option<&dyn Param> {
        self.index_of(id)?;
        self.params.find_param(ParamId(id))
    }
}

unsafe extern "C" fn plugin_init(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_destroy<P: Plugin>(plugin: *const clap_plugin) {
    guard((), || {
        if plugin.is_null() {
            return;
        }
        // SAFETY: the host destroys each plugin once, and `plugin_data` is
        // the pointer `factory_create` got from `Box::into_raw`.
        drop(unsafe { Box::from_raw((*plugin).plugin_data.cast::<Instance<P>>()) });
    });
}

unsafe extern "C" fn plugin_activate<P: Plugin>(
    plugin: *const clap_plugin,
    sample_rate: f64,
    _min_frames: u32,
    max_frames: u32,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(instance) = (unsafe { instance::<P>(plugin) }) else {
            return false;
        };
        // SAFETY: `activate` runs on the main thread while the plugin is not
        // processing, so no other thread is using the audio state.
        let audio = unsafe { &mut *instance.audio.get() };
        let max_frames = max_frames.max(1) as usize;

        audio.sample_rate = sample_rate;
        audio.buffer = AudioBuffer::new(P::LAYOUT.outputs, max_frames);
        audio.sidechain = AudioBuffer::new(P::LAYOUT.sidechain, max_frames);
        audio.events = EventBuffer::with_capacity(EVENTS_PER_BLOCK);
        audio.plugin.prepare(&ProcessSpec {
            sample_rate,
            max_block_size: max_frames,
            num_channels: P::LAYOUT.outputs,
        });
        let latency = u32::try_from(audio.plugin.latency_samples()).unwrap_or(u32::MAX);
        instance.latency.store(latency, Ordering::Relaxed);
        true
    })
}

unsafe extern "C" fn plugin_deactivate<P: Plugin>(plugin: *const clap_plugin) {
    guard((), || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(instance) = (unsafe { instance::<P>(plugin) }) else {
            return;
        };
        // SAFETY: `deactivate` runs on the main thread after processing has
        // stopped, so no other thread is using the audio state.
        let audio = unsafe { &mut *instance.audio.get() };
        audio.plugin.reset();
    });
}

unsafe extern "C" fn plugin_start_processing(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_stop_processing(_plugin: *const clap_plugin) {}

unsafe extern "C" fn plugin_on_main_thread(_plugin: *const clap_plugin) {}

unsafe extern "C" fn plugin_reset<P: Plugin>(plugin: *const clap_plugin) {
    guard((), || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(instance) = (unsafe { instance::<P>(plugin) }) else {
            return;
        };
        // SAFETY: `reset` runs on the audio thread, which is the only thread
        // using the audio state while the plugin is active.
        let audio = unsafe { &mut *instance.audio.get() };
        audio.plugin.reset();
    });
}

unsafe extern "C" fn plugin_get_extension<P: Plugin>(
    _plugin: *const clap_plugin,
    id: *const c_char,
) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: the host passes a valid NUL-terminated string.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_PARAMS {
        ptr::from_ref(Vtables::<P>::PARAMS).cast()
    } else if id == CLAP_EXT_STATE {
        ptr::from_ref(Vtables::<P>::STATE).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        ptr::from_ref(Vtables::<P>::AUDIO_PORTS).cast()
    } else if id == CLAP_EXT_NOTE_PORTS && takes_notes::<P>() {
        ptr::from_ref(Vtables::<P>::NOTE_PORTS).cast()
    } else if id == CLAP_EXT_LATENCY {
        ptr::from_ref(Vtables::<P>::LATENCY).cast()
    } else {
        ptr::null()
    }
}

// --- Processing ---------------------------------------------------------

unsafe extern "C" fn plugin_process<P: Plugin>(
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    guard(CLAP_PROCESS_ERROR, || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(instance) = (unsafe { instance::<P>(plugin) }) else {
            return CLAP_PROCESS_ERROR;
        };
        // SAFETY: the host passes a `clap_process` that is valid for the
        // duration of this call, or null.
        let Some(process) = (unsafe { process.as_ref() }) else {
            return CLAP_PROCESS_ERROR;
        };
        // SAFETY: `process` runs on the audio thread, which is the only
        // thread using the audio state while the plugin is active.
        let audio = unsafe { &mut *instance.audio.get() };

        let frames = process.frames_count as usize;
        if frames > audio.buffer.capacity() {
            // More than the host promised when activating.
            return CLAP_PROCESS_ERROR;
        }
        audio.buffer.set_num_frames(frames);
        audio.buffer.clear();

        // SAFETY: the pointers in `process` are valid for this call.
        unsafe {
            instance.gather_events(audio, process.in_events, process.out_events);
            let inputs = ports(process.audio_inputs, process.audio_inputs_count);
            if P::LAYOUT.inputs > 0 {
                if let Some(main) = inputs.first() {
                    read_port(main, &mut audio.buffer, P::LAYOUT.inputs);
                }
            }
            if P::LAYOUT.sidechain > 0 {
                audio.sidechain.set_num_frames(frames);
                audio.sidechain.clear();
                let index = usize::from(P::LAYOUT.inputs > 0);
                if let Some(port) = inputs.get(index) {
                    read_port(port, &mut audio.sidechain, P::LAYOUT.sidechain);
                }
            }
        }

        // SAFETY: the transport pointer is valid for this call, or null.
        let transport =
            unsafe { process.transport.as_ref() }.map_or(Transport::STOPPED, convert_transport);
        let mut context = ProcessContext::new(&audio.events, &transport);
        if P::LAYOUT.sidechain > 0 {
            context = context.with_sidechain(&audio.sidechain);
        }
        audio.plugin.process(&mut audio.buffer, &context);

        // SAFETY: the output pointers in `process` are valid for this call.
        // Everything was read from the inputs before this point, so writing
        // is sound even when the host processes in place.
        unsafe {
            let outputs = ports(process.audio_outputs, process.audio_outputs_count);
            for (index, port) in outputs.iter().enumerate() {
                write_port(port, (index == 0).then_some(&audio.buffer), frames);
            }
        }
        CLAP_PROCESS_CONTINUE
    })
}

/// The host's audio ports as a slice.
///
/// # Safety
///
/// `first` must point to `count` valid ports, or be null.
unsafe fn ports<'a>(first: *const clap_audio_buffer, count: u32) -> &'a [clap_audio_buffer] {
    if first.is_null() || count == 0 {
        return &[];
    }
    // SAFETY: guaranteed by this function's contract.
    unsafe { std::slice::from_raw_parts(first, count as usize) }
}

/// Copies up to `channels` channels of a host port into `buffer`.
///
/// # Safety
///
/// `port` must describe valid 32-bit channel pointers, each with at least
/// `buffer.num_frames()` frames, or have a null `data32`.
unsafe fn read_port(port: &clap_audio_buffer, buffer: &mut AudioBuffer<f32>, channels: usize) {
    if port.data32.is_null() {
        return;
    }
    let frames = buffer.num_frames();
    let count = channels
        .min(port.channel_count as usize)
        .min(buffer.num_channels());
    for index in 0..count {
        // SAFETY: `index` is below the port's channel count.
        let channel = unsafe { *port.data32.add(index) };
        if channel.is_null() {
            continue;
        }
        // SAFETY: the host guarantees `frames` readable samples per channel.
        let samples = unsafe { std::slice::from_raw_parts(channel, frames) };
        buffer.channel_mut(index).copy_from_slice(samples);
    }
}

/// Fills a host port from `source`, or with silence where `source` has no
/// channel for it.
///
/// # Safety
///
/// `port` must describe valid 32-bit channel pointers, each with at least
/// `frames` writable frames, or have a null `data32`.
unsafe fn write_port(port: &clap_audio_buffer, source: Option<&AudioBuffer<f32>>, frames: usize) {
    if port.data32.is_null() {
        return;
    }
    for index in 0..port.channel_count as usize {
        // SAFETY: `index` is below the port's channel count.
        let channel = unsafe { *port.data32.add(index) };
        if channel.is_null() {
            continue;
        }
        // SAFETY: the host guarantees `frames` writable samples per channel,
        // and no reference to the host's input memory is alive here.
        let samples = unsafe { std::slice::from_raw_parts_mut(channel, frames) };
        match source.filter(|buffer| index < buffer.num_channels()) {
            Some(buffer) => samples.copy_from_slice(buffer.channel(index)),
            None => samples.fill(0.0),
        }
    }
}

fn convert_transport(clap: &clap_event_transport) -> Transport {
    let has = |flag: u32| clap.flags & flag != 0;
    let beats = |fixed: i64| fixed as f64 / CLAP_BEATTIME_FACTOR as f64;
    let timeline = has(CLAP_TRANSPORT_HAS_BEATS_TIMELINE);
    Transport {
        playing: has(CLAP_TRANSPORT_IS_PLAYING),
        recording: has(CLAP_TRANSPORT_IS_RECORDING),
        tempo_bpm: has(CLAP_TRANSPORT_HAS_TEMPO).then_some(clap.tempo),
        time_signature: has(CLAP_TRANSPORT_HAS_TIME_SIGNATURE)
            .then_some((clap.tsig_num, clap.tsig_denom)),
        position_samples: None,
        position_beats: timeline.then(|| beats(clap.song_pos_beats)),
        bar_start_beats: timeline.then(|| beats(clap.bar_start)),
        loop_beats: (timeline && has(CLAP_TRANSPORT_IS_LOOP_ACTIVE))
            .then(|| (beats(clap.loop_start_beats), beats(clap.loop_end_beats))),
    }
}

fn note_ref(note_id: i32, channel: i16, key: i16) -> Option<NoteRef> {
    let id = u32::try_from(note_id).ok();
    let channel = u8::try_from(channel).unwrap_or(0) & 0x0F;
    match (u8::try_from(key), id) {
        (Ok(key), Some(id)) => Some(NoteRef::with_id(channel, key & 0x7F, id)),
        (Ok(key), None) => Some(NoteRef::new(channel, key & 0x7F)),
        // The key is a wildcard but the id is not: the id alone decides.
        (Err(_), Some(id)) => Some(NoteRef::with_id(channel, 0, id)),
        (Err(_), None) => None,
    }
}

fn expression(id: clap_note_expression) -> Option<Expression> {
    Some(match id {
        CLAP_NOTE_EXPRESSION_VOLUME => Expression::Volume,
        CLAP_NOTE_EXPRESSION_PAN => Expression::Pan,
        CLAP_NOTE_EXPRESSION_TUNING => Expression::Tuning,
        CLAP_NOTE_EXPRESSION_VIBRATO => Expression::Vibrato,
        CLAP_NOTE_EXPRESSION_BRIGHTNESS => Expression::Brightness,
        CLAP_NOTE_EXPRESSION_PRESSURE => Expression::Pressure,
        _ => return None,
    })
}

impl<P: Plugin> Instance<P> {
    /// Fills `audio.events` for the coming block and keeps host and plugin
    /// in agreement about parameter values.
    ///
    /// # Safety
    ///
    /// `input` and `output` must be valid event lists for the duration of
    /// the call, or null.
    unsafe fn gather_events(
        &self,
        audio: &mut Audio<P>,
        input: *const clap_input_events,
        output: *const clap_output_events,
    ) {
        let params: &dyn ParamSet = &*self.params;
        audio.events.clear();

        // Changes the plugin has not seen yet go first, on frame 0.
        audio.watcher.emit_changes(params, &mut audio.events);

        // Changes the host has not seen yet are reported to it, unless the
        // host made them itself by loading state.
        if !self.host_knows_values.swap(true, Ordering::Relaxed) {
            self.remember_for_host(audio);
        }
        let mut known = audio.known_to_host.iter_mut();
        params.visit(&mut |param| {
            let Some(known) = known.next() else {
                return;
            };
            let plain = param.plain();
            if plain != *known {
                *known = plain;
                // SAFETY: `output` is valid for this call, per our contract.
                unsafe { send_param_value(output, param.param_id().0, to_clap(param, plain)) };
            }
        });

        // Then what the host sends.
        // SAFETY: `input` is valid for this call, per our contract.
        unsafe { for_each_event(input, |header| self.convert(audio, header)) };

        apply_param_events(params, &audio.events);
        audio.watcher.remember(params);
    }

    fn remember_for_host(&self, audio: &mut Audio<P>) {
        let mut known = audio.known_to_host.iter_mut();
        self.params.visit(&mut |param| {
            if let Some(known) = known.next() {
                *known = param.plain();
            }
        });
    }

    /// Turns one host event into ours and queues it.
    ///
    /// # Safety
    ///
    /// `header` must point to a valid CLAP event.
    unsafe fn convert(&self, audio: &mut Audio<P>, header: *const clap_event_header) {
        // SAFETY: guaranteed by this function's contract.
        let head = unsafe { &*header };
        if head.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return;
        }
        /// Reads the event as its concrete type, if it is large enough.
        ///
        /// # Safety
        ///
        /// `header` must be the start of a valid event whose type is `T`.
        unsafe fn read<'a, T>(header: *const clap_event_header) -> Option<&'a T> {
            // SAFETY: the caller guarantees the event's type; its size field
            // is checked before the larger struct is read.
            unsafe { ((*header).size as usize >= size_of::<T>()).then(|| &*header.cast::<T>()) }
        }

        let kind = match head.type_ {
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                // SAFETY: these three types all carry a `clap_event_note`.
                let Some(event) = (unsafe { read::<clap_event_note>(header) }) else {
                    return;
                };
                let Some(note) = note_ref(event.note_id, event.channel, event.key) else {
                    return;
                };
                let velocity = event.velocity as f32;
                match head.type_ {
                    CLAP_EVENT_NOTE_ON => EventKind::NoteOn { note, velocity },
                    CLAP_EVENT_NOTE_OFF => EventKind::NoteOff { note, velocity },
                    _ => EventKind::NoteChoke { note },
                }
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                // SAFETY: this type carries a `clap_event_note_expression`.
                let Some(event) = (unsafe { read::<clap_event_note_expression>(header) }) else {
                    return;
                };
                let (Some(note), Some(expression)) = (
                    note_ref(event.note_id, event.channel, event.key),
                    expression(event.expression_id),
                ) else {
                    return;
                };
                EventKind::NoteExpression {
                    note,
                    expression,
                    value: event.value,
                }
            }
            CLAP_EVENT_PARAM_VALUE => {
                // SAFETY: this type carries a `clap_event_param_value`.
                let Some(event) = (unsafe { read::<clap_event_param_value>(header) }) else {
                    return;
                };
                // Per-note automation is not supported yet.
                if event.note_id >= 0 || event.key >= 0 {
                    return;
                }
                let (Some(index), Some(param)) =
                    (self.index_of(event.param_id), self.param(event.param_id))
                else {
                    return;
                };
                let value = from_clap(param, event.value);
                audio.known_to_host[index] = value;
                EventKind::ParamValue {
                    param: ParamId(event.param_id),
                    value,
                }
            }
            CLAP_EVENT_PARAM_MOD => {
                // SAFETY: this type carries a `clap_event_param_mod`.
                let Some(event) = (unsafe { read::<clap_event_param_mod>(header) }) else {
                    return;
                };
                let Some(param) = self.param(event.param_id) else {
                    return;
                };
                // The host gives the offset in its own scale. Convert it to
                // the parameter's unit around where the parameter is now.
                let plain = param.plain();
                let moved = from_clap(param, to_clap(param, plain) + event.amount);
                EventKind::ParamModulation {
                    param: ParamId(event.param_id),
                    amount: moved - plain,
                    note: note_ref(event.note_id, event.channel, event.key),
                }
            }
            CLAP_EVENT_MIDI => {
                // SAFETY: this type carries a `clap_event_midi`.
                let Some(event) = (unsafe { read::<clap_event_midi>(header) }) else {
                    return;
                };
                let Some(message) = MidiMessage::from_bytes(&event.data) else {
                    return;
                };
                Event::from_midi(head.time, message).kind
            }
            _ => return,
        };
        // A full buffer drops the event; allocating here is not an option.
        let _ = audio.events.push(Event {
            offset: head.time,
            kind,
        });
    }
}

/// Calls `handle` for every event in a host's input list.
///
/// # Safety
///
/// `list` must be a valid event list for the duration of the call, or null.
unsafe fn for_each_event(
    list: *const clap_input_events,
    mut handle: impl FnMut(*const clap_event_header),
) {
    // SAFETY: guaranteed by this function's contract.
    let Some(events) = (unsafe { list.as_ref() }) else {
        return;
    };
    let (Some(size), Some(get)) = (events.size, events.get) else {
        return;
    };
    // SAFETY: the host's own functions, called with the host's own list.
    for index in 0..unsafe { size(list) } {
        // SAFETY: as above; `index` is below the size just reported.
        let header = unsafe { get(list, index) };
        if !header.is_null() {
            handle(header);
        }
    }
}

/// Tells the host that a parameter changed without it.
///
/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn send_param_value(list: *const clap_output_events, param_id: clap_id, value: f64) {
    // SAFETY: guaranteed by this function's contract.
    let Some(push) = (unsafe { list.as_ref() }).and_then(|events| events.try_push) else {
        return;
    };
    let event = clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        },
        param_id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    };
    // The pointer is taken to the whole event and then viewed as its header,
    // which is its first field. A pointer taken to the header field alone
    // would only be allowed to reach the header, and the host reads the
    // rest of the event through it. Miri caught exactly that.
    let header = (&raw const event).cast::<clap_event_header>();
    // SAFETY: the host's own function with the host's own list; the event
    // is valid for the call and the host copies it.
    unsafe { push(list, header) };
}

// --- Parameters extension -----------------------------------------------

unsafe extern "C" fn params_count<P: Plugin>(plugin: *const clap_plugin) -> u32 {
    guard(0, || {
        // SAFETY: the host passes a plugin it created through our factory.
        unsafe { instance::<P>(plugin) }.map_or(0, |instance| instance.ids.len() as u32)
    })
}

unsafe extern "C" fn params_get_info<P: Plugin>(
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory,
        // and a `clap_param_info` it owns, or null.
        let (Some(instance), Some(info)) =
            (unsafe { instance::<P>(plugin) }, unsafe { info.as_mut() })
        else {
            return false;
        };
        let Some(param) = instance
            .ids
            .get(index as usize)
            .and_then(|&id| instance.params.find_param(id))
        else {
            return false;
        };

        let flags = param.info().flags();
        let stepped = step_count(param);
        let mut clap_flags = 0;
        if flags.automatable {
            clap_flags |= CLAP_PARAM_IS_AUTOMATABLE;
        }
        if flags.hidden {
            clap_flags |= CLAP_PARAM_IS_HIDDEN;
        }
        if flags.bypass {
            clap_flags |= CLAP_PARAM_IS_BYPASS;
        }
        match stepped {
            Some(_) => clap_flags |= CLAP_PARAM_IS_STEPPED,
            None => clap_flags |= CLAP_PARAM_IS_MODULATABLE,
        }

        info.id = param.param_id().0;
        info.flags = clap_flags;
        info.cookie = ptr::null_mut();
        copy_text(param.name(), &mut info.name);
        copy_text("", &mut info.module);
        info.min_value = 0.0;
        info.max_value = stepped.unwrap_or(1.0);
        info.default_value = to_clap(param, param.default_plain());
        true
    })
}

unsafe extern "C" fn params_get_value<P: Plugin>(
    plugin: *const clap_plugin,
    id: clap_id,
    out: *mut f64,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(param) = (unsafe { instance::<P>(plugin) }).and_then(|i| i.param(id)) else {
            return false;
        };
        if out.is_null() {
            return false;
        }
        // SAFETY: `out` is a non-null pointer to an `f64` the host owns.
        unsafe { *out = to_clap(param, param.plain()) };
        true
    })
}

unsafe extern "C" fn params_value_to_text<P: Plugin>(
    plugin: *const clap_plugin,
    id: clap_id,
    value: f64,
    out: *mut c_char,
    capacity: u32,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(param) = (unsafe { instance::<P>(plugin) }).and_then(|i| i.param(id)) else {
            return false;
        };
        if out.is_null() || capacity == 0 {
            return false;
        }
        let mut text = param.plain_to_text(from_clap(param, value));
        let unit = param.info().unit();
        if !unit.is_empty() {
            text.push(' ');
            text.push_str(unit);
        }
        // SAFETY: the host provides `capacity` writable characters at `out`.
        copy_text(&text, unsafe {
            std::slice::from_raw_parts_mut(out, capacity as usize)
        });
        true
    })
}

unsafe extern "C" fn params_text_to_value<P: Plugin>(
    plugin: *const clap_plugin,
    id: clap_id,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(param) = (unsafe { instance::<P>(plugin) }).and_then(|i| i.param(id)) else {
            return false;
        };
        if text.is_null() || out.is_null() {
            return false;
        }
        // SAFETY: the host passes a valid NUL-terminated string.
        let Ok(text) = unsafe { CStr::from_ptr(text) }.to_str() else {
            return false;
        };
        let Some(plain) = param.text_to_plain(text) else {
            return false;
        };
        // SAFETY: `out` is a non-null pointer to an `f64` the host owns.
        unsafe { *out = to_clap(param, plain) };
        true
    })
}

unsafe extern "C" fn params_flush<P: Plugin>(
    plugin: *const clap_plugin,
    input: *const clap_input_events,
    _output: *const clap_output_events,
) {
    guard((), || {
        // SAFETY: the host passes a plugin it created through our factory.
        let Some(instance) = (unsafe { instance::<P>(plugin) }) else {
            return;
        };
        // SAFETY: `flush` is called instead of `process`: on the audio
        // thread while active, on the main thread while not. Either way no
        // other thread is using the audio state.
        let audio = unsafe { &mut *instance.audio.get() };

        // Only parameter values matter here. They are stored straight into
        // the parameters; the plugin's watcher is left alone, so the next
        // block still tells the plugin about them as events.
        // SAFETY: `input` is valid for this call, or null.
        unsafe {
            for_each_event(input, |header| {
                let head = &*header;
                if head.space_id != CLAP_CORE_EVENT_SPACE_ID
                    || head.type_ != CLAP_EVENT_PARAM_VALUE
                    || (head.size as usize) < size_of::<clap_event_param_value>()
                {
                    return;
                }
                let event = &*header.cast::<clap_event_param_value>();
                let (Some(index), Some(param)) = (
                    instance.index_of(event.param_id),
                    instance.param(event.param_id),
                ) else {
                    return;
                };
                param.set_plain(from_clap(param, event.value));
                audio.known_to_host[index] = param.plain();
            });
        }
    });
}

// --- State extension ----------------------------------------------------

unsafe extern "C" fn state_save<P: Plugin>(
    plugin: *const clap_plugin,
    stream: *const clap_ostream,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory,
        // and a stream that is valid for this call, or null.
        let (Some(instance), Some(write)) = (
            unsafe { instance::<P>(plugin) },
            unsafe { stream.as_ref() }.and_then(|stream| stream.write),
        ) else {
            return false;
        };
        let text = save_state::<P>(&instance.params);
        let mut rest = text.as_bytes();
        while !rest.is_empty() {
            // SAFETY: the host's own function with the host's own stream,
            // and a buffer of exactly the length passed.
            let written = unsafe { write(stream, rest.as_ptr().cast(), rest.len() as u64) };
            match usize::try_from(written) {
                Ok(count) if count > 0 && count <= rest.len() => rest = &rest[count..],
                _ => return false,
            }
        }
        true
    })
}

unsafe extern "C" fn state_load<P: Plugin>(
    plugin: *const clap_plugin,
    stream: *const clap_istream,
) -> bool {
    guard(false, || {
        // SAFETY: the host passes a plugin it created through our factory,
        // and a stream that is valid for this call, or null.
        let (Some(instance), Some(read)) = (
            unsafe { instance::<P>(plugin) },
            unsafe { stream.as_ref() }.and_then(|stream| stream.read),
        ) else {
            return false;
        };
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            // SAFETY: the host's own function with the host's own stream,
            // and a buffer of exactly the length passed.
            let count = unsafe { read(stream, chunk.as_mut_ptr().cast(), chunk.len() as u64) };
            match usize::try_from(count) {
                Ok(0) => break,
                Ok(count) if count <= chunk.len() => bytes.extend_from_slice(&chunk[..count]),
                _ => return false,
            }
            if bytes.len() > MAX_STATE_BYTES {
                return false;
            }
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return false;
        };
        if load_state::<P>(&instance.params, text).is_err() {
            return false;
        }
        // Every value may have changed without an event, so the host is
        // asked to read them all again. It then knows them, and the audio
        // thread must not report them back as if they were news.
        instance.host_knows_values.store(false, Ordering::Relaxed);
        // SAFETY: `load` runs on the main thread, where the host's parameter
        // extension may be called, and the host outlives the plugin.
        unsafe { rescan_values(instance.host) };
        true
    })
}

/// Asks the host to read every parameter value again.
///
/// # Safety
///
/// `host` must be the live host that created the plugin, or null, and this
/// must be called on the main thread.
unsafe fn rescan_values(host: *const clap_host) {
    // SAFETY: guaranteed by this function's contract.
    let Some(get_extension) = (unsafe { host.as_ref() }).and_then(|host| host.get_extension) else {
        return;
    };
    // SAFETY: the host's own function with the host's own pointer. A
    // non-null result is the host's `clap_host_params`, valid while the
    // host is.
    let params = unsafe {
        get_extension(host, CLAP_EXT_PARAMS.as_ptr())
            .cast::<clap_host_params>()
            .as_ref()
    };
    if let Some(rescan) = params.and_then(|params| params.rescan) {
        // SAFETY: as above, and we are on the main thread.
        unsafe { rescan(host, CLAP_PARAM_RESCAN_VALUES) };
    }
}

// --- Ports and latency --------------------------------------------------

fn takes_notes<P: Plugin>() -> bool {
    matches!(
        P::INFO.kind,
        PluginKind::Instrument | PluginKind::NoteEffect
    )
}

/// The channel counts of the ports in one direction, main port first.
fn port_channels<P: Plugin>(is_input: bool) -> ([usize; 2], usize) {
    let layout = P::LAYOUT;
    let candidates = if is_input {
        [layout.inputs, layout.sidechain]
    } else {
        [layout.outputs, 0]
    };
    let mut present = [0; 2];
    let mut count = 0;
    for channels in candidates {
        if channels > 0 {
            present[count] = channels;
            count += 1;
        }
    }
    (present, count)
}

unsafe extern "C" fn audio_ports_count<P: Plugin>(
    _plugin: *const clap_plugin,
    is_input: bool,
) -> u32 {
    port_channels::<P>(is_input).1 as u32
}

unsafe extern "C" fn audio_ports_get<P: Plugin>(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    guard(false, || {
        let (channels, count) = port_channels::<P>(is_input);
        let index = index as usize;
        // SAFETY: `info` is a struct the host owns, or null.
        let Some(info) = (unsafe { info.as_mut() }).filter(|_| index < count) else {
            return false;
        };
        let channels = channels[index];
        // The main port is the first one, except that an instrument's only
        // input port is its sidechain.
        let is_main = index == 0 && !(is_input && P::LAYOUT.inputs == 0);

        info.id = index as clap_id;
        copy_text(if is_main { "Main" } else { "Sidechain" }, &mut info.name);
        info.flags = if is_main { CLAP_AUDIO_PORT_IS_MAIN } else { 0 };
        info.channel_count = channels as u32;
        info.port_type = match channels {
            1 => CLAP_PORT_MONO.as_ptr(),
            2 => CLAP_PORT_STEREO.as_ptr(),
            _ => ptr::null(),
        };
        // The main input and output can share memory when they are the
        // same shape.
        info.in_place_pair = if is_main && P::LAYOUT.inputs == P::LAYOUT.outputs {
            0
        } else {
            CLAP_INVALID_ID
        };
        true
    })
}

unsafe extern "C" fn note_ports_count<P: Plugin>(
    _plugin: *const clap_plugin,
    is_input: bool,
) -> u32 {
    u32::from(is_input && takes_notes::<P>())
}

unsafe extern "C" fn note_ports_get<P: Plugin>(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    guard(false, || {
        // SAFETY: `info` is a struct the host owns, or null.
        let Some(info) = (unsafe { info.as_mut() }) else {
            return false;
        };
        if index != 0 || !is_input || !takes_notes::<P>() {
            return false;
        }
        info.id = 0;
        info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
        info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
        copy_text("Notes", &mut info.name);
        true
    })
}

unsafe extern "C" fn latency_get<P: Plugin>(plugin: *const clap_plugin) -> u32 {
    guard(0, || {
        // SAFETY: the host passes a plugin it created through our factory.
        unsafe { instance::<P>(plugin) }
            .map_or(0, |instance| instance.latency.load(Ordering::Relaxed))
    })
}
