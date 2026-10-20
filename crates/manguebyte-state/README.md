# manguebyte-state

Application state for the Manguebyte Audio Framework: a document tree with undo and redo, change journals, parameter links, saved state, settings files and snapshots for the audio thread.

- A `Document` is a tree of typed nodes with properties (numbers, text,
  switches, binary data, references to parameters and to other nodes) and
  ordered children. Node ids never change and are never reused.
- Every change is a named transaction that can be undone and redone. A knob
  drag is one step (`begin_gesture` / `end_gesture`); a series of nudges
  merges by key (`edit_merging`); history is bounded by steps and bytes.
- Nothing calls back. Readers drain a change journal with their own cursor,
  or poll a version from another thread.
- A document mirrors a plugin's parameters: the user's changes are undoable,
  host automation is not. Put a `SharedDocument` in a parameter struct and
  `manguebyte_plugin::save_state` saves it with the values.
- A versioned text form and a compact binary form, both loaded tolerantly,
  with migrations for an application's older schemas.
- `Settings`: a key and value file in the user's configuration folder.
- `snapshot`: hand immutable snapshots to the audio thread, which never
  allocates, frees or locks to read them.

```rust
use manguebyte_state::Document;

let mut doc = Document::new("instrument");
let zone = doc
    .edit("Add zone", |tx| {
        let zone = tx.add_child(tx.root(), "zone")?;
        tx.set(zone, "key", 60)?;
        Ok(zone)
    })
    .unwrap();
assert_eq!(doc.undo_name(), Some("Add zone"));
doc.undo();
assert!(!doc.contains(zone));
doc.redo();
assert!(doc.contains(zone), "the same node, with the same id");
```

ADR 0008 (`docs/adr/0008-application-state.md` in the repository) explains
the design.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

Documentation: <https://docs.rs/manguebyte-state>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
