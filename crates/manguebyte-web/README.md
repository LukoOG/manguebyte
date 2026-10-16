# manguebyte-web

A Manguebyte engine for the browser, that also builds, runs and is tested
natively. The engine holds numbered parts; each plays the eight-voice synth
or the drum kit through an optional low-pass filter, an optional reverb,
then gain and pan. Events are 32-byte records that land on the exact frame
they name, and the output is the same, bit for bit, whatever the size of
the quanta it is rendered in.

Built for `wasm32-unknown-unknown`, the crate is a WebAssembly module with
plain `extern "C"` exports (`src/ffi.rs`) and no imports. `js/worklet.js` is
an `AudioWorkletProcessor` that instantiates it, `js/engine.js` the
main-thread helper, and `js/ring.js` the shared event ring both use.
`examples/web/` is a demo page with a small server; its README says how to
run it.

```sh
cargo build -p manguebyte-web --target wasm32-unknown-unknown
node --test "crates/manguebyte-web/js/tests/*.test.mjs"   # Node.js 21 or later
cargo test -p manguebyte-web                               # the native tests
```

The crate documentation (`cargo doc -p manguebyte-web --open`) describes
the engine, its parameters and the threads in full.

## The contract with Aratu

[Aratu](https://github.com/alessonqueirozdev-hub/aratu) writes records and
reads the drum map, so these are fixed: a change to any of them is a change
to `ffi::ABI_VERSION` (now 2) and must be agreed with Aratu.
`tests/record.rs`, `tests/ring.rs` and `js/tests/ring.test.mjs` pin them,
the last two against the same golden bytes.

### Event record

32 bytes, every multi-byte field little endian:

| Bytes  | Field      | Type | Meaning |
|--------|------------|------|---------|
| 0..8   | `frame`    | u64  | the frame the event lands on, counted from the engine's first |
| 8      | `kind`     | u8   | 1 note on, 2 note off, 3 param, 4 choke, 5 all notes off |
| 9      | padding    |      | written as 0, never read |
| 10..12 | `part`     | u16  | the part the event is for |
| 12     | `note`     | u8   | MIDI note, 0 to 127 (note on and note off) |
| 13     | `channel`  | u8   | MIDI channel, 0 to 15 (note on and note off) |
| 14..16 | padding    |      | written as 0, never read |
| 16..20 | `velocity` | f32  | 0 to 1 (note on); 0 is a note off |
| 20..24 | `param`    | u32  | the parameter: the 32-bit FNV-1a hash of its identifier, such as `"lowpass.cutoff"` (param) |
| 24..32 | `value`    | f64  | the parameter's new value in its own units, finite (param) |

Choke stops every note of the part at once; all notes off releases them.
Code 0 and codes above 5 are not kinds. A record with an unknown kind, or
with a field its kind uses out of range, is ignored and counted; fields a
kind does not use may hold anything. An event whose frame has passed lands
on the first frame of the quantum being rendered, and one further ahead
waits.

### Shared ring

A ring of capacity `C` (a power of two) is `16 + 32·C` bytes:

| Bytes      | Contents |
|------------|----------|
| 0..4       | `head`: records ever taken out, u32, written only by the consumer |
| 4..8       | `tail`: records ever put in, u32, written only by the producer |
| 8..12      | the capacity `C`, u32 |
| 12..16     | the ASCII text `MBR1` |
| 16..16+32C | `C` slots, each one record |

The counters wrap at 2³²; record `n` lives in slot `n mod C`. The producer
writes a slot and then stores `tail`; the consumer loads `tail`, reads, and
then stores `head` (with `Atomics.store` and `Atomics.load` in JavaScript).
A page has `SharedArrayBuffer` only when it is cross-origin isolated
(`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`); without it, `js/engine.js`
sends the records in messages.

### Drum map

| Drum | Name in the JSON | General MIDI note | Aratu symbol |
|------|------------------|-------------------|--------------|
| kick | `kick` | 36 | `b` |
| snare | `snare` | 38 | `s` |
| closed hi-hat | `closed_hat` | 42 | `c` |
| open hi-hat | `open_hat` | 46 | `o` |
| crash | `crash` | 49 | `x` |
| ride | `ride` | 51 | `r` |

The export `mb_drum_map_json` (and `drum_map_json` natively) gives it as
`[{"name":"kick","note":36,"symbol":"b"},...]`. A closed hat chokes a
sounding open hat, with a 2 ms fade.

## Licence

MIT OR Apache-2.0, like the rest of Manguebyte.
