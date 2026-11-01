//! The glue between a running [`Standalone`] plugin and the audio settings
//! view of [`manguebyte_ui::audio_settings`], with the `standalone`
//! feature.
//!
//! The view takes a plain model and hands back requests; this module fills
//! the model in from [`Standalone::choices`], [`Standalone::settings`] and
//! [`Standalone::report`], and carries each request out with the
//! standalone's `set_` methods. [`sync`] does both, and follows devices
//! that come and go: a program calls it about every
//! [`DEFAULT_POLL_INTERVAL`](manguebyte_devices::DEFAULT_POLL_INTERVAL),
//! from a timer of the application ([`App::set_timer`]) or from a thread
//! that owns the standalone ([`AudioSettings`] is `Send` and `Sync`, so the
//! view on the interface's thread and the glue on another share it).
//! ADR 0022 says why the glue lives here.
//!
//! [`App::set_timer`]: manguebyte_window::App::set_timer
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
//! use manguebyte_devices::DEFAULT_POLL_INTERVAL;
//! use manguebyte_gfx::{Point, Size};
//! use manguebyte_gui::{audio_settings, open_app_window};
//! use manguebyte_standalone::{Standalone, StandaloneOptions};
//! use manguebyte_ui::audio_settings::AudioSettings;
//! use manguebyte_ui::{IntoView, State, Theme, floating_panel, label, pill, row};
//! use manguebyte_window::{App, AppWindowOptions, TimerId};
//!
//! let mut synth = Standalone::<MySynth>::start(StandaloneOptions::default())?;
//!
//! // The view, in a panel a pill opens over the program's own interface.
//! let settings = AudioSettings::new();
//! let open = State::new(false);
//! let view = row((label("My synth"), pill("Audio settings").opens(&open))).with_panels([
//!     floating_panel("Audio settings", open.clone(), State::new(Point::new(20.0, 40.0)), settings.view()),
//! ]);
//! let app = App::new()?;
//! let window = open_app_window(&app, AppWindowOptions::new("My synth", Size::new(480.0, 320.0)), Theme::default(), None, view)?;
//!
//! // From the application's timer: carry out what the person picked,
//! // follow devices that came or went, and show what runs now.
//! let polled = settings.clone();
//! app.set_timer(TimerId(1), DEFAULT_POLL_INTERVAL.as_millis() as u32, move |_: &App| {
//!     if audio_settings::sync(&mut synth, &polled) {
//!         println!("{}", synth.report());
//!     }
//! })?;
//! app.run()?;
//! # drop(window);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use manguebyte_devices::{DeviceChoice, Direction};
use manguebyte_plugin::Plugin;
use manguebyte_standalone::{
    DeviceOption, SettingsChoices, SettingsReport, Standalone, StandaloneSettings,
};
use manguebyte_ui::audio_settings::{
    AudioSettings, AudioSettingsModel, AudioSettingsRequest, DeviceItem, MidiPortItem,
};

/// Carries out what the person picked in `settings`' views, follows
/// devices that came or went ([`Standalone::poll_devices`]), and, when
/// anything changed or the model is [stale](AudioSettings::is_stale), shows
/// what runs now. Returns whether anything changed; [`Standalone::report`]
/// then says what.
///
/// A program that calls it does not call `poll_devices` itself, or the
/// view would miss what that call found. Listing the devices and what each
/// accepts is done only when something changed, never on a quiet call.
pub fn sync<P: Plugin>(standalone: &mut Standalone<P>, settings: &AudioSettings) -> bool {
    let requests = settings.take_requests();
    let mut changed = settings.is_stale() || !requests.is_empty();
    for request in &requests {
        apply(standalone, request);
    }
    if standalone.poll_devices().is_some() {
        changed = true;
    }
    if changed {
        settings.set_model(model(standalone));
    }
    changed
}

/// Carries out one request with the standalone's `set_` methods and
/// returns the report of what it opened.
pub fn apply<'a, P: Plugin>(
    standalone: &'a mut Standalone<P>,
    request: &AudioSettingsRequest,
) -> &'a SettingsReport {
    match request {
        AudioSettingsRequest::Output(id) => standalone.set_output_device(id),
        AudioSettingsRequest::Input(Some(id)) => standalone.set_input_device(id),
        AudioSettingsRequest::Input(None) => standalone.set_input(DeviceChoice::None),
        AudioSettingsRequest::SampleRate(rate) => standalone.set_sample_rate(Some(*rate)),
        AudioSettingsRequest::BufferSize(frames) => standalone.set_buffer_size(Some(*frames)),
        AudioSettingsRequest::MidiInput { id, open } => standalone.set_midi_input(id, *open),
        AudioSettingsRequest::MidiOutput(id) => standalone.set_midi_output(id.as_deref()),
    }
}

/// The model of what `standalone` runs now and what can be chosen. When
/// the devices cannot be listed, the lists are empty and the report says
/// why.
pub fn model<P: Plugin>(standalone: &Standalone<P>) -> AudioSettingsModel {
    match standalone.choices() {
        Ok(choices) => model_from(
            &choices,
            standalone.settings(),
            standalone.report(),
            P::LAYOUT.inputs > 0,
            P::NOTE_OUTPUT,
        ),
        Err(error) => {
            let mut model = model_from(
                &SettingsChoices::default(),
                standalone.settings(),
                standalone.report(),
                P::LAYOUT.inputs > 0,
                P::NOTE_OUTPUT,
            );
            model.report = format!("{}\nThe devices cannot be listed: {error}", model.report);
            model
        }
    }
}

/// The model of `choices`, with the devices, rate, buffer and MIDI output
/// port that `report` says run, the input section shown when `show_input`
/// and the MIDI output section when `show_midi_output`.
///
/// The rates and buffer sizes are those the output device that plays
/// accepts, with the ones in use added if the device did not list them, so
/// that what runs is always shown chosen. The buffer size is that of the
/// report, or else of `settings`; `None` when the device chooses.
pub fn model_from(
    choices: &SettingsChoices,
    settings: &StandaloneSettings,
    report: &SettingsReport,
    show_input: bool,
    show_midi_output: bool,
) -> AudioSettingsModel {
    let items = |options: &[DeviceOption]| {
        options
            .iter()
            .map(|option| DeviceItem::new(&option.device.id, &option.device.name))
            .collect::<Vec<_>>()
    };
    let output = report.output.as_ref().map(|device| device.id.clone());
    let playing = output
        .as_deref()
        .and_then(|id| choices.device(Direction::Output, id));
    let sample_rate = report.device_sample_rate;
    let buffer_size = report.buffer_size.or(settings.buffer_size);
    let with = |mut values: Vec<u32>, current: Option<u32>| {
        if let Some(current) = current {
            if !values.contains(&current) {
                values.push(current);
                values.sort_unstable();
            }
        }
        values
    };
    AudioSettingsModel {
        outputs: items(&choices.outputs),
        output,
        inputs: items(&choices.inputs),
        input: report.input.as_ref().map(|device| device.id.clone()),
        show_input,
        sample_rates: with(
            playing.map(|o| o.sample_rates.clone()).unwrap_or_default(),
            sample_rate,
        ),
        sample_rate,
        buffer_sizes: with(
            playing.map(|o| o.buffer_sizes.clone()).unwrap_or_default(),
            buffer_size,
        ),
        buffer_size,
        midi_inputs: choices
            .midi_inputs
            .iter()
            .map(|port| MidiPortItem::new(&port.port.id, &port.port.name, port.open))
            .collect(),
        midi_outputs: choices
            .midi_outputs
            .iter()
            .map(|port| DeviceItem::new(&port.port.id, &port.port.name))
            .collect(),
        midi_output: report.midi_output.as_ref().map(|port| port.id.clone()),
        show_midi_output,
        report: report.to_string(),
    }
}
