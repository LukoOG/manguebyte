//! The audio settings view: what it shows of a model, the requests the
//! person's picks make, a model whose devices change, the keyboard and
//! assistive technology.

use manguebyte_gfx::{Point, Size};
use manguebyte_ui::audio_settings::{
    AudioSettings, AudioSettingsModel, AudioSettingsRequest, DeviceItem, MAX_MIDI_PORTS,
    MidiPortItem, REPORT_LINES,
};
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{Action, IntoView, Key, Role, State, Theme, floating_panel, label};

use AudioSettingsRequest::{BufferSize, Input, MidiInput, Output, SampleRate};

/// An effect's settings: three outputs, a microphone, two MIDI ports.
fn model() -> AudioSettingsModel {
    AudioSettingsModel {
        outputs: vec![
            DeviceItem::new("spk", "Speakers"),
            DeviceItem::new("hp", "Headphones"),
            DeviceItem::new("usb", "USB Interface"),
        ],
        output: Some("spk".into()),
        inputs: vec![DeviceItem::new("mic", "Microphone")],
        input: Some("mic".into()),
        show_input: true,
        sample_rates: vec![44_100, 48_000, 96_000],
        sample_rate: Some(48_000),
        buffer_sizes: vec![128, 256, 512],
        buffer_size: Some(256),
        midi_inputs: vec![
            MidiPortItem::new("keys", "Keys", true),
            MidiPortItem::new("pads", "Pads", false),
        ],
        report:
            "Playing on \"Speakers\" at 48000 Hz, 256-frame buffer; recording from \"Microphone\""
                .into(),
    }
}

fn open(settings: &AudioSettings) -> Harness {
    let mut ui = Harness::new(settings.view(), Size::new(360.0, 480.0), 1.0);
    ui.frame();
    ui
}

fn settings(model: AudioSettingsModel) -> AudioSettings {
    let settings = AudioSettings::new();
    settings.set_model(model);
    settings
}

/// Opens the combo box `name` and picks `item` from its menu.
fn pick(ui: &mut Harness, name: &str, item: &str) {
    ui.click(ui.get(Role::ComboBox, name).center());
    ui.frame();
    ui.click(ui.get(Role::MenuItem, item).center());
    ui.frame();
}

#[test]
fn the_view_shows_every_section_of_the_model() {
    let settings = settings(model());
    let ui = open(&settings);
    let value = |name: &str| ui.get(Role::ComboBox, name).value_text().map(str::to_owned);
    assert_eq!(value("Output device").as_deref(), Some("Speakers"));
    assert_eq!(value("Input device").as_deref(), Some("Microphone"));
    assert_eq!(value("Sample rate").as_deref(), Some("48000 Hz"));
    assert_eq!(
        value("Buffer size").as_deref(),
        Some("256 samples (5.3 ms)")
    );
    assert!(ui.find(Role::Label, "MIDI inputs").is_some());
    assert_eq!(ui.get(Role::Switch, "Keys").node.state.checked, Some(true));
    assert_eq!(ui.get(Role::Switch, "Pads").node.state.checked, Some(false));
    assert!(ui.find(Role::Label, "No MIDI input ports").is_none());
    assert!(
        ui.find(
            Role::Label,
            "Playing on \"Speakers\" at 48000 Hz, 256-frame buffer; recording from \"Microphone\""
        )
        .is_some()
    );
    assert!(ui.find(Role::Group, "Audio settings").is_some());
    ui.assert_accessible();
    // The slots for ports not there are hidden: two switches, no more.
    let switches = ui
        .everything()
        .into_iter()
        .filter(|found| found.role() == Role::Switch)
        .count();
    assert_eq!(switches, 2);
}

#[test]
fn each_pick_makes_one_request() {
    let settings = settings(model());
    let mut ui = open(&settings);
    assert!(settings.take_requests().is_empty(), "nothing picked yet");

    pick(&mut ui, "Output device", "USB Interface");
    pick(&mut ui, "Input device", "None");
    pick(&mut ui, "Sample rate", "96000 Hz");
    pick(&mut ui, "Buffer size", "128 samples (2.7 ms)");
    ui.click(ui.get(Role::Switch, "Pads").center());
    ui.click(ui.get(Role::Switch, "Keys").center());
    assert_eq!(
        settings.take_requests(),
        [
            Output("usb".into()),
            Input(None),
            SampleRate(96_000),
            BufferSize(128),
            MidiInput {
                id: "keys".into(),
                open: false
            },
            MidiInput {
                id: "pads".into(),
                open: true
            },
        ]
    );
    assert!(settings.take_requests().is_empty(), "each asked for once");
    // The picks are in the model now, as the program will find them.
    let taken = settings.model();
    assert_eq!(taken.output.as_deref(), Some("usb"));
    assert_eq!(taken.input, None);
    assert_eq!(taken.sample_rate, Some(96_000));
    assert_eq!(taken.buffer_size, Some(128));

    // Back to the microphone.
    pick(&mut ui, "Input device", "Microphone");
    assert_eq!(settings.take_requests(), [Input(Some("mic".into()))]);
}

#[test]
fn picking_what_is_shown_already_asks_nothing() {
    let settings = settings(model());
    let mut ui = open(&settings);
    pick(&mut ui, "Output device", "Speakers");
    pick(&mut ui, "Sample rate", "48000 Hz");
    assert!(settings.take_requests().is_empty());
}

#[test]
fn a_section_changed_twice_asks_once_for_its_last_value() {
    let settings = settings(model());
    let mut ui = open(&settings);
    pick(&mut ui, "Output device", "Headphones");
    pick(&mut ui, "Output device", "USB Interface");
    assert_eq!(settings.take_requests(), [Output("usb".into())]);
}

#[test]
fn the_keyboard_reaches_every_control_in_order_and_changes_them() {
    let settings = settings(model());
    let mut ui = open(&settings);
    let mut order = Vec::new();
    for _ in 0..6 {
        assert!(ui.tab());
        let focused = ui.focused().expect("Tab gives the focus");
        order.push(focused.name().to_owned());
    }
    assert_eq!(
        order,
        [
            "Output device",
            "Input device",
            "Sample rate",
            "Buffer size",
            "Keys",
            "Pads"
        ]
    );
    // Pads has the focus: Space opens it.
    assert!(ui.key(Key::Character(' ')));
    // Back to the sample rate, and up one.
    for _ in 0..3 {
        assert!(ui.shift_tab());
    }
    assert_eq!(ui.focused().unwrap().name(), "Sample rate");
    assert!(ui.key(Key::Down));
    assert_eq!(
        settings.take_requests(),
        [
            SampleRate(96_000),
            MidiInput {
                id: "pads".into(),
                open: true
            }
        ]
    );
    ui.frame();
    ui.assert_accessible();
}

#[test]
fn assistive_technology_can_pick_and_toggle() {
    let settings = settings(model());
    let mut ui = open(&settings);
    let output = ui.get(Role::ComboBox, "Output device");
    assert!(ui.act(&output, Action::Press));
    ui.frame();
    let phones = ui.get(Role::MenuItem, "Headphones");
    assert!(ui.act(&phones, Action::Press));
    let keys = ui.get(Role::Switch, "Keys");
    assert!(ui.act(&keys, Action::Press));
    assert_eq!(
        settings.take_requests(),
        [
            Output("hp".into()),
            MidiInput {
                id: "keys".into(),
                open: false
            }
        ]
    );
}

#[test]
fn a_model_whose_devices_change_keeps_a_valid_selection() {
    let settings = settings(model());
    let mut ui = open(&settings);

    // A device comes before the one in use: the same device stays chosen.
    let mut next = model();
    next.outputs.insert(0, DeviceItem::new("hdmi", "HDMI"));
    settings.set_model(next.clone());
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("Speakers")
    );
    assert!(settings.take_requests().is_empty(), "no pick was made");

    // The device in use goes away and nothing plays: nothing is chosen,
    // and the box says so.
    next.outputs.retain(|device| device.id != "spk");
    next.output = None;
    next.sample_rate = None;
    settings.set_model(next.clone());
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("No device")
    );
    assert_eq!(
        ui.get(Role::ComboBox, "Sample rate").value_text(),
        Some("Unknown")
    );
    assert_eq!(
        ui.get(Role::ComboBox, "Buffer size").value_text(),
        Some("256 samples"),
        "no rate, no milliseconds"
    );
    // A current value that is not in its list shows as nothing chosen.
    next.output = Some("gone".into());
    settings.set_model(next.clone());
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("No device")
    );
    assert!(settings.take_requests().is_empty());

    // A pick from what is listed now still names the right device.
    pick(&mut ui, "Output device", "Headphones");
    assert_eq!(settings.take_requests(), [Output("hp".into())]);

    // A port goes away, another comes: the switches follow, by slot.
    next.midi_inputs = vec![MidiPortItem::new("synth", "Synth", true)];
    settings.set_model(next.clone());
    ui.frame();
    assert!(ui.find(Role::Switch, "Keys").is_none());
    assert!(ui.find(Role::Switch, "Pads").is_none());
    assert_eq!(ui.get(Role::Switch, "Synth").node.state.checked, Some(true));
    next.midi_inputs.clear();
    settings.set_model(next);
    ui.frame();
    assert!(ui.find(Role::Switch, "Synth").is_none());
    assert!(ui.find(Role::Label, "No MIDI input ports").is_some());
    ui.assert_accessible();
    assert!(settings.take_requests().is_empty());
}

#[test]
fn a_pick_not_taken_before_a_new_model_is_kept() {
    let settings = settings(model());
    let mut ui = open(&settings);
    pick(&mut ui, "Output device", "Headphones");
    ui.click(ui.get(Role::Switch, "Pads").center());
    // The program sets a new model before it polls: a port was added in
    // front of the others.
    let mut next = model();
    next.midi_inputs
        .insert(0, MidiPortItem::new("new", "New", false));
    settings.set_model(next);
    ui.frame();
    assert_eq!(
        settings.take_requests(),
        [
            Output("hp".into()),
            MidiInput {
                id: "pads".into(),
                open: true
            }
        ]
    );
    // The new model shows what it says, not the pick.
    assert_eq!(
        ui.get(Role::ComboBox, "Output device").value_text(),
        Some("Speakers")
    );
    assert_eq!(ui.get(Role::Switch, "Pads").node.state.checked, Some(false));
}

#[test]
fn an_instrument_has_no_input_section() {
    let settings = settings(AudioSettingsModel {
        show_input: false,
        ..model()
    });
    let mut ui = open(&settings);
    assert!(ui.find(Role::ComboBox, "Input device").is_none());
    let output = ui.get(Role::ComboBox, "Output device").bounds;
    let rate = ui.get(Role::ComboBox, "Sample rate").bounds;
    assert!(rate.y - output.bottom() < 9.0, "no room left for it");
    assert!(ui.tab());
    assert!(ui.tab());
    assert_eq!(ui.focused().unwrap().name(), "Sample rate");
    // Shown again when the model says so.
    settings.set_model(model());
    ui.frame();
    assert!(ui.find(Role::ComboBox, "Input device").is_some());
}

#[test]
fn the_report_is_shown_line_by_line_and_left_out_when_empty() {
    let mut with_report = model();
    with_report.report = "Playing on \"Speakers\" at 48000 Hz\n\
        The output \"Interface\" is missing; \"Speakers\" plays instead"
        .into();
    let settings = settings(with_report.clone());
    let mut ui = open(&settings);
    assert!(
        ui.find(Role::Label, "Playing on \"Speakers\" at 48000 Hz")
            .is_some()
    );
    assert!(
        ui.find(
            Role::Label,
            "The output \"Interface\" is missing; \"Speakers\" plays instead"
        )
        .is_some()
    );
    let last = ui.get(Role::Switch, "Pads").bounds;
    // Many lines: the last shown holds the rest.
    with_report.report = (1..=REPORT_LINES + 2)
        .map(|n| format!("Line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    settings.set_model(with_report.clone());
    ui.frame();
    let joined = (REPORT_LINES..=REPORT_LINES + 2)
        .map(|n| format!("Line {n}"))
        .collect::<Vec<_>>()
        .join("; ");
    assert!(ui.find(Role::Label, &joined).is_some(), "{joined}");
    with_report.report.clear();
    settings.set_model(with_report);
    ui.frame();
    assert!(ui.find(Role::Label, "Line 1").is_none());
    assert_eq!(ui.get(Role::Switch, "Pads").bounds, last);
}

#[test]
fn ports_past_the_limit_are_not_shown() {
    let mut many = model();
    many.midi_inputs = (0..MAX_MIDI_PORTS + 3)
        .map(|n| MidiPortItem::new(format!("p{n}"), format!("Port {n}"), false))
        .collect();
    let settings = settings(many);
    let ui = Harness::new(settings.view(), Size::new(360.0, 1_200.0), 1.0);
    let mut ui = ui;
    ui.frame();
    let last = format!("Port {}", MAX_MIDI_PORTS - 1);
    assert!(ui.find(Role::Switch, &last).is_some());
    let past = format!("Port {MAX_MIDI_PORTS}");
    assert!(ui.find(Role::Switch, &past).is_none());
}

#[test]
fn the_same_model_again_draws_nothing() {
    let settings = settings(model());
    let mut ui = open(&settings);
    ui.advance_ms(1_000);
    settings.set_model(model());
    let frame = ui.frame();
    assert_eq!(frame.damaged, 0);
    assert!(!frame.laid_out);
}

#[test]
fn a_view_built_again_shows_the_same_states() {
    let settings = settings(model());
    let mut first = open(&settings);
    pick(&mut first, "Output device", "Headphones");
    drop(first);
    // Opened again, as an editor reopens: the pick is still there and
    // still to be taken.
    let second = open(&settings);
    assert_eq!(
        second.get(Role::ComboBox, "Output device").value_text(),
        Some("Headphones")
    );
    assert_eq!(settings.take_requests(), [Output("hp".into())]);
}

#[test]
fn the_view_fits_in_a_floating_panel() {
    let settings = settings(model());
    let open_panel = State::new(true);
    let view = label("Synth").padding(8.0).with_panels([floating_panel(
        "Audio settings",
        open_panel.clone(),
        State::new(Point::new(10.0, 10.0)),
        settings.view(),
    )]);
    let mut ui = Harness::new(view, Size::new(480.0, 520.0), 1.0);
    ui.set_theme(Theme::light());
    ui.frame();
    let region = ui.get(Role::Region, "Audio settings");
    let output = ui.get(Role::ComboBox, "Output device");
    assert!(region.bounds.contains(output.center()));
    ui.assert_accessible();
    // Escape closes the panel once the focus is in it.
    ui.act(&output, Action::Focus);
    assert!(ui.key(Key::Escape));
    ui.frame();
    assert!(!open_panel.get());
    assert!(ui.find(Role::ComboBox, "Output device").is_none());
}
