# manguebyte-web

A Manguebyte engine for the browser, that also builds, runs and is tested
natively. The engine holds numbered parts; each plays an
[instrument](#instruments) (the eight-voice synth, the drum kit, an SFZ
instrument, or one of `manguebyte-dsp`'s: plucked strings, FM, a tonewheel
organ through its rotating speaker, an 808-style drum machine, the 808's
kick bass, an acid bass, drum kits from an acoustic one to the 808, 909,
606 and CR-78 and kits for trap, techno, house, drum and bass and lo-fi,
a polyphonic synthesiser with 68 presets, mallets, bowed strings, winds
and a choir) through a chain of [effects](#effects), then gain
and pan, into a [master bus](#the-master-bus) with a shared reverb, a glue
compressor and a true-peak limiter. Every kind and preset plays at one
[loudness](#loudness), -24 LUFS on a standard phrase, and a sampler part
measures each instrument as it loads to play it there. Events are 32-byte
records that land on the exact frame they name, and the output is the
same, bit for bit, whatever the size of the quanta it is rendered in.

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
transient; the instruments of `manguebyte-dsp` as their voices say (see
[Accents, velocity and slides](#accents-velocity-and-slides)). The crate
documentation gives the details.

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

### Percussion keys

Aratu also writes lines for Brazilian percussion and a few other hand
instruments. Their keys are fixed here, so a sampler kit that has them
(`sampler_kit`) is played the same way whoever made it. Where General MIDI
or General MIDI 2 already names the instrument, its key is used; the rest
take free keys after the guitar tap. The engine has no synthesized version
of most of these yet, so a kit without a region for one of them stays
silent on it. A [drum kit](#drum-kits-drum_kit) part plays the clap where
its kit has one, and the shaker on its kit's maracas (or cabasa), the
shakers of General MIDI 1 (see there).

| Stroke | Name | Key | Source of the key |
|--------|------|-----|-------------------|
| hand clap | `clap` | 39 | General MIDI |
| agogô, high bell | `agogo-high` | 67 | General MIDI (High Agogo) |
| agogô, low bell | `agogo-low` | 68 | General MIDI (Low Agogo) |
| shaker | `shaker` | 82 | General MIDI 2 (Shaker) |
| cuíca, high | `cuica-high` | 78 | General MIDI (Mute Cuica, the high squeak) |
| cuíca, low | `cuica-low` | 79 | General MIDI (Open Cuica, the low groan) |
| triangle, closed | `triangle-closed` | 80 | General MIDI (Mute Triangle) |
| triangle, open | `triangle-open` | 81 | General MIDI (Open Triangle) |
| surdo, muted | `surdo-muted` | 86 | General MIDI 2 (Mute Surdo) |
| surdo, open | `surdo-open` | 87 | General MIDI 2 (Open Surdo) |
| pandeiro, thumb | `pandeiro-thumb` | 89 | Manguebyte |
| pandeiro, fingertips | `pandeiro-fingers` | 90 | Manguebyte |
| pandeiro, slap | `pandeiro-slap` | 91 | Manguebyte |
| pandeiro, muted | `pandeiro-muted` | 92 | Manguebyte |
| zabumba, open | `zabumba-open` | 93 | Manguebyte |
| zabumba, muted | `zabumba-muted` | 94 | Manguebyte |
| zabumba, stick (bacalhau) | `zabumba-stick` | 95 | Manguebyte |
| tamborim, open | `tamborim-open` | 96 | Manguebyte |
| tamborim, muted | `tamborim-muted` | 97 | Manguebyte |
| alfaia, open | `alfaia-open` | 98 | Manguebyte |
| alfaia, muted | `alfaia-muted` | 99 | Manguebyte |
| gonguê, open | `gongue-open` | 100 | Manguebyte |
| gonguê, muted | `gongue-muted` | 101 | Manguebyte |
| cajón, bass | `cajon-bass` | 102 | Manguebyte |
| cajón, slap | `cajon-slap` | 103 | Manguebyte |

Where a player damps a ringing instrument with the hand (triangle, surdo,
gonguê), a kit can let the muted stroke cut the open one with `group` and
`off_by`, as it does for the hi-hats.

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
line instead), and on the instruments of `manguebyte-dsp` their own
releases, or a fade where theirs would click (see
[Cancel and all notes off](#cancel-and-all-notes-off)). `tests/stage.rs` stops a loud, sustained sound of every kind
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
| | `shape` | | 0 tanh, 1 soft clip, 2 hard clip, 3 fold, 4 tube | 0 | choice |
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
  change of curve cross-fades. The first four curves are odd, so they add
  odd harmonics only and no offset appears; the drive is not oversampled
  (its aliasing is that of the curve). The **tube** (`shape` 4, `'tube'`
  in `SHAPES`) is asymmetric, in the manner of a triode biased off the
  middle of its range:

  ```text
  f(x) = ¾ · (tanh(³⁄₂ · x + ½ ln 2) − ⅓)
  ```

  The bias, `½ ln 2`, puts the resting point a third of the way up the
  hyperbolic tangent (its tangent is exactly ⅓), so `f(0) = 0` and the
  slope there is 1: a quiet signal passes unchanged, and to second order
  `f(x) ≈ x − x²/2`. The positive half, which runs into the bend sooner,
  as a triode's grid starts to conduct, flattens towards ½; the negative
  half, heading for cutoff, eases towards −1. Bent unequally, a wave gains
  even harmonics: a kilohertz at the drive's defaults comes out with its
  second and fourth harmonics 18 dB below the fundamental, where the odd
  curves give none (below -300 dB). That also shifts its middle, so after
  the curve a first-order high-pass at 5 Hz (`TUBE_DC_HZ`),
  `y[n] = x[n] − x[n−1] + R·y[n−1]` with `R = exp(−2π·5 Hz/rate)`, takes
  the offset out: a tenth of the peak and more on a loud tone, less than
  10⁻⁴ of it after the filter. The drawn curve is `f` itself
  (`tube_curve`), before the filter. At the drive's defaults a phrase
  plays within 0.6 LU of the tanh's loudness (`tests/loudness.rs`), so
  changing the curve changes the colour, not the level. Pakarinen and
  Yeh, "A review of digital techniques for modeling vacuum-tube guitar
  amplifiers" (*Computer Music Journal* 33(2), 2009), survey this kind of
  static model.
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

They are `manguebyte-dsp`'s own response curves (`manguebyte_dsp::curve`),
made from the settings the parameters hold with the limits the effects
apply, so what is drawn is what the effect runs:

| Effect | Curve |
|---|---|
| high-pass, low-pass | `SvfResponse`, the state-variable filter's own response |
| EQ | the three bands' `BiquadResponse`s, of the coefficients the equaliser runs, in a row |
| drive | the `ShaperCurve`, or this crate's `tube_curve` before its high-pass, between the drive's gains and its mix |
| compressor, glue | `CompressorTransfer`, with the make-up and the parallel mix |
| limiter | `LimiterTransfer`, at the ceiling or `OUTPUT_CEILING`, the lower |
| chorus, flanger | `ChorusSweep` |
| phaser | `PhaserSweep` |
| tremolo, auto-pan | `LfoCycle`, through what the effect does with its wave |
| delay | `DelayTaps`, with the ping-pong's spread between the sides: echo `k` of the louder side is the tap's gain times `1 - p/2 + p/2·|1-2p|^(k-1)`, `p` the ping-pong |
| reverb | `ReverbDecay` at 1 kHz, where reverberation times are quoted (a part's reverb has damping 0.45 and size 0.6; the shared one its parameters) |

The bitcrusher is this crate's own, and so is its staircase; so is the
tube's curve. `tests/effects.rs` checks every curve against the `manguebyte-dsp`
function, to the precision of an `f32`, and against what is measured
through the engine: the equaliser's and the filters' curves within 0.1 dB,
the compressor's within 0.1 dB, the delay's taps exactly. They live in
`src/curve.rs`, one match arm per effect in `part_curve` and
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
`partChain`, `limiterReduction`, and `partScope` and `partLevels` (see
[Scopes and levels](#scopes-and-levels)) complete it; `EFFECT`, `CHAIN`,
`EFFECT_ORDER`, `EFFECT_PARAMS`, `VISUAL`, `SHAPES` and `SCOPE_FRAMES`
name everything.

## Scopes and levels

Every part keeps its latest 4 096 frames (`SCOPE_FRAMES`, 85 ms at
48 kHz) and its peak and RMS levels, for Aratu's editor to draw: what the
part puts into the master bus, after its effects, gain and pan, in both
channels (its send to the shared reverb is not in it).

```js
// Every frame the editor draws:
const { frame, time, left, right } = await engine.partScope(0, 2048);
const { peak, rms } = await engine.partLevels(0);   // [left, right], linear
const every = await engine.partLevels();            // [{ part, frame, peak, rms }, ...]
```

- **The scope.** `partScope(part, frames)` gives the newest `frames`
  frames, at most 4 096 and none before frame 0; frames from before the
  part was made are silence. `frame` is the engine frame of the first
  sample, the frame it is heard on, and `time` its `AudioContext` time
  (before the device's own latency). The parts run the master bus's
  look-ahead (111 frames at 48 kHz) ahead of the output, so the newest 111
  samples are about to be heard. A part made again over itself keeps its
  scope and levels, which belong to its number; a removed part has none
  (`null`).
- **The levels.** The peak rises at once to every sample above it and falls
  20 dB in 1.7 s (`PEAK_RETURN_SECONDS`, the return time IEC 60268-18 gives
  digital peak meters), in a straight line in decibels; the RMS is the
  square of the signal averaged by a one-pole filter with a time constant of
  0.3 s (`RMS_SECONDS`), so a steady sine at full scale reads `1/√2`. Both
  are linear, 1 at full scale; both settle to exactly 0 in silence.
- **On the audio thread.** The render call feeds each part's meter one
  frame at a time, writing into a ring that was made with the part: no
  allocation, no lock, and the same samples and levels whatever the quanta
  (`tests/scope.rs` reads them after quanta of 1, 7, 128 and 512 frames and
  compares the bits). Reading copies out of the ring, between render calls,
  and does not allocate either (`Engine::part_scope_into`,
  `Engine::part_levels`, and the exports `mb_part_scope`, `mb_scope_ptr`,
  `mb_part_scope_end`, `mb_part_levels`, `mb_levels_ptr`,
  `mb_scope_frames`; `tests/realtime.rs` reads every part after every
  quantum under the counting allocator).
- **Shared memory.** When the page is cross-origin isolated, `engine.js`
  gives the worklet a `SharedArrayBuffer` of one slot per part it can hold
  (`js/scope.js`, 48 bytes and 8 bytes a frame each: 33 KiB a part, 1 MiB
  for the default 32). After every render quantum the worklet copies the
  frames each part rendered, and its levels, into its slot: three export
  calls and two copies of 128 samples a part. A slot is a sequence lock:
  the worklet makes its counter odd, writes, and makes it even, never
  waiting; `partScope` and `partLevels` read the slot, without a message,
  and read again if the counter was odd or moved. Without shared memory,
  or with `scopes: false` in `ManguebyteEngine.create`, they ask the
  worklet, which answers from the engine between quanta: a message each
  way. `engine.scopeMode` says which, `'shared'` or `'message'` (or `null`
  for a module without the exports, when they reject).

`js/scope.js` lays a slot out as `seq` (u32), `part` (i32, -1 for a free
slot), `written` (u32, frames ever written, wrapping at 2³²), four bytes
of padding, `end` (f64, the engine frame after the newest one), the four
levels (f32: peak left, peak right, RMS left, RMS right), eight bytes of
padding, then the left and right channels of `capacity` frames each, frame
`n` of the slot at `n mod capacity`. `js/tests/scope.test.mjs` checks it,
and `partScope` and `partLevels` against the engine in both modes.

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

The [instruments](#instruments) of `manguebyte-dsp` keep it at 2 too.
Instrument codes 4 to 9 are new (an older engine refuses them with
`BAD_INSTRUMENT`, -3, so a page knows), their parameters are new ids, and
the export `mb_set_instrument` is new; `js/worklet.js` looks for it by name
and answers `BAD_INSTRUMENT` without it. Nothing that was there means
anything else: the drum map is the same, every kind played the accent bit
already, and a note on before a note off was already two notes; on the two
new monophonic kinds it is a slide, which no existing writer sends to them.
The synth now takes its accent as an argument of the note
(`Synth::note_on_accented`) instead of a flag set on every voice
beforehand, which sounds the same.

[Loudness](#loudness) keeps it at 2 as well: it changes how loud the
parts are, not what any record or export means. `sampler.normalize` is a
new parameter id; the export `mb_sfz_measure` and its status 1
(`MEASURING`) are new, and `js/worklet.js` looks for the export by name,
finishing a load in one step without it; `mb_sfz_finish` still finishes a
load in one call, measuring the instrument there if no step did.

The newer voices of `manguebyte-dsp` keep it at 2 too: instrument codes
10 to 15 (`drum_kit`, `poly`, `mallet`, `bowed`, `wind`, `choir`) are new,
and an older engine refuses them with `BAD_INSTRUMENT` (-3); their
parameters are new ids. Nothing that was there means anything else: a
drum kit part answers the drum map as the other kits do, and takes the
accent bit as every kind does; Aratu's percussion keys mean on it what
they mean on a sampler kit. A note on before a note off on the same
channel was already two notes; on the parts that slur (the monophonic
presets of `poly`, `wind`, and `bowed` and `choir` with legato on) it is
a slur, which no existing writer sends to them.

Playing an instrument before it is measured keeps it at 2 as well. The
exports `mb_sfz_install`, `mb_sampler_measure` and `mb_sampler_make_up_db`
are new, and `js/worklet.js` looks for them by name, loading the older way
without them (measured, then handed over, both promises settling at once).
`mb_sfz_measure` and `mb_sfz_finish` mean what they did; the one thing
they do differently is that the part now plays an instrument with round
robins and random choices as they were when it was loaded, a clone being
measured, where before it went on from where the measurement had left
them. In JavaScript, `loadSfz` resolves sooner, once the part plays the
instrument rather than once it is measured, with the same summary; code
that awaited it and then played notes works as it did, the first few tens
of milliseconds of them at the provisional make-up. Code that wants the
level measured awaits `loading.normalized`. The status -102 is new.

The [scopes and levels](#scopes-and-levels) and the tube keep it at 2 as
well. The exports `mb_scope_frames`, `mb_part_scope`, `mb_scope_ptr`,
`mb_part_scope_end`, `mb_part_levels` and `mb_levels_ptr` are new, and
`js/worklet.js` looks for them by name: without them its ready message says
`scopes: null`, and `partScope` and `partLevels` reject with the new status
-103. The worklet's `scope` option and its ready message's `scopes` field
are new; nothing the parts play changed. `drive.shape` 4 is a new value of
a parameter that was there: an older engine, which holds a choice to its
last value, plays the fold for it, and every value that was there means
what it did.

## Instruments

A part plays one of sixteen kinds of instrument, named by a code in the
exports and by a name in `js/engine.js` (`INSTRUMENT`,
`InstrumentKind::name`):

| Code | Name | `InstrumentKind` | What it plays | Notes at once |
|---|---|---|---|---|
| 0 | `synth` | `Synth` | two detuned oscillators and a sub, a resonant low-pass with its envelope | 8 |
| 1 | `drums` | `Drums` | the synthesized kit of the [drum map](#drum-map) | one of each drum |
| 2 | `sampler` | `Sampler` | an SFZ instrument ([Sampler parts](#sampler-parts)) | 32 |
| 3 | `sampler_kit` | `SamplerKit` | an SFZ drum kit that falls back on the synthesized drums | 32 |
| 4 | `plucked` | `Plucked` | plucked strings by physical modelling: nylon and steel guitar, finger and slap bass | 8 |
| 5 | `fm` | `Fm` | four-operator FM: electric piano, tubular bell, FM bass | 8 |
| 6 | `organ` | `Organ` | a tonewheel organ (drawbars, percussion, key click) through a rotating speaker | 16 |
| 7 | `analog_kit` | `AnalogKit` | an 808-style drum machine on the drum map, with the synthesized ride and guitar tap | 16 |
| 8 | `kick_bass` | `KickBass` | the 808's long kick played as a bass, sliding | 1 |
| 9 | `acid` | `Acid` | the acid bass: a saw or square through a diode ladder, with accent and slide | 1 |
| 10 | `drum_kit` | `DrumKit` | [drum kits](#drum-kits-drum_kit): acoustic by modal synthesis, the 808, 909, 606 and CR-78, and kits for trap, techno, house, drum and bass and lo-fi, on the drum map | 12 |
| 11 | `poly` | `Poly` | the [polyphonic synthesiser](#polyphonic-synthesiser-poly) and its 68 presets: basses, leads, pads, plucks, keys, brass, effects | 4 to 8 by preset (`poly.voices`), 1 for a monophonic one |
| 12 | `mallet` | `Mallet` | [mallets](#mallets-mallet) by modal synthesis: marimba, xylophone, vibraphone, glockenspiel, celesta, kalimba, steel pan, tubular bells | 12 |
| 13 | `bowed` | `Bowed` | [bowed strings](#bowed-strings-bowed) by digital waveguide: violin, viola, cello, double bass, a section, pizzicatos | 6 |
| 14 | `wind` | `Wind` | [winds](#winds-wind) by digital waveguide: flute, clarinet, saxophone, trumpet, trombone | 4, one while legato |
| 15 | `choir` | `Choir` | a [choir](#choir-choir) by formant synthesis: "aah", "ooh", a solo voice | 6 |

Kinds 4 to 15 are the voices of `manguebyte_dsp::instruments`
(`PluckedString`, `FmVoice`, `OrganVoice` and `RotarySpeaker`,
`AnalogKitVoice`, `KickBassVoice`, `AcidVoice`) played by a `Synth`, as
that module recommends: the organ's sixteen voices share one `OrganManual`
(`OrganVoice::voices`), so its percussion is single-triggered; the drum
machine's sixteen share one `ChokeGroup` (`AnalogKitVoice::voices`), so the
closed hat chokes the open one; the two basses are a synth of one voice
with legato on. The newer voices (`DrumKitVoice`, `PolySynthVoice`,
`MalletVoice`, `BowedString`, `WindVoice`, `ChoirVoice`) place their notes
in the stereo field, so those parts' synths render two channels: a kit's
pieces sit where the kit puts them, a synthesiser's unison voices spread,
a section's players and a choir's singers stand apart. The drum kit's
voices share one set of choke groups (`DrumKitVoice::voices`).
Notes reach them through `Synth::note_on_accented`, with bit 0 of the
record's flags.

```js
await engine.createPart(1, 'acid', ['delay']);   // a part of a kind
await engine.setInstrument(2, 'organ');          // another kind, the same chain
engine.setPreset(1, 'acid', 'squelch', nextBar); // PRESETS.acid
engine.setRotary(2, 'fast', nextBar);            // ROTARY: stop, slow, fast
engine.noteOn(1, 45, 0.9, t, 0, true);           // accented, as Aratu writes "A"
await engine.createPart(3, 'drum_kit', 'none');
engine.setPreset(3, 'drum_kit', '909', nextBar); // PRESETS.drum_kit
await engine.createPart(4, 'poly');
engine.setPreset(4, 'poly', 'warm_analog_pad', nextBar); // PRESETS.poly
```

**Allocation.** An instrument is made, with all its memory (a plucked
part's strings, the organ's tonewheels and speaker, every voice), when a
part is given it: `createPart` (`mb_create_part`, `Engine::create_part`) or
`setInstrument` (`mb_set_instrument`, `Engine::set_instrument`), control
calls. `setInstrument` keeps the part's chain, its effects and their
settings, gain, pan and the send; the new instrument's parameters start at
their defaults, and the notes of the one it replaces stop. Rendering never
allocates, whatever the parts play (`tests/realtime.rs`).

**Presets and knobs.** The plucked, FM, organ, acid, drum kit,
synthesiser, mallet, bowed, wind and choir parts have a `<kind>.preset`
parameter. A preset is a set of knob positions: setting it
moves every knob it covers to the preset's position, on the frame it lands
on, and the parameter JSON and `paramValue` say where they are. So a knob
set on the same frame after the preset sticks, and one set before it is
moved. The JSON's defaults are the first preset's positions.

**Levels.** Every kind and every preset plays its standard phrase at the
same loudness, -24 LUFS ([Loudness](#loudness)), and one note at full
velocity, accented, stays under the master limiter's ceiling on every
kind: the loudest, the drum machine's snare, peaks at -0.6 dBTP. The dsp
presets' levels are trimmed by `PLUCKED_LEVEL_DB` (-6 dB), `FM_LEVEL_DB`
(-3 dB) and `ANALOG_KIT_LEVEL_DB` (+2 dB), and each preset's make-up gain
comes on top. Chords of six strings at full velocity reach the limiter,
which turns them down gently. The drum kits, the violin's pizzicato and
the synthesiser's Plucked String play narrower dynamics than
`manguebyte-dsp` gives them, so that their loudest note, accented at full
velocity, stays under the ceiling at the target too (see each); the
acoustic kit's kick, the loudest of the newer voices, peaks at -0.7 dBTP.

### Accents, velocity and slides

Every kind is louder and brighter with velocity and with an accent, by
amounts its voice's settings say; `tests/voices.rs` measures each: half the
velocity is 4.5 to 12.5 dB down; an accent adds 2 to 5.7 dB, and for the
strings, FM and acid a brighter spectrum (the centroid of the first 85 ms
4 to 72% higher). The organ and the acid bass have no velocity of their own,
as on the instruments; `organ.velocity` and `acid.velocity` default to 0.5,
so that Aratu's dynamics are heard on them (half the velocity is 6 and
4.5 dB down). On the newer kinds `tests/models.rs` measures the energy of
the first 0.3 s of a note of 47 presets (every kit on its kick, snare and
hat, pads, plucks, every mallet, the strings, the winds, the choirs): half
the velocity is 5 to 15.5 dB down, an accent adds 1.5 to 5 dB, and the
spectrum of an accented note is up to 35% brighter (the centroid of its
first 85 ms, of its tone from 0.1 s for the bowed, blown and sung ones);
an acoustic drum struck harder rings its low modes harder too, and is at
most 6% darker there by that measure.

**Slides.** On the two monophonic kinds, a note struck while the last one
is still held slides from it: its pitch glides there (`acid.slide`,
`kick_bass.glide`: 95% of the way in that time, whatever the interval) and
neither envelope starts again. Aratu writes a slide by letting the notes
overlap: the next note's note on comes before the last note's note off,
either earlier in time or on the same frame and pushed first. A note off
before the next note on, even on the same frame, strikes the next note
afresh. Only notes on the same channel slide. The replaced note is
forgotten, so its late note off changes nothing.

Aratu writes slurs on the newer kinds the same way. The synthesiser's
monophonic presets glide to the next note and strike their envelopes
again, its legato ones glide without striking them when the notes
overlap; a wind part (`wind.legato` on, the default) changes notes as
keys and valves do, the breath going on; a bowed part with `bowed.legato`
on stops the string at its new length, the bow going on; a choir with
`choir.legato` on slurs its vowel to the next pitch. `bowed.portamento`
and `wind.portamento` make those slurs slide through the pitches between.

```js
engine.noteOn(1, 45, 0.9, t);          // A
engine.noteOn(1, 57, 0.9, t + step);   // slides up to the A above...
engine.noteOff(1, 45, t + step);       // ...because this comes after it
engine.noteOff(1, 57, t + 2 * step);
```

### Cancel and all notes off

The voices of `manguebyte-dsp` have no fade of their own, so each of these
parts keeps two synths of its voices: the notes go to one, and a cancel
hands it over to fade out in a straight line over 8 ms while the other,
clean, takes the notes that follow, so a note on the cancel's frame after
it plays in full, as on every other kind. (A second cancel within the 8 ms
of the first cuts the first one's fade short.) All notes off releases the
strings, the FM voices, the basses, the synthesiser, the mallets, the
bowed strings, the winds and the choir as they release; the organ, whose
contacts open with a click, and the two drum kits, whose drums are
one-shots, fade out instead, over 8 ms (`PANIC_FADE_SECONDS`) and 50 ms
(`DAMP_SECONDS`). Choke stops everything at once. `tests/stage.rs` checks
every kind: no step after a cancel or an all-notes-off larger than the
steps before, and silence once the fade is over; the organ's speaker,
which hears the organ a millisecond late, rings on below -120 dB within
5 ms, and is cleared when it falls asleep. `tests/models.rs` does the same
for more presets of the newer kinds (the 909, trap and lo-fi kits, pads,
a supersaw, chord stabs and a riser, the vibraphone and the tubular
bells, a string section, cellos, a double bass pizzicato, a trumpet,
saxophones in chords, the "ooh" choir and the solo voice), against the
largest step of the sound before or of the same sound left to go on, as
a pad whose filter opens grows brighter as it goes.

### Plucked strings (`plucked`)

`PluckedString`, a Karplus-Strong string with an exact tuning all-pass,
a loss filter that makes low notes ring longer and every note darken,
stiffness, the pluck's shape and position, and a body or a pickup. Eight
voices: a voice taken over fades its string over 4 ms.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `plucked.preset` | | 0 nylon guitar, 1 steel guitar, 2 finger bass, 3 slap bass | 0 | choice |
| `plucked.decay` | s | 0.1 to 20, how long middle C rings while held | 3 | skew 0.35 |
| `plucked.brightness` | Hz | 100 to 20 000, the pluck's low-pass at full velocity | 2 800 | log |
| `plucked.position` | | 0.02 to 0.5 of the string from the bridge | 0.16 | linear |
| `plucked.tone` | Hz | 200 to 20 000, the pickup's or the body's low-pass | 6 500 | log |
| `plucked.release` | s | 0.01 to 2, a released string's fall | 0.18 | skew 0.35 |

| Preset | `decay` | `brightness` | `position` | `tone` | `release` |
|---|---|---|---|---|---|
| nylon guitar | 3 | 2 800 | 0.16 | 6 500 | 0.18 |
| steel guitar | 4.5 | 6 500 | 0.12 | 11 000 | 0.25 |
| finger bass | 6 | 1 600 | 0.2 | 2 600 | 0.12 |
| slap bass | 5 | 9 000 | 0.28 | 6 000 | 0.1 |

Velocity plucks softer and darker; an accent louder and brighter, and the
slap bass's pops. Aratu strums a chord by striking its strings a few
milliseconds apart (`examples/aratu_song.rs` uses 12 ms).

### FM (`fm`)

`FmVoice`, four sine operators: the electric piano (tine and tone bar),
the tubular bell, the FM bass. The knobs are factors on the preset, so a
preset puts them back to 1.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `fm.preset` | | 0 electric piano, 1 tubular bell, 2 FM bass | 0 | choice |
| `fm.brightness` | x | 0 to 2, on every modulator's index and every feedback | 1 | linear |
| `fm.decay` | x | 0.25 to 4, on every operator's decay | 1 | log |
| `fm.release` | x | 0.25 to 4, on every operator's release | 1 | log |

A note played harder or accented is brighter, its modulators' indices
higher. High notes are less bright than low ones: the voice turns its
indices down where its sidebands would pass 0.45 of the sample rate.

### Organ (`organ`)

`OrganVoice`, nine drawbars from the tempered tonewheels, single-triggered
percussion, key click and leakage, sixteen voices on one manual, heard
through a `RotarySpeaker`. The speaker is part of the organ part, not an
effect: it is the organ's own cabinet, and its speed is played like a
switch on the instrument, a parameter that lands on its frame; it takes the
organ's one channel and makes the stereo pair the chain then works on; and
an effect would have changed the chain's bits and order, which Aratu's
method chains name, for one instrument. So the drive, if a part has it,
comes after the speaker, as a microphone's preamplifier would, not before
it as the cabinet's own amplifier does; the speaker has no overdrive of its
own. It is asleep (`Engine::speaker_asleep`) while the organ is silent, and
wakes with the organ's first sound, its rotors at their resting places
turning at the speed set.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `organ.preset` | | 0 jazz, 1 gospel, 2 full, 3 ballad | 0 | choice |
| `organ.drawbar_16`, `_5_1_3`, `_8`, `_4`, `_2_2_3`, `_2`, `_1_3_5`, `_1_1_3`, `_1` | | 0 (in) to 8 (out), 3 dB a step | 8 8 8 0 0 0 0 0 0 | linear |
| `organ.percussion` | | 0 off, 1 second harmonic fast, 2 second slow, 3 third fast, 4 third slow | 3 | choice |
| `organ.click` | | 0 to 1 | 0.5 | linear |
| `organ.leakage` | | 0 to 1 | 0.4 | linear |
| `organ.velocity` | | 0 to 1: at 1 the lowest velocity is 24 dB down | 0.5 | linear |
| `organ.rotary` | | 0 stop, 1 slow (chorale), 2 fast (tremolo) | 1 | choice |
| `organ.speaker` | | 0 (straight) to 1 (all through the speaker) | 1 | linear, 20 ms |

| Preset | Drawbars | Percussion | `click` | `leakage` |
|---|---|---|---|---|
| jazz | 888000000 | third, fast | 0.5 | 0.4 |
| gospel | 888800000 | second, fast | 0.6 | 0.4 |
| full | 888888888 | off | 0.7 | 0.6 |
| ballad | 838000000 | off | 0.15 | 0.4 |

A preset leaves the velocity and the speaker alone. The rotors get to a
new speed at their own pace, the horn in under a second, the drum in
several: `setRotary(part, 'fast', time)` on the beat is how Aratu plays the
switch.

### Drum machine (`analog_kit`)

`AnalogKitVoice`, sixteen drums modelled on the 808's circuits. It answers
the drum map; the two drums the machine does not have come from the
synthesized kit of `drums`, so a pattern written for one plays on the
other:

| Drum map | Note | Plays |
|---|---|---|
| kick | 36 | the machine's kick |
| snare | 38 | its snare |
| closed hat | 42 | its closed hat, which chokes its open hat |
| open hat | 46 | its open hat |
| crash | 49 | its cymbal, the machine's one |
| ride | 51 | the synthesized ride |
| high, mid, floor tom | 50, 47, 43 | its high, mid and low tom |
| side stick | 37 | its rimshot |
| guitar tap | 88 | the synthesized guitar tap |

Every other key the machine's General MIDI map gives a drum plays it: 35
the kick, 39 the clap, 40 the snare, 41 and 45 and 48 the toms, 44 the
closed hat, 52, 55, 57 and 59 the cymbal, 56 the cowbell, 62 to 64 the
congas, 70 the maracas, 75 the claves. Other keys are silent. A drum is a
one-shot; a note off changes nothing.

It has the kit's parameters, which reach both halves: `kit.tune`
(semitones, -12 to 12, 0) and `kit.decay` (x, 0.25 to 4, 1) tune and
stretch every drum, and each drum's level (`kick.level`... 0 to 1, 0.8)
sets the machine's drum it plays (0.8 is the machine's own level) or the
synthesized one. Then the machine's own:

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `analog.tone` | | 0 to 1: the kick's click, the snare's upper head, the hats' band... | 0.5 | linear |
| `analog.snap` | | 0 to 1: the kick's drop and click, the snare's wires, the toms' noise... | 0.5 | linear |
| `analog.kick_decay` | x | 0.25 to 8, on the kick's decay, the machine's long boom | 1 | log |

An accent is 4 dB louder and snappier. All notes off fades every drum out
over 50 ms.

### Kick bass (`kick_bass`)

`KickBassVoice`, the 808's kick as a bass: a sine that drops from above
the note, rings for seconds and growls through a saturator. One voice,
legato: it slides (see above). Its knobs are those of the machine's one
setting.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `kick_bass.tune` | st | -12 to 12 | 0 | linear |
| `kick_bass.decay` | s | 0.05 to 20, a held note's fall | 2.5 | skew 0.35 |
| `kick_bass.sweep` | st | 0 to 48, how far above the note it starts | 12 | linear |
| `kick_bass.drive` | | 0 to 1 | 0.3 | linear |
| `kick_bass.glide` | s | 0 to 2, a slide's time | 0.08 | skew 0.35 |
| `kick_bass.click` | | 0 to 1 | 0.2 | linear |

### Acid bass (`acid`)

`AcidVoice`, the 303's line: a band-limited saw or square through a diode
ladder and an amplifier, twice oversampled (16 frames late). One voice,
legato: it slides. Its accent is the machine's: louder, a deeper and
shorter filter envelope, and a sweep that builds over accents close
together.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `acid.preset` | | 0 classic, 1 squelch, 2 square | 0 | choice |
| `acid.wave` | | 0 saw, 1 square | 0 | choice |
| `acid.cutoff` | Hz | 30 to 8 000, with the envelope closed | 320 | log |
| `acid.resonance` | | 0 to 1 | 0.72 | linear |
| `acid.env_mod` | | 0 to 1 (4.5 octaves) | 0.6 | linear |
| `acid.decay` | s | 0.05 to 4, the filter envelope | 0.5 | skew 0.35 |
| `acid.accent` | | 0 to 1 | 0.7 | linear |
| `acid.slide` | s | 0.005 to 1 | 0.06 | skew 0.35 |
| `acid.drive` | dB | -12 to 24, into the filter | 6 | linear |
| `acid.velocity` | | 0 to 1: at 1 the lowest velocity is 18 dB down | 0.5 | linear |

| Preset | `wave` | `cutoff` | `resonance` | `env_mod` | `decay` | `accent` | `drive` |
|---|---|---|---|---|---|---|---|
| classic | saw | 320 | 0.72 | 0.6 | 0.5 | 0.7 | 6 |
| squelch | saw | 260 | 0.9 | 0.8 | 0.3 | 0.9 | 9 |
| square | square | 420 | 0.6 | 0.5 | 0.7 | 0.7 | 6 |

Every preset slides in 0.06 s; a preset leaves the velocity alone.

### Drum kits (`drum_kit`)

`DrumKitVoice`, a kit whose piece on each key is chosen and tuned:
acoustic drums and cymbals by modal synthesis, and the drums of the 909,
606, CR-78 and 808. Twelve voices share one set of choke groups, so the
closed hat (and the pedal hat) chokes the open one; a piece struck again
while it rings takes its own voice. Pieces are one-shots: a note off
changes nothing. The kit's pieces sit in the stereo field where the kit
puts them.

| `drum_kit.preset` | Kit |
|---|---|
| 0 `acoustic` | an acoustic kit by modal synthesis: kick, snare and its rimshot, side stick, six toms, hi-hats, two crashes, two rides and the bell, china, splash, congas and bongos; the clap, tambourine, cowbell, maracas, guiro and claves of the CR-78 and the 909 |
| 1 `808` | the 808's sixteen drums |
| 2 `909` | the 909's eleven: kick, snare, clap, rimshot, three toms, hats, crash, ride |
| 3 `606` | the 606's seven: kick, snare, two toms, hats, cymbal |
| 4 `cr_78` | the CR-78's: kick, snare, rimshot, hats, cymbal, cowbell, guiros, maracas, tambourine, bongos, conga, claves |
| 5 `trap` | the 808's long kick tuned down, a crisp 909 snare and clap, the 808's tight hats, rimshot and cowbell, the 909's cymbals |
| 6 `techno` | the 909's kick with a rumble, a punchy clap, its hats, ride, crash, snare, rimshot and toms |
| 7 `house` | the 909's kick, a long clap, short crisp hats, ride, a shaker and tambourine, the 808's congas and cowbell |
| 8 `drum_and_bass` | the 808's deep kick cut short under an acoustic kit tight and bright |
| 9 `lo_fi` | a soft acoustic kit through a twelve-bit sampler at 26 kHz, saturated and dark |

**The drum map.** A note of the drum map plays the kit's piece on its
key (kick 36, side stick 37, snare 38, closed hat 42, open hat 46, crash
49, ride 51, toms 50, 47 and 43), every other General MIDI key the kit
has a piece on plays it too (35 to 81: the clap on 39, the pedal hat on
44, the rimshot or electric snare on 40, congas, cowbell...), and a drum of
the map the kit has no piece for is the synthesized drum of `drums`, at the
note's velocity, with its accent, through the part's chain, as on the
drum machine: the guitar tap (88) on every kit, the side stick on the 606,
the three toms on the CR-78. `tests/models.rs` checks every drum of the
map on every kit: the kit's own where it has one, and where it has not
the synthesized drum to the bit, in the centre.

**Aratu's percussion keys.** The clap (39) is the kit's clap where it has
one (every kit but the 606 and the CR-78). The shaker (82, past the last
key a kit maps) plays the kit's maracas (70), or its cabasa (69): the
acoustic, 808, CR-78, house and lo-fi kits. The kits have no agogô, cuíca, triangle, surdo,
pandeiro, zabumba, tamborim, alfaia, gonguê or cajón, and the synthesized
kit has none either, so those keys are silent on every kit; a
`sampler_kit` with samples of them plays them.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `drum_kit.preset` | | 0 to 9, as above | 0 | choice |
| `kit.tune` | st | -12 to 12, added to every piece's tuning and the synthesized drums' | 0 | linear |
| `kit.decay` | x | 0.25 to 4, on every piece's decay and the synthesized drums' | 1 | log |
| `kick.level`... `guitar_tap.level` | | 0 to 1, the level of the piece on the drum's key (0.8 is the kit's own) or of the synthesized drum | 0.8 | linear |
| `drum_kit.tone` | | -1 to 1, added to every piece's tone: the stick, where a drum is struck, a machine's colour | 0 | linear |
| `drum_kit.snap` | | -1 to 1, added to every piece's snap: the wires, a cymbal's shimmer, a machine's snappy | 0 | linear |
| `drum_kit.velocity` | dB | 0 to 60, how much quieter the softest strike is | the preset's | linear |
| `drum_kit.accent` | dB | 0 to 12, how much louder an accented strike is | the preset's | linear |

Velocity and an accent also strike harder: an acoustic piece is brighter,
a machine's snap stronger. **Dynamics.** The kits of `manguebyte-dsp` span
30 dB of velocity and accent by 4 dB (the lo-fi kit 24 dB). At the
loudness target a spiky kit (the acoustic kick's beater, the 606's short
kick) would then put its loudest drum, accented at full velocity, past
the master limiter's ceiling: its groove at velocity 0.8 sits that far
below it. So the presets play narrower (`KIT_DYNAMICS` in
`src/modelled.rs`): the acoustic kit 11 dB and 2 dB, the 808 24 and 3, the
909 30 and 3, the 606 10 and 1.5, the CR-78 16 and 2, trap 30 and 4,
techno and house 30 and 3, drum and bass 24 and 3, lo-fi 24 and 4. A
preset moves `drum_kit.velocity` and `drum_kit.accent` there; widen them
and an accented drum at full velocity may reach the limiter. All notes off
fades every piece out over 50 ms.

### Polyphonic synthesiser (`poly`)

`PolySynthVoice`, three oscillators of classic shapes and wavetables with
up to sixteen unison voices, sync, FM and ring modulation, a sub and
noise, two filters of fifteen models, four envelopes, three LFOs and a
sixteen-slot modulation matrix, twice oversampled (20.5 frames late), and
its library of 68 presets, `PRESETS.poly` in `js/engine.js` (the names in
lower case, words joined by underscores):

| Category | `poly.preset` |
|---|---|
| basses | 0 `moog_bass`, 1 `reese`, 2 `wobble_bass`, 3 `deep_sub`, 4 `fm_growl`, 5 `house_organ_bass`, 6 `acid_line`, 7 `garage_square`, 8 `wavetable_growl`, 9 `synthpop_bass`, 10 `funk_bass`, 11 `808_glide`, 12 `rolling_bass`, 13 `rubber_bass`, 14 `talking_bass` |
| leads | 15 `trance_supersaw`, 16 `square_lead`, 17 `sync_lead`, 18 `chiptune`, 19 `hoover`, 20 `mono_saw_lead`, 21 `whistle_lead`, 22 `digital_lead`, 23 `talking_lead`, 24 `screamer`, 25 `future_lead` |
| pads | 26 `warm_analog_pad`, 27 `string_machine`, 28 `glass_pad`, 29 `dark_drone`, 30 `choir_pad`, 31 `formant_pad`, 32 `pwm_pad`, 33 `shimmer_pad`, 34 `evolving_spectra`, 35 `soft_sine_pad`, 36 `comb_wash`, 37 `sweep_pad` |
| plucks | 38 `house_pluck`, 39 `bell_pluck`, 40 `marimba`, 41 `plucked_string`, 42 `future_pluck`, 43 `glass_pluck`, 44 `trance_pluck`, 45 `pizzicato`, 46 `kalimba` |
| keys | 47 `polysynth_piano`, 48 `clavinet`, 49 `fm_electric_piano`, 50 `drawbar_organ`, 51 `juno_keys`, 52 `wurly`, 53 `vibraphone` |
| brass | 54 `synth_brass`, 55 `brass_stab`, 56 `jump_brass`, 57 `rave_chord_stab`, 58 `soft_horns`, 59 `fanfare` |
| effects | 60 `riser`, 61 `downlifter`, 62 `noise_sweep`, 63 `laser_zap`, 64 `random_bleeps`, 65 `wind`, 66 `impact_boom`, 67 `siren` |

`manguebyte_dsp::instruments::poly` describes each. **Modes.** A preset is
polyphonic, monophonic or legato, as its settings say. A part keeps voices
for both: eight for the polyphonic presets, of which a preset plays as many
as `poly.voices` says, and one for the others. A monophonic preset (Moog
Bass, 808 Glide, Acid Line, the mono leads...) glides into the next note
and strikes its envelopes again from where they are; a legato one glides
without striking them again when the next note on comes before the last
note's note off, as Aratu writes a slide. A preset of another mode
releases the notes of the one before, which ring on as they release.

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `poly.preset` | | 0 to 67, as above | 0 | choice |
| `poly.cutoff` | oct | -4 to 4, moves both filters' cutoffs | 0 | linear |
| `poly.resonance` | | -1 to 1, added to both filters' resonance | 0 | linear |
| `poly.attack` | x | 0.1 to 10, on the amplifier envelope's attack | 1 | log |
| `poly.release` | x | 0.1 to 10, on the amplifier envelope's release | 1 | log |
| `poly.detune` | x | 0 to 2, on every oscillator's unison detune | 1 | linear |
| `poly.spread` | x | 0 to 2, on every oscillator's unison spread across the stereo field | 1 | linear |
| `poly.mod_wheel` | | 0 to 1, the modulation wheel the preset's matrix reads (vibrato on the supersaw, the wobble's depth...) | 0 | linear |
| `poly.voices` | | 1 to 8, how many notes a polyphonic preset plays at once | the preset's | whole |

A preset puts the macros back to neutral and `poly.voices` to the
preset's; the wheel stays where it is. Synced LFOs follow `master.tempo`.
The velocity and an accent are the preset's: louder (24 dB of velocity at
most, an accent of 3 dB by default), and wherever its matrix reads them,
brighter. "Plucked String" plays narrower, 0.2 of the velocity range and a
1.3 dB accent: its comb is excited by a burst of noise seeded by the note,
which for an accented G4 at full velocity peaks 10 dB above the same note
at 0.8 and would otherwise reach the limiter at the loudness target.

### Mallets (`mallet`)

`MalletVoice`, struck bars, tines, plates and tubes by modal synthesis:
mallet hardness, strike position, resonator tubes, the vibraphone's motor
and damper. Twelve voices.

| `mallet.preset` | |
|---|---|
| 0 `marimba` | rosewood bars tuned 1 : 4 : 10, yarn mallets, tubes that bloom; no damper |
| 1 `xylophone` | harder bars tuned 1 : 3 : 6, hard mallets, short-lived |
| 2 `vibraphone` | aluminium bars, the motor's tremolo, a damper |
| 3 `glockenspiel` | steel bars, bright and long |
| 4 `celesta` | struck plates over resonators, a damper |
| 5 `kalimba` | plucked tines on a box |
| 6 `steel_pan` | a note whose octave grows from its fundamental |
| 7 `tubular_bells` | tubes, the strike tone an octave below the fourth mode, a damper |

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `mallet.preset` | | 0 to 7, as above | 0 | choice |
| `mallet.hardness` | x | 0.25 to 4: harder is a shorter contact, brighter | 1 | log |
| `mallet.decay` | x | 0.25 to 4, on how long the bars ring | 1 | log |
| `mallet.release` | s | 0.01 to 60, a released note's fall, the damper; 60 is none | the preset's | skew 0.35 |
| `mallet.position` | | 0 to 1 of the bar from its end, 0.5 the centre | the preset's | linear |
| `mallet.motor` | Hz | 0 to 16, the vibraphone motor's speed, 0 stopped | the preset's | linear |

A preset puts the factors back to 1 and the release, the position and the
motor to its own. Velocity and an accent strike harder: louder and
brighter.

### Bowed strings (`bowed`)

`BowedString`, a string by digital waveguide bowed with the friction of
rosin: Helmholtz motion, the bow's bite, a player's ear for intonation,
drift and swell, vibrato that waits, a body of air and wood resonances,
sections of several players to a note, pizzicato. Six voices.

| `bowed.preset` | |
|---|---|
| 0 `violin`, 1 `viola`, 2 `cello`, 3 `double_bass` | a soloist |
| 4 `string_section` | several players to a note, tuned and starting a little apart |
| 5 `violin_pizzicato`, 6 `cello_pizzicato`, 7 `double_bass_pizzicato` | plucked |

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `bowed.preset` | | 0 to 7, as above | 0 | choice |
| `bowed.pressure` | | 0 (airy) to 1 (dense, grainy) | the preset's | linear |
| `bowed.position` | | 0.02 (at the bridge, bright) to 0.4 (over the fingerboard, veiled) of the string | the preset's | linear |
| `bowed.vibrato` | ct | 0 to 100, the swing either way | the preset's | linear |
| `bowed.attack` | s | 0.005 to 2, the bow reaching its speed | the preset's | skew 0.35 |
| `bowed.release` | s | 0.02 to 10, a released note's fall | the preset's | skew 0.35 |
| `bowed.tone` | Hz | 500 to 20 000, the final low-pass | the preset's | log |
| `bowed.portamento` | | 0 a slur is a change of finger, 1 the finger slides | the preset's (0) | switch |
| `bowed.legato` | | 0 every note bowed on its own, 1 a note struck while another is held slurs to it | 0 | switch |

A preset moves every knob but `bowed.legato` to its own. With legato on,
Aratu slurs by letting the notes overlap, as on the basses: the bow goes
on, and the finger stops the string at its new length. Velocity is the
bow's speed; an accent bites harder. The violin's pizzicato plays
narrower, 14 dB of velocity and a 2 dB accent instead of 24 and 3: a
finger plucking harder lets the string go sharper, so its loudest pluck
would otherwise reach the limiter at the loudness target.

### Winds (`wind`)

`WindVoice`, a reed, the lips or an air jet driving a bore by digital
waveguide, with breath noise, tonguing, a player's ear, drift and swell,
vibrato, and legato that changes notes as keys and valves do. Four
voices.

| `wind.preset` | 0 `flute`, 1 `clarinet`, 2 `saxophone`, 3 `trumpet`, 4 `trombone` |
|---|---|

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `wind.preset` | | 0 to 4, as above | 0 | choice |
| `wind.noise` | | 0 to 1, how much noise the breath carries | the preset's | linear |
| `wind.vibrato` | ct | 0 to 100, the swing either way | the preset's | linear |
| `wind.attack` | s | 0.002 to 1, the breath building | the preset's | skew 0.35 |
| `wind.release` | s | 0.005 to 2, the breath stopping | the preset's | skew 0.35 |
| `wind.tone` | Hz | 500 to 20 000, the final low-pass | the preset's | log |
| `wind.portamento` | | 0 a slur changes the note at once, as keys and valves do; 1 it glides | 0 | switch |
| `wind.legato` | | 1 one player: a note struck while another is held slurs to it; 0 a section, every note its own | 1 | switch |

A preset moves every knob but `wind.legato` to its own. A wind is one
player by default: overlapping notes slur on one voice, the breath going
on, and chords sound only with `wind.legato` at 0. Velocity is the breath:
louder and brighter; an accent pushes harder.

### Choir (`choir`)

`ChoirVoice`, sung vowels by formant synthesis: a glottal pulse through
five formants, from basses to sopranos by the key, several detuned
singers to a note. Six voices.

| `choir.preset` | 0 `aah_choir`, 1 `ooh_choir`, 2 `solo_voice` |
|---|---|

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `choir.preset` | | 0 to 2, as above | 0 | choice |
| `choir.vowel` | | 0 to 4: a, e, i, o, u, and blends between | the preset's | linear |
| `choir.vibrato` | ct | 0 to 200, the swing either way | the preset's | linear |
| `choir.attack` | s | 0.005 to 2 | the preset's | skew 0.35 |
| `choir.release` | s | 0.01 to 4 | the preset's | skew 0.35 |
| `choir.breathiness` | | 0 to 1 | the preset's | linear |
| `choir.brightness` | Hz | 500 to 12 000, the voices' brightness at full velocity | the preset's | log |
| `choir.legato` | | 0 every note sung on its own, 1 a note struck while another is held slurs to it | 0 | switch |

A preset moves every knob but `choir.legato` to its own. A vowel changes
while the voices sing, over the preset's time. Velocity sings louder and
brighter; an accent louder.

### What the voices cost

The newer voices are costly. What one
sounding voice of each costs in the WebAssembly module, built for
release, at 48 kHz in quanta of 128 frames as an `AudioWorklet` renders
them, in Node.js on a 15 W laptop processor (an Intel Core i3-1215U), as a
share of one core: `examples/voice_cost.mjs`, the least of five runs, over
four sessions. The machine was busy with other work, and the sessions
differed by up to half again; the ranges are theirs.

| Voice | Per voice | The part at its voices |
|---|---|---|
| drum kit, the acoustic crash ringing | 8 to 11% | |
| drum kit, the acoustic kit's crash, ride, open hat and toms ringing together | 3.1 to 6.2% | 12 voices: up to 75% |
| drum kit, the 909's | 1.6 to 2.3% | 20 to 28% |
| drum kit, the lo-fi kit's (its sampler on every piece) | 7.8 to 10% | 93 to 120% |
| `poly`, one held note of each preset | 2.9% (Laser Zap) to 16% (Wavetable Growl) | 4 to 8 voices by preset: 40% at most |
| `poly`, Trance Supersaw, String Machine, Warm Analog Pad, Evolving Spectra in chords of eight | 10 to 14% | at their 4 to 6 voices: 40 to 80% |
| `poly`, the init patch, one saw through the ladder | 6.0% | |
| `poly`, sixteen unison saws, no filter | 5.9% | |
| `poly`, sixteen unison wavetables, no filter | 9.3% | |
| `poly`, three oscillators of sixteen unison voices, two driven filters | 32% | |
| mallet, vibraphone, tubular bells | 0.4 to 0.6% | 12 voices: 7% |
| bowed, violin, cello | 2.8 to 3.8% | 6 voices: 17 to 23% |
| bowed, string section | 5.9 to 7.4% | 6 voices: 36 to 44% |
| wind, flute, trumpet | 2.5 to 4.8% | 4 voices: 10 to 19%; legato, one: 3 to 5% |
| choir, "aah" and "ooh" | 4.8 to 7.1% | 6 voices: 29 to 43% |
| choir, solo voice | 1.6 to 2.7% | 10 to 16% |

A part costs about 0.15% with nothing sounding. No preset of the library
goes past seven unison voices an oscillator; the sixteen-voice patches,
which only code can make, are timed through `examples/unison_cost.rs`.
Unison voices are cheap; the oversampled filters and saturation, and the
wavetables, are what cost.

**Polyphony limits.** So that a live set stays in real time, each part is
capped: the drum kits at 12 voices, the mallets at 12, the bowed strings
and the choir at 6, the winds at 4 (one while `wind.legato` is on and the
notes overlap). A polyphonic preset of the synthesiser plays as many
voices as keep the part within 40% of a core on that laptop, by what one
held note of it cost (the least of three sessions), four at the least (a
triad and a note ringing on) and eight at the most: `POLY_PRESET_VOICES`
in `src/modelled.rs`, which a preset sets `poly.voices` to: 8 for
Marimba, Plucked String, Laser Zap and Random Bleeps, 7 for Glass Pad,
Drawbar Organ and Siren, 4 for most pads, keys and brass. `poly.voices` takes it up to 8
or down to 1. A note past a part's voices takes the one that will be
missed least over, fading it out.

On such a laptop a set of a 909 groove (four pieces ringing: 8%), a bass
(two voices: 20%), a pad (four voices: 40 to 50%), a lead (two voices:
20%) and a string section (four notes: 25%) comes to more than a whole
core, and a browser's audio thread wants to stay well under half of its
quantum. Keep the costly presets and the full sections few there: a
machine three times as fast takes a set of that kind.
`examples/voice_cost.mjs` measures the machine at hand.

### The synth, the drums and the samplers

| Parameter | Unit | Range | Default | Scale |
|---|---|---|---|---|
| `synth.wave` | | 0 saw, 1 square, 2 triangle, 3 sine | 0 | choice |
| `synth.detune` | ct | 0 to 50, between the two oscillators | 9 | linear |
| `synth.sub` | | 0 to 1 | 0.35 | linear |
| `synth.cutoff` | Hz | 40 to 16 000 | 900 | log, 10 ms |
| `synth.resonance` | | 0 to 1 | 0.25 | linear |
| `synth.env_amount` | oct | 0 to 6 | 2.5 | linear |
| `synth.filter_decay` | s | 0.01 to 4 | 0.35 | skew 0.35 |
| `synth.attack`, `synth.decay` | s | 0.001 to 4, 0.01 to 8 | 0.003, 0.6 | skew 0.35 |
| `synth.sustain` | | 0 to 1 | 0.5 | linear |
| `synth.release` | s | 0.005 to 8 | 0.25 | skew 0.35 |
| `kit.tune` (drums, sampler kit, drum machine) | st | -12 to 12 | 0 | linear |
| `kit.decay` | x | 0.25 to 4 | 1 | log |
| `kick.level`, `snare.level`... `guitar_tap.level` | | 0 to 1 | 0.8 | linear |
| `sampler.interpolation` | | 0 cubic, 1 sinc | 0 | choice |
| `sampler.normalize` (sampler, sampler kit) | | 0 off: the samples as they are; 1 on: at the make-up measured as the instrument loaded ([Loudness](#loudness)) | 1 | switch, 20 ms |

### Demo

`cargo run -p manguebyte-web --example aratu_song -- <directory>` writes
eight bars of an Aratu-like song through the whole engine, the drum
machine, an acid line with accents and slides, organ chords with the
speaker sped up and slowed down, and a strummed steel guitar, and each part
alone.

`cargo run --release -p manguebyte-web --example new_voices_songs --
<directory>` writes a song of eight bars for each family of the newer
voices, scaled to peak at -3 dBFS: a techno groove on the 909 kit with a
rolling bass and chord stabs the wheel opens, a trap beat with hat rolls
over the 808 glide bass sliding, a house groove with the shaker, a garage
bass and a warm pad, a lo-fi beat with a vibraphone and an electric piano,
and an orchestral bit of a string section, cellos, a clarinet slurring
its tune, a flute, a marimba, an "aah" choir and the acoustic kit's toms
and crash.

## Sampler parts

A sampler part (`InstrumentKind::Sampler`, code 2, `'sampler'` in
`js/engine.js`) plays an SFZ instrument through `manguebyte-sampler`. It
is silent until it has one:

```js
await engine.createPart(4, 'sampler');
const { regions, warnings } = await engine.loadSfz(4, 'samples/piano.grand/voice.sfz');
engine.noteOn(4, 60, 0.8, context.currentTime + 0.05);
engine.setParam(4, 'sampler.normalize', 0); // as recorded, off the loudness target
```

`loadSfz` resolves once the part plays the instrument, as soon as its
samples are decoded: a note scheduled after that uses it, while the notes
of the instrument it replaces ring on with that one to their end. The
instrument's [loudness](#sampler-instruments) is measured after that, while
it plays, and the promise has a second one, `normalized`, for when it is
done (see [How an instrument is loaded](#how-an-instrument-is-loaded)):

```js
const loading = engine.loadSfz(4, 'samples/kit/voice.sfz');
await loading;                              // playable: notes from now on use it
const { makeUp } = await loading.normalized; // at the loudness target, makeUp dB
```

`loadSfz` rejects, naming the file, if a fetch fails or a file is not valid
SFZ, WAV, AIFF or FLAC, and the part keeps what it had; `normalized`
rejects then too, and if the part is given another instrument, made again
or removed before the measurement ends. Natively, `SfzLoad` does the same
work and `Engine::load_instrument` installs the result, measured;
`Engine::install_instrument` and `Engine::measure_instrument` do it the
way the worklet does, the instrument playing while it is measured.

- A sample plays at its own rate, read at the ratio of that rate to the
  context's: a 24 kHz sample in a 48 kHz context is read at half speed,
  so nothing is resampled while loading. The reading between frames is
  cubic, or a windowed sinc with `sampler.interpolation` set to 1 (cleaner
  far up the spectrum, about 3.6 times the cost; see below).
- A sample that peaks at full scale peaks at -6 dB (`SAMPLER_LEVEL_DB`) at
  full velocity, which follows the SFZ square law (`amp_veltrack=100`),
  before the make-up gain the part's loudness normalisation measured when
  the instrument loaded, which brings it to the loudness target
  ([Sampler instruments](#sampler-instruments)).
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
`sampler.interpolation` and `sampler.normalize`, whose make-up is the
sampled half's: the borrowed drums keep the drums' own, as in a `drums`
part. Choke stops both halves; all notes off releases
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
(`LOAD_BYTES_PER_QUANTUM`, always at least one file). A load then goes
through three moments:

```text
decode          a file or so a quantum, between render quanta
  → playable    one quantum: the instrument is made and handed to the part,
                at a provisional make-up; loadSfz resolves
  → normalised  27 quanta of 8 192 frames of the phrase measured, 75 ms at
                48 kHz; the part glides to the make-up measured over 20 ms,
                and loading.normalized resolves
```

When the last file is in, one quantum makes the instrument and hands it to
the part (`mb_sfz_install`) through the sampler's lock-free
single-producer single-consumer loader, which the sampler reads when it
next renders, so a note on the very next frame plays it. The same quantum
sets up the measurement of the part's [loudness](#sampler-instruments) on
a clone of the instrument, which shares its samples, and estimates the
make-up, which the part plays at until the measurement is done. Each
quantum after that with no file to decode renders 8 192 frames of the
phrase (`MEASURE_FRAMES_PER_QUANTUM`, `mb_sampler_measure`), so that
instruments loading together all become playable before any is measured;
the last step brings the part to the make-up measured, gliding over the
20 ms that switching `sampler.normalize` takes (at once if nothing
sounds). Notes that start before the hand-over play the instrument the
part had, and ring on with it; notes after it play the new one, at the
provisional make-up and then at the measured one, and so do the old
instrument's notes still ringing, since the make-up is a part gain. The
instrument a part replaces goes back through the loader and is freed by
`mb_collect`, which the worklet calls between quanta. The render call never
allocates, frees, waits or measures.

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

## Loudness

Every kind of instrument and every preset plays equally loud: its
standard phrase comes out of a part at **-24 LUFS** integrated loudness
(ITU-R BS.1770-4, `LOUDNESS_TARGET_LUFS`), within 1 LU
(`LOUDNESS_TOLERANCE_LU`). A sampler part brings each instrument there as
it loads.

### The target

-24 LUFS is the loudest target at which every kind's loudest single
note, accented at full velocity, still stays under the master limiter's
ceiling (-0.3 dBTP), so that the limiter leaves one note alone, as the
engine always promised. The drum machine sets it: its velocity and accent
span 9 dB, so its snare struck that hard is 23.4 dB above the loudness of
its groove. At -18 LUFS, the first figure considered, that snare would
peak at +5.4 dBTP and the synthesized kick and snare at +0.8, every
accented drum hit going into the limiter. -24 sits by the -23 LUFS that
EBU R 128 asks of a whole programme (and the -24 of ATSC A/85), and where
most of the engine's melodic parts already were (the synth and the nylon
guitar at -22.6); a few parts together come to about -18 LUFS, and
`master.gain` takes a set further, the limiter catching what it must.

### The method

Each kind is measured on a standard phrase suited to it (`Phrase`, two
bars at 120 bpm, every note at velocity 0.8, none accented, measured for
4.5 s from the first note):

| Phrase | Notes | Kinds and presets |
|---|---|---|
| `melody` | C4 E4 G4 A4 G4, E4 D4 C4 D4 E4 C4, in quarters and eighths held for nine tenths of their length | synth, nylon and steel guitar, tubular bell, sampler; the synthesiser's leads, plucks and effects; every mallet; violin, viola, cello, the pizzicatos of the violin and the cello; every wind; the solo voice |
| `bass` | the melody two octaves down, around C2 | finger and slap bass, FM bass, kick bass, the three acid presets; the synthesiser's basses; the double bass and its pizzicato |
| `chord` | C4 E4 G4 for a bar, A3 C4 E4 for a bar, held for nine tenths | electric piano, the four organ presets; the synthesiser's pads, keys and brass; the string section; the "aah" and "ooh" choirs |
| `groove` | the closed hat on every eighth, the kick on beats 1 and 3, the snare on 2 and 4 | drums, drum machine, sampler kit, every drum kit |

The part has a neutral chain (no effect, gain 0 dB, centred, no send),
and the master limiter is kept out of it: the master gain is turned down
20 dB, the readings are put back up by as much, and every measurement
checks that the limiter did nothing. The meter is `manguebyte-analysis`'s
`LoudnessMeter`, on the part's two channels. Each kind and preset then
plays at a make-up gain, the target minus what it measured, on top of
what its voices put out: a pure gain, so velocity and accents keep their
dynamics exactly (`tests/voices.rs` still measures them). A preset's
make-up comes with its settings, on the frame the preset lands on: the
presets being as loud as each other, a note sounding through a change of
preset keeps about its level. The synthesized drums carry theirs wherever
they play, so the drums a sampler kit or the drum machine borrows match a
`drums` part. The table is `InstrumentKind::calibration` (one
`Calibration` per preset, in the order of the preset's values, with
`InstrumentKind::preset_param` naming the parameter); only each kind's
own knobs at their defaults are calibrated, so a knob that changes the
loudness (`synth.wave`, the drawbars, `acid.drive`) still does.

### Measured

`cargo test -p manguebyte-web --test loudness -- --nocapture` prints
this. Before is each kind as it played until now; the peak is the true
peak, after calibration, of the loudest single note of the phrase's keys
(and every drum of the map for a kit), accented at full velocity.

| Kind | Preset | Phrase | Before, LUFS | Make-up, dB | After, LUFS | Accented peak, dBTP |
|---|---|---|---|---|---|---|
| `synth` | | melody | -22.57 | -1.43 | -24.00 | -6.5 |
| `drums` | | groove | -19.24 | -4.76 | -24.00 | -5.2 |
| `plucked` | nylon guitar | melody | -22.58 | -1.42 | -24.00 | -8.3 |
| `plucked` | steel guitar | melody | -32.96 | +8.96 | -24.00 | -5.4 |
| `plucked` | finger bass | bass | -21.09 | -2.91 | -24.00 | -6.9 |
| `plucked` | slap bass | bass | -24.35 | +0.35 | -24.00 | -6.6 |
| `fm` | electric piano | chord | -25.06 | +1.06 | -24.00 | -10.3 |
| `fm` | tubular bell | melody | -23.32 | -0.68 | -24.00 | -13.9 |
| `fm` | FM bass | bass | -17.74 | -6.26 | -24.00 | -11.9 |
| `organ` | jazz | chord | -22.58 | -1.42 | -24.00 | -16.9 |
| `organ` | gospel | chord | -21.65 | -2.35 | -24.00 | -16.1 |
| `organ` | full | chord | -20.33 | -3.67 | -24.00 | -16.8 |
| `organ` | ballad | chord | -21.93 | -2.07 | -24.00 | -19.0 |
| `analog_kit` | | groove | -24.26 | +0.26 | -24.00 | -0.6 |
| `kick_bass` | | bass | -14.09 | -9.91 | -24.00 | -13.8 |
| `acid` | classic | bass | -20.30 | -3.70 | -24.00 | -7.4 |
| `acid` | squelch | bass | -22.00 | -2.00 | -24.00 | -6.3 |
| `acid` | square | bass | -18.34 | -5.66 | -24.00 | -10.5 |
| `drum_kit` | Acoustic | groove | -26.76 | +2.76 | -24.00 | -0.7 |
| `drum_kit` | 808 | groove | -26.26 | +2.26 | -24.00 | -1.6 |
| `drum_kit` | 909 | groove | -27.34 | +3.34 | -24.00 | -1.3 |
| `drum_kit` | 606 | groove | -28.09 | +4.09 | -24.00 | -1.2 |
| `drum_kit` | CR-78 | groove | -28.54 | +4.54 | -24.00 | -1.2 |
| `drum_kit` | Trap | groove | -23.04 | -0.96 | -24.00 | -4.1 |
| `drum_kit` | Techno | groove | -26.78 | +2.78 | -24.00 | -1.9 |
| `drum_kit` | House | groove | -27.58 | +3.58 | -24.00 | -1.1 |
| `drum_kit` | Drum and bass | groove | -25.85 | +1.85 | -24.00 | -1.9 |
| `drum_kit` | Lo-fi | groove | -28.47 | +4.47 | -24.00 | -4.3 |
| `poly` | Moog Bass | bass | -21.31 | -2.69 | -24.00 | -6.4 |
| `poly` | Reese | bass | -14.53 | -9.47 | -24.00 | -12.8 |
| `poly` | Wobble Bass | bass | -17.22 | -6.78 | -24.00 | -10.4 |
| `poly` | Deep Sub | bass | -12.38 | -11.62 | -24.00 | -15.2 |
| `poly` | FM Growl | bass | -14.17 | -9.83 | -24.00 | -13.1 |
| `poly` | House Organ Bass | bass | -16.53 | -7.47 | -24.00 | -10.7 |
| `poly` | Acid Line | bass | -16.08 | -7.92 | -24.00 | -10.3 |
| `poly` | Garage Square | bass | -16.79 | -7.21 | -24.00 | -10.8 |
| `poly` | Wavetable Growl | bass | -12.64 | -11.36 | -24.00 | -15.4 |
| `poly` | Synthpop Bass | bass | -15.70 | -8.30 | -24.00 | -12.0 |
| `poly` | Funk Bass | bass | -24.85 | +0.85 | -24.00 | -1.8 |
| `poly` | 808 Glide | bass | -12.86 | -11.14 | -24.00 | -14.7 |
| `poly` | Rolling Bass | bass | -21.34 | -2.66 | -24.00 | -4.8 |
| `poly` | Rubber Bass | bass | -17.09 | -6.91 | -24.00 | -10.0 |
| `poly` | Talking Bass | bass | -17.22 | -6.78 | -24.00 | -10.0 |
| `poly` | Trance Supersaw | melody | -15.96 | -8.04 | -24.00 | -12.0 |
| `poly` | Square Lead | melody | -12.87 | -11.13 | -24.00 | -14.7 |
| `poly` | Sync Lead | melody | -12.06 | -11.94 | -24.00 | -14.9 |
| `poly` | Chiptune | melody | -13.05 | -10.95 | -24.00 | -17.0 |
| `poly` | Hoover | melody | -17.21 | -6.79 | -24.00 | -10.1 |
| `poly` | Mono Saw Lead | melody | -16.04 | -7.96 | -24.00 | -11.4 |
| `poly` | Whistle Lead | melody | -12.67 | -11.33 | -24.00 | -14.9 |
| `poly` | Digital Lead | melody | -13.30 | -10.70 | -24.00 | -14.0 |
| `poly` | Talking Lead | melody | -21.17 | -2.83 | -24.00 | -5.8 |
| `poly` | Screamer | melody | -10.43 | -13.57 | -24.00 | -17.1 |
| `poly` | Future Lead | melody | -14.48 | -9.52 | -24.00 | -11.8 |
| `poly` | Warm Analog Pad | chord | -10.85 | -13.15 | -24.00 | -20.2 |
| `poly` | String Machine | chord | -13.13 | -10.87 | -24.00 | -13.9 |
| `poly` | Glass Pad | chord | -12.51 | -11.49 | -24.00 | -22.9 |
| `poly` | Dark Drone | chord | -9.88 | -14.12 | -24.00 | -29.7 |
| `poly` | Choir Pad | chord | -10.86 | -13.14 | -24.00 | -20.3 |
| `poly` | Formant Pad | chord | -8.42 | -15.58 | -24.00 | -18.8 |
| `poly` | PWM Pad | chord | -8.32 | -15.68 | -24.00 | -19.8 |
| `poly` | Shimmer Pad | chord | -13.68 | -10.32 | -24.00 | -17.9 |
| `poly` | Evolving Spectra | chord | -12.81 | -11.19 | -24.00 | -26.8 |
| `poly` | Soft Sine Pad | chord | -9.90 | -14.10 | -24.00 | -20.5 |
| `poly` | Comb Wash | chord | -9.66 | -14.34 | -24.00 | -25.5 |
| `poly` | Sweep Pad | chord | -12.53 | -11.47 | -24.00 | -34.9 |
| `poly` | House Pluck | melody | -22.12 | -1.88 | -24.00 | -4.8 |
| `poly` | Bell Pluck | melody | -12.29 | -11.71 | -24.00 | -15.3 |
| `poly` | Marimba | melody | -15.85 | -8.15 | -24.00 | -10.8 |
| `poly` | Plucked String | melody | -24.16 | +0.16 | -24.00 | -1.0 |
| `poly` | Future Pluck | melody | -18.88 | -5.12 | -24.00 | -7.3 |
| `poly` | Glass Pluck | melody | -16.85 | -7.15 | -24.00 | -10.5 |
| `poly` | Trance Pluck | melody | -21.92 | -2.08 | -24.00 | -1.7 |
| `poly` | Pizzicato | melody | -26.00 | +2.00 | -24.00 | -1.5 |
| `poly` | Kalimba | melody | -14.33 | -9.67 | -24.00 | -13.1 |
| `poly` | Polysynth Piano | chord | -18.17 | -5.83 | -24.00 | -7.7 |
| `poly` | Clavinet | chord | -23.60 | -0.40 | -24.00 | -2.9 |
| `poly` | FM Electric Piano | chord | -14.80 | -9.20 | -24.00 | -11.9 |
| `poly` | Drawbar Organ | chord | -8.58 | -15.42 | -24.00 | -21.4 |
| `poly` | Juno Keys | chord | -13.78 | -10.22 | -24.00 | -14.4 |
| `poly` | Wurly | chord | -13.93 | -10.07 | -24.00 | -12.2 |
| `poly` | Vibraphone | chord | -14.75 | -9.25 | -24.00 | -13.0 |
| `poly` | Synth Brass | chord | -12.66 | -11.34 | -24.00 | -15.7 |
| `poly` | Brass Stab | chord | -24.01 | +0.01 | -24.00 | -3.0 |
| `poly` | Jump Brass | chord | -11.61 | -12.39 | -24.00 | -16.6 |
| `poly` | Rave Chord Stab | chord | -23.02 | -0.98 | -24.00 | -4.7 |
| `poly` | Soft Horns | chord | -10.69 | -13.31 | -24.00 | -16.8 |
| `poly` | Fanfare | chord | -12.30 | -11.70 | -24.00 | -16.3 |
| `poly` | Riser | melody | -18.51 | -5.49 | -24.00 | -12.7 |
| `poly` | Downlifter | melody | -15.81 | -8.19 | -24.00 | -10.4 |
| `poly` | Noise Sweep | melody | -29.07 | +5.07 | -24.00 | -10.3 |
| `poly` | Laser Zap | melody | -19.99 | -4.01 | -24.00 | -6.5 |
| `poly` | Random Bleeps | melody | -14.31 | -9.69 | -24.00 | -13.0 |
| `poly` | Wind | melody | -20.75 | -3.25 | -24.00 | -15.7 |
| `poly` | Impact Boom | melody | -10.88 | -13.12 | -24.00 | -16.6 |
| `poly` | Siren | melody | -8.25 | -15.75 | -24.00 | -19.6 |
| `mallet` | Marimba | melody | -16.82 | -7.18 | -24.00 | -11.1 |
| `mallet` | Xylophone | melody | -21.46 | -2.54 | -24.00 | -8.6 |
| `mallet` | Vibraphone | melody | -20.32 | -3.68 | -24.00 | -10.4 |
| `mallet` | Glockenspiel | melody | -17.48 | -6.52 | -24.00 | -11.1 |
| `mallet` | Celesta | melody | -21.00 | -3.00 | -24.00 | -11.0 |
| `mallet` | Kalimba | melody | -14.61 | -9.39 | -24.00 | -9.2 |
| `mallet` | Steel pan | melody | -15.19 | -8.81 | -24.00 | -10.1 |
| `mallet` | Tubular bells | melody | -13.46 | -10.54 | -24.00 | -7.0 |
| `bowed` | Violin | melody | -29.28 | +5.28 | -24.00 | -3.1 |
| `bowed` | Viola | melody | -28.96 | +4.96 | -24.00 | -4.2 |
| `bowed` | Cello | melody | -29.19 | +5.19 | -24.00 | -5.7 |
| `bowed` | Double bass | bass | -30.54 | +6.54 | -24.00 | -4.0 |
| `bowed` | String section | chord | -23.73 | -0.27 | -24.00 | -12.1 |
| `bowed` | Violin pizzicato | melody | -35.59 | +11.52 | -24.07 | -1.0 |
| `bowed` | Cello pizzicato | melody | -36.33 | +12.33 | -24.00 | -1.3 |
| `bowed` | Double bass pizzicato | bass | -32.75 | +8.75 | -24.00 | -1.8 |
| `wind` | Flute | melody | -27.27 | +3.27 | -24.00 | -11.6 |
| `wind` | Clarinet | melody | -21.52 | -2.48 | -24.00 | -9.3 |
| `wind` | Saxophone | melody | -20.69 | -3.31 | -24.00 | -12.9 |
| `wind` | Trumpet | melody | -23.75 | -0.25 | -24.00 | -7.8 |
| `wind` | Trombone | melody | -22.09 | -1.91 | -24.00 | -8.9 |
| `choir` | Aah choir | chord | -14.44 | -9.56 | -24.00 | -11.6 |
| `choir` | Ooh choir | chord | -16.49 | -7.51 | -24.00 | -14.6 |
| `choir` | Solo voice | melody | -15.01 | -8.99 | -24.00 | -10.3 |
| `sampler` | a recorded fixture at -6 dBFS | melody | -22.80 | -1.20, measured | -24.00 | -10.6 |
| `sampler_kit` | a recorded fixture kit | groove | -27.12 | +3.12, measured | -24.00 | -5.3 |

The worst case is the drum machine's snare at -0.6 dBTP, then the
acoustic drum kit's kick at -0.7: no single note of any kind reaches the
limiter. The spread before was 18.9 LU, from the steel guitar 9 LU under
the target to the kick bass 10 over; the newer voices spread over 27 LU,
from the violin's pizzicato 11.6 LU under it to the PWM pad 15.7 over,
and every one of their 102 rows lands within 0.07 LU of it. The "before"
of the drum kits, the violin's pizzicato and Plucked String is at their
narrowed dynamics ([Drum kits](#drum-kits-drum_kit)).

### The table test

`tests/loudness.rs` plays every row of the table on its phrase and fails
for one more than 1 LU from the target, saying what its make-up should
be; it measures the accented peaks above and fails if one reaches the
ceiling. A new kind does not compile until `calibration` and
`preset_param` have their arms; a preset added to a kind's list of names
does not compile until its row is there (the rows are arrays as long as
the lists); and a unit test checks each row against the instrument's
preset parameter, any whose id ends in `.preset`, by count and by name.
To calibrate a new row, give it a make-up of 0, run the command above,
and put in the make-up the failure asks for.

### Sampler instruments

Recorded instruments come at any level, so a sampler part measures each
one when it loads, and plays it at the make-up that brings it to the
target (`Engine::sampler_make_up_db` says what that was). The instrument
plays its kind's phrase offline, into a scratch buffer, through
`SamplerVoice`s of the same sampler code the part plays it with, and the
same meter reads it:

- a melodic instrument plays the melody, moved by whole octaves to where
  it has the most of its notes, each note it has no region for played on
  the nearest key it has;
- a sampler kit plays the groove on the drums of it that it has (the
  drums it borrows from the synthesized kit are calibrated already); a
  kit with none of them, a pandeiro, say, plays its own keys in turn, one
  on each eighth note of the groove.

Rendering the phrase was chosen over measuring the samples, weighted by
the keys they cover, because it hears what sets the level of what is
played and the samples alone do not say: the regions' `volume` and
`amplitude`, their velocity tracking at velocity 0.8, envelopes, loops,
layered regions and how the regions share the keyboard; and it costs
about the same, a pass over the sound. The voices see one note each, so
choke groups between notes and `polyphony` limits are left out of the
measurement, which a phrase of single notes or short drums barely
touches. The make-up is held back where the phrase's loudest frame,
struck at full velocity and accented (3.9 dB more under the square law,
and the accent's 3 dB), would pass -1 dBFS (`NORMALIZE_PEAK_DB`), so a
very spiky instrument plays under the target rather than into the
limiter, and it stays within 24 dB either way (`NORMALIZE_RANGE_DB`).
Two fixture instruments recorded 12 dB apart come out at -24.00 LUFS
both, melodic or a kit (`tests/loudness.rs`).

It is `sampler.normalize`, on by default. Off, the part plays the samples
as they are, at `SAMPLER_LEVEL_DB`; switched while notes sound, the level
glides over 20 ms, at once in silence, the same in every quantum. The
make-up is a part gain on the sampled half, so an instrument loaded while
the notes of the one it replaces ring on brings those notes to its
make-up too, over 20 ms. An instrument loaded while normalisation is off
is not measured, and plays as recorded even if it is switched on later;
`Engine::load_instrument_unmeasured` loads one as it is whatever the
switch says, for instruments whose levels are set already.

The measurement renders 4.5 s of sound and allocates, so it is never the
render call's work (`tests/realtime.rs` proves it, with the part playing
while it is measured): natively `Engine::load_instrument` does it at once,
`Engine::measure_instrument` a step at a time, and in a browser the
worklet does it between render quanta (see
[How an instrument is loaded](#how-an-instrument-is-loaded)). In a debug
build in Node.js, a step took 8 to 9 ms against 10 to 13 ms for decoding
a 64 KiB file, which is what the steps were sized to.

**Before the measurement ends** the part already plays the instrument, at
a provisional make-up. Three were possible: 0 dB, the samples as
recorded, is off by the whole make-up, -11 to +11 dB on the fixtures; the
make-up of the instrument the part had before says nothing about this one
(a quiet piano followed by a hot kit would start some 30 dB too loud); so
the part plays at an estimate, which costs almost nothing. It is the
energy each note of the phrase reads from the samples
(`Instrument::note_energy` of `manguebyte-sampler`: the regions' gains,
velocity tracking, pan, pitch, loops and layers, round robins and random
ranges averaged), a melodic note held for its length and a drum for a
beat, spread over the phrase's two bars and taken as loud as a signal of
that power is by BS.1770 with its K-weighting flat. It leaves out the
envelopes, the K-weighting, the gate and the hold-back for the peaks.
Working it out reads each frame the phrase's distinct notes play once,
a small part of one step of the measurement, in the quantum that hands
the instrument over. On the eight fixture instruments of
`tests/loudness.rs` (two recorded instruments, melodic and a kit, at
three levels, looped sines and a kit like Aratu's), it lands within
0.22 dB of the make-up measured; the test allows 1 dB.

The measurement plays a clone of the instrument, which shares its decoded
samples and has round robins and random choices of its own, so what the
part plays never depends on how far the measurement has got:
`tests/loudness.rs` plays an instrument whose regions take turns, replacing
one still ringing, in quanta of 1 to 672 frames and with the measurement
stepped between the quanta or not at all until its last step, and gets
the same samples, bit for bit; the output depends only on the frame the
last step is taken before. It measures the glide that follows, 960 frames
in a straight line at 48 kHz from the provisional gain to the measured
one, within 0.01% at every frame, and that with `sampler.normalize` off
nothing changes, bit for bit, whether the measurement ends or not.

### For Aratu

The per-voice gains Aratu keeps on its side can be removed: every kind
and preset now comes out at the same loudness, and its recorded
instruments are measured and brought there as they load. Keep a gain
only where a voice should sit above or below the others in the mix, and
put it in the part's `gain`. The gains measured until now matched peaks,
not loudness: an organ, whose tones are close to sines, peaks some 10 dB
under a plucked string or a drum of the same loudness, which is why it
seemed to need 3.33 times its level, and by BS.1770 it was already as
loud as the synth. Parts that were loud before are quieter now (the
drums by 4.8 dB, the FM bass by 6.3, the kick bass by 9.9), so a set as a
whole may want `master.gain` raised a few decibels; the limiter keeps it
under its ceiling.

## Licence

MIT OR Apache-2.0, like the rest of Manguebyte.
