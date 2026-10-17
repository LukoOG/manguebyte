//! What the tests of the lossy decoders share: the audio the files in
//! `data/` were encoded from, how near a decode is to it, and the ways of
//! reading a file that every decoder is put through.

use std::io::Cursor;

use manguebyte_core::AudioBuffer;
use manguebyte_formats::AudioReader;

use super::{Rng, slice};

/// The audio the Ogg Vorbis and MP3 files in `data/` were encoded from, as
/// `data/README.md` says: `frames` frames at `rate` hertz, the same for
/// every run.
///
/// Five stretches follow each other, each made to put a different part of
/// a lossy decoder to work: three partials of a tone (long blocks, and the
/// coarse quantisation of a few strong lines), a sweep from 40 Hz to three
/// tenths of the sample rate (every band in turn), digital silence (frames
/// that code nothing), white noise (every codeword of every table), and
/// clicks over a quiet tone (the short blocks an encoder switches to at a
/// transient). Every channel has its own pitch and its own noise, so that
/// two channels swapped or mixed up show.
pub fn lossy_signal(channels: usize, rate: u32, frames: usize) -> AudioBuffer<f32> {
    use std::f64::consts::TAU;
    let rate = f64::from(rate);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (channel, samples) in buffer.iter_channels_mut().enumerate() {
        let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15 ^ channel as u64);
        let pitch = 110.0 * (1.0 + 0.37 * channel as f64);
        let top = rate * 0.3;
        let mut phase = 0.0f64;
        for (index, sample) in samples.iter_mut().enumerate() {
            let time = index as f64 / rate;
            let part = index as f64 / frames as f64;
            let value = if part < 0.3 {
                0.3 * (TAU * pitch * time).sin()
                    + 0.15 * (TAU * pitch * 3.01 * time).sin()
                    + 0.05 * (TAU * pitch * 7.0 * time).sin()
            } else if part < 0.5 {
                let frequency = 40.0 * (top / 40.0).powf((part - 0.3) / 0.2);
                phase += TAU * frequency / rate;
                0.4 * phase.sin()
            } else if part < 0.6 {
                0.0
            } else if part < 0.8 {
                0.25 * ((rng.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0)
            } else {
                let since = time % 0.1;
                0.05 * (TAU * 2.0 * pitch * time).sin()
                    + 0.6 * (-since * 200.0).exp() * (TAU * 2_000.0 * since).sin()
            };
            *sample = value as f32;
        }
    }
    buffer
}

/// A long stretch of near silence, broken every quarter of a second by a
/// short burst of a tone whose pitch climbs from burst to burst: audio that
/// takes few bytes, so that a file long enough for a seek to have to search
/// it stays small, and in which every place sounds different from every
/// other.
pub fn lossy_bursts(rate: u32, frames: usize) -> AudioBuffer<f32> {
    use std::f64::consts::TAU;
    let rate = f64::from(rate);
    let period = (rate / 4.0) as usize;
    let burst = (rate * 0.04) as usize;
    let mut buffer = AudioBuffer::new(1, frames);
    for (index, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
        let within = index % period;
        if within < burst {
            let count = index / period;
            let pitch = 300.0 + (count % 40) as f64 * 50.0;
            let time = within as f64 / rate;
            let envelope = (std::f64::consts::PI * within as f64 / burst as f64).sin();
            *sample = (0.5 * envelope * (TAU * pitch * time).sin()) as f32;
        }
    }
    buffer
}

/// Steady tones, a different chord on every channel: what a lossy codec at
/// a high rate gives back closely, to hold a decoder to a high
/// signal-to-noise ratio.
pub fn lossy_tones(channels: usize, rate: u32, frames: usize) -> AudioBuffer<f32> {
    use std::f64::consts::TAU;
    let rate = f64::from(rate);
    let mut buffer = AudioBuffer::new(channels, frames);
    for (channel, samples) in buffer.iter_channels_mut().enumerate() {
        let pitch = 220.0 * (1.0 + 0.25 * channel as f64);
        for (index, sample) in samples.iter_mut().enumerate() {
            let time = index as f64 / rate;
            let value = 0.3 * (TAU * pitch * time).sin()
                + 0.2 * (TAU * pitch * 1.5 * time + 1.0).sin()
                + 0.1 * (TAU * pitch * 2.5 * time + 2.0).sin();
            *sample = value as f32;
        }
    }
    buffer
}

/// How close `decoded` is to `source` over `range`: the signal-to-noise
/// ratio in decibels, every channel together, and the largest difference
/// of a sample.
pub fn closeness(
    decoded: &AudioBuffer<f32>,
    source: &AudioBuffer<f32>,
    range: std::ops::Range<usize>,
) -> (f64, f32) {
    let mut signal = 0.0f64;
    let mut noise = 0.0f64;
    let mut worst = 0.0f32;
    for (got, want) in decoded.iter_channels().zip(source.iter_channels()) {
        for (&got, &want) in got[range.clone()].iter().zip(&want[range.clone()]) {
            signal += f64::from(want) * f64::from(want);
            noise += f64::from(got - want) * f64::from(got - want);
            worst = worst.max((got - want).abs());
        }
    }
    (10.0 * (signal / noise.max(1e-30)).log10(), worst)
}

/// The gain that brings `decoded` nearest to `source` over `range`, least
/// squares and every channel together, and the signal-to-noise ratio in
/// decibels once it is applied: how alike the two are in shape, whatever
/// level an encoder chose to give its output.
pub fn fitted_closeness(
    decoded: &AudioBuffer<f32>,
    source: &AudioBuffer<f32>,
    range: std::ops::Range<usize>,
) -> (f64, f64) {
    let mut cross = 0.0f64;
    let mut power = 0.0f64;
    for (got, want) in decoded.iter_channels().zip(source.iter_channels()) {
        for (&got, &want) in got[range.clone()].iter().zip(&want[range.clone()]) {
            cross += f64::from(got) * f64::from(want);
            power += f64::from(got) * f64::from(got);
        }
    }
    let gain = cross / power.max(1e-30);
    let mut signal = 0.0f64;
    let mut noise = 0.0f64;
    for (got, want) in decoded.iter_channels().zip(source.iter_channels()) {
        for (&got, &want) in got[range.clone()].iter().zip(&want[range.clone()]) {
            signal += f64::from(want) * f64::from(want);
            noise += (gain * f64::from(got) - f64::from(want)).powi(2);
        }
    }
    (1.0 / gain, 10.0 * (signal / noise.max(1e-30)).log10())
}

/// The name of every source, which is also the name of its WAV file.
pub const SOURCES: [&str; 16] = [
    "signal-2ch-44100",
    "signal-1ch-22050",
    "signal-6ch-48000",
    "signal-1ch-8000",
    "signal-2ch-96000",
    "tones-2ch-48000",
    "tones-2ch-44100-short",
    "bursts-1ch-8000",
    "signal-2ch-32000",
    "signal-2ch-24000",
    "signal-1ch-16000",
    "signal-2ch-11025",
    "signal-1ch-12000",
    "tones-2ch-44100",
    "signal-2ch-48000",
    "signal-1ch-48000",
];

/// The audio of a source and its sample rate.
pub fn source(name: &str) -> (AudioBuffer<f32>, u32) {
    let (kind, rest) = name.split_once('-').unwrap();
    let mut parts = rest.split('-');
    let channels: usize = parts
        .next()
        .unwrap()
        .trim_end_matches("ch")
        .parse()
        .unwrap();
    let rate: u32 = parts.next().unwrap().parse().unwrap();
    let buffer = match (kind, parts.next()) {
        ("tones", Some("short")) => lossy_tones(channels, rate, 100),
        ("tones", None) => lossy_tones(channels, rate, rate as usize),
        ("bursts", None) => lossy_bursts(rate, rate as usize * 150 + 5),
        ("signal", None) => {
            // Lengths that are no round number of anything a codec uses.
            let frames = match rate {
                44_100 => 110_257,
                22_050 => 44_111,
                48_000 => 57_601,
                8_000 => 12_007,
                96_000 => 48_013,
                32_000 => 40_003,
                24_000 => 30_011,
                16_000 => 20_021,
                11_025 => 16_005,
                12_000 => 15_013,
                _ => unreachable!(),
            };
            lossy_signal(channels, rate, frames)
        }
        _ => unreachable!(),
    };
    (buffer, rate)
}

/// What a decoded file is held to.
pub struct Bounds {
    pub name: &'static str,
    pub bytes: &'static [u8],
    /// The source it was encoded from, by name.
    pub source: &'static str,
    /// Where in the decoded audio the source starts, and how long the
    /// decoded audio is when that is not the length of the source.
    pub offset: usize,
    pub len: Option<usize>,
    /// The least signal-to-noise ratio over the whole source, in decibels.
    pub snr: f64,
    /// The least ratio over the steady tones, once their level is matched:
    /// the first three tenths of a signal source, or the whole of a source
    /// of tones or bursts.
    pub tones_snr: f64,
    /// Where the level of those tones must lie, against the source: an
    /// encoder may choose to give its output a level of its own.
    pub level: (f64, f64),
    /// The largest difference of a sample.
    pub max_error: f32,
}

impl Bounds {
    /// Bounds for a file that decodes to its source, in place and at its
    /// length.
    pub fn new(name: &'static str, bytes: &'static [u8], source: &'static str) -> Self {
        Self {
            name,
            bytes,
            source,
            offset: 0,
            len: None,
            snr: 0.0,
            tones_snr: 0.0,
            level: (0.99, 1.01),
            max_error: 1.0,
        }
    }

    pub fn snr(self, whole: f64, tones: f64) -> Self {
        Self {
            snr: whole,
            tones_snr: tones,
            ..self
        }
    }

    pub fn level(self, low: f64, high: f64) -> Self {
        Self {
            level: (low, high),
            ..self
        }
    }

    pub fn max_error(self, max_error: f32) -> Self {
        Self { max_error, ..self }
    }

    pub fn at(self, offset: usize, len: usize) -> Self {
        Self {
            offset,
            len: Some(len),
            ..self
        }
    }

    /// Checks a decode against the bounds, and prints how near it came.
    pub fn check(&self, decoded: &AudioBuffer<f32>, rate: u32) {
        let name = self.name;
        let (source, source_rate) = source(self.source);
        assert_eq!(rate, source_rate, "{name}");
        assert_eq!(decoded.num_channels(), source.num_channels(), "{name}");
        let frames = source.num_frames();
        let len = self.len.unwrap_or(frames);
        assert_eq!(decoded.num_frames(), len, "{name}: length");
        let aligned = slice(decoded, self.offset..self.offset + frames);
        let (snr, worst) = closeness(&aligned, &source, 0..frames);
        let tones = if self.source.starts_with("signal") {
            0..frames * 3 / 10
        } else {
            0..frames
        };
        let (level, tones_snr) = fitted_closeness(&aligned, &source, tones);
        println!(
            "{name}: {snr:.2} dB, largest error {worst:.4}; tones at level {level:.4}, \
             {tones_snr:.2} dB once matched"
        );
        assert!(snr >= self.snr, "{name}: {snr:.2} dB");
        assert!(
            tones_snr >= self.tones_snr,
            "{name}: {tones_snr:.2} dB on the tones"
        );
        assert!(
            (self.level.0..=self.level.1).contains(&level),
            "{name}: level {level:.4}"
        );
        assert!(worst <= self.max_error, "{name}: error of {worst}");
    }
}

/// Reads a whole file through [`AudioReader`] in blocks of random sizes.
pub fn read_in_random_blocks(bytes: &[u8], seed: u64) -> AudioBuffer<f32> {
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    let channels = reader.num_channels();
    let mut collected = vec![Vec::new(); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 5_000);
    let mut rng = Rng::new(seed);
    loop {
        let size = rng.between(0, 5_000) as usize;
        block.set_num_frames(size);
        let got = reader.read(&mut block).unwrap();
        for (out, channel) in collected.iter_mut().zip(block.iter_channels()) {
            out.extend_from_slice(&channel[..got]);
        }
        if got < size {
            break;
        }
    }
    let mut buffer = AudioBuffer::new(channels, collected[0].len());
    for (out, samples) in buffer.iter_channels_mut().zip(&collected) {
        out.copy_from_slice(samples);
    }
    buffer
}

/// Seeks all over a file, near and far, and checks that what is read after
/// each seek is, bit for bit, the same stretch of `whole`.
pub fn check_seeks_are_exact(
    name: &str,
    bytes: &[u8],
    whole: &AudioBuffer<f32>,
    rounds: usize,
    seed: u64,
) {
    let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    let len = whole.num_frames() as u64;
    let mut rng = Rng::new(seed);
    let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 1_000);
    for round in 0..rounds {
        let here = reader.position();
        let target = match round % 8 {
            0 => rng.below(2_000),
            1 => len.saturating_sub(rng.below(3_000)),
            2 => len + rng.below(100),
            3 => here + rng.below(3_000),
            4 => here.saturating_sub(rng.below(3_000)),
            _ => rng.below(len),
        };
        let landed = reader
            .seek(target)
            .unwrap_or_else(|error| panic!("{name}: seek to {target}: {error}"));
        assert_eq!(landed, target.min(len), "{name}: seek to {target}");
        let size = rng.between(1, 1_000) as usize;
        block.set_num_frames(size);
        let got = reader
            .read(&mut block)
            .unwrap_or_else(|error| panic!("{name}: read at {landed}: {error}"));
        assert_eq!(
            got as u64,
            (size as u64).min(len - landed),
            "{name}: at {landed}"
        );
        let from = landed as usize;
        let want = slice(whole, from..from + got);
        for (channel, (read, expected)) in
            block.iter_channels().zip(want.iter_channels()).enumerate()
        {
            let same = read[..got]
                .iter()
                .zip(expected)
                .all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "{name}: {got} frames at {landed}, channel {channel}");
        }
    }
}

/// Decodes as much of `bytes` as can be decoded through [`AudioReader`],
/// and says whether it stopped at an error.
pub fn read_what_can_be_read(bytes: &[u8]) -> Option<(AudioBuffer<f32>, bool)> {
    let mut reader = AudioReader::new(Cursor::new(bytes)).ok()?;
    let channels = reader.num_channels();
    let mut collected = vec![Vec::new(); channels];
    let mut block = AudioBuffer::<f32>::new(channels, 2_048);
    let mut failed = false;
    for _ in 0..10_000 {
        match reader.read(&mut block) {
            Ok(0) => break,
            Ok(got) => {
                for (out, channel) in collected.iter_mut().zip(block.iter_channels()) {
                    out.extend_from_slice(&channel[..got]);
                }
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let mut buffer = AudioBuffer::new(channels, collected[0].len());
    for (out, samples) in buffer.iter_channels_mut().zip(&collected) {
        out.copy_from_slice(samples);
    }
    Some((buffer, failed))
}

/// Opens, seeks and reads data that may be anything. Nothing may panic or
/// hang, and a seek that works lands where it was asked to, or at the end.
pub fn poke(bytes: &[u8]) {
    let Ok(mut reader) = AudioReader::new(Cursor::new(bytes)) else {
        return;
    };
    let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 300);
    for target in [3, 5_000, u64::MAX, 1, 40_000] {
        if let Ok(landed) = reader.seek(target) {
            assert!(landed <= target);
            assert_eq!(reader.position(), landed);
        }
        let _ = reader.read(&mut block);
    }
}

/// Works out the checksum of every Ogg page in `bytes` again.
pub fn fix_page_checksums(bytes: &mut [u8]) {
    let mut at = 0;
    while at + 27 <= bytes.len() {
        if &bytes[at..at + 4] != b"OggS" {
            at += 1;
            continue;
        }
        let segments = usize::from(bytes[at + 26]);
        if at + 27 + segments > bytes.len() {
            return;
        }
        let body: usize = bytes[at + 27..at + 27 + segments]
            .iter()
            .map(|&l| usize::from(l))
            .sum();
        let end = at + 27 + segments + body;
        if end > bytes.len() {
            return;
        }
        bytes[at + 22..at + 26].fill(0);
        let crc = super::ogg_crc(&bytes[at..end]);
        bytes[at + 22..at + 26].copy_from_slice(&crc.to_le_bytes());
        at = end;
    }
}
