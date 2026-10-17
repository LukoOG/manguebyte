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
// drumMapJson, paramValue, activeVoices, stats, latency, partChain,
// masterParamsJson, masterParamValue, effectCurve, effectRunning,
// limiterReduction; also `records` (an ArrayBuffer of records, for the
// message fallback) and `close`. Messages are handled between render
// quanta, so the curves the effectCurve message works out read the
// parameters as the last quantum left them, never while it renders.
//
// Loading an SFZ instrument (engine.js loadSfz) takes sfzBegin, which
// parses the text at once and answers with the sample files it names, then
// one sfzFile per file and an sfzFinish. Decoding the files is the costly
// part, so the files and the finish wait in a queue, and after each render
// quantum process() works through it until it has decoded
// LOAD_BYTES_PER_QUANTUM bytes of files (always at least one step): the
// audio is never held up by more than about one file's decoding, and
// mb_render itself never allocates. The finish hands the instrument to its
// part at once (mb_sfz_install), at a provisional make-up, and answers:
// the part is playable. Its loudness normalisation then measures a clone
// of it, MEASURE_FRAMES_PER_QUANTUM frames of its phrase a quantum
// (mb_sampler_measure), in quanta with no load step to take, so that other
// instruments become playable first; when it is done the part glides to
// the make-up measured, and a second answer goes to the id the finish
// named as `normalized`. A message marked `now`, sent while the context is
// not running and process() is not being called, works through the whole
// queue, and every measurement, at once instead. Each load carries a
// token; an sfzBegin for the same part makes the steps of an earlier load
// stale, and they are dropped, and a later install, or a part made, given
// another instrument or removed, makes a measurement under way stale. A
// module without mb_sfz_install measures first (mb_sfz_measure) and hands
// the instrument over measured (mb_sfz_finish), answering both at once.

import { EventRing, RECORD_SIZE } from './ring.js';

/** The interface version this file was written for (`ffi::ABI_VERSION`). */
const ABI_VERSION = 2;

/**
 * Bytes of sample files decoded after one render quantum, at most, unless
 * one file is larger. The README gives what that costs.
 */
export const LOAD_BYTES_PER_QUANTUM = 64 * 1024;

/**
 * Frames of an instrument's phrase measured after one render quantum, for
 * the part's loudness normalisation (`PROBE_STEP_FRAMES` in Rust): about
 * what decoding LOAD_BYTES_PER_QUANTUM bytes costs.
 */
export const MEASURE_FRAMES_PER_QUANTUM = 8192;

/** `ffi::status::MEASURING`: a measurement has more to do. */
const MEASURING = 1;

/** Render quanta between two calls that free the instruments parts are done with. */
const COLLECT_EVERY = 512;

// Statuses of the load steps: `ffi::status`, and two of this file's own.
const BAD_INPUT = -12;
/** A later load of the same part took over. */
const STALE = -100;
/** The module predates sampler parts. */
const UNSUPPORTED = -101;
/** The part was made again, given another kind or removed before its instrument was measured. */
const REPLACED = -102;
/** The most frames mb_sampler_measure takes, for a measurement done at once. */
const ALL_FRAMES = 0xffffffff;
/** The module predates the effects (`ffi::status::NO_SUCH_EFFECT`). */
const NO_EFFECTS = -14;
/** The module predates setting a part's instrument (`ffi::status::BAD_INSTRUMENT`). */
const BAD_INSTRUMENT = -3;

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
  'cancelled',
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
    this.jobs = [];
    this.loadTokens = new Map();
    this.loadErrors = new Map();
    // Instruments playing at a provisional make-up: the measurements to
    // step, in order, and by part the token of the one under way.
    this.measures = [];
    this.measuring = new Map();
    this.quanta = 0;
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

  /** Copies `bytes` (an ArrayBuffer or a typed array) into the input area; false if there is no room. */
  input(bytes) {
    const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    const start = this.exports.mb_input_ptr(this.engine, view.length) >>> 0;
    this.refresh();
    if (start === 0) return false;
    this.heap.set(view, start);
    return true;
  }

  /** A copy of the engine's text buffer. */
  textBuffer() {
    return this.text(this.exports.mb_text_ptr(this.engine));
  }

  /** Whether `token` is the load of `part` under way. */
  current(part, token) {
    return this.loadTokens.get(part) === token;
  }

  /**
   * One step of a load: decodes a file, or finishes, handing the
   * instrument to its part to be measured while it plays (or, with a
   * module from before that, measures some of it first). Returns false
   * when a measurement before the hand-over has more to do: the job stays
   * first in the queue.
   */
  step(job) {
    const x = this.exports;
    const { part, token } = job;
    if (!this.current(part, token)) {
      if (job.type === 'finish') {
        this.reply(job.id, { status: STALE, error: null });
        this.reply(job.normalized, { status: STALE });
      }
      return true;
    }
    if (job.type === 'file') {
      if (this.loadErrors.has(part)) return true;
      const path = new Uint8Array(job.path);
      const file = new Uint8Array(job.bytes);
      const both = new Uint8Array(path.length + file.length);
      both.set(path, 0);
      both.set(file, path.length);
      const status = this.input(both) ? x.mb_sfz_add_file(this.engine, part, path.length, file.length) : BAD_INPUT;
      if (status !== 0) this.loadErrors.set(part, { status, error: this.textBuffer() });
      return true;
    }
    const failed = this.loadErrors.get(part);
    if (failed) {
      this.loadTokens.delete(part);
      this.loadErrors.delete(part);
      x.mb_sfz_abandon(this.engine, part);
      this.reply(job.id, failed);
      this.reply(job.normalized, { status: failed.status });
      return true;
    }
    if (typeof x.mb_sfz_install === 'function') {
      this.loadTokens.delete(part);
      const status = x.mb_sfz_install(this.engine, part);
      const text = this.textBuffer();
      if (status !== 0) {
        this.reply(job.id, { status, error: text });
        this.reply(job.normalized, { status });
        return true;
      }
      this.reply(job.id, { status, summary: text });
      this.measuring.set(part, token);
      this.measures.push({ part, token, id: job.normalized });
      return true;
    }
    // A module from before loudness normalisation finishes in one step.
    if (typeof x.mb_sfz_measure === 'function') {
      const measured = x.mb_sfz_measure(this.engine, part, MEASURE_FRAMES_PER_QUANTUM);
      if (measured === MEASURING) {
        this.jobs.unshift(job);
        return false;
      }
      if (measured !== 0) {
        this.loadTokens.delete(part);
        this.reply(job.id, { status: measured, error: this.textBuffer() });
        this.reply(job.normalized, { status: measured });
        return true;
      }
    }
    this.loadTokens.delete(part);
    const status = x.mb_sfz_finish(this.engine, part);
    const text = this.textBuffer();
    this.reply(job.id, status === 0 ? { status, summary: text } : { status, error: text });
    this.reply(job.normalized, status === 0 ? { status, makeUp: this.makeUp(part) } : { status });
    return true;
  }

  /** The make-up the part's instrument was measured at, in dB, or null. */
  makeUp(part) {
    const x = this.exports;
    if (typeof x.mb_sampler_make_up_db !== 'function') return null;
    const makeUp = x.mb_sampler_make_up_db(this.engine, part);
    return Number.isNaN(makeUp) ? null : makeUp;
  }

  /**
   * One step of the first measurement under way, of `frames` frames:
   * answers its `normalized` id once it is over, or once the part plays
   * something else.
   */
  measureStep(frames) {
    const x = this.exports;
    const job = this.measures[0];
    const current = this.measuring.get(job.part);
    if (current !== job.token) {
      this.measures.shift();
      this.reply(job.id, { status: current === undefined ? REPLACED : STALE });
      return;
    }
    const status = x.mb_sampler_measure(this.engine, job.part, frames);
    if (status === MEASURING) return;
    this.measures.shift();
    this.measuring.delete(job.part);
    this.reply(job.id, status === 0 ? { status, makeUp: this.makeUp(job.part) } : { status });
  }

  /**
   * Works through the queued load steps, until `budget` bytes of files are
   * decoded (always one step). A finish, which makes the instrument, its
   * clone and the estimate of its make-up, counts as the whole budget, and
   * comes first in its quantum, as does a step of a measurement. With no
   * budget, every measurement is done too.
   */
  work(budget) {
    let used = 0;
    let steps = 0;
    while (this.jobs.length > 0 && (steps === 0 || used < budget)) {
      const job = this.jobs[0];
      if (job.type === 'finish' && steps > 0 && budget !== Infinity) break;
      this.jobs.shift();
      if (job.type === 'file') used += job.bytes.byteLength;
      this.step(job);
      if (job.type === 'finish') used += LOAD_BYTES_PER_QUANTUM;
      steps += 1;
    }
    if (budget === Infinity) {
      while (this.measures.length > 0) this.measureStep(ALL_FRAMES);
    }
    this.refresh();
  }

  reply(id, result) {
    if (id !== undefined) this.port.postMessage({ type: 'reply', id, result });
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
      case 'sfzBegin': {
        if (typeof x.mb_sfz_begin !== 'function') {
          result = { status: UNSUPPORTED, error: null };
          break;
        }
        this.loadTokens.set(message.part, message.token);
        this.loadErrors.delete(message.part);
        const status = this.input(message.text)
          ? x.mb_sfz_begin(engine, message.part, message.text.byteLength)
          : BAD_INPUT;
        if (status === 0) {
          result = { status, files: this.text(x.mb_sfz_files_json(engine, message.part)) };
        } else {
          this.loadTokens.delete(message.part);
          // Only the load's own failures write a message.
          result = { status, error: status <= -10 ? this.textBuffer() : null };
        }
        break;
      }
      case 'sfzFile':
      case 'sfzFinish':
        this.jobs.push({ ...message, type: message.type === 'sfzFile' ? 'file' : 'finish' });
        if (message.now) this.work(Infinity);
        return;
      case 'sfzAbandon':
        if (this.current(message.part, message.token)) {
          this.loadTokens.delete(message.part);
          this.loadErrors.delete(message.part);
          x.mb_sfz_abandon(engine, message.part);
        }
        return;
      case 'close':
        this.closed = true;
        x.mb_engine_free(engine);
        result = 0;
        break;
      // Each of the next three drops a measurement under way on the part.
      case 'createPart':
        result = x.mb_create_part(engine, message.part, message.kind, message.chain);
        if (result === 0) this.measuring.delete(message.part);
        break;
      case 'setInstrument':
        // A module from before the export cannot change a part's
        // instrument: it answers as for an instrument it does not know.
        result = typeof x.mb_set_instrument === 'function'
          ? x.mb_set_instrument(engine, message.part, message.kind)
          : BAD_INSTRUMENT;
        if (result === 0) this.measuring.delete(message.part);
        break;
      case 'removePart':
        result = x.mb_remove_part(engine, message.part);
        if (result === 0) this.measuring.delete(message.part);
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
      case 'latency':
        // A module without the export: the quantum this processor renders.
        result = typeof x.mb_latency_frames === 'function' ? x.mb_latency_frames(engine) : this.maxQuantum;
        break;
      // The effects and the master bus. A module that predates them has
      // none of these exports, and answers as an engine without the part
      // or the effect would.
      case 'partChain':
        result = typeof x.mb_part_chain === 'function' ? x.mb_part_chain(engine, message.part) : NO_EFFECTS;
        break;
      case 'masterParamsJson':
        result = typeof x.mb_master_params_json === 'function' ? this.text(x.mb_master_params_json(engine)) : null;
        break;
      case 'masterParamValue':
        result = typeof x.mb_master_param_value === 'function' ? x.mb_master_param_value(engine, message.param) : NaN;
        break;
      case 'effectCurve': {
        if (typeof x.mb_effect_curve !== 'function') {
          result = { status: NO_EFFECTS, points: null };
          break;
        }
        const count = x.mb_effect_curve(engine, message.part, message.effect, message.points);
        if (count < 0) {
          result = { status: count, points: null };
          break;
        }
        this.refresh();
        const start = x.mb_curve_ptr(engine) >>> 0;
        // A copy, its buffer transferred with the reply.
        result = { status: 0, points: this.heap.slice(start, start + count * 8).buffer };
        break;
      }
      case 'effectRunning':
        result = typeof x.mb_effect_running === 'function'
          ? x.mb_effect_running(engine, message.part, message.effect)
          : NO_EFFECTS;
        break;
      case 'limiterReduction':
        result = typeof x.mb_limiter_reduction_db === 'function' ? x.mb_limiter_reduction_db(engine) : 0;
        break;
      default:
        result = undefined;
    }
    if (!this.closed) this.refresh();
    this.reply(message.id, result);
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
    // Instruments become playable first; measurements fill the quanta with
    // nothing to load.
    if (this.jobs.length > 0) this.work(LOAD_BYTES_PER_QUANTUM);
    else if (this.measures.length > 0) {
      this.measureStep(MEASURE_FRAMES_PER_QUANTUM);
      this.refresh();
    }
    this.quanta += 1;
    if (this.quanta % COLLECT_EVERY === 0 && typeof x.mb_collect === 'function') {
      x.mb_collect(this.engine);
      this.refresh();
    }
    return true;
  }
}

registerProcessor(PROCESSOR_NAME, ManguebyteProcessor);
