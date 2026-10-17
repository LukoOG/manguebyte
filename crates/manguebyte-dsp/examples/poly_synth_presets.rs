//! Renders every preset of the poly synth as a short phrase suited to its
//! category (a bass line, a lead melody, pad chords, a pluck arpeggio,
//! keyboard comping, brass stabs, effects) to 16-bit, 48 kHz stereo WAV
//! files named `<category>_<name>.wav`, and measures what a voice costs.
//!
//! ```text
//! cargo run --release -p manguebyte-dsp --example poly_synth_presets -- <directory>
//! cargo run --release -p manguebyte-dsp --example poly_synth_presets -- --levels
//! cargo run --release -p manguebyte-dsp --example poly_synth_presets -- --cost
//! ```
//!
//! Each file is scaled so that its peak is at -3 dBFS, so the presets can
//! be judged side by side at a similar level; the level the preset itself
//! made is printed beside it, with the time the render took against the
//! time it lasts and what one held voice of it costs. The bass line has
//! accents, as a step sequencer or Aratu sends them, and the mono and
//! legato presets play some of its notes and of the melody's overlapping,
//! so that they slide.
//!
//! With `--levels` nothing is written: for each preset, the example prints
//! the level that would put the peak of one note at -9 dBFS, which is how
//! the presets' levels were set. With `--cost` it only measures what a
//! held voice costs.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::instruments::poly::{
    Category, EnvParams, FilterModel, FilterParams, ModDestination, ModSlot, ModSource, OscParams,
    PRESETS, PolySynthParams, PolySynthVoice, Preset, TableShape, VoiceMode, Wave, matrix,
};
use manguebyte_dsp::{ProcessSpec, Processor, Synth};

const RATE: u32 = 48_000;

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: RATE as f64,
    max_block_size: 256,
    num_channels: 2,
};

/// The peak every file is scaled to, in decibels below full scale.
const PEAK_DBFS: f64 = -3.0;

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

/// Timed acts, in seconds, and how long to let the last note ring.
#[derive(Debug, Default)]
struct Score {
    acts: Vec<(f64, Act)>,
    tempo: f64,
    end: f64,
}

impl Score {
    fn new(tempo: f64) -> Self {
        Self {
            tempo,
            ..Self::default()
        }
    }

    /// The length of a beat, in seconds.
    fn beat(&self) -> f64 {
        60.0 / self.tempo
    }

    /// A note `length` seconds long.
    fn note(&mut self, at: f64, key: u8, length: f64, velocity: f32, accent: bool) {
        self.acts.push((
            at,
            Act::On {
                key,
                velocity,
                accent,
            },
        ));
        self.acts.push((at + length, Act::Off(key)));
        self.end = self.end.max(at + length);
    }

    /// The acts in time order, in frames; at the same frame, releases
    /// first, so that a note ending as the next one starts does not count
    /// as held into it.
    fn frames(&self) -> Vec<(usize, Act)> {
        let mut acts: Vec<(usize, Act)> = self
            .acts
            .iter()
            .map(|&(at, act)| ((at * f64::from(RATE)).round() as usize, act))
            .collect();
        acts.sort_by_key(|&(frame, act)| (frame, matches!(act, Act::On { .. })));
        acts
    }
}

/// Plays `score` on `synth` for `seconds`, each act on its exact frame.
fn render(synth: &mut Synth<PolySynthVoice>, score: &Score, seconds: f64) -> AudioBuffer<f32> {
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

/// Writes `audio` as a 16-bit stereo WAV file.
fn write_wav(path: &Path, audio: &AudioBuffer<f32>) -> io::Result<()> {
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
    for frame in 0..frames {
        for channel in 0..2 {
            let sample = audio.channel(channel)[frame];
            let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    fs::write(path, bytes)
}

/// A file name from a preset's: its category, an underscore, and its
/// name in lower case with hyphens for spaces.
fn file_name(preset: &Preset) -> String {
    let name: String = preset
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{}_{name}.wav", preset.category.name())
}

/// A bass line in A minor at 124 a minute, four bars of eighths and
/// sixteenths, with accents and, for the mono and legato presets, notes
/// that overlap the next so that they slide.
fn bass_line(sliding: bool) -> Score {
    let mut score = Score::new(124.0);
    let sixteenth = score.beat() / 4.0;
    // Steps of a bar: key (0 is a rest), length in sixteenths, accent.
    let bars: [&[(u8, usize, bool)]; 4] = [
        &[
            (33, 2, true),
            (0, 1, false),
            (33, 1, false),
            (45, 2, false),
            (33, 2, false),
            (0, 2, false),
            (43, 2, true),
            (45, 2, false),
            (31, 2, false),
        ],
        &[
            (29, 3, true),
            (29, 1, false),
            (41, 2, false),
            (29, 2, false),
            (36, 2, true),
            (0, 2, false),
            (38, 2, false),
            (40, 2, false),
        ],
        &[
            (33, 2, true),
            (0, 1, false),
            (33, 1, false),
            (45, 2, false),
            (33, 2, false),
            (48, 2, true),
            (47, 2, false),
            (45, 2, false),
            (43, 2, false),
        ],
        &[
            (41, 2, true),
            (41, 2, false),
            (40, 2, false),
            (40, 2, true),
            (38, 2, false),
            (36, 2, false),
            (35, 2, false),
            (33, 2, true),
        ],
    ];
    let mut step = 0;
    for bar in bars {
        for &(key, length, accent) in bar {
            if key > 0 {
                let at = step as f64 * sixteenth;
                // Every other long note runs into the next: a slide.
                let held = if sliding && length >= 2 && step % 4 == 2 {
                    length as f64 * sixteenth + 0.03
                } else {
                    length as f64 * sixteenth * 0.8
                };
                score.note(at, key, held, if accent { 1.0 } else { 0.8 }, accent);
            }
            step += length;
        }
    }
    score.end += 0.6;
    score
}

/// A lead melody in A minor at 120 a minute: a long note to hear the
/// vibrato, runs, and for the mono and legato presets a few slurs.
fn melody(sliding: bool) -> Score {
    let mut score = Score::new(120.0);
    let beat = score.beat();
    // Key, start and length in beats.
    let notes: [(u8, f64, f64); 15] = [
        (69, 0.0, 1.5),
        (72, 1.5, 0.5),
        (76, 2.0, 2.0),
        (74, 4.0, 0.5),
        (72, 4.5, 0.5),
        (71, 5.0, 0.5),
        (72, 5.5, 0.5),
        (74, 6.0, 1.0),
        (67, 7.0, 1.0),
        (69, 8.0, 0.75),
        (72, 8.75, 0.75),
        (79, 9.5, 0.5),
        (77, 10.0, 1.0),
        (76, 11.0, 1.0),
        (69, 12.0, 3.0),
    ];
    for (index, &(key, start, length)) in notes.iter().enumerate() {
        let overlap = sliding && matches!(index, 1 | 4 | 5 | 10 | 11);
        let held = if overlap {
            length * beat + 0.05
        } else {
            length * beat * 0.92
        };
        score.note(start * beat, key, held, 0.85, false);
    }
    score.end += 1.5;
    score
}

/// Pad chords at 120 a minute: A minor, F, C and G, a bar each, then the
/// last chord let ring.
fn pad_chords() -> Score {
    let mut score = Score::new(120.0);
    let bar = 4.0 * score.beat();
    let chords: [&[u8]; 4] = [
        &[57, 60, 64, 69],
        &[53, 57, 60, 65],
        &[55, 60, 64, 67],
        &[55, 59, 62, 67],
    ];
    for (index, chord) in chords.iter().enumerate() {
        for &key in *chord {
            score.note(index as f64 * bar, key, bar * 0.98, 0.7, false);
        }
        score.note(index as f64 * bar, chord[0] - 12, bar * 0.98, 0.6, false);
    }
    score.end += 3.0;
    score
}

/// A pluck arpeggio at 124 a minute: sixteenths up and down A minor, F, C
/// and G, a bar each.
fn arpeggio() -> Score {
    let mut score = Score::new(124.0);
    let sixteenth = score.beat() / 4.0;
    let chords: [[u8; 4]; 4] = [
        [57, 60, 64, 69],
        [53, 57, 60, 65],
        [55, 60, 64, 67],
        [55, 59, 62, 67],
    ];
    let order = [0, 1, 2, 3, 2, 1, 2, 3, 0, 2, 3, 1, 2, 3, 2, 1];
    for (bar, chord) in chords.iter().enumerate() {
        for (step, &index) in order.iter().enumerate() {
            let at = (bar * 16 + step) as f64 * sixteenth;
            let key = chord[index] + if step >= 8 { 12 } else { 0 };
            let velocity = if step % 4 == 0 { 0.95 } else { 0.7 };
            score.note(at, key, sixteenth * 0.9, velocity, false);
        }
    }
    score.end += 1.5;
    score
}

/// Keyboard comping at 100 a minute: chords on the beat and off it, a
/// melody note on top, A minor, D minor, G and C.
fn comping() -> Score {
    let mut score = Score::new(100.0);
    let beat = score.beat();
    let chords: [&[u8]; 4] = [&[57, 60, 64], &[57, 62, 65], &[55, 59, 62], &[55, 60, 64]];
    let rhythm = [(0.0, 0.9), (1.5, 0.4), (2.0, 0.4), (3.0, 0.9)];
    for (bar, chord) in chords.iter().enumerate() {
        let start = bar as f64 * 4.0 * beat;
        score.note(start, chord[0] - 12, 3.8 * beat, 0.7, false);
        for &(at, length) in &rhythm {
            for &key in *chord {
                score.note(start + at * beat, key, length * beat, 0.7, false);
            }
        }
        score.note(start + 2.5 * beat, chord[2] + 12, 1.4 * beat, 0.8, false);
    }
    score.end += 1.5;
    score
}

/// Brass at 112 a minute: off-beat stabs, then a chord that swells and is
/// held.
fn stabs() -> Score {
    let mut score = Score::new(112.0);
    let beat = score.beat();
    let chords: [&[u8]; 3] = [&[57, 60, 64], &[60, 64, 67], &[59, 62, 67]];
    for (bar, chord) in chords.iter().enumerate() {
        for hit in [0.5, 1.5, 2.5, 3.0] {
            for &key in *chord {
                score.note((bar as f64 * 4.0 + hit) * beat, key, 0.3 * beat, 0.9, false);
            }
        }
    }
    for key in [53, 57, 60, 65] {
        score.note(12.0 * beat, key, 4.0 * beat, 0.8, false);
    }
    score.end += 1.5;
    score
}

/// Effects at 120 a minute: one long note, then three short ones.
fn effects() -> Score {
    let mut score = Score::new(120.0);
    score.note(0.0, 60, 4.5, 0.9, false);
    for (index, key) in [72u8, 67, 60].into_iter().enumerate() {
        score.note(6.0 + index as f64 * 0.75, key, 0.4, 0.9, false);
    }
    score.end += 2.0;
    score
}

/// The phrase for a preset.
fn phrase(preset: &Preset) -> Score {
    let sliding = preset.params.mode != VoiceMode::Poly;
    match preset.category {
        Category::Bass => bass_line(sliding),
        Category::Lead => melody(sliding),
        Category::Pad => pad_chords(),
        Category::Pluck => arpeggio(),
        Category::Keys => comping(),
        Category::Brass => stabs(),
        Category::Fx => effects(),
    }
}

/// How long one voice holding one note of `params` takes to render, as a
/// share of the time it lasts at 48 kHz, best of three.
fn cost(params: &PolySynthParams) -> f64 {
    // The least of many short measurements: on a machine doing other work
    // as well, the least is the one least disturbed.
    let mut synth = PolySynthVoice::synth::<f32>(params, 1);
    synth.prepare(&SPEC);
    synth.note_on(NoteRef::new(0, 48), 0.8);
    let mut block = AudioBuffer::<f32>::new(2, 256);
    let blocks = 40;
    let seconds = (blocks * 256) as f64 / f64::from(RATE);
    let mut best = f64::INFINITY;
    for _ in 0..60 {
        let start = Instant::now();
        for _ in 0..blocks {
            synth.process(&mut block);
        }
        best = best.min(start.elapsed().as_secs_f64() / seconds);
        std::hint::black_box(block.peak());
    }
    best
}

/// The heaviest patch: three sawtooth oscillators of sixteen unison voices
/// each, through two driven filters with every LFO and envelope modulating.
fn heaviest() -> PolySynthParams {
    let stack = OscParams::new(Wave::Saw, 0.7).unison(16, 0.5, 1.0);
    let mut params = PolySynthParams {
        oscillators: [
            stack,
            OscParams::table(TableShape::Spectral, 0.4, 0.6).unison(16, 0.4, 1.0),
            stack.tuned(12.0, 0.0),
        ],
        filters: [
            FilterParams::new(FilterModel::LadderLp24, 2_000.0, 0.5).driven(6.0),
            FilterParams::new(FilterModel::Ms20Lp, 5_000.0, 0.3).driven(6.0),
        ],
        ..PolySynthParams::INIT
    };
    params.envelopes[1] = EnvParams::adsr(0.01, 1.0, 0.5, 0.5);
    params.saturation = 0.3;
    params.matrix = matrix(&[
        ModSlot::new(ModSource::Lfo1, ModDestination::Detune, 0.2),
        ModSlot::new(ModSource::Lfo2, ModDestination::Cutoff, 1.0),
        ModSlot::new(ModSource::Env3, ModDestination::Osc2Shape, 0.5),
    ]);
    params
}

/// Prints, for each preset, the level that puts the peak of one note (a
/// second long, at velocity 0.8, on a key suited to its category) at
/// -9 dBFS.
fn print_levels() {
    for preset in PRESETS.iter() {
        let key = match preset.category {
            Category::Bass => 36,
            Category::Lead | Category::Pluck => 69,
            _ => 60,
        };
        let mut score = Score::new(120.0);
        score.note(0.0, key, 1.2, 0.8, false);
        let mut synth = PolySynthVoice::synth::<f32>(&preset.params, 2);
        synth.prepare(&SPEC);
        let audio = render(&mut synth, &score, 2.0);
        let peak = f64::from(audio.peak()).max(1e-9);
        let level = preset.params.level * 10f64.powf(-9.0 / 20.0) / peak;
        println!(
            "{:24} level {:.2}, now {:.2}",
            preset.name, level, preset.params.level
        );
    }
}

/// Prints what one held voice costs at 48 kHz: the init patch (a saw
/// through the ladder), a 16-voice saw stack and a 16-voice wavetable stack
/// with no filter, and the heaviest patch.
fn print_cost() {
    let unfiltered = |osc: OscParams| PolySynthParams {
        oscillators: [osc, OscParams::OFF, OscParams::OFF],
        filters: [FilterParams::OFF, FilterParams::OFF],
        ..PolySynthParams::INIT
    };
    let saws = unfiltered(OscParams::new(Wave::Saw, 1.0).unison(16, 0.5, 1.0));
    let tables = unfiltered(OscParams::table(TableShape::Spectral, 0.4, 1.0).unison(16, 0.5, 1.0));
    for (name, params) in [
        ("the init patch", PolySynthParams::INIT),
        ("16 unison saws", saws),
        ("16 unison wavetables", tables),
        (
            "three 16-voice unison oscillators and two driven filters",
            heaviest(),
        ),
    ] {
        println!(
            "one voice of {name}: {:.1}% of a core",
            100.0 * cost(&params)
        );
    }
}

fn main() -> io::Result<()> {
    let argument = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    match argument.as_str() {
        "--levels" => {
            print_levels();
            return Ok(());
        }
        "--cost" => {
            print_cost();
            return Ok(());
        }
        _ => {}
    }
    let directory = Path::new(&argument);
    fs::create_dir_all(directory)?;
    print_cost();

    let mut heaviest_preset = (0.0, "");
    for preset in PRESETS.iter() {
        let score = phrase(preset);
        let mut synth = PolySynthVoice::synth::<f32>(&preset.params, 8);
        synth.prepare(&SPEC);
        synth
            .voices_mut()
            .for_each(|voice| voice.set_tempo(score.tempo));
        let seconds = score.end;
        let start = Instant::now();
        let mut audio = render(&mut synth, &score, seconds);
        let took = start.elapsed().as_secs_f64() / seconds;
        let peak = f64::from(audio.peak());
        if peak > 0.0 {
            let gain = 10f64.powf(PEAK_DBFS / 20.0) / peak;
            audio.apply_gain(gain as f32);
        }
        let file = file_name(preset);
        write_wav(&directory.join(&file), &audio)?;
        let per_voice = cost(&preset.params);
        if per_voice > heaviest_preset.0 {
            heaviest_preset = (per_voice, preset.name);
        }
        println!(
            "{file:40} made a peak of {:6.1} dBFS; the phrase took {:5.1}% of its time, a held \
             voice {:5.2}%",
            20.0 * peak.log10(),
            100.0 * took,
            100.0 * per_voice
        );
    }
    println!(
        "the costliest preset per voice: {} at {:.2}% of a core",
        heaviest_preset.1,
        100.0 * heaviest_preset.0
    );
    Ok(())
}
