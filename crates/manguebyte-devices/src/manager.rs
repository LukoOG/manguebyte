//! Opening the devices a setup names, keeping them running, and following
//! the system when devices come and go.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use manguebyte_core::AudioBuffer;
#[cfg(feature = "midi")]
use manguebyte_core::spsc::Producer;

#[cfg(feature = "midi")]
use crate::backend::MidiSender;
use crate::backend::{
    Backend, Endpoint, EngineCallback, ErrorSink, OpenedEndpoint, OpenedFormat, StreamRequest,
    SystemBackend,
};
use crate::hotplug::{DeviceEvent, DeviceWatcher, Poller, WatchSlot, WatchedStream};
#[cfg(feature = "midi")]
use crate::midi::{MidiError, MidiInputMessage, MidiReceiver, MidiSink, Timed, merged_receiver};
#[cfg(feature = "midi")]
use crate::setup::{DeviceRef, Fallback};
use crate::setup::{Plan, plan};
use crate::{
    AudioHost, CallbackInfo, DeviceError, DeviceInfo, DeviceSetup, DeviceSnapshot, DeviceSummary,
    Direction, DuplexSettings, HostInfo, MockBackend, RestoreReport, StreamSettings,
};

/// The largest block a device manager hands its callback unless told
/// otherwise, in frames.
pub const DEFAULT_MAX_BLOCK_SIZE: usize = 1_024;

/// How long a stopping stream may take to give its callback back.
const RETURN_TIMEOUT: Duration = Duration::from_secs(2);

/// The format a callback will run with, as told to
/// [`AudioCallback::prepare`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamFormat {
    /// Sample rate, in hertz.
    pub sample_rate: f64,
    /// The largest block [`AudioCallback::process`] will be handed.
    pub max_block_size: usize,
    /// Input channels; zero with no input.
    pub input_channels: usize,
    /// Output channels; zero with no output.
    pub output_channels: usize,
}

/// One block of audio, as a device manager hands it to its callback.
#[non_exhaustive]
pub struct DeviceIo<'a> {
    /// Captured audio, with the input device's channels; no channels when
    /// there is no input.
    pub input: &'a AudioBuffer<f32>,
    /// Silent audio to fill, with the output device's channels; no
    /// channels when there is no output. As long as `input`.
    pub output: &'a mut AudioBuffer<f32>,
    /// When the block is heard and was captured.
    pub info: CallbackInfo,
    #[cfg(feature = "midi")]
    midi: &'a mut MidiReceiver,
}

impl DeviceIo<'_> {
    /// Frames in this block.
    pub fn num_frames(&self) -> usize {
        self.output.num_frames()
    }

    /// Hands over the MIDI that arrived on every open input port since the
    /// last block, each message with its frame inside this block (see
    /// [`MidiReceiver`]). Call it once per block.
    ///
    /// Real-time safe.
    #[cfg(feature = "midi")]
    pub fn read_midi(&mut self, handle: impl FnMut(u32, MidiInputMessage)) {
        let frames = self.output.num_frames();
        self.midi.read(frames, self.info.sample_rate, handle);
    }

    /// Hands over the MIDI that arrived since the last block as the
    /// framework's events, read through `input` so that MPE becomes
    /// per-note events (see [`MidiReceiver::read_events`]). Call it once
    /// per block, instead of [`read_midi`](Self::read_midi).
    ///
    /// Real-time safe.
    #[cfg(feature = "midi")]
    pub fn read_midi_events(
        &mut self,
        input: &mut manguebyte_dsp::note_input::NoteInput,
        events: &mut manguebyte_dsp::event::EventBuffer,
    ) {
        let frames = self.output.num_frames();
        self.midi
            .read_events(frames, self.info.sample_rate, input, events);
    }
}

/// What a [`DeviceManager`] runs.
///
/// A closure taking `&mut DeviceIo<'_>` is one; a type that needs to know
/// the format before it runs implements the trait.
///
/// The manager keeps the callback across restarts: when the devices
/// change, it stops the stream, gets the callback back, calls
/// [`prepare`](Self::prepare) with the new format and starts again.
pub trait AudioCallback: Send + 'static {
    /// Called before the stream starts, on the thread that starts it, with
    /// the format it will run with. Allocate here.
    fn prepare(&mut self, format: &StreamFormat) {
        let _ = format;
    }

    /// Processes one block, on the audio thread. Must not allocate, lock,
    /// block or do I/O.
    fn process(&mut self, io: &mut DeviceIo<'_>);

    /// Called after the stream stopped, on the thread that stopped it.
    fn release(&mut self) {}
}

impl<F> AudioCallback for F
where
    F: FnMut(&mut DeviceIo<'_>) + Send + 'static,
{
    fn process(&mut self, io: &mut DeviceIo<'_>) {
        self(io);
    }
}

/// The callback, and what it reads MIDI from, while no stream holds them.
struct Parked {
    callback: Box<dyn AudioCallback>,
    #[cfg(feature = "midi")]
    midi: MidiReceiver,
}

/// What a stream runs. When the stream drops it, it sends the callback
/// back to the manager.
struct Engine {
    parked: Option<Parked>,
    returns: Sender<Parked>,
}

impl Engine {
    fn run(
        &mut self,
        input: &AudioBuffer<f32>,
        output: &mut AudioBuffer<f32>,
        info: &CallbackInfo,
    ) {
        let Some(parked) = &mut self.parked else {
            return;
        };
        let mut io = DeviceIo {
            input,
            output,
            info: *info,
            #[cfg(feature = "midi")]
            midi: &mut parked.midi,
        };
        parked.callback.process(&mut io);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(parked) = self.parked.take() {
            let _ = self.returns.send(parked);
        }
    }
}

struct Running {
    stream: Box<dyn Send>,
    format: StreamFormat,
    input: Option<OpenedEndpoint>,
    output: Option<OpenedEndpoint>,
}

/// Opens the input, output and MIDI devices a [`DeviceSetup`] names, runs
/// one callback on them, and restores the setup in a later run with
/// fallbacks for whatever is missing.
///
/// ```no_run
/// use manguebyte_devices::{DeviceChoice, DeviceIo, DeviceManager, DeviceSetup};
///
/// // What the settings file held, or the defaults on a first run.
/// let saved: Option<String> = None;
/// let setup = saved
///     .and_then(|text| text.parse().ok())
///     .unwrap_or_else(|| DeviceSetup::default().with_input(DeviceChoice::Default));
///
/// let mut manager = DeviceManager::new();
/// let report = manager.start(&setup, |io: &mut DeviceIo<'_>| {
///     // Pass the first input channel through to every output channel.
///     if io.input.num_channels() > 0 {
///         for channel in io.output.iter_channels_mut() {
///             channel.copy_from_slice(io.input.channel(0));
///         }
///     }
/// })?;
/// for fallback in &report.fallbacks {
///     println!("{fallback}");
/// }
///
/// // Later, when the program quits:
/// let to_save = manager.setup().to_string();
/// # let _ = to_save;
/// # Ok::<(), manguebyte_devices::DeviceError>(())
/// ```
///
/// # Following the system
///
/// Devices come and go while the program runs. [`poll_devices`] (from a
/// timer of the application's own) or [`watch`] (a thread of its own)
/// report what changed as [`DeviceEvent`]s; [`follow`] reacts to one: it
/// restarts on the saved device when it comes back, on the default when
/// the stream's device is lost, and opens MIDI ports of the setup as they
/// appear.
///
/// [`poll_devices`]: Self::poll_devices
/// [`watch`]: Self::watch
/// [`follow`]: Self::follow
pub struct DeviceManager {
    backend: Arc<dyn Backend>,
    setup: DeviceSetup,
    max_block_size: usize,
    drift_correction: bool,
    errors: ErrorSink,
    callback: Option<Box<dyn AudioCallback>>,
    returns: (Sender<Parked>, Receiver<Parked>),
    running: Option<Running>,
    watched: WatchSlot,
    generation: u64,
    poller: Poller,
    #[cfg(feature = "midi")]
    midi_receiver: Option<MidiReceiver>,
    #[cfg(feature = "midi")]
    midi_shared: Arc<Mutex<Producer<Timed>>>,
    #[cfg(feature = "midi")]
    midi_inputs: Vec<(DeviceSummary, Box<dyn Send>)>,
    #[cfg(feature = "midi")]
    midi_output: Option<(DeviceSummary, Box<dyn MidiSender>)>,
    /// Puts packets for the output port together, made when first needed.
    #[cfg(feature = "midi")]
    midi_writer: Option<crate::midi::PacketWriter>,
}

impl std::fmt::Debug for DeviceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceManager")
            .field("host", &self.backend.host_name())
            .field("setup", &self.setup)
            .field("running", &self.is_running())
            .finish_non_exhaustive()
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceManager {
    /// A manager for the system's devices, on its default host. Nothing is
    /// opened until [`start`](Self::start).
    pub fn new() -> Self {
        Self::with_backend(Arc::new(SystemBackend::new(AudioHost::default_host())))
    }

    /// A manager for the devices of a [`MockBackend`], for tests and for
    /// running without hardware.
    pub fn headless(mock: &MockBackend) -> Self {
        Self::with_backend(Arc::new(mock.clone()))
    }

    fn with_backend(backend: Arc<dyn Backend>) -> Self {
        let watched: WatchSlot = Arc::new(Mutex::new(None));
        #[cfg(feature = "midi")]
        let (midi_shared, midi_receiver) = merged_receiver();
        Self {
            poller: Poller::new(Arc::clone(&backend), Arc::clone(&watched)),
            backend,
            setup: DeviceSetup::default(),
            max_block_size: DEFAULT_MAX_BLOCK_SIZE,
            drift_correction: true,
            errors: Arc::new(|error| eprintln!("manguebyte: audio stream error: {error}")),
            callback: None,
            returns: mpsc::channel(),
            running: None,
            watched,
            generation: 0,
            #[cfg(feature = "midi")]
            midi_receiver: Some(midi_receiver),
            #[cfg(feature = "midi")]
            midi_shared,
            #[cfg(feature = "midi")]
            midi_inputs: Vec::new(),
            #[cfg(feature = "midi")]
            midi_output: None,
            #[cfg(feature = "midi")]
            midi_writer: None,
        }
    }

    /// The audio hosts that can be chosen.
    pub fn hosts(&self) -> Vec<HostInfo> {
        self.backend.hosts()
    }

    /// The name of the host in use.
    pub fn host_name(&self) -> String {
        self.backend.host_name()
    }

    /// What is connected now, on the host in use.
    pub fn devices(&self) -> Result<DeviceSnapshot, DeviceError> {
        self.backend.snapshot()
    }

    /// Everything about one device of the host in use, with its
    /// configurations, for a settings dialog.
    pub fn device_info(&self, direction: Direction, id: &str) -> Result<DeviceInfo, DeviceError> {
        self.backend.device_info(direction, id)
    }

    /// Sets the largest block the callback is handed. Takes effect when
    /// the stream next starts.
    ///
    /// # Panics
    ///
    /// Panics if `frames` is zero.
    pub fn set_max_block_size(&mut self, frames: usize) {
        assert!(frames > 0, "the block size must be non-zero");
        self.max_block_size = frames;
    }

    /// Whether a stream joining an input and an output that are separate
    /// devices follows the drift between their clocks (on by default). See
    /// [`DuplexSettings::drift_correction`]. Takes effect when the stream
    /// next starts.
    pub fn set_drift_correction(&mut self, on: bool) {
        self.drift_correction = on;
    }

    /// Where stream errors go; standard error by default. Called from the
    /// backend's threads, never from the callback's path. Takes effect when
    /// the stream next starts.
    pub fn set_error_handler(&mut self, handler: impl Fn(DeviceError) + Send + Sync + 'static) {
        self.errors = Arc::new(handler);
    }

    /// Opens the devices of `setup` and starts running `callback` on them.
    ///
    /// Whatever the setup names that cannot be had is replaced, as the
    /// returned report says: a missing device by one with its name or by the
    /// default, a rate or channel count the device cannot do by one it can.
    /// When nothing at all can be opened the report says so and no stream
    /// runs; [`follow`](Self::follow) starts one once a device appears.
    ///
    /// A stream already running is stopped first, and its callback dropped.
    pub fn start(
        &mut self,
        setup: &DeviceSetup,
        callback: impl AudioCallback,
    ) -> Result<RestoreReport, DeviceError> {
        self.stop();
        if let Some(mut old) = self.callback.take() {
            old.release();
        }
        self.callback = Some(Box::new(callback));
        self.setup = setup.clone();
        self.open()
    }

    /// Changes the setup, and restarts the stream on it with the same
    /// callback.
    pub fn apply(&mut self, setup: &DeviceSetup) -> Result<RestoreReport, DeviceError> {
        self.setup = setup.clone();
        self.open()
    }

    /// Restarts the stream on the setup, for example after its device was
    /// lost: the saved devices are looked for again, with the same
    /// fallbacks as at the start.
    pub fn restart(&mut self) -> Result<RestoreReport, DeviceError> {
        self.open()
    }

    /// Stops the stream. The callback is kept for [`restart`](Self::restart),
    /// and MIDI ports stay open.
    pub fn stop(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        if let Ok(mut watched) = self.watched.lock() {
            *watched = None;
        }
        drop(running.stream);
        match self.returns.1.recv_timeout(RETURN_TIMEOUT) {
            Ok(parked) => self.park(parked),
            Err(_) => {
                // The backend never dropped the callback: it is gone, and so
                // is the MIDI receiver it held. Start MIDI afresh.
                #[cfg(feature = "midi")]
                self.renew_midi();
            }
        }
    }

    /// Stops the stream and gives the callback back.
    pub fn take_callback(&mut self) -> Option<Box<dyn AudioCallback>> {
        self.stop();
        self.callback.take()
    }

    /// The setup asked for, to save and restore in a later run. It names
    /// the devices that were asked for, not those a fallback put in their
    /// place, so that they are used again when they come back.
    pub fn setup(&self) -> &DeviceSetup {
        &self.setup
    }

    /// Whether a stream is running.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// The format of the running stream.
    pub fn format(&self) -> Option<StreamFormat> {
        self.running.as_ref().map(|running| running.format)
    }

    /// The device the running stream uses in one direction.
    pub fn running_device(&self, direction: Direction) -> Option<&DeviceSummary> {
        self.endpoint(direction).map(|endpoint| &endpoint.device)
    }

    /// The settings the running stream's device has in one direction.
    pub fn running_settings(&self, direction: Direction) -> Option<StreamSettings> {
        self.endpoint(direction).map(|endpoint| endpoint.settings)
    }

    fn endpoint(&self, direction: Direction) -> Option<&OpenedEndpoint> {
        let running = self.running.as_ref()?;
        match direction {
            Direction::Input => running.input.as_ref(),
            Direction::Output => running.output.as_ref(),
        }
    }

    /// Lists the devices now and reports what changed since the last call,
    /// including a running stream whose device was lost. The first call only
    /// learns what is there. Call it from a timer of the application's
    /// own, about every [`DEFAULT_POLL_INTERVAL`](crate::DEFAULT_POLL_INTERVAL).
    pub fn poll_devices(&mut self) -> Vec<DeviceEvent> {
        self.poller.poll()
    }

    /// Starts a thread that lists the devices every `interval` and hands
    /// what changed to `handler`, as [`poll_devices`](Self::poll_devices)
    /// would. Hand the events back to the manager's thread, through a
    /// channel for example, and pass them to [`follow`](Self::follow).
    pub fn watch<H>(&self, interval: Duration, handler: H) -> std::io::Result<DeviceWatcher>
    where
        H: FnMut(DeviceEvent) + Send + 'static,
    {
        DeviceWatcher::spawn(
            Poller::new(Arc::clone(&self.backend), Arc::clone(&self.watched)),
            interval,
            handler,
        )
    }

    /// Reacts to a change among the devices: restarts the stream when its
    /// device was lost, or when the setup would now pick other devices than
    /// those running (the saved device came back, the default changed while
    /// the setup asks for the default, a first device appeared), and opens
    /// or closes the setup's MIDI ports as they come and go.
    ///
    /// Returns `None` when the event changes nothing here, and what the
    /// restart reported otherwise.
    pub fn follow(&mut self, event: &DeviceEvent) -> Option<Result<RestoreReport, DeviceError>> {
        match event {
            DeviceEvent::StreamLost {
                direction, device, ..
            } => {
                // An event about a stream that has been replaced since,
                // following an earlier event, needs nothing more.
                let running = self.running_device(*direction);
                let current = self.running.is_none() || running.is_some_and(|d| d.id == device.id);
                current.then(|| self.restart())
            }
            DeviceEvent::DeviceAdded { .. }
            | DeviceEvent::DeviceRemoved { .. }
            | DeviceEvent::DefaultChanged { .. } => {
                if self.callback.is_none() && self.running.is_none() {
                    return None;
                }
                let planned = self.plan_now().ok()?;
                let wanted = |side: &Option<(DeviceSummary, StreamSettings)>| {
                    side.as_ref().map(|(device, _)| device.id.clone())
                };
                let running = |direction| self.running_device(direction).map(|d| d.id.clone());
                if wanted(&planned.output) == running(Direction::Output)
                    && wanted(&planned.input) == running(Direction::Input)
                {
                    return None;
                }
                Some(self.restart())
            }
            #[cfg(feature = "midi")]
            DeviceEvent::MidiPortAdded { .. } | DeviceEvent::MidiPortRemoved { .. } => {
                let planned = self.plan_now().ok()?;
                let mut report = RestoreReport::default();
                let before = self.open_midi_ports();
                self.connect_midi(&planned, &mut report);
                (self.open_midi_ports() != before).then_some(Ok(report))
            }
            _ => None,
        }
    }

    fn plan_now(&self) -> Result<Plan, DeviceError> {
        let snapshot = self.backend.snapshot()?;
        let backend = Arc::clone(&self.backend);
        Ok(plan(&self.setup, &snapshot, &|direction, id| {
            backend.device_info(direction, id).ok()
        }))
    }

    fn park(&mut self, parked: Parked) {
        let mut callback = parked.callback;
        callback.release();
        self.callback = Some(callback);
        #[cfg(feature = "midi")]
        {
            self.midi_receiver = Some(parked.midi);
        }
    }

    fn open(&mut self) -> Result<RestoreReport, DeviceError> {
        self.stop();
        let Some(callback) = self.callback.take() else {
            return Err(DeviceError::NoCallback);
        };
        let mut report = RestoreReport::default();

        let wanted_host = self.setup.host.clone();
        if self.backend.select_host(wanted_host.as_deref()).is_err() {
            let _ = self.backend.select_host(None);
            report
                .fallbacks
                .push(crate::setup::Fallback::HostUnavailable {
                    wanted: wanted_host.unwrap_or_default(),
                    used: self.backend.host_name(),
                });
        }
        let planned = match self.plan_now() {
            Ok(planned) => planned,
            Err(error) => {
                self.callback = Some(callback);
                return Err(error);
            }
        };
        report.fallbacks.extend(planned.fallbacks.iter().cloned());
        #[cfg(feature = "midi")]
        self.connect_midi(&planned, &mut report);

        let endpoint = |side: &Option<(DeviceSummary, StreamSettings)>| {
            side.as_ref().map(|(device, settings)| Endpoint {
                id: device.id.clone(),
                settings: *settings,
            })
        };
        let mut request = StreamRequest {
            input: endpoint(&planned.input),
            output: endpoint(&planned.output),
            duplex: DuplexSettings::new(self.max_block_size)
                .with_drift_correction(self.drift_correction),
        };
        if request.input.is_none() && request.output.is_none() {
            self.callback = Some(callback);
            return Ok(report);
        }

        match self.try_start(&request, callback) {
            Ok(()) => Ok(report),
            Err((error, callback)) => {
                let Some(callback) = callback else {
                    return Err(error);
                };
                // An input that will not open must not keep the output
                // silent: try again without it.
                let (Some(_), Some(_), Some((input, _))) =
                    (&request.input, &request.output, &planned.input)
                else {
                    self.callback = Some(callback);
                    return Err(error);
                };
                request.input = None;
                match self.try_start(&request, callback) {
                    Ok(()) => {
                        report.fallbacks.push(crate::setup::Fallback::InputDropped {
                            device: input.clone(),
                            reason: error.to_string(),
                        });
                        Ok(report)
                    }
                    Err((error, callback)) => {
                        self.callback = callback;
                        Err(error)
                    }
                }
            }
        }
    }

    /// Starts `request` with `callback`. On failure, hands the callback
    /// back with the error, unless the backend lost it.
    #[allow(clippy::type_complexity)]
    fn try_start(
        &mut self,
        request: &StreamRequest,
        callback: Box<dyn AudioCallback>,
    ) -> Result<(), (DeviceError, Option<Box<dyn AudioCallback>>)> {
        #[cfg(feature = "midi")]
        let Some(mut midi) = self.midi_receiver.take() else {
            return Err((DeviceError::NoCallback, Some(callback)));
        };
        #[cfg(feature = "midi")]
        midi.restart();
        let mut parked = Some(Parked {
            callback,
            #[cfg(feature = "midi")]
            midi,
        });
        let returns = self.returns.0.clone();
        let max_block_size = self.max_block_size;
        let mut prepared = None;
        let result = self.backend.start(
            request,
            &mut |format: OpenedFormat| -> EngineCallback {
                let mut parked = parked.take();
                let format = StreamFormat {
                    sample_rate: f64::from(format.sample_rate),
                    max_block_size,
                    input_channels: format.input_channels,
                    output_channels: format.output_channels,
                };
                if let Some(parked) = &mut parked {
                    parked.callback.prepare(&format);
                }
                prepared = Some(format);
                let mut engine = Engine {
                    parked,
                    returns: returns.clone(),
                };
                Box::new(move |input, output, info| engine.run(input, output, info))
            },
            Arc::clone(&self.errors),
        );

        match result {
            Ok(started) => {
                self.generation += 1;
                let endpoints = [
                    (Direction::Output, started.output.as_ref()),
                    (Direction::Input, started.input.as_ref()),
                ]
                .into_iter()
                .filter_map(|(direction, endpoint)| {
                    let endpoint = endpoint?;
                    Some((
                        direction,
                        endpoint.device.clone(),
                        Arc::clone(&endpoint.health),
                    ))
                })
                .collect();
                if let Ok(mut watched) = self.watched.lock() {
                    *watched = Some(WatchedStream {
                        generation: self.generation,
                        endpoints,
                    });
                }
                let format = prepared.unwrap_or(StreamFormat {
                    sample_rate: f64::from(started.format.sample_rate),
                    max_block_size,
                    input_channels: started.format.input_channels,
                    output_channels: started.format.output_channels,
                });
                self.running = Some(Running {
                    stream: started.stream,
                    format,
                    input: started.input,
                    output: started.output,
                });
                Ok(())
            }
            Err(error) => {
                // Either the backend failed before making the callback, and
                // it is still here, or it dropped what it made, which sent
                // the callback back.
                let parked = parked
                    .take()
                    .or_else(|| self.returns.1.recv_timeout(RETURN_TIMEOUT).ok());
                match parked {
                    Some(parked) => {
                        #[cfg(feature = "midi")]
                        {
                            self.midi_receiver = Some(parked.midi);
                        }
                        Err((error, Some(parked.callback)))
                    }
                    None => {
                        #[cfg(feature = "midi")]
                        self.renew_midi();
                        Err((error, None))
                    }
                }
            }
        }
    }
}

#[cfg(feature = "midi")]
impl DeviceManager {
    /// Opens the MIDI ports `planned` names and closes those it does not.
    fn connect_midi(&mut self, planned: &Plan, report: &mut RestoreReport) {
        self.midi_inputs
            .retain(|(port, _)| planned.midi_inputs.iter().any(|other| other.id == port.id));
        for port in &planned.midi_inputs {
            if self.midi_inputs.iter().any(|(open, _)| open.id == port.id) {
                continue;
            }
            let sink = MidiSink::Shared(Arc::clone(&self.midi_shared));
            match self.backend.connect_midi_input(&port.id, sink) {
                Ok(connection) => self.midi_inputs.push((port.clone(), connection)),
                Err(error) => report.fallbacks.push(Fallback::MidiPortMissing {
                    direction: Direction::Input,
                    wanted: DeviceRef::from(port),
                    reason: error.to_string(),
                }),
            }
        }
        match &planned.midi_output {
            None => self.midi_output = None,
            Some(port) => {
                if self.midi_output.as_ref().map(|(open, _)| &open.id) != Some(&port.id) {
                    self.midi_output = None;
                    match self.backend.connect_midi_output(&port.id) {
                        Ok(sender) => self.midi_output = Some((port.clone(), sender)),
                        Err(error) => report.fallbacks.push(Fallback::MidiPortMissing {
                            direction: Direction::Output,
                            wanted: DeviceRef::from(port),
                            reason: error.to_string(),
                        }),
                    }
                }
            }
        }
    }

    fn open_midi_ports(&self) -> Vec<String> {
        self.midi_inputs
            .iter()
            .map(|(port, _)| port.id.clone())
            .chain(self.midi_output.iter().map(|(port, _)| port.id.clone()))
            .collect()
    }

    /// A new MIDI queue for the callback, when the old receiver was lost
    /// with a callback that never came back. The ports are connected to it
    /// again.
    fn renew_midi(&mut self) {
        let (shared, receiver) = merged_receiver();
        self.midi_shared = shared;
        self.midi_receiver = Some(receiver);
        let ports: Vec<DeviceSummary> = self.midi_inputs.drain(..).map(|(port, _)| port).collect();
        for port in ports {
            let sink = MidiSink::Shared(Arc::clone(&self.midi_shared));
            if let Ok(connection) = self.backend.connect_midi_input(&port.id, sink) {
                self.midi_inputs.push((port, connection));
            }
        }
    }

    /// The MIDI input ports that are open.
    pub fn midi_inputs(&self) -> Vec<DeviceSummary> {
        self.midi_inputs
            .iter()
            .map(|(port, _)| port.clone())
            .collect()
    }

    /// The MIDI output port that is open.
    pub fn midi_output(&self) -> Option<&DeviceSummary> {
        self.midi_output.as_ref().map(|(port, _)| port)
    }

    /// Adds a MIDI input port to the setup and opens it at once, without
    /// restarting the audio.
    pub fn enable_midi_input(&mut self, port: DeviceRef) -> Result<(), MidiError> {
        if !self.setup.midi_inputs.contains(&port) {
            self.setup.midi_inputs.push(port.clone());
        }
        if self.midi_inputs.iter().any(|(open, _)| open.id == port.id) {
            return Ok(());
        }
        let sink = MidiSink::Shared(Arc::clone(&self.midi_shared));
        let connection = self.backend.connect_midi_input(&port.id, sink)?;
        self.midi_inputs.push((
            DeviceSummary {
                id: port.id,
                name: port.name,
            },
            connection,
        ));
        Ok(())
    }

    /// Takes a MIDI input port out of the setup and closes it.
    pub fn disable_midi_input(&mut self, id: &str) {
        self.setup.midi_inputs.retain(|port| port.id != id);
        self.midi_inputs.retain(|(port, _)| port.id != id);
    }

    /// Sets the MIDI output port of the setup and opens it, or closes it.
    pub fn set_midi_output(&mut self, port: Option<DeviceRef>) -> Result<(), MidiError> {
        self.midi_output = None;
        self.setup.midi_output = port.clone();
        if let Some(port) = port {
            let sender = self.backend.connect_midi_output(&port.id)?;
            self.midi_output = Some((
                DeviceSummary {
                    id: port.id,
                    name: port.name,
                },
                sender,
            ));
        }
        Ok(())
    }

    /// Sends one complete MIDI message to the open MIDI output port.
    ///
    /// This talks to the operating system and may block: never call it
    /// from the audio thread.
    pub fn send_midi(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        match &mut self.midi_output {
            Some((_, sender)) => sender.send(bytes),
            None => Err(MidiError::PortNotFound(String::from(
                "no MIDI output is open",
            ))),
        }
    }

    /// Sends one Universal MIDI Packet to the open MIDI output port, as
    /// the MIDI 1.0 bytes it stands for; see
    /// [`PacketWriter`](crate::midi::PacketWriter) for what becomes what.
    ///
    /// This talks to the operating system and may block: never call it
    /// from the audio thread.
    pub fn send_midi_packet(
        &mut self,
        packet: &manguebyte_midi::ump::Packet,
    ) -> Result<(), MidiError> {
        let Some((_, sender)) = &mut self.midi_output else {
            return Err(MidiError::PortNotFound(String::from(
                "no MIDI output is open",
            )));
        };
        let writer = self
            .midi_writer
            .get_or_insert_with(crate::midi::PacketWriter::new);
        let mut result = Ok(());
        writer.write(packet, |bytes| {
            if result.is_ok() {
                result = sender.send(bytes);
            }
        });
        result
    }
}

impl Drop for DeviceManager {
    fn drop(&mut self) {
        // The stream goes before the MIDI ports that feed it.
        self.stop();
    }
}
