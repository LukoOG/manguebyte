//! Pitch detection by YIN and by the McLeod pitch method.

use core::f64::consts::PI;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::fft::{Complex, RealFft};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::input::{Source, for_each_sample};
use crate::note::Note;
use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, usable_rate};

/// Half the length of the band-limited interpolation kernel, in lags.
const KERNEL_HALF_WIDTH: usize = 32;

/// The length of the band-limited interpolation kernel, in lags.
const TAPS: usize = 2 * KERNEL_HALF_WIDTH;

/// The Kaiser parameter of the interpolation kernel's window.
const KERNEL_BETA: f64 = 9.0;

/// Terms kept of the power series of the kernel's window, `I0(beta sqrt(u))
/// = sum c_k u^k` with `c_k = (beta^2 / 4)^k / (k!)^2`. For `beta = 9` the
/// first term left out is below 1e-21, and the sum is at least 1.
const WINDOW_TERMS: usize = 28;

/// Golden-section steps of the band-limited refinement. Each keeps 0.618
/// of the bracket, which starts two lags wide: 30 leave about 1e-6 lags,
/// a ten-thousandth of a cent at the shortest period of 24 samples.
const SEARCH_STEPS: usize = 30;

/// A frame whose mean square is below this, about -200 dBFS, is silence.
const SILENCE_MEAN_SQUARE: f64 = 1e-20;

/// The longest period searched, in samples: 0.37 Hz at 48 kHz.
const MAX_PERIOD: usize = 1 << 17;

/// The pitch detection algorithm and its one parameter.
///
/// A later version of this crate may add methods: a match on this enum
/// needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum PitchMethod {
    /// YIN: A. de Cheveigné and H. Kawahara, "YIN, a fundamental frequency
    /// estimator for speech and music", Journal of the Acoustical Society
    /// of America 111(4), 2002.
    ///
    /// The squared difference between the signal and itself delayed by a
    /// lag is small at the period. YIN normalises it by its running mean
    /// (the cumulative mean normalised difference, step 3 of the paper) and
    /// takes the shortest lag whose dip goes below `threshold` (step 4). If
    /// there is none it takes the deepest dip. The confidence is one minus
    /// the normalised difference at the bottom of the dip, the paper's
    /// aperiodicity.
    ///
    /// Each dip is judged by its bottom as interpolated between whole lags,
    /// not by its lowest whole lag (see [`PitchDetector`]), and only dips
    /// whose lowest whole lag lies within the frequency limits count. When
    /// there is none, or when no dip goes below the threshold and the
    /// function goes deeper just outside the limits than in any dip inside
    /// them, there is no pitch.
    Yin {
        /// The absolute threshold of step 4, from 0 to 1. A lower one
        /// asks for a clearer period before taking the first dip; 0.15 by
        /// default.
        threshold: f64,
    },
    /// The McLeod pitch method: P. McLeod and G. Wyvill, "A smarter way to
    /// find pitch", Proceedings of the International Computer Music
    /// Conference, 2005.
    ///
    /// The normalised square difference function is the autocorrelation
    /// divided by the energy of the overlapping parts, so it is 1 at a
    /// perfect period. The method takes the highest point of each positive
    /// lobe after the first (the key maxima) and picks the first whose
    /// height is at least `cutoff` times the highest. The confidence is
    /// that height, the paper's clarity.
    ///
    /// Heights are interpolated between whole lags before they are
    /// compared (see [`PitchDetector`]), and only maxima whose whole lag
    /// lies within the frequency limits count: when there is none, there is
    /// no pitch.
    Mpm {
        /// The fraction of the highest key maximum the chosen one must
        /// reach, from 0 to 1; 0.9 by default.
        cutoff: f64,
    },
}

impl PitchMethod {
    /// YIN with its default threshold, 0.15.
    pub const fn yin() -> Self {
        Self::Yin { threshold: 0.15 }
    }

    /// The McLeod pitch method with its default cutoff, 0.9.
    pub const fn mpm() -> Self {
        Self::Mpm { cutoff: 0.9 }
    }

    fn sanitised(self) -> Self {
        match self {
            Self::Yin { threshold } if threshold.is_nan() => Self::yin(),
            Self::Yin { threshold } => Self::Yin {
                threshold: threshold.clamp(0.0, 1.0),
            },
            Self::Mpm { cutoff } if cutoff.is_nan() => Self::mpm(),
            Self::Mpm { cutoff } => Self::Mpm {
                cutoff: cutoff.clamp(0.0, 1.0),
            },
        }
    }
}

impl Default for PitchMethod {
    /// YIN with its default threshold.
    fn default() -> Self {
        Self::yin()
    }
}

/// How the chosen period is placed between two whole lags.
///
/// This decides only where the period of the chosen dip or peak is put,
/// and, with [`BandLimited`](Self::BandLimited), how exactly its depth or
/// height is measured. Which dip or peak is chosen does not depend on it:
/// candidates are always compared by their band-limited interpolation (see
/// [`PitchDetector`]).
///
/// A later version of this crate may add ways: a match on this enum needs
/// a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum LagInterpolation {
    /// A parabola through the best lag and its two neighbours, as both
    /// papers do. Cheap, and exact for a sine, but a lag function with
    /// many harmonics is sharper than a parabola at its extremum: on a
    /// sawtooth, a square wave or a tone without its fundamental near
    /// 2 kHz at 44.1 or 48 kHz this errs by up to 5.3 cents (see the
    /// tests), and more at lower sample rates.
    Parabolic,
    /// The lag function interpolated as the band-limited function it is,
    /// and its extremum searched for between the two neighbours.
    ///
    /// The products of a signal with itself delayed by a fraction of a
    /// sample are made of the same frequencies as the signal, so they can
    /// be interpolated from whole lags with a windowed sinc (64 taps,
    /// Kaiser window with parameter 9, normalised to unit gain at zero
    /// frequency), and a golden-section search finds the extremum to a
    /// millionth of a lag. On the same tones this keeps the error within
    /// 1.8 cents at 44.1 and 48 kHz; [`PitchDetector`] lists other rates.
    /// It costs about 2,000 kernel taps per analysis more than the
    /// parabola. It is the default.
    #[default]
    BandLimited,
}

/// How a [`PitchDetector`] analyses: method, frequency limits, hop,
/// interpolation and source.
///
/// ```
/// use manguebyte_analysis::{LagInterpolation, PitchMethod, PitchSettings};
///
/// let settings = PitchSettings::new(PitchMethod::mpm())
///     .with_range(80.0, 1_000.0)
///     .with_hop(128)
///     .with_interpolation(LagInterpolation::Parabolic);
/// assert_eq!(settings.min_frequency, 80.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PitchSettings {
    /// The algorithm.
    pub method: PitchMethod,
    /// The lowest fundamental searched for, in hertz; 50 Hz by default. It
    /// sets the longest period and, through it, the length of the frame
    /// analysed: about two of those periods.
    ///
    /// The longest period is rounded up to whole samples and is at most
    /// 131072 samples, so a lower limit below `sample_rate / 131072`
    /// (0.37 Hz at 48 kHz, 76.3 Hz at 10 MHz) is raised to it.
    /// [`PitchDetector::frequency_range`] reports the limits in use.
    pub min_frequency: f64,
    /// The highest fundamental searched for, in hertz; 2 kHz by default.
    ///
    /// The shortest period is rounded down to whole samples and is at
    /// least 2, so an upper limit above half the sample rate is lowered to
    /// it. See [`PitchDetector::frequency_range`].
    pub max_frequency: f64,
    /// Samples from one analysis to the next, at least 1; 256 by default.
    pub hop: usize,
    /// How the period is placed between whole lags.
    pub interpolation: LagInterpolation,
    /// Which channel, or the mean of all of them, is analysed.
    pub source: Source,
}

impl PitchSettings {
    /// Settings for `method`, with the defaults listed on each field.
    pub fn new(method: PitchMethod) -> Self {
        Self {
            method,
            min_frequency: 50.0,
            max_frequency: 2_000.0,
            hop: 256,
            interpolation: LagInterpolation::BandLimited,
            source: Source::Mean,
        }
    }

    /// Sets the lowest and the highest fundamental searched for, in hertz.
    pub fn with_range(mut self, min_frequency: f64, max_frequency: f64) -> Self {
        self.min_frequency = min_frequency;
        self.max_frequency = max_frequency;
        self
    }

    /// Sets the hop, in samples.
    pub fn with_hop(mut self, hop: usize) -> Self {
        self.hop = hop;
        self
    }

    /// Sets how the period is placed between whole lags.
    pub fn with_interpolation(mut self, interpolation: LagInterpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Sets which channel, or the mean of all, is analysed.
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    fn sanitised(mut self) -> Self {
        let usable = |frequency: f64, fallback: f64| {
            if frequency.is_finite() && frequency > 0.0 {
                frequency
            } else {
                fallback
            }
        };
        let low = usable(self.min_frequency, 50.0);
        let high = usable(self.max_frequency, 2_000.0);
        (self.min_frequency, self.max_frequency) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        self.hop = self.hop.max(1);
        self.method = self.method.sanitised();
        self
    }
}

impl Default for PitchSettings {
    /// YIN from 50 Hz to 2 kHz; see [`new`](Self::new).
    fn default() -> Self {
        Self::new(PitchMethod::yin())
    }
}

/// A pitch estimate: a fundamental frequency and how sure the detector is
/// of it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pitch {
    /// The fundamental frequency in hertz, or 0 when the detector found no
    /// period: before its first analysis, in silence, or when the method
    /// has no candidate period within the limits (see [`PitchMethod`]).
    pub frequency: f64,
    /// From 0 to 1: how periodic the frame is at that frequency. With the
    /// default band-limited interpolation, steady tones read above 0.97
    /// with either method and white noise below 0.2, but noise whose power
    /// lies at low frequencies can read up to about 0.7 (see
    /// [`PitchDetector`]). 0 when there is no frequency.
    pub confidence: f64,
}

impl Pitch {
    /// No pitch: frequency and confidence both 0.
    pub const NONE: Self = Self {
        frequency: 0.0,
        confidence: 0.0,
    };

    /// Whether this is no pitch at all.
    pub fn is_none(&self) -> bool {
        !(self.frequency.is_finite() && self.frequency > 0.0)
    }

    /// The nearest note, with A4 at 440 Hz, or `None` without a pitch.
    pub fn note(&self) -> Option<Note> {
        Note::nearest(self.frequency)
    }
}

/// Frequency and confidence in one atomic word, so that a reader never
/// sees the frequency of one analysis with the confidence of another.
fn pack(pitch: Pitch) -> u64 {
    (u64::from((pitch.frequency as f32).to_bits()) << 32)
        | u64::from((pitch.confidence as f32).to_bits())
}

fn unpack(bits: u64) -> Pitch {
    Pitch {
        frequency: f64::from(f32::from_bits((bits >> 32) as u32)),
        confidence: f64::from(f32::from_bits(bits as u32)),
    }
}

#[derive(Debug)]
struct PitchShared {
    estimate: AtomicU64,
    analyses: AtomicU64,
    reset: Flag,
}

/// The readings of a [`PitchDetector`], for any thread.
///
/// Cloning is cheap and every clone reads the same detector. Nothing here
/// locks or allocates. The frequency and the confidence are published
/// together in one atomic word, in single precision.
#[derive(Debug, Clone)]
pub struct PitchReadings {
    shared: Arc<PitchShared>,
}

impl PitchReadings {
    /// The newest estimate.
    pub fn pitch(&self) -> Pitch {
        unpack(self.shared.estimate.load(Ordering::Relaxed))
    }

    /// How many analyses the detector has made since its last reset.
    pub fn analysis_count(&self) -> u64 {
        self.shared.analyses.load(Ordering::Relaxed)
    }

    /// Asks the detector to start again. It does so at the start of the
    /// next block it measures, on the audio thread.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }
}

/// The sizes everything is allocated with, which follow from the settings
/// and the sample rate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geometry {
    /// The shortest and the longest period searched, in samples.
    shortest: usize,
    longest: usize,
    /// Samples per analysis.
    frame: usize,
    /// YIN's integration window, in samples.
    window: usize,
    /// The transform size.
    fft: usize,
}

impl Geometry {
    fn new(settings: &PitchSettings, sample_rate: f64) -> Self {
        // The casts saturate; the limits keep everything allocatable.
        let shortest =
            ((sample_rate / settings.max_frequency).floor() as usize).clamp(2, MAX_PERIOD - 2);
        let longest = ((sample_rate / settings.min_frequency).ceil() as usize)
            .clamp(shortest + 2, MAX_PERIOD);
        // YIN compares a window of `longest` samples with itself delayed
        // by up to `longest` lags, and the interpolation reaches a kernel
        // width further on either side. The McLeod method uses the whole
        // frame, so even its longest lag overlaps half of it.
        let frame = 2 * longest + 2 * KERNEL_HALF_WIDTH + 1;
        let fft = match settings.method {
            // The cross-correlation of the window with the frame only needs
            // the frame not to wrap around.
            PitchMethod::Yin { .. } => frame.next_power_of_two(),
            // An autocorrelation needs twice the frame to be free of
            // wrapped terms.
            PitchMethod::Mpm { .. } => (2 * frame - 1).next_power_of_two(),
        };
        Self {
            shortest,
            longest,
            frame,
            window: longest,
            fft,
        }
    }

    /// Lags kept: from `-KERNEL_HALF_WIDTH` to `longest + KERNEL_HALF_WIDTH + 1`.
    fn lags(&self) -> usize {
        self.longest + 2 * KERNEL_HALF_WIDTH + 2
    }
}

/// A pitch detector: YIN or the McLeod pitch method, with the lag function
/// computed through the FFT, sub-sample interpolation, frequency limits, a
/// confidence, and blocks of any size.
///
/// Every [`hop`](PitchSettings::hop) samples, once a frame has filled, the
/// detector analyses the last [`frame_len`](Self::frame_len) samples: about
/// two periods of the lowest frequency searched. The estimate is published
/// to [`PitchReadings`] at the end of the block and is available here from
/// [`latest`](Self::latest), in double precision.
///
/// ```
/// use manguebyte_analysis::{PitchDetector, PitchSettings};
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut detector = PitchDetector::new(PitchSettings::default());
/// detector.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 512, num_channels: 1 });
/// let readings = detector.readings().clone();
///
/// // A tenth of a second of 220 Hz, the A below middle C.
/// let tone: Vec<f32> = (0..4_800)
///     .map(|n| (core::f32::consts::TAU * 220.0 * n as f32 / 48_000.0).sin())
///     .collect();
/// detector.measure_samples(&tone);
///
/// let pitch = readings.pitch();
/// assert!((pitch.frequency - 220.0).abs() < 0.05);
/// assert!(pitch.confidence > 0.9);
/// let note = pitch.note().unwrap();
/// assert_eq!(note.to_string(), "A3");
/// assert!(note.cents().abs() < 0.5);
/// ```
///
/// # How a period is chosen
///
/// Both methods compare candidates: YIN the dips of its lag function, the
/// McLeod method its peaks. Near the shortest periods a dip or a peak of a
/// tone rich in harmonics can be only two samples wide, so its value at the
/// nearest whole lag says little about how deep or high it goes. Judged
/// there, the dip at one period can miss a threshold that the dip at two
/// periods reaches, and the detector takes the octave below with full
/// confidence. So every candidate is judged by its extremum between whole
/// lags: the lag function is read, band-limited, half way between whole
/// lags (two dot products with a 64-tap kernel computed once), and a
/// parabola through the most extreme of those points, half a lag apart,
/// estimates the extremum. This does not depend on the
/// [interpolation](PitchSettings::interpolation) setting, which decides
/// only how the chosen period is placed.
///
/// # Accuracy
///
/// The tests synthesise sines, sawtooths, square waves, tones without
/// their fundamental and tones whose second harmonic is two and three
/// times as strong as the fundamental, all with every harmonic up to half
/// the sample rate. With the default settings they are tried at every
/// semitone from 55 Hz to 2 kHz, every 7 to 10 Hz from 1.4 to 2 kHz, and
/// at random frequencies in between. The largest errors measured with the
/// band-limited interpolation, with either method:
///
/// | Sample rate | Sines | Tones rich in harmonics |
/// |---|---|---|
/// | 8 kHz | 1.3 cents | 13.3 cents |
/// | 16 kHz | 0.2 cents | 5.7 cents |
/// | 22.05 kHz | 0.2 cents | 3.5 cents |
/// | 44.1 and 48 kHz | 0.2 cents | 1.8 cents |
/// | 96 kHz (every other semitone) | 0.2 cents | 0.2 cents |
///
/// The error grows with the share of a tone's energy in harmonics close to
/// half the sample rate: the interpolation kernel reproduces frequencies
/// up to 80% of half the sample rate, and is less exact above that. There
/// are no octave errors on any of these tones, at any of these rates, with
/// either method. Parabolic interpolation was tried at 44.1 and 48 kHz: no
/// octave errors either, and errors of up to 5.3 cents on the tones rich in
/// harmonics.
///
/// With the band-limited interpolation these tones read a confidence above
/// 0.97. White noise reads below 0.2, pink noise below 0.4. Noise whose
/// power lies at low frequencies changes
/// slowly and can look nearly periodic over a frame: white noise
/// low-passed at 200 Hz read up to 0.63 and brown noise up to 0.69 in the
/// tests, at 48 kHz with either method. A confidence that is to mean "a
/// note is playing" has to lie above what the expected background reads.
///
/// # Limits
///
/// Only periods between the two frequency limits are considered, as
/// [`frequency_range`](Self::frequency_range) reports them, so a tone above
/// the highest is reported at a subharmonic that lies inside them. A tone
/// below the lowest is reported at a harmonic inside them if its spectrum
/// makes one look periodic, and otherwise not at all: neither method
/// reports the end of the range for it. The interpolated period can lie up
/// to one sample beyond either limit.
///
/// A second harmonic much stronger than the fundamental makes half the
/// period look like a period. YIN takes the octave above once the second
/// harmonic is more than `sqrt(2 / threshold - 1)` times as strong as the
/// fundamental (3.5 times, 10.9 dB, with the default threshold), and the
/// McLeod method once it is more than `sqrt((1 + cutoff) / (1 - cutoff))`
/// times as strong (4.4 times, 12.8 dB, with the default cutoff). Some
/// voices and instruments have a second harmonic that strong. A lower
/// threshold or a higher cutoff moves the limit up.
///
/// A tone without its fundamental is only recognised as such while at
/// least two of its harmonics lie below half the sample rate: a single one
/// is a sine, and says nothing about the fundamental.
///
/// The estimate describes a whole frame, so it lags behind the signal by
/// about half a frame: 21 ms at 48 kHz with the default lowest frequency of
/// 50 Hz. A frame that holds a change of note gives one estimate between
/// the two, or neither, with a lower confidence.
///
/// The McLeod method's lag function is lifted by a constant offset in the
/// signal, which can hide its zero crossings; YIN's is not. Block a DC
/// offset upstream (for example with
/// [`DcBlocker`](manguebyte_dsp::DcBlocker)) when using it.
///
/// Neither method is told about inharmonicity, vibrato within a frame or
/// several simultaneous notes; it finds one period. YIN's step 6, the
/// search for a better estimate in neighbouring frames, is not implemented.
///
/// Input is screened as for every meter in the crate (see the
/// [crate documentation](crate#bad-samples)).
#[derive(Debug)]
pub struct PitchDetector {
    settings: PitchSettings,
    sample_rate: f64,
    geometry: Geometry,
    fft: RealFft<f64>,
    history: Box<[f64]>,
    /// Where the next sample goes in `history`, which is also where the
    /// oldest one is.
    write: usize,
    /// Samples still to come before the next analysis.
    until_analysis: usize,
    frame: Box<[f64]>,
    scratch: Box<[f64]>,
    spectrum: Box<[Complex<f64>]>,
    reference: Box<[Complex<f64>]>,
    /// Running sums of the squared samples of the frame: entry `i` is the
    /// sum of the first `i`.
    squares: Box<[f64]>,
    /// The lag function, from `-KERNEL_HALF_WIDTH` on.
    lags: Box<[f64]>,
    /// YIN's cumulative mean normalised difference, from lag 0 to the last
    /// lag kept.
    normalised: Box<[f64]>,
    /// YIN's running sums of the difference: entry `t` is the sum from lag
    /// 1 to lag `t`.
    cumulative: Box<[f64]>,
    /// The McLeod method's key maxima of one analysis: whole lag and
    /// interpolated height.
    maxima: Box<[(usize, f64)]>,
    /// The interpolation kernel for half a lag, computed once.
    half_kernel: [f64; TAPS],
    latest: Pitch,
    analyses: u64,
    published: u64,
    readings: PitchReadings,
}

impl PitchDetector {
    /// Creates a detector. This allocates for 44.1 kHz, the rate assumed
    /// until [`prepare`](Self::prepare) is called; nothing after it does,
    /// except `prepare`.
    ///
    /// Settings that make no sense are made usable: frequency limits that
    /// are not positive numbers take their defaults and are swapped if in
    /// the wrong order, the hop is at least 1, and thresholds are kept
    /// within 0 to 1.
    pub fn new(settings: PitchSettings) -> Self {
        let settings = settings.sanitised();
        let geometry = Geometry::new(&settings, DEFAULT_SAMPLE_RATE);
        let mut detector = Self {
            settings,
            sample_rate: DEFAULT_SAMPLE_RATE,
            geometry,
            fft: RealFft::new(geometry.fft),
            history: Box::default(),
            write: 0,
            until_analysis: 0,
            frame: Box::default(),
            scratch: Box::default(),
            spectrum: Box::default(),
            reference: Box::default(),
            squares: Box::default(),
            lags: Box::default(),
            normalised: Box::default(),
            cumulative: Box::default(),
            maxima: Box::default(),
            half_kernel: kernel(0.5),
            latest: Pitch::NONE,
            analyses: 0,
            published: u64::MAX,
            readings: PitchReadings {
                shared: Arc::new(PitchShared {
                    estimate: AtomicU64::new(pack(Pitch::NONE)),
                    analyses: AtomicU64::new(0),
                    reset: Flag::default(),
                }),
            },
        };
        detector.allocate();
        detector.reset();
        detector
    }

    /// Takes the sample rate from `spec`, allocates for it and starts
    /// again.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        let geometry = Geometry::new(&self.settings, self.sample_rate);
        if geometry != self.geometry {
            self.geometry = geometry;
            self.fft = RealFft::new(geometry.fft);
            self.allocate();
        }
        self.reset();
    }

    fn allocate(&mut self) {
        let geometry = self.geometry;
        let bins = geometry.fft / 2 + 1;
        self.history = vec![0.0; geometry.frame].into_boxed_slice();
        self.frame = vec![0.0; geometry.fft].into_boxed_slice();
        self.scratch = vec![0.0; geometry.fft].into_boxed_slice();
        self.spectrum = vec![Complex::ZERO; bins].into_boxed_slice();
        self.reference = vec![Complex::ZERO; bins].into_boxed_slice();
        self.squares = vec![0.0; geometry.frame + 1].into_boxed_slice();
        self.lags = vec![0.0; geometry.lags()].into_boxed_slice();
        // YIN normalises every lag it has, from 0 to the last one kept.
        let positive_lags = geometry.lags() - KERNEL_HALF_WIDTH;
        self.normalised = vec![1.0; positive_lags].into_boxed_slice();
        self.cumulative = vec![0.0; positive_lags].into_boxed_slice();
        // Each key maximum has its own whole lag within the limits.
        self.maxima = vec![(0, 0.0); geometry.longest + 1].into_boxed_slice();
    }

    /// Forgets everything heard so far and publishes no pitch. Real-time
    /// safe.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.write = 0;
        self.until_analysis = self.geometry.frame;
        self.latest = Pitch::NONE;
        self.analyses = 0;
        self.publish();
    }

    /// Analyses one block, taking the samples its
    /// [`source`](PitchSettings::source) says. Real-time safe.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        if self.readings.shared.reset.take() {
            self.reset();
        }
        let source = self.settings.source;
        source.for_each(buffer, |sample| self.push(sample));
        if self.published != self.analyses {
            self.publish();
        }
    }

    /// Analyses a stream of samples given directly, ignoring
    /// [`source`](PitchSettings::source). Real-time safe.
    pub fn measure_samples<S: Sample>(&mut self, samples: &[S]) {
        if self.readings.shared.reset.take() {
            self.reset();
        }
        for_each_sample(samples, |sample| self.push(sample));
        if self.published != self.analyses {
            self.publish();
        }
    }

    /// The newest estimate, in double precision.
    pub fn latest(&self) -> Pitch {
        self.latest
    }

    /// The readings, to read here or to clone for another thread.
    pub fn readings(&self) -> &PitchReadings {
        &self.readings
    }

    /// The settings as the detector uses them, after any correction.
    pub fn settings(&self) -> &PitchSettings {
        &self.settings
    }

    /// The sample rate in use, in hertz.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// How many samples each analysis looks at.
    pub fn frame_len(&self) -> usize {
        self.geometry.frame
    }

    /// The shortest and the longest period searched, in whole samples.
    pub fn period_range(&self) -> (usize, usize) {
        (self.geometry.shortest, self.geometry.longest)
    }

    /// The lowest and the highest fundamental searched for, in hertz: the
    /// limits of the [settings](Self::settings) as the detector applies
    /// them at the sample rate in use.
    ///
    /// They differ from the settings in three ways. The periods are whole
    /// samples, rounded so that the range only grows. The longest period is
    /// at most 131072 samples, which raises the lowest frequency at high
    /// sample rates: the default 50 Hz becomes 76.3 Hz at 10 MHz. And the
    /// shortest period is at least 2 samples, which lowers the highest
    /// frequency to half the sample rate.
    ///
    /// ```
    /// use manguebyte_analysis::{PitchDetector, PitchSettings};
    /// use manguebyte_dsp::ProcessSpec;
    ///
    /// let mut detector = PitchDetector::new(PitchSettings::default());
    /// detector.prepare(&ProcessSpec { sample_rate: 44_100.0, max_block_size: 512, num_channels: 1 });
    /// // 44100 / 50 = 882 samples; 44100 / 2000 = 22.05, rounded down to 22.
    /// assert_eq!(detector.period_range(), (22, 882));
    /// assert_eq!(detector.frequency_range(), (50.0, 44_100.0 / 22.0));
    /// ```
    pub fn frequency_range(&self) -> (f64, f64) {
        let Geometry {
            shortest, longest, ..
        } = self.geometry;
        (
            self.sample_rate / longest as f64,
            self.sample_rate / shortest as f64,
        )
    }

    /// How many analyses have been made since the last reset.
    pub fn analysis_count(&self) -> u64 {
        self.analyses
    }

    fn publish(&mut self) {
        let shared = &self.readings.shared;
        shared.estimate.store(pack(self.latest), Ordering::Relaxed);
        shared.analyses.store(self.analyses, Ordering::Relaxed);
        self.published = self.analyses;
    }

    #[inline]
    fn push(&mut self, sample: f64) {
        if let Some(slot) = self.history.get_mut(self.write) {
            *slot = sample;
        }
        self.write += 1;
        if self.write == self.history.len() {
            self.write = 0;
        }
        self.until_analysis -= 1;
        if self.until_analysis == 0 {
            self.until_analysis = self.settings.hop;
            self.analyse();
        }
    }

    fn analyse(&mut self) {
        // The frame, oldest sample first, then zeros to the transform size.
        let (newer, older) = self.history.split_at(self.write);
        let mut samples = older.iter().chain(newer);
        for slot in self.frame.iter_mut() {
            *slot = samples.next().copied().unwrap_or(0.0);
        }
        let mut running = 0.0;
        for (sum, &sample) in self.squares.iter_mut().skip(1).zip(self.frame.iter()) {
            running += sample * sample;
            *sum = running;
        }
        self.latest = match self.settings.method {
            PitchMethod::Yin { threshold } => self.yin(threshold),
            PitchMethod::Mpm { cutoff } => self.mpm(cutoff),
        };
        self.analyses += 1;
    }

    /// The sum of the squares of `len` frame samples from `start` on.
    fn energy(&self, start: usize, len: usize) -> f64 {
        energy(&self.squares, start, len)
    }

    /// The lag function of the last analysis, to read.
    #[cfg(test)]
    fn lag_function(&self) -> LagFunction<'_> {
        LagFunction {
            values: &self.lags,
            half: &self.half_kernel,
        }
    }

    fn yin(&mut self, threshold: f64) -> Pitch {
        let Geometry {
            shortest,
            longest,
            window,
            ..
        } = self.geometry;
        let k = KERNEL_HALF_WIDTH;
        // The window compared with its delayed copies starts `k` samples
        // into the frame, so that lags down to `-k` can be formed.
        let reference_energy = self.energy(k, window);
        if reference_energy <= window as f64 * SILENCE_MEAN_SQUARE {
            return Pitch::NONE;
        }

        // Step 2 of the paper, through the FFT:
        // d(t) = sum (x[j] - x[j + t])^2
        //      = energy of the window + energy of the delayed window
        //        - 2 * cross-correlation at lag t,
        // and the cross-correlation of the window with the whole frame is
        // the inverse transform of conj(W) X.
        self.scratch.fill(0.0);
        if let (Some(target), Some(source)) = (
            self.scratch.get_mut(k..k + window),
            self.frame.get(k..k + window),
        ) {
            target.copy_from_slice(source);
        }
        self.fft.forward(&self.frame, &mut self.spectrum);
        self.fft.forward(&self.scratch, &mut self.reference);
        for (value, reference) in self.spectrum.iter_mut().zip(self.reference.iter()) {
            *value = reference.conj() * *value;
        }
        self.fft.inverse(&self.spectrum, &mut self.scratch);
        let correlations = &self.scratch;
        let squares = &self.squares;
        let size = correlations.len();
        for (index, slot) in self.lags.iter_mut().enumerate() {
            // `index` is the lag plus `k`, and also where the delayed window
            // starts in the frame. A negative lag sits at the end of the
            // circular correlation.
            let correlation = if index >= k {
                correlations.get(index - k)
            } else {
                correlations.get(size - (k - index))
            }
            .copied()
            .unwrap_or(0.0);
            let difference = reference_energy + energy(squares, index, window) - 2.0 * correlation;
            // Rounding can leave a hair below zero where it is zero.
            *slot = difference.max(0.0);
        }

        // Step 3: the cumulative mean normalised difference.
        let differences = self.lags.iter().skip(k);
        let mut running = 0.0;
        for (tau, ((slot, sum), &difference)) in self
            .normalised
            .iter_mut()
            .zip(self.cumulative.iter_mut())
            .zip(differences)
            .enumerate()
        {
            // The running sum starts at lag 1; lag 0 is defined as 1.
            if tau > 0 {
                running += difference;
            }
            *sum = running;
            *slot = if tau > 0 && running > 0.0 {
                difference * tau as f64 / running
            } else {
                1.0
            };
        }

        // Step 4: the first dip that goes below the threshold, or failing
        // that the deepest one. A dip is a local minimum at whole lags, and
        // it is judged by its bottom as interpolated between them: near the
        // shortest periods a dip can be a sample or two wide, and its whole
        // lags can all lie above the threshold while its bottom is far
        // below it.
        let normalised = |tau: usize| self.normalised.get(tau).copied().unwrap_or(1.0);
        let cumulative = &self.cumulative;
        // The normalised difference at the fractional lag `position` near
        // the whole lag `tau`, from the difference there.
        let aperiodicity = |position: f64, difference: f64, tau: usize| {
            let sum = cumulative.get(tau).copied().unwrap_or(0.0);
            if sum > 0.0 {
                difference.max(0.0) * position / sum
            } else {
                1.0
            }
        };
        let lags = LagFunction {
            values: &self.lags,
            half: &self.half_kernel,
        };
        let mut first_below = None;
        let mut deepest: Option<(usize, f64)> = None;
        for tau in shortest..=longest {
            let value = normalised(tau);
            let dip = value < normalised(tau - 1) && value <= normalised(tau + 1);
            if !dip {
                continue;
            }
            let (position, bottom) = lags.estimate(tau, false);
            let depth = aperiodicity(position, bottom, tau);
            if depth < threshold {
                first_below = Some((tau, depth));
                break;
            }
            if deepest.is_none_or(|(_, deepest)| depth < deepest) {
                deepest = Some((tau, depth));
            }
        }
        // The paper falls back on the deepest point of the function. It is
        // known here from `KERNEL_HALF_WIDTH` lags before the shortest
        // period to as many after the longest. If it lies outside the
        // limits, the period it stands for is outside them too, and there
        // is no pitch inside: a tone below the lowest frequency, for
        // example, leaves only a slope down to the longest period (with
        // ripples or flat stretches on it if the tone is rich in harmonics)
        // and its dip beyond.
        let below = shortest.saturating_sub(KERNEL_HALF_WIDTH).max(1)..shortest;
        let above = longest + 1..=longest + KERNEL_HALF_WIDTH + 1;
        let lowest_outside = below
            .chain(above)
            .map(normalised)
            .fold(f64::INFINITY, f64::min);
        let deepest = deepest.filter(|&(_, depth)| depth <= lowest_outside);
        let Some((best, estimated)) = first_below.or(deepest) else {
            return Pitch::NONE;
        };

        // Step 5, on the raw difference, whose minimum the normalisation
        // would pull towards shorter lags.
        let interpolation = self.settings.interpolation;
        let (period, bottom) = lags.refine(best, false, interpolation);
        let depth = match interpolation {
            LagInterpolation::BandLimited => aperiodicity(period, bottom, best),
            // The estimate is closer to the band-limited bottom than the
            // parabola through whole lags.
            LagInterpolation::Parabolic => estimated,
        };
        Pitch {
            frequency: self.sample_rate / period,
            confidence: (1.0 - depth).clamp(0.0, 1.0),
        }
    }

    fn mpm(&mut self, cutoff: f64) -> Pitch {
        let Geometry {
            shortest,
            longest,
            frame,
            ..
        } = self.geometry;
        let k = KERNEL_HALF_WIDTH;
        let total = self.energy(0, frame);
        if total <= frame as f64 * SILENCE_MEAN_SQUARE {
            return Pitch::NONE;
        }

        // The autocorrelation r(t) = sum x[j] x[j + t] over the overlap is
        // the inverse transform of |X|^2, the frame being zero-padded to
        // twice its length. The normalised square difference function is
        // n(t) = 2 r(t) / m(t), with m(t) the energy of both overlapping
        // parts, as the paper defines it.
        self.fft.forward(&self.frame, &mut self.spectrum);
        for value in self.spectrum.iter_mut() {
            *value = Complex::new(value.norm_sqr(), 0.0);
        }
        self.fft.inverse(&self.spectrum, &mut self.scratch);
        let last = longest + k + 1;
        let squares = &self.squares;
        let (negative, positive) = self.lags.split_at_mut(k);
        for (tau, (slot, &correlation)) in positive.iter_mut().zip(self.scratch.iter()).enumerate()
        {
            let len = frame.saturating_sub(tau);
            let overlap = energy(squares, 0, len) + energy(squares, tau, len);
            *slot = if overlap > 0.0 {
                (2.0 * correlation / overlap).clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        // The function is even; the interpolation reaches below lag zero.
        // `negative` holds lags -k to -1, `positive` lags 0 on.
        for (slot, &value) in negative.iter_mut().rev().zip(positive.iter().skip(1)) {
            *slot = value;
        }

        // Key maxima within the limits, each with its height interpolated
        // between whole lags: near the shortest periods a peak can be a
        // sample or two wide, and its whole lags can miss the cutoff that
        // its top reaches. The highest, then the first that reaches
        // `cutoff` times it.
        let lags = LagFunction {
            values: &self.lags,
            half: &self.half_kernel,
        };
        let count = lags.key_maxima(shortest, longest, last, &mut self.maxima);
        let maxima = self.maxima.get(..count).unwrap_or(&[]);
        let highest = maxima
            .iter()
            .fold(f64::NEG_INFINITY, |highest, &(_, height)| {
                highest.max(height)
            });
        if highest.is_nan() || highest <= 0.0 {
            return Pitch::NONE;
        }
        let Some(&(best, estimated)) = maxima
            .iter()
            .find(|&&(_, height)| height >= cutoff * highest)
        else {
            return Pitch::NONE;
        };
        let interpolation = self.settings.interpolation;
        let (period, height) = lags.refine(best, true, interpolation);
        let clarity = match interpolation {
            LagInterpolation::BandLimited => height,
            // The estimate is closer to the band-limited top than the
            // parabola through whole lags.
            LagInterpolation::Parabolic => estimated,
        };
        Pitch {
            frequency: self.sample_rate / period,
            confidence: clarity.clamp(0.0, 1.0),
        }
    }
}

/// A lag function of one analysis, from lag `-KERNEL_HALF_WIDTH` on, with
/// the means to read it between whole lags.
#[derive(Debug, Clone, Copy)]
struct LagFunction<'a> {
    values: &'a [f64],
    /// The interpolation kernel for half a lag.
    half: &'a [f64; TAPS],
}

impl LagFunction<'_> {
    /// The function at the whole lag `lag`.
    fn at(self, lag: usize) -> f64 {
        self.values
            .get(lag + KERNEL_HALF_WIDTH)
            .copied()
            .unwrap_or(0.0)
    }

    /// The function half a lag after the whole lag `lag`: the same as
    /// [`interpolate`](Self::interpolate) there, with the kernel computed
    /// once instead of at every call.
    fn after_half(self, lag: usize) -> f64 {
        // `interpolate` at `lag + 0.5` weighs the whole lags from
        // `lag + 1 - K` to `lag + K`, which are stored from `lag + 1` on.
        match self.values.get(lag + 1..lag + 1 + TAPS) {
            Some(values) => values
                .iter()
                .zip(self.half.iter())
                .map(|(value, weight)| value * weight)
                .sum(),
            None => self.interpolate(lag as f64 + 0.5),
        }
    }

    /// The function at a fractional `lag`, by windowed-sinc interpolation
    /// of the whole lags around it.
    fn interpolate(self, lag: f64) -> f64 {
        let position = lag + KERNEL_HALF_WIDTH as f64;
        let whole = position.floor();
        let fraction = position - whole;
        let base = whole as usize;
        let at = |index: usize| self.values.get(index).copied().unwrap_or(0.0);
        if fraction == 0.0 {
            return at(base);
        }
        kernel(fraction)
            .iter()
            .enumerate()
            .map(|(tap, weight)| weight * at((base + tap + 1).wrapping_sub(KERNEL_HALF_WIDTH)))
            .sum()
    }

    /// The extremum next to the whole lag `centre`, which is a local
    /// extremum at whole lags (a maximum if `maximum`, else a minimum):
    /// its position and value, estimated at a cost low enough to spend on
    /// every candidate.
    ///
    /// The function is read at the whole lags either side of `centre` and,
    /// band-limited, half way between them. A parabola goes through the
    /// most extreme of the three middle points and its two neighbours,
    /// half a lag apart. Half a lag apart, the parabola is close to the
    /// band-limited function even where a peak is only two lags wide, as
    /// the peaks of a tone rich in harmonics near its shortest period are.
    fn estimate(self, centre: usize, maximum: bool) -> (f64, f64) {
        let sign = if maximum { 1.0 } else { -1.0 };
        let [far_before, before, middle, after, far_after] = [
            self.at(centre - 1),
            self.after_half(centre - 1),
            self.at(centre),
            self.after_half(centre),
            self.at(centre + 1),
        ]
        .map(|value| sign * value);
        let (shift, (offset, value)) = if before > middle && before >= after {
            (-0.5, vertex(far_before, before, middle))
        } else if after > middle {
            (0.5, vertex(middle, after, far_after))
        } else {
            (0.0, vertex(before, middle, after))
        };
        (centre as f64 + shift + 0.5 * offset, sign * value)
    }

    /// The position and value of the extremum next to the whole lag
    /// `centre` (a maximum if `maximum`, else a minimum), placed as
    /// `interpolation` says.
    fn refine(self, centre: usize, maximum: bool, interpolation: LagInterpolation) -> (f64, f64) {
        let whole = centre as f64;
        match interpolation {
            LagInterpolation::Parabolic => {
                let (offset, value) =
                    vertex(self.at(centre - 1), self.at(centre), self.at(centre + 1));
                (whole + offset, value)
            }
            LagInterpolation::BandLimited => {
                let sign = if maximum { -1.0 } else { 1.0 };
                let objective = |lag: f64| sign * self.interpolate(lag);
                let period = golden_section(whole - 1.0, whole + 1.0, objective);
                (period, self.interpolate(period))
            }
        }
    }

    /// Writes into `maxima` the key maxima whose whole lag lies within
    /// `shortest..=longest`, looking at lags up to `last`, and returns how
    /// many there are.
    ///
    /// A key maximum is the highest point of a positive lobe after the one
    /// around lag zero. A lobe counts if its highest whole lag lies within
    /// the limits, as in the paper; a lobe that rises beyond them has its
    /// top outside. Its key maximum is then the local maximum at whole lags
    /// whose [estimated](Self::estimate) top is the highest in the lobe,
    /// written with that height.
    fn key_maxima(
        self,
        shortest: usize,
        longest: usize,
        last: usize,
        maxima: &mut [(usize, f64)],
    ) -> usize {
        let value = |tau: usize| self.at(tau);
        let limits = shortest..=longest;
        let mut count = 0;
        let mut tau = 1;
        while tau <= last && value(tau) > 0.0 {
            tau += 1;
        }
        loop {
            while tau <= last && value(tau) <= 0.0 {
                tau += 1;
            }
            if tau > last {
                return count;
            }
            let mut highest_whole = tau;
            let mut top: Option<(usize, f64)> = None;
            while tau <= last && value(tau) > 0.0 {
                if value(tau) > value(highest_whole) {
                    highest_whole = tau;
                }
                let local = value(tau - 1) < value(tau) && value(tau) >= value(tau + 1);
                if local && limits.contains(&tau) {
                    let (_, height) = self.estimate(tau, true);
                    if top.is_none_or(|(_, highest)| height > highest) {
                        top = Some((tau, height));
                    }
                }
                tau += 1;
            }
            if !limits.contains(&highest_whole) {
                continue;
            }
            // Lobes are disjoint and each has its own lag within the
            // limits, so `maxima` has room for every one.
            if let (Some(top), Some(slot)) = (top, maxima.get_mut(count)) {
                *slot = top;
                count += 1;
            }
        }
    }
}

/// The vertex of the parabola through `(-1, before)`, `(0, at)` and
/// `(1, after)`: its abscissa, kept within one step of the middle, and its
/// value.
fn vertex(before: f64, at: f64, after: f64) -> (f64, f64) {
    let curvature = before - 2.0 * at + after;
    let offset = if curvature == 0.0 {
        0.0
    } else {
        (0.5 * (before - after) / curvature).clamp(-1.0, 1.0)
    };
    (offset, at - 0.25 * (before - after) * offset)
}

/// The weights of the band-limited interpolation kernel for a point
/// `fraction` of a lag past a whole lag, for the whole lags from
/// `1 - KERNEL_HALF_WIDTH` to `KERNEL_HALF_WIDTH` away from it: a sinc under
/// a Kaiser window, normalised to unit gain at zero frequency.
fn kernel(fraction: f64) -> [f64; TAPS] {
    // sin(pi (m - f)) is -sin(pi f) for even m and +sin(pi f) for odd.
    let sine = (PI * fraction).sin();
    let half = KERNEL_HALF_WIDTH as f64;
    let mut weights = [0.0; TAPS];
    let mut total = 0.0;
    for (tap, weight) in weights.iter_mut().enumerate() {
        // m runs from 1 - K to K.
        let m = tap as f64 + 1.0 - half;
        let distance = m - fraction;
        let sign = if tap % 2 == 1 { -1.0 } else { 1.0 };
        let sinc = sign * sine / (PI * distance);
        let ratio = distance / half;
        *weight = sinc * kaiser_window(1.0 - ratio * ratio);
        total += *weight;
    }
    for weight in &mut weights {
        *weight /= total;
    }
    weights
}

/// The sum of the squares of `len` samples from `start` on, given the
/// running sums `squares` (entry `i` is the sum of the first `i`).
fn energy(squares: &[f64], start: usize, len: usize) -> f64 {
    let at = |index: usize| squares.get(index).copied().unwrap_or(0.0);
    at(start + len) - at(start)
}

/// The coefficients of the power series of the zeroth-order modified
/// Bessel function of the first kind (Abramowitz and Stegun, 9.6.12),
/// written in `u = (x / beta)^2` for the kernel's `beta`.
const WINDOW_SERIES: [f64; WINDOW_TERMS] = window_series();

const fn window_series() -> [f64; WINDOW_TERMS] {
    let quarter_square = KERNEL_BETA * KERNEL_BETA / 4.0;
    let mut series = [0.0; WINDOW_TERMS];
    let mut term = 1.0;
    let mut k = 0;
    let mut order = 0.0;
    while k < WINDOW_TERMS {
        series[k] = term;
        k += 1;
        order += 1.0;
        term = term * quarter_square / (order * order);
    }
    series
}

/// The Kaiser window of the kernel, without its constant divisor (the
/// kernel is normalised anyway): `I0(beta sqrt(u))` for `u` from 0 at the
/// ends of the kernel to 1 in its middle, by Horner's rule.
fn kaiser_window(u: f64) -> f64 {
    let u = u.max(0.0);
    WINDOW_SERIES
        .iter()
        .rev()
        .fold(0.0, |sum, &coefficient| sum * u + coefficient)
}

/// The minimum of `objective` between `low` and `high` by golden-section
/// search (J. Kiefer, "Sequential minimax search for a maximum",
/// Proceedings of the American Mathematical Society, 1953), assuming it
/// has one there.
fn golden_section(mut low: f64, mut high: f64, objective: impl Fn(f64) -> f64) -> f64 {
    let ratio = (5f64.sqrt() - 1.0) / 2.0;
    let mut left = high - ratio * (high - low);
    let mut right = low + ratio * (high - low);
    let mut at_left = objective(left);
    let mut at_right = objective(right);
    for _ in 0..SEARCH_STEPS {
        if at_left < at_right {
            high = right;
            right = left;
            at_right = at_left;
            left = high - ratio * (high - low);
            at_left = objective(left);
        } else {
            low = left;
            left = right;
            at_left = at_right;
            right = low + ratio * (high - low);
            at_right = objective(right);
        }
    }
    0.5 * (low + high)
}

/// As a processor the detector analyses the block and leaves it unchanged,
/// so it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for PitchDetector {
    fn prepare(&mut self, spec: &ProcessSpec) {
        PitchDetector::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        PitchDetector::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(settings: PitchSettings, sample_rate: f64) -> PitchDetector {
        let mut detector = PitchDetector::new(settings);
        detector.prepare(&ProcessSpec {
            sample_rate,
            max_block_size: 512,
            num_channels: 1,
        });
        detector
    }

    #[test]
    fn the_frame_follows_from_the_lowest_frequency() {
        let detector = prepared(PitchSettings::default(), 48_000.0);
        assert_eq!(detector.period_range(), (24, 960));
        assert_eq!(detector.frame_len(), 2 * 960 + 65);
        let detector = prepared(PitchSettings::default().with_range(2_000.0, 50.0), 48_000.0);
        assert_eq!(detector.period_range(), (24, 960));
        let detector = prepared(
            PitchSettings::default()
                .with_range(f64::NAN, 0.0)
                .with_hop(0),
            44_100.0,
        );
        assert_eq!(detector.period_range(), (22, 882));
        assert_eq!(detector.settings().hop, 1);
        // Absurd limits stay allocatable.
        let detector = prepared(PitchSettings::default().with_range(1e-9, 1e12), 48_000.0);
        assert_eq!(detector.period_range(), (2, MAX_PERIOD));
    }

    #[test]
    fn analyses_come_every_hop_once_the_frame_is_full() {
        let mut detector = prepared(PitchSettings::default().with_hop(100), 48_000.0);
        let frame = detector.frame_len();
        detector.measure_samples(&vec![0.0f32; frame - 1]);
        assert_eq!(detector.analysis_count(), 0);
        detector.measure_samples(&[0.0f32]);
        assert_eq!(detector.analysis_count(), 1);
        detector.measure_samples(&vec![0.0f32; 250]);
        assert_eq!(detector.analysis_count(), 3);
        assert_eq!(detector.readings().analysis_count(), 3);
        // Silence has no pitch.
        assert_eq!(detector.latest(), Pitch::NONE);
        assert!(detector.readings().pitch().is_none());
    }

    #[test]
    fn frequency_and_confidence_travel_together() {
        let pitch = Pitch {
            frequency: 1_234.5,
            confidence: 0.875,
        };
        assert_eq!(unpack(pack(pitch)), pitch);
        assert_eq!(unpack(pack(Pitch::NONE)), Pitch::NONE);
        assert!(Pitch::NONE.is_none() && Pitch::NONE.note().is_none());
        assert_eq!(pitch.note().unwrap().to_string(), "D#6");
    }

    #[test]
    fn the_interpolation_kernel_reproduces_band_limited_functions() {
        // Cosines from nearly constant to 80% of the highest frequency a
        // lag function can hold are reproduced between whole lags.
        let mut detector = prepared(PitchSettings::default(), 48_000.0);
        let mut worst = 0.0f64;
        for frequency in [0.02, 0.2, 1.0, 2.0, 2.5] {
            for (index, slot) in detector.lags.iter_mut().enumerate() {
                let lag = index as f64 - KERNEL_HALF_WIDTH as f64;
                *slot = (frequency * lag).cos();
            }
            let lags = detector.lag_function();
            for lag in [100.25, 333.5, 500.9, 700.0, 123.123] {
                let error = (lags.interpolate(lag) - (frequency * lag).cos()).abs();
                worst = worst.max(error);
                assert!(error < 2e-5, "{frequency} rad at {lag}: {error}");
            }
            // Half lags, read with the kernel computed once, are the same
            // numbers to the last bit.
            for lag in [100, 333, 500, 959] {
                assert_eq!(
                    lags.after_half(lag).to_bits(),
                    lags.interpolate(lag as f64 + 0.5).to_bits()
                );
            }
        }
        println!("largest interpolation error: {worst:.2e}");
        // A whole lag is read as it is.
        let lags = detector.lag_function();
        assert_eq!(lags.interpolate(700.0), lags.at(700));
        // And the search finds the extremum of a parabola anywhere in its
        // bracket, to the millionth of a lag its 30 steps allow.
        for vertex in [100.0, 100.3, 100.5, 100.77] {
            let found = golden_section(99.6, 101.6, |x| (x - vertex) * (x - vertex));
            assert!((found - vertex).abs() < 1e-6, "{vertex}: {found}");
        }
        // I0(1) from Abramowitz and Stegun, table 9.8: u = (1/9)^2.
        assert!((kaiser_window(1.0 / 81.0) - 1.266_065_877_752_008).abs() < 1e-14);
        assert_eq!(kaiser_window(0.0), 1.0);
        assert_eq!(kaiser_window(-0.5), 1.0);
    }

    #[test]
    fn candidates_are_judged_by_their_interpolated_extremum() {
        // Peaks a sample or two wide with their top anywhere between two
        // whole lags: a sine with a period of 4.5 lags (1778 Hz at 8 kHz),
        // and the autocorrelation of a sawtooth without its fundamental
        // with a period of 24.49 lags (1960 Hz at 48 kHz), harmonics 2 to
        // 12, the last at 98% of half the sample rate. Their whole lags can
        // read as little as 0.77 and 0.85; the estimate from half lags
        // comes within a hundredth of the top, 1, and within a few
        // hundredths of a lag of where it is. Dips, upside down, alike.
        let mut detector = prepared(PitchSettings::default(), 48_000.0);
        for (period, first, last, height, place) in
            [(4.5, 1, 1, 0.01, 0.01), (24.49, 2, 12, 0.01, 0.05)]
        {
            let weights: f64 = (first..=last).map(|k| 1.0 / f64::from(k * k)).sum();
            let (mut lowest_whole, mut worst_height, mut worst_place) = (1.0f64, 0.0f64, 0.0f64);
            for offset in [0.0, 0.17, 0.33, 0.5, 0.67, 0.83] {
                let top = 240.0 + offset;
                for maximum in [true, false] {
                    for (index, slot) in detector.lags.iter_mut().enumerate() {
                        let lag = index as f64 - KERNEL_HALF_WIDTH as f64;
                        let phase = core::f64::consts::TAU * (lag - top) / period;
                        let value = (first..=last)
                            .map(|k| (f64::from(k) * phase).cos() / f64::from(k * k))
                            .sum::<f64>()
                            / weights;
                        *slot = if maximum { value } else { 1.0 - value };
                    }
                    let lags = detector.lag_function();
                    let sign = if maximum { 1.0 } else { -1.0 };
                    let centre = if sign * lags.at(240) >= sign * lags.at(241) {
                        240
                    } else {
                        241
                    };
                    let whole = if maximum {
                        lags.at(centre)
                    } else {
                        1.0 - lags.at(centre)
                    };
                    lowest_whole = lowest_whole.min(whole);
                    let (position, value) = lags.estimate(centre, maximum);
                    let found = if maximum { value } else { 1.0 - value };
                    worst_height = worst_height.max((found - 1.0).abs());
                    worst_place = worst_place.max((position - top).abs());
                }
            }
            println!(
                "period {period}: whole lags as low as {lowest_whole:.3}; estimate within {worst_height:.2e} of the top and {worst_place:.3} lags of its place"
            );
            assert!(lowest_whole < 0.9, "{lowest_whole}");
            assert!(worst_height < height, "{worst_height}");
            assert!(worst_place < place, "{worst_place}");
        }
    }

    #[test]
    fn odd_settings_are_made_usable() {
        assert_eq!(
            PitchMethod::Yin {
                threshold: f64::NAN
            }
            .sanitised(),
            PitchMethod::yin()
        );
        assert_eq!(
            PitchMethod::Mpm { cutoff: 7.0 }.sanitised(),
            PitchMethod::Mpm { cutoff: 1.0 }
        );
        assert_eq!(
            PitchMethod::Yin { threshold: -1.0 }.sanitised(),
            PitchMethod::Yin { threshold: 0.0 }
        );
        assert_eq!(PitchMethod::default(), PitchMethod::yin());
    }
}
