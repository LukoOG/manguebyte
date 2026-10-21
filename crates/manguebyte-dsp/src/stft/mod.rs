//! Spectral processing: the short-time Fourier transform, a change to each
//! spectrum, and weighted overlap-add back to sound.
//!
//! [`Stft`] buffers its input, and every `hop` samples it takes the newest
//! `fft_size` of them, multiplies them by the analysis window, transforms
//! them with the crate's [real FFT](crate::fft::RealFft), hands the spectrum
//! to a [`SpectralProcessor`], transforms the result back, multiplies it by
//! the synthesis window and adds it into the output. This is the weighted
//! overlap-add (WOLA) scheme of J. B. Allen and L. R. Rabiner, "A unified
//! approach to short-time Fourier analysis and synthesis", Proceedings of the
//! IEEE, 1977, and of D. W. Griffin and J. S. Lim, "Signal estimation from
//! modified short-time Fourier transform", IEEE Transactions on Acoustics,
//! Speech, and Signal Processing, 1984; J. O. Smith III, *Spectral Audio
//! Signal Processing* (CCRMA, online book), sets out the overlap-add
//! constraints it relies on.
//!
//! # Reconstruction
//!
//! Write `a` for the analysis window, `s` for the synthesis window, `N` for
//! the transform size and `R` for the hop. Each output sample is the sum of
//! the frames that overlap it, so an unchanged spectrum gives back the input
//! multiplied by
//!
//! ```text
//! W(c) = Σ a[c + kR] · s[c + kR]     (over every k with 0 ≤ c + kR < N)
//! ```
//!
//! where `c` is the sample's position within a hop. The windows and hop
//! satisfy the *weighted constant-overlap-add* condition when `W` is the
//! same for every `c`; with a rectangular synthesis window this is the plain
//! COLA condition on the analysis window alone. [`Stft::new`] computes `W`
//! in double precision and refuses, with
//! [`StftError::NotOverlapAdd`], windows whose `W` varies by more than
//! [`OVERLAP_ADD_TOLERANCE`] of its mean. It then divides the synthesis
//! window by `W(i mod R)`, so an unchanged spectrum reconstructs the input
//! exactly, up to the rounding of the transforms.
//!
//! Some pairs that pass, for transform size `N`:
//!
//! | Analysis | Synthesis | Hops | `W` before normalising |
//! |---|---|---|---|
//! | Hann | rectangular | `N/2`, `N/3`, `N/4`, ... | `N / (2R)` |
//! | Hann | Hann | `N/3`, `N/4`, ... (not `N/2`) | `3N / (8R)` |
//! | √Hann | √Hann | `N/2`, `N/3`, ... | `N / (2R)` |
//! | Hamming | rectangular | `N/2`, `N/3`, ... | `0.54 N / R` |
//! | Blackman | rectangular | `N/3`, `N/4`, ... | `0.42 N / R` |
//! | rectangular | rectangular | any divisor of `N` | `N / R` |
//!
//! A window that is also applied after the change, as a synthesis window,
//! fades out whatever the change spreads to the edges of the frame, which is
//! why changing a spectrum calls for one. The defaults are Hann on both
//! sides with a hop of a quarter of the transform.
//!
//! # Latency
//!
//! Frames are taken at fixed points of the stream, counted from
//! [`prepare`](crate::Processor::prepare) or
//! [`reset`](crate::Processor::reset), whatever blocks the host cuts it
//! into, so the output is the same bit for bit for any block sizes. Sample
//! `n` is complete once the last frame that overlaps it has been added. That
//! frame ends up to `N - 1` samples after `n`, less the samples at the start
//! of the synthesis window that are zero, which that frame cannot reach:
//!
//! ```text
//! latency = N - 1 - z      (z: leading zeros of the synthesis window)
//! ```
//!
//! So a rectangular synthesis window gives `N - 1`, and a periodic Hann or
//! √Hann window, whose first value is zero, `N - 2`. A latency of `N - R`,
//! often quoted, is only possible when the host's blocks are exactly one
//! hop long and line up with the frames.
//!
//! # Example
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::fft::Complex;
//! use manguebyte_dsp::stft::{SpectralFrame, Stft, StftSettings};
//! use manguebyte_dsp::{ProcessSpec, Processor};
//!
//! // A brick-wall low-pass: every bin above bin 64 of 1024 is cleared.
//! let settings = StftSettings::new(1_024).with_hop(256);
//! let mut lowpass = Stft::from_fn(settings, |frame: SpectralFrame<'_, f32>| {
//!     frame.bins[64..].fill(Complex::ZERO);
//! })
//! .unwrap();
//! assert_eq!(lowpass.latency(), 1_022);
//!
//! lowpass.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 512, num_channels: 2 });
//! let mut block = AudioBuffer::<f32>::new(2, 512);
//! lowpass.process(&mut block);
//! assert_eq!(lowpass.latency_samples(), 1_022);
//! ```
//!
//! Three effects built on it come with the module: [`SpectralGate`], a gate
//! per bin that can learn a noise profile and so serves as a simple
//! denoiser, [`SpectralFreeze`], which holds a spectrum, and [`PitchShift`],
//! a basic phase-vocoder pitch shifter.

use core::fmt;

use manguebyte_core::{AudioBuffer, Sample};

use crate::fft::{Complex, RealFft};
use crate::window::{Symmetry, Window};
use crate::{ProcessSpec, Processor};

mod freeze;
mod gate;
mod pitch;
#[cfg(test)]
mod tests;

pub use freeze::SpectralFreeze;
pub use gate::SpectralGate;
pub use pitch::{MAX_PITCH_RATIO, MIN_PITCH_RATIO, PitchShift};

/// Largest transform an [`Stft`] accepts.
pub const MAX_FFT_SIZE: usize = 1 << 16;

/// How far the overlapped windows may stray from a constant sum, as a
/// fraction of their mean, before [`Stft::new`] refuses them.
///
/// Windows that satisfy the condition in exact arithmetic stray by about
/// `1e-15` once computed; ones that do not stray by far more than this.
pub const OVERLAP_ADD_TOLERANCE: f64 = 1e-9;

/// The shape of an analysis or synthesis window, always taken in its
/// periodic form (see [`Symmetry::Periodic`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StftWindow {
    /// The window itself.
    Shape(Window),
    /// The square root of the window, so that the same window used for
    /// analysis and synthesis multiplies to the original: `SquareRoot(Hann)`
    /// on both sides overlap-adds like Hann alone.
    SquareRoot(Window),
}

impl From<Window> for StftWindow {
    fn from(window: Window) -> Self {
        Self::Shape(window)
    }
}

impl StftWindow {
    /// The window's value at `index` out of `len` samples, or `None` if it
    /// is not finite or is the square root of a negative value.
    fn value(self, index: usize, len: usize) -> Option<f64> {
        let value = match self {
            Self::Shape(window) => window.value(index, len, Symmetry::Periodic),
            Self::SquareRoot(window) => {
                let value = window.value(index, len, Symmetry::Periodic);
                if value < 0.0 {
                    return None;
                }
                value.sqrt()
            }
        };
        value.is_finite().then_some(value)
    }
}

/// How an [`Stft`] cuts the signal into frames: transform size, hop and
/// windows.
///
/// ```
/// use manguebyte_dsp::stft::{StftSettings, StftWindow};
/// use manguebyte_dsp::window::Window;
///
/// // Root-Hann on both sides at half overlap.
/// let settings = StftSettings::new(2_048)
///     .with_hop(1_024)
///     .with_windows(StftWindow::SquareRoot(Window::Hann), StftWindow::SquareRoot(Window::Hann));
/// assert_eq!(settings.hop, 1_024);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct StftSettings {
    /// Samples per frame, which is also the transform size: any length from
    /// 1 to [`MAX_FFT_SIZE`]. Powers of two are the fastest. The spectrum
    /// has `fft_size / 2 + 1` bins, `sample_rate / fft_size` apart.
    pub fft_size: usize,
    /// Samples from one frame to the next, from 1 to `fft_size`. A quarter
    /// of the transform by default.
    pub hop: usize,
    /// The window each frame is multiplied by before it is transformed.
    /// Hann by default.
    pub analysis: StftWindow,
    /// The window each frame is multiplied by after it is transformed back,
    /// before it is added to the output. Hann by default.
    pub synthesis: StftWindow,
}

impl StftSettings {
    /// Frames of `fft_size` samples, a hop of a quarter of that, and Hann
    /// windows for analysis and synthesis.
    pub fn new(fft_size: usize) -> Self {
        let hann = StftWindow::Shape(Window::Hann);
        Self {
            fft_size,
            hop: (fft_size / 4).max(1),
            analysis: hann,
            synthesis: hann,
        }
    }

    /// Sets the hop, in samples.
    #[must_use]
    pub fn with_hop(mut self, hop: usize) -> Self {
        self.hop = hop;
        self
    }

    /// Sets the analysis window.
    #[must_use]
    pub fn with_analysis(mut self, window: impl Into<StftWindow>) -> Self {
        self.analysis = window.into();
        self
    }

    /// Sets the synthesis window. [`Window::Rectangular`] means plain
    /// overlap-add, with no window after the change.
    #[must_use]
    pub fn with_synthesis(mut self, window: impl Into<StftWindow>) -> Self {
        self.synthesis = window.into();
        self
    }

    /// Sets both windows.
    #[must_use]
    pub fn with_windows(
        self,
        analysis: impl Into<StftWindow>,
        synthesis: impl Into<StftWindow>,
    ) -> Self {
        self.with_analysis(analysis).with_synthesis(synthesis)
    }
}

impl Default for StftSettings {
    /// Frames of 2048 samples; see [`new`](Self::new).
    fn default() -> Self {
        Self::new(2_048)
    }
}

/// Why an [`Stft`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum StftError {
    /// The transform size is not from 1 to [`MAX_FFT_SIZE`].
    FftSize(usize),
    /// The hop is zero or longer than the transform.
    Hop {
        /// The hop asked for.
        hop: usize,
        /// The transform size.
        fft_size: usize,
    },
    /// A window has a value that is not finite, or is the square root of a
    /// window with negative values (such as [`Window::FlatTop`]).
    InvalidWindow,
    /// The analysis and synthesis windows, overlapped at this hop, do not
    /// add up to a constant, so the output would be the input modulated at
    /// the frame rate.
    NotOverlapAdd {
        /// The transform size.
        fft_size: usize,
        /// The hop.
        hop: usize,
        /// How far the overlapped sum strays, from its smallest to its
        /// largest value, as a fraction of its mean. Infinite when the sum
        /// is zero somewhere.
        ripple: f64,
    },
}

impl fmt::Display for StftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FftSize(size) => write!(
                f,
                "transform size {size} is not from 1 to {MAX_FFT_SIZE} samples"
            ),
            Self::Hop { hop, fft_size } => write!(
                f,
                "hop {hop} is not from 1 to the transform size of {fft_size} samples"
            ),
            Self::InvalidWindow => write!(
                f,
                "a window value is not finite, or a square-root window has negative values"
            ),
            Self::NotOverlapAdd {
                fft_size,
                hop,
                ripple,
            } => write!(
                f,
                "the analysis and synthesis windows do not overlap-add to a constant \
                 at a hop of {hop} in frames of {fft_size} (the sum strays by {:.3}% of \
                 its mean); Hann on both sides needs a hop of fft_size / 3 or less, \
                 Hann with a rectangular synthesis window a hop of fft_size / 2 or less",
                ripple * 100.0
            ),
        }
    }
}

impl std::error::Error for StftError {}

/// What a [`SpectralProcessor`] is told in
/// [`prepare`](SpectralProcessor::prepare) about the frames to come.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct StftLayout {
    /// Samples per frame and transform size.
    pub fft_size: usize,
    /// Samples from one frame to the next.
    pub hop: usize,
    /// Bins in each spectrum: `fft_size / 2 + 1`.
    pub bins: usize,
    /// Channels, each with its own frames.
    pub channels: usize,
    /// Sample rate in hertz.
    pub sample_rate: f64,
    /// Multiplies a bin's magnitude into the amplitude of a sine centred on
    /// that bin: `2 / Σ analysis window`. (Bin 0 and, for even sizes, the
    /// last bin read twice their amplitude.)
    pub amplitude_scale: f64,
}

impl StftLayout {
    /// The layout of `fft_size`-sample frames every `hop` samples of
    /// `channels` channels, at `sample_rate`, with an analysis window whose
    /// values add up to `analysis_sum`. An [`Stft`] works this out itself;
    /// this is for testing a [`SpectralProcessor`] on its own.
    pub fn new(
        fft_size: usize,
        hop: usize,
        channels: usize,
        sample_rate: f64,
        analysis_sum: f64,
    ) -> Self {
        Self {
            fft_size,
            hop,
            bins: fft_size / 2 + 1,
            channels,
            sample_rate,
            amplitude_scale: 2.0 / analysis_sum,
        }
    }

    /// The frequency at the centre of `bin`, in hertz.
    pub fn bin_frequency(&self, bin: usize) -> f64 {
        bin as f64 * self.sample_rate / self.fft_size as f64
    }
}

/// One channel's spectrum of one frame, handed to a [`SpectralProcessor`]
/// to change in place.
#[derive(Debug)]
#[non_exhaustive]
pub struct SpectralFrame<'a, S: Sample = f32> {
    /// The channel the frame belongs to.
    pub channel: usize,
    /// Frames since [`prepare`](crate::Processor::prepare) or
    /// [`reset`](crate::Processor::reset); every channel's frames at the same
    /// point of the stream share an index.
    pub index: u64,
    /// The bins, from 0 Hz to half the sample rate. The forward transform is
    /// unscaled, so a sine of amplitude `A` centred on a bin reads
    /// `A / amplitude_scale` there (see [`StftLayout::amplitude_scale`]).
    pub bins: &'a mut [Complex<S>],
}

impl<'a, S: Sample> SpectralFrame<'a, S> {
    /// A frame of `bins` for `channel`, numbered `index`. An [`Stft`] makes
    /// these itself; this is for testing a [`SpectralProcessor`] on its own.
    pub fn new(channel: usize, index: u64, bins: &'a mut [Complex<S>]) -> Self {
        Self {
            channel,
            index,
            bins,
        }
    }
}

/// A change to a spectrum, applied by an [`Stft`] to every frame.
///
/// # Real-time contract
///
/// [`prepare`](Self::prepare) may allocate.
/// [`process_frame`](Self::process_frame) and [`reset`](Self::reset) run on
/// the audio thread and must not allocate, lock or do I/O.
///
/// For each frame the channels come in order, 0 first. An effect that keeps
/// state from one frame to the next keeps it per channel.
pub trait SpectralProcessor<S: Sample = f32> {
    /// Gets ready for frames laid out as `layout` says. Called by
    /// [`Stft`]'s own `prepare`.
    fn prepare(&mut self, layout: &StftLayout) {
        let _ = layout;
    }

    /// Changes one channel's spectrum in place.
    fn process_frame(&mut self, frame: SpectralFrame<'_, S>);

    /// Forgets what earlier frames left behind.
    fn reset(&mut self) {}
}

/// A [`SpectralProcessor`] made from a closure, by [`Stft::from_fn`].
#[derive(Debug, Clone)]
pub struct SpectralFn<F>(pub F);

impl<S: Sample, F: FnMut(SpectralFrame<'_, S>)> SpectralProcessor<S> for SpectralFn<F> {
    fn process_frame(&mut self, frame: SpectralFrame<'_, S>) {
        (self.0)(frame);
    }
}

/// A short-time Fourier transform processor: analysis, a change to each
/// spectrum, and weighted overlap-add resynthesis.
///
/// See the [module documentation](self) for the conditions on the windows,
/// the normalisation and the latency.
///
/// Building one plans the transform and computes the windows. Its buffers
/// are sized in [`prepare`](Processor::prepare), for the spec's channel
/// count; processing allocates nothing.
///
/// # Channels
///
/// Channels of a block beyond those prepared for are left as they are,
/// neither changed nor delayed. Channels the block lacks are taken as
/// silent, so every channel stays in step.
pub struct Stft<P, S: Sample = f32> {
    fft_size: usize,
    hop: usize,
    latency: usize,
    /// Index of the first non-zero value of the synthesis window.
    first_synthesis: usize,
    analysis_sum: f64,
    analysis: Vec<S>,
    /// The synthesis window divided by the overlapped sum of both windows.
    synthesis: Vec<S>,
    fft: RealFft<S>,
    frame: Vec<S>,
    spectrum: Vec<Complex<S>>,
    channels: usize,
    sample_rate: f64,
    /// Per channel, the last `fft_size` input samples, as a ring indexed by
    /// time modulo `fft_size`.
    input: Vec<S>,
    /// Per channel, the output summed so far, as a ring indexed the same way.
    output: Vec<S>,
    /// Where in the rings the next sample goes.
    position: usize,
    /// Samples since the last frame.
    fill: usize,
    /// Frames since `prepare` or `reset`.
    frames: u64,
    processor: P,
}

impl<P, S: Sample> fmt::Debug for Stft<P, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stft")
            .field("fft_size", &self.fft_size)
            .field("hop", &self.hop)
            .field("latency", &self.latency)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl<F, S: Sample> Stft<SpectralFn<F>, S>
where
    F: FnMut(SpectralFrame<'_, S>),
{
    /// An STFT processor that changes each spectrum with a closure.
    ///
    /// # Errors
    ///
    /// As [`new`](Stft::new).
    pub fn from_fn(settings: StftSettings, change: F) -> Result<Self, StftError> {
        Self::new(settings, SpectralFn(change))
    }
}

impl<P: SpectralProcessor<S>, S: Sample> Stft<P, S> {
    /// An STFT processor that changes each spectrum with `processor`.
    /// Allocates; call it off the audio thread.
    ///
    /// # Errors
    ///
    /// Fails if the transform size is not from 1 to [`MAX_FFT_SIZE`], if
    /// the hop is not from 1 to the transform size, if a window is not
    /// finite or is the square root of negative values, or if the windows
    /// do not overlap-add to a constant at this hop (see the
    /// [module documentation](self)).
    pub fn new(settings: StftSettings, processor: P) -> Result<Self, StftError> {
        let StftSettings {
            fft_size,
            hop,
            analysis,
            synthesis,
        } = settings;
        if !(1..=MAX_FFT_SIZE).contains(&fft_size) {
            return Err(StftError::FftSize(fft_size));
        }
        if !(1..=fft_size).contains(&hop) {
            return Err(StftError::Hop { hop, fft_size });
        }
        let table = |window: StftWindow| -> Result<Vec<f64>, StftError> {
            (0..fft_size)
                .map(|index| window.value(index, fft_size))
                .collect::<Option<Vec<f64>>>()
                .ok_or(StftError::InvalidWindow)
        };
        let analysis = table(analysis)?;
        let synthesis = table(synthesis)?;

        // The overlapped sum of the product, at each position in a hop.
        let sums: Vec<f64> = (0..hop)
            .map(|offset| {
                (offset..fft_size)
                    .step_by(hop)
                    .map(|index| analysis[index] * synthesis[index])
                    .sum()
            })
            .collect();
        let mean = sums.iter().sum::<f64>() / hop as f64;
        let (low, high) = sums
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &sum| {
                (low.min(sum), high.max(sum))
            });
        let ripple = if mean > 0.0 && low > 0.0 {
            (high - low) / mean
        } else {
            f64::INFINITY
        };
        // Finite, or infinite where a sum is zero: the windows are finite.
        if ripple > OVERLAP_ADD_TOLERANCE {
            return Err(StftError::NotOverlapAdd {
                fft_size,
                hop,
                ripple,
            });
        }

        // Every position has a non-zero sum, so the synthesis window is not
        // zero everywhere.
        let first_synthesis = synthesis
            .iter()
            .position(|&value| value != 0.0)
            .unwrap_or(0);
        let normalised: Vec<S> = synthesis
            .iter()
            .enumerate()
            .map(|(index, &value)| S::from_f64(value / sums[index % hop]))
            .collect();

        Ok(Self {
            fft_size,
            hop,
            latency: fft_size - 1 - first_synthesis,
            first_synthesis,
            analysis_sum: analysis.iter().sum(),
            analysis: analysis.iter().map(|&value| S::from_f64(value)).collect(),
            synthesis: normalised,
            fft: RealFft::new(fft_size),
            frame: vec![S::ZERO; fft_size],
            spectrum: vec![Complex::ZERO; fft_size / 2 + 1],
            channels: 0,
            sample_rate: 48_000.0,
            input: Vec::new(),
            output: Vec::new(),
            position: 0,
            fill: 0,
            frames: 0,
            processor,
        })
    }

    /// Samples per frame and transform size.
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Samples from one frame to the next.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// How many samples late the output is: the transform size, less one,
    /// less the leading zeros of the synthesis window. An unchanged spectrum
    /// gives back the input delayed by exactly this.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// The layout the spectral processor was last prepared with.
    pub fn layout(&self) -> StftLayout {
        StftLayout::new(
            self.fft_size,
            self.hop,
            self.channels,
            self.sample_rate,
            self.analysis_sum,
        )
    }

    /// The spectral processor.
    pub fn processor(&self) -> &P {
        &self.processor
    }

    /// The spectral processor, to change its settings. Real-time safe as
    /// far as the processor's own setters are.
    pub fn processor_mut(&mut self) -> &mut P {
        &mut self.processor
    }

    /// Gives back the spectral processor.
    pub fn into_processor(self) -> P {
        self.processor
    }

    /// Forgets all past input and output. Real-time safe.
    pub fn clear(&mut self) {
        self.input.fill(S::ZERO);
        self.output.fill(S::ZERO);
        self.position = 0;
        self.fill = 0;
        self.frames = 0;
        self.processor.reset();
    }

    /// Processes a block in place. Real-time safe.
    pub fn process_block(&mut self, buffer: &mut AudioBuffer<S>) {
        let frames = buffer.num_frames();
        let present = buffer.num_channels().min(self.channels);
        let size = self.fft_size;
        let mut start = 0;
        while start < frames {
            // Up to the end of the block or the next frame, whichever is
            // first.
            let count = (self.hop - self.fill).min(frames - start);
            let completes = self.fill + count == self.hop;

            for (channel, ring) in self.input.chunks_exact_mut(size).enumerate() {
                let mut slot = self.position;
                if channel < present {
                    for &sample in &buffer.channel(channel)[start..start + count] {
                        ring[slot] = sample;
                        slot = if slot + 1 == size { 0 } else { slot + 1 };
                    }
                } else {
                    for _ in 0..count {
                        ring[slot] = S::ZERO;
                        slot = if slot + 1 == size { 0 } else { slot + 1 };
                    }
                }
            }

            // The sample that completes a hop is read out after its frame
            // has been added.
            let before = if completes { count - 1 } else { count };
            self.emit(buffer, present, start, before, self.position);
            if completes {
                let newest = (self.position + count - 1) % size;
                self.run_frame(newest);
                self.emit(buffer, present, start + before, 1, newest);
                self.fill = 0;
            } else {
                self.fill += count;
            }
            self.position = (self.position + count) % size;
            start += count;
        }
    }

    /// Writes `count` samples of output from `offset` in the block, for the
    /// input samples that went to ring slots from `slot` on, and clears what
    /// it read.
    fn emit(
        &mut self,
        buffer: &mut AudioBuffer<S>,
        present: usize,
        offset: usize,
        count: usize,
        slot: usize,
    ) {
        let size = self.fft_size;
        let first = (slot + size - self.latency) % size;
        for (channel, ring) in self.output.chunks_exact_mut(size).enumerate() {
            let mut read = first;
            if channel < present {
                for out in &mut buffer.channel_mut(channel)[offset..offset + count] {
                    *out = core::mem::replace(&mut ring[read], S::ZERO);
                    read = if read + 1 == size { 0 } else { read + 1 };
                }
            } else {
                for _ in 0..count {
                    ring[read] = S::ZERO;
                    read = if read + 1 == size { 0 } else { read + 1 };
                }
            }
        }
    }

    /// Analyses, changes and overlap-adds the frame that ends at ring slot
    /// `newest`, for every channel.
    fn run_frame(&mut self, newest: usize) {
        let Self {
            fft_size: size,
            first_synthesis,
            analysis,
            synthesis,
            fft,
            frame,
            spectrum,
            input,
            output,
            frames,
            processor,
            ..
        } = self;
        let size = *size;
        let oldest = if newest + 1 == size { 0 } else { newest + 1 };
        for (channel, (ring, sum)) in input
            .chunks_exact(size)
            .zip(output.chunks_exact_mut(size))
            .enumerate()
        {
            let samples = ring[oldest..].iter().chain(&ring[..oldest]);
            for ((value, &sample), &weight) in frame.iter_mut().zip(samples).zip(analysis.iter()) {
                *value = sample * weight;
            }
            fft.forward(frame, spectrum);
            processor.process_frame(SpectralFrame::new(channel, *frames, spectrum));
            fft.inverse(spectrum, frame);

            // Frame index `i` lands on ring slot `oldest + i`. The leading
            // zeros of the synthesis window are skipped: the output they
            // would touch has already been read.
            let mut slot = (oldest + *first_synthesis) % size;
            for (&value, &weight) in frame[*first_synthesis..]
                .iter()
                .zip(&synthesis[*first_synthesis..])
            {
                sum[slot] += value * weight;
                slot = if slot + 1 == size { 0 } else { slot + 1 };
            }
        }
        *frames += 1;
    }
}

impl<P: SpectralProcessor<S>, S: Sample> Processor<S> for Stft<P, S> {
    /// Sizes the buffers for the spec's channels, prepares the spectral
    /// processor and clears everything. Allocates when the channel count
    /// grows.
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.channels = spec.num_channels;
        self.sample_rate = spec.sample_rate;
        let len = self.channels * self.fft_size;
        self.input.resize(len, S::ZERO);
        self.output.resize(len, S::ZERO);
        let layout = self.layout();
        self.processor.prepare(&layout);
        self.clear();
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.process_block(buffer);
    }

    fn reset(&mut self) {
        self.clear();
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }
}

/// Brings a phase into `-π..=π`.
#[inline]
pub(crate) fn wrap_phase(phase: f64) -> f64 {
    use core::f64::consts::TAU;
    phase - TAU * (phase / TAU).round()
}
