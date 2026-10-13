//! A small polyphonic synth and sequencer shared by the examples.
//!
//! It shows the pieces working together: a [`Synth`] allocating voices, notes
//! and automation delivered as events on their exact frame, a transport that
//! keeps musical time, and effects chained after the instrument.

// Each example uses a different subset of this module.
#![allow(dead_code)]

use manguebyte::prelude::*;

const BPM: f64 = 112.0;
const POLYPHONY: usize = 8;

/// The song as `(start, length, key)`, with times in sixteenth notes.
/// A minor: three chords under a pentatonic riff.
const NOTES: [(u32, u32, u8); 21] = [
    // Chords.
    (0, 7, 57),
    (0, 7, 60),
    (0, 7, 64),
    (8, 7, 55),
    (8, 7, 59),
    (8, 7, 62),
    (16, 4, 57),
    (16, 4, 60),
    (16, 4, 64),
    // Riff.
    (0, 2, 45),
    (2, 1, 48),
    (3, 1, 50),
    (4, 2, 52),
    (6, 1, 55),
    (7, 1, 57),
    (8, 2, 55),
    (10, 2, 52),
    (12, 1, 50),
    (13, 1, 48),
    (14, 2, 45),
    (16, 4, 69),
];

/// Length of the song in sixteenth notes, before the tail.
const SONG_SIXTEENTHS: u32 = 20;
/// Extra time for echoes and reverb to fade.
const TAIL_SIXTEENTHS: u32 = 16;

const CUTOFF: ParamId = ParamId::from_name("cutoff");

/// One note: a sawtooth through a resonant low-pass that closes as the note
/// fades.
struct PluckVoice {
    oscillator: Oscillator,
    filter: StateVariableFilter<f32>,
    envelope: Adsr,
    velocity: f32,
    cutoff: f64,
}

impl PluckVoice {
    fn new() -> Self {
        Self {
            oscillator: Oscillator::new(Waveform::Saw, 220.0),
            filter: StateVariableFilter::new(SvfMode::Lowpass, 1_200.0, 2.0),
            envelope: Adsr::new(AdsrParams {
                attack: 0.004,
                decay: 0.25,
                sustain: 0.35,
                release: 0.3,
            }),
            velocity: 0.0,
            cutoff: 1_200.0,
        }
    }
}

impl Voice for PluckVoice {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.oscillator.set_sample_rate(spec.sample_rate);
        self.envelope.set_sample_rate(spec.sample_rate);
        // The voice is mono inside and is copied to every output channel.
        self.filter.prepare(&ProcessSpec {
            num_channels: 1,
            ..*spec
        });
    }

    fn note_on(&mut self, note: NoteRef, velocity: f32) {
        self.oscillator
            .set_frequency(note_to_hz(f64::from(note.key)));
        self.velocity = velocity;
        self.envelope.note_on();
    }

    fn note_off(&mut self, _velocity: f32) {
        self.envelope.note_off();
    }

    fn choke(&mut self) {
        self.envelope.silence();
        self.filter.reset();
        self.oscillator.reset_phase();
    }

    fn is_active(&self) -> bool {
        self.envelope.is_active()
    }

    fn render(&mut self, buffer: &mut AudioBuffer<f32>) {
        for frame in 0..buffer.num_frames() {
            let level = self.envelope.next_sample();
            // The filter follows the envelope, sample by sample. A
            // state-variable filter stays stable under this.
            self.filter.set_frequency(self.cutoff * (0.3 + 3.0 * level));
            let raw = self.oscillator.next_sample() as f32;
            let sample = self.filter.process_sample(0, raw) * level as f32 * self.velocity;
            for channel in buffer.iter_channels_mut() {
                channel[frame] += sample;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f64) {
        if param == CUTOFF {
            self.cutoff = value;
        }
    }
}

type Instrument = (Synth<PluckVoice>, Delay<f32>, Reverb<f32>, Gain<f32>);

/// Plays the song once through synth, echo, reverb and gain.
///
/// Notes land on their exact frame whatever block sizes the caller asks for.
pub struct RiffPlayer {
    instrument: Instrument,
    events: EventBuffer,
    transport: Transport,
    sample_rate: f64,
    max_block_size: usize,
    /// Frames rendered since the start.
    position: usize,
}

impl RiffPlayer {
    pub fn new(sample_rate: f64, num_channels: usize, max_block_size: usize) -> Self {
        let mut instrument: Instrument = (
            Synth::with_voices(POLYPHONY, PluckVoice::new),
            Delay::new(1.0)
                .with_time(60.0 / BPM * 0.75)
                .with_feedback(0.3)
                .with_mix(0.18),
            Reverb::new()
                .with_decay_seconds(1.8)
                .with_damping(0.5)
                .with_mix(0.2),
            Gain::from_db(-14.0),
        );
        instrument.prepare(&ProcessSpec {
            sample_rate,
            max_block_size,
            num_channels,
        });

        Self {
            instrument,
            events: EventBuffer::with_capacity(64),
            transport: Transport::playing_at(BPM),
            sample_rate,
            max_block_size,
            position: 0,
        }
    }

    /// Frame on which a time in sixteenth notes falls.
    fn frame_of(&self, sixteenths: u32) -> usize {
        (f64::from(sixteenths) * self.sample_rate * 60.0 / BPM / 4.0).round() as usize
    }

    /// Length of the song plus a tail for the effects to fade, in frames.
    pub fn total_frames(&self) -> usize {
        self.frame_of(SONG_SIXTEENTHS + TAIL_SIXTEENTHS)
    }

    pub fn is_finished(&self) -> bool {
        self.position >= self.total_frames()
    }

    /// Fills `buffer` with the next stretch of audio.
    pub fn render(&mut self, buffer: &mut AudioBuffer<f32>) {
        let frames = buffer.num_frames();
        let mut done = 0;
        while done < frames {
            let span = (frames - done).min(self.max_block_size);
            self.collect_events(span);
            let context = ProcessContext::new(&self.events, &self.transport);
            let instrument = &mut self.instrument;
            buffer.windowed(done..done + span, |block| {
                instrument.process_with(block, &context);
            });
            self.transport.advance(span, self.sample_rate);
            self.position += span;
            done += span;
        }
    }

    /// Gathers what happens in the next `span` frames.
    fn collect_events(&mut self, span: usize) {
        self.events.clear();
        let block = self.position..self.position + span;
        let offset = |frame: usize| (frame - block.start) as u32;

        // Automation: open the filter a little on every sixteenth note. The
        // steps sit at fixed points in the song, not at block boundaries, so
        // the result is the same whatever the block size.
        for step in 0..=SONG_SIXTEENTHS {
            let at = self.frame_of(step);
            if block.contains(&at) {
                let progress = f64::from(step) / f64::from(SONG_SIXTEENTHS);
                let kind = EventKind::ParamValue {
                    param: CUTOFF,
                    value: 700.0 + 1_500.0 * progress,
                };
                self.events
                    .push(Event {
                        offset: offset(at),
                        kind,
                    })
                    .ok();
            }
        }

        for &(start, length, key) in &NOTES {
            let note = NoteRef::new(0, key);
            let on = self.frame_of(start);
            // Release a little early so repeated keys re-strike cleanly.
            let off = on + (self.frame_of(start + length) - on) * 9 / 10;
            if block.contains(&on) {
                let kind = EventKind::NoteOn {
                    note,
                    velocity: 0.8,
                };
                self.events
                    .push(Event {
                        offset: offset(on),
                        kind,
                    })
                    .ok();
            }
            if block.contains(&off) {
                let kind = EventKind::NoteOff {
                    note,
                    velocity: 0.0,
                };
                self.events
                    .push(Event {
                        offset: offset(off),
                        kind,
                    })
                    .ok();
            }
        }
    }
}
