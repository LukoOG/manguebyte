// What the voices of each kind cost in the WebAssembly module, as a share
// of real time on one core at 48 kHz, rendered in quanta of 128 frames as
// an AudioWorklet renders them.
//
//   cargo build -p manguebyte-web --release --target wasm32-unknown-unknown
//   node crates/manguebyte-web/examples/voice_cost.mjs \
//     target/wasm32-unknown-unknown/release/manguebyte_web.wasm [--all-poly] \
//     [--unison target/wasm32-unknown-unknown/release/examples/unison_cost.wasm]
//
// For each case it holds `notes` notes on one part through no effect and
// times 4 s of them, the least of five runs, less the same part with no
// notes; per voice is that divided by the notes. The drum kits strike their
// piece again every 0.5 s, so that it rings the whole time. With --all-poly
// it also times one note of every preset of the polyphonic synthesiser,
// and with --unison the sixteen-voice unison patches of
// examples/unison_cost.rs, built with
//
//   cargo build -p manguebyte-web --release --example unison_cost --target wasm32-unknown-unknown

import { readFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';

import { encodeRecord } from '../js/ring.js';

const [wasmPath, ...flags] = process.argv.slice(2);
if (!wasmPath) {
  console.error('usage: node voice_cost.mjs <manguebyte_web.wasm> [--all-poly]');
  process.exit(2);
}
const x = new WebAssembly.Instance(new WebAssembly.Module(readFileSync(wasmPath)), {}).exports;

const RATE = 48_000;
const QUANTUM = 128;
const SECONDS = 4;
const RUNS = 5;
const KIND = {
  drum_kit: 10,
  poly: 11,
  mallet: 12,
  bowed: 13,
  wind: 14,
  choir: 15,
};

function push(engine, records) {
  const capacity = x.mb_staging_capacity(engine);
  for (let start = 0; start < records.length; start += capacity) {
    const chunk = records.slice(start, start + capacity);
    const view = new DataView(x.memory.buffer, x.mb_staging_ptr(engine), chunk.length * 32);
    chunk.forEach((record, index) => encodeRecord(view, index * 32, record));
    const queued = x.mb_push_staged(engine, chunk.length);
    if (queued !== chunk.length) throw new Error(`queued ${queued} of ${chunk.length}`);
  }
}

/** Seconds of rendering for SECONDS of sound, the least of RUNS. */
function time(kind, settings, notes, strikeEvery) {
  let best = Infinity;
  for (let run = 0; run < RUNS; run++) {
    const engine = x.mb_engine_new(RATE, QUANTUM, 4, 8_192, 256);
    if (x.mb_create_part(engine, 0, kind, 0) !== 0) throw new Error('create_part');
    const records = settings.map(([param, value]) => ({ kind: 'param', part: 0, param, value }));
    const total = (SECONDS + 1) * RATE;
    for (const [index, key] of notes.entries()) {
      if (strikeEvery) {
        for (let frame = index * 7; frame < total; frame += strikeEvery * RATE) {
          records.push({ kind: 'note_on', part: 0, note: key, velocity: 0.9, frame });
        }
      } else {
        records.push({ kind: 'note_on', part: 0, note: key, velocity: 0.9, frame: index * 7 });
      }
    }
    push(engine, records);
    // A second to settle: attacks, swells, filters opening.
    for (let frame = 0; frame < RATE; frame += QUANTUM) x.mb_render(engine, QUANTUM);
    const start = performance.now();
    for (let frame = 0; frame < SECONDS * RATE; frame += QUANTUM) x.mb_render(engine, QUANTUM);
    best = Math.min(best, (performance.now() - start) / 1_000);
    const voices = x.mb_active_voices(engine, 0);
    x.mb_engine_free(engine);
    if (notes.length && voices < notes.length) {
      throw new Error(`${voices} voices sounding of ${notes.length}`);
    }
  }
  return best;
}

const share = (seconds) => (100 * seconds) / SECONDS;

function measure(label, kind, settings, notes, strikeEvery = 0) {
  const empty = time(kind, settings, [], 0);
  const full = time(kind, settings, notes, strikeEvery);
  const each = (full - empty) / notes.length;
  console.log(
    `${label.padEnd(44)} ${String(notes.length).padStart(2)} notes ${share(full).toFixed(2).padStart(6)} %` +
      `   per voice ${share(each).toFixed(3).padStart(6)} %   part alone ${share(empty).toFixed(3)} %`,
  );
  return share(each);
}

const preset = (kind, value) => [[`${kind}.preset`, value]];
const chord = [48, 52, 55, 59, 60, 64, 67, 71];

console.log(`manguebyte-web voice cost, ${RATE} Hz, quanta of ${QUANTUM}, ${SECONDS} s timed, best of ${RUNS}`);
measure('drum_kit acoustic: crash, ride, open hat, toms', KIND.drum_kit, preset('drum_kit', 0), [49, 51, 46, 43, 47, 50, 57, 59], 0.5);
measure('drum_kit acoustic: crash alone', KIND.drum_kit, preset('drum_kit', 0), [49], 0.5);
measure('drum_kit 909: crash, ride, open hat, toms', KIND.drum_kit, preset('drum_kit', 2), [49, 51, 46, 43, 47, 50, 57, 59], 0.5);
measure('drum_kit lo-fi: crash, ride, open hat, toms', KIND.drum_kit, preset('drum_kit', 9), [49, 51, 46, 43, 47, 50, 57, 59], 0.5);
// The heaviest presets of the library: sixteen unison voices on every
// oscillator, and the busiest pads.
measure('poly Trance Supersaw', KIND.poly, [...preset('poly', 15), ['poly.voices', 8]], chord);
measure('poly Warm Analog Pad', KIND.poly, [...preset('poly', 26), ['poly.voices', 8]], chord);
measure('poly String Machine', KIND.poly, [...preset('poly', 27), ['poly.voices', 8]], chord);
measure('poly Evolving Spectra', KIND.poly, [...preset('poly', 34), ['poly.voices', 8]], chord);
measure('poly Hoover', KIND.poly, preset('poly', 19), chord.slice(0, 1));
measure('mallet vibraphone', KIND.mallet, preset('mallet', 2), chord);
measure('mallet tubular bells', KIND.mallet, preset('mallet', 7), chord);
measure('bowed violin', KIND.bowed, preset('bowed', 0), [67, 71, 74, 76, 79, 83]);
measure('bowed cello', KIND.bowed, preset('bowed', 2), [36, 43, 48, 55, 60, 64]);
measure('bowed string section', KIND.bowed, preset('bowed', 4), [48, 52, 55, 60, 64, 67]);
measure('wind flute (legato off)', KIND.wind, [['wind.legato', 0]], [72, 76, 79, 84]);
measure('wind trumpet (legato off)', KIND.wind, [['wind.preset', 3], ['wind.legato', 0]], [60, 64, 67, 72]);
measure('choir aah', KIND.choir, preset('choir', 0), [48, 52, 55, 60, 64, 67]);
measure('choir ooh', KIND.choir, preset('choir', 1), [48, 52, 55, 60, 64, 67]);
measure('choir solo voice', KIND.choir, preset('choir', 2), [60, 64, 67, 72, 76, 79]);

if (flags.includes('--all-poly')) {
  // The preset names, from the part's description of its parameters.
  const engine = x.mb_engine_new(RATE, QUANTUM, 4, 64, 8);
  x.mb_create_part(engine, 0, KIND.poly, 0);
  const pointer = x.mb_params_json(engine, 0);
  const text = new TextDecoder().decode(new Uint8Array(x.memory.buffer, pointer, x.mb_text_len(engine)));
  const names = JSON.parse(text).find((param) => param.id === 'poly.preset').scale.names;
  x.mb_engine_free(engine);
  const costs = names.map((name, index) => [measure(`poly ${index} ${name}`, KIND.poly, preset('poly', index), [60]), name]);
  costs.sort((a, b) => b[0] - a[0]);
  console.log('heaviest:', costs.slice(0, 6).map(([cost, name]) => `${name} ${cost.toFixed(2)} %`).join(', '));
}

// The sixteen-voice unison patches no preset goes as far as, from the
// module of examples/unison_cost.rs.
const unison = flags.indexOf('--unison');
if (unison >= 0) {
  const u = new WebAssembly.Instance(new WebAssembly.Module(readFileSync(flags[unison + 1])), {}).exports;
  u.unison_cost(0, 1); // builds the wavetables
  const blocks = (SECONDS * RATE) / QUANTUM;
  const names = [
    'the init patch: one saw through the ladder',
    'sixteen unison saws, no filter',
    'sixteen unison wavetables, no filter',
    'three oscillators of sixteen unison voices, two driven filters',
  ];
  for (const [patch, name] of names.entries()) {
    let best = Infinity;
    for (let run = 0; run < RUNS; run++) {
      const start = performance.now();
      u.unison_cost(patch, blocks);
      const short = performance.now();
      u.unison_cost(patch, 1);
      // Less the making of the voice, which the one-block call times.
      best = Math.min(best, (short - start - (performance.now() - short)) / 1_000);
    }
    console.log(`poly ${name.padEnd(64)} per voice ${share(best).toFixed(2).padStart(6)} %`);
  }
}
