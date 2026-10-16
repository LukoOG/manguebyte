//! Renders a short phrase on every instrument preset to 16-bit, 48 kHz
//! stereo WAV files, to judge them by ear.
//!
//! ```text
//! cargo run -p manguebyte-dsp --example instrument_wavs -- <directory>
//! ```
//!
//! Nothing is normalised: each file is at the level the voice makes, so the
//! presets can be compared with each other. The peak and RMS level of each
//! file are printed. Capital letters in the drum and bass-line patterns
//! below are accents, as Aratu writes them.

use std::fs;
use std::io;
use std::path::Path;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::instruments::{
    AcidParams, AcidVoice, AnalogDrum, AnalogKitParams, AnalogKitVoice, FmParams, FmVoice,
    KickBassParams, KickBassVoice, OrganParams, OrganVoice, PluckedString, RotaryParams,
    RotarySpeaker, RotarySpeed, StringParams,
};
use manguebyte_dsp::{Oversampled, ProcessSpec, Processor, Synth, Voice, Waveshaper};

const RATE: u32 = 48_000;

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: RATE as f64,
    max_block_size: 256,
    num_channels: 2,
};

/// Something a player does.
#[derive(Debug, Clone, Copy)]
enum Act {
    On {
        key: u8,
        velocity: f32,
        accent: bool,
    },
    Off(u8),
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

/// Plays `score` on `synth` for `seconds`, each act on its exact frame.
fn render<V: Voice>(synth: &mut Synth<V>, score: &Score, seconds: f64) -> AudioBuffer<f32> {
    let total = (seconds * f64::from(RATE)) as usize;
    let acts = score.frames();
    let mut out = AudioBuffer::<f32>::new(2, total);
    let mut piece = AudioBuffer::<f32>::new(2, 256);
    let (mut frame, mut next) = (0, 0);
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
    out
}

/// Runs `audio` through `processor` in blocks of 256, calling `at` before
/// each block with the time in seconds.
fn through<P: Processor<f32>>(
    audio: &mut AudioBuffer<f32>,
    processor: &mut P,
    mut at: impl FnMut(&mut P, f64),
) {
    processor.prepare(&SPEC);
    let frames = audio.num_frames();
    let mut start = 0;
    while start < frames {
        let end = (start + 256).min(frames);
        at(processor, start as f64 / f64::from(RATE));
        audio.windowed(start..end, |block| processor.process(block));
        start = end;
    }
}

/// Writes `audio` as a 16-bit stereo WAV file and prints its levels.
fn write_wav(directory: &Path, name: &str, audio: &AudioBuffer<f32>) -> io::Result<()> {
    let frames = audio.num_frames();
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
            let sample = audio.channel(channel)[frame];
            squares += f64::from(sample) * f64::from(sample);
            let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let file = format!("{name}.wav");
    fs::write(directory.join(&file), bytes)?;
    let peak = audio.peak();
    let rms = (squares / (2 * frames.max(1)) as f64).sqrt();
    println!(
        "{file:32} peak {:6.1} dBFS, rms {:6.1} dBFS",
        20.0 * f64::from(peak).log10(),
        20.0 * rms.log10()
    );
    Ok(())
}

/// Fingerpicked A minor, D minor, E seventh and A minor at 90 a minute:
/// the bass on the beat and the strings above it in eighths, let ring.
fn fingerpicking() -> Score {
    let eighth = 60.0 / 90.0 / 2.0;
    let chords: [&[u8]; 4] = [
        &[45, 52, 57, 60, 64],
        &[50, 57, 62, 65, 69],
        &[40, 47, 50, 56, 59, 64],
        &[45, 52, 57, 60, 64, 69],
    ];
    let mut score = Score::default();
    for (bar, chord) in chords.iter().enumerate() {
        let start = bar as f64 * 8.0 * eighth;
        score.note(start, chord[0], 8.0 * eighth, 0.85, false);
        let upper = &chord[1..];
        let order = [0, 1, 2, 3, 2, 1, 2, 3];
        for (step, &string) in order.iter().enumerate().skip(1) {
            let key = upper[string.min(upper.len() - 1)];
            let velocity = 0.55 + 0.1 * ((step % 3) as f32);
            score.note(start + step as f64 * eighth, key, 1.4, velocity, false);
        }
    }
    score
}

/// Strummed G, C, D and E minor at 100 a minute: down, down-up, up-down-up,
/// the strings 12 ms apart, up-strokes lighter and from the top.
fn strumming() -> Score {
    let beat = 60.0 / 100.0;
    let chords: [&[u8]; 4] = [
        &[43, 47, 50, 55, 59, 67],
        &[48, 52, 55, 60, 64],
        &[50, 57, 62, 66],
        &[40, 47, 52, 55, 59, 64],
    ];
    let strokes = [
        (0.0, true),
        (1.0, true),
        (1.5, false),
        (2.5, false),
        (3.0, true),
        (3.5, false),
    ];
    let mut score = Score::default();
    for (bar, chord) in chords.iter().enumerate() {
        for (index, &(at, down)) in strokes.iter().enumerate() {
            let start = (bar as f64 * 4.0 + at) * beat;
            let length = strokes
                .get(index + 1)
                .map_or(4.0 - at, |&(next, _)| next - at)
                * beat;
            let strings: Vec<u8> = if down {
                chord.to_vec()
            } else {
                chord.iter().rev().take(4).copied().collect()
            };
            for (order, &key) in strings.iter().enumerate() {
                let velocity = if down { 0.8 } else { 0.55 };
                score.note(
                    start + order as f64 * 0.012,
                    key,
                    length,
                    velocity,
                    at == 0.0,
                );
            }
        }
    }
    score
}

/// A bass line from steps: `(key, sixteenths long, velocity, accent)`,
/// `key` 0 for a rest, sixteenths at `tempo`.
fn line(tempo: f64, steps: &[(u8, f64, f32, bool)], gate: f64) -> Score {
    let sixteenth = 60.0 / tempo / 4.0;
    let mut score = Score::default();
    let mut at = 0.0;
    for &(key, length, velocity, accent) in steps {
        if key > 0 {
            score.note(at, key, length * sixteenth * gate, velocity, accent);
        }
        at += length * sixteenth;
    }
    score
}

/// A two-bar finger bass groove in E at 100 a minute, played twice.
fn finger_line() -> Score {
    let bar = [
        (28, 2.0, 0.9, false),
        (28, 1.0, 0.6, false),
        (40, 1.0, 0.75, false),
        (0, 2.0, 0.0, false),
        (31, 2.0, 0.8, false),
        (33, 2.0, 0.85, false),
        (35, 1.0, 0.7, false),
        (33, 1.0, 0.6, false),
        (31, 2.0, 0.8, false),
        (28, 2.0, 0.9, false),
    ];
    let steps: Vec<_> = bar.iter().cycle().take(4 * bar.len()).copied().collect();
    line(100.0, &steps, 0.85)
}

/// A slap groove: the thumb on the low E and G, pops (accented) an octave
/// up, ghost notes between.
fn slap_line() -> Score {
    let bar = [
        (28, 1.0, 0.95, false),
        (28, 1.0, 0.35, false),
        (40, 1.0, 0.9, true),
        (28, 1.0, 0.35, false),
        (28, 1.0, 0.9, false),
        (0, 1.0, 0.0, false),
        (40, 1.0, 0.9, true),
        (38, 1.0, 0.8, true),
        (31, 1.0, 0.95, false),
        (31, 1.0, 0.35, false),
        (43, 1.0, 0.9, true),
        (31, 1.0, 0.35, false),
        (33, 1.0, 0.9, false),
        (35, 1.0, 0.85, false),
        (45, 1.0, 0.9, true),
        (40, 1.0, 0.85, true),
    ];
    let steps: Vec<_> = bar.iter().cycle().take(3 * bar.len()).copied().collect();
    line(100.0, &steps, 0.6)
}

/// F major seventh, E minor seventh, D minor seventh and C major seventh,
/// two beats each at 80 a minute, with the root below and a melody above.
fn piano_chords() -> Score {
    let beat = 60.0 / 80.0;
    let chords: [&[u8]; 4] = [
        &[53, 57, 60, 64],
        &[52, 55, 59, 62],
        &[50, 53, 57, 60],
        &[48, 52, 55, 59],
    ];
    let melody = [
        (0.0, 72),
        (1.0, 71),
        (2.0, 69),
        (2.5, 67),
        (3.0, 69),
        (4.0, 67),
        (5.0, 64),
        (6.0, 67),
    ];
    let mut score = Score::default();
    for round in 0..2 {
        for (index, chord) in chords.iter().enumerate() {
            let start = (round * 8 + index * 2) as f64 * beat;
            score.note(start, chord[0] - 12, 1.9 * beat, 0.7, false);
            for (order, &key) in chord.iter().enumerate() {
                score.note(
                    start + 0.008 * order as f64,
                    key,
                    1.8 * beat,
                    0.5 + 0.05 * order as f32,
                    false,
                );
            }
        }
        for &(at, key) in &melody {
            let velocity = if round == 1 { 0.95 } else { 0.7 };
            score.note(
                (round as f64 * 8.0 + at) * beat,
                key,
                0.45 * beat,
                velocity,
                round == 1 && at == 0.0,
            );
        }
    }
    score
}

/// Bells: an arpeggio up, then a chord, ringing.
fn bells() -> Score {
    let mut score = Score::default();
    for (index, key) in [72, 76, 79, 84, 79, 76].into_iter().enumerate() {
        score.note(index as f64 * 0.7, key, 0.3, 0.8, false);
    }
    for key in [60, 67, 72] {
        score.note(4.6, key, 0.3, 0.9, true);
    }
    score
}

/// An eighties synth-pop bass in sixteenths at 118 a minute.
fn fm_line() -> Score {
    let bar = [
        (36, 1.0, 0.9, true),
        (36, 1.0, 0.6, false),
        (48, 1.0, 0.8, false),
        (36, 1.0, 0.6, false),
        (36, 1.0, 0.85, false),
        (48, 1.0, 0.7, false),
        (36, 1.0, 0.6, false),
        (46, 1.0, 0.8, false),
        (34, 1.0, 0.9, true),
        (34, 1.0, 0.6, false),
        (46, 1.0, 0.8, false),
        (34, 1.0, 0.6, false),
        (39, 1.0, 0.85, false),
        (41, 1.0, 0.7, false),
        (43, 1.0, 0.8, false),
        (46, 1.0, 0.75, false),
    ];
    let steps: Vec<_> = bar.iter().cycle().take(4 * bar.len()).copied().collect();
    line(118.0, &steps, 0.7)
}

/// Jazz organ comping at 120 a minute: shell voicings of a II-V-I in F,
/// stabbed and held, and a run on top.
fn organ_comping() -> Score {
    let beat = 0.5;
    let mut score = Score::default();
    let chords: [(f64, f64, &[u8]); 6] = [
        (0.0, 0.4, &[55, 58, 62, 65]),
        (1.5, 0.4, &[55, 58, 62, 65]),
        (3.0, 1.0, &[55, 58, 60, 64]),
        (4.0, 0.4, &[53, 57, 60, 64]),
        (5.5, 2.4, &[53, 57, 60, 64]),
        (8.0, 3.5, &[53, 58, 62, 65]),
    ];
    for &(at, length, keys) in &chords {
        for &key in keys {
            score.note(at * beat, key, length * beat, 1.0, false);
        }
        score.note(at * beat, keys[0] - 24, length * beat, 1.0, false);
    }
    let run = [72, 74, 75, 77, 79, 77, 75, 74, 72, 70, 69, 72];
    for (index, &key) in run.iter().enumerate() {
        score.note(
            (8.0 + 0.5 * index as f64) * beat,
            key,
            0.45 * beat,
            1.0,
            index == 0,
        );
    }
    score.note(14.5 * beat, 77, 3.0 * beat, 1.0, true);
    score
}

/// Two bars of drums at 100 a minute, twice, written as Aratu writes drum
/// lines: one character a sixteenth, capitals accented.
fn groove() -> Score {
    let lines: [(AnalogDrum, &str); 9] = [
        (AnalogDrum::Kick, "B.....b.b.......B.....b.b.....b."),
        (AnalogDrum::Snare, "....s.......S.......s.......S..s"),
        (AnalogDrum::Clap, "............c...............C..."),
        (AnalogDrum::ClosedHat, "CcCcCcCcCcCcCcC.CcCcCcCcCcCcCc.."),
        (AnalogDrum::OpenHat, "...............o..............O."),
        (AnalogDrum::Cowbell, "..c.......c.......c..c....c....."),
        (AnalogDrum::Rimshot, "......r.........................."),
        (AnalogDrum::HighTom, "........................tt......"),
        (AnalogDrum::LowTom, "..........................tt.t.."),
    ];
    let sixteenth = 60.0 / 100.0 / 4.0;
    let mut score = Score::default();
    for round in 0..2 {
        for (drum, pattern) in lines {
            for (step, symbol) in pattern.chars().enumerate() {
                if symbol == '.' {
                    continue;
                }
                let at = (round * 32 + step) as f64 * sixteenth;
                let accent = symbol.is_ascii_uppercase();
                score.note(at, drum.key(), 0.05, 0.85, accent);
            }
        }
    }
    score
}

/// Every drum of the kit once, plain, then accented.
fn kit_tour() -> Score {
    let mut score = Score::default();
    for (index, drum) in AnalogDrum::ALL.iter().enumerate() {
        score.note(index as f64 * 0.6, drum.key(), 0.05, 1.0, false);
        score.note(index as f64 * 0.6 + 0.3, drum.key(), 0.05, 1.0, true);
    }
    score
}

/// A trap bass line at 140 a minute, half time, with slides: notes that
/// overlap the next one glide into it.
fn trap_line() -> Score {
    let beat = 60.0 / 140.0;
    let mut score = Score::default();
    // (beat, key, beats long, accent); a length past the next note's start
    // makes a slide.
    let notes = [
        (0.0, 33, 1.75, true),
        (1.5, 33, 0.5, false),
        (2.25, 36, 1.0, false),
        (3.0, 38, 1.2, false),
        (4.0, 33, 2.75, true),
        (6.5, 40, 0.5, false),
        (7.0, 31, 1.0, false),
        (8.0, 33, 1.2, true),
        (9.0, 45, 1.0, false),
        (10.0, 33, 2.0, false),
        (12.0, 28, 1.0, true),
        (13.0, 31, 1.0, false),
        (14.0, 33, 2.0, false),
    ];
    for &(at, key, length, accent) in &notes {
        score.note(at * beat, key, length * beat, 1.0, accent);
    }
    score
}

/// Four bars of an acid line at 130 a minute: one step a sixteenth, as
/// `(key, accent, slide)`, `key` 0 for a rest. A slid step holds its gate
/// into the next, which glides.
fn acid_line() -> Score {
    let steps: [(u8, bool, bool); 16] = [
        (36, true, false),
        (36, false, true),
        (48, false, false),
        (36, false, false),
        (0, false, false),
        (39, true, true),
        (41, false, false),
        (36, false, false),
        (43, true, false),
        (36, false, true),
        (36, false, false),
        (48, true, true),
        (46, false, false),
        (0, false, false),
        (41, true, false),
        (43, false, true),
    ];
    let sixteenth = 60.0 / 130.0 / 4.0;
    let mut score = Score::default();
    for bar in 0..4 {
        for (index, &(key, accent, slide)) in steps.iter().enumerate() {
            if key == 0 {
                continue;
            }
            let at = (bar * 16 + index) as f64 * sixteenth;
            let length = if slide { 1.1 } else { 0.5 } * sixteenth;
            score.note(at, key, length, 1.0, accent);
        }
    }
    score
}

/// The acid line with the cut-off and the envelope turned up bar by bar, as
/// a player rides the knobs: the settings change between bars while the
/// voice plays on.
fn knob_ride() -> AudioBuffer<f32> {
    let mut synth = Synth::with_voices(1, || AcidVoice::new(AcidParams::classic()));
    synth.set_legato(true);
    synth.prepare(&SPEC);
    let score = acid_line();
    let bar = 4.0 * 60.0 / 130.0;
    let knobs = [(220.0, 0.35), (320.0, 0.55), (500.0, 0.75), (800.0, 0.9)];
    let mut sections = Vec::new();
    for (index, (cutoff, env_mod)) in knobs.into_iter().enumerate() {
        let params = AcidParams {
            cutoff,
            env_mod,
            ..AcidParams::classic()
        };
        synth
            .voices_mut()
            .for_each(|voice| voice.set_params(params));
        let start = index as f64 * bar;
        let part = Score(
            score
                .0
                .iter()
                .filter(|(at, _)| (start..start + bar).contains(at))
                .map(|&(at, act)| (at - start, act))
                .collect(),
        );
        sections.push(render(&mut synth, &part, bar));
    }
    let frames = sections.iter().map(AudioBuffer::num_frames).sum();
    let mut audio = AudioBuffer::<f32>::new(2, frames);
    let mut start = 0;
    for section in &sections {
        let end = start + section.num_frames();
        for channel in 0..2 {
            audio.channel_mut(channel)[start..end].copy_from_slice(section.channel(channel));
        }
        start = end;
    }
    audio
}

fn main() -> io::Result<()> {
    let directory = std::env::args()
        .nth(1)
        .expect("usage: instrument_wavs <directory>");
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;

    // Plucked strings.
    let strings = [
        (
            "guitar_nylon",
            StringParams::nylon_guitar(),
            fingerpicking(),
            12.0,
        ),
        (
            "guitar_steel",
            StringParams::steel_guitar(),
            strumming(),
            10.5,
        ),
        (
            "bass_finger",
            StringParams::finger_bass(),
            finger_line(),
            10.5,
        ),
        ("bass_slap", StringParams::slap_bass(), slap_line(), 7.5),
    ];
    for (name, params, score, seconds) in strings {
        let mut synth = Synth::with_voices(8, || PluckedString::new(params));
        synth.prepare(&SPEC);
        write_wav(directory, name, &render(&mut synth, &score, seconds))?;
    }

    // FM.
    let fm = [
        (
            "fm_electric_piano",
            FmParams::electric_piano(),
            piano_chords(),
            13.0,
        ),
        ("fm_tubular_bell", FmParams::tubular_bell(), bells(), 10.0),
        ("fm_bass", FmParams::fm_bass(), fm_line(), 8.5),
    ];
    for (name, params, score, seconds) in fm {
        let mut synth = Synth::with_voices(10, || FmVoice::new(params));
        synth.prepare(&SPEC);
        write_wav(directory, name, &render(&mut synth, &score, seconds))?;
    }

    // The organ, dry and through the rotary speaker: slow, then fast from
    // the third second and slow again from the seventh, to hear the rotors
    // speed up and slow down.
    let organs = [
        ("organ_jazz_dry", OrganParams::jazz(), false, false),
        ("organ_jazz_rotary", OrganParams::jazz(), true, false),
        (
            "organ_gospel_rotary_driven",
            OrganParams::gospel(),
            true,
            true,
        ),
        ("organ_full_rotary", OrganParams::full(), true, false),
        ("organ_ballad_rotary", OrganParams::ballad(), true, false),
    ];
    for (name, params, rotary, driven) in organs {
        let mut synth = Synth::new(OrganVoice::voices(16, params));
        synth.prepare(&SPEC);
        let mut audio = render(&mut synth, &organ_comping(), 10.0);
        if driven {
            let shaper = Waveshaper::<f32>::new(manguebyte_dsp::ShaperCurve::Tanh)
                .with_drive_db(12.0)
                .with_output_db(-6.0);
            through(&mut audio, &mut Oversampled::new(shaper, 4), |_, _| {});
        }
        if rotary {
            let mut leslie = RotarySpeaker::<f32>::new(RotaryParams::default());
            through(&mut audio, &mut leslie, |leslie, t| {
                let speed = if (3.0..7.0).contains(&t) {
                    RotarySpeed::Fast
                } else {
                    RotarySpeed::Slow
                };
                leslie.set_speed(speed);
            });
        }
        write_wav(directory, name, &audio)?;
    }

    // Drums.
    let mut kit = Synth::new(AnalogKitVoice::voices(12, AnalogKitParams::default()));
    kit.prepare(&SPEC);
    write_wav(
        directory,
        "drums_groove",
        &render(&mut kit, &groove(), 10.5),
    )?;
    let mut kit = Synth::new(AnalogKitVoice::voices(4, AnalogKitParams::default()));
    kit.prepare(&SPEC);
    write_wav(
        directory,
        "drums_kit_tour",
        &render(&mut kit, &kit_tour(), 10.5),
    )?;

    // The kick played as a bass, with slides.
    let mut bass = Synth::with_voices(1, || KickBassVoice::new(KickBassParams::default()));
    bass.set_legato(true);
    bass.prepare(&SPEC);
    write_wav(
        directory,
        "kick_bass_slides",
        &render(&mut bass, &trap_line(), 8.0),
    )?;

    // Acid.
    let acid = [
        ("acid_classic", AcidParams::classic()),
        ("acid_squelch", AcidParams::squelch()),
        ("acid_square", AcidParams::square()),
    ];
    for (name, params) in acid {
        let mut synth = Synth::with_voices(1, || AcidVoice::new(params));
        synth.set_legato(true);
        synth.prepare(&SPEC);
        write_wav(directory, name, &render(&mut synth, &acid_line(), 8.0))?;
    }
    write_wav(directory, "acid_knob_ride", &knob_ride())?;
    Ok(())
}
