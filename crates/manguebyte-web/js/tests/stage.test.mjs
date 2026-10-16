// Playing live through the JavaScript API: cancel() and latency().

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm } from './wasm.js';
import { FakeContext, QUANTUM, RATE, make } from './audio-thread.js';

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);

for (const shared of [true, false]) {
  const mode = shared ? 'shared' : 'message';

  test(`${mode}: cancel() drops the rest of a part and fades what sounds`, { skip: skipWithoutWasm }, async () => {
    const context = new FakeContext(0);
    const engine = await make(context, { shared });
    try {
      await engine.createPart(0, 'synth', 'none');
      await engine.createPart(1, 'drums', 'none');
      engine.noteOn(0, 45, 1, 0);
      engine.noteOn(0, 52, 1, 6000 / RATE);
      engine.noteOn(0, 57, 1, 9000 / RATE);
      engine.noteOn(1, 36, 1, 9000 / RATE);
      // Edited: what is left of part 0 from frame 4 000 goes.
      engine.cancel(0, 4000 / RATE);
      engine.noteOn(0, 64, 0.5, 7000 / RATE);
      const { left } = await context.run(Math.ceil(12000 / QUANTUM));
      const stats = await engine.stats();
      assert.equal(stats.cancelled, 2);
      // Silent from the end of the fade (8 ms) to the new note.
      assert.ok(left.slice(4000 + 385, 7000).every((sample) => sample === 0));
      assert.ok(peak(left.slice(3000, 4000)) > 0.1, 'it sounded before');
      assert.ok(peak(left.slice(7000, 9000)) > 0.02, 'the new note sounds');
      assert.equal(await engine.activeVoices(1), 1, 'the other part keeps its kick');
      await engine.close();
    } finally {
      engine.release();
    }
  });
}

test('latency() is a render quantum and the context\'s own latency', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  context.baseLatency = 0.005;
  context.outputLatency = 0.02;
  const engine = await make(context);
  try {
    await context.run(2);
    const latency = await engine.latency();
    assert.deepEqual(latency, {
      engineFrames: QUANTUM,
      outputFrames: 1200,
      frames: QUANTUM + 1200,
      seconds: (QUANTUM + 1200) / RATE,
    });
    // An event due one quantum ahead lands on its frame.
    await engine.createPart(0, 'drums', 'none');
    const due = context.frame + latency.engineFrames;
    engine.noteOn(0, 36, 1, due / RATE);
    const { start, left } = await context.run(4);
    assert.equal(start + left.findIndex((sample) => sample !== 0), due);
    assert.equal((await engine.stats()).late, 0);
    await engine.close();
  } finally {
    engine.release();
  }
});
