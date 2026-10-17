//! Renders every physically modelled instrument preset playing a phrase in
//! its idiom (marimba arpeggios, vibraphone chords with the motor on, a
//! legato cello line, a trumpet fanfare, choir chords...) to 16-bit, 48 kHz
//! stereo WAV files, to judge them by ear, and measures what a voice of each
//! costs.
//!
//! ```text
//! cargo run --release -p manguebyte-dsp --example physical_model_wavs -- <directory>
//! ```
//!
//! Each file is normalised to a peak of -3 dBFS, so that every instrument
//! can be heard at a comfortable level; the level the voice made before
//! that is printed beside it. The file names are in Portuguese, the
//! language of the musicians they were first rendered for.
//!
//! The cost of a voice is measured by holding one note on a synth of one
//! voice for four seconds and timing the render: it is printed as the share
//! of one processor core a voice takes at 48 kHz. Run it with `--release`
//! for numbers that mean anything.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Expression, NoteRef};
use manguebyte_dsp::instruments::{
    BowedParams, BowedString, ChoirParams, ChoirVoice, MalletParams, MalletVoice, WindParams,
    WindVoice,
};
use manguebyte_dsp::{ProcessSpec, Processor, Synth, Voice};

const RATE: u32 = 48_000;

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: RATE as f64,
    max_block_size: 256,
    num_channels: 2,
};

/// The peak every file is normalised to: -3 dBFS.
const PEAK: f32 = 0.707_945_8;

/// Something a player does.
#[derive(Debug, Clone, Copy)]
enum Act {
    On {
        key: u8,
        velocity: f32,
        accent: bool,
    },
    Off(u8),
    /// An expression sent to every voice: breath, pressure, the vowel.
    Express(Expression, f64),
}

/// Timed acts, in seconds.
#[derive(Debug, Default)]
struct Score(Vec<(f64, Act)>);

impl Score {
    /// A note `length` seconds long.
    fn note(&mut self, at: f64, key: u8, length: f64, velocity: f32, accent: bool) {
        self.0.push((
            at,
            Act::On {
                key,
                velocity,
                accent,
            },
        ));
        self.0.push((at + length, Act::Off(key)));
    }

    /// A chord, its notes `roll` seconds apart from the lowest up.
    fn chord(&mut self, at: f64, keys: &[u8], length: f64, velocity: f32, roll: f64) {
        for (n, &key) in keys.iter().enumerate() {
            let start = at + roll * n as f64;
            self.note(start, key, length - roll * n as f64, velocity, false);
        }
    }

    /// A melody from `at`, a beat `beat` seconds long: each note is
    /// `(key, beats, velocity, accent)`, a key of 0 a rest. A `slurred`
    /// line holds each note into the next, so a legato synth slurs them; a
    /// detached one leaves a breath between them.
    fn line(&mut self, at: f64, beat: f64, notes: &[(u8, f64, f32, bool)], slurred: bool) -> f64 {
        let mut time = at;
        let count = notes.len();
        for (n, &(key, beats, velocity, accent)) in notes.iter().enumerate() {
            let length = beats * beat;
            if key != 0 {
                let held = if slurred && n + 1 < count && notes[n + 1].0 != 0 {
                    length + 0.03
                } else if slurred {
                    length * 0.97
                } else {
                    length * 0.85
                };
                self.note(time, key, held, velocity, accent);
            }
            time += length;
        }
        time
    }

    fn express(&mut self, at: f64, expression: Expression, value: f64) {
        self.0.push((at, Act::Express(expression, value)));
    }

    /// The acts in time order, in frames; at the same frame, releases
    /// first, so that a note ending as the next one starts does not count
    /// as held into it.
    fn frames(&self) -> Vec<(usize, Act)> {
        let mut acts: Vec<(usize, Act)> = self
            .0
            .iter()
            .map(|&(at, act)| ((at * f64::from(RATE)).round() as usize, act))
            .collect();
        acts.sort_by_key(|&(frame, act)| (frame, matches!(act, Act::On { .. })));
        acts
    }
}

/// Plays `score` on `synth` for `seconds`, each act on its exact frame, and
/// says how long that took.
fn render<V: Voice>(synth: &mut Synth<V>, score: &Score, seconds: f64) -> (AudioBuffer<f32>, f64) {
    let total = (seconds * f64::from(RATE)) as usize;
    let acts = score.frames();
    let mut out = AudioBuffer::<f32>::new(2, total);
    let mut piece = AudioBuffer::<f32>::new(2, 256);
    let (mut frame, mut next) = (0, 0);
    let clock = Instant::now();
    while frame < total {
        while let Some(&(at, act)) = acts.get(next) {
            if at > frame {
                break;
            }
            match act {
                Act::On {
                    key,
                    velocity,
                    accent,
                } => synth.note_on_accented(NoteRef::new(0, key), velocity, accent),
                Act::Off(key) => synth.note_off(NoteRef::new(0, key), 0.0),
                Act::Express(expression, value) => synth
                    .voices_mut()
                    .for_each(|voice| voice.expression(expression, value)),
            }
            next += 1;
        }
        let until = acts.get(next).map_or(total, |&(at, _)| at.min(total));
        let end = (frame + 256).min(until).max(frame + 1).min(total);
        piece.set_num_frames(end - frame);
        synth.process(&mut piece);
        for channel in 0..2 {
            out.channel_mut(channel)[frame..end].copy_from_slice(piece.channel(channel));
        }
        frame = end;
    }
    (out, clock.elapsed().as_secs_f64())
}

/// Normalises `audio` to [`PEAK`] and writes it as a 16-bit stereo WAV
/// file, printing the level it had.
fn write_wav(directory: &Path, name: &str, audio: &AudioBuffer<f32>) -> io::Result<()> {
    let frames = audio.num_frames();
    let peak = audio.peak();
    let scale = if peak > 0.0 { PEAK / peak } else { 1.0 };
    let data_bytes = u32::try_from(frames * 4).expect("a short file");
    let mut bytes = Vec::with_capacity(44 + frames * 4);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // format chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // two channels
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes()); // bytes a second
    bytes.extend_from_slice(&4u16.to_le_bytes()); // bytes a frame
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    let mut squares = 0.0f64;
    for frame in 0..frames {
        for channel in 0..2 {
            let sample = audio.channel(channel)[frame] * scale;
            squares += f64::from(sample) * f64::from(sample);
            let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let file = format!("{name}.wav");
    fs::write(directory.join(&file), bytes)?;
    let rms = (squares / (2 * frames.max(1)) as f64).sqrt();
    print!(
        "{file:36} rendered at {:6.1} dBFS, written with rms {:6.1} dBFS",
        20.0 * f64::from(peak).log10(),
        20.0 * rms.log10()
    );
    Ok(())
}

/// Renders `score` on `synth` into `directory` as `name`.
fn phrase<V: Voice>(
    directory: &Path,
    name: &str,
    mut synth: Synth<V>,
    legato: bool,
    score: &Score,
    seconds: f64,
) -> io::Result<()> {
    synth.set_legato(legato);
    synth.prepare(&SPEC);
    let (audio, took) = render(&mut synth, score, seconds);
    write_wav(directory, name, &audio)?;
    println!(", {:5.1}x real time", seconds / took);
    Ok(())
}

/// The share of one core a voice takes at 48 kHz, holding `key` for four
/// seconds on a synth of one voice.
///
/// The least of three runs is taken, as the others were slowed by
/// whatever else the machine was doing.
fn cost<V: Voice + Clone>(voice: V, key: u8, velocity: f32) -> f64 {
    let mut score = Score::default();
    score.note(0.0, key, 3.5, velocity, false);
    let seconds = 4.0;
    (0..3)
        .map(|_| {
            let mut synth = Synth::new([voice.clone()]);
            synth.prepare(&SPEC);
            render(&mut synth, &score, seconds).1 / seconds
        })
        .fold(f64::MAX, f64::min)
}

/// Four bars of sixteenths at 112 a minute, up and down C, A minor, F and
/// G across two octaves and a half, the beat leaned on, and a last chord.
fn marimba_arpeggios() -> Score {
    let sixteenth = 60.0 / 112.0 / 4.0;
    let chords: [[u8; 8]; 4] = [
        [48, 55, 60, 64, 67, 72, 76, 79],
        [45, 52, 57, 60, 64, 69, 72, 76],
        [41, 48, 53, 57, 60, 65, 69, 72],
        [43, 50, 55, 59, 62, 67, 71, 74],
    ];
    let mut score = Score::default();
    for (bar, chord) in chords.iter().enumerate() {
        for step in 0..16 {
            let index = if step < 8 { step } else { 15 - step };
            let velocity = if step % 4 == 0 {
                0.85
            } else {
                0.55 + 0.05 * (step % 2) as f32
            };
            let at = (bar * 16 + step) as f64 * sixteenth;
            score.note(at, chord[index], 0.5, velocity, false);
        }
    }
    score.chord(64.0 * sixteenth, &[48, 60, 64, 67, 72], 3.0, 0.8, 0.0);
    score
}

/// A quick run up and down G major, staccato, then repeated high notes.
fn xylophone_run() -> Score {
    let sixteenth = 60.0 / 132.0 / 4.0;
    let keys = [
        67, 69, 71, 72, 74, 76, 78, 79, 81, 83, 84, 86, 88, 90, 91, 0, 91, 88, 86, 83, 79, 83, 86,
        88, 91, 0, 91, 0, 91, 91, 95, 0,
    ];
    let mut score = Score::default();
    for (step, &key) in keys.iter().enumerate() {
        if key != 0 {
            let accent = step % 8 == 0;
            score.note(step as f64 * sixteenth, key, 0.12, 0.75, accent);
        }
    }
    score.chord(32.0 * sixteenth, &[79, 83, 86, 91], 2.0, 0.85, 0.0);
    score
}

/// Rootless jazz voicings, two bars each at 72 a minute, rolled a little
/// and held so the motor's tremolo is heard, the damper lifting between
/// chords; a melody note answers on top of each.
fn vibraphone_chords() -> Score {
    let beat = 60.0 / 72.0;
    let chords: [(&[u8], u8); 4] = [
        (&[53, 57, 60, 64], 69), // D minor ninth
        (&[53, 59, 64, 69], 74), // G thirteenth
        (&[55, 59, 62, 64], 71), // C major ninth
        (&[55, 61, 65, 70], 73), // A seventh, altered
    ];
    let mut score = Score::default();
    for (n, &(chord, top)) in chords.iter().enumerate() {
        let at = n as f64 * 4.0 * beat;
        score.chord(at, chord, 3.6 * beat, 0.7, 0.015);
        score.note(at + 2.5 * beat, top, 1.4 * beat, 0.6, false);
    }
    score.chord(16.0 * beat, &[53, 57, 60, 64, 69], 6.0, 0.6, 0.03);
    score
}

/// A plain tune high up, like a music box's.
fn glockenspiel_tune() -> Score {
    let beat = 60.0 / 96.0;
    let notes = [
        (79, 1.0, 0.7, false),
        (83, 0.5, 0.6, false),
        (86, 0.5, 0.6, false),
        (91, 1.0, 0.8, false),
        (88, 1.0, 0.7, false),
        (86, 0.5, 0.6, false),
        (84, 0.5, 0.6, false),
        (83, 1.0, 0.65, false),
        (81, 1.0, 0.6, false),
        (84, 0.5, 0.65, false),
        (88, 0.5, 0.65, false),
        (86, 1.0, 0.75, false),
        (83, 1.0, 0.65, false),
        (79, 3.0, 0.7, false),
    ];
    let mut score = Score::default();
    score.line(0.0, beat, &notes, false);
    score
}

/// Gentle broken chords high on the celesta: F major seventh, E minor
/// seventh, D minor seventh, C major seventh.
fn celesta_figures() -> Score {
    let eighth = 60.0 / 80.0 / 2.0;
    let chords: [[u8; 4]; 4] = [
        [77, 81, 84, 88],
        [76, 79, 83, 86],
        [74, 77, 81, 84],
        [72, 76, 79, 83],
    ];
    let mut score = Score::default();
    for (bar, chord) in chords.iter().enumerate() {
        let order = [0, 1, 2, 3, 2, 1, 2, 3];
        for (step, &index) in order.iter().enumerate() {
            let at = (bar * 8 + step) as f64 * eighth;
            score.note(at, chord[index], 0.8, 0.5 + 0.05 * (step % 3) as f32, false);
        }
    }
    score.chord(32.0 * eighth, &[72, 76, 79, 84], 3.0, 0.55, 0.04);
    score
}

/// The kalimba's two thumbs taking turns, low tine then high, over C, A
/// minor, F and G, twice.
fn kalimba_pattern() -> Score {
    let eighth = 60.0 / 100.0 / 2.0;
    let chords: [[u8; 6]; 4] = [
        [60, 64, 67, 72, 76, 79],
        [57, 60, 64, 69, 72, 76],
        [65, 69, 72, 77, 81, 84],
        [67, 71, 74, 79, 83, 86],
    ];
    let order = [0, 4, 2, 3, 1, 5, 2, 4];
    let mut score = Score::default();
    for round in 0..2 {
        for (bar, chord) in chords.iter().enumerate() {
            for (step, &index) in order.iter().enumerate() {
                let at = ((round * 4 + bar) * 8 + step) as f64 * eighth;
                let velocity = if step % 2 == 0 { 0.75 } else { 0.55 };
                score.note(at, chord[index], 1.0, velocity, false);
            }
        }
    }
    score.chord(64.0 * eighth, &[60, 67, 76], 3.0, 0.7, 0.03);
    score
}

/// A calypso riff on the tenor pan, syncopated sixteenths, with a roll on
/// the long notes as pan players hold them.
fn steel_pan_riff() -> Score {
    let sixteenth = 60.0 / 116.0 / 4.0;
    // (sixteenth, key, velocity, accent)
    let riff: [(usize, u8, f32, bool); 12] = [
        (0, 76, 0.8, true),
        (3, 79, 0.65, false),
        (6, 84, 0.8, true),
        (8, 81, 0.6, false),
        (10, 79, 0.65, false),
        (12, 76, 0.6, false),
        (14, 74, 0.7, false),
        (16, 72, 0.8, true),
        (19, 74, 0.6, false),
        (22, 76, 0.75, true),
        (24, 77, 0.6, false),
        (26, 79, 0.7, false),
    ];
    let mut score = Score::default();
    for round in 0..3 {
        let offset = round * 32;
        for &(step, key, velocity, accent) in &riff {
            let at = (offset + step) as f64 * sixteenth;
            score.note(at, key, 0.3, velocity, accent);
        }
        // A roll on the last note of the riff, as a long note is played.
        let last = if round == 2 { 72 } else { 81 };
        for step in 28..32 {
            let at = (offset + step) as f64 * sixteenth;
            score.note(at, last, 0.25, 0.5 + 0.05 * (step % 2) as f32, false);
        }
    }
    for step in 0..16 {
        let at = (96 + step) as f64 * sixteenth;
        score.note(at, 72, 0.2, 0.45 + 0.02 * step as f32, false);
    }
    score.chord(112.0 * sixteenth, &[72, 76, 79], 2.0, 0.8, 0.0);
    score
}

/// Changes on the bells, a stroke a second, each left to ring.
fn bell_changes() -> Score {
    let mut score = Score::default();
    let keys = [76, 72, 74, 67, 67, 74, 76, 72];
    for (n, &key) in keys.iter().enumerate() {
        let accent = n % 4 == 0;
        score.note(n as f64 * 1.1, key, 6.0, 0.75, accent);
    }
    score.chord(9.4, &[60, 67, 72], 6.0, 0.85, 0.0);
    score
}

/// A singing violin line in D major at 66 a minute, slurred in pairs and
/// threes, long notes for the vibrato to bloom.
fn violin_melody() -> Score {
    let beat = 60.0 / 66.0;
    let mut score = Score::default();
    let mut at = score.line(
        0.0,
        beat,
        &[
            (69, 1.0, 0.7, false),
            (74, 2.0, 0.8, false),
            (78, 0.5, 0.75, false),
            (76, 0.5, 0.7, false),
        ],
        true,
    );
    at = score.line(
        at,
        beat,
        &[
            (74, 1.0, 0.75, false),
            (73, 0.5, 0.7, false),
            (74, 0.5, 0.7, false),
            (76, 2.0, 0.8, false),
        ],
        true,
    );
    at = score.line(
        at,
        beat,
        &[
            (71, 0.5, 0.7, false),
            (73, 0.5, 0.7, false),
            (74, 0.5, 0.75, false),
            (76, 0.5, 0.75, false),
            (78, 1.0, 0.85, false),
            (79, 1.0, 0.85, false),
        ],
        true,
    );
    score.line(
        at,
        beat,
        &[
            (81, 3.0, 0.9, true),
            (78, 1.0, 0.7, false),
            (74, 4.0, 0.75, false),
        ],
        true,
    );
    score
}

/// The viola, lower and darker, in C minor.
fn viola_melody() -> Score {
    let beat = 1.0; // 60 a minute
    let mut score = Score::default();
    let at = score.line(
        0.0,
        beat,
        &[
            (60, 1.0, 0.7, false),
            (63, 1.0, 0.75, false),
            (67, 2.0, 0.8, false),
            (65, 0.5, 0.7, false),
            (63, 0.5, 0.7, false),
            (62, 1.0, 0.7, false),
        ],
        true,
    );
    score.line(
        at,
        beat,
        &[
            (63, 0.5, 0.7, false),
            (65, 0.5, 0.7, false),
            (67, 1.0, 0.8, false),
            (68, 2.0, 0.85, false),
            (67, 1.0, 0.75, false),
            (60, 3.0, 0.7, false),
        ],
        true,
    );
    score
}

/// A legato cello line in G major, low in the instrument, a beat a second.
fn cello_legato() -> Score {
    let beat = 1.0; // 60 a minute
    let mut score = Score::default();
    let at = score.line(
        0.0,
        beat,
        &[
            (43, 1.0, 0.75, false),
            (50, 1.0, 0.75, false),
            (55, 1.0, 0.8, false),
            (59, 2.0, 0.85, false),
            (57, 0.5, 0.75, false),
            (55, 0.5, 0.75, false),
        ],
        true,
    );
    score.line(
        at,
        beat,
        &[
            (54, 1.0, 0.75, false),
            (55, 1.0, 0.75, false),
            (52, 1.0, 0.7, false),
            (50, 1.0, 0.7, false),
            (47, 1.0, 0.7, false),
            (43, 3.0, 0.75, false),
        ],
        true,
    );
    score
}

/// A slow bowed bass line, from G down to the low E and up.
fn contrabass_line() -> Score {
    let beat = 1.0; // 60 a minute
    let mut score = Score::default();
    score.line(
        0.0,
        beat,
        &[
            (43, 2.0, 0.8, false),
            (41, 1.0, 0.75, false),
            (40, 1.0, 0.75, false),
            (38, 2.0, 0.8, false),
            (36, 1.0, 0.75, false),
            (35, 1.0, 0.75, false),
            (33, 2.0, 0.8, false),
            (28, 4.0, 0.85, false),
        ],
        true,
    );
    score
}

/// The string section: C, A minor, F, G and C, four parts, each chord
/// growing as the bows press in.
fn string_chords() -> Score {
    let beat = 1.0; // 60 a minute
    let chords: [[u8; 4]; 5] = [
        [48, 55, 64, 72],
        [45, 57, 64, 72],
        [41, 57, 65, 69],
        [43, 55, 62, 71],
        [48, 55, 64, 72],
    ];
    let mut score = Score::default();
    for (n, chord) in chords.iter().enumerate() {
        let at = n as f64 * 3.0 * beat;
        let length = if n == 4 { 5.0 } else { 3.0 * beat + 0.02 };
        score.chord(at, chord, length, 0.65, 0.0);
        // Pressure: in, then easing at the end of each chord.
        for step in 0..12 {
            let t = f64::from(step) / 12.0;
            let swell = 0.55 + 0.45 * (core::f64::consts::PI * t).sin();
            score.express(at + t * 3.0 * beat, Expression::Pressure, swell);
        }
    }
    score
}

/// Plucked violin, quick eighths in G.
fn violin_pizzicato() -> Score {
    let eighth = 60.0 / 120.0 / 2.0;
    let keys = [
        67, 71, 74, 79, 74, 71, 67, 71, 69, 72, 76, 81, 76, 72, 69, 72, 71, 74, 79, 83, 79, 74, 71,
        74, 79, 0, 74, 0, 67,
    ];
    let mut score = Score::default();
    for (step, &key) in keys.iter().enumerate() {
        if key != 0 {
            let velocity = if step % 4 == 0 { 0.85 } else { 0.65 };
            score.note(step as f64 * eighth, key, 0.4, velocity, step % 8 == 0);
        }
    }
    score
}

/// Plucked cello, a walking line.
fn cello_pizzicato() -> Score {
    let beat = 60.0 / 100.0;
    let keys = [
        48, 52, 55, 57, 53, 57, 60, 62, 55, 59, 62, 65, 60, 55, 52, 48,
    ];
    let mut score = Score::default();
    for (step, &key) in keys.iter().enumerate() {
        let velocity = if step % 4 == 0 { 0.85 } else { 0.7 };
        score.note(step as f64 * beat, key, 0.8 * beat, velocity, false);
    }
    score
}

/// Jazz bass: a walking line through D minor, G seventh and C at 120 a
/// minute.
fn contrabass_walking() -> Score {
    let beat = 60.0 / 120.0;
    let keys = [
        38, 41, 45, 48, 43, 47, 50, 41, 36, 40, 43, 47, 48, 45, 43, 40, 38, 41, 45, 48, 31, 35, 38,
        41, 36,
    ];
    let mut score = Score::default();
    for (step, &key) in keys.iter().enumerate() {
        let velocity = if step % 2 == 0 { 0.85 } else { 0.72 };
        let length = if step + 1 == keys.len() {
            2.0
        } else {
            0.9 * beat
        };
        score.note(step as f64 * beat, key, length, velocity, false);
    }
    score
}

/// A flute melody in D, long notes for its vibrato, slurred.
fn flute_melody() -> Score {
    let beat = 60.0 / 72.0;
    let mut score = Score::default();
    let at = score.line(
        0.0,
        beat,
        &[
            (74, 2.0, 0.7, false),
            (76, 0.5, 0.65, false),
            (78, 0.5, 0.7, false),
            (81, 3.0, 0.8, false),
        ],
        true,
    );
    let at = score.line(
        at + 0.1,
        beat,
        &[
            (79, 0.5, 0.7, false),
            (78, 0.5, 0.7, false),
            (76, 1.0, 0.7, false),
            (74, 1.0, 0.65, false),
            (71, 1.0, 0.65, false),
        ],
        true,
    );
    score.line(
        at + 0.1,
        beat,
        &[
            (73, 1.0, 0.7, false),
            (76, 1.0, 0.75, false),
            (74, 4.0, 0.75, false),
        ],
        true,
    );
    score
}

/// A clarinet phrase up from the low register through the break and back,
/// in C minor.
fn clarinet_phrase() -> Score {
    let eighth = 60.0 / 88.0 / 2.0;
    let mut score = Score::default();
    let at = score.line(
        0.0,
        eighth,
        &[
            (55, 2.0, 0.65, false),
            (60, 1.0, 0.65, false),
            (63, 1.0, 0.7, false),
            (67, 1.0, 0.7, false),
            (70, 1.0, 0.75, false),
            (72, 2.0, 0.8, false),
            (75, 3.0, 0.85, false),
            (74, 1.0, 0.75, false),
        ],
        true,
    );
    let at = score.line(
        at,
        eighth,
        &[
            (72, 1.0, 0.7, false),
            (70, 1.0, 0.7, false),
            (67, 1.0, 0.7, false),
            (63, 1.0, 0.65, false),
            (62, 2.0, 0.65, false),
            (0, 1.0, 0.0, false),
            (63, 1.0, 0.7, true),
        ],
        false,
    );
    score.line(at, eighth, &[(60, 6.0, 0.7, false)], true);
    score
}

/// A blues phrase for the saxophone, swung, with a push on the blue notes.
fn saxophone_phrase() -> Score {
    let beat = 60.0 / 96.0;
    let (long, short) = (2.0 / 3.0, 1.0 / 3.0);
    let mut score = Score::default();
    let at = score.line(
        0.0,
        beat,
        &[
            (58, long, 0.7, false),
            (61, short, 0.65, false),
            (63, long, 0.7, false),
            (64, short, 0.75, true),
            (65, 1.0, 0.8, false),
            (68, long, 0.75, false),
            (70, short + 1.0, 0.85, true),
        ],
        true,
    );
    let at = score.line(
        at + 0.15,
        beat,
        &[
            (68, long, 0.75, false),
            (65, short, 0.7, false),
            (64, long, 0.75, true),
            (63, short, 0.7, false),
            (61, 1.0, 0.7, false),
            (58, 2.0, 0.75, false),
        ],
        true,
    );
    score.line(
        at + 0.2,
        beat,
        &[(53, 0.5, 0.7, false), (58, 3.0, 0.8, true)],
        true,
    );
    score
}

/// A trumpet fanfare: tongued triplets, arpeggios in C, accents.
fn trumpet_fanfare() -> Score {
    let beat = 60.0 / 112.0;
    let t = 1.0 / 3.0;
    let mut score = Score::default();
    score.line(
        0.0,
        beat,
        &[
            (67, t, 0.75, false),
            (67, t, 0.7, false),
            (67, t, 0.7, false),
            (72, 2.0, 0.85, true),
            (67, 0.5, 0.75, false),
            (72, 0.5, 0.75, false),
            (76, 1.0, 0.85, true),
            (79, 2.0, 0.9, true),
            (76, t, 0.75, false),
            (76, t, 0.7, false),
            (76, t, 0.7, false),
            (79, 0.5, 0.8, false),
            (76, 0.5, 0.75, false),
            (72, 1.0, 0.8, true),
            (74, 1.0, 0.8, false),
            (67, t, 0.75, false),
            (67, t, 0.7, false),
            (67, t, 0.7, false),
            (72, 4.0, 0.95, true),
        ],
        false,
    );
    score
}

/// A trombone phrase in B flat, slurred so the slide glides between notes.
fn trombone_phrase() -> Score {
    let beat = 60.0 / 70.0;
    let mut score = Score::default();
    let at = score.line(
        0.0,
        beat,
        &[
            (46, 1.0, 0.75, true),
            (53, 1.0, 0.75, false),
            (58, 2.0, 0.85, false),
            (57, 0.5, 0.75, false),
            (53, 0.5, 0.7, false),
        ],
        true,
    );
    score.line(
        at + 0.1,
        beat,
        &[
            (50, 1.0, 0.7, false),
            (51, 1.0, 0.75, false),
            (53, 2.0, 0.8, false),
            (46, 3.0, 0.8, true),
        ],
        true,
    );
    score
}

/// Four-part choir chords, three seconds each.
fn choir_chords(chords: &[[u8; 4]]) -> Score {
    let mut score = Score::default();
    for (n, chord) in chords.iter().enumerate() {
        let at = n as f64 * 3.0;
        let length = if n + 1 == chords.len() { 4.5 } else { 3.05 };
        score.chord(at, chord, length, 0.7, 0.0);
    }
    score
}

/// A solo singer in F major, slurred, the vowel moving from note to note
/// (through the brightness expression: u 0, o 0.25, a 0.5, e 0.75, i 1).
fn solo_vowels() -> Score {
    let beat = 60.0 / 66.0;
    let mut score = Score::default();
    let notes = [
        (65, 1.0, 0.7, false),
        (69, 1.0, 0.75, false),
        (72, 2.0, 0.85, false),
        (70, 1.0, 0.75, false),
        (69, 1.0, 0.7, false),
        (67, 1.0, 0.7, false),
        (65, 3.0, 0.75, false),
    ];
    let vowels = [0.5, 0.5, 0.25, 0.75, 1.0, 0.5, 0.25];
    score.line(0.0, beat, &notes, true);
    let mut at = 0.0;
    for (&(_, beats, _, _), &vowel) in notes.iter().zip(&vowels) {
        score.express(at, Expression::Brightness, vowel);
        at += beats * beat;
    }
    score
}

fn main() -> io::Result<()> {
    let directory: PathBuf = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    fs::create_dir_all(&directory)?;
    let directory = directory.as_path();

    // Mallets.
    let mallets = [
        (
            "marimba_arpejo",
            MalletParams::marimba(),
            marimba_arpeggios(),
            13.0,
        ),
        (
            "xilofone_escala",
            MalletParams::xylophone(),
            xylophone_run(),
            6.5,
        ),
        (
            "vibrafone_acordes_motor",
            MalletParams::vibraphone(),
            vibraphone_chords(),
            20.0,
        ),
        (
            "glockenspiel_melodia",
            MalletParams::glockenspiel(),
            glockenspiel_tune(),
            10.0,
        ),
        (
            "celesta_arpejos",
            MalletParams::celesta(),
            celesta_figures(),
            15.0,
        ),
        (
            "kalimba_padrao",
            MalletParams::kalimba(),
            kalimba_pattern(),
            23.0,
        ),
        (
            "steel_pan_riff",
            MalletParams::steel_pan(),
            steel_pan_riff(),
            17.0,
        ),
        (
            "sinos_tubulares",
            MalletParams::tubular_bells(),
            bell_changes(),
            17.0,
        ),
    ];
    for (name, params, score, seconds) in mallets {
        let synth = Synth::with_voices(12, || MalletVoice::new(params));
        phrase(directory, name, synth, false, &score, seconds)?;
    }

    // Bowed and plucked strings.
    let bowed = [
        (
            "violino_melodia",
            BowedParams::violin(),
            violin_melody(),
            true,
            26.0,
        ),
        (
            "viola_melodia",
            BowedParams::viola(),
            viola_melody(),
            true,
            17.0,
        ),
        (
            "violoncelo_legato",
            BowedParams::cello(),
            cello_legato(),
            true,
            18.0,
        ),
        (
            "contrabaixo_arco",
            BowedParams::contrabass(),
            contrabass_line(),
            true,
            17.0,
        ),
        (
            "cordas_acordes",
            BowedParams::string_ensemble(),
            string_chords(),
            false,
            18.0,
        ),
        (
            "violino_pizzicato",
            BowedParams::violin_pizzicato(),
            violin_pizzicato(),
            false,
            9.0,
        ),
        (
            "violoncelo_pizzicato",
            BowedParams::cello_pizzicato(),
            cello_pizzicato(),
            false,
            11.0,
        ),
        (
            "contrabaixo_pizzicato_walking",
            BowedParams::contrabass_pizzicato(),
            contrabass_walking(),
            false,
            15.0,
        ),
    ];
    for (name, params, score, legato, seconds) in bowed {
        let voices = if legato { 2 } else { 8 };
        let synth = Synth::with_voices(voices, || BowedString::new(params));
        phrase(directory, name, synth, legato, &score, seconds)?;
    }

    // Winds.
    let winds = [
        (
            "flauta_melodia_vibrato",
            WindParams::flute(),
            flute_melody(),
            20.0,
        ),
        (
            "clarinete_frase",
            WindParams::clarinet(),
            clarinet_phrase(),
            14.0,
        ),
        (
            "saxofone_blues",
            WindParams::saxophone(),
            saxophone_phrase(),
            14.0,
        ),
        (
            "trompete_fanfarra",
            WindParams::trumpet(),
            trumpet_fanfare(),
            11.0,
        ),
        (
            "trombone_frase",
            WindParams::trombone(),
            trombone_phrase(),
            15.0,
        ),
    ];
    for (name, params, score, seconds) in winds {
        let synth = Synth::with_voices(2, || WindVoice::new(params));
        phrase(directory, name, synth, true, &score, seconds)?;
    }

    // Voices.
    let aah = choir_chords(&[
        [48, 55, 64, 72],
        [53, 57, 65, 72],
        [50, 59, 67, 74],
        [48, 55, 64, 72],
    ]);
    let ooh = choir_chords(&[
        [45, 57, 64, 72],
        [50, 57, 65, 74],
        [52, 56, 64, 71],
        [45, 57, 64, 69],
    ]);
    let synth = Synth::with_voices(8, || ChoirVoice::new(ChoirParams::aah_choir()));
    phrase(directory, "coro_aah_acordes", synth, false, &aah, 15.0)?;
    let synth = Synth::with_voices(8, || ChoirVoice::new(ChoirParams::ooh_choir()));
    phrase(directory, "coro_ooh_acordes", synth, false, &ooh, 15.0)?;
    let synth = Synth::with_voices(2, || ChoirVoice::new(ChoirParams::solo_voice()));
    phrase(
        directory,
        "voz_solo_vogais",
        synth,
        true,
        &solo_vowels(),
        13.0,
    )?;

    // What a voice costs.
    println!();
    println!("{:28} share of one core per voice at 48 kHz", "preset");
    let report = |name: &str, share: f64| {
        println!("{name:28} {:6.2} %", 100.0 * share);
    };
    for (name, params, key) in [
        ("marimba", MalletParams::marimba(), 60),
        ("xylophone", MalletParams::xylophone(), 72),
        ("vibraphone", MalletParams::vibraphone(), 65),
        ("glockenspiel", MalletParams::glockenspiel(), 84),
        ("celesta", MalletParams::celesta(), 72),
        ("kalimba", MalletParams::kalimba(), 67),
        ("steel pan", MalletParams::steel_pan(), 72),
        ("tubular bells", MalletParams::tubular_bells(), 67),
    ] {
        report(name, cost(MalletVoice::new(params), key, 0.8));
    }
    for (name, params, key) in [
        ("violin", BowedParams::violin(), 69),
        ("viola", BowedParams::viola(), 60),
        ("cello", BowedParams::cello(), 48),
        ("double bass", BowedParams::contrabass(), 36),
        (
            "string section (4 players)",
            BowedParams::string_ensemble(),
            60,
        ),
        ("violin pizzicato", BowedParams::violin_pizzicato(), 69),
    ] {
        report(name, cost(BowedString::new(params), key, 0.8));
    }
    for (name, params, key) in [
        ("flute", WindParams::flute(), 74),
        ("clarinet", WindParams::clarinet(), 60),
        ("saxophone", WindParams::saxophone(), 61),
        ("trumpet", WindParams::trumpet(), 67),
        ("trombone", WindParams::trombone(), 53),
    ] {
        report(name, cost(WindVoice::new(params), key, 0.8));
    }
    for (name, params) in [
        ("aah choir (6 singers)", ChoirParams::aah_choir()),
        ("ooh choir (6 singers)", ChoirParams::ooh_choir()),
        ("solo voice", ChoirParams::solo_voice()),
    ] {
        report(name, cost(ChoirVoice::new(params), 60, 0.8));
    }
    Ok(())
}
