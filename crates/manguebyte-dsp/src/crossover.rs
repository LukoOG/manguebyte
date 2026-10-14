//! Linkwitz-Riley crossovers: splitting a signal into bands that add back
//! up to the signal, with only its phase turned.

use core::f64::consts::{PI, SQRT_2};

use manguebyte_core::{AudioBuffer, Sample};

use crate::cutoff::GlidingCutoff;
use crate::{ProcessSpec, Processor};

/// Lowest crossover frequency, in hertz.
const LOWEST_HZ: f64 = 1.0;
/// Highest crossover frequency, as a fraction of the sample rate.
const HIGHEST_RATIO: f64 = 0.49;
/// How long a crossover frequency takes to reach a new value, at least.
const GLIDE_SECONDS: f64 = 0.05;
/// The most a crossover frequency moves in one period of itself, in
/// octaves.
const OCTAVES_PER_PERIOD: f64 = 0.1;
/// The most a crossover frequency moves in one second, in octaves.
const OCTAVES_PER_SECOND: f64 = 50.0;

/// Most crossover frequencies, one fewer than the most bands.
const MAX_SPLITS: usize = 4;

/// The damping of the two second-order factors of a fourth-order
/// Butterworth polynomial: `2·sin(π/8)` and `2·sin(3π/8)`.
fn butterworth_4_damping() -> [f64; 2] {
    [2.0 * (PI / 8.0).sin(), 2.0 * (3.0 * PI / 8.0).sin()]
}

/// How steep a [`Crossover`] is.
///
/// A Linkwitz-Riley crossover of order `2N` splits with the square of a
/// Butterworth filter of order `N`: each band is 6.02 dB down at the
/// crossover frequency, the two bands of a split are in phase at every
/// frequency, and they add up to an all-pass filter, so the sum has a flat
/// response. (S. H. Linkwitz, "Active Crossover Networks for Noncoincident
/// Drivers", Journal of the Audio Engineering Society 24(1), 1976.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossoverSlope {
    /// Second order, 12 dB per octave. The high band comes out with its
    /// polarity inverted, which is what makes the bands add up flat.
    Lr2,
    /// Fourth order, 24 dB per octave. The usual choice.
    Lr4,
    /// Eighth order, 48 dB per octave.
    Lr8,
}

impl CrossoverSlope {
    /// Every slope, from the gentlest.
    pub const ALL: [Self; 3] = [Self::Lr2, Self::Lr4, Self::Lr8];

    /// The order of the Butterworth filter that is squared: 1, 2 or 4.
    pub fn butterworth_order(self) -> usize {
        match self {
            Self::Lr2 => 1,
            Self::Lr4 => 2,
            Self::Lr8 => 4,
        }
    }

    /// How fast each band falls far from its crossover frequency: 12, 24
    /// or 48 dB per octave.
    pub fn db_per_octave(self) -> f64 {
        12.0 * self.butterworth_order() as f64
    }
}

/// What one sample needs of a crossover frequency: the gains of a
/// trapezoidal one-pole and of the two state-variable filters a split is
/// made of.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// `g / (1 + g)`, for a one-pole.
    one_pole: f64,
    /// Andrew Simper's `a1, a2, a3` for each damping in use.
    svf: [[f64; 3]; 2],
}

impl Tuning {
    /// A placeholder for the splits a crossover does not have.
    const UNUSED: Self = Self {
        one_pole: 0.0,
        svf: [[0.0; 3]; 2],
    };

    fn new(g: f64, damping: &[f64; 2]) -> Self {
        let svf = damping.map(|k| {
            let a1 = 1.0 / (1.0 + g * (g + k));
            let a2 = g * a1;
            [a1, a2, g * a2]
        });
        Self {
            one_pole: g / (1.0 + g),
            svf,
        }
    }
}

/// One sample through a trapezoidal one-pole low-pass, in the
/// topology-preserving form of Vadim Zavalishin, "The Art of VA Filter
/// Design", chapter 3.
#[inline]
fn one_pole(memory: &mut f64, x: f64, gain: f64) -> f64 {
    let v = (x - *memory) * gain;
    let low = v + *memory;
    *memory = low + v;
    low
}

/// One sample through a trapezoidal state-variable filter (Andrew Simper,
/// "Linear Trapezoidal Integrated SVF", Cytomic, 2013): the band-pass
/// output before it is scaled by the damping, and the low-pass output.
#[inline]
fn svf(memory: &mut [f64; 2], x: f64, a: &[f64; 3]) -> (f64, f64) {
    let v3 = x - memory[1];
    let band = a[0] * memory[0] + a[1] * v3;
    let low = memory[1] + a[1] * memory[0] + a[2] * v3;
    memory[0] = 2.0 * band - memory[0];
    memory[1] = 2.0 * low - memory[1];
    (band, low)
}

/// The memory of one channel.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Memory {
    /// Per split: the four sections of the low-pass chain, and the second
    /// section of the all-pass of an eighth-order split.
    splits: [[[f64; 2]; 5]; MAX_SPLITS],
    /// Per band below the top two, and per split above it: an all-pass of
    /// up to two sections.
    compensation: [[[[f64; 2]; 2]; MAX_SPLITS]; MAX_SPLITS - 1],
}

impl Memory {
    const SILENT: Self = Self {
        splits: [[[0.0; 2]; 5]; MAX_SPLITS],
        compensation: [[[[0.0; 2]; 2]; MAX_SPLITS]; MAX_SPLITS - 1],
    };
}

/// Splits a signal into two to five bands that add back up to it.
///
/// The bands are Linkwitz-Riley bands (see [`CrossoverSlope`]): each is
/// 6.02 dB down at a crossover frequency, all of them are in phase with
/// each other at every frequency, and their sum is the input passed through
/// an all-pass filter. The sum therefore has a flat response; only its
/// phase turns, around each crossover frequency.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::{Crossover, CrossoverSlope, ProcessSpec, Processor};
///
/// // Three bands: below 200 Hz, 200 Hz to 2 kHz, above 2 kHz.
/// let mut crossover = Crossover::<f32>::new(CrossoverSlope::Lr4, &[200.0, 2_000.0]);
/// crossover.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A tone at 1 kHz lands in the middle band.
/// let mut tone = AudioBuffer::<f32>::new(1, 4_800);
/// for (n, sample) in tone.channel_mut(0).iter_mut().enumerate() {
///     *sample = (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
/// }
/// crossover.split(&tone);
/// let level = |band: usize| crossover.band(band).channel(0)[2_400..].iter().fold(0.0f32, |p, s| p.max(s.abs()));
/// assert!(level(1) > 0.9 && level(0) < 0.05 && level(2) < 0.1);
///
/// // And the three add back up to a tone of the same level.
/// let mut sum = AudioBuffer::<f32>::new(1, 4_800);
/// crossover.sum_into(&mut sum);
/// let peak = sum.channel(0)[2_400..].iter().fold(0.0f32, |p, s| p.max(s.abs()));
/// assert!((peak - 1.0).abs() < 1e-3);
/// ```
///
/// # How it works
///
/// Each split at a crossover frequency `fc` computes its low band as a
/// Butterworth low-pass applied twice, `1 / B(s)²`, and the all-pass
/// `B(-s) / B(s)` that the two bands must add up to; the high band is the
/// all-pass minus the low band. Because `B(s)·B(-s) = 1 + (-s²)^N` for a
/// Butterworth polynomial of order `N`, that difference is exactly the
/// Linkwitz-Riley high band `(-s²)^N / B(s)²`, and the two add up to the
/// all-pass whatever rounding does to either. All filters are trapezoidal
/// one-poles (for [`Lr2`](CrossoverSlope::Lr2)) or state-variable filters
/// in the topology-preserving form, so they are the bilinear transform of
/// the analogue ones, with the frequency pre-warped to land exactly.
/// Measured, two bands add up to the all-pass to within 1e-9, so the sum
/// is flat to within 1e-8 dB, and each band follows its formula to within
/// 1e-13 in plain gain.
///
/// With more than two bands the splits run from the lowest frequency up,
/// each splitting what is above the one before. The bands below then miss
/// the phase turn of the splits above them, so each is passed through the
/// all-pass of every split above its own upper one: all bands then carry
/// the same phase, and their sum is the product of all the all-passes,
/// still flat. This is the usual compensation. Without it, the sum of a
/// three-band LR4 split at 500 Hz and 1 kHz dips by 3.6 dB between them,
/// and at 200 Hz and 2 kHz by 0.11 dB.
///
/// # Use
///
/// [`split`](Self::split) writes the bands to buffers the crossover owns,
/// allocated by [`prepare`](Processor::prepare); read them with
/// [`band`](Self::band), process them in place through
/// [`band_mut`](Self::band_mut), and add them back up with
/// [`sum_into`](Self::sum_into). As a [`Processor`], it splits and adds
/// back up in one go, which leaves the all-pass: the phase a dry signal
/// needs to line up with the bands of another crossover.
///
/// A crossover frequency changed with
/// [`set_frequency`](Self::set_frequency) glides there on a musical scale,
/// so that a change does not click: over 50 ms, or longer where that would
/// move it faster than 50 octaves per second or than a tenth of an octave
/// in one period of the frequency it has reached. A move of an octave
/// above 500 Hz takes the 50 ms; from 1 kHz to 100 Hz, or back, takes
/// 0.14 s, and across the whole range from 20 kHz to 20 Hz 0.8 s, most of
/// it in the lowest octaves. The filters stay stable however the
/// frequencies move.
///
/// # Limits
///
/// While a frequency glides, the sum is the output of an all-pass whose
/// frequency moves, and that is not quite flat. How far it strays depends
/// on how far the frequency moves while the filters ring, which is to say
/// per period, and the limits on the pace hold that down at every
/// frequency. Measured at 48 kHz, with tones from 50 Hz to 5 kHz and the
/// frequency moved from 1 kHz to 100 Hz and back, and with tones from
/// 20 Hz to 20 kHz and the frequency moved from 20 kHz to 20 Hz, the sum's
/// peak rose by at most about 0.05 dB for LR2, 0.17 dB for LR4 and 0.66 dB
/// for LR8. Gliding over 50 ms whatever the distance, without the limits,
/// the move from 20 kHz to 20 Hz raised it by up to 2.7, 5.8 and 8.8 dB.
/// (The tones were 1/3 or 1/2 octave apart, then 1/36 octave apart around
/// the worst; a tone between them can do a little worse.) The bands
/// themselves change smoothly, without a click.
///
/// Crossover frequencies are limited to between 1 Hz and 0.49 of the
/// sample rate. The bands mean what their names say when the frequencies
/// rise from the first to the last; the sum is flat whatever their order.
/// [`split`](Self::split) handles at most the block size the crossover was
/// prepared for, and the bands are that long; a [`Processor`] call of any
/// length is handled in pieces. Channels it was not prepared for are left
/// out of the bands, and left as they are by
/// [`process`](Processor::process). The slope and the number of bands are
/// fixed when the crossover is made.
#[derive(Debug, Clone)]
pub struct Crossover<S: Sample = f32> {
    slope: CrossoverSlope,
    /// How many crossover frequencies there are: one fewer than the bands.
    splits: usize,
    cutoffs: [GlidingCutoff; MAX_SPLITS],
    /// The damping of the state-variable sections of a split.
    damping: [f64; 2],
    memory: Vec<Memory>,
    bands: Vec<AudioBuffer<S>>,
}

impl<S: Sample> Crossover<S> {
    /// Most bands a crossover splits into.
    pub const MAX_BANDS: usize = MAX_SPLITS + 1;

    /// Creates a crossover at `frequencies`, in hertz, which makes one band
    /// more than there are frequencies. At most four frequencies are used,
    /// for five bands; none at all leaves one band, the input itself. Until
    /// [`prepare`](Processor::prepare) is called it assumes 44.1 kHz and
    /// has no bands to write to.
    pub fn new(slope: CrossoverSlope, frequencies: &[f64]) -> Self {
        let splits = frequencies.len().min(MAX_SPLITS);
        let cutoff = |hz: f64| {
            GlidingCutoff::new(hz, LOWEST_HZ, HIGHEST_RATIO, GLIDE_SECONDS)
                .with_pace_limit(OCTAVES_PER_PERIOD, OCTAVES_PER_SECOND)
        };
        let mut cutoffs = [cutoff(1_000.0); MAX_SPLITS];
        for (slot, &hz) in cutoffs.iter_mut().zip(&frequencies[..splits]) {
            *slot = cutoff(hz);
        }
        let damping = match slope {
            CrossoverSlope::Lr2 => [2.0, 2.0],
            CrossoverSlope::Lr4 => [SQRT_2, SQRT_2],
            CrossoverSlope::Lr8 => butterworth_4_damping(),
        };
        Self {
            slope,
            splits,
            cutoffs,
            damping,
            memory: Vec::new(),
            bands: vec![AudioBuffer::new(0, 0); splits + 1],
        }
    }

    /// A crossover into two bands at `frequency`.
    pub fn two_way(slope: CrossoverSlope, frequency: f64) -> Self {
        Self::new(slope, &[frequency])
    }

    /// Moves crossover frequency `index`, in hertz. It glides there over
    /// 50 ms or longer (see the [type documentation](Self)). Real-time
    /// safe. An `index` the crossover does not have is ignored.
    pub fn set_frequency(&mut self, index: usize, frequency: f64) {
        if let Some(cutoff) = self.cutoffs[..self.splits].get_mut(index) {
            cutoff.set(frequency);
        }
    }

    /// Crossover frequency `index` in hertz, after the limits: where it is
    /// or is heading for. `None` for an `index` the crossover does not
    /// have.
    pub fn frequency(&self, index: usize) -> Option<f64> {
        self.cutoffs[..self.splits]
            .get(index)
            .map(GlidingCutoff::limited)
    }

    /// The slope.
    pub fn slope(&self) -> CrossoverSlope {
        self.slope
    }

    /// How many bands [`split`](Self::split) writes.
    pub fn num_bands(&self) -> usize {
        self.splits + 1
    }

    /// Band `index`, from the lowest, as the last [`split`](Self::split)
    /// left it.
    ///
    /// # Panics
    ///
    /// Panics if `index` is not below [`num_bands`](Self::num_bands).
    pub fn band(&self, index: usize) -> &AudioBuffer<S> {
        &self.bands[index]
    }

    /// Band `index`, to be processed in place before the bands are added
    /// back up.
    ///
    /// # Panics
    ///
    /// Panics if `index` is not below [`num_bands`](Self::num_bands).
    pub fn band_mut(&mut self, index: usize) -> &mut AudioBuffer<S> {
        &mut self.bands[index]
    }

    /// Splits `input` into the bands. Real-time safe.
    ///
    /// At most as many frames as the crossover was prepared for are split,
    /// and the bands are that long; channels beyond those it was prepared
    /// for are left out, and band channels the input does not have are
    /// silent.
    pub fn split(&mut self, input: &AudioBuffer<S>) {
        let frames = input
            .num_frames()
            .min(self.bands.first().map_or(0, AudioBuffer::capacity));
        for band in &mut self.bands {
            band.set_num_frames(frames);
            band.clear();
        }
        let mut after = self.cutoffs;
        let channels = input.num_channels().min(self.memory.len());
        for channel in 0..channels {
            let mut cutoffs = self.cutoffs;
            let mut memory = self.memory[channel];
            for (frame, &sample) in input.channel(channel)[..frames].iter().enumerate() {
                let values = self.step(&mut cutoffs, &mut memory, sample.to_f64());
                for (band, value) in self.bands.iter_mut().zip(values) {
                    band.channel_mut(channel)[frame] = S::from_f64(value);
                }
            }
            self.memory[channel] = memory;
            after = cutoffs;
        }
        if channels == 0 {
            // No channel to split, but time still passes.
            for _ in 0..frames {
                for cutoff in &mut after[..self.splits] {
                    cutoff.next_gain();
                }
            }
        }
        self.cutoffs = after;
    }

    /// Writes the sum of the bands to `output`, over as many channels and
    /// frames as both have; the rest of `output` is left as it is.
    pub fn sum_into(&self, output: &mut AudioBuffer<S>) {
        let frames = output
            .num_frames()
            .min(self.bands.first().map_or(0, AudioBuffer::num_frames));
        let channels = output
            .num_channels()
            .min(self.bands.first().map_or(0, AudioBuffer::num_channels));
        for channel in 0..channels {
            let out = &mut output.channel_mut(channel)[..frames];
            out.fill(S::ZERO);
            for band in &self.bands {
                for (out, &value) in out.iter_mut().zip(band.channel(channel)) {
                    *out += value;
                }
            }
        }
    }

    /// One frame of one channel through every split and every compensating
    /// all-pass: the value of each band.
    #[inline]
    fn step(
        &self,
        cutoffs: &mut [GlidingCutoff; MAX_SPLITS],
        memory: &mut Memory,
        x: f64,
    ) -> [f64; MAX_SPLITS + 1] {
        let splits = self.splits;
        let mut tunings = [Tuning::UNUSED; MAX_SPLITS];
        for (tuning, cutoff) in tunings.iter_mut().zip(&mut cutoffs[..splits]) {
            *tuning = Tuning::new(cutoff.next_gain(), &self.damping);
        }
        let mut values = [0.0; MAX_SPLITS + 1];
        let mut rest = x;
        for k in 0..splits {
            let (low, high) = self.split_once(&mut memory.splits[k], rest, &tunings[k]);
            values[k] = low;
            rest = high;
        }
        values[splits] = rest;
        // Each band below the top two through the all-passes of the splits
        // above its own upper one.
        for (band, compensation) in memory
            .compensation
            .iter_mut()
            .enumerate()
            .take(splits.saturating_sub(1))
        {
            for k in band + 1..splits {
                values[band] = self.all_pass(&mut compensation[k], values[band], &tunings[k]);
            }
        }
        values
    }

    /// One sample through one split: the low band and the high band.
    #[inline]
    fn split_once(&self, memory: &mut [[f64; 2]; 5], x: f64, tuning: &Tuning) -> (f64, f64) {
        let [k1, k2] = self.damping;
        let (low, all) = match self.slope {
            CrossoverSlope::Lr2 => {
                let first = one_pole(&mut memory[0][0], x, tuning.one_pole);
                let low = one_pole(&mut memory[1][0], first, tuning.one_pole);
                (low, 2.0 * first - x)
            }
            CrossoverSlope::Lr4 => {
                let (band, first) = svf(&mut memory[0], x, &tuning.svf[0]);
                let (_, low) = svf(&mut memory[1], first, &tuning.svf[0]);
                (low, x - 2.0 * k1 * band)
            }
            CrossoverSlope::Lr8 => {
                let (band, low) = svf(&mut memory[0], x, &tuning.svf[0]);
                let half = x - 2.0 * k1 * band;
                let (band, _) = svf(&mut memory[4], half, &tuning.svf[1]);
                let all = half - 2.0 * k2 * band;
                let (_, low) = svf(&mut memory[1], low, &tuning.svf[1]);
                let (_, low) = svf(&mut memory[2], low, &tuning.svf[0]);
                let (_, low) = svf(&mut memory[3], low, &tuning.svf[1]);
                (low, all)
            }
        };
        (low, all - low)
    }

    /// One sample through the all-pass of a split.
    #[inline]
    fn all_pass(&self, memory: &mut [[f64; 2]; 2], x: f64, tuning: &Tuning) -> f64 {
        let [k1, k2] = self.damping;
        match self.slope {
            CrossoverSlope::Lr2 => 2.0 * one_pole(&mut memory[0][0], x, tuning.one_pole) - x,
            CrossoverSlope::Lr4 => x - 2.0 * k1 * svf(&mut memory[0], x, &tuning.svf[0]).0,
            CrossoverSlope::Lr8 => {
                let half = x - 2.0 * k1 * svf(&mut memory[0], x, &tuning.svf[0]).0;
                half - 2.0 * k2 * svf(&mut memory[1], half, &tuning.svf[1]).0
            }
        }
    }
}

impl<S: Sample> Processor<S> for Crossover<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        for cutoff in &mut self.cutoffs {
            cutoff.prepare(spec.sample_rate);
        }
        self.memory = vec![Memory::SILENT; spec.num_channels];
        for band in &mut self.bands {
            band.resize(spec.num_channels, spec.max_block_size);
        }
    }

    /// Splits and adds the bands back up, which leaves the input passed
    /// through the crossover's all-pass. A block longer than the crossover
    /// was prepared for is handled in pieces.
    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let piece = self.bands.first().map_or(0, AudioBuffer::capacity).max(1);
        let frames = buffer.num_frames();
        let mut start = 0;
        while start < frames {
            let end = (start + piece).min(frames);
            buffer.windowed(start..end, |part| {
                self.split(part);
                self.sum_into(part);
            });
            start = end;
        }
    }

    fn reset(&mut self) {
        self.memory.fill(Memory::SILENT);
        for cutoff in &mut self.cutoffs {
            cutoff.snap();
        }
        for band in &mut self.bands {
            band.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;
    use crate::fft::{Complex, RealFft};
    use crate::numeric::{strict_buffer_peak, strict_peak};
    use crate::testing::{Random, db, largest_jump, noise, tail_peak};

    type C64 = Complex<f64>;

    const RATES: [f64; 4] = [44_100.0, 48_000.0, 96_000.0, 192_000.0];

    fn divide(a: C64, b: C64) -> C64 {
        let size = b.norm_sqr();
        C64::new(
            (a.re * b.re + a.im * b.im) / size,
            (a.im * b.re - a.re * b.im) / size,
        )
    }

    /// The Butterworth polynomial a slope squares, at `s`.
    fn butterworth(slope: CrossoverSlope, s: C64) -> C64 {
        let one = C64::new(1.0, 0.0);
        let quadratic = |k: f64| s * s + s.scale(k) + one;
        match slope {
            CrossoverSlope::Lr2 => s + one,
            CrossoverSlope::Lr4 => quadratic(SQRT_2),
            CrossoverSlope::Lr8 => {
                let [k1, k2] = butterworth_4_damping();
                quadratic(k1) * quadratic(k2)
            }
        }
    }

    /// The low band, the high band and the all-pass of a split at
    /// `crossover_hz`, at `frequency`, as the analogue formulas have them
    /// after the bilinear transform: `1 / B(s)²`, `(-s²)^N / B(s)²` and
    /// `B(-s) / B(s)`, with `s = j·tan(π·f/fs) / tan(π·fc/fs)`.
    fn theory(slope: CrossoverSlope, crossover_hz: f64, frequency: f64, rate: f64) -> [C64; 3] {
        let x = (PI * frequency / rate).tan() / (PI * crossover_hz / rate).tan();
        let s = C64::new(0.0, x);
        let one = C64::new(1.0, 0.0);
        let b = butterworth(slope, s);
        let mut high = one;
        for _ in 0..slope.butterworth_order() {
            high = high * -(s * s);
        }
        [
            divide(one, b * b),
            divide(high, b * b),
            divide(butterworth(slope, -s), b),
        ]
    }

    /// The impulse response of every band, from a crossover prepared for
    /// a block that long, checked to have died away.
    fn band_responses(crossover: &mut Crossover<f64>, rate: f64, len: usize) -> Vec<Vec<f64>> {
        crossover.prepare(&ProcessSpec {
            sample_rate: rate,
            max_block_size: len,
            num_channels: 1,
        });
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = 1.0;
        crossover.split(&impulse);
        (0..crossover.num_bands())
            .map(|band| {
                let response = crossover.band(band).channel(0).to_vec();
                assert!(tail_peak(&response, 256) < 1e-13, "band {band} is cut off");
                response
            })
            .collect()
    }

    /// The spectrum of a response: bin `k` is `k·rate / len`.
    fn spectrum(response: &[f64]) -> Vec<C64> {
        let mut fft = RealFft::<f64>::new(response.len());
        let mut bins = vec![C64::ZERO; fft.spectrum_len()];
        fft.forward(response, &mut bins);
        bins
    }

    fn sum(responses: &[Vec<f64>]) -> Vec<f64> {
        let mut total = vec![0.0; responses[0].len()];
        for response in responses {
            for (total, value) in total.iter_mut().zip(response) {
                *total += value;
            }
        }
        total
    }

    const LEN: usize = 1 << 16;

    #[test]
    fn two_bands_add_up_to_an_all_pass_flat_within_a_hundredth_of_a_decibel() {
        let mut flatness = 0.0f64;
        let mut band_error = 0.0f64;
        for rate in RATES {
            for slope in CrossoverSlope::ALL {
                for crossover_hz in [100.0, 1_000.0, 10_000.0] {
                    let mut crossover = Crossover::two_way(slope, crossover_hz);
                    let responses = band_responses(&mut crossover, rate, LEN);
                    let total = spectrum(&sum(&responses));
                    let bands: Vec<Vec<C64>> = responses.iter().map(|r| spectrum(r)).collect();
                    for (bin, measured) in total.iter().enumerate().step_by(16) {
                        let frequency = bin as f64 * rate / LEN as f64;
                        let [low, high, all] = theory(slope, crossover_hz, frequency, rate);
                        // The sum is the all-pass, phase and all.
                        assert!(
                            (*measured - all).norm() < 1e-9,
                            "{slope:?} {crossover_hz} Hz at {frequency} Hz"
                        );
                        flatness = flatness.max(db(measured.norm()).abs());
                        // And each band is what the formulas say.
                        band_error = band_error.max((bands[0][bin] - low).norm());
                        band_error = band_error.max((bands[1][bin] - high).norm());
                    }
                }
            }
        }
        // Flat within a hundredth of a decibel, and in fact within a
        // millionth of that.
        assert!(flatness < 1e-8, "{flatness} dB");
        // Each band is the formula to within 1e-13 in plain gain (the
        // largest seen was 6.7e-14, transform included), about -260 dB:
        // that is where the high band, a difference of two signals near 1
        // far below its crossover, stops being exact.
        assert!(band_error < 1e-13, "bands off by {band_error}");
    }

    #[test]
    fn each_band_is_six_decibels_down_at_the_crossover_and_the_two_are_in_phase() {
        const RATE: f64 = 48_000.0;
        for slope in CrossoverSlope::ALL {
            // A crossover frequency on a bin, so that it can be read off.
            let crossover_hz = 1_024.0 * RATE / LEN as f64 * 2.0;
            let mut crossover = Crossover::two_way(slope, crossover_hz);
            let responses = band_responses(&mut crossover, RATE, LEN);
            let (low, high) = (spectrum(&responses[0]), spectrum(&responses[1]));
            let at = 2_048;
            assert!((low[at].norm() - 0.5).abs() < 1e-9, "{slope:?}");
            assert!((high[at].norm() - 0.5).abs() < 1e-9, "{slope:?}");
            assert!((db(low[at].norm()) + 6.0206).abs() < 1e-4);
            // In phase everywhere: the sizes add up to the size of the sum.
            for bin in (1..low.len() - 1).step_by(7) {
                let sizes = low[bin].norm() + high[bin].norm();
                assert!(
                    (sizes - 1.0).abs() < 1e-9,
                    "{slope:?} at bin {bin}: {sizes}"
                );
            }
        }
    }

    #[test]
    fn the_bands_fall_twelve_twenty_four_and_forty_eight_decibels_per_octave() {
        const RATE: f64 = 192_000.0;
        let len = 1 << 17;
        for slope in CrossoverSlope::ALL {
            // 6.02 dB per octave for each of the 2N poles.
            let poles = 2 * slope.butterworth_order();
            let expected = 20.0 * 2f64.log10() * poles as f64;
            assert_eq!(slope.db_per_octave(), 6.0 * poles as f64);
            // Far above a crossover at 50 Hz (bins 546 and 1092, 800 and
            // 1600 Hz) and far below one at 4 kHz (bins 85 and 170, 125
            // and 250 Hz), where the bilinear transform bends the frequency
            // axis by less than 0.05 dB over an octave. Bins an exact octave
            // apart, so that the octave is exact.
            let mut low = Crossover::two_way(slope, 50.0);
            let low = spectrum(&band_responses(&mut low, RATE, len)[0]);
            let slope_db = db(low[1_092].norm()) - db(low[546].norm());
            assert!((slope_db + expected).abs() < 0.1, "{slope:?}: {slope_db}");
            let mut high = Crossover::two_way(slope, 4_000.0);
            let high = spectrum(&band_responses(&mut high, RATE, len)[1]);
            let slope_db = db(high[170].norm()) - db(high[85].norm());
            assert!((slope_db - expected).abs() < 0.1, "{slope:?}: {slope_db}");
        }
    }

    #[test]
    fn three_to_five_bands_add_up_flat_and_stay_in_phase() {
        const RATE: f64 = 48_000.0;
        let all_frequencies = [80.0, 500.0, 2_500.0, 9_000.0];
        for slope in CrossoverSlope::ALL {
            for bands in 3..=5 {
                let frequencies = &all_frequencies[..bands - 1];
                let mut crossover = Crossover::new(slope, frequencies);
                assert_eq!(crossover.num_bands(), bands);
                let responses = band_responses(&mut crossover, RATE, LEN);
                let total = spectrum(&sum(&responses));
                let spectra: Vec<Vec<C64>> = responses.iter().map(|r| spectrum(r)).collect();
                for bin in (1..total.len() - 1).step_by(13) {
                    let frequency = bin as f64 * RATE / LEN as f64;
                    // The sum: the product of every split's all-pass.
                    let mut all = C64::new(1.0, 0.0);
                    for &hz in frequencies {
                        all = all * theory(slope, hz, frequency, RATE)[2];
                    }
                    assert!((total[bin] - all).norm() < 1e-9, "{slope:?}, {bands} bands");
                    // Each band: the high bands of the splits below it and
                    // the low band of its own, in size.
                    for (band, measured) in spectra.iter().enumerate() {
                        let mut size = 1.0;
                        for (split, &hz) in frequencies.iter().enumerate() {
                            let [low, high, _] = theory(slope, hz, frequency, RATE);
                            if split < band {
                                size *= high.norm();
                            } else if split == band {
                                size *= low.norm();
                            }
                        }
                        let value = measured[bin];
                        assert!((value.norm() - size).abs() < 1e-9, "band {band}");
                        // In phase with the sum: their ratio is a positive
                        // real number.
                        if size > 1e-6 {
                            let ratio = divide(value, total[bin]);
                            assert!(
                                ratio.re > 0.0 && ratio.im.abs() < 1e-6 * ratio.re,
                                "band {band} at {frequency} Hz: {ratio:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn without_the_compensating_all_passes_the_sum_would_dip() {
        // Three bands without compensation: the low band misses the phase
        // turn of the upper split, and the sum is LP0 + AP1·HP0.
        const RATE: f64 = 48_000.0;
        let dip = |slope: CrossoverSlope, low_hz: f64, high_hz: f64| {
            let mut worst = 0.0f64;
            for i in 0..2_000 {
                let frequency = 20.0 * 1_000f64.powf(i as f64 / 2_000.0);
                let [low, high, _] = theory(slope, low_hz, frequency, RATE);
                let [_, _, upper] = theory(slope, high_hz, frequency, RATE);
                worst = worst.min(db((low + upper * high).norm()));
            }
            worst
        };
        let lr4 = dip(CrossoverSlope::Lr4, 500.0, 1_000.0);
        assert!((lr4 + 3.56).abs() < 0.01, "{lr4}");
        let lr4_wide = dip(CrossoverSlope::Lr4, 200.0, 2_000.0);
        assert!((lr4_wide + 0.113).abs() < 0.001, "{lr4_wide}");
        // With it, the same split is flat (above, to 1e-9 in gain).
        let mut crossover = Crossover::new(CrossoverSlope::Lr4, &[500.0, 1_000.0]);
        let total = spectrum(&sum(&band_responses(&mut crossover, RATE, LEN)));
        let worst = total
            .iter()
            .fold(0.0f64, |worst, bin| worst.max(db(bin.norm()).abs()));
        assert!(worst < 1e-8, "{worst}");
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        type Change = fn(&mut Crossover<f64>);
        let render = |block: usize| {
            let mut crossover =
                Crossover::<f64>::new(CrossoverSlope::Lr8, &[100.0, 1_000.0, 5_000.0]);
            crossover.prepare(&ProcessSpec {
                sample_rate: 48_000.0,
                max_block_size: 6_000,
                num_channels: 2,
            });
            let signal = noise::<f64>(2, 6_000, 12);
            let mut bands = vec![AudioBuffer::<f64>::new(2, 6_000); 4];
            // The same changes on the same frames, whatever the block size.
            let changes: [(usize, Change); 4] = [
                (700, |crossover| crossover.set_frequency(0, 300.0)),
                (1_000, |crossover| {
                    crossover.set_frequency(1, 3_000.0);
                    crossover.set_frequency(2, 2_000.0);
                }),
                (1_100, |crossover| crossover.set_frequency(0, 40.0)),
                (6_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                let mut at = start;
                while at < end {
                    let stop = (at + block).min(end);
                    let mut piece = signal.clone();
                    piece.set_num_frames(stop);
                    piece.windowed(at..stop, |part| crossover.split(part));
                    for (band, out) in bands.iter_mut().enumerate() {
                        for channel in 0..2 {
                            out.channel_mut(channel)[at..stop]
                                .copy_from_slice(crossover.band(band).channel(channel));
                        }
                    }
                    at = stop;
                }
                change(&mut crossover);
                start = end;
            }
            bands
        };
        let reference = render(512);
        assert!(strict_buffer_peak(&reference[3]) > 0.1);
        for block in [1, 7, 64, 333, 6_000] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    /// A tone of amplitude 0.5 through a two-way crossover at 48 kHz whose
    /// frequency is moved from `from` to `to` on frame 9600, by blocks of
    /// 64, until 2400 frames after it has arrived: the low band, the high
    /// band, their sum, and the largest step of the tone itself. With
    /// `paced` false the glide takes its 50 ms whatever the distance, as it
    /// would without the limit on its pace.
    fn glide(
        slope: CrossoverSlope,
        from: f64,
        to: f64,
        tone_hz: f64,
        paced: bool,
    ) -> [Vec<f64>; 4] {
        const RATE: f64 = 48_000.0;
        let mut crossover = Crossover::<f64>::two_way(slope, from);
        if !paced {
            crossover.cutoffs[0] =
                GlidingCutoff::new(from, LOWEST_HZ, HIGHEST_RATIO, GLIDE_SECONDS);
        }
        crossover.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 64,
            num_channels: 1,
        });
        let tone = |n: usize| 0.5 * (TAU * tone_hz * n as f64 / RATE).sin();
        let mut piece = AudioBuffer::<f64>::new(1, 64);
        let mut sum = AudioBuffer::<f64>::new(1, 64);
        let (mut low, mut high, mut total) = (Vec::new(), Vec::new(), Vec::new());
        let mut start = 0;
        let mut arrived = None;
        while arrived.is_none_or(|at| start < at + 2_400) {
            if start == 9_600 {
                crossover.set_frequency(0, to);
            }
            for (i, sample) in piece.channel_mut(0).iter_mut().enumerate() {
                *sample = tone(start + i);
            }
            crossover.split(&piece);
            low.extend_from_slice(crossover.band(0).channel(0));
            high.extend_from_slice(crossover.band(1).channel(0));
            crossover.sum_into(&mut sum);
            total.extend_from_slice(sum.channel(0));
            start += 64;
            if start > 9_600 && arrived.is_none() && !crossover.cutoffs[0].is_gliding() {
                arrived = Some(start);
            }
        }
        let tone: Vec<f64> = (0..start).map(tone).collect();
        [low, high, total, vec![largest_jump(&tone)]]
    }

    /// How far the peak of the sum rose above the tone's, in decibels, from
    /// frame 4800 on.
    fn rise(slope: CrossoverSlope, from: f64, to: f64, tone_hz: f64, paced: bool) -> f64 {
        let [_, _, total, _] = glide(slope, from, to, tone_hz, paced);
        db(strict_peak(&total[4_800..]) / 0.5)
    }

    /// The largest [`rise`] over tones from `high` down to `low` hertz,
    /// `steps` to the octave, then 1/36 octave apart around the worst of
    /// those, within a sixth of an octave.
    fn worst_rise(
        slope: CrossoverSlope,
        (from, to): (f64, f64),
        paced: bool,
        (low, high, steps): (f64, f64, i32),
    ) -> f64 {
        let at = |tone: f64| rise(slope, from, to, tone, paced);
        let (mut worst, mut centre) = (f64::NEG_INFINITY, high);
        let mut tone = high;
        for step in 1.. {
            if tone < low {
                break;
            }
            let rise = at(tone);
            if rise > worst {
                (worst, centre) = (rise, tone);
            }
            tone = high * 2f64.powf(-f64::from(step) / f64::from(steps));
        }
        for step in -6..=6 {
            let tone = centre * 2f64.powf(f64::from(step) / 36.0);
            if step != 0 && (low..=high).contains(&tone) {
                worst = worst.max(at(tone));
            }
        }
        worst
    }

    #[test]
    fn a_crossover_frequency_glides_without_a_click() {
        // A 300 Hz tone, with the crossover moved from above it to below
        // it: the tone passes from the low band to the high band, gliding.
        let [low, high, total, step] = glide(CrossoverSlope::Lr4, 1_000.0, 100.0, 300.0, true);
        for band in [&low, &high, &total] {
            let jump = largest_jump(band);
            assert!(
                jump < 1.15 * step[0],
                "a jump of {jump} against {}",
                step[0]
            );
        }
        assert!(strict_peak(&low[8_000..9_600]) > 0.45);
        let end = low.len() - 2_400;
        assert!(strict_peak(&low[end..]) < 0.01);
        assert!(strict_peak(&high[end..]) > 0.49);
        // Held still, the sum is flat (a sampled peak can only fall short
        // of the true one, here by at most 0.0004 dB).
        let still = rise(CrossoverSlope::Lr8, 300.0, 300.0, 300.0, true);
        assert!(still < 1e-9 && still > -1e-3, "{still} dB");
    }

    #[test]
    fn the_sum_of_a_gliding_crossover_rises_by_at_most_the_stated_amount() {
        // The figures the documentation quotes, in decibels: the largest
        // rise of the sum's peak with the glide held to its pace, for tones
        // from 5 kHz down to 50 Hz (1/3 octave apart) and the frequency
        // moved from 1 kHz to 100 Hz and back, and for tones from 20 kHz
        // down to 20 Hz (half an octave apart) and the whole range down;
        // and without the limits on the pace, for the whole range down
        // (tones 1/3 octave apart). Each slope on a thread of its own, to
        // keep the test quick.
        let measure = |slope: CrossoverSlope| {
            let middle = (50.0, 5_000.0, 3);
            let paced = worst_rise(slope, (1_000.0, 100.0), true, middle)
                .max(worst_rise(slope, (100.0, 1_000.0), true, middle))
                .max(worst_rise(
                    slope,
                    (20_000.0, 20.0),
                    true,
                    (20.0, 20_000.0, 2),
                ));
            let unpaced = worst_rise(slope, (20_000.0, 20.0), false, (20.0, 20_000.0, 3));
            (paced, unpaced)
        };
        let measured: Vec<(f64, f64)> = std::thread::scope(|scope| {
            let threads: Vec<_> = CrossoverSlope::ALL
                .into_iter()
                .map(|slope| scope.spawn(move || measure(slope)))
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });
        // About 0.05, 0.17 and 0.66 dB held to the pace; 2.7, 5.8 and
        // 8.8 dB without it.
        let stated = [(0.05, 2.7), (0.17, 5.8), (0.66, 8.8)];
        for ((paced, unpaced), (most, without)) in measured.iter().zip(stated) {
            assert!(
                *paced <= most + 0.01 && *paced > most - 0.02,
                "{measured:?}"
            );
            assert!((unpaced - without).abs() < 0.1, "{measured:?}");
        }
    }

    #[test]
    fn a_change_of_frequency_takes_the_stated_time() {
        let seconds = |from: f64, to: f64| {
            let mut crossover = Crossover::<f64>::two_way(CrossoverSlope::Lr4, from);
            crossover.prepare(&ProcessSpec {
                sample_rate: 48_000.0,
                max_block_size: 1,
                num_channels: 1,
            });
            crossover.set_frequency(0, to);
            let one = AudioBuffer::<f64>::new(1, 1);
            let mut frames = 0;
            while crossover.cutoffs[0].is_gliding() {
                crossover.split(&one);
                frames += 1;
            }
            frames as f64 / 48_000.0
        };
        // An octave above 500 Hz: the 50 ms.
        for (from, to) in [(500.0, 1_000.0), (2_000.0, 1_000.0), (10_000.0, 20_000.0)] {
            let time = seconds(from, to);
            assert!((time - 0.05).abs() < 1e-4, "{from} to {to}: {time} s");
        }
        // From 1 kHz to 100 Hz and back: 20 ms for the octave above 500 Hz
        // at 50 octaves per second, and 10·(1/100 - 1/500) / ln 2 seconds
        // for the rest at a tenth of an octave per period.
        let expected = 0.02 + 10.0 * (1.0 / 100.0 - 1.0 / 500.0) / 2f64.ln();
        for (from, to) in [(1_000.0, 100.0), (100.0, 1_000.0)] {
            let time = seconds(from, to);
            assert!(
                (time / expected - 1.0).abs() < 0.01,
                "{from} to {to}: {time} s"
            );
        }
        // The whole range down: 0.8 s.
        let time = seconds(20_000.0, 20.0);
        assert!((time - 0.8).abs() < 0.01, "{time} s");
    }

    #[test]
    fn it_stays_bounded_when_the_frequencies_are_thrown_about() {
        // Noise of peak 1, every frequency set anywhere every few samples.
        let mut random = Random::new(23);
        for slope in CrossoverSlope::ALL {
            for rate in [22_050.0, 192_000.0] {
                let mut crossover =
                    Crossover::<f64>::new(slope, &[100.0, 1_000.0, 5_000.0, 10_000.0]);
                crossover.prepare(&ProcessSpec {
                    sample_rate: rate,
                    max_block_size: 16,
                    num_channels: 1,
                });
                let mut worst = 0.0f64;
                let mut sum = AudioBuffer::<f64>::new(1, 16);
                for seed in 0..2_000 {
                    for index in 0..4 {
                        crossover.set_frequency(index, random.log_between(1.0, rate));
                    }
                    let block = noise::<f64>(1, 1 + seed % 16, seed as u64);
                    crossover.split(&block);
                    crossover.sum_into(&mut sum);
                    for band in 0..5 {
                        worst = worst.max(strict_buffer_peak(crossover.band(band)));
                    }
                    worst = worst.max(strict_buffer_peak(&sum));
                }
                assert!(worst < 4.0, "{slope:?} at {rate} Hz reached {worst}");
            }
        }
    }

    #[test]
    fn processing_in_place_leaves_the_all_pass_whatever_the_block() {
        const RATE: f64 = 48_000.0;
        let frequencies = [150.0, 1_500.0, 6_000.0];
        let mut crossover = Crossover::<f64>::new(CrossoverSlope::Lr8, &frequencies);
        // Prepared for blocks of 100, handed one of 65536.
        crossover.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 100,
            num_channels: 1,
        });
        let mut impulse = AudioBuffer::<f64>::new(1, LEN);
        impulse.channel_mut(0)[0] = 1.0;
        crossover.process(&mut impulse);
        let total = spectrum(impulse.channel(0));
        for bin in (0..total.len()).step_by(11) {
            let frequency = bin as f64 * RATE / LEN as f64;
            let mut all = C64::new(1.0, 0.0);
            for &hz in &frequencies {
                all = all * theory(CrossoverSlope::Lr8, hz, frequency, RATE)[2];
            }
            assert!((total[bin] - all).norm() < 1e-9);
        }
        // And split, which can hold only 100 frames, splits only those.
        let long = noise::<f64>(1, 1_000, 2);
        crossover.split(&long);
        assert_eq!(crossover.band(0).num_frames(), 100);
    }

    #[test]
    fn channels_are_independent_and_reset_clears_them() {
        let mut crossover = Crossover::<f32>::two_way(CrossoverSlope::Lr4, 200.0);
        crossover.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 32,
            num_channels: 2,
        });
        let mut buffer = AudioBuffer::<f32>::new(2, 32);
        buffer.channel_mut(0).fill(1.0);
        crossover.split(&buffer);
        assert!(crossover.band(0).channel(0)[31] > 0.0);
        assert_eq!(crossover.band(0).channel(1), &[0.0; 32]);
        assert_eq!(crossover.band(1).channel(1), &[0.0; 32]);

        crossover.reset();
        assert_eq!(strict_buffer_peak(crossover.band(0)), 0.0);
        buffer.clear();
        crossover.split(&buffer);
        assert_eq!(strict_buffer_peak(crossover.band(0)), 0.0);
        assert_eq!(strict_buffer_peak(crossover.band(1)), 0.0);
    }

    #[test]
    fn it_survives_garbage_and_use_before_it_is_prepared() {
        // Not prepared: nothing to split into, audio passes untouched, and
        // nothing panics.
        let mut crossover = Crossover::<f32>::new(CrossoverSlope::Lr2, &[100.0, 1_000.0]);
        assert_eq!(crossover.slope(), CrossoverSlope::Lr2);
        assert_eq!(crossover.num_bands(), 3);
        let mut buffer = noise::<f32>(2, 64, 1);
        let before = buffer.clone();
        crossover.process(&mut buffer);
        assert!(buffer == before);
        crossover.split(&buffer);
        assert_eq!(crossover.band(2).num_channels(), 0);

        // Frequencies are limited; ones it does not have are ignored.
        crossover.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 64,
            num_channels: 1,
        });
        for (value, expected) in [
            (f64::NAN, 1.0),
            (-5.0, 1.0),
            (f64::INFINITY, 0.49 * 48_000.0),
            (1e9, 0.49 * 48_000.0),
        ] {
            crossover.set_frequency(1, value);
            assert_eq!(crossover.frequency(1), Some(expected));
        }
        crossover.set_frequency(7, 300.0);
        assert_eq!(crossover.frequency(7), None);
        assert_eq!(crossover.frequency(0), Some(100.0));

        // More channels than prepared: left as they are by `process`.
        let mut wide = noise::<f32>(3, 64, 2);
        let before = wide.clone();
        crossover.process(&mut wide);
        assert_eq!(wide.channel(2), before.channel(2));
        assert!(strict_buffer_peak(&wide) < 10.0);

        // No frequency at all: one band, the input itself. More than four:
        // five bands.
        let mut single = Crossover::<f64>::new(CrossoverSlope::Lr4, &[]);
        single.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 64,
            num_channels: 1,
        });
        let input = noise::<f64>(1, 64, 3);
        single.split(&input);
        assert_eq!(single.num_bands(), 1);
        assert!(*single.band(0) == input);
        let many = Crossover::<f64>::new(CrossoverSlope::Lr4, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(many.num_bands(), Crossover::<f64>::MAX_BANDS);

        // A band can be processed in place before the bands are added up.
        let mut two = Crossover::<f64>::two_way(CrossoverSlope::Lr4, 1_000.0);
        two.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 64,
            num_channels: 1,
        });
        two.split(&input);
        two.band_mut(1).clear();
        let mut out = AudioBuffer::<f64>::new(1, 64);
        two.sum_into(&mut out);
        assert!(out == *two.band(0));
    }
}
