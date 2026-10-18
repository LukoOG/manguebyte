//! What a part sounds like, for an editor to draw: the latest frames of its
//! signal and its levels.
//!
//! Each part keeps a [`Meter`], fed on the audio thread with what the part
//! adds to the master bus: its sound after its effects, gain and pan, in
//! both channels (its send to the shared reverb is not in it). Feeding it
//! copies the frames into a ring and runs two meters over them, one frame
//! at a time, so it never allocates and what it holds does not depend on
//! how the frames were cut into quanta. Reading it, between render calls,
//! copies out of the ring.

use manguebyte_core::AudioBuffer;

/// How many of a part's latest frames its scope holds, in each channel:
/// 85 ms at 48 kHz. [`Engine::part_scope`](crate::Engine::part_scope)
/// gives up to this many.
pub const SCOPE_FRAMES: usize = 4_096;

/// How long a part's peak level takes to fall 20 dB once the signal has
/// gone quiet: the return time IEC 60268-18 gives digital peak meters. It
/// rises at once, on the frame of the peak, and falls in a straight line in
/// decibels.
pub const PEAK_RETURN_SECONDS: f64 = 1.7;

/// The time constant of a part's RMS level: the square of the signal
/// averaged by a one-pole filter, exponentially, over about the time a VU
/// meter integrates over. A steady sine at full scale reads `1/√2`.
pub const RMS_SECONDS: f64 = 0.3;

/// Below this a peak level is 0: -200 dB. It keeps the meters' memory out
/// of the numbers too small for the floating point to hold at full
/// precision, which WebAssembly does not flush to zero.
const PEAK_FLOOR: f64 = 1e-10;

/// Below this a mean square is 0: -200 dB too.
const POWER_FLOOR: f64 = 1e-20;

/// A part's levels, linear (1 is full scale), on the last frame it
/// rendered: `[left, right]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PartLevels {
    /// The peak level, which rises at once to every sample larger than it
    /// and falls 20 dB in [`PEAK_RETURN_SECONDS`].
    pub peak: [f32; 2],
    /// The RMS level, averaged over [`RMS_SECONDS`].
    pub rms: [f32; 2],
}

impl PartLevels {
    /// `peak` in decibels below full scale, `-inf` for silence.
    pub fn peak_db(&self) -> [f64; 2] {
        self.peak.map(|level| 20.0 * f64::from(level).log10())
    }

    /// `rms` in decibels below full scale, `-inf` for silence.
    pub fn rms_db(&self) -> [f64; 2] {
        self.rms.map(|level| 20.0 * f64::from(level).log10())
    }
}

/// The latest frames of a part, as
/// [`Engine::part_scope`](crate::Engine::part_scope) gives them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartScope {
    /// The frame of the first sample, counted as events count them: the
    /// frame of the output it is heard on.
    pub frame: u64,
    /// The left channel, oldest sample first.
    pub left: Vec<f32>,
    /// The right channel.
    pub right: Vec<f32>,
}

/// A part's scope and levels.
pub(crate) struct Meter {
    left: Box<[f32]>,
    right: Box<[f32]>,
    /// The frame after the newest one in the ring.
    end: u64,
    peak: [f64; 2],
    power: [f64; 2],
    /// What the peak is multiplied by on a frame that does not raise it.
    fall: f64,
    /// How far the mean square moves towards a frame's square.
    smoothing: f64,
}

impl Meter {
    /// A silent meter whose next frame is `frame`. Allocates.
    pub(crate) fn new(sample_rate: f64, frame: u64) -> Self {
        Self {
            left: vec![0.0; SCOPE_FRAMES].into_boxed_slice(),
            right: vec![0.0; SCOPE_FRAMES].into_boxed_slice(),
            end: frame,
            peak: [0.0; 2],
            power: [0.0; 2],
            fall: 10f64.powf(-1.0 / (PEAK_RETURN_SECONDS * sample_rate)),
            smoothing: 1.0 - (-1.0 / (RMS_SECONDS * sample_rate)).exp(),
        }
    }

    /// Takes the frames of `stereo`, the part's next ones. Real-time safe.
    pub(crate) fn feed(&mut self, stereo: &AudioBuffer<f32>) {
        let frames = stereo.num_frames();
        let (fall, smoothing) = (self.fall, self.smoothing);
        for (side, (samples, ring)) in stereo
            .iter_channels()
            .zip([&mut self.left, &mut self.right])
            .enumerate()
        {
            let (mut peak, mut power) = (self.peak[side], self.power[side]);
            for &sample in samples {
                let sample = f64::from(sample);
                peak = (peak * fall).max(sample.abs());
                if peak < PEAK_FLOOR {
                    peak = 0.0;
                }
                power += (sample * sample - power) * smoothing;
                if power < POWER_FLOOR {
                    power = 0.0;
                }
            }
            (self.peak[side], self.power[side]) = (peak, power);
            // The newest frames into the ring, wrapping round it.
            let newest = &samples[frames.saturating_sub(SCOPE_FRAMES)..];
            let start =
                ((self.end + (frames - newest.len()) as u64) % SCOPE_FRAMES as u64) as usize;
            let first = newest.len().min(SCOPE_FRAMES - start);
            ring[start..start + first].copy_from_slice(&newest[..first]);
            ring[..newest.len() - first].copy_from_slice(&newest[first..]);
        }
        self.end += frames as u64;
    }

    /// Makes `frame` the frame after the newest one, for a meter made
    /// before it was known which frame its part starts on. Real-time safe.
    pub(crate) fn start_at(&mut self, frame: u64) {
        self.end = frame;
    }

    /// The frame after the newest one.
    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    /// Copies the newest frames into `left` and `right`, as many as the
    /// shorter holds, at most [`SCOPE_FRAMES`] and at most the frames since
    /// frame 0. Returns how many, and the frame of the first. Real-time
    /// safe.
    pub(crate) fn copy_into(&self, left: &mut [f32], right: &mut [f32]) -> (usize, u64) {
        let count = left
            .len()
            .min(right.len())
            .min(SCOPE_FRAMES)
            .min(usize::try_from(self.end).unwrap_or(usize::MAX));
        let from = self.end - count as u64;
        let start = (from % SCOPE_FRAMES as u64) as usize;
        let first = count.min(SCOPE_FRAMES - start);
        for (out, ring) in [(left, &self.left), (right, &self.right)] {
            out[..first].copy_from_slice(&ring[start..start + first]);
            out[first..count].copy_from_slice(&ring[..count - first]);
        }
        (count, from)
    }

    pub(crate) fn levels(&self) -> PartLevels {
        PartLevels {
            peak: self.peak.map(|peak| peak as f32),
            rms: self.power.map(|power| power.sqrt() as f32),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(left: &[f32], right: &[f32]) -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(2, left.len());
        buffer.channel_mut(0).copy_from_slice(left);
        buffer.channel_mut(1).copy_from_slice(right);
        buffer
    }

    #[test]
    fn the_ring_keeps_the_newest_frames_in_order() {
        let mut meter = Meter::new(48_000.0, 10);
        let ramp: Vec<f32> = (0..10_000).map(|n| n as f32).collect();
        let negative: Vec<f32> = ramp.iter().map(|&n| -n).collect();
        // In pieces that wrap round the ring, and one longer than it.
        for range in [
            0..100,
            100..4_000,
            4_000..4_097,
            4_097..9_999,
            9_999..10_000,
        ] {
            meter.feed(&stereo(&ramp[range.clone()], &negative[range]));
        }
        assert_eq!(meter.end(), 10_010);
        let mut left = vec![0.0; 5_000];
        let mut right = vec![0.0; 5_000];
        let (count, from) = meter.copy_into(&mut left, &mut right);
        assert_eq!((count, from), (SCOPE_FRAMES, 10_010 - SCOPE_FRAMES as u64));
        assert_eq!(left[..count], ramp[10_000 - SCOPE_FRAMES..]);
        assert_eq!(right[..count], negative[10_000 - SCOPE_FRAMES..]);
        let (count, from) = meter.copy_into(&mut left[..3], &mut right[..7]);
        assert_eq!((count, from), (3, 10_007));
        assert_eq!(left[..3], [9_997.0, 9_998.0, 9_999.0]);
    }

    #[test]
    fn a_new_meter_has_no_frames_before_frame_zero() {
        let mut meter = Meter::new(48_000.0, 0);
        meter.feed(&stereo(&[0.5; 5], &[0.25; 5]));
        let mut left = [1.0; 64];
        let mut right = [1.0; 64];
        assert_eq!(meter.copy_into(&mut left, &mut right), (5, 0));
        assert_eq!(left[..5], [0.5; 5]);
    }

    #[test]
    fn the_peak_falls_twenty_decibels_in_its_return_time_and_the_rms_reads_a_sine() {
        // A sine at a quarter of the rate, whose samples hit its crests.
        let rate = if cfg!(miri) { 1_000.0 } else { 48_000.0 };
        let mut meter = Meter::new(rate, 0);
        let frames = (2.0 * rate) as usize;
        let sine: Vec<f32> = (0..frames)
            .map(|n| (core::f64::consts::TAU * 0.25 * n as f64).sin() as f32)
            .collect();
        meter.feed(&stereo(&sine, &vec![0.0; frames]));
        let levels = meter.levels();
        assert!((levels.peak[0] - 1.0).abs() < 1e-3, "{levels:?}");
        assert!(
            (f64::from(levels.rms[0]) - core::f64::consts::FRAC_1_SQRT_2).abs() < 3e-3,
            "{levels:?}"
        );
        assert_eq!(levels.peak[1], 0.0);
        assert_eq!(levels.rms[1], 0.0);
        let before = meter.levels().peak_db()[0];
        let quiet = vec![0.0; (PEAK_RETURN_SECONDS * rate) as usize];
        meter.feed(&stereo(&quiet, &quiet));
        let fallen = before - meter.levels().peak_db()[0];
        assert!((fallen - 20.0).abs() < 0.01, "{fallen}");
        // In silence both settle to exactly nothing.
        let long = vec![0.0; 40 * rate as usize];
        meter.feed(&stereo(&long, &long));
        assert_eq!(meter.levels(), PartLevels::default());
    }
}
