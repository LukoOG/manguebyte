// A stand-in for the browser's part of an AudioWorklet, in Node.js.
//
// Node has no AudioWorklet, so this file defines AudioWorkletProcessor,
// registerProcessor, sampleRate and currentFrame, loads worklet.js and
// engine.js against them, connects each node to its processor with a
// MessageChannel, and calls process() itself, one render quantum at a time,
// the way the audio thread would.

import { wasmBytes } from './wasm.js';

export const RATE = 48000;
export const QUANTUM = 128;

const registered = new Map();
let portForNextProcessor = null;
globalThis.registerProcessor = (name, processor) => registered.set(name, processor);
globalThis.sampleRate = RATE;
globalThis.currentFrame = 0;
globalThis.AudioWorkletProcessor = class {
  constructor() {
    this.port = portForNextProcessor;
  }
};

const engineModule = await import('../engine.js');
await import('../worklet.js');

export const { DRUM_NOTES, DRUM_SYMBOLS, ManguebyteEngine } = engineModule;

/** A stand-in for an AudioContext and its audio thread. */
export class FakeContext {
  constructor(startFrame = 0) {
    this.sampleRate = RATE;
    this.frame = startFrame;
    this.processors = [];
    this.audioWorklet = { addModule: async () => {} };
    const context = this;
    this.AudioWorkletNode = class {
      constructor(_context, name, options) {
        const channel = new MessageChannel();
        this.port = channel.port1;
        portForNextProcessor = channel.port2;
        globalThis.currentFrame = context.frame;
        this.processor = new (registered.get(name))(options);
        context.processors.push(this.processor);
      }
    };
  }

  get currentTime() {
    return this.frame / RATE;
  }

  /** Lets messages between the threads go through. */
  async settle() {
    for (let i = 0; i < 3; i++) await new Promise((resolve) => setImmediate(resolve));
  }

  /**
   * Renders `quanta` quanta and returns the left and right samples with the
   * context frame of the first one. `skip` quanta pass without process().
   */
  async run(quanta, { skip = 0 } = {}) {
    this.frame += skip * QUANTUM;
    const start = this.frame;
    const left = new Float32Array(quanta * QUANTUM);
    const right = new Float32Array(quanta * QUANTUM);
    for (let q = 0; q < quanta; q++) {
      await this.settle();
      globalThis.currentFrame = this.frame;
      const output = [new Float32Array(QUANTUM), new Float32Array(QUANTUM)];
      // A processor that returns false is finished, as in a browser.
      this.processors = this.processors.filter((processor) => processor.process([], [output], {}));
      left.set(output[0], q * QUANTUM);
      right.set(output[1], q * QUANTUM);
      this.frame += QUANTUM;
    }
    await this.settle();
    return { start, left, right };
  }
}

/** Makes an engine in `context` with the module the tests were given. */
export async function make(context, options = {}) {
  return ManguebyteEngine.create(context, {
    wasm: wasmBytes(),
    AudioWorkletNode: context.AudioWorkletNode,
    ...options,
  });
}

/** The index of the first sample that is not 0, or -1. */
export const firstSound = (samples) => samples.findIndex((sample) => sample !== 0);
