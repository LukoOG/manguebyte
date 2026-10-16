//! Helpers shared by the integration tests.

#![allow(dead_code)]

use manguebyte_web::{
    Chain, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, Pushed, SfzLoad,
};

pub const RATE: f64 = 48_000.0;

/// An engine at 48 kHz with the default limits.
pub fn engine() -> Engine {
    Engine::new(EngineConfig::new(RATE)).unwrap()
}

/// An engine with one part, `0`, of `kind` through `chain`.
pub fn engine_with(kind: InstrumentKind, chain: Chain) -> Engine {
    let mut engine = engine();
    engine.create_part_with(0, kind, chain).unwrap();
    engine
}

/// Renders `frames` frames in quanta of `quantum` and returns both channels.
pub fn render(engine: &mut Engine, frames: usize, quantum: usize) -> (Vec<f32>, Vec<f32>) {
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    while left.len() < frames {
        let count = engine.render(quantum.min(frames - left.len()));
        let (l, r) = engine.output();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
        assert_eq!(count, l.len());
    }
    (left, right)
}

/// Pushes every record, insisting each one is queued.
pub fn push_all(engine: &mut Engine, records: &[EventRecord]) {
    for &record in records {
        assert_eq!(engine.push(record), Pushed::Queued, "{record:?}");
    }
}

pub fn id(name: &str) -> ParamId {
    ParamId::from_name(name)
}

pub fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

pub fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

/// The first frame that is not exactly zero.
pub fn onset(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|&sample| sample != 0.0)
}

/// A 16-bit mono WAV file of `samples` at `rate`, the kind Aratu's
/// instruments are made of.
pub fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut buffer = manguebyte_core::AudioBuffer::<f32>::new(1, samples.len());
    buffer.channel_mut(0).copy_from_slice(samples);
    let mut bytes = Vec::new();
    manguebyte_formats::wav::write(
        &mut bytes,
        &buffer,
        rate,
        manguebyte_formats::wav::WavFormat::Int16,
    )
    .unwrap();
    bytes
}

/// `frames` frames of a sine at `frequency`, `amplitude` at its peak.
pub fn sine(frequency: f64, rate: f64, frames: usize, amplitude: f64) -> Vec<f32> {
    (0..frames)
        .map(|n| (amplitude * (core::f64::consts::TAU * frequency * n as f64 / rate).sin()) as f32)
        .collect()
}

/// `frames` frames of white noise, `amplitude` at most, that fades out.
pub fn noise(frames: usize, amplitude: f64, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..frames)
        .map(|n| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let white = (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
            (amplitude * white * (1.0 - n as f64 / frames as f64)) as f32
        })
        .collect()
}

/// Loads `sfz` with `files` through [`SfzLoad`], file by file, and gives
/// it to part `part`, which must be a sampler part.
pub fn load_sfz(engine: &mut Engine, part: u16, sfz: &str, files: &[(String, Vec<u8>)]) {
    let mut load = SfzLoad::new(sfz).unwrap_or_else(|error| panic!("{error}"));
    let wanted: Vec<String> = load.paths().map(str::to_owned).collect();
    for path in wanted {
        let (_, bytes) = files
            .iter()
            .find(|(name, _)| *name == path)
            .unwrap_or_else(|| panic!("no fixture {path}"));
        assert!(load.add_file(&path, bytes.clone()).unwrap(), "{path}");
    }
    let instrument = load.finish().unwrap_or_else(|error| panic!("{error}"));
    engine.load_instrument(part, instrument).unwrap();
}

/// A melodic instrument like Aratu's: an A4 sampled at 24 kHz for the keys
/// up to 74 and an A5 sampled at 32 kHz for those above, each looped
/// continuously over whole periods, with a short release.
pub fn tone_instrument() -> (String, Vec<(String, Vec<u8>)>) {
    // 440 Hz makes 11 periods in 600 frames at 24 kHz, and 880 Hz 11 in
    // 400 at 32 kHz: loops of whole numbers of periods meet themselves.
    let low = wav16(&sine(440.0, 24_000.0, 24_000, 0.8), 24_000);
    let high = wav16(&sine(880.0, 32_000.0, 32_000, 0.8), 32_000);
    let sfz = "// Made like Aratu's voices.
        <control> default_path=./
        <global> ampeg_release=0.05
        <region> sample=a4.wav pitch_keycenter=69 lokey=0 hikey=74 loop_mode=loop_continuous loop_start=12000 loop_end=17999
        <region> sample=a5.wav pitch_keycenter=81 lokey=75 hikey=127 loop_mode=loop_continuous loop_start=16000 loop_end=23999";
    (
        sfz.to_owned(),
        vec![("a4.wav".to_owned(), low), ("a5.wav".to_owned(), high)],
    )
}
