# manguebyte-graph

A dynamic audio processing graph for the Manguebyte Audio Framework: rewire it while it plays, with automatic latency compensation.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

`Graph` is the description, edited on any thread; `GraphEngine` is the
processor that runs it on the audio thread. A commit compiles a plan and
hands it over through a wait-free queue, so the engine never allocates,
locks or frees, not even to switch plans, and removed nodes are freed off
the audio thread. Nodes keep their state across edits, and so does
whatever is travelling through a delay the old and the new plan share.

- **Sidechains.** Every node has a main input and a sidechain, which it
  receives as `ProcessContext::sidechain`, as a plugin receives its second
  bus. A `Route` names the bus and the channels a connection carries.
- **Events.** Notes, parameter changes and MIDI travel along event
  connections on their frames. A node added with `add_with_events` sends
  them, as an arpeggiator or a hosted plugin with a note output does;
  several inputs are merged in a fixed order.
- **Feedback.** A loop must pass through a delayed connection, with a
  delay in frames, so that it sounds the same whatever the block size; any
  other loop is refused.
- **Latency compensation.** Paths of different latency are lined up where
  they meet, at a main input, a sidechain, an event input or the output.

What it does not do yet is listed in
[TECH_DEBT](https://github.com/alessonqueirozdev-hub/manguebyte/blob/main/docs/TECH_DEBT.md)
items 19, 20 and 88.

Documentation: <https://docs.rs/manguebyte-graph>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
