# The Aratu bridge: share the music, not the audio

[Aratu](https://aratu.live) is a live-coding language in the browser. Its
sound engine is `manguebyte-web`: Rust compiled to WebAssembly, driven
from JavaScript by control calls (make a part, give it effects, load an SFZ
instrument) and by 32-byte event records.

The same engine builds natively. The **Aratu Bridge** plugin
([`plugins/aratu-bridge`](../plugins/aratu-bridge)) runs it inside a DAW as
a CLAP, VST 3 or Audio Unit instrument. Instead of streaming audio from the
browser to the DAW, the page sends the plugin what it would send its own
engine: the control calls, the event records and the bytes of its sample
files, a few bytes per note. The plugin renders them with the same engine,
in time with the DAW's transport, on the DAW's own audio device:

- the browser never opens the audio device, so there is no fight over an
  exclusive device (ASIO, or WASAPI exclusive mode);
- the sound lands on a DAW track, where it is recorded, mixed and
  processed like any other instrument's;
- the music follows the DAW's tempo, loops and play cursor, sample for
  sample.

`plugins/aratu-bridge/tests/same_bits.rs` proves the central claim: a
session of control calls and events, sent over a real WebSocket on
127.0.0.1 and rendered by the plugin through `manguebyte_plugin::Harness`
in blocks of changing sizes at a tempo, is the same, bit for bit, as the
same session rendered by the engine directly.

This document is the protocol's specification. The byte layout is
`plugins/aratu-bridge/src/protocol.rs`; the JavaScript client is
`crates/manguebyte-web/js/bridge.js`; `crates/manguebyte-web/js/tests/
fixtures/bridge-golden.hex` pins one message on both sides.

## Transport

- A WebSocket (RFC 6455) on **`ws://127.0.0.1:47870`**. The port is the
  plugin's `Port` parameter (1024 to 65535), saved with the DAW project;
  each instance of the plugin needs a port of its own.
- The plugin binds **127.0.0.1 only**, never another interface: nothing
  off the machine can reach it. A page should connect to `127.0.0.1`
  rather than `localhost`, which a browser may resolve to `::1` first.
- The client must offer the subprotocol **`aratu-bridge.1`**
  (`new WebSocket(url, 'aratu-bridge.1')`), and the plugin selects it.
- One page at a time: while one is connected, another handshake is refused
  with HTTP 409. When the page goes, every note is let go and every event
  waiting is dropped; the parts stay until the next page says hello.
- Every message is binary; a text message closes the connection with code
  1003. No WebSocket extension is offered or accepted.

The WebSocket, SHA-1 (RFC 3174) and base 64 (RFC 4648) are written from the
RFCs in the plugin (`src/ws.rs`, `src/sha1.rs`, `src/base64.rs`) and tested
with the RFCs' own vectors: the handshake example of RFC 6455 section 1.3
and the masked "Hello" of section 5.7, RFC 3174 section 7.3 and RFC 4648
section 10.

## Security

A page on any web site could try to open a WebSocket to 127.0.0.1, so the
plugin checks every handshake before it upgrades:

- **Origin.** A browser always sends the page's origin, and the plugin
  refuses (HTTP 403) a handshake without one, or from an origin not on its
  allow-list. The list is `https://aratu.live`, `http://localhost:*`,
  `http://127.0.0.1:*`, `https://localhost:*` and `https://127.0.0.1:*`
  (`:*` is any port), and whatever the environment variable
  `ARATU_BRIDGE_ORIGINS` (comma-separated) and the file
  `aratu-bridge/origins.txt` in the user's configuration folder
  (`%APPDATA%` on Windows, `~/Library/Application Support` on macOS,
  `$XDG_CONFIG_HOME` or `~/.config` elsewhere; one origin a line, `#`
  for comments) add. They are read when the plugin is activated. `null`,
  the origin of a file or a sandboxed frame, is never allowed.
- **Host.** The `Host` header must be `127.0.0.1`, `localhost` or `[::1]`
  with the plugin's port (HTTP 403 otherwise), which keeps out a page that
  makes a name of its own point at 127.0.0.1 (DNS rebinding); its Origin
  would be refused as well.
- **Version 13** of WebSocket (HTTP 426 otherwise), a 16-byte key, the
  subprotocol (HTTP 400 otherwise).
- **Sizes.** The handshake is read up to 8 KiB, for 2 s at most. A message
  is at most 64 MiB (room for one large sample file); a frame whose header
  announces more, or fragments that add up to more, close the connection
  with code 1009 before the payload is read. An events message carries at
  most 65 536 records, an SFZ text at most 4 MiB. The plugin's queue to the
  audio thread holds 65 536 items, and its stores 32 768 events for the
  current lap of the timeline and as many for the next; what does not fit
  is dropped and counted.
- Frames that break RFC 6455 (unmasked from a client, reserved bits,
  unknown opcodes, fragmented or long control frames, a continuation with
  nothing to continue, a 64-bit length with its top bit set, an invalid
  close code, text or a close reason that is not UTF-8) close the
  connection with code 1002 or 1007.

A process on the same machine can always connect, Origin or not: the
bridge does not defend against local software, only against web pages.

## Browser constraints

What is known, and what must still be tried in real browsers. So far the
bridge has been tried by hand once: on 19 October 2026, a page in
Chromium served from `http://127.0.0.1` played through the CLAP build in
REAPER 7.82 on Windows, which applied all 44 events the page sent, none
late or dropped, following REAPER's tempo.

- **Mixed content.** An `https://` page may open `ws://127.0.0.1` and
  `ws://localhost`: the W3C *Secure Contexts* specification counts
  loopback addresses as potentially trustworthy, and Chrome and Firefox
  follow it for WebSockets. Safari has been stricter about `ws://` from
  `https://` pages in the past. **To test**: aratu.live in current Chrome,
  Firefox, Safari and Edge, connecting to `ws://127.0.0.1:47870`.
- **Private (local) network access.** Chrome restricts requests from
  public web sites to the local network and to loopback ("Private Network
  Access", now "Local Network Access"), and may ask the user for
  permission. The rollout has changed over releases and its coverage of
  WebSockets is not settled here. **To test**: whether Chrome shows a
  permission prompt for aratu.live, and whether the connection works once
  it is granted. If a browser blocks it, the page can be served from
  `http://localhost` (allowed by default) as a fallback.
- **Background tabs.** A page whose tab is hidden gets its timers slowed
  to once a second or less. In `'seconds'` mode the client sends events as
  they are scheduled, not from a timer, but Aratu's own scheduler runs on
  timers: it should plan far enough ahead (`latency().ahead`, and more for
  a hidden tab) or keep the tab visible. **To test** with Aratu.
- No cross-origin isolation is needed: the bridge uses no
  `SharedArrayBuffer`.

## Messages

Every message starts with an eight-byte header; every number is little
endian.

| Bytes | Field     | Meaning |
|-------|-----------|---------|
| 0     | `type`    | the message type |
| 1     | `flags`   | 0 |
| 2..4  | reserved  | 0 |
| 4..8  | `request` | an id the page chooses for a request; the reply carries it back |

### From the page

| Type | Name | Body | Answer |
|------|------|------|--------|
| 0x01 | hello | `u32` protocol version (1), `u32` engine interface version (`ffi::ABI_VERSION`, 2), the page's name in UTF-8 | a welcome |
| 0x02 | events | `u32` epoch, `u32` 0, then records | none |
| 0x10 | create part | `u16` part, `u16` 0, `u32` instrument code, `u32` chain bits | a reply |
| 0x11 | set instrument | `u16` part, `u16` 0, `u32` instrument code | a reply |
| 0x12 | remove part | `u16` part | a reply |
| 0x13 | set chain | `u16` part, `u16` 0, `u32` chain bits | a reply |
| 0x20 | SFZ begin | `u16` part, `u16` 0, the SFZ text in UTF-8 | a reply with the files it names, a JSON array |
| 0x21 | SFZ file | `u16` part, `u16` length of the path, the path in UTF-8, the file's bytes | a reply |
| 0x22 | SFZ finish | `u16` part | a reply with a JSON summary |
| 0x23 | SFZ abandon | `u16` part | a reply |
| 0x30 | query | `u32` what, `u32` argument (a part) | a reply |
| 0x31 | sync | none | a reply once everything sent before it is in the audio thread's queue |
| 0x32 | panic | none | a reply; every note is let go and every waiting event dropped |

Before the hello only a hello is answered (other requests get status
-203). A hello whose versions differ from the plugin's is answered with
status -200 and the connection is closed with code 4000. A hello starts a
**new session**: a fresh engine, with no parts, is made off the audio
thread and swapped in.

The codes are the engine's own: instrument codes as `InstrumentKind::code`
gives them and `INSTRUMENT` in `js/engine.js` lists them, chain bits as
`Chain::bits` and `CHAIN`.

### Records

An events message carries the engine's 32-byte records
(`manguebyte_web::record`), with one change: **bytes 0..8 hold an `f64`
beat instead of the `u64` frame**. The beat is a position on the DAW's
timeline in quarter notes, as the DAW's transport counts them. A NaN beat
means "as soon as possible". Every other byte is the record as the
browser engine reads it: kind, flags (the accent), part, note, channel,
velocity, parameter id, value.

Records for `master.tempo` and `master.beat` are dropped (and counted as
`overridden`): the DAW's transport sets them, at the frame each block
starts on when the tempo changes or the transport starts, jumps or goes
round its loop, so that tempo-synced effects follow the DAW.

### From the plugin

| Type | Name | Body |
|------|------|------|
| 0x81 | welcome | `u32` protocol version, `u32` engine interface version, `f64` sample rate, `u32` latency in frames, `u32` largest block, `u32` most parts (64), `u32` largest message, `u32` epoch, the plugin's version in UTF-8 |
| 0x82 | transport | see below; every 10 ms while the DAW plays, every 100 ms while it is stopped, and at once when the epoch, the tempo or playing changes |
| 0x83 | reply | `i32` status, then what was asked for |
| 0x84 | status | the counters, every 250 ms (see below) |
| 0x85 | notice | `u32` code, text in UTF-8: 1 the host reset the plugin (make the parts again), 2 messages were dropped, 3 a part refused an SFZ instrument |

The transport, 68 bytes:

| Bytes  | Field | |
|--------|-------|-|
| 0..8   | `beat` | `f64`, the DAW's position at the start of the latest block |
| 8..16  | `tempo` | `f64`, quarter notes per minute |
| 16..24 | `bar_start` | `f64`, NaN if the DAW does not say |
| 24..32 | `loop_start` | `f64`, NaN without a loop |
| 32..40 | `loop_end` | `f64`, NaN without a loop |
| 40..48 | `age` | `f64`, seconds from that block to this message |
| 48..56 | `block` | `u64`, blocks processed |
| 56..60 | `epoch` | `u32` |
| 60..64 | time signature | `u16` numerator, `u16` denominator; 0 if unknown |
| 64     | flags | 1 playing, 2 recording, 4 looping, 8 the DAW gave the position |
| 65..68 | padding | 0 |

The status, 19 `u64`s then an `f64`: the engine's `EngineStats` (queued,
ignored, dropped, late, applied, unrouted, displaced, stalled, recovered,
limited, cancelled), then the bridge's: events played late at the start of
a block, events too late to play (more than 0.1 s), events of an old
epoch, events cleared by a jump, a loop's end or a panic, events dropped
for want of room, events played as soon as possible, records overridden by
the DAW's tempo; then the master limiter's gain reduction in dB.

### Statuses

0 is success; the negative codes of `ffi::status` mean what they mean for
the browser engine (-3 unknown instrument, -4 unknown chain bit, -5 every
part slot taken, -6 no such part, -8 not a sampler, -9 no load under way,
-10 bad SFZ, -11 bad sample, -12 input too large, -13 the part has
instruments it has not taken). The bridge adds -200 bad message, -201 not
available over the bridge, -202 busy, -203 no session, -204 stale (a later
load took over, or the part changed while it loaded).

Control calls are answered at once, with the status the engine would
give, from what the plugin knows of the session's parts; the part, its
instrument or its stages are made off the audio thread and queued for it,
**in the order of the messages**, so an event sent after a reply reaches
the part after the call. An SFZ finish is answered once the instrument is
decoded, measured for loudness normalisation and queued; the summary is
`{"regions", "samples", "sample_bytes", "warnings", "make_up"}`.

### Queries

| What | Argument | Reply |
|------|----------|-------|
| 1 drum map | | the drum map, JSON |
| 2 parameters | part | the part's parameters, JSON, at their defaults |
| 3 master parameters | | JSON, at their defaults |
| 4 part chain | part | the chain bits as the status, -6 if there is no such part |
| 5 counters | | a status message's body |
| 6 latency | | `u32` latency in frames, `u32` largest block, `f64` how many seconds ahead of the DAW's position to send events |

The parameter descriptions come from a scratch engine with a part of the
same kind and chain: their ranges, units and defaults are right, but the
values are the defaults, not what the session set.

## Timing

The browser engine schedules events by frame: a record lands on the frame
it names, and the engine renders its parts its master bus's look-ahead
(111 frames at 48 kHz) ahead of its output so that it does. In a DAW, the
DAW's transport is the clock, so the bridge schedules **in beats**, and
maps them on the DAW's timeline at the start of every block
(`src/timeline.rs`):

- **The latency.** The plugin reports the master bus's look-ahead as its
  latency, and the DAW compensates it. An event for the beat that falls
  `n` frames into a block is pushed into the engine for frame
  `engine.frame() + lookahead + n`, which the engine's parts render in
  this very block, exactly; the DAW shifts the output back by the
  latency, and the event is heard on its beat. So every event of a block
  lands on its frame with no prediction of where the DAW will be.
- **Rounding.** At the block's tempo, a beat `b` is at frame
  `round((b - block_start_beat) × frames_per_beat)` of the block; an event
  rounding to the next block waits for it. Rounding is done once per
  event, from the block it falls in.
- **Tempo changes** take effect from the block the DAW reports them in;
  a block that starts where the last one ended at either tempo is not a
  jump. `master.tempo` is set on that block's first frame.
- **Loops.** When the DAW loops and a block runs past the loop's end, the
  block is cut there: its first frames play the end of the lap, the rest
  the start of the next. A DAW that cuts its blocks at the loop's end
  gives a block that starts at the loop's start, and that is a wrap too. At
  a wrap every note is let go (a note held across the loop's end would
  never get its note off), the events left for the lap that ended are
  dropped, the **epoch moves on by 1**, and the events the page sent for
  the next lap take their place. `master.beat` is set to the loop's start.
- **Jumps.** A block that does not start where the last one ended, give
  or take a frame, is a jump: the user moved the cursor, or the DAW
  started elsewhere. Every note is let go, every waiting event dropped, and
  the **epoch moves on by 2**, so that events sent for the next lap of a
  loop that will not come are dropped too.
- **Stop.** When the DAW stops, every note is let go; events wait for
  their beat, and play when the DAW plays them. Starting again where it
  stopped is not a jump. Events for "as soon as possible" play at once,
  stopped or not, which is how a page plays while the DAW is stopped.
- **Epochs.** Every events message names the epoch it was planned for.
  The plugin keeps events of its current epoch in one store and those of
  the next epoch (the next lap of a loop) in another, and drops the rest
  as stale. The transport message says the epoch, and the page plans for
  it.
- **Early and late.** An event may be sent any time before its beat: it
  waits in the plugin (32 768 events per lap). One whose beat has already
  passed when it arrives plays at the start of the block it arrives in, if
  it is no more than 0.1 s late, and is dropped otherwise; both are
  counted. A page should send events at least `latency().ahead` seconds
  before the DAW reaches them: two of the DAW's blocks and 30 ms.
- **No position.** A DAW that does not give a beat position is counted
  for: the beat moves on by the frames rendered at the tempo, from 0.

### What the page sees

`bridge.js` keeps the DAW's last transport and when it arrived, and
estimates where the DAW is now: `beat + elapsed × tempo / 60` while it
plays. In `'seconds'` mode a time on the page's clock becomes a beat that
way, at the time it is scheduled, so a page written for the local engine
plays in time with the DAW unchanged; in `'beats'` mode the page gives
beats itself, and lines its bars up with the DAW's (`transport.barStart`,
the time signature). Either way:

- an event due past the loop's end is sent for the next lap (its beat
  moved back by the loop's length, the epoch plus one), so loops go round
  without a gap; one more than a lap ahead is held until the loop wraps;
- after a jump, in `'seconds'` mode, what is still to come is sent again,
  mapped from the new position, for the new epoch; in `'beats'` mode the
  page plans again (`ontransport` says `jumped`);
- while the DAW is stopped, `'seconds'` events are held and sent "as soon
  as possible" when their time comes.

## Sampler data

SFZ instruments travel as bytes: the page fetches the text and the files
itself (they need not be reachable from the plugin) and sends them one
message each. The plugin parses and decodes them on a worker thread, then
measures the instrument for loudness normalisation as
`Engine::load_instrument` does (`DetachedSfz::measured`, the probe that
renders 4.5 s of the instrument's phrase), and hands the measured
instrument to the part on the audio thread without allocating
(`Engine::give_sfz`). The part plays it at the measured make-up from its
first note, where the browser plays it first at an estimate and glides to
the measured level some 28 quanta later. The instruments a part has
finished with go back to the worker thread to be freed.

## The real-time side

The engine's control calls allocate, which a plugin's audio thread must
not. `manguebyte-web` cuts each in two (`src/engine/handoff.rs`): a
`DetachedPart`, `DetachedInstrument`, `DetachedStages` or `DetachedSfz` is
made off the audio thread from the engine's configuration, and
`Engine::attach_part`, `detach_part`, `swap_instrument`, `set_chain_from`
and `give_sfz` hand it over without touching the heap, leaving what the
engine lets go of in the same slot, which goes back to be dropped. The
engine renders exactly what the control call would have made it render
(`crates/manguebyte-web/tests/handoff.rs`). In the plugin:

- the server thread speaks WebSocket and makes the slots; a worker thread
  loads SFZ instruments;
- one lock-free queue (`manguebyte_core::spsc`) carries everything to the
  audio thread, in the order the page sent it; a second carries the slots
  and the finished instruments back;
- the audio thread keeps the events by beat in two fixed-capacity heaps
  and publishes the transport and the counters through a sequence lock of
  atomics, which never makes it wait;
- `plugins/aratu-bridge/tests/realtime.rs` watches every `process` call
  with a counting allocator while a page sends everything it can and the
  host loops, jumps, stops and changes its tempo.

## Integration with Aratu

The Aratu side is Aratu's to write; what it needs:

```js
import { ManguebyteEngine } from './engine.js';
import { AratuBridge } from './bridge.js';

// "Play here" or "play in the DAW", with the same calls afterwards.
const engine = inDaw
  ? await AratuBridge.connect({ port: 47870 })            // timeBase 'seconds'
  : await ManguebyteEngine.create(context, { wasm: 'manguebyte_web.wasm' });

await engine.createPart(0, 'drums');
engine.noteOn(0, 36, 1, engine.context.currentTime + 0.1);
```

- Schedule from `engine.context.currentTime`, which for the bridge is a
  clock of the page mapped onto the DAW's position, rather than from an
  `AudioContext` of its own.
- To follow the DAW's bars and tempo, use `timeBase: 'beats'` (or, in
  `'seconds'` mode, `timeAtBeat(beat)`) and read `transport` (`tempo`,
  `beat`, `barStart`, `timeSignature`, `loopStart`, `loopEnd`, `playing`)
  or `ontransport`. Aratu's own tempo setting should give way to the
  DAW's while it plays in the DAW.
- Show `state` ('connecting', 'open', 'closed', 'error', and `onstate`)
  and offer to reconnect; after `onnotice` code 1, make the parts again.
- `loadSfz(part, url)` works as it does locally; `paramValue`,
  `activeVoices`, `effectCurve`, `effectRunning`, `partScope` and
  `partLevels` reject over the bridge, and `params()` gives defaults.

## Limits

They are listed in [TECH_DEBT.md](TECH_DEBT.md), item 84: one stereo
output (no output bus per part group yet), no editor, one page per plugin
instance, the parameter descriptions at their defaults, and the browser
questions above. It has been tried by hand once, as a CLAP in REAPER 7.82
on Windows with a Chromium page on `http://127.0.0.1`
([HOST_TESTS.md](HOST_TESTS.md)); other DAWs, the VST 3 and AU builds and
other browsers have not been tried.
