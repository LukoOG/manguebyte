// The instruments of manguebyte-dsp in parts, through the JavaScript API:
// createPart and setInstrument with every kind, presets, the organ's
// rotating speaker, accents and the slides of the two basses.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm, wasmBytes } from './wasm.js';
import { FakeContext, QUANTUM, RATE, make } from './audio-thread.js';

const { CHAIN, INSTRUMENT, PRESETS, ROTARY } = await import('../engine.js');

/** `OUTPUT_CEILING` in Rust, an f32. */
const OUTPUT_CEILING = Math.fround(0.966);

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);

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
  // Half a semitone either side, a cent at a time.
  for (let cents = -50; cents <= 50; cents += 1) {
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

test('the module plays every instrument code and sets instruments on parts', { skip: skipWithoutWasm }, () => {
  const x = new WebAssembly.Instance(new WebAssembly.Module(wasmBytes()), {}).exports;
  assert.equal(typeof x.mb_set_instrument, 'function');
  // Only added: the interface keeps its version.
  assert.equal(x.mb_abi_version(), 2);
  const engine = x.mb_engine_new(RATE, 128, 16, 64, 8);
  for (const [name, code] of Object.entries(INSTRUMENT)) {
    assert.equal(x.mb_create_part(engine, code, code, CHAIN.full), 0, name);
  }
  const codes = Object.values(INSTRUMENT);
  assert.deepEqual(codes, [...codes.keys()], 'codes 0 to 9, in order');
  assert.equal(x.mb_create_part(engine, 10, codes.length, CHAIN.full), -3, 'one past the last');
  assert.equal(x.mb_set_instrument(engine, 0, INSTRUMENT.acid), 0);
  assert.equal(x.mb_set_instrument(engine, 0, 99), -3);
  assert.equal(x.mb_set_instrument(engine, 15, INSTRUMENT.fm), -6, 'no such part');
  assert.equal(x.mb_set_instrument(engine, 70000, INSTRUMENT.fm), -2);
  x.mb_engine_free(engine);
});

test('setInstrument changes what a part plays and keeps its chain', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'synth', ['delay']);
    engine.setParam(0, 'gain', -6);
    engine.setParam(0, 'delay.feedback', 0.2);
    await context.run(1);
    await engine.setInstrument(0, 'acid');
    const params = await engine.params(0);
    assert.equal(params[0].id, 'acid.preset');
    assert.deepEqual(params[0].scale.names, ['Classic', 'Squelch', 'Square']);
    assert.equal(params.find((param) => param.id === 'synth.wave'), undefined);
    assert.equal(await engine.partChain(0), CHAIN.delay);
    assert.equal(await engine.paramValue(0, 'gain'), -6, 'kept');
    assert.equal(await engine.paramValue(0, 'delay.feedback'), Math.fround(0.2), 'kept');
    await assert.rejects(engine.setInstrument(5, 'organ'), /no such part/);
    assert.throws(() => engine.setInstrument(0, 'theremin'), RangeError);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('every new kind plays its notes in tune and within the ceiling', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { maxParts: 8 });
  try {
    const cases = [
      ['plucked', 45],
      ['fm', 57],
      ['organ', 57],
      ['kick_bass', 33],
      ['acid', 45],
    ];
    for (const [index, [kind]] of cases.entries()) {
      await engine.createPart(index, kind, 'none');
      engine.setParam(index, 'pan', 0);
    }
    // Heard straight, so that the speaker's Doppler shift does not move the pitch.
    engine.setParam(2, 'organ.speaker', 0);
    await context.run(1);
    for (const [index, [kind, key]] of cases.entries()) {
      engine.noteOn(index, key, 0.9, context.currentTime + 0.01);
      engine.noteOff(index, key, context.currentTime + 0.6);
      const { left } = await context.run(Math.ceil((0.7 * RATE) / QUANTUM));
      assert.ok(peak(left) > 0.05, `${kind}: ${peak(left)}`);
      assert.ok(peak(left) <= OUTPUT_CEILING, kind);
      // From 0.25 to 0.55 s: after the kick bass's drop, while held.
      const held = left.slice(Math.round(0.25 * RATE), Math.round(0.55 * RATE));
      const error = cents(frequencyNear(held, keyHz(key)), keyHz(key));
      assert.ok(Math.abs(error) <= 2, `${kind}: ${error} cents`);
      await context.run(Math.ceil((2 * RATE) / QUANTUM));
    }
    assert.equal((await engine.stats()).recovered, 0);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('an accent and a slide reach the voices from the records', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'acid', 'none');
    await engine.createPart(1, 'acid', 'none');
    await context.run(1);
    const start = context.currentTime + 0.01;
    // Part 0 plain, part 1 accented: the same note otherwise.
    engine.noteOn(0, 36, 1, start);
    engine.noteOn(1, 36, 1, start + 1, 0, true);
    engine.noteOff(0, 36, start + 0.3);
    engine.noteOff(1, 36, start + 1.3);
    const { left } = await context.run(Math.ceil((2 * RATE) / QUANTUM));
    const at = (seconds) => Math.round(seconds * RATE);
    const plain = peak(left.slice(at(0), at(0.3)));
    const accented = peak(left.slice(at(1), at(1.3)));
    assert.ok(20 * Math.log10(accented / plain) > 2, `${plain} ${accented}`);

    // A slide: the next note starts while the last is held.
    const slide = context.currentTime + 0.01;
    engine.noteOn(0, 45, 1, slide);
    engine.noteOn(0, 57, 1, slide + 0.3);
    engine.noteOff(0, 45, slide + 0.31);
    engine.noteOff(0, 57, slide + 0.8);
    await context.run(Math.ceil((0.4 * RATE) / QUANTUM));
    assert.equal(await engine.activeVoices(0), 1, 'one voice slid');
    await context.run(Math.ceil((1 * RATE) / QUANTUM));
    assert.equal(await engine.activeVoices(0), 0);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('presets move their knobs, and the organ speeds its speaker up', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    for (const [index, kind] of ['plucked', 'fm', 'organ', 'acid'].entries()) {
      await engine.createPart(index, kind);
      const params = await engine.params(index);
      const preset = params.find((param) => param.id === `${kind}.preset`);
      assert.equal(preset.scale.names.length, PRESETS[kind].length, kind);
      assert.equal(preset.default, 0);
    }
    engine.setPreset(0, 'plucked', 'slap_bass');
    engine.setPreset(2, 'organ', 'full');
    engine.setParam(2, 'organ.click', 0.2);
    engine.setPreset(3, 'acid', 2);
    engine.setRotary(2, 'fast');
    await context.run(1);
    assert.equal(await engine.paramValue(0, 'plucked.preset'), 3);
    assert.equal(await engine.paramValue(0, 'plucked.brightness'), 9000, 'the slap\'s pluck');
    assert.equal(await engine.paramValue(2, 'organ.drawbar_1'), 8, 'full organ: every drawbar out');
    assert.equal(await engine.paramValue(2, 'organ.click'), Math.fround(0.2), 'set after the preset');
    assert.equal(await engine.paramValue(2, 'organ.rotary'), ROTARY.indexOf('fast'));
    assert.equal(await engine.paramValue(3, 'acid.wave'), 1, 'the square preset');
    assert.throws(() => engine.setPreset(0, 'drums', 0), /has no presets/);
    assert.throws(() => engine.setPreset(0, 'plucked', 'banjo'), /no preset banjo/);
    assert.throws(() => engine.setPreset(0, 'plucked', 4), RangeError);
    assert.throws(() => engine.setRotary(2, 'warp'), RangeError);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('the analogue kit plays the drum map, its ride and guitar tap synthesized', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'analog_kit', 'none');
    await engine.createPart(1, 'drums', 'none');
    await context.run(1);
    const due = context.currentTime + 0.01;
    // The ride on both parts: the same drum, the same samples.
    engine.noteOn(0, 51, 0.9, due);
    engine.noteOn(1, 51, 0.9, due);
    engine.setParam(0, 'pan', -1, due);
    engine.setParam(1, 'pan', 1, due);
    const { left, right } = await context.run(Math.ceil((0.5 * RATE) / QUANTUM));
    assert.ok(peak(left) > 0.05);
    for (let n = 0; n < left.length; n++) assert.equal(left[n], right[n], `frame ${n}`);
    // The kick is the machine's, not the synthesized one.
    const kick = context.currentTime + 0.01;
    engine.noteOn(0, 36, 1, kick, 0, true);
    engine.noteOn(1, 36, 1, kick, 0, true);
    const kicks = await context.run(Math.ceil((0.5 * RATE) / QUANTUM));
    assert.ok(peak(kicks.left) > 0.1 && peak(kicks.right) > 0.1);
    assert.ok(kicks.left.some((sample, n) => sample !== kicks.right[n]));
    await engine.close();
  } finally {
    engine.release();
  }
});
