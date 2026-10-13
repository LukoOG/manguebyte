//! A small monophonic synth voice and sequencer shared by the examples.

// Each example uses a different subset of this module.
#![allow(dead_code)]

use manguebyte::prelude::*;

/// The riff: MIDI note and length in sixteenth notes. A minor pentatonic.
const RIFF: [(u8, u32); 12] = [
    (45, 2),
    (48, 1),
    (50, 1),
    (52, 2),
    (55, 1),
    (57, 1),
    (55, 2),
    (52, 2),
    (50, 1),
    (48, 1),
    (45, 2),
    (57, 4),
];

const BPM: f64 = 112.0;
/// Fraction of each step the note is held before it is released.
const GATE: f64 = 0.7;

type Voice = (Oscillator, Filter<f32>, Adsr, Delay<f32>, Gain<f32>);

/// Plays [`RIFF`] once through a saw â†’ lowpass â†’ envelope â†’ echo â†’ gain chain.
///
/// Notes are triggered at exact frame positions, whatever block sizes the
/// caller asks for.
pub struct RiffPlayer {
    voice: Voice,
    scratch: AudioBuffer<f32>,
    frames_per_sixteenth: usize,
    /// Frames rendered since the start.
    position: usize,
    /// Index into [`RIFF`] of the note that starts next.
    next_note: usize,
    /// Frame at which `next_note` starts.
    next_note_at: usize,
    /// Frame at which the sounding note is released, if one is held.
    release_at: Option<usize>,
}

impl RiffPlayer {
    pub fn new(sample_rate: f64, num_channels: usize, max_block_size: usize) -> Self {
        let mut voice: Voice = (
            Oscillator::new(Waveform::Saw, 110.0),
            Filter::new(FilterShape::Lowpass, 1_600.0, 2.5),
            Adsr::new(AdsrParams {
                attack: 0.004,
                decay: 0.14,
                sustain: 0.3,
                release: 0.18,
            }),
            Delay::new(1.0)
                .with_time(60.0 / BPM * 0.75)
                .with_feedback(0.35)
                .with_mix(0.22),
            Gain::from_db(-10.0),
        );
        voice.prepare(&ProcessSpec {
            sample_rate,
            max_block_size,
            num_channels,
        });

        Self {
            voice,
            scratch: AudioBuffer::new(num_channels, max_block_size),
            frames_per_sixteenth: (sample_rate * 60.0 / BPM / 4.0) as usize,
            position: 0,
            next_note: 0,
            next_note_at: 0,
            release_at: None,
        }
    }

    /// Length of the riff plus a tail for the echoes to fade, in frames.
    pub fn total_frames(&self) -> usize {
        let sixteenths: u32 = RIFF.iter().map(|(_, length)| length).sum();
        (sixteenths as usize + 12) * self.frames_per_sixteenth
    }

    pub fn is_finished(&self) -> bool {
        self.position >= self.total_frames()
    }

    /// Fills `buffer` with the next stretch of audio.
    pub fn render(&mut self, buffer: &mut AudioBuffer<f32>) {
        let frames = buffer.num_frames();
        let mut done = 0;
        while done < frames {
            self.fire_due_events();

            // Run up to the next event, so it lands on its exact frame.
            let mut span = (frames - done).min(self.scratch.capacity());
            for event in [self.release_at, self.upcoming_note()]
                .into_iter()
                .flatten()
            {
                span = span.min(event - self.position);
            }

            self.scratch.set_num_frames(span);
            self.voice.process(&mut self.scratch);
            for (out, rendered) in buffer.iter_channels_mut().zip(self.scratch.iter_channels()) {
                out[done..done + span].copy_from_slice(rendered);
            }
            done += span;
            self.position += span;
        }
    }

    fn upcoming_note(&self) -> Option<usize> {
        (self.next_note < RIFF.len()).then_some(self.next_note_at)
    }

    fn fire_due_events(&mut self) {
        let due_note = self.upcoming_note() == Some(self.position);
        let (oscillator, _, envelope, _, _) = &mut self.voice;

        if self.release_at == Some(self.position) {
            envelope.note_off();
            self.release_at = None;
        }
        if due_note {
            let (note, sixteenths) = RIFF[self.next_note];
            let length = sixteenths as usize * self.frames_per_sixteenth;
            oscillator.set_frequency(note_to_hz(f64::from(note)));
            envelope.note_on();
            self.release_at = Some(self.position + ((length as f64 * GATE) as usize).max(1));
            self.next_note += 1;
            self.next_note_at += length;
        }
    }
}
