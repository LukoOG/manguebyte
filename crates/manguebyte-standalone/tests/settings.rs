//! Choosing devices, rate, buffer and MIDI ports for a standalone plugin,
//! and changing them while it plays, against the headless backend: no
//! sound card is opened, so these run on every system.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use manguebyte_core::AudioBuffer;
use manguebyte_devices::{
    DeviceChoice, DeviceManager, DeviceRef, DeviceSummary, Direction, Fallback, MockBackend,
    SwapKind,
};
use manguebyte_dsp::event::EventKind;
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_params::{FloatParam, FloatRange, param_set};
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind};
use manguebyte_standalone::{
    MidiInputs, RatePolicy, SettingsReport, Standalone, StandaloneOptions, StandaloneSettings,
};

/// What a [`Counter`] instance tells about itself, reached through its
/// parameters, which only that instance holds.
struct CounterParams {
    level: FloatParam,
    /// The sample rate of every `prepare`, in order.
    prepared: Mutex<Vec<f64>>,
    /// Blocks processed.
    blocks: AtomicU64,
    /// Note-ons received.
    notes: AtomicU64,
}
param_set!(CounterParams { level });

/// Plays its level on the left and how many blocks it has processed on
/// the right, so that a new instance, or one prepared again, shows.
struct Counter(Arc<CounterParams>);

impl Plugin for Counter {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.standalone-counter",
        name: "Counter",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Counts its blocks",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = CounterParams;

    fn new() -> Self {
        Self(Arc::new(CounterParams {
            level: FloatParam::new("level", "Level", FloatRange::linear(0.0, 1.0), 0.5),
            prepared: Mutex::new(Vec::new()),
            blocks: AtomicU64::new(0),
            notes: AtomicU64::new(0),
        }))
    }
    fn params(&self) -> Arc<CounterParams> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.0.prepared.lock().unwrap().push(spec.sample_rate);
    }
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>) {
        let blocks = self.0.blocks.fetch_add(1, Ordering::Relaxed) + 1;
        for event in context.events {
            if let EventKind::NoteOn { .. } = event.kind {
                self.0.notes.fetch_add(1, Ordering::Relaxed);
            }
        }
        buffer.channel_mut(0).fill(self.0.level.value());
        buffer.channel_mut(1).fill(blocks as f32);
    }
}

/// Halves its mono input.
struct Half(Arc<CounterParams>);

impl Plugin for Half {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.standalone-half-effect",
        name: "Half",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Halves its input",
        kind: PluginKind::Effect,
    };
    const LAYOUT: AudioLayout = AudioLayout::MONO_EFFECT;
    type Params = CounterParams;

    fn new() -> Self {
        Self(Counter::new().0)
    }
    fn params(&self) -> Arc<CounterParams> {
        Arc::clone(&self.0)
    }
    fn prepare(&mut self, _: &ProcessSpec) {}
    fn process(&mut self, buffer: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {
        buffer.apply_gain(0.5);
    }
}

/// Two outputs at 48 kHz, one at 44.1 kHz, a microphone, and two MIDI
/// keyboards.
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

fn summary(id: &str, name: &str) -> DeviceSummary {
    DeviceSummary {
        id: id.into(),
        name: name.into(),
    }
}

fn start<P: Plugin>(mock: &MockBackend, options: StandaloneOptions) -> Standalone<P> {
    let mut manager = DeviceManager::headless(mock);
    manager.set_crossfade(Duration::ZERO);
    Standalone::<P>::start_on(manager, options).unwrap()
}

fn with_settings(settings: StandaloneSettings) -> StandaloneOptions {
    StandaloneOptions {
        settings,
        ..StandaloneOptions::default()
    }
}

/// One block of 64 frames from the output device `id`, as (left, right)
/// of its last frame.
fn play(mock: &MockBackend, id: &str) -> Option<(f32, f32)> {
    let mut block = [0.0f32; 2 * 64];
    mock.render_on(id, &mut block)
        .then(|| (block[2 * 63], block[2 * 63 + 1]))
}

fn prepared<P: Plugin<Params = CounterParams>>(app: &Standalone<P>) -> Vec<f64> {
    app.params().prepared.lock().unwrap().clone()
}

#[test]
fn the_options_open_the_devices_rate_buffer_and_ports_asked_for() {
    let mock = studio();
    let settings = StandaloneSettings::default()
        .with_output(DeviceChoice::Device(DeviceRef::new("usb", "USB Interface")))
        .with_sample_rate(96_000)
        .with_buffer_size(128)
        .with_midi_inputs(MidiInputs::ids(["pads"]));
    let app = start::<Counter>(&mock, with_settings(settings.clone()));

    let report = app.report();
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.output, Some(summary("usb", "USB Interface")));
    assert_eq!(report.input, None, "an instrument opens no input");
    assert_eq!(report.device_sample_rate, Some(96_000));
    assert_eq!(report.buffer_size, Some(128));
    assert_eq!(report.plugin_sample_rate, 96_000);
    assert_eq!(report.midi_inputs, [summary("pads", "Pads")]);
    assert_eq!(app.sample_rate(), 96_000.0);
    assert_eq!(app.device_name(), "USB Interface");
    assert_eq!(app.midi_input_names(), ["Pads"]);
    assert_eq!(app.settings(), &settings);
    let (device, opened) = mock.running(Direction::Output).unwrap();
    assert_eq!(device, "usb");
    assert_eq!(
        (opened.sample_rate, opened.buffer_size),
        (Some(96_000), Some(128))
    );
    // Prepared once, at the device's rate, though it was made at another.
    assert_eq!(prepared(&app), [96_000.0]);
    assert_eq!(
        report.to_string(),
        "Playing on \"USB Interface\" at 96000 Hz, 128-frame buffer"
    );

    // Notes from the port asked for reach the plugin; the other port is
    // not open.
    assert!(mock.send_midi("pads", &[0x90, 60, 100]));
    assert!(!mock.send_midi("keys", &[0x90, 61, 100]));
    assert_eq!(play(&mock, "usb"), Some((0.5, 1.0)));
    assert_eq!(app.params().notes.load(Ordering::Relaxed), 1);
}

#[test]
fn what_cannot_be_had_falls_back_and_the_report_says_so() {
    let mock = studio();
    let settings = StandaloneSettings::default()
        .with_output(DeviceChoice::Device(DeviceRef::new("gone", "Gone")))
        .with_buffer_size(8)
        .with_midi_inputs(MidiInputs::Only(vec![
            DeviceRef::new("keys-2", "Keys"),
            DeviceRef::new("nope", "Nope"),
        ]));
    let app = start::<Counter>(&mock, with_settings(settings.clone()));
    let report = app.report();
    assert_eq!(report.output, Some(summary("speakers", "Speakers")));
    assert_eq!(report.buffer_size, None, "the device chooses");
    assert_eq!(report.midi_inputs, [summary("keys", "Keys")], "by its name");
    assert!(
        matches!(
            &report.fallbacks[..],
            [
                Fallback::DeviceReplaced { by_name: false, .. },
                Fallback::BufferSize { wanted: 8 },
                Fallback::MidiPortReplaced { .. },
                Fallback::MidiPortMissing { .. },
            ]
        ),
        "{:?}",
        report.fallbacks
    );
    assert!(!report.is_exact());
    let text = report.to_string();
    assert_eq!(text.lines().count(), 5, "{text}");
    assert!(text.contains("\"Gone\" is not connected"), "{text}");
    // The settings still name what was asked for, to be used when it
    // comes back.
    assert_eq!(app.settings(), &settings);
}

#[test]
fn with_no_device_the_plugin_waits_for_one() {
    let mock = MockBackend::new();
    let mut app = start::<Counter>(&mock, StandaloneOptions::default());
    assert_eq!(app.report().output, None);
    assert_eq!(app.device_name(), "");
    assert!(matches!(
        &app.report().fallbacks[..],
        [Fallback::NoDevice {
            direction: Direction::Output,
            ..
        }]
    ));
    assert!(app.report().to_string().starts_with("No output device"));
    assert!(app.poll_devices().is_none(), "nothing changed");

    mock.add_device(
        Direction::Output,
        MockBackend::device("late", "Late", 2, 44_100),
    );
    let report = app.poll_devices().expect("a device appeared").clone();
    assert_eq!(report.output, Some(summary("late", "Late")));
    // Made at 48 kHz for want of a device, and prepared again at the
    // device's rate.
    assert_eq!(report.plugin_sample_rate, 44_100);
    assert!(report.prepared_again);
    assert_eq!(prepared(&app), [48_000.0, 44_100.0]);
    assert_eq!(play(&mock, "late"), Some((0.5, 1.0)));
}

#[test]
fn the_same_plugin_plays_on_when_the_output_changes() {
    let mock = studio();
    let mut app = start::<Counter>(&mock, StandaloneOptions::default());
    let state = app.save_state();
    app.params().level.set(0.25);
    for _ in 0..3 {
        play(&mock, "speakers").unwrap();
    }
    assert_eq!(play(&mock, "speakers"), Some((0.25, 4.0)));

    // Another device at the same rate: not prepared again.
    let report = app.set_output_device("phones").clone();
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.output, Some(summary("phones", "Headphones")));
    assert!(!report.prepared_again);
    assert_eq!(report.swap.map(|swap| swap.kind), Some(SwapKind::Gap));
    assert_eq!(play(&mock, "speakers"), None, "closed");
    // The same instance: its level, and its blocks counted on.
    assert_eq!(play(&mock, "phones"), Some((0.25, 5.0)));
    assert_eq!(prepared(&app), [48_000.0]);
    assert_eq!(
        app.settings().output,
        DeviceChoice::Device(DeviceRef::new("phones", "Headphones"))
    );

    // A device at another rate: the same instance, prepared again at it.
    let report = app.set_output_device("usb").clone();
    assert!(report.prepared_again);
    assert_eq!(report.plugin_sample_rate, 44_100);
    assert_eq!(report.device_sample_rate, Some(44_100));
    assert!(!report.is_resampled());
    assert_eq!(app.sample_rate(), 44_100.0);
    assert_eq!(prepared(&app), [48_000.0, 44_100.0]);
    assert_eq!(play(&mock, "usb"), Some((0.25, 6.0)));

    // A buffer size is the device's business: not prepared again.
    let report = app.set_buffer_size(Some(256)).clone();
    assert_eq!(report.buffer_size, Some(256));
    assert!(!report.prepared_again);
    assert_eq!(prepared(&app), [48_000.0, 44_100.0]);
    assert_eq!(play(&mock, "usb"), Some((0.25, 7.0)));

    // The state the program saves is still the plugin's.
    assert!(app.load_state(&state).is_ok());
    assert_eq!(app.params().level.value(), 0.5);
}

#[test]
fn a_fixed_rate_prepares_the_plugin_once_and_resamples_the_device() {
    let mock = studio();
    mock.set_concurrent_streams(true);
    // An interface that runs at 44.1 kHz and nothing else.
    let mut old = MockBackend::device("old", "Old Interface", 2, 44_100);
    for config in &mut old.configs {
        config.min_sample_rate = 44_100;
        config.max_sample_rate = 44_100;
    }
    mock.add_device(Direction::Output, old);
    let options = StandaloneOptions {
        rate_policy: RatePolicy::Fixed(48_000),
        ..StandaloneOptions::default()
    };
    let mut app = start::<Counter>(&mock, options);
    assert_eq!(app.rate_policy(), RatePolicy::Fixed(48_000));
    play(&mock, "speakers").unwrap();

    let report = app.set_output_device("old").clone();
    assert_eq!(report.output, Some(summary("old", "Old Interface")));
    assert_eq!(report.device_sample_rate, Some(44_100));
    assert_eq!(report.plugin_sample_rate, 48_000);
    assert!(report.is_resampled());
    assert!(!report.prepared_again);
    // The rate was the plugin's, not the settings': no fallback to tell.
    assert!(report.is_exact(), "{report}");
    // Both devices can play at once: a crossfade, with no gap.
    assert_eq!(report.swap.map(|swap| swap.kind), Some(SwapKind::Crossfade));
    assert!(report.to_string().contains("resampled from 48000 Hz"));
    // The old device hands the plugin over to the new one as both play.
    let mut handed_over = false;
    for _ in 0..64 {
        let _ = play(&mock, "speakers");
        if play(&mock, "old").is_some_and(|(left, _)| left == 0.5) {
            handed_over = true;
            break;
        }
    }
    assert!(handed_over, "the new device never played the plugin");
    assert_eq!(prepared(&app), [48_000.0]);

    // A device that can run at the plugin's rate is asked for it, though
    // its own is 44.1 kHz.
    let report = app.set_output_device("usb").clone();
    assert_eq!(report.device_sample_rate, Some(48_000));
    assert!(!report.is_resampled());

    // Asking the devices for another rate leaves the plugin's alone.
    let report = app.set_sample_rate(Some(96_000)).clone();
    assert_eq!(report.device_sample_rate, Some(96_000));
    assert_eq!(report.plugin_sample_rate, 48_000);
    assert_eq!(prepared(&app), [48_000.0]);
}

#[test]
fn midi_ports_open_and_close_while_it_plays() {
    let mock = studio();
    let mut app = start::<Counter>(&mock, StandaloneOptions::default());
    assert_eq!(
        app.report().midi_inputs,
        [summary("keys", "Keys"), summary("pads", "Pads")]
    );
    let choices = app.choices().unwrap();
    assert!(choices.midi_inputs.iter().all(|port| port.open));

    // Closing one of all the ports lists the others.
    let report = app.set_midi_input("keys", false).clone();
    assert_eq!(report.midi_inputs, [summary("pads", "Pads")]);
    assert_eq!(
        app.settings().midi_inputs,
        MidiInputs::Only(vec![DeviceRef::new("pads", "Pads")])
    );
    assert!(!mock.send_midi("keys", &[0x90, 60, 100]));
    let choices = app.choices().unwrap();
    assert_eq!(
        choices
            .midi_inputs
            .iter()
            .map(|port| (port.port.id.as_str(), port.open))
            .collect::<Vec<_>>(),
        [("keys", false), ("pads", true)]
    );

    // Opened again, and a port that is not there.
    app.set_midi_input("keys", true);
    assert!(mock.send_midi("keys", &[0x90, 60, 100]));
    let report = app.set_midi_input("ghost", true).clone();
    assert!(matches!(
        &report.fallbacks[..],
        [Fallback::MidiPortMissing { .. }]
    ));
    play(&mock, "speakers").unwrap();
    assert_eq!(app.params().notes.load(Ordering::Relaxed), 1);

    // None of it touched the audio.
    assert_eq!(report.swap.map(|swap| swap.kind), Some(SwapKind::Start));
    assert_eq!(prepared(&app), [48_000.0]);
    assert_eq!(play(&mock, "speakers").map(|(_, blocks)| blocks), Some(2.0));

    // No port at all.
    let report = app.apply_settings(app.settings().clone().with_midi_inputs(MidiInputs::NONE));
    assert!(report.midi_inputs.is_empty());
    assert!(!mock.send_midi("pads", &[0x90, 60, 100]));
}

#[test]
fn every_port_includes_one_plugged_in_later() {
    let mock = studio();
    let mut app = start::<Counter>(&mock, StandaloneOptions::default());
    mock.add_midi_port(Direction::Input, "drums", "Drums");
    let report = app.poll_devices().expect("a port appeared").clone();
    assert_eq!(report.midi_inputs.len(), 3);
    assert!(mock.send_midi("drums", &[0x99, 36, 100]));
    mock.remove_midi_port(Direction::Input, "drums");
    let report = app.poll_devices().expect("a port went").clone();
    assert_eq!(report.midi_inputs.len(), 2);
    assert!(report.fallbacks.is_empty(), "{:?}", report.fallbacks);
    assert_eq!(app.settings().midi_inputs, MidiInputs::All);

    // A list does not grow by itself.
    app.set_midi_input("keys", false);
    mock.add_midi_port(Direction::Input, "drums", "Drums");
    let _ = app.poll_devices();
    assert_eq!(app.report().midi_inputs, [summary("pads", "Pads")]);
}

#[test]
fn saved_settings_open_the_same_devices_in_the_next_run() {
    let mock = studio();
    let saved = {
        let mut app = start::<Counter>(&mock, StandaloneOptions::default());
        let settings = app
            .settings()
            .clone()
            .with_output(DeviceChoice::Device(DeviceRef::new("usb", "USB Interface")))
            .with_sample_rate(88_200)
            .with_buffer_size(512)
            .with_midi_inputs(MidiInputs::ids(["pads"]));
        assert!(app.apply_settings(settings).is_exact());
        app.settings().to_string()
    };
    assert!(!mock.is_running(), "stopped when dropped");

    let settings: StandaloneSettings = saved.parse().unwrap();
    let app = start::<Counter>(&mock, with_settings(settings.clone()));
    assert_eq!(app.settings(), &settings);
    let report: &SettingsReport = app.report();
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.output, Some(summary("usb", "USB Interface")));
    assert_eq!(report.device_sample_rate, Some(88_200));
    assert_eq!(report.buffer_size, Some(512));
    assert_eq!(report.midi_inputs, [summary("pads", "Pads")]);
    assert_eq!(prepared(&app), [88_200.0]);
}

#[test]
fn the_choices_list_each_device_with_what_it_accepts() {
    let mock = studio();
    let mut narrow = MockBackend::device("narrow", "Narrow", 2, 44_100);
    for config in &mut narrow.configs {
        config.min_sample_rate = 44_100;
        config.max_sample_rate = 48_000;
        config.buffer_size = Some((128, 512));
    }
    mock.add_device(Direction::Output, narrow);
    let app = start::<Counter>(&mock, StandaloneOptions::default());
    let choices = app.choices().unwrap();
    assert_eq!(choices.host, "Mock");
    assert_eq!(choices.outputs.len(), 4);
    assert!(choices.inputs.is_empty(), "an instrument takes no input");
    let narrow = choices.device(Direction::Output, "narrow").unwrap();
    assert_eq!(narrow.sample_rates, [44_100, 48_000]);
    assert_eq!(narrow.buffer_sizes, [128, 256, 512]);
    assert!(!narrow.is_default);
    assert!(
        choices
            .device(Direction::Output, "speakers")
            .unwrap()
            .is_default
    );

    drop(app);
    let effect = start::<Half>(&mock, StandaloneOptions::default());
    let choices = effect.choices().unwrap();
    assert_eq!(choices.inputs.len(), 1);
}

#[test]
fn an_effect_records_from_its_input_until_it_is_turned_off() {
    let mock = studio();
    let mut app = start::<Half>(&mock, StandaloneOptions::default());
    assert_eq!(app.report().input, Some(summary("mic", "Microphone")));
    assert_eq!(app.input_name(), Some("Microphone"));
    assert!(
        app.report()
            .to_string()
            .contains("recording from \"Microphone\"")
    );

    let report = app.set_input(DeviceChoice::None).clone();
    assert_eq!(report.input, None);
    assert!(report.is_exact(), "{report}");
    assert_eq!(app.settings().input, DeviceChoice::None);
    let report = app.set_input_device("mic").clone();
    assert_eq!(report.input, Some(summary("mic", "Microphone")));
}

#[test]
fn a_change_that_fails_is_undone_and_reported() {
    let mock = studio();
    let mut app = start::<Counter>(&mock, StandaloneOptions::default());
    let earlier = app.settings().clone();
    mock.fail_listing(true);
    let report = app.set_sample_rate(Some(96_000)).clone();
    assert!(report.error.is_some(), "{report}");
    assert!(
        report
            .to_string()
            .contains("The change failed and was undone")
    );
    assert_eq!(app.settings(), &earlier);

    // Once the devices can be listed again, the settings open as before.
    mock.fail_listing(false);
    let report = app.apply_settings(earlier).clone();
    assert!(report.is_exact(), "{report}");
    assert_eq!(report.device_sample_rate, Some(48_000));
}

#[test]
fn a_standalone_can_move_to_another_thread() {
    fn send<T: Send>() {}
    send::<Standalone<Counter>>();
}
