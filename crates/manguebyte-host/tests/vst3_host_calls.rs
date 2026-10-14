//! What the VST 3 host calls on a plugin, and on which thread: which buses
//! it switches on, and where it asks for the tail.
//!
//! The plugin is written here and reached through
//! `Vst3Module::from_factory`. It records every call these tests are
//! about, and its buses are chosen to tell the cases apart: a main stereo
//! output and an auxiliary one, both wanted on from the start, though the
//! plugin refuses to switch on the auxiliary one; an event input; and two
//! event outputs, the first wanted on from the start and the second not.
//!
//! Every `unsafe` block in this file is one side or the other of the VST 3
//! interfaces, with pointers that are valid for the call, so the blocks are
//! not commented one by one.
#![cfg(all(feature = "vst3", not(miri)))]
#![allow(clippy::undocumented_unsafe_blocks)]
#![allow(non_snake_case)]
// The bindings give enumerations a different integer type on Windows than
// elsewhere, so a cast needed on one platform is redundant on the other.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::Processor;
use manguebyte_host::vst3::vst3::Steinberg::Vst::{
    self as vst, IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentTrait,
};
use manguebyte_host::vst3::vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IPluginBaseTrait, IPluginFactory, IPluginFactoryTrait,
    PClassInfo, PFactoryInfo, TBool, TUID, kInvalidArgument, kNotImplemented, kResultFalse,
    kResultOk, tresult,
};
use manguebyte_host::vst3::vst3::{Class, ComPtr, ComWrapper};
use manguebyte_host::vst3::{ClassId, HostEvent, Tail, Vst3Module};

const RATE: f64 = 48_000.0;

const PROBE: ClassId = ClassId::from_words([0x0BAD_CAFE, 0x1234_5678, 0x9ABC_DEF0, 0x0F0F_0F0F]);

const AUDIO: i32 = vst::MediaTypes_::kAudio as i32;
const EVENT: i32 = vst::MediaTypes_::kEvent as i32;
const INPUT: i32 = vst::BusDirections_::kInput as i32;
const OUTPUT: i32 = vst::BusDirections_::kOutput as i32;

/// What the host did to the probe.
struct Record {
    /// `activateBus` calls: media type, direction, index, state.
    buses_activated: Mutex<Vec<(i32, i32, i32, u8)>>,
    /// The threads `getTailSamples` was called on.
    tail_asked_on: Mutex<Vec<ThreadId>>,
    /// What `getTailSamples` answers.
    tail: AtomicU32,
    /// For each `process` call, whether an output event list was passed.
    output_events_given: Mutex<Vec<bool>>,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            buses_activated: Mutex::new(Vec::new()),
            tail_asked_on: Mutex::new(Vec::new()),
            tail: AtomicU32::new(100),
            output_events_given: Mutex::new(Vec::new()),
        }
    }
}

/// One object, component and processor, with no controller.
struct Probe {
    record: Arc<Record>,
}

impl Class for Probe {
    type Interfaces = (IComponent, IAudioProcessor);
}

impl IPluginBaseTrait for Probe {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Probe {
    unsafe fn getControllerClassId(&self, _class: *mut TUID) -> tresult {
        kNotImplemented
    }
    unsafe fn setIoMode(&self, _mode: vst::IoMode) -> tresult {
        kNotImplemented
    }
    unsafe fn getBusCount(&self, media: vst::MediaType, direction: vst::BusDirection) -> i32 {
        match (media as i32, direction as i32) {
            // No audio input, one event input, two of each output.
            (AUDIO, INPUT) => 0,
            (EVENT, INPUT) => 1,
            _ => 2,
        }
    }
    unsafe fn getBusInfo(
        &self,
        media: vst::MediaType,
        direction: vst::BusDirection,
        index: i32,
        bus: *mut vst::BusInfo,
    ) -> tresult {
        if !(0..unsafe { self.getBusCount(media, direction) }).contains(&index) || bus.is_null() {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = direction;
        let audio = media as i32 == AUDIO;
        bus.channelCount = if audio { 2 } else { 16 };
        bus.busType = if index == 0 {
            vst::BusTypes_::kMain as vst::BusType
        } else {
            vst::BusTypes_::kAux as vst::BusType
        };
        // Every bus wants to be on from the start, except the second event
        // output.
        let default_active = audio || index == 0;
        bus.flags = if default_active {
            vst::BusInfo_::BusFlags_::kDefaultActive as u32
        } else {
            0
        };
        for (slot, unit) in bus.name.iter_mut().zip("Probe".encode_utf16().chain([0])) {
            *slot = unit;
        }
        kResultOk
    }
    unsafe fn getRoutingInfo(
        &self,
        _input: *mut vst::RoutingInfo,
        _output: *mut vst::RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(
        &self,
        media: vst::MediaType,
        direction: vst::BusDirection,
        index: i32,
        state: TBool,
    ) -> tresult {
        let call = (media as i32, direction as i32, index, state as u8);
        self.record.buses_activated.lock().unwrap().push(call);
        // The auxiliary audio output cannot be switched on.
        if call == (AUDIO, OUTPUT, 1, 1) {
            kResultFalse
        } else {
            kResultOk
        }
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for Probe {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut vst::SpeakerArrangement,
        _num_inputs: i32,
        _outputs: *mut vst::SpeakerArrangement,
        _num_outputs: i32,
    ) -> tresult {
        kResultOk
    }
    unsafe fn getBusArrangement(
        &self,
        _direction: vst::BusDirection,
        _index: i32,
        arrangement: *mut vst::SpeakerArrangement,
    ) -> tresult {
        unsafe { *arrangement = vst::SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == vst::SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, _setup: *mut vst::ProcessSetup) -> tresult {
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut vst::ProcessData) -> tresult {
        let Some(data) = (unsafe { data.as_mut() }) else {
            return kInvalidArgument;
        };
        self.record
            .output_events_given
            .lock()
            .unwrap()
            .push(!data.outputEvents.is_null());
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        self.record
            .tail_asked_on
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        self.record.tail.load(Relaxed)
    }
}

struct ProbeFactory {
    record: Arc<Record>,
}

impl Class for ProbeFactory {
    type Interfaces = (IPluginFactory,);
}

/// Hands a new object out through the interface `iid` asks for.
unsafe fn hand_out(unknown: ComPtr<FUnknown>, iid: *const TUID, obj: *mut *mut c_void) -> tresult {
    let pointer = unknown.as_ptr();
    unsafe { ((*(*pointer).vtbl).queryInterface)(pointer, iid, obj) }
}

impl IPluginFactoryTrait for ProbeFactory {
    unsafe fn getFactoryInfo(&self, _info: *mut PFactoryInfo) -> tresult {
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        1
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some(info) = (unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        if index != 0 {
            return kInvalidArgument;
        }
        info.cid = PROBE.bytes().map(|byte| byte as c_char);
        info.cardinality = 0x7FFF_FFFF;
        for (slot, &byte) in info.category.iter_mut().zip(b"Audio Module Class\0") {
            *slot = byte as c_char;
        }
        for (slot, &byte) in info.name.iter_mut().zip(b"Probe\0") {
            *slot = byte as c_char;
        }
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        class: FIDString,
        interface: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        unsafe { *obj = ptr::null_mut() };
        if ClassId::from_bytes(unsafe { *class.cast::<[u8; 16]>() }) != PROBE {
            return kInvalidArgument;
        }
        let probe = ComWrapper::new(Probe {
            record: Arc::clone(&self.record),
        });
        unsafe { hand_out(probe.to_com_ptr().unwrap(), interface.cast::<TUID>(), obj) }
    }
}

fn probe_module(record: &Arc<Record>) -> Vst3Module {
    let factory = ComWrapper::new(ProbeFactory {
        record: Arc::clone(record),
    })
    .to_com_ptr::<IPluginFactory>()
    .unwrap()
    .into_raw();
    // SAFETY: the factory above, which keeps to the VST 3 interfaces; its
    // reference is handed over.
    unsafe { Vst3Module::from_factory(factory.cast(), "probe") }.unwrap()
}

/// VST 3 has every bus off until the host switches it on, and a bus's
/// `kDefaultActive` flag asks the host to. The host switches on what it
/// uses, notably the event output whose events it reads, and every bus
/// that asks; it switches the others off, and records what the plugin
/// accepted.
#[test]
fn the_buses_the_host_uses_and_the_ones_the_plugin_wants_are_switched_on() {
    let record = Arc::new(Record::default());
    let module = probe_module(&record);
    let mut plugin = module.create(PROBE).unwrap();
    assert_eq!(plugin.audio_outputs().len(), 2);
    assert_eq!(plugin.event_inputs().len(), 1);
    assert_eq!(plugin.event_outputs().len(), 2);
    assert!(plugin.event_outputs()[0].default_active);
    assert!(!plugin.event_outputs()[1].default_active);
    let every_bus = || {
        let buses = [
            plugin.audio_outputs(),
            plugin.event_inputs(),
            plugin.event_outputs(),
        ];
        buses.into_iter().flatten().cloned().collect::<Vec<_>>()
    };
    assert!(every_bus().iter().all(|bus| !bus.active));
    assert!(record.buses_activated.lock().unwrap().is_empty());

    let mut processor = plugin.activate(RATE, 64).unwrap();
    processor.process(&mut AudioBuffer::<f32>::new(2, 64));
    assert_eq!(
        *record.output_events_given.lock().unwrap(),
        [true],
        "the host did not pass an output event list"
    );

    let activated = record.buses_activated.lock().unwrap().clone();
    for (call, why) in [
        (
            (EVENT, OUTPUT, 0, 1),
            "the event output the host reads from",
        ),
        ((AUDIO, OUTPUT, 0, 1), "the main output"),
        ((EVENT, INPUT, 0, 1), "the event input"),
        ((AUDIO, OUTPUT, 1, 1), "an output that asks to be on"),
        ((EVENT, OUTPUT, 1, 0), "an unused bus that does not ask"),
    ] {
        assert!(
            activated.contains(&call),
            "{why} was not switched as expected; activateBus calls: {activated:?}"
        );
    }
    assert_eq!(activated.len(), 5, "{activated:?}");

    // What the plugin accepted is recorded; what it refused is remarked on.
    assert!(plugin.audio_outputs()[0].active);
    assert!(!plugin.audio_outputs()[1].active, "the plugin refused it");
    assert!(plugin.event_inputs()[0].active);
    assert!(plugin.event_outputs()[0].active);
    assert!(!plugin.event_outputs()[1].active);
    let remarks = plugin.pump();
    assert!(
        remarks.iter().any(|event| matches!(
            event,
            HostEvent::Remark(text) if text.contains("audio output bus 1")
        )),
        "{remarks:?}"
    );
    drop(processor);
}

/// VST 3 has `IAudioProcessor::getTailSamples` called on the main thread
/// only. The audio half reports the value the main thread last read: at
/// activation, and at every `pump`.
#[test]
fn the_tail_is_asked_for_on_the_main_thread_only() {
    let record = Arc::new(Record::default());
    let module = probe_module(&record);
    let mut plugin = module.create(PROBE).unwrap();
    let main = std::thread::current().id();
    let mut processor = plugin.activate(RATE, 64).unwrap();

    let (mut processor, first) = std::thread::spawn(move || {
        processor.process(&mut AudioBuffer::<f32>::new(2, 64));
        let tail = processor.tail();
        (processor, tail)
    })
    .join()
    .unwrap();
    assert_eq!(first, Tail::Frames(100));

    // The plugin's tail changes; the audio half learns of it once the main
    // thread has asked.
    record.tail.store(200, Relaxed);
    assert_eq!(processor.tail(), Tail::Frames(100));
    plugin.pump();
    let (processor, second) = std::thread::spawn(move || {
        processor.process(&mut AudioBuffer::<f32>::new(2, 64));
        let tail = processor.tail();
        (processor, tail)
    })
    .join()
    .unwrap();
    assert_eq!(second, Tail::Frames(200));
    record.tail.store(u32::MAX, Relaxed);
    assert_eq!(plugin.tail(), Tail::Infinite);
    assert_eq!(processor.tail(), Tail::Infinite);

    let asked = record.tail_asked_on.lock().unwrap().clone();
    assert!(!asked.is_empty(), "the tail was never asked for");
    assert!(
        asked.iter().all(|thread| *thread == main),
        "getTailSamples was called on the audio thread"
    );
    drop(processor);
    drop(plugin);
}
