// The main-thread helper of manguebyte-web.
//
//   import { ManguebyteEngine } from './engine.js';
//
//   const context = new AudioContext();
//   const engine = await ManguebyteEngine.create(context, { wasm: 'manguebyte_web.wasm' });
//   engine.node.connect(context.destination);
//   await engine.createPart(0, 'drums');
//   engine.noteOn(0, 36, 1.0, context.currentTime + 0.1);
//
// It loads worklet.js into the context, makes the AudioWorkletNode, owns the
// event ring, and turns AudioContext times into the engine's frames.
//
// Shared memory and its fallback. When SharedArrayBuffer is available the
// records travel through a ring in shared memory (ring.js), with Atomics on
// its head and tail, and nothing is posted per event. A page only gets
// SharedArrayBuffer when it is cross-origin isolated, which takes two
// response headers on the page:
//
//   Cross-Origin-Opener-Policy: same-origin
//   Cross-Origin-Embedder-Policy: require-corp
//
// Without them the helper copies the records into messages on the node's
// port instead; timing stays exact as long as the records arrive before
// their frame.
//
// Sample rate. The engine runs at the context's sample rate, from 3000 to
// 768000 Hz (`EngineConfig::MIN_SAMPLE_RATE` and `MAX_SAMPLE_RATE`, the range
// browsers allow for an AudioContext). create() rejects with a message that
// names the rate if the context's is outside it.
//
// Time. The engine counts frames from the first frame its worklet renders.
// frameAt(time) maps an AudioContext time in seconds to that count, using
// the context frame the worklet reports for its frame 0. Until the first
// report it assumes the engine starts at the context's current time, which
// holds while the context is suspended and is corrected when the report
// arrives. A record whose frame has passed when it reaches the engine is
// played at the start of the next render quantum.

import { EventRing, KIND, RECORD_SIZE, encodeRecord, paramId } from './ring.js';

export { FLAG, KIND, RECORD_SIZE, paramId } from './ring.js';

/** Instrument codes, as `InstrumentKind::code` gives them. */
export const INSTRUMENT = Object.freeze({ synth: 0, drums: 1 });

/** Chain bits, as `Chain::bits` gives them. */
export const CHAIN = Object.freeze({ none: 0, lowpass: 1, reverb: 2, full: 3 });

/** The drum kit's General MIDI notes, by name. */
export const DRUM_NOTES = Object.freeze({
  kick: 36,
  snare: 38,
  closed_hat: 42,
  open_hat: 46,
  crash: 49,
  ride: 51,
});

/** The drum kit's notes by the symbols Aratu writes. */
export const DRUM_SYMBOLS = Object.freeze({ b: 36, s: 38, c: 42, o: 46, x: 49, r: 51 });

/** The status codes of the control calls (`ffi::status`). */
export const STATUS = Object.freeze({
  0: 'ok',
  [-1]: 'no engine',
  [-2]: 'part number above 65535',
  [-3]: 'unknown instrument',
  [-4]: 'unknown chain bit',
  [-5]: 'every part slot is taken',
  [-6]: 'no such part',
  [-7]: 'configuration out of range',
});

const PROCESSOR_NAME = 'manguebyte-engine';

function chainBits(chain) {
  if (chain === undefined) return CHAIN.full;
  if (typeof chain === 'number') return chain;
  if (typeof chain === 'string') {
    if (!(chain in CHAIN)) throw new RangeError(`unknown chain ${chain}`);
    return CHAIN[chain];
  }
  return (chain.lowpass ? CHAIN.lowpass : 0) | (chain.reverb ? CHAIN.reverb : 0);
}

function instrumentCode(kind) {
  if (typeof kind === 'number') return kind;
  if (!(kind in INSTRUMENT)) throw new RangeError(`unknown instrument ${kind}`);
  return INSTRUMENT[kind];
}

async function wasmBytes(source) {
  if (source instanceof WebAssembly.Module) return source;
  if (source instanceof ArrayBuffer) return source.slice(0);
  if (ArrayBuffer.isView(source)) {
    return source.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength);
  }
  if (typeof Response !== 'undefined' && source instanceof Response) return source.arrayBuffer();
  if (typeof source === 'string' || source instanceof URL) {
    const response = await fetch(source);
    if (!response.ok) throw new Error(`could not load ${source}: ${response.status}`);
    return response.arrayBuffer();
  }
  throw new TypeError('options.wasm must be a URL, bytes, a Response or a WebAssembly.Module');
}

export class ManguebyteEngine {
  /**
   * Loads the worklet into `context`, makes the node and waits until the
   * engine is running in it. Options:
   *
   *   wasm             the module: URL, ArrayBuffer, typed array, Response
   *                    or WebAssembly.Module (required)
   *   workletUrl       where worklet.js is (default: next to this file)
   *   shared           use a SharedArrayBuffer ring (default: when the page
   *                    is cross-origin isolated)
   *   ringCapacity     records the ring holds (default 4096)
   *   maxQuantum, maxParts, pendingCapacity, stagingRecords
   *                    engine limits (defaults 512, 32, 4096, 256)
   *   AudioWorkletNode the node class (default: the global one)
   */
  static async create(context, options = {}) {
    const workletUrl = options.workletUrl ?? new URL('./worklet.js', import.meta.url);
    const shared =
      options.shared ??
      (typeof SharedArrayBuffer === 'function' && globalThis.crossOriginIsolated === true);
    if (options.wasm === undefined) throw new TypeError('options.wasm is required');
    const [wasm] = await Promise.all([wasmBytes(options.wasm), context.audioWorklet.addModule(workletUrl)]);
    const ring = shared ? EventRing.create(options.ringCapacity ?? 4096) : null;
    const Node = options.AudioWorkletNode ?? globalThis.AudioWorkletNode;
    const node = new Node(context, PROCESSOR_NAME, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      processorOptions: {
        wasm,
        ring: ring ? ring.buffer : null,
        maxQuantum: options.maxQuantum ?? 512,
        maxParts: options.maxParts ?? 32,
        pendingCapacity: options.pendingCapacity ?? 4096,
        stagingRecords: options.stagingRecords ?? 256,
      },
    });
    const engine = new ManguebyteEngine(context, node, ring);
    try {
      await engine.ready;
    } catch (error) {
      engine.release();
      throw error;
    }
    return engine;
  }

  /** Use create(). */
  constructor(context, node, ring) {
    this.context = context;
    this.node = node;
    this.ring = ring;
    this.sampleRate = context.sampleRate;
    this.origin = Math.round(context.currentTime * context.sampleRate);
    this.originReported = false;
    this.backlog = [];
    this.backlogTimer = null;
    this.batch = null;
    this.batchLength = 0;
    this.nextId = 1;
    this.waiting = new Map();
    this.closed = false;
    this.ready = new Promise((resolve, reject) => {
      this.readyCallbacks = { resolve, reject };
    });
    node.port.onmessage = (event) => this.receive(event.data);
  }

  /** 'shared' when records go through the shared ring, 'message' otherwise. */
  get mode() {
    return this.ring ? 'shared' : 'message';
  }

  receive(message) {
    switch (message && message.type) {
      case 'ready':
        this.readyCallbacks.resolve();
        break;
      case 'error':
        this.readyCallbacks.reject(new Error(`manguebyte worklet: ${message.message}`));
        break;
      case 'origin':
        this.origin = message.origin;
        this.originReported = true;
        break;
      case 'reply': {
        const callbacks = this.waiting.get(message.id);
        if (callbacks) {
          this.waiting.delete(message.id);
          callbacks.resolve(message.result);
        }
        break;
      }
      default:
    }
  }

  request(type, fields = {}) {
    if (this.closed) return Promise.reject(new Error('the engine is closed'));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.node.port.postMessage({ type, id, ...fields });
    });
  }

  async control(type, fields) {
    const status = await this.request(type, fields);
    if (status !== 0) throw new Error(`${type}: ${STATUS[status] ?? `status ${status}`}`);
  }

  /** The engine frame of AudioContext time `time` (seconds). */
  frameAt(time) {
    return Math.round(time * this.sampleRate) - this.origin;
  }

  /** The AudioContext time of engine frame `frame`. */
  timeAt(frame) {
    return (frame + this.origin) / this.sampleRate;
  }

  /** The frame `time` maps to, or 0 ("as soon as possible") if it is undefined or has passed. */
  frameFor(time) {
    return time === undefined ? 0 : Math.max(0, this.frameAt(time));
  }

  /**
   * Sends one record (see encodeRecord in ring.js): `frame` is an engine
   * frame; a record whose frame has passed plays at the next quantum.
   * Returns true if the record was handed over at once, false if the ring
   * was full and it waits, in order, to be handed over shortly.
   */
  schedule(record) {
    if (this.closed) throw new Error('the engine is closed');
    if (!this.ring) {
      this.addToBatch(record);
      return true;
    }
    if (this.backlog.length === 0 && this.ring.push(record)) return true;
    const bytes = new Uint8Array(RECORD_SIZE);
    encodeRecord(new DataView(bytes.buffer), 0, record);
    this.backlog.push(bytes);
    this.flushBacklog();
    return false;
  }

  addToBatch(record) {
    if (!this.batch) {
      this.batch = new DataView(new ArrayBuffer(RECORD_SIZE * 64));
      this.batchLength = 0;
      queueMicrotask(() => this.flushBatch());
    }
    if ((this.batchLength + 1) * RECORD_SIZE > this.batch.byteLength) {
      const grown = new Uint8Array(this.batch.byteLength * 2);
      grown.set(new Uint8Array(this.batch.buffer));
      this.batch = new DataView(grown.buffer);
    }
    encodeRecord(this.batch, this.batchLength * RECORD_SIZE, record);
    this.batchLength += 1;
  }

  flushBatch() {
    if (!this.batch) return;
    const bytes = this.batch.buffer.slice(0, this.batchLength * RECORD_SIZE);
    this.batch = null;
    this.batchLength = 0;
    if (!this.closed) this.node.port.postMessage({ type: 'records', bytes }, [bytes]);
  }

  flushBacklog() {
    while (this.backlog.length > 0 && this.ring.pushBytes(this.backlog[0]) === 1) this.backlog.shift();
    if (this.backlog.length > 0 && this.backlogTimer === null && !this.closed) {
      this.backlogTimer = setTimeout(() => {
        this.backlogTimer = null;
        this.flushBacklog();
      }, 5);
    }
  }

  /**
   * Resolves once the worklet has received every record scheduled so far
   * by message. In shared mode the ring is seen at once and this only
   * waits for one round trip. Useful before OfflineAudioContext
   * startRendering(), which can finish before a message arrives.
   */
  async sync() {
    this.flushBatch();
    await this.request('sync');
  }

  /**
   * Starts `note` on `part` with `velocity` (0 to 1, default 1) at
   * AudioContext time `time` (default: now). A velocity of 0 is a note off,
   * as in MIDI. An `accent`ed note, what Aratu writes in capitals, plays
   * louder and brighter.
   */
  noteOn(part, note, velocity = 1, time = undefined, channel = 0, accent = false) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.note_on, part, note, channel, velocity, accent });
  }

  /** Releases `note` on `part` at `time`. */
  noteOff(part, note, time, channel = 0) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.note_off, part, note, channel });
  }

  /** Sets parameter `id` (its text id or its number) of `part` to `value`, in its own units, at `time`. */
  setParam(part, id, value, time) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.param, part, param: id, value });
  }

  /** Releases every note of `part` at `time` (default: now). */
  allNotesOff(part, time) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.all_notes_off, part });
  }

  /** Stops every note of `part` at once, at `time`. */
  choke(part, time) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.choke, part });
  }

  /** Makes part `part` a 'synth' or 'drums' with `chain` ('full', 'none', bits or {lowpass, reverb}). */
  createPart(part, kind = 'synth', chain = 'full') {
    return this.control('createPart', { part, kind: instrumentCode(kind), chain: chainBits(chain) });
  }

  /** Removes a part. */
  removePart(part) {
    return this.control('removePart', { part });
  }

  /** Changes which stages a part's chain has. */
  setChain(part, chain) {
    return this.control('setChain', { part, chain: chainBits(chain) });
  }

  async textRequest(type, fields) {
    const bytes = await this.request(type, fields);
    return bytes === null ? null : new TextDecoder().decode(bytes);
  }

  /** A part's parameters as JSON text (see `Engine::params_json`), or null if there is no such part. */
  paramsJson(part) {
    return this.textRequest('paramsJson', { part });
  }

  /** A part's parameters, parsed. */
  async params(part) {
    const json = await this.paramsJson(part);
    return json === null ? null : JSON.parse(json);
  }

  /** The drum map as JSON text. */
  drumMapJson() {
    return this.textRequest('drumMapJson');
  }

  /** The value a parameter of a part holds, or NaN. */
  paramValue(part, id) {
    return this.request('paramValue', { part, param: typeof id === 'string' ? paramId(id) : id });
  }

  /** How many voices of a part sound, or -1 if there is no such part. */
  activeVoices(part) {
    return this.request('activeVoices', { part });
  }

  /**
   * The engine's counters (`EngineStats`: queued, ignored, dropped, late,
   * applied, unrouted, displaced, stalled, recovered, limited) and its frame.
   */
  stats() {
    return this.request('stats');
  }

  /** Frees the engine; the node falls silent and can be disconnected. */
  async close() {
    if (this.closed) return;
    this.flushBatch();
    await this.request('close');
    this.release();
  }

  /**
   * Stops talking to the worklet. Every request still waiting for its
   * answer is rejected, as is `ready` if the worklet never said it was.
   */
  release() {
    this.closed = true;
    if (this.backlogTimer !== null) clearTimeout(this.backlogTimer);
    this.backlogTimer = null;
    this.node.port.onmessage = null;
    if (typeof this.node.port.close === 'function') this.node.port.close();
    const closed = new Error('the engine is closed');
    for (const callbacks of this.waiting.values()) callbacks.reject(closed);
    this.waiting.clear();
    // Settling a promise twice does nothing, so this is safe after `ready`.
    this.readyCallbacks.reject(closed);
  }
}
