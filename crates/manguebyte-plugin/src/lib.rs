//! The plugin API: write a processor once and run it anywhere.
//!
//! A [`Plugin`] is a set of parameters plus a function that processes audio
//! and events. It knows nothing about any plugin format. Format wrappers
//! (CLAP, VST3 and the others), the standalone application wrapper and the
//! test [`Harness`] all drive the same trait, so what is tested here is what
//! runs in a host.
//!
//! ```
//! use std::sync::Arc;
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::{ProcessContext, ProcessSpec};
//! use manguebyte_params::{FloatParam, FloatRange, param_set};
//! use manguebyte_plugin::{AudioLayout, Harness, Plugin, PluginInfo, PluginKind};
//!
//! struct GainParams { gain: FloatParam }
//! param_set!(GainParams { gain });
//!
//! struct Gain { params: Arc<GainParams> }
//!
//! impl Plugin for Gain {
//!     const INFO: PluginInfo = PluginInfo {
//!         id: "org.example.gain",
//!         name: "Gain",
//!         vendor: "Example",
//!         version: "1.0.0",
//!         description: "Turns things up and down",
//!         kind: PluginKind::Effect,
//!     };
//!     const LAYOUT: AudioLayout = AudioLayout::STEREO_EFFECT;
//!     type Params = GainParams;
//!
//!     fn new() -> Self {
//!         let range = FloatRange::linear(0.0, 2.0);
//!         Self { params: Arc::new(GainParams { gain: FloatParam::new("gain", "Gain", range, 1.0) }) }
//!     }
//!     fn params(&self) -> Arc<GainParams> { Arc::clone(&self.params) }
//!     fn prepare(&mut self, _: &ProcessSpec) {}
//!     fn process(&mut self, buffer: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {
//!         buffer.apply_gain(self.params.gain.value());
//!     }
//! }
//!
//! // No host needed to try it out.
//! let mut harness = Harness::<Gain>::new(48_000.0, 256);
//! harness.params().gain.set(0.5);
//!
//! let mut input = AudioBuffer::<f32>::new(2, 100);
//! input.channel_mut(0).fill(1.0);
//! let output = harness.process(&input);
//! assert_eq!(output.channel(0), &[0.5; 100]);
//! ```

mod harness;
mod state;

use std::sync::Arc;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event, EventBuffer, EventKind};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_params::ParamSet;

pub use harness::Harness;
pub use state::{StateError, load_state, save_state};

/// What sort of plugin this is, so a host can list it in the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    /// Makes sound from notes.
    Instrument,
    /// Changes sound.
    Effect,
    /// Changes or generates notes without touching sound.
    NoteEffect,
    /// Measures sound without changing it.
    Analyzer,
}

/// The fixed facts about a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginInfo {
    /// A globally unique identifier in reverse-domain form, such as
    /// `"com.example.reverb"`. Hosts store it in sessions, so it must never
    /// change once a version has shipped.
    pub id: &'static str,
    /// The name shown to people.
    pub name: &'static str,
    /// Who makes it.
    pub vendor: &'static str,
    /// Its version, such as `"1.2.0"`.
    pub version: &'static str,
    /// One line about what it does.
    pub description: &'static str,
    /// What sort of plugin it is.
    pub kind: PluginKind,
}

/// How many channels a plugin reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioLayout {
    /// Channels of the main input. Zero for an instrument.
    pub inputs: usize,
    /// Channels of the main output.
    pub outputs: usize,
    /// Channels of the sidechain input. Zero if there is none.
    pub sidechain: usize,
}

impl AudioLayout {
    /// Stereo in, stereo out.
    pub const STEREO_EFFECT: Self = Self {
        inputs: 2,
        outputs: 2,
        sidechain: 0,
    };
    /// Mono in, mono out.
    pub const MONO_EFFECT: Self = Self {
        inputs: 1,
        outputs: 1,
        sidechain: 0,
    };
    /// No input, stereo out.
    pub const STEREO_INSTRUMENT: Self = Self {
        inputs: 0,
        outputs: 2,
        sidechain: 0,
    };
}

/// An audio plugin, independent of any plugin format.
///
/// # Threads
///
/// The plugin value itself lives on the audio thread: [`process`] and
/// [`reset`] are called there and must not allocate, lock, block or panic.
/// [`prepare`] is called while audio is stopped and may take its time.
///
/// Everything another thread needs goes through the parameters, which are
/// shared in an [`Arc`] and are safe to read and write from anywhere. A host
/// saving or restoring state, a user interface moving a control and the
/// audio thread reading a value never need to coordinate.
///
/// [`process`]: Self::process
/// [`reset`]: Self::reset
/// [`prepare`]: Self::prepare
pub trait Plugin: Send + 'static {
    /// The fixed facts about the plugin.
    const INFO: PluginInfo;
    /// How many channels it reads and writes.
    const LAYOUT: AudioLayout;
    /// Its parameters.
    type Params: ParamSet + 'static;

    /// Creates the plugin, with its parameters at their defaults.
    fn new() -> Self
    where
        Self: Sized;

    /// The plugin's parameters, shared with whoever else needs them.
    fn params(&self) -> Arc<Self::Params>;

    /// Gets ready to process under `spec`. Called before the first block and
    /// again whenever the sample rate or block size changes.
    fn prepare(&mut self, spec: &ProcessSpec);

    /// Processes one block in place.
    ///
    /// `buffer` has [`LAYOUT.outputs`](AudioLayout::outputs) channels. The
    /// first [`LAYOUT.inputs`](AudioLayout::inputs) of them hold the input;
    /// any others are silent. `context` carries the events of the block with
    /// the frame each falls on, the transport, and the sidechain if there is
    /// one.
    ///
    /// A plugin can learn about parameter changes in either of two ways,
    /// and both are always correct:
    ///
    /// - **Poll.** The parameters already hold the value they will have at
    ///   the end of the block. Reading them once per block is all a simple
    ///   plugin needs.
    /// - **Follow the events.** Every change is also in `context.events`
    ///   with its exact frame, whoever made it: host automation, a user
    ///   interface writing to the parameter, or restored state. A plugin
    ///   that tracks a value from these events alone never misses a change
    ///   and never needs to poll.
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>);

    /// Clears internal state such as filter memory and tails, without
    /// touching parameters.
    fn reset(&mut self) {}

    /// How many frames late the output is compared with the input.
    fn latency_samples(&self) -> usize {
        0
    }
}

/// Notices parameter changes that did not come from the host, and turns
/// them into events.
///
/// A parameter can be written at any moment by a user interface or by a
/// state restore. Those writes go straight to the parameter and would
/// otherwise be visible only to a plugin that polls. A wrapper keeps one of
/// these, and before each block asks it to add a [`EventKind::ParamValue`]
/// event at frame 0 for every parameter whose value is not what the plugin
/// last saw. The event stream is then a complete record of every change.
///
/// All methods after [`new`](Self::new) are real-time safe.
#[derive(Debug)]
pub struct ParamWatcher {
    /// The value of each parameter as the plugin last saw it, in the order
    /// the set visits them.
    seen: Vec<f64>,
}

impl ParamWatcher {
    /// Starts watching `params` from their current values. Allocates.
    pub fn new(params: &dyn ParamSet) -> Self {
        let mut seen = Vec::with_capacity(params.len());
        params.visit(&mut |param| seen.push(param.plain()));
        Self { seen }
    }

    /// Adds an event at frame 0 for every parameter that has changed since
    /// [`remember`](Self::remember) was last called.
    ///
    /// Call it on an empty buffer, before the host's own events are added,
    /// so that host automation on frame 0 comes later in the stream and
    /// takes precedence. If the buffer fills up, the remaining changes are
    /// picked up on the next block.
    pub fn emit_changes(&mut self, params: &dyn ParamSet, events: &mut EventBuffer) {
        let mut seen = self.seen.iter_mut();
        params.visit(&mut |param| {
            let Some(seen) = seen.next() else {
                return;
            };
            let value = param.plain();
            if value != *seen {
                let kind = EventKind::ParamValue {
                    param: param.param_id(),
                    value,
                };
                if events.push(Event { offset: 0, kind }).is_ok() {
                    *seen = value;
                }
            }
        });
    }

    /// Records the parameters' values as seen. Call it after the block's
    /// events have been stored with [`apply_param_events`].
    pub fn remember(&mut self, params: &dyn ParamSet) {
        let mut seen = self.seen.iter_mut();
        params.visit(&mut |param| {
            if let Some(seen) = seen.next() {
                *seen = param.plain();
            }
        });
    }
}

/// Stores every parameter value found in `events` into `params`.
///
/// A wrapper calls this before each block so that anything polling a
/// parameter sees the value automation has given it. When a parameter
/// changes more than once in the block, the last change wins. Real-time
/// safe.
pub fn apply_param_events(params: &dyn ParamSet, events: &EventBuffer) {
    for event in events {
        if let EventKind::ParamValue { param, value } = event.kind {
            if let Some(target) = params.find_param(param) {
                target.set_plain(value);
            }
        }
    }
}
