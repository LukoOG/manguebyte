// The newer voices of manguebyte-dsp in parts, through the JavaScript API:
// the drum kits on the drum map, the polyphonic synthesiser's presets by
// name, the mallets, the bowed strings, the winds and the choir in tune,
// accents from the records, and a monophonic preset sliding.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm } from './wasm.js';
import { FakeContext, QUANTUM, RATE, make } from './audio-thread.js';

const { INSTRUMENT, PRESETS } = await import('../engine.js');

/** `OUTPUT_CEILING` in Rust, an f32. */
const OUTPUT_CEILING = Math.fround(0.966);

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);
const rms = (samples) => Math.sqrt(samples.reduce((sum, sample) => sum + sample * sample, 0) / samples.length);

/** The frequency of the strongest component near `expected`, by a sweep of single-bin DFTs. */
function frequencyNear(samples, expected) {
  const magnitude = (frequency) => {
    let re = 0;
    let im = 0;
    for (let n = 0; n < samples.length; n++) {
      const hann = 0.5 - 0.5 * Math.cos((2 * Math.PI * n) / samples.length);
      const phase = (2 * Math.PI * frequency * n) / RATE;
      re += samples[n] * hann * Math.cos(phase);
      im -= samples[n] * hann * Math.sin(phase);
    }
    return Math.hypot(re, im);
  };
  let best = expected;
  let bestMagnitude = -1;
  for (let cents = -50; cents <= 50; cents += 0.5) {
    const frequency = expected * 2 ** (cents / 1200);
    const value = magnitude(frequency);
    if (value > bestMagnitude) {
      bestMagnitude = value;
      best = frequency;
    }
  }
  return best;
}

const cents = (measured, expected) => 1200 * Math.log2(measured / expected);
const keyHz = (key) => 440 * 2 ** ((key - 69) / 12);
const quanta = (seconds) => Math.ceil((seconds * RATE) / QUANTUM);

test('the new kinds have their codes and their presets by name', { skip: skipWithoutWasm }, async () => {
  assert.deepEqual(
    [INSTRUMENT.drum_kit, INSTRUMENT.poly, INSTRUMENT.mallet, INSTRUMENT.bowed, INSTRUMENT.wind, INSTRUMENT.choir],
    [10, 11, 12, 13, 14, 15],
  );
  const context = new FakeContext(0);
  const engine = await make(context, { maxParts: 8 });
  try {
    const kinds = ['drum_kit', 'poly', 'mallet', 'bowed', 'wind', 'choir'];
    for (const [index, kind] of kinds.entries()) {
      await engine.createPart(index, kind, 'none');
      const params = await engine.params(index);
      const preset = params.find((param) => param.id === `${kind}.preset`);
      assert.equal(preset.scale.names.length, PRESETS[kind].length, kind);
      // The JavaScript names are the description's, in lower case, joined
      // by underscores.
      const snake = preset.scale.names.map((name) => name.toLowerCase().replace(/[^a-z0-9]+/g, '_'));
      assert.deepEqual(snake, [...PRESETS[kind]], kind);
    }
    engine.setPreset(0, 'drum_kit', '909');
    engine.setPreset(1, 'poly', 'trance_supersaw');
    engine.setPreset(2, 'mallet', 'vibraphone');
    engine.setPreset(3, 'bowed', 'cello');
    engine.setPreset(4, 'wind', 'trumpet');
    engine.setPreset(5, 'choir', 'solo_voice');
    await context.run(1);
    assert.equal(await engine.paramValue(0, 'drum_kit.preset'), 2);
    assert.equal(await engine.paramValue(1, 'poly.preset'), 15);
    assert.ok((await engine.paramValue(2, 'mallet.motor')) > 0, "the vibraphone's motor turns");
    assert.equal(await engine.paramValue(3, 'bowed.preset'), 2);
    assert.equal(await engine.paramValue(4, 'wind.legato'), 1, 'a wind slurs by default');
    assert.equal(await engine.paramValue(5, 'choir.preset'), 2);
    assert.throws(() => engine.setPreset(1, 'poly', 'banjo'), /no preset banjo/);
    assert.throws(() => engine.setPreset(0, 'drum_kit', 10), RangeError);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('the new kinds play their notes in tune and within the ceiling', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { maxParts: 8 });
  try {
    const cases = [
      ['poly', [['poly.preset', PRESETS.poly.indexOf('deep_sub')]], 36],
      ['mallet', [], 60],
      ['bowed', [['bowed.preset', 2], ['bowed.vibrato', 0]], 48],
      ['wind', [['wind.preset', 1], ['wind.vibrato', 0]], 62],
      ['choir', [['choir.preset', 2], ['choir.vibrato', 0]], 60],
    ];
    for (const [index, [kind, settings]] of cases.entries()) {
      await engine.createPart(index, kind, 'none');
      for (const [id, value] of settings) engine.setParam(index, id, value);
    }
    await context.run(1);
    for (const [index, [kind, , key]] of cases.entries()) {
      engine.noteOn(index, key, 0.8, context.currentTime + 0.01);
      engine.noteOff(index, key, context.currentTime + 1.0);
      const { left, right } = await context.run(quanta(1.1));
      assert.ok(peak(left) > 0.01, `${kind}: ${peak(left)}`);
      assert.ok(Math.max(peak(left), peak(right)) <= OUTPUT_CEILING, kind);
      // From 0.45 to 0.95 s, the two sides together.
      const held = left
        .slice(Math.round(0.45 * RATE), Math.round(0.95 * RATE))
        .map((sample, n) => sample + right[Math.round(0.45 * RATE) + n]);
      const error = cents(frequencyNear(held, keyHz(key)), keyHz(key));
      assert.ok(Math.abs(error) <= 5, `${kind}: ${error} cents`);
      await context.run(quanta(2));
    }
    assert.equal((await engine.stats()).recovered, 0);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('a drum kit plays the drum map and borrows the guitar tap', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'drum_kit', 'none');
    await engine.createPart(1, 'drums', 'none');
    engine.setPreset(0, 'drum_kit', 'techno');
    await context.run(1);
    // The guitar tap on both parts: the synthesized drum, the same samples.
    const due = context.currentTime + 0.01;
    engine.noteOn(0, 88, 0.9, due);
    engine.noteOn(1, 88, 0.9, due);
    engine.setParam(0, 'pan', -1, due);
    engine.setParam(1, 'pan', 1, due);
    const tap = await context.run(quanta(0.4));
    assert.ok(peak(tap.left) > 0.02);
    for (let n = 0; n < tap.left.length; n++) assert.equal(tap.left[n], tap.right[n], `frame ${n}`);
    // The kick, snare and hats are the kit's; an accent is louder.
    engine.setParam(0, 'pan', 0);
    engine.setParam(1, 'pan', 0);
    for (const key of [36, 38, 42, 46, 49, 51]) {
      const plain = context.currentTime + 0.01;
      engine.noteOn(0, key, 1, plain);
      const once = await context.run(quanta(0.5));
      engine.noteOn(0, key, 1, context.currentTime + 0.01, 0, true);
      const accented = await context.run(quanta(0.5));
      const louder = 20 * Math.log10(rms(accented.left) / rms(once.left));
      assert.ok(peak(once.left) > 0.02, `key ${key}`);
      assert.ok(louder > 1, `key ${key}: ${louder} dB`);
    }
    await engine.close();
  } finally {
    engine.release();
  }
});

test('a monophonic preset and a legato wind slur on one voice', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'poly', 'none');
    engine.setPreset(0, 'poly', '808_glide');
    await engine.createPart(1, 'wind', 'none');
    await engine.createPart(2, 'poly', 'none');
    engine.setPreset(2, 'poly', 'warm_analog_pad');
    await context.run(1);
    const start = context.currentTime + 0.01;
    for (const part of [0, 1, 2]) {
      const low = part === 1 ? 62 : 45;
      engine.noteOn(part, low, 0.9, start);
      engine.noteOn(part, low + 7, 0.9, start + 0.3);
      engine.noteOff(part, low, start + 0.31);
      engine.noteOff(part, low + 7, start + 0.8);
    }
    await context.run(quanta(0.5));
    assert.equal(await engine.activeVoices(0), 1, 'the 808 slid');
    assert.equal(await engine.activeVoices(1), 1, 'the flute slurred');
    assert.equal(await engine.activeVoices(2), 2, 'the pad plays both');
    await context.run(quanta(3));
    for (const part of [0, 1, 2]) assert.equal(await engine.activeVoices(part), 0);
    await engine.close();
  } finally {
    engine.release();
  }
});
