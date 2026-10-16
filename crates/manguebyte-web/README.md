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
reads the drum map, so these are fixed: a change to any of them that an
existing reader or writer would get wrong is a change to `ffi::ABI_VERSION`
(now 2) and must be agreed with Aratu. The flags byte was a compatible
change: it was padding written as 0, which still means no flag, and an
engine from before it ignores it.
`tests/record.rs`, `tests/ring.rs` and `js/tests/ring.test.mjs` pin them,
the last two against the same golden bytes.

### Event record

32 bytes, every multi-byte field little endian:

| Bytes  | Field      | Type | Meaning |
|--------|------------|------|---------|
| 0..8   | `frame`    | u64  | the frame the event lands on, counted from the engine's first |
| 8      | `kind`     | u8   | 1 note on, 2 note off, 3 param, 4 choke, 5 all notes off |
| 9      | `flags`    | u8   | bit 0 accent (note on); bits 1 to 7 reserved: written as 0, ignored |
| 10..12 | `part`     | u16  | the part the event is for |
| 12     | `note`     | u8   | MIDI note, 0 to 127 (note on and note off) |
| 13     | `channel`  | u8   | MIDI channel, 0 to 15 (note on and note off) |
| 14..16 | padding    |      | written as 0, never read |
| 16..20 | `velocity` | f32  | 0 to 1 (note on); 0 is a note off |
| 20..24 | `param`    | u32  | the parameter: the 32-bit FNV-1a hash of its identifier, such as `"lowpass.cutoff"` (param) |
| 24..32 | `value`    | f64  | the parameter's new value in its own units, finite (param) |

An accented note on (flag bit 0, a note Aratu writes in capitals) plays
louder and brighter: a synth note 3.5 dB louder, with its filter envelope
opening one octave further and an attack twice as fast; a drum 3 dB
louder, with its high frequencies lifted over the first 30 ms, most at the
transient. The crate documentation gives the details.

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
| high tom | `high_tom` | 50 | `t` |
| mid tom | `mid_tom` | 47 | `m` |
| floor tom | `floor_tom` | 43 | `f` |
| side stick | `side_stick` | 37 | `k` |
| guitar body tap | `guitar_tap` | 88 | none (`null`) |

The export `mb_drum_map_json` (and `drum_map_json` natively) gives it as
`[{"name":"kick","note":36,"symbol":"b"},...]`, `symbol` a one-letter
string or `null`. A closed hat chokes a sounding open hat, with a 2 ms
fade.

The toms are tuned membranes, at 165, 123 and 84 Hz before `kit.tune`,
that start about a third sharp and settle within a quarter of a second,
with the stick's noise; they ring for about 0.5, 0.6 and 0.8 s (to -60 dB,
times `kit.decay`). The side stick is a woody knock of a few resonances
between 330 Hz and 3 kHz on a sharp click, gone within 80 ms. The guitar
tap is the thump of a guitar's body, its air resonance at 100 Hz and its
top at 205 and 240 Hz, for about 0.3 s, under the click of a nail. Note 88
is not a General MIDI drum and has no symbol in a drum line, so the JSON
gives `null` for it; a part strikes it by its note. The first six rows
keep their places; new drums are added after them.

`cargo run -p manguebyte-web --example drum_wavs -- <directory>` writes
every drum, plain and accented, and a short groove to WAV files.

## Licence

MIT OR Apache-2.0, like the rest of Manguebyte.
