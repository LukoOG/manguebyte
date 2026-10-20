//! The parameter link: user edits are undoable, host automation is not, and
//! a plugin's saved state carries its document.

use std::sync::{Arc, Weak};

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::{ProcessContext, ProcessSpec};
use manguebyte_params::{BoolParam, FloatParam, FloatRange, Param, param_set};
use manguebyte_plugin::{AudioLayout, Plugin, PluginInfo, PluginKind, load_state, save_state};
use manguebyte_state::{
    ChangeKind, Changes, Document, EditError, NodeId, Origin, SharedDocument, Value,
};

struct SamplerParams {
    cutoff: FloatParam,
    mono: BoolParam,
    doc: SharedDocument,
}
param_set!(SamplerParams { cutoff, mono, extra doc });

/// A plugin whose editor keeps a document: zones beside the parameters.
struct Sampler {
    params: Arc<SamplerParams>,
}

impl Plugin for Sampler {
    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.test.sampler",
        name: "Sampler",
        vendor: "Manguebyte tests",
        version: "0.1.0",
        description: "Keeps a document with its state",
        kind: PluginKind::Instrument,
    };
    const LAYOUT: AudioLayout = AudioLayout::STEREO_INSTRUMENT;
    type Params = SamplerParams;

    fn new() -> Self {
        let params = Arc::new(SamplerParams {
            cutoff: FloatParam::new(
                "cutoff",
                "Cutoff",
                FloatRange::logarithmic(20.0, 20_000.0),
                1_000.0,
            ),
            mono: BoolParam::new("mono", "Mono", false),
            doc: SharedDocument::new(Document::new("instrument")),
        });
        {
            let mut doc = params.doc.lock();
            let mirror = doc
                .edit("Set up", |tx| tx.add_child(tx.root(), "params"))
                .unwrap();
            doc.link_params(&params, mirror).unwrap();
            doc.clear_history();
        }
        Self { params }
    }

    fn params(&self) -> Arc<SamplerParams> {
        Arc::clone(&self.params)
    }

    fn prepare(&mut self, _: &ProcessSpec) {}

    fn process(&mut self, _: &mut AudioBuffer<f32>, _: &ProcessContext<'_, f32>) {}
}

fn mirror(doc: &Document) -> NodeId {
    doc.param_mirror().expect("linked")
}

#[test]
fn the_mirror_follows_host_automation_without_touching_history() {
    let params = Sampler::new().params();
    let mut doc = params.doc.lock();
    let node = mirror(&doc);
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(1_000.0)));
    assert_eq!(doc.get(node, "mono"), Some(&Value::Float(0.0)));
    assert!(!doc.can_undo());

    let mut reader = doc.cursor();
    // The host automates: it writes the atomics, as a wrapper does.
    params.cutoff.set_plain(2_000.0);
    params.mono.set_plain(1.0);
    assert_eq!(doc.sync_params(), 2);
    assert_eq!(doc.sync_params(), 0, "nothing new");
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(2_000.0)));
    assert!(!doc.can_undo(), "automation is not a step of history");

    let Changes::Since(changes) = doc.changes(&mut reader) else {
        panic!("nothing missed");
    };
    let seen: Vec<_> = changes
        .map(|change| (change.kind.clone(), change.origin))
        .collect();
    assert_eq!(
        seen,
        [
            (
                ChangeKind::Property {
                    name: "cutoff".into()
                },
                Origin::Params
            ),
            (
                ChangeKind::Property {
                    name: "mono".into()
                },
                Origin::Params
            ),
        ]
    );
}

#[test]
fn user_edits_to_parameters_are_undoable_steps() {
    let params = Sampler::new().params();
    let mut doc = params.doc.lock();
    let node = mirror(&doc);

    doc.edit("Change cutoff", |tx| {
        tx.set_param("cutoff", 440.0).map(drop)
    })
    .unwrap();
    assert_eq!(params.cutoff.value(), 440.0);
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(440.0)));

    // A knob drag on the mirror property is one step, through the param.
    doc.begin_gesture("Drag cutoff");
    for hz in [500.0, 800.0, 30_000.0] {
        doc.edit("drag", |tx| tx.set(node, "cutoff", hz)).unwrap();
    }
    doc.end_gesture();
    assert_eq!(params.cutoff.value(), 20_000.0, "clamped by the parameter");
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(20_000.0)));
    doc.edit("Mono", |tx| tx.set(node, "mono", true)).unwrap();
    assert!(params.mono.value());
    assert_eq!(
        doc.undo_names().collect::<Vec<_>>(),
        ["Mono", "Drag cutoff", "Change cutoff"]
    );

    doc.undo();
    assert!(!params.mono.value());
    doc.undo();
    assert_eq!(params.cutoff.value(), 440.0);
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(440.0)));

    // Automation in between: syncing records nothing, and undo puts back
    // what the user had before their edit.
    params.cutoff.set_plain(3_000.0);
    doc.sync_params();
    assert_eq!(doc.undo_names().count(), 1);
    doc.undo();
    assert_eq!(params.cutoff.value(), 1_000.0);
    doc.redo();
    assert_eq!(params.cutoff.value(), 440.0);
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(440.0)));
}

#[test]
fn the_mirror_cannot_be_broken_by_edits() {
    let params = Sampler::new().params();
    let mut doc = params.doc.lock();
    let node = mirror(&doc);
    let before = doc.to_text();
    assert_eq!(
        doc.edit("x", |tx| tx.remove_property(node, "cutoff").map(drop)),
        Err(EditError::Mirror)
    );
    assert_eq!(
        doc.edit("x", |tx| tx.set(node, "cutoff", "loud")),
        Err(EditError::Mirror)
    );
    assert_eq!(
        doc.edit("x", |tx| tx.remove_node(node)),
        Err(EditError::Mirror)
    );
    assert_eq!(
        doc.edit("x", |tx| tx.set_param("nope", 1.0).map(drop)),
        Err(EditError::NoSuchParam(String::from("nope")))
    );
    assert_eq!(
        doc.edit("x", |tx| tx.set_param("cutoff", f64::INFINITY).map(drop)),
        Err(EditError::NotFinite)
    );
    // A property of the mirror that is not a parameter is an ordinary one.
    doc.edit("Note", |tx| tx.set(node, "comment", "warm"))
        .unwrap();
    doc.undo();
    assert_eq!(doc.to_text(), before);
    // Moving the mirror is fine.
    doc.edit("Move", |tx| {
        let group = tx.add_child(tx.root(), "group")?;
        tx.move_node(node, group, 0)
    })
    .unwrap();
    assert_eq!(doc.sync_params(), 0);
}

#[test]
fn a_failed_edit_takes_parameter_changes_back_too() {
    let params = Sampler::new().params();
    let mut doc = params.doc.lock();
    let result = doc.edit("Fail", |tx| {
        tx.set_param("cutoff", 5_000.0)?;
        tx.remove_node(tx.root())
    });
    assert_eq!(result, Err(EditError::Root));
    assert_eq!(params.cutoff.value(), 1_000.0);
    assert_eq!(
        doc.get(mirror(&doc), "cutoff"),
        Some(&Value::Float(1_000.0))
    );
}

#[test]
fn plugin_state_carries_the_document() {
    let plugin = Sampler::new();
    let params = plugin.params();
    let zone = {
        let mut doc = params.doc.lock();
        let zone = doc
            .edit("Add zone", |tx| {
                let zone = tx.add_child(tx.root(), "zone")?;
                tx.set(zone, "sample", "kick.wav")?;
                tx.set(zone, "key", 36)?;
                tx.set_param("cutoff", 750.0)?;
                Ok(zone)
            })
            .unwrap();
        assert!(doc.can_undo());
        zone
    };
    let saved = save_state::<Sampler>(&params);
    assert!(saved.starts_with("manguebyte-plugin 2 org.manguebyte.test.sampler\n"));
    assert!(saved.contains("\ncutoff=750\n"));
    assert!(saved.contains("\nsection document\n| manguebyte-state 1\n"));

    let fresh = Sampler::new().params();
    let watch = fresh.doc.watch();
    let version = watch.version();
    let report = load_state::<Sampler>(&fresh, &saved).unwrap();
    assert!(report.is_exact());
    assert_eq!(fresh.cutoff.value(), 750.0);
    assert!(watch.version() > version, "a watcher sees the restore");
    let doc = fresh.doc.lock();
    assert_eq!(doc.to_text(), params.doc.lock().to_text());
    assert_eq!(doc.get(zone, "key"), Some(&Value::Int(36)), "ids survive");
    assert!(
        !doc.can_undo(),
        "a host's restore is not the editor's to undo"
    );
    assert_eq!(doc.param_mirror(), params.doc.lock().param_mirror());
}

#[test]
fn saved_state_holds_automation_the_mirror_never_saw() {
    // The editor is closed, so nothing syncs the mirror while the host
    // automates; then the host saves.
    let params = Sampler::new().params();
    params.cutoff.set_plain(5_000.0);
    let saved = save_state::<Sampler>(&params);
    assert!(saved.contains("\ncutoff=5000\n"));
    assert!(saved.contains(" cutoff = float 5000\n"), "{saved}");

    // State whose document disagrees with its values, as a hand edit or a
    // race could make: the values win.
    let skewed = saved.replace("cutoff = float 5000", "cutoff = float 50");
    let fresh = Sampler::new().params();
    load_state::<Sampler>(&fresh, &skewed).unwrap();
    assert_eq!(fresh.cutoff.value(), 5_000.0);
    let doc = fresh.doc.lock();
    assert_eq!(
        doc.get(mirror(&doc), "cutoff"),
        Some(&Value::Float(5_000.0))
    );
}

#[test]
fn a_replaced_document_wins_over_the_live_parameters() {
    let params = Sampler::new().params();
    let mut doc = params.doc.lock();
    let mut file = Document::from_text(&doc.to_text()).unwrap().0;
    let node = mirror(&doc);
    // The file was saved with other values: edit them in, unlinked.
    file.edit("Old values", |tx| {
        tx.set(node, "cutoff", 99_999.0)?;
        tx.set(node, "mono", 1.0)
    })
    .unwrap();
    doc.replace(file);
    assert_eq!(params.cutoff.value(), 20_000.0, "clamped on the way in");
    assert!(params.mono.value());
    assert_eq!(doc.get(node, "cutoff"), Some(&Value::Float(20_000.0)));
    assert_eq!(doc.param_mirror(), Some(node));

    // A document without the mirror node drops the link.
    doc.replace(Document::new("instrument"));
    assert_eq!(doc.param_mirror(), None);
    assert_eq!(doc.sync_params(), 0);
}

#[test]
fn the_link_does_not_keep_the_parameters_alive() {
    let params = Sampler::new().params();
    let weak: Weak<SamplerParams> = Arc::downgrade(&params);
    let shared = params.doc.clone();
    drop(params);
    assert!(weak.upgrade().is_none(), "no cycle through the document");
    let mut doc = shared.lock();
    assert_eq!(
        doc.edit("x", |tx| tx.set_param("cutoff", 1.0).map(drop)),
        Err(EditError::NoParamLink)
    );
    assert_eq!(doc.sync_params(), 0);
    assert_eq!(doc.len(), 2);
}
