//! A spectrum analyser for display: the short-time Fourier transform, in
//! decibels, with ballistics, peak hold and fractional-octave bands.

use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};
use manguebyte_dsp::fft::{Complex, RealFft};
use manguebyte_dsp::window::{Symmetry, Window};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::bands::{Band, OctaveFraction};
use crate::input::{Source, for_each_sample};
use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, usable_rate};
use crate::triple_buffer;
use crate::weighting::Weighting;

/// The lowest level the analyser reports, in decibels. Zero, which has no
/// level, reads this.
///
/// It is far below anything audio holds: 24-bit audio has its last bit at
/// -144 dBFS, and the analyser computes in double precision, whose
/// rounding noise lies near -300 dB.
pub const SPECTRUM_FLOOR_DB: f64 = -200.0;

/// The largest Kaiser parameter the analyser accepts. Beyond it the window
/// is no better for audio, and the series that computes it loses accuracy.
const MAX_KAISER_BETA: f64 = 50.0;

/// How a [`SpectrumAnalyzer`] analyses: transform size, hop, window,
/// weighting, bands, ballistics and peak hold.
///
/// Start from [`new`](Self::new) and change what you need:
///
/// ```
/// use manguebyte_analysis::{OctaveFraction, SpectrumSettings, Weighting};
/// use manguebyte_dsp::window::Window;
///
/// let settings = SpectrumSettings::new(8_192)
///     .with_hop(2_048)
///     .with_window(Window::BlackmanHarris)
///     .with_weighting(Weighting::A)
///     .with_bands(OctaveFraction::Third)
///     .with_ballistics(0.0, 0.5)
///     .with_peak_hold(2.0, 20.0);
/// assert_eq!(settings.hop, 2_048);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct SpectrumSettings {
    /// Samples per transform: any length from 2 up. Powers of two are the
    /// fastest. The bins are `sample_rate / fft_size` apart, and there are
    /// `fft_size / 2 + 1` of them.
    pub fft_size: usize,
    /// Samples from the start of one transform to the start of the next,
    /// at least 1. Half the transform size, the default, suits the Hann
    /// window; a hop longer than the transform skips samples.
    pub hop: usize,
    /// The window each block is multiplied by before it is transformed,
    /// taken in its periodic form. Hann by default. A Kaiser parameter that
    /// is not a number is taken as 0, and its magnitude is limited to 50.
    pub window: Window,
    /// The frequency weighting applied to bins and bands. None (Z) by
    /// default.
    pub weighting: Weighting,
    /// Fractional-octave bands to compute as well, or `None` for none (the
    /// default).
    pub bands: Option<OctaveFraction>,
    /// The bands run from the one containing this frequency, in hertz.
    /// 20 Hz by default.
    pub band_low_hz: f64,
    /// The bands run up to the one containing this frequency, in hertz.
    /// 20 kHz by default.
    pub band_high_hz: f64,
    /// Time constant of a rising reading, in seconds: the time the power of
    /// a bin or a band takes to cover 63% of the way to a higher one. From
    /// silence, a steady tone reads 2 dB under its level after one time
    /// constant. Zero, the default, follows at once.
    pub attack_seconds: f64,
    /// Time constant of a falling reading, in seconds: the time the power
    /// takes to cover 63% of the way to a lower one. A reading whose sound
    /// stops falls at `10 log10(e) / release_seconds`, about 4.34 dB per
    /// second divided by this time, whatever its level was. Zero, the
    /// default, follows at once.
    pub release_seconds: f64,
    /// How long a peak is held, in seconds. Infinite, the default, holds
    /// the highest reading until the peaks are reset.
    pub peak_hold_seconds: f64,
    /// How fast a peak falls once its hold time is over, in decibels per
    /// second; never below the reading itself. 20 dB/s by default; zero
    /// stops it falling.
    pub peak_fall_db_per_second: f64,
    /// Which channel, or the mean of all of them, is analysed.
    pub source: Source,
}

impl SpectrumSettings {
    /// Settings for transforms of `fft_size` samples, with the defaults
    /// listed on each field.
    pub fn new(fft_size: usize) -> Self {
        Self {
            fft_size,
            hop: (fft_size / 2).max(1),
            window: Window::Hann,
            weighting: Weighting::Z,
            bands: None,
            band_low_hz: 20.0,
            band_high_hz: 20_000.0,
            attack_seconds: 0.0,
            release_seconds: 0.0,
            peak_hold_seconds: f64::INFINITY,
            peak_fall_db_per_second: 20.0,
            source: Source::Mean,
        }
    }

    /// Sets the hop, in samples.
    pub fn with_hop(mut self, hop: usize) -> Self {
        self.hop = hop;
        self
    }

    /// Sets the window.
    pub fn with_window(mut self, window: Window) -> Self {
        self.window = window;
        self
    }

    /// Sets the frequency weighting.
    pub fn with_weighting(mut self, weighting: Weighting) -> Self {
        self.weighting = weighting;
        self
    }

    /// Adds fractional-octave bands, from 20 Hz to 20 kHz unless
    /// [`with_band_range`](Self::with_band_range) says otherwise.
    pub fn with_bands(mut self, fraction: OctaveFraction) -> Self {
        self.bands = Some(fraction);
        self
    }

    /// Sets the frequencies, in hertz, whose bands are the first and the
    /// last.
    pub fn with_band_range(mut self, low_hz: f64, high_hz: f64) -> Self {
        self.band_low_hz = low_hz;
        self.band_high_hz = high_hz;
        self
    }

    /// Sets the attack and release time constants, in seconds.
    pub fn with_ballistics(mut self, attack_seconds: f64, release_seconds: f64) -> Self {
        self.attack_seconds = attack_seconds;
        self.release_seconds = release_seconds;
        self
    }

    /// Sets the peak hold time, in seconds, and how fast a peak falls
    /// after it, in decibels per second.
    pub fn with_peak_hold(mut self, hold_seconds: f64, fall_db_per_second: f64) -> Self {
        self.peak_hold_seconds = hold_seconds;
        self.peak_fall_db_per_second = fall_db_per_second;
        self
    }

    /// Sets which channel, or the mean of all, is analysed.
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    /// The settings with every field made usable, as the analyser takes
    /// them.
    fn sanitised(mut self) -> Self {
        self.fft_size = self.fft_size.max(2);
        self.hop = self.hop.max(1);
        if let Window::Kaiser(beta) = self.window {
            let beta = if beta.is_nan() { 0.0 } else { beta.abs() };
            self.window = Window::Kaiser(beta.min(MAX_KAISER_BETA));
        }
        let time = |seconds: f64| if seconds > 0.0 { seconds } else { 0.0 };
        self.attack_seconds = time(self.attack_seconds);
        self.release_seconds = time(self.release_seconds);
        self.peak_hold_seconds = if self.peak_hold_seconds.is_nan() {
            f64::INFINITY
        } else {
            time(self.peak_hold_seconds)
        };
        self.peak_fall_db_per_second = if self.peak_fall_db_per_second.is_finite() {
            time(self.peak_fall_db_per_second)
        } else {
            0.0
        };
        self
    }
}

impl Default for SpectrumSettings {
    /// Transforms of 4096 samples; see [`new`](Self::new).
    fn default() -> Self {
        Self::new(4_096)
    }
}

/// One complete analysis, as a user interface reads it from a
/// [`SpectrumReader`].
///
/// All levels are in decibels, in single precision, and never below
/// [`SPECTRUM_FLOOR_DB`]. Until the analyser has transformed its first
/// block every level is the floor and [`frame_count`](Self::frame_count) is
/// zero.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumFrame {
    magnitudes: Box<[f32]>,
    peaks: Box<[f32]>,
    bands: Box<[f32]>,
    band_peaks: Box<[f32]>,
    sample_rate: f64,
    fft_size: usize,
    frames: u64,
}

impl SpectrumFrame {
    fn new(bins: usize, bands: usize, fft_size: usize) -> Self {
        let floor = SPECTRUM_FLOOR_DB as f32;
        Self {
            magnitudes: vec![floor; bins].into_boxed_slice(),
            peaks: vec![floor; bins].into_boxed_slice(),
            bands: vec![floor; bands].into_boxed_slice(),
            band_peaks: vec![floor; bands].into_boxed_slice(),
            sample_rate: DEFAULT_SAMPLE_RATE,
            fft_size,
            frames: 0,
        }
    }

    /// The level of each bin, after weighting and ballistics, in dBFS: a
    /// sine at full scale that falls on a bin reads 0 there, away from the
    /// lowest and highest few bins. See [`SpectrumAnalyzer`] for what a bin
    /// reads.
    pub fn magnitudes_db(&self) -> &[f32] {
        &self.magnitudes
    }

    /// The held peak of each bin, in dBFS.
    pub fn peaks_db(&self) -> &[f32] {
        &self.peaks
    }

    /// The level of each band, after weighting and ballistics, in dBFS:
    /// the power in the band relative to that of a sine at full scale.
    /// Empty without bands.
    pub fn bands_db(&self) -> &[f32] {
        &self.bands
    }

    /// The held peak of each band, in dBFS. Empty without bands.
    pub fn band_peaks_db(&self) -> &[f32] {
        &self.band_peaks
    }

    /// The sample rate the frame was computed at, in hertz.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The transform size the frame was computed with.
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// The frequency of bin `bin`, in hertz.
    pub fn bin_frequency(&self, bin: usize) -> f64 {
        bin as f64 * self.sample_rate / self.fft_size as f64
    }

    /// How many transforms the analyser had computed since it was last
    /// reset when it published this frame. Zero means none yet.
    pub fn frame_count(&self) -> u64 {
        self.frames
    }
}

/// What the analyser shares with its reader, apart from the frames.
#[derive(Debug, Default)]
struct SpectrumShared {
    reset: Flag,
    reset_peaks: Flag,
}

/// The reading end of a [`SpectrumAnalyzer`], for a user-interface thread.
///
/// It hands out the newest complete frame the analyser has published,
/// through a [triple buffer](crate::triple_buffer): neither side ever
/// waits for the other, and a frame is never seen half written. There is
/// one reader per analyser; it can be moved to any thread.
///
/// Nothing here allocates or locks.
#[derive(Debug)]
pub struct SpectrumReader {
    frames: triple_buffer::Reader<SpectrumFrame>,
    shared: Arc<SpectrumShared>,
    bands: Arc<[Band]>,
}

impl SpectrumReader {
    /// Fetches the newest frame if the analyser has published one since the
    /// last fetch, and says whether it had.
    pub fn update(&mut self) -> bool {
        self.frames.update()
    }

    /// The frame fetched last.
    pub fn frame(&self) -> &SpectrumFrame {
        self.frames.get()
    }

    /// Fetches the newest frame, if there is a new one, and returns the
    /// frame fetched last: [`update`](Self::update) and then
    /// [`frame`](Self::frame).
    pub fn latest(&mut self) -> &SpectrumFrame {
        self.frames.latest()
    }

    /// The bands, in the order of [`SpectrumFrame::bands_db`].
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    /// Asks the analyser to start again: history, ballistics, peaks and
    /// frame count. It does so at the start of its next block.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }

    /// Asks the analyser to clear its held peaks only, at the start of its
    /// next block.
    pub fn request_peak_reset(&self) {
        self.shared.reset_peaks.raise();
    }
}

/// The ballistics and the peak hold of one reading, per frame.
#[derive(Debug, Clone, Copy)]
struct Ballistics {
    /// The share of the way to a higher power covered in one frame.
    attack: f64,
    /// The share of the way to a lower power covered in one frame.
    release: f64,
    hold_frames: u64,
    /// How far a peak falls in one frame once its hold is over, in dB.
    fall_db: f64,
}

impl Ballistics {
    fn new(settings: &SpectrumSettings, sample_rate: f64) -> Self {
        let frame_seconds = settings.hop as f64 / sample_rate;
        let coefficient = |seconds: f64| {
            if seconds > 0.0 {
                1.0 - (-frame_seconds / seconds).exp()
            } else {
                1.0
            }
        };
        let hold_frames = if settings.peak_hold_seconds == f64::INFINITY {
            u64::MAX
        } else {
            // The cast saturates, so an absurd time becomes "for ever".
            (settings.peak_hold_seconds / frame_seconds).round() as u64
        };
        Self {
            attack: coefficient(settings.attack_seconds),
            release: coefficient(settings.release_seconds),
            hold_frames,
            fall_db: settings.peak_fall_db_per_second * frame_seconds,
        }
    }

    /// Moves the smoothed power `power` towards the new power `raw`, sets
    /// `level` to it in decibels and updates its peak.
    ///
    /// The smoothing is an exponential average of the power, as a sound
    /// level meter's time weighting is (IEC 61672-1), so a reading whose
    /// sound stops falls at a steady rate in decibels, set by the release
    /// time alone.
    #[inline]
    fn apply(
        &self,
        raw: f64,
        power: &mut f64,
        level: &mut f64,
        peak: &mut f64,
        hold_left: &mut u64,
    ) {
        let coefficient = if raw > *power {
            self.attack
        } else {
            self.release
        };
        if coefficient >= 1.0 {
            *power = raw;
        } else {
            *power += coefficient * (raw - *power);
        }
        *level = to_db(*power);
        if *level >= *peak {
            *peak = *level;
            *hold_left = self.hold_frames;
        } else if *hold_left == 0 {
            *peak = (*peak - self.fall_db).max(*level);
        } else if *hold_left != u64::MAX {
            // `u64::MAX` holds for ever.
            *hold_left -= 1;
        }
    }
}

/// A power relative to that of a full-scale sine, in decibels, never below
/// the floor.
#[inline]
fn to_db(power: f64) -> f64 {
    (10.0 * power.log10()).max(SPECTRUM_FLOOR_DB)
}

/// Which bins feed which band, and how much of each: the frequency range a
/// bin stands for, shared out by how much of it lies in the band.
#[derive(Debug, Default)]
struct BandLayout {
    /// `(bin, share)` for every band, one band after the other.
    entries: Vec<(usize, f64)>,
    /// Where each band's entries end in `entries`.
    ends: Vec<usize>,
}

impl BandLayout {
    fn new(bands: &[Band], fft_size: usize, sample_rate: f64) -> Self {
        let bins = fft_size / 2 + 1;
        let spacing = sample_rate / fft_size as f64;
        let nyquist = sample_rate / 2.0;
        let mut entries = Vec::new();
        let mut ends = Vec::with_capacity(bands.len());
        for band in bands {
            // The bins whose range can overlap the band.
            let first = ((band.lower() / spacing - 0.5).floor().max(0.0)) as usize;
            let last = ((band.upper() / spacing + 0.5).ceil() as usize).min(bins - 1);
            for bin in first..=last {
                let centre = bin as f64 * spacing;
                let low = (centre - spacing / 2.0).max(0.0);
                let high = (centre + spacing / 2.0).min(nyquist);
                let overlap = high.min(band.upper()) - low.max(band.lower());
                if overlap > 0.0 && high > low {
                    entries.push((bin, overlap / (high - low)));
                }
            }
            ends.push(entries.len());
        }
        Self { entries, ends }
    }
}

/// A spectrum analyser: short-time Fourier transforms of a stream, in
/// decibels, with attack and release, peak hold, frequency weighting and
/// fractional-octave bands, published for a user interface to draw.
///
/// Every [`hop`](SpectrumSettings::hop) samples, once [`fft_size`] samples
/// have arrived, the last `fft_size` samples are multiplied by the window
/// and transformed. Each bin and each band then moves towards its new
/// level with the attack or the release time constant, its peak is held,
/// and at the end of the block the newest frame is published to the
/// [`SpectrumReader`]. The analyser itself also exposes its state in
/// double precision, for use on the audio thread or offline.
///
/// [`fft_size`]: SpectrumSettings::fft_size
///
/// ```
/// use manguebyte_analysis::{SpectrumAnalyzer, SpectrumSettings};
/// use manguebyte_dsp::ProcessSpec;
///
/// let (mut analyzer, mut reader) = SpectrumAnalyzer::new(SpectrumSettings::new(1_024));
/// analyzer.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 512, num_channels: 1 });
///
/// // A full-scale sine on bin 100, which is 100 * 48000 / 1024 Hz.
/// let signal: Vec<f32> = (0..4_096)
///     .map(|n| (core::f32::consts::TAU * 100.0 * n as f32 / 1_024.0).sin())
///     .collect();
/// analyzer.measure_samples(&signal);
///
/// // On the user-interface thread: the newest frame.
/// let frame = reader.latest();
/// assert!(frame.frame_count() > 0);
/// assert!(frame.magnitudes_db()[100].abs() < 0.001);
/// assert!((frame.bin_frequency(100) - 4_687.5).abs() < 1e-9);
/// ```
///
/// # What a bin reads
///
/// Bin `k` is the frequency `k * sample_rate / fft_size`. A bin's level is
/// the magnitude of the transform divided by what a sine at full scale
/// gives there, so a sine at full scale that falls exactly on a bin reads
/// 0 dBFS in it, with every window, as long as its mirror image is out of
/// the way (see below). A constant (bin 0) and a tone at half the sample
/// rate (the last bin, for an even size) read their own value. A sine
/// between two bins reads lower, by the scalloping loss of the window: up
/// to 3.9 dB with the rectangular window, 1.4 dB with Hann, under 0.01 dB
/// with the flat top, which exists for that reason.
///
/// A real sine is two rotating phasors, one at its frequency and one at
/// its negative, and the window spreads each over the bins near it. On
/// bin `k`, the mirror image lies `2k` bins away, and `fft_size - 2k` bins
/// away near the top. Where the window leaks at that distance, the image
/// adds to the reading or takes from it, depending on the sine's phase.
/// A window made of a sum of `n` cosines (Hann and Hamming 2, Blackman 3,
/// Blackman-Harris and Nuttall 4, the flat top 5) does not leak at all at
/// `n` or more whole bins; the rectangular window leaks at no whole number
/// of bins, the triangular one at no even number. So with an even size a
/// sine on a bin reads 0 dBFS exactly on every bin but these, and the same
/// counted down from `fft_size / 2`:
///
/// | Window | Bins that read the image | Reading on bin 1 |
/// |---|---|---|
/// | Rectangular, triangular, Hann, Hamming | none | 0 dB |
/// | Blackman | 1 | -0.87 to +0.79 dB |
/// | Blackman-Harris | 1 | -1.9 to +1.6 dB |
/// | Nuttall | 1 | -2.0 to +1.6 dB |
/// | Flat top | 1 and 2 | -8.9 to +4.3 dB; bin 2 within 0.14 dB |
///
/// With an odd size the distance near the top is odd, so the bins just
/// below half the sample rate read their image with every window but the
/// rectangular one: the top bin with Hann, Hamming and Blackman, the top
/// two with Blackman-Harris, Nuttall and the flat top, and several with the
/// triangular window. The Kaiser window leaks a little at every distance:
/// with parameter 8, -0.69 to +0.64 dB on bin 1 and within 0.003 dB from
/// bin 2 on. The tests compare each of these readings, at an even and an
/// odd size, with what the window's own transform predicts.
///
/// Noise is spread over all bins, and a bin of it reads higher the wider
/// the window's equivalent noise bandwidth is. To read the power of noise
/// from bins, divide the squared magnitude by
/// [`noise_bandwidth`](Self::noise_bandwidth); the bands do that.
///
/// # What a band reads
///
/// A band's level is the power of the bins it covers, in dBFS: relative to
/// the power of a sine at full scale, so the band that holds a full-scale
/// sine reads 0 dB. Each bin stands for the range of frequencies half a bin
/// either side of it, and a bin whose range straddles an edge of the band
/// gives the band the share of its power that lies inside. The bands of a
/// range therefore add up to the power of the bins they cover, and for
/// white and pink noise each band reads the power that lies in it.
///
/// # Limits
///
/// A band narrower than a few bins is not resolved: the window spreads
/// every tone over two to five bins, and a narrow band's level is mostly
/// shared out from its neighbours'. For a band to read its own content
/// within a fraction of a decibel it should be at least four bins wide;
/// third-octave bands are, from about `17 * sample_rate / fft_size` hertz
/// up. A band that reaches above half the sample rate reads the part below
/// it, and one wholly above reads the floor.
///
/// Ballistics act on power, once per hop: each bin and each band is an
/// exponential average of its power, with the attack time constant while
/// the power rises and the release one while it falls, as the time
/// weightings of a sound level meter are (IEC 61672-1). A reading whose
/// sound stops therefore falls in a straight line in decibels, at
/// 4.34 dB/s divided by the release time, and how fast it falls does not
/// depend on [`SPECTRUM_FLOOR_DB`] or on where it started. The peak hold
/// acts on the decibel values: a peak is held, then falls at its own rate
/// in decibels, never below the reading. Times shorter than a hop act
/// within one frame.
///
/// Like every meter in the crate, the analyser screens its input (see the
/// [crate documentation](crate#bad-samples)). A sample at the limit is a
/// click across every bin of the frames that contain it, up to about
/// +120 dBFS with a Hann window of 4096 samples, and an infinite peak hold
/// keeps it until the peaks are reset. With a release time the readings
/// come down from it at their steady rate: with 0.5 s, 8.7 dB/s, so about
/// 14 s to fall back to a programme at 0 dBFS.
#[derive(Debug)]
pub struct SpectrumAnalyzer {
    settings: SpectrumSettings,
    sample_rate: f64,
    fft: RealFft<f64>,
    window: Box<[f64]>,
    /// Per bin: from the magnitude of the transform to the amplitude of a
    /// sine, with the weighting.
    amplitude_scale: Box<[f64]>,
    /// Per bin: from the squared magnitude to power relative to that of a
    /// full-scale sine, with the weighting.
    power_scale: Box<[f64]>,
    noise_bandwidth: f64,
    history: Box<[f64]>,
    /// Where the next sample goes in `history`, which is also where the
    /// oldest one is.
    write: usize,
    /// Samples still to come before the next transform.
    until_frame: usize,
    input: Box<[f64]>,
    spectrum: Box<[Complex<f64>]>,
    /// Per bin: the power of the last transform, for the bands.
    bin_power: Box<[f64]>,
    /// Per bin: the squared amplitude after ballistics, relative to a
    /// full-scale sine.
    smoothed_bins: Box<[f64]>,
    magnitudes: Box<[f64]>,
    peaks: Box<[f64]>,
    hold_left: Box<[u64]>,
    bands: Arc<[Band]>,
    layout: BandLayout,
    /// Per band: the power after ballistics, relative to a full-scale sine.
    smoothed_bands: Box<[f64]>,
    band_levels: Box<[f64]>,
    band_peaks: Box<[f64]>,
    band_hold_left: Box<[u64]>,
    ballistics: Ballistics,
    frames: u64,
    published: u64,
    writer: triple_buffer::Writer<SpectrumFrame>,
    shared: Arc<SpectrumShared>,
}

impl SpectrumAnalyzer {
    /// Creates an analyser and the reader a user interface draws from.
    /// This allocates; nothing after it does, except
    /// [`prepare`](Self::prepare).
    ///
    /// Settings that make no sense are made usable: see each field of
    /// [`SpectrumSettings`]. Until it is prepared the analyser assumes
    /// 44.1 kHz.
    pub fn new(settings: SpectrumSettings) -> (Self, SpectrumReader) {
        let settings = settings.sanitised();
        let size = settings.fft_size;
        let bins = size / 2 + 1;
        let bands: Arc<[Band]> = match settings.bands {
            Some(fraction) => fraction
                .bands(settings.band_low_hz, settings.band_high_hz)
                .into(),
            None => Arc::new([]),
        };
        let mut window = vec![0.0; size].into_boxed_slice();
        settings.window.fill(&mut window, Symmetry::Periodic);
        let sum: f64 = window.iter().sum();
        let sum_of_squares: f64 = window.iter().map(|value| value * value).sum();
        // The coherent gain is positive for every window there is; the
        // guard only keeps a degenerate one from producing NaNs.
        let coherent_gain = (sum / size as f64).abs().max(f64::MIN_POSITIVE);
        let noise_bandwidth = sum_of_squares / (size as f64 * coherent_gain * coherent_gain);

        let (writer, reader) =
            triple_buffer::channel_with(|| SpectrumFrame::new(bins, bands.len(), size));
        let shared = Arc::new(SpectrumShared::default());
        let floor = SPECTRUM_FLOOR_DB;
        let mut analyzer = Self {
            settings,
            sample_rate: DEFAULT_SAMPLE_RATE,
            fft: RealFft::new(size),
            window,
            amplitude_scale: vec![0.0; bins].into_boxed_slice(),
            power_scale: vec![0.0; bins].into_boxed_slice(),
            noise_bandwidth,
            history: vec![0.0; size].into_boxed_slice(),
            write: 0,
            until_frame: size,
            input: vec![0.0; size].into_boxed_slice(),
            spectrum: vec![Complex::ZERO; bins].into_boxed_slice(),
            bin_power: vec![0.0; bins].into_boxed_slice(),
            smoothed_bins: vec![0.0; bins].into_boxed_slice(),
            magnitudes: vec![floor; bins].into_boxed_slice(),
            peaks: vec![floor; bins].into_boxed_slice(),
            hold_left: vec![0; bins].into_boxed_slice(),
            bands: Arc::clone(&bands),
            layout: BandLayout::default(),
            smoothed_bands: vec![0.0; bands.len()].into_boxed_slice(),
            band_levels: vec![floor; bands.len()].into_boxed_slice(),
            band_peaks: vec![floor; bands.len()].into_boxed_slice(),
            band_hold_left: vec![0; bands.len()].into_boxed_slice(),
            ballistics: Ballistics::new(&settings, DEFAULT_SAMPLE_RATE),
            frames: 0,
            published: u64::MAX,
            writer,
            shared: Arc::clone(&shared),
        };
        analyzer.configure(DEFAULT_SAMPLE_RATE, coherent_gain);
        analyzer.reset();
        let reader = SpectrumReader {
            frames: reader,
            shared,
            bands,
        };
        (analyzer, reader)
    }

    /// Takes the sample rate from `spec` and starts again. This allocates
    /// the table that maps bins to bands, whose size depends on the rate.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        let sum: f64 = self.window.iter().sum();
        let coherent_gain = (sum / self.window.len() as f64)
            .abs()
            .max(f64::MIN_POSITIVE);
        self.configure(usable_rate(spec.sample_rate), coherent_gain);
        self.reset();
    }

    /// Everything that depends on the sample rate.
    fn configure(&mut self, sample_rate: f64, coherent_gain: f64) {
        self.sample_rate = sample_rate;
        let size = self.settings.fft_size;
        let last = size / 2;
        let spacing = sample_rate / size as f64;
        for (bin, (amplitude, power)) in self
            .amplitude_scale
            .iter_mut()
            .zip(self.power_scale.iter_mut())
            .enumerate()
        {
            // A sine of amplitude A on bin k gives |X| = A N g / 2; a
            // constant, or a tone at half the sample rate, gives A N g.
            let edge = bin == 0 || (size % 2 == 0 && bin == last);
            let to_amplitude = if edge { 1.0 } else { 2.0 } / (size as f64 * coherent_gain);
            let gain = self.settings.weighting.gain(bin as f64 * spacing);
            *amplitude = to_amplitude * gain;
            // Power relative to a full-scale sine, which has a mean square
            // of 1/2. A bin away from the ends stands for its mirror image
            // as well; bin 0 and the half-rate bin do not.
            let edge_factor = if edge { 2.0 } else { 1.0 };
            *power = to_amplitude * to_amplitude * gain * gain * edge_factor / self.noise_bandwidth;
        }
        self.layout = BandLayout::new(&self.bands, size, sample_rate);
        self.ballistics = Ballistics::new(&self.settings, sample_rate);
    }

    /// Clears the history, the ballistics, the peaks and the frame count,
    /// and publishes an empty frame. Real-time safe.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.write = 0;
        self.until_frame = self.settings.fft_size;
        self.smoothed_bins.fill(0.0);
        self.smoothed_bands.fill(0.0);
        self.magnitudes.fill(SPECTRUM_FLOOR_DB);
        self.band_levels.fill(SPECTRUM_FLOOR_DB);
        self.reset_peaks();
        self.frames = 0;
        self.publish();
    }

    /// Clears the held peaks only. Real-time safe.
    pub fn reset_peaks(&mut self) {
        self.peaks.fill(SPECTRUM_FLOOR_DB);
        self.band_peaks.fill(SPECTRUM_FLOOR_DB);
        self.hold_left.fill(0);
        self.band_hold_left.fill(0);
        // Make the change visible even if no frame follows.
        self.published = u64::MAX;
    }

    /// Analyses one block, taking the samples its
    /// [`source`](SpectrumSettings::source) says, and publishes the newest
    /// frame if a transform was computed. Real-time safe.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        self.take_requests();
        let source = self.settings.source;
        source.for_each(buffer, |sample| self.push(sample));
        self.publish_if_new();
    }

    /// Analyses a stream of samples given directly, ignoring
    /// [`source`](SpectrumSettings::source). Real-time safe.
    pub fn measure_samples<S: Sample>(&mut self, samples: &[S]) {
        self.take_requests();
        for_each_sample(samples, |sample| self.push(sample));
        self.publish_if_new();
    }

    /// The settings as the analyser uses them, after any correction.
    pub fn settings(&self) -> &SpectrumSettings {
        &self.settings
    }

    /// The sample rate in use, in hertz.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The number of bins: `fft_size / 2 + 1`.
    pub fn bin_count(&self) -> usize {
        self.magnitudes.len()
    }

    /// The frequency of bin `bin`, in hertz.
    pub fn bin_frequency(&self, bin: usize) -> f64 {
        bin as f64 * self.sample_rate / self.settings.fft_size as f64
    }

    /// The bands, in the order of [`bands_db`](Self::bands_db).
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    /// The window's equivalent noise bandwidth, in bins: 1 for the
    /// rectangular window, 1.5 for Hann, about 3.8 for the flat top. The
    /// power of noise in a bin is its squared magnitude divided by this.
    pub fn noise_bandwidth(&self) -> f64 {
        self.noise_bandwidth
    }

    /// The level of each bin, after weighting and ballistics, in dBFS.
    pub fn magnitudes_db(&self) -> &[f64] {
        &self.magnitudes
    }

    /// The held peak of each bin, in dBFS.
    pub fn peaks_db(&self) -> &[f64] {
        &self.peaks
    }

    /// The level of each band, after weighting and ballistics, in dBFS.
    pub fn bands_db(&self) -> &[f64] {
        &self.band_levels
    }

    /// The held peak of each band, in dBFS.
    pub fn band_peaks_db(&self) -> &[f64] {
        &self.band_peaks
    }

    /// How many transforms have been computed since the last reset.
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    fn take_requests(&mut self) {
        if self.shared.reset.take() {
            self.reset();
        }
        if self.shared.reset_peaks.take() {
            self.reset_peaks();
        }
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
        self.until_frame -= 1;
        if self.until_frame == 0 {
            self.until_frame = self.settings.hop;
            self.transform();
        }
    }

    /// Transforms the last `fft_size` samples and moves every reading.
    fn transform(&mut self) {
        // Oldest sample first: the history from the write position on,
        // then from the start.
        let (newer, older) = self.history.split_at(self.write);
        for ((slot, &sample), &weight) in self
            .input
            .iter_mut()
            .zip(older.iter().chain(newer))
            .zip(self.window.iter())
        {
            *slot = sample * weight;
        }
        self.fft.forward(&self.input, &mut self.spectrum);

        let ballistics = self.ballistics;
        let scales = self.amplitude_scale.iter().zip(self.power_scale.iter());
        let readings = self
            .smoothed_bins
            .iter_mut()
            .zip(self.magnitudes.iter_mut())
            .zip(self.peaks.iter_mut())
            .zip(self.hold_left.iter_mut());
        for (
            ((value, (&amplitude, &power_scale)), power),
            (((smoothed, level), peak), hold_left),
        ) in self
            .spectrum
            .iter()
            .zip(scales)
            .zip(self.bin_power.iter_mut())
            .zip(readings)
        {
            let squared = value.norm_sqr();
            *power = squared * power_scale;
            // The squared amplitude relative to a full-scale sine.
            let raw = squared * amplitude * amplitude;
            ballistics.apply(raw, smoothed, level, peak, hold_left);
        }

        let readings = self
            .smoothed_bands
            .iter_mut()
            .zip(self.band_levels.iter_mut())
            .zip(self.band_peaks.iter_mut())
            .zip(self.band_hold_left.iter_mut());
        let mut start = 0;
        for (&end, (((smoothed, level), peak), hold_left)) in self.layout.ends.iter().zip(readings)
        {
            let entries = self.layout.entries.get(start..end).unwrap_or(&[]);
            let power: f64 = entries
                .iter()
                .map(|&(bin, share)| share * self.bin_power.get(bin).copied().unwrap_or(0.0))
                .sum();
            start = end;
            ballistics.apply(power, smoothed, level, peak, hold_left);
        }
        self.frames += 1;
    }

    fn publish_if_new(&mut self) {
        if self.published != self.frames {
            self.publish();
        }
    }

    /// Copies the current state into the frame the reader gets next.
    fn publish(&mut self) {
        let frame = self.writer.pending_mut();
        let narrow = |target: &mut [f32], source: &[f64]| {
            for (target, &source) in target.iter_mut().zip(source) {
                *target = source as f32;
            }
        };
        narrow(&mut frame.magnitudes, &self.magnitudes);
        narrow(&mut frame.peaks, &self.peaks);
        narrow(&mut frame.bands, &self.band_levels);
        narrow(&mut frame.band_peaks, &self.band_peaks);
        frame.sample_rate = self.sample_rate;
        frame.fft_size = self.settings.fft_size;
        frame.frames = self.frames;
        self.writer.publish();
        self.published = self.frames;
    }
}

/// As a processor the analyser analyses the block and leaves it unchanged,
/// so it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for SpectrumAnalyzer {
    fn prepare(&mut self, spec: &ProcessSpec) {
        SpectrumAnalyzer::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        SpectrumAnalyzer::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn prepared(settings: SpectrumSettings) -> (SpectrumAnalyzer, SpectrumReader) {
        let (mut analyzer, reader) = SpectrumAnalyzer::new(settings);
        analyzer.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: 1,
        });
        (analyzer, reader)
    }

    fn sine(frequency: f64, amplitude: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|n| amplitude * (core::f64::consts::TAU * frequency * n as f64 / RATE).sin())
            .collect()
    }

    #[test]
    fn the_noise_bandwidth_of_each_window_is_the_textbook_value() {
        // Harris (1978), table 1: equivalent noise bandwidth in bins.
        for (window, expected) in [
            (Window::Rectangular, 1.0),
            (Window::Triangular, 1.333),
            (Window::Hann, 1.5),
            (Window::Hamming, 1.363),
            (Window::Blackman, 1.727),
            (Window::BlackmanHarris, 2.004),
        ] {
            let (analyzer, _) =
                SpectrumAnalyzer::new(SpectrumSettings::new(4_096).with_window(window));
            let measured = analyzer.noise_bandwidth();
            assert!(
                (measured - expected).abs() < 0.002,
                "{window:?}: {measured}"
            );
        }
    }

    #[test]
    fn transforms_come_every_hop_once_the_first_block_is_full() {
        let (mut analyzer, _) = prepared(SpectrumSettings::new(256).with_hop(100));
        analyzer.measure_samples(&[0.0f32; 255]);
        assert_eq!(analyzer.frame_count(), 0);
        analyzer.measure_samples(&[0.0f32; 1]);
        assert_eq!(analyzer.frame_count(), 1);
        analyzer.measure_samples(&[0.0f32; 99]);
        assert_eq!(analyzer.frame_count(), 1);
        analyzer.measure_samples(&[0.0f32; 1_001]);
        assert_eq!(analyzer.frame_count(), 12);
        // A hop longer than the transform skips samples between frames.
        let (mut sparse, _) = prepared(SpectrumSettings::new(64).with_hop(1_000));
        sparse.measure_samples(&[0.0f32; 2_064]);
        assert_eq!(sparse.frame_count(), 3);
    }

    #[test]
    fn a_constant_and_a_tone_at_half_the_rate_read_their_own_value() {
        let (mut analyzer, _) =
            prepared(SpectrumSettings::new(512).with_window(Window::Rectangular));
        analyzer.measure_samples(&[0.5f64; 512]);
        assert!((analyzer.magnitudes_db()[0] - 20.0 * 0.5f64.log10()).abs() < 1e-9);
        let alternating: Vec<f64> = (0..512)
            .map(|n| if n % 2 == 0 { 0.25 } else { -0.25 })
            .collect();
        analyzer.reset();
        analyzer.measure_samples(&alternating);
        assert!((analyzer.magnitudes_db()[256] - 20.0 * 0.25f64.log10()).abs() < 1e-9);
        assert_eq!(analyzer.magnitudes_db()[0], SPECTRUM_FLOOR_DB);
        // A constant has the power of a sine 3 dB louder than its value.
        // With Hann it lands in bins 0 and 1, which the octave bands from
        // 0.09 Hz to 1.4 kHz cover but for the lowest 0.09 Hz of bin 0's
        // 46.9 Hz: 0.006 dB.
        let (mut banded, _) = prepared(
            SpectrumSettings::new(512)
                .with_window(Window::Hann)
                .with_bands(OctaveFraction::Whole)
                .with_band_range(0.1, 1_000.0),
        );
        banded.measure_samples(&[1.0f64; 512]);
        let total: f64 = banded
            .bands_db()
            .iter()
            .map(|db| 10f64.powf(db / 10.0))
            .sum();
        assert!(
            (10.0 * total.log10() - 3.0103).abs() < 0.01,
            "{}",
            10.0 * total.log10()
        );
    }

    #[test]
    fn ballistics_follow_their_time_constants() {
        // 50 ms attack and 200 ms release, frames of 480 samples (10 ms)
        // that do not overlap, so each one is all tone or all silence.
        let settings = SpectrumSettings::new(480)
            .with_hop(480)
            .with_window(Window::Rectangular)
            .with_ballistics(0.05, 0.2);
        let (mut analyzer, _) = prepared(settings);
        // 1 kHz: ten whole periods per frame, on bin 10.
        let bin = 10;
        let tone = sine(bin as f64 * RATE / 480.0, 1.0, 480 * 100);
        // From silence, after n frames the power has covered
        // 1 - e^(-n 10 ms / 50 ms) of the way: 2 dB under after 50 ms.
        for (n, frame) in tone.chunks(480).take(12).enumerate() {
            analyzer.measure_samples(frame);
            let elapsed = (n + 1) as f64 * 0.01;
            let expected = 10.0 * (1.0 - (-elapsed / 0.05).exp()).log10();
            let read = analyzer.magnitudes_db()[bin];
            assert!((read - expected).abs() < 1e-9, "{elapsed} s: {read}");
            if n == 4 {
                assert!((read + 2.0).abs() < 0.01, "{read}");
            }
        }
        // A second in all, twenty time constants: e^-20 of the way is left.
        analyzer.measure_samples(&tone[12 * 480..]);
        let start = analyzer.magnitudes_db()[bin];
        let expected = 10.0 * (1.0 - (-20.0f64).exp()).log10();
        assert!((start - expected).abs() < 1e-12, "{start}");
        // Then release into silence: a straight line in decibels, 4.34 dB
        // per release time, 21.7 dB/s, all the way down.
        let per_second = 10.0 * core::f64::consts::E.log10() / 0.2;
        for n in 1..=200 {
            analyzer.measure_samples(&[0.0f64; 480]);
            let expected = start - per_second * n as f64 * 0.01;
            let read = analyzer.magnitudes_db()[bin];
            assert!((read - expected).abs() < 1e-9, "frame {n}: {read}");
        }
        // The peak stayed where the level was highest.
        assert_eq!(analyzer.peaks_db()[bin], start);
    }

    #[test]
    fn peaks_are_held_then_fall_and_can_be_cleared() {
        // Frames every 10 ms, hold 100 ms, then fall 50 dB/s.
        let settings = SpectrumSettings::new(480)
            .with_hop(480)
            .with_window(Window::Rectangular)
            .with_peak_hold(0.1, 50.0);
        let (mut analyzer, mut reader) = prepared(settings);
        let bin = 7;
        analyzer.measure_samples(&sine(bin as f64 * 100.0, 1.0, 480));
        assert!(analyzer.peaks_db()[bin].abs() < 1e-9);
        // Ten silent frames: still held.
        analyzer.measure_samples(&vec![0.0f64; 480 * 10]);
        assert!(analyzer.peaks_db()[bin].abs() < 1e-9);
        assert_eq!(analyzer.magnitudes_db()[bin], SPECTRUM_FLOOR_DB);
        // Ten more: falling by half a decibel per frame.
        analyzer.measure_samples(&vec![0.0f64; 480 * 10]);
        assert!(
            (analyzer.peaks_db()[bin] + 5.0).abs() < 1e-9,
            "{}",
            analyzer.peaks_db()[bin]
        );
        // A reader can clear the peaks; the analyser does it next block.
        reader.request_peak_reset();
        assert!(reader.latest().peaks_db()[bin] > -6.0);
        analyzer.measure_samples::<f64>(&[]);
        assert_eq!(analyzer.peaks_db()[bin], SPECTRUM_FLOOR_DB);
        assert_eq!(reader.latest().peaks_db()[bin], SPECTRUM_FLOOR_DB as f32);
        assert_eq!(reader.frame().frame_count(), 21);
        // And can ask for a full reset.
        reader.request_reset();
        analyzer.measure_samples::<f64>(&[]);
        assert_eq!(reader.latest().frame_count(), 0);
    }

    #[test]
    fn weighting_scales_each_bin_by_the_curve() {
        for weighting in [Weighting::A, Weighting::C] {
            let settings = SpectrumSettings::new(4_800).with_weighting(weighting);
            let (mut analyzer, _) = prepared(settings);
            // Bins are 10 Hz apart: 100 Hz, 1 kHz and 10 kHz fall on bins.
            for frequency in [100.0, 1_000.0, 10_000.0] {
                analyzer.reset();
                analyzer.measure_samples(&sine(frequency, 1.0, 4_800));
                let bin = (frequency / 10.0) as usize;
                let read = analyzer.magnitudes_db()[bin];
                let expected = weighting.gain_db(frequency);
                assert!(
                    (read - expected).abs() < 1e-6,
                    "{weighting:?} {frequency}: {read}"
                );
            }
        }
    }

    #[test]
    fn settings_that_make_no_sense_are_made_usable() {
        let settings = SpectrumSettings::new(0)
            .with_hop(0)
            .with_window(Window::Kaiser(f64::NAN))
            .with_ballistics(f64::NAN, -1.0)
            .with_peak_hold(f64::NAN, f64::INFINITY)
            .sanitised();
        assert_eq!((settings.fft_size, settings.hop), (2, 1));
        assert_eq!(settings.window, Window::Kaiser(0.0));
        assert_eq!(
            (settings.attack_seconds, settings.release_seconds),
            (0.0, 0.0)
        );
        assert_eq!(settings.peak_hold_seconds, f64::INFINITY);
        assert_eq!(settings.peak_fall_db_per_second, 0.0);
        let huge = SpectrumSettings::new(8)
            .with_window(Window::Kaiser(-1e6))
            .sanitised();
        assert_eq!(huge.window, Window::Kaiser(MAX_KAISER_BETA));
        let (mut analyzer, mut reader) = SpectrumAnalyzer::new(huge);
        analyzer.measure_samples(&[1.0f32; 64]);
        assert!(
            reader
                .latest()
                .magnitudes_db()
                .iter()
                .all(|db| db.is_finite())
        );
        assert_eq!(SpectrumSettings::default().fft_size, 4_096);
    }
}
