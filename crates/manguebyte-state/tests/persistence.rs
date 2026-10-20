//! Saving and loading documents: round trips, old and newer versions,
//! migrations and damaged input.

use manguebyte_state::{
    Document, FORMAT_VERSION, LoadError, MAX_DEPTH, Migrations, NodeId, NodeTree, ParamRef, Value,
};

fn tree(doc: &Document) -> String {
    format!("{:?}", doc.subtree(doc.root()).unwrap())
}

/// A document with one of everything.
fn sample() -> Document {
    let mut doc = Document::new("instrument");
    doc.set_schema(3);
    doc.edit("Build", |tx| {
        let root = tx.root();
        tx.set(root, "name", "Kit \"Mangue\"\nline two\t\u{1}")?;
        tx.set(root, "gain", 0.1)?;
        tx.set(root, "zero", -0.0)?;
        tx.set(root, "big", 1e21)?;
        tx.set(root, "tiny", 1.5e-7)?;
        let params = tx.add_child(root, "params")?;
        let removed = tx.add_child(root, "removed")?;
        let zone = tx.add_child(root, "zone")?;
        tx.set(zone, "key", -60)?;
        tx.set(zone, "max", i64::MAX)?;
        tx.set(zone, "loop", true)?;
        tx.set(zone, "sample", vec![0u8, 1, 0xfe, 0xff])?;
        tx.set(zone, "empty", Vec::<u8>::new())?;
        tx.set(zone, "follows", params)?;
        tx.set(zone, "dangling", removed)?;
        tx.set(zone, "modulates", ParamRef::new("env.attack").unwrap())?;
        tx.set(zone, "unicode", "ação 日本")?;
        let nested = tx.add_child(zone, "layer")?;
        tx.add_child(nested, "leaf")?;
        tx.remove_node(removed)
    })
    .unwrap();
    doc
}

#[test]
fn text_and_binary_round_trip_exactly() {
    let doc = sample();
    let text = doc.to_text();
    let (from_text, report) = Document::from_text(&text).unwrap();
    assert!(report.is_exact(), "{report:?}");
    assert_eq!(report.format_version, FORMAT_VERSION);
    assert_eq!(tree(&from_text), tree(&doc));
    assert_eq!(from_text.schema(), 3);
    assert_eq!(
        from_text.to_text(),
        text,
        "the same document is the same text"
    );

    let bytes = doc.to_bytes();
    assert!(bytes.len() < text.len());
    let (from_bytes, report) = Document::from_bytes(&bytes).unwrap();
    assert!(report.is_exact(), "{report:?}");
    assert_eq!(tree(&from_bytes), tree(&doc));
    assert_eq!(from_bytes.to_bytes(), bytes);
    assert_eq!(from_bytes.to_text(), text);
}

#[test]
fn ids_are_not_reused_after_loading() {
    let doc = sample();
    // Node 2 was removed; neither it nor any other id comes back.
    let highest = doc
        .to_text()
        .lines()
        .find_map(|line| line.strip_prefix("next "))
        .unwrap()
        .to_owned();
    for (mut loaded, _) in [
        Document::from_text(&doc.to_text()).unwrap(),
        Document::from_bytes(&doc.to_bytes()).unwrap(),
    ] {
        let fresh = loaded
            .edit("Add", |tx| tx.add_child(tx.root(), "new"))
            .unwrap();
        assert_eq!(fresh.get().to_string(), highest);
    }
}

/// What version 1 of the text form looks like. A change to the writer that
/// breaks this test changes the format: give it a new version and keep
/// reading this one.
const VERSION_1: &str = "\
manguebyte-state 1
schema 3
next 6
node 0 instrument {
  big = float 1000000000000000000000
  gain = float 0.1
  name = text \"Kit \\\"Mangue\\\"\\nline two\\t\\u{1}\"
  tiny = float 0.00000015
  zero = float -0
  node 1 params {
  }
  node 3 zone {
    dangling = node 2
    empty = bytes
    follows = node 1
    key = int -60
    loop = bool true
    max = int 9223372036854775807
    modulates = param \"env.attack\"
    sample = bytes 0001feff
    unicode = text \"ação 日本\"
    node 4 layer {
      node 5 leaf {
      }
    }
  }
}
";

#[test]
fn version_1_text_is_written_and_read_as_it_always_was() {
    assert_eq!(sample().to_text(), VERSION_1);
    let (doc, report) = Document::from_text(VERSION_1).unwrap();
    assert!(report.is_exact());
    assert_eq!(tree(&doc), tree(&sample()));
}

#[test]
fn text_from_a_newer_version_loads_as_far_as_it_can() {
    let newer = "\
manguebyte-state 2
schema 9
# A comment, and a blank line.

created-by \"Parabolic 4\"
next 5
node 0 instrument {
  gain = float 0.5
  colour = rgba 255 0 0 255
  broken = float not-a-number
  nan = float NaN
  layout {
    x = int 1
    nested {
    }
  }
  bad name = int 1
  node 1 zone {
    key = int 60\r
  }
  node 2 bad kind {
  }
  node 3 has_bad_id_next {
  }
  node x zone {
    key = int 61
  }
  something new
}
node 4 second-root {
}
";
    let (doc, report) = Document::from_text(newer).unwrap();
    assert_eq!(report.format_version, 2);
    assert_eq!(report.schema_found, 9);
    assert!(!report.is_exact());
    let lines: Vec<usize> = report.skipped.iter().map(|skipped| skipped.at).collect();
    assert_eq!(lines, [5, 9, 10, 11, 12, 17, 21, 25, 28, 30], "{report:#?}");

    assert_eq!(doc.get(doc.root(), "gain"), Some(&Value::Float(0.5)));
    assert_eq!(doc.get(doc.root(), "colour"), None);
    let zones: Vec<NodeId> = doc.children_of_kind(doc.root(), "zone").collect();
    assert_eq!(zones.len(), 2);
    assert_eq!(
        doc.get(zones[0], "key"),
        Some(&Value::Int(60)),
        "CRLF is fine"
    );
    assert_eq!(
        doc.get(zones[1], "key"),
        Some(&Value::Int(61)),
        "a fresh id"
    );
    assert!(zones[1].get() >= 5);
    assert_eq!(doc.schema(), 9, "a newer schema is kept as it is");
}

#[test]
fn text_that_is_not_a_document_is_refused() {
    assert_eq!(
        Document::from_text("").unwrap_err(),
        LoadError::MissingHeader
    );
    assert_eq!(
        Document::from_text("manguebyte-params 1\n").unwrap_err(),
        LoadError::MissingHeader
    );
    assert_eq!(
        Document::from_text("manguebyte-state 1\nschema 1\n").unwrap_err(),
        LoadError::NoRoot
    );
    assert_eq!(
        Document::from_text("manguebyte-state 1\nnode 0 a {\n").unwrap_err(),
        LoadError::Unbalanced(2)
    );
    assert_eq!(
        Document::from_text("manguebyte-state 1\n}\n").unwrap_err(),
        LoadError::Unbalanced(2)
    );
    let mut deep = String::from("manguebyte-state 1\n");
    for depth in 0..=MAX_DEPTH + 1 {
        deep.push_str(&format!("node {depth} n {{\n"));
    }
    assert_eq!(Document::from_text(&deep).unwrap_err(), LoadError::TooDeep);
}

#[test]
fn duplicate_ids_are_renumbered_and_reported() {
    let text = "manguebyte-state 1\nnode 7 root {\n  node 1 a {\n  }\n  node 1 b {\n  }\n}\n";
    let (doc, report) = Document::from_text(text).unwrap();
    assert_eq!(report.renumbered, 2, "the root's 7 and the second 1");
    assert!(!report.is_exact());
    let [a, b] = doc.children(doc.root()) else {
        panic!("two children");
    };
    assert_eq!(a.get(), 1);
    assert_ne!(b.get(), 1);
    assert_eq!(doc.kind(*b), Some("b"));
}

#[test]
fn migrations_bring_old_schemas_up_to_date_in_both_forms() {
    let migrations = || {
        Migrations::new(3)
            .step(1, |root| {
                for zone in &mut root.children {
                    if let Some(note) = zone.remove("note") {
                        zone.set("key", note);
                    }
                }
            })
            .step(2, |root| {
                root.children.push(NodeTree::new("effects"));
            })
    };
    let mut old = Document::new("instrument");
    old.set_schema(1);
    old.edit("Old", |tx| {
        let zone = tx.add_child(tx.root(), "zone")?;
        tx.set(zone, "note", 60)
    })
    .unwrap();

    for (doc, report) in [
        Document::from_text_with(&old.to_text(), &migrations()).unwrap(),
        Document::from_bytes_with(&old.to_bytes(), &migrations()).unwrap(),
    ] {
        assert_eq!((report.schema_found, report.schema), (1, 3));
        assert!(report.is_exact());
        assert_eq!(doc.schema(), 3);
        let zone = doc.children(doc.root())[0];
        assert_eq!(doc.get(zone, "key"), Some(&Value::Int(60)));
        assert_eq!(doc.get(zone, "note"), None);
        let effects = doc.children(doc.root())[1];
        assert_eq!(doc.kind(effects), Some("effects"));
        assert!(
            effects.get() >= 2,
            "a node a migration adds gets a fresh id"
        );
    }

    // A schema newer than the application's loads as it is, and says so.
    let mut newer = Document::new("instrument");
    newer.set_schema(5);
    let (doc, report) = Document::from_text_with(&newer.to_text(), &migrations()).unwrap();
    assert_eq!(doc.schema(), 5);
    assert!(!report.is_exact());

    // A migration that breaks the tree is reported.
    let bad = Migrations::new(1).step(0, |root| root.kind = String::from("not valid"));
    let error = Document::from_text_with(&Document::new("a").to_text(), &bad).unwrap_err();
    assert!(matches!(error, LoadError::Migration(_)));
}

#[test]
fn the_binary_form_skips_unknown_types_and_survives_damage() {
    let mut doc = Document::new("root");
    doc.edit("Set", |tx| {
        tx.set(tx.root(), "a", 5)?;
        tx.set(tx.root(), "b", "keep")
    })
    .unwrap();
    let mut bytes = doc.to_bytes();
    // Property "a": name length 1, 'a', then the type byte for int (2).
    let at = bytes
        .windows(3)
        .position(|window| window == [1, b'a', 2])
        .unwrap()
        + 2;
    bytes[at] = 99;
    let (read, report) = Document::from_bytes(&bytes).unwrap();
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert_eq!(read.get(read.root(), "a"), None);
    assert_eq!(read.get(read.root(), "b"), Some(&Value::from("keep")));

    assert_eq!(
        Document::from_bytes(b"").unwrap_err(),
        LoadError::MissingHeader
    );
    assert_eq!(
        Document::from_bytes(b"MBSTATE").unwrap_err(),
        LoadError::MissingHeader
    );

    // Every truncation is refused, never a panic; every corrupted byte
    // reads as something or is refused.
    let bytes = sample().to_bytes();
    for len in 0..bytes.len() {
        assert!(Document::from_bytes(&bytes[..len]).is_err(), "length {len}");
    }
    let stride = if cfg!(miri) { 37 } else { 1 };
    for at in (0..bytes.len()).step_by(stride) {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut damaged = bytes.clone();
            damaged[at] ^= flip;
            let _ = Document::from_bytes(&damaged);
        }
    }

    // And so is every truncation of the text.
    let text = sample().to_text();
    for (len, _) in text.char_indices().step_by(stride) {
        let _ = Document::from_text(&text[..len]);
    }
}
