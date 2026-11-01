//! A ready-made "Audio settings" view for a standalone program: output and
//! input devices, sample rate, buffer size, MIDI input ports, the MIDI
//! output port, and what the last change opened.
//!
//! The view knows nothing of devices. It shows an [`AudioSettingsModel`],
//! plain names, identifiers and numbers the program fills in, and turns
//! what the person picks into [`AudioSettingsRequest`]s the program polls
//! with [`AudioSettings::take_requests`] and carries out, then hands back
//! the model of what runs now with [`AudioSettings::set_model`]. Nothing
//! calls the program back (ADR 0002 and 0003: state is polled). This keeps
//! `manguebyte-ui` free of `manguebyte-standalone` and its system audio and
//! MIDI libraries (ADR 0001); the glue from a running
//! `manguebyte_standalone::Standalone` to the model and back is
//! `manguebyte_gui::audio_settings`, behind that crate's `standalone`
//! feature (ADR 0022).
//!
//! The sections, top to bottom:
//!
//! - **Output device** and **Input device**, combo boxes of the devices'
//!   names; the input's list starts with "None", and the section is left
//!   out for a plugin with no inputs
//!   ([`show_input`](AudioSettingsModel::show_input)).
//! - **Sample rate** and **Buffer size**, combo boxes of the values the
//!   output device accepts; a buffer is shown in samples and, when the rate
//!   is known, milliseconds.
//! - **MIDI inputs**: a switch for each port, on when it is open, up to
//!   [`MAX_MIDI_PORTS`]; "No MIDI input ports" when there is none.
//! - **MIDI output**, a combo box of the output ports' names after "None",
//!   for a plugin that sends notes
//!   ([`show_midi_output`](AudioSettingsModel::show_midi_output)).
//! - The report of the last change, a line for each line of
//!   [`report`](AudioSettingsModel::report), up to [`REPORT_LINES`].
//!
//! Every part is an ordinary widget: the keyboard, the wheel, menus and
//! assistive technology work as they do for [`ComboBox`] and [`Toggle`].
//! The view is built once; when ports come and go, slots built for
//! [`MAX_MIDI_PORTS`] are shown or hidden ([`Shown::First`]), so nothing
//! is built again while it runs.
//!
//! ```
//! use manguebyte_gfx::Size;
//! use manguebyte_ui::audio_settings::{
//!     AudioSettings, AudioSettingsModel, AudioSettingsRequest, DeviceItem, MidiPortItem,
//! };
//! use manguebyte_ui::testing::Harness;
//! use manguebyte_ui::{Role, Theme};
//!
//! let settings = AudioSettings::new();
//! settings.set_model(AudioSettingsModel {
//!     outputs: vec![DeviceItem::new("spk", "Speakers"), DeviceItem::new("hp", "Headphones")],
//!     output: Some("spk".into()),
//!     sample_rates: vec![44_100, 48_000],
//!     sample_rate: Some(48_000),
//!     buffer_sizes: vec![128, 256],
//!     buffer_size: Some(256),
//!     midi_inputs: vec![MidiPortItem::new("keys", "Keys", true)],
//!     report: "Playing on \"Speakers\" at 48000 Hz".into(),
//!     ..AudioSettingsModel::default()
//! });
//! let mut ui = Harness::new(settings.view(), Size::new(320.0, 320.0), 1.0);
//! ui.frame();
//! assert_eq!(ui.get(Role::ComboBox, "Output device").value_text(), Some("Speakers"));
//!
//! // The person picks the headphones and closes the keyboard's port.
//! ui.click(ui.get(Role::ComboBox, "Output device").center());
//! ui.frame();
//! ui.click(ui.get(Role::MenuItem, "Headphones").center());
//! ui.click(ui.get(Role::Switch, "Keys").center());
//!
//! // The program polls, from its timer.
//! assert_eq!(
//!     settings.take_requests(),
//!     [
//!         AudioSettingsRequest::Output("hp".into()),
//!         AudioSettingsRequest::MidiInput { id: "keys".into(), open: false },
//!     ]
//! );
//! assert!(settings.take_requests().is_empty(), "each pick is asked for once");
//! ```

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use manguebyte_gfx::{Scene, Size};

use crate::access::{AccessNode, Role};
use crate::layout::Style;
use crate::source::State;
use crate::view::{IntoView, View, row};
use crate::widget::{AccessCx, MeasureCx, PaintCx, Shown, UpdateCx, Widget};
use crate::widgets::{Choices, ComboBox, Label, Toggle, label};

/// The most MIDI input ports the view shows a switch for; ports past it
/// are not shown.
pub const MAX_MIDI_PORTS: usize = 16;

/// The most lines of the report the view shows; the lines past it are
/// joined to the last, which is cut to its width.
pub const REPORT_LINES: usize = 4;

/// Space between the sections.
const SPACING: f32 = 8.0;

/// One audio device the view lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceItem {
    /// The identifier a request names it by.
    pub id: String,
    /// The name shown.
    pub name: String,
}

impl DeviceItem {
    /// A device identified by `id` and shown as `name`.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> DeviceItem {
        DeviceItem {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// One MIDI input port the view lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MidiPortItem {
    /// The identifier a request names it by.
    pub id: String,
    /// The name shown.
    pub name: String,
    /// Whether it is open.
    pub open: bool,
}

impl MidiPortItem {
    /// A port identified by `id`, shown as `name`, open or not.
    pub fn new(id: impl Into<String>, name: impl Into<String>, open: bool) -> MidiPortItem {
        MidiPortItem {
            id: id.into(),
            name: name.into(),
            open,
        }
    }
}

/// What an audio settings view shows: plain data the program fills in from
/// its devices and the last change's report.
///
/// A current value that is not in its list (a device that went away, a rate
/// the list does not have) shows as nothing chosen; the program usually
/// puts the current rate and buffer size in their lists.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioSettingsModel {
    /// The output devices, in the order shown.
    pub outputs: Vec<DeviceItem>,
    /// The identifier of the output device that plays, if any.
    pub output: Option<String>,
    /// The input devices, in the order shown after "None".
    pub inputs: Vec<DeviceItem>,
    /// The identifier of the input device that records; `None` for none.
    pub input: Option<String>,
    /// Whether the input section is shown: false for an instrument, which
    /// has no inputs.
    pub show_input: bool,
    /// The sample rates that can be chosen, in hertz.
    pub sample_rates: Vec<u32>,
    /// The sample rate the devices run at, if known.
    pub sample_rate: Option<u32>,
    /// The hardware buffer sizes that can be chosen, in frames.
    pub buffer_sizes: Vec<u32>,
    /// The buffer size in use, if one was asked for and accepted; `None`
    /// shows "Default".
    pub buffer_size: Option<u32>,
    /// The MIDI input ports, open or not.
    pub midi_inputs: Vec<MidiPortItem>,
    /// The MIDI output ports, in the order shown after "None".
    pub midi_outputs: Vec<DeviceItem>,
    /// The identifier of the MIDI output port the notes go to; `None` for
    /// none.
    pub midi_output: Option<String>,
    /// Whether the MIDI output section is shown: true for a plugin that
    /// sends notes.
    pub show_midi_output: bool,
    /// What the last change opened, in a few lines.
    pub report: String,
}

/// A change the person asked for in an audio settings view, for the
/// program to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioSettingsRequest {
    /// Play on the output device with this identifier.
    Output(String),
    /// Record from the input device with this identifier, or from none.
    Input(Option<String>),
    /// Run the devices at this sample rate, in hertz.
    SampleRate(u32),
    /// Use a hardware buffer of this many frames.
    BufferSize(u32),
    /// Open or close the MIDI input port with this identifier.
    MidiInput {
        /// The port's identifier.
        id: String,
        /// Whether to open it.
        open: bool,
    },
    /// Send the notes to the MIDI output port with this identifier, or to
    /// none.
    MidiOutput(Option<String>),
}

/// An audio settings view's states, shared by every view it builds and the
/// program: the model in, the requests out.
///
/// Cloning it shares it. It is `Send` and `Sync`, so a plugin's editor
/// spec can keep one and build its view each time the editor opens. The
/// program owns the devices: it calls [`take_requests`](Self::take_requests)
/// from its own timer, carries each request out, and gives the view what
/// runs now with [`set_model`](Self::set_model). A pick the program has
/// not taken yet when it sets a new model is kept, worked out against the
/// model it was made on.
#[derive(Clone)]
pub struct AudioSettings {
    shared: Arc<Shared>,
}

struct Shared {
    output: State<Choices>,
    input: State<Choices>,
    rate: State<Choices>,
    buffer: State<Choices>,
    /// Which sections the view shows: all, or all but the report.
    sections: State<Shown>,
    /// The output alone, or the output and the input.
    devices: State<Shown>,
    /// The note that there is no port, or the ports.
    midi: State<Shown>,
    /// How many port switches are shown.
    ports: State<Shown>,
    port_names: Vec<State<String>>,
    port_open: Vec<State<bool>>,
    /// The MIDI inputs alone, or with the MIDI output.
    midi_sections: State<Shown>,
    midi_output: State<Choices>,
    /// How many report lines are shown.
    lines: State<Shown>,
    report: Vec<State<String>>,
    inner: Mutex<Inner>,
}

struct Inner {
    model: AudioSettingsModel,
    pending: Vec<AudioSettingsRequest>,
    stale: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings::new()
    }
}

impl fmt::Debug for AudioSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioSettings")
            .field("model", &self.model())
            .finish_non_exhaustive()
    }
}

impl AudioSettings {
    /// Settings with an empty model, marked [stale](Self::is_stale) until
    /// the program sets one.
    pub fn new() -> AudioSettings {
        let shown = |shown| State::new(shown);
        AudioSettings {
            shared: Arc::new(Shared {
                output: State::new(Choices::default()),
                input: State::new(Choices::default()),
                rate: State::new(Choices::default()),
                buffer: State::new(Choices::default()),
                sections: shown(Shown::First(3)),
                devices: shown(Shown::First(1)),
                midi: shown(Shown::Only(0)),
                ports: shown(Shown::First(0)),
                port_names: (0..MAX_MIDI_PORTS)
                    .map(|_| State::new(String::new()))
                    .collect(),
                port_open: (0..MAX_MIDI_PORTS).map(|_| State::new(false)).collect(),
                midi_sections: shown(Shown::First(2)),
                midi_output: State::new(Choices::default()),
                lines: shown(Shown::First(0)),
                report: (0..REPORT_LINES)
                    .map(|_| State::new(String::new()))
                    .collect(),
                inner: Mutex::new(Inner {
                    model: AudioSettingsModel::default(),
                    pending: Vec::new(),
                    stale: true,
                }),
            }),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Nothing that can panic runs while it is held.
        self.shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The model shown, with the person's picks the program has taken
    /// already.
    pub fn model(&self) -> AudioSettingsModel {
        self.inner().model.clone()
    }

    /// Whether the model should be filled in again: until the first
    /// [`set_model`](Self::set_model), and after
    /// [`mark_stale`](Self::mark_stale).
    pub fn is_stale(&self) -> bool {
        self.inner().stale
    }

    /// Says the model no longer describes what runs, because the program
    /// changed the settings itself (a session loaded, say), so that glue
    /// that polls [`is_stale`](Self::is_stale) fills it in again.
    pub fn mark_stale(&self) {
        self.inner().stale = true;
    }

    /// Shows `model`. Only what changed is set, so a model that is the same
    /// as before draws nothing. A pick not taken yet is kept as a request.
    pub fn set_model(&self, model: AudioSettingsModel) {
        let mut inner = self.inner();
        self.collect(&mut inner);
        self.show(&model);
        inner.model = model;
        inner.stale = false;
    }

    /// The changes the person asked for since the last call, in the order
    /// of the sections: output, input, rate, buffer, each input port, then
    /// the output port. A
    /// section changed twice asks once, for its last value; picking what
    /// the model shows already asks nothing.
    pub fn take_requests(&self) -> Vec<AudioSettingsRequest> {
        let mut inner = self.inner();
        self.collect(&mut inner);
        std::mem::take(&mut inner.pending)
    }

    /// Turns what the widgets hold and the model does not into requests,
    /// and takes them into the model, so that each is asked for once.
    fn collect(&self, inner: &mut Inner) {
        let shared = &self.shared;
        let Inner { model, pending, .. } = inner;
        if let Some(at) = shared.output.with(Choices::chosen) {
            if let Some(device) = model.outputs.get(at) {
                if model.output.as_deref() != Some(device.id.as_str()) {
                    model.output = Some(device.id.clone());
                    pending.push(AudioSettingsRequest::Output(device.id.clone()));
                }
            }
        }
        if model.show_input {
            if let Some(at) = shared.input.with(Choices::chosen) {
                if let Some(id) = after_none(&model.inputs, at).filter(|id| *id != model.input) {
                    model.input.clone_from(&id);
                    pending.push(AudioSettingsRequest::Input(id));
                }
            }
        }
        if let Some(at) = shared.rate.with(Choices::chosen) {
            if let Some(&rate) = model.sample_rates.get(at) {
                if model.sample_rate != Some(rate) {
                    model.sample_rate = Some(rate);
                    pending.push(AudioSettingsRequest::SampleRate(rate));
                }
            }
        }
        if let Some(at) = shared.buffer.with(Choices::chosen) {
            if let Some(&frames) = model.buffer_sizes.get(at) {
                if model.buffer_size != Some(frames) {
                    model.buffer_size = Some(frames);
                    pending.push(AudioSettingsRequest::BufferSize(frames));
                }
            }
        }
        for (port, open) in model.midi_inputs.iter_mut().zip(&shared.port_open) {
            let open = open.get();
            if port.open != open {
                port.open = open;
                pending.push(AudioSettingsRequest::MidiInput {
                    id: port.id.clone(),
                    open,
                });
            }
        }
        if model.show_midi_output {
            if let Some(at) = shared.midi_output.with(Choices::chosen) {
                if let Some(id) =
                    after_none(&model.midi_outputs, at).filter(|id| *id != model.midi_output)
                {
                    model.midi_output.clone_from(&id);
                    pending.push(AudioSettingsRequest::MidiOutput(id));
                }
            }
        }
    }

    /// Puts `model` into the widgets' states, setting only those that
    /// change.
    fn show(&self, model: &AudioSettingsModel) {
        let shared = &self.shared;
        let names = |devices: &[DeviceItem]| devices.iter().map(|d| d.name.clone()).collect();
        let position = |devices: &[DeviceItem], id: &Option<String>| {
            id.as_ref()
                .and_then(|id| devices.iter().position(|device| device.id == *id))
        };
        set(
            &shared.output,
            Choices {
                items: names(&model.outputs),
                selected: position(&model.outputs, &model.output),
            },
        );
        set(&shared.input, with_none(&model.inputs, &model.input));
        set(
            &shared.rate,
            Choices {
                items: model
                    .sample_rates
                    .iter()
                    .map(|rate| format!("{rate} Hz"))
                    .collect(),
                selected: model
                    .sample_rate
                    .and_then(|rate| model.sample_rates.iter().position(|&r| r == rate)),
            },
        );
        set(
            &shared.buffer,
            Choices {
                items: model
                    .buffer_sizes
                    .iter()
                    .map(|&frames| buffer_text(frames, model.sample_rate))
                    .collect(),
                selected: model
                    .buffer_size
                    .and_then(|frames| model.buffer_sizes.iter().position(|&f| f == frames)),
            },
        );
        set(
            &shared.devices,
            Shown::First(if model.show_input { 2 } else { 1 }),
        );
        let ports = model.midi_inputs.len().min(MAX_MIDI_PORTS);
        set(&shared.midi, Shown::Only(usize::from(ports > 0)));
        set(&shared.ports, Shown::First(ports));
        for (at, port) in model.midi_inputs.iter().take(MAX_MIDI_PORTS).enumerate() {
            set(&shared.port_names[at], port.name.clone());
            set(&shared.port_open[at], port.open);
        }
        set(
            &shared.midi_sections,
            Shown::First(if model.show_midi_output { 3 } else { 2 }),
        );
        set(
            &shared.midi_output,
            with_none(&model.midi_outputs, &model.midi_output),
        );
        let mut lines: Vec<String> = model
            .report
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if lines.len() > REPORT_LINES {
            let rest = lines.split_off(REPORT_LINES - 1);
            lines.push(rest.join("; "));
        }
        set(
            &shared.sections,
            Shown::First(if lines.is_empty() { 3 } else { 4 }),
        );
        set(&shared.lines, Shown::First(lines.len()));
        for (at, line) in lines.into_iter().enumerate() {
            set(&shared.report[at], line);
        }
    }

    /// The view, which may be built as many times as it is opened: every
    /// view built shares these states. Its outermost group is named
    /// "Audio settings" for assistive technology; it has no title of its
    /// own, so that a [`floating_panel`](crate::floating_panel) or a
    /// window's title names it.
    pub fn view(&self) -> View {
        let shared = &self.shared;
        let devices = reveal(
            &shared.devices,
            Style::column().with_spacing(SPACING),
            vec![
                ComboBox::from_state("Output device", shared.output.clone())
                    .placeholder("No device")
                    .into_view(),
                ComboBox::from_state("Input device", shared.input.clone()).into_view(),
            ],
        );
        let format = row((
            ComboBox::from_state("Sample rate", shared.rate.clone())
                .placeholder("Unknown")
                .flex(1.0),
            ComboBox::from_state("Buffer size", shared.buffer.clone())
                .placeholder("Default")
                .flex(1.0),
        ))
        .spacing(SPACING);
        let ports = reveal(
            &shared.ports,
            Style::column(),
            shared
                .port_names
                .iter()
                .zip(&shared.port_open)
                .map(|(name, open)| Toggle::from_state(name.clone(), open.clone()).into_view())
                .collect::<Vec<_>>(),
        );
        let inputs = reveal(
            &shared.midi,
            Style::stack(),
            vec![label("No MIDI input ports").muted().into_view(), ports],
        );
        let midi = reveal(
            &shared.midi_sections,
            Style::column().with_spacing(SPACING),
            vec![
                label("MIDI inputs").heading().into_view(),
                inputs,
                ComboBox::from_state("MIDI output", shared.midi_output.clone())
                    .placeholder("No port")
                    .into_view(),
            ],
        );
        let report = reveal(
            &shared.lines,
            Style::column(),
            shared
                .report
                .iter()
                .map(|line| Label::from_state(line.clone()).caption().into_view())
                .collect::<Vec<_>>(),
        );
        reveal(
            &shared.sections,
            Style::column().with_spacing(SPACING),
            vec![devices, format, midi, report],
        )
        .named("Audio settings")
    }
}

/// The choices of a list of "None", then `devices`, with `chosen`
/// selected: "None" for none.
fn with_none(devices: &[DeviceItem], chosen: &Option<String>) -> Choices {
    let mut items = vec![String::from("None")];
    items.extend(devices.iter().map(|device| device.name.clone()));
    let selected = match chosen {
        None => Some(0),
        Some(id) => devices
            .iter()
            .position(|device| device.id == *id)
            .map(|at| at + 1),
    };
    Choices { items, selected }
}

/// The identifier picked at `at` in a list [`with_none`] made: `Some(None)`
/// for "None", `None` past the end.
fn after_none(devices: &[DeviceItem], at: usize) -> Option<Option<String>> {
    match at {
        0 => Some(None),
        _ => devices.get(at - 1).map(|device| Some(device.id.clone())),
    }
}

/// Sets `state` to `value` if it holds another.
fn set<T: Clone + PartialEq + 'static>(state: &State<T>, value: T) {
    if state.with(|held| *held != value) {
        state.set(value);
    }
}

/// How a buffer of `frames` is listed: in samples, and in milliseconds at
/// `rate` if it is known.
fn buffer_text(frames: u32, rate: Option<u32>) -> String {
    match rate.filter(|&rate| rate > 0) {
        Some(rate) => {
            let ms = f64::from(frames) * 1000.0 / f64::from(rate);
            format!("{frames} samples ({ms:.1} ms)")
        }
        None => format!("{frames} samples"),
    }
}

/// A container laid out by `style` that shows the children `shown` says.
fn reveal(shown: &State<Shown>, style: Style, children: Vec<View>) -> View {
    View::with_children(
        Reveal {
            shown: shown.clone(),
            seen: None,
        },
        style,
        children,
    )
}

/// The widget of [`reveal`]: a group whose children come and go with a
/// state.
struct Reveal {
    shown: State<Shown>,
    seen: Option<u64>,
}

impl Widget for Reveal {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Group, "")
    }

    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        Size::default()
    }

    fn update(&mut self, cx: &mut UpdateCx<'_>) {
        let version = self.shown.version();
        if self.seen != Some(version) {
            self.seen = Some(version);
            cx.show(self.shown.get());
        }
    }

    fn paint(&mut self, _cx: &PaintCx<'_>, _scene: &mut Scene) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_is_listed_in_samples_and_milliseconds() {
        assert_eq!(buffer_text(256, None), "256 samples");
        assert_eq!(buffer_text(480, Some(48_000)), "480 samples (10.0 ms)");
        assert_eq!(buffer_text(64, Some(0)), "64 samples");
    }

    #[test]
    fn a_long_report_is_folded_into_its_last_line() {
        let settings = AudioSettings::new();
        settings.set_model(AudioSettingsModel {
            report: "one\ntwo\n\nthree\nfour\nfive".into(),
            ..AudioSettingsModel::default()
        });
        let lines: Vec<String> = settings.shared.report.iter().map(State::get).collect();
        assert_eq!(lines, ["one", "two", "three", "four; five"]);
        assert_eq!(settings.shared.lines.get(), Shown::First(4));
    }

    #[test]
    fn the_settings_can_be_kept_by_an_editor_spec() {
        fn kept_by_a_spec<T: Send + Sync + 'static>() {}
        kept_by_a_spec::<AudioSettings>();
    }
}
