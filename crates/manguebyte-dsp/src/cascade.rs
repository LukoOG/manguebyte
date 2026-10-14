//! Second-order sections run one after another, and the processor that
//! runs a designed IIR filter.

use core::marker::PhantomData;

use manguebyte_core::{AudioBuffer, Sample, Smoothed};

use crate::iir::{self, Design, Sections};
use crate::numeric::limit;
use crate::{BiquadCoefficients, ProcessSpec, Processor};

/// A section in state-variable form: `[g, k, high, band, low]`, the
/// integrator gain, the damping, and the weights of the high-pass,
/// band-pass and low-pass outputs. With `s = (z - 1) / (z + 1)` and
/// `s = g·σ`, the section is `(high·σ² + band·σ + low) / (σ² + k·σ + 1)`.
pub(crate) type Row = [f64; 5];

/// A section that passes its input through: `(σ + 1)² / (σ + 1)²`.
pub(crate) const IDENTITY: Row = [1.0, 2.0, 1.0, 2.0, 1.0];

/// Smallest value the outer coefficients of a section's analogue
/// denominator are given, so that a section that is not stable still gives
/// numbers. No stable section in the range of [`iir`] comes near it.
const FLOOR: f64 = 1e-30;

/// The state-variable form of a section.
///
/// Mapped back through the bilinear transform, with `1/z = (1 - s) / (1 +
/// s)`, the section's denominator is `A·s² + B·s + C` with `A = 1 - a1 +
/// a2`, `B = 2·(1 - a2)` and `C = 1 + a1 + a2`, all three positive for a
/// stable section, and its numerator is `(b0 - b1 + b2)·s² + 2·(b0 - b2)·s +
/// (b0 + b1 + b2)`. With `s = g·σ`, `g = sqrt(C / A)` and `k = B / (A·g)`
/// the section becomes `(high·σ² + band·σ + low) / (σ² + k·σ + 1)`: the
/// three outputs of a state-variable filter, weighted. The sums are taken
/// in the order that keeps them exact near 0 Hz and half the sample rate,
/// as in [`Sections::magnitude_at`](iir::Sections::magnitude_at).
///
/// A section that is not stable cannot be put in this form as it is: its
/// damping is limited to zero, which puts its poles on the unit circle. A
/// section with a coefficient that is not a finite number, or one so far
/// out that its state-variable form is not made of finite numbers with a
/// positive gain (a pole at 1e300, whose gain `g` overflows), is replaced
/// by one that passes its input through.
fn state_variable(section: &BiquadCoefficients) -> Row {
    let c = section;
    if ![c.b0, c.b1, c.b2, c.a1, c.a2].iter().all(|x| x.is_finite()) {
        return IDENTITY;
    }
    let outer_a = ((1.0 - c.a1) + c.a2).max(FLOOR);
    let outer_c = ((1.0 + c.a1) + c.a2).max(FLOOR);
    let middle = (2.0 * (1.0 - c.a2)).max(0.0);
    let g = (outer_c / outer_a).sqrt();
    let row = [
        g,
        middle / (outer_a * g),
        ((c.b0 - c.b1) + c.b2) / outer_a,
        2.0 * (c.b0 - c.b2) / (outer_a * g),
        ((c.b0 + c.b1) + c.b2) / outer_c,
    ];
    if usable(&row) { row } else { IDENTITY }
}

/// Whether a section in state-variable form can be run: every number
/// finite, the gain above zero, the damping not below it, and what one
/// step needs of them finite too.
fn usable(row: &Row) -> bool {
    row.iter().all(|x| x.is_finite())
        && row[0] > 0.0
        && row[1] >= 0.0
        && ready(row).iter().all(|x| x.is_finite())
}

/// What one step of a section needs: the three gains of Andrew Simper's
/// solution of the loop, `k`, and the three output weights.
type Ready = [f64; 7];

fn ready(row: &Row) -> Ready {
    let [g, k, high, band, low] = *row;
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    [a1, a2, g * a2, k, high, band, low]
}

/// One sample through one section.
///
/// The trapezoidal state-variable filter of Andrew Simper, "Linear
/// Trapezoidal Integrated SVF" (Cytomic, 2013), in the topology-preserving
/// form of Vadim Zavalishin, "The Art of VA Filter Design", chapter 4.
#[inline]
fn tick(memory: &mut [f64; 2], x: f64, c: &Ready) -> f64 {
    let v3 = x - memory[1];
    let v1 = c[0] * memory[0] + c[1] * v3;
    let v2 = memory[1] + c[1] * memory[0] + c[2] * v3;
    memory[0] = 2.0 * v1 - memory[0];
    memory[1] = 2.0 * v2 - memory[1];
    c[4] * (x - c[3] * v1 - v2) + c[5] * v1 + c[6] * v2
}

/// Where a glide from `start` is after the fraction `t` of it: the gain
/// `g` moves by a constant ratio per sample (its `span` is the logarithm
/// of the whole ratio), so that a section's frequency moves by equal
/// intervals in equal times; the other numbers move in straight lines.
#[inline]
fn lerp(start: &Row, span: &Row, t: f64) -> Row {
    let mut out = [0.0; 5];
    out[0] = start[0] * (t * span[0]).exp();
    for i in 1..5 {
        out[i] = start[i] + t * span[i];
    }
    out
}

/// What [`lerp`] needs to go from `start` to `end`.
fn span(start: &Row, end: &Row) -> Row {
    let mut out = [0.0; 5];
    out[0] = (end[0] / start[0]).ln();
    for i in 1..5 {
        out[i] = end[i] - start[i];
    }
    out
}

/// Second-order sections run one after another, in double precision.
///
/// This is how a high-order IIR filter is run: as a chain of
/// [`BiquadCoefficients`]. Up to [`MAX_SECTIONS`](Self::MAX_SECTIONS)
/// sections fit, which is enough for any design from [`iir`](crate::iir);
/// [`IirFilter`] is the processor that designs them for you.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::iir::{Band, Design};
/// use manguebyte_dsp::{BiquadCascade, ProcessSpec, Processor};
///
/// // A sixth-order Butterworth high-pass at 80 Hz, as three sections.
/// let sections = Design::butterworth(Band::Highpass { cutoff_hz: 80.0 }, 6).sections(48_000.0);
/// let mut filter = BiquadCascade::<f32>::new(&sections);
/// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
///
/// // A steady level is removed entirely.
/// let mut block = AudioBuffer::<f32>::new(1, 4_800);
/// block.channel_mut(0).fill(0.5);
/// filter.process(&mut block);
/// assert!(block.channel(0)[4_799].abs() < 1e-4);
/// ```
///
/// # How the sections are run
///
/// Not in a direct form, but each as a state-variable filter in the
/// topology-preserving form (Zavalishin, "The Art of VA Filter Design",
/// chapter 4), into which a section's coefficients convert exactly through
/// the bilinear transform. Its response is the same; what differs is its
/// memory. That memory is the charge of the two capacitors of an analogue
/// filter, and without input, one step carries it on by the Cayley
/// transform of `g·[[-k, -1], [1, 0]]`, which cannot lengthen it for any
/// gain `g` and damping `k` of zero or more. So whatever the coefficients
/// do from one sample to the next, the energy held in a section can only
/// fall while nothing comes into it, and a change of sections cannot make
/// the memory grow by itself. Coefficients moved through a direct form do
/// not have that property: in a test that switches every 100 ms between
/// very resonant designs, gliding the same way, noise of peak 1 came out of
/// a direct-form cascade at more than a million; out of this one, below
/// 1.5.
///
/// Everything is kept in double precision whatever the sample type. Single
/// precision is not enough for steep filters at low frequencies: rounded to
/// `f32`, the coefficients of a 16th-order Chebyshev type I low-pass at
/// 1 Hz and 192 kHz put a pole outside the unit circle, and those of a
/// 16th-order Butterworth low-pass at 10 Hz change its response by more
/// than 10 dB. See [the design module](crate::iir#precision) for where
/// double precision in turn stops being enough.
///
/// Made from the [`Sections`] of a design with
/// [`from_sections`](Self::from_sections) or
/// [`set_from_sections`](Self::set_from_sections), the cascade takes each
/// section in state-variable form as worked out from the design itself.
/// Made from [`BiquadCoefficients`] with [`new`](Self::new) or
/// [`set_sections`](Self::set_sections), it converts them, and can then be
/// no more precise than they are, which for a pole close to 0 Hz or half
/// the sample rate is much less (the same module says how much).
///
/// # Changing the sections
///
/// [`set_sections`](Self::set_sections) and
/// [`set_from_sections`](Self::set_from_sections) glide every section from where it
/// is to where it is going, over 20 ms by default
/// ([`with_glide`](Self::with_glide) changes that): its gain `g`, which is
/// the pre-warped frequency `tan(π·f/fs)` of its poles, by the same ratio
/// on every sample, so that the frequency moves by nearly equal intervals in
/// equal times, and its damping and output weights in straight lines. The
/// damping stays positive on the way, so every section stays stable. A
/// section scaled to unity gain at 0 Hz at both ends, as every low-pass and
/// band-stop section of a design is, has unity gain at 0 Hz all the way, and
/// one scaled at half the sample rate keeps it there. Sections that come or
/// go glide from or to a section that passes the signal through.
///
/// # Limits
///
/// A glide moves the sections, not the response as a whole: half-way
/// between two band-pass designs is not the band-pass half-way between
/// them, and a change between very different designs makes a transient. In
/// the tests, a 200 Hz tone through an eighth-order band-pass whose band
/// jumps from 150-250 Hz to 5-6 kHz moves by at most 3.2 times its own
/// largest step while the filter glides, against 22 times when the
/// sections are switched at once. For a filter that moves all the time, a
/// [`StateVariableFilter`](crate::StateVariableFilter) or a
/// [`LadderFilter`](crate::LadderFilter) is cheaper. Sections beyond
/// [`MAX_SECTIONS`](Self::MAX_SECTIONS) are ignored. Channels the cascade
/// was not prepared for are left as they are.
#[derive(Debug, Clone)]
pub struct BiquadCascade<S: Sample = f32> {
    /// The sections asked for, padded with sections that pass through.
    target: [BiquadCoefficients; iir::MAX_SECTIONS],
    /// Their state-variable form, and what a step needs of it.
    rows: [Row; iir::MAX_SECTIONS],
    ready: [Ready; iir::MAX_SECTIONS],
    /// Where the glide started, and how far each number moves over it.
    start: [Row; iir::MAX_SECTIONS],
    span: [Row; iir::MAX_SECTIONS],
    /// How many sections were asked for.
    len: usize,
    /// How many sections run: during a glide, the larger of the old and
    /// the new count. From here on every section holds no memory.
    active: usize,
    /// How far the glide has come, from 0 to 1.
    position: Smoothed<f64>,
    glide_seconds: f64,
    /// The memory of each section, per channel.
    state: Vec<[[f64; 2]; iir::MAX_SECTIONS]>,
    marker: PhantomData<S>,
}

impl<S: Sample> BiquadCascade<S> {
    /// Most sections a cascade runs.
    pub const MAX_SECTIONS: usize = iir::MAX_SECTIONS;

    /// How long a change of sections glides, unless set otherwise.
    pub const DEFAULT_GLIDE_SECONDS: f64 = 0.02;

    /// Creates a cascade of `sections`, run in the order given. Until
    /// [`prepare`](Processor::prepare) is called it has no memory and
    /// passes audio through.
    pub fn new(sections: &[BiquadCoefficients]) -> Self {
        let mut cascade = Self {
            target: [BiquadCoefficients::IDENTITY; iir::MAX_SECTIONS],
            rows: [IDENTITY; iir::MAX_SECTIONS],
            ready: [ready(&IDENTITY); iir::MAX_SECTIONS],
            start: [IDENTITY; iir::MAX_SECTIONS],
            span: [[0.0; 5]; iir::MAX_SECTIONS],
            len: 0,
            active: 0,
            position: Smoothed::new(1.0),
            glide_seconds: Self::DEFAULT_GLIDE_SECONDS,
            state: Vec::new(),
            marker: PhantomData,
        };
        cascade.set_sections(sections);
        cascade
    }

    /// Sets how long [`set_sections`](Self::set_sections) takes to arrive,
    /// in seconds, limited to `0.0..=1.0`. Zero switches at once. Takes
    /// effect at the next [`prepare`](Processor::prepare).
    #[must_use]
    pub fn with_glide(mut self, seconds: f64) -> Self {
        self.glide_seconds = limit(seconds, 0.0, 1.0);
        self
    }

    /// Creates a cascade of the sections of a [`Design`], in the form it
    /// runs them, worked out from the design itself rather than from its
    /// coefficients. Until [`prepare`](Processor::prepare) is called it has
    /// no memory and passes audio through.
    ///
    /// Prefer this to [`new`](Self::new) for a design: the
    /// [`BiquadCoefficients`] a design is rounded to cannot carry a pole very
    /// close to 0 Hz, to half the sample rate or to the unit circle (see
    /// [the design module](crate::iir#precision)), and this way the cascade
    /// does not depend on them.
    ///
    /// ```
    /// use manguebyte_core::AudioBuffer;
    /// use manguebyte_dsp::iir::{Band, Design};
    /// use manguebyte_dsp::{BiquadCascade, ProcessSpec, Processor};
    ///
    /// let sections = Design::butterworth(Band::Lowpass { cutoff_hz: 2_000.0 }, 4).sections(48_000.0);
    /// let mut filter = BiquadCascade::<f64>::from_sections(&sections);
    /// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 4_800, num_channels: 1 });
    ///
    /// // A steady level comes through a low-pass at exactly its own level.
    /// let mut block = AudioBuffer::<f64>::new(1, 4_800);
    /// block.channel_mut(0).fill(0.5);
    /// filter.process(&mut block);
    /// assert!((block.channel(0)[4_799] - 0.5).abs() < 1e-12);
    /// assert_eq!(filter.sections(), sections.as_slice());
    /// ```
    pub fn from_sections(sections: &Sections) -> Self {
        let mut cascade = Self::new(&[]);
        cascade.set_from_sections(sections);
        cascade
    }

    /// Replaces the sections with those of a [`Design`], in the form the
    /// cascade runs them, gliding from the old ones to the new ones (see
    /// [`from_sections`](Self::from_sections)). Real-time safe. Asking for
    /// the sections already in use, or already being glided to, changes
    /// nothing.
    pub fn set_from_sections(&mut self, sections: &Sections) {
        self.load(sections.as_slice(), sections.rows());
    }

    /// Replaces the sections, gliding from the old ones to the new ones.
    /// Real-time safe. Asking for the sections already in use, or already
    /// being glided to, changes nothing.
    ///
    /// For the sections of a [`Design`],
    /// [`set_from_sections`](Self::set_from_sections) is more precise.
    pub fn set_sections(&mut self, sections: &[BiquadCoefficients]) {
        let sections = &sections[..sections.len().min(iir::MAX_SECTIONS)];
        let mut rows = [IDENTITY; iir::MAX_SECTIONS];
        for (row, section) in rows.iter_mut().zip(sections) {
            *row = state_variable(section);
        }
        self.load(sections, &rows[..sections.len()]);
    }

    /// Glides to `sections`, run in the state-variable form `rows`.
    fn load(&mut self, sections: &[BiquadCoefficients], rows: &[Row]) {
        let len = sections.len().min(rows.len()).min(iir::MAX_SECTIONS);
        let (sections, rows) = (&sections[..len], &rows[..len]);
        if sections == &self.target[..self.len] && rows == &self.rows[..self.len] {
            return;
        }
        // Start from wherever the sections are now.
        let gliding = self.position.is_smoothing();
        let t = self.position.current();
        for k in 0..iir::MAX_SECTIONS {
            self.start[k] = if k >= self.active {
                IDENTITY
            } else if gliding {
                lerp(&self.start[k], &self.span[k], t)
            } else {
                self.rows[k]
            };
        }
        self.target = [BiquadCoefficients::IDENTITY; iir::MAX_SECTIONS];
        self.target[..len].copy_from_slice(sections);
        self.rows = [IDENTITY; iir::MAX_SECTIONS];
        for (to, row) in self.rows.iter_mut().zip(rows) {
            *to = if usable(row) { *row } else { IDENTITY };
        }
        for k in 0..iir::MAX_SECTIONS {
            self.ready[k] = ready(&self.rows[k]);
            self.span[k] = span(&self.start[k], &self.rows[k]);
        }
        self.len = len;
        self.active = self.active.max(self.len);
        self.position.set_immediate(0.0);
        self.position.set_target(1.0);
        if !self.position.is_smoothing() {
            self.arrive();
        }
    }

    /// The sections asked for, in the order they run.
    pub fn sections(&self) -> &[BiquadCoefficients] {
        &self.target[..self.len]
    }

    /// Whether the sections are still on their way to the ones asked for.
    pub fn is_gliding(&self) -> bool {
        self.position.is_smoothing()
    }

    /// Lets go of the sections that went away, on every channel.
    fn arrive(&mut self) {
        for memory in &mut self.state {
            memory[self.len..self.active].fill([0.0; 2]);
        }
        self.active = self.len;
    }
}

impl<S: Sample> Processor<S> for BiquadCascade<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.position.set_ramp(spec.sample_rate, self.glide_seconds);
        self.position.set_immediate(1.0);
        self.state = vec![[[0.0; 2]; iir::MAX_SECTIONS]; spec.num_channels];
        self.active = self.len;
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        let len = self.len;
        let mut after = self.position;
        let mut ran = false;
        for (samples, memory) in buffer.iter_channels_mut().zip(&mut self.state) {
            // Every channel runs a copy of the glide, so all of them see the
            // same coefficients and let go of sections on the same sample.
            let mut position = self.position;
            let mut active = self.active;
            for sample in samples {
                let mut x = sample.to_f64();
                if position.is_smoothing() {
                    let t = position.next_value();
                    if position.is_smoothing() {
                        for (k, pair) in memory[..active].iter_mut().enumerate() {
                            let row = lerp(&self.start[k], &self.span[k], t);
                            x = tick(pair, x, &ready(&row));
                        }
                    } else {
                        // Arrived: the sections that went away pass the
                        // signal through by now, and are let go.
                        memory[len..active].fill([0.0; 2]);
                        active = len;
                        for (pair, c) in memory[..active].iter_mut().zip(&self.ready) {
                            x = tick(pair, x, c);
                        }
                    }
                } else {
                    for (pair, c) in memory[..active].iter_mut().zip(&self.ready) {
                        x = tick(pair, x, c);
                    }
                }
                *sample = S::from_f64(x);
            }
            after = position;
            ran = true;
        }
        if !ran {
            // No channel to process, but time still passes.
            for _ in 0..buffer.num_frames() {
                after.next_value();
            }
        }
        self.position = after;
        if !self.position.is_smoothing() && self.active != len {
            // The glide ended in this block. The channels it carried let go
            // of the sections that went away on that sample; the others,
            // which it did not carry, let go of them now.
            self.arrive();
        }
    }

    fn reset(&mut self) {
        self.state.fill([[0.0; 2]; iir::MAX_SECTIONS]);
        self.position.set_immediate(1.0);
        self.active = self.len;
    }
}

/// A classic IIR filter as a processor: a [`Design`] from
/// [`iir`](crate::iir), run as a [`BiquadCascade`] made from its sections
/// with [`BiquadCascade::from_sections`], so that it follows the design
/// rather than the coefficients the design rounds to.
///
/// The design is made for the sample rate given to
/// [`prepare`](Processor::prepare), and again whenever
/// [`set_design`](Self::set_design) is called, which is real-time safe: a
/// design allocates nothing, and the cascade glides from the old sections to
/// the new ones over 20 ms, so a change once per block does not click.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::iir::{Band, Design};
/// use manguebyte_dsp::{IirFilter, ProcessSpec, Processor};
///
/// // A fourth-order Chebyshev type I band-pass with 0.5 dB of ripple.
/// let band = Band::Bandpass { low_hz: 300.0, high_hz: 3_000.0 };
/// let mut filter = IirFilter::<f32>::new(Design::chebyshev_i(band, 4, 0.5));
/// filter.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 9_600, num_channels: 1 });
///
/// // A tone in the band comes through at between 0 and -0.5 dB.
/// let mut block = AudioBuffer::<f32>::new(1, 9_600);
/// for (n, sample) in block.channel_mut(0).iter_mut().enumerate() {
///     *sample = (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
/// }
/// filter.process(&mut block);
/// let peak = block.channel(0)[4_800..].iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
/// assert!(peak <= 1.0 + 1e-6 && peak >= 0.94);
/// ```
#[derive(Debug, Clone)]
pub struct IirFilter<S: Sample = f32> {
    design: Design,
    sample_rate: f64,
    cascade: BiquadCascade<S>,
}

impl<S: Sample> IirFilter<S> {
    /// Creates a filter. Until [`prepare`](Processor::prepare) is called it
    /// assumes 44.1 kHz.
    pub fn new(design: Design) -> Self {
        let sample_rate = 44_100.0;
        Self {
            design,
            sample_rate,
            cascade: BiquadCascade::from_sections(&design.sections(sample_rate)),
        }
    }

    /// Sets how long a change of design takes to arrive, in seconds,
    /// limited to `0.0..=1.0`. Zero switches at once. Takes effect at the
    /// next [`prepare`](Processor::prepare).
    #[must_use]
    pub fn with_glide(mut self, seconds: f64) -> Self {
        self.cascade = self.cascade.with_glide(seconds);
        self
    }

    /// Changes the design. Real-time safe: the new sections are computed
    /// without allocating, and the cascade glides to them. Setting the
    /// design already in use changes nothing.
    pub fn set_design(&mut self, design: Design) {
        self.design = design;
        self.cascade
            .set_from_sections(&design.sections(self.sample_rate));
    }

    /// The design, as last set.
    pub fn design(&self) -> Design {
        self.design
    }

    /// The sections of the design at the current sample rate.
    pub fn sections(&self) -> &[BiquadCoefficients] {
        self.cascade.sections()
    }
}

impl<S: Sample> Processor<S> for IirFilter<S> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.sample_rate = spec.sample_rate;
        self.cascade
            .set_from_sections(&self.design.sections(self.sample_rate));
        self.cascade.prepare(spec);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.cascade.process(buffer);
    }

    fn reset(&mut self) {
        self.cascade.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iir::Band;
    use crate::numeric::{strict_buffer_peak, strict_peak};
    use crate::testing::{Random, gain_at, largest_jump, noise, run_in_blocks, sine, tail_peak};

    const RATE: f64 = 48_000.0;

    fn spec(sample_rate: f64, channels: usize) -> ProcessSpec {
        ProcessSpec {
            sample_rate,
            max_block_size: 512,
            num_channels: channels,
        }
    }

    fn lowpass(cutoff_hz: f64, order: usize) -> Design {
        Design::butterworth(Band::Lowpass { cutoff_hz }, order)
    }

    /// The impulse response, checked to have died away within `len`.
    fn impulse_response<P: Processor<f64>>(filter: &mut P, len: usize) -> Vec<f64> {
        let mut impulse = AudioBuffer::<f64>::new(1, len);
        impulse.channel_mut(0)[0] = 1.0;
        run_in_blocks(filter, &mut impulse, 512);
        let response = impulse.channel(0).to_vec();
        assert!(tail_peak(&response, 256) < 1e-14, "the response is cut off");
        response
    }

    #[test]
    fn the_filter_realises_the_response_of_its_design() {
        let designs = [
            (lowpass(1_000.0, 8), 48_000.0, 1 << 13),
            (
                Design::chebyshev_i(Band::Highpass { cutoff_hz: 200.0 }, 7, 1.0),
                44_100.0,
                1 << 16,
            ),
            (
                Design::chebyshev_ii(
                    Band::Bandpass {
                        low_hz: 500.0,
                        high_hz: 2_000.0,
                    },
                    5,
                    60.0,
                ),
                96_000.0,
                1 << 16,
            ),
            (
                Design::butterworth(
                    Band::Bandstop {
                        low_hz: 900.0,
                        high_hz: 1_100.0,
                    },
                    4,
                ),
                48_000.0,
                1 << 15,
            ),
            (
                Design::chebyshev_i(
                    Band::Lowpass {
                        cutoff_hz: 15_000.0,
                    },
                    16,
                    0.1,
                ),
                192_000.0,
                1 << 16,
            ),
        ];
        for (design, rate, len) in designs {
            let mut filter = IirFilter::<f64>::new(design);
            filter.prepare(&spec(rate, 1));
            let response = impulse_response(&mut filter, len);
            let sections = design.sections(rate);
            assert_eq!(filter.sections(), sections.as_slice());
            // Against the formula itself, not the coefficients.
            let mut worst = 0.0f64;
            for i in 0..=200 {
                let frequency = 0.499 * rate * i as f64 / 200.0;
                let expected = design.magnitude_at(frequency, rate);
                let measured = gain_at(&response, frequency, rate);
                let error = (measured - expected).abs() / expected.max(1e-3);
                worst = worst.max(error);
            }
            assert!(worst < 1e-10, "{design:?}: {worst}");
        }
    }

    #[test]
    fn without_input_the_memory_of_a_section_never_gains_energy() {
        // One section whose coefficients jump at random on every sample
        // (once at once, once gliding), among stable designs of every
        // kind: rung by a burst of noise, then left without input.
        let mut random = Random::new(17);
        let kinds = |random: &mut Random| {
            let hz = random.log_between(5.0, 0.49 * RATE);
            let band = match (4.0 * random.unit()) as usize {
                0 => Band::Lowpass { cutoff_hz: hz },
                1 => Band::Highpass { cutoff_hz: hz },
                2 => Band::Bandpass {
                    low_hz: hz,
                    high_hz: hz * 1.01,
                },
                _ => Band::Bandstop {
                    low_hz: hz,
                    high_hz: hz * 1.5,
                },
            };
            Design::chebyshev_i(band, 2, random.between(0.01, 6.0)).sections(RATE)[0]
        };
        let ringing = Band::Bandpass {
            low_hz: 1_000.0,
            high_hz: 1_010.0,
        };
        let ringing = Design::chebyshev_i(ringing, 1, 1.0).sections(RATE)[0];
        for glide in [0.0, 0.002] {
            let mut cascade = BiquadCascade::<f64>::new(&[ringing]).with_glide(glide);
            cascade.prepare(&spec(RATE, 1));
            let mut one = AudioBuffer::<f64>::new(1, 1);
            for _ in 0..200 {
                one.channel_mut(0)[0] = random.between(-1.0, 1.0);
                cascade.process(&mut one);
            }
            let energy = |cascade: &BiquadCascade<f64>| {
                let [a, b] = cascade.state[0][0];
                a * a + b * b
            };
            let mut before = energy(&cascade);
            assert!(before > 1e-3, "nothing rings");
            let mut loud = 0;
            for _ in 0..20_000 {
                cascade.set_sections(&[kinds(&mut random)]);
                one.channel_mut(0)[0] = 0.0;
                cascade.process(&mut one);
                let now = energy(&cascade);
                // Room for rounding: of the last digits, and of the steps
                // between subnormal numbers once it has all but died away.
                assert!(
                    now <= before * (1.0 + 1e-12) + 1e-300,
                    "{before} grew to {now}"
                );
                before = now;
                if now > 1e-6 {
                    loud += 1;
                }
            }
            assert!(loud > 100, "it died away after {loud} samples");
        }
    }

    #[test]
    fn it_runs_in_double_precision_whatever_the_sample_type() {
        // The same input through a single- and a double-precision filter:
        // the single-precision output is the double-precision output
        // rounded, sample for sample, because everything inside is the
        // same double-precision arithmetic.
        let design = Design::chebyshev_i(Band::Lowpass { cutoff_hz: 1_000.0 }, 16, 0.5);
        let mut single = IirFilter::<f32>::new(design);
        let mut double = IirFilter::<f64>::new(design);
        single.prepare(&spec(192_000.0, 2));
        double.prepare(&spec(192_000.0, 2));
        let input = noise::<f32>(2, 20_000, 5);
        let mut wide = AudioBuffer::<f64>::new(2, 20_000);
        for (to, from) in wide.iter_channels_mut().zip(input.iter_channels()) {
            for (to, from) in to.iter_mut().zip(from) {
                *to = f64::from(*from);
            }
        }
        let mut narrow = input.clone();
        run_in_blocks(&mut single, &mut narrow, 333);
        run_in_blocks(&mut double, &mut wide, 512);
        for (narrow, wide) in narrow.iter_channels().zip(wide.iter_channels()) {
            for (a, b) in narrow.iter().zip(wide) {
                assert_eq!(*a, *b as f32);
            }
        }
        assert!(strict_buffer_peak(&narrow) > 1e-3);
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        type Change = fn(&mut IirFilter<f64>);
        let render = |block: usize| {
            let mut filter = IirFilter::<f64>::new(lowpass(300.0, 8));
            filter.prepare(&spec(RATE, 2));
            let mut signal = noise::<f64>(2, 8_000, 9);
            // The same changes on the same frames, whatever the block size:
            // fewer sections, more sections, other kinds of band, one in
            // the middle of a glide, and the same design again.
            let changes: [(usize, Change); 6] = [
                (700, |filter| filter.set_design(lowpass(4_000.0, 3))),
                (1_000, |filter| {
                    let band = Band::Bandpass {
                        low_hz: 200.0,
                        high_hz: 2_000.0,
                    };
                    filter.set_design(Design::chebyshev_ii(band, 16, 70.0));
                }),
                (1_500, |filter| {
                    let band = Band::Bandstop {
                        low_hz: 900.0,
                        high_hz: 1_200.0,
                    };
                    filter.set_design(Design::chebyshev_i(band, 2, 1.0));
                }),
                (4_000, |filter| filter.set_design(lowpass(4_000.0, 3))),
                (5_000, |filter| filter.set_design(lowpass(4_000.0, 3))),
                (8_000, |_| {}),
            ];
            let mut start = 0;
            for (end, change) in changes {
                signal.windowed(start..end, |part| run_in_blocks(&mut filter, part, block));
                change(&mut filter);
                start = end;
            }
            signal
        };
        let reference = render(512);
        assert!(strict_buffer_peak(&reference) > 0.1);
        for block in [1, 7, 64, 333, 8_000] {
            assert!(render(block) == reference, "blocks of {block}");
        }
    }

    #[test]
    fn a_glide_moves_by_equal_intervals_and_arrives_on_the_stated_sample() {
        // One low-pass section gliding from 100 Hz to 1600 Hz: 20 ms at
        // 48 kHz is 960 samples, and half-way it is at 400 Hz.
        let at = |hz: f64| lowpass(hz, 2).sections(RATE)[0];
        let mut cascade = BiquadCascade::<f64>::new(&[at(100.0)]);
        cascade.prepare(&spec(RATE, 1));
        cascade.set_sections(&[at(1_600.0)]);
        let mut one = AudioBuffer::<f64>::new(1, 1);
        let mut gains = Vec::new();
        while cascade.is_gliding() {
            cascade.process(&mut one);
            let t = cascade.position.current();
            gains.push(lerp(&cascade.start[0], &cascade.span[0], t)[0]);
        }
        assert_eq!(gains.len(), 960);
        // Half-way, the gain is the geometric mean of the two ends: the
        // pre-warped frequency of 400 Hz, to within the 0.2 % by which
        // tan(π·f/fs) is not quite proportional to f.
        let g = |hz: f64| state_variable(&at(hz))[0];
        let middle = (g(100.0) * g(1_600.0)).sqrt();
        assert!((gains[479] / middle - 1.0).abs() < 1e-12);
        assert!((gains[479] / g(400.0) - 1.0).abs() < 2e-3);
        // Equal ratios from one sample to the next.
        let ratio = gains[1] / gains[0];
        for pair in gains[..959].windows(2) {
            assert!((pair[1] / pair[0] / ratio - 1.0).abs() < 1e-12);
        }
        // It lands on the new section itself, not near it.
        assert_eq!(cascade.ready[0], ready(&state_variable(&at(1_600.0))));

        // A steady level keeps its gain of one all the way, give or take
        // what moving the memory adds, which is small.
        let mut cascade = IirFilter::<f64>::new(lowpass(100.0, 6));
        cascade.prepare(&spec(RATE, 1));
        let mut level = AudioBuffer::<f64>::new(1, 48_000);
        level.channel_mut(0).fill(0.5);
        level.windowed(0..24_000, |part| run_in_blocks(&mut cascade, part, 512));
        cascade.set_design(lowpass(5_000.0, 6));
        level.windowed(24_000..48_000, |part| {
            run_in_blocks(&mut cascade, part, 512)
        });
        let settled = &level.channel(0)[20_000..];
        let drift = settled
            .iter()
            .fold(0.0f64, |worst, s| worst.max((s - 0.5).abs()));
        assert!(drift < 1e-3, "the level moved by {drift}");
    }

    /// A 200 Hz tone of amplitude 0.5 through a filter whose design changes
    /// from `from` to `to` on frame 5000, by blocks of 64: the largest step
    /// of the output, divided by the largest step of the tone.
    fn click(glide: f64, from: Design, to: Design) -> f64 {
        let mut filter = IirFilter::<f64>::new(from).with_glide(glide);
        filter.prepare(&spec(RATE, 1));
        let mut signal = sine::<f64>(1, 9_600, 200.0, RATE, 0.5);
        let step = largest_jump(signal.channel(0));
        signal.windowed(0..5_000, |part| run_in_blocks(&mut filter, part, 64));
        filter.set_design(to);
        signal.windowed(5_000..9_600, |part| run_in_blocks(&mut filter, part, 64));
        largest_jump(signal.channel(0)) / step
    }

    #[test]
    fn a_change_of_design_glides_without_a_click() {
        let low = lowpass(1_000.0, 4);
        let high = Design::butterworth(Band::Highpass { cutoff_hz: 2_000.0 }, 4);
        let band =
            |low_hz, high_hz| Design::chebyshev_i(Band::Bandpass { low_hz, high_hz }, 8, 1.0);
        let cases = [
            (low, high, 1.1),
            (high, low, 1.1),
            (low, band(150.0, 250.0), 1.2),
            (band(150.0, 250.0), band(5_000.0, 6_000.0), 3.2),
        ];
        for (from, to, most) in cases {
            let glided = click(BiquadCascade::<f64>::DEFAULT_GLIDE_SECONDS, from, to);
            assert!(glided < most, "{from:?} to {to:?}: {glided}");
        }
        // Switched at once, the same changes jump: the test can see a click.
        assert!(click(0.0, low, high) > 50.0);
        assert!(click(0.0, band(150.0, 250.0), band(5_000.0, 6_000.0)) > 20.0);
    }

    /// Narrow, steep band-passes at both ends of the spectrum, a steep
    /// low-pass and a steep high-pass.
    fn very_resonant_designs() -> [Design; 4] {
        [
            Design::chebyshev_i(
                Band::Bandpass {
                    low_hz: 30.0,
                    high_hz: 31.0,
                },
                16,
                3.0,
            ),
            Design::chebyshev_i(
                Band::Bandpass {
                    low_hz: 15_000.0,
                    high_hz: 15_100.0,
                },
                16,
                3.0,
            ),
            Design::chebyshev_ii(Band::Lowpass { cutoff_hz: 20.0 }, 16, 100.0),
            Design::butterworth(
                Band::Highpass {
                    cutoff_hz: 20_000.0,
                },
                16,
            ),
        ]
    }

    #[test]
    fn switching_between_very_resonant_designs_does_not_run_away() {
        // The designs one after another, noise of peak 1 in.
        let designs = very_resonant_designs();
        let mut worst = 0.0f64;
        for every in [7, 64, 480, 960, 4_800] {
            let mut filter = IirFilter::<f64>::new(designs[0]);
            filter.prepare(&ProcessSpec {
                sample_rate: RATE,
                max_block_size: 4_800,
                num_channels: 1,
            });
            for (round, seed) in (0..200).zip(1..) {
                filter.set_design(designs[round % 4]);
                let mut block = noise::<f64>(1, every, seed);
                filter.process(&mut block);
                worst = worst.max(strict_buffer_peak(&block));
            }
        }
        assert!(worst < 1.5, "the output reached {worst}");
    }

    #[test]
    fn a_direct_form_cascade_glided_the_same_way_runs_away() {
        // What the documentation compares with: the same sections in
        // transposed direct form II, their coefficients moved in straight
        // lines over 960 samples, and the same switching every 4800.
        const GLIDE: usize = 960;
        let designs = very_resonant_designs();
        let rows = |design: &Design| -> [[f64; 5]; iir::MAX_SECTIONS] {
            let mut rows = [[1.0, 0.0, 0.0, 0.0, 0.0]; iir::MAX_SECTIONS];
            for (row, s) in rows.iter_mut().zip(design.sections(RATE).iter()) {
                *row = [s.b0, s.b1, s.b2, s.a1, s.a2];
            }
            rows
        };
        let mut now = rows(&designs[0]);
        let mut memory = [[0.0f64; 2]; iir::MAX_SECTIONS];
        let mut random = Random::new(1);
        let mut worst = 0.0f64;
        for round in 0..40 {
            let (from, to) = (now, rows(&designs[round % 4]));
            for n in 0..4_800 {
                let t = ((n + 1) as f64 / GLIDE as f64).min(1.0);
                let mut x = random.between(-1.0, 1.0);
                for k in 0..iir::MAX_SECTIONS {
                    for i in 0..5 {
                        now[k][i] = from[k][i] + t * (to[k][i] - from[k][i]);
                    }
                    let c = now[k];
                    let y = c[0] * x + memory[k][0];
                    memory[k][0] = c[1] * x - c[3] * y + memory[k][1];
                    memory[k][1] = c[2] * x - c[4] * y;
                    x = y;
                }
                worst = worst.max(x.abs());
            }
        }
        assert!(worst > 1e6, "it reached only {worst}");
    }

    #[test]
    fn sections_that_go_away_hold_no_memory_when_they_come_back() {
        let mut filter = IirFilter::<f64>::new(lowpass(1_000.0, 8));
        filter.prepare(&spec(RATE, 2));
        let mut signal = noise::<f64>(2, 2_000, 3);
        filter.process(&mut signal);
        // From four sections to one: the other three glide to sections
        // that pass the signal through, and are let go on the sample the
        // glide ends.
        filter.set_design(lowpass(1_000.0, 2));
        assert_eq!(filter.cascade.active, 4);
        let mut signal = noise::<f64>(2, 959, 4);
        filter.process(&mut signal);
        assert_eq!(filter.cascade.active, 4);
        let mut one = noise::<f64>(2, 1, 5);
        filter.process(&mut one);
        assert_eq!(filter.cascade.active, 1);
        for memory in &filter.cascade.state {
            assert!(memory[1..].iter().all(|pair| *pair == [0.0; 2]));
        }
        // Silence, until the one that is left has died away; then the
        // three come back, and nothing old comes out of them.
        let mut silence = AudioBuffer::<f64>::new(2, 48_000);
        filter.process(&mut silence);
        filter.set_design(lowpass(1_000.0, 8));
        let mut silence = AudioBuffer::<f64>::new(2, 4_800);
        filter.process(&mut silence);
        assert!(strict_buffer_peak(&silence) < 1e-100);
    }

    #[test]
    fn channels_a_block_does_not_carry_let_go_of_sections_too() {
        let mut filter = IirFilter::<f64>::new(lowpass(1_000.0, 8));
        filter.prepare(&spec(RATE, 2));
        let mut both = noise::<f64>(2, 2_000, 3);
        filter.process(&mut both);
        // The glide to fewer sections runs its course with only the first
        // channel carried; the second still lets go of them.
        filter.set_design(lowpass(1_000.0, 2));
        let mut first = noise::<f64>(1, 2_000, 4);
        filter.process(&mut first);
        assert_eq!(filter.cascade.active, 1);
        for memory in &filter.cascade.state {
            assert!(memory[1..].iter().all(|pair| *pair == [0.0; 2]));
        }
    }

    #[test]
    fn channels_are_independent_and_reset_clears_them() {
        let mut filter = IirFilter::<f32>::new(lowpass(200.0, 4));
        filter.prepare(&spec(RATE, 2));
        let mut buffer = AudioBuffer::<f32>::new(2, 32);
        buffer.channel_mut(0).fill(1.0);
        filter.process(&mut buffer);
        assert!(buffer.channel(0)[31] > 0.0);
        assert_eq!(buffer.channel(1), &[0.0; 32]);

        filter.reset();
        buffer.clear();
        filter.process(&mut buffer);
        assert_eq!(strict_buffer_peak(&buffer), 0.0);
    }

    #[test]
    fn it_survives_garbage_and_use_before_it_is_prepared() {
        // Not prepared: there is no memory for any channel, so audio passes
        // untouched, and nothing panics.
        let sections = lowpass(500.0, 6).sections(RATE);
        let mut cascade = BiquadCascade::<f32>::new(&sections);
        assert_eq!(cascade.sections(), sections.as_slice());
        let mut buffer = noise::<f32>(2, 64, 1);
        let before = buffer.clone();
        cascade.process(&mut buffer);
        assert!(buffer == before);

        // More channels than prepared for: the others are left alone.
        cascade.prepare(&spec(RATE, 1));
        let mut wide = noise::<f32>(3, 600, 2);
        let before = wide.clone();
        cascade.process(&mut wide);
        assert!(wide.channel(0) != before.channel(0));
        assert_eq!(wide.channel(2), before.channel(2));

        // More sections than fit: the first sixteen run.
        let many = [sections[0]; 20];
        cascade.set_sections(&many);
        assert_eq!(cascade.sections().len(), BiquadCascade::<f32>::MAX_SECTIONS);
        // The same sections again do not restart the glide.
        let mut block = AudioBuffer::<f32>::new(1, 2_000);
        cascade.process(&mut block);
        assert!(!cascade.is_gliding());
        cascade.set_sections(&many);
        assert!(!cascade.is_gliding());

        // A section that is not a number passes the signal through; one that
        // is not stable is run on the edge of stability, not beyond it.
        let nan = BiquadCoefficients {
            a1: f64::NAN,
            ..sections[0]
        };
        let unstable = BiquadCoefficients {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: -1.9,
            a2: 1.2,
        };
        let mut cascade = BiquadCascade::<f64>::new(&[nan, unstable]).with_glide(0.0);
        cascade.prepare(&spec(RATE, 1));
        assert_eq!(cascade.rows[0], IDENTITY);
        assert_eq!(cascade.rows[1][1], 0.0);
        let mut block = noise::<f64>(1, 48_000, 7);
        block.channel_mut(0)[1_000..].fill(0.0);
        cascade.process(&mut block);
        let peak = strict_peak(block.channel(0));
        assert!(peak.is_finite() && peak < 1e4, "{peak}");
        let tail = strict_peak(&block.channel(0)[40_000..]);
        assert!(tail < peak, "{tail}");

        // A block with no channel still moves the glide on.
        let mut filter = IirFilter::<f64>::new(lowpass(100.0, 2));
        filter.prepare(&spec(RATE, 1));
        filter.set_design(lowpass(10_000.0, 2));
        let mut empty = AudioBuffer::<f64>::new(0, 960);
        filter.process(&mut empty);
        assert!(!filter.cascade.is_gliding());
        let design = lowpass(10_000.0, 2);
        assert_eq!(filter.design(), design);
        assert_eq!(filter.sections(), design.sections(RATE).as_slice());
    }

    #[test]
    fn the_processor_follows_the_design_where_its_coefficients_lose_digits() {
        // A second-order Butterworth low-pass at 1 Hz and 48 kHz: its poles
        // are 9e-5 from the unit circle and about as far from z = 1, and its
        // coefficients carry them to about 8e-9. A cascade made from the
        // sections runs the design itself; one made from the coefficients
        // runs those, and measures as they do.
        let design = lowpass(1.0, 2);
        let sections = design.sections(RATE);
        let len = 1 << 19;
        let mut exact = BiquadCascade::<f64>::from_sections(&sections);
        let mut rounded = BiquadCascade::<f64>::new(&sections);
        exact.prepare(&spec(RATE, 1));
        rounded.prepare(&spec(RATE, 1));
        let exact = impulse_response(&mut exact, len);
        let rounded = impulse_response(&mut rounded, len);
        let (mut by_exact, mut by_rounded, mut rounded_against_coefficients) =
            (0.0f64, 0.0f64, 0.0f64);
        for frequency in [0.0, 0.1, 0.5, 1.0, 2.0, 5.0] {
            let formula = design.magnitude_at(frequency, RATE);
            let coefficients = sections.magnitude_at(frequency, RATE);
            let exact = gain_at(&exact, frequency, RATE);
            let rounded = gain_at(&rounded, frequency, RATE);
            by_exact = by_exact.max((exact / formula - 1.0).abs());
            by_rounded = by_rounded.max((rounded / formula - 1.0).abs());
            rounded_against_coefficients =
                rounded_against_coefficients.max((rounded / coefficients - 1.0).abs());
        }
        assert!(by_exact < 1e-12, "{by_exact}");
        assert!(by_rounded > 5e-9 && by_rounded < 1e-8, "{by_rounded}");
        assert!(
            rounded_against_coefficients < 1e-12,
            "{rounded_against_coefficients}"
        );
    }

    #[test]
    fn a_design_its_coefficients_cannot_carry_runs_from_the_design_itself() {
        // A second-order type II low-pass with 200 dB at 10 Hz and 192 kHz.
        // Its poles are the reciprocals of a point of the ellipse with
        // `sinh μ`, `cosh μ` at `θ = π/4`, `μ = asinh(ε)/2`: of size
        // `1 / sqrt(sinh² μ + cos² θ)` and damping `2·sinh μ·sin θ` times
        // that, which puts them at 1.4e-4 Hz; and the zeros at `1 / cos θ`.
        // Rounded to coefficients, `1 + a1 + a2` would be about 2e-17, below
        // their last digit: the cascade made from them used to see a section
        // with a gain of 1.1e14 at 0 Hz.
        const RATE: f64 = 192_000.0;
        let design = Design::chebyshev_ii(Band::Lowpass { cutoff_hz: 10.0 }, 2, 200.0);
        let mut filter = IirFilter::<f64>::new(design);
        filter.prepare(&spec(RATE, 1));
        let [g, k, high, band, low] = filter.cascade.rows[0];
        let epsilon = (10f64.powi(20) - 1.0).sqrt();
        let mu = epsilon.asinh() / 2.0;
        let theta = core::f64::consts::FRAC_PI_4;
        let size = 1.0 / (mu.sinh().powi(2) + theta.cos().powi(2)).sqrt();
        let edge = (core::f64::consts::PI * 10.0 / RATE).tan();
        assert!((g / (size * edge) - 1.0).abs() < 1e-13, "{g}");
        assert!(
            (k / (2.0 * mu.sinh() * theta.sin() * size) - 1.0).abs() < 1e-13,
            "{k}"
        );
        assert!(
            (high / (size * theta.cos()).powi(2) - 1.0).abs() < 1e-13,
            "{high}"
        );
        assert_eq!(band, 0.0);
        // Unity gain at 0 Hz, which is the weight of the low-pass output.
        assert!((low - 1.0).abs() < 1e-15, "{low}");
        // From the coefficients, the same section is nothing like it.
        let from_coefficients = state_variable(&filter.sections()[0]);
        assert!(
            (from_coefficients[0] / g - 1.0).abs() > 0.5
                || (from_coefficients[4] - 1.0).abs() > 0.5,
            "{from_coefficients:?}"
        );
        // And it runs: a burst of noise, then silence, never grows.
        let mut burst = noise::<f64>(1, 4_800, 3);
        filter.process(&mut burst);
        let mut silence = AudioBuffer::<f64>::new(1, 48_000);
        filter.process(&mut silence);
        let (loud, quiet) = (strict_buffer_peak(&burst), strict_buffer_peak(&silence));
        assert!(loud.is_finite() && quiet <= loud, "{loud}, {quiet}");
    }

    #[test]
    fn huge_coefficients_pass_the_signal_through_and_leave_no_trace() {
        // Poles far beyond the unit circle, on either side. A section whose
        // state-variable form is not made of finite numbers is replaced by
        // one that passes its input through; one that is, is run on the
        // edge of stability. Either way nothing that is not a number comes
        // out, and a section set afterwards works.
        for a1 in [1e300, -1e300, 1e200, f64::MAX, -f64::MAX] {
            let huge = BiquadCoefficients {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1,
                a2: 0.0,
            };
            let mut cascade = BiquadCascade::<f64>::new(&[huge]).with_glide(0.0);
            cascade.prepare(&spec(RATE, 1));
            let mut block = AudioBuffer::<f64>::new(1, 64);
            block.channel_mut(0).fill(0.5);
            cascade.process(&mut block);
            assert!(strict_buffer_peak(&block).is_finite(), "{a1}");
            cascade.set_sections(&[BiquadCoefficients::IDENTITY]);
            block.channel_mut(0).fill(0.5);
            cascade.process(&mut block);
            assert!(block.channel(0)[10..].iter().all(|&s| s == 0.5), "{a1}");
        }
        // The first of them: its gain `g` would be infinite, so it is the
        // section that passes its input through.
        let overflowing = BiquadCoefficients {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 1e300,
            a2: 0.0,
        };
        assert_eq!(state_variable(&overflowing), IDENTITY);
    }
}
