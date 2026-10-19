# manguebyte-devices

Audio and MIDI device input and output for the Manguebyte Audio Framework.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

It lists and opens audio devices, runs callbacks on planar `f32` blocks,
follows devices as they come and go, restores saved setups with
fallbacks, and opens MIDI ports. Its `DeviceManager` can also run an
engine at a fixed format and change the devices under it in the middle of
a song, without stopping or preparing it again: a crossfade where two
devices can play at once, a short faded gap where they cannot
(`DeviceManager::start_engine`, the `hotswap` module).

Documentation: <https://docs.rs/manguebyte-devices>

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
