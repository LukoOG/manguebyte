# manguebyte-web

A Manguebyte engine for the browser, that also builds, runs and is tested
natively. The engine holds numbered parts; each plays the eight-voice
synth, the drum kit or an SFZ instrument through a chain of
[effects](#effects), then gain and pan, into a [master bus](#the-master-bus)
with a shared reverb, a glue compressor and a true-peak limiter. Events
are 32-byte records that land on the exact frame they name, and the output
is the same, bit for bit, whatever the size of the quanta it is rendered
in.

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
| 8      | `kind`     | u8   | 1 note on, 2 note off, 3 param, 4 choke, 5 all notes off, 6 cancel |
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

Choke stops every note of the part at once; all notes off releases them,
never with a click (see [Playing live](#playing-live)); cancel drops the
part's events due on its frame or later that came before it, and fades its
notes out there. Code 0 and codes above 6 are not kinds. A record with
an unknown kind, or with a field its kind uses out of range, is ignored
and counted; fields a kind does not use may hold anything. An event whose
frame has passed lands on the first frame of the quantum being rendered,
and one further ahead waits.

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

## Playing live

**Cancel** (kind 6, `engine.cancel(part, time)`, `EventRecord::cancel`).
When the engine takes the record, every event of its part due on its
frame or later that reached the engine before it is dropped (the
`cancelled` counter, `mb_stat` 10, counts them); events sent after it are
kept. On its frame every note of the part still sounding fades out in a
straight line over 8 ms (`CANCEL_FADE_SECONDS`), on every kind of part,
while a note started on that frame after the cancel plays in full. So when
the performer edits a part in the middle of a bar, Aratu sends a cancel
for the first frame it can still reach, then the new events:

```js
const from = context.currentTime + (await engine.latency()).engineFrames / context.sampleRate;
engine.cancel(part, from);
for (const note of newNotes) engine.noteOn(part, note.key, note.velocity, note.time);
```

A full queue takes a cancel all the same, since what it drops matters more
than its fade, and it only makes room. The kind was a code every engine
ignored, and no writer sent it, so the ABI version stays 2: an older
engine ignores and counts it.

**All notes off never clicks.** It lets every note go with a fade: the
synth's release (5 ms at the shortest, falling 60 dB), the drums' 50 ms
(`DAMP_SECONDS`), a sampler's own releases but never less than 8 ms
(`PANIC_FADE_SECONDS`: one-shots and shorter releases fade in a straight
line instead). `tests/stage.rs` stops a loud, sustained sound of every kind
of part (a four-note chord, ringing cymbals and toms, sampled chords
looping, a sampled hat with synthesized drums) on its loudest frame and
checks that no step from one sample to the next after it is larger than
the largest step before it by more than 2% of its peak; none is larger at
all, while a choke, which cuts, adds 73% (synth) and 92% (sampler).

**Latency.** `mb_latency_frames` (`Engine::latency_frames`) is how far
ahead of the engine's current frame an event must be due to be sure of
landing on its frame: one render quantum, the size of the last render
(the maximum quantum before the first), since the quantum being rendered
may have begun when the event arrives, and the master bus's look-ahead
(`Engine::master_latency_frames`, see [The master bus](#the-master-bus)):
111 frames at 48 kHz, 2.3 ms, so 239 frames in a browser. The engine
renders its parts that far ahead of the output, so an event scheduled
that far ahead is still heard on its own frame, exactly as before; one
that comes later lands on the first frame the engine renders next, which
is heard 111 frames into the next quantum. Every event pushed before the
first render lands on its frame. `await engine.latency()` gives
`{ engineFrames, outputFrames, frames, seconds }`: the engine's frames,
the context's own output latency (`baseLatency` plus `outputLatency`,
where the browser gives them), and the two together, for Aratu to show
and to schedule ahead by. Records sent by message, without shared memory,
also take the time a message takes to cross, which no browser reports.

## Effects

Every part has a chain of effects, which run in a fixed order:

```text
high-pass → low-pass → EQ → drive → bitcrush → compressor
  → chorus → flanger → phaser → tremolo → auto-pan → delay → reverb
  → gain and pan → send to the master bus's reverb
```

Tone first, so that what is driven and compressed is the sound as it was
meant; dynamics before modulation, so that the compressor does not pump
with a sweep; time last, so that each echo and each reflection carries the
colour of everything before it. The order does not depend on the order of
a method chain: `.delay(0.25).chorus()` and `.chorus().delay(0.25)` sound
the same.

An effect is in a part's chain or not (the chain's bits, `1 << code`), and
in the chain it is switched on or off by its `<effect>.on` parameter. Being
in the chain is what gives it memory and parameters: a control call puts
it there (`setChain`, or `setEffect` the first time), which is where the
memory is allocated (a delay's four seconds of it most of all). The switch
is an ordinary parameter, so it travels in a record and lands on its exact
frame, with the settings sent for the same frame before it. An effect not
in the chain, or switched off and done fading, costs nothing: it is not
run at all. The low-pass and the reverb start switched on, as they always
did; every other effect starts off.

Switching never clicks. An effect fades in and out over 10 ms
(`EFFECT_FADE_SECONDS`), cross-faded with the signal that goes into it; the
delay and the reverb fade their input instead, so their echoes and tails
ring on after they are switched off, then fall asleep. A fade only matters
while sound passes: on a frame that follows a silent one the switch goes
straight to its end, so an effect switched on together with a note applies
in full from the note's first frame. Every setting glides: levels and
mixes over 20 ms in a straight line, frequencies over 20 ms exponentially,
the chorus's, flanger's and phaser's delays and depths over 50 ms, a delay
time by cross-fading two read heads over 20 ms (`DELAY_GLIDE_SECONDS`),
which neither clicks nor bends the pitch of the echoes. A setting sent
while its effect is off applies at once. `tests/effects.rs` switches every
effect in, changes it hard and switches it out under a steady tone: no
step from one sample to the next is larger than the largest step of the
steady sound around it by more than 2% of the tone's peak.

The signal processing is `manguebyte-dsp`'s: `StateVariableFilter` for the
filters, `Biquad` designed by `BiquadCoefficients` (Audio EQ Cookbook) for
the equaliser, `ShaperCurve` for the drive, `Compressor`, `Chorus` for the
chorus and the flanger, `Phaser`, `Lfo` for the tremolo and the auto-pan,
`DelayLine` for the delay, `Reverb`, `Limiter`. The bitcrusher, the gain
stages of the tremolo and auto-pan, and the delay's heads, damping and
ping-pong routing are this crate's.

### Every effect and parameter

Identifiers are `"<effect>.<parameter>"`; each effect also has
`"<effect>.on"`, 0 or 1. The parameters are listed in the order positional
settings fill them (`EFFECT_PARAMS` in `js/engine.js`): `.delay(0.25, 0.5)`
is a time of 0.25 s and a feedback of 0.5. "log" is a control that moves
evenly in ratios; the parameter JSON gives every scale exactly.

| Effect, `visual` | Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|---|
| `highpass`, `filter_curve` | `cutoff` | Hz | 20 to 20 000 | 200 | log |
| | `resonance` | | 0 to 1 | 0 | linear |
| `lowpass`, `filter_curve` | `cutoff` | Hz | 20 to 20 000 | 20 000 | log |
| | `resonance` | | 0 to 1 | 0 | linear |
| `eq`, `eq_curve` | `low_gain`, `mid_gain`, `high_gain` | dB | -24 to 24 | 0 | linear |
| | `low_freq` (low shelf) | Hz | 20 to 1 000 | 120 | log |
| | `mid_freq` (peak) | Hz | 100 to 10 000 | 1 000 | log |
| | `mid_q` | | 0.1 to 10 | 0.7 | log |
| | `high_freq` (high shelf) | Hz | 1 000 to 20 000 | 8 000 | log |
| `drive`, `transfer_curve` | `amount` | dB | 0 to 36 | 12 | linear |
| | `output` | dB | -24 to 12 | -6 | linear |
| | `mix` | | 0 to 1 | 1 | linear |
| | `shape` | | 0 tanh, 1 soft clip, 2 hard clip, 3 fold | 0 | choice |
| `bitcrush`, `bitcrush_steps` | `bits` | bits | 1 to 16 | 8 | linear |
| | `rate` | Hz | 100 to 48 000 | 48 000 | log |
| | `mix` | | 0 to 1 | 1 | linear |
| `compressor`, `transfer_curve` | `threshold` | dB | -60 to 0 | -18 | linear |
| | `ratio` | | 1 to 20 | 4 | log |
| | `attack` | ms | 0.1 to 200 | 10 | log |
| | `release` | ms | 5 to 2 000 | 120 | log |
| | `knee` | dB | 0 to 24 | 6 | linear |
| | `makeup` | dB | 0 to 24 | 0 | linear |
| | `mix` (parallel) | | 0 to 1 | 1 | linear |
| `chorus`, `lfo_shape` | `mix` | | 0 to 1 | 0.5 | linear |
| | `rate` | Hz | 0.05 to 10 | 0.8 | log |
| | `depth` | ms | 0 to 10 | 2.5 | linear |
| | `delay` | ms | 1 to 40 | 15 | linear |
| | `voices` | | 1 to 8 | 3 | whole |
| | `spread` | | 0 to 1 | 1 | linear |
| `flanger`, `lfo_shape` | `mix` | | 0 to 1 | 0.5 | linear |
| | `rate` | Hz | 0.02 to 10 | 0.25 | log |
| | `depth` | ms | 0 to 5 | 1.5 | linear |
| | `feedback` | | -0.95 to 0.95 | 0.7 | linear |
| | `delay` | ms | 0.5 to 10 | 2 | linear |
| `phaser`, `lfo_shape` | `mix` | | 0 to 1 | 0.5 | linear |
| | `rate` | Hz | 0.02 to 10 | 0.5 | log |
| | `depth` | oct | 0 to 6 | 2 | linear |
| | `center` | Hz | 100 to 10 000 | 1 000 | log |
| | `feedback` | | -0.95 to 0.95 | 0.3 | linear |
| | `stages` | | 2 to 12 | 4 | whole |
| | `spread` | | 0 to 1 | 0 | linear |
| `tremolo`, `lfo_shape` | `rate` | Hz | 0.1 to 30 | 5 | log |
| | `depth` | | 0 to 1 | 0.5 | linear |
| | `shape` | | 0 sine, 1 triangle, 2 square, 3 saw up, 4 saw down | 0 | choice |
| | `sync` | beats | 0 to 16, 0 is off | 0 | linear |
| `autopan`, `lfo_shape` | `rate` | Hz | 0.05 to 20 | 0.5 | log |
| | `depth` | | 0 to 1 | 1 | linear |
| | `shape` | | as the tremolo's | 0 | choice |
| | `sync` | beats | 0 to 16, 0 is off | 0 | linear |
| `delay`, `delay_taps` | `time` | s | 0.001 to 4 | 0.375 | skew 0.35 |
| | `feedback` | | 0 to 0.95 | 0.35 | linear |
| | `mix` | | 0 to 1 | 0.3 | linear |
| | `damping` | | 0 to 1 | 0.3 | linear |
| | `pingpong` | | 0 to 1 | 0 | linear |
| | `sync` | beats | 0 to 16, 0 is off | 0 | linear |
| `reverb`, `reverb_decay` | `mix` | | 0 to 1 | 0.15 | linear |
| | `decay` | s | 0.2 to 10 | 1.6 | skew 0.35 |
| (every part) | `send` | | 0 to 1 | 0 | linear |

What they do, in short:

- **Filters**: 12 dB per octave, resonance from a Q of 0.71 (flat) to 11.3.
- **EQ**: a low shelf and a high shelf of Q 0.71 (the steepest without a
  bump) and a peak between, each a cookbook biquad in double precision.
  A shelf is half its gain at its corner frequency.
- **Drive**: gain into the curve, gain out, mixed with the clean signal; a
  change of curve cross-fades. The curves are odd, so no offset appears;
  the drive is not oversampled (its aliasing is that of the curve).
- **Bitcrush**: holds the signal for whole periods of `rate` (a
  sample-and-hold, aliasing on purpose) and rounds it to steps of
  `2^(1-bits)`, half-way away from zero: the staircase is odd, so there is
  no offset and silence stays exact silence, and nothing subnormal is
  left. Fractional bits give the step sizes in between.
- **Compressor**: feed-forward, level across both sides, gain computed and
  smoothed in decibels; `mix` below 1 is parallel compression.
- **Chorus, flanger**: one to eight voices reading a delay line by cubic
  interpolation, swept by a sine; the flanger is one voice with feedback.
- **Phaser**: 2 to 12 first-order all-pass stages swept on a musical
  scale; one notch per two stages.
- **Tremolo**: gain from `1 - depth` at the trough of its wave to 1 at the
  crest. **Auto-pan**: the part's place swung `depth` either side of the
  centre, at constant power. Both follow their wave through a 1 ms
  smoother, so the square and saw waves do not click. With `sync` above 0
  the wave takes `sync` beats per cycle at `master.tempo`, a cycle
  starting on beat 0 of `master.beat`.
- **Delay**: a line per side, read at a whole number of frames; feedback
  through a one-pole low-pass (`damping` 0 is none, then a corner falling
  from 20 kHz to 500 Hz); `pingpong` 1 sends the input to the left line and
  crosses the feedback, so the echoes alternate left and right. With
  `sync` above 0 the time is `sync` beats at `master.tempo`, and follows
  it. Times are limited to 4 s (`DELAY_MAX_SECONDS`).
- **Reverb**: the reverb parts always had (an eight-line feedback delay
  network), sleeping while nothing reaches it.

### Curves

Each effect names in the JSON what an editor draws for it, `"visual"`.
`Engine::effect_curve(part, effect, points)` and the export
`mb_effect_curve(engine, part, effect, points)` work the curve out from
the values the parameters hold, outside the render call (the worklet
handles the request between quanta), never from what the render call is
working on; `mb_curve_ptr` then points at `x, y` pairs of `f32`. In
JavaScript, `await engine.effectCurve(part, 'eq', 256)` gives
`{ visual, x, y }`. The axes:

| `visual` | `x` | `y` |
|---|---|---|
| `filter_curve`, `eq_curve` | frequency, Hz, evenly spaced in octaves from 20 Hz to 20 kHz | gain, dB |
| `transfer_curve` | input level, dBFS, -60 to 6 (compressor, glue, limiter); input sample, -1 to 1 (drive) | output level, dBFS; output sample |
| `lfo_shape` | place in one cycle, 0 to 1 | delay, ms (chorus, flanger); frequency, Hz (phaser); gain (tremolo); place, -1 to 1 (auto-pan) |
| `delay_taps` | time after a sound, s | gain of the echo heard then, the louder side, until -60 dB |
| `reverb_decay` | time, s, to 1.5 times the decay | level of the tail, dB |
| `bitcrush_steps` | input sample, -1 to 1 | output sample |

They come from the formulas the effects run: the filters' responses from
their biquad coefficients (`BiquadCoefficients::magnitude_at`; the
state-variable filters are the same bilinear transforms), the compressor's
static curve (`Compressor::static_reduction_db`), one cycle of an `Lfo`.
`tests/effects.rs` checks them against what is measured through the
engine: the equaliser's and the filters' curves within 0.1 dB, the
compressor's within 0.1 dB, the delay's taps exactly. They live in
`src/curve.rs`, to be replaced by `manguebyte-dsp`'s own response curves
when it has them: one match arm per effect in `part_curve` and
`master_curve`.

## The master bus

The parts, added up, and the return of a shared reverb go through:

```text
parts + shared reverb → glue compressor → master.gain → true-peak limiter → output
```

- **The shared reverb** takes what each part sends (`send`, after gain and
  pan) and returns it wet. It sleeps, costing nothing, while nothing is
  sent. `master.reverb.level` (dB, -60 to 6, 0), `.decay` (s, 0.2 to 10,
  2.2), `.size` (0 to 1, 0.7), `.damping` (0 to 1, 0.5).
- **The glue compressor**, `manguebyte-dsp`'s `Compressor`, gentle by
  default and off until `master.glue.on`: `.threshold` (dB, -40 to 0,
  -12), `.ratio` (1 to 10, 2), `.attack` (ms, 0.1 to 100, 20), `.release`
  (ms, 10 to 2 000, 250), `.knee` (dB, 0 to 24, 6), `.makeup` (dB, 0 to
  18, 0). It fades in and out like a part's effects.
- **`master.gain`** (dB, -60 to 12, 0): the level into the limiter.
- **The limiter**, `manguebyte-dsp`'s `Limiter`, looking 2 ms ahead
  (`MASTER_LOOKAHEAD_SECONDS`), keeps the waveform under
  `master.limiter.ceiling` (dBTP, -12 to -0.3, -0.3) and every sample
  within `OUTPUT_CEILING`, so that a live set never clips;
  `master.limiter.release` (ms, 10 to 1 000, 150). It is true-peak aware:
  beside the audio it is fed a key channel, the largest magnitude of the
  waveform between each pair of samples, read by interpolating by eight
  with a 257-tap windowed sinc (as ITU-R BS.1770-4 measures true peak), so
  that it turns the level down for the peaks between samples too. Below
  the ceiling it leaves the sound exactly as it is. `tests/master.rs` plays
  a set of four loud parts, driven and with their highs boosted 12 dB, at
  ceilings of -0.3, -1 and -6 dBTP: the samples peak at the ceiling, and
  a true-peak meter of `manguebyte-analysis`, sixteen times oversampled,
  reads the waveform at most 0.08 dB above it (the filter rolls off above
  0.44 of the sample rate, and eight points a sample can miss a crest by
  up to 0.17 dB at half of it). The default ceiling is
  the level the engine always kept its samples under; a set streamed
  through a lossy codec wants -1, as EBU R 128 asks.
- **`master.tempo`** (bpm, 20 to 400, 120) and **`master.beat`** (the beat
  at the frame it is set on; the engine counts on from it) are what synced
  effects follow.

A master parameter's record may name any part: it belongs to no part, and
a part's cancel leaves it. `Engine::master_params_json` and the export
`mb_master_params_json` describe them, in the format of a part's.

**Latency.** Reading between samples takes 16 frames (`TRUE_PEAK_DELAY`)
and the limiter's look-ahead 2 ms less a frame, so the bus delays the
sound by `master_latency_frames(rate)`: 111 frames at 48 kHz (2.31 ms), 103
at 44.1 kHz. The engine renders its parts that far ahead of the output,
which keeps every event on its own frame, and counts it in
`latency_frames`. The bus comes to rest after 200 ms of digital silence
beyond that, and is skipped until sound returns.

## In JavaScript

```js
import { ManguebyteEngine, EFFECT_PARAMS } from './engine.js';

await engine.createPart(0, 'synth', 'none');
// .delay(0.25, 0.5).chorus() on the next bar
await engine.setEffects(0, { delay: [0.25, 0.5], chorus: [] }, nextBar);
// one effect, by name, by position or by its first parameter
await engine.setEffect(0, 'eq', { low_gain: 3, high_gain: -2 }, nextBar);
await engine.setEffect(0, 'tremolo', [4, 1, 'square'], nextBar);
await engine.setEffect(0, 'lowpass', 800, nextBar);
engine.removeEffect(0, 'chorus', nextBar);
engine.setSend(0, 0.3, nextBar);
engine.setMaster({ tempo: 128, glue: true, limiter: { ceiling: -1 } }, nextBar);
const { visual, x, y } = await engine.effectCurve(0, 'eq', 256);
```

`setEffect(part, name, settings, time)` puts the effect in the part's
chain the first time (the promise resolves once that is done and the
records are scheduled, so a `time` at least `latency()` ahead still lands
on its frame), then schedules the settings and the switch for `time`.
Settings are an object by name, an array in `EFFECT_PARAMS` order, a
number for the first parameter, `{}` for the effect as it is set, or
`false` to switch it off; a `shape` may be a name (`SHAPES`).
`setEffects(part, effects, time)` makes `effects` the part's effects from
`time`: the ones it names on, every other one in the chain off, the
low-pass and the reverb included; that is the shape of a method chain
once it is evaluated. `removeEffect`, `setSend`, `setMaster`,
`effectCurve`, `effectRunning`, `masterParams`, `masterParamValue`,
`partChain` and `limiterReduction` complete it; `EFFECT`, `CHAIN`,
`EFFECT_ORDER`, `EFFECT_PARAMS`, `VISUAL` and `SHAPES` name everything.

## What did not change in the contract

The record, the ring and the drum map are as they were, and
`ffi::ABI_VERSION` stays 2: everything here is added beside what was
there. Chain bits 2 to 12 are new (an older engine refuses them with
`BAD_CHAIN`); the effects' and the master's parameters are new ids (an
older engine counts their records as unrouted); the exports
`mb_part_chain`, `mb_master_params_json`, `mb_master_param_value`,
`mb_effect_curve`, `mb_curve_ptr`, `mb_effect_running` and
`mb_limiter_reduction_db` and the status -14 are new, and `js/worklet.js`
looks for them by name. A part's description lists more parameters (each
effect's switch, `send`) and gives an effect's parameters two more fields,
`effect` and `visual`; a reader that looks parameters up by id reads it as
before. `mb_latency_frames` is larger by the master's look-ahead, which is
what it is for: an event scheduled that far ahead lands on its frame as
before, and only an event that arrives late lands later than it did.

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
