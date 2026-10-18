//! Renders the Brazilian percussion of `DrumKit::brazilian` to 16-bit,
//! 48 kHz stereo WAV files, to judge it by ear: a tour that plays every
//! stroke alone (soft, then medium, then accented), and grooves of
//! maracatu, samba, baião and coco.
//!
//! ```text
//! cargo run --release -p manguebyte-dsp --example brazilian_percussion_wavs -- <directory>
//! ```
//!
//! The grooves add the acoustic kit's snare where the style has one (the
//! caixa of maracatu and samba) and go through a small room (the crate's
//! [`Reverb`]); every file is brought to a peak of -3 dBFS. The strokes are
//! placed a few milliseconds off the grid and at slightly different
//! velocities, from a fixed seed, as players place them.

use std::fs;
use std::io;
use std::path::Path;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::instruments::{DrumKit, DrumKitVoice, DrumModel, Piece, gm, percussion};
use manguebyte_dsp::{ProcessSpec, Processor, Reverb, Synth};

const RATE: u32 = 48_000;

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: RATE as f64,
    max_block_size: 256,
    num_channels: 2,
};

/// The peak every file is brought to, in dBFS.
const PEAK_DBFS: f32 = -3.0;

/// Free keys for the extra drums of a section: a second and a third surdo,
/// tuned up, and a second alfaia.
const SURDO_2_OPEN: u8 = 83;
const SURDO_2_MUTED: u8 = 84;
const SURDO_3: u8 = 85;
const ALFAIA_2_OPEN: u8 = 76;
const ALFAIA_2_MUTED: u8 = 77;

/// A stroke: when, on which key, how hard, accented or not.
#[derive(Debug, Clone, Copy)]
struct Stroke {
    at: f64,
    key: u8,
    velocity: f32,
    accent: bool,
}

/// A repeatable source of small deviations.
struct Feel(u64);

impl Feel {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// One line of a groove: a pattern of one or more bars in sixteenth notes,
/// each symbol a stroke (`.` a rest), and what each symbol plays: its key,
/// its velocity and whether it is accented.
struct Line {
    pattern: &'static str,
    play: fn(char) -> Option<(u8, f32, bool)>,
}

/// The strokes of `lines` over `bars` bars of four beats at `tempo`, each
/// sixteenth `swing` of a pair long (0.5 is straight), placed by `feel`.
fn groove(lines: &[Line], tempo: f64, swing: f64, bars: usize, feel: &mut Feel) -> Vec<Stroke> {
    let step = 60.0 / tempo / 4.0;
    let steps = bars * 16;
    let mut strokes = Vec::new();
    for line in lines {
        let symbols: Vec<char> = line.pattern.chars().filter(|c| *c != ' ').collect();
        for n in 0..steps {
            let Some((key, velocity, accent)) = (line.play)(symbols[n % symbols.len()]) else {
                continue;
            };
            let late = if n % 2 == 1 {
                (swing - 0.5) * 2.0 * step
            } else {
                0.0
            };
            let at = 0.1 + n as f64 * step + late + 0.003 * feel.next();
            let velocity = (velocity + 0.05 * feel.next() as f32).clamp(0.05, 1.0);
            strokes.push(Stroke {
                at: at.max(0.0),
                key,
                velocity,
                accent,
            });
        }
    }
    strokes.sort_by(|a, b| a.at.total_cmp(&b.at));
    strokes
}

/// The kit of the grooves: the Brazilian percussion, the acoustic kit's
/// snare as the caixa, and the extra drums of a section.
fn section_kit() -> DrumKit {
    let mut kit = DrumKit::brazilian();
    let acoustic = DrumKit::acoustic();
    kit.set(gm::SNARE, acoustic.piece(gm::SNARE).copied());
    let piece = |key: u8| *DrumKit::brazilian().piece(key).unwrap();
    // The second surdo damps itself, not the first.
    let tuned = |key, tune: f64, pan: f64| Piece {
        tune,
        pan,
        choke: 1,
        ..piece(key)
    };
    kit.set(SURDO_2_OPEN, Some(tuned(percussion::OPEN_SURDO, 4.0, 0.3)));
    kit.set(SURDO_2_MUTED, Some(tuned(percussion::MUTE_SURDO, 4.0, 0.3)));
    kit.set(
        SURDO_3,
        Some(Piece {
            choke: 0,
            ..tuned(percussion::OPEN_SURDO, 7.0, -0.35)
        }),
    );
    kit.set(
        ALFAIA_2_OPEN,
        Some(Piece {
            choke: 8,
            ..tuned(percussion::ALFAIA_OPEN, 3.0, 0.35)
        }),
    );
    kit.set(
        ALFAIA_2_MUTED,
        Some(Piece {
            choke: 8,
            ..tuned(percussion::ALFAIA_MUTED, 3.0, 0.35)
        }),
    );
    kit
}

/// Plays `strokes` on `kit`, of 24 voices, for `seconds`.
fn render(kit: DrumKit, strokes: &[Stroke], seconds: f64) -> AudioBuffer<f32> {
    let mut synth = Synth::new(DrumKitVoice::voices(24, kit));
    synth.prepare(&SPEC);
    let total = (seconds * f64::from(RATE)) as usize;
    let mut out = AudioBuffer::<f32>::new(2, total);
    let mut block = AudioBuffer::<f32>::new(2, 256);
    let frames: Vec<usize> = strokes
        .iter()
        .map(|stroke| (stroke.at * f64::from(RATE)).round() as usize)
        .collect();
    let (mut frame, mut next) = (0, 0);
    while frame < total {
        while next < strokes.len() && frames[next] <= frame {
            let stroke = strokes[next];
            let note = NoteRef::new(9, stroke.key);
            synth.note_on_accented(note, stroke.velocity, stroke.accent);
            synth.note_off(note, 0.0);
            next += 1;
        }
        let until = frames.get(next).map_or(total, |&at| at.min(total));
        let end = (frame + 256).min(until).max(frame + 1).min(total);
        block.set_num_frames(end - frame);
        synth.process(&mut block);
        for channel in 0..2 {
            out.channel_mut(channel)[frame..end].copy_from_slice(block.channel(channel));
        }
        frame = end;
    }
    out
}

/// `audio` through a small room.
fn in_a_room(mut audio: AudioBuffer<f32>) -> AudioBuffer<f32> {
    let mut room = Reverb::<f32>::new()
        .with_decay_seconds(0.9)
        .with_damping(0.45)
        .with_mix(0.16);
    room.set_size(0.3);
    room.prepare(&SPEC);
    let frames = audio.num_frames();
    let mut start = 0;
    while start < frames {
        let end = (start + 256).min(frames);
        audio.windowed(start..end, |block| room.process(block));
        start = end;
    }
    audio
}

/// `audio` brought to a peak of [`PEAK_DBFS`].
fn normalised(mut audio: AudioBuffer<f32>) -> AudioBuffer<f32> {
    let gain = 10f32.powf(PEAK_DBFS / 20.0) / audio.peak().max(1e-9);
    for channel in audio.iter_channels_mut() {
        for sample in channel {
            *sample *= gain;
        }
    }
    audio
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
    let rms = (squares / (2 * frames.max(1)) as f64).sqrt();
    println!(
        "{file:28} {:5.1} s, peak {:6.1} dBFS, rms {:6.1} dBFS",
        frames as f64 / f64::from(RATE),
        20.0 * f64::from(audio.peak()).log10(),
        20.0 * rms.log10()
    );
    Ok(())
}

/// Every stroke alone, in the order of its key: softly, at medium and
/// accented, then left to ring.
fn tour() -> AudioBuffer<f32> {
    let kit = DrumKit::brazilian();
    let mut strokes = Vec::new();
    let mut at = 0.2;
    for key in percussion::ALL {
        let model = kit.piece(key).unwrap().model;
        println!("{at:6.2} s: {key} {model:?}");
        for (velocity, accent) in [(0.45, false), (0.75, false), (1.0, true)] {
            strokes.push(Stroke {
                at,
                key,
                velocity,
                accent,
            });
            at += 0.42;
        }
        let ring = match model {
            DrumModel::TriangleOpen => 3.0,
            DrumModel::GongueOpen
            | DrumModel::AgogoHigh
            | DrumModel::AgogoLow
            | DrumModel::SurdoOpen => 1.8,
            _ => 0.7,
        };
        at += ring;
    }
    normalised(render(kit, &strokes, at + 0.5))
}

/// Maracatu de baque virado at 92: the alfaias' marcação and a second
/// alfaia's answers, the gonguê's ostinato, the caixa's sixteenths and
/// the ganzá.
fn maracatu() -> AudioBuffer<f32> {
    use percussion::*;
    let lines = [
        Line {
            pattern: "O..mO.m.O..mO.m. O..mO.m.O..mOmmO",
            play: |c| match c {
                'O' => Some((ALFAIA_OPEN, 0.95, true)),
                'm' => Some((ALFAIA_MUTED, 0.6, false)),
                _ => None,
            },
        },
        Line {
            pattern: "..o.m...o.o.m.o. ..o.m...o.m.oooo",
            play: |c| match c {
                'o' => Some((ALFAIA_2_OPEN, 0.75, false)),
                'm' => Some((ALFAIA_2_MUTED, 0.55, false)),
                _ => None,
            },
        },
        Line {
            pattern: "O.m.o.m.O.m.o.mm",
            play: |c| match c {
                'O' => Some((GONGUE_OPEN, 0.9, true)),
                'o' => Some((GONGUE_OPEN, 0.7, false)),
                'm' => Some((GONGUE_MUTED, 0.65, false)),
                _ => None,
            },
        },
        Line {
            pattern: "XgxgXgxgXgxgXgxx",
            play: |c| match c {
                'X' => Some((gm::SNARE, 0.75, true)),
                'x' => Some((gm::SNARE, 0.5, false)),
                'g' => Some((gm::SNARE, 0.3, false)),
                _ => None,
            },
        },
        Line {
            pattern: "x.X.x.X.x.X.x.X.",
            play: |c| match c {
                'X' => Some((SHAKER, 0.8, true)),
                'x' => Some((SHAKER, 0.55, false)),
                _ => None,
            },
        },
    ];
    let strokes = groove(&lines, 92.0, 0.5, 8, &mut Feel(0x6d61_7261));
    let seconds = 0.1 + 8.0 * 4.0 * 60.0 / 92.0 + 2.5;
    normalised(in_a_room(render(section_kit(), &strokes, seconds)))
}

/// Samba at 100: the surdos of primeira, segunda and terceira, the
/// tamborim's telecoteco, the agogô, the pandeiro, the cuíca, the ganzá
/// and a soft caixa.
fn samba() -> AudioBuffer<f32> {
    use percussion::*;
    let lines = [
        Line {
            pattern: "m...O...m...O...",
            play: |c| match c {
                'O' => Some((OPEN_SURDO, 0.95, true)),
                'm' => Some((MUTE_SURDO, 0.55, false)),
                _ => None,
            },
        },
        Line {
            pattern: "O...m...O...m.m.",
            play: |c| match c {
                'O' => Some((SURDO_2_OPEN, 0.85, false)),
                'm' => Some((SURDO_2_MUTED, 0.5, false)),
                _ => None,
            },
        },
        Line {
            pattern: "..o..o.o..o...o. ..o..o.o.o.o..oo",
            play: |c| (c == 'o').then_some((SURDO_3, 0.6, false)),
        },
        Line {
            pattern: "X.x.xX.x.Xx.x.X. X.x.xX.x.Xx.xmX.",
            play: |c| match c {
                'X' => Some((TAMBORIM_OPEN, 0.85, true)),
                'x' => Some((TAMBORIM_OPEN, 0.6, false)),
                'm' => Some((TAMBORIM_MUTED, 0.6, false)),
                _ => None,
            },
        },
        Line {
            pattern: "h.l.h.ll.h.l.hl. h.l.h.ll.h.l.h.l",
            play: |c| match c {
                'h' => Some((HIGH_AGOGO, 0.7, false)),
                'l' => Some((LOW_AGOGO, 0.65, false)),
                _ => None,
            },
        },
        Line {
            pattern: "tfsftfsftfsftfsf tfsftfsftfsfmfsf",
            play: |c| match c {
                't' => Some((PANDEIRO_THUMB, 0.75, false)),
                'f' => Some((PANDEIRO_FINGERS, 0.45, false)),
                's' => Some((PANDEIRO_SLAP, 0.7, true)),
                'm' => Some((PANDEIRO_MUTED, 0.6, false)),
                _ => None,
            },
        },
        Line {
            pattern: "................ ......h...l.h... ................ ..l.h.....h.l.l.",
            play: |c| match c {
                'h' => Some((MUTE_CUICA, 0.75, false)),
                'l' => Some((OPEN_CUICA, 0.7, false)),
                _ => None,
            },
        },
        Line {
            pattern: "gXgxgXgxgXgxgXgx",
            play: |c| match c {
                'X' => Some((SHAKER, 0.75, true)),
                'x' => Some((SHAKER, 0.55, false)),
                'g' => Some((SHAKER, 0.35, false)),
                _ => None,
            },
        },
        Line {
            pattern: "g.xgg.xgg.xgg.xX",
            play: |c| match c {
                'X' => Some((gm::SNARE, 0.55, true)),
                'x' => Some((gm::SNARE, 0.4, false)),
                'g' => Some((gm::SNARE, 0.22, false)),
                _ => None,
            },
        },
    ];
    let strokes = groove(&lines, 100.0, 0.53, 8, &mut Feel(0x7361_6d62));
    let seconds = 0.1 + 8.0 * 4.0 * 60.0 / 100.0 + 2.5;
    normalised(in_a_room(render(section_kit(), &strokes, seconds)))
}

/// Baião at 104: the zabumba's mallet, open on the beat and on the
/// sixteenth before the next, muted after, its stick on the off-beats;
/// the triangle held and let ring; and the ganzá.
fn baiao() -> AudioBuffer<f32> {
    use percussion::*;
    let lines = [
        Line {
            pattern: "O..o..m.O..o..m. O..o..m.O..o.mm.",
            play: |c| match c {
                'O' => Some((ZABUMBA_OPEN, 0.95, true)),
                'o' => Some((ZABUMBA_OPEN, 0.75, false)),
                'm' => Some((ZABUMBA_MUTED, 0.65, false)),
                _ => None,
            },
        },
        Line {
            pattern: "..s...s.s.s...s. ..s...s.s.s.s.ss",
            play: |c| (c == 's').then_some((ZABUMBA_STICK, 0.6, false)),
        },
        Line {
            pattern: "mmOmmmOmmmOmmmOm",
            play: |c| match c {
                'O' => Some((OPEN_TRIANGLE, 0.8, true)),
                'm' => Some((MUTE_TRIANGLE, 0.55, false)),
                _ => None,
            },
        },
        Line {
            pattern: "x.X.x.X.x.X.x.X.",
            play: |c| match c {
                'X' => Some((SHAKER, 0.6, true)),
                'x' => Some((SHAKER, 0.4, false)),
                _ => None,
            },
        },
    ];
    let strokes = groove(&lines, 104.0, 0.52, 8, &mut Feel(0x6261_6961));
    let seconds = 0.1 + 8.0 * 4.0 * 60.0 / 104.0 + 2.5;
    normalised(in_a_room(render(section_kit(), &strokes, seconds)))
}

/// Coco de roda at 112: the zabumba and the pandeiro, the ganzá's
/// sixteenths, the triangle, and the hand claps of the ring of dancers on
/// the off-beats.
fn coco() -> AudioBuffer<f32> {
    use percussion::*;
    let lines = [
        Line {
            pattern: "O.....m.o.m...m. O.....m.o.m.o.m.",
            play: |c| match c {
                'O' => Some((ZABUMBA_OPEN, 0.9, true)),
                'o' => Some((ZABUMBA_OPEN, 0.7, false)),
                'm' => Some((ZABUMBA_MUTED, 0.6, false)),
                _ => None,
            },
        },
        Line {
            pattern: "tfsftfsftfsftfsf",
            play: |c| match c {
                't' => Some((PANDEIRO_THUMB, 0.75, false)),
                'f' => Some((PANDEIRO_FINGERS, 0.45, false)),
                's' => Some((PANDEIRO_SLAP, 0.65, true)),
                _ => None,
            },
        },
        Line {
            pattern: "XxgxXxgxXxgxXxgx",
            play: |c| match c {
                'X' => Some((SHAKER, 0.75, true)),
                'x' => Some((SHAKER, 0.5, false)),
                'g' => Some((SHAKER, 0.3, false)),
                _ => None,
            },
        },
        Line {
            pattern: "mmOmmmOmmmOmmmOm",
            play: |c| match c {
                'O' => Some((OPEN_TRIANGLE, 0.7, false)),
                'm' => Some((MUTE_TRIANGLE, 0.5, false)),
                _ => None,
            },
        },
        Line {
            pattern: "....x.......x... ....x.......x.x.",
            play: |c| (c == 'x').then_some((HAND_CLAP, 0.8, false)),
        },
    ];
    let strokes = groove(&lines, 112.0, 0.52, 8, &mut Feel(0x636f_636f));
    let seconds = 0.1 + 8.0 * 4.0 * 60.0 / 112.0 + 2.5;
    normalised(in_a_room(render(section_kit(), &strokes, seconds)))
}

fn main() -> io::Result<()> {
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "brazilian_percussion_wavs".to_owned());
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    write_wav(directory, "percussion_tour", &tour())?;
    write_wav(directory, "maracatu_groove", &maracatu())?;
    write_wav(directory, "samba_groove", &samba())?;
    write_wav(directory, "baiao_groove", &baiao())?;
    write_wav(directory, "coco_groove", &coco())?;
    Ok(())
}
