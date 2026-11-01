//! Runs a [`Plugin`] as a standalone application.
//!
//! The same plugin that is exported to a host can be played directly: its
//! audio goes to the sound card, its notes come from the MIDI keyboards
//! the settings name (every connected one by default), and an effect takes
//! its input from an input device. A plugin that sends notes
//! ([`Plugin::NOTE_OUTPUT`]) can send them to a MIDI output port, as MIDI
//! 1.0 or MPE, with [`Standalone::start_with_midi_output`].
//!
//! The devices are those of [`StandaloneOptions::settings`]: devices by
//! stable identifier, sample rate, buffer size and MIDI input ports, with
//! fallbacks for what is missing, reported in [`Standalone::report`]. While
//! the plugin plays, the program lists what can be chosen
//! ([`Standalone::choices`]) and changes any of it
//! ([`Standalone::apply_settings`] and the `set_` methods) without making
//! the plugin again: the same instance plays on, its parameters, state and
//! editor untouched, and is prepared again only when its sample rate
//! changes, as [`RatePolicy`] says. [`Standalone::settings`] is the value to
//! save with the session and hand back in the options of the next run.
//! [ADR 0021](https://github.com/alessonqueirozdev-hub/manguebyte/blob/main/docs/adr/0021-standalone-device-settings.md)
//! tells why it works this way.
//!
//! The plugin plays with a real transport: a [`TransportClock`] that
//! starts playing at [`StandaloneOptions::tempo_bpm`], and that the program
//! works through [`Standalone::transport`]: play, stop, seek, loop, and a
//! new tempo map with tempo changes, ramps and time signatures while it
//! plays.
//! The plugin sees it as a host's transport, and the map in
//! `ProcessContext::timeline`.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use manguebyte_core::AudioBuffer;
//! # use manguebyte_dsp::{ProcessContext, ProcessSpec};
//! # use manguebyte_params::param_set;
//! # use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};
//! # struct NoParams {}
//! # param_set!(NoParams {});
//! # struct MySynth(Arc<NoParams>);
//! # impl Plugin for MySynth {
//! #     const INFO: PluginInfo = PluginInfo { id: "x", name: "x", vendor: "x", version: "1", description: "x", kind: PluginKind::Instrument };
//! #     const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
//! #     type Params = NoParams;
//! #     fn new() -> Self { Self(Arc::new(NoParams {})) }
//! #     fn params(&self) -> Arc<NoParams> { Arc::clone(&self.0) }
//! #     fn prepare(&mut self, _: &ProcessSpec) {}
//! #     fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
//! # }
//! use manguebyte_midi::MidiMessage;
//! use manguebyte_standalone::{Standalone, StandaloneOptions, StandaloneSettings};
//!
//! // The settings saved with the last session, or the defaults.
//! let saved: Option<String> = None;
//! let mut options = StandaloneOptions::default();
//! options.settings = saved
//!     .and_then(|text| text.parse::<StandaloneSettings>().ok())
//!     .unwrap_or_default();
//!
//! let mut app = Standalone::<MySynth>::start(options)?;
//! println!("{}", app.report());
//!
//! // Notes can also come from the program itself: a computer keyboard, an
//! // on-screen one, a sequencer.
//! app.send(MidiMessage::NoteOn { channel: 0, note: 60, velocity: 100 });
//!
//! // A settings screen: what can be chosen, and a change, while it plays.
//! let choices = app.choices()?;
//! if let Some(other) = choices.outputs.iter().find(|option| !option.is_default) {
//!     let report = app.set_output_device(&other.device.id);
//!     println!("{report}");
//! }
//!
//! // When the program quits, the settings go with the session.
//! let to_save = app.settings().to_string();
//! # let _ = to_save;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod engine;
mod settings;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use manguebyte_core::spsc::{self, Producer};
use manguebyte_devices::midi::{
    MidiError, MidiOutput, MidiOutputThread, MidiProtocol, midi_output_queue,
};
use manguebyte_devices::{
    DeviceChoice, DeviceError, DeviceEvent, DeviceInfo, DeviceManager, DeviceRef, DeviceSetup,
    DeviceSnapshot, DeviceSummary, Direction, EngineFormat, Fallback, HostInfo, RestoreReport,
};
use manguebyte_dsp::timeline::{ClockControl, TempoMap, TimeSignature, TransportClock};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::Zone;
use manguebyte_params::LoadReport;
use manguebyte_plugin::output::OutputProtocol;
use manguebyte_plugin::{Plugin, StateError, load_state, save_state};

pub use engine::Engine;
pub use settings::{
    COMMON_BUFFER_SIZES, COMMON_SAMPLE_RATES, DeviceOption, MidiInputs, MidiPortOption, RatePolicy,
    SettingsChoices, SettingsReport, StandaloneSettings,
};

/// How many packets can wait for the MIDI output thread: a dense part
/// with expressions sends a few thousand a second.
const MIDI_OUTPUT_QUEUE: usize = 8_192;

/// The sample rate the plugin starts at when no device tells it one.
const FALLBACK_SAMPLE_RATE: u32 = 48_000;

/// How to run a plugin standalone.
#[derive(Debug, Clone, PartialEq)]
pub struct StandaloneOptions {
    /// Largest block the plugin will be asked to process, in frames. A
    /// device whose buffer is longer has it cut into blocks of this size.
    pub block_size: usize,
    /// The devices, sample rate, buffer size and MIDI input ports to open:
    /// what the program saved from [`Standalone::settings`] in an earlier
    /// run, or the defaults.
    pub settings: StandaloneSettings,
    /// Whether the plugin follows the device's sample rate or keeps one
    /// of its own; it follows by default.
    pub rate_policy: RatePolicy,
    /// Tempo the transport starts at, in beats per minute, in 4/4; 120 if
    /// it is not a positive, finite number. [`Standalone::transport`] can
    /// hand it a whole tempo map later.
    pub tempo_bpm: f64,
    /// Where the plugin's auxiliary output buses go: for each bus in
    /// order, the device channel of its first channel, or `None` to leave
    /// it unheard, as [`Engine::with_aux_channels`] takes them. Empty, the
    /// default, sends each bus that is on by default to the channels after
    /// the main output's, one bus after another.
    pub aux_channels: &'static [Option<usize>],
}

impl Default for StandaloneOptions {
    fn default() -> Self {
        Self {
            block_size: 512,
            settings: StandaloneSettings::default(),
            rate_policy: RatePolicy::default(),
            tempo_bpm: 120.0,
            aux_channels: &[],
        }
    }
}

/// Why a plugin could not be started standalone.
#[derive(Debug)]
pub enum StandaloneError {
    /// The audio device could not be opened or started.
    Device(DeviceError),
    /// The plugin's parameters are not well formed.
    InvalidPlugin(String),
}

impl fmt::Display for StandaloneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(f, "{error}"),
            Self::InvalidPlugin(reason) => write!(f, "the plugin is not well formed: {reason}"),
        }
    }
}

impl std::error::Error for StandaloneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(error) => Some(error),
            Self::InvalidPlugin(_) => None,
        }
    }
}

impl From<DeviceError> for StandaloneError {
    fn from(error: DeviceError) -> Self {
        Self::Device(error)
    }
}

/// A plugin running on the sound card. Audio stops when it is dropped.
///
/// The plugin is made once, when it starts, and runs under a
/// [`DeviceManager`] for as long as this lives: changing devices, rate,
/// buffer size or MIDI ports keeps the same instance, so its parameters
/// ([`params`](Self::params)), its state and any editor bound to them stay
/// valid. Changes are made from the program's own thread, never the audio
/// thread, and each returns a [`SettingsReport`] of what it opened.
pub struct Standalone<P: Plugin> {
    params: Arc<P::Params>,
    notes: Mutex<Producer<MidiMessage>>,
    transport: ClockControl,
    /// Runs the engine. Before the MIDI output thread, which it feeds, so
    /// that it is dropped first, and the thread sends what the last blocks
    /// left.
    manager: DeviceManager,
    settings: StandaloneSettings,
    rate_policy: RatePolicy,
    /// The format the engine runs at: the plugin's rate, the largest
    /// block, its inputs and the output device's channels.
    format: EngineFormat,
    report: SettingsReport,
    midi_problems: Vec<MidiError>,
    /// The thread that sends the plugin's notes to the MIDI output port,
    /// and the port's name.
    midi_output: Option<(MidiOutputThread, String)>,
}

/// Where a plugin that sends notes sends them when it runs standalone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiOutputOptions {
    /// The output port, as [`MidiPortInfo::id`] names it.
    ///
    /// [`MidiPortInfo::id`]: manguebyte_devices::midi::MidiPortInfo::id
    pub port_id: String,
    /// For a port that speaks MIDI 1.0, as every port does for now
    /// ([`MidiProtocol`]): `Some` writes the plugin's notes as MPE in this
    /// zone, so that each keeps its own tuning, pressure and brightness,
    /// and `None` as plain MIDI 1.0 by the default translation, which
    /// keeps polyphonic pressure alone of them.
    pub mpe: Option<Zone>,
}

impl MidiOutputOptions {
    /// Plain MIDI 1.0 to the port `port_id`.
    pub fn new(port_id: impl Into<String>) -> Self {
        Self {
            port_id: port_id.into(),
            mpe: None,
        }
    }

    /// MPE in a lower zone of all fifteen member channels instead.
    #[must_use]
    pub fn mpe(mut self) -> Self {
        self.mpe = Zone::lower(15);
        self
    }
}

impl<P: Plugin> Standalone<P> {
    /// Creates the plugin and starts playing it on the system's devices,
    /// those of [`StandaloneOptions::settings`].
    ///
    /// Whatever the settings name that cannot be had falls back, as
    /// [`report`](Self::report) says: a missing device to one with its name
    /// or to the default, a rate or buffer size the device cannot do to one
    /// it can, an input that cannot run with the output to none (the effect
    /// hears silence), a MIDI port that cannot be opened left out, because
    /// a missing keyboard should not stop the sound. With no output device
    /// at all the plugin starts and waits; [`poll_devices`](Self::poll_devices)
    /// starts it when one appears.
    pub fn start(options: StandaloneOptions) -> Result<Self, StandaloneError> {
        Self::start_on(DeviceManager::new(), options, None)
    }

    /// [`start`](Self::start), and sends what the plugin sends out, from
    /// [`Plugin::process_with_output`], to a MIDI output port, each packet
    /// on time (see [`MidiTransmitter`](manguebyte_devices::midi::MidiTransmitter)).
    ///
    /// For a plugin that does not set [`Plugin::NOTE_OUTPUT`] the port is
    /// not opened. A port that cannot be opened is listed in
    /// [`midi_problems`](Self::midi_problems), and the plugin plays on.
    pub fn start_with_midi_output(
        options: StandaloneOptions,
        midi_output: MidiOutputOptions,
    ) -> Result<Self, StandaloneError> {
        Self::start_on(DeviceManager::new(), options, Some(midi_output))
    }

    /// [`start`](Self::start) on the devices of `manager` instead of the
    /// system's: one made with [`DeviceManager::headless`] for tests and
    /// for running without hardware, or with
    /// [`DeviceManager::with_audio_backend`] for a driver model of the
    /// program's own. The manager is the standalone's from then on; its
    /// crossfade, error handler and drift correction, set before, are
    /// kept. `midi_output` is as in
    /// [`start_with_midi_output`](Self::start_with_midi_output), on the
    /// system's MIDI ports.
    pub fn start_on(
        mut manager: DeviceManager,
        options: StandaloneOptions,
        midi_output: Option<MidiOutputOptions>,
    ) -> Result<Self, StandaloneError> {
        let block_size = options.block_size.max(1);
        let mut midi_problems = Vec::new();
        let ports = manager
            .devices()
            .map(|snapshot| snapshot.midi_inputs)
            .unwrap_or_default();
        let setup = Self::device_setup(&options.settings, options.rate_policy, &ports);
        let format = Self::planned_format(
            &manager,
            &setup,
            options.rate_policy,
            Self::fallback_format(block_size),
        );

        let (notes, note_receiver) = spsc::channel(1_024);
        let engine = Engine::<P>::new(
            f64::from(format.sample_rate),
            block_size,
            options.tempo_bpm,
            Vec::new(),
            note_receiver,
        )
        .map_err(StandaloneError::InvalidPlugin)?;
        let map = TempoMap::new(options.tempo_bpm, TimeSignature::FOUR_FOUR).unwrap_or_default();
        let (mut clock, transport) =
            TransportClock::with_control(map, f64::from(format.sample_rate));
        clock.play();
        let mut engine = engine
            .with_transport_clock(clock)
            .with_aux_channels(options.aux_channels);
        let params = engine.params();

        let mut sender = None;
        if let Some(choice) = midi_output.filter(|_| P::NOTE_OUTPUT) {
            match MidiOutput::open(&choice.port_id) {
                Ok(port) => {
                    let protocol = match (port.protocol(), choice.mpe) {
                        (MidiProtocol::Midi1, Some(zone)) => OutputProtocol::Mpe(zone),
                        (MidiProtocol::Midi1, None) => OutputProtocol::Midi1,
                        // A port that speaks MIDI 2.0, when there are any.
                        _ => OutputProtocol::Midi2,
                    };
                    let (transmitter, queue) = midi_output_queue(MIDI_OUTPUT_QUEUE);
                    engine = engine.with_midi_output(transmitter, protocol);
                    let name = port.name().to_owned();
                    sender = Some((port.spawn(queue), name));
                }
                Err(error) => midi_problems.push(error),
            }
        }

        let restored = manager.start_engine(&setup, format, engine)?;
        // The first poll only learns what is there.
        let _ = manager.poll_devices();
        let mut app = Self {
            params,
            notes: Mutex::new(notes),
            transport,
            manager,
            settings: options.settings,
            rate_policy: options.rate_policy,
            format,
            report: SettingsReport::default(),
            midi_problems,
            midi_output: sender,
        };
        let mut fallbacks = restored.fallbacks;
        let mut error = None;
        match app.correct_format(&setup) {
            Ok(Some((report, _))) => fallbacks = report.fallbacks,
            Ok(None) => {}
            Err(problem) => error = Some(problem.to_string()),
        }
        app.report = app.current_report(fallbacks, false, error);
        Ok(app)
    }

    /// The format to start with when nothing is known of the devices.
    fn fallback_format(block_size: usize) -> EngineFormat {
        EngineFormat::new(
            FALLBACK_SAMPLE_RATE,
            block_size,
            P::LAYOUT.inputs,
            P::LAYOUT.outputs.max(2),
        )
    }

    /// The format the engine should run at on the devices `setup` would
    /// open now: the device's rate under [`RatePolicy::FollowDevice`], and
    /// the device's output channels. What cannot be known is taken from
    /// `current`.
    fn planned_format(
        manager: &DeviceManager,
        setup: &DeviceSetup,
        policy: RatePolicy,
        current: EngineFormat,
    ) -> EngineFormat {
        let preview = manager.preview(setup).ok();
        let sample_rate = match policy {
            RatePolicy::Fixed(rate) if rate > 0 => rate,
            _ => preview
                .as_ref()
                .and_then(|preview| preview.sample_rate())
                .unwrap_or(current.sample_rate),
        };
        let output_channels = preview
            .as_ref()
            .and_then(|preview| preview.output.as_ref())
            .and_then(|(device, settings)| {
                settings.channels.or_else(|| {
                    manager
                        .device_info(Direction::Output, &device.id)
                        .ok()?
                        .default_channels
                })
            })
            .map_or(current.output_channels, usize::from);
        EngineFormat {
            sample_rate,
            output_channels: output_channels.max(1),
            ..current
        }
    }

    /// The device setup the settings stand for now.
    fn setup(&self) -> DeviceSetup {
        let ports = match self.settings.midi_inputs {
            MidiInputs::All => self
                .manager
                .devices()
                .map(|snapshot| snapshot.midi_inputs)
                .unwrap_or_default(),
            MidiInputs::Only(_) => Vec::new(),
        };
        Self::device_setup(&self.settings, self.rate_policy, &ports)
    }

    /// The device setup `settings` stand for, with `ports` as the MIDI
    /// input ports connected. Under a fixed rate, the devices are asked
    /// for the plugin's rate unless the settings name one, so that they
    /// are resampled only when they cannot run at it.
    fn device_setup(
        settings: &StandaloneSettings,
        policy: RatePolicy,
        ports: &[DeviceSummary],
    ) -> DeviceSetup {
        let mut setup = settings.to_setup(P::LAYOUT.inputs > 0, ports);
        if let (None, RatePolicy::Fixed(rate)) = (setup.sample_rate, policy) {
            setup.sample_rate = (rate > 0).then_some(rate);
        }
        setup
    }

    /// Whether the plugin should be at another format on what runs now:
    /// another rate when it follows the device, or the device's channels.
    /// If so, applies `setup` with it, and returns what that reported and
    /// whether the rate changed.
    fn correct_format(
        &mut self,
        setup: &DeviceSetup,
    ) -> Result<Option<(RestoreReport, bool)>, DeviceError> {
        let Some(running) = self.manager.format() else {
            return Ok(None);
        };
        let sample_rate = match self.rate_policy {
            RatePolicy::Fixed(rate) if rate > 0 => rate,
            _ => running.sample_rate.round() as u32,
        };
        let output_channels = match running.output_channels {
            0 => self.format.output_channels,
            channels => channels,
        };
        let wanted = EngineFormat {
            sample_rate,
            output_channels,
            ..self.format
        };
        if wanted == self.format || sample_rate == 0 {
            return Ok(None);
        }
        let rate_changed = wanted.sample_rate != self.format.sample_rate;
        self.format = wanted;
        let report = self.manager.apply_with_format(setup, wanted)?;
        Ok(Some((report, rate_changed)))
    }

    /// Opens `setup`, at the format it needs. Returns what was reported
    /// and whether the plugin's rate changed.
    fn open(&mut self, setup: &DeviceSetup) -> Result<(RestoreReport, bool), DeviceError> {
        let wanted = Self::planned_format(&self.manager, setup, self.rate_policy, self.format);
        let mut rate_changed = wanted.sample_rate != self.format.sample_rate;
        let mut report = if wanted == self.format {
            self.manager.apply(setup)?
        } else {
            self.format = wanted;
            self.manager.apply_with_format(setup, wanted)?
        };
        // What opened can still differ from what was planned: a driver
        // that could not be described while another ran, another host.
        if let Some((corrected, changed)) = self.correct_format(setup)? {
            report = corrected;
            rate_changed |= changed;
        }
        Ok((report, rate_changed))
    }

    /// What runs now, as a report.
    fn current_report(
        &self,
        mut fallbacks: Vec<Fallback>,
        prepared_again: bool,
        error: Option<String>,
    ) -> SettingsReport {
        // A rate the settings did not ask for, but a fixed rate did, is
        // not a fallback: the device is resampled, as the report says.
        if self.settings.sample_rate.is_none() {
            fallbacks.retain(|fallback| !matches!(fallback, Fallback::SampleRate { .. }));
        }
        let manager = &self.manager;
        let format = manager.format();
        SettingsReport {
            output: manager.running_device(Direction::Output).cloned(),
            input: manager.running_device(Direction::Input).cloned(),
            device_sample_rate: format.map(|format| format.sample_rate.round() as u32),
            buffer_size: manager
                .running_settings(Direction::Output)
                .and_then(|settings| settings.buffer_size),
            output_channels: format
                .map(|format| format.output_channels)
                .filter(|&channels| channels > 0),
            plugin_sample_rate: self.format.sample_rate,
            prepared_again,
            midi_inputs: manager.midi_inputs(),
            fallbacks,
            error,
            swap: manager.last_swap(),
        }
    }

    /// Changes every setting at once, while the plugin plays, and returns
    /// what was opened.
    ///
    /// The same plugin goes on. Under [`RatePolicy::FollowDevice`], when
    /// the devices run at another rate, the old device fades out, the
    /// plugin is prepared again at the new rate and the new device fades
    /// in; otherwise the new devices take over from the old ones, with a
    /// crossfade where the system can play both, and a device at another
    /// rate than the plugin's is resampled. MIDI ports open and close
    /// without touching the audio.
    ///
    /// Whatever cannot be had falls back, and the report lists it. A
    /// change that cannot be made at all is undone: the earlier settings
    /// are opened again and the report's [`error`](SettingsReport::error)
    /// says what failed.
    pub fn apply_settings(&mut self, settings: StandaloneSettings) -> &SettingsReport {
        let earlier = std::mem::replace(&mut self.settings, settings);
        let setup = self.setup();
        let report = match self.open(&setup) {
            Ok((restored, rate_changed)) => {
                self.current_report(restored.fallbacks, rate_changed, None)
            }
            Err(error) => {
                self.settings = earlier;
                let setup = self.setup();
                match self.open(&setup) {
                    Ok((restored, rate_changed)) => self.current_report(
                        restored.fallbacks,
                        rate_changed,
                        Some(error.to_string()),
                    ),
                    Err(again) => self.current_report(
                        Vec::new(),
                        false,
                        Some(format!(
                            "{error}; the earlier settings could not be opened again either: {again}"
                        )),
                    ),
                }
            }
        };
        self.report = report;
        &self.report
    }

    /// Changes the audio host, by [`HostInfo::name`], or goes back to the
    /// system's default with `None`. The devices are the host's defaults
    /// unless the settings name devices it has.
    pub fn set_host(&mut self, host: Option<&str>) -> &SettingsReport {
        let mut settings = self.settings.clone();
        settings.host = host.map(str::to_owned);
        self.apply_settings(settings)
    }

    /// Changes the output device: [`DeviceChoice::Default`] follows the
    /// system's default.
    pub fn set_output(&mut self, choice: DeviceChoice) -> &SettingsReport {
        let mut settings = self.settings.clone();
        settings.output = choice;
        self.apply_settings(settings)
    }

    /// Changes the output device to the one with this identifier, among
    /// those [`choices`](Self::choices) lists, so that its name is saved
    /// with it.
    pub fn set_output_device(&mut self, id: &str) -> &SettingsReport {
        let choice = self.named(Direction::Output, id);
        self.set_output(choice)
    }

    /// Changes the input device, for a plugin with inputs:
    /// [`DeviceChoice::None`] turns the input off and the plugin hears
    /// silence.
    pub fn set_input(&mut self, choice: DeviceChoice) -> &SettingsReport {
        let mut settings = self.settings.clone();
        settings.input = choice;
        self.apply_settings(settings)
    }

    /// Changes the input device to the one with this identifier.
    pub fn set_input_device(&mut self, id: &str) -> &SettingsReport {
        let choice = self.named(Direction::Input, id);
        self.set_input(choice)
    }

    /// A device choice by identifier, with the device's name when it is
    /// connected.
    fn named(&self, direction: Direction, id: &str) -> DeviceChoice {
        let name = self
            .manager
            .devices()
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .devices(direction)
                    .iter()
                    .find(|device| device.id == id)
                    .map(|device| device.name.clone())
            })
            .unwrap_or_default();
        DeviceChoice::Device(DeviceRef::new(id, name))
    }

    /// Changes the sample rate asked of the devices, or leaves it to them
    /// with `None`.
    pub fn set_sample_rate(&mut self, sample_rate: Option<u32>) -> &SettingsReport {
        let mut settings = self.settings.clone();
        settings.sample_rate = sample_rate;
        self.apply_settings(settings)
    }

    /// Changes the hardware buffer size, or leaves it to the device with
    /// `None`. The plugin is not prepared again: its largest block stays
    /// [`StandaloneOptions::block_size`].
    pub fn set_buffer_size(&mut self, frames: Option<u32>) -> &SettingsReport {
        let mut settings = self.settings.clone();
        settings.buffer_size = frames;
        self.apply_settings(settings)
    }

    /// Opens or closes one MIDI input port, by identifier, without
    /// touching the audio. Closing a port while every port is listened to
    /// ([`MidiInputs::All`]) turns the settings into a list of the ports
    /// open now but that one. A port that cannot be opened is reported as
    /// a fallback.
    pub fn set_midi_input(&mut self, id: &str, open: bool) -> &SettingsReport {
        let connected = self
            .manager
            .devices()
            .map(|snapshot| snapshot.midi_inputs)
            .unwrap_or_default();
        let port = connected
            .iter()
            .find(|port| port.id == id)
            .map_or_else(|| DeviceRef::new(id, String::new()), DeviceRef::from);
        let mut fallbacks = Vec::new();
        if open {
            if let MidiInputs::Only(ports) = &mut self.settings.midi_inputs {
                if !ports.iter().any(|known| known.id == id) {
                    ports.push(port.clone());
                }
            }
            if let Err(error) = self.manager.enable_midi_input(port.clone()) {
                fallbacks.push(Fallback::MidiPortMissing {
                    direction: Direction::Input,
                    wanted: port,
                    reason: error.to_string(),
                });
            }
        } else {
            match &mut self.settings.midi_inputs {
                MidiInputs::All => {
                    let open = self.manager.midi_inputs();
                    self.settings.midi_inputs = MidiInputs::Only(
                        open.iter()
                            .filter(|port| port.id != id)
                            .map(DeviceRef::from)
                            .collect(),
                    );
                }
                MidiInputs::Only(ports) => ports.retain(|known| known.id != id),
            }
            self.manager.disable_midi_input(id);
        }
        self.report = self.current_report(fallbacks, false, None);
        &self.report
    }

    /// Lists the devices and follows what changed since the last call, as
    /// a host does: a device of the settings that comes back is used again,
    /// a device that goes away is replaced by the default, a MIDI port of
    /// the settings (or any port, with [`MidiInputs::All`]) is opened when
    /// it appears. Call it from a timer of the program's own, about every
    /// [`DEFAULT_POLL_INTERVAL`](manguebyte_devices::DEFAULT_POLL_INTERVAL).
    ///
    /// Returns the report of what changed, or `None` when nothing did.
    pub fn poll_devices(&mut self) -> Option<&SettingsReport> {
        let events = self.manager.poll_devices();
        let mut changed = false;
        let mut fallbacks = Vec::new();
        let mut error = None;
        let every_port = self.settings.midi_inputs == MidiInputs::All;
        for event in &events {
            match event {
                DeviceEvent::MidiPortAdded {
                    direction: Direction::Input,
                    port,
                } if every_port => {
                    if let Err(problem) = self.manager.enable_midi_input(DeviceRef::from(port)) {
                        fallbacks.push(Fallback::MidiPortMissing {
                            direction: Direction::Input,
                            wanted: DeviceRef::from(port),
                            reason: problem.to_string(),
                        });
                    }
                    changed = true;
                    continue;
                }
                DeviceEvent::MidiPortRemoved {
                    direction: Direction::Input,
                    port,
                } if every_port => {
                    self.manager.disable_midi_input(&port.id);
                    changed = true;
                    continue;
                }
                _ => {}
            }
            match self.manager.follow(event) {
                Some(Ok(report)) => {
                    changed = true;
                    fallbacks.extend(report.fallbacks);
                }
                Some(Err(problem)) => {
                    changed = true;
                    error = Some(problem.to_string());
                }
                None => {}
            }
        }
        if !changed {
            return None;
        }
        // A device that took over can run at another rate than the plugin.
        let mut prepared_again = false;
        let setup = self.manager.setup().clone();
        match self.correct_format(&setup) {
            Ok(Some((report, rate_changed))) => {
                fallbacks = report.fallbacks;
                prepared_again = rate_changed;
            }
            Ok(None) => {}
            Err(problem) => error = Some(problem.to_string()),
        }
        self.report = self.current_report(fallbacks, prepared_again, error);
        Some(&self.report)
    }

    /// What can be chosen now: hosts, the devices with the rates and
    /// buffer sizes each accepts (inputs only for a plugin with inputs),
    /// and MIDI input ports, each marked open or not.
    pub fn choices(&self) -> Result<SettingsChoices, DeviceError> {
        let snapshot = self.manager.devices()?;
        let describe = |direction: Direction, devices: &[DeviceSummary]| {
            let default = match direction {
                Direction::Output => snapshot.default_output.as_deref(),
                Direction::Input => snapshot.default_input.as_deref(),
            };
            devices
                .iter()
                .map(|device| {
                    let info = self.manager.device_info(direction, &device.id).ok();
                    DeviceOption::new(
                        device.clone(),
                        default == Some(device.id.as_str()),
                        info.as_ref(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let open = self.manager.midi_inputs();
        Ok(SettingsChoices {
            hosts: self.manager.hosts(),
            host: self.manager.host_name(),
            outputs: describe(Direction::Output, &snapshot.outputs),
            inputs: if P::LAYOUT.inputs > 0 {
                describe(Direction::Input, &snapshot.inputs)
            } else {
                Vec::new()
            },
            midi_inputs: snapshot
                .midi_inputs
                .iter()
                .map(|port| MidiPortOption {
                    port: port.clone(),
                    open: open.iter().any(|known| known.id == port.id),
                })
                .collect(),
        })
    }

    /// The audio hosts that can be chosen.
    pub fn hosts(&self) -> Vec<HostInfo> {
        self.manager.hosts()
    }

    /// What is connected now, on the host in use.
    pub fn devices(&self) -> Result<DeviceSnapshot, DeviceError> {
        self.manager.devices()
    }

    /// Everything about one device of the host in use, with its
    /// configurations.
    pub fn device_info(&self, direction: Direction, id: &str) -> Result<DeviceInfo, DeviceError> {
        self.manager.device_info(direction, id)
    }

    /// The settings asked for, to save with the session and hand back in
    /// [`StandaloneOptions::settings`]. They name the devices that were
    /// asked for, not those a fallback put in their place, so that they
    /// are used again when they come back.
    pub fn settings(&self) -> &StandaloneSettings {
        &self.settings
    }

    /// What the last change opened, or the start when nothing has changed
    /// since.
    pub fn report(&self) -> &SettingsReport {
        &self.report
    }

    /// Whether the plugin follows the device's rate or keeps its own.
    pub fn rate_policy(&self) -> RatePolicy {
        self.rate_policy
    }

    /// Sets how long the devices fade out and in when they change; 30 ms
    /// unless told otherwise. Zero switches at once, with a click.
    pub fn set_crossfade(&mut self, fade: Duration) {
        self.manager.set_crossfade(fade);
    }

    /// The device manager the plugin runs under, to read what it runs:
    /// its format, the running devices and settings, the last swap.
    pub fn device_manager(&self) -> &DeviceManager {
        &self.manager
    }

    /// The plugin's parameters. Writing to them changes the sound at once.
    pub fn params(&self) -> &P::Params {
        &self.params
    }

    /// The plugin's parameters, shared, for an editor
    /// (`Editor::open_in_app`) or another thread.
    pub fn params_arc(&self) -> Arc<P::Params> {
        Arc::clone(&self.params)
    }

    /// Plays a MIDI message as if it had come from a keyboard, at the start
    /// of the next block. Returns `false` if too many messages are already
    /// waiting.
    pub fn send(&self, message: MidiMessage) -> bool {
        let mut notes = self
            .notes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        notes.push(message).is_ok()
    }

    /// The transport: play, stop, seek, loop, a new tempo map, and where
    /// it is. Commands take effect at the start of the next block.
    pub fn transport(&self) -> &ClockControl {
        &self.transport
    }

    /// Name of the output device that plays; empty when none does.
    pub fn device_name(&self) -> &str {
        self.report
            .output
            .as_ref()
            .map_or("", |device| device.name.as_str())
    }

    /// Name of the input device, if the plugin is an effect and one was
    /// opened.
    pub fn input_name(&self) -> Option<&str> {
        self.report
            .input
            .as_ref()
            .map(|device| device.name.as_str())
    }

    /// Sample rate the plugin is running at, in hertz. The device's can
    /// differ under [`RatePolicy::Fixed`]; see [`report`](Self::report).
    pub fn sample_rate(&self) -> f64 {
        f64::from(self.format.sample_rate)
    }

    /// Names of the MIDI input ports being listened to.
    pub fn midi_input_names(&self) -> Vec<&str> {
        self.report
            .midi_inputs
            .iter()
            .map(|port| port.name.as_str())
            .collect()
    }

    /// Why the MIDI output port could not be opened, if it could not.
    /// Input ports that could not be opened are in the
    /// [report](Self::report)'s fallbacks.
    pub fn midi_problems(&self) -> &[MidiError] {
        &self.midi_problems
    }

    /// Name of the MIDI output port the plugin's notes are sent to, if one
    /// is open.
    pub fn midi_output_name(&self) -> Option<&str> {
        self.midi_output.as_ref().map(|(_, name)| name.as_str())
    }

    /// How many messages the MIDI output port refused, if one is open.
    pub fn midi_output_errors(&self) -> Option<u64> {
        self.midi_output.as_ref().map(|(thread, _)| thread.errors())
    }

    /// Captures the plugin's state as text.
    pub fn save_state(&self) -> String {
        save_state::<P>(&self.params)
    }

    /// Restores state captured by [`save_state`](Self::save_state).
    pub fn load_state(&self, text: &str) -> Result<LoadReport, StateError> {
        load_state::<P>(&self.params, text)
    }
}
