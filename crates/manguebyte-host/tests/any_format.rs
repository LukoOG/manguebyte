//! Lists, loads, inspects, restores and runs Mangue Pluck through the one
//! interface over both formats, as CLAP and as VST 3, from the same built
//! library, which exports both.
//!
//! The plugin is the same whichever format carries it, so through either
//! it has to sound the same, bit for bit, and the same as the plugin with
//! no host at all.
//!
//! Not under Miri, which cannot load a shared library.
#![cfg(all(feature = "clap", feature = "vst3", not(miri)))]

mod common;

use mangue_pluck::Pluck;
use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::Processor;
use manguebyte_dsp::event::{EventKind, Expression, NoteRef};
use manguebyte_host::any::{Error, Format, Kind, Library, Notice, Plugin};
use manguebyte_host::clap::ClapError;
use manguebyte_host::vst3::Vst3Error;
use manguebyte_midi::MidiMessage;
use manguebyte_params::ParamSet;
use manguebyte_plugin::{Harness, Plugin as _};

use common::{assert_same_bits, render};

const RATE: f64 = 48_000.0;
const FRAMES: usize = 24_000;

/// Loads the built library as `format`.
fn library(format: Format) -> Library {
    let path = common::built_library("mangue_pluck");
    // SAFETY: the example plugin built from this workspace, whose entry
    // points are the framework's own CLAP and VST 3 wrappers.
    unsafe { Library::load_as(path, format) }.expect("Mangue Pluck loads")
}

/// The plugin's parameter called `name`, in the ids of its format.
fn param(plugin: &Plugin, name: &str) -> ParamId {
    plugin
        .params()
        .into_iter()
        .find(|param| param.name == name)
        .unwrap_or_else(|| panic!("no parameter {name}"))
        .id
}

/// Notes, expression, the pitch wheel and automation, each on a frame of
/// its own. Every parameter of this plugin is continuous, which both its
/// wrappers move as a position between 0 and 1, so a value means the same
/// in either format. Expression values convert exactly in both.
fn song(plugin: &Plugin) -> Vec<(u64, EventKind)> {
    let low = NoteRef::with_id(0, 45, 1);
    let fifth = NoteRef::with_id(0, 52, 2);
    let value = |name, value| EventKind::ParamValue {
        param: param(plugin, name),
        value,
    };
    vec![
        (
            100,
            EventKind::NoteOn {
                note: low,
                velocity: 0.8,
            },
        ),
        (2_500, value("Resonance", 0.6)),
        (
            3_000,
            EventKind::NoteExpression {
                note: low,
                expression: Expression::Tuning,
                value: 0.468_75,
            },
        ),
        (
            4_000,
            EventKind::NoteOn {
                note: fifth,
                velocity: 0.7,
            },
        ),
        (
            4_500,
            EventKind::NoteExpression {
                note: fifth,
                expression: Expression::Pressure,
                value: 0.75,
            },
        ),
        (
            5_500,
            EventKind::Midi(MidiMessage::PitchBend {
                channel: 0,
                value: 2_000,
            }),
        ),
        (7_777, value("Decay", 0.8)),
        (
            9_100,
            EventKind::NoteOff {
                note: low,
                velocity: 0.3,
            },
        ),
        (10_000, value("Reverb", 0.6)),
        (
            13_000,
            EventKind::NoteOff {
                note: fifth,
                velocity: 0.0,
            },
        ),
    ]
}

/// The song through one format, as the boxed processor the interface
/// hands out.
fn play(format: Format) -> (AudioBuffer, usize) {
    let library = library(format);
    let mut plugin = library.create(&library.plugins()[0].id).unwrap();
    let schedule = song(&plugin);
    let mut processor: Box<dyn Processor<f32> + Send> = plugin.activate(RATE, 256).unwrap();
    let mut audio = AudioBuffer::<f32>::new(2, FRAMES);
    let rendered = render(
        &mut processor,
        RATE,
        &schedule,
        &[256],
        256,
        &mut audio,
        None,
    );
    drop(processor);
    plugin.deactivate().unwrap();
    assert_eq!(plugin.failed_blocks(), 0);
    assert_eq!(plugin.lost_events(), 0);
    assert_eq!(plugin.undeliverable_events(), 0, "{format}");
    (audio, rendered.heap_calls)
}

#[test]
fn one_library_lists_the_same_plugin_in_both_formats() {
    let path = common::built_library("mangue_pluck");
    // The file exports both entry points; by its name alone it is taken as
    // CLAP.
    // SAFETY: the example plugin built from this workspace.
    let chosen = unsafe { Library::load(&path) }.unwrap();
    assert_eq!(chosen.format(), Format::Clap);
    assert_eq!(chosen.path(), path);

    for (format, id) in [
        (Format::Clap, "org.manguebyte.pluck"),
        (Format::Vst3, "E3AC906E0FE4AF333672CEE11FA34823"),
    ] {
        let library = library(format);
        assert_eq!(library.format(), format);
        let [entry] = library.plugins() else {
            panic!(
                "{format}: expected one plugin, found {:?}",
                library.plugins()
            );
        };
        assert_eq!(entry.format, format);
        assert_eq!(entry.id, id);
        assert_eq!(entry.name, Pluck::INFO.name);
        assert_eq!(entry.vendor, Pluck::INFO.vendor);
        assert_eq!(entry.version, Pluck::INFO.version);
        assert_eq!(entry.kind, Kind::Instrument);
        assert!(!entry.categories.is_empty());
        assert_eq!(library.as_clap().is_some(), format == Format::Clap);
        assert_eq!(library.as_vst3().is_some(), format == Format::Vst3);

        let plugin = library.create(id).unwrap();
        assert_eq!(plugin.format(), format);
        assert_eq!(plugin.entry(), entry);
        assert!(plugin.takes_notes());
        assert!(plugin.audio_inputs().is_empty());
        let outputs = plugin.audio_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!((outputs[0].channels, outputs[0].is_main), (2, true));
        assert_eq!(plugin.latency(), 0);
        assert!(!plugin.is_active() && !plugin.has_processor());

        assert!(matches!(
            library.create("org.example.nothing"),
            Err(Error::UnknownPlugin(_))
        ));
    }
}

/// A library reports the path its handle was loaded from, in either
/// format, also when the same file was loaded before through another
/// spelling of its path and the two handles share one loaded library.
#[test]
fn a_library_reports_the_path_it_was_given_in_either_format() {
    let built = common::built_library("mangue_pluck");
    // `Path` comparison drops `.` but keeps `..`, so this is another path
    // to the same file.
    let folder = built.parent().unwrap();
    let respelled = folder
        .join("..")
        .join(folder.file_name().unwrap())
        .join(built.file_name().unwrap());
    assert_ne!(respelled, built);
    for format in [Format::Clap, Format::Vst3] {
        // SAFETY: the example plugin built from this workspace, whose entry
        // points are the framework's own CLAP and VST 3 wrappers.
        let (first, second) = unsafe {
            (
                Library::load_as(&built, format).unwrap(),
                Library::load_as(&respelled, format).unwrap(),
            )
        };
        assert_eq!(first.path(), built.as_path(), "{format}");
        assert_eq!(second.path(), respelled.as_path(), "{format}");
    }
}

/// A VST 3 class identifier is a number written in hexadecimal, and either
/// case names the same plugin. A CLAP identifier is text, matched exactly.
#[test]
fn a_vst3_identifier_may_be_written_in_either_case() {
    let vst3 = library(Format::Vst3);
    let id = vst3.plugins()[0].id.clone();
    assert_eq!(id, id.to_uppercase(), "entries are written in upper case");
    let mixed: String = id
        .chars()
        .enumerate()
        .map(|(at, digit)| {
            if at % 2 == 0 {
                digit.to_ascii_lowercase()
            } else {
                digit
            }
        })
        .collect();
    for written in [id.to_lowercase(), mixed] {
        let plugin = vst3.create(&written).unwrap();
        assert_eq!(plugin.entry().id, id, "{written}");
    }
    assert!(matches!(
        vst3.create("E3AC906E0FE4AF333672CEE11FA3482"),
        Err(Error::UnknownPlugin(_))
    ));
    assert!(matches!(
        vst3.create("00000000000000000000000000000000"),
        Err(Error::UnknownPlugin(_))
    ));

    let clap = library(Format::Clap);
    assert!(matches!(
        clap.create("ORG.MANGUEBYTE.PLUCK"),
        Err(Error::UnknownPlugin(_))
    ));
}

#[test]
fn parameters_read_the_same_through_both_formats() {
    let clap = library(Format::Clap);
    let vst3 = library(Format::Vst3);
    let clap = clap.create(&clap.plugins()[0].id).unwrap();
    let vst3 = vst3.create(&vst3.plugins()[0].id).unwrap();
    let declared = Pluck::new().params();

    let mut checked = 0;
    declared.visit(&mut |declared| {
        let name = declared.name();
        let ours = clap.params().into_iter().find(|p| p.name == name).unwrap();
        let theirs = vst3.params().into_iter().find(|p| p.name == name).unwrap();
        // Both scales run from 0 to 1 for a continuous parameter, and the
        // default is the same position.
        assert_eq!((ours.min, ours.max), (0.0, 1.0));
        assert_eq!((theirs.min, theirs.max), (0.0, 1.0));
        assert_eq!(ours.default, theirs.default);
        assert_eq!(ours.steps, None);
        assert_eq!(theirs.steps, None);
        assert_eq!(ours.automatable, theirs.automatable);
        assert_eq!(clap.param_value(ours.id), vst3.param_value(theirs.id));
        // VST 3 keeps the unit apart from the text; CLAP's text ends in it.
        let unit = &theirs.unit;
        let text = vst3.param_to_text(theirs.id, 0.5).unwrap();
        let with_unit = clap.param_to_text(ours.id, 0.5).unwrap();
        assert_eq!(with_unit.trim_end_matches(unit.as_str()).trim_end(), text);
        assert_eq!(
            clap.text_to_param(ours.id, &with_unit),
            vst3.text_to_param(theirs.id, &text)
        );
        checked += 1;
    });
    assert_eq!(checked, 6);
}

#[test]
fn the_plugin_sounds_the_same_through_either_format_and_alone() {
    let (through_clap, clap_heap) = play(Format::Clap);
    let (through_vst3, vst3_heap) = play(Format::Vst3);
    assert_eq!((clap_heap, vst3_heap), (0, 0), "a host used the heap");
    assert!(through_clap.peak() > 0.05);
    assert_same_bits(&through_vst3, &through_clap, "VST 3 against CLAP");

    // And as the plugin plays alone, with values in its own unit.
    let mut harness = Harness::<Pluck>::new(RATE, 256);
    let fresh = Pluck::new().params();
    let clap = library(Format::Clap);
    let plugin = clap.create(&clap.plugins()[0].id).unwrap();
    for (frame, kind) in song(&plugin) {
        harness.schedule(frame, common::for_harness(&*fresh, kind));
    }
    assert_same_bits(
        &through_clap,
        &harness.render(FRAMES),
        "CLAP against the plugin alone",
    );
}

#[test]
fn state_is_restored_through_the_interface_and_kept_to_its_format() {
    let mut saved = Vec::new();
    for format in [Format::Clap, Format::Vst3] {
        let library = library(format);
        let id = &library.plugins()[0].id;
        let mut plugin = library.create(id).unwrap();
        let level = param(&plugin, "Level");
        plugin.set_param(level, 0.25).unwrap();
        let state = plugin.save_state().unwrap();

        let mut other = library.create(id).unwrap();
        assert_ne!(other.param_value(level), plugin.param_value(level));
        other.load_state(&state).unwrap();
        // The plugin keeps a value as a 32-bit float in its own unit; both
        // formats show what it keeps.
        let shown = other.param_value(level).unwrap();
        assert!((shown - 0.25).abs() < 1e-6, "{format}: {shown}");
        for notice in other.pump() {
            assert!(
                matches!(notice, Notice::ParamsChanged),
                "{format}: {notice:?}"
            );
        }
        saved.push((format, state));
    }

    // Each format's state belongs to that format.
    let clap = library(Format::Clap);
    let mut clap = clap.create(&clap.plugins()[0].id).unwrap();
    assert_eq!(
        clap.load_state(&saved[1].1),
        Err(Error::Clap(ClapError::StateLoadFailed))
    );
    let vst3 = library(Format::Vst3);
    let mut vst3 = vst3.create(&vst3.plugins()[0].id).unwrap();
    assert!(matches!(
        vst3.load_state(&saved[0].1),
        Err(Error::Vst3(Vst3Error::NotAState(_)))
    ));
}

#[test]
fn a_boxed_audio_half_runs_on_another_thread() {
    for format in [Format::Clap, Format::Vst3] {
        let library = library(format);
        let mut plugin = library.create(&library.plugins()[0].id).unwrap();
        let schedule = song(&plugin);
        let mut processor = plugin.activate(RATE, 256).unwrap();
        let audio = std::thread::spawn(move || {
            let mut audio = AudioBuffer::<f32>::new(2, FRAMES);
            render(
                &mut processor,
                RATE,
                &schedule,
                &[256],
                256,
                &mut audio,
                None,
            );
            audio
        })
        .join()
        .unwrap();
        assert!(audio.peak() > 0.05, "{format}");
        assert!(!plugin.has_processor());
        plugin.deactivate().unwrap();
        assert!(plugin.as_clap().is_some() == (format == Format::Clap));
        assert!(plugin.as_vst3().is_some() == (format == Format::Vst3));
    }
}

#[test]
fn what_is_not_a_plugin_is_an_error_in_either_format() {
    let folder = common::scratch_folder("any-not-plugins");
    let text = folder.join("notes.txt");
    std::fs::write(&text, "not a library").unwrap();
    // SAFETY: the system refuses the file before any of it is run.
    let outcome = unsafe { Library::load(&text) };
    assert!(matches!(outcome, Err(Error::Clap(ClapError::Load { .. }))));
    for (name, format) in [
        ("missing.clap", Format::Clap),
        ("missing.vst3", Format::Vst3),
    ] {
        // SAFETY: there is nothing at the path.
        let outcome = unsafe { Library::load(folder.join(name)) };
        match (format, outcome) {
            (Format::Clap, Err(Error::Clap(ClapError::NotFound(_)))) => {}
            (Format::Vst3, Err(Error::Vst3(Vst3Error::NotFound(_)))) => {}
            (format, other) => panic!("{format}: {other:?}"),
        }
    }
    // A library of the system, which is a plugin of neither format.
    if cfg!(windows) {
        let system = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap())
            .join("System32/kernel32.dll");
        // SAFETY: a library of the system, sound to load into any process,
        // which exports neither entry point.
        let outcome = unsafe { Library::load(&system) };
        assert_eq!(outcome.unwrap_err(), Error::UnknownFormat(system));
    }
    let _ = std::fs::remove_dir_all(&folder);
}
