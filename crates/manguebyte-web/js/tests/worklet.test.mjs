// The worklet and the main-thread helper together, in Node.js, with the
// stand-ins of audio-thread.js for the browser's worklet globals.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm } from './wasm.js';
import { DRUM_NOTES, DRUM_SYMBOLS, FakeContext, QUANTUM, RATE, firstSound, make } from './audio-thread.js';

for (const shared of [true, false]) {
  const mode = shared ? 'shared' : 'message';

  test(`${mode}: a note scheduled at a context time sounds on that frame`, { skip: skipWithoutWasm }, async () => {
    // The context has run for 0.1 s before the node is made.
    const context = new FakeContext(4800);
    const engine = await make(context, { shared });
    assert.equal(engine.mode, mode);
    await engine.createPart(0, 'drums');
    await engine.createPart(1, 'synth', { lowpass: true, reverb: false });
    const time = (4800 + 1000) / RATE;
    assert.equal(engine.frameAt(time), 1000);
    assert.equal(engine.noteOn(0, 36, 1, time), true);
    engine.noteOn(1, 45, 0.8, time + 0.01);
    const { start, left, right } = await context.run(40);
    assert.equal(start, 4800);
    assert.equal(engine.origin, 4800);
    assert.equal(firstSound(left), 1000, 'the kick lands on its frame');
    assert.equal(firstSound(right), 1000);
    assert.ok(left.every((sample) => Number.isFinite(sample) && Math.abs(sample) <= 1));
    const stats = await engine.stats();
    assert.equal(stats.queued, 2);
    assert.equal(stats.applied, 2);
    assert.equal(stats.late, 0);
    assert.equal(stats.frame, 40 * QUANTUM);
    assert.equal(await engine.activeVoices(1), 1);
    engine.allNotesOff(1);
    engine.setParam(1, 'gain', -6);
    await context.run(1);
    assert.equal(await engine.paramValue(1, 'gain'), -6);
    await engine.close();
  });

  test(`${mode}: when process() is skipped the origin moves and times still map`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    await engine.createPart(0, 'drums', 'none');
    await context.run(4);
    assert.equal(engine.origin, 0);
    // Ten quanta pass without the processor being called.
    context.frame += 10 * QUANTUM;
    await context.run(1);
    assert.equal(engine.origin, 10 * QUANTUM);
    const target = context.frame + 300;
    engine.noteOn(0, 38, 1, target / RATE);
    const { start, left } = await context.run(8);
    assert.equal(start + firstSound(left), target);
    await engine.close();
  });

  // Before the fix the worklet stopped taking records once the engine's
  // queue was full, so the snare stayed behind the far kicks and never
  // played.
  test(`${mode}: events far ahead do not keep out one for the next quantum`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared, pendingCapacity: 4 });
    try {
      await engine.createPart(0, 'drums', 'none');
      for (let i = 0; i < 4; i++) engine.noteOn(0, 36, 1, (10000000 + i) / RATE);
      engine.noteOn(0, 38, 1, 300 / RATE);
      engine.noteOn(0, 36, 1, 20000000 / RATE); // after them all: it waits
      const { left } = await context.run(4);
      assert.equal(firstSound(left), 300, 'the snare lands on its frame');
      const stats = await engine.stats();
      assert.equal(stats.displaced, 1);
      assert.equal(stats.late, 0);
      assert.ok(stats.stalled >= 1, 'the last record was left for later');
      await engine.close();
    } finally {
      // A failed assertion must not leave the port open and the test
      // process waiting on it.
      engine.release();
    }
  });

  test(`${mode}: a note without a velocity is played at full velocity`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    await engine.createPart(0, 'drums', 'none');
    await engine.createPart(1, 'drums', 'none');
    engine.noteOn(0, 38, undefined, 200 / RATE);
    engine.noteOn(1, 38);
    const { left } = await context.run(4);
    assert.equal(firstSound(left), 0);
    const stats = await engine.stats();
    assert.equal(stats.applied, 2);
    assert.equal(await engine.activeVoices(0), 1);
    assert.equal(await engine.activeVoices(1), 1);
    await engine.close();
  });

  test(`${mode}: an accented note reaches the engine as one`, { skip: skipWithoutWasm }, async () => {
    const peaks = [];
    for (const accent of [false, true]) {
      const context = new FakeContext(0);
      const engine = await make(context, { shared });
      await engine.createPart(0, 'drums', 'none');
      engine.noteOn(0, 38, 0.8, 0, 0, accent);
      const { left } = await context.run(8);
      peaks.push(left.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0));
      await engine.close();
    }
    const db = 20 * Math.log10(peaks[1] / peaks[0]);
    assert.ok(db > 2.5 && db < 6, `the accent added ${db} dB`);
  });

  test(`${mode}: a note whose time has passed plays at the next quantum`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    await engine.createPart(0, 'drums');
    await context.run(10);
    engine.noteOn(0, 36, 1, 0.001); // long gone
    engine.noteOn(0, 42, 1); // no time: as soon as possible
    const { left } = await context.run(2);
    // On the first frame the engine renders, which it renders the master
    // bus's look-ahead, 111 frames, ahead of the output.
    assert.equal(firstSound(left), 111);
    assert.equal((await engine.stats()).late, 2);
    await engine.close();
  });
}

test('sync() waits until records sent by message have reached the worklet', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: false });
  const processor = context.processors[0];
  engine.noteOn(0, 36, 1, 0.5);
  engine.noteOn(0, 38, 1, 0.6);
  assert.equal(processor.messages.length, 0, 'nothing has arrived yet');
  await engine.sync();
  assert.equal(processor.messages.length, 1, 'one message with both records');
  assert.equal(processor.messages[0].length, 64);
  await engine.close();
});

test('a full ring keeps the rest in order and hands it over later', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: true, ringCapacity: 4 });
  await engine.createPart(0, 'drums');
  const handed = [];
  for (let i = 0; i < 10; i++) handed.push(engine.noteOn(0, 42, 1, (1000 + i * 2000) / RATE));
  assert.deepEqual(handed, [true, true, true, true, false, false, false, false, false, false]);
  for (let i = 0; i < 6 && (await engine.stats()).queued < 10; i++) {
    await new Promise((resolve) => setTimeout(resolve, 10));
    await context.run(1);
  }
  const stats = await engine.stats();
  assert.equal(stats.queued, 10);
  assert.equal(stats.dropped, 0);
  await engine.close();
});

test('control calls report errors and describe parameters', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: true, maxParts: 1 });
  await engine.createPart(3, 'synth');
  await assert.rejects(engine.createPart(4, 'drums'), /every part slot is taken/);
  await assert.rejects(engine.setChain(9, 'none'), /no such part/);
  assert.throws(() => engine.createPart(0, 'theremin'), RangeError);
  const params = await engine.params(3);
  assert.equal(params[0].id, 'synth.wave');
  assert.equal(params.at(-1).id, 'send');
  assert.equal(await engine.paramsJson(8), null);
  const map = JSON.parse(await engine.drumMapJson());
  assert.equal(map.find((entry) => entry.symbol === 'o').note, 46);
  // The helper's tables are the module's map.
  assert.deepEqual({ ...DRUM_NOTES }, Object.fromEntries(map.map((entry) => [entry.name, entry.note])));
  assert.deepEqual(
    { ...DRUM_SYMBOLS },
    Object.fromEntries(map.filter((entry) => entry.symbol !== null).map((entry) => [entry.symbol, entry.note])),
  );
  await engine.setChain(3, 'none');
  assert.equal((await engine.params(3)).some((param) => param.id === 'reverb.mix'), false);
  await engine.removePart(3);
  assert.equal(await engine.activeVoices(3), -1);
  assert.throws(() => engine.schedule({ kind: 1, note: 300 }), RangeError);
  await engine.close();
  assert.throws(() => engine.noteOn(0, 36, 1), /closed/);
  assert.equal(context.processors.length, 1);
  const output = await context.run(1);
  assert.equal(context.processors.length, 0, 'the closed processor asked to stop');
  assert.ok(output.left.every((sample) => sample === 0));
});

// Without the rejection these requests never settle: the timeout turns that
// into a failure.
test('requests still waiting when the helper is released are rejected', { skip: skipWithoutWasm, timeout: 10000 }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context, { shared: true });
  // Asked, and released before the worklet can answer.
  const pending = [engine.stats(), engine.paramsJson(0), engine.createPart(0, 'drums')];
  engine.release();
  for (const request of pending) await assert.rejects(request, /closed/);
  assert.equal(engine.waiting.size, 0);
  await assert.rejects(engine.stats(), /closed/);
});

test('a sample rate the engine does not run at is named in the error', { skip: skipWithoutWasm }, async () => {
  for (const rate of [2000, 800000]) {
    globalThis.sampleRate = rate;
    try {
      await assert.rejects(make(new FakeContext(0)), new RegExp(`sample rate ${rate} Hz is out of range`));
    } finally {
      globalThis.sampleRate = RATE;
    }
  }
});

test('a module that is not the engine is reported', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  await assert.rejects(
    make(context, { wasm: new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]) }),
    /manguebyte worklet/,
  );
  await assert.rejects(make(context, { maxQuantum: 0 }), /out of range/);
});
