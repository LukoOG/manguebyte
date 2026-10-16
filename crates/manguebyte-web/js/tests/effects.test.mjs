// The effects and the master bus through the JavaScript API: setEffect and
// its friends schedule the parameters Aratu's method chains name, and
// effectCurve reads back what an editor draws.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm, wasmBytes } from './wasm.js';
import { FakeContext, QUANTUM, RATE, make } from './audio-thread.js';

const {
  CHAIN,
  EFFECT,
  EFFECT_ORDER,
  EFFECT_PARAMS,
  SHAPES,
  VISUAL,
} = await import('../engine.js');

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);

test('the module exports the effects and the master bus', { skip: skipWithoutWasm }, () => {
  const module = new WebAssembly.Module(wasmBytes());
  const x = new WebAssembly.Instance(module, {}).exports;
  for (const name of [
    'mb_part_chain',
    'mb_master_params_json',
    'mb_master_param_value',
    'mb_effect_curve',
    'mb_curve_ptr',
    'mb_effect_running',
    'mb_limiter_reduction_db',
  ]) {
    assert.equal(typeof x[name], 'function', name);
  }
  // Only added: the interface keeps its version.
  assert.equal(x.mb_abi_version(), 2);
  const engine = x.mb_engine_new(RATE, 128, 4, 64, 8);
  assert.equal(x.mb_create_part(engine, 0, 1, CHAIN.all), 0);
  assert.equal(x.mb_part_chain(engine, 0), CHAIN.all);
  assert.equal(x.mb_part_chain(engine, 3), -6);
  assert.equal(x.mb_create_part(engine, 1, 0, CHAIN.all + 1), -4, 'an unknown bit');
  // A curve: points at mb_curve_ptr, x and y in turn.
  const count = x.mb_effect_curve(engine, 0, EFFECT.eq, 64);
  assert.equal(count, 64);
  const points = new Float32Array(x.memory.buffer, x.mb_curve_ptr(engine) >>> 0, 2 * count);
  assert.equal(points[0], 20);
  assert.equal(points[1], 0, 'a flat equaliser');
  assert.equal(x.mb_effect_curve(engine, 0, 99, 64), -14);
  assert.equal(x.mb_effect_curve(engine, 0, EFFECT.glue, 64), -14, 'the glue is the master\'s');
  assert.equal(x.mb_effect_curve(engine, -1, EFFECT.glue, 64), 64);
  assert.equal(x.mb_effect_curve(engine, 7, EFFECT.eq, 64), -6);
  assert.equal(x.mb_effect_running(engine, 0, EFFECT.delay), 0);
  assert.equal(x.mb_effect_running(engine, 0, EFFECT.lowpass), 1);
  assert.equal(x.mb_effect_running(engine, -1, EFFECT.limiter), 1);
  assert.equal(x.mb_limiter_reduction_db(engine), 0);
  x.mb_engine_free(engine);
});

test('the lists of engine.js are the module\'s', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'synth', 'all');
    const params = await engine.params(0);
    const effects = [...new Set(params.filter((param) => param.effect).map((param) => param.effect))];
    assert.deepEqual(effects, EFFECT_ORDER, 'the chain runs in this order');
    for (const name of EFFECT_ORDER) {
      const ids = params.filter((param) => param.effect === name).map((param) => param.id);
      assert.deepEqual(ids, [`${name}.on`, ...EFFECT_PARAMS[name].map((key) => `${name}.${key}`)]);
      for (const param of params.filter((param) => param.effect === name)) {
        assert.equal(param.visual, VISUAL[name], param.id);
      }
      assert.equal(CHAIN[name], 1 << EFFECT[name]);
    }
    for (const [effect, names] of Object.entries(SHAPES)) {
      const shape = params.find((param) => param.id === `${effect}.shape`);
      assert.equal(shape.scale.names.length, names.length, effect);
    }
    assert.deepEqual(params.slice(-3).map((param) => param.id), ['gain', 'pan', 'send']);
    const master = await engine.masterParams();
    assert.deepEqual(
      [...new Set(master.filter((param) => param.effect).map((param) => param.effect))],
      ['reverb', 'glue', 'limiter'],
    );
    await engine.close();
  } finally {
    engine.release();
  }
});

for (const shared of [true, false]) {
  const mode = shared ? 'shared' : 'message';

  test(`${mode}: setEffect switches an effect on the beat with its settings`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    try {
      await engine.createPart(0, 'drums', 'none');
      assert.equal(await engine.partChain(0), CHAIN.none);
      // .delay(0.25, 0.5, 0.3, 0) on frame 4 800, and a side stick on the
      // same frame: time, feedback, mix and no damping.
      const at = 4800 / RATE;
      await engine.setEffect(0, 'delay', [0.25, 0.5, 0.3, 0], at);
      assert.equal(await engine.partChain(0), CHAIN.delay, 'put in the chain');
      engine.noteOn(0, 37, 1, at);
      const { left } = await context.run(Math.ceil(30000 / QUANTUM));
      assert.equal(await engine.paramValue(0, 'delay.on'), 1);
      assert.equal(await engine.paramValue(0, 'delay.time'), 0.25);
      assert.equal(await engine.paramValue(0, 'delay.feedback'), 0.5);
      assert.equal(await engine.effectRunning(0, 'delay'), true);
      // Silent until the stick, which lands on its frame, then echoes a
      // quarter of a second apart, each half the last.
      assert.equal(left.findIndex((sample) => sample !== 0), 4800);
      const echo = (k) => peak(left.slice(4800 + k * 12000, 4800 + k * 12000 + 2000));
      const ratio = echo(2) / echo(1);
      assert.ok(Math.abs(ratio - 0.5) < 0.01, `${ratio}`);
      assert.equal((await engine.stats()).late, 0);

      // The curve: echoes 0.25 s apart.
      const taps = await engine.effectCurve(0, 'delay', 32);
      assert.equal(taps.visual, 'delay_taps');
      assert.ok(Math.abs(taps.x[0] - 0.25) < 1e-6 && Math.abs(taps.x[1] - 0.5) < 1e-6);
      assert.ok(Math.abs(taps.y[1] / taps.y[0] - 0.5) < 1e-6);

      // By name and by its first parameter; then off.
      await engine.setEffect(0, 'delay', { feedback: 0.25 });
      await engine.setEffect(0, 'delay', 0.125);
      engine.removeEffect(0, 'delay');
      await context.run(1);
      assert.equal(await engine.paramValue(0, 'delay.feedback'), 0.25);
      assert.equal(await engine.paramValue(0, 'delay.time'), 0.125);
      assert.equal(await engine.paramValue(0, 'delay.on'), 0);
      await assert.rejects(engine.setEffect(0, 'echo', {}), /no effect echo/);
      await assert.rejects(engine.setEffect(0, 'delay', { colour: 1 }), /no setting colour/);
      await assert.rejects(engine.setEffect(0, 'tremolo', { shape: 'wobbly' }), /no shape wobbly/);
      await assert.rejects(engine.setEffect(9, 'delay', {}), /no such part/);
      await engine.close();
    } finally {
      engine.release();
    }
  });
}

test('setEffects makes a method chain the part\'s effects', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'synth');
    // .delay(0.25).chorus().tremolo(4, 1, 'square')
    await engine.setEffects(0, { delay: [0.25], chorus: [], tremolo: [4, 1, 'square'] }, 0.01);
    await context.run(8);
    const on = async (name) => (await engine.paramValue(0, `${name}.on`)) === 1;
    assert.equal(await on('delay'), true);
    assert.equal(await on('chorus'), true);
    assert.equal(await on('tremolo'), true);
    assert.equal(await engine.paramValue(0, 'tremolo.shape'), 2);
    // The low-pass and the reverb were in the chain, and are switched off.
    assert.equal(await on('lowpass'), false);
    assert.equal(await on('reverb'), false);
    assert.equal(await engine.partChain(0), CHAIN.full | CHAIN.delay | CHAIN.chorus | CHAIN.tremolo);
    // The next evaluation drops the chorus.
    await engine.setEffects(0, { delay: [0.25], tremolo: [4] });
    await context.run(1);
    assert.equal(await on('chorus'), false);
    assert.equal(await on('delay'), true);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('setMaster sets the tempo and the bus, and curves are drawn for it', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(2, 'drums');
    engine.setMaster({ tempo: 90, gain: -3, glue: true, limiter: { ceiling: -2 }, reverb: { decay: 3 } });
    // A cancel of a part leaves the master's records alone.
    engine.setMaster({ glue: { threshold: -20 } }, 0.05);
    engine.cancel(0, 0);
    engine.setSend(2, 0.5);
    await context.run(Math.ceil(0.06 * RATE / QUANTUM));
    assert.equal(await engine.masterParamValue('master.tempo'), 90);
    assert.equal(await engine.masterParamValue('master.gain'), -3);
    assert.equal(await engine.masterParamValue('master.glue.on'), 1);
    assert.equal(await engine.masterParamValue('master.glue.threshold'), -20);
    assert.equal(await engine.paramValue(2, 'send'), 0.5);
    assert.equal(await engine.effectRunning('master', 'glue'), true);
    const limiter = await engine.effectCurve('master', 'limiter', 67);
    assert.equal(limiter.visual, 'transfer_curve');
    assert.equal(limiter.x.length, 67);
    assert.equal(limiter.y[limiter.y.length - 1], -2, 'nothing above the ceiling');
    const reverb = await engine.effectCurve('master', 'reverb', 16);
    assert.equal(reverb.visual, 'reverb_decay');
    assert.ok(Math.abs(reverb.x[reverb.x.length - 1] - 4.5) < 1e-5);
    assert.throws(() => engine.setMaster({ volume: 1 }), /no setting volume/);
    assert.equal(typeof (await engine.limiterReduction()), 'number');
    await engine.close();
  } finally {
    engine.release();
  }
});
