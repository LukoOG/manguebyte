// The event records and the event ring of manguebyte-web, in JavaScript.
//
// This file is shared by the main-thread helper (engine.js) and the audio
// worklet (worklet.js). It follows the layouts documented in the Rust
// crate, `manguebyte_web::record` and `manguebyte_web::ring`, byte for
// byte; js/tests/ring.test.mjs checks it against the same golden bytes as
// the Rust ring.
//
// Record, 32 bytes, little endian:
//
//   bytes  0..8   frame     u64   absolute frame since the engine started
//   byte   8      kind      u8    1 note_on, 2 note_off, 3 param,
//                                 4 choke, 5 all_notes_off, 6 cancel
//   byte   9      flags     u8    bit 0 accent (the note was written in
//                                 capitals); bits 1 to 7 reserved,
//                                 written as 0 and ignored
//   bytes 10..12  part      u16
//   byte  12      note      u8    MIDI note, 0 to 127
//   byte  13      channel   u8    MIDI channel, 0 to 15
//   bytes 14..16  padding         written as 0
//   bytes 16..20  velocity  f32   0 to 1
//   bytes 20..24  param     u32   parameter id: FNV-1a of its text id
//   bytes 24..32  value     f64   in the parameter's own units
//
// Only a note on reads the flags. A 0 there, which is what a writer that
// predates the flags byte put in its padding, means no accent.
//
// The engine ignores a record whose kind is unknown or whose fields are out
// of range. encodeRecord refuses values that do not fit their field at all
// (a note of 300 would otherwise wrap round to 44).
//
// Ring of capacity C (a power of two), 16 + 32 * C bytes:
//
//   bytes 0..4    head      u32   records ever taken out (consumer writes)
//   bytes 4..8    tail      u32   records ever put in (producer writes)
//   bytes 8..12   capacity  u32
//   bytes 12..16  magic     u32   "MBR1"
//   bytes 16..    C slots of 32 bytes; record n is in slot n mod C
//
// head and tail run freely and wrap at 2^32. The producer writes a slot and
// then publishes it with Atomics.store on tail; the consumer reads tail
// with Atomics.load, copies the slots, then hands them back with
// Atomics.store on head. Neither side ever waits.

/** Size of one record in bytes. */
export const RECORD_SIZE = 32;

/** The kind codes of the `kind` byte. */
export const KIND = Object.freeze({
  note_on: 1,
  note_off: 2,
  param: 3,
  choke: 4,
  all_notes_off: 5,
  cancel: 6,
});

/** The bits of the `flags` byte. Bits 1 to 7 are reserved. */
export const FLAG = Object.freeze({
  accent: 1,
});

/** Bytes before the first slot of a ring. */
export const RING_HEADER_BYTES = 16;

/** The ring's magic number: "MBR1" read as a little-endian u32. */
export const RING_MAGIC = 0x3152424d;

/** The largest capacity a ring can have. */
export const RING_MAX_CAPACITY = 1 << 24;

const HEAD = 0;
const TAIL = 1;
const CAPACITY = 2;
const MAGIC = 3;

const TWO_32 = 4294967296;

/** How many bytes a ring of `capacity` records takes. */
export function ringBytes(capacity) {
  return RING_HEADER_BYTES + RECORD_SIZE * capacity;
}

/**
 * The parameter id of a text identifier: the 32-bit FNV-1a hash of its
 * UTF-8 bytes, as `ParamId::from_name` computes it in Rust.
 */
export function paramId(name) {
  const bytes = new TextEncoder().encode(name);
  let hash = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) {
    hash ^= bytes[i];
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

function integer(name, value, max) {
  if (!Number.isInteger(value) || value < 0 || value > max) {
    throw new RangeError(`record ${name} must be an integer from 0 to ${max}, not ${value}`);
  }
  return value;
}

function checkOffset(name, value, length) {
  if (!Number.isInteger(value) || value < 0 || value > length) {
    throw new RangeError(`${name} must be an integer from 0 to ${length}, not ${value}`);
  }
}

function checkCount(name, value) {
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative integer, not ${value}`);
  }
}

function kindCode(kind) {
  if (typeof kind === 'string') {
    const code = KIND[kind];
    if (code === undefined) throw new RangeError(`unknown record kind ${kind}`);
    return code;
  }
  return integer('kind', kind, 255);
}

/**
 * Writes `record` as 32 bytes at `offset` of `view` (a DataView), padding
 * set to zero.
 *
 * `record` has `frame` (a non-negative integer Number up to 2^53, or a
 * BigInt), `kind` (a code or a key of KIND), `part`, and as the kind needs
 * them `note`, `channel`, `velocity`, `accent` (true for an accented note
 * on), `param` (a number, or a text id that is hashed with paramId) and
 * `value`. Missing fields are written as 0. `flags`, the raw byte, is
 * there for tests: it is combined with `accent`, and a writer leaves its
 * reserved bits at 0.
 */
export function encodeRecord(view, offset, record) {
  const frame = record.frame ?? 0;
  if (typeof frame === 'bigint') {
    if (frame < 0n || frame >= 1n << 64n) throw new RangeError(`record frame out of range: ${frame}`);
    view.setBigUint64(offset, frame, true);
  } else {
    integer('frame', frame, Number.MAX_SAFE_INTEGER);
    view.setUint32(offset, frame % TWO_32, true);
    view.setUint32(offset + 4, Math.floor(frame / TWO_32), true);
  }
  const param = typeof record.param === 'string' ? paramId(record.param) : (record.param ?? 0);
  view.setUint8(offset + 8, kindCode(record.kind));
  const flags = integer('flags', record.flags ?? 0, 255) | (record.accent ? FLAG.accent : 0);
  view.setUint8(offset + 9, flags);
  view.setUint16(offset + 10, integer('part', record.part ?? 0, 65535), true);
  view.setUint8(offset + 12, integer('note', record.note ?? 0, 255));
  view.setUint8(offset + 13, integer('channel', record.channel ?? 0, 255));
  view.setUint16(offset + 14, 0, true);
  view.setFloat32(offset + 16, record.velocity ?? 0, true);
  view.setUint32(offset + 20, integer('param', param, 0xffffffff), true);
  view.setFloat64(offset + 24, record.value ?? 0, true);
}

/**
 * Reads the record at `offset` of `view`; the frame comes back as a BigInt,
 * the flags byte as `flags` and its accent bit as `accent`.
 */
export function decodeRecord(view, offset) {
  const flags = view.getUint8(offset + 9);
  return {
    frame: view.getBigUint64(offset, true),
    kind: view.getUint8(offset + 8),
    flags,
    accent: (flags & FLAG.accent) !== 0,
    part: view.getUint16(offset + 10, true),
    note: view.getUint8(offset + 12),
    channel: view.getUint8(offset + 13),
    velocity: view.getFloat32(offset + 16, true),
    param: view.getUint32(offset + 20, true),
    value: view.getFloat64(offset + 24, true),
  };
}

/**
 * One end of an event ring held in a SharedArrayBuffer (or, for tests and
 * the message fallback, a plain ArrayBuffer). The main thread uses it as
 * the producer and the worklet as the consumer; each object must be used
 * by one side only.
 */
export class EventRing {
  /**
   * Makes a new ring that holds `capacity` records, rounded up to a power
   * of two, with both counters at `start`.
   */
  static create(capacity, { shared = true, start = 0 } = {}) {
    if (!Number.isInteger(capacity) || capacity < 1 || capacity > RING_MAX_CAPACITY) {
      throw new RangeError(`a ring holds 1 to ${RING_MAX_CAPACITY} records, not ${capacity}`);
    }
    let rounded = 1;
    while (rounded < capacity) rounded *= 2;
    const bytes = ringBytes(rounded);
    const buffer = shared ? new SharedArrayBuffer(bytes) : new ArrayBuffer(bytes);
    const header = new Uint32Array(buffer, 0, 4);
    header[HEAD] = start >>> 0;
    header[TAIL] = start >>> 0;
    header[CAPACITY] = rounded;
    header[MAGIC] = RING_MAGIC;
    return new EventRing(buffer);
  }

  /** Opens a ring that already exists in `buffer`. */
  constructor(buffer) {
    const header = new Uint32Array(buffer, 0, 4);
    const capacity = header[CAPACITY];
    if (header[MAGIC] !== RING_MAGIC) throw new Error('not an event ring: wrong magic number');
    if (capacity === 0 || (capacity & (capacity - 1)) !== 0 || buffer.byteLength < ringBytes(capacity)) {
      throw new Error('not an event ring: bad capacity');
    }
    this.buffer = buffer;
    this.capacity = capacity;
    this.mask = capacity - 1;
    this.header = header;
    this.bytes = new Uint8Array(buffer, 0, ringBytes(capacity));
    this.view = new DataView(buffer, 0, ringBytes(capacity));
  }

  /** How many records are waiting. */
  get length() {
    const head = Atomics.load(this.header, HEAD);
    const tail = Atomics.load(this.header, TAIL);
    return (tail - head) >>> 0;
  }

  /** How many more records fit right now. */
  get space() {
    return this.capacity - this.length;
  }

  /** Puts a record in (see encodeRecord). Returns false if the ring is full. */
  push(record) {
    const head = Atomics.load(this.header, HEAD);
    const tail = this.header[TAIL];
    if (((tail - head) >>> 0) >= this.capacity) return false;
    encodeRecord(this.view, RING_HEADER_BYTES + RECORD_SIZE * (tail & this.mask), record);
    Atomics.store(this.header, TAIL, (tail + 1) >>> 0);
    return true;
  }

  /**
   * Puts in records already encoded, 32 bytes each, from `source` (a
   * Uint8Array) starting at byte `offset`: `count` of them, or every whole
   * record from `offset` on, and never more than `source` holds from there.
   * Returns how many fitted; the rest are left to the caller.
   *
   * Throws a TypeError if `source` is not a Uint8Array, and a RangeError if
   * `offset` is not an integer from 0 to `source.length`, or `count` is not
   * a non-negative integer. The ring is not touched then.
   */
  pushBytes(source, offset = 0, count = undefined) {
    if (!(source instanceof Uint8Array)) throw new TypeError('pushBytes needs a Uint8Array');
    checkOffset('offset', offset, source.length);
    const whole = Math.floor((source.length - offset) / RECORD_SIZE);
    if (count !== undefined) checkCount('count', count);
    const head = Atomics.load(this.header, HEAD);
    const tail = this.header[TAIL];
    const free = this.capacity - ((tail - head) >>> 0);
    const n = Math.min(count ?? whole, whole, free);
    for (let i = 0; i < n; i++) {
      const slot = RING_HEADER_BYTES + RECORD_SIZE * ((tail + i) & this.mask);
      const from = offset + RECORD_SIZE * i;
      for (let b = 0; b < RECORD_SIZE; b++) this.bytes[slot + b] = source[from + b];
    }
    Atomics.store(this.header, TAIL, (tail + n) >>> 0);
    return n;
  }

  /**
   * Copies up to `max` of the oldest records into `target` (a Uint8Array,
   * such as a view of WebAssembly memory) from byte `offset`, and leaves
   * them in the ring. Never copies more than `target` holds from `offset`.
   * Returns how many were copied. Allocates nothing, so the worklet can
   * call it while rendering. Throws as `pushBytes` does for a bad `target`,
   * `offset` or `max`.
   */
  peekInto(target, offset, max) {
    if (!(target instanceof Uint8Array)) throw new TypeError('peekInto needs a Uint8Array');
    checkOffset('offset', offset, target.length);
    checkCount('max', max);
    const head = this.header[HEAD];
    const tail = Atomics.load(this.header, TAIL);
    const room = Math.floor((target.length - offset) / RECORD_SIZE);
    const n = Math.min((tail - head) >>> 0, max, room);
    const bytes = this.bytes;
    for (let i = 0; i < n; i++) {
      const slot = RING_HEADER_BYTES + RECORD_SIZE * ((head + i) & this.mask);
      const to = offset + RECORD_SIZE * i;
      for (let b = 0; b < RECORD_SIZE; b++) target[to + b] = bytes[slot + b];
    }
    return n;
  }

  /**
   * Hands back the `count` oldest records without reading them, at most as
   * many as are waiting. Returns how many were handed back. Throws a
   * RangeError if `count` is not a non-negative integer.
   */
  discard(count) {
    checkCount('count', count);
    const head = this.header[HEAD];
    const tail = Atomics.load(this.header, TAIL);
    const n = Math.min((tail - head) >>> 0, count);
    Atomics.store(this.header, HEAD, (head + n) >>> 0);
    return n;
  }

  /**
   * Takes out up to `max` records, copying their bytes into `target` from
   * byte `offset`, as `peekInto` does, and hands them back to the
   * producer. Returns how many were taken.
   */
  popInto(target, offset, max) {
    return this.discard(this.peekInto(target, offset, max));
  }

  /** Takes out the oldest record, decoded, or null if none is waiting. */
  pop() {
    const head = this.header[HEAD];
    const tail = Atomics.load(this.header, TAIL);
    if (head === tail) return null;
    const record = decodeRecord(this.view, RING_HEADER_BYTES + RECORD_SIZE * (head & this.mask));
    Atomics.store(this.header, HEAD, (head + 1) >>> 0);
    return record;
  }
}
