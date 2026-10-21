# manguebyte-vst3

Builds VST 3 plugins from the Manguebyte plugin API.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

`export_vst3!(MyPlugin)` exports a `Plugin` as a VST 3 component that is
also its own controller: audio and event buses, automation on its frames,
notes and note expressions, the MIDI controllers it declares, state,
latency and an editor. It processes 32-bit and 64-bit audio (`kSample64`,
through the plugin's 32-bit path, so both give the same numbers), and
`setBusArrangements` and `activateBus` choose among the layouts the
plugin offers in `Plugin::LAYOUTS`. Not there yet: program lists.

Documentation: <https://docs.rs/manguebyte-vst3>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
