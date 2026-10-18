# manguebyte-pro-io

ASIO driver support for the Manguebyte Audio Framework, under the GPLv3: a
device backend for [`manguebyte-devices`](../manguebyte-devices).

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte).
Unlike the rest of the framework, this crate is **opt-in and under the
GPLv3**: nothing else in the framework depends on it, and the `manguebyte`
facade does not include it. Depend on it directly to use it.

Documentation: <https://docs.rs/manguebyte-pro-io>

## What it does

- Lists the installed drivers from the registry
  (`HKEY_LOCAL_MACHINE\SOFTWARE\ASIO`), with their class identifiers,
  descriptions and libraries.
- Loads one through COM and initialises it, on a thread of its own that is
  a single-threaded COM apartment, as drivers expect. A process loads one
  driver at a time, and the API enforces it.
- Asks the driver its channels and their sample types, latencies, buffer
  sizes (smallest, largest, preferred, granularity), sample rates and
  clocks.
- Creates its buffers, starts and stops it, and runs a callback on planar
  `f32` blocks, with input and output in one callback on the driver's
  clock. Every sample type the SDK defines is converted: 16, 24 and 32-bit
  integers in both byte orders, 32 and 64-bit floats, and the 32-bit words
  holding 16, 18, 20 or 24 bits; the DSD types are reported as
  unsupported. The path from the driver to the callback allocates nothing
  and takes no lock (tested with a counting allocator).
- Answers the driver's `asioMessage` selectors as the SDK documents them,
  takes `bufferSwitchTimeInfo` when the driver offers it, and calls
  `outputReady` when the driver asks for it.
- Never acts on a reset request inside the callback that carries it. A
  reset, a new buffer size, a new sample rate or a loss of sync is counted,
  heard on the driver's thread, and acted on by whoever owns the stream: it
  drops the stream and the driver, and opens them again.
- Plugs into a `DeviceManager` (`ProIoBackend`), through the backend seam
  `manguebyte-devices` offers any outside crate. The manager sees a
  driver's reset as a stream lost and restarts it, as it does a device that
  was unplugged and came back. Saved setups, fallbacks and device events
  work as with the system's devices.
- Comes with a fake driver written in Rust (`FakeDevice`), a COM-shaped
  object with the driver interface's vtable, which runs all of the above on
  any system without hardware.

```rust
use manguebyte_devices::{DeviceChoice, DeviceError, DeviceIo, DeviceManager, DeviceSetup};
use manguebyte_pro_io::ProIoBackend;

fn main() -> Result<(), DeviceError> {
    let mut manager = DeviceManager::with_audio_backend(ProIoBackend::new());
    // One driver, input and output together.
    let setup = DeviceSetup::default().with_input(DeviceChoice::Default);
    manager.start(&setup, |io: &mut DeviceIo<'_>| {
        // The output starts silent. Fill it here, without allocating.
        let _ = io.input.num_channels();
    })?;
    loop {
        // Hear about resets and unplugged devices, and follow them.
        for event in manager.poll_devices() {
            manager.follow(&event);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
```

`examples/asio_probe.rs` opens every installed driver, runs two seconds of
silence on it and reports what it saw:

```text
cargo run -p manguebyte-pro-io --example asio_probe
```

Drivers exist on Windows only. Elsewhere the driver list is empty and the
fake driver runs the tests.

## Licence

This crate is under the [GNU General Public License, version 3](LICENSE)
only (`GPL-3.0-only`).

Its driver interface is declared by hand from the Steinberg ASIO SDK 2.3.4
interface definitions, which Steinberg licenses either under its own
proprietary licence or under the GPLv3. The proprietary licence forbids
re-working the interface inside a framework, so this crate uses the SDK
under its GPLv3 option, and is itself under the GPLv3. No file of the SDK is
in this repository.

What that means for a program:

- **A program that uses this crate is under the GPLv3** as a whole, and must
  be distributed under its terms, with its source.
- **GPLv3 and AGPLv3 programs can use it.** Aratu, under the AGPL-3.0, can.
- **A closed-source program must not use this crate.** It takes the other
  path, which keeps the rest of the framework's MIT or Apache-2.0 licence:
  the application turns on `cpal`'s own `asio` feature, with an SDK it
  downloads from Steinberg and licenses itself, and `manguebyte-devices`
  then lists ASIO among its hosts. The documentation of
  [`manguebyte_devices::audio_hosts`](https://docs.rs/manguebyte-devices/latest/manguebyte_devices/fn.audio_hosts.html#asio)
  ([source](../manguebyte-devices/src/host.rs)) explains how.

No other crate of the workspace may depend on this one; a check in CI
(`scripts/check-layering.sh`) makes sure.

ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
