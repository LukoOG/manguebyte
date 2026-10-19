# Aratu Bridge

A CLAP, VST 3 and Audio Unit instrument that plays the music of an
[Aratu](https://aratu.live) page inside a DAW. The page sends the plugin
what it would send its own sound engine (control calls, 32-byte event
records and its sample files) over a WebSocket on 127.0.0.1, and the plugin
renders them with the same engine, `manguebyte-web`, natively, in time with
the DAW's transport and on the DAW's audio device. The sound lands on the
plugin's track, to be recorded, mixed and processed; the browser never
touches the audio device.

[`docs/ARATU_BRIDGE.md`](../../docs/ARATU_BRIDGE.md) specifies the
protocol, its security rules and its timing; the page's side is
[`crates/manguebyte-web/js/bridge.js`](../../crates/manguebyte-web/js/bridge.js).

## Parameters

| Parameter | Range | Default | |
|-----------|-------|---------|-|
| Gain | -60 to +12 dB | 0 dB | the output level, after the engine's master bus and its limiter |
| Port | 1024 to 65535 | 47870 | the TCP port on 127.0.0.1 the plugin listens on; not automatable, saved with the project |

Each instance of the plugin needs a port of its own: a second instance on
a port already taken does not listen until its port is changed.

The plugin's latency is the engine's master bus look-ahead (111 frames at
48 kHz), which the DAW compensates. It has one stereo output and no editor.

It has been tried by hand once, as a CLAP in REAPER 7.82 on Windows, with
a page in Chromium served from `http://127.0.0.1`: the page sent 44
events and the plugin applied 44, none late or dropped, following
REAPER's tempo. Other DAWs, the VST 3 and AU builds, other browsers and
`https://` pages are not tried yet (`docs/TECH_DEBT.md`, item 84).

## Build and install

From the repository's root, with the framework's command-line tool:

```sh
# Bundles in target/bundled (or --out <folder>).
cargo run -p cargo-mangue -- mangue bundle -p aratu-bridge --release --format clap,vst3

# Bundles, then copies them where hosts look for the current user:
#   Windows  %LOCALAPPDATA%\Programs\Common\CLAP and ...\VST3
#   Linux    ~/.clap and ~/.vst3
#   macOS    ~/Library/Audio/Plug-Ins/CLAP and .../VST3
cargo run -p cargo-mangue -- mangue install -p aratu-bridge --release --format clap,vst3

# On a Mac only: the Audio Unit, Aratu Bridge.component, into
# ~/Library/Audio/Plug-Ins/Components.
cargo run -p cargo-mangue -- mangue install -p aratu-bridge --release --format au
```

With `cargo-mangue` installed (`cargo install --path crates/cargo-mangue`),
the same commands are `cargo mangue bundle ...` and `cargo mangue install
...`. Close the DAW before installing over a copy it has loaded.

What a bundle holds, to copy by hand instead:

| Format | On Windows | Copy to |
|--------|------------|---------|
| CLAP | `Aratu Bridge.clap` (one file) | `%LOCALAPPDATA%\Programs\Common\CLAP\` or `%COMMONPROGRAMFILES%\CLAP\` |
| VST 3 | the folder `Aratu Bridge.vst3` (with `Contents\x86_64-win\Aratu Bridge.vst3` inside) | `%LOCALAPPDATA%\Programs\Common\VST3\` or `%COMMONPROGRAMFILES%\VST3\` |

## Use it in a DAW

1. Put **Aratu Bridge** on an instrument track (in REAPER: Insert virtual
   instrument on new track, CLAP or VST3i). Arm or monitor the track if the
   DAW only processes instruments that are.
2. Set the project's tempo; the plugin follows it, its loops and its play
   cursor.
3. In the page, connect:

   ```js
   import { AratuBridge } from './bridge.js';
   const engine = await AratuBridge.connect({ port: 47870 });
   await engine.createPart(0, 'drums');
   engine.noteOn(0, 36, 1, engine.context.currentTime + 0.1);
   ```

   The page must come from an allowed origin: `https://aratu.live`, or
   `http://localhost` / `http://127.0.0.1` on any port. Add others with the
   environment variable `ARATU_BRIDGE_ORIGINS` (comma-separated) or the
   file `aratu-bridge/origins.txt` in the user's configuration folder
   (`%APPDATA%` on Windows), one per line, before the DAW starts.
4. Press play. Events are scheduled on the DAW's timeline; while it is
   stopped, `bridge.js` plays what the page schedules as soon as it is due.

## Tests

```sh
cargo test -p aratu-bridge
cargo test -p manguebyte-host --test clap_aratu_bridge   # hosted as a CLAP
node --test crates/manguebyte-web/js/tests/bridge.test.mjs
```

- `tests/same_bits.rs`: a session sent over a real socket renders, through
  `Harness` in three block patterns at two tempos, exactly as the engine
  renders it alone.
- `tests/transport.rs`: a tempo change, a loop gone round inside a block,
  a jump and a stop, each event heard on its beat's frame.
- `tests/server.rs`: origins, the Host, the subprotocol, one page at a
  time, pings, closes, fragments, oversized and malformed frames, versions
  and statuses, over a socket.
- `tests/realtime.rs`: no heap call in `process` while a page sends
  everything it can and the host loops, jumps and stops.
- `tests/golden.rs`: one message's bytes, the same as the JavaScript
  client's.

Licensed, like the rest of the repository, under MIT or Apache 2.0.
