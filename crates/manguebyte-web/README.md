# manguebyte-web

A Manguebyte engine for the browser, that also builds, runs and is tested
natively. The engine holds numbered parts; each plays the eight-voice
synth, the drum kit or an SFZ instrument through an optional low-pass
filter, an optional reverb, then gain and pan. Events are 32-byte records
that land on the exact frame they name, and the output is the same, bit
for bit, whatever the size of the quanta it is rendered in.

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

## Sampler parts

A sampler part (`InstrumentKind::Sampler`, code 2, `'sampler'` in
`js/engine.js`) plays an SFZ instrument through `manguebyte-sampler`. It
is silent until it has one:

```js
await engine.createPart(4, 'sampler');
const { regions, warnings } = await engine.loadSfz(4, 'samples/piano.grand/voice.sfz');
engine.noteOn(4, 60, 0.8, context.currentTime + 0.05);
```

`loadSfz` resolves once the part plays the instrument: a note scheduled
after that uses it, while the notes of the instrument it replaces ring on
with that one to their end. It rejects, naming the file, if a fetch fails
or a file is not valid SFZ, WAV, AIFF or FLAC, and the part keeps what it
had. Natively, `SfzLoad` does the same work and `Engine::load_instrument`
installs the result.

- A sample plays at its own rate, read at the ratio of that rate to the
  context's: a 24 kHz sample in a 48 kHz context is read at half speed,
  so nothing is resampled while loading. The reading between frames is
  cubic, or a windowed sinc with `sampler.interpolation` set to 1 (cleaner
  far up the spectrum, about 3.6 times the cost; see below).
- A sample that peaks at full scale peaks at -6 dB (`SAMPLER_LEVEL_DB`) at
  full velocity, which follows the SFZ square law (`amp_veltrack=100`).
  An accented note is 3 dB louder (`SAMPLER_ACCENT_DB`), and only that
  note: an earlier one on the same key, still ringing, is not.
- Note off releases a note as its region says; one-shots ignore it. Choke
  stops every voice after one more frame. All notes off releases every
  voice, but never in less than 8 ms (`PANIC_FADE_SECONDS`): a one-shot,
  or a voice whose `ampeg_release` is shorter, fades out in a straight
  line over 8 ms instead, so it never clicks.
- `loop_mode=loop_continuous` with `loop_start` and `loop_end` loops
  through the interpolator, so the seam adds nothing: a loop of whole
  periods meets itself with no step larger than the wave's own
  (`tests/sampler.rs` measures 0.023080 at the seams against 0.023091
  before the first). `group` and `off_by` choke: the open hat
  (`group=1 off_by=2`) fades out over 6 ms when the closed hat (`group=2`)
  is struck.
- 32 voices (`SAMPLER_VOICES`), a voice per region sounding, and a quarter
  as many again for voices fading out after being cut short.

### Sampler kits

A sampler kit (`InstrumentKind::SamplerKit`, code 3, `'sampler_kit'`) is
a sampler part marked as a drum kit. For a note of the drum map that its
SFZ text has no region for, such as the guitar tap (88) of Aratu's kit, it
plays the synthesized drum of a `drums` part instead: at the note's
velocity, with its accent, through the part's chain, gain and pan. Until
it has an instrument it plays every drum that way, so a kit sounds while
its samples load. It has the kit's parameters (`kit.tune`, `kit.decay`,
`guitar_tap.level` and the rest) for those drums, beside
`sampler.interpolation`. Choke stops both halves; all notes off releases
the samples as above and fades the synthesized drums over 50 ms
(`DAMP_SECONDS`). A closed hat chokes an open hat of its own half: a
sampled one through the SFZ text's `group` and `off_by`, a synthesized one
as the drum kit does.

The fallback is chosen when the part is made, not decided from the notes.
Done for every sampler part, a melodic instrument whose regions leave a
gap, say a glockenspiel whose lowest key is 79, would answer a low note
with a kick drum; a part marked as a kit makes the intent plain, and has
the kit's parameters to control the drums it borrows. What counts as
missing is a key no region with an `attack`, `first` or `legato` trigger
covers (`Instrument::covers_key`), whatever its velocities, so a kit with a
region for a key at some velocities only plays nothing at the others.

Every opcode Aratu's instruments use is played: `sample`, `default_path`,
`lokey`, `hikey`, `key`, `pitch_keycenter`, `loop_mode`, `loop_start`,
`loop_end`, `ampeg_release`, `group` and `off_by`, with `<control>`,
`<global>`, `<group>` and `<region>`; `tests/aratu.rs` loads all fifteen
voices with no warning and plays every region when Aratu's samples are on
the machine (set `ARATU_SAMPLES` to their folder). The text cannot
`#include` other files.

### How an instrument is loaded

The page fetches `voice.sfz` and hands it to the worklet, which parses it
at once and answers with the sample files it names; the page fetches them,
relative to the SFZ file, and transfers each to the worklet as it arrives.
Decoding is the costly part, so the worklet keeps the files in a queue and,
after each render quantum, decodes up to 64 KiB of them
(`LOAD_BYTES_PER_QUANTUM`, always at least one file). When the last is in,
it makes the instrument and hands it to the part through the sampler's
lock-free single-producer single-consumer loader, which the sampler reads
when it next renders; the instrument it replaces goes back the same way
and is freed by `mb_collect`, which the worklet calls between quanta. The
render call never allocates, frees or waits.

The alternative was to decode on the main thread, in a second instance of
the module, and send the finished instrument over. But a worklet's
WebAssembly memory is its own: the decoded samples would have to be copied
into it all the same, on the audio thread, and copying 32-bit samples
costs about as much as decoding 16-bit ones, so the audio thread would do
the same work, twice the bytes would cross between threads, and the
instrument would need a format of its own to cross in. Decoding in place,
a step at a time, keeps one copy and one code path, and its price is that
the audio thread allocates between quanta (the module's allocator is
single-threaded and never locks) and that a load takes a quantum per file:
Aratu's grand piano, 20 files, is in after 20 quanta, 53 ms at 48 kHz.
While the context is not running, process() is not called, so the page
marks its messages and the worklet decodes everything at once.

In a release build, on a busy laptop in Node.js (not an AudioWorklet),
decoding Aratu's largest file (192 KB, 96 000 frames) took 0.4 to 0.9 ms,
parsing an SFZ file and finishing an instrument a few hundredths of a
millisecond, against 2.7 ms for a quantum at 48 kHz. Making the first
engine makes the sinc table every instrument shares, about 11 ms, once.
Eight piano voices took 0.09 ms a quantum with cubic interpolation and
0.32 ms with the sinc; 32 voices 0.21 ms with cubic.

## Licence

MIT OR Apache-2.0, like the rest of Manguebyte.
