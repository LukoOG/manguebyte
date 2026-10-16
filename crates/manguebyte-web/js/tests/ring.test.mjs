// The JavaScript ring and records against the Rust layout.
//
//   node --test crates/manguebyte-web/js/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Worker } from 'node:worker_threads';

import {
  EventRing,
  FLAG,
  KIND,
  RECORD_SIZE,
  RING_MAGIC,
  decodeRecord,
  encodeRecord,
  paramId,
  ringBytes,
} from '../ring.js';

const golden = readFileSync(new URL('./fixtures/ring-golden.hex', import.meta.url), 'utf8').replace(/\s+/g, '');

const hex = (bytes) => Buffer.from(bytes).toString('hex');

// The same scenario as `the_rust_ring_ends_with_the_golden_bytes` in
// crates/manguebyte-web/tests/ring.rs.
const records = [
  { frame: 1, kind: 'note_on', part: 2, note: 60, velocity: 0.5 },
  { frame: 0x0102030405060708n, kind: KIND.param, part: 513, param: 'lowpass.cutoff', value: 1234.5 },
  { frame: 77, kind: 'choke', part: 65535 },
  { frame: Number.MAX_SAFE_INTEGER - 1 + 1, kind: 'all_notes_off', part: 7 },
  { frame: 99, kind: 'note_on', part: 3, note: 127, channel: 9, velocity: 1, accent: true },
  { frame: 5, kind: 200, flags: 0xfe, part: 1, note: 255, channel: 255, velocity: -1.5, param: 0xdeadbeef, value: -0 },
];

test('the JavaScript ring ends with the same bytes as the Rust ring', () => {
  // u64::MAX >> 11 is 2^53 - 1, the largest exact Number.
  assert.equal(records[3].frame, 2 ** 53 - 1);
  const ring = EventRing.create(4, { start: 0xfffffffe });
  assert.equal(ring.push(records[0]), true);
  assert.equal(ring.push(records[1]), true);
  assert.equal(ring.push(records[2]), true);
  const first = ring.pop();
  assert.equal(first.frame, 1n);
  assert.equal(first.note, 60);
  assert.equal(ring.push(records[3]), true);
  assert.equal(ring.push(records[4]), true);
  assert.equal(ring.push(records[5]), false, 'the ring is full');
  assert.equal(ring.pop().part, 513);
  assert.equal(hex(new Uint8Array(ring.buffer)), golden);
  assert.ok(ring.buffer instanceof SharedArrayBuffer);
});

test('a record is laid out byte by byte as agreed', () => {
  const view = new DataView(new ArrayBuffer(RECORD_SIZE));
  new Uint8Array(view.buffer).fill(0xaa); // padding must be overwritten with 0
  encodeRecord(view, 0, {
    frame: 0x0807060504030201n,
    kind: 3,
    flags: 0x09,
    part: 0x0b0a,
    note: 0x0c,
    channel: 0x0d,
    velocity: new Float32Array(new Uint32Array([0x13121110]).buffer)[0],
    param: 0x17161514,
    value: new Float64Array(new BigUint64Array([0x1f1e1d1c1b1a1918n]).buffer)[0],
  });
  assert.equal(
    hex(new Uint8Array(view.buffer)),
    '0102030405060708' + '03' + '09' + '0a0b' + '0c' + '0d' + '0000' + '10111213' + '14151617' + '18191a1b1c1d1e1f',
  );
  const back = decodeRecord(view, 0);
  assert.equal(back.frame, 0x0807060504030201n);
  assert.equal(back.part, 0x0b0a);
  assert.equal(back.param, 0x17161514);
  assert.equal(back.flags, 0x09);
  assert.equal(back.accent, true);
  // A frame as a Number is split into two 32-bit halves exactly.
  encodeRecord(view, 0, { frame: 2 ** 40 + 12345, kind: 1 });
  assert.equal(view.getBigUint64(0, true), 2n ** 40n + 12345n);
});

test('the accent is bit 0 of byte 9, and a record without it has a 0 there', () => {
  assert.deepEqual({ ...FLAG }, { accent: 1 });
  const plain = new Uint8Array(RECORD_SIZE);
  const accented = new Uint8Array(RECORD_SIZE);
  const note = { frame: 10, kind: 'note_on', part: 2, note: 38, velocity: 0.75 };
  encodeRecord(new DataView(plain.buffer), 0, note);
  encodeRecord(new DataView(accented.buffer), 0, { ...note, accent: true });
  assert.equal(plain[9], 0);
  assert.equal(accented[9], 1);
  // Nothing else differs.
  for (let i = 0; i < RECORD_SIZE; i++) if (i !== 9) assert.equal(plain[i], accented[i], `byte ${i}`);
  assert.equal(decodeRecord(new DataView(plain.buffer), 0).accent, false);
  assert.equal(decodeRecord(new DataView(accented.buffer), 0).accent, true);
  // The raw byte and the accent combine; reserved bits read back as they are.
  const view = new DataView(new ArrayBuffer(RECORD_SIZE));
  encodeRecord(view, 0, { ...note, flags: 0xfe, accent: true });
  assert.equal(view.getUint8(9), 0xff);
  encodeRecord(view, 0, { ...note, flags: 0xfe });
  assert.deepEqual([decodeRecord(view, 0).flags, decodeRecord(view, 0).accent], [0xfe, false]);
  for (const flags of [256, -1, 0.5]) assert.throws(() => encodeRecord(view, 0, { ...note, flags }), RangeError);
});

test('the kind codes and the parameter hash match the Rust ones', () => {
  assert.deepEqual({ ...KIND }, { note_on: 1, note_off: 2, param: 3, choke: 4, all_notes_off: 5, cancel: 6 });
  // FNV-1a test vectors, and the ids Rust checks in tests/params.rs.
  assert.equal(paramId(''), 0x811c9dc5);
  assert.equal(paramId('a'), 0xe40c292c);
  assert.equal(paramId('foobar'), 0xbf9cf968);
  assert.equal(paramId('gain'), 458499838);
  assert.equal(paramId('lowpass.cutoff'), 3456878227);
  assert.equal(RING_MAGIC, Buffer.from('MBR1').readUInt32LE(0));
  assert.equal(ringBytes(1024), 16 + 32 * 1024);
});

test('values that do not fit their field are refused, not wrapped', () => {
  const view = new DataView(new ArrayBuffer(RECORD_SIZE));
  for (const bad of [
    { kind: 'nope' },
    { kind: 256 },
    { kind: 1, note: 300 },
    { kind: 1, note: -1 },
    { kind: 1, note: 1.5 },
    { kind: 1, part: 65536 },
    { kind: 1, channel: 256 },
    { kind: 3, param: 2 ** 32 },
    { kind: 1, frame: -1 },
    { kind: 1, frame: 2 ** 53 },
    { kind: 1, frame: Number.NaN },
    { kind: 1, frame: -1n },
    { kind: 1, frame: 2n ** 64n },
  ]) {
    assert.throws(() => encodeRecord(view, 0, bad), RangeError, JSON.stringify(bad, (_, v) => (typeof v === 'bigint' ? `${v}n` : v)));
  }
  // In range for the field, out of range for MIDI: written as it is, for
  // the engine to ignore.
  encodeRecord(view, 0, { kind: 1, note: 200, channel: 99, velocity: 7 });
  assert.equal(view.getUint8(12), 200);
  assert.equal(view.getUint8(13), 99);
});

test('records come out in order across the counter wraparound, and a full ring refuses', () => {
  for (const start of [0xfffffffd, 0xffffffff, 0, 0xffffff9b]) {
    const ring = EventRing.create(5, { start });
    assert.equal(ring.capacity, 8);
    let nextIn = 0;
    let nextOut = 0;
    const target = new Uint8Array(RECORD_SIZE * 8);
    const view = new DataView(target.buffer);
    for (let burst = 0; burst < 200; burst++) {
      for (let i = 0; i < (burst * 7) % 9; i++) {
        if (ring.push({ frame: nextIn, kind: 'choke', part: nextIn % 65536 })) nextIn++;
      }
      assert.equal(ring.length, nextIn - nextOut);
      assert.equal(ring.space, 8 - (nextIn - nextOut));
      const n = ring.popInto(target, 0, (burst * 5) % 7);
      for (let i = 0; i < n; i++) {
        assert.equal(view.getBigUint64(i * RECORD_SIZE, true), BigInt(nextOut));
        nextOut++;
      }
    }
    assert.ok(nextIn > 300);
    while (ring.length > 0) {
      assert.equal(ring.pop().frame, BigInt(nextOut));
      nextOut++;
    }
    assert.equal(nextOut, nextIn);
    assert.equal(ring.pop(), null);
  }
  const ring = EventRing.create(2);
  assert.equal(ring.push({ frame: 1, kind: 4 }), true);
  assert.equal(ring.push({ frame: 2, kind: 4 }), true);
  assert.equal(ring.push({ frame: 3, kind: 4 }), false);
  const encoded = new Uint8Array(RECORD_SIZE * 2);
  encodeRecord(new DataView(encoded.buffer), 0, { frame: 4, kind: 4 });
  encodeRecord(new DataView(encoded.buffer), RECORD_SIZE, { frame: 5, kind: 4 });
  assert.equal(ring.pushBytes(encoded), 0);
  assert.equal(ring.pop().frame, 1n);
  assert.equal(ring.pushBytes(encoded), 1, 'only one fits');
  assert.deepEqual([ring.pop().frame, ring.pop().frame, ring.pop()], [2n, 4n, null]);
});

// Before these checks, pushBytes(new Uint8Array(0), 32) returned -1 and
// moved tail behind head, so the ring claimed 2^32 - 1 records, and a
// negative count replayed stale slots; a count beyond the source pushed
// records of zeros.
test('hostile offsets and counts never move the counters the wrong way', () => {
  const one = new Uint8Array(RECORD_SIZE);
  encodeRecord(new DataView(one.buffer), 0, { frame: 7, kind: 'choke', part: 1 });
  const counters = (ring) => [...new Uint32Array(ring.buffer, 0, 2)];

  const ring = EventRing.create(8);
  assert.equal(ring.pushBytes(one), 1);
  const before = counters(ring);
  for (const [offset, count] of [
    [33, undefined],
    [-1, undefined],
    [0.5, undefined],
    [Number.NaN, undefined],
    [0, -2],
    [0, -1],
    [0, 1.5],
    [0, Number.NaN],
    [0, Number.POSITIVE_INFINITY],
    ['0', undefined],
  ]) {
    assert.throws(() => ring.pushBytes(one, offset, count), RangeError, `${offset} ${count}`);
  }
  assert.throws(() => ring.pushBytes([1, 2, 3]), TypeError);
  assert.throws(() => ring.pushBytes(new Float32Array(8)), TypeError);
  assert.deepEqual(counters(ring), before, 'nothing moved');
  assert.equal(ring.length, 1);

  // At the end of the source, or with a count beyond it: only whole
  // records that are there go in.
  assert.equal(ring.pushBytes(one, 32), 0);
  assert.equal(ring.pushBytes(new Uint8Array(0)), 0);
  assert.equal(ring.pushBytes(one, 0, 3), 1);
  assert.equal(ring.pushBytes(one, 1), 0, 'a torn record is not pushed');
  assert.equal(ring.length, 2);

  const target = new Uint8Array(RECORD_SIZE * 4);
  for (const [offset, max] of [
    [-1, 1],
    [target.length + 1, 1],
    [0, -1],
    [0, 0.5],
    [0, Number.NaN],
  ]) {
    assert.throws(() => ring.popInto(target, offset, max), RangeError, `${offset} ${max}`);
    assert.throws(() => ring.peekInto(target, offset, max), RangeError, `${offset} ${max}`);
  }
  assert.throws(() => ring.discard(-1), RangeError);
  assert.throws(() => ring.discard(Number.NaN), RangeError);
  assert.equal(ring.length, 2, 'nothing was taken');

  // A target too small for what is asked takes what fits.
  assert.equal(ring.popInto(target, target.length - RECORD_SIZE + 1, 2), 0);
  assert.equal(ring.peekInto(target, target.length - RECORD_SIZE, 2), 1);
  assert.equal(ring.length, 2, 'peeking leaves the records in');
  assert.equal(ring.popInto(target, 0, 10), 2);
  assert.equal(ring.length, 0);
  assert.equal(ring.discard(5), 0, 'nothing to hand back');
  assert.deepEqual(counters(ring), [2, 2]);
});

test('peeking and handing back move through the ring in order', () => {
  const ring = EventRing.create(4, { start: 0xfffffffe });
  for (let frame = 0; frame < 4; frame++) assert.equal(ring.push({ frame, kind: 'choke' }), true);
  const target = new Uint8Array(RECORD_SIZE * 4);
  const view = new DataView(target.buffer);
  assert.equal(ring.peekInto(target, 0, 4), 4);
  assert.deepEqual([0, 1, 2, 3].map((i) => view.getBigUint64(i * RECORD_SIZE, true)), [0n, 1n, 2n, 3n]);
  assert.equal(ring.discard(3), 3);
  assert.equal(ring.push({ frame: 4, kind: 'choke' }), true);
  assert.equal(ring.peekInto(target, 0, 4), 2);
  assert.deepEqual([0, 1].map((i) => view.getBigUint64(i * RECORD_SIZE, true)), [3n, 4n]);
  assert.equal(ring.discard(10), 2);
  assert.equal(ring.pop(), null);
});

test('a ring is checked when it is opened', () => {
  assert.throws(() => EventRing.create(0), RangeError);
  assert.throws(() => EventRing.create(2 ** 24 + 1), RangeError);
  assert.throws(() => new EventRing(new SharedArrayBuffer(48)), /magic/);
  const ring = EventRing.create(4);
  new Uint32Array(ring.buffer)[2] = 3;
  assert.throws(() => new EventRing(ring.buffer), /capacity/);
  new Uint32Array(ring.buffer)[2] = 8; // larger than the buffer
  assert.throws(() => new EventRing(ring.buffer), /capacity/);
});

test('two threads pass every record in order through shared memory', async () => {
  const COUNT = 50000;
  const ring = EventRing.create(64, { start: 0xffffff00 });
  const worker = new Worker(
    `
    const { workerData, parentPort } = require('node:worker_threads');
    import(workerData.url).then(({ EventRing }) => {
      const ring = new EventRing(workerData.buffer);
      let frame = 0;
      while (frame < workerData.count) {
        if (ring.push({ frame, kind: 3, part: frame % 65536, param: frame, value: frame / 2 })) frame++;
      }
      parentPort.postMessage('done');
    });
    `,
    { eval: true, workerData: { buffer: ring.buffer, count: COUNT, url: new URL('../ring.js', import.meta.url).href } },
  );
  let expected = 0;
  const target = new Uint8Array(RECORD_SIZE * 16);
  const view = new DataView(target.buffer);
  const deadline = Date.now() + 30000;
  while (expected < COUNT) {
    assert.ok(Date.now() < deadline, `stalled at ${expected}`);
    const n = ring.popInto(target, 0, 16);
    for (let i = 0; i < n; i++) {
      const record = decodeRecord(view, i * RECORD_SIZE);
      assert.equal(record.frame, BigInt(expected));
      assert.equal(record.part, expected % 65536);
      assert.equal(record.param, expected);
      assert.equal(record.value, expected / 2);
      expected++;
    }
    if (n === 0) await new Promise((resolve) => setImmediate(resolve));
  }
  await new Promise((resolve) => worker.once('message', resolve));
  await worker.terminate();
  assert.equal(ring.length, 0);
});
