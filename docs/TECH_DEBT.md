# Technical debt

An audit of what the code owes, so that shortcuts are chosen on purpose and
paid back in order. Update it when an item is fixed or a new one is taken on.

Last audit: 2026-10-13, covering every crate in the workspace.

## Scoring

Each item is scored from 1 to 5 on three axes:

- **Impact**: how much it slows down people using or extending the framework.
- **Risk**: how bad it is if it is never fixed.
- **Effort**: how hard the fix is.

`Priority = (Impact + Risk) × (6 − Effort)`. Higher goes first.

## Open items

| # | Item | Type | Impact | Risk | Effort | Priority |
|---|---|---|---|---|---|---|
| 1 | `Processor::process` takes only a buffer: no MIDI, no timestamped parameter events, no transport, no separate input and output | Architecture | 5 | 5 | 3 | 30 |
| 2 | File parsers (WAV, parameter state, MIDI bytes) are not fuzzed | Test | 2 | 4 | 2 | 24 |
| 3 | The allocation guard covers `dsp` only, not the device block adapters or `ParamSmoother` | Test | 2 | 4 | 2 | 24 |
| 4 | No benchmarks, so a performance regression is invisible | Test | 3 | 3 | 2 | 24 |
| 5 | No dependency audit (`cargo deny`) in CI for licences and advisories | Infrastructure | 2 | 4 | 1 | 30 |
| 6 | `Sample` has almost no maths (`sin`, `exp`, `tanh`, `min`, `max`), so generic code round-trips through `f64` | Code | 3 | 2 | 2 | 20 |
| 7 | WAV is read whole into memory; there is no streaming reader or writer | Architecture | 4 | 2 | 3 | 18 |
| 8 | Duplex streams correct clock drift by dropping or padding, which is audible on long runs with two devices | Code | 2 | 3 | 4 | 10 |
| 9 | `core` needs `std` (`Arc`, `Vec`), which blocks embedded targets | Architecture | 2 | 2 | 2 | 16 |
| 10 | FFT is scalar radix-2; no split-radix, no SIMD | Code | 3 | 1 | 3 | 12 |
| 11 | Device callbacks receive no timing information (stream time, latency) | Architecture | 3 | 2 | 2 | 20 |
| 12 | No CHANGELOG, and nothing is published on crates.io, so the crate names are not reserved | Documentation | 2 | 3 | 1 | 25 |
| 13 | Hardware tests run only by hand (`--ignored`) | Test | 2 | 2 | 4 | 8 |
| 14 | `ChoiceParam` saves an index, so reordering options silently changes old sessions | Code | 2 | 3 | 2 | 20 |
| 15 | The duplex error handler is behind a `Mutex`. It is never taken on the audio path, but it is the one lock in the crate | Code | 1 | 2 | 2 | 12 |

### Why item 1 leads

Everything in Phase 2 (plugins) is built on the process call. JUCE's
`AudioProcessor` cannot express sample-accurate automation, MIDI 2.0, note
expression or per-voice modulation, and retrofitting them is what its
"AudioProcessor v2" effort is about. Adding a context to `Processor` now, while
there are six processors, is a day of work. Adding it after the plugin
wrappers exist would mean changing every wrapper and every user's plugin.

The intended shape:

```rust
pub struct ProcessContext<'a, S: Sample> {
    pub input: &'a AudioBuffer<S>,       // may be empty for generators
    pub output: &'a mut AudioBuffer<S>,
    pub events: &'a EventBuffer,          // notes, parameters, expression: all timestamped
    pub transport: &'a Transport,
}
```

with the current in-place `process(&mut AudioBuffer)` kept as the simple form
for pure effects.

## Paid back in this audit

| Item | What was done |
|---|---|
| Real-time safety rested on doc comments alone | `crates/manguebyte-dsp/tests/realtime.rs` installs a counting allocator and fails if any processor or transform allocates, reallocates or frees while processing |
| The wait-free queue's `unsafe` code was only tested natively | CI runs `manguebyte-core` under Miri, which detects undefined behaviour and data races |
| A build without the audio backend was never exercised | CI builds and tests the facade with `--no-default-features` |
| `ParamSmoother` in `f32` stalled short of its target on long exponential ramps and never reported finishing | State is kept in `f64`; a regression test covers a two-second ramp at 96 kHz |
| Device code duplicated an eleven-arm match per stream direction | One `with_sample_type!` macro shared by output, input and duplex |

## Remediation plan

Small items are done alongside feature work in the area they touch. The
order below is for the ones that need their own change.

**Next (before any plugin work)**

1. Item 1: `ProcessContext`, `EventBuffer` and `Transport`, with the existing
   processors ported.
2. Item 5: `cargo deny` in CI.
3. Item 12: CHANGELOG and a first `0.1.0` release to crates.io.
4. Item 3: extend the allocation guard to `devices` and `params`.

**Soon (during the rest of Phase 1)**

5. Item 6: maths on `Sample`, done together with the filters that need it.
6. Item 7: streaming readers and writers, done with the second file format.
7. Item 11: timing information in device callbacks.
8. Item 2 and item 4: fuzz targets and benchmarks.
9. Item 14: `EnumParam` that saves a stable name per variant.

**Later**

10. Item 9: `no_std` for `core` and `dsp`.
11. Item 8: drift correction, once a resampler exists.
12. Item 10: faster FFT kernels behind the same API.
13. Item 13: a self-hosted runner with a loopback device.
