//! Programme loudness after ITU-R BS.1770-4 and EBU R 128.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use manguebyte_core::{AtomicF64, AudioBuffer, Sample};
use manguebyte_dsp::{ProcessSpec, Processor};

use crate::gating::{GatedLoudness, RangeHistogram, lufs};
use crate::kweighting::KWeighting;
use crate::shared::{DEFAULT_SAMPLE_RATE, Flag, screened, usable_rate};

/// The programme loudness EBU R 128 asks for, in LUFS. On the relative
/// scale of an EBU meter this level is 0 LU.
pub const EBU_R128_TARGET_LUFS: f64 = -23.0;

/// The meter cuts time into slices of 10 ms and builds every window out of
/// whole slices.
const SLICES_PER_SECOND: f64 = 100.0;

/// The momentary window and the gating block: 400 ms.
const MOMENTARY_SLICES: usize = 40;

/// The short-term window: 3 s.
const SHORT_TERM_SLICES: usize = 300;

/// A new gating block, and a new short-term value for the loudness range,
/// every 100 ms. For 400 ms blocks that is the 75% overlap of BS.1770-4;
/// for the range it is the 10 Hz that Tech 3342 requires.
const STEP_SLICES: u64 = 10;

/// How much one channel counts towards the loudness of the programme.
///
/// BS.1770-4 sums the power of the channels with a weight each (Table 3 for
/// five channels, Table 4 for any loudspeaker position) and leaves low
/// frequency effects channels out.
///
/// The Recommendation is revised from time to time, so a later version of
/// this crate may name further weights: a `match` on this enum needs a
/// wildcard arm. [`gain`](Self::gain) gives the number for any of them.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ChannelWeight {
    /// Weight 1.0: left, right, centre, and every position Table 4 of the
    /// Recommendation does not single out.
    Unity,
    /// Weight 1.41, about +1.5 dB: surround channels, to the sides of the
    /// listener.
    Surround,
    /// Not measured at all: a low frequency effects channel.
    Excluded,
    /// Any other power weight. Not part of the Recommendation; a mono file
    /// that will be played over two loudspeakers can be measured with
    /// `Custom(2.0)`, for example. A weight that is not a positive number
    /// excludes the channel.
    Custom(f64),
}

impl ChannelWeight {
    /// The weight as the factor the channel's power is multiplied by.
    pub fn gain(self) -> f64 {
        match self {
            Self::Unity => 1.0,
            Self::Surround => 1.41,
            Self::Excluded => 0.0,
            Self::Custom(gain) if gain.is_finite() && gain > 0.0 => gain,
            Self::Custom(_) => 0.0,
        }
    }

    /// The weight of a loudspeaker at the given azimuth and elevation in
    /// degrees, by Table 4 of BS.1770-4: [`Surround`](Self::Surround) for
    /// an elevation within 30 degrees of the horizontal and an azimuth from
    /// 60 to 120 degrees to either side, [`Unity`](Self::Unity) anywhere
    /// else.
    ///
    /// ```
    /// use manguebyte_analysis::ChannelWeight;
    ///
    /// assert_eq!(ChannelWeight::at_position(30.0, 0.0), ChannelWeight::Unity);
    /// assert_eq!(ChannelWeight::at_position(-110.0, 0.0), ChannelWeight::Surround);
    /// assert_eq!(ChannelWeight::at_position(110.0, 45.0), ChannelWeight::Unity);
    /// ```
    pub fn at_position(azimuth_degrees: f64, elevation_degrees: f64) -> Self {
        // Fold the azimuth into 0..=180 to either side.
        let turned = azimuth_degrees.rem_euclid(360.0);
        let azimuth = if turned > 180.0 {
            360.0 - turned
        } else {
            turned
        };
        if elevation_degrees.abs() < 30.0 && (60.0..=120.0).contains(&azimuth) {
            Self::Surround
        } else {
            Self::Unity
        }
    }
}

/// The readings of a [`LoudnessMeter`] side by side, as
/// [`LoudnessReadings::snapshot`] returns them.
///
/// Loudness is in LUFS, and is minus infinity where there is nothing to
/// report: silence, or an integrated loudness before any block has passed
/// the gate.
///
/// A later version of the meter may have more readings (the loudness of
/// each channel on its own is one it does not have yet), and they would
/// become fields here. The struct is therefore non-exhaustive: its fields
/// can be read, but outside this crate it cannot be built, nor taken apart
/// without `..`.
///
/// ```
/// use manguebyte_analysis::{Loudness, LoudnessMeter};
///
/// let meter = LoudnessMeter::stereo();
/// let Loudness { integrated, range, .. } = meter.readings().snapshot();
/// // Nothing has been measured yet.
/// assert_eq!((integrated, range), (f64::NEG_INFINITY, 0.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Loudness {
    /// Momentary loudness: the last 400 ms.
    pub momentary: f64,
    /// Short-term loudness: the last 3 s.
    pub short_term: f64,
    /// Integrated (programme) loudness: everything since the last reset,
    /// gated.
    pub integrated: f64,
    /// Loudness range in LU.
    pub range: f64,
    /// The highest momentary loudness since the last reset.
    pub max_momentary: f64,
    /// The highest short-term loudness since the last reset.
    pub max_short_term: f64,
}

#[derive(Debug)]
struct Shared {
    momentary: AtomicF64,
    short_term: AtomicF64,
    integrated: AtomicF64,
    range: AtomicF64,
    max_momentary: AtomicF64,
    max_short_term: AtomicF64,
    measured_seconds: AtomicF64,
    paused: AtomicBool,
    reset: Flag,
}

/// The readings and the controls of a [`LoudnessMeter`], for any thread.
///
/// Cloning is cheap and every clone belongs to the same meter. Nothing here
/// locks or allocates: readings are atomic loads, and the two controls set
/// a flag that the audio thread looks at when its next block begins.
///
/// Each reading is consistent in itself. Two readings taken one after the
/// other may come from either side of an update.
#[derive(Debug, Clone)]
pub struct LoudnessReadings {
    shared: Arc<Shared>,
}

impl LoudnessReadings {
    /// Momentary loudness in LUFS: a sliding 400 ms window, not gated.
    /// Updated every 10 ms.
    pub fn momentary(&self) -> f64 {
        self.shared.momentary.load(Ordering::Relaxed)
    }

    /// Short-term loudness in LUFS: a sliding 3 s window, not gated.
    /// Updated every 10 ms.
    pub fn short_term(&self) -> f64 {
        self.shared.short_term.load(Ordering::Relaxed)
    }

    /// Integrated loudness in LUFS: the gated loudness of everything
    /// measured since the last reset. Updated every 100 ms. Minus infinity
    /// until a 400 ms block has been louder than -70 LUFS.
    pub fn integrated(&self) -> f64 {
        self.shared.integrated.load(Ordering::Relaxed)
    }

    /// Loudness range (LRA) in LU. Updated every 100 ms, from three seconds
    /// in. Zero until then.
    ///
    /// Tech 3341 asks a meter to show that the value is not yet stable
    /// during the first minute; [`measured_seconds`](Self::measured_seconds)
    /// tells when that minute is over.
    pub fn range(&self) -> f64 {
        self.shared.range.load(Ordering::Relaxed)
    }

    /// The highest momentary loudness since the last reset, in LUFS.
    ///
    /// Only complete windows count: the first value appears 400 ms after a
    /// reset, and it never includes audio from before the reset or from a
    /// pause. Minus infinity until then.
    pub fn max_momentary(&self) -> f64 {
        self.shared.max_momentary.load(Ordering::Relaxed)
    }

    /// The highest short-term loudness since the last reset, in LUFS.
    ///
    /// Only complete windows count, so the first value appears three
    /// seconds after a reset. Minus infinity until then.
    pub fn max_short_term(&self) -> f64 {
        self.shared.max_short_term.load(Ordering::Relaxed)
    }

    /// How much audio the integrated loudness and the range are based on,
    /// in seconds: the time since the last reset, without the time spent
    /// paused.
    pub fn measured_seconds(&self) -> f64 {
        self.shared.measured_seconds.load(Ordering::Relaxed)
    }

    /// All readings, loaded one after the other. If the audio thread
    /// updates them in between, some are from before the update and some
    /// from after.
    pub fn snapshot(&self) -> Loudness {
        Loudness {
            momentary: self.momentary(),
            short_term: self.short_term(),
            integrated: self.integrated(),
            range: self.range(),
            max_momentary: self.max_momentary(),
            max_short_term: self.max_short_term(),
        }
    }

    /// Asks the meter to start its integrated loudness, loudness range and
    /// maxima again. It does so when its next block begins, on the audio
    /// thread. Momentary and short-term loudness carry on.
    ///
    /// Nothing from before the reset enters the new measurement: its first
    /// gating block begins with the first whole 10 ms slice after it.
    pub fn request_reset(&self) {
        self.shared.reset.raise();
    }

    /// Pauses or continues the measurement of integrated loudness, loudness
    /// range and the maxima, from the next block on. Momentary and
    /// short-term loudness are measured regardless.
    ///
    /// Audio from before a pause and audio from after it are never put into
    /// one gating block: after continuing, the first block begins with the
    /// first whole 10 ms slice and is complete 400 ms after that.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Whether the measurement is paused.
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }
}

/// A loudness meter after ITU-R BS.1770-4 and EBU R 128: momentary,
/// short-term and integrated loudness, loudness range, and the maxima of
/// the first two.
///
/// Loudness normalisation replaces "how high are the peaks" by "how loud
/// does it sound". The measure is the power of the signal after a frequency
/// weighting ([`KWeighting`]), summed over the channels with more weight on
/// the surrounds, taken on three time scales, and reported in LUFS: loudness
/// units relative to full scale, a decibel scale on which a full-scale
/// 997 Hz sine in one front channel reads -3.01.
///
/// | Reading | Window | Defined in |
/// |---|---|---|
/// | Momentary | sliding 400 ms | EBU Tech 3341 |
/// | Short-term | sliding 3 s | EBU Tech 3341 |
/// | Integrated | since reset, over 400 ms blocks; blocks under -70 LUFS are dropped, then blocks more than 10 LU under the level of the rest | BS.1770-4 |
/// | Range (LRA) | since reset; 10th to 95th percentile of the short-term values, after dropping those under -70 LUFS and then those more than 20 LU under the level of the rest | EBU Tech 3342 |
///
/// # Using it
///
/// Create it with the weight of each channel, [`prepare`](Self::prepare) it
/// with the sample rate, and hand it every block with
/// [`measure`](Self::measure) or, in a chain of processors, through
/// [`Processor::process`], which measures and leaves the audio alone. Clone
/// [`readings`](Self::readings) for the thread that draws the meter.
///
/// ```
/// use manguebyte_analysis::LoudnessMeter;
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::ProcessSpec;
///
/// let mut meter = LoudnessMeter::stereo();
/// meter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 2 });
/// // For the user interface: reads from any thread, without locks.
/// let readings = meter.readings().clone();
///
/// // Five seconds of a 1 kHz sine at -23 dBFS in both channels.
/// let amplitude = 10f64.powf(-23.0 / 20.0);
/// let mut block = AudioBuffer::<f32>::new(2, 4_800);
/// for index in 0..50 {
///     for frame in 0..4_800 {
///         let time = (index * 4_800 + frame) as f64 / 48_000.0;
///         let sample = (amplitude * (core::f64::consts::TAU * 1_000.0 * time).sin()) as f32;
///         block.channel_mut(0)[frame] = sample;
///         block.channel_mut(1)[frame] = sample;
///     }
///     meter.measure(&block);
/// }
///
/// assert!((readings.momentary() + 23.0).abs() < 0.1);
/// assert!((readings.short_term() + 23.0).abs() < 0.1);
/// assert!((readings.integrated() + 23.0).abs() < 0.1);
/// assert!(readings.range() < 0.1);
/// ```
///
/// # Real-time behaviour
///
/// Everything is allocated by the constructor, about 320 kB, and nothing
/// after it allocates, locks or waits, for a programme of any length. The
/// work per block is two second-order filters per channel, plus a few
/// hundred additions every 10 ms and a pass over the occupied part of a
/// histogram every 100 ms. The readings do not depend on how the audio is
/// cut into blocks, to the last bit.
///
/// # Accuracy and limits
///
/// - **Windows.** Time is cut into 10 ms slices and every window is made
///   of whole slices, so the "sliding" windows slide in steps of 10 ms,
///   ten times finer than Tech 3341 requires. The maximum of the momentary
///   loudness of a 400 ms burst can therefore be up to 0.055 LU low, when
///   the burst starts 5 ms off the grid. At sample rates that are not a
///   multiple of 100 Hz the slices alternate between two lengths so that
///   the windows stay 400 ms and 3 s to within one sample.
/// - **Before a window is full**, the momentary and short-term readings
///   treat the time before the start as silence. Gating blocks and the
///   short-term values behind the loudness range are only taken once they
///   are complete, as BS.1770-4 asks.
/// - **Integrated loudness** keeps its gating blocks in a histogram of
///   0.01 LU bins instead of a list, which is what bounds the memory. The
///   mean of the blocks is exact. The relative gate decides whole bins,
///   which is exact whenever the blocks in the gate's own bin lie on one
///   side of it, and otherwise equals the exact method with the gate moved
///   by less than 0.01 LU.
/// - **Loudness range** counts its short-term values in a histogram of the
///   same bins, and that is approximate in two ways. Each percentile is
///   reported as the centre of its bin, which moves the range by at most
///   0.01 LU. And the relative gate decides whole bins, as it does for
///   integrated loudness: that is exact whenever the values in the gate's
///   own bin lie on one side of it, and otherwise equals the exact method
///   with the gate moved by less than 0.01 LU. So the range is within
///   0.01 LU of the function printed in Tech 3342, unless short-term values
///   lie on both sides of the gate and less than 0.01 LU from it. Then the
///   meter keeps or drops them together where the exact method keeps some.
///   That can change the result by far more than 0.01 LU, because the
///   range is a distance between percentiles and the values that are kept
///   decide where the 10th percentile falls. In a programme built for it,
///   with a quiet passage that creeps across the gate 21 LU under a loud
///   one, the meter reads 0 LU where the exact method reads 21 LU. The
///   exact method has the same cliff: with its gate 0.01 LU higher it
///   reads 0 LU too.
/// - **Levels above +30 LUFS**, which no signal within full scale reaches,
///   all fall into the last bin of the histograms: their mean stays exact,
///   but the relative gate and the range no longer tell them apart.
/// - **Other sample rates** use K-weighting filters derived from the 48 kHz
///   ones; see [`KWeighting`] for how closely they match.
/// - **Channels.** The weights are fixed at construction. A block with more
///   channels has the extra ones ignored; one with fewer measures the
///   missing ones as silence. A mono signal is one channel of weight 1, as
///   the Recommendation has it, and so reads 3 LU lower than the same
///   signal in both channels of a stereo pair.
/// - **Bad samples.** A sample that is not finite (a NaN or an infinity) is
///   measured as silence and one beyond 180 dB over full scale as 180 dB
///   over, so that no input can leave the meter stuck. A sample at that
///   limit is out of the momentary reading half a second later and out of
///   the short-term reading after 3.1 s: the length of the window, and
///   some 70 ms for which the high-pass of the K-weighting rings on after
///   a sample that large. Integrated loudness, range and the maxima are
///   taken over everything since the last reset, and keep it until the
///   next one.
/// - **Not included:** the loudness of each channel on its own, and the
///   momentary loudness of ITU-R BS.1771, which is an exponential average
///   where EBU Mode uses a window.
#[derive(Debug)]
pub struct LoudnessMeter {
    /// Power weight of each channel; zero for one that is left out.
    gains: Vec<f64>,
    sample_rate: f64,
    filters: KWeighting,
    /// Per channel, the memory of the shelf and then of the high-pass.
    memory: Vec<[f64; 4]>,
    /// Per channel, the sum of squared filtered samples of the slice under
    /// way.
    partial: Vec<f64>,
    /// Samples still to come in the slice under way.
    remaining: usize,
    /// Slices completed since the last full reset. Places the boundaries.
    slices: u64,
    /// The channel-weighted sum of squares of each of the last 300 slices.
    /// `next` is where the newest will go, which is where the oldest is.
    ring: Vec<f64>,
    next: usize,
    /// Whether integration is running, as opposed to paused.
    running: bool,
    /// Slices completed since integration last started or continued.
    integrating: u64,
    /// Whether the slice under way began before integration started or
    /// continued, and so is not to be counted.
    skip_slice: bool,
    /// Slices that went into the integration since the last reset.
    measured: u64,
    integrated: GatedLoudness,
    range: RangeHistogram,
    /// The highest window means so far, as mean squares.
    max_momentary: f64,
    max_short_term: f64,
    readings: LoudnessReadings,
}

impl LoudnessMeter {
    /// Creates a meter with one weight per channel, in the order the
    /// channels have in the buffers it will be given. This allocates;
    /// nothing after it does.
    ///
    /// Until [`prepare`](Self::prepare) is called it assumes 44.1 kHz.
    pub fn new(weights: &[ChannelWeight]) -> Self {
        let channels = weights.len();
        let mut meter = Self {
            gains: weights.iter().map(|weight| weight.gain()).collect(),
            sample_rate: DEFAULT_SAMPLE_RATE,
            filters: KWeighting::for_sample_rate(DEFAULT_SAMPLE_RATE),
            memory: vec![[0.0; 4]; channels],
            partial: vec![0.0; channels],
            remaining: 1,
            slices: 0,
            ring: vec![0.0; SHORT_TERM_SLICES],
            next: 0,
            running: true,
            integrating: 0,
            skip_slice: false,
            measured: 0,
            integrated: GatedLoudness::new(),
            range: RangeHistogram::new(),
            max_momentary: 0.0,
            max_short_term: 0.0,
            readings: LoudnessReadings {
                shared: Arc::new(Shared {
                    momentary: AtomicF64::new(f64::NEG_INFINITY),
                    short_term: AtomicF64::new(f64::NEG_INFINITY),
                    integrated: AtomicF64::new(f64::NEG_INFINITY),
                    range: AtomicF64::new(0.0),
                    max_momentary: AtomicF64::new(f64::NEG_INFINITY),
                    max_short_term: AtomicF64::new(f64::NEG_INFINITY),
                    measured_seconds: AtomicF64::new(0.0),
                    paused: AtomicBool::new(false),
                    reset: Flag::default(),
                }),
            },
        };
        meter.reset();
        meter
    }

    /// A meter for one channel of weight 1.
    pub fn mono() -> Self {
        Self::new(&[ChannelWeight::Unity])
    }

    /// A meter for left and right.
    pub fn stereo() -> Self {
        Self::new(&[ChannelWeight::Unity; 2])
    }

    /// A meter for five channels in the order left, right, centre, left
    /// surround, right surround.
    pub fn surround_5_0() -> Self {
        use ChannelWeight::{Surround, Unity};
        Self::new(&[Unity, Unity, Unity, Surround, Surround])
    }

    /// A meter for six channels in the order left, right, centre, low
    /// frequency effects, left surround, right surround: the order of WAV
    /// files and of most hosts. The fourth channel is not measured.
    pub fn surround_5_1() -> Self {
        use ChannelWeight::{Excluded, Surround, Unity};
        Self::new(&[Unity, Unity, Unity, Excluded, Surround, Surround])
    }

    /// How many channels the meter has a weight for.
    pub fn num_channels(&self) -> usize {
        self.gains.len()
    }

    /// Takes the sample rate from `spec`, derives the filters for it and
    /// starts the whole measurement again. The channels were fixed by the
    /// constructor.
    pub fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = usable_rate(spec.sample_rate);
        self.filters = KWeighting::for_sample_rate(self.sample_rate);
        self.reset();
    }

    /// Starts the whole measurement again: filters, windows, integrated
    /// loudness, range and maxima. Whether the meter is paused is kept.
    pub fn reset(&mut self) {
        self.memory.fill([0.0; 4]);
        self.partial.fill(0.0);
        self.ring.fill(0.0);
        self.next = 0;
        self.slices = 0;
        self.remaining = self.slice_frames(0);
        let shared = &self.readings.shared;
        shared.momentary.store(f64::NEG_INFINITY, Ordering::Relaxed);
        shared
            .short_term
            .store(f64::NEG_INFINITY, Ordering::Relaxed);
        self.reset_integration();
    }

    /// Starts integrated loudness, loudness range and the maxima again,
    /// which is what the reset button of an EBU meter does. Momentary and
    /// short-term loudness carry on.
    ///
    /// From another thread, use [`LoudnessReadings::request_reset`].
    pub fn reset_integration(&mut self) {
        self.integrated.clear();
        self.range.clear();
        self.max_momentary = 0.0;
        self.max_short_term = 0.0;
        self.restart_count();
        self.measured = 0;
        let shared = &self.readings.shared;
        shared
            .integrated
            .store(f64::NEG_INFINITY, Ordering::Relaxed);
        shared.range.store(0.0, Ordering::Relaxed);
        shared
            .max_momentary
            .store(f64::NEG_INFINITY, Ordering::Relaxed);
        shared
            .max_short_term
            .store(f64::NEG_INFINITY, Ordering::Relaxed);
        shared.measured_seconds.store(0.0, Ordering::Relaxed);
    }

    /// The readings and controls, to use here or to clone for another
    /// thread.
    pub fn readings(&self) -> &LoudnessReadings {
        &self.readings
    }

    /// Measures one block. Real-time safe.
    pub fn measure<S: Sample>(&mut self, buffer: &AudioBuffer<S>) {
        self.obey();
        let frames = buffer.num_frames();
        let channels = buffer.num_channels().min(self.gains.len());
        let KWeighting { shelf, highpass } = self.filters;

        let mut start = 0;
        while start < frames {
            // Up to the end of the block or of the slice, whichever is
            // first.
            let end = start + (frames - start).min(self.remaining);
            for channel in 0..channels {
                if self.gains[channel] == 0.0 {
                    continue;
                }
                let [mut s1, mut s2, mut h1, mut h2] = self.memory[channel];
                let mut sum = self.partial[channel];
                for &sample in &buffer.channel(channel)[start..end] {
                    let input = screened(sample.to_f64());
                    // Both filters in transposed direct form II.
                    let shelved = shelf.b0 * input + s1;
                    s1 = shelf.b1 * input - shelf.a1 * shelved + s2;
                    s2 = shelf.b2 * input - shelf.a2 * shelved;
                    let weighted = highpass.b0 * shelved + h1;
                    h1 = highpass.b1 * shelved - highpass.a1 * weighted + h2;
                    h2 = highpass.b2 * shelved - highpass.a2 * weighted;
                    sum += weighted * weighted;
                }
                self.memory[channel] = [s1, s2, h1, h2];
                self.partial[channel] = sum;
            }
            self.remaining -= end - start;
            start = end;
            if self.remaining == 0 {
                self.finish_slice();
            }
        }
    }

    /// Carries out what another thread asked for through the readings.
    fn obey(&mut self) {
        if self.readings.shared.reset.take() {
            self.reset_integration();
        }
        let running = !self.readings.shared.paused.load(Ordering::Relaxed);
        if running != self.running {
            self.running = running;
            self.restart_count();
        }
    }

    /// Starts counting the slices of the measurement from here. Gating
    /// blocks, short-term values and maxima are only taken from windows
    /// made of counted slices, which keeps everything from before this
    /// moment out of them.
    fn restart_count(&mut self) {
        self.integrating = 0;
        // A slice that is already under way holds audio from before.
        self.skip_slice = self.remaining != self.slice_frames(self.slices);
    }

    /// Closes the slice under way: updates the windows, and every tenth
    /// time the gated measures.
    fn finish_slice(&mut self) {
        let mut energy = 0.0;
        for (partial, &gain) in self.partial.iter_mut().zip(&self.gains) {
            energy += gain * *partial;
            *partial = 0.0;
        }
        self.ring[self.next] = energy;
        self.next = (self.next + 1) % SHORT_TERM_SLICES;
        self.slices += 1;
        self.remaining = self.slice_frames(self.slices);

        let momentary = self.window_mean(MOMENTARY_SLICES);
        let short_term = self.window_mean(SHORT_TERM_SLICES);
        let shared = &self.readings.shared;
        shared.momentary.store(lufs(momentary), Ordering::Relaxed);
        shared.short_term.store(lufs(short_term), Ordering::Relaxed);

        if !self.running {
            return;
        }
        if self.skip_slice {
            self.skip_slice = false;
            return;
        }
        self.integrating += 1;
        self.measured += 1;
        shared
            .measured_seconds
            .store(self.measured as f64 / SLICES_PER_SECOND, Ordering::Relaxed);
        // Everything below looks only at windows that lie wholly inside the
        // measurement, so that nothing from before a reset or from a pause
        // leaks into it.
        let elapsed = self.integrating;
        if elapsed >= MOMENTARY_SLICES as u64 && momentary > self.max_momentary {
            self.max_momentary = momentary;
            shared
                .max_momentary
                .store(lufs(momentary), Ordering::Relaxed);
        }
        if elapsed >= SHORT_TERM_SLICES as u64 && short_term > self.max_short_term {
            self.max_short_term = short_term;
            shared
                .max_short_term
                .store(lufs(short_term), Ordering::Relaxed);
        }

        // The first gating block is complete after 400 ms, and another one
        // every 100 ms from then on. Each is the momentary window at that
        // moment.
        if is_step(elapsed, MOMENTARY_SLICES) {
            self.integrated.push(momentary);
            shared
                .integrated
                .store(self.integrated.loudness(), Ordering::Relaxed);
        }
        // The same for the short-term values of the loudness range, from
        // three seconds in.
        if is_step(elapsed, SHORT_TERM_SLICES) {
            self.range.push(short_term);
            shared.range.store(self.range.range(), Ordering::Relaxed);
        }
    }

    /// The mean square over the newest `slices` slices.
    fn window_mean(&self, slices: usize) -> f64 {
        let first = (self.next + SHORT_TERM_SLICES - slices) % SHORT_TERM_SLICES;
        let mut sum = 0.0;
        // Always summed afresh and oldest first, so nothing drifts and the
        // result does not depend on where the ring happens to wrap.
        for offset in 0..slices {
            sum += self.ring[(first + offset) % SHORT_TERM_SLICES];
        }
        let now = self.slices as f64;
        sum / (self.boundary(now) - self.boundary(now - slices as f64))
    }

    /// The sample at which slice number `slice` begins, counted from the
    /// last full reset. Negative numbers are the silence before it.
    fn boundary(&self, slice: f64) -> f64 {
        // At least one sample to the slice, whatever the rate claims to be.
        let frames_per_slice = (self.sample_rate / SLICES_PER_SECOND).max(1.0);
        (slice * frames_per_slice).round()
    }

    /// How many samples slice number `slice` has.
    fn slice_frames(&self, slice: u64) -> usize {
        let slice = slice as f64;
        ((self.boundary(slice + 1.0) - self.boundary(slice)) as usize).max(1)
    }
}

/// Whether a window of `window` slices that is taken every [`STEP_SLICES`]
/// is due after `elapsed` slices.
fn is_step(elapsed: u64, window: usize) -> bool {
    let window = window as u64;
    elapsed >= window && (elapsed - window) % STEP_SLICES == 0
}

/// As a processor the meter measures the block and leaves it unchanged, so
/// it can sit anywhere in a chain.
impl<S: Sample> Processor<S> for LoudnessMeter {
    fn prepare(&mut self, spec: &ProcessSpec) {
        LoudnessMeter::prepare(self, spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.measure(buffer);
    }

    fn reset(&mut self) {
        LoudnessMeter::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::TAU;

    use super::*;

    fn spec(sample_rate: f64, channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 4_800,
            num_channels: channels,
        }
    }

    /// Feeds `seconds` of a sine at `frequency` and `dbfs` into every
    /// channel, continuing from sample `position`.
    fn feed(
        meter: &mut LoudnessMeter,
        sample_rate: f64,
        frequency: f64,
        dbfs: f64,
        seconds: f64,
        position: &mut u64,
    ) {
        let amplitude = 10f64.powf(dbfs / 20.0);
        let mut left = (seconds * sample_rate).round() as usize;
        let mut buffer = AudioBuffer::<f64>::new(meter.num_channels(), 4_800);
        while left > 0 {
            let frames = left.min(4_800);
            buffer.set_num_frames(frames);
            for channel in buffer.iter_channels_mut() {
                for (frame, sample) in channel.iter_mut().enumerate() {
                    let n = (*position + frame as u64) as f64;
                    *sample = amplitude * (TAU * frequency * n / sample_rate).sin();
                }
            }
            meter.measure(&buffer);
            *position += frames as u64;
            left -= frames;
        }
    }

    /// The K-weighting gain at 1 kHz is 0.007 dB more than the constant in
    /// the loudness formula takes off, so that is what a 1 kHz tone reads
    /// over its level.
    const AT_1_KHZ: f64 = 0.007;

    #[test]
    fn a_full_scale_sine_in_one_front_channel_reads_minus_3_01() {
        // BS.1770-4, Annex 1: "If a 0 dB FS, 1 kHz (997 Hz to be exact)
        // sine wave is applied to the left, centre, or right channel input,
        // the indicated loudness will equal -3.01 LKFS."
        for (mut meter, channel) in [
            (LoudnessMeter::surround_5_0(), 0),
            (LoudnessMeter::surround_5_0(), 1),
            (LoudnessMeter::surround_5_0(), 2),
            (LoudnessMeter::mono(), 0),
        ] {
            meter.prepare(&spec(48_000.0, meter.num_channels()));
            let mut buffer = AudioBuffer::<f64>::new(meter.num_channels(), 4_800);
            for block in 0..50 {
                for (frame, sample) in buffer.channel_mut(channel).iter_mut().enumerate() {
                    let n = (block * 4_800 + frame) as f64;
                    *sample = (TAU * 997.0 * n / 48_000.0).sin();
                }
                meter.measure(&buffer);
            }
            let loudness = meter.readings().snapshot();
            for reading in [loudness.momentary, loudness.short_term, loudness.integrated] {
                assert!((reading + 3.01).abs() < 0.005, "{reading}");
            }
        }
    }

    #[test]
    fn surround_channels_weigh_more_and_the_lfe_nothing() {
        // The same tone in one channel at a time of a 5.1 meter.
        let reading = |channel: usize| {
            let mut meter = LoudnessMeter::surround_5_1();
            meter.prepare(&spec(48_000.0, 6));
            let mut buffer = AudioBuffer::<f64>::new(6, 4_800);
            for block in 0..10 {
                for (frame, sample) in buffer.channel_mut(channel).iter_mut().enumerate() {
                    let n = (block * 4_800 + frame) as f64;
                    *sample = 0.1 * (TAU * 997.0 * n / 48_000.0).sin();
                }
                meter.measure(&buffer);
            }
            meter.readings().integrated()
        };
        let front = reading(0);
        assert!((front + 23.01).abs() < 0.005, "{front}");
        assert!((reading(1) - front).abs() < 1e-9);
        assert!((reading(2) - front).abs() < 1e-9);
        assert_eq!(reading(3), f64::NEG_INFINITY);
        // 10 log10(1.41) = 1.49 dB.
        assert!((reading(4) - front - 1.492).abs() < 1e-3);
        assert!((reading(5) - reading(4)).abs() < 1e-9);
    }

    #[test]
    fn weights_follow_the_tables_of_the_recommendation() {
        assert_eq!(ChannelWeight::Unity.gain(), 1.0);
        assert_eq!(ChannelWeight::Surround.gain(), 1.41);
        assert_eq!(ChannelWeight::Excluded.gain(), 0.0);
        assert_eq!(ChannelWeight::Custom(2.0).gain(), 2.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(ChannelWeight::Custom(bad).gain(), 0.0);
        }

        // Table 5 of BS.1770-4, by loudspeaker label: the middle layer at
        // 60, 90 and 110 degrees to either side weighs 1.41, and nothing
        // else does.
        use ChannelWeight::{Surround, Unity};
        for (azimuth, elevation, weight) in [
            (0.0, 0.0, Unity),
            (30.0, 0.0, Unity),
            (-30.0, 0.0, Unity),
            (60.0, 0.0, Surround),
            (-60.0, 0.0, Surround),
            (90.0, 0.0, Surround),
            (-90.0, 0.0, Surround),
            (110.0, 0.0, Surround),
            (-110.0, 0.0, Surround),
            (135.0, 0.0, Unity),
            (-135.0, 0.0, Unity),
            (180.0, 0.0, Unity),
            (90.0, 30.0, Unity),
            (-110.0, 30.0, Unity),
            (45.0, -30.0, Unity),
            (0.0, 90.0, Unity),
            // The same directions written another way round the circle.
            (250.0, 0.0, Surround),
            (-270.0, 10.0, Surround),
            (540.0, 0.0, Unity),
        ] {
            assert_eq!(
                ChannelWeight::at_position(azimuth, elevation),
                weight,
                "{azimuth}, {elevation}"
            );
        }

        assert_eq!(LoudnessMeter::mono().num_channels(), 1);
        assert_eq!(LoudnessMeter::stereo().num_channels(), 2);
        assert_eq!(
            LoudnessMeter::surround_5_0().gains,
            [1.0, 1.0, 1.0, 1.41, 1.41]
        );
        assert_eq!(
            LoudnessMeter::surround_5_1().gains,
            [1.0, 1.0, 1.0, 0.0, 1.41, 1.41]
        );
    }

    #[test]
    fn a_reading_is_minus_infinity_until_there_is_something_to_read() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        let silent = Loudness {
            momentary: f64::NEG_INFINITY,
            short_term: f64::NEG_INFINITY,
            integrated: f64::NEG_INFINITY,
            range: 0.0,
            max_momentary: f64::NEG_INFINITY,
            max_short_term: f64::NEG_INFINITY,
        };
        assert_eq!(meter.readings().snapshot(), silent);
        meter.measure(&AudioBuffer::<f32>::new(2, 48_000));
        assert_eq!(meter.readings().snapshot(), silent);
        assert_eq!(meter.readings().measured_seconds(), 1.0);

        // 390 ms of tone: momentary and short-term show it, diluted by the
        // silence before; no gating block is complete.
        let mut position = 0;
        meter.reset();
        feed(&mut meter, 48_000.0, 1_000.0, -20.0, 0.39, &mut position);
        let readings = meter.readings();
        let expected = -20.0 + AT_1_KHZ + 10.0 * (0.39f64 / 0.4).log10();
        assert!((readings.momentary() - expected).abs() < 0.01);
        let expected = -20.0 + AT_1_KHZ + 10.0 * (0.39f64 / 3.0).log10();
        assert!((readings.short_term() - expected).abs() < 0.01);
        assert_eq!(readings.integrated(), f64::NEG_INFINITY);
        // The next slice completes the first block.
        feed(&mut meter, 48_000.0, 1_000.0, -20.0, 0.01, &mut position);
        assert!((meter.readings().integrated() + 20.0 - AT_1_KHZ).abs() < 0.01);
        assert_eq!(meter.readings().range(), 0.0);
    }

    #[test]
    fn resetting_the_integration_leaves_the_windows_running() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        let readings = meter.readings().clone();
        let mut position = 0;
        feed(&mut meter, 48_000.0, 1_000.0, -10.0, 5.0, &mut position);
        assert!((readings.integrated() + 10.0 - AT_1_KHZ).abs() < 0.01);
        assert!((readings.max_short_term() + 10.0 - AT_1_KHZ).abs() < 0.01);
        assert_eq!(readings.measured_seconds(), 5.0);

        // Asked for from the reader's side; done when the next block comes.
        readings.request_reset();
        assert!(readings.integrated().is_finite());
        feed(&mut meter, 48_000.0, 1_000.0, -30.0, 0.39, &mut position);
        assert_eq!(readings.integrated(), f64::NEG_INFINITY);
        // The short-term window still holds the louder tone.
        assert!(readings.short_term() > -11.0);
        // Not a block yet: the first one must lie wholly after the reset.
        feed(&mut meter, 48_000.0, 1_000.0, -30.0, 0.01, &mut position);
        let first = readings.integrated();
        assert!(first > -30.0 && first < -10.0, "{first}");

        // The other reset clears everything.
        meter.reset();
        assert_eq!(readings.short_term(), f64::NEG_INFINITY);
        assert_eq!(readings.max_momentary(), f64::NEG_INFINITY);
        assert_eq!(readings.measured_seconds(), 0.0);
    }

    #[test]
    fn a_paused_meter_keeps_its_integrated_loudness() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        let readings = meter.readings().clone();
        let mut position = 0;
        feed(&mut meter, 48_000.0, 1_000.0, -23.0, 4.0, &mut position);
        let before = readings.snapshot();

        readings.set_paused(true);
        assert!(readings.is_paused());
        feed(&mut meter, 48_000.0, 1_000.0, -3.0, 4.0, &mut position);
        let paused = readings.snapshot();
        // The windows saw the loud tone; the measurement did not.
        assert!((paused.momentary + 3.0 - AT_1_KHZ).abs() < 0.01);
        assert_eq!(paused.integrated, before.integrated);
        assert_eq!(paused.range, before.range);
        assert_eq!(paused.max_momentary, before.max_momentary);
        assert_eq!(paused.max_short_term, before.max_short_term);
        assert_eq!(readings.measured_seconds(), 4.0);

        // Continuing: no block may contain the loud tone from the pause,
        // so none is taken for the first 390 ms, and the maxima wait too.
        readings.set_paused(false);
        feed(&mut meter, 48_000.0, 1_000.0, -23.0, 0.39, &mut position);
        assert_eq!(readings.integrated(), before.integrated);
        assert_eq!(readings.max_momentary(), before.max_momentary);
        feed(&mut meter, 48_000.0, 1_000.0, -23.0, 3.61, &mut position);
        assert!((readings.integrated() + 23.0 - AT_1_KHZ).abs() < 0.01);
        assert_eq!(readings.measured_seconds(), 8.0);
        // A full reset does not un-pause.
        readings.set_paused(true);
        meter.reset();
        assert!(readings.is_paused());
    }

    #[test]
    fn broken_input_does_not_break_the_meter() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        let mut garbage = AudioBuffer::<f32>::new(2, 4_800);
        for (index, sample) in garbage.channel_mut(0).iter_mut().enumerate() {
            *sample = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX][index % 4];
        }
        garbage.channel_mut(1).fill(f32::MIN);
        meter.measure(&garbage);
        let during = meter.readings().momentary();
        assert!(during.is_finite(), "{during}");

        // Four seconds later the windows have forgotten it, and a reset of
        // the integration clears the rest.
        let mut position = 0;
        feed(&mut meter, 48_000.0, 1_000.0, -23.0, 4.0, &mut position);
        meter.reset_integration();
        feed(&mut meter, 48_000.0, 1_000.0, -23.0, 4.0, &mut position);
        let loudness = meter.readings().snapshot();
        assert!((loudness.momentary + 23.0 - AT_1_KHZ).abs() < 0.01);
        assert!((loudness.short_term + 23.0 - AT_1_KHZ).abs() < 0.01);
        assert!((loudness.integrated + 23.0 - AT_1_KHZ).abs() < 0.01);
    }

    #[test]
    fn blocks_of_any_shape_are_accepted() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        // No frames, no channels, too few and too many channels.
        meter.measure(&AudioBuffer::<f32>::new(2, 0));
        meter.measure(&AudioBuffer::<f32>::new(0, 64));
        meter.measure(&AudioBuffer::<f64>::new(1, 64));
        meter.measure(&AudioBuffer::<f64>::new(8, 64));
        assert_eq!(meter.readings().momentary(), f64::NEG_INFINITY);

        // No channels at all, and rates no sound card has.
        let mut empty = LoudnessMeter::new(&[]);
        for rate in [0.0, 1.0, 99.0, 11_025.0, 1e7, f64::NAN] {
            empty.prepare(&spec(rate, 0));
            empty.measure(&AudioBuffer::<f32>::new(2, 1_000));
            meter.prepare(&spec(rate, 2));
            meter.measure(&AudioBuffer::<f32>::new(2, 1_000));
            assert_eq!(meter.readings().integrated(), f64::NEG_INFINITY);
        }
    }

    #[test]
    fn windows_keep_their_length_at_a_rate_that_does_not_divide_by_100() {
        // 22.05 kHz: a slice is 220.5 samples, so slices alternate between
        // 220 and 221, and 40 of them are 8,820 samples, 400 ms exactly.
        let mut meter = LoudnessMeter::mono();
        meter.prepare(&spec(22_050.0, 1));
        let lengths: Vec<usize> = (0..6).map(|slice| meter.slice_frames(slice)).collect();
        assert_eq!(lengths.iter().sum::<usize>(), 1_323);
        assert!(lengths.iter().all(|&length| length == 220 || length == 221));
        assert_eq!(meter.boundary(1_000.0) - meter.boundary(960.0), 8_820.0);
        assert_eq!(meter.boundary(1_000.0) - meter.boundary(700.0), 66_150.0);
        // Before the start, the windows reach back into silence of the
        // same length.
        assert_eq!(meter.boundary(0.0) - meter.boundary(-40.0), 8_820.0);

        // And the reading shows it: a tone that lasts exactly 400 ms fills
        // the momentary window completely, whichever way the slices fall.
        // Were the window 20 samples longer, as 40 slices of 221 would be,
        // the reading would be 0.01 LU lower.
        let mut position = 0;
        feed(&mut meter, 22_050.0, 1_000.0, -20.0, 0.4, &mut position);
        let filled = meter.readings().momentary();
        let weighting = KWeighting::for_sample_rate(22_050.0).magnitude_at(1_000.0, 22_050.0);
        let expected = lufs(0.5 * (0.1 * weighting).powi(2));
        assert!((filled - expected).abs() < 0.002, "{filled} vs {expected}");
    }

    #[test]
    fn the_windows_slide_in_steps_of_ten_milliseconds() {
        // The highest momentary loudness of a 400 ms burst that begins
        // `offset` seconds in.
        let highest = |offset: f64| {
            let mut meter = LoudnessMeter::stereo();
            meter.prepare(&spec(48_000.0, 2));
            let mut position = 0;
            let silence = f64::NEG_INFINITY;
            feed(
                &mut meter,
                48_000.0,
                1_000.0,
                silence,
                offset,
                &mut position,
            );
            feed(&mut meter, 48_000.0, 1_000.0, -23.0, 0.4, &mut position);
            feed(&mut meter, 48_000.0, 1_000.0, silence, 0.5, &mut position);
            meter.readings().max_momentary()
        };
        // On the grid, one window holds the whole burst.
        let on_grid = highest(0.03);
        assert!((on_grid + 23.0 - AT_1_KHZ).abs() < 0.002, "{on_grid}");
        assert!((highest(0.0) - on_grid).abs() < 0.002);
        assert!((highest(0.17) - on_grid).abs() < 0.002);
        // Half a slice off it, the best window holds 395 ms of the burst:
        // 10 log10(395 / 400) = -0.055 LU. This is the largest error the
        // slices can cause, and what the documentation quotes.
        let off_grid = highest(0.035);
        assert!((off_grid - on_grid + 0.0546).abs() < 0.002, "{off_grid}");
        // A window that only moved every 100 ms would lose 50 ms of it,
        // 0.58 LU, and fail Tech 3341.
        assert!(on_grid - off_grid < 0.06);
    }

    #[test]
    fn a_reset_in_the_middle_of_a_slice_keeps_what_came_before_out() {
        let mut meter = LoudnessMeter::stereo();
        meter.prepare(&spec(48_000.0, 2));
        let mut position = 0;
        // A loud tone, and a reset that falls 5 ms into a slice.
        feed(&mut meter, 48_000.0, 1_000.0, -10.0, 5.005, &mut position);
        meter.reset_integration();
        // A quiet tone from there. The slice under way holds 5 ms of the
        // loud one, so it is not counted: the first block begins with the
        // next slice, at 5.01 s, and is complete at 5.41 s.
        feed(&mut meter, 48_000.0, 1_000.0, -50.0, 0.4, &mut position);
        assert_eq!(meter.readings().integrated(), f64::NEG_INFINITY);
        assert_eq!(meter.readings().max_momentary(), f64::NEG_INFINITY);
        feed(&mut meter, 48_000.0, 1_000.0, -50.0, 0.005, &mut position);
        let first = meter.readings().integrated();
        // With those 5 ms in the block it would read about -29 LUFS.
        assert!((first + 50.0 - AT_1_KHZ).abs() < 0.2, "{first}");
        assert_eq!(meter.readings().max_momentary(), first);
        assert_eq!(meter.readings().measured_seconds(), 0.4);

        // The same when the measurement continues after a pause.
        meter.readings().set_paused(true);
        feed(&mut meter, 48_000.0, 1_000.0, -10.0, 1.003, &mut position);
        meter.readings().set_paused(false);
        feed(&mut meter, 48_000.0, 1_000.0, -50.0, 4.0, &mut position);
        let loudness = meter.readings().snapshot();
        assert!((loudness.integrated + 50.0 - AT_1_KHZ).abs() < 0.2);
        assert!((loudness.max_momentary + 50.0 - AT_1_KHZ).abs() < 0.2);
        assert!((loudness.max_short_term + 50.0 - AT_1_KHZ).abs() < 0.2);
    }
}
