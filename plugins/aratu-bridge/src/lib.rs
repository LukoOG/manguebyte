//! Aratu Bridge: share the music, not the audio.
//!
//! [Aratu](https://aratu.live) is a live-coding language in the browser,
//! whose sound engine is `manguebyte-web`, Rust compiled to WebAssembly.
//! This plugin runs the same engine natively, inside a DAW. Instead of
//! streaming audio from the browser, the page sends the plugin what it
//! sends its own engine (control calls, 32-byte event records and sample
//! files) over a WebSocket on 127.0.0.1, and the plugin renders them on the
//! DAW's audio device, in time with the DAW's transport: the events are
//! scheduled in beats, and land on the frame their beat falls on. The sound
//! lands on a track, where it can be recorded, mixed and processed, and no
//! audio device is shared between the browser and the DAW.
//!
//! `docs/ARATU_BRIDGE.md` specifies the protocol. The parts:
//!
//! - [`ws`], [`sha1`] and [`base64`]: WebSocket (RFC 6455), written from
//!   the RFCs, with no dependency;
//! - [`protocol`]: the messages;
//! - [`origin`]: which pages may connect;
//! - [`timeline`]: the host's transport, block by block, in beats;
//! - [`audio`]: the audio thread's side, which never allocates;
//! - [`server`]: the thread that speaks to the page and makes the parts;
//! - [`client`]: a Rust client, what the tests play through.
//!
//! The plugin has two parameters: the output gain, and the port it
//! listens on ([`DEFAULT_PORT`] unless changed). Its latency is the engine
//! master bus's look-ahead, which the host compensates.

pub mod audio;
pub mod base64;
pub mod client;
pub mod origin;
pub mod protocol;
pub mod server;
pub mod sha1;
pub mod shared;
pub mod store;
pub mod timeline;
pub mod ws;

use std::sync::Arc;
use std::time::{Duration, Instant};

use manguebyte::plugin::MidiControllers;
use manguebyte::prelude::*;
use manguebyte_web::{Engine, master_latency_frames};

use crate::audio::{Audio, FromAudio, ToAudio};
pub use crate::protocol::DEFAULT_PORT;
use crate::server::{ServerHandle, engine_config};
use crate::shared::Shared;

/// How many items the queue to the audio thread holds.
pub const TO_AUDIO_CAPACITY: usize = 1 << 16;

/// How many items the queue back from it holds.
pub const FROM_AUDIO_CAPACITY: usize = 1 << 12;

/// The plugin's parameters.
pub struct BridgeParams {
    /// The output level, in decibels, after the engine's master bus.
    pub gain: FloatParam,
    /// The TCP port the plugin listens on, on 127.0.0.1 only. Each
    /// instance needs a port of its own.
    pub port: IntParam,
}
param_set!(BridgeParams { gain, port });

impl BridgeParams {
    fn new() -> Self {
        Self {
            gain: FloatParam::new("gain", "Gain", FloatRange::linear(-60.0, 12.0), 0.0)
                .with_unit("dB")
                .with_precision(1),
            port: IntParam::new("port", "Port", 1_024, 65_535, i32::from(DEFAULT_PORT))
                .non_automatable(),
        }
    }
}

/// The plugin.
pub struct AratuBridge {
    params: Arc<BridgeParams>,
    shared: Arc<Shared>,
    audio: Option<Audio>,
    server: Option<ServerHandle>,
    gain: ParamSmoother,
    prepared: Option<(f64, usize)>,
}

impl AratuBridge {
    /// Listens on `port` instead of the parameter's port; 0 asks the system
    /// for a free one, as the tests do. `None` goes back to the parameter.
    pub fn set_port_override(&self, port: Option<u16>) {
        self.shared.set_port_override(port);
    }

    /// The port the plugin listens on, if it listens.
    pub fn local_port(&self) -> Option<u16> {
        self.shared.bound_port()
    }

    /// Waits up to `timeout` until the plugin listens on the port it is
    /// asked for, and returns it.
    pub fn wait_listening(&self, timeout: Duration) -> Option<u16> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            let wanted = self
                .shared
                .port_override()
                .unwrap_or(self.params.port.value() as u16);
            if let Some((requested, port)) = self.shared.bound() {
                if requested == wanted {
                    return Some(port);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    /// Whether a page is connected.
    pub fn connected(&self) -> bool {
        self.shared
            .connected
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The counters the audio thread keeps, if the plugin is active.
    pub fn status(&self) -> Option<protocol::Status> {
        self.audio.as_ref().map(Audio::status)
    }

    /// The engine the plugin renders with, if it is active.
    pub fn engine(&self) -> Option<&Engine> {
        self.audio.as_ref().map(Audio::engine)
    }
}

impl Plugin for AratuBridge {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.aratu-bridge",
        name: "Aratu Bridge",
        vendor: "Manguebyte",
        version: env!("CARGO_PKG_VERSION"),
        description: "Plays an Aratu page's music in the DAW, in time with its transport",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    // The page plays it; the host's MIDI is not listened to.
    const MIDI_CONTROLLERS: MidiControllers = MidiControllers::NONE;
    type Params = BridgeParams;

    fn new() -> Self {
        let mut gain = ParamSmoother::new(Smoothing::Linear(20.0));
        gain.prepare(48_000.0, 1.0);
        Self {
            params: Arc::new(BridgeParams::new()),
            shared: Arc::new(Shared::default()),
            audio: None,
            server: None,
            gain,
            prepared: None,
        }
    }

    fn params(&self) -> Arc<BridgeParams> {
        Arc::clone(&self.params)
    }

    fn prepare(&mut self, spec: &ProcessSpec) {
        let rate = spec.sample_rate;
        let quantum = spec.max_block_size.clamp(1, 8_192);
        let current = self.gain.target();
        self.gain.prepare(rate, current);
        if self.prepared == Some((rate, quantum)) {
            return;
        }
        let Ok(engine) = Engine::new(engine_config(rate, quantum)) else {
            self.audio = None;
            return;
        };
        let latency = master_latency_frames(rate) as u32;
        self.shared.set_sample_rate(rate);
        self.shared
            .max_quantum
            .store(quantum as u32, std::sync::atomic::Ordering::Release);
        self.shared.max_block.store(
            spec.max_block_size as u32,
            std::sync::atomic::Ordering::Release,
        );
        self.shared
            .latency
            .store(latency, std::sync::atomic::Ordering::Release);
        self.shared
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.prepared = Some((rate, quantum));
        // A new engine and new queues; the old server goes with the old
        // queues, and a new one listens.
        self.server = None;
        let (to_audio, incoming) = spsc::channel::<ToAudio>(TO_AUDIO_CAPACITY);
        let (outgoing, from_audio) = spsc::channel::<FromAudio>(FROM_AUDIO_CAPACITY);
        self.audio = Some(Audio::new(Box::new(engine), incoming, outgoing));
        let params = Arc::clone(&self.params);
        let port = Box::new(move || params.port.value().clamp(1_024, 65_535) as u16);
        let (origins, _) = origin::AllowList::from_environment();
        self.shared
            .shutdown
            .store(false, std::sync::atomic::Ordering::Release);
        self.server = Some(server::spawn(
            Arc::clone(&self.shared),
            port,
            to_audio,
            from_audio,
            origins,
        ));
    }

    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        let Some(audio) = self.audio.as_mut() else {
            buffer.clear();
            return;
        };
        audio.process(buffer, context.transport, &self.shared);
        let target = decibels::db_to_gain(f64::from(self.params.gain.value())) as f32;
        if target != self.gain.target() {
            self.gain.set_target(target);
        }
        if self.gain.is_smoothing() {
            let frames = buffer.num_frames();
            for frame in 0..frames {
                let gain = self.gain.next_value();
                for channel in buffer.iter_channels_mut() {
                    channel[frame] *= gain;
                }
            }
        } else {
            let gain = self.gain.current();
            if gain != 1.0 {
                buffer.apply_gain(gain);
            }
        }
    }

    fn latency_samples(&self) -> usize {
        self.prepared
            .map_or(0, |(rate, _)| master_latency_frames(rate))
    }
}

impl Drop for AratuBridge {
    fn drop(&mut self) {
        // Stops the server first, so that nothing is sent to an engine
        // that is gone.
        self.server = None;
    }
}

manguebyte::clap::export_clap!(AratuBridge);
manguebyte::vst3::export_vst3!(AratuBridge);
manguebyte_au::export_au!(AratuBridge);
