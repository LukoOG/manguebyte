# Architecture

This document explains how Manguebyte is put together and why. Read it before
adding a crate, changing a core trait, or touching anything that runs on the
audio thread. Individual decisions are recorded in [`adr/`](adr/).

## What we are building

A framework that covers what JUCE covers for C++ (devices, MIDI, DSP, files,
parameters, plugins, user interfaces, hosting) and that is better where JUCE
is known to hurt. [JUCE_PARITY.md](JUCE_PARITY.md) tracks the first half of
that sentence module by module. This document is about the second half.

Four requirements shape everything:

| Requirement | What it means in practice |
|---|---|
| **Real-time safe by construction** | Code on the audio thread never allocates, locks, blocks or panics, and the framework never does so on the user's behalf. Tests enforce it. |
| **Light** | You pay only for what you use. Each area is its own crate; the core has no dependencies; nothing needs a GUI or an event loop to process audio. |
| **Complete** | One coherent set of types from the sound card to the plugin format, so the parts fit without glue. |
| **Scalable** | The same processor runs in a test, an offline render, a standalone app and every plugin format, in `f32` or `f64`. |

## Crates

Dependencies only point downwards. A crate may use anything below it and
nothing beside or above it.

```text
                         manguebyte            facade: re-exports + prelude
                              |
      +-----------+-----------+-----------+-----------+
      |           |           |           |           |
   devices     formats       dsp        params       midi
   (cpal)                     |           |
      |           |           |           |
      +-----------+-----------+-----------+-----------+
                              |
                            core                no dependencies
```

| Crate | Role | Depends on |
|---|---|---|
| `manguebyte-core` | `AudioBuffer`, `Sample`, smoothing, atomics, wait-free queue | nothing |
| `manguebyte-midi` | MIDI messages and sample-accurate event buffers | nothing |
| `manguebyte-params` | Parameters, ranges, smoothing, saved state | core |
| `manguebyte-dsp` | `Processor`, filters, oscillators, FFT, windows | core |
| `manguebyte-formats` | Audio file reading and writing | core |
| `manguebyte-devices` | Sound card input, output and duplex | core, `cpal` |
| `manguebyte` | Everything under one name | all of the above |

Planned crates slot into the same picture: `manguebyte-plugin` (the
format-independent plugin API) sits on dsp, params and midi; one crate per
plugin format sits on that; `manguebyte-gui` sits beside them and is never a
dependency of anything that processes audio; `manguebyte-graph` and
`manguebyte-host` sit on the plugin API.

Rules that keep this healthy:

- **`core` stays dependency-free.** It is the one crate everybody compiles.
- **Processing never depends on presentation.** JUCE's plugin processing and
  hosting code depended on its GUI modules until 8.0.11, when a headless
  variant was split out. Here that dependency cannot be written: the GUI
  crate is above the plugin API, not below it.
- **A dependency needs a reason.** Platform audio is a reason. Convenience is
  not.

## Threads

Audio software has one thread that must never be late, and several that may
take their time. Every design decision follows from keeping them apart.

```text
   UI / host / worker threads                      audio thread
   (may allocate, lock, block)                (must do none of those)

   set a parameter  ----- atomic store ------>  read the value
   send a command   ----- spsc queue -------->  pop, apply
   show a meter     <---- atomic / spsc ------  publish level
   load a file  -> build new state
                    ----- spsc queue -------->  swap it in
                    <---- spsc queue ---------  send the old one back to be freed
```

What crosses between the two sides:

| Mechanism | Use it for | Type |
|---|---|---|
| Atomic value | One number the other side polls: a parameter, a meter | `AtomicF32`, the `Param` types |
| Wait-free queue | Ordered messages, blocks of audio, ownership of buffers | `spsc::channel` |
| Nothing else | | |

There is no third mechanism. In particular the framework has **no listener
callbacks**: a callback that fires on whichever thread wrote a value is how
user-interface code ends up running on the audio thread. In JUCE, parameter
listeners are invoked synchronously on the calling thread, including the
audio thread during host automation, and the attachment classes respond by
posting to the operating system's message queue, which takes a lock. Here the
audio thread polls a `ParamSmoother` or a `Changed`, and an interface polls on
its own timer. See [ADR 0003](adr/0003-parameters-without-listeners.md).

### The real-time contract

`Processor::prepare` may allocate and take its time. `Processor::process` and
`Processor::reset` run on the audio thread and must not:

- allocate or free memory,
- lock, block or wait,
- do I/O of any kind,
- panic.

This is checked, not hoped for. `crates/manguebyte-dsp/tests/realtime.rs`
installs a counting allocator and fails if any processor touches the heap
while processing. Every new processor is added to that test. The only
`unsafe` code in the framework, the wait-free queue, runs under Miri in CI.

### State

Hosts call for a plugin's state from threads of their own choosing, while
audio is running. JUCE leaves the synchronisation to each plugin author. Here
parameter values are the single source of truth and live in atomics, so
`ParamSet::save` and `ParamSet::load` are safe from any thread at any time
with no lock. Larger state (a sample, a wavetable) follows the swap pattern
in the diagram above: build off-thread, hand over through a queue, return the
old value through a queue to be freed.

## Data flow

```text
  file ---> formats ---+                              +---> formats ---> file
                       |                              |
  sound card -> devices -+-> AudioBuffer -> Processor -+-> devices -> sound card
                       |     (planar)        chain    |
  MIDI port --> midi --+-> MidiBuffer ----^           +---> midi ---> MIDI port
                              params -----^
```

- **`AudioBuffer<S>`** is planar: one contiguous slice per channel, which is
  what DSP loops and SIMD want. Devices and files speak interleaved; the
  conversion happens once, at the edge.
- **`Sample`** is `f32` or `f64`. Processors are generic over it. Coefficient
  design is done in `f64` regardless.
- **`Processor`** transforms a buffer in place. A tuple of processors is a
  processor, resolved at compile time, so a fixed chain costs the same as
  calling each stage by hand and the compiler can inline across stages.
  `Box<dyn Processor>` covers chains decided at run time.
- **`MidiBuffer`** holds events with their position inside the block, so a
  note can start on the exact sample it should.

## What the design learns from JUCE

JUCE is the reference for what a complete audio framework contains, and its
issue tracker, forum and changelog are a record of what goes wrong in one
over twenty years. The table lists the lessons already built in, and the ones
that the plugin API must honour when it is written.

| Observed in JUCE | Decision here | Status |
|---|---|---|
| Parameter listeners run on the audio thread during automation | No listeners; both sides poll atomics | Done |
| Attachments post to the OS message queue from the audio thread | The only bridges are atomics and a wait-free queue | Done |
| State callbacks race with processing; each plugin must synchronise | Parameters are atomic and are the source of truth | Done for parameters |
| Saved state can drift from parameter values | State is derived from the values on demand, never cached | Done |
| Audio thread reads parameters through a string lookup | Typed struct fields; ids are for hosts and interfaces only | Done |
| Processing and hosting depended on GUI modules | The GUI crate sits above the plugin API | By construction |
| No native CLAP | CLAP is the first plugin format, not an add-on | Planned |
| No sample-accurate automation in `AudioProcessor` | The process call carries timestamped events from day one | Planned |
| No MIDI 2.0, note expression or per-voice modulation in the processor model | The event type is designed for them before any format wrapper exists | Planned |
| Real-time safety rests on convention | A counting-allocator test and Miri in CI | Done, grows with the code |

Sources for the left column are in
[ADR 0002](adr/0002-real-time-thread-model.md) and
[ADR 0003](adr/0003-parameters-without-listeners.md). JUCE is studied as a
reference; its code is never copied. See
[ADR 0004](adr/0004-juce-as-reference.md).

## What we would revisit as it grows

- **`Processor::process` takes only a buffer.** It needs a context carrying
  events, transport and separate inputs and outputs before the plugin API can
  be built on it. This is the next architectural step; see
  [TECH_DEBT.md](TECH_DEBT.md).
- **Duplex streams absorb clock drift by dropping or padding.** Once a
  resampler exists, they should correct drift by adaptive resampling.
- **The FFT is scalar radix-2.** Split-radix and SIMD kernels are a
  self-contained optimisation behind the same API.
- **`core` uses `std`** for `Arc` and `Vec`. `no_std` with `alloc` is a
  mechanical change, wanted for embedded targets.
