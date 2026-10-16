// The demo: a one-bar drum pattern in Aratu's drum symbols and a bass line,
// looped, scheduled a little ahead of time through the event ring.

import { DRUM_SYMBOLS, ManguebyteEngine } from '../../js/engine.js';

const TEMPO = 112; // beats per minute
const STEP = 60 / TEMPO / 4; // a sixteenth note, in seconds
const LOOKAHEAD = 0.15; // how far ahead records are scheduled, in seconds
const TICK_MS = 25; // how often the scheduler wakes

const DRUMS = 0;
const BASS = 1;

// One bar of sixteenths. Each step lists the drums struck on it:
// b kick, s snare, c closed hat, o open hat, r ride, x crash.
const DRUM_PATTERN = ['bc', 'c', 'c', 'bc', 'sc', 'c', 'bc', 'o', 'c', 'bc', 'c', 'c', 'sc', 'c', 'bc', 'or'];
// The bass: a MIDI note on each step, or null for a rest.
const BASS_PATTERN = [33, null, 45, 33, null, 33, 36, null, 33, null, 45, 33, 38, null, 40, 43];

const rows = [
  { label: 'b kick', symbol: 'b' },
  { label: 's snare', symbol: 's' },
  { label: 'c hat', symbol: 'c' },
  { label: 'o open', symbol: 'o' },
  { label: 'r ride', symbol: 'r' },
  { label: 'bass', symbol: null },
];

const startButton = document.getElementById('start');
const stopButton = document.getElementById('stop');
const status = document.getElementById('status');
const grid = document.getElementById('grid');
const sliders = document.getElementById('sliders');

const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const noteName = (note) => `${NOTE_NAMES[note % 12]}${Math.floor(note / 12) - 1}`;

const cells = [];
for (const row of rows) {
  const label = document.createElement('div');
  label.className = 'label';
  label.textContent = row.label;
  grid.append(label);
  for (let step = 0; step < 16; step++) {
    const cell = document.createElement('div');
    cell.className = 'cell';
    const on = row.symbol ? DRUM_PATTERN[step].includes(row.symbol) : BASS_PATTERN[step] !== null;
    if (on) cell.classList.add('on');
    if (!row.symbol && on) cell.textContent = noteName(BASS_PATTERN[step]);
    grid.append(cell);
    (cells[step] ??= []).push(cell);
  }
}

let context = null;
let engine = null;
let timer = null;
let nextStep = 0;
let nextTime = 0;
// When the last Stop lets every note go: a new start must not come before
// it, or that all-notes-off would cut the new notes.
let stopTime = 0;
const shown = [];

function scheduleStep(step, time) {
  for (const symbol of DRUM_PATTERN[step]) {
    const accent = step % 4 === 0 ? 1 : 0.72;
    engine.noteOn(DRUMS, DRUM_SYMBOLS[symbol], symbol === 'c' ? accent * 0.7 : accent, time);
  }
  const note = BASS_PATTERN[step];
  if (note !== null) {
    engine.noteOn(BASS, note, step % 4 === 0 ? 0.95 : 0.8, time);
    engine.noteOff(BASS, note, time + STEP * 0.8);
  }
  shown.push({ step, time });
}

function tick() {
  while (nextTime < context.currentTime + LOOKAHEAD) {
    scheduleStep(nextStep, nextTime);
    nextStep = (nextStep + 1) % 16;
    nextTime += STEP;
  }
}

function drawPlayhead() {
  if (!engine) return;
  while (shown.length > 1 && shown[1].time <= context.currentTime) shown.shift();
  const now = shown.length > 0 && shown[0].time <= context.currentTime ? shown[0].step : -1;
  cells.forEach((column, step) => column.forEach((cell) => cell.classList.toggle('now', step === now)));
  requestAnimationFrame(drawPlayhead);
}

function addSlider(part, param, label, toText) {
  const id = `${part}-${param.id}`;
  const name = document.createElement('label');
  name.htmlFor = id;
  name.textContent = label;
  const input = document.createElement('input');
  input.type = 'range';
  input.id = id;
  input.min = '0';
  input.max = '1000';
  const value = document.createElement('output');
  // Logarithmic parameters get a logarithmic slider, as their description says.
  const log = param.scale.kind === 'log';
  const toPlain = (position) =>
    log ? param.min * (param.max / param.min) ** position : param.min + (param.max - param.min) * position;
  const toPosition = (plain) =>
    log ? Math.log(plain / param.min) / Math.log(param.max / param.min) : (plain - param.min) / (param.max - param.min);
  input.value = String(Math.round(toPosition(param.default) * 1000));
  value.textContent = toText(param.default);
  input.addEventListener('input', () => {
    const plain = toPlain(Number(input.value) / 1000);
    value.textContent = toText(plain);
    engine.setParam(part, param.id, plain);
  });
  sliders.append(name, input, value);
}

async function start() {
  startButton.disabled = true;
  try {
    if (!engine) {
      context = new AudioContext({ latencyHint: 'interactive' });
      engine = await ManguebyteEngine.create(context, { wasm: '/manguebyte_web.wasm' });
      engine.node.connect(context.destination);
      await engine.createPart(DRUMS, 'drums', 'full');
      await engine.createPart(BASS, 'synth', 'full');
      engine.setParam(DRUMS, 'reverb.mix', 0.1);
      engine.setParam(DRUMS, 'gain', -3);
      engine.setParam(BASS, 'synth.cutoff', 420);
      engine.setParam(BASS, 'synth.env_amount', 3.2);
      engine.setParam(BASS, 'synth.decay', 0.35);
      engine.setParam(BASS, 'synth.sustain', 0.45);
      engine.setParam(BASS, 'synth.release', 0.09);
      engine.setParam(BASS, 'reverb.mix', 0.06);
      const params = await engine.params(BASS);
      const byId = Object.fromEntries(params.map((param) => [param.id, param]));
      addSlider(BASS, { ...byId['synth.cutoff'], default: 420 }, 'Bass cutoff', (hz) => `${Math.round(hz)} Hz`);
      addSlider(BASS, byId['lowpass.cutoff'], 'Bass low-pass', (hz) => `${Math.round(hz)} Hz`);
      addSlider(DRUMS, { ...byId['reverb.mix'], default: 0.1 }, 'Drum reverb', (mix) => `${Math.round(mix * 100)} %`);
    }
    await context.resume();
    nextStep = 0;
    // Records on the same frame are applied in the order they were sent, so
    // notes from stopTime on come after the all-notes-off sent at Stop.
    nextTime = Math.max(context.currentTime + 0.1, stopTime);
    shown.length = 0;
    tick();
    timer = setInterval(tick, TICK_MS);
    requestAnimationFrame(drawPlayhead);
    stopButton.disabled = false;
    status.textContent =
      engine.mode === 'shared'
        ? 'Playing. Records go through a SharedArrayBuffer ring.'
        : 'Playing. The page is not cross-origin isolated, so records go by message.';
  } catch (error) {
    status.textContent = `Could not start: ${error.message}`;
    startButton.disabled = false;
  }
}

function stop() {
  clearInterval(timer);
  timer = null;
  // Steps up to LOOKAHEAD ahead are already in the engine: let them play,
  // and let every note go after the last of them.
  stopTime = nextTime;
  engine.allNotesOff(DRUMS, stopTime);
  engine.allNotesOff(BASS, stopTime);
  stopButton.disabled = true;
  startButton.disabled = false;
  status.textContent = 'Stopped.';
}

startButton.addEventListener('click', start);
stopButton.addEventListener('click', stop);
status.textContent = globalThis.crossOriginIsolated
  ? 'Ready. The page is cross-origin isolated.'
  : 'Ready. The page is not cross-origin isolated; the message fallback will be used.';
