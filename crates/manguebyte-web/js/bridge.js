// The Aratu bridge's client: plays through the Aratu Bridge plugin in a
// DAW instead of the engine in this page.
//
//   import { AratuBridge } from './bridge.js';
//
//   const engine = await AratuBridge.connect({ port: 47870 });
//   await engine.createPart(0, 'drums');
//   engine.noteOn(0, 36, 1.0, engine.context.currentTime + 0.1);
//
// AratuBridge extends ManguebyteEngine (engine.js) and keeps its methods:
// createPart, setInstrument, removePart, setChain, setEffect(s), setMaster,
// setSend, setPreset, setRotary, noteOn, noteOff, setParam, allNotesOff,
// choke, cancel, loadSfz, sync, latency, stats, params, masterParams,
// drumMapJson, partChain, close. So code written for the local engine
// switches to the DAW with the one call that makes the engine. What the
// plugin cannot answer over the network (paramValue, activeVoices,
// effectCurve, effectRunning, partScope, partLevels) rejects with an
// error that says so. docs/ARATU_BRIDGE.md has the protocol.
//
// Time. The DAW's transport is the clock. The plugin reports where the
// DAW is (`transport`: playing, tempo, beat, time signature, loop, epoch)
// every 10 ms while it plays, and events are scheduled in beats on the
// DAW's timeline, landing on the frame their beat falls on. Two ways to
// give a time, chosen with `timeBase`:
//
//   'seconds' (default)  as with the local engine, a time in seconds on
//                        `engine.context.currentTime`, a clock of this
//                        page; it is turned into a beat with the DAW's
//                        latest position and tempo. While the DAW is
//                        stopped, events are held and played as soon as
//                        possible when their time comes, so that live
//                        coding is heard without pressing play.
//   'beats'              a beat (quarter notes) on the DAW's timeline;
//                        read `transport` and `currentBeat` to know
//                        where it is. While the DAW is stopped, events
//                        wait in the plugin for their beat.
//
// An undefined time means "as soon as possible" in both. A loop or a jump
// of the DAW moves the epoch on (by 1 for a loop going round, by 2 for a
// jump): the plugin drops what was sent for an older epoch. This client
// sends an event due past the loop's end for the next lap, so that loops
// go round seamlessly, and in 'seconds' mode sends again, mapped anew,
// what is still to come after a jump. In 'beats' mode the page plans again
// from the transport after a jump (`ontransport` says `jumped`).

import { ManguebyteEngine, STATUS } from './engine.js';
import { RECORD_SIZE, encodeRecord, paramId } from './ring.js';

/** The version of the bridge protocol (`protocol::PROTOCOL_VERSION`). */
export const PROTOCOL_VERSION = 1;
/** The engine interface it carries (`ffi::ABI_VERSION`). */
export const ABI_VERSION = 2;
/** The WebSocket subprotocol. */
export const SUBPROTOCOL = 'aratu-bridge.1';
/** The port the plugin listens on unless told otherwise. */
export const DEFAULT_PORT = 47870;
/** The size of a message's header. */
export const HEADER_SIZE = 8;

/** Message types (`protocol::page` and `protocol::plugin`). */
export const MESSAGE = Object.freeze({
  hello: 0x01,
  events: 0x02,
  createPart: 0x10,
  setInstrument: 0x11,
  removePart: 0x12,
  setChain: 0x13,
  sfzBegin: 0x20,
  sfzFile: 0x21,
  sfzFinish: 0x22,
  sfzAbandon: 0x23,
  query: 0x30,
  sync: 0x31,
  panic: 0x32,
  welcome: 0x81,
  transport: 0x82,
  reply: 0x83,
  status: 0x84,
  notice: 0x85,
});

/** What a query asks for (`protocol::query`). */
export const QUERY = Object.freeze({
  drumMapJson: 1,
  paramsJson: 2,
  masterParamsJson: 3,
  partChain: 4,
  stats: 5,
  latency: 6,
});

/** Notice codes (`protocol::notice`). */
export const NOTICE = Object.freeze({ engineReset: 1, overflow: 2, loadRefused: 3 });

/** The bridge's own statuses, beside those of STATUS. */
export const BRIDGE_STATUS = Object.freeze({
  [-200]: 'the plugin does not understand the message',
  [-201]: 'not available over the bridge',
  [-202]: 'the plugin is busy',
  [-203]: 'no session: say hello first, or the host has not activated the plugin',
  [-204]: 'a later load took over, or the part changed',
});

const STAT_NAMES = [
  'queued', 'ignored', 'dropped', 'late', 'applied', 'unrouted', 'displaced', 'stalled', 'recovered',
  'limited', 'cancelled',
];
const BRIDGE_STAT_NAMES = ['bridgeLate', 'tooLate', 'stale', 'cleared', 'overflow', 'immediate', 'overridden'];

function statusText(status) {
  return STATUS[status] ?? BRIDGE_STATUS[status] ?? `status ${status}`;
}

/** A message: the header and `body`. */
export function message(kind, request, body = new Uint8Array(0)) {
  const bytes = new Uint8Array(HEADER_SIZE + body.byteLength);
  const view = new DataView(bytes.buffer);
  view.setUint8(0, kind);
  view.setUint32(4, request >>> 0, true);
  bytes.set(body instanceof Uint8Array ? body : new Uint8Array(body), HEADER_SIZE);
  return bytes;
}

/** Writes `record` at `offset` of `view` with `beat` (NaN: now) for a frame. */
export function encodeBeatRecord(view, offset, beat, record) {
  encodeRecord(view, offset, { ...record, frame: 0 });
  view.setFloat64(offset, beat, true);
}

/** The body of an events message: the epoch, then the records. */
export function eventsBody(epoch, entries) {
  const body = new Uint8Array(8 + RECORD_SIZE * entries.length);
  const view = new DataView(body.buffer);
  view.setUint32(0, epoch >>> 0, true);
  entries.forEach(({ beat, record }, index) => encodeBeatRecord(view, 8 + RECORD_SIZE * index, beat, record));
  return body;
}

/** Reads a transport message's body (`protocol::Transport`). */
export function decodeTransport(view, offset = 0) {
  const flags = view.getUint8(offset + 64);
  const f = (at) => view.getFloat64(offset + at, true);
  return {
    beat: f(0),
    tempo: f(8),
    barStart: f(16),
    loopStart: f(24),
    loopEnd: f(32),
    age: f(40),
    block: Number(view.getBigUint64(offset + 48, true)),
    epoch: view.getUint32(offset + 56, true),
    timeSignature: [view.getUint16(offset + 60, true), view.getUint16(offset + 62, true)],
    playing: (flags & 1) !== 0,
    recording: (flags & 2) !== 0,
    looping: (flags & 4) !== 0,
    fromHost: (flags & 8) !== 0,
  };
}

function utf8(text) {
  return new TextEncoder().encode(text);
}

function concat(...parts) {
  const bytes = new Uint8Array(parts.reduce((sum, part) => sum + part.byteLength, 0));
  let at = 0;
  for (const part of parts) {
    bytes.set(part instanceof Uint8Array ? part : new Uint8Array(part), at);
    at += part.byteLength;
  }
  return bytes;
}

function u16(value) {
  const bytes = new Uint8Array(2);
  new DataView(bytes.buffer).setUint16(0, value, true);
  return bytes;
}

function u32(value) {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value >>> 0, true);
  return bytes;
}

const UNSUPPORTED = new Set([
  'paramValue', 'activeVoices', 'effectCurve', 'effectRunning', 'partScope', 'partLevels', 'masterParamValue',
]);

export class AratuBridge extends ManguebyteEngine {
  /**
   * Connects to the plugin and says hello. Resolves once the plugin
   * answers, rejects if it refuses or cannot be reached. Options:
   *
   *   port       the plugin's port (default DEFAULT_PORT)
   *   url        the whole address instead (default ws://127.0.0.1:port)
   *   timeBase   'seconds' (default) or 'beats', see above
   *   name       the page's name, sent in the hello
   *   WebSocket  the WebSocket class (default: the global one)
   *   clock      a function giving seconds, for 'seconds' mode
   *              (default: performance.now() / 1000)
   */
  static async connect(options = {}) {
    const url = options.url ?? `ws://127.0.0.1:${options.port ?? DEFAULT_PORT}`;
    const Socket = options.WebSocket ?? globalThis.WebSocket;
    if (typeof Socket !== 'function') throw new TypeError('AratuBridge.connect needs a WebSocket');
    const socket = new Socket(url, SUBPROTOCOL);
    const bridge = new AratuBridge(socket, options);
    try {
      await bridge.ready;
    } catch (error) {
      bridge.release();
      throw error;
    }
    return bridge;
  }

  /** Use connect(). */
  constructor(socket, options = {}) {
    const clock = options.clock ?? (() => globalThis.performance.now() / 1000);
    const context = {
      sampleRate: 48000,
      get currentTime() {
        return clock();
      },
      state: 'running',
      baseLatency: 0,
      outputLatency: 0,
    };
    super(context, { port: { postMessage() {}, close() {} } }, null, null);
    this.socket = socket;
    this.clock = clock;
    this.timeBase = options.timeBase ?? 'seconds';
    if (this.timeBase !== 'seconds' && this.timeBase !== 'beats') {
      throw new RangeError(`timeBase is 'seconds' or 'beats', not ${this.timeBase}`);
    }
    this.name = options.name ?? 'aratu';
    /** 'connecting', 'open' (after the hello), 'closed' or 'error'. */
    this.state = 'connecting';
    /** The plugin's hello: sampleRate, latency, maxBlock, maxParts, version... */
    this.welcome = null;
    /** The DAW's transport, as the plugin last reported it. */
    this.transport = null;
    /** When it arrived, on `clock`. */
    this.transportTime = 0;
    /** The plugin's counters, as it last reported them. */
    this.lastStatus = null;
    /** Called with the state when it changes. */
    this.onstate = null;
    /** Called with the transport when it arrives, `jumped` and `wrapped` added. */
    this.ontransport = null;
    /** Called with { code, text } when the plugin has something to say. */
    this.onnotice = null;
    this.epoch = 0;
    /** Events still to come, kept to send again after a jump or a wrap. */
    this.entries = [];
    this.outbox = new Map();
    this.flushQueued = false;
    this.holdTimer = null;
    socket.binaryType = 'arraybuffer';
    socket.onopen = () => this.sendMessage(MESSAGE.hello, this.nextId++, concat(u32(PROTOCOL_VERSION), u32(ABI_VERSION), utf8(this.name)));
    socket.onmessage = (event) => this.receiveBytes(event.data);
    socket.onerror = () => this.fail(new Error('the bridge could not reach the plugin'));
    socket.onclose = (event) => this.closedBy(event);
  }

  /** 'bridge'. */
  get mode() {
    return 'bridge';
  }

  setState(state) {
    if (this.state === state) return;
    this.state = state;
    if (typeof this.onstate === 'function') this.onstate(state);
  }

  fail(error) {
    if (this.state === 'closed') return;
    this.setState('error');
    this.readyCallbacks.reject(error);
  }

  closedBy(event) {
    const wasOpen = this.state === 'open';
    if (!wasOpen && this.state !== 'error') {
      const reason = event && event.reason ? `: ${event.reason}` : '';
      this.readyCallbacks.reject(new Error(`the plugin closed the connection (${event?.code ?? '?'})${reason}`));
    }
    this.setState('closed');
    super.release();
  }

  sendMessage(kind, request, body) {
    if (this.socket.readyState !== undefined && this.socket.readyState !== 1) {
      throw new Error('the bridge is not connected');
    }
    this.socket.send(message(kind, request, body));
  }

  /** Sends a request and resolves with { status, body }. */
  ask(kind, body) {
    if (this.closed) return Promise.reject(new Error('the engine is closed'));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      try {
        this.flushBatch();
        this.sendMessage(kind, id, body);
      } catch (error) {
        this.waiting.delete(id);
        reject(error);
      }
    });
  }

  receiveBytes(data) {
    const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    if (bytes.byteLength < HEADER_SIZE) return;
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const kind = view.getUint8(0);
    const request = view.getUint32(4, true);
    const body = new DataView(bytes.buffer, bytes.byteOffset + HEADER_SIZE, bytes.byteLength - HEADER_SIZE);
    switch (kind) {
      case MESSAGE.welcome: {
        this.welcome = {
          protocol: body.getUint32(0, true),
          abi: body.getUint32(4, true),
          sampleRate: body.getFloat64(8, true),
          latency: body.getUint32(16, true),
          maxBlock: body.getUint32(20, true),
          maxParts: body.getUint32(24, true),
          maxMessage: body.getUint32(28, true),
          epoch: body.getUint32(32, true),
          version: new TextDecoder().decode(bytes.subarray(HEADER_SIZE + 36)),
        };
        this.sampleRate = this.welcome.sampleRate;
        this.context.sampleRate = this.welcome.sampleRate;
        this.epoch = this.welcome.epoch;
        this.setState('open');
        this.readyCallbacks.resolve();
        break;
      }
      case MESSAGE.reply: {
        const status = body.getInt32(0, true);
        const payload = bytes.slice(HEADER_SIZE + 4);
        const callbacks = this.waiting.get(request);
        if (callbacks) {
          this.waiting.delete(request);
          callbacks.resolve({ status, body: payload });
        } else if (this.state === 'connecting') {
          this.fail(new Error(`the plugin refused the hello: ${new TextDecoder().decode(payload)}`));
        }
        break;
      }
      case MESSAGE.transport:
        this.receiveTransport(decodeTransport(body));
        break;
      case MESSAGE.status: {
        const read = (index) => Number(body.getBigUint64(index * 8, true));
        const status = {};
        STAT_NAMES.forEach((name, index) => (status[name] = read(index)));
        BRIDGE_STAT_NAMES.forEach((name, index) => (status[name] = read(11 + index)));
        status.limiterDb = body.getFloat64(18 * 8, true);
        this.lastStatus = status;
        break;
      }
      case MESSAGE.notice: {
        const code = body.getUint32(0, true);
        const text = new TextDecoder().decode(bytes.subarray(HEADER_SIZE + 4));
        if (code === NOTICE.engineReset) this.chains.clear();
        if (typeof this.onnotice === 'function') this.onnotice({ code, text });
        break;
      }
      default:
    }
  }

  receiveTransport(transport) {
    const previous = this.epoch;
    this.transport = transport;
    this.transportTime = this.clock() - transport.age;
    const change = (transport.epoch - previous) >>> 0;
    const jumped = change >= 2;
    const wrapped = change === 1;
    this.epoch = transport.epoch;
    if (change !== 0) this.replan(previous, wrapped);
    this.placeHeld();
    this.prune();
    if (typeof this.ontransport === 'function') this.ontransport({ ...transport, jumped, wrapped });
  }

  /** Where the DAW is now, in beats, by its last report and its tempo. */
  get currentBeat() {
    const t = this.transport;
    if (!t) return 0;
    if (!t.playing) return t.beat;
    return t.beat + ((this.clock() - this.transportTime) * t.tempo) / 60;
  }

  /** The beat a time on `context.currentTime` falls on, at the DAW's tempo. */
  beatAt(time) {
    const t = this.transport;
    if (!t) return null;
    return t.beat + ((time - this.transportTime) * t.tempo) / 60;
  }

  /** The time on `context.currentTime` the DAW reaches `beat`, at its tempo, if it plays. */
  timeAtBeat(beat) {
    const t = this.transport;
    if (!t || !t.playing) return null;
    return this.transportTime + ((beat - t.beat) * 60) / t.tempo;
  }

  /** The time, or NaN for "as soon as possible". */
  frameFor(time) {
    return time === undefined ? NaN : time;
  }

  /** The beat and the epoch an entry goes to now, or null if it must wait. */
  place(entry) {
    let beat;
    if (Number.isNaN(entry.time)) return { beat: NaN, epoch: this.epoch };
    if (this.timeBase === 'beats') {
      beat = entry.time;
    } else {
      const t = this.transport;
      if (!t || !t.playing) return null;
      beat = this.beatAt(entry.time);
    }
    const t = this.transport;
    if (t && t.looping && t.playing && beat >= t.loopEnd) {
      const length = t.loopEnd - t.loopStart;
      if (beat >= t.loopEnd + length) return null;
      return { beat: t.loopStart + (beat - t.loopEnd), epoch: (this.epoch + 1) >>> 0 };
    }
    return { beat, epoch: this.epoch };
  }

  /** Schedules a record whose `frame` is a time (see frameFor). */
  schedule(record) {
    if (this.closed) throw new Error('the engine is closed');
    const entry = { time: record.frame, record: { ...record, frame: 0 }, sent: null };
    if (!Number.isNaN(entry.time)) this.entries.push(entry);
    this.send(entry);
    return true;
  }

  send(entry) {
    const place = this.place(entry);
    if (place === null) {
      this.armHold();
      return false;
    }
    entry.sent = place.epoch;
    let batch = this.outbox.get(place.epoch);
    if (!batch) this.outbox.set(place.epoch, (batch = []));
    batch.push({ beat: place.beat, record: entry.record });
    if (!this.flushQueued) {
      this.flushQueued = true;
      queueMicrotask(() => this.flushBatch());
    }
    return true;
  }

  flushBatch() {
    this.flushQueued = false;
    if (this.outbox.size === 0 || this.closed) return;
    const batches = [...this.outbox];
    this.outbox.clear();
    for (const [epoch, records] of batches) {
      for (let start = 0; start < records.length; start += 4096) {
        this.sendMessage(MESSAGE.events, 0, eventsBody(epoch, records.slice(start, start + 4096)));
      }
    }
  }

  /** After the epoch moved from `previous`: what is still to come goes again. */
  replan(previous, wrapped) {
    const now = this.clock();
    const keep = [];
    for (const entry of this.entries) {
      if (entry.sent === this.epoch) {
        keep.push(entry); // sent for the next lap, which this is
      } else if (entry.sent === null) {
        keep.push(entry); // held: placed below
      } else if (this.timeBase === 'seconds' && entry.time > now) {
        entry.sent = null;
        keep.push(entry);
        this.send(entry);
      }
      // Otherwise it belonged to the lap that ended, or, in beats mode,
      // to before a jump: the page plans again.
    }
    this.entries = keep;
    void previous;
    void wrapped;
  }

  /** Sends the held entries that can go now. */
  placeHeld() {
    for (const entry of this.entries) {
      if (entry.sent === null) this.send(entry);
    }
  }

  /** Forgets what is past. */
  prune() {
    const now = this.clock();
    const beat = this.transport ? this.currentBeat : 0;
    this.entries = this.entries.filter((entry) =>
      entry.sent === null ||
      (this.timeBase === 'seconds' ? entry.time > now - 1 : entry.time > beat - 8 || entry.sent !== this.epoch),
    );
  }

  /** While the DAW is stopped, 'seconds' events play as soon as possible when due. */
  armHold() {
    if (this.holdTimer !== null || this.timeBase !== 'seconds' || this.closed) return;
    this.holdTimer = setTimeout(() => {
      this.holdTimer = null;
      this.pump();
    }, 5);
  }

  /** Plays the held events that are due, while the DAW is stopped. Tests call it directly. */
  pump() {
    const now = this.clock();
    let waiting = false;
    for (const entry of this.entries) {
      if (entry.sent !== null) continue;
      if (this.transport && this.transport.playing) {
        this.send(entry);
      } else if (entry.time <= now) {
        this.send({ ...entry, time: NaN });
        entry.sent = -1; // played
      } else {
        waiting = true;
      }
    }
    this.entries = this.entries.filter((entry) => entry.sent !== -1);
    if (waiting) this.armHold();
  }

  async control(type, fields) {
    const part = u16(fields.part);
    let kind;
    let body;
    switch (type) {
      case 'createPart':
        kind = MESSAGE.createPart;
        body = concat(part, u16(0), u32(fields.kind), u32(fields.chain));
        break;
      case 'setInstrument':
        kind = MESSAGE.setInstrument;
        body = concat(part, u16(0), u32(fields.kind));
        break;
      case 'removePart':
        kind = MESSAGE.removePart;
        body = part;
        break;
      case 'setChain':
        kind = MESSAGE.setChain;
        body = concat(part, u16(0), u32(fields.chain));
        break;
      default:
        throw new Error(`${type}: not available over the bridge`);
    }
    const { status } = await this.ask(kind, body);
    if (status !== 0) throw new Error(`${type}: ${statusText(status)}`);
  }

  async query(what, argument = 0) {
    return this.ask(MESSAGE.query, concat(u32(what), u32(argument)));
  }

  async request(type, fields = {}) {
    switch (type) {
      case 'partChain':
        return (await this.query(QUERY.partChain, fields.part)).status;
      case 'paramsJson':
      case 'drumMapJson':
      case 'masterParamsJson': {
        const { status, body } = await this.query(QUERY[type], fields.part ?? 0);
        return status === 0 ? body : null;
      }
      case 'sync': {
        const { status } = await this.ask(MESSAGE.sync, new Uint8Array(0));
        if (status !== 0) throw new Error(`sync: ${statusText(status)}`);
        return null;
      }
      case 'stats':
        return this.stats();
      case 'limiterReduction':
        return this.lastStatus ? -Math.abs(this.lastStatus.limiterDb) : 0;
      case 'close':
        return null;
      default:
        throw new Error(`${type}: not available over the bridge`);
    }
  }

  /**
   * The latency between scheduling and hearing: `engineFrames`, the
   * plugin's, which the DAW compensates (`frames` and `seconds` are it),
   * and `ahead`, how many seconds ahead of the DAW's position events
   * should be sent so that they arrive in time.
   */
  async latency() {
    const { status, body } = await this.query(QUERY.latency);
    if (status !== 0) throw new Error(`latency: ${statusText(status)}`);
    const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
    const engineFrames = view.getUint32(0, true);
    return {
      engineFrames,
      outputFrames: 0,
      frames: engineFrames,
      seconds: engineFrames / this.sampleRate,
      maxBlock: view.getUint32(4, true),
      ahead: view.getFloat64(8, true),
    };
  }

  /** The engine's counters and the bridge's, as the plugin keeps them. */
  async stats() {
    const { status, body } = await this.query(QUERY.stats);
    if (status !== 0) throw new Error(`stats: ${statusText(status)}`);
    const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
    const stats = {};
    STAT_NAMES.forEach((name, index) => (stats[name] = Number(view.getBigUint64(index * 8, true))));
    BRIDGE_STAT_NAMES.forEach((name, index) => (stats[name] = Number(view.getBigUint64((11 + index) * 8, true))));
    stats.limiterDb = view.getFloat64(18 * 8, true);
    return stats;
  }

  /** Lets every note go and drops every event waiting in the plugin. */
  async panic() {
    this.entries = [];
    this.outbox.clear();
    await this.ask(MESSAGE.panic, new Uint8Array(0));
  }

  async loadSfzSteps(part, url, options, normalized) {
    if (this.closed) throw new Error('the engine is closed');
    const get = options.fetch ?? globalThis.fetch;
    if (typeof get !== 'function') throw new TypeError('loadSfz needs fetch, or options.fetch');
    const base = new URL(url, options.base ?? globalThis.location?.href);
    const bytesOf = async (address) => {
      const response = await get(address.href);
      if (!response.ok) throw new Error(`loadSfz: could not fetch ${address.href}: ${response.status}`);
      return new Uint8Array(await response.arrayBuffer());
    };
    const failure = ({ status, body }) =>
      new Error(`loadSfz ${base.href}: ${body.byteLength > 0 ? new TextDecoder().decode(body) : statusText(status)}`);
    const text = await bytesOf(base);
    const begun = await this.ask(MESSAGE.sfzBegin, concat(u16(part), u16(0), text));
    if (begun.status !== 0) throw failure(begun);
    const paths = JSON.parse(new TextDecoder().decode(begun.body));
    try {
      for (const path of paths) {
        const bytes = await bytesOf(new URL(path, base));
        const encoded = utf8(path);
        const added = await this.ask(MESSAGE.sfzFile, concat(u16(part), u16(encoded.byteLength), encoded, bytes));
        if (added.status !== 0) throw failure(added);
      }
    } catch (error) {
      if (!this.closed) this.sendMessage(MESSAGE.sfzAbandon, this.nextId++, u16(part));
      throw error;
    }
    const finished = await this.ask(MESSAGE.sfzFinish, u16(part));
    if (finished.status !== 0) {
      const error = failure(finished);
      normalized.reject(error);
      throw error;
    }
    const summary = JSON.parse(new TextDecoder().decode(finished.body));
    // Measured before it was handed over: playable and normalised at once.
    normalized.resolve({ makeUp: summary.make_up ?? null });
    return summary;
  }

  async close() {
    if (this.closed) return;
    this.flushBatch();
    this.setState('closed');
    this.release();
  }

  release() {
    if (this.holdTimer !== null) clearTimeout(this.holdTimer);
    this.holdTimer = null;
    if (this.socket.readyState === 0 || this.socket.readyState === 1) this.socket.close(1000, 'bye');
    super.release();
  }
}

for (const name of UNSUPPORTED) {
  AratuBridge.prototype[name] = async function unsupported() {
    throw new Error(`${name}: not available over the bridge`);
  };
}

export { paramId };
