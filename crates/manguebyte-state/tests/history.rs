//! Undo, redo, transactions, gestures, merging, limits and the journal.

use manguebyte_state::{
    ChangeKind, Changes, Document, EditError, HistoryLimits, NodeId, NodeTree, Origin, Value,
};

fn tree(doc: &Document) -> String {
    format!("{:?}", doc.subtree(doc.root()).unwrap())
}

fn names(doc: &Document) -> (Vec<&str>, Vec<&str>) {
    (doc.undo_names().collect(), doc.redo_names().collect())
}

#[test]
fn undo_and_redo_walk_the_history_both_ways() {
    let mut doc = Document::new("song");
    let empty = tree(&doc);
    let part = doc
        .edit("Add part", |tx| {
            let part = tx.add_child(tx.root(), "part")?;
            tx.set(part, "name", "Bass")?;
            Ok(part)
        })
        .unwrap();
    let with_part = tree(&doc);
    doc.edit("Rename part", |tx| tx.set(part, "name", "Baixo"))
        .unwrap();
    let renamed = tree(&doc);
    doc.edit("Remove part", |tx| tx.remove_node(part)).unwrap();
    assert!(!doc.contains(part));

    assert_eq!(
        names(&doc),
        (vec!["Remove part", "Rename part", "Add part"], vec![])
    );
    assert!(doc.undo());
    assert_eq!(tree(&doc), renamed, "the very same node, id included");
    assert!(doc.undo());
    assert_eq!(tree(&doc), with_part);
    assert!(doc.undo());
    assert_eq!(tree(&doc), empty);
    assert!(!doc.undo());
    assert!(!doc.can_undo());
    assert_eq!(doc.redo_name(), Some("Add part"));

    assert!(doc.redo());
    assert!(doc.redo());
    assert_eq!(tree(&doc), renamed);
    assert_eq!(doc.get(part, "name"), Some(&Value::from("Baixo")));
    assert!(doc.redo());
    assert!(!doc.redo());
    assert_eq!(doc.len(), 1);
}

#[test]
fn a_transaction_is_one_step_and_a_failed_one_is_taken_back() {
    let mut doc = Document::new("kit");
    let before = tree(&doc);
    let result = doc.edit("Add zones", |tx| {
        for key in 36..40 {
            let zone = tx.add_child(tx.root(), "zone")?;
            tx.set(zone, "key", key)?;
        }
        let first = tx.doc().children(tx.root())[0];
        tx.move_node(first, tx.root(), 3)?;
        tx.set(tx.root(), "bad name", 1)
    });
    assert_eq!(
        result,
        Err(EditError::InvalidName(String::from("bad name")))
    );
    assert_eq!(tree(&doc), before, "everything the edit did is taken back");
    assert!(!doc.can_undo());

    doc.edit("Add zones", |tx| {
        for key in 36..40 {
            let zone = tx.add_child(tx.root(), "zone")?;
            tx.set(zone, "key", key)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(doc.len(), 5);
    assert_eq!(doc.undo_names().count(), 1);
    doc.undo();
    assert_eq!(tree(&doc), before);
}

#[test]
fn an_edit_that_changes_nothing_makes_no_step_and_keeps_redo() {
    let mut doc = Document::new("kit");
    doc.edit("Set", |tx| tx.set(tx.root(), "gain", 0.5))
        .unwrap();
    doc.edit("Set", |tx| tx.set(tx.root(), "gain", 0.7))
        .unwrap();
    doc.undo();
    doc.edit("Same", |tx| tx.set(tx.root(), "gain", 0.5))
        .unwrap();
    doc.edit("Back and forth", |tx| {
        tx.set(tx.root(), "gain", 0.9)?;
        tx.set(tx.root(), "gain", 0.5)
    })
    .unwrap();
    doc.edit("Nothing", |_| Ok(())).unwrap();
    assert_eq!(names(&doc), (vec!["Set"], vec!["Set"]));
    // `-0.0` is not `0.0`: saved state tells them apart, so history does.
    doc.edit("Zero", |tx| tx.set(tx.root(), "z", 0.0)).unwrap();
    doc.edit("Minus zero", |tx| tx.set(tx.root(), "z", -0.0))
        .unwrap();
    assert_eq!(doc.undo_name(), Some("Minus zero"));
}

#[test]
fn a_new_step_discards_the_redo_branch() {
    let mut doc = Document::new("kit");
    for value in 1..=4 {
        doc.edit(&format!("Set {value}"), |tx| tx.set(tx.root(), "v", value))
            .unwrap();
    }
    doc.undo();
    doc.undo();
    assert_eq!(
        names(&doc),
        (vec!["Set 2", "Set 1"], vec!["Set 3", "Set 4"])
    );
    let bytes_before = doc.history_bytes();

    doc.edit("Branch", |tx| tx.set(tx.root(), "v", 10)).unwrap();
    assert_eq!(names(&doc), (vec!["Branch", "Set 2", "Set 1"], vec![]));
    assert!(!doc.redo());
    assert!(doc.history_bytes() < bytes_before, "the branch is freed");
    doc.undo();
    assert_eq!(doc.get(doc.root(), "v"), Some(&Value::Int(2)));
}

#[test]
fn a_gesture_is_one_step_however_long() {
    let mut doc = Document::new("synth");
    let filter = doc
        .edit("Add filter", |tx| tx.add_child(tx.root(), "filter"))
        .unwrap();
    doc.edit("Set cutoff", |tx| tx.set(filter, "cutoff", 1_000.0))
        .unwrap();
    let after_one = doc.history_bytes();

    doc.begin_gesture("Change filter cutoff");
    assert!(doc.is_gesture_open());
    for frame in 0..1_000 {
        doc.edit("frame", |tx| {
            tx.set(filter, "cutoff", 1_000.0 + f64::from(frame))?;
            tx.set(filter, "resonance", f64::from(frame) / 1_000.0)
        })
        .unwrap();
    }
    doc.end_gesture();
    assert_eq!(doc.undo_name(), Some("Change filter cutoff"));
    assert_eq!(doc.undo_names().count(), 3);
    // Two properties' worth of steps, not two thousand.
    assert!(
        doc.history_bytes() - after_one < 1_024,
        "{}",
        doc.history_bytes()
    );

    doc.undo();
    assert_eq!(doc.get(filter, "cutoff"), Some(&Value::Float(1_000.0)));
    assert_eq!(doc.get(filter, "resonance"), None);
    doc.redo();
    assert_eq!(doc.get(filter, "cutoff"), Some(&Value::Float(1_999.0)));
}

#[test]
fn a_gesture_back_where_it_started_leaves_no_step() {
    let mut doc = Document::new("synth");
    doc.edit("Set", |tx| tx.set(tx.root(), "pan", 0.0)).unwrap();
    doc.begin_gesture("Change pan");
    for value in [0.1, 0.3, -0.2, 0.0] {
        doc.edit("drag", |tx| tx.set(tx.root(), "pan", value))
            .unwrap();
    }
    doc.end_gesture();
    assert_eq!(names(&doc), (vec!["Set"], vec![]));

    // Opening a gesture ends the one before, and plain edits join it.
    doc.begin_gesture("First");
    doc.edit("a", |tx| tx.set(tx.root(), "a", 1)).unwrap();
    doc.begin_gesture("Second");
    doc.edit("b", |tx| tx.set(tx.root(), "b", 1)).unwrap();
    doc.edit("c", |tx| tx.add_child(tx.root(), "c")).unwrap();
    doc.end_gesture();
    assert_eq!(names(&doc).0, ["Second", "First", "Set"]);
}

#[test]
fn edits_with_the_same_key_merge_until_something_else_happens() {
    let mut doc = Document::new("kit");
    let zone = doc
        .edit("Add zone", |tx| tx.add_child(tx.root(), "zone"))
        .unwrap();
    for step in 1..=5 {
        doc.edit_merging("Nudge zone", ("nudge", zone), |tx| {
            tx.set(zone, "key", step)
        })
        .unwrap();
    }
    assert_eq!(names(&doc).0, ["Nudge zone", "Add zone"]);

    // Another key, another step.
    doc.edit_merging("Nudge velocity", ("velocity", zone), |tx| {
        tx.set(zone, "vel", 1)
    })
    .unwrap();
    doc.edit_merging("Nudge zone", ("nudge", zone), |tx| tx.set(zone, "key", 9))
        .unwrap();
    assert_eq!(
        names(&doc).0,
        ["Nudge zone", "Nudge velocity", "Nudge zone", "Add zone"]
    );

    // An undo in between breaks the series too.
    doc.undo();
    doc.edit_merging("Nudge zone", ("nudge", zone), |tx| tx.set(zone, "key", 7))
        .unwrap();
    doc.edit_merging("Nudge zone", ("nudge", zone), |tx| tx.set(zone, "key", 8))
        .unwrap();
    assert_eq!(
        names(&doc).0,
        ["Nudge zone", "Nudge velocity", "Nudge zone", "Add zone"]
    );
    doc.undo();
    assert_eq!(doc.get(zone, "key"), Some(&Value::Int(5)));
}

#[test]
fn history_is_bounded_by_steps_and_by_bytes() {
    let mut doc = Document::new("kit");
    doc.set_history_limits(HistoryLimits {
        max_steps: 3,
        max_bytes: usize::MAX,
    });
    for value in 0..10 {
        doc.edit(&format!("Set {value}"), |tx| tx.set(tx.root(), "v", value))
            .unwrap();
    }
    assert_eq!(names(&doc).0, ["Set 9", "Set 8", "Set 7"]);
    doc.undo();
    doc.undo();
    // Undo and redo together count.
    doc.edit("Branch", |tx| tx.set(tx.root(), "v", 100))
        .unwrap();
    assert_eq!(names(&doc), (vec!["Branch", "Set 7"], vec![]));

    // Bytes: a megabyte per step, two megabytes allowed.
    let mut doc = Document::new("kit");
    doc.set_history_limits(HistoryLimits {
        max_steps: 100,
        max_bytes: 2 << 20,
    });
    for step in 0..5u8 {
        doc.edit(&format!("Sample {step}"), |tx| {
            tx.set(tx.root(), "sample", vec![step; 1 << 20])
        })
        .unwrap();
        assert!(doc.history_bytes() <= 2 << 20 || doc.undo_names().count() == 1);
    }
    assert_eq!(names(&doc).0, ["Sample 4"]);

    // The newest step is kept even when it alone is over the limit.
    doc.edit("Huge", |tx| tx.set(tx.root(), "sample", vec![9u8; 3 << 20]))
        .unwrap();
    assert_eq!(names(&doc).0, ["Huge"]);
    assert!(doc.undo());
    assert_eq!(
        doc.get(doc.root(), "sample").unwrap().as_bytes().unwrap()[0],
        4
    );

    // Lowering the limits trims at once; zero steps keeps no history.
    doc.redo();
    doc.set_history_limits(HistoryLimits {
        max_steps: 0,
        max_bytes: usize::MAX,
    });
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(doc.history_bytes(), 0);
    doc.edit("Lost", |tx| tx.set(tx.root(), "v", 1)).unwrap();
    assert!(!doc.can_undo());
    assert_eq!(doc.get(doc.root(), "v"), Some(&Value::Int(1)));
}

#[test]
fn structure_edits_check_their_arguments() {
    let mut doc = Document::new("root");
    let (a, b) = doc
        .edit("Build", |tx| {
            let a = tx.add_child(tx.root(), "a")?;
            let b = tx.add_child(a, "b")?;
            Ok((a, b))
        })
        .unwrap();
    let before = tree(&doc);
    let mut fails = |edit: &dyn Fn(&mut manguebyte_state::Tx<'_>) -> Result<(), EditError>| {
        doc.edit("Fail", |tx| edit(tx)).unwrap_err()
    };
    assert_eq!(fails(&|tx| tx.remove_node(tx.root())), EditError::Root);
    assert_eq!(fails(&|tx| tx.move_node(a, b, 0)), EditError::IntoItself);
    assert_eq!(fails(&|tx| tx.move_node(a, a, 0)), EditError::IntoItself);
    assert_eq!(
        fails(&|tx| tx.move_node(b, tx.root(), 5)),
        EditError::IndexOutOfRange { index: 5, len: 1 }
    );
    assert_eq!(
        fails(&|tx| tx.insert_child(a, 2, "c").map(drop)),
        EditError::IndexOutOfRange { index: 2, len: 1 }
    );
    assert_eq!(
        fails(&|tx| tx.add_child(a, "has space").map(drop)),
        EditError::InvalidName(String::from("has space"))
    );
    assert_eq!(fails(&|tx| tx.set(a, "f", f64::NAN)), EditError::NotFinite);
    assert_eq!(
        fails(&|tx| tx.set_param("cutoff", 1.0).map(drop)),
        EditError::NoParamLink
    );
    assert_eq!(tree(&doc), before);

    // Too deep.
    let mut doc = Document::new("root");
    let result = doc.edit("Deep", |tx| {
        let mut at = tx.root();
        for _ in 0..=manguebyte_state::MAX_DEPTH {
            at = tx.add_child(at, "n")?;
        }
        Ok(())
    });
    assert_eq!(result, Err(EditError::TooDeep));
    assert_eq!(doc.len(), 1);
}

#[test]
fn moves_and_removals_restore_exact_positions() {
    let mut doc = Document::new("root");
    let ids = doc
        .edit("Build", |tx| {
            (0..5)
                .map(|index| {
                    let child = tx.add_child(tx.root(), "item")?;
                    tx.set(child, "index", index)?;
                    Ok(child)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    let before = tree(&doc);
    doc.edit("Shuffle", |tx| {
        tx.move_node(ids[0], tx.root(), 4)?;
        tx.move_node(ids[3], ids[1], 0)?;
        tx.remove_node(ids[2])?;
        tx.move_node(ids[1], tx.root(), 0)
    })
    .unwrap();
    assert_eq!(doc.children(doc.root()), [ids[1], ids[4], ids[0]]);
    assert_eq!(doc.children(ids[1]), [ids[3]]);
    assert_eq!(doc.index_in_parent(ids[0]), Some(2));
    doc.undo();
    assert_eq!(tree(&doc), before);
}

#[test]
fn pasted_trees_get_fresh_ids_and_keep_their_own_references() {
    let mut doc = Document::new("root");
    let (group, other) = doc
        .edit("Build", |tx| {
            let group = tx.add_child(tx.root(), "group")?;
            let a = tx.add_child(group, "a")?;
            let b = tx.add_child(group, "b")?;
            let other = tx.add_child(tx.root(), "other")?;
            tx.set(b, "follows", a)?;
            tx.set(b, "outside", other)?;
            Ok((group, other))
        })
        .unwrap();
    let copy = doc.subtree(group).unwrap();
    let pasted = doc
        .edit("Duplicate group", |tx| tx.insert_tree(tx.root(), 2, &copy))
        .unwrap();
    assert_ne!(pasted, group);
    let [a, b] = doc.children(pasted) else {
        panic!("two children")
    };
    assert_eq!(doc.get(*b, "follows"), Some(&Value::Node(*a)));
    assert_eq!(doc.get(*b, "outside"), Some(&Value::Node(other)));
    assert_eq!(doc.index_in_parent(pasted), Some(2));

    let mut built = NodeTree::new("built").with("x", 1);
    built.children.push(NodeTree::new("leaf"));
    let id = doc
        .edit("Insert", |tx| tx.insert_tree(tx.root(), 0, &built))
        .unwrap();
    assert_eq!(doc.kind(doc.children(id)[0]), Some("leaf"));
    doc.undo();
    doc.undo();
    assert_eq!(doc.len(), 5);
}

#[test]
fn the_journal_tells_each_reader_what_changed() {
    let mut doc = Document::new("root");
    let watch = doc.watch();
    let mut reader = doc.cursor();
    let child = doc
        .edit("Add", |tx| {
            let child = tx.add_child(tx.root(), "c")?;
            tx.set(child, "x", 1)?;
            Ok(child)
        })
        .unwrap();
    doc.undo();
    assert_eq!(watch.version(), doc.version());
    assert_eq!(doc.version(), 4);

    let Changes::Since(changes) = doc.changes(&mut reader) else {
        panic!("nothing missed");
    };
    let seen: Vec<_> = changes
        .map(|c| (c.node, c.kind.clone(), c.origin))
        .collect();
    assert_eq!(
        seen,
        [
            (
                NodeId::ROOT,
                ChangeKind::ChildAdded { child, index: 0 },
                Origin::Edit
            ),
            (
                child,
                ChangeKind::Property { name: "x".into() },
                Origin::Edit
            ),
            (
                child,
                ChangeKind::Property { name: "x".into() },
                Origin::Undo
            ),
            (
                NodeId::ROOT,
                ChangeKind::ChildRemoved { child, index: 0 },
                Origin::Undo
            ),
        ]
    );
    assert!(matches!(doc.changes(&mut reader), Changes::Since(c) if c.len() == 0));

    doc.set_journal_capacity(2);
    for value in 0..3 {
        doc.edit("Set", |tx| tx.set(tx.root(), "v", value)).unwrap();
    }
    assert!(matches!(doc.changes(&mut reader), Changes::Missed));
    assert_eq!(doc.journal_capacity(), 2);
}

/// A small random number generator, so the test needs no dependency and
/// every run is the same.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn all_nodes(doc: &Document) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        nodes.push(node);
        stack.extend(doc.children(node).iter().copied());
    }
    nodes
}

fn random_value(rng: &mut Rng, nodes: &[NodeId]) -> Value {
    match rng.below(8) {
        0 => Value::Int(rng.below(3) as i64),
        1 => Value::Float([0.0, -0.0, 0.5][rng.below(3)]),
        2 => Value::from(["a", "b"][rng.below(2)]),
        3 => Value::from(vec![rng.below(3) as u8; rng.below(3)]),
        4 => Value::Bool(rng.chance(50)),
        5 => Value::Node(nodes[rng.below(nodes.len())]),
        6 => Value::Param(manguebyte_state::ParamRef::new("cutoff").unwrap()),
        _ => Value::Int(7),
    }
}

/// One random edit; failures are fine, they change nothing.
fn random_op(rng: &mut Rng, tx: &mut manguebyte_state::Tx<'_>) {
    let nodes = all_nodes(tx.doc());
    let node = nodes[rng.below(nodes.len())];
    let name = ["a", "b", "c"][rng.below(3)];
    let _ = match rng.below(9) {
        0..=2 => tx.set(node, name, random_value(rng, &nodes)),
        3 => tx.remove_property(node, name).map(drop),
        4 => tx.add_child(node, ["x", "y"][rng.below(2)]).map(drop),
        5 => {
            let index = rng.below(tx.doc().children(node).len() + 1);
            tx.insert_child(node, index, "z").map(drop)
        }
        6 => tx.remove_node(node),
        7 => {
            let target = nodes[rng.below(nodes.len())];
            let len = tx.doc().children(target).len();
            tx.move_node(node, target, rng.below(len + 1))
        }
        _ => {
            let copy = tx.doc().subtree(node).unwrap();
            let target = nodes[rng.below(nodes.len())];
            if copy.len() < 20 {
                tx.insert_tree(target, 0, &copy).map(drop)
            } else {
                Ok(())
            }
        }
    };
}

/// Every state the history can reach, checked against the document.
struct Model {
    doc: Document,
    /// `states[i]` is the tree after `i` undo steps from the start.
    states: Vec<String>,
}

impl Model {
    fn counts(&self) -> (usize, usize) {
        (self.doc.undo_names().count(), self.doc.redo_names().count())
    }

    fn check(&self) {
        let (undo, redo) = self.counts();
        assert_eq!(self.states.len(), undo + redo + 1);
        assert_eq!(tree(&self.doc), self.states[undo]);
    }

    /// After something that may have dropped a gesture or a series that
    /// came to nothing: that step's state is its predecessor's.
    fn dropped(&mut self, before: (usize, usize), after: (usize, usize)) {
        if after.0 + 1 == before.0 && after.1 == before.1 {
            let gone = self.states.remove(before.0);
            assert_eq!(gone, self.states[after.0]);
        }
    }

    fn undo(&mut self) -> bool {
        let before = self.counts();
        let undone = self.doc.undo();
        let after = self.counts();
        if undone {
            assert_eq!(after.1, before.1 + 1);
            if after.0 + 2 == before.0 {
                // A gesture or a series that came to nothing went first.
                let gone = self.states.remove(before.0);
                assert_eq!(gone, self.states[before.0 - 1]);
            } else {
                assert_eq!(after.0 + 1, before.0);
            }
        } else {
            self.dropped(before, after);
        }
        self.check();
        undone
    }

    fn redo(&mut self) -> bool {
        let before = self.counts();
        let redone = self.doc.redo();
        let after = self.counts();
        if redone {
            assert_eq!((after.0, after.1 + 1), (before.0 + 1, before.1));
        } else {
            self.dropped(before, after);
        }
        self.check();
        redone
    }

    fn edited(&mut self, before: (usize, usize)) {
        let after = self.counts();
        if after == before {
            // Merged into the newest step, or no step at all.
            self.states[after.0] = tree(&self.doc);
        } else {
            assert_eq!(after.1, 0, "a new step leaves nothing to redo");
            self.states.truncate(after.0);
            self.states.push(tree(&self.doc));
        }
    }
}

#[test]
fn undoing_random_edits_restores_every_earlier_tree_exactly() {
    let steps = if cfg!(miri) { 60 } else { 3_000 };
    for seed in [1u64, 0x9e37_79b9_7f4a_7c15, 42] {
        let mut rng = Rng(seed);
        let mut doc = Document::new("root");
        doc.set_history_limits(HistoryLimits {
            max_steps: usize::MAX,
            max_bytes: usize::MAX,
        });
        let mut model = Model {
            states: vec![tree(&doc)],
            doc,
        };
        for _ in 0..steps {
            let before = model.counts();
            match rng.below(20) {
                0..=8 => {
                    let ops = 1 + rng.below(4);
                    model
                        .doc
                        .edit("Random", |tx| {
                            for _ in 0..ops {
                                random_op(&mut rng, tx);
                            }
                            Ok(())
                        })
                        .unwrap();
                    model.edited(before);
                }
                9 | 10 => {
                    let key = rng.below(2);
                    model
                        .doc
                        .edit_merging("Series", key, |tx| {
                            random_op(&mut rng, tx);
                            Ok(())
                        })
                        .unwrap();
                    model.edited(before);
                }
                11 => {
                    let state = tree(&model.doc);
                    let result = model.doc.edit("Failing", |tx| {
                        for _ in 0..1 + rng.below(4) {
                            random_op(&mut rng, tx);
                        }
                        tx.remove_node(tx.root())
                    });
                    assert_eq!(result, Err(EditError::Root));
                    assert_eq!(tree(&model.doc), state, "a failed edit is taken back");
                    assert_eq!(model.counts(), before);
                }
                12 => {
                    model.doc.begin_gesture("Gesture");
                    let after = model.counts();
                    model.dropped(before, after);
                }
                13 => {
                    model.doc.end_gesture();
                    let after = model.counts();
                    model.dropped(before, after);
                }
                14..=16 => {
                    model.undo();
                }
                _ => {
                    model.redo();
                }
            }
            model.check();
        }

        // The walk went somewhere: about a thousand steps deep for 3 000
        // actions, with dozens of nodes.
        assert!(model.states.len() > steps / 10, "{}", model.states.len());
        // All the way back, and all the way forward again.
        while model.undo() {}
        assert_eq!(tree(&model.doc), model.states[0]);
        while model.redo() {}
        assert_eq!(model.counts().1, 0);
        assert_eq!(tree(&model.doc), *model.states.last().unwrap());

        // And what the edits made saves and loads exactly.
        let (text, report) = Document::from_text(&model.doc.to_text()).unwrap();
        assert!(report.is_exact(), "{report:?}");
        assert_eq!(tree(&text), tree(&model.doc));
        let (bytes, report) = Document::from_bytes(&model.doc.to_bytes()).unwrap();
        assert!(report.is_exact(), "{report:?}");
        assert_eq!(tree(&bytes), tree(&model.doc));
    }
}
