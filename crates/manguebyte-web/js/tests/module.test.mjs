// The WebAssembly module itself, driven from Node.js through its exports.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { KIND, RECORD_SIZE, encodeRecord, paramId } from '../ring.js';
import { skipWithoutWasm, wasmBytes } from './wasm.js';

const RATE = 48000;
/** `OUTPUT_CEILING` in Rust, an f32. */
const OUTPUT_CEILING = Math.fround(0.966);

function instantiate() {
  const module = new WebAssembly.Module(wasmBytes());
  return { module, x: new WebAssembly.Instance(module, {}).exports };
}

/** An engine with a drum part 0 and a synth part 1, and helpers around it. */
function setUp(maxQuantum = 128) {
  const { x } = instantiate();
  const engine = x.mb_engine_new(RATE, maxQuantum, 8, 1024, 64);
  assert.notEqual(engine, 0);
  assert.equal(x.mb_create_part(engine, 0, 1, 3), 0);
  assert.equal(x.mb_create_part(engine, 1, 0, 3), 0);
  const push = (records) => {
    const heap = new DataView(x.memory.buffer);
    const staging = x.mb_staging_ptr(engine) >>> 0;
    records.forEach((record, i) => encodeRecord(heap, staging + i * RECORD_SIZE, record));
    return x.mb_push_staged(engine, records.length);
  };
  const render = (frames, quantum) => {
    const left = new Float32Array(frames);
    const right = new Float32Array(frames);
    let done = 0;
    while (done < frames) {
      const n = x.mb_render(engine, Math.min(quantum, frames - done));
      assert.equal(n, Math.min(quantum, frames - done, maxQuantum));
      left.set(new Float32Array(x.memory.buffer, x.mb_output_ptr(engine, 0) >>> 0, n), done);
      right.set(new Float32Array(x.memory.buffer, x.mb_output_ptr(engine, 1) >>> 0, n), done);
      done += n;
    }
    return { left, right };
  };
  const text = (start) => {
    assert.notEqual(start, 0);
    const bytes = new Uint8Array(x.memory.buffer, start >>> 0, x.mb_text_len(engine));
    return new TextDecoder().decode(bytes);
  };
  return { x, engine, push, render, text };
}

const pattern = [
  { frame: 300, kind: KIND.note_on, part: 0, note: 36, velocity: 1 },
  { frame: 6000, kind: KIND.note_on, part: 0, note: 38, velocity: 0.8 },
  { frame: 6000, kind: KIND.note_on, part: 0, note: 42, velocity: 0.6 },
  { frame: 1000, kind: KIND.note_on, part: 1, note: 33, velocity: 0.9 },
  { frame: 1000 + 128 * 20 + 5, kind: KIND.param, part: 1, param: 'synth.cutoff', value: 400 },
  { frame: 9000, kind: KIND.note_off, part: 1, note: 33 },
  { frame: 9001, kind: KIND.param, part: 0, param: 'pan', value: -0.75 },
  { frame: 12000, kind: KIND.all_notes_off, part: 0 },
];

test('the module imports nothing and exports the documented functions', { skip: skipWithoutWasm }, () => {
  const { module, x } = instantiate();
  assert.deepEqual(WebAssembly.Module.imports(module), []);
  const exported = WebAssembly.Module.exports(module).map((entry) => entry.name).sort();
  for (const name of [
    'memory',
    'mb_abi_version',
    'mb_record_size',
    'mb_engine_new',
    'mb_engine_free',
    'mb_staging_ptr',
    'mb_staging_capacity',
    'mb_push_staged',
    'mb_admit_staged',
    'mb_min_sample_rate',
    'mb_max_sample_rate',
    'mb_pending_room',
    'mb_clear_pending',
    'mb_render',
    'mb_output_ptr',
    'mb_frame',
    'mb_create_part',
    'mb_remove_part',
    'mb_set_chain',
    'mb_params_json',
    'mb_drum_map_json',
    'mb_text_len',
    'mb_param_value',
    'mb_active_voices',
    'mb_stat',
    'mb_text_ptr',
    'mb_input_ptr',
    'mb_sfz_begin',
    'mb_sfz_files_json',
    'mb_sfz_add_file',
    'mb_sfz_finish',
    'mb_sfz_abandon',
    'mb_collect',
  ]) {
    assert.ok(exported.includes(name), name);
  }
  assert.equal(x.mb_abi_version(), 2);
  assert.equal(x.mb_record_size(), 32);
  assert.equal(x.mb_engine_new(1000, 128, 4, 64, 64), 0, 'a bad sample rate makes no engine');
  // Every rate browsers allow for an AudioContext, and no other.
  assert.deepEqual([x.mb_min_sample_rate(), x.mb_max_sample_rate()], [3000, 768000]);
  for (const [rate, made] of [
    [2999, false],
    [3000, true],
    [768000, true],
    [768001, false],
  ]) {
    const engine = x.mb_engine_new(rate, 128, 1, 8, 8);
    assert.equal(engine !== 0, made, `${rate} Hz`);
    x.mb_engine_free(engine);
  }
  assert.equal(x.mb_render(0, 128), 0, 'a null engine does nothing');
});

test('events pushed through the staging area land on their frames', { skip: skipWithoutWasm }, () => {
  const { x, engine, push, render } = setUp();
  assert.equal(push(pattern), pattern.length);
  assert.equal(x.mb_stat(engine, 0), pattern.length);
  const { left, right } = render(24000, 128);
  const firstSound = left.findIndex((sample) => sample !== 0);
  assert.equal(firstSound, 300, 'the kick lands on frame 300');
  let peak = 0;
  for (let i = 0; i < left.length; i++) {
    assert.ok(Number.isFinite(left[i]) && Math.abs(left[i]) <= OUTPUT_CEILING);
    assert.ok(Number.isFinite(right[i]) && Math.abs(right[i]) <= OUTPUT_CEILING);
    peak = Math.max(peak, Math.abs(left[i]), Math.abs(right[i]));
  }
  assert.ok(peak > 0.2 && peak < 1, `peak ${peak}`);
  assert.equal(x.mb_frame(engine), 24000);
  assert.equal(x.mb_stat(engine, 4), pattern.length, 'every event was applied');
  assert.equal(x.mb_stat(engine, 5), 0, 'and reached its part');
  assert.equal(x.mb_param_value(engine, 1, paramId('synth.cutoff')), 400);
  assert.equal(x.mb_param_value(engine, 0, paramId('pan')), -0.75);
  // The drums were damped at frame 12000 and the bass released at 9000.
  assert.equal(x.mb_active_voices(engine, 0), 0);
  x.mb_engine_free(engine);
});

test('the output is the same for every quantum size', { skip: skipWithoutWasm }, () => {
  const runs = [1, 7, 128, 512].map((quantum) => {
    const { engine, push, render, x } = setUp(512);
    push(pattern);
    const output = render(20000, quantum);
    x.mb_engine_free(engine);
    return output;
  });
  const bits = (samples) => new Uint32Array(samples.buffer);
  for (const run of runs.slice(1)) {
    assert.deepEqual(bits(run.left), bits(runs[0].left));
    assert.deepEqual(bits(run.right), bits(runs[0].right));
  }
});

test('a late event lands at the start of the next quantum and a far one waits', { skip: skipWithoutWasm }, () => {
  const { x, engine, push, render } = setUp();
  render(1280, 128);
  push([
    { frame: 5, kind: KIND.note_on, part: 0, note: 36, velocity: 1 },
    { frame: 50000, kind: KIND.note_on, part: 0, note: 38, velocity: 1 },
  ]);
  const { left } = render(128, 128);
  assert.notEqual(left[0], 0, 'the late kick starts on the first frame');
  assert.equal(x.mb_stat(engine, 3), 1, 'one late event');
  assert.equal(x.mb_pending_room(engine), 1023, 'the snare waits');
  x.mb_clear_pending(engine);
  assert.equal(x.mb_pending_room(engine), 1024);
  x.mb_engine_free(engine);
});

test('a full queue takes earlier records and leaves later ones to the caller', { skip: skipWithoutWasm }, () => {
  const { x } = instantiate();
  const engine = x.mb_engine_new(RATE, 128, 1, 2, 8);
  assert.equal(x.mb_create_part(engine, 0, 1, 0), 0);
  const heap = new DataView(x.memory.buffer);
  const staging = x.mb_staging_ptr(engine) >>> 0;
  const records = [
    { frame: 9000000, kind: KIND.note_on, part: 0, note: 36, velocity: 1 },
    { frame: 9000001, kind: KIND.note_on, part: 0, note: 36, velocity: 1 },
    { frame: 100, kind: KIND.note_on, part: 0, note: 38, velocity: 1 }, // pushes out 9000001
    { frame: 9500000, kind: KIND.note_on, part: 0, note: 36, velocity: 1 }, // stops here
    { frame: 50, kind: KIND.note_on, part: 0, note: 42, velocity: 1 },
  ];
  records.forEach((record, i) => encodeRecord(heap, staging + i * RECORD_SIZE, record));
  assert.equal(x.mb_admit_staged(engine, records.length), 3);
  // dropped, displaced, stalled
  assert.deepEqual([2, 6, 7].map((index) => x.mb_stat(engine, index)), [1, 1, 1]);
  x.mb_render(engine, 128);
  const left = new Float32Array(x.memory.buffer, x.mb_output_ptr(engine, 0) >>> 0, 128);
  assert.equal(left.findIndex((sample) => sample !== 0), 100, 'the snare lands on frame 100');
  x.mb_engine_free(engine);
});

test('hostile records are ignored and counted', { skip: skipWithoutWasm }, () => {
  const { x, engine, push, render } = setUp();
  const heap = new Uint8Array(x.memory.buffer);
  const staging = x.mb_staging_ptr(engine) >>> 0;
  let seed = 12345;
  for (let i = 0; i < 64 * RECORD_SIZE; i++) {
    seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
    heap[staging + i] = seed >>> 24;
  }
  const queued = x.mb_push_staged(engine, 1000); // more than the staging area holds
  assert.ok(queued <= 64);
  // queued + ignored + dropped - displaced: every record pushed.
  assert.equal(x.mb_stat(engine, 0) + x.mb_stat(engine, 1) + x.mb_stat(engine, 2) - x.mb_stat(engine, 6), 64);
  assert.ok(x.mb_stat(engine, 1) > 0, 'some were ignored');
  render(4096, 128);
  push([{ frame: 0, kind: KIND.note_on, part: 0, note: 200, velocity: 1 }]);
  assert.ok(x.mb_stat(engine, 1) > 0);
  x.mb_engine_free(engine);
});

test('an accented note plays louder than the same note without', { skip: skipWithoutWasm }, () => {
  const peaks = [false, true].map((accent) => {
    const { x, engine, push, render } = setUp();
    assert.equal(x.mb_set_chain(engine, 0, 0), 0);
    assert.equal(x.mb_set_chain(engine, 1, 0), 0);
    push([
      { frame: 0, kind: KIND.note_on, part: 0, note: 38, velocity: 0.8, accent },
      { frame: 0, kind: KIND.note_on, part: 1, note: 45, velocity: 0.8, accent },
    ]);
    const { left } = render(4096, 128);
    x.mb_engine_free(engine);
    return left.reduce((peak, sample) => Math.max(peak, Math.abs(sample)), 0);
  });
  const db = 20 * Math.log10(peaks[1] / peaks[0]);
  assert.ok(db > 2.5 && db < 6, `the accent added ${db} dB`);
});

test('parts are configured by control calls and describe their parameters', { skip: skipWithoutWasm }, () => {
  const { x, engine, text } = setUp();
  const params = JSON.parse(text(x.mb_params_json(engine, 1)));
  const ids = params.map((param) => param.id);
  assert.deepEqual(ids.slice(-6), ['lowpass.cutoff', 'lowpass.resonance', 'reverb.mix', 'reverb.decay', 'gain', 'pan']);
  assert.equal(ids[0], 'synth.wave');
  for (const param of params) assert.equal(param.param_id, paramId(param.id), param.id);
  const gain = params.find((param) => param.id === 'gain');
  assert.deepEqual([gain.min, gain.max, gain.default, gain.unit, gain.smoothing_ms], [-60, 12, 0, 'dB', 20]);

  assert.equal(x.mb_set_chain(engine, 0, 0), 0);
  const drums = JSON.parse(text(x.mb_params_json(engine, 0))).map((param) => param.id);
  assert.deepEqual(drums, [
    'kit.tune',
    'kit.decay',
    'kick.level',
    'snare.level',
    'closed_hat.level',
    'open_hat.level',
    'ride.level',
    'crash.level',
    'high_tom.level',
    'mid_tom.level',
    'floor_tom.level',
    'side_stick.level',
    'guitar_tap.level',
    'gain',
    'pan',
  ]);
  const map = JSON.parse(text(x.mb_drum_map_json(engine)));
  assert.deepEqual(
    map.map((entry) => [entry.name, entry.note, entry.symbol]),
    [
      ['kick', 36, 'b'],
      ['snare', 38, 's'],
      ['closed_hat', 42, 'c'],
      ['open_hat', 46, 'o'],
      ['crash', 49, 'x'],
      ['ride', 51, 'r'],
      ['high_tom', 50, 't'],
      ['mid_tom', 47, 'm'],
      ['floor_tom', 43, 'f'],
      ['side_stick', 37, 'k'],
      ['guitar_tap', 88, null],
    ],
  );
  assert.equal(x.mb_params_json(engine, 9), 0);
  assert.equal(x.mb_create_part(engine, 2, 7, 0), -3);
  assert.equal(x.mb_create_part(engine, 70000, 0, 0), -2);
  assert.equal(x.mb_remove_part(engine, 0), 0);
  assert.equal(x.mb_active_voices(engine, 0), -1);
  x.mb_engine_free(engine);
});
