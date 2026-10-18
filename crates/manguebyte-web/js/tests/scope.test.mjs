// The parts' scopes and levels: the shared layout of scope.js, the exports
// behind it, and partScope and partLevels through the worklet, with shared
// memory and without.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { PartScopes, SCOPE_FRAMES, scopeBytes } from '../scope.js';
import { skipWithoutWasm, wasmBytes } from './wasm.js';
import { FakeContext, QUANTUM, RATE, make } from './audio-thread.js';

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);

test('a slot keeps the newest frames of its part, in order, as they wrap', () => {
  const scopes = PartScopes.create(3, 8);
  assert.equal(scopes.buffer.byteLength, scopeBytes(3, 8));
  assert.deepEqual(scopes.parts(), []);
  assert.equal(scopes.read(5, 4), null);
  // Source frames: left at 0, right at 16, levels at 32.
  const source = new Float32Array(40);
  const fill = (from, count) => {
    for (let k = 0; k < count; k++) {
      source[k] = from + k;
      source[16 + k] = -(from + k);
    }
  };
  source.set([0.5, 0.25, 0.125, 0.0625], 32);
  // A part made at frame 3: two frames so far.
  fill(1, 2);
  scopes.assign(1, 5, source, 0, 16, 2, 3, 32);
  assert.deepEqual(scopes.parts(), [5]);
  let read = scopes.read(5, 100);
  assert.equal(read.frame, 0);
  assert.deepEqual([...read.left], [0, 1, 2]);
  assert.deepEqual([...read.right], [0, -1, -2]);
  assert.deepEqual(scopes.levels(5), { frame: 3, peak: [0.5, 0.25], rms: [0.125, 0.0625] });
  // Five more, then twelve, of which the slot only sees the newest eight.
  fill(3, 5);
  scopes.append(1, source, 0, 16, 5, 5, 8, 32);
  read = scopes.read(5, 6);
  assert.equal(read.frame, 2);
  assert.deepEqual([...read.left], [2, 3, 4, 5, 6, 7]);
  fill(12, 8);
  scopes.append(1, source, 0, 16, 8, 12, 20, 32);
  read = scopes.read(5, 100);
  assert.equal(read.frame, 12);
  assert.deepEqual([...read.left], [12, 13, 14, 15, 16, 17, 18, 19]);
  // Four frames of which the engine had kept only the last: silence before.
  fill(23, 1);
  scopes.append(1, source, 0, 16, 1, 4, 24, 32);
  assert.deepEqual([...scopes.read(5, 5).left], [19, 0, 0, 0, 23]);
  // Every write leaves the sequence even.
  assert.equal(Atomics.load(scopes.words, scopes.base(1) / 4) % 2, 0);
  scopes.release(1);
  assert.equal(scopes.read(5, 4), null);
  assert.deepEqual(scopes.parts(), []);
  assert.throws(() => PartScopes.create(1, 6), RangeError);
  assert.throws(() => new PartScopes(new SharedArrayBuffer(64)), /not a buffer of part scopes/);
});

test('a reader tries again while the writer is in the middle of a slot', () => {
  const scopes = PartScopes.create(1, 4);
  const source = new Float32Array([1, 2, 3, 4, -1, -2, -3, -4, 0, 0, 0, 0]);
  scopes.assign(0, 2, source, 0, 4, 4, 4, 8);
  // The writer has begun and not finished: no consistent copy, so the
  // reader gives up after its attempts with the last thing it saw, here
  // nothing.
  Atomics.add(scopes.words, 0 + scopes.base(0) / 4, 1);
  assert.equal(scopes.read(2, 4), null);
  Atomics.add(scopes.words, 0 + scopes.base(0) / 4, 1);
  assert.deepEqual([...scopes.read(2, 4).left], [1, 2, 3, 4]);
});

test('the module exports the scopes and levels', { skip: skipWithoutWasm }, () => {
  const x = new WebAssembly.Instance(new WebAssembly.Module(wasmBytes()), {}).exports;
  for (const name of ['mb_scope_frames', 'mb_part_scope', 'mb_scope_ptr', 'mb_part_scope_end', 'mb_part_levels', 'mb_levels_ptr']) {
    assert.equal(typeof x[name], 'function', name);
  }
  // Only added: the interface keeps its version.
  assert.equal(x.mb_abi_version(), 2);
  assert.equal(x.mb_scope_frames(), SCOPE_FRAMES);
  const engine = x.mb_engine_new(RATE, 128, 4, 64, 8);
  assert.equal(x.mb_create_part(engine, 0, 1, 3), 0);
  assert.equal(x.mb_part_scope_end(engine, 0), 0);
  assert.equal(x.mb_part_scope(engine, 0, 64), 0);
  // A kick on frame 0, rendered: the frames of the part, the master bus's
  // look-ahead ahead of the output.
  const staging = x.mb_staging_ptr(engine) >>> 0;
  const record = new DataView(x.memory.buffer, staging, 32);
  record.setBigUint64(0, 0n, true);
  record.setUint8(8, 1);
  record.setUint8(12, 36);
  record.setFloat32(16, 1, true);
  assert.equal(x.mb_push_staged(engine, 1), 1);
  x.mb_render(engine, 128);
  const end = x.mb_part_scope_end(engine, 0);
  assert.equal(end, 128 + 111);
  const count = x.mb_part_scope(engine, 0, 10000);
  assert.equal(count, end);
  const laid = new Float32Array(x.memory.buffer, x.mb_scope_ptr(engine) >>> 0, 2 * count);
  const output = new Float32Array(x.memory.buffer, x.mb_output_ptr(engine, 0) >>> 0, 128);
  // What was heard of the part is what came out.
  assert.deepEqual([...laid.subarray(0, 128)], [...output]);
  assert.ok(peak(laid) > 0.1);
  assert.equal(x.mb_part_levels(engine, 0), 0);
  const levels = new Float32Array(x.memory.buffer, x.mb_levels_ptr(engine) >>> 0, 4);
  assert.ok(levels[0] > levels[2] && levels[2] > 0, `${[...levels]}`);
  assert.equal(x.mb_part_scope(engine, 3, 64), -6);
  assert.equal(x.mb_part_levels(engine, 3), -6);
  assert.ok(Number.isNaN(x.mb_part_scope_end(engine, 3)));
  x.mb_engine_free(engine);
});

for (const shared of [true, false]) {
  const mode = shared ? 'shared' : 'message';

  test(`${mode}: partScope gives what the part put into the master bus`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    try {
      assert.equal(engine.scopeMode, mode);
      await engine.createPart(0, 'drums', 'none');
      await engine.createPart(1, 'synth', 'none');
      engine.setParam(0, 'pan', -0.5);
      engine.noteOn(0, 38, 1, 1000 / RATE);
      engine.noteOn(1, 57, 0.8, 1500 / RATE);
      const { left, right } = await context.run(40);
      const heard = 40 * QUANTUM;
      const scope = await engine.partScope(0, SCOPE_FRAMES);
      assert.equal(scope.left.length, SCOPE_FRAMES);
      assert.equal(scope.frame, heard + 111 - SCOPE_FRAMES);
      assert.equal(scope.time, scope.frame / RATE);
      // The drums alone through the bus: two parts add up, so compare the
      // drums' scope with the drums' share, by itself, before the synth.
      const before = 1500 - scope.frame;
      for (let k = 0; k < before; k++) {
        assert.equal(scope.left[k], left[scope.frame + k], `frame ${scope.frame + k}`);
        assert.equal(scope.right[k], right[scope.frame + k]);
      }
      assert.ok(peak(scope.left) > peak(scope.right), 'panned left');
      const synth = await engine.partScope(1, 256);
      assert.equal(synth.left.length, 256);
      assert.ok(peak(synth.left) > 0.01);

      const one = await engine.partLevels(0);
      assert.equal(one.part, 0);
      assert.equal(one.frame, heard + 111);
      assert.ok(one.peak[0] > one.peak[1] && one.rms[0] > 0, JSON.stringify(one));
      const all = await engine.partLevels();
      assert.deepEqual(all.map((levels) => levels.part), [0, 1]);
      assert.deepEqual(all[0], one);

      // Kept when the part is made again, gone when it is removed.
      await engine.createPart(0, 'synth', 'none');
      assert.deepEqual((await engine.partScope(0, 64)).left, scope.left.slice(-64));
      await engine.removePart(1);
      assert.equal(await engine.partScope(1, 64), null);
      assert.equal(await engine.partLevels(1), null);
      assert.deepEqual((await engine.partLevels()).map((levels) => levels.part), [0]);
      await context.run(2);
      assert.equal((await engine.partScope(0, 64)).frame, 42 * QUANTUM + 111 - 64);
      await engine.close();
    } finally {
      engine.release();
    }
  });
}

test('with shared memory partScope reads it without a message', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: true });
  try {
    await engine.createPart(2, 'drums');
    engine.noteOn(2, 36, 1);
    await context.run(4);
    const posted = [];
    const post = engine.node.port.postMessage.bind(engine.node.port);
    engine.node.port.postMessage = (message, transfer) => {
      posted.push(message.type);
      post(message, transfer);
    };
    const scope = await engine.partScope(2, 100);
    const levels = await engine.partLevels(2);
    assert.deepEqual(posted, []);
    assert.equal(scope.left.length, 100);
    assert.ok(levels.peak[0] > 0);
    engine.node.port.postMessage = post;
    await engine.close();
  } finally {
    engine.release();
  }
});

test('scopes: false keeps shared records and asks for the scopes by message', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: true, scopes: false });
  try {
    assert.equal(engine.mode, 'shared');
    assert.equal(engine.scopeMode, 'message');
    await engine.createPart(0, 'drums');
    engine.noteOn(0, 42, 1);
    await context.run(3);
    const scope = await engine.partScope(0, 128);
    assert.ok(peak(scope.left) > 0.01);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('the drive takes the tube by name', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'synth', 'none');
    await engine.setEffect(0, 'drive', { shape: 'tube', amount: 18 });
    await context.run(1);
    assert.equal(await engine.paramValue(0, 'drive.shape'), 4);
    const curve = await engine.effectCurve(0, 'drive', 65);
    // Asymmetric: the curve bends the positive half sooner.
    assert.ok(curve.y[64] < -curve.y[0], `${curve.y[0]} ${curve.y[64]}`);
    await engine.close();
  } finally {
    engine.release();
  }
});
