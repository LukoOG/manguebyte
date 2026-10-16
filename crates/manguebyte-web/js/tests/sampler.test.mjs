// Sampler parts through the JavaScript API: loadSfz fetches an SFZ file and
// its samples (generated here, 16-bit mono WAV files like Aratu's), the
// worklet decodes them between render quanta, and the part plays them.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import { skipWithoutWasm } from './wasm.js';
import { FakeContext, QUANTUM, RATE, firstSound, make } from './audio-thread.js';

/** A 16-bit mono WAV file of `samples` at `rate`. */
function wav16(samples, rate) {
  const bytes = new Uint8Array(44 + samples.length * 2);
  const view = new DataView(bytes.buffer);
  const text = (at, string) => [...string].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)));
  text(0, 'RIFF');
  view.setUint32(4, 36 + samples.length * 2, true);
  text(8, 'WAVEfmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, 'data');
  view.setUint32(40, samples.length * 2, true);
  samples.forEach((sample, i) => view.setInt16(44 + i * 2, Math.round(sample * 32767), true));
  return bytes;
}

function sine(frequency, rate, frames, amplitude) {
  return Array.from({ length: frames }, (_, n) => amplitude * Math.sin((2 * Math.PI * frequency * n) / rate));
}

const BASE = 'https://samples.test/voices/tone/';

/** A melodic instrument and a file that is not audio, served by a stand-in for fetch. */
function server() {
  const files = new Map([
    [`${BASE}voice.sfz`, new TextEncoder().encode(
      '<control> default_path=./\n<global> ampeg_release=0.05\n' +
        '<region> sample=a4.wav pitch_keycenter=69 lokey=0 hikey=74 loop_mode=loop_continuous loop_start=12000 loop_end=17999\n' +
        '<region> sample=low/a5.wav pitch_keycenter=81 lokey=75 hikey=127\n',
    )],
    [`${BASE}a4.wav`, wav16(sine(440, 24000, 24000, 0.8), 24000)],
    [`${BASE}low/a5.wav`, wav16(sine(880, 32000, 16000, 0.8), 32000)],
    [`${BASE}broken.sfz`, new TextEncoder().encode('<region> sample=junk.wav')],
    [`${BASE}junk.wav`, new TextEncoder().encode('RIFF1234WAVEjunk')],
    [`${BASE}missing.sfz`, new TextEncoder().encode('<region> sample=nowhere.wav')],
  ]);
  const asked = [];
  const fetch = async (href) => {
    asked.push(href);
    const bytes = files.get(href);
    if (!bytes) return { ok: false, status: 404 };
    return { ok: true, status: 200, arrayBuffer: async () => bytes.slice().buffer };
  };
  return { fetch, asked };
}

/** Renders quanta until `promise` settles, at most `limit` of them; returns its outcome and how many it took. */
async function runUntil(context, promise, limit = 200) {
  let settled = null;
  promise.then(
    (value) => (settled = { value }),
    (error) => (settled = { error }),
  );
  let quanta = 0;
  while (settled === null && quanta < limit) {
    await context.run(1);
    quanta += 1;
  }
  assert.notEqual(settled, null, 'the promise never settled');
  return { ...settled, quanta };
}

const peak = (samples) => samples.reduce((top, sample) => Math.max(top, Math.abs(sample)), 0);

test('loadSfz loads between render quanta and the part plays the instrument', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  context.state = 'running';
  const engine = await make(context);
  try {
    await engine.createPart(0, 'sampler', 'none');
    const { fetch, asked } = server();
    const loading = engine.loadSfz(0, `${BASE}voice.sfz`, { fetch });
    const { value: summary, quanta } = await runUntil(context, loading);
    assert.deepEqual(summary, { regions: 2, samples: 2, sample_bytes: (24000 + 16000) * 4, warnings: [] });
    assert.deepEqual(asked, [`${BASE}voice.sfz`, `${BASE}a4.wav`, `${BASE}low/a5.wav`]);
    assert.ok(quanta >= 1, 'the files were decoded by process()');
    // Live: a note scheduled now plays the instrument on its frame.
    const at = context.frame + 2 * QUANTUM + 40;
    engine.noteOn(0, 69, 1, at / RATE);
    engine.noteOn(0, 81, 0.5, at / RATE, 1, true);
    const { start, left, right } = await context.run(40);
    // A sine starts at 0, so the first sound is one frame later.
    assert.equal(start + firstSound(left), at + 1);
    assert.deepEqual(left, right);
    assert.ok(peak(left) > 0.3 && peak(left) < 1, `peak ${peak(left)}`);
    assert.equal(await engine.activeVoices(0), 2);
    engine.allNotesOff(0);
    await context.run(40);
    assert.equal(await engine.activeVoices(0), 0);
    const params = await engine.params(0);
    assert.ok(params.some((param) => param.id === 'sampler.interpolation'));
    await engine.close();
  } finally {
    engine.release();
  }
});

test('loadSfz finishes at once while the context is not running', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  context.state = 'suspended';
  const engine = await make(context);
  try {
    await engine.createPart(3, 'sampler');
    const summary = await engine.loadSfz(3, 'tone/voice.sfz', { fetch: server().fetch, base: 'https://samples.test/voices/' });
    assert.equal(summary.regions, 2);
    engine.noteOn(3, 60, 1);
    const { left } = await context.run(4);
    assert.ok(peak(left) > 0.1);
    await engine.close();
  } finally {
    engine.release();
  }
});

test('loadSfz rejects, naming the file, and the part keeps its instrument', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  const engine = await make(context);
  try {
    await engine.createPart(0, 'sampler', 'none');
    await engine.createPart(1, 'synth');
    const { fetch } = server();
    await engine.loadSfz(0, `${BASE}voice.sfz`, { fetch });
    await assert.rejects(engine.loadSfz(0, `${BASE}broken.sfz`, { fetch }), /junk\.wav/);
    await assert.rejects(engine.loadSfz(0, `${BASE}missing.sfz`, { fetch }), /nowhere\.wav: 404/);
    await assert.rejects(engine.loadSfz(0, `${BASE}none.sfz`, { fetch }), /none\.sfz: 404/);
    await assert.rejects(engine.loadSfz(1, `${BASE}voice.sfz`, { fetch }), /not a sampler/);
    await assert.rejects(engine.loadSfz(9, `${BASE}voice.sfz`, { fetch }), /no such part/);
    engine.noteOn(0, 69, 1);
    const { left } = await context.run(4);
    assert.ok(peak(left) > 0.1, 'the first instrument still plays');
    await engine.close();
  } finally {
    engine.release();
  }
});

test('a later load of the same part takes over from one under way', { skip: skipWithoutWasm }, async () => {
  const context = new FakeContext(0);
  context.state = 'running';
  const engine = await make(context);
  try {
    await engine.createPart(0, 'sampler', 'none');
    const { fetch } = server();
    const first = engine.loadSfz(0, `${BASE}voice.sfz`, { fetch });
    const second = engine.loadSfz(0, `${BASE}voice.sfz`, { fetch });
    const both = Promise.allSettled([first, second]);
    const { value: [a, b] } = await runUntil(context, both);
    assert.equal(b.status, 'fulfilled');
    assert.equal(a.status, 'rejected');
    assert.match(String(a.reason), /later load/);
    await engine.close();
  } finally {
    engine.release();
  }
});
