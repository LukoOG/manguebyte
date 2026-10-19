# manguebyte-host

Plugin hosting for the Manguebyte Audio Framework: load CLAP, VST 3 and LV2 plugins and run them as processors.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

It loads plugins made by anyone and runs them as processors, through one
interface for all three formats (`any`), with every event on its frame and
nothing allocated while processing. The thread rules are types: the
main-thread half of a CLAP or VST 3 plugin cannot be sent to another
thread. LV2 bundles are read with the crate's own Turtle reader, which
runs no code. `scan` finds the plugins installed on a system and lists
each CLAP and VST 3 file in a process of its own with a time limit, so one
that crashes or hangs is reported and the host goes on, with a cache of
what it found. Not yet: hosting AU plugins, the editors of hosted plugins,
reading the notes and MIDI a hosted plugin sends, and runs against plugins
from other vendors.

Documentation: <https://docs.rs/manguebyte-host>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
