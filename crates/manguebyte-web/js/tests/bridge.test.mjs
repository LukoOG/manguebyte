// The bridge's client (bridge.js) against a stand-in for the plugin's
// socket: what it sends, byte for byte, and what it does with what the
// plugin says. The Rust side reads and writes the same golden bytes
// (plugins/aratu-bridge/tests/golden.rs) and is tested against a real
// socket there.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  AratuBridge,
  DEFAULT_PORT,
  MESSAGE,
  NOTICE,
  PROTOCOL_VERSION,
  QUERY,
  SUBPROTOCOL,
  eventsBody,
  message,
} from '../bridge.js';
import { CHAIN, INSTRUMENT, KIND, paramId } from '../engine.js';

const golden = readFileSync(new URL('./fixtures/bridge-golden.hex', import.meta.url), 'utf8')
  .split('\n')
  .filter((line) => !line.startsWith('#'))
  .join('')
  .replace(/\s+/g, '');

const hex = (bytes) => Buffer.from(bytes).toString('hex');

/** A WebSocket that records what is sent and lets the test answer. */
class MockSocket {
  static last = null;

  constructor(url, protocol) {
    this.url = url;
    this.protocol = protocol;
    this.readyState = 0;
    this.sent = [];
    this.closedWith = null;
    MockSocket.last = this;
    queueMicrotask(() => {
      this.readyState = 1;
      this.onopen?.();
    });
  }

  send(bytes) {
    this.sent.push(new Uint8Array(bytes));
  }

  close(code, reason) {
    this.closedWith = code;
    this.readyState = 3;
    this.onclose?.({ code, reason });
  }

  /** The plugin sends `bytes`. */
  receive(bytes) {
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }

  /** Decodes what the client sent: type, request and body. */
  messages() {
    return this.sent.map((bytes) => {
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      return { kind: view.getUint8(0), request: view.getUint32(4, true), body: bytes.subarray(8) };
    });
  }

  take() {
    const all = this.messages();
    this.sent = [];
    return all;
  }
}

function welcome({ epoch = 0, sampleRate = 48000 } = {}) {
  const body = new Uint8Array(36 + 5);
  const view = new DataView(body.buffer);
  view.setUint32(0, PROTOCOL_VERSION, true);
  view.setUint32(4, 2, true);
  view.setFloat64(8, sampleRate, true);
  view.setUint32(16, 111, true);
  view.setUint32(20, 512, true);
  view.setUint32(24, 64, true);
  view.setUint32(28, 1 << 26, true);
  view.setUint32(32, epoch, true);
  body.set(new TextEncoder().encode('0.1.0'), 36);
  return message(MESSAGE.welcome, 1, body);
}

function reply(request, status, payload = new Uint8Array(0)) {
  const body = new Uint8Array(4 + payload.byteLength);
  new DataView(body.buffer).setInt32(0, status, true);
  body.set(payload, 4);
  return message(MESSAGE.reply, request, body);
}

function transport({ beat = 0, tempo = 120, playing = true, epoch = 0, loop = null, age = 0 } = {}) {
  const body = new Uint8Array(68);
  const view = new DataView(body.buffer);
  view.setFloat64(0, beat, true);
  view.setFloat64(8, tempo, true);
  view.setFloat64(16, Math.floor(beat / 4) * 4, true);
  view.setFloat64(24, loop ? loop[0] : NaN, true);
  view.setFloat64(32, loop ? loop[1] : NaN, true);
  view.setFloat64(40, age, true);
  view.setBigUint64(48, 1n, true);
  view.setUint32(56, epoch, true);
  view.setUint16(60, 4, true);
  view.setUint16(62, 4, true);
  view.setUint8(64, (playing ? 1 : 0) | (loop ? 4 : 0) | 8);
  return message(MESSAGE.transport, 0, body);
}

/** The records of an events message: { epoch, records: [{ beat, kind, part, note, ... }] }. */
function events(body) {
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  const records = [];
  for (let at = 8; at < body.byteLength; at += 32) {
    records.push({
      beat: view.getFloat64(at, true),
      kind: view.getUint8(at + 8),
      flags: view.getUint8(at + 9),
      part: view.getUint16(at + 10, true),
      note: view.getUint8(at + 12),
      velocity: view.getFloat32(at + 16, true),
      param: view.getUint32(at + 20, true),
      value: view.getFloat64(at + 24, true),
    });
  }
  return { epoch: view.getUint32(0, true), records };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A connected bridge, its socket, and a clock the test moves. */
async function connected(options = {}) {
  const clock = { now: 100 };
  const connecting = AratuBridge.connect({ WebSocket: MockSocket, clock: () => clock.now, ...options });
  await flush();
  const socket = MockSocket.last;
  const [hello] = socket.take();
  assert.equal(hello.kind, MESSAGE.hello);
  socket.receive(welcome(options));
  const bridge = await connecting;
  return { bridge, socket, clock };
}

/** Answers every request the client has sent with `status`. */
function answerAll(socket, status = 0, payload) {
  for (const sent of socket.take()) {
    if (sent.kind !== MESSAGE.events) socket.receive(reply(sent.request, status, payload));
  }
}

test('the events message is laid out as the Rust side lays it out', () => {
  const body = eventsBody(7, [
    { beat: 1.5, record: { kind: 'note_on', part: 3, note: 60, channel: 2, velocity: 0.75, accent: true } },
    { beat: NaN, record: { kind: 'param', part: 0, param: 'lowpass.cutoff', value: 1234.5 } },
  ]);
  assert.equal(hex(message(MESSAGE.events, 9, body)), golden);
});

test('it connects to 127.0.0.1 with the subprotocol and says hello', async () => {
  const states = [];
  const connecting = AratuBridge.connect({ WebSocket: MockSocket, clock: () => 0, name: 'test page' });
  const socket = MockSocket.last;
  assert.equal(socket.url, `ws://127.0.0.1:${DEFAULT_PORT}`);
  assert.equal(socket.protocol, SUBPROTOCOL);
  await flush();
  const [hello] = socket.take();
  assert.equal(hello.kind, MESSAGE.hello);
  const view = new DataView(hello.body.buffer, hello.body.byteOffset, hello.body.byteLength);
  assert.equal(view.getUint32(0, true), PROTOCOL_VERSION);
  assert.equal(view.getUint32(4, true), 2);
  assert.equal(new TextDecoder().decode(hello.body.subarray(8)), 'test page');
  socket.receive(welcome({ sampleRate: 44100, epoch: 6 }));
  const bridge = await connecting;
  bridge.onstate = (state) => states.push(state);
  assert.equal(bridge.state, 'open');
  assert.equal(bridge.mode, 'bridge');
  assert.equal(bridge.sampleRate, 44100);
  assert.equal(bridge.epoch, 6);
  assert.equal(bridge.welcome.latency, 111);
  await bridge.close();
  assert.equal(socket.closedWith, 1000);
  assert.deepEqual(states, ['closed']);
  assert.throws(() => bridge.noteOn(0, 60), /closed/);
});

test('a refusal or a closed socket rejects the connection', async () => {
  const connecting = AratuBridge.connect({ WebSocket: MockSocket, clock: () => 0 });
  await flush();
  MockSocket.last.close(4000, 'version mismatch');
  await assert.rejects(connecting, /4000.*version mismatch/);
});

test('control calls carry their arguments and fail with the status', async () => {
  const { bridge, socket } = await connected();
  const made = bridge.createPart(2, 'plucked', ['lowpass', 'delay']);
  const [create] = socket.messages();
  assert.equal(create.kind, MESSAGE.createPart);
  const view = new DataView(create.body.buffer, create.body.byteOffset, create.body.byteLength);
  assert.equal(view.getUint16(0, true), 2);
  assert.equal(view.getUint32(4, true), INSTRUMENT.plucked);
  assert.equal(view.getUint32(8, true), CHAIN.lowpass | CHAIN.delay);
  answerAll(socket);
  await made;
  assert.equal(await bridge.partChain(2), CHAIN.lowpass | CHAIN.delay, 'remembered, as the local engine does');

  // An effect the part does not have: the chain grows first, then the
  // settings go out with the switch.
  const effect = bridge.setEffect(2, 'chorus', { mix: 0.5 }, undefined);
  await flush();
  const [chain] = socket.messages();
  assert.equal(chain.kind, MESSAGE.setChain);
  answerAll(socket);
  await effect;
  await flush();
  const [sent] = socket.take();
  const { records } = events(sent.body);
  assert.deepEqual(
    records.map((record) => [record.kind, record.param, record.value]),
    [
      [KIND.param, paramId('chorus.mix'), 0.5],
      [KIND.param, paramId('chorus.on'), 1],
    ],
  );
  assert.ok(records.every((record) => Number.isNaN(record.beat)), 'no time: as soon as possible');

  const removed = bridge.removePart(9);
  answerAll(socket, -6);
  await assert.rejects(removed, /removePart: no such part/);
  await assert.rejects(bridge.paramValue(0, 'gain'), /not available over the bridge/);
  await assert.rejects(bridge.partScope(0), /not available over the bridge/);
});

test('in beats mode a time is a beat, and past the loop it goes to the next lap', async () => {
  const { bridge, socket } = await connected({ timeBase: 'beats', epoch: 3 });
  socket.receive(transport({ beat: 6, epoch: 3, loop: [4, 8] }));
  bridge.noteOn(1, 60, 0.5, 7.5, 0, true);
  bridge.noteOff(1, 60, 8.25);
  bridge.noteOn(1, 62, 0.5, 13); // beyond the next lap: held
  await flush();
  const sent = socket.take().map((m) => events(m.body));
  assert.deepEqual(
    sent.map(({ epoch, records }) => [epoch, records.map((r) => [r.kind, r.beat])]),
    [
      [3, [[KIND.note_on, 7.5]]],
      [4, [[KIND.note_off, 4.25]]],
    ],
  );
  assert.equal(sent[0].records[0].flags & 1, 1, 'accented');
  // The loop goes round: the next lap's events are already there, the
  // held one is not due yet; the old lap is forgotten.
  socket.receive(transport({ beat: 4.1, epoch: 4, loop: [4, 8] }));
  await flush();
  assert.deepEqual(socket.take(), []);
});

test('in seconds mode a time maps to a beat at the DAW tempo, and a jump sends again', async () => {
  const { bridge, socket, clock } = await connected();
  const seen = [];
  bridge.ontransport = (t) => seen.push(t);
  // At clock 100 the DAW is at beat 16 and plays at 120 bpm.
  socket.receive(transport({ beat: 16, epoch: 0 }));
  assert.equal(bridge.beatAt(100.5), 17);
  assert.equal(bridge.timeAtBeat(18), 101);
  clock.now = 100.25;
  assert.equal(bridge.currentBeat, 16.5);
  bridge.noteOn(0, 36, 1, bridge.context.currentTime + 0.5); // beat 17.5
  bridge.setParam(0, 'gain', -3, 102); // beat 20
  await flush();
  let [first] = socket.take();
  assert.deepEqual(events(first.body).records.map((r) => r.beat), [17.5, 20]);
  // The user moves the cursor to beat 0: what is still to come is sent
  // again for the new epoch, from the new position.
  clock.now = 100.5;
  socket.receive(transport({ beat: 0, epoch: 2 }));
  await flush();
  [first] = socket.take();
  const again = events(first.body);
  assert.equal(again.epoch, 2);
  assert.deepEqual(again.records.map((r) => r.beat), [0.5, 3]);
  assert.equal(seen.at(-1).jumped, true);
});

test('while the DAW is stopped, seconds-mode events play when due', async () => {
  const { bridge, socket, clock } = await connected();
  socket.receive(transport({ beat: 2, playing: false }));
  bridge.noteOn(0, 38, 1, 100.2);
  await flush();
  assert.deepEqual(socket.take(), [], 'held');
  clock.now = 100.3;
  bridge.pump();
  await flush();
  const [sent] = socket.take();
  const { records } = events(sent.body);
  assert.equal(records.length, 1);
  assert.ok(Number.isNaN(records[0].beat), 'played as soon as possible');
  bridge.release();
});

test('queries, sync, latency and stats go over the socket', async () => {
  const { bridge, socket } = await connected();
  const json = bridge.drumMapJson();
  const [asked] = socket.take();
  assert.equal(asked.kind, MESSAGE.query);
  assert.equal(new DataView(asked.body.buffer, asked.body.byteOffset).getUint32(0, true), QUERY.drumMapJson);
  socket.receive(reply(asked.request, 0, new TextEncoder().encode('[{"name":"kick"}]')));
  assert.equal(await json, '[{"name":"kick"}]');

  const synced = bridge.sync();
  answerAll(socket);
  await synced;

  const latency = bridge.latency();
  const payload = new Uint8Array(16);
  const view = new DataView(payload.buffer);
  view.setUint32(0, 111, true);
  view.setUint32(4, 512, true);
  view.setFloat64(8, 0.051, true);
  answerAll(socket, 0, payload);
  assert.deepEqual(await latency, {
    engineFrames: 111,
    outputFrames: 0,
    frames: 111,
    seconds: 111 / 48000,
    maxBlock: 512,
    ahead: 0.051,
  });

  const stats = bridge.stats();
  const counters = new Uint8Array(19 * 8);
  const cv = new DataView(counters.buffer);
  cv.setBigUint64(4 * 8, 42n, true); // applied
  cv.setBigUint64(17 * 8, 3n, true); // overridden
  cv.setFloat64(18 * 8, -1.5, true);
  answerAll(socket, 0, counters);
  const read = await stats;
  assert.equal(read.applied, 42);
  assert.equal(read.overridden, 3);
  assert.equal(read.limiterDb, -1.5);

  const notices = [];
  bridge.onnotice = (notice) => notices.push(notice);
  bridge.chains.set(0, 3);
  const body = new Uint8Array(4 + 2);
  new DataView(body.buffer).setUint32(0, NOTICE.engineReset, true);
  body.set(new TextEncoder().encode('hi'), 4);
  socket.receive(message(MESSAGE.notice, 0, body));
  assert.deepEqual(notices, [{ code: NOTICE.engineReset, text: 'hi' }]);
  assert.equal(bridge.chains.size, 0, 'parts must be made again');
  bridge.release();
});

test('an SFZ instrument goes as bytes, file by file', async () => {
  const { bridge, socket } = await connected();
  const files = {
    'https://example.test/kit/kit.sfz': new TextEncoder().encode('<region> sample=kick.wav'),
    'https://example.test/kit/kick.wav': new Uint8Array([1, 2, 3]),
  };
  const fetch = async (url) => ({
    ok: Object.hasOwn(files, url),
    status: 200,
    arrayBuffer: async () => files[url].slice().buffer,
  });
  const loading = bridge.loadSfz(4, 'https://example.test/kit/kit.sfz', { fetch });
  const answer = async (kind, status, payload) => {
    for (let i = 0; i < 20 && socket.sent.length === 0; i++) await flush();
    const [sent] = socket.take();
    assert.equal(sent.kind, kind);
    socket.receive(reply(sent.request, status, payload));
    return sent;
  };
  const begin = await answer(MESSAGE.sfzBegin, 0, new TextEncoder().encode('["kick.wav"]'));
  assert.equal(new TextDecoder().decode(begin.body.subarray(4)), '<region> sample=kick.wav');
  const file = await answer(MESSAGE.sfzFile, 0);
  const view = new DataView(file.body.buffer, file.body.byteOffset, file.body.byteLength);
  assert.equal(view.getUint16(0, true), 4);
  assert.equal(view.getUint16(2, true), 8);
  assert.equal(new TextDecoder().decode(file.body.subarray(4, 12)), 'kick.wav');
  assert.deepEqual([...file.body.subarray(12)], [1, 2, 3]);
  await answer(
    MESSAGE.sfzFinish,
    0,
    new TextEncoder().encode('{"regions":1,"samples":1,"sample_bytes":12,"warnings":[],"make_up":2.5}'),
  );
  const summary = await loading;
  assert.equal(summary.regions, 1);
  assert.deepEqual(await loading.normalized, { makeUp: 2.5 });

  const failing = bridge.loadSfz(4, 'https://example.test/kit/kit.sfz', { fetch });
  await answer(MESSAGE.sfzBegin, -8);
  await assert.rejects(failing, /the part is not a sampler/);
  bridge.release();
});
