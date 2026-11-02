//! The audio settings view driving a standalone plugin, through
//! `audio_settings::sync`, on the headless device backend: no sound card
//! or MIDI port is opened, so these run on every system.

use std::sync::Arc;
use std::time::Duration;

use manguebyte_core::AudioBuffer;
use manguebyte_devices::{DeviceManager, Direction, MockBackend};
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_gfx::Size;
use manguebyte_gui::audio_settings;
use manguebyte_params::{FloatParam, FloatRange, param_set};
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};
use manguebyte_standalone::{Standalone, StandaloneOptions};
use manguebyte_ui::audio_settings::{AudioSettings, AudioSettingsRequest, DeviceItem};
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{Role, Theme};

struct Params {
    level: FloatParam,
}
param_set!(Params { level });

/// Plays its level; an instrument.
struct Synth(Arc<Params>);

impl Plugin for Synth {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.gui-settings-synth",
        name: "Synth",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Plays its level",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = Params;

    fn new() -> Self {
        Self(Arc::new(Params {
            level: FloatParam::new("level", "Level", FloatRange::linear(0.0, 1.0), 0.0),
        }))
    }
    fn params(&self) -> Arc<Params> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {
        let level = self.0.level.value();
        for channel in 0..buffer.num_channels() {
            buffer.channel_mut(channel).fill(level);
        }
    }
}

/// Passes its input through; an effect.
struct Thru(Arc<Params>);

impl Plugin for Thru {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.gui-settings-thru",
        name: "Thru",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Passes its input",
        kind: PluginKind::Effect,
    };
    const LAYOUT: AudioLayout = AudioLayout::MONO_EFFECT;
    type Params = Params;

    fn new() -> Self {
        Self(Synth::new().0)
    }
    fn params(&self) -> Arc<Params> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

/// Two outputs at 48 kHz, one at 44.1 kHz, a microphone, two keyboards.
fn studio() -> MockBackend {
    let mock = MockBackend::new();
    for (id, name, rate) in [
        ("speakers", "Speakers", 48_000),
        ("phones", "Headphones", 48_000),
        ("usb", "USB Interface", 44_100),
    ] {
        mock.add_device(Direction::Output, MockBackend::device(id, name, 2, rate));
    }
    mock.add_device(
        Direction::Input,
        MockBackend::device("mic", "Microphone", 1, 48_000),
    );
    mock.add_midi_port(Direction::Input, "keys", "Keys");
    mock.add_midi_port(Direction::Input, "pads", "Pads");
    mock
}

fn start<P: Plugin>(mock: &MockBackend) -> Standalone<P> {
    let mut manager = DeviceManager::headless(mock);
    manager.set_crossfade(Duration::ZERO);
    Standalone::<P>::start_on(manager, StandaloneOptions::default()).unwrap()
}

fn open(settings: &AudioSettings) -> Harness {
    let mut ui = Harness::new(settings.view(), Size::new(360.0, 480.0), 1.0);
    ui.set_theme(Theme::dark());
    ui.frame();
    ui
}

fn pick(ui: &mut Harness, name: &str, item: &str) {
    ui.click(ui.get(Role::ComboBox, name).center());
    ui.frame();
    ui.click(ui.get(Role::MenuItem, item).center());
    ui.frame();
}

#[test]
fn the_first_sync_fills_the_model_in_and_a_quiet_one_does_nothing() {
    let mock = studio();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    assert!(settings.is_stale());
    assert!(audio_settings::sync(&mut app, &settings));
    assert!(!settings.is_stale());
    let model = settings.model();
    assert_eq!(
        model.outputs,
        [
            DeviceItem::new("speakers", "Speakers"),
            DeviceItem::new("phones", "Headphones"),
            DeviceItem::new("usb", "USB Interface"),
        ]
    );
    assert_eq!(model.output.as_deref(), Some("speakers"));
    assert!(!model.show_input, "an instrument has no input");
    assert!(model.inputs.is_empty());
    assert_eq!(model.sample_rate, Some(48_000));
    assert!(model.sample_rates.contains(&48_000));
    assert_eq!(
        model
            .midi_inputs
            .iter()
            .map(|port| (port.id.as_str(), port.open))
            .collect::<Vec<_>>(),
        [("keys", true), ("pads", true)]
    );
    assert_eq!(model.report, app.report().to_string());

    // Nothing picked, nothing plugged: nothing to do.
    assert!(!audio_settings::sync(&mut app, &settings));

    let ui = open(&settings);
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("Speakers")
    );
    assert!(ui.find(Role::ComboBox, "Input device").is_none());
    ui.assert_accessible();
}

#[test]
fn a_pick_reaches_the_standalone_and_its_report_comes_back() {
    let mock = studio();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    let mut ui = open(&settings);

    pick(&mut ui, "Output device", "USB Interface");
    assert!(audio_settings::sync(&mut app, &settings));
    let report = app.report().clone();
    assert_eq!(report.output.as_ref().map(|d| d.id.as_str()), Some("usb"));
    assert_eq!(report.device_sample_rate, Some(44_100), "the rate follows");
    assert!(mock.is_running());

    // The view shows what runs now, and the report.
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("USB Interface")
    );
    assert_eq!(
        ui.get(Role::ComboBox, "Sample rate").value_text(),
        Some("44100 Hz")
    );
    let first_line = report.to_string().lines().next().unwrap().to_owned();
    assert!(ui.find(Role::Label, &first_line).is_some(), "{first_line}");

    // A sample rate and a buffer size.
    pick(&mut ui, "Sample rate", "48000 Hz");
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.report().device_sample_rate, Some(48_000));
    assert_eq!(app.settings().sample_rate, Some(48_000));
    ui.frame();
    let buffers: Vec<String> = settings
        .model()
        .buffer_sizes
        .iter()
        .map(u32::to_string)
        .collect();
    assert!(buffers.contains(&String::from("512")), "{buffers:?}");
    pick(&mut ui, "Buffer size", "512 samples (10.7 ms)");
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.report().buffer_size, Some(512));
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Buffer size").value_text(),
        Some("512 samples (10.7 ms)")
    );
    assert!(settings.take_requests().is_empty());
}

#[test]
fn a_midi_switch_opens_and_closes_the_port() {
    let mock = studio();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    let mut ui = open(&settings);

    ui.click(ui.get(Role::Switch, "Keys").center());
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.midi_input_names(), ["Pads"]);
    assert!(!mock.send_midi("keys", &[0x90, 60, 100]));
    ui.frame();
    assert_eq!(ui.get(Role::Switch, "Keys").node.state.checked, Some(false));

    ui.click(ui.get(Role::Switch, "Keys").center());
    assert!(audio_settings::sync(&mut app, &settings));
    assert!(mock.send_midi("keys", &[0x90, 60, 100]));
    ui.frame();
    assert_eq!(ui.get(Role::Switch, "Keys").node.state.checked, Some(true));
}

#[test]
fn devices_that_come_and_go_are_followed_and_the_choice_stays_valid() {
    let mock = studio();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    let mut ui = open(&settings);

    // A device and a port are plugged in.
    mock.add_device(
        Direction::Output,
        MockBackend::device("hdmi", "HDMI", 2, 48_000),
    );
    mock.add_midi_port(Direction::Input, "drums", "Drums");
    assert!(audio_settings::sync(&mut app, &settings));
    ui.frame();
    assert!(settings.model().outputs.iter().any(|d| d.id == "hdmi"));
    assert_eq!(ui.get(Role::Switch, "Drums").node.state.checked, Some(true));

    // The device that plays goes away: the default takes over, and the
    // view shows it chosen.
    pick(&mut ui, "Output device", "Headphones");
    audio_settings::sync(&mut app, &settings);
    assert!(mock.remove_device(Direction::Output, "phones"));
    assert!(audio_settings::sync(&mut app, &settings));
    ui.frame();
    let model = settings.model();
    assert!(model.outputs.iter().all(|d| d.id != "phones"));
    let playing = app.report().output.clone().map(|d| d.name);
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        playing.as_deref().or(Some("No device"))
    );
    assert!(settings.take_requests().is_empty(), "no pick was invented");
}

#[test]
fn an_effects_input_can_be_turned_off_and_on() {
    let mock = studio();
    let mut app = start::<Thru>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    assert!(settings.model().show_input);
    let mut ui = open(&settings);
    assert_eq!(
        ui.get(Role::ComboBox, "Input device").value_text(),
        Some("Microphone")
    );
    pick(&mut ui, "Input device", "None");
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.report().input, None);
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Input device").value_text(),
        Some("None")
    );
    pick(&mut ui, "Input device", "Microphone");
    audio_settings::sync(&mut app, &settings);
    assert_eq!(app.input_name(), Some("Microphone"));
}

#[test]
fn each_request_is_carried_out_by_the_matching_setter() {
    let mock = studio();
    let mut app = start::<Thru>(&mock);
    let report = audio_settings::apply(&mut app, &AudioSettingsRequest::Output("phones".into()));
    assert_eq!(report.output.as_ref().unwrap().name, "Headphones");
    let report = audio_settings::apply(&mut app, &AudioSettingsRequest::Input(None));
    assert!(report.input.is_none());
    let report = audio_settings::apply(&mut app, &AudioSettingsRequest::SampleRate(96_000));
    assert_eq!(report.device_sample_rate, Some(96_000));
    let report = audio_settings::apply(&mut app, &AudioSettingsRequest::BufferSize(256));
    assert_eq!(report.buffer_size, Some(256));
    let report = audio_settings::apply(
        &mut app,
        &AudioSettingsRequest::MidiInput {
            id: "pads".into(),
            open: false,
        },
    );
    assert_eq!(report.midi_inputs.len(), 1);
}

#[test]
fn a_program_that_changed_the_settings_itself_marks_the_model_stale() {
    let mock = studio();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    app.set_output_device("usb");
    settings.mark_stale();
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(settings.model().output.as_deref(), Some("usb"));
}

#[test]
fn listing_that_fails_shows_empty_lists_and_says_why() {
    let mock = studio();
    let app = start::<Synth>(&mock);
    mock.fail_listing(true);
    let model = audio_settings::model(&app);
    assert!(model.outputs.is_empty());
    assert!(
        model.report.contains("The devices cannot be listed"),
        "{}",
        model.report
    );
    mock.fail_listing(false);
}

/// Sends notes; an instrument with a MIDI output.
struct Arp(Arc<Params>);

impl Plugin for Arp {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.gui-settings-arp",
        name: "Arp",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Sends notes",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    const NOTE_OUTPUT: bool = true;
    type Params = Params;

    fn new() -> Self {
        Self(Synth::new().0)
    }
    fn params(&self) -> Arc<Params> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

/// The studio with two MIDI output ports.
fn studio_with_outputs() -> MockBackend {
    let mock = studio();
    mock.add_midi_port(Direction::Output, "synth", "Hardware Synth");
    mock.add_midi_port(Direction::Output, "loop", "Loopback");
    mock
}

#[test]
fn the_midi_output_pick_reaches_a_plugin_that_sends_notes() {
    let mock = studio_with_outputs();
    let mut app = start::<Arp>(&mock);
    let settings = AudioSettings::new();
    assert!(audio_settings::sync(&mut app, &settings));
    let model = settings.model();
    assert!(model.show_midi_output);
    assert_eq!(
        model.midi_outputs,
        [
            DeviceItem::new("synth", "Hardware Synth"),
            DeviceItem::new("loop", "Loopback"),
        ]
    );
    assert_eq!(model.midi_output, None);
    let mut ui = open(&settings);
    assert_eq!(
        ui.get(Role::ComboBox, "MIDI output").value_text(),
        Some("None")
    );

    pick(&mut ui, "MIDI output", "Hardware Synth");
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.midi_output_name(), Some("Hardware Synth"));
    assert_eq!(settings.model().midi_output.as_deref(), Some("synth"));
    assert!(app.report().to_string().contains("Hardware Synth"));
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "MIDI output").value_text(),
        Some("Hardware Synth")
    );

    pick(&mut ui, "MIDI output", "None");
    assert!(audio_settings::sync(&mut app, &settings));
    assert_eq!(app.midi_output_name(), None);
    assert_eq!(app.settings().midi_output, None);
    assert!(!audio_settings::sync(&mut app, &settings), "quiet");
}

#[test]
fn a_plugin_that_sends_no_notes_is_offered_no_midi_output() {
    let mock = studio_with_outputs();
    let mut app = start::<Synth>(&mock);
    let settings = AudioSettings::new();
    audio_settings::sync(&mut app, &settings);
    assert!(!settings.model().show_midi_output);
    let ui = open(&settings);
    assert!(ui.find(Role::ComboBox, "MIDI output").is_none());
}
