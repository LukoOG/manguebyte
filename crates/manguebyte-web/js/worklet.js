// The AudioWorkletProcessor of manguebyte-web.
//
// It instantiates the WebAssembly module built from the crate
// (`cargo build -p manguebyte-web --target wasm32-unknown-unknown`), which
// imports nothing, and renders through its exports (see the Rust module
// `manguebyte_web::ffi`). Load it with `audioWorklet.addModule` and make the
// node with the helper in engine.js, which passes these processorOptions:
//
//   wasm             the module: a WebAssembly.Module, or its bytes
//   ring             a SharedArrayBuffer holding an event ring (ring.js),
//                    or null to receive records by message instead
//   maxQuantum       most frames per render call (default 512)
//   maxParts         parts that can exist at once (default 32)
//   pendingCapacity  events that can wait for their frame (default 4096)
//   stagingRecords   records copied into the engine per push (default 256)
//
// Every render quantum it moves records from the ring (or from the
// messages received) into the engine, then renders. When the engine's queue
// of waiting events is full, a record due before the latest waiting event
// still goes in and pushes that event out; the first record due no earlier
// than every waiting event stops the move, and it and the records behind it
// stay in the ring (or in the messages) for a later quantum
// (`mb_admit_staged`). The engine's frame 0 is the first frame this processor renders;
// it tells the main thread which context frame that is with an `origin`
// message, and again if the browser ever skips calling process().
//
// Messages from the main thread (each with an `id`, answered by a `reply`
// carrying the same id): createPart, removePart, setChain, paramsJson,
// drumMapJson, paramValue, activeVoices, stats; also `records` (an
// ArrayBuffer of records, for the message fallback) and `close`.

import { EventRing, RECORD_SIZE } from './ring.js';

/** The interface version this file was written for (`ffi::ABI_VERSION`). */
const ABI_VERSION = 2;

/** The name the processor is registered under. */
export const PROCESSOR_NAME = 'manguebyte-engine';

/** The counters of `mb_stat`, in the order of their indices. */
const STAT_NAMES = [
  'queued',
  'ignored',
  'dropped',
  'late',
  'applied',
  'unrouted',
  'displaced',
  'stalled',
  'recovered',
  'limited',
];

export class ManguebyteProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const o = (options && options.processorOptions) || {};
    this.maxQuantum = o.maxQuantum ?? 512;
    this.closed = false;
    this.frame = 0;
    this.origin = null;
    this.messages = [];
    this.messageOffset = 0;
    try {
      const module = o.wasm instanceof WebAssembly.Module ? o.wasm : new WebAssembly.Module(o.wasm);
      this.exports = new WebAssembly.Instance(module, {}).exports;
      const x = this.exports;
      if (x.mb_abi_version() !== ABI_VERSION) {
        throw new Error(`the module has interface ${x.mb_abi_version()}, this worklet ${ABI_VERSION}`);
      }
      const [lowest, highest] = [x.mb_min_sample_rate(), x.mb_max_sample_rate()];
      if (!(sampleRate >= lowest && sampleRate <= highest)) {
        throw new Error(`the sample rate ${sampleRate} Hz is out of range: the engine runs at ${lowest} to ${highest} Hz`);
      }
      this.engine = x.mb_engine_new(
        sampleRate,
        this.maxQuantum,
        o.maxParts ?? 32,
        o.pendingCapacity ?? 4096,
        o.stagingRecords ?? 256,
      );
      if (this.engine === 0) throw new Error('the engine settings are out of range');
      this.ring = o.ring ? new EventRing(o.ring) : null;
      this.refresh();
    } catch (error) {
      this.closed = true;
      this.port.postMessage({ type: 'error', message: String(error && error.message ? error.message : error) });
      return;
    }
    this.port.onmessage = (event) => this.receive(event.data);
    this.port.postMessage({ type: 'ready', mode: this.ring ? 'shared' : 'message' });
  }

  /** Makes the views of WebAssembly memory again if the memory has grown. */
  refresh() {
    const x = this.exports;
    if (this.memory === x.memory.buffer) return;
    this.memory = x.memory.buffer;
    this.heap = new Uint8Array(this.memory);
    this.staging = x.mb_staging_ptr(this.engine) >>> 0;
    this.stagingCapacity = x.mb_staging_capacity(this.engine);
    this.left = new Float32Array(this.memory, x.mb_output_ptr(this.engine, 0) >>> 0, this.maxQuantum);
    this.right = new Float32Array(this.memory, x.mb_output_ptr(this.engine, 1) >>> 0, this.maxQuantum);
  }

  /**
   * A copy of the UTF-8 text a text export wrote, or null. It is decoded on
   * the main thread, because some browsers give worklets no TextDecoder.
   */
  text(start) {
    if (start === 0) return null;
    this.refresh();
    const length = this.exports.mb_text_len(this.engine);
    return this.heap.slice(start >>> 0, (start >>> 0) + length);
  }

  receive(message) {
    if (this.closed || !message) return;
    const x = this.exports;
    const engine = this.engine;
    let result;
    switch (message.type) {
      case 'records':
        this.messages.push(new Uint8Array(message.bytes));
        return;
      case 'close':
        this.closed = true;
        x.mb_engine_free(engine);
        result = 0;
        break;
      case 'createPart':
        result = x.mb_create_part(engine, message.part, message.kind, message.chain);
        break;
      case 'removePart':
        result = x.mb_remove_part(engine, message.part);
        break;
      case 'setChain':
        result = x.mb_set_chain(engine, message.part, message.chain);
        break;
      case 'paramsJson':
        result = this.text(x.mb_params_json(engine, message.part));
        break;
      case 'drumMapJson':
        result = this.text(x.mb_drum_map_json(engine));
        break;
      case 'paramValue':
        result = x.mb_param_value(engine, message.part, message.param);
        break;
      case 'activeVoices':
        result = x.mb_active_voices(engine, message.part);
        break;
      case 'stats':
        result = {};
        STAT_NAMES.forEach((name, index) => {
          result[name] = x.mb_stat(engine, index);
        });
        result.frame = x.mb_frame(engine);
        break;
      default:
        result = undefined;
    }
    if (!this.closed) this.refresh();
    if (message.id !== undefined) this.port.postMessage({ type: 'reply', id: message.id, result });
  }

  /**
   * Copies up to `max` records that arrived by message into the staging
   * area, leaving them in the messages. Returns how many were copied.
   */
  peekMessages(max) {
    let copied = 0;
    let offset = this.messageOffset;
    for (let m = 0; m < this.messages.length && copied < max; m++) {
      const bytes = this.messages[m];
      const whole = Math.floor(bytes.length / RECORD_SIZE);
      const n = Math.min(whole - offset, max - copied);
      const from = offset * RECORD_SIZE;
      const to = this.staging + copied * RECORD_SIZE;
      for (let b = 0; b < n * RECORD_SIZE; b++) this.heap[to + b] = bytes[from + b];
      copied += n;
      offset = 0;
    }
    return copied;
  }

  /** Forgets the `count` oldest records that arrived by message. */
  discardMessages(count) {
    let left = count;
    while (left > 0 && this.messages.length > 0) {
      const whole = Math.floor(this.messages[0].length / RECORD_SIZE);
      const n = Math.min(whole - this.messageOffset, left);
      left -= n;
      this.messageOffset += n;
      if (this.messageOffset >= whole) {
        this.messages.shift();
        this.messageOffset = 0;
      }
    }
  }

  /**
   * Moves records into the engine until there are none left or the engine
   * takes no more (see the top of this file).
   */
  drain() {
    const x = this.exports;
    for (;;) {
      const n = this.ring
        ? this.ring.peekInto(this.heap, this.staging, this.stagingCapacity)
        : this.peekMessages(this.stagingCapacity);
      if (n === 0) return;
      const taken = x.mb_admit_staged(this.engine, n);
      if (this.ring) this.ring.discard(taken);
      else this.discardMessages(taken);
      if (taken < n) return;
    }
  }

  process(inputs, outputs) {
    if (this.closed) return false;
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const frames = output[0].length;
    const origin = currentFrame - this.frame;
    if (origin !== this.origin) {
      this.origin = origin;
      this.port.postMessage({ type: 'origin', origin });
    }
    const x = this.exports;
    this.refresh();
    this.drain();
    const left = output[0];
    const right = output.length > 1 ? output[1] : null;
    let done = 0;
    while (done < frames) {
      const n = x.mb_render(this.engine, Math.min(frames - done, this.maxQuantum));
      if (n === 0) break;
      for (let i = 0; i < n; i++) left[done + i] = this.left[i];
      if (right) for (let i = 0; i < n; i++) right[done + i] = this.right[i];
      done += n;
    }
    this.frame += done;
    return true;
  }
}

registerProcessor(PROCESSOR_NAME, ManguebyteProcessor);
