//! Renders every drum kit of `manguebyte_dsp::instruments` to 16-bit,
//! 48 kHz stereo WAV files, to judge them by ear: a tour of each kit (every
//! piece struck twice, plainly and then accented) and short grooves in the
//! genres the kits are made for. Then it measures how much processor each
//! kind of piece costs.
//!
//! ```text
//! cargo run --release -p manguebyte-dsp --example drum_kit_wavs -- <directory>
//! ```
//!
//! Nothing is normalised: each file is at the level the kit makes, times a
//! fixed gain written below for each groove, so that the grooves peak near
//! -3 dBFS. The acoustic pop-rock groove goes through a small room (the
//! crate's [`Reverb`]), as drums are recorded; everything else is dry. The
//! peak and RMS level of each file are printed.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::instruments::{
    AnalogDrum, DrumKit, DrumKitVoice, DrumModel, KickBassParams, KickBassVoice, Piece, gm,
};
use manguebyte_dsp::{ProcessSpec, Processor, Reverb, Synth, Voice};

const RATE: u32 = 48_000;

/// The gain of every tour: 2 dB, so that the accented strikes of the
/// loudest pieces peak near -3 dBFS.
const TOUR_GAIN: f32 = 1.26;

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

    /// A drum strike: a short note.
    fn hit(&mut self, at: f64, key: u8, velocity: f32, accent: bool) {
        self.note(at, key, 0.03, velocity, accent);
    }

    /// The acts in time order, in frames; at the same frame, releases
    /// first.
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
                } => synth.note_on_accented(NoteRef::new(9, key), velocity, accent),
                Act::Off(key) => synth.note_off(NoteRef::new(9, key), 0.0),
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

/// A kit of `voices` voices, prepared.
fn kit(kit: DrumKit, voices: usize) -> Synth<DrumKitVoice> {
    let mut synth = Synth::new(DrumKitVoice::voices(voices, kit));
    synth.prepare(&SPEC);
    synth
}

/// `audio` times `gain`.
fn scaled(mut audio: AudioBuffer<f32>, gain: f32) -> AudioBuffer<f32> {
    for channel in audio.iter_channels_mut() {
        for sample in channel {
            *sample *= gain;
        }
    }
    audio
}

/// The sum of two renders of the same length.
fn mixed(mut audio: AudioBuffer<f32>, other: &AudioBuffer<f32>) -> AudioBuffer<f32> {
    for channel in 0..2 {
        for (sample, &add) in audio
            .channel_mut(channel)
            .iter_mut()
            .zip(other.channel(channel))
        {
            *sample += add;
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
    let peak = audio.peak();
    let rms = (squares / (2 * frames.max(1)) as f64).sqrt();
    println!(
        "{file:36} {:5.1} s, peak {:6.1} dBFS, rms {:6.1} dBFS",
        frames as f64 / f64::from(RATE),
        20.0 * f64::from(peak).log10(),
        20.0 * rms.log10()
    );
    Ok(())
}

/// How long a piece is left to ring in a tour, in seconds.
fn ring(model: DrumModel) -> f64 {
    use DrumModel as M;
    match model {
        M::Crash | M::Ride | M::RideBell | M::China => 2.6,
        M::Splash | M::OpenHat | M::Crash909 | M::Ride909 | M::Cymbal606 | M::CymbalCr78 => 1.4,
        M::Analog(AnalogDrum::Cymbal) => 1.4,
        M::Kick | M::Tom | M::OpenHat909 | M::Kick909 => 0.9,
        _ => 0.6,
    }
}

/// Every piece of `kit` whose key `keep` accepts, struck twice: at 0.85,
/// then accented. Keys that play the same piece as an earlier key are
/// skipped.
fn tour(kit: &DrumKit, keep: impl Fn(u8) -> bool) -> (Score, f64) {
    let mut score = Score::default();
    let mut at = 0.2;
    let mut heard = Vec::new();
    for key in kit.keys().filter(|&key| keep(key)) {
        let piece = *kit.piece(key).expect("a mapped key");
        if heard.contains(&piece) {
            continue;
        }
        heard.push(piece);
        let gap = ring(piece.model);
        score.hit(at, key, 0.85, false);
        score.hit(at + gap, key, 0.85, true);
        at += 2.0 * gap;
    }
    (score, at + 0.5)
}

/// A drum line written as Aratu writes them: one character a step, `.` a
/// rest, a lower-case letter a stroke, an upper-case one an accent, `g` a
/// ghost note (soft). `steps` steps to a beat, `swing` the share of a pair
/// of steps the first takes (0.5 is straight).
struct Line<'a> {
    key: u8,
    pattern: &'a str,
}

fn groove(
    lines: &[Line<'_>],
    tempo: f64,
    steps: usize,
    swing: f64,
    velocity: f32,
    bars: usize,
) -> (Score, f64) {
    let step = 60.0 / tempo / steps as f64;
    let mut score = Score::default();
    for line in lines {
        let length = line.pattern.chars().count();
        assert_eq!(
            (bars * 4 * steps) % length,
            0,
            "a line of {length} steps in {bars} bars"
        );
        let repeats = (bars * 4 * steps) / length;
        for round in 0..repeats {
            for (index, symbol) in line.pattern.chars().enumerate() {
                if symbol == '.' {
                    continue;
                }
                let n = round * length + index;
                if n >= bars * 4 * steps {
                    break;
                }
                let late = if n % 2 == 1 {
                    (swing - 0.5) * 2.0 * step
                } else {
                    0.0
                };
                let (velocity, accent) = match symbol {
                    'g' => (0.35, false),
                    'o' => (velocity * 0.75, false),
                    c if c.is_ascii_uppercase() => (velocity, true),
                    _ => (velocity, false),
                };
                score.hit(0.1 + n as f64 * step + late, line.key, velocity, accent);
            }
        }
    }
    (score, 0.1 + (bars * 4 * steps) as f64 * step)
}

/// A roll: `count` strokes from `at`, `gap` apart, rising in velocity.
fn roll(score: &mut Score, key: u8, at: f64, gap: f64, count: usize, from: f32, to: f32) {
    for k in 0..count {
        let velocity = from + (to - from) * k as f32 / (count - 1).max(1) as f32;
        score.hit(at + k as f64 * gap, key, velocity, false);
    }
}

/// Trap at 140 a minute, in half time: a punchy kick, a crisp clap and
/// snare on the third beat, eighth-note hi-hats with rolls in sixteenths,
/// thirty-seconds and triplets, an open hat, and the long 808 sliding
/// underneath on a kick-bass voice.
fn trap() -> AudioBuffer<f32> {
    let tempo = 140.0;
    let beat = 60.0 / tempo;
    let lines = [
        Line {
            key: gm::ACOUSTIC_BASS_DRUM,
            pattern: "K.........k.....K......k..k.....",
        },
        Line {
            key: gm::HAND_CLAP,
            pattern: "........C...............C.......",
        },
        Line {
            key: gm::SNARE,
            pattern: "........s...............s......g",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "h.o.h.o.h.o.....h.o.h.o.h.o.....",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "......o.........................",
        },
        Line {
            key: gm::SIDE_STICK,
            pattern: "...r......r.......r..r....r.....",
        },
    ];
    let (mut score, seconds) = groove(&lines, tempo, 4, 0.5, 0.9, 4);
    // Rolls where the eighths stop: thirty-seconds, then triplets, then a
    // sixteenth-note triplet climb.
    for bar in 0..4 {
        let start = 0.1 + (bar * 4) as f64 * beat;
        match bar % 2 {
            0 => roll(
                &mut score,
                gm::CLOSED_HI_HAT,
                start + 3.0 * beat,
                beat / 8.0,
                8,
                0.45,
                0.9,
            ),
            _ => {
                roll(
                    &mut score,
                    gm::CLOSED_HI_HAT,
                    start + 3.0 * beat,
                    beat / 6.0,
                    6,
                    0.5,
                    0.8,
                );
                roll(
                    &mut score,
                    gm::CLOSED_HI_HAT,
                    start + 3.5 * beat,
                    beat / 12.0,
                    6,
                    0.4,
                    0.95,
                );
            }
        }
    }
    let mut drums = kit(DrumKit::trap(), 24);
    let drums = render(&mut drums, &score, seconds + 1.5);
    // The 808: notes that overlap the next slide into it.
    let mut bass = Synth::with_voices(1, || {
        KickBassVoice::new(KickBassParams {
            decay: 1.8,
            sweep: 9.0,
            drive: 0.45,
            level: 0.42,
            ..KickBassParams::default()
        })
    });
    bass.set_legato(true);
    bass.prepare(&SPEC);
    let mut line = Score::default();
    let notes = [
        (0.0, 33, 2.2, true),
        (2.5, 33, 0.5, false),
        (3.0, 36, 1.0, false),
        (4.0, 33, 1.75, true),
        (5.5, 40, 0.5, false),
        (6.0, 38, 2.0, false),
        (8.0, 33, 2.2, true),
        (10.5, 33, 0.5, false),
        (11.0, 45, 0.6, false),
        (11.5, 33, 2.0, false),
        (13.5, 31, 1.2, false),
        (14.5, 28, 1.5, true),
    ];
    for &(at, key, length, accent) in &notes {
        line.note(0.1 + at * beat, key, length * beat, 1.0, accent);
    }
    let bass = render(&mut bass, &line, seconds + 1.5);
    scaled(mixed(drums, &bass), 0.6)
}

/// Techno at 130: the 909 kick with its rumble on every beat, a punchy
/// clap on two and four, the open hat on the off-beats, sixteenths of
/// closed hat, the ride from the third bar, and a rimshot figure.
fn techno() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "K...K...K...K...",
        },
        Line {
            key: gm::HAND_CLAP,
            pattern: "....C.......C...",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "..o...o...o...o.",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "gh.hgh.hgh.hgh.H",
        },
        Line {
            key: gm::SIDE_STICK,
            pattern: "...r..r....r..r.",
        },
        Line {
            key: gm::RIDE,
            pattern: "................................r.r.r.r.r.r.r.r.r.r.r.r.r.r.r.r.",
        },
    ];
    let (score, seconds) = groove(&lines, 130.0, 4, 0.5, 0.85, 4);
    let mut drums = kit(DrumKit::techno(), 20);
    scaled(render(&mut drums, &score, seconds + 1.5), 1.33)
}

/// House at 124 with a 58 % swing: four on the floor, a clap on two and
/// four, swung hi-hats and shaker, the open hat on the off-beat, a
/// tambourine and congas.
fn house() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "K...K...K...K...",
        },
        Line {
            key: gm::HAND_CLAP,
            pattern: "....C.......C...",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "..h...h...h...h.",
        },
        Line {
            key: gm::PEDAL_HI_HAT,
            pattern: ".g.g.g.g.g.g.g.g",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "..............o.",
        },
        Line {
            key: gm::MARACAS,
            pattern: "gsgsgsgsgsgsgsgs",
        },
        Line {
            key: gm::TAMBOURINE,
            pattern: "....t.......t...",
        },
        Line {
            key: gm::MUTE_HIGH_CONGA,
            pattern: "...c..c.......c.",
        },
        Line {
            key: gm::LOW_CONGA,
            pattern: "..........c..c..",
        },
    ];
    let (score, seconds) = groove(&lines, 124.0, 4, 0.58, 0.85, 4);
    let mut drums = kit(DrumKit::house(), 20);
    scaled(render(&mut drums, &score, seconds + 1.5), 1.29)
}

/// Drum and bass at 174: the two-step, the deep kick on one and the "and"
/// of two, the tight acoustic snare on two and four with ghost notes,
/// fast hi-hats, and a ride bell.
fn drum_and_bass() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "K.........k.....K.........k..k..",
        },
        Line {
            key: gm::ACOUSTIC_BASS_DRUM,
            pattern: "k.........k.....k.........k..k..",
        },
        Line {
            key: gm::SNARE,
            pattern: "....S..g.g..S.g.....S..g.g..S..g",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "hghghghghghghghghghghghghghghgo.",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "..............................o.",
        },
        Line {
            key: gm::RIDE_BELL,
            pattern: "..b.......b.......b.......b.....",
        },
    ];
    let (mut score, seconds) = groove(&lines, 174.0, 4, 0.5, 0.9, 4);
    // A crash to start.
    score.hit(0.1, gm::CRASH, 0.9, true);
    let mut drums = kit(DrumKit::drum_and_bass(), 24);
    scaled(render(&mut drums, &score, seconds + 2.0), 0.61)
}

/// Lo-fi at 85 with a lazy 62 % swing: a soft kick, the snare on two and
/// four, side-stick ghosts, swung dark hi-hats and a shaker, through the
/// twelve-bit sampler.
fn lo_fi() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "k.......k.k.....k.........k..k..",
        },
        Line {
            key: gm::SNARE,
            pattern: "....s.......s.......s.......s...",
        },
        Line {
            key: gm::SIDE_STICK,
            pattern: "...........g..........g.......g.",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "h.o.h.o.h.o.h.o.h.o.h.o.h.o.h...",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "..............................o.",
        },
        Line {
            key: gm::MARACAS,
            pattern: ".g.g.g.g.g.g.g.g",
        },
    ];
    let (score, seconds) = groove(&lines, 85.0, 4, 0.62, 0.8, 4);
    let mut drums = kit(DrumKit::lo_fi(), 16);
    scaled(render(&mut drums, &score, seconds + 1.5), 2.2)
}

/// An acoustic pop-rock groove at 104: two bars on the hi-hat, a bar on
/// the ride with the bell and the pedal hat on two and four, a fill down
/// the toms, and a crash on the last downbeat; through a small room.
fn pop_rock() -> AudioBuffer<f32> {
    let tempo = 104.0;
    let beat = 60.0 / tempo;
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: concat!(
                "K.....k.K.k.....",
                "K.....k.K.k.....",
                "K.....k.K.k.....",
                "K.....k........."
            ),
        },
        Line {
            key: gm::SNARE,
            pattern: concat!(
                "....S.......S...",
                "....S.....g.S...",
                "....S.......S...",
                "....S..........."
            ),
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: concat!(
                "H.o.H.o.H.o.H.o.",
                "H.o.H.o.H.o.H.oo",
                "................",
                "................"
            ),
        },
        Line {
            key: gm::PEDAL_HI_HAT,
            pattern: concat!(
                "................",
                "................",
                "....o.......o...",
                "....o..........."
            ),
        },
        Line {
            key: gm::RIDE,
            pattern: concat!(
                "................",
                "................",
                "r.o.r.o.r.o.r.o.",
                "r.o.r..........."
            ),
        },
        Line {
            key: gm::RIDE_BELL,
            pattern: concat!(
                "................",
                "................",
                "b.......b.......",
                "................"
            ),
        },
    ];
    let (mut score, _) = groove(&lines, tempo, 4, 0.5, 0.85, 4);
    // The crash at the start, the fill in the last bar, the crash after.
    score.hit(0.1, gm::CRASH, 0.85, true);
    let fill = 0.1 + 12.0 * beat + 2.0 * beat;
    let toms = [
        gm::HIGH_TOM,
        gm::HIGH_TOM,
        gm::HIGH_MID_TOM,
        gm::HIGH_MID_TOM,
        gm::LOW_TOM,
        gm::LOW_TOM,
        gm::HIGH_FLOOR_TOM,
        gm::LOW_FLOOR_TOM,
    ];
    for (k, &key) in toms.iter().enumerate() {
        score.hit(
            fill + k as f64 * beat / 4.0,
            key,
            0.7 + 0.03 * k as f32,
            false,
        );
    }
    score.hit(fill + 2.0 * beat, gm::CRASH_2, 0.85, false);
    score.hit(fill + 2.0 * beat, gm::BASS_DRUM, 0.95, true);
    let seconds = fill + 2.0 * beat + 3.5;
    let mut drums = kit(DrumKit::acoustic(), 24);
    let mut audio = render(&mut drums, &score, seconds);
    let mut room = Reverb::<f32>::new()
        .with_decay_seconds(0.7)
        .with_damping(0.5)
        .with_mix(0.18);
    room.set_size(0.25);
    room.prepare(&SPEC);
    let frames = audio.num_frames();
    let mut start = 0;
    while start < frames {
        let end = (start + 256).min(frames);
        audio.windowed(start..end, |block| room.process(block));
        start = end;
    }
    scaled(audio, 1.0)
}

/// The 606 at 125, as an acid-house rhythm section: the kick on every
/// beat, the snare on two and four with accents, sixteenths of closed hat
/// with the open hat on the off-beats, a tom figure and the cymbal.
fn style_606() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "K...K...K...K..k",
        },
        Line {
            key: gm::SNARE,
            pattern: "....S.......S.s.",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "hhHhhh.hhhHhhh.h",
        },
        Line {
            key: gm::OPEN_HI_HAT,
            pattern: "......o.......o.",
        },
        Line {
            key: gm::LOW_TOM,
            pattern: concat!(
                "................",
                "................",
                "................",
                "........t..t..t."
            ),
        },
        Line {
            key: gm::HIGH_TOM,
            pattern: concat!(
                "................",
                "................",
                "................",
                "....t..t..t....."
            ),
        },
        Line {
            key: gm::CRASH,
            pattern: concat!(
                "c...............",
                "................",
                "................",
                "................"
            ),
        },
    ];
    let (score, seconds) = groove(&lines, 125.0, 4, 0.5, 0.85, 4);
    let mut drums = kit(DrumKit::style_606(), 16);
    scaled(render(&mut drums, &score, seconds + 1.5), 0.72)
}

/// The CR-78 at 100, in the manner of its preset rhythms: the soft kick
/// and snare, eighths of hi-hat, the guiro, maracas, cowbell and a bongo
/// and conga figure.
fn style_cr78() -> AudioBuffer<f32> {
    let lines = [
        Line {
            key: gm::BASS_DRUM,
            pattern: "k.....k...k.....",
        },
        Line {
            key: gm::SNARE,
            pattern: "....s.......s...",
        },
        Line {
            key: gm::CLOSED_HI_HAT,
            pattern: "h.h.h.h.h.h.h.hH",
        },
        Line {
            key: gm::LONG_GUIRO,
            pattern: "g.......g.......",
        },
        Line {
            key: gm::SHORT_GUIRO,
            pattern: "....g.......g...",
        },
        Line {
            key: gm::MARACAS,
            pattern: ".m.m.m.m.m.m.m.m",
        },
        Line {
            key: gm::HIGH_BONGO,
            pattern: "..b..b.....b..b.",
        },
        Line {
            key: gm::LOW_CONGA,
            pattern: "..........c..c..",
        },
        Line {
            key: gm::COWBELL,
            pattern: "c.........................c.....",
        },
    ];
    let (score, seconds) = groove(&lines, 100.0, 4, 0.5, 0.85, 4);
    let mut drums = kit(DrumKit::style_cr78(), 16);
    scaled(render(&mut drums, &score, seconds + 1.5), 1.5)
}

/// The least of five measurements of [`cost_once`]: the others were
/// slowed by whatever else the machine was doing.
fn cost(model: DrumModel, seconds: f64) -> f64 {
    (0..5)
        .map(|_| cost_once(model, seconds))
        .fold(f64::INFINITY, f64::min)
}

/// Strikes `model` again and again on one voice for `seconds` of audio
/// and returns the processor time per second of audio, as a share of one
/// core.
fn cost_once(model: DrumModel, seconds: f64) -> f64 {
    let mut kit = DrumKit::empty();
    kit.set(gm::SNARE, Some(Piece::new(model)));
    let mut synth = Synth::new(DrumKitVoice::voices(1, kit));
    synth.prepare(&SPEC);
    let mut buffer = AudioBuffer::<f32>::new(2, 256);
    let blocks = (seconds * f64::from(RATE) / 256.0) as usize;
    // A strike every second, held: the piece rings for the whole run.
    let every = (f64::from(RATE) / 256.0) as usize;
    let started = Instant::now();
    for block in 0..blocks {
        if block % every == 0 {
            synth.note_on(NoteRef::new(9, gm::SNARE), 0.9);
        }
        synth.process(&mut buffer);
    }
    started.elapsed().as_secs_f64() / seconds
}

fn main() -> io::Result<()> {
    let directory = std::env::args()
        .nth(1)
        .expect("usage: drum_kit_wavs <directory>");
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;

    // Tours: every piece, plainly and accented. The acoustic kit in two:
    // the drums and percussion, then the cymbals.
    let acoustic = DrumKit::acoustic();
    let cymbal = |key: u8| {
        matches!(
            acoustic.piece(key).map(|p| p.model),
            Some(
                DrumModel::ClosedHat
                    | DrumModel::PedalHat
                    | DrumModel::OpenHat
                    | DrumModel::Crash
                    | DrumModel::Ride
                    | DrumModel::RideBell
                    | DrumModel::China
                    | DrumModel::Splash
            )
        )
    };
    let (score, seconds) = tour(&acoustic, |key| !cymbal(key));
    write_wav(
        directory,
        "acoustic_kit_drums_tour",
        &scaled(render(&mut kit(acoustic, 8), &score, seconds), TOUR_GAIN),
    )?;
    let (score, seconds) = tour(&acoustic, cymbal);
    write_wav(
        directory,
        "acoustic_kit_cymbals_tour",
        &scaled(render(&mut kit(acoustic, 8), &score, seconds), TOUR_GAIN),
    )?;
    for (name, preset) in [
        ("808_tour", DrumKit::style_808()),
        ("909_tour", DrumKit::style_909()),
        ("606_tour", DrumKit::style_606()),
        ("cr78_tour", DrumKit::style_cr78()),
        ("trap_kit_tour", DrumKit::trap()),
        ("techno_kit_tour", DrumKit::techno()),
        ("house_kit_tour", DrumKit::house()),
        ("drum_and_bass_kit_tour", DrumKit::drum_and_bass()),
        ("lo_fi_kit_tour", DrumKit::lo_fi()),
    ] {
        let (score, seconds) = tour(&preset, |_| true);
        write_wav(
            directory,
            name,
            &scaled(render(&mut kit(preset, 8), &score, seconds), TOUR_GAIN),
        )?;
    }

    // Grooves.
    write_wav(directory, "acoustic_pop_rock_groove", &pop_rock())?;
    write_wav(directory, "trap_groove", &trap())?;
    write_wav(directory, "techno_groove", &techno())?;
    write_wav(directory, "house_groove", &house())?;
    write_wav(directory, "drum_and_bass_groove", &drum_and_bass())?;
    write_wav(directory, "lo_fi_groove", &lo_fi())?;
    write_wav(directory, "606_groove", &style_606())?;
    write_wav(directory, "cr78_groove", &style_cr78())?;

    // What each kind of piece costs, ringing the whole time.
    println!();
    println!("processor time per voice at 48 kHz, as a share of one core:");
    for model in [
        DrumModel::Crash,
        DrumModel::Ride,
        DrumModel::China,
        DrumModel::OpenHat,
        DrumModel::ClosedHat,
        DrumModel::Snare,
        DrumModel::Tom,
        DrumModel::Kick,
        DrumModel::Kick909,
        DrumModel::OpenHat909,
        DrumModel::Clap909,
        DrumModel::TambourineCr78,
        DrumModel::Analog(AnalogDrum::Cymbal),
    ] {
        println!(
            "  {:32} {:6.3} %",
            format!("{model:?}"),
            100.0 * cost(model, 4.0)
        );
    }
    Ok(())
}
