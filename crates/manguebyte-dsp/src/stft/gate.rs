//! A gate per frequency bin, with a learned noise profile.

use manguebyte_core::Sample;

use super::{SpectralFrame, SpectralProcessor, StftLayout};

/// A spectral gate: every bin quieter than its threshold is turned down to
/// the floor, every bin louder is left alone.
///
/// Each bin's threshold is the larger of a threshold common to all bins and
/// the bin's own, which is either set by hand or learned from a stretch of
/// noise. Learned that way the gate is a simple denoiser: let it hear the
/// noise alone with [`set_learning(true)`](Self::set_learning), stop
/// learning, and it keeps whatever rises a margin above the noise in each
/// bin. Being a hard decision per bin and per frame, it leaves the "musical
/// noise" such gates are known for when the noise and the signal are close.
///
/// Levels are amplitudes of a sine centred on the bin, in decibels relative
/// to full scale (see [`StftLayout::amplitude_scale`]). It works on any
/// sample type.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::stft::{SpectralGate, Stft, StftSettings};
/// use manguebyte_dsp::{ProcessSpec, Processor};
///
/// let gate = SpectralGate::new(-50.0);
/// let mut stft = Stft::<_, f32>::new(StftSettings::new(1_024), gate).unwrap();
/// stft.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 1 });
///
/// // Learn the noise, then gate 10 dB above it.
/// stft.processor_mut().set_margin_db(10.0);
/// stft.processor_mut().set_learning(true);
/// let mut block = AudioBuffer::<f32>::new(1, 256);
/// stft.process(&mut block);
/// stft.processor_mut().set_learning(false);
/// ```
#[derive(Debug, Clone)]
pub struct SpectralGate {
    threshold: f64,
    floor: f64,
    margin: f64,
    learning: bool,
    learned_frames: u64,
    amplitude_scale: f64,
    /// While learning, the summed amplitude of each bin.
    sums: Vec<f64>,
    /// Each bin's own threshold, as an amplitude; zero for none.
    profile: Vec<f64>,
}

fn amplitude(db: f64) -> f64 {
    if db.is_nan() {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}

impl SpectralGate {
    /// A gate with a threshold of `threshold_db` on every bin, a floor of
    /// silence, a learning margin of 10 dB and no learned profile.
    pub fn new(threshold_db: f64) -> Self {
        Self {
            threshold: amplitude(threshold_db),
            floor: 0.0,
            margin: amplitude(10.0),
            learning: false,
            learned_frames: 0,
            amplitude_scale: 1.0,
            sums: Vec::new(),
            profile: Vec::new(),
        }
    }

    /// Sets the threshold common to every bin, in dBFS. Real-time safe.
    pub fn set_threshold_db(&mut self, threshold_db: f64) {
        self.threshold = amplitude(threshold_db);
    }

    /// Sets the gain of a gated bin, in decibels: minus infinity (the
    /// default) silences it, -20 dB leaves a tenth. Real-time safe.
    pub fn set_floor_db(&mut self, floor_db: f64) {
        self.floor = amplitude(floor_db).min(1.0);
    }

    /// Sets how far above the learned noise a bin's threshold is put, in
    /// decibels; 10 dB by default. Applies to the next profile learned.
    /// Real-time safe.
    pub fn set_margin_db(&mut self, margin_db: f64) {
        self.margin = amplitude(margin_db);
    }

    /// Starts or stops learning a noise profile. While it learns, the gate
    /// lets everything through and averages the level of each bin, over
    /// every channel. When it stops, each bin's threshold becomes its
    /// average level raised by the margin. Stopping without having heard a
    /// frame keeps the old profile. Real-time safe.
    pub fn set_learning(&mut self, learning: bool) {
        if learning && !self.learning {
            self.sums.fill(0.0);
            self.learned_frames = 0;
        } else if !learning && self.learning && self.learned_frames > 0 {
            let scale = self.margin / self.learned_frames as f64;
            for (threshold, &sum) in self.profile.iter_mut().zip(&self.sums) {
                *threshold = sum * scale;
            }
        }
        self.learning = learning;
    }

    /// Whether it is learning a noise profile.
    pub fn is_learning(&self) -> bool {
        self.learning
    }

    /// Sets one bin's own threshold, in dBFS; minus infinity removes it.
    /// Ignored for a bin beyond the spectrum. Real-time safe once prepared.
    pub fn set_bin_threshold_db(&mut self, bin: usize, threshold_db: f64) {
        if let Some(threshold) = self.profile.get_mut(bin) {
            *threshold = amplitude(threshold_db);
        }
    }

    /// Removes every bin's own threshold, leaving the common one. Real-time
    /// safe.
    pub fn clear_profile(&mut self) {
        self.profile.fill(0.0);
    }

    /// Each bin's own threshold, as an amplitude (zero for none). Empty
    /// until prepared.
    pub fn profile(&self) -> &[f64] {
        &self.profile
    }
}

impl<S: Sample> SpectralProcessor<S> for SpectralGate {
    /// Sizes the profile for the layout's bins. A profile learned for the
    /// same number of bins is kept.
    fn prepare(&mut self, layout: &StftLayout) {
        self.amplitude_scale = layout.amplitude_scale;
        if self.profile.len() != layout.bins {
            self.profile = vec![0.0; layout.bins];
        }
        self.sums = vec![0.0; layout.bins];
        self.learned_frames = 0;
    }

    fn process_frame(&mut self, frame: SpectralFrame<'_, S>) {
        let scale = self.amplitude_scale * self.amplitude_scale;
        if self.learning {
            for (sum, bin) in self.sums.iter_mut().zip(frame.bins.iter()) {
                *sum += (bin.norm_sqr().to_f64() * scale).sqrt();
            }
            self.learned_frames += 1;
            return;
        }
        let floor = S::from_f64(self.floor);
        let common = self.threshold * self.threshold;
        for (index, bin) in frame.bins.iter_mut().enumerate() {
            let own = self.profile.get(index).copied().unwrap_or(0.0);
            let threshold = common.max(own * own);
            if bin.norm_sqr().to_f64() * scale < threshold {
                *bin = bin.scale(floor);
            }
        }
    }
}
