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
// Instruments. A part plays one of INSTRUMENT: the synth, the drums, the
// samplers, or manguebyte-dsp's plucked strings, FM, organ, 808-style kit,
// kick bass and acid bass. createPart makes a part of a kind, setInstrument
// gives a part another one and keeps its effects, setPreset and setRotary
// move their knobs on the beat. On the two basses a note slides from the
// last when its noteOn comes before the last note's noteOff.
//
// Effects. A method chain such as `.delay(0.25, 0.5).chorus()` maps onto
//
//   await engine.setEffects(part, { delay: [0.25, 0.5], chorus: [] }, time);
//
// or one effect at a time, setEffect(part, 'delay', [0.25, 0.5], time).
// Settings go by name, by position (EFFECT_PARAMS) or as one number for the
// first parameter; they land on the frame of `time` with the effect's
// switch. setMaster sets the master bus (tempo, glue compressor, shared
// reverb, limiter), setSend a part's send to its reverb, and effectCurve
// gives the points an editor draws for an effect (VISUAL names the kind).
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

/** Instrument codes, as `InstrumentKind::code` gives them, by `InstrumentKind::name`. */
export const INSTRUMENT = Object.freeze({
  synth: 0,
  drums: 1,
  sampler: 2,
  sampler_kit: 3,
  plucked: 4,
  fm: 5,
  organ: 6,
  analog_kit: 7,
  kick_bass: 8,
  acid: 9,
});

/**
 * The presets of the instruments that have them, in the order of the
 * values of their `<kind>.preset` parameter: setPreset(part, 'plucked',
 * 'slap_bass') sets `plucked.preset` to 3. A preset moves the knobs it
 * covers to its positions, on the frame it lands on; the README says which.
 */
export const PRESETS = Object.freeze({
  plucked: Object.freeze(['nylon_guitar', 'steel_guitar', 'finger_bass', 'slap_bass']),
  fm: Object.freeze(['electric_piano', 'tubular_bell', 'fm_bass']),
  organ: Object.freeze(['jazz', 'gospel', 'full', 'ballad']),
  acid: Object.freeze(['classic', 'squelch', 'square']),
});

/** The speeds of an organ part's rotating speaker, in the order of the values of `organ.rotary`. */
export const ROTARY = Object.freeze(['stop', 'slow', 'fast']);

/**
 * Effect codes, as `Effect::code` gives them. A part's effect's bit in its
 * chain is `1 << code`; glue and limiter belong to the master bus only.
 */
export const EFFECT = Object.freeze({
  lowpass: 0,
  reverb: 1,
  highpass: 2,
  eq: 3,
  drive: 4,
  bitcrush: 5,
  compressor: 6,
  chorus: 7,
  flanger: 8,
  phaser: 9,
  tremolo: 10,
  autopan: 11,
  delay: 12,
  glue: 13,
  limiter: 14,
});

/**
 * Chain bits, as `Chain::bits` gives them: one per effect a part can have.
 * 'full' is the chain parts are made with, the low-pass and the reverb.
 */
export const CHAIN = Object.freeze({
  none: 0,
  lowpass: 1,
  reverb: 2,
  full: 3,
  highpass: 1 << EFFECT.highpass,
  eq: 1 << EFFECT.eq,
  drive: 1 << EFFECT.drive,
  bitcrush: 1 << EFFECT.bitcrush,
  compressor: 1 << EFFECT.compressor,
  chorus: 1 << EFFECT.chorus,
  flanger: 1 << EFFECT.flanger,
  phaser: 1 << EFFECT.phaser,
  tremolo: 1 << EFFECT.tremolo,
  autopan: 1 << EFFECT.autopan,
  delay: 1 << EFFECT.delay,
  all: (1 << 13) - 1,
});

/** A part's effects in the order its chain runs them (`Effect::PART_ORDER`). */
export const EFFECT_ORDER = Object.freeze([
  'highpass',
  'lowpass',
  'eq',
  'drive',
  'bitcrush',
  'compressor',
  'chorus',
  'flanger',
  'phaser',
  'tremolo',
  'autopan',
  'delay',
  'reverb',
]);

/**
 * The parameters of each effect after its switch (`<effect>.on`), by the
 * name after the dot, in the order positional settings fill them:
 * setEffect(part, 'delay', [0.25, 0.5]) is a time of 0.25 s and a
 * feedback of 0.5. The README lists their units and ranges.
 */
export const EFFECT_PARAMS = Object.freeze({
  highpass: Object.freeze(['cutoff', 'resonance']),
  lowpass: Object.freeze(['cutoff', 'resonance']),
  eq: Object.freeze(['low_gain', 'mid_gain', 'high_gain', 'low_freq', 'mid_freq', 'mid_q', 'high_freq']),
  drive: Object.freeze(['amount', 'output', 'mix', 'shape']),
  bitcrush: Object.freeze(['bits', 'rate', 'mix']),
  compressor: Object.freeze(['threshold', 'ratio', 'attack', 'release', 'knee', 'makeup', 'mix']),
  chorus: Object.freeze(['mix', 'rate', 'depth', 'delay', 'voices', 'spread']),
  flanger: Object.freeze(['mix', 'rate', 'depth', 'feedback', 'delay']),
  phaser: Object.freeze(['mix', 'rate', 'depth', 'center', 'feedback', 'stages', 'spread']),
  tremolo: Object.freeze(['rate', 'depth', 'shape', 'sync']),
  autopan: Object.freeze(['rate', 'depth', 'shape', 'sync']),
  delay: Object.freeze(['time', 'feedback', 'mix', 'damping', 'pingpong', 'sync']),
  reverb: Object.freeze(['mix', 'decay']),
});

/** What an editor draws for each effect (`Visual`), as the parameter JSON's `visual` names it. */
export const VISUAL = Object.freeze({
  lowpass: 'filter_curve',
  highpass: 'filter_curve',
  eq: 'eq_curve',
  drive: 'transfer_curve',
  compressor: 'transfer_curve',
  glue: 'transfer_curve',
  limiter: 'transfer_curve',
  bitcrush: 'bitcrush_steps',
  chorus: 'lfo_shape',
  flanger: 'lfo_shape',
  phaser: 'lfo_shape',
  tremolo: 'lfo_shape',
  autopan: 'lfo_shape',
  delay: 'delay_taps',
  reverb: 'reverb_decay',
});

/** The names a `shape` setting can be given as text, in the order of their values. */
export const SHAPES = Object.freeze({
  drive: Object.freeze(['tanh', 'soft', 'hard', 'fold']),
  tremolo: Object.freeze(['sine', 'triangle', 'square', 'saw_up', 'saw_down']),
  autopan: Object.freeze(['sine', 'triangle', 'square', 'saw_up', 'saw_down']),
});

/** The part number the curve and running exports take for the master bus (`ffi::MASTER`). */
const MASTER = -1;

/** The drum kit's General MIDI notes, by name. */
export const DRUM_NOTES = Object.freeze({
  kick: 36,
  snare: 38,
  closed_hat: 42,
  open_hat: 46,
  crash: 49,
  ride: 51,
  high_tom: 50,
  mid_tom: 47,
  floor_tom: 43,
  side_stick: 37,
  guitar_tap: 88,
});

/**
 * The drum kit's notes by the symbols Aratu writes in a drum line. The
 * guitar tap has none: Aratu plays it from a guitar line.
 */
export const DRUM_SYMBOLS = Object.freeze({ b: 36, s: 38, c: 42, o: 46, x: 49, r: 51, t: 50, m: 47, f: 43, k: 37 });

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
  [-8]: 'the part is not a sampler',
  [-9]: 'no SFZ instrument is being loaded for the part',
  [-10]: 'the SFZ text is not valid',
  [-11]: 'a sample could not be loaded',
  [-12]: 'the input is too large',
  [-13]: 'the part has instruments it has not taken yet',
  [-14]: 'no such effect',
  [-100]: 'a later load of the same part took over',
  [-101]: 'the module has no sampler parts',
  [-102]: 'the part was made again, given another kind or removed before its instrument was measured',
});

const PROCESSOR_NAME = 'manguebyte-engine';

function chainBit(name) {
  if (!Object.hasOwn(CHAIN, name)) throw new RangeError(`unknown chain ${name}`);
  return CHAIN[name];
}

/** Bits from a number, a name of CHAIN, a list of names, or an object of flags by name. */
function chainBits(chain) {
  if (chain === undefined) return CHAIN.full;
  if (typeof chain === 'number') return chain;
  if (typeof chain === 'string') return chainBit(chain);
  if (Array.isArray(chain)) return chain.reduce((bits, name) => bits | chainBit(name), 0);
  return Object.entries(chain).reduce((bits, [name, on]) => (on ? bits | chainBit(name) : bits), 0);
}

/** The bit of a part's effect, or a RangeError. */
function effectBit(name) {
  if (!Object.hasOwn(EFFECT_PARAMS, name)) throw new RangeError(`a part has no effect ${name}`);
  return CHAIN[name];
}

/** A setting's value: a number, a boolean, or a shape's name. */
function settingValue(effect, key, value) {
  if (typeof value === 'boolean') return value ? 1 : 0;
  if (typeof value === 'string' && key === 'shape' && SHAPES[effect]) {
    const index = SHAPES[effect].indexOf(value);
    if (index < 0) throw new RangeError(`${effect} has no shape ${value}: ${SHAPES[effect].join(', ')}`);
    return index;
  }
  if (typeof value !== 'number') throw new TypeError(`${effect}.${key} must be a number, not ${value}`);
  return value;
}

/**
 * The settings of `effect` as [id, value] pairs, from an object by name, an
 * array in EFFECT_PARAMS order, or a number for the first parameter.
 */
function effectSettings(effect, settings) {
  const names = EFFECT_PARAMS[effect];
  let entries;
  if (settings === undefined || settings === null || settings === true) entries = [];
  else if (typeof settings === 'number') entries = [[names[0], settings]];
  else if (Array.isArray(settings)) {
    if (settings.length > names.length) throw new RangeError(`${effect} takes at most ${names.length} settings`);
    entries = settings.map((value, index) => [names[index], value]);
  } else if (typeof settings === 'object') entries = Object.entries(settings);
  else throw new TypeError(`the settings of ${effect} must be an object, an array or a number`);
  return entries
    .filter(([, value]) => value !== undefined)
    .map(([key, value]) => {
      if (key !== 'on' && !names.includes(key)) throw new RangeError(`${effect} has no setting ${key}`);
      return [`${effect}.${key}`, settingValue(effect, key, value)];
    });
}

/** The master bus's settings as [id, value] pairs (see setMaster). */
function masterSettings(settings) {
  const pairs = [];
  const groups = {
    glue: ['on', 'threshold', 'ratio', 'attack', 'release', 'knee', 'makeup'],
    reverb: ['level', 'decay', 'size', 'damping'],
    limiter: ['ceiling', 'release'],
  };
  for (const [key, value] of Object.entries(settings)) {
    if (value === undefined) continue;
    if (key === 'tempo' || key === 'beat' || key === 'gain') {
      pairs.push([`master.${key}`, settingValue('master', key, value)]);
    } else if (Object.hasOwn(groups, key)) {
      const group = typeof value === 'boolean' && key === 'glue' ? { on: value } : value;
      if (typeof group !== 'object' || group === null) throw new TypeError(`master ${key} takes an object`);
      for (const [name, setting] of Object.entries(group)) {
        if (setting === undefined) continue;
        if (!groups[key].includes(name)) throw new RangeError(`the master ${key} has no setting ${name}`);
        pairs.push([`master.${key}.${name}`, settingValue(key, name, setting)]);
      }
    } else {
      throw new RangeError(`the master bus has no setting ${key}`);
    }
  }
  return pairs;
}

function instrumentCode(kind) {
  if (typeof kind === 'number') return kind;
  if (!Object.hasOwn(INSTRUMENT, kind)) throw new RangeError(`unknown instrument ${kind}`);
  return INSTRUMENT[kind];
}

/** The value of `<kind>.preset` for a preset given by name or by value. */
function presetValue(kind, preset) {
  if (!Object.hasOwn(PRESETS, kind)) throw new RangeError(`${kind} has no presets`);
  const names = PRESETS[kind];
  if (typeof preset === 'number') {
    if (!Number.isInteger(preset) || preset < 0 || preset >= names.length) {
      throw new RangeError(`${kind} has presets 0 to ${names.length - 1}`);
    }
    return preset;
  }
  const index = names.indexOf(preset);
  if (index < 0) throw new RangeError(`${kind} has no preset ${preset}: ${names.join(', ')}`);
  return index;
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
    /** The chain bits of each part, as this helper last set or read them. */
    this.chains = new Map();
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

  request(type, fields = {}, transfer = []) {
    if (this.closed) return Promise.reject(new Error('the engine is closed'));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.node.port.postMessage({ type, id, ...fields }, transfer);
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

  /**
   * Cancels what is left of `part` from `time` (default: now): every event
   * of the part scheduled before this call for `time` or later is dropped,
   * and at `time` the notes of the part still sounding fade out over 8 ms
   * (`CANCEL_FADE_SECONDS`). Events scheduled after this call are kept, so
   * the way to replace the rest of an edited bar is cancel(), then the new
   * events. Pick `time` at least latency().engineFrames ahead, so that the
   * cancel lands before the events it is meant to drop start.
   */
  cancel(part, time) {
    return this.schedule({ frame: this.frameFor(time), kind: KIND.cancel, part });
  }

  /**
   * The latency between scheduling and hearing, in frames of the context:
   *
   *   engineFrames  the engine's own, one render quantum and the master
   *                 bus's look-ahead, 111 frames at 48 kHz
   *                 (`Engine::latency_frames`): an event due sooner than
   *                 that after the current time may land late
   *   outputFrames  the context's, from baseLatency and outputLatency
   *                 (0 where the browser does not say)
   *   frames        the two together, and seconds, the same in seconds
   *
   * Records sent by message (without shared memory) also take the time a
   * message takes to cross, which no browser reports.
   */
  async latency() {
    const engineFrames = await this.request('latency');
    const outputSeconds = (this.context.baseLatency ?? 0) + (this.context.outputLatency ?? 0);
    const outputFrames = Math.round(outputSeconds * this.sampleRate);
    const frames = engineFrames + outputFrames;
    return { engineFrames, outputFrames, frames, seconds: frames / this.sampleRate };
  }

  /**
   * Makes part `part` play `kind` (a name of INSTRUMENT, or its code) with
   * `chain` ('full', the low-pass and the reverb; 'none'; bits; a list of
   * effect names; or {lowpass, reverb, delay, ...} flags), as setChain
   * takes it. The kinds:
   *
   *   'synth'        the eight-voice synth
   *   'drums'        the synthesized drum kit of the drum map
   *   'sampler'      an SFZ instrument, silent until loadSfz gives it one
   *   'sampler_kit'  an SFZ drum kit that plays the synthesized drum for a
   *                  note of the drum map it has no sample for, and every
   *                  drum until loadSfz gives it one
   *   'plucked'      guitars and basses (PRESETS.plucked)
   *   'fm'           electric piano, tubular bell, FM bass (PRESETS.fm)
   *   'organ'        a tonewheel organ through a rotating speaker
   *                  (PRESETS.organ, setRotary)
   *   'analog_kit'   the 808-style drum machine on the drum map; its ride
   *                  and guitar tap are the synthesized kit's
   *   'kick_bass'    the 808's long kick as a bass, one voice, sliding
   *   'acid'         the acid bass, one voice, sliding (PRESETS.acid)
   *
   * On the two basses a note slides from the one before when it starts
   * while that one is held: noteOn before the last note's noteOff.
   */
  createPart(part, kind = 'synth', chain = 'full') {
    // A bad kind or chain throws at once, as it always has.
    const code = instrumentCode(kind);
    const bits = chainBits(chain);
    return this.control('createPart', { part, kind: code, chain: bits }).then(() => {
      this.chains.set(part, bits);
    });
  }

  /**
   * Makes part `part` play `kind` (as createPart takes it) and keeps its
   * chain: its effects, their settings, gain, pan and the send. The
   * instrument's own parameters start at their defaults, and the notes of
   * the one it replaces stop. Resolves once the part plays it, so notes
   * scheduled after that, at least latency() ahead, use it. This is where
   * the instrument's memory is made; rendering never allocates.
   */
  setInstrument(part, kind) {
    const code = instrumentCode(kind);
    return this.control('setInstrument', { part, kind: code });
  }

  /**
   * Sets the preset of `part`, which plays `kind` ('plucked', 'fm',
   * 'organ' or 'acid'), at `time`: `preset` is a name of PRESETS[kind] or
   * its value. The knobs the preset covers move to its positions on that
   * frame, so knobs set for the same frame are set after it.
   */
  setPreset(part, kind, preset, time = undefined) {
    return this.setParam(part, `${kind}.preset`, presetValue(kind, preset), time);
  }

  /**
   * Sets the speed of an organ part's rotating speaker at `time`: 'stop',
   * 'slow' (chorale) or 'fast' (tremolo), or 0, 1 or 2. The rotors get
   * there at their own pace, the horn in under a second, the drum in
   * several.
   */
  setRotary(part, speed, time = undefined) {
    const value = typeof speed === 'number' ? speed : ROTARY.indexOf(speed);
    if (!Number.isInteger(value) || value < 0 || value >= ROTARY.length) {
      throw new RangeError(`the rotary speeds are ${ROTARY.join(', ')}`);
    }
    return this.setParam(part, 'organ.rotary', value, time);
  }

  /**
   * Loads an SFZ instrument into sampler part `part` (made with
   * createPart(part, 'sampler') or 'sampler_kit') and resolves once the
   * part plays it: notes scheduled from then on use it, while the notes of
   * the instrument it replaces ring on to their end. Resolves with a summary,
   * `{ regions, samples, sample_bytes, warnings }`, `warnings` being what
   * the parser had to say about opcodes it ignored.
   *
   * The text is fetched from `url`; the worklet parses it and names the
   * sample files, which are fetched relative to `url` and handed over as
   * they arrive; the worklet decodes them a few at a time between render
   * quanta, so loading never interrupts the sound (see worklet.js). Options:
   *
   *   fetch    the function that fetches a URL (default: the global fetch)
   *   base     what a relative `url` is resolved against (default: the
   *            page's address)
   *
   * Rejects, naming the file, if a fetch fails or a file is not valid SFZ,
   * WAV, AIFF or FLAC; the part keeps the instrument it had.
   *
   * The part plays the instrument as soon as its samples are decoded,
   * before its loudness is measured (`sampler.normalize`): until then at a
   * provisional make-up, an estimate, and then at the make-up measured,
   * gliding there over 20 ms. The promise returned has a second one,
   * `normalized`, that resolves with `{ makeUp }` (the make-up measured,
   * dB, or null if the instrument was not measured) once the part plays at
   * that level, some 28 render quanta later, and rejects if the load fails
   * or the part is given another instrument first:
   *
   *   const loading = engine.loadSfz(4, 'piano/voice.sfz');
   *   await loading;              // playable
   *   await loading.normalized;   // at the loudness target
   *
   * Code that awaited loadSfz before works as it did: the instrument plays
   * from then on, only sooner, and for its first few tens of milliseconds
   * at the estimate.
   */
  loadSfz(part, url, options = {}) {
    let settle;
    const normalized = new Promise((resolve, reject) => {
      settle = { resolve, reject };
    });
    // Handled, so that a caller who never looks at it hears of a failure
    // once, from the load itself.
    normalized.catch(() => {});
    const playable = this.loadSfzSteps(part, url, options, settle);
    playable.catch((error) => settle.reject(error));
    playable.normalized = normalized;
    return playable;
  }

  /** The steps of loadSfz, which settles `normalized` once the worklet says. */
  async loadSfzSteps(part, url, options, normalized) {
    if (this.closed) throw new Error('the engine is closed');
    const get = options.fetch ?? globalThis.fetch;
    if (typeof get !== 'function') throw new TypeError('loadSfz needs fetch, or options.fetch');
    const base = new URL(url, options.base ?? globalThis.location?.href);
    const bytesOf = async (address) => {
      const response = await get(address.href);
      if (!response.ok) throw new Error(`loadSfz: could not fetch ${address.href}: ${response.status}`);
      return response.arrayBuffer();
    };
    const failure = (result) => {
      const detail = result.error ? new TextDecoder().decode(result.error) : (STATUS[result.status] ?? `status ${result.status}`);
      return new Error(`loadSfz ${base.href}: ${detail}`);
    };
    const text = await bytesOf(base);
    const token = this.nextId++;
    const begun = await this.request('sfzBegin', { part, token, text }, [text]);
    if (begun.status !== 0) throw failure(begun);
    const paths = JSON.parse(new TextDecoder().decode(begun.files));
    // Not running, the context does not call process(), which works through
    // the files: the worklet decodes them as they come instead.
    const now = () => this.context.state !== 'running';
    try {
      await Promise.all(
        paths.map(async (path) => {
          const bytes = await bytesOf(new URL(path, base));
          const encoded = new TextEncoder().encode(path);
          const fields = { type: 'sfzFile', part, token, path: encoded.buffer, bytes, now: now() };
          if (!this.closed) this.node.port.postMessage(fields, [encoded.buffer, bytes]);
        }),
      );
    } catch (error) {
      if (!this.closed) this.node.port.postMessage({ type: 'sfzAbandon', part, token });
      throw error;
    }
    // The worklet answers a second id once the instrument is measured.
    const measured = this.nextId++;
    this.waiting.set(measured, {
      resolve: (result) => {
        if (result.status === 0) normalized.resolve({ makeUp: result.makeUp ?? null });
        else normalized.reject(failure(result));
      },
      reject: normalized.reject,
    });
    let finished;
    try {
      finished = await this.request('sfzFinish', { part, token, now: now(), normalized: measured });
    } catch (error) {
      this.waiting.delete(measured);
      throw error;
    }
    if (finished.status !== 0) {
      this.waiting.delete(measured);
      throw failure(finished);
    }
    return JSON.parse(new TextDecoder().decode(finished.summary));
  }

  /** Removes a part. */
  removePart(part) {
    return this.control('removePart', { part }).then(() => {
      this.chains.delete(part);
    });
  }

  /**
   * Changes which stages a part's chain has: bits, a name of CHAIN, a list
   * of names or an object of flags (`{ delay: true, chorus: true }`). A
   * stage that joins for the first time gets its memory now; one whose
   * switch is on fades in, one that leaves fades out. To switch effects
   * on the beat, use setEffect, which puts them in the chain beforehand.
   */
  setChain(part, chain) {
    const bits = chainBits(chain);
    return this.control('setChain', { part, chain: bits }).then(() => {
      this.chains.set(part, bits);
    });
  }

  /** The bits of a part's chain (CHAIN), or null if there is no such part. */
  async partChain(part) {
    if (this.chains.has(part)) return this.chains.get(part);
    const bits = await this.request('partChain', { part });
    if (bits < 0) return null;
    this.chains.set(part, bits);
    return bits;
  }

  /**
   * Switches effect `name` of `part` on at AudioContext time `time`
   * (default: now), with `settings` in the effect's own units:
   *
   *   setEffect(part, 'delay', { time: 0.25, feedback: 0.5 }, t)   by name
   *   setEffect(part, 'delay', [0.25, 0.5], t)       in EFFECT_PARAMS order
   *   setEffect(part, 'delay', 0.25, t)              its first parameter
   *   setEffect(part, 'delay', {}, t)                as it is set
   *   setEffect(part, 'delay', false, t)             off: removeEffect
   *
   * which is how a method chain such as `.delay(0.25, 0.5)` maps onto it.
   * A `shape` may be given by name (SHAPES). Every setting lands on the
   * frame of `time`, before the switch, so an effect that was off starts
   * with them and does not glide into them; one already on glides. An
   * effect switched on fades in over 10 ms (`EFFECT_FADE_SECONDS`).
   *
   * The first time a part uses an effect it is added to the part's chain,
   * a control call that makes the effect's memory; the promise resolves
   * once that is done and the records are scheduled, so a `time` at least
   * latency() ahead after that still lands on its frame. Later calls
   * schedule at once.
   */
  async setEffect(part, name, settings = {}, time = undefined) {
    if (settings === false) {
      this.removeEffect(part, name, time);
      return;
    }
    const bit = effectBit(name);
    const pairs = effectSettings(name, settings);
    const chain = await this.partChain(part);
    if (chain === null) throw new Error(`setEffect: ${STATUS[-6]} ${part}`);
    if ((chain & bit) === 0) await this.setChain(part, chain | bit);
    const switched = pairs.find(([id]) => id === `${name}.on`);
    for (const [id, value] of pairs) {
      if (id !== `${name}.on`) this.setParam(part, id, value, time);
    }
    this.setParam(part, `${name}.on`, switched ? switched[1] : 1, time);
  }

  /**
   * Switches effect `name` of `part` off at `time` (default: now). It
   * fades out over 10 ms; the delay and the reverb let their echoes and
   * tails ring on.
   */
  removeEffect(part, name, time = undefined) {
    effectBit(name);
    return this.setParam(part, `${name}.on`, 0, time);
  }

  /**
   * Makes `effects` the effects of `part` from `time`: those it names are
   * switched on with their settings, as setEffect does, and every other
   * effect in the part's chain is switched off, the low-pass and the
   * reverb included. `effects` is an object by name, whose values are
   * settings as setEffect takes them, which is the shape of a method chain
   * once evaluated:
   *
   *   // .delay(0.25).chorus()
   *   await engine.setEffects(part, { delay: [0.25], chorus: [] }, nextBar);
   *
   * The order of the chain does not change: it is EFFECT_ORDER.
   */
  async setEffects(part, effects, time = undefined) {
    for (const name of Object.keys(effects)) effectBit(name);
    const chain = await this.partChain(part);
    if (chain === null) throw new Error(`setEffects: ${STATUS[-6]} ${part}`);
    const wanted = Object.keys(effects).reduce((bits, name) => bits | CHAIN[name], 0);
    if ((chain | wanted) !== chain) await this.setChain(part, chain | wanted);
    for (const name of EFFECT_ORDER) {
      if (Object.hasOwn(effects, name) && effects[name] !== false) {
        await this.setEffect(part, name, effects[name], time);
      } else if ((chain & CHAIN[name]) !== 0) {
        this.removeEffect(part, name, time);
      }
    }
  }

  /** Sets how much of `part` goes to the master bus's shared reverb, 0 to 1, at `time`. */
  setSend(part, amount, time = undefined) {
    return this.setParam(part, 'send', amount, time);
  }

  /**
   * Sets the master bus at `time` (default: now):
   *
   *   setMaster({
   *     tempo: 128,          // beats per minute, what synced effects follow
   *     beat: 0,             // the beat the engine is at on that frame
   *     gain: -3,            // dB into the limiter
   *     glue: { on: true, threshold: -14, ratio: 2, attack: 20, release: 250, knee: 6, makeup: 1 },
   *     reverb: { level: 0, decay: 2.2, size: 0.7, damping: 0.5 },
   *     limiter: { ceiling: -1, release: 150 },
   *   }, time);
   *
   * `glue: true` and `glue: false` switch the glue compressor alone. The
   * master's records name part 0, but belong to no part: no cancel drops
   * them.
   */
  setMaster(settings, time = undefined) {
    const pairs = masterSettings(settings);
    // Switches last, so that what is switched on starts with its settings.
    pairs.sort(([a], [b]) => Number(a.endsWith('.on')) - Number(b.endsWith('.on')));
    for (const [id, value] of pairs) this.setParam(0, id, value, time);
  }

  /** The master bus's parameters as JSON text (see `Engine::master_params_json`). */
  masterParamsJson() {
    return this.textRequest('masterParamsJson');
  }

  /** The master bus's parameters, parsed. */
  async masterParams() {
    return JSON.parse(await this.masterParamsJson());
  }

  /** The value a parameter of the master bus holds (`'master.tempo'`...), or NaN. */
  masterParamValue(id) {
    return this.request('masterParamValue', { param: typeof id === 'string' ? paramId(id) : id });
  }

  /**
   * The curve an editor draws for effect `name` of `part`, or of the
   * master bus with `part` 'master' (glue, limiter, reverb), worked out
   * from the values its parameters hold, with `points` points (2 to 4096;
   * a delay gives one per echo):
   *
   *   { visual, x: Float32Array, y: Float32Array }
   *
   * `visual` is VISUAL[name]; the README gives the axes of each kind.
   * Rejects if there is no such part or effect.
   */
  async effectCurve(part, name, points = 128) {
    if (!Object.hasOwn(EFFECT, name)) throw new RangeError(`unknown effect ${name}`);
    const master = part === 'master';
    const result = await this.request('effectCurve', {
      part: master ? MASTER : part,
      effect: EFFECT[name],
      points,
    });
    if (result.status < 0) throw new Error(`effectCurve ${name}: ${STATUS[result.status] ?? `status ${result.status}`}`);
    const pairs = new Float32Array(result.points);
    const count = pairs.length / 2;
    const x = new Float32Array(count);
    const y = new Float32Array(count);
    for (let i = 0; i < count; i++) {
      x[i] = pairs[2 * i];
      y[i] = pairs[2 * i + 1];
    }
    return { visual: VISUAL[name], x, y };
  }

  /**
   * Whether effect `name` of `part` (or of 'master') is doing work: on,
   * fading, or with echoes or a tail ringing. An editor can light it up.
   */
  async effectRunning(part, name) {
    if (!Object.hasOwn(EFFECT, name)) throw new RangeError(`unknown effect ${name}`);
    const result = await this.request('effectRunning', {
      part: part === 'master' ? MASTER : part,
      effect: EFFECT[name],
    });
    if (result < 0) throw new Error(`effectRunning ${name}: ${STATUS[result] ?? `status ${result}`}`);
    return result === 1;
  }

  /** How far the master limiter is turning the level down, dB, for a meter. */
  limiterReduction() {
    return this.request('limiterReduction');
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
   * applied, unrouted, displaced, stalled, recovered, limited, cancelled)
   * and its frame.
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
