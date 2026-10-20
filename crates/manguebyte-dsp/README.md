# manguebyte-dsp

Signal processing for the Manguebyte Audio Framework: oscillators, wavetables, filters, EQ, dynamics with sidechain, delay, modulation, reverb, convolution, FIR, oversampling, a polyphonic synth with instrument voices (plucked strings, FM, tonewheel organ, acid bass, mallets by modal synthesis, bowed strings and winds by digital waveguide, choirs by formant synthesis, drum kits: acoustic by modal synthesis, Brazilian percussion by physical modelling, and drum machines, a deep polyphonic synthesiser with 68 presets) and zero-cost processor chains.

Part of the [Manguebyte Audio Framework](https://github.com/alessonqueirozdev-hub/manguebyte). Most projects depend on
[`manguebyte`](https://crates.io/crates/manguebyte), which gathers every part
under one name; depend on this crate directly to use only this part.

Documentation: <https://docs.rs/manguebyte-dsp>

## Transport and tempo map

A host tells a processor where its transport is (`Transport`, in the
`ProcessContext` of every block). For a program that keeps time itself, the
`timeline` module has the other side:

- `TempoMap`: tempo points (steps and linear ramps) and time signatures on
  a musical timeline, with closed-form conversions between frames, seconds,
  quarter notes (PPQ) and bars, beats and ticks, in both directions. A ramp
  is linear in time, as CLAP's `tempo_inc` describes one; the map's
  documentation gives the arithmetic. `TempoMap::from_midi_file` reads a
  Standard MIDI File's tempo map.
- `TransportClock`: plays a map block by block, with play, stop, seeking
  and a loop. Each block gives its `Transport`, the pieces it is cut into
  where the loop goes round, and the frame on which any beat, tempo point
  or bar line lands, so that events go on exact frames. Positions do not
  depend on the block sizes, and advancing never allocates.
- `ClockControl`: works a clock from another thread through the wait-free
  queues of `manguebyte-core`, including handing it a new map.

```rust
use manguebyte_dsp::timeline::{TempoCurve, TempoMap, TimeSignature, TransportClock};

let mut map = TempoMap::new(120.0, TimeSignature::FOUR_FOUR).unwrap();
// A ramp from bar 2 to 140 bpm at bar 4.
map.set_tempo(8.0, 120.0, TempoCurve::Linear).unwrap();
map.set_tempo(16.0, 140.0, TempoCurve::Step).unwrap();
let (mut clock, control) = TransportClock::with_control(map, 48_000.0);
control.set_loop_beats(Some((8.0, 16.0)));
control.play();

// On the audio thread:
let block = clock.next_block(512);
let transport = block.transport(clock.map());
```

The plugin `Harness` and the standalone runner drive their plugins with a
`TransportClock`, and hand the plugin the map in `ProcessContext::timeline`.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
