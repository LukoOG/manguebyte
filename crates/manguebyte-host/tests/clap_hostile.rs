//! Hosts a plugin that does everything a plugin should not.
//!
//! The example plugins are well behaved, so hosting them says nothing about
//! what happens when a plugin is not. Here the plugin is written in this
//! file, against the raw C interface, with a script that tells it how to
//! misbehave: lie about how many plugins, parameters and ports it has,
//! leave strings unterminated, answer with numbers that are not numbers,
//! refuse to start, fail while processing, call the host from the wrong
//! thread, write state without end.
//!
//! It is also the one plugin here that talks back: it reports note ends,
//! parameter changes, gestures and MIDI while processing, and uses every
//! callback and extension the host offers, so those are tested here too.
//!
//! The plugin lives in this executable and is reached through
//! `ClapLibrary::from_entry`. Its code runs under the same heap watcher as
//! the host's, and is written not to allocate while processing, so a
//! non-zero count is still the host's doing.
//!
//! Every `unsafe` block in this file is one side or the other of the C
//! interface, with pointers that are valid for the call, so the blocks are
//! not commented one by one.
#![cfg(feature = "clap")]
#![allow(clippy::undocumented_unsafe_blocks)]

mod common;

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize,
};
use std::thread::ThreadId;

use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::event::{Event, EventBuffer, EventKind, Expression, NoteRef};
use manguebyte_dsp::{ProcessContext, Processor, Transport};
use manguebyte_host::clap::clap_sys::audio_buffer::clap_audio_buffer;
use manguebyte_host::clap::clap_sys::entry::clap_plugin_entry;
use manguebyte_host::clap::clap_sys::events::*;
use manguebyte_host::clap::clap_sys::ext::audio_ports::*;
use manguebyte_host::clap::clap_sys::ext::latency::*;
use manguebyte_host::clap::clap_sys::ext::log::*;
use manguebyte_host::clap::clap_sys::ext::note_ports::*;
use manguebyte_host::clap::clap_sys::ext::params::*;
use manguebyte_host::clap::clap_sys::ext::state::*;
use manguebyte_host::clap::clap_sys::ext::tail::*;
use manguebyte_host::clap::clap_sys::ext::thread_check::*;
use manguebyte_host::clap::clap_sys::factory::plugin_factory::*;
use manguebyte_host::clap::clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use manguebyte_host::clap::clap_sys::host::clap_host;
use manguebyte_host::clap::clap_sys::id::CLAP_INVALID_ID;
use manguebyte_host::clap::clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use manguebyte_host::clap::clap_sys::process::*;
use manguebyte_host::clap::clap_sys::stream::{clap_istream, clap_ostream};
use manguebyte_host::clap::clap_sys::version::{CLAP_VERSION, clap_version};
use manguebyte_host::clap::{
    ClapError, ClapLibrary, ClapPlugin, HostEvent, HostInfo, LogSeverity, MAX_FEATURES,
    MAX_LATENCY_FRAMES, MAX_PARAMS, MAX_PLUGINS_PER_LIBRARY, MAX_TEXT_BYTES, OutputEvent,
    OutputKind, ParamsRescan, ProcessFailure, ProcessStatus, Tail,
};
use manguebyte_midi::MidiMessage;

// --- The script ------------------------------------------------------------

const GAIN: ParamId = ParamId(10);
const MODE: ParamId = ParamId(11);
const WILD: ParamId = ParamId(13);

/// What the plugin asks to get back with every event for its gain.
const COOKIE: *mut c_void = 0xC0FFEE as *mut c_void;

/// Things the plugin does to the host when its main-thread callback runs.
mod ask {
    pub const RESCAN_VALUES: u32 = 1 << 0;
    pub const RESCAN_ALL: u32 = 1 << 1;
    pub const DIRTY: u32 = 1 << 2;
    pub const LATENCY: u32 = 1 << 3;
    pub const PORT_NAMES: u32 = 1 << 4;
    pub const PORT_LIST: u32 = 1 << 5;
    pub const NOTE_PORTS: u32 = 1 << 6;
    pub const CLEAR: u32 = 1 << 7;
    pub const FLUSH: u32 = 1 << 8;
    pub const LOG: u32 = 1 << 9;
    pub const FROM_A_WORKER: u32 = 1 << 10;
    pub const LOOK_AROUND: u32 = 1 << 11;
}

/// What a fake plugin is told to do, and what it saw. Shared by a test and
/// the plugin it hosts.
struct Script {
    // What to do.
    library_init: AtomicBool,
    factory: AtomicBool,
    plugin_count: AtomicU32,
    create: AtomicBool,
    has_process: AtomicBool,
    extensions: AtomicBool,
    plugin_init: AtomicBool,
    activate: AtomicBool,
    start: AtomicBool,
    status: AtomicI32,
    param_count: AtomicU32,
    /// Text without terminators and numbers that are not numbers.
    wild: AtomicBool,
    layout: AtomicU32,
    midi_only: AtomicBool,
    constant_right: AtomicBool,
    report: AtomicBool,
    chatty: AtomicBool,
    asks: AtomicU32,
    latency: AtomicU32,
    tail: AtomicU32,
    /// 0 saves and loads, 1 refuses, 2 never stops writing.
    state_mode: AtomicU32,
    state: Mutex<Vec<u8>>,

    // What was seen.
    host: AtomicPtr<clap_host>,
    clock: AtomicU32,
    descriptor_calls: AtomicU32,
    creates: AtomicU32,
    info_calls: AtomicU32,
    destroyed: AtomicU32,
    destroyed_at: AtomicU32,
    destroyed_on: Mutex<Option<ThreadId>>,
    deactivated_at: AtomicU32,
    stopped_at: AtomicU32,
    starts: AtomicU32,
    resets: AtomicU32,
    flushes: AtomicU32,
    callbacks: AtomicU32,
    blocks: AtomicU32,
    write_calls: AtomicU32,
    read_calls: AtomicU32,
    largest_block: AtomicU32,
    allowed_block: AtomicU32,
    unsorted: AtomicBool,
    late: AtomicBool,
    bad_cookie: AtomicBool,
    gain: AtomicU64,
    mode: AtomicU64,
    main_in_process: AtomicBool,
    audio_in_process: AtomicBool,
    main_in_flush: AtomicBool,
    audio_in_flush: AtomicBool,
    main_on_main: AtomicBool,
    audio_on_main: AtomicBool,
    main_in_worker: AtomicBool,
    audio_in_worker: AtomicBool,
    transport_flags: AtomicU32,
    tempo: AtomicU64,
    beats: AtomicI64,
    steady: AtomicI64,
    inputs_given: AtomicU32,
    outputs_given: AtomicU32,
    side_level: AtomicU32,
    /// Events received, each as frame since activation, type and a detail.
    seen: [AtomicU64; 64],
    seen_len: AtomicUsize,
    loaded: Mutex<Vec<u8>>,
    host_text: Mutex<String>,
    rescan_flags_supported: AtomicU32,
    dialects: AtomicU32,
    unknown_extension_is_null: AtomicBool,
}

impl Script {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            library_init: AtomicBool::new(true),
            factory: AtomicBool::new(true),
            plugin_count: AtomicU32::new(7),
            create: AtomicBool::new(true),
            has_process: AtomicBool::new(true),
            extensions: AtomicBool::new(true),
            plugin_init: AtomicBool::new(true),
            activate: AtomicBool::new(true),
            start: AtomicBool::new(true),
            status: AtomicI32::new(CLAP_PROCESS_CONTINUE),
            param_count: AtomicU32::new(5),
            wild: AtomicBool::new(false),
            layout: AtomicU32::new(0),
            midi_only: AtomicBool::new(false),
            constant_right: AtomicBool::new(false),
            report: AtomicBool::new(false),
            chatty: AtomicBool::new(false),
            asks: AtomicU32::new(0),
            latency: AtomicU32::new(64),
            tail: AtomicU32::new(100),
            state_mode: AtomicU32::new(0),
            state: Mutex::new(b"the fake plugin's state, 0123456789".to_vec()),
            host: AtomicPtr::new(ptr::null_mut()),
            clock: AtomicU32::new(1),
            descriptor_calls: AtomicU32::new(0),
            creates: AtomicU32::new(0),
            info_calls: AtomicU32::new(0),
            destroyed: AtomicU32::new(0),
            destroyed_at: AtomicU32::new(0),
            destroyed_on: Mutex::new(None),
            deactivated_at: AtomicU32::new(0),
            stopped_at: AtomicU32::new(0),
            starts: AtomicU32::new(0),
            resets: AtomicU32::new(0),
            flushes: AtomicU32::new(0),
            callbacks: AtomicU32::new(0),
            blocks: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
            read_calls: AtomicU32::new(0),
            largest_block: AtomicU32::new(0),
            allowed_block: AtomicU32::new(0),
            unsorted: AtomicBool::new(false),
            late: AtomicBool::new(false),
            bad_cookie: AtomicBool::new(false),
            gain: AtomicU64::new(1.0f64.to_bits()),
            mode: AtomicU64::new(1.0f64.to_bits()),
            main_in_process: AtomicBool::new(false),
            audio_in_process: AtomicBool::new(false),
            main_in_flush: AtomicBool::new(false),
            audio_in_flush: AtomicBool::new(false),
            main_on_main: AtomicBool::new(false),
            audio_on_main: AtomicBool::new(true),
            main_in_worker: AtomicBool::new(true),
            audio_in_worker: AtomicBool::new(true),
            transport_flags: AtomicU32::new(0),
            tempo: AtomicU64::new(0),
            beats: AtomicI64::new(0),
            steady: AtomicI64::new(-1),
            inputs_given: AtomicU32::new(u32::MAX),
            outputs_given: AtomicU32::new(u32::MAX),
            side_level: AtomicU32::new(0),
            seen: [const { AtomicU64::new(0) }; 64],
            seen_len: AtomicUsize::new(0),
            loaded: Mutex::new(Vec::new()),
            host_text: Mutex::new(String::new()),
            rescan_flags_supported: AtomicU32::new(0),
            dialects: AtomicU32::new(0),
            unknown_extension_is_null: AtomicBool::new(false),
        })
    }

    fn tick(&self) -> u32 {
        self.clock.fetch_add(1, Relaxed)
    }

    /// Records an event the plugin received. Does not allocate.
    fn saw(&self, frame: i64, type_: u16, detail: u16) {
        let index = self.seen_len.fetch_add(1, Relaxed);
        if let Some(slot) = self.seen.get(index) {
            let packed = ((frame as u64) << 32) | (u64::from(type_) << 16) | u64::from(detail);
            slot.store(packed, Relaxed);
        }
    }

    /// The events received since the last call, as `(frame, type, detail)`.
    fn take_seen(&self) -> Vec<(u32, u16, u16)> {
        let count = self.seen_len.swap(0, Relaxed).min(self.seen.len());
        self.seen[..count]
            .iter()
            .map(|slot| {
                let packed = slot.load(Relaxed);
                ((packed >> 32) as u32, (packed >> 16) as u16, packed as u16)
            })
            .collect()
    }

    fn gain(&self) -> f64 {
        f64::from_bits(self.gain.load(Relaxed))
    }

    /// The host, as the plugin holds it, for a test to call the way a
    /// plugin's own threads would.
    fn host(&self) -> *const clap_host {
        self.host.load(Relaxed)
    }
}

fn yes(flag: &AtomicBool) -> bool {
    flag.load(Relaxed)
}

// --- The entry point and the factory ---------------------------------------

thread_local! {
    /// The script the next library loaded on this thread will follow. An
    /// entry point's functions take no context, so this is how a test
    /// reaches them. Tests run one to a thread.
    static NEXT_SCRIPT: RefCell<Option<Arc<Script>>> = const { RefCell::new(None) };
    static INITS: Cell<u32> = const { Cell::new(0) };
    static DEINITS: Cell<u32> = const { Cell::new(0) };
    static PATH_GIVEN: RefCell<String> = const { RefCell::new(String::new()) };
}

unsafe extern "C" fn entry_init(path: *const c_char) -> bool {
    let wanted = NEXT_SCRIPT.with(|next| {
        next.borrow()
            .as_ref()
            .is_some_and(|script| yes(&script.library_init))
    });
    if wanted {
        INITS.with(|count| count.set(count.get() + 1));
        let path = unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned();
        PATH_GIVEN.with(|given| *given.borrow_mut() = path);
    }
    wanted
}

unsafe extern "C" fn entry_deinit() {
    DEINITS.with(|count| count.set(count.get() + 1));
}

#[repr(C)]
struct Factory {
    table: clap_plugin_factory,
    script: Arc<Script>,
    descriptors: Vec<*const clap_plugin_descriptor>,
}

fn leak(text: &str) -> *const c_char {
    CString::new(text).unwrap().into_raw()
}

fn describe(
    id: Option<&str>,
    name: Option<&str>,
    version: clap_version,
    features: Option<usize>,
) -> *const clap_plugin_descriptor {
    let text = |text: Option<&str>| text.map_or(ptr::null(), leak);
    let features = features.map_or(ptr::null(), |count| {
        let mut list: Vec<*const c_char> = (0..count)
            .map(|index| leak(&format!("feature-{index}")))
            .collect();
        list.push(ptr::null());
        Box::leak(list.into_boxed_slice()).as_ptr()
    });
    Box::into_raw(Box::new(clap_plugin_descriptor {
        clap_version: version,
        id: text(id),
        name: text(name),
        vendor: text(name),
        url: ptr::null(),
        manual_url: ptr::null(),
        support_url: ptr::null(),
        version: text(name.map(|_| "1.0")),
        description: text(name),
        features,
    }))
}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } != CLAP_PLUGIN_FACTORY_ID {
        return ptr::null();
    }
    let script = NEXT_SCRIPT.with(|next| next.borrow().clone().unwrap());
    if !yes(&script.factory) {
        return ptr::null();
    }
    let old = clap_version {
        major: 0,
        minor: 9,
        revision: 0,
    };
    let long = "n".repeat(10_000);
    let descriptors = vec![
        describe(Some("test.fake"), Some("Fake"), CLAP_VERSION, Some(2)),
        ptr::null(),
        // Nothing but an id.
        describe(Some("test.bare"), None, CLAP_VERSION, None),
        // No id, an id already taken, and a version this host cannot use.
        describe(Some(""), Some("Nameless"), CLAP_VERSION, Some(0)),
        describe(Some("test.fake"), Some("Twin"), CLAP_VERSION, Some(0)),
        describe(Some("test.old"), Some("Old"), old, Some(0)),
        // More text and more features than anyone needs.
        describe(Some("test.long"), Some(&long), CLAP_VERSION, Some(100)),
    ];
    Box::into_raw(Box::new(Factory {
        table: clap_plugin_factory {
            get_plugin_count: Some(factory_count),
            get_plugin_descriptor: Some(factory_descriptor),
            create_plugin: Some(factory_create),
        },
        script,
        descriptors,
    }))
    .cast()
}

unsafe extern "C" fn factory_count(factory: *const clap_plugin_factory) -> u32 {
    let factory = unsafe { &*factory.cast::<Factory>() };
    factory.script.plugin_count.load(Relaxed)
}

unsafe extern "C" fn factory_descriptor(
    factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    let factory = unsafe { &*factory.cast::<Factory>() };
    factory.script.descriptor_calls.fetch_add(1, Relaxed);
    factory
        .descriptors
        .get(index as usize)
        .copied()
        .unwrap_or(ptr::null())
}

const GOOD_ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};

static ENTRY: clap_plugin_entry = GOOD_ENTRY;

/// Loads the fake library with `script` in charge.
fn load(script: &Arc<Script>) -> Result<ClapLibrary, ClapError> {
    NEXT_SCRIPT.with(|next| *next.borrow_mut() = Some(Arc::clone(script)));
    unsafe { ClapLibrary::from_entry(&raw const ENTRY, "/plugins/fake.clap") }
}

/// Loads the fake library and creates its plugin.
fn fresh(script: &Arc<Script>) -> ClapPlugin {
    load(script).unwrap().create("test.fake").unwrap()
}

/// Loads the fake library, creates its plugin, and takes what the host has
/// to say about the plugin's parameter list out of the way. Those remarks
/// have a test of their own.
fn host(script: &Arc<Script>) -> ClapPlugin {
    let mut plugin = fresh(script);
    let remarks = plugin.pump();
    assert_eq!(warnings(&remarks).len(), remarks.len(), "{remarks:?}");
    plugin
}

fn inits() -> (u32, u32) {
    (INITS.with(Cell::get), DEINITS.with(Cell::get))
}

// --- The plugin -------------------------------------------------------------

#[repr(C)]
struct Instance {
    table: clap_plugin,
    host: *const clap_host,
    script: Arc<Script>,
}

unsafe fn instance<'a>(plugin: *const clap_plugin) -> &'a Instance {
    unsafe { &*(*plugin).plugin_data.cast::<Instance>() }
}

/// One of the host's extensions, as the plugin gets at it.
unsafe fn host_extension<'a, T>(host: *const clap_host, id: &CStr) -> Option<&'a T> {
    unsafe {
        (*host).get_extension.unwrap()(host, id.as_ptr())
            .cast::<T>()
            .as_ref()
    }
}

unsafe extern "C" fn factory_create(
    factory: *const clap_plugin_factory,
    host: *const clap_host,
    _id: *const c_char,
) -> *const clap_plugin {
    let factory = unsafe { &*factory.cast::<Factory>() };
    let script = &factory.script;
    script.creates.fetch_add(1, Relaxed);
    script.host.store(host.cast_mut(), Relaxed);
    if !yes(&script.create) {
        return ptr::null();
    }
    let process: unsafe extern "C" fn(*const clap_plugin, *const clap_process) -> i32 = process;
    let instance = Box::into_raw(Box::new(Instance {
        table: clap_plugin {
            desc: factory.descriptors[0],
            plugin_data: ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: yes(&script.has_process).then_some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        host,
        script: Arc::clone(script),
    }));
    unsafe {
        (*instance).table.plugin_data = instance.cast();
        &raw const (*instance).table
    }
}

unsafe extern "C" fn init(plugin: *const clap_plugin) -> bool {
    yes(&unsafe { instance(plugin) }.script.plugin_init)
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    let instance = unsafe { Box::from_raw((*plugin).plugin_data.cast::<Instance>()) };
    let script = &instance.script;
    script.destroyed.fetch_add(1, Relaxed);
    script.destroyed_at.store(script.tick(), Relaxed);
    *script.destroyed_on.lock().unwrap() = Some(std::thread::current().id());
}

unsafe extern "C" fn activate(plugin: *const clap_plugin, _rate: f64, _min: u32, max: u32) -> bool {
    let this = unsafe { instance(plugin) };
    this.script.allowed_block.store(max, Relaxed);
    if yes(&this.script.activate) {
        // A plugin is to say so when activating changes its latency.
        let latency: &clap_host_latency =
            unsafe { host_extension(this.host, CLAP_EXT_LATENCY) }.unwrap();
        unsafe { latency.changed.unwrap()(this.host) };
    }
    yes(&this.script.activate)
}

unsafe extern "C" fn deactivate(plugin: *const clap_plugin) {
    let script = &unsafe { instance(plugin) }.script;
    script.deactivated_at.store(script.tick(), Relaxed);
}

unsafe extern "C" fn start_processing(plugin: *const clap_plugin) -> bool {
    let script = &unsafe { instance(plugin) }.script;
    script.starts.fetch_add(1, Relaxed);
    yes(&script.start)
}

unsafe extern "C" fn stop_processing(plugin: *const clap_plugin) {
    let script = &unsafe { instance(plugin) }.script;
    script.stopped_at.store(script.tick(), Relaxed);
}

unsafe extern "C" fn reset(plugin: *const clap_plugin) {
    unsafe { instance(plugin) }
        .script
        .resets
        .fetch_add(1, Relaxed);
}

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// Reads the events the host hands over, records them and answers some.
/// Shared by `process` and `flush`. Does not allocate.
unsafe fn take_events(
    script: &Script,
    input: *const clap_input_events,
    output: *const clap_output_events,
    start: i64,
    frames: u32,
) {
    let push = unsafe { (*output).try_push.unwrap() };
    let count = unsafe { (*input).size.unwrap()(input) };
    let mut last = 0;
    for index in 0..count {
        let event = unsafe { (*input).get.unwrap()(input, index) };
        let head = unsafe { &*event };
        script.unsorted.fetch_or(head.time < last, Relaxed);
        script.late.fetch_or(head.time >= frames, Relaxed);
        last = head.time;
        let frame = start + i64::from(head.time);
        match head.type_ {
            CLAP_EVENT_PARAM_VALUE => {
                let value = unsafe { &*event.cast::<clap_event_param_value>() };
                script.saw(frame, head.type_, value.param_id as u16);
                if value.param_id == GAIN.0 {
                    script.gain.store(value.value.to_bits(), Relaxed);
                    script.bad_cookie.fetch_or(value.cookie != COOKIE, Relaxed);
                } else if value.param_id == MODE.0 {
                    script.mode.store(value.value.to_bits(), Relaxed);
                }
            }
            CLAP_EVENT_PARAM_MOD => {
                let modulation = unsafe { &*event.cast::<clap_event_param_mod>() };
                script.saw(frame, head.type_, modulation.key as u16);
            }
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                let note = unsafe { &*event.cast::<clap_event_note>() };
                script.saw(frame, head.type_, note.key as u16);
                if head.type_ == CLAP_EVENT_NOTE_OFF {
                    // The note is over as soon as it is released.
                    let mut end = *note;
                    end.header.type_ = CLAP_EVENT_NOTE_END;
                    unsafe { push(output, (&raw const end).cast()) };
                }
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                let shape = unsafe { &*event.cast::<clap_event_note_expression>() };
                script.saw(frame, head.type_, shape.expression_id as u16);
            }
            CLAP_EVENT_MIDI => {
                let midi = unsafe { &*event.cast::<clap_event_midi>() };
                script.saw(frame, head.type_, u16::from(midi.data[0]));
                // Sent straight back out.
                unsafe { push(output, event) };
            }
            other => script.saw(frame, other, 0),
        }
    }
}

unsafe extern "C" fn process(plugin: *const clap_plugin, process: *const clap_process) -> i32 {
    let this = unsafe { instance(plugin) };
    let script = &*this.script;
    let process = unsafe { &*process };
    let frames = process.frames_count;
    script.blocks.fetch_add(1, Relaxed);
    script.largest_block.fetch_max(frames, Relaxed);
    script.steady.store(process.steady_time, Relaxed);
    script
        .inputs_given
        .store(process.audio_inputs_count, Relaxed);
    script
        .outputs_given
        .store(process.audio_outputs_count, Relaxed);

    let check: &clap_host_thread_check =
        unsafe { host_extension(this.host, CLAP_EXT_THREAD_CHECK) }.unwrap();
    script
        .main_in_process
        .store(unsafe { check.is_main_thread.unwrap()(this.host) }, Relaxed);
    script.audio_in_process.store(
        unsafe { check.is_audio_thread.unwrap()(this.host) },
        Relaxed,
    );
    if let Some(transport) = unsafe { process.transport.as_ref() } {
        script.transport_flags.store(transport.flags, Relaxed);
        script.tempo.store(transport.tempo.to_bits(), Relaxed);
        script.beats.store(transport.song_pos_beats, Relaxed);
    }

    unsafe {
        take_events(
            script,
            process.in_events,
            process.out_events,
            process.steady_time,
            frames,
        );
    }
    let push = unsafe { (*process.out_events).try_push.unwrap() };
    if yes(&script.report) {
        // Someone turns the gain knob in the plugin's window.
        let gesture = |type_| clap_event_param_gesture {
            header: header::<clap_event_param_gesture>(2, type_),
            param_id: GAIN.0,
        };
        let value = |time, value| clap_event_param_value {
            header: header::<clap_event_param_value>(time, CLAP_EVENT_PARAM_VALUE),
            param_id: GAIN.0,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        };
        let begin = gesture(CLAP_EVENT_PARAM_GESTURE_BEGIN);
        let end = gesture(CLAP_EVENT_PARAM_GESTURE_END);
        let turned = value(2, 1.5);
        // And a few things no host should act on: a value that is not a
        // number, another vendor's event, nothing at all, and an event
        // that claims to be too small to be what it says it is.
        let broken = value(3, f64::NAN);
        let mut foreign = value(3, 0.1);
        foreign.header.space_id = 1_234;
        let mut short = value(3, 0.2);
        short.header.size = 8;
        // A time beyond the block: kept, on the block's last frame.
        let late = value(1_000_000, 1.75);
        unsafe {
            push(process.out_events, (&raw const begin).cast());
            push(process.out_events, (&raw const turned).cast());
            push(process.out_events, (&raw const broken).cast());
            push(process.out_events, (&raw const foreign).cast());
            push(process.out_events, ptr::null());
            push(process.out_events, (&raw const short).cast());
            push(process.out_events, (&raw const late).cast());
            push(process.out_events, (&raw const end).cast());
        }
        script.gain.store(1.75f64.to_bits(), Relaxed);
    }
    if yes(&script.chatty) {
        let log: &clap_host_log = unsafe { host_extension(this.host, CLAP_EXT_LOG) }.unwrap();
        let tail: &clap_host_tail = unsafe { host_extension(this.host, CLAP_EXT_TAIL) }.unwrap();
        unsafe {
            log.log.unwrap()(
                this.host,
                CLAP_LOG_WARNING,
                c"said while processing".as_ptr(),
            );
            tail.changed.unwrap()(this.host);
            (*this.host).request_restart.unwrap()(this.host);
            (*this.host).request_process.unwrap()(this.host);
            (*this.host).request_callback.unwrap()(this.host);
        }
    }

    // The sound: the main input times the gain, or the gain alone for a
    // plugin with no input.
    let gain = script.gain() as f32;
    let inputs = unsafe { ports(process.audio_inputs, process.audio_inputs_count) };
    let outputs = unsafe { ports(process.audio_outputs, process.audio_outputs_count) };
    if let Some(side) = inputs.get(1) {
        let level = unsafe { channel(side, 0, frames) }.first().copied();
        script
            .side_level
            .store(level.unwrap_or(0.0).to_bits(), Relaxed);
    }
    if let Some(output) = outputs.first() {
        for index in 0..output.channel_count {
            let target = unsafe { channel(output, index, frames) };
            match inputs.first().filter(|input| index < input.channel_count) {
                Some(input) => {
                    let source = unsafe { channel(input, index, frames) };
                    for (out, &sample) in target.iter_mut().zip(source.iter()) {
                        *out = sample * gain;
                    }
                }
                None => target.fill(gain),
            }
        }
        if yes(&script.constant_right) && output.channel_count > 1 {
            // One value for the whole channel: only the first sample is
            // written, and the mask says so. The rest is deliberately
            // left as something else.
            let right = unsafe { channel(output, 1, frames) };
            right.fill(9.0);
            right[0] = 0.25;
            unsafe { (*process.audio_outputs).constant_mask = 1 << 1 };
        }
    }
    script.status.load(Relaxed)
}

unsafe fn ports<'a>(first: *const clap_audio_buffer, count: u32) -> &'a [clap_audio_buffer] {
    if first.is_null() || count == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(first, count as usize) }
}

#[allow(clippy::mut_from_ref)]
unsafe fn channel<'a>(port: &clap_audio_buffer, index: u32, frames: u32) -> &'a mut [f32] {
    unsafe { std::slice::from_raw_parts_mut(*port.data32.add(index as usize), frames as usize) }
}

unsafe extern "C" fn get_extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    static PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(params_count),
        get_info: Some(params_get_info),
        get_value: Some(params_get_value),
        value_to_text: Some(params_value_to_text),
        text_to_value: Some(params_text_to_value),
        flush: Some(params_flush),
    };
    static STATE: clap_plugin_state = clap_plugin_state {
        save: Some(state_save),
        load: Some(state_load),
    };
    static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
        count: Some(audio_ports_count),
        get: Some(audio_ports_get),
    };
    static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
        count: Some(note_ports_count),
        get: Some(note_ports_get),
    };
    static LATENCY: clap_plugin_latency = clap_plugin_latency {
        get: Some(latency_get),
    };
    static TAIL: clap_plugin_tail = clap_plugin_tail {
        get: Some(tail_get),
    };
    if !yes(&unsafe { instance(plugin) }.script.extensions) {
        return ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_PARAMS {
        ptr::from_ref(&PARAMS).cast()
    } else if id == CLAP_EXT_STATE {
        ptr::from_ref(&STATE).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        ptr::from_ref(&AUDIO_PORTS).cast()
    } else if id == CLAP_EXT_NOTE_PORTS {
        ptr::from_ref(&NOTE_PORTS).cast()
    } else if id == CLAP_EXT_LATENCY {
        ptr::from_ref(&LATENCY).cast()
    } else if id == CLAP_EXT_TAIL {
        ptr::from_ref(&TAIL).cast()
    } else {
        ptr::null()
    }
}

/// Everything a plugin may ask of its host from the main thread.
unsafe extern "C" fn on_main_thread(plugin: *const clap_plugin) {
    let this = unsafe { instance(plugin) };
    let (script, host) = (&*this.script, this.host);
    script.callbacks.fetch_add(1, Relaxed);
    let asks = script.asks.swap(0, Relaxed);
    let has = |bit: u32| asks & bit != 0;
    unsafe {
        let params: &clap_host_params = host_extension(host, CLAP_EXT_PARAMS).unwrap();
        let state: &clap_host_state = host_extension(host, CLAP_EXT_STATE).unwrap();
        let latency: &clap_host_latency = host_extension(host, CLAP_EXT_LATENCY).unwrap();
        let audio_ports: &clap_host_audio_ports =
            host_extension(host, CLAP_EXT_AUDIO_PORTS).unwrap();
        let note_ports: &clap_host_note_ports = host_extension(host, CLAP_EXT_NOTE_PORTS).unwrap();
        let log: &clap_host_log = host_extension(host, CLAP_EXT_LOG).unwrap();
        let check: &clap_host_thread_check = host_extension(host, CLAP_EXT_THREAD_CHECK).unwrap();

        if has(ask::RESCAN_VALUES) {
            params.rescan.unwrap()(host, CLAP_PARAM_RESCAN_VALUES | CLAP_PARAM_RESCAN_TEXT);
        }
        if has(ask::RESCAN_ALL) {
            params.rescan.unwrap()(host, CLAP_PARAM_RESCAN_ALL);
        }
        if has(ask::DIRTY) {
            state.mark_dirty.unwrap()(host);
        }
        if has(ask::LATENCY) {
            latency.changed.unwrap()(host);
        }
        if has(ask::PORT_NAMES) {
            audio_ports.rescan.unwrap()(host, CLAP_AUDIO_PORTS_RESCAN_NAMES);
        }
        if has(ask::PORT_LIST) {
            audio_ports.rescan.unwrap()(host, CLAP_AUDIO_PORTS_RESCAN_LIST);
        }
        if has(ask::NOTE_PORTS) {
            note_ports.rescan.unwrap()(host, CLAP_NOTE_PORTS_RESCAN_ALL);
        }
        if has(ask::CLEAR) {
            params.clear.unwrap()(host, GAIN.0, CLAP_PARAM_CLEAR_AUTOMATIONS);
        }
        if has(ask::FLUSH) {
            params.request_flush.unwrap()(host);
        }
        if has(ask::LOG) {
            let long = CString::new("z".repeat(5_000)).unwrap();
            log.log.unwrap()(host, CLAP_LOG_INFO, c"hello from the plugin".as_ptr());
            log.log.unwrap()(host, CLAP_LOG_HOST_MISBEHAVING, c"I blame you".as_ptr());
            log.log.unwrap()(host, 77, ptr::null());
            log.log.unwrap()(host, CLAP_LOG_DEBUG, long.as_ptr());
        }
        if has(ask::LOOK_AROUND) {
            script
                .main_on_main
                .store(check.is_main_thread.unwrap()(host), Relaxed);
            script
                .audio_on_main
                .store(check.is_audio_thread.unwrap()(host), Relaxed);
            let text = |pointer| CStr::from_ptr(pointer).to_string_lossy().into_owned();
            *script.host_text.lock().unwrap() = format!(
                "{} | {} | {} | {} | CLAP {}",
                text((*host).name),
                text((*host).vendor),
                text((*host).url),
                text((*host).version),
                (*host).clap_version.major
            );
            let supported = (0..32)
                .filter(|bit| audio_ports.is_rescan_flag_supported.unwrap()(host, 1 << bit))
                .fold(0u32, |all, bit| all | 1 << bit);
            script.rescan_flags_supported.store(supported, Relaxed);
            script
                .dialects
                .store(note_ports.supported_dialects.unwrap()(host), Relaxed);
            let unknown = (*host).get_extension.unwrap()(host, c"clap.no-such-thing".as_ptr());
            let prefix = (*host).get_extension.unwrap()(host, c"clap.param".as_ptr());
            let empty = (*host).get_extension.unwrap()(host, c"".as_ptr());
            let null = (*host).get_extension.unwrap()(host, ptr::null());
            let all_null =
                unknown.is_null() && prefix.is_null() && empty.is_null() && null.is_null();
            script.unknown_extension_is_null.store(all_null, Relaxed);
        }
        if has(ask::FROM_A_WORKER) {
            // A plugin's own thread. It may log and ask for a callback;
            // the rest belongs to the main thread, and it calls it anyway.
            let host = host as usize;
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let host = host as *const clap_host;
                    params.rescan.unwrap()(host, CLAP_PARAM_RESCAN_ALL);
                    params.clear.unwrap()(host, GAIN.0, CLAP_PARAM_CLEAR_ALL);
                    state.mark_dirty.unwrap()(host);
                    latency.changed.unwrap()(host);
                    audio_ports.rescan.unwrap()(host, CLAP_AUDIO_PORTS_RESCAN_LIST);
                    note_ports.rescan.unwrap()(host, CLAP_NOTE_PORTS_RESCAN_ALL);
                    log.log.unwrap()(host, CLAP_LOG_ERROR, c"from a worker".as_ptr());
                    script
                        .main_in_worker
                        .store(check.is_main_thread.unwrap()(host), Relaxed);
                    script
                        .audio_in_worker
                        .store(check.is_audio_thread.unwrap()(host), Relaxed);
                });
            });
        }
    }
}

// --- The plugin's parameters -----------------------------------------------

unsafe extern "C" fn params_count(plugin: *const clap_plugin) -> u32 {
    unsafe { instance(plugin) }.script.param_count.load(Relaxed)
}

unsafe extern "C" fn params_get_info(
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    let script = &unsafe { instance(plugin) }.script;
    script.info_calls.fetch_add(1, Relaxed);
    let info = unsafe { &mut *info };
    let name = |field: &mut [c_char], text: &str| {
        for (slot, byte) in field.iter_mut().zip(text.bytes()) {
            *slot = byte as c_char;
        }
    };
    match index {
        0 => {
            info.id = GAIN.0;
            info.cookie = COOKIE;
            info.flags = CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_MODULATABLE;
            name(&mut info.name, "Gain");
            (info.min_value, info.max_value, info.default_value) = (0.0, 2.0, 1.0);
        }
        1 => {
            info.id = MODE.0;
            info.flags = CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM | CLAP_PARAM_IS_HIDDEN;
            name(&mut info.name, "Mode");
            name(&mut info.module, "Group/Sub");
            (info.min_value, info.max_value, info.default_value) = (0.0, 3.0, 1.0);
        }
        2 => {
            // A range that is not a range.
            info.id = 12;
            name(&mut info.name, "Broken");
            (info.min_value, info.max_value, info.default_value) = (f64::NAN, 1.0, 0.0);
        }
        3 => {
            // An id that is already taken.
            info.id = GAIN.0;
            name(&mut info.name, "Second gain");
            (info.min_value, info.max_value, info.default_value) = (0.0, 9.0, 3.0);
        }
        4 => {
            // No terminator anywhere, and a default outside the range.
            info.id = WILD.0;
            info.flags = CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_BYPASS | CLAP_PARAM_IS_PERIODIC;
            info.name.fill(b'x' as c_char);
            info.module.fill(b'm' as c_char);
            (info.min_value, info.max_value, info.default_value) = (-1.0, 1.0, 99.0);
        }
        _ => return false,
    }
    true
}

unsafe extern "C" fn params_get_value(plugin: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    let script = &unsafe { instance(plugin) }.script;
    let value = match id {
        10 if yes(&script.wild) => f64::NAN,
        10 => script.gain(),
        11 => f64::from_bits(script.mode.load(Relaxed)),
        13 => f64::INFINITY,
        _ => return false,
    };
    unsafe { *out = value };
    true
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const clap_plugin,
    _id: u32,
    value: f64,
    out: *mut c_char,
    capacity: u32,
) -> bool {
    let out = unsafe { std::slice::from_raw_parts_mut(out, capacity as usize) };
    if yes(&unsafe { instance(plugin) }.script.wild) {
        // Every byte it was offered, and no terminator.
        out.fill(b'y' as c_char);
        return true;
    }
    let text = format!("{value:.2} units\0");
    for (slot, byte) in out.iter_mut().zip(text.bytes()) {
        *slot = byte as c_char;
    }
    true
}

unsafe extern "C" fn params_text_to_value(
    _plugin: *const clap_plugin,
    _id: u32,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let number = text.split_whitespace().next().unwrap_or("");
    // "nan" parses, to something that is not a number.
    match number.parse::<f64>() {
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
    let this = unsafe { instance(plugin) };
    let script = &*this.script;
    script.flushes.fetch_add(1, Relaxed);
    let check: &clap_host_thread_check =
        unsafe { host_extension(this.host, CLAP_EXT_THREAD_CHECK) }.unwrap();
    script
        .main_in_flush
        .store(unsafe { check.is_main_thread.unwrap()(this.host) }, Relaxed);
    script.audio_in_flush.store(
        unsafe { check.is_audio_thread.unwrap()(this.host) },
        Relaxed,
    );
    unsafe { take_events(script, input, output, 0, 1) };
    if yes(&script.report) {
        // The plugin moved its own mode switch.
        let moved = clap_event_param_value {
            header: header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE),
            param_id: MODE.0,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value: 2.0,
        };
        unsafe { (*output).try_push.unwrap()(output, (&raw const moved).cast()) };
    }
}

// --- The plugin's state ----------------------------------------------------

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let script = &unsafe { instance(plugin) }.script;
    let write = unsafe { (*stream).write.unwrap() };
    match script.state_mode.load(Relaxed) {
        1 => false,
        2 => {
            // Writes until it is stopped.
            let megabyte = vec![0xAB_u8; 1 << 20];
            loop {
                script.write_calls.fetch_add(1, Relaxed);
                if unsafe { write(stream, megabyte.as_ptr().cast(), megabyte.len() as u64) } <= 0 {
                    return false;
                }
            }
        }
        _ => {
            // As CLAP asks: keep offering the rest until all of it is
            // taken.
            let state = script.state.lock().unwrap().clone();
            let mut rest = state.as_slice();
            while !rest.is_empty() {
                script.write_calls.fetch_add(1, Relaxed);
                let taken = unsafe { write(stream, rest.as_ptr().cast(), rest.len() as u64) };
                if taken <= 0 || taken as usize > rest.len() {
                    return false;
                }
                rest = &rest[taken as usize..];
            }
            // And a write of nothing, which a careless host might trip on.
            unsafe { write(stream, rest.as_ptr().cast(), 0) == 0 }
        }
    }
}

unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let this = unsafe { instance(plugin) };
    let script = &this.script;
    let read = unsafe { (*stream).read.unwrap() };
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16];
    loop {
        script.read_calls.fetch_add(1, Relaxed);
        let count = unsafe { read(stream, chunk.as_mut_ptr().cast(), chunk.len() as u64) };
        match count {
            0 => break,
            1..=16 => bytes.extend_from_slice(&chunk[..count as usize]),
            _ => return false,
        }
    }
    // Reading past the end stays at the end.
    if unsafe { read(stream, chunk.as_mut_ptr().cast(), 16) } != 0 {
        return false;
    }
    *script.loaded.lock().unwrap() = bytes;
    if script.state_mode.load(Relaxed) == 1 {
        return false;
    }
    let params: &clap_host_params = unsafe { host_extension(this.host, CLAP_EXT_PARAMS) }.unwrap();
    unsafe { params.rescan.unwrap()(this.host, CLAP_PARAM_RESCAN_VALUES) };
    true
}

// --- The plugin's ports, latency and tail ----------------------------------

unsafe extern "C" fn audio_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    match (
        unsafe { instance(plugin) }.script.layout.load(Relaxed),
        is_input,
    ) {
        // A stereo input with a mono sidechain, a stereo output with a
        // mono extra.
        (0, _) => 2,
        // More ports than any host serves.
        (1, true) => 100,
        (1, false) => 1,
        // One port with a thousand channels.
        (2, _) => 1,
        // No audio at all.
        (3, _) => 0,
        // Two ports counted, one described.
        (4, _) => 2,
        // An instrument whose only output is not flagged as the main one.
        (5, true) => 0,
        (5, false) => 1,
        // As many ports as are served, each as wide as is served.
        (6, _) => 32,
        _ => 0,
    }
}

unsafe extern "C" fn audio_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    let layout = unsafe { instance(plugin) }.script.layout.load(Relaxed);
    let info = unsafe { &mut *info };
    if layout == 4 && index == 1 {
        return false;
    }
    let main = index == 0 && layout != 5;
    info.id = if is_input { 100 } else { 200 } + index;
    info.flags = if main { CLAP_AUDIO_PORT_IS_MAIN } else { 0 };
    info.channel_count = match (layout, main) {
        (2, _) => 1_000,
        (6, _) => 64,
        (_, true) => 2,
        (5, false) => 2,
        (_, false) => 1,
    };
    info.port_type = match info.channel_count {
        2 => CLAP_PORT_STEREO.as_ptr(),
        1 => CLAP_PORT_MONO.as_ptr(),
        _ => ptr::null(),
    };
    info.in_place_pair = if main {
        if is_input { 200 } else { 100 }
    } else {
        CLAP_INVALID_ID
    };
    if main {
        for (slot, byte) in info.name.iter_mut().zip(b"Main\0") {
            *slot = *byte as c_char;
        }
    } else {
        // No terminator.
        info.name.fill(b'p' as c_char);
    }
    true
}

unsafe extern "C" fn note_ports_count(_plugin: *const clap_plugin, _is_input: bool) -> u32 {
    1
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    _is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if index != 0 {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 7;
    info.supported_dialects = if yes(&unsafe { instance(plugin) }.script.midi_only) {
        CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE
    } else {
        CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
    };
    info.preferred_dialect = CLAP_NOTE_DIALECT_MIDI;
    info.name.fill(b'k' as c_char);
    true
}

unsafe extern "C" fn latency_get(plugin: *const clap_plugin) -> u32 {
    unsafe { instance(plugin) }.script.latency.load(Relaxed)
}

unsafe extern "C" fn tail_get(plugin: *const clap_plugin) -> u32 {
    unsafe { instance(plugin) }.script.tail.load(Relaxed)
}

// --- Helpers for the tests --------------------------------------------------

fn filled(channels: usize, frames: usize, value: f32) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(channels, frames);
    for channel in buffer.iter_channels_mut() {
        channel.fill(value);
    }
    buffer
}

fn events(list: &[(u32, EventKind)]) -> EventBuffer {
    let mut buffer = EventBuffer::with_capacity(list.len().max(1));
    for &(offset, kind) in list {
        buffer.push(Event { offset, kind }).unwrap();
    }
    buffer
}

fn warnings(pumped: &[HostEvent]) -> Vec<&str> {
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

// --- Loading and creating ---------------------------------------------------

#[test]
fn a_library_that_lies_about_its_contents_is_listed_within_limits() {
    let script = Script::new();
    script.plugin_count.store(u32::MAX, Relaxed);
    let before = inits();
    let library = load(&script).unwrap();
    assert_eq!(inits(), (before.0 + 1, before.1), "init runs once");
    assert_eq!(
        PATH_GIVEN.with(|path| path.borrow().clone()),
        "/plugins/fake.clap"
    );
    assert_eq!(library.path(), std::path::Path::new("/plugins/fake.clap"));

    // It claimed four thousand million plugins and was asked about 1,024.
    assert_eq!(
        script.descriptor_calls.load(Relaxed) as usize,
        MAX_PLUGINS_PER_LIBRARY
    );
    let ids: Vec<&str> = library
        .descriptors()
        .iter()
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(ids, ["test.fake", "test.bare", "test.long"]);

    let fake = library.descriptor("test.fake").unwrap();
    assert_eq!((fake.name.as_str(), fake.version.as_str()), ("Fake", "1.0"));
    assert_eq!(fake.features, ["feature-0", "feature-1"]);
    assert_eq!(fake.url, "");
    // Null where text should be: empty text.
    let bare = library.descriptor("test.bare").unwrap();
    assert_eq!((bare.name.as_str(), bare.vendor.as_str()), ("", ""));
    assert!(bare.features.is_empty());
    // Too much of everything: cut.
    let long = library.descriptor("test.long").unwrap();
    assert_eq!(long.name.len(), MAX_TEXT_BYTES);
    assert_eq!(long.features.len(), MAX_FEATURES);
    assert!(library.descriptor("test.old").is_none());
    assert!(library.descriptor("").is_none());

    // A plugin keeps its library loaded, and `deinit` waits for it.
    let plugin = library.create("test.fake").unwrap();
    drop(library);
    assert_eq!(inits(), (before.0 + 1, before.1));
    drop(plugin);
    assert_eq!(script.destroyed.load(Relaxed), 1);
    assert_eq!(inits(), (before.0 + 1, before.1 + 1), "deinit runs once");
}

#[test]
fn an_entry_point_that_is_wrong_is_refused() {
    let before = inits();
    let script = Script::new();
    NEXT_SCRIPT.with(|next| *next.borrow_mut() = Some(Arc::clone(&script)));

    let null = unsafe { ClapLibrary::from_entry(ptr::null(), "x") };
    assert_eq!(null.unwrap_err(), ClapError::MalformedEntry);

    static OLD: clap_plugin_entry = clap_plugin_entry {
        clap_version: clap_version {
            major: 0,
            minor: 9,
            revision: 3,
        },
        ..GOOD_ENTRY
    };
    let old = unsafe { ClapLibrary::from_entry(&raw const OLD, "x") };
    assert_eq!(
        old.unwrap_err(),
        ClapError::IncompatibleVersion {
            major: 0,
            minor: 9,
            revision: 3
        }
    );

    static HOLLOW: clap_plugin_entry = clap_plugin_entry {
        get_factory: None,
        ..GOOD_ENTRY
    };
    let hollow = unsafe { ClapLibrary::from_entry(&raw const HOLLOW, "x") };
    assert_eq!(hollow.unwrap_err(), ClapError::MalformedEntry);
    assert_eq!(inits(), before, "nothing was initialised so far");

    // A refused `init` must not be followed by `deinit`.
    script.library_init.store(false, Relaxed);
    assert_eq!(load(&script).unwrap_err(), ClapError::InitFailed);
    assert_eq!(inits(), before);

    // No factory: initialised, found empty, and put away again.
    script.library_init.store(true, Relaxed);
    script.factory.store(false, Relaxed);
    assert_eq!(load(&script).unwrap_err(), ClapError::NoFactory);
    assert_eq!(inits(), (before.0 + 1, before.1 + 1));

    // A factory with nothing in it is a library with no plugins.
    script.factory.store(true, Relaxed);
    script.plugin_count.store(0, Relaxed);
    let empty = load(&script).unwrap();
    assert!(empty.descriptors().is_empty());
    assert_eq!(
        empty.create("test.fake").unwrap_err(),
        ClapError::UnknownPlugin("test.fake".to_owned())
    );
    assert_eq!(
        script.creates.load(Relaxed),
        0,
        "the factory was asked anyway"
    );
}

#[test]
fn a_plugin_that_cannot_be_made_is_an_error() {
    let script = Script::new();
    let library = load(&script).unwrap();

    script.create.store(false, Relaxed);
    assert_eq!(
        library.create("test.fake").unwrap_err(),
        ClapError::CreateFailed("test.fake".to_owned())
    );
    script.create.store(true, Relaxed);

    // A plugin that will not initialise is destroyed, as CLAP requires.
    script.plugin_init.store(false, Relaxed);
    assert_eq!(
        library.create("test.fake").unwrap_err(),
        ClapError::PluginInitFailed
    );
    assert_eq!(script.destroyed.load(Relaxed), 1);
    script.plugin_init.store(true, Relaxed);

    // So is one that has no way to process.
    script.has_process.store(false, Relaxed);
    assert_eq!(
        library.create("test.fake").unwrap_err(),
        ClapError::MalformedPlugin("process")
    );
    assert_eq!(script.destroyed.load(Relaxed), 2);
    script.has_process.store(true, Relaxed);

    // One with no extensions at all is a plugin with nothing to show, and
    // still a plugin.
    script.extensions.store(false, Relaxed);
    let mut plain = library.create("test.fake").unwrap();
    assert!(plain.params().is_empty() && plain.audio_outputs().is_empty());
    assert_eq!((plain.latency(), plain.tail()), (0, None));
    assert!(matches!(plain.save_state(), Err(ClapError::Unsupported(_))));
    assert!(matches!(
        plain.load_state(b"x"),
        Err(ClapError::Unsupported(_))
    ));
    assert!(matches!(plain.flush(), Err(ClapError::Unsupported(_))));
    assert_eq!(
        plain.set_param(GAIN, 1.0),
        Err(ClapError::UnknownParam(GAIN))
    );
    // It has no ports, so what goes in comes out as silence.
    let mut processor = plain.activate(48_000.0, 32).unwrap();
    let mut block = filled(2, 32, 0.5);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!((heap, block.peak()), (0, 0.0));
    assert_eq!(processor.status(), ProcessStatus::Continue);
    assert_eq!((processor.latency_samples(), processor.tail()), (0, None));
    assert_eq!(
        (
            script.inputs_given.load(Relaxed),
            script.outputs_given.load(Relaxed)
        ),
        (0, 0)
    );
}

// --- Parameters -------------------------------------------------------------

#[test]
fn parameters_are_listed_within_limits_and_read_with_care() {
    let script = Script::new();
    let mut plugin = fresh(&script);

    // Five were declared. One has a range that is not a pair of numbers
    // and one repeats an id; three are left.
    let ids: Vec<ParamId> = plugin.params().iter().map(|param| param.id).collect();
    assert_eq!(ids, [GAIN, MODE, WILD]);
    assert_eq!(
        warnings(&plugin.pump()),
        [
            "2 parameter(s) were left out: no description, a range that is not two numbers in \
          order, or an id already taken"
        ]
    );

    let gain = plugin.param(GAIN).unwrap();
    assert_eq!(
        (gain.name.as_str(), gain.min, gain.max, gain.default),
        ("Gain", 0.0, 2.0, 1.0)
    );
    assert!(gain.flags.automatable && gain.flags.modulatable && !gain.flags.stepped);
    let mode = plugin.param(MODE).unwrap();
    assert_eq!(mode.module, "Group/Sub");
    assert!(mode.flags.stepped && mode.flags.enumerated && mode.flags.hidden);
    assert_eq!(
        mode.flags.bits,
        CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM | CLAP_PARAM_IS_HIDDEN
    );
    // Fields with no terminator are read to their end and no further, and
    // a default outside the range is brought inside it.
    let wild = plugin.param(WILD).unwrap();
    assert_eq!(wild.name, "x".repeat(256));
    assert_eq!(wild.module, "m".repeat(1_024));
    assert_eq!((wild.min, wild.max, wild.default), (-1.0, 1.0, 1.0));
    assert!(wild.flags.read_only && wild.flags.bypass && wild.flags.periodic);

    // Values and text.
    assert_eq!(plugin.param_value(GAIN), Some(1.0));
    assert_eq!(plugin.param_value(WILD), None, "infinity is not a value");
    assert_eq!(plugin.param_value(ParamId(12)), None, "a refused parameter");
    assert_eq!(
        plugin.param_to_text(GAIN, 0.5).as_deref(),
        Some("0.50 units")
    );
    assert_eq!(plugin.text_to_param(GAIN, "1.25 units"), Some(1.25));
    assert_eq!(plugin.text_to_param(GAIN, "loud"), None);
    assert_eq!(plugin.text_to_param(GAIN, "nan"), None);
    assert_eq!(plugin.text_to_param(GAIN, "inf"), None);
    script.wild.store(true, Relaxed);
    assert_eq!(plugin.param_value(GAIN), None);
    // The plugin filled all it was given and added no terminator.
    assert_eq!(plugin.param_to_text(GAIN, 0.5), Some("y".repeat(255)));
    script.wild.store(false, Relaxed);

    // Setting a value while inactive is a flush on the main thread, with
    // the cookie the plugin asked for, and what the plugin says in return
    // comes out of the next pump.
    script.report.store(true, Relaxed);
    plugin.set_param(GAIN, 5.0).unwrap();
    assert_eq!(script.gain(), 2.0, "held to the declared range");
    assert_eq!(plugin.param_value(GAIN), Some(2.0));
    assert!(!yes(&script.bad_cookie));
    assert_eq!(script.flushes.load(Relaxed), 1);
    assert!(yes(&script.main_in_flush) && !yes(&script.audio_in_flush));
    assert_eq!(
        script.take_seen(),
        [(0, CLAP_EVENT_PARAM_VALUE, GAIN.0 as u16)]
    );
    assert_eq!(
        plugin.pump(),
        [HostEvent::ParamValue {
            param: MODE,
            value: 2.0
        }]
    );
    assert_eq!(
        plugin.set_param(ParamId(12), 0.0),
        Err(ClapError::UnknownParam(ParamId(12)))
    );

    // A plugin that claims a million parameters is asked about 65,536.
    let greedy = Script::new();
    greedy.param_count.store(1_000_000, Relaxed);
    let mut plugin = fresh(&greedy);
    assert_eq!(greedy.info_calls.load(Relaxed) as usize, MAX_PARAMS);
    assert_eq!(plugin.params().len(), 3);
    let pumped = plugin.pump();
    let notes = warnings(&pumped);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!(
        notes[0],
        "the plugin declares 1000000 parameters; the first 65536 are used"
    );
    assert!(notes[1].starts_with("65533 parameter(s) were left out"));
}

// --- Processing -------------------------------------------------------------

#[test]
fn events_cross_in_both_directions_on_their_frames() {
    let script = Script::new();
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    assert_eq!(script.allowed_block.load(Relaxed), 64);

    let note = NoteRef::with_id(2, 60, 7);
    let controller = MidiMessage::ControlChange {
        channel: 3,
        controller: 74,
        value: 99,
    };
    let list = events(&[
        (
            3,
            EventKind::ParamModulation {
                param: GAIN,
                amount: 0.25,
                note: Some(note),
            },
        ),
        (
            5,
            EventKind::NoteOn {
                note,
                velocity: 0.5,
            },
        ),
        (
            70,
            EventKind::ParamValue {
                param: GAIN,
                value: 0.5,
            },
        ),
        (130, EventKind::Midi(controller)),
        (
            199,
            EventKind::NoteExpression {
                note,
                expression: Expression::Pan,
                value: 1.0,
            },
        ),
        // On the frame after the last one: it belongs to the next block.
        (
            200,
            EventKind::NoteOff {
                note,
                velocity: 0.0,
            },
        ),
        // A parameter the plugin does not have is not sent to it.
        (
            10,
            EventKind::ParamValue {
                param: ParamId(999),
                value: 0.5,
            },
        ),
    ]);
    let mut transport = Transport::playing_at(97.0);
    transport.position_beats = Some(8.0);
    let sidechain = filled(1, 200, 0.125);
    let context = ProcessContext::new(&list, &transport).with_sidechain(&sidechain);

    // 200 frames for a plugin that takes 64 at a time.
    let mut block = filled(2, 200, 1.0);
    let heap = common::heap_calls_during(|| processor.process_with(&mut block, &context));
    assert_eq!(heap, 0, "the host used the heap while processing");
    assert_eq!(processor.status(), ProcessStatus::Continue);
    assert_eq!(script.blocks.load(Relaxed), 4);
    assert_eq!(script.largest_block.load(Relaxed), 64);
    assert_eq!(script.starts.load(Relaxed), 1);
    assert!(!yes(&script.unsorted) && !yes(&script.late) && !yes(&script.bad_cookie));

    // Each event on its own frame, whichever piece it fell in.
    assert_eq!(
        script.take_seen(),
        [
            (3, CLAP_EVENT_PARAM_MOD, 60),
            (5, CLAP_EVENT_NOTE_ON, 60),
            (70, CLAP_EVENT_PARAM_VALUE, GAIN.0 as u16),
            (130, CLAP_EVENT_MIDI, 0xB3),
            (
                199,
                CLAP_EVENT_NOTE_EXPRESSION,
                CLAP_NOTE_EXPRESSION_PAN as u16
            ),
        ]
    );
    // The gain took effect in the piece its event was in.
    assert!(block.channel(0)[..64].iter().all(|&sample| sample == 1.0));
    assert!(block.channel(1)[64..].iter().all(|&sample| sample == 0.5));
    // What the plugin sent back is on the frame it sent it on, counted
    // from the start of the caller's block.
    assert_eq!(
        processor.output_events(),
        [OutputEvent {
            offset: 130,
            kind: OutputKind::Event(EventKind::Midi(controller))
        }]
    );
    // The plugin was shown both inputs and both outputs, and the sidechain
    // was in the second input.
    assert_eq!(
        (
            script.inputs_given.load(Relaxed),
            script.outputs_given.load(Relaxed)
        ),
        (2, 2)
    );
    assert_eq!(f32::from_bits(script.side_level.load(Relaxed)), 0.125);
    // The transport of the last piece, 192 frames on.
    let flags = script.transport_flags.load(Relaxed);
    assert_ne!(flags & CLAP_TRANSPORT_IS_PLAYING, 0);
    assert_ne!(flags & CLAP_TRANSPORT_HAS_TEMPO, 0);
    assert_eq!(f64::from_bits(script.tempo.load(Relaxed)), 97.0);
    let beats = script.beats.load(Relaxed) as f64 / CLAP_BEATTIME_FACTOR as f64;
    assert!((beats - (8.0 + 192.0 * 97.0 / (60.0 * 48_000.0))).abs() < 1e-8);
    assert_eq!(script.steady.load(Relaxed), 192);
    assert_eq!(plugin.undeliverable_events(), 1, "the unknown parameter");

    // The note-off waited, and opens the next block. The plugin answers
    // it with a note end, which carries the note's id back.
    let mut block = filled(2, 64, 1.0);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0);
    assert_eq!(script.take_seen(), [(200, CLAP_EVENT_NOTE_OFF, 60)]);
    assert_eq!(
        processor.output_events(),
        [OutputEvent {
            offset: 0,
            kind: OutputKind::NoteEnd { note }
        }]
    );

    // A block with no frames delivers nothing and loses nothing.
    let held = events(&[(
        0,
        EventKind::NoteOn {
            note,
            velocity: 1.0,
        },
    )]);
    let mut empty = AudioBuffer::<f32>::new(2, 0);
    processor.process_with(&mut empty, &ProcessContext::new(&held, &transport));
    assert_eq!(
        script.blocks.load(Relaxed),
        5,
        "the plugin was called for nothing"
    );
    processor.process(&mut block);
    assert_eq!(script.take_seen(), [(264, CLAP_EVENT_NOTE_ON, 60)]);

    // `reset` reaches the plugin, and throws away what was waiting.
    processor.process_with(&mut empty, &ProcessContext::new(&held, &transport));
    processor.reset();
    assert_eq!(script.resets.load(Relaxed), 1);
    processor.process(&mut block);
    assert!(script.take_seen().is_empty());
    assert_eq!(
        (plugin.undeliverable_events(), plugin.lost_events()),
        (1, 0)
    );
}

#[test]
fn what_the_plugin_reports_reaches_both_halves() {
    let script = Script::new();
    script.report.store(true, Relaxed);
    script.constant_right.store(true, Relaxed);
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 16).unwrap();
    assert_eq!(plugin.pump(), [HostEvent::LatencyChanged(64)]);

    // 40 frames, so three pieces: the plugin reports in each.
    let mut block = filled(2, 40, 1.0);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0);
    let value = |offset, value| OutputEvent {
        offset,
        kind: OutputKind::Event(EventKind::ParamValue { param: GAIN, value }),
    };
    let begin = |offset| OutputEvent {
        offset,
        kind: OutputKind::GestureBegin { param: GAIN },
    };
    let end = |offset| OutputEvent {
        offset,
        kind: OutputKind::GestureEnd { param: GAIN },
    };
    // Per piece: begin, the value, the value whose time was beyond the
    // piece (kept, on the piece's last frame), end. The value that is not
    // a number, the foreign event, the null and the undersized one are
    // not there.
    assert_eq!(
        processor.output_events(),
        [
            begin(2),
            value(2, 1.5),
            value(15, 1.75),
            end(2),
            begin(18),
            value(18, 1.5),
            value(31, 1.75),
            end(18),
            begin(34),
            value(34, 1.5),
            value(39, 1.75),
            end(34),
        ]
    );
    // A channel the plugin declared constant is that value throughout,
    // though it wrote only the first sample.
    assert!(block.channel(1).iter().all(|&sample| sample == 0.25));
    assert!(block.channel(0).iter().all(|&sample| sample > 0.9));

    // The same reports, in order, on the main thread.
    let pumped = plugin.pump();
    let on_main: Vec<HostEvent> = (0..3)
        .flat_map(|_| {
            [
                HostEvent::GestureBegin { param: GAIN },
                HostEvent::ParamValue {
                    param: GAIN,
                    value: 1.5,
                },
                HostEvent::ParamValue {
                    param: GAIN,
                    value: 1.75,
                },
                HostEvent::GestureEnd { param: GAIN },
            ]
        })
        .collect();
    assert_eq!(pumped, on_main);
    assert_eq!(plugin.param_value(GAIN), Some(1.75));

    // The next block starts a fresh list.
    script.report.store(false, Relaxed);
    processor.process(&mut filled(2, 8, 1.0));
    assert!(processor.output_events().is_empty());
    assert!(plugin.pump().is_empty());

    // If nobody pumps, the queue to the main thread fills, the audio
    // thread carries on, and the loss is counted.
    script.report.store(true, Relaxed);
    let mut long = filled(2, 16 * 300, 1.0);
    let heap = common::heap_calls_during(|| processor.process(&mut long));
    assert_eq!(heap, 0);
    assert_eq!(processor.output_events().len(), 1_200);
    assert_eq!(plugin.pump().len(), 1_024);
    assert_eq!(plugin.lost_events(), 1_200 - 1_024);
}

#[test]
fn notes_become_midi_for_a_plugin_that_only_takes_midi() {
    let script = Script::new();
    script.midi_only.store(true, Relaxed);
    let mut plugin = host(&script);
    let [port] = plugin.note_inputs() else {
        panic!("expected one note input");
    };
    assert!(!port.clap && port.midi && port.midi_mpe && !port.midi2);
    assert_eq!((port.id, port.name.as_str()), (7, "k".repeat(256).as_str()));

    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let note = NoteRef::with_id(4, 62, 1);
    let list = events(&[
        (
            1,
            EventKind::NoteOn {
                note,
                velocity: 1.0,
            },
        ),
        (
            2,
            EventKind::NoteExpression {
                note,
                expression: Expression::Pressure,
                value: 0.5,
            },
        ),
        // No MIDI message says this, so it is not sent.
        (
            3,
            EventKind::NoteExpression {
                note,
                expression: Expression::Tuning,
                value: 0.5,
            },
        ),
        (
            4,
            EventKind::NoteOff {
                note,
                velocity: 0.0,
            },
        ),
    ]);
    processor.process_with(
        &mut filled(2, 64, 0.0),
        &ProcessContext::new(&list, &Transport::STOPPED),
    );
    assert_eq!(
        script.take_seen(),
        [
            (1, CLAP_EVENT_MIDI, 0x94),
            (2, CLAP_EVENT_MIDI, 0xA4),
            (4, CLAP_EVENT_MIDI, 0x84),
        ]
    );
    assert_eq!(plugin.undeliverable_events(), 1);
    // The plugin echoes MIDI, which comes back as the notes it means.
    let kinds: Vec<OutputKind> = processor
        .output_events()
        .iter()
        .map(|event| event.kind)
        .collect();
    let plain = NoteRef::new(4, 62);
    assert_eq!(
        kinds,
        [
            OutputKind::Event(EventKind::NoteOn {
                note: plain,
                velocity: 1.0
            }),
            OutputKind::Event(EventKind::NoteExpression {
                note: plain,
                expression: Expression::Pressure,
                value: 64.0 / 127.0
            }),
            OutputKind::Event(EventKind::NoteOff {
                note: plain,
                velocity: 0.0
            }),
        ]
    );
}

#[test]
fn a_plugin_that_fails_leaves_silence_and_says_so() {
    let script = Script::new();
    let mut plugin = host(&script);

    // It will not activate.
    script.activate.store(false, Relaxed);
    assert_eq!(
        plugin.activate(48_000.0, 64).unwrap_err(),
        ClapError::ActivationFailed
    );
    assert!(!plugin.is_active() && !plugin.has_processor());
    script.activate.store(true, Relaxed);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();

    // It will not start: silence, and it is asked again next time.
    script.start.store(false, Relaxed);
    let mut block = filled(2, 64, 1.0);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!((heap, block.peak()), (0, 0.0));
    assert_eq!(
        processor.status(),
        ProcessStatus::Failed(ProcessFailure::StartRefused)
    );
    assert_eq!(
        script.blocks.load(Relaxed),
        0,
        "processed without having started"
    );
    script.start.store(true, Relaxed);
    let mut block = filled(2, 64, 1.0);
    processor.process(&mut block);
    assert_eq!(
        (processor.status(), block.peak()),
        (ProcessStatus::Continue, 1.0)
    );
    assert_eq!(script.starts.load(Relaxed), 2);

    // It reports an error, or something that is not a status: what it
    // wrote is thrown away.
    for bad in [CLAP_PROCESS_ERROR, 99, -1] {
        script.status.store(bad, Relaxed);
        let mut block = filled(2, 64, 1.0);
        let heap = common::heap_calls_during(|| processor.process(&mut block));
        assert_eq!((heap, block.peak()), (0, 0.0), "status {bad}");
        assert_eq!(
            processor.status(),
            ProcessStatus::Failed(ProcessFailure::Error)
        );
    }
    // The main thread hears of each kind of failure once, and can count
    // the blocks.
    script.status.store(CLAP_PROCESS_CONTINUE, Relaxed);
    assert_eq!(plugin.failed_blocks(), 4);
    assert_eq!(
        plugin.pump(),
        [
            HostEvent::LatencyChanged(64),
            HostEvent::ProcessFailed(ProcessFailure::Error),
            HostEvent::ProcessFailed(ProcessFailure::StartRefused),
        ]
    );
    assert!(plugin.pump().is_empty(), "failures were reported twice");

    // The other statuses are passed on, and the sound is kept.
    for (status, expected) in [
        (
            CLAP_PROCESS_CONTINUE_IF_NOT_QUIET,
            ProcessStatus::ContinueIfNotQuiet,
        ),
        (CLAP_PROCESS_TAIL, ProcessStatus::Tail),
        (CLAP_PROCESS_SLEEP, ProcessStatus::Sleep),
        (CLAP_PROCESS_CONTINUE, ProcessStatus::Continue),
    ] {
        script.status.store(status, Relaxed);
        let mut block = filled(2, 64, 1.0);
        processor.process(&mut block);
        assert_eq!((processor.status(), block.peak()), (expected, 1.0));
    }
    assert_eq!(plugin.failed_blocks(), 4);
}

#[test]
fn layouts_beyond_the_limits_are_refused_before_anything_is_allocated() {
    for (layout, why) in [
        (1, "more audio ports than this host serves"),
        (2, "an audio port with more channels than this host serves"),
        (4, "an audio port the plugin counts but does not describe"),
    ] {
        let script = Script::new();
        script.layout.store(layout, Relaxed);
        let mut plugin = host(&script);
        assert_eq!(
            plugin.activate(48_000.0, 64).unwrap_err(),
            ClapError::Unsupported(why),
            "layout {layout}"
        );
        assert!(!plugin.is_active());
        assert_eq!(
            script.allowed_block.load(Relaxed),
            0,
            "the plugin was activated"
        );
    }

    // Within the port limits, 4,096 channels in all, but at this block
    // size more memory than is given out.
    let script = Script::new();
    script.layout.store(6, Relaxed);
    let mut plugin = host(&script);
    assert_eq!(
        plugin.audio_inputs().len() + plugin.audio_outputs().len(),
        64
    );
    assert_eq!(
        plugin.activate(48_000.0, 8_192).unwrap_err(),
        ClapError::Unsupported("more channels at this block size than this host allocates for")
    );
    assert_eq!(script.allowed_block.load(Relaxed), 0);
    // At a size that fits, every one of those ports is served.
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = filled(2, 64, 0.5);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!((heap, block.peak()), (0, 0.5));
    assert_eq!(
        (
            script.inputs_given.load(Relaxed),
            script.outputs_given.load(Relaxed)
        ),
        (32, 32)
    );

    // The ordinary layout, read with its unterminated names.
    let script = Script::new();
    let plugin = host(&script);
    let [main, side] = plugin.audio_inputs() else {
        panic!("expected two inputs");
    };
    assert_eq!(
        (main.id, main.name.as_str(), main.channels),
        (100, "Main", 2)
    );
    assert!(main.is_main && !side.is_main);
    assert_eq!(main.in_place_pair, Some(200));
    assert_eq!(main.port_type.as_deref(), Some("stereo"));
    assert_eq!((side.id, side.channels, side.in_place_pair), (101, 1, None));
    assert_eq!(side.name, "p".repeat(256));
    assert_eq!(side.port_type.as_deref(), Some("mono"));

    // An output that is not flagged as the main one is still what is
    // heard, since it is all there is.
    let script = Script::new();
    script.layout.store(5, Relaxed);
    let mut plugin = host(&script);
    assert!(plugin.audio_inputs().is_empty());
    assert!(!plugin.audio_outputs()[0].is_main);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    let mut block = filled(3, 64, 0.0);
    processor.process(&mut block);
    assert!(
        block
            .channel(0)
            .iter()
            .chain(block.channel(1))
            .all(|&s| s == 1.0)
    );
    assert!(block.channel(2).iter().all(|&s| s == 0.0));
}

// --- The plugin's requests --------------------------------------------------

#[test]
fn the_plugins_requests_are_answered_by_pump() {
    let script = Script::new();
    let info = HostInfo {
        name: "Test Host".to_owned(),
        vendor: "Someone".to_owned(),
        url: "https://example.org".to_owned(),
        version: "9.8.7".to_owned(),
    };
    let mut plugin = load(&script)
        .unwrap()
        .create_with("test.fake", &info)
        .unwrap();
    let _ = plugin.pump();
    let host = script.host() as usize;

    // From a thread of the plugin's own, the three requests any thread may
    // make.
    std::thread::spawn(move || unsafe {
        let host = host as *const clap_host;
        (*host).request_restart.unwrap()(host);
        (*host).request_process.unwrap()(host);
        (*host).request_callback.unwrap()(host);
        (*host).request_callback.unwrap()(host);
    })
    .join()
    .unwrap();
    assert_eq!(script.callbacks.load(Relaxed), 0, "called back before pump");
    assert_eq!(
        plugin.pump(),
        [HostEvent::RestartRequested, HostEvent::ProcessRequested]
    );
    assert_eq!(script.callbacks.load(Relaxed), 1);
    assert!(plugin.pump().is_empty());
    assert_eq!(script.callbacks.load(Relaxed), 1, "called back unasked");

    // On the main thread, in its callback, the plugin uses every
    // extension. What it asks for there is answered in the same pump.
    let ask_for = |asks: u32| {
        script.asks.store(asks, Relaxed);
        let host = host as *const clap_host;
        unsafe { (*host).request_callback.unwrap()(host) };
    };
    ask_for(ask::LOOK_AROUND);
    assert!(plugin.pump().is_empty());
    assert!(yes(&script.main_on_main) && !yes(&script.audio_on_main));
    assert_eq!(
        *script.host_text.lock().unwrap(),
        "Test Host | Someone | https://example.org | 9.8.7 | CLAP 1"
    );
    // Every rescan flag CLAP defines, and no other.
    assert_eq!(script.rescan_flags_supported.load(Relaxed), 0b11_1111);
    assert_eq!(
        script.dialects.load(Relaxed),
        CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
    );
    assert!(yes(&script.unknown_extension_is_null));

    ask_for(ask::RESCAN_VALUES | ask::DIRTY | ask::PORT_NAMES | ask::NOTE_PORTS | ask::CLEAR);
    let rescan = ParamsRescan {
        values: true,
        text: true,
        ..ParamsRescan::default()
    };
    assert_eq!(
        plugin.pump(),
        [
            HostEvent::StateDirty,
            HostEvent::ParamClearRequested {
                param: GAIN,
                all: false,
                automation: true,
                modulation: false
            },
            HostEvent::ParamsRescanned(rescan),
            HostEvent::AudioPortsChanged,
            HostEvent::NotePortsChanged,
        ]
    );
    assert!(plugin.pump().is_empty());

    // A flush it asks for while inactive is done there and then.
    ask_for(ask::FLUSH);
    assert!(plugin.pump().is_empty());
    assert_eq!(script.flushes.load(Relaxed), 1);
    assert!(yes(&script.main_in_flush) && !yes(&script.audio_in_flush));

    // Log messages come out with their level, in order, cut to a length.
    ask_for(ask::LOG);
    let pumped = plugin.pump();
    let log = |severity, message: &str| HostEvent::Log {
        severity,
        message: message.to_owned(),
    };
    assert_eq!(pumped.len(), 4);
    assert_eq!(pumped[0], log(LogSeverity::Info, "hello from the plugin"));
    assert_eq!(pumped[1], log(LogSeverity::HostMisbehaving, "I blame you"));
    // A level CLAP does not define, and no text at all.
    assert_eq!(pumped[2], log(LogSeverity::Warning, ""));
    assert_eq!(pumped[3], log(LogSeverity::Debug, &"z".repeat(240)));

    // From the wrong thread, main-thread functions are refused: nothing
    // is rescanned, nothing is marked. The log, which any thread may use,
    // still works.
    ask_for(ask::FROM_A_WORKER);
    let pumped = plugin.pump();
    assert_eq!(
        pumped,
        [
            log(
                LogSeverity::PluginMisbehaving,
                "the plugin called a main-thread function of the host from another thread 6 \
                 time(s); the calls were ignored"
            ),
            log(LogSeverity::Error, "from a worker"),
        ]
    );
    assert!(!yes(&script.main_in_worker) && !yes(&script.audio_in_worker));
    assert!(plugin.pump().is_empty());
}

#[test]
fn what_needs_the_plugin_inactive_waits_until_it_is() {
    let script = Script::new();
    let mut plugin = host(&script);
    let host = script.host() as usize;
    let ask_for = |asks: u32| {
        script.asks.store(asks, Relaxed);
        let host = host as *const clap_host;
        unsafe { (*host).request_callback.unwrap()(host) };
    };
    assert_eq!(plugin.latency(), 0, "latency is read on activation");
    assert_eq!(plugin.tail(), Some(Tail::Frames(100)));

    // Activation reads the latency the plugin now has.
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    assert_eq!((plugin.latency(), processor.latency_samples()), (64, 64));
    assert_eq!(plugin.pump(), [HostEvent::LatencyChanged(64)]);
    assert_eq!(processor.tail(), Some(Tail::Frames(100)));
    script.tail.store(u32::MAX, Relaxed);
    assert_eq!(
        (plugin.tail(), processor.tail()),
        (Some(Tail::Infinite), Some(Tail::Infinite))
    );

    // While it runs, the plugin says its whole parameter list and its
    // ports have changed. CLAP allows neither to be read now.
    script.layout.store(5, Relaxed);
    script.param_count.store(2, Relaxed);
    ask_for(ask::RESCAN_ALL | ask::PORT_LIST | ask::NOTE_PORTS | ask::FLUSH);
    assert_eq!(plugin.pump(), [HostEvent::FlushRequested]);
    assert_eq!(plugin.params().len(), 3);
    assert_eq!(plugin.audio_inputs().len(), 2);
    assert_eq!(
        script.flushes.load(Relaxed),
        0,
        "flushed on the wrong thread"
    );

    // The flush the plugin asked for is the audio thread's to do.
    plugin.set_param(GAIN, 0.5).unwrap();
    let heap = common::heap_calls_during(|| processor.flush());
    assert_eq!(heap, 0);
    assert_eq!(script.flushes.load(Relaxed), 1);
    assert!(yes(&script.audio_in_flush));
    assert_eq!(script.gain(), 0.5);
    assert_eq!(script.blocks.load(Relaxed), 0, "a flush is not a block");

    // From inside `process` the plugin logs, says its tail changed and
    // makes all three requests. None of it costs the audio thread a heap
    // call.
    script.chatty.store(true, Relaxed);
    script.latency.store(u32::MAX, Relaxed);
    let mut block = filled(2, 64, 1.0);
    let heap = common::heap_calls_during(|| processor.process(&mut block));
    assert_eq!(heap, 0);
    assert!(yes(&script.audio_in_process));
    script.chatty.store(false, Relaxed);
    // The callback it asked for says the latency changed.
    script.asks.store(ask::LATENCY, Relaxed);
    let pumped = plugin.pump();
    assert_eq!(
        pumped,
        [
            HostEvent::RestartRequested,
            HostEvent::ProcessRequested,
            HostEvent::Log {
                severity: LogSeverity::PluginMisbehaving,
                message: format!(
                    "the plugin reports a latency of {} frames; {MAX_LATENCY_FRAMES} is used",
                    u32::MAX
                ),
            },
            HostEvent::LatencyChanged(MAX_LATENCY_FRAMES),
            HostEvent::TailChanged,
            HostEvent::Log {
                severity: LogSeverity::Warning,
                message: "said while processing".to_owned(),
            },
        ]
    );
    assert_eq!(processor.latency_samples(), MAX_LATENCY_FRAMES as usize);

    // Deactivated, the lists are read at last.
    drop(processor);
    plugin.deactivate().unwrap();
    let all = ParamsRescan {
        values: true,
        text: true,
        info: true,
        all: true,
    };
    assert_eq!(
        plugin.pump(),
        [
            HostEvent::ParamsRescanned(all),
            HostEvent::AudioPortsChanged,
            HostEvent::NotePortsChanged,
        ]
    );
    assert_eq!(plugin.params().len(), 2);
    assert!(plugin.audio_inputs().is_empty());
    assert_eq!(plugin.audio_outputs().len(), 1);
    assert_eq!(plugin.param(WILD), None);
}

// --- State ------------------------------------------------------------------

#[test]
fn state_streams_take_any_size_of_call_and_have_a_limit() {
    let script = Script::new();
    let mut plugin = host(&script);
    let state = script.state.lock().unwrap().clone();
    assert_eq!(state.len(), 35);

    // All at once, and a byte at a time: the plugin offers the rest until
    // it has all been taken.
    assert_eq!(plugin.save_state().unwrap(), state);
    assert_eq!(script.write_calls.swap(0, Relaxed), 1);
    plugin.set_stream_chunk(1);
    assert_eq!(plugin.save_state().unwrap(), state);
    assert_eq!(script.write_calls.swap(0, Relaxed), 35);
    plugin.set_stream_chunk(8);
    assert_eq!(plugin.save_state().unwrap(), state);
    assert_eq!(script.write_calls.swap(0, Relaxed), 5);

    // Reading: the plugin asks for 16 bytes at a time and gets what there
    // is, then nothing, then nothing again.
    for (chunk, calls) in [(0, 4), (5, 8), (1, 36)] {
        plugin.set_stream_chunk(chunk);
        plugin.load_state(&state).unwrap();
        assert_eq!(
            *script.loaded.lock().unwrap(),
            state,
            "{chunk} bytes per call"
        );
        assert_eq!(
            script.read_calls.swap(0, Relaxed),
            calls,
            "{chunk} bytes per call"
        );
    }
    plugin.load_state(&[]).unwrap();
    assert!(script.loaded.lock().unwrap().is_empty());
    let rescan = ParamsRescan {
        values: true,
        ..ParamsRescan::default()
    };
    assert_eq!(plugin.pump(), [HostEvent::ParamsRescanned(rescan)]);

    // Refusals come back as errors.
    script.state_mode.store(1, Relaxed);
    assert_eq!(plugin.save_state(), Err(ClapError::StateSaveFailed));
    assert_eq!(plugin.load_state(&state), Err(ClapError::StateLoadFailed));
    assert!(plugin.pump().is_empty());

    // A plugin that never stops writing is stopped at 64 MiB: it writes a
    // megabyte at a time, and the 65th is refused. (Too much memory for an
    // interpreter, so not under Miri.)
    if !cfg!(miri) {
        script.state_mode.store(2, Relaxed);
        script.write_calls.store(0, Relaxed);
        plugin.set_stream_chunk(0);
        assert_eq!(plugin.save_state(), Err(ClapError::StateTooLarge));
        assert_eq!(script.write_calls.load(Relaxed), 65);
    }
}

// --- Life and death ----------------------------------------------------------

#[test]
fn the_plugin_is_stopped_deactivated_and_destroyed_in_order_on_its_main_thread() {
    let here = std::thread::current().id();

    // The ordinary way round.
    let script = Script::new();
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    assert_eq!(
        plugin.activate(48_000.0, 64).unwrap_err(),
        ClapError::AlreadyActive
    );
    processor.process(&mut filled(2, 64, 0.0));
    assert!(yes(&script.main_in_process) && yes(&script.audio_in_process));
    drop(processor);
    drop(plugin);
    let at = |stamp: &AtomicU32| stamp.load(Relaxed);
    assert!(at(&script.stopped_at) > 0, "processing was never stopped");
    assert!(at(&script.stopped_at) < at(&script.deactivated_at));
    assert!(at(&script.deactivated_at) < at(&script.destroyed_at));
    assert_eq!(script.destroyed.load(Relaxed), 1);
    assert_eq!(*script.destroyed_on.lock().unwrap(), Some(here));

    // A processor that never processed was never started, so it is not
    // stopped either.
    let script = Script::new();
    let mut plugin = host(&script);
    drop(plugin.activate(48_000.0, 64).unwrap());
    drop(plugin);
    assert_eq!(
        (at(&script.stopped_at), script.starts.load(Relaxed)),
        (0, 0)
    );
    assert!(at(&script.deactivated_at) > 0 && script.destroyed.load(Relaxed) == 1);

    // The audio half used and dropped on another thread: the plugin still
    // goes on this one.
    let script = Script::new();
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    std::thread::spawn(move || {
        processor.process(&mut filled(2, 64, 0.0));
    })
    .join()
    .unwrap();
    // There it was the audio thread and not the main thread.
    assert!(!yes(&script.main_in_process) && yes(&script.audio_in_process));
    assert!(!plugin.has_processor());
    assert_eq!(script.destroyed.load(Relaxed), 0);
    drop(plugin);
    assert_eq!(script.destroyed.load(Relaxed), 1);
    assert_eq!(*script.destroyed_on.lock().unwrap(), Some(here));
    assert!(at(&script.stopped_at) < at(&script.deactivated_at));

    // The main half dropped first: the plugin lives for as long as its
    // audio half, and goes with it when that is dropped here.
    let script = Script::new();
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    drop(plugin);
    assert_eq!(script.destroyed.load(Relaxed), 0);
    let mut block = filled(2, 64, 1.0);
    processor.process(&mut block);
    assert_eq!(
        (processor.status(), block.peak()),
        (ProcessStatus::Continue, 1.0)
    );
    drop(processor);
    assert_eq!(script.destroyed.load(Relaxed), 1);
    assert_eq!(*script.destroyed_on.lock().unwrap(), Some(here));

    // The main half dropped first and the audio half dropped elsewhere:
    // there is no thread left on which CLAP allows the plugin to be
    // destroyed, so it is not. It is stopped, and left.
    let script = Script::new();
    let before = inits();
    let mut plugin = host(&script);
    let mut processor = plugin.activate(48_000.0, 64).unwrap();
    drop(plugin);
    std::thread::spawn(move || {
        processor.process(&mut filled(2, 64, 0.0));
        drop(processor);
    })
    .join()
    .unwrap();
    assert!(at(&script.stopped_at) > 0);
    assert_eq!(
        script.destroyed.load(Relaxed),
        0,
        "destroyed off its main thread"
    );
    assert_eq!(at(&script.deactivated_at), 0);
    assert_eq!(
        inits().1,
        before.1,
        "the library was unloaded under a live plugin"
    );
}
