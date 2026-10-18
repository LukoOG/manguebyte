// The parts' scopes and levels in shared memory, for an editor to draw.
//
// This file is shared by the main-thread helper (engine.js) and the audio
// worklet (worklet.js). The worklet is the only writer: after every render
// quantum it copies the frames each part rendered, and its levels, out of
// the engine (`mb_part_scope`, `mb_part_levels`) into a slot of a
// SharedArrayBuffer. The page reads the slots whenever it likes, without a
// message, as often as it draws. Without shared memory the worklet answers
// messages instead (engine.js partScope and partLevels).
//
// Layout, in the platform's byte order (the two threads share one machine):
//
//   bytes 0..4     magic      u32   "MBS1"
//   bytes 4..8     slots      u32   how many parts it has room for
//   bytes 8..12    capacity   u32   frames of each channel a slot holds,
//                                   a power of two
//   bytes 12..16   0
//   then one slot of 48 + 8 * capacity bytes per part:
//     bytes  0..4    seq      u32   odd while the worklet writes the slot
//     bytes  4..8    part     i32   the part, or -1 for a free slot
//     bytes  8..12   written  u32   frames ever written; wraps at 2^32
//     bytes 12..16   0
//     bytes 16..24   end      f64   the engine frame after the newest one
//     bytes 24..40   levels   f32   peak left, peak right, RMS left, RMS right
//     bytes 40..48   0
//     bytes 48..     left     f32   capacity frames; frame n of the slot
//                                   (counting `written`) at n mod capacity
//     then           right    f32   the same
//
// A slot is a sequence lock: the writer makes `seq` odd with Atomics.add,
// writes, and makes it even again; a reader loads `seq`, copies what it
// wants, and loads `seq` again, trying again if it was odd or has moved.
// The writer never waits. A slot holds the newest `capacity` frames of its
// part; frames before the part was made read as silence, and none come
// before engine frame 0.

/** Frames of each channel a slot holds: `SCOPE_FRAMES` in Rust, 85 ms at 48 kHz. */
export const SCOPE_FRAMES = 4096;

/** The magic number: "MBS1" read as a little-endian u32. */
export const SCOPE_MAGIC = 0x3153424d;

/** Bytes before the first slot. */
export const SCOPE_HEADER_BYTES = 16;

/** Bytes of a slot before its frames. */
export const SLOT_HEADER_BYTES = 48;

/** How many times a reader copies a slot before it gives up on a consistent copy. */
const ATTEMPTS = 64;

// Indices of the slot's header, in 32-bit words from its start.
const SEQ = 0;
const PART = 1;
const WRITTEN = 2;
// Byte offsets.
const END = 16;
const LEVELS = 24;

/** How many bytes the scopes of `slots` parts take, each `capacity` frames long. */
export function scopeBytes(slots, capacity = SCOPE_FRAMES) {
  return SCOPE_HEADER_BYTES + slots * (SLOT_HEADER_BYTES + 8 * capacity);
}

export class PartScopes {
  /** Scopes for `slots` parts in a new SharedArrayBuffer, every slot free. */
  static create(slots, capacity = SCOPE_FRAMES) {
    if (!Number.isInteger(slots) || slots < 1) throw new RangeError(`scopes need at least one slot, not ${slots}`);
    if (!Number.isInteger(capacity) || capacity < 1 || (capacity & (capacity - 1)) !== 0 || capacity > 1 << 24) {
      throw new RangeError(`a scope's capacity must be a power of two up to 2^24, not ${capacity}`);
    }
    const buffer = new SharedArrayBuffer(scopeBytes(slots, capacity));
    const words = new Int32Array(buffer, 0, 4);
    words[0] = SCOPE_MAGIC;
    words[1] = slots;
    words[2] = capacity;
    const scopes = new PartScopes(buffer);
    for (let slot = 0; slot < slots; slot++) scopes.words[scopes.base(slot) / 4 + PART] = -1;
    return scopes;
  }

  /** The scopes in `buffer`, as create() laid them out. */
  constructor(buffer) {
    const header = new Int32Array(buffer, 0, 4);
    if (header[0] !== SCOPE_MAGIC) throw new Error('not a buffer of part scopes');
    this.buffer = buffer;
    this.slots = header[1];
    this.capacity = header[2];
    this.slotBytes = SLOT_HEADER_BYTES + 8 * this.capacity;
    if (buffer.byteLength < scopeBytes(this.slots, this.capacity)) throw new Error('the buffer of part scopes is too short');
    this.words = new Int32Array(buffer);
    this.floats = new Float32Array(buffer);
    this.doubles = new Float64Array(buffer);
  }

  /** Where slot `slot` starts, in bytes. */
  base(slot) {
    return SCOPE_HEADER_BYTES + slot * this.slotBytes;
  }

  // The writer's side: the worklet, on the audio thread. None of these
  // allocates.

  /**
   * Gives slot `slot` to part `part`, with the part's newest `count` frames
   * (at most the capacity) from `source`, a Float32Array, at `left` and
   * `right`, the frame after them `end` and its four `levels` at `levels`.
   */
  assign(slot, part, source, left, right, count, end, levels) {
    const word = this.base(slot) / 4;
    Atomics.add(this.words, word + SEQ, 1);
    this.words[word + PART] = part;
    this.words[word + WRITTEN] = 0;
    this.put(slot, source, left, right, count, this.capacity, end, levels);
    Atomics.add(this.words, word + SEQ, 1);
  }

  /**
   * Adds `advance` frames to slot `slot`, of which the newest `count` (at
   * most the capacity) are in `source` at `left` and `right`; any before
   * them read as silence. Then the frame after them, `end`, and the four
   * levels at `levels`.
   */
  append(slot, source, left, right, count, advance, end, levels) {
    const word = this.base(slot) / 4;
    Atomics.add(this.words, word + SEQ, 1);
    this.put(slot, source, left, right, count, advance, end, levels);
    Atomics.add(this.words, word + SEQ, 1);
  }

  put(slot, source, left, right, count, advance, end, levels) {
    const base = this.base(slot);
    const word = base / 4;
    const mask = this.capacity - 1;
    const frames = word + SLOT_HEADER_BYTES / 4;
    const written = this.words[word + WRITTEN] >>> 0;
    const first = written + advance - count;
    // Frames the engine no longer had read as silence.
    for (let k = Math.max(0, advance - this.capacity); k < advance - count; k++) {
      const at = (written + k) & mask;
      this.floats[frames + at] = 0;
      this.floats[frames + this.capacity + at] = 0;
    }
    for (let k = 0; k < count; k++) {
      const at = (first + k) & mask;
      this.floats[frames + at] = source[left + k];
      this.floats[frames + this.capacity + at] = source[right + k];
    }
    this.words[word + WRITTEN] = (written + advance) >>> 0;
    this.doubles[(base + END) / 8] = end;
    for (let i = 0; i < 4; i++) this.floats[(base + LEVELS) / 4 + i] = source[levels + i];
  }

  /** Frees slot `slot`. */
  release(slot) {
    const word = this.base(slot) / 4;
    Atomics.add(this.words, word + SEQ, 1);
    this.words[word + PART] = -1;
    Atomics.add(this.words, word + SEQ, 1);
  }

  // The reader's side: the page.

  /** The slot part `part` is in, or -1. */
  slotOf(part) {
    for (let slot = 0; slot < this.slots; slot++) {
      if (Atomics.load(this.words, this.base(slot) / 4 + PART) === part) return slot;
    }
    return -1;
  }

  /** The parts that have a slot, in increasing order. */
  parts() {
    const parts = [];
    for (let slot = 0; slot < this.slots; slot++) {
      const part = Atomics.load(this.words, this.base(slot) / 4 + PART);
      if (part >= 0) parts.push(part);
    }
    return parts.sort((a, b) => a - b);
  }

  /**
   * Copies a consistent view of part `part`'s slot with `copy(base, word)`,
   * which returns what it read, or null if the part has no slot. A copy the
   * writer moved under keeps being tried, ATTEMPTS times at most; the last
   * one is kept after that.
   */
  consistent(part, copy) {
    let result = null;
    for (let attempt = 0; attempt < ATTEMPTS; attempt++) {
      const slot = this.slotOf(part);
      if (slot < 0) return null;
      const base = this.base(slot);
      const word = base / 4;
      const before = Atomics.load(this.words, word + SEQ);
      if ((before & 1) !== 0) continue;
      if (this.words[word + PART] !== part) continue;
      result = copy(base, word);
      if (Atomics.load(this.words, word + SEQ) === before) return result;
    }
    return result;
  }

  /**
   * The newest `frames` frames of part `part` (at most the capacity and at
   * most the frames since engine frame 0): `{ frame, left, right }`, `frame`
   * the engine frame of the first sample. Null if the part has no slot.
   */
  read(part, frames) {
    return this.consistent(part, (base, word) => {
      const end = this.doubles[(base + END) / 8];
      const count = Math.max(0, Math.min(Math.floor(frames), this.capacity, end));
      const written = this.words[word + WRITTEN] >>> 0;
      const mask = this.capacity - 1;
      const at = word + SLOT_HEADER_BYTES / 4;
      const left = new Float32Array(count);
      const right = new Float32Array(count);
      const first = (written - count) >>> 0;
      for (let k = 0; k < count; k++) {
        const index = (first + k) & mask;
        left[k] = this.floats[at + index];
        right[k] = this.floats[at + this.capacity + index];
      }
      return { frame: end - count, left, right };
    });
  }

  /**
   * The levels of part `part` on the newest frame in its slot:
   * `{ frame, peak: [left, right], rms: [left, right] }`, linear, `frame`
   * the frame after them. Null if the part has no slot.
   */
  levels(part) {
    return this.consistent(part, (base) => {
      const at = (base + LEVELS) / 4;
      const f = this.floats;
      return {
        frame: this.doubles[(base + END) / 8],
        peak: [f[at], f[at + 1]],
        rms: [f[at + 2], f[at + 3]],
      };
    });
  }
}
