//! Notes, expressions and MIDI sent out of a plugin, as a VST 3 host gets
//! them on the plugin's output event bus.
//!
//! As in `abi.rs`, the test plays the host through the COM interfaces, and
//! implements the output event list itself, collecting what the plugin
//! adds to it during `process`.
//!
//! Every `unsafe` block in this file is a call across that interface with
//! pointers the test owns and keeps alive for the duration of the call, so
//! the blocks are not commented one by one.
#![allow(clippy::undocumented_unsafe_blocks)]
#![allow(non_snake_case)]
// The bindings give enumerations a different integer type on Windows than
// elsewhere, so a cast needed on one platform is redundant on the other.
#![allow(clippy::unnecessary_cast)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::ptr;
use std::sync::Arc;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{EventKind, Expression, NoteRef};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::ump::{ChannelVoice2, NoteAttribute, PITCH_BEND_CENTRE};
use manguebyte_params::param_set;
use manguebyte_plugin::output::OutputBuffer;
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};
use manguebyte_vst3::vst3::Steinberg::Vst::{
    self as vst, IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentTrait, IEventList,
    IEventListTrait,
};
use manguebyte_vst3::vst3::Steinberg::{
    IPluginBaseTrait, IPluginFactory, IPluginFactoryTrait, kResultFalse, kResultOk, tresult,
};
use manguebyte_vst3::vst3::{Class, ComPtr, ComWrapper, Interface};
use manguebyte_vst3::{class_id, plugin_factory};

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

/// For every note it is given, sends a note an octave up with an id,
/// shapes it, and sends some MIDI of each kind.
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
                    output.expression(at, up, Expression::Tuning, 12.0);
                    output.expression(at, up, Expression::Pressure, 0.5);
                    // A note with no id, shaped by channel and key.
                    let plain = NoteRef::new(3, key);
                    output.note_on(at, plain, 1.0);
                    output.expression(at, plain, Expression::Pan, 1.0);
                    // MIDI 2.0: a note bent per note, and a channel bend.
                    output.midi2(
                        at + 1,
                        ChannelVoice2::NoteOn {
                            channel: 5,
                            note: 40,
                            velocity: 0x8000,
                            attribute: NoteAttribute::NONE,
                        },
                    );
                    output.midi2(
                        at + 1,
                        ChannelVoice2::PerNotePitchBend {
                            channel: 5,
                            note: 40,
                            value: 0,
                        },
                    );
                    output.midi2(
                        at + 1,
                        ChannelVoice2::PitchBend {
                            channel: 5,
                            value: PITCH_BEND_CENTRE,
                        },
                    );
                    output.midi1(
                        1_000,
                        MidiMessage::ControlChange {
                            channel: 2,
                            controller: 64,
                            value: 127,
                        },
                    );
                }
                EventKind::NoteOff { note, velocity } => {
                    // By id alone, as VST 3 hosts address notes.
                    output.note_off(
                        at,
                        NoteRef::with_id(0, 0, 100 + u32::from(note.key)),
                        velocity,
                    );
                }
                _ => {}
            }
        }
    }
}

/// An effect that sends nothing has no output event bus.
struct Quiet(Arc<NoParams>);

impl Plugin for Quiet {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.quiet",
        kind: PluginKind::Effect,
        ..Octaver::INFO
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
    type Params = NoParams;

    fn new() -> Self {
        Self(Arc::new(NoParams {}))
    }
    fn params(&self) -> Arc<NoParams> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

// --- The host --------------------------------------------------------------

/// An event list: the host's input, or where the plugin's output goes.
struct Events {
    events: RefCell<Vec<vst::Event>>,
}

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> i32 {
        self.events.borrow().len() as i32
    }
    unsafe fn getEvent(&self, index: i32, event: *mut vst::Event) -> tresult {
        let Some(found) = self.events.borrow().get(index as usize).copied() else {
            return kResultFalse;
        };
        unsafe { *event = found };
        kResultOk
    }
    unsafe fn addEvent(&self, event: *mut vst::Event) -> tresult {
        self.events.borrow_mut().push(unsafe { *event });
        kResultOk
    }
}

fn iid<I: Interface>() -> *const c_char {
    I::IID.as_ptr().cast()
}

struct Host {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
}

impl Host {
    fn new<P: Plugin>() -> Self {
        unsafe {
            let factory = ComPtr::<IPluginFactory>::from_raw(plugin_factory::<P>().cast()).unwrap();
            let class = class_id(P::INFO.id);
            let mut object = ptr::null_mut();
            let result = factory.createInstance(class.as_ptr(), iid::<IComponent>(), &mut object);
            assert_eq!(result, kResultOk);
            let component = ComPtr::<IComponent>::from_raw(object.cast()).unwrap();
            assert_eq!(component.initialize(ptr::null_mut()), kResultOk);
            let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
            let mut setup = vst::ProcessSetup {
                processMode: vst::ProcessModes_::kRealtime as i32,
                symbolicSampleSize: vst::SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: 32,
                sampleRate: 48_000.0,
            };
            assert_eq!(processor.setupProcessing(&mut setup), kResultOk);
            assert_eq!(component.setActive(1), kResultOk);
            assert_eq!(processor.setProcessing(1), kResultOk);
            Self {
                component,
                processor,
            }
        }
    }

    fn event_bus(&self, direction: u32) -> (i32, Option<vst::BusInfo>) {
        let media = vst::MediaTypes_::kEvent as vst::MediaType;
        unsafe {
            let count = self.component.getBusCount(media, direction as _);
            let mut info: vst::BusInfo = std::mem::zeroed();
            let found = self
                .component
                .getBusInfo(media, direction as _, 0, &mut info)
                == kResultOk;
            (count, found.then_some(info))
        }
    }

    /// Processes a block of `frames` frames with these notes in, and
    /// returns what the plugin added to the output list, and how often it
    /// used the heap.
    fn process(&self, frames: usize, notes: Vec<vst::Event>) -> (Vec<vst::Event>, usize) {
        let mut channels = vec![vec![0.0f32; frames]; 2];
        let mut pointers: Vec<*mut f32> = channels.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let bus = vst::AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: vst::AudioBusBuffers__type0 {
                channelBuffers32: pointers.as_mut_ptr(),
            },
        };
        let mut inputs = [bus];
        let mut outputs = [bus];
        let input = ComWrapper::new(Events {
            events: RefCell::new(notes),
        });
        // Room for what comes out, so that collecting it is not counted.
        let output = ComWrapper::new(Events {
            events: RefCell::new(Vec::with_capacity(64)),
        });
        let mut data = vst::ProcessData {
            processMode: vst::ProcessModes_::kRealtime as i32,
            symbolicSampleSize: vst::SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: 1,
            numOutputs: 1,
            inputs: inputs.as_mut_ptr(),
            outputs: outputs.as_mut_ptr(),
            inputParameterChanges: ptr::null_mut(),
            outputParameterChanges: ptr::null_mut(),
            inputEvents: input.as_com_ref::<IEventList>().unwrap().as_ptr(),
            outputEvents: output.as_com_ref::<IEventList>().unwrap().as_ptr(),
            processContext: ptr::null_mut(),
        };
        HEAP_CALLS.with(|calls| calls.set(0));
        IN_PROCESS.with(|watching| watching.set(true));
        let status = unsafe { self.processor.process(&mut data) };
        IN_PROCESS.with(|watching| watching.set(false));
        assert_eq!(status, kResultOk);
        let heap = HEAP_CALLS.with(Cell::get);
        let got = output.events.borrow().clone();
        (got, heap)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            self.component.terminate();
        }
    }
}

const INPUT: u32 = vst::BusDirections_::kInput as u32;
const OUTPUT: u32 = vst::BusDirections_::kOutput as u32;

fn note(offset: i32, on: bool, pitch: i16) -> vst::Event {
    use vst::Event_::EventTypes_ as types;
    let mut event: vst::Event = unsafe { std::mem::zeroed() };
    event.sampleOffset = offset;
    if on {
        event.r#type = types::kNoteOnEvent as u16;
        event.__field0.noteOn = vst::NoteOnEvent {
            channel: 0,
            pitch,
            tuning: 0.0,
            velocity: 0.5,
            length: 0,
            noteId: -1,
        };
    } else {
        event.r#type = types::kNoteOffEvent as u16;
        event.__field0.noteOff = vst::NoteOffEvent {
            channel: 0,
            pitch,
            velocity: 0.25,
            noteId: -1,
            tuning: 0.0,
        };
    }
    event
}

fn wide(field: &[u16]) -> String {
    let len = field.iter().position(|&unit| unit == 0).unwrap();
    String::from_utf16(&field[..len]).unwrap()
}

/// The id made up for the note on `key` of `channel`, which has none.
fn made_up(channel: i32, key: i32) -> i32 {
    0x7FFF_F800 + channel * 128 + key
}

#[test]
fn a_plugin_that_sends_notes_has_an_output_event_bus() {
    let host = Host::new::<Octaver>();
    let (count, info) = host.event_bus(OUTPUT);
    assert_eq!(count, 1);
    let info = info.unwrap();
    assert_eq!(wide(&info.name), "Notes out");
    assert_eq!(info.channelCount, 16);
    assert_eq!(host.event_bus(INPUT).0, 1);
    drop(host);

    let quiet = Host::new::<Quiet>();
    let (count, info) = quiet.event_bus(OUTPUT);
    assert_eq!(count, 0);
    assert!(info.is_none());
}

#[test]
fn notes_expressions_and_midi_reach_the_output_event_bus() {
    use vst::Event_::EventTypes_ as types;
    use vst::NoteExpressionTypeIDs_ as ids;

    let host = Host::new::<Octaver>();
    let (got, heap) = host.process(16, vec![note(2, true, 48)]);
    assert_eq!(heap, 0, "sending notes out used the heap");
    let kinds: Vec<u32> = got.iter().map(|event| u32::from(event.r#type)).collect();
    assert_eq!(
        kinds,
        [
            types::kNoteOnEvent as u32,
            types::kNoteExpressionValueEvent as u32,
            types::kPolyPressureEvent as u32,
            types::kNoteOnEvent as u32,
            types::kNoteExpressionValueEvent as u32,
            types::kNoteOnEvent as u32,
            types::kNoteExpressionValueEvent as u32,
            types::kLegacyMIDICCOutEvent as u32,
            types::kLegacyMIDICCOutEvent as u32,
        ]
    );
    let offsets: Vec<i32> = got.iter().map(|event| event.sampleOffset).collect();
    // The controller sent past the block lands on its last frame.
    assert_eq!(offsets, [2, 2, 2, 2, 2, 3, 3, 3, 15]);
    unsafe {
        let on = got[0].__field0.noteOn;
        assert_eq!(
            (on.channel, on.pitch, on.noteId, on.velocity),
            (0, 60, 148, 0.5)
        );
        // Twelve semitones up: 12 / 240 above the middle.
        let tuning = got[1].__field0.noteExpressionValue;
        assert_eq!(
            (tuning.typeId, tuning.noteId),
            (ids::kTuningTypeID as u32, 148)
        );
        assert!((tuning.value - (0.5 + 12.0 / 240.0)).abs() < 1e-12);
        let pressure = got[2].__field0.polyPressure;
        assert_eq!(
            (pressure.pitch, pressure.noteId, pressure.pressure),
            (60, 148, 0.5)
        );
        // The note without an id gets one made from its channel and key,
        // and its expression finds it by that id.
        let plain = got[3].__field0.noteOn;
        assert_eq!((plain.channel, plain.noteId), (3, made_up(3, 60)));
        let pan = got[4].__field0.noteExpressionValue;
        assert_eq!(
            (pan.typeId, pan.noteId, pan.value),
            (ids::kPanTypeID as u32, made_up(3, 60), 1.0)
        );
        // The MIDI 2.0 note, read into a note, and its per-note bend all
        // the way down, 48 semitones, into its tuning.
        let raised = got[5].__field0.noteOn;
        assert_eq!(
            (raised.channel, raised.pitch, raised.noteId),
            (5, 40, made_up(5, 40))
        );
        let bend = got[6].__field0.noteExpressionValue;
        assert_eq!(bend.noteId, made_up(5, 40));
        assert!((bend.value - (0.5 - 48.0 / 240.0)).abs() < 1e-12);
        // The channel bend, at its centre, low seven bits first.
        let wheel = got[7].__field0.midiCCOut;
        assert_eq!(
            (
                wheel.controlNumber,
                wheel.channel,
                wheel.value,
                wheel.value2
            ),
            (vst::ControllerNumbers_::kPitchBend as u8, 5, 0, 64)
        );
        let pedal = got[8].__field0.midiCCOut;
        assert_eq!(
            (pedal.controlNumber, pedal.channel, pedal.value),
            (64, 2, 127)
        );
    }

    // The note-off names the note by id alone; it goes out on its key.
    let (got, heap) = host.process(16, vec![note(5, false, 48)]);
    assert_eq!(heap, 0, "sending notes out used the heap");
    assert_eq!(got.len(), 1);
    unsafe {
        let off = got[0].__field0.noteOff;
        assert_eq!(
            (off.channel, off.pitch, off.noteId, off.velocity),
            (0, 60, 148, 0.25)
        );
    }
    assert_eq!(got[0].sampleOffset, 5);
}
