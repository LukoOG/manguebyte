//! Holding a spectrum.

use manguebyte_core::Sample;

use super::{SpectralFrame, SpectralProcessor, StftLayout, wrap_phase};
use crate::fft::Complex;

/// A spectral freeze: while frozen, every frame repeats the magnitudes of
/// the frame the freeze began on, whatever the input does.
///
/// Repeating a frame as it was would cancel itself out in the overlap-add,
/// so the phase of each bin keeps turning at the rate it was measured to
/// turn between the last two frames before the freeze: the phase-vocoder
/// view of a bin as one sinusoid, as M. Dolson describes it in "The phase
/// vocoder: a tutorial", Computer Music Journal, 1986. A steady sound frozen
/// this way goes on at its own pitch and level; anything else becomes a
/// drone of the spectrum it had at that moment.
///
/// The freeze starts on the first frame after
/// [`set_frozen(true)`](Self::set_frozen), and the overlap-add fades it in
/// and out over one frame. It works on any sample type and keeps its state in double
/// precision, one set per channel.
///
/// ```
/// use manguebyte_dsp::stft::{SpectralFreeze, Stft, StftSettings};
///
/// let mut stft = Stft::<_, f32>::new(StftSettings::new(2_048), SpectralFreeze::new()).unwrap();
/// stft.processor_mut().set_frozen(true);
/// assert!(stft.processor().is_frozen());
/// ```
#[derive(Debug, Clone, Default)]
pub struct SpectralFreeze {
    frozen: bool,
    bins: usize,
    /// Per channel and bin: the phase of the last frame heard.
    last_phase: Vec<f64>,
    /// Per channel and bin: how far the phase turned between the last two
    /// frames heard.
    advance: Vec<f64>,
    /// Per channel and bin: the magnitude held.
    magnitude: Vec<f64>,
    /// Per channel and bin: the phase of the last frame put out while
    /// frozen.
    phase: Vec<f64>,
    /// Per channel: whether the held spectrum has been taken.
    holding: Vec<bool>,
}

impl SpectralFreeze {
    /// A freeze, not frozen.
    pub fn new() -> Self {
        Self::default()
    }

    /// Freezes or lets go. Real-time safe.
    pub fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
    }

    /// Whether it is frozen.
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }
}

impl<S: Sample> SpectralProcessor<S> for SpectralFreeze {
    fn prepare(&mut self, layout: &StftLayout) {
        self.bins = layout.bins;
        let len = layout.bins * layout.channels;
        for state in [
            &mut self.last_phase,
            &mut self.advance,
            &mut self.magnitude,
            &mut self.phase,
        ] {
            *state = vec![0.0; len];
        }
        self.holding = vec![false; layout.channels];
    }

    fn process_frame(&mut self, frame: SpectralFrame<'_, S>) {
        let Some(holding) = self.holding.get_mut(frame.channel) else {
            return;
        };
        let range = frame.channel * self.bins..(frame.channel + 1) * self.bins;
        let last_phase = &mut self.last_phase[range.clone()];
        let advance = &mut self.advance[range.clone()];
        let magnitude = &mut self.magnitude[range.clone()];
        let phase = &mut self.phase[range];

        if self.frozen && *holding {
            for ((bin, last), ((&magnitude, phase), &advance)) in frame
                .bins
                .iter_mut()
                .zip(last_phase)
                .zip(magnitude.iter().zip(phase).zip(&*advance))
            {
                // Keep following the input, so that letting go and freezing
                // again measures the sound as it is then.
                *last = bin.im.to_f64().atan2(bin.re.to_f64());
                *phase = wrap_phase(*phase + advance);
                *bin = Complex::from_polar(magnitude, *phase);
            }
            return;
        }

        // Listening: measure each bin, and pass the frame through.
        for (((bin, last), advance), (magnitude, phase)) in frame
            .bins
            .iter()
            .zip(last_phase)
            .zip(advance)
            .zip(magnitude.iter_mut().zip(phase.iter_mut()))
        {
            let re = bin.re.to_f64();
            let im = bin.im.to_f64();
            let now = im.atan2(re);
            *advance = wrap_phase(now - *last);
            *last = now;
            *magnitude = re.hypot(im);
            *phase = now;
        }
        // This frame, as heard, is the one held from the next on.
        *holding = self.frozen;
    }

    fn reset(&mut self) {
        self.last_phase.fill(0.0);
        self.advance.fill(0.0);
        self.magnitude.fill(0.0);
        self.phase.fill(0.0);
        self.holding.fill(false);
    }
}
