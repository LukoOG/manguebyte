//! Drives the notes a plugin sends through the LV2 wrapper, as a host
//! does: they come out of the notify port as `midi:MidiEvent`s on their
//! frames, which read back through the framework's `NoteInput` as the
//! events the plugin sent, and `run` stays off the heap while it writes
//! them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event, EventKind, Expression, NoteRef};
use manguebyte_dsp::note_input::{Mpe, NoteInput};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_lv2::sys::*;
use manguebyte_lv2::{Descriptor, PortLayout, PortRole, bundle_files};
use manguebyte_midi::MidiMessage;
use manguebyte_params::param_set;
use manguebyte_plugin::output::OutputBuffer;
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};

// --- Watching the heap ---------------------------------------------------------

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is bumping a thread-local counter, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn heap_calls(f: impl FnOnce()) -> usize {
    HEAP_CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    f();
    WATCHING.with(|watching| watching.set(false));
    HEAP_CALLS.with(Cell::get)
}

// --- The plugin -----------------------------------------------------------------

struct NoParams {}
param_set!(NoParams {});

/// Plays every note it is given an octave up, with the pressure it is
/// given, and sends the modulation wheel on as it is.
struct Octave {
    params: Arc<NoParams>,
}

impl Plugin for Octave {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.octave",
        name: "Octave",
        vendor: "Manguebyte",
        version: "0.1.0",
        description: "Plays an octave up",
        kind: PluginKind::NoteEffect,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    const NOTE_OUTPUT: bool = true;
    type Params = NoParams;

    fn new() -> Self {
        Self {
            params: Arc::new(NoParams {}),
        }
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.params)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        self.process_with_output(buffer, context, &mut OutputBuffer::new());
    }
    fn process_with_output(
        &mut self,
        _: &mut AudioBuffer<f32>,
        context: &ProcessContext<'_, f32>,
        output: &mut OutputBuffer,
    ) {
        let up = |note: NoteRef| NoteRef::new(note.channel, note.key + 12);
        for event in context.events {
            match event.kind {
                EventKind::NoteOn { note, velocity } => {
                    output.note_on(event.offset, up(note), velocity);
                }
                EventKind::NoteOff { note, velocity } => {
                    output.note_off(event.offset, up(note), velocity);
                }
                EventKind::NoteExpression {
                    note,
                    expression,
                    value,
                } => {
                    output.expression(event.offset, up(note), expression, value);
                }
                EventKind::Midi(message) => {
                    output.midi1(event.offset, message);
                }
                _ => {}
            }
        }
    }
}

/// A plugin that sends nothing, to compare its Turtle with.
struct Quiet {
    params: Arc<NoParams>,
}

impl Plugin for Quiet {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.quiet",
        name: "Quiet",
        vendor: "Manguebyte",
        version: "0.1.0",
        description: "Sends nothing",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = NoParams;

    fn new() -> Self {
        Self {
            params: Arc::new(NoParams {}),
        }
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.params)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

// --- A host ----------------------------------------------------------------------

static URIS: Mutex<Vec<CString>> = Mutex::new(Vec::new());

fn urid(uri: &CStr) -> u32 {
    let mut uris = URIS.lock().unwrap_or_else(|poison| poison.into_inner());
    let index = match uris.iter().position(|known| known.as_c_str() == uri) {
        Some(index) => index,
        None => {
            uris.push(uri.to_owned());
            uris.len() - 1
        }
    };
    index as u32 + 1
}

unsafe extern "C" fn map_uri(_: *mut c_void, uri: *const c_char) -> u32 {
    // SAFETY: the plugin passes a C string, as `urid:map` requires.
    urid(unsafe { CStr::from_ptr(uri) })
}

/// An atom sequence being written, in 8-byte words.
struct Seq {
    bytes: Vec<u8>,
}

impl Seq {
    fn new() -> Self {
        let mut bytes = Vec::new();
        bytes.extend(0u32.to_ne_bytes());
        bytes.extend(urid(LV2_ATOM__Sequence).to_ne_bytes());
        bytes.extend(urid(LV2_ATOM__frameTime).to_ne_bytes());
        bytes.extend(0u32.to_ne_bytes());
        Self { bytes }
    }

    fn event(mut self, frame: i64, type_: u32, body: &[u8]) -> Self {
        self.bytes.extend(frame.to_ne_bytes());
        self.bytes.extend((body.len() as u32).to_ne_bytes());
        self.bytes.extend(type_.to_ne_bytes());
        self.bytes.extend(body);
        self.bytes.resize(self.bytes.len().next_multiple_of(8), 0);
        self
    }

    fn midi(self, frame: i64, message: &[u8]) -> Self {
        self.event(frame, urid(LV2_MIDI__MidiEvent), message)
    }

    /// A `time:Position` with its speed only, which cuts the block.
    fn position(self, frame: i64) -> Self {
        let mut body = Vec::new();
        body.extend(0u32.to_ne_bytes());
        body.extend(urid(LV2_TIME__Position).to_ne_bytes());
        body.extend(urid(LV2_TIME__speed).to_ne_bytes());
        body.extend(0u32.to_ne_bytes());
        body.extend(8u32.to_ne_bytes());
        body.extend(urid(LV2_ATOM__Double).to_ne_bytes());
        body.extend(1.0f64.to_ne_bytes());
        self.event(frame, urid(LV2_ATOM__Object), &body)
    }

    fn finish(&self) -> Vec<u64> {
        let mut bytes = self.bytes.clone();
        let size = (bytes.len() - 8) as u32;
        bytes[..4].copy_from_slice(&size.to_ne_bytes());
        bytes
            .chunks(8)
            .map(|chunk| {
                let mut full = [0u8; 8];
                full[..chunk.len()].copy_from_slice(chunk);
                u64::from_ne_bytes(full)
            })
            .collect()
    }
}

/// The events of a sequence the plugin wrote: frame, type and body.
fn read_sequence(words: &[u64]) -> Vec<(i64, u32, Vec<u8>)> {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_ne_bytes()).collect();
    let size = u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as usize;
    assert_eq!(
        u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        urid(LV2_ATOM__Sequence)
    );
    let end = 8 + size;
    let mut at = 16;
    let mut events = Vec::new();
    while at < end {
        let frame = i64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap());
        let length = u32::from_ne_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let type_ = u32::from_ne_bytes(bytes[at + 12..at + 16].try_into().unwrap());
        events.push((frame, type_, bytes[at + 16..at + 16 + length].to_vec()));
        at += (16 + length).next_multiple_of(8);
    }
    assert_eq!(at, end, "the sequence's size counts whole events");
    events
}

struct Host {
    descriptor: &'static Descriptor,
    handle: LV2_Handle,
    ports: PortLayout,
    latency: f32,
    _map: Box<LV2_URID_Map>,
    _features: Box<[LV2_Feature]>,
    _list: Box<[*const LV2_Feature]>,
}

impl Host {
    fn new() -> Self {
        let descriptor = manguebyte_lv2::descriptor!(Octave);
        let mut map = Box::new(LV2_URID_Map {
            handle: ptr::null_mut(),
            map: Some(map_uri),
        });
        let features = vec![LV2_Feature {
            URI: LV2_URID__map.as_ptr(),
            data: (&raw mut *map).cast(),
        }]
        .into_boxed_slice();
        let mut list: Vec<*const LV2_Feature> = features.iter().map(ptr::from_ref).collect();
        list.push(ptr::null());
        let list = list.into_boxed_slice();
        let lv2 = descriptor.raw();
        // SAFETY: the features and everything they point at live in the
        // host until `cleanup`.
        let handle = unsafe {
            lv2.instantiate.unwrap()(
                descriptor.as_ptr(),
                48_000.0,
                c"/nowhere/".as_ptr(),
                list.as_ptr(),
            )
        };
        assert!(!handle.is_null());
        // SAFETY: a live instance, not yet active.
        unsafe { lv2.activate.unwrap()(handle) };
        Self {
            descriptor,
            handle,
            ports: PortLayout::new(Octave::LAYOUT, 0),
            latency: -1.0,
            _map: map,
            _features: features,
            _list: list,
        }
    }

    /// Runs one block with `control` coming in and `room` bytes in the
    /// notify buffer after its atom header. Returns the heap calls `run`
    /// made and the notify buffer as the plugin left it.
    fn run(&mut self, frames: usize, control: &Seq, room: usize) -> (usize, Vec<u64>) {
        let mut outputs = vec![vec![0.0f32; frames]; 2];
        let mut control = control.finish();
        let mut notify = vec![0u64; 1 + room.div_ceil(8)];
        notify[0] = room as u64;
        let lv2 = self.descriptor.raw();
        let connect = lv2.connect_port.unwrap();
        for port in 0..self.ports.count() {
            let data: *mut c_void = match self.ports.role(port).unwrap() {
                PortRole::AudioOutput(channel) => outputs[channel].as_mut_ptr().cast(),
                PortRole::Control => control.as_mut_ptr().cast(),
                PortRole::Notify => notify.as_mut_ptr().cast(),
                PortRole::Latency => (&raw mut self.latency).cast(),
                role => panic!("Octave has no {role:?} port"),
            };
            // SAFETY: every buffer lives until `run` returns.
            unsafe { connect(self.handle, port as u32, data) };
        }
        let run = lv2.run.unwrap();
        let handle = self.handle;
        // SAFETY: every port is connected to memory valid for this block.
        let calls = heap_calls(|| unsafe { run(handle, frames as u32) });
        for port in 0..self.ports.count() {
            // SAFETY: disconnecting is always allowed.
            unsafe { connect(self.handle, port as u32, ptr::null_mut()) };
        }
        (calls, notify)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let lv2 = self.descriptor.raw();
        // SAFETY: a live, active instance, deactivated and then freed.
        unsafe {
            lv2.deactivate.unwrap()(self.handle);
            lv2.cleanup.unwrap()(self.handle);
        }
    }
}

// --- The tests -----------------------------------------------------------------------

#[test]
fn notes_sent_come_out_of_the_notify_port_and_read_back_as_they_were_sent() {
    let mut host = Host::new();
    // A note, then a transport position that cuts the block at frame 8,
    // then pressure, the wheel and the note's end after it.
    let control = Seq::new()
        .midi(3, &[0x91, 60, 100])
        .position(8)
        .midi(10, &[0xA1, 60, 40])
        .midi(11, &[0xB1, 1, 64])
        .midi(14, &[0x81, 60, 0]);
    let (calls, notify) = host.run(16, &control, 1024);
    assert_eq!(calls, 0, "run used the heap");

    let events = read_sequence(&notify);
    let midi = urid(LV2_MIDI__MidiEvent);
    assert!(events.iter().all(|(_, type_, _)| *type_ == midi));
    // On the block's frames, though the second piece began at 8.
    let frames: Vec<i64> = events.iter().map(|(frame, ..)| *frame).collect();
    assert_eq!(frames, [3, 10, 11, 14]);

    let mut input = NoteInput::new(Mpe::Off);
    let mut read = Vec::new();
    for (frame, _, body) in &events {
        let message = MidiMessage::from_bytes(body).unwrap();
        input.midi1(*frame as u32, message, |event: Event| read.push(event));
    }
    let note = NoteRef::new(1, 72);
    let kinds: Vec<EventKind> = read.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::NoteOn {
                note,
                velocity: 100.0 / 127.0
            },
            EventKind::NoteExpression {
                note,
                expression: Expression::Pressure,
                value: 40.0 / 127.0
            },
            EventKind::Midi(MidiMessage::ControlChange {
                channel: 1,
                controller: 1,
                value: 64
            }),
            EventKind::NoteOff {
                note,
                velocity: 0.0
            },
        ]
    );
}

#[test]
fn a_notify_buffer_too_small_keeps_what_fits_and_nothing_more() {
    let mut host = Host::new();
    let control = Seq::new()
        .midi(0, &[0x90, 60, 100])
        .midi(1, &[0x90, 62, 100])
        .midi(2, &[0x90, 64, 100]);
    // The sequence body and two events of 16 bytes and a padded message.
    let (calls, notify) = host.run(8, &control, 8 + 2 * 24);
    assert_eq!(calls, 0);
    let events = read_sequence(&notify);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].2, [0x90, 74, 100]);
    // With no room for a sequence at all, nothing is written.
    let (_, notify) = host.run(8, &control, 4);
    assert_eq!(notify[0], 4);
}

#[test]
fn only_a_plugin_that_sends_notes_says_its_notify_port_carries_midi() {
    let supports = "atom:supports midi:MidiEvent ;";
    let octave = bundle_files::<Octave>("octave.so").plugin;
    let quiet = bundle_files::<Quiet>("quiet.so").plugin;
    // The control port takes MIDI in both; the notify port only in one.
    assert_eq!(octave.matches("midi:MidiEvent").count(), 2, "{octave}");
    assert!(octave.contains(supports));
    assert_eq!(quiet.matches("midi:MidiEvent").count(), 1, "{quiet}");
}
