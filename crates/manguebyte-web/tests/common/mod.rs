//! Helpers shared by the integration tests.

#![allow(dead_code)]

use manguebyte_web::{
    Chain, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, Pushed, SfzInstrument,
    SfzLoad,
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

/// Makes the instrument of `sfz` with `files` through [`SfzLoad`], file by
/// file.
pub fn build_sfz(sfz: &str, files: &[(String, Vec<u8>)]) -> SfzInstrument {
    let mut load = SfzLoad::new(sfz).unwrap_or_else(|error| panic!("{error}"));
    let wanted: Vec<String> = load.paths().map(str::to_owned).collect();
    for path in wanted {
        let (_, bytes) = files
            .iter()
            .find(|(name, _)| *name == path)
            .unwrap_or_else(|| panic!("no fixture {path}"));
        assert!(load.add_file(&path, bytes.clone()).unwrap(), "{path}");
    }
    load.finish().unwrap_or_else(|error| panic!("{error}"))
}

/// Loads `sfz` with `files` through [`SfzLoad`], file by file, and gives
/// it to part `part`, which must be a sampler part: measured, so that the
/// part plays it at the loudness target while it normalises.
pub fn load_sfz(engine: &mut Engine, part: u16, sfz: &str, files: &[(String, Vec<u8>)]) {
    let instrument = build_sfz(sfz, files);
    engine.load_instrument(part, instrument).unwrap();
}

/// [`load_sfz`] without the measurement: the part plays the samples at
/// [`sampler_level`], as they are.
pub fn load_sfz_unmeasured(engine: &mut Engine, part: u16, sfz: &str, files: &[(String, Vec<u8>)]) {
    let instrument = build_sfz(sfz, files);
    engine.load_instrument_unmeasured(part, instrument).unwrap();
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

/// The level a sampler part plays a sample at, at full velocity in the
/// centre: [`manguebyte_web::SAMPLER_LEVEL_DB`] as a gain.
pub fn sampler_level() -> f32 {
    10f64.powf(manguebyte_web::SAMPLER_LEVEL_DB / 20.0) as f32
}

/// An instrument that plays `samples`, at the engine's rate, as they are:
/// a one-shot on every key, read at its own rate from key 60, stored as
/// 32-bit floats. With it, a sampler part is a signal generator whose
/// output is `samples` times [`sampler_level`], frame for frame.
pub fn signal_instrument(samples: &[f32]) -> (String, Vec<(String, Vec<u8>)>) {
    let mut buffer = manguebyte_core::AudioBuffer::<f32>::new(1, samples.len());
    buffer.channel_mut(0).copy_from_slice(samples);
    let mut bytes = Vec::new();
    manguebyte_formats::wav::write(
        &mut bytes,
        &buffer,
        RATE as u32,
        manguebyte_formats::wav::WavFormat::Float32,
    )
    .unwrap();
    let sfz = "<region> sample=signal.wav lokey=0 hikey=127 pitch_keycenter=60 loop_mode=one_shot";
    (sfz.to_owned(), vec![("signal.wav".to_owned(), bytes)])
}

/// An engine whose part 0, through `chain`, plays `samples` (see
/// [`signal_instrument`]) when key 60 is struck, as they are: the
/// instrument is not measured for loudness normalisation.
pub fn signal_engine(samples: &[f32], chain: Chain) -> Engine {
    let mut engine = engine_with(InstrumentKind::Sampler, chain);
    let (sfz, files) = signal_instrument(samples);
    load_sfz_unmeasured(&mut engine, 0, &sfz, &files);
    engine
}

/// The largest step from one sample to the next in `samples`.
pub fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

/// The root mean square of `samples`.
pub fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len().max(1) as f64).sqrt()
}

/// A kit like Aratu's: GM keys, one-shots, the open hat in group 1 cut off
/// by group 2, the closed hat; at 32 kHz.
pub fn kit_instrument() -> (String, Vec<(String, Vec<u8>)>) {
    let decaying = |frequency: f64, frames: usize| {
        let mut tone = sine(frequency, 32_000.0, frames, 0.9);
        for (n, sample) in tone.iter_mut().enumerate() {
            *sample *= (1.0 - n as f32 / frames as f32).powi(2);
        }
        tone
    };
    let files = vec![
        (
            "kick.wav".to_owned(),
            wav16(&decaying(60.0, 16_000), 32_000),
        ),
        ("snare.wav".to_owned(), wav16(&noise(8_000, 0.8, 3), 32_000)),
        (
            "closed-hat.wav".to_owned(),
            wav16(&noise(2_000, 0.6, 5), 32_000),
        ),
        (
            "open-hat.wav".to_owned(),
            wav16(&noise(32_000, 0.6, 7), 32_000),
        ),
    ];
    let sfz = "<control> default_path=./
        <group> loop_mode=one_shot
        <region> sample=kick.wav key=36
        <region> sample=snare.wav key=38
        <region> sample=closed-hat.wav key=42 group=2
        <region> sample=open-hat.wav key=46 group=1 off_by=2";
    (sfz.to_owned(), files)
}
