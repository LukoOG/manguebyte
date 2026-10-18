//! Notes, expressions and MIDI sent out of a plugin, as a CLAP host gets
//! them.
//!
//! As in `abi.rs`, the test plays the host: it creates the plugin through
//! the entry point, says which note dialects it takes through its own
//! `clap_host_note_ports`, and collects what the plugin pushes onto the
//! output event list of `process`.
//!
//! Every `unsafe` block in this file is a call across that interface with
//! pointers the test owns and keeps alive for the duration of the call, so
//! the blocks are not commented one by one.
#![allow(clippy::undocumented_unsafe_blocks)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::Arc;

use manguebyte_clap::clap_sys::audio_buffer::clap_audio_buffer;
use manguebyte_clap::clap_sys::entry::clap_plugin_entry;
use manguebyte_clap::clap_sys::events::*;
use manguebyte_clap::clap_sys::ext::note_ports::*;
use manguebyte_clap::clap_sys::factory::plugin_factory::{
    CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory,
};
use manguebyte_clap::clap_sys::host::clap_host;
use manguebyte_clap::clap_sys::plugin::clap_plugin;
use manguebyte_clap::clap_sys::process::*;
use manguebyte_clap::clap_sys::version::CLAP_VERSION;
use manguebyte_clap::plugin_entry;
use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{EventKind, Expression, NoteRef};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::translate::scale_7_to_32;
use manguebyte_midi::ump::{ChannelVoice2, NoteAttribute, PITCH_BEND_CENTRE};
use manguebyte_params::param_set;
use manguebyte_plugin::output::OutputBuffer;
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};

// --- Watching the heap ---------------------------------------------------

thread_local! {
    static IN_PROCESS: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record() {
        let _ = IN_PROCESS.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record();
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// --- The plugin under test -------------------------------------------------

struct NoParams {}
param_set!(NoParams {});

/// For every note it is given, sends a note an octave up with an
/// identifier, shapes it, and sends some MIDI of each kind about it.
struct Octaver(Arc<NoParams>);

impl Plugin for Octaver {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.octaver",
        name: "Octaver",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Sends each note an octave up",
        kind: PluginKind::NoteEffect,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
    const NOTE_OUTPUT: bool = true;
    type Params = NoParams;

    fn new() -> Self {
        Self(Arc::new(NoParams {}))
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.0)
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
        for event in context.events {
            let at = event.offset;
            match event.kind {
                EventKind::NoteOn { note, velocity } => {
                    let key = note.key + 12;
                    let up = NoteRef::with_id(note.channel, key, 100 + u32::from(note.key));
                    output.note_on(at, up, velocity);
                    output.expression(at, up, Expression::Tuning, 0.25);
                    output.expression(at, up, Expression::Pan, 0.5);
                    // Per-note pitch bend of a note of its own, a quarter
                    // of the way up.
                    output.midi2(
                        at,
                        ChannelVoice2::NoteOn {
                            channel: 1,
                            note: key,
                            velocity: 0xFFFF,
                            attribute: NoteAttribute::NONE,
                        },
                    );
                    output.midi2(
                        at + 1,
                        ChannelVoice2::PerNotePitchBend {
                            channel: 1,
                            note: key,
                            value: PITCH_BEND_CENTRE + (1 << 29),
                        },
                    );
                    output.midi1(
                        at + 1,
                        MidiMessage::ControlChange {
                            channel: 0,
                            controller: 1,
                            value: 99,
                        },
                    );
                    // Past the end of the block: sent on its last frame.
                    output.midi1(
                        1_000,
                        MidiMessage::ProgramChange {
                            channel: 0,
                            program: 5,
                        },
                    );
                }
                EventKind::NoteOff { note, velocity } => {
                    let up = NoteRef::with_id(0, 0, 100 + u32::from(note.key));
                    output.note_off(at, up, velocity);
                }
                _ => {}
            }
        }
    }
}

/// The entry point, as `export_clap!` would export it.
static ENTRY: clap_plugin_entry = plugin_entry::<Octaver>();

// --- The host --------------------------------------------------------------

/// What the host's `clap_host_note_ports` says, kept in `host_data`.
struct HostState {
    dialects: Option<u32>,
}

unsafe extern "C" fn host_dialects(host: *const clap_host) -> u32 {
    unsafe {
        (*(*host).host_data.cast::<HostState>())
            .dialects
            .unwrap_or(0)
    }
}
unsafe extern "C" fn host_rescan(_: *const clap_host, _: u32) {}

static HOST_NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(host_dialects),
    rescan: Some(host_rescan),
};

unsafe extern "C" fn host_get_extension(
    host: *const clap_host,
    id: *const c_char,
) -> *const c_void {
    let state = unsafe { &*(*host).host_data.cast::<HostState>() };
    if unsafe { CStr::from_ptr(id) } == CLAP_EXT_NOTE_PORTS && state.dialects.is_some() {
        ptr::from_ref(&HOST_NOTE_PORTS).cast()
    } else {
        ptr::null()
    }
}
unsafe extern "C" fn host_request(_: *const clap_host) {}

/// One event the plugin pushed, copied out of the list.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Got {
    Note {
        time: u32,
        type_: u16,
        id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
    },
    Expression {
        time: u32,
        expression: i32,
        id: i32,
        channel: i16,
        key: i16,
        value: f64,
    },
    Midi {
        time: u32,
        data: [u8; 3],
    },
    Midi2 {
        time: u32,
        data: [u32; 4],
    },
}

unsafe extern "C" fn events_size(_: *const clap_input_events) -> u32 {
    0
}
unsafe extern "C" fn events_get_none(
    _: *const clap_input_events,
    _: u32,
) -> *const clap_event_header {
    ptr::null()
}

/// The host's input list: notes as `(time, type, key)`.
unsafe extern "C" fn notes_size(list: *const clap_input_events) -> u32 {
    unsafe { (*(*list).ctx.cast::<Vec<clap_event_note>>()).len() as u32 }
}
unsafe extern "C" fn notes_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    unsafe {
        ptr::from_ref(&(&(*(*list).ctx.cast::<Vec<clap_event_note>>()))[index as usize]).cast()
    }
}

unsafe extern "C" fn events_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    unsafe {
        let got = &mut *(*list).ctx.cast::<Vec<Got>>();
        let time = (*event).time;
        match (*event).type_ {
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                let note = &*event.cast::<clap_event_note>();
                assert_eq!((*event).size as usize, size_of::<clap_event_note>());
                got.push(Got::Note {
                    time,
                    type_: (*event).type_,
                    id: note.note_id,
                    channel: note.channel,
                    key: note.key,
                    velocity: note.velocity,
                });
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                let shaped = &*event.cast::<clap_event_note_expression>();
                got.push(Got::Expression {
                    time,
                    expression: shaped.expression_id,
                    id: shaped.note_id,
                    channel: shaped.channel,
                    key: shaped.key,
                    value: shaped.value,
                });
            }
            CLAP_EVENT_MIDI => got.push(Got::Midi {
                time,
                data: (*event.cast::<clap_event_midi>()).data,
            }),
            CLAP_EVENT_MIDI2 => got.push(Got::Midi2 {
                time,
                data: (*event.cast::<clap_event_midi2>()).data,
            }),
            _ => {}
        }
    }
    true
}

struct Host {
    plugin: *const clap_plugin,
    host: *mut clap_host,
    state: *mut HostState,
}

impl Host {
    fn new(dialects: Option<u32>) -> Self {
        let state = Box::into_raw(Box::new(HostState { dialects }));
        let host = Box::into_raw(Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: state.cast(),
            name: c"test host".as_ptr(),
            vendor: c"Manguebyte".as_ptr(),
            url: c"".as_ptr(),
            version: c"1".as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request),
            request_process: Some(host_request),
            request_callback: Some(host_request),
        }));
        unsafe {
            assert!(ENTRY.init.unwrap()(c"/plugins/test.clap".as_ptr()));
            let factory = ENTRY.get_factory.unwrap()(CLAP_PLUGIN_FACTORY_ID.as_ptr())
                .cast::<clap_plugin_factory>();
            let plugin = (*factory).create_plugin.unwrap()(
                factory,
                host,
                c"org.manguebyte.test.octaver".as_ptr(),
            );
            assert!(!plugin.is_null());
            assert!((*plugin).init.unwrap()(plugin));
            assert!((*plugin).activate.unwrap()(plugin, 48_000.0, 1, 32));
            assert!((*plugin).start_processing.unwrap()(plugin));
            Self {
                plugin,
                host,
                state,
            }
        }
    }

    fn note_port(&self, is_input: bool) -> Option<clap_note_port_info> {
        unsafe {
            let ports =
                (*self.plugin).get_extension.unwrap()(self.plugin, CLAP_EXT_NOTE_PORTS.as_ptr())
                    .cast::<clap_plugin_note_ports>()
                    .as_ref()?;
            assert_eq!(ports.count.unwrap()(self.plugin, is_input), 1);
            let mut info: clap_note_port_info = std::mem::zeroed();
            ports.get.unwrap()(self.plugin, 0, is_input, &raw mut info).then_some(info)
        }
    }

    /// Processes a block of `frames` frames with these notes in, and
    /// returns what came out and how often the plugin used the heap.
    fn process(&self, frames: u32, notes: Vec<(u32, u16, i16)>) -> (Vec<Got>, usize) {
        let mut channels = vec![vec![0.0f32; frames as usize]; 2];
        let mut pointers: Vec<*mut f32> = channels.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let port = clap_audio_buffer {
            data32: pointers.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut outputs = [port];
        let mut sent: Vec<clap_event_note> = notes
            .into_iter()
            .map(|(time, type_, key)| clap_event_note {
                header: clap_event_header {
                    size: size_of::<clap_event_note>() as u32,
                    time,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_,
                    flags: 0,
                },
                note_id: -1,
                port_index: 0,
                channel: 0,
                key,
                velocity: 0.5,
            })
            .collect();
        let input = if sent.is_empty() {
            clap_input_events {
                ctx: ptr::null_mut(),
                size: Some(events_size),
                get: Some(events_get_none),
            }
        } else {
            clap_input_events {
                ctx: (&raw mut sent).cast(),
                size: Some(notes_size),
                get: Some(notes_get),
            }
        };
        // Room for what comes out, so that collecting it is not counted.
        let mut got: Vec<Got> = Vec::with_capacity(64);
        let output = clap_output_events {
            ctx: (&raw mut got).cast(),
            try_push: Some(events_push),
        };
        // One pointer for both, in place, so that the input's does not
        // outlive the output's taking.
        let ports = outputs.as_mut_ptr();
        let process = clap_process {
            steady_time: -1,
            frames_count: frames,
            transport: ptr::null(),
            audio_inputs: ports.cast_const(),
            audio_outputs: ports,
            audio_inputs_count: 1,
            audio_outputs_count: 1,
            in_events: &raw const input,
            out_events: &raw const output,
        };
        HEAP_CALLS.with(|calls| calls.set(0));
        IN_PROCESS.with(|watching| watching.set(true));
        let status = unsafe { (*self.plugin).process.unwrap()(self.plugin, &raw const process) };
        IN_PROCESS.with(|watching| watching.set(false));
        assert_eq!(status, CLAP_PROCESS_CONTINUE);
        (got, HEAP_CALLS.with(Cell::get))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            (*self.plugin).stop_processing.unwrap()(self.plugin);
            (*self.plugin).deactivate.unwrap()(self.plugin);
            (*self.plugin).destroy.unwrap()(self.plugin);
            ENTRY.deinit.unwrap()();
            drop(Box::from_raw(self.host));
            drop(Box::from_raw(self.state));
        }
    }
}

const ALL: u32 = CLAP_NOTE_DIALECT_CLAP
    | CLAP_NOTE_DIALECT_MIDI
    | CLAP_NOTE_DIALECT_MIDI_MPE
    | CLAP_NOTE_DIALECT_MIDI2;

/// One note in, at frame 2, held over a block and released at frame 5 of
/// the next.
fn play(host: &Host) -> Vec<Got> {
    let (mut got, heap) = host.process(16, vec![(2, CLAP_EVENT_NOTE_ON, 48)]);
    assert_eq!(heap, 0, "sending notes out used the heap");
    let (off, heap) = host.process(16, vec![(5, CLAP_EVENT_NOTE_OFF, 48)]);
    assert_eq!(heap, 0, "sending notes out used the heap");
    got.extend(off);
    got
}

#[test]
fn the_plugin_has_an_output_note_port_in_every_dialect() {
    let host = Host::new(Some(ALL));
    let output = host.note_port(false).expect("an output note port");
    assert_eq!(output.supported_dialects, ALL);
    assert_eq!(output.preferred_dialect, CLAP_NOTE_DIALECT_CLAP);
    let name = unsafe { CStr::from_ptr(output.name.as_ptr()) };
    assert_eq!(name, c"Notes out");
    assert!(host.note_port(true).is_some());
}

#[test]
fn a_host_of_clap_notes_and_midi_gets_notes_expressions_and_midi() {
    for dialects in [Some(CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI), None] {
        let host = Host::new(dialects);
        let got = play(&host);
        let expected = [
            Got::Note {
                time: 2,
                type_: CLAP_EVENT_NOTE_ON,
                id: 148,
                channel: 0,
                key: 60,
                velocity: 0.5,
            },
            Got::Expression {
                time: 2,
                expression: CLAP_NOTE_EXPRESSION_TUNING,
                id: 148,
                channel: 0,
                key: 60,
                value: 0.25,
            },
            // The framework's pan of 0.5 is three quarters of CLAP's way.
            Got::Expression {
                time: 2,
                expression: CLAP_NOTE_EXPRESSION_PAN,
                id: 148,
                channel: 0,
                key: 60,
                value: 0.75,
            },
            // The MIDI 2.0 note, read into a CLAP note with no id...
            Got::Note {
                time: 2,
                type_: CLAP_EVENT_NOTE_ON,
                id: -1,
                channel: 1,
                key: 60,
                velocity: 1.0,
            },
        ];
        assert_eq!(got[..4], expected, "{dialects:?}");
        // ...and its per-note bend into its tuning, a quarter of 48
        // semitones up.
        let Got::Expression {
            time: 3,
            expression: CLAP_NOTE_EXPRESSION_TUNING,
            id: -1,
            channel: 1,
            key: 60,
            value,
        } = got[4]
        else {
            panic!("{:?}", got[4]);
        };
        assert!((value - 12.0).abs() < 1e-6, "{value}");
        assert_eq!(
            got[5],
            Got::Midi {
                time: 3,
                data: [0xB0, 1, 99],
            }
        );
        // Past the block: on its last frame.
        assert_eq!(
            got[6],
            Got::Midi {
                time: 15,
                data: [0xC0, 5, 0],
            }
        );
        assert_eq!(
            got[7],
            Got::Note {
                time: 5,
                type_: CLAP_EVENT_NOTE_OFF,
                id: 148,
                channel: 0,
                key: 60,
                velocity: 0.5,
            }
        );
        assert_eq!(got.len(), 8);
    }
}

#[test]
fn a_host_of_midi_2_gets_the_packets_as_they_are() {
    let host = Host::new(Some(CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI2));
    let got = play(&host);
    // The framework's notes stay CLAP's own...
    assert!(matches!(got[0], Got::Note { id: 148, .. }));
    // ...and the plugin's MIDI 2.0 goes out unchanged: the note-on, and
    // the per-note pitch bend.
    assert_eq!(
        got[3],
        Got::Midi2 {
            time: 2,
            data: [0x4091_3C00, 0xFFFF_0000, 0, 0],
        }
    );
    assert_eq!(
        got[4],
        Got::Midi2 {
            time: 3,
            data: [0x4061_3C00, 0xA000_0000, 0, 0],
        }
    );
    // MIDI 1.0 is translated up, since the host takes no MIDI 1.0.
    assert_eq!(
        got[5],
        Got::Midi2 {
            time: 3,
            data: [0x40B0_0100, scale_7_to_32(99), 0, 0],
        }
    );
}

#[test]
fn a_host_without_clap_notes_gets_everything_as_midi() {
    // MIDI 2.0 only: the framework's notes too, as packets.
    let host = Host::new(Some(CLAP_NOTE_DIALECT_MIDI2));
    let got = play(&host);
    assert_eq!(
        got[0],
        Got::Midi2 {
            time: 2,
            data: [0x4090_3C00, 0x8000_0000, 0, 0],
        }
    );
    assert!(got.iter().all(|event| matches!(event, Got::Midi2 { .. })));
    drop(host);

    // MPE only: the zone's configuration, then the note on a member
    // channel with its bend before it.
    let host = Host::new(Some(CLAP_NOTE_DIALECT_MIDI_MPE));
    let got = play(&host);
    let bytes: Vec<[u8; 3]> = got
        .iter()
        .map(|event| match event {
            Got::Midi { data, .. } => *data,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        bytes[..7],
        [
            [0xB0, 100, 6],
            [0xB0, 101, 0],
            [0xB0, 6, 15],
            [0xE1, 0, 0x40],
            [0xD1, 0, 0],
            [0xB1, 74, 64],
            [0x91, 60, 64],
        ]
    );
    drop(host);

    // MIDI 1.0 only: by the default translation, tuning dropped.
    let host = Host::new(Some(CLAP_NOTE_DIALECT_MIDI));
    let got = play(&host);
    assert_eq!(
        got[0],
        Got::Midi {
            time: 2,
            data: [0x90, 60, 64],
        }
    );
    assert_eq!(
        got[1],
        Got::Midi {
            time: 2,
            data: [0x91, 60, 127],
        }
    );
}
