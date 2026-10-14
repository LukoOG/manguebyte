//! Hosts a small probe plugin at the edges of what the CLAP host promises.
//!
//! Each test states something the host's documentation, the CLAP
//! specification or the framework's real-time rules require, in a corner
//! where it once did otherwise: a transport at the end of its range, a
//! callback asked for from inside a callback, a main thread that has ended,
//! a plugin that reports values outside the range it declared or for
//! parameters it never listed, a sidechain shorter than the block, events
//! in a block the plugin could not be called for, and a note port that
//! reads MIDI only as MPE.
//!
//! The plugin under test is written in this file against the raw C
//! interface and reached through `ClapLibrary::from_entry`, so the tests
//! need no shared library and say exactly what the plugin does. Its code
//! runs under the same heap watcher as the host's and does not allocate
//! while processing, so a count that is not zero is the host's doing.
//!
//! Every `unsafe` block in this file is one side or the other of the C
//! interface, with pointers that are valid for the call, so the blocks are
//! not commented one by one.
#![cfg(feature = "clap")]
#![allow(clippy::undocumented_unsafe_blocks)]

mod common;

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::event::{Event, EventBuffer, EventKind, NoteRef};
use manguebyte_dsp::{ProcessContext, ProcessSpec, Processor, Transport};
use manguebyte_host::clap::clap_sys::entry::clap_plugin_entry;
use manguebyte_host::clap::clap_sys::events::*;
use manguebyte_host::clap::clap_sys::ext::audio_ports::*;
use manguebyte_host::clap::clap_sys::ext::note_ports::*;
use manguebyte_host::clap::clap_sys::ext::params::*;
use manguebyte_host::clap::clap_sys::ext::thread_check::*;
use manguebyte_host::clap::clap_sys::factory::plugin_factory::*;
use manguebyte_host::clap::clap_sys::fixedpoint::CLAP_SECTIME_FACTOR;
use manguebyte_host::clap::clap_sys::host::clap_host;
use manguebyte_host::clap::clap_sys::id::CLAP_INVALID_ID;
use manguebyte_host::clap::clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use manguebyte_host::clap::clap_sys::process::*;
use manguebyte_host::clap::clap_sys::version::CLAP_VERSION;
use manguebyte_host::clap::{
    ClapLibrary, ClapPlugin, EVENTS_PER_BLOCK, HostEvent, LogSeverity, OutputKind, ProcessFailure,
    ProcessStatus,
};
use manguebyte_midi::MidiMessage;

use common::heap_calls_during;

// --- The plugin --------------------------------------------------------------

const ID: &str = "edges.probe";

/// The plugin's one parameter. It declares the range 0 to 2.
const KNOB: ParamId = ParamId(1);
const KNOB_MAX: f64 = 2.0;

/// A parameter the plugin never lists, and reports on all the same when
/// told to.
const GHOST: ParamId = ParamId(99);

/// What the plugin is told to do, and what it saw.
struct Probe {
    /// How many more times the main-thread callback asks for another one.
    rearm: AtomicU32,
    /// A value the plugin reports in every `process` and every `flush`, as
    /// the bits of an `f64`. Not a number: nothing is reported.
    report: AtomicU64,
    /// The parameter that report is about.
    report_id: AtomicU32,
    /// Whether the report comes with a gesture around it.
    report_gesture: AtomicBool,
    /// What `get_value` answers for the knob, as the bits of an `f64`.
    value: AtomicU64,
    /// The dialects of the plugin's note input.
    dialects: AtomicU32,
    /// Whether the plugin is an effect with a stereo main input and a mono
    /// sidechain. Otherwise it has no input at all.
    effect: AtomicBool,
    /// What `start_processing` answers.
    start: AtomicBool,

    callbacks: AtomicU32,
    /// Calls of `process`.
    pieces: AtomicU32,
    notes_seen: AtomicU32,
    midi_seen: AtomicU32,
    /// The status byte of the last MIDI event received.
    midi_status: AtomicU32,
    values_seen: AtomicU32,
    /// The last value received for the knob, as the bits of an `f64`.
    knob: AtomicU64,
    /// How often the plugin was asked for the text of a value, and the last
    /// value it was asked about, as the bits of an `f64`.
    texts_asked: AtomicU32,
    text_of: AtomicU64,
    /// The position in seconds of the last piece, as CLAP's fixed point.
    seconds: AtomicI64,
    /// What the host's thread check said from inside the last `process`.
    main_in_process: AtomicBool,
    audio_in_process: AtomicBool,
    destroyed: AtomicU32,
    destroyed_on: Mutex<Option<ThreadId>>,
    host: AtomicPtr<clap_host>,
}

impl Probe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rearm: AtomicU32::new(0),
            report: AtomicU64::new(f64::NAN.to_bits()),
            report_id: AtomicU32::new(KNOB.0),
            report_gesture: AtomicBool::new(false),
            value: AtomicU64::new(1.0f64.to_bits()),
            dialects: AtomicU32::new(CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI),
            effect: AtomicBool::new(false),
            start: AtomicBool::new(true),
            callbacks: AtomicU32::new(0),
            pieces: AtomicU32::new(0),
            notes_seen: AtomicU32::new(0),
            midi_seen: AtomicU32::new(0),
            midi_status: AtomicU32::new(0),
            values_seen: AtomicU32::new(0),
            knob: AtomicU64::new(f64::NAN.to_bits()),
            texts_asked: AtomicU32::new(0),
            text_of: AtomicU64::new(f64::NAN.to_bits()),
            seconds: AtomicI64::new(-1),
            main_in_process: AtomicBool::new(false),
            audio_in_process: AtomicBool::new(false),
            destroyed: AtomicU32::new(0),
            destroyed_on: Mutex::new(None),
            host: AtomicPtr::new(ptr::null_mut()),
        })
    }

    /// The host as the plugin holds it, for a test to call the way the
    /// plugin would.
    fn host(&self) -> *const clap_host {
        self.host.load(Relaxed)
    }

    /// The last value the plugin received for its knob.
    fn knob(&self) -> f64 {
        f64::from_bits(self.knob.load(Relaxed))
    }
}

thread_local! {
    /// The probe the next library loaded on this thread belongs to. An entry
    /// point's functions take no context.
    static NEXT: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
}

#[repr(C)]
struct Factory {
    table: clap_plugin_factory,
    probe: Arc<Probe>,
    descriptor: *const clap_plugin_descriptor,
}

#[repr(C)]
struct Instance {
    table: clap_plugin,
    host: *const clap_host,
    probe: Arc<Probe>,
}

unsafe fn instance<'a>(plugin: *const clap_plugin) -> &'a Instance {
    unsafe { &*(*plugin).plugin_data.cast::<Instance>() }
}

/// The host's thread check, as a plugin gets at it.
unsafe fn thread_check<'a>(host: *const clap_host) -> &'a clap_host_thread_check {
    unsafe {
        &*(*host).get_extension.unwrap()(host, CLAP_EXT_THREAD_CHECK.as_ptr())
            .cast::<clap_host_thread_check>()
    }
}

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } != CLAP_PLUGIN_FACTORY_ID {
        return ptr::null();
    }
    let probe = NEXT.with(|next| next.borrow().clone().unwrap());
    let descriptor = Box::into_raw(Box::new(clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: c"edges.probe".as_ptr(),
        name: c"Probe".as_ptr(),
        vendor: ptr::null(),
        url: ptr::null(),
        manual_url: ptr::null(),
        support_url: ptr::null(),
        version: ptr::null(),
        description: ptr::null(),
        features: ptr::null(),
    }));
    Box::into_raw(Box::new(Factory {
        table: clap_plugin_factory {
            get_plugin_count: Some(factory_count),
            get_plugin_descriptor: Some(factory_descriptor),
            create_plugin: Some(factory_create),
        },
        probe,
        descriptor,
    }))
    .cast()
}

unsafe extern "C" fn factory_count(_factory: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn factory_descriptor(
    factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        unsafe { (*factory.cast::<Factory>()).descriptor }
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn factory_create(
    factory: *const clap_plugin_factory,
    host: *const clap_host,
    _id: *const c_char,
) -> *const clap_plugin {
    let factory = unsafe { &*factory.cast::<Factory>() };
    factory.probe.host.store(host.cast_mut(), Relaxed);
    let instance = Box::into_raw(Box::new(Instance {
        table: clap_plugin {
            desc: factory.descriptor,
            plugin_data: ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        host,
        probe: Arc::clone(&factory.probe),
    }));
    unsafe {
        (*instance).table.plugin_data = instance.cast();
        &raw const (*instance).table
    }
}

static ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};

unsafe extern "C" fn init(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    let instance = unsafe { Box::from_raw((*plugin).plugin_data.cast::<Instance>()) };
    instance.probe.destroyed.fetch_add(1, Relaxed);
    *instance.probe.destroyed_on.lock().unwrap() = Some(std::thread::current().id());
}

unsafe extern "C" fn activate(
    _plugin: *const clap_plugin,
    _rate: f64,
    _min: u32,
    _max: u32,
) -> bool {
    true
}

unsafe extern "C" fn deactivate(_plugin: *const clap_plugin) {}

unsafe extern "C" fn start_processing(plugin: *const clap_plugin) -> bool {
    unsafe { instance(plugin) }.probe.start.load(Relaxed)
}

unsafe extern "C" fn stop_processing(_plugin: *const clap_plugin) {}

unsafe extern "C" fn reset(_plugin: *const clap_plugin) {}

fn header<T>(type_: u16) -> clap_event_header {
    clap_event_header {
        size: size_of::<T>() as u32,
        time: 0,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// Counts what the host hands over. Shared by `process` and `flush`. Does
/// not allocate.
unsafe fn take_events(probe: &Probe, input: *const clap_input_events) {
    let count = unsafe { (*input).size.unwrap()(input) };
    for index in 0..count {
        let event = unsafe { (*input).get.unwrap()(input, index) };
        match unsafe { (*event).type_ } {
            CLAP_EVENT_NOTE_ON => {
                probe.notes_seen.fetch_add(1, Relaxed);
            }
            CLAP_EVENT_MIDI => {
                let midi = unsafe { &*event.cast::<clap_event_midi>() };
                probe.midi_seen.fetch_add(1, Relaxed);
                probe.midi_status.store(u32::from(midi.data[0]), Relaxed);
            }
            CLAP_EVENT_PARAM_VALUE => {
                let value = unsafe { &*event.cast::<clap_event_param_value>() };
                probe.values_seen.fetch_add(1, Relaxed);
                if value.param_id == KNOB.0 {
                    probe.knob.store(value.value.to_bits(), Relaxed);
                }
            }
            _ => {}
        }
    }
}

/// Says what the probe was told to say about a parameter. Shared by
/// `process` and `flush`. Does not allocate.
unsafe fn report(probe: &Probe, output: *const clap_output_events) {
    let value = f64::from_bits(probe.report.load(Relaxed));
    if value.is_nan() {
        return;
    }
    let param_id = probe.report_id.load(Relaxed);
    let push = unsafe { (*output).try_push.unwrap() };
    let gesture = |type_| clap_event_param_gesture {
        header: header::<clap_event_param_gesture>(type_),
        param_id,
    };
    let with_gesture = probe.report_gesture.load(Relaxed);
    if with_gesture {
        let begin = gesture(CLAP_EVENT_PARAM_GESTURE_BEGIN);
        unsafe { push(output, (&raw const begin).cast()) };
    }
    let event = clap_event_param_value {
        header: header::<clap_event_param_value>(CLAP_EVENT_PARAM_VALUE),
        param_id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    };
    unsafe { push(output, (&raw const event).cast()) };
    if with_gesture {
        let end = gesture(CLAP_EVENT_PARAM_GESTURE_END);
        unsafe { push(output, (&raw const end).cast()) };
    }
}

/// The sound: the sidechain's first channel, sample for sample, if the
/// plugin has one, and otherwise a constant 1.
unsafe extern "C" fn process(plugin: *const clap_plugin, process: *const clap_process) -> i32 {
    let this = unsafe { instance(plugin) };
    let probe = &*this.probe;
    let process = unsafe { &*process };
    let frames = process.frames_count as usize;
    probe.pieces.fetch_add(1, Relaxed);
    if let Some(transport) = unsafe { process.transport.as_ref() } {
        probe.seconds.store(transport.song_pos_seconds, Relaxed);
    }
    let check = unsafe { thread_check(this.host) };
    probe
        .main_in_process
        .store(unsafe { check.is_main_thread.unwrap()(this.host) }, Relaxed);
    probe.audio_in_process.store(
        unsafe { check.is_audio_thread.unwrap()(this.host) },
        Relaxed,
    );

    unsafe {
        take_events(probe, process.in_events);
        report(probe, process.out_events);
    }

    let side: Option<&[f32]> = (process.audio_inputs_count > 1).then(|| unsafe {
        let port = &*process.audio_inputs.add(1);
        std::slice::from_raw_parts(*port.data32, frames)
    });
    if process.audio_outputs_count > 0 {
        let output = unsafe { &*process.audio_outputs };
        for channel in 0..output.channel_count as usize {
            let target =
                unsafe { std::slice::from_raw_parts_mut(*output.data32.add(channel), frames) };
            match side {
                Some(side) => target.copy_from_slice(side),
                None => target.fill(1.0),
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn get_extension(
    _plugin: *const clap_plugin,
    id: *const c_char,
) -> *const c_void {
    static PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(params_count),
        get_info: Some(params_get_info),
        get_value: Some(params_get_value),
        value_to_text: Some(params_value_to_text),
        text_to_value: Some(params_text_to_value),
        flush: Some(params_flush),
    };
    static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
        count: Some(audio_ports_count),
        get: Some(audio_ports_get),
    };
    static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
        count: Some(note_ports_count),
        get: Some(note_ports_get),
    };
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_PARAMS {
        ptr::from_ref(&PARAMS).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        ptr::from_ref(&AUDIO_PORTS).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        ptr::from_ref(&NOTE_PORTS).cast()
    } else {
        ptr::null()
    }
}

/// Counts the call and, while told to, asks for another one: what a plugin
/// does that spreads work over several turns of the host's main loop.
unsafe extern "C" fn on_main_thread(plugin: *const clap_plugin) {
    let this = unsafe { instance(plugin) };
    this.probe.callbacks.fetch_add(1, Relaxed);
    // Only the main thread touches the count, so a load and a store do.
    let left = this.probe.rearm.load(Relaxed);
    if left > 0 {
        this.probe.rearm.store(left - 1, Relaxed);
        unsafe { (*this.host).request_callback.unwrap()(this.host) };
    }
}

unsafe extern "C" fn params_count(_plugin: *const clap_plugin) -> u32 {
    1
}

unsafe extern "C" fn params_get_info(
    _plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    if index != 0 {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = KNOB.0;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE;
    for (slot, byte) in info.name.iter_mut().zip(b"Knob\0") {
        *slot = *byte as c_char;
    }
    (info.min_value, info.max_value, info.default_value) = (0.0, KNOB_MAX, 1.0);
    true
}

unsafe extern "C" fn params_get_value(plugin: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if id != KNOB.0 {
        return false;
    }
    unsafe { *out = f64::from_bits(instance(plugin).probe.value.load(Relaxed)) };
    true
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const clap_plugin,
    _id: u32,
    value: f64,
    out: *mut c_char,
    capacity: u32,
) -> bool {
    let probe = &unsafe { instance(plugin) }.probe;
    probe.texts_asked.fetch_add(1, Relaxed);
    probe.text_of.store(value.to_bits(), Relaxed);
    if capacity > 0 {
        unsafe { *out = 0 };
    }
    true
}

unsafe extern "C" fn params_text_to_value(
    _plugin: *const clap_plugin,
    _id: u32,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    match unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .parse::<f64>()
    {
        Ok(value) => {
            unsafe { *out = value };
            true
        }
        Err(_) => false,
    }
}

unsafe extern "C" fn params_flush(
    plugin: *const clap_plugin,
    input: *const clap_input_events,
    output: *const clap_output_events,
) {
    let probe = &*unsafe { instance(plugin) }.probe;
    unsafe {
        take_events(probe, input);
        report(probe, output);
    }
}

unsafe extern "C" fn audio_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    match (
        unsafe { instance(plugin) }.probe.effect.load(Relaxed),
        is_input,
    ) {
        (true, true) => 2,
        (false, true) => 0,
        (_, false) => 1,
    }
}

unsafe extern "C" fn audio_ports_get(
    _plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index > 1 || (!is_input && index > 0) {
        return false;
    }
    let info = unsafe { &mut *info };
    let main = index == 0;
    info.id = if is_input { 10 } else { 20 } + index;
    info.flags = if main { CLAP_AUDIO_PORT_IS_MAIN } else { 0 };
    info.channel_count = if main { 2 } else { 1 };
    info.port_type = if main {
        CLAP_PORT_STEREO.as_ptr()
    } else {
        CLAP_PORT_MONO.as_ptr()
    };
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

unsafe extern "C" fn note_ports_count(_plugin: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(is_input)
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if index != 0 || !is_input {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = unsafe { instance(plugin) }.probe.dialects.load(Relaxed);
    info.preferred_dialect = info.supported_dialects;
    true
}

// --- Helpers for the tests ---------------------------------------------------

/// Loads the probe's library and creates its plugin on this thread.
fn hosted(probe: &Arc<Probe>) -> ClapPlugin {
    NEXT.with(|next| *next.borrow_mut() = Some(Arc::clone(probe)));
    let library = unsafe { ClapLibrary::from_entry(&raw const ENTRY, "edges") }.unwrap();
    library.create(ID).unwrap()
}

fn events(list: &[(u32, EventKind)]) -> EventBuffer {
    let mut buffer = EventBuffer::with_capacity(list.len().max(1));
    for &(offset, kind) in list {
        buffer.push(Event { offset, kind }).unwrap();
    }
    buffer
}

fn note_on() -> EventKind {
    EventKind::NoteOn {
        note: NoteRef::new(0, 60),
        velocity: 1.0,
    }
}

fn knob(value: f64) -> EventKind {
    EventKind::ParamValue { param: KNOB, value }
}

fn spec(sample_rate: f64) -> ProcessSpec {
    ProcessSpec {
        sample_rate,
        max_block_size: 64,
        num_channels: 2,
    }
}

/// The host's remarks about the plugin among what `pump` returned.
fn remarks(pumped: &[HostEvent]) -> Vec<&str> {
    pumped
        .iter()
        .filter_map(|event| match event {
            HostEvent::Log {
                severity: LogSeverity::PluginMisbehaving,
                message,
            } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

// --- The transport -----------------------------------------------------------

/// Code on the audio thread must not panic, whatever the caller hands it.
///
/// `Transport::advance` adds the frame count to `position_samples` with a
/// plain `+`. With a position within one block of `i64::MAX` that
/// overflows, which is a panic in a build with overflow checks. The host
/// moves a copy of the caller's transport on only between the pieces of a
/// block, never after the last one, and with an addition of its own that
/// stops at the end of the range.
#[test]
fn a_transport_at_the_end_of_its_range_does_not_panic_the_audio_thread() {
    // One piece, and four: activated for 64 frames, blocks of 64 and 200.
    for frames in [64, 200] {
        let probe = Probe::new();
        let mut plugin = hosted(&probe);
        let mut processor = plugin.activate(48_000.0, 64).unwrap();

        let mut transport = Transport::playing_at(120.0);
        transport.position_samples = Some(i64::MAX - 10);
        let list = EventBuffer::new();
        let mut block = AudioBuffer::<f32>::new(2, frames);
        let mut heap = usize::MAX;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            heap = heap_calls_during(|| {
                processor.process_with(&mut block, &ProcessContext::new(&list, &transport));
            });
        }));
        assert!(
            outcome.is_ok(),
            "process_with panicked on a transport position near i64::MAX"
        );
        assert_eq!(heap, 0, "the host used the heap");
        assert_eq!(block.peak(), 1.0, "and the plugin's output was kept");
        assert_eq!(processor.status(), ProcessStatus::Continue);
        assert_eq!(probe.pieces.load(Relaxed) as usize, frames.div_ceil(64));
        assert_eq!(
            transport.position_samples,
            Some(i64::MAX - 10),
            "the caller's transport was moved"
        );
        assert_eq!(plugin.failed_blocks(), 0);
    }
}

/// A block longer than the activation size is processed in pieces, and each
/// piece is told where the music is at its own first frame.
#[test]
fn every_piece_of_a_block_is_told_where_the_music_is() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let seconds = |frames: f64| ((frames / 48_000.0) * CLAP_SECTIME_FACTOR as f64).round() as i64;

    let mut transport = Transport::playing_at(120.0);
    transport.position_samples = Some(48_000);
    let list = EventBuffer::new();
    let context = ProcessContext::new(&list, &transport);

    // One piece: the position the caller gave.
    processor.process_with(&mut AudioBuffer::<f32>::new(2, 64), &context);
    assert_eq!(probe.seconds.load(Relaxed), seconds(48_000.0));

    // Four pieces, the last of which starts 192 frames into the block.
    processor.process_with(&mut AudioBuffer::<f32>::new(2, 200), &context);
    assert_eq!(probe.pieces.load(Relaxed), 5);
    assert_eq!(probe.seconds.load(Relaxed), seconds(48_192.0));

    // A stopped transport does not move from piece to piece.
    transport.playing = false;
    processor.process_with(
        &mut AudioBuffer::<f32>::new(2, 200),
        &ProcessContext::new(&list, &transport),
    );
    assert_eq!(probe.seconds.load(Relaxed), seconds(48_000.0));
}

// --- The plugin's requests ---------------------------------------------------

/// `request_callback` asks the host to call `on_main_thread` on its main
/// thread, and a plugin may ask again from inside that call: it is how work
/// is spread over several turns of the main loop. `ClapPlugin::pump` is
/// where a requested callback is made, and one asked for from inside the
/// callback is made by the next `pump`.
#[test]
fn a_callback_asked_for_inside_the_callback_is_made_by_the_next_pump() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let _ = plugin.pump();
    assert_eq!(probe.callbacks.load(Relaxed), 0);

    probe.rearm.store(1, Relaxed);
    unsafe { (*probe.host()).request_callback.unwrap()(probe.host()) };
    let _ = plugin.pump();
    assert_eq!(probe.callbacks.load(Relaxed), 1, "the first callback");
    assert_eq!(
        probe.rearm.load(Relaxed),
        0,
        "the plugin did ask for a second one"
    );

    // The plugin asked for one more while it was being called back. It
    // comes with the next pump, and only once however often the
    // application pumps after that.
    let _ = plugin.pump();
    assert_eq!(
        probe.callbacks.load(Relaxed),
        2,
        "a callback requested from inside on_main_thread was never made"
    );
    for _ in 0..3 {
        let _ = plugin.pump();
    }
    assert_eq!(probe.callbacks.load(Relaxed), 2, "called back unasked");
}

/// A plugin that asks for another callback every time it is called back
/// gets one per `pump`, and `pump` returns each time.
#[test]
fn a_plugin_that_always_asks_again_is_called_back_once_per_pump() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let _ = plugin.pump();

    probe.rearm.store(u32::MAX, Relaxed);
    unsafe { (*probe.host()).request_callback.unwrap()(probe.host()) };
    for round in 1..=5 {
        let _ = plugin.pump();
        assert_eq!(probe.callbacks.load(Relaxed), round);
    }
    // Once it stops asking, the last request is answered and that is all.
    probe.rearm.store(0, Relaxed);
    let _ = plugin.pump();
    let _ = plugin.pump();
    assert_eq!(probe.callbacks.load(Relaxed), 6);
}

// --- Parameter values, there and back ----------------------------------------

/// A value on its way to the plugin is held to the range the plugin
/// declared, whichever of the four ways it takes: set while the plugin is
/// inactive, set while it is active, as an event in a block, and as the
/// value whose text is asked for.
#[test]
fn what_a_plugin_is_told_a_parameter_is_stays_inside_the_declared_range() {
    for (told, kept) in [(99.0f64, KNOB_MAX), (-99.0, 0.0), (1.25, 1.25)] {
        let probe = Probe::new();
        let mut plugin = hosted(&probe);

        // Inactive: handed over there and then, through a flush.
        plugin.set_param(KNOB, told).unwrap();
        assert_eq!(probe.knob(), kept, "set while inactive");

        // The text of a value.
        assert!(plugin.param_to_text(KNOB, told).is_some());
        assert_eq!(f64::from_bits(probe.text_of.load(Relaxed)), kept);

        // Active: through the queue, with the next block.
        let mut processor = plugin.activate(48_000.0, 64).unwrap();
        probe.knob.store(f64::NAN.to_bits(), Relaxed);
        plugin.set_param(KNOB, told).unwrap();
        let mut block = AudioBuffer::<f32>::new(2, 64);
        processor.process(&mut block);
        assert_eq!(probe.knob(), kept, "set while active");

        // An event of the block.
        probe.knob.store(f64::NAN.to_bits(), Relaxed);
        let list = events(&[(7, knob(told))]);
        processor.process_with(&mut block, &ProcessContext::new(&list, &Transport::STOPPED));
        assert_eq!(probe.knob(), kept, "as an event");
        assert_eq!(probe.values_seen.load(Relaxed), 3);
    }

    // Something that is not a number is not sent at all.
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    assert!(plugin.set_param(KNOB, f64::NAN).is_err());
    assert_eq!(plugin.param_to_text(KNOB, f64::NAN), None);
    assert_eq!(probe.texts_asked.load(Relaxed), 0);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let list = events(&[(0, knob(f64::NAN))]);
    processor.process_with(
        &mut AudioBuffer::<f32>::new(2, 64),
        &ProcessContext::new(&list, &Transport::STOPPED),
    );
    assert_eq!(probe.values_seen.load(Relaxed), 0);
    assert_eq!(plugin.undeliverable_events(), 1);
}

/// A plugin's answers are not taken on faith: a parameter value is held to
/// the range the plugin itself declared, on every way a value comes back
/// from it.
#[test]
fn what_a_plugin_says_a_parameter_is_stays_inside_the_declared_range() {
    for (said, kept) in [(99.0f64, KNOB_MAX), (-99.0, 0.0), (1.25, 1.25)] {
        let probe = Probe::new();
        probe.report.store(said.to_bits(), Relaxed);
        probe.value.store(said.to_bits(), Relaxed);
        let mut plugin = hosted(&probe);
        let declared = plugin.param(KNOB).unwrap();
        assert_eq!((declared.min, declared.max), (0.0, KNOB_MAX));

        let mut seen: Vec<(&str, f64)> = Vec::new();
        seen.extend(plugin.param_value(KNOB).map(|value| ("param_value", value)));
        seen.extend(
            plugin
                .text_to_param(KNOB, &said.to_string())
                .map(|value| ("text_to_param", value)),
        );

        // A flush on the main thread, while the plugin is not active.
        plugin.flush().unwrap();
        for event in plugin.pump() {
            if let HostEvent::ParamValue { param, value } = event {
                assert_eq!(param, KNOB);
                seen.push(("flush", value));
            }
        }

        let mut processor = plugin.activate(48_000.0, 64).unwrap();
        let mut block = AudioBuffer::<f32>::new(2, 64);
        let heap = heap_calls_during(|| processor.process(&mut block));
        assert_eq!(heap, 0, "the host used the heap");
        for event in processor.output_events() {
            if let OutputKind::Event(EventKind::ParamValue { param, value }) = event.kind {
                assert_eq!(param, KNOB);
                seen.push(("output_events", value));
            }
        }
        let pumped = plugin.pump();
        assert!(remarks(&pumped).is_empty(), "{pumped:?}");
        for event in pumped {
            if let HostEvent::ParamValue { value, .. } = event {
                seen.push(("pump", value));
            }
        }

        assert_eq!(
            seen,
            [
                ("param_value", kept),
                ("text_to_param", kept),
                ("flush", kept),
                ("output_events", kept),
                ("pump", kept),
            ],
            "the plugin said {said} for a parameter declared 0..={KNOB_MAX}"
        );
    }
}

/// Infinity and "not a number" are not values at all, whichever way they
/// come back.
#[test]
fn a_value_that_is_not_a_finite_number_is_not_passed_on() {
    for said in [f64::INFINITY, f64::NEG_INFINITY] {
        let probe = Probe::new();
        probe.report.store(said.to_bits(), Relaxed);
        probe.value.store(said.to_bits(), Relaxed);
        let mut plugin = hosted(&probe);
        assert_eq!(plugin.param_value(KNOB), None);
        assert_eq!(plugin.text_to_param(KNOB, &said.to_string()), None);
        assert_eq!(plugin.text_to_param(KNOB, "NaN"), None);
        plugin.flush().unwrap();
        assert_eq!(plugin.pump(), []);

        let mut processor = plugin.activate(48_000.0, 64).unwrap();
        processor.process(&mut AudioBuffer::<f32>::new(2, 64));
        assert_eq!(processor.output_events(), []);
        assert_eq!(plugin.pump(), []);
    }
}

/// A value or a gesture for a parameter the plugin never listed is of no
/// use to an application, which could not look the parameter up. It is not
/// passed on by either half, and the main thread is told that it happened.
#[test]
fn a_report_about_a_parameter_the_plugin_never_listed_is_dropped_and_remarked_on() {
    let probe = Probe::new();
    probe.report.store(0.5f64.to_bits(), Relaxed);
    probe.report_id.store(GHOST.0, Relaxed);
    probe.report_gesture.store(true, Relaxed);
    let mut plugin = hosted(&probe);
    assert_eq!(plugin.param(GHOST), None);
    let remark = |count: u32| {
        format!(
            "the plugin reported a value or a gesture for a parameter that is not in its list \
             {count} time(s); the reports were dropped"
        )
    };
    let only_the_remark = |pumped: Vec<HostEvent>, count: u32| {
        assert_eq!(
            pumped,
            [HostEvent::Log {
                severity: LogSeverity::PluginMisbehaving,
                message: remark(count),
            }]
        );
    };

    // On the main thread, through a flush: a gesture begins, a value, the
    // gesture ends.
    plugin.flush().unwrap();
    only_the_remark(plugin.pump(), 3);
    assert_eq!(plugin.pump(), [], "remarked on twice");

    // On the audio thread, in each of the four pieces of a block.
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 200);
    let heap = heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0, "the host used the heap");
    assert_eq!(processor.output_events(), []);
    assert_eq!(processor.status(), ProcessStatus::Continue);
    only_the_remark(plugin.pump(), 12);

    // And through the audio half's own flush.
    let heap = heap_calls_during(|| processor.flush());
    assert_eq!(heap, 0);
    assert_eq!(processor.output_events(), []);
    only_the_remark(plugin.pump(), 3);

    // The same reports about a parameter the plugin does list arrive.
    probe.report_id.store(KNOB.0, Relaxed);
    processor.process(&mut AudioBuffer::<f32>::new(2, 64));
    let kinds: Vec<OutputKind> = processor
        .output_events()
        .iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            OutputKind::GestureBegin { param: KNOB },
            OutputKind::Event(knob(0.5)),
            OutputKind::GestureEnd { param: KNOB },
        ]
    );
    assert_eq!(
        plugin.pump(),
        [
            HostEvent::GestureBegin { param: KNOB },
            HostEvent::ParamValue {
                param: KNOB,
                value: 0.5
            },
            HostEvent::GestureEnd { param: KNOB },
        ]
    );
    // None of this is an event that was lost or could not be sent.
    assert_eq!(
        (plugin.lost_events(), plugin.undeliverable_events()),
        (0, 0)
    );
}

// --- The sidechain -----------------------------------------------------------

/// A sidechain shorter than the block cannot be lined up with it, and is
/// treated as not connected. That is decided for the block as a whole, so
/// the same call gives the same audio whatever size the plugin was
/// activated with.
#[test]
fn a_short_sidechain_is_treated_alike_whatever_the_activation_size() {
    let render = |max_block: usize, side_frames: usize| {
        let probe = Probe::new();
        probe.effect.store(true, Relaxed);
        let mut plugin = hosted(&probe);
        assert_eq!(plugin.audio_inputs().len(), 2);
        let mut processor = plugin.activate(48_000.0, max_block).unwrap();

        let mut sidechain = AudioBuffer::<f32>::new(1, side_frames);
        sidechain.channel_mut(0).fill(0.5);
        let list = EventBuffer::new();
        let context = ProcessContext::new(&list, &Transport::STOPPED).with_sidechain(&sidechain);
        let mut block = AudioBuffer::<f32>::new(2, 200);
        let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
        assert_eq!(heap, 0, "the host used the heap");
        assert_eq!(processor.status(), ProcessStatus::Continue);
        block
    };
    let heard = |block: &AudioBuffer<f32>| {
        block
            .channel(0)
            .iter()
            .filter(|&&sample| sample != 0.0)
            .count()
    };

    // Shorter than the block by half, by one frame, and empty: not
    // connected, in one piece and in several. The lengths around 64 and
    // 128 are the ones that used to let the first pieces through.
    for side_frames in [0, 1, 63, 64, 65, 100, 128, 192, 199] {
        for max_block in [256, 200, 64, 7, 1] {
            let block = render(max_block, side_frames);
            assert_eq!(
                heard(&block),
                0,
                "a sidechain of {side_frames} frames reached a 200-frame block cut into pieces \
                 of {max_block}"
            );
        }
    }

    // As long as the block, or longer: every frame of it reaches the
    // plugin, in one piece and in several, and what is beyond the block is
    // not used.
    for side_frames in [200, 201, 1_000] {
        let whole = render(256, side_frames);
        assert!(whole.channel(0).iter().all(|&sample| sample == 0.5));
        for max_block in [200, 64, 7, 1] {
            common::assert_same_bits(
                &render(max_block, side_frames),
                &whole,
                &format!("a sidechain of {side_frames} frames in pieces of {max_block}"),
            );
        }
    }
}

// --- Blocks the plugin is not called for -------------------------------------

/// A block that ends in silence before the plugin is called keeps its
/// events. They are delivered on the first frame of the next block the
/// plugin does process, once, and nothing is counted as lost.
#[test]
fn an_event_in_a_block_that_fails_is_delivered_by_the_next_block_that_does_not() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();

    // Prepared for another sample rate: two silent blocks, a parameter
    // value in the first and a note in the second.
    processor.prepare(&spec(44_100.0));
    let mut block = AudioBuffer::<f32>::new(2, 64);
    for list in [events(&[(3, knob(0.25))]), events(&[(40, note_on())])] {
        block.channel_mut(0).fill(1.0);
        let context = ProcessContext::new(&list, &Transport::STOPPED);
        let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
        assert_eq!(heap, 0, "the host used the heap");
        assert_eq!(block.peak(), 0.0);
        assert_eq!(
            processor.status(),
            ProcessStatus::Failed(ProcessFailure::SampleRateMismatch)
        );
    }
    assert_eq!(probe.pieces.load(Relaxed), 0, "the plugin was called");
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (0, 0)
    );

    // The rate put right: both arrive with the next block, and only then.
    processor.prepare(&spec(48_000.0));
    let heap = heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0);
    assert_eq!(processor.status(), ProcessStatus::Continue);
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (1, 1)
    );
    assert_eq!(probe.knob(), 0.25);
    processor.process(&mut block);
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (1, 1),
        "an event was delivered twice"
    );
    assert_eq!(
        (plugin.lost_events(), plugin.undeliverable_events()),
        (0, 0)
    );
    assert_eq!(plugin.failed_blocks(), 2);
}

/// The same holds when the plugin refuses to start processing.
#[test]
fn an_event_in_a_block_the_plugin_would_not_start_for_is_delivered_once_it_starts() {
    let probe = Probe::new();
    probe.start.store(false, Relaxed);
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();

    let list = events(&[(0, knob(1.5)), (10, note_on())]);
    let context = ProcessContext::new(&list, &Transport::STOPPED);
    let mut block = AudioBuffer::<f32>::new(2, 64);
    let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
    assert_eq!(heap, 0, "the host used the heap");
    assert_eq!(
        processor.status(),
        ProcessStatus::Failed(ProcessFailure::StartRefused)
    );
    assert_eq!(probe.pieces.load(Relaxed), 0);

    probe.start.store(true, Relaxed);
    processor.process(&mut block);
    assert_eq!(processor.status(), ProcessStatus::Continue);
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (1, 1)
    );
    assert_eq!(probe.knob(), 1.5);
    assert_eq!(
        (plugin.lost_events(), plugin.undeliverable_events()),
        (0, 0)
    );
}

/// 1,024 events can wait for the next block. What does not fit is counted
/// as lost, so that an application can tell, and what does fit is
/// delivered in the order it came.
#[test]
fn events_that_cannot_wait_any_longer_are_counted_as_lost() {
    const WAITING: usize = 1_024;
    const PER_BLOCK: usize = 400;
    const BLOCKS: usize = 3;

    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    processor.prepare(&spec(44_100.0));

    // Every event sets the knob to a value of its own, so that the last
    // one delivered can be told from the rest.
    let value_of = |index: usize| index as f64 / (PER_BLOCK * BLOCKS) as f64;
    let mut block = AudioBuffer::<f32>::new(2, 64);
    for round in 0..BLOCKS {
        let mut list = EventBuffer::with_capacity(PER_BLOCK);
        for index in 0..PER_BLOCK {
            let kind = knob(value_of(round * PER_BLOCK + index));
            list.push(Event { offset: 0, kind }).unwrap();
        }
        let context = ProcessContext::new(&list, &Transport::STOPPED);
        let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
        assert_eq!(heap, 0, "the host used the heap");
    }
    let sent = PER_BLOCK * BLOCKS;
    assert_eq!(plugin.lost_events() as usize, sent - WAITING);

    processor.prepare(&spec(48_000.0));
    let heap = heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0);
    assert_eq!(probe.values_seen.load(Relaxed) as usize, WAITING);
    assert_eq!(probe.knob(), value_of(WAITING - 1));
    assert_eq!(plugin.lost_events() as usize, sent - WAITING);
}

/// A reset drops what is sounding and leaves parameters as they are, and
/// the events waiting for the next block are treated alike: a note is
/// forgotten, a parameter value still arrives, and neither counts as lost.
#[test]
fn a_reset_forgets_a_waiting_note_and_still_delivers_a_waiting_parameter_value() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 64);

    // Both events are placed past the end of the block, so both wait.
    let list = events(&[(64, knob(0.75)), (64, note_on())]);
    processor.process_with(&mut block, &ProcessContext::new(&list, &Transport::STOPPED));
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (0, 0)
    );

    let heap = heap_calls_during(|| processor.reset());
    assert_eq!(heap, 0, "the host used the heap");
    processor.process(&mut block);
    assert_eq!(
        (
            probe.values_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed)
        ),
        (1, 0)
    );
    assert_eq!(probe.knob(), 0.75);
    assert_eq!(
        (plugin.lost_events(), plugin.undeliverable_events()),
        (0, 0)
    );
}

// --- The limit on events -----------------------------------------------------

/// `EVENTS_PER_BLOCK` is counted over two different stretches, as its
/// documentation says: what the plugin is handed in one call of its process
/// function, and what is kept of its reports over one block of the
/// caller's.
#[test]
fn the_event_limit_is_per_call_on_the_way_in_and_per_block_on_the_way_out() {
    const EXTRA: usize = 100;
    let filled = |second_piece_from: usize| {
        let mut list = EventBuffer::with_capacity(2 * EVENTS_PER_BLOCK);
        for index in 0..EVENTS_PER_BLOCK + EXTRA {
            let offset = if index < second_piece_from { 0 } else { 64 };
            let kind = knob(0.5);
            list.push(Event { offset, kind }).unwrap();
        }
        list
    };

    // On the way in. A block of two pieces, with every event in the first:
    // the plugin gets a call's worth, and the rest is counted as lost.
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 128);
    let list = filled(usize::MAX);
    let context = ProcessContext::new(&list, &Transport::STOPPED);
    let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
    assert_eq!(heap, 0, "the host used the heap");
    assert_eq!(probe.pieces.load(Relaxed), 2);
    assert_eq!(
        probe.values_seen.swap(0, Relaxed) as usize,
        EVENTS_PER_BLOCK
    );
    assert_eq!(plugin.lost_events() as usize, EXTRA);

    // As many events again, with the surplus in the second piece: each call
    // stays within the limit, and every event arrives.
    let list = filled(EVENTS_PER_BLOCK);
    let context = ProcessContext::new(&list, &Transport::STOPPED);
    let heap = heap_calls_during(|| processor.process_with(&mut block, &context));
    assert_eq!(heap, 0);
    assert_eq!(
        probe.values_seen.load(Relaxed) as usize,
        EVENTS_PER_BLOCK + EXTRA
    );
    assert_eq!(plugin.lost_events() as usize, EXTRA, "an event was lost");

    // On the way out. The plugin reports once in every call, and is called
    // once a frame. Over one block of more frames than the limit, the
    // limit is what is kept.
    let probe = Probe::new();
    probe.report.store(0.5f64.to_bits(), Relaxed);
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 1).unwrap();
    let mut long = AudioBuffer::<f32>::new(2, EVENTS_PER_BLOCK + EXTRA);
    let heap = heap_calls_during(|| processor.process(&mut long));
    assert_eq!(heap, 0, "the host used the heap");
    assert_eq!(
        probe.pieces.swap(0, Relaxed) as usize,
        EVENTS_PER_BLOCK + EXTRA
    );
    assert_eq!(processor.output_events().len(), EVENTS_PER_BLOCK);
    // Each on the frame of the call it was made in.
    assert!(
        processor
            .output_events()
            .iter()
            .enumerate()
            .all(|(frame, event)| event.offset as usize == frame)
    );
    assert!(plugin.lost_events() as usize >= EXTRA);

    // The same number of reports over two blocks: all of them are kept,
    // since the count starts again with each block.
    let half = (EVENTS_PER_BLOCK + EXTRA) / 2;
    for _ in 0..2 {
        let mut block = AudioBuffer::<f32>::new(2, half);
        processor.process(&mut block);
        assert_eq!(processor.output_events().len(), half);
    }
}

// --- Note ports --------------------------------------------------------------

/// A note port that reads MIDI only as MPE is sent plain MIDI, as
/// `NotePortInfo::midi_mpe` says: a note becomes a note-on message, and a
/// MIDI message goes through as it is.
#[test]
fn a_note_port_that_takes_only_mpe_is_sent_plain_midi_as_documented() {
    let probe = Probe::new();
    probe.dialects.store(CLAP_NOTE_DIALECT_MIDI_MPE, Relaxed);
    let mut plugin = hosted(&probe);
    let [port] = plugin.note_inputs() else {
        panic!("expected one note input");
    };
    assert!(port.midi_mpe && !port.midi && !port.clap);

    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = AudioBuffer::<f32>::new(2, 64);
    let list = events(&[(0, note_on())]);
    let heap = heap_calls_during(|| {
        processor.process_with(&mut block, &ProcessContext::new(&list, &Transport::STOPPED));
    });
    assert_eq!(heap, 0);
    assert_eq!(
        (probe.midi_seen.load(Relaxed), plugin.undeliverable_events()),
        (1, 0),
        "(MIDI events the plugin received, events the host did not send)"
    );
    assert_eq!(probe.midi_status.load(Relaxed), 0x90);
    assert_eq!(probe.notes_seen.load(Relaxed), 0, "sent as a CLAP note");

    // Pitch bend on a member channel, as an MPE controller sends it.
    let bend = EventKind::Midi(MidiMessage::PitchBend {
        channel: 3,
        value: 1_000,
    });
    processor.process_with(
        &mut block,
        &ProcessContext::new(&events(&[(5, bend)]), &Transport::STOPPED),
    );
    assert_eq!(
        (probe.midi_seen.load(Relaxed), plugin.undeliverable_events()),
        (2, 0)
    );
    assert_eq!(probe.midi_status.load(Relaxed), 0xE3);
}

/// A port with no dialect this host can write is sent nothing, and the
/// events are counted as what they are: undeliverable.
#[test]
fn a_note_port_that_takes_only_midi_2_is_sent_nothing() {
    let probe = Probe::new();
    probe.dialects.store(CLAP_NOTE_DIALECT_MIDI2, Relaxed);
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    processor.process_with(
        &mut AudioBuffer::<f32>::new(2, 64),
        &ProcessContext::new(&events(&[(0, note_on())]), &Transport::STOPPED),
    );
    assert_eq!(
        (
            probe.midi_seen.load(Relaxed),
            probe.notes_seen.load(Relaxed),
            plugin.undeliverable_events()
        ),
        (0, 0, 1)
    );
}

// --- Threads -----------------------------------------------------------------

/// When the main half is dropped first and the audio half is dropped on
/// another thread, the plugin can no longer be destroyed where CLAP
/// requires, and is left loaded instead. That has to hold when the thread
/// that created the plugin has ended by then, which is the natural reason
/// for a main half to go first.
///
/// A thread used to be told apart by the address of a thread-local, which
/// the system hands to a later thread once the first has ended. The later
/// thread then passed for the plugin's main thread and destroyed it. How
/// often an address comes round is up to the system (between 1 and 255 of
/// 256 rounds on the machine this was found on), so many rounds are run.
#[test]
fn a_plugin_is_never_destroyed_on_a_thread_other_than_its_main_thread() {
    // Fewer under an interpreter, where starting a thread is slow and an
    // address comes round no less readily.
    const ROUNDS: usize = if cfg!(miri) { 8 } else { 256 };
    let mut destroyed_elsewhere = 0;
    let mut taken_for_main = 0;
    for _ in 0..ROUNDS {
        let probe = Probe::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        let for_main = Arc::clone(&probe);
        // The plugin's main thread: creates, activates, lets go of the main
        // half and ends.
        let main_thread = std::thread::spawn(move || {
            let mut plugin = hosted(&for_main);
            let processor = plugin.activate(48_000.0, 64).unwrap();
            sender.send(processor).unwrap();
            drop(plugin);
            std::thread::current().id()
        })
        .join()
        .unwrap();
        let mut processor = receiver.recv().unwrap();
        assert_eq!(probe.destroyed.load(Relaxed), 0);

        // Another thread, started after the main thread is gone.
        let host = probe.host() as usize;
        let passed_for_main = std::thread::spawn(move || {
            processor.process(&mut AudioBuffer::<f32>::new(2, 64));
            // What the plugin is told when it asks on this thread, outside
            // any call into it.
            let host = host as *const clap_host;
            let check = unsafe { thread_check(host) };
            let main = unsafe { check.is_main_thread.unwrap()(host) };
            let audio = unsafe { check.is_audio_thread.unwrap()(host) };
            assert!(!audio, "the audio role outlived the call");
            drop(processor);
            main
        })
        .join()
        .unwrap();
        // Inside `process` that thread was the audio thread, and was not
        // the main thread.
        assert!(probe.audio_in_process.load(Relaxed));
        if passed_for_main || probe.main_in_process.load(Relaxed) {
            taken_for_main += 1;
        }
        if probe.destroyed.load(Relaxed) > 0 {
            assert_ne!(*probe.destroyed_on.lock().unwrap(), Some(main_thread));
            destroyed_elsewhere += 1;
        }
    }
    assert_eq!(
        taken_for_main, 0,
        "in {taken_for_main} of {ROUNDS} rounds a later thread was told it is the plugin's main \
         thread"
    );
    assert_eq!(
        destroyed_elsewhere, 0,
        "in {destroyed_elsewhere} of {ROUNDS} rounds the plugin was destroyed on a thread that \
         was not its main thread"
    );
}

/// The other way round stays as it was: an audio half dropped on the thread
/// that created the plugin, after the main half, takes the plugin with it,
/// there.
#[test]
fn a_plugin_whose_halves_both_go_on_its_main_thread_is_destroyed_there() {
    let probe = Probe::new();
    let mut plugin = hosted(&probe);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    drop(plugin);
    assert_eq!(probe.destroyed.load(Relaxed), 0);
    processor.process(&mut AudioBuffer::<f32>::new(2, 64));
    // On the main thread, in the audio role for the length of the call.
    assert!(probe.main_in_process.load(Relaxed) && probe.audio_in_process.load(Relaxed));
    drop(processor);
    assert_eq!(probe.destroyed.load(Relaxed), 1);
    assert_eq!(
        *probe.destroyed_on.lock().unwrap(),
        Some(std::thread::current().id())
    );
}
