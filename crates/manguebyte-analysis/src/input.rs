//! Which part of a block an analyser listens to.

use manguebyte_core::{AudioBuffer, Sample};

use crate::shared::screened;

/// Which part of a block the [`SpectrumAnalyzer`](crate::SpectrumAnalyzer)
/// and the [`PitchDetector`](crate::PitchDetector) analyse.
///
/// Both look at one stream of samples. A meter can measure each channel on
/// its own; a spectrum or a pitch of each channel would need an analyser
/// per channel, which is what to build when that is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Source {
    /// The mean of all the channels of the block. A sine at full scale in
    /// every channel reads full scale; one in only one of two channels
    /// reads 6 dB lower.
    #[default]
    Mean,
    /// One channel, by index. A block without that channel is analysed as
    /// silence of the block's length, so time keeps moving.
    Channel(usize),
}

impl Source {
    /// Calls `sink` with each sample this source takes from `buffer`, in
    /// order, after screening it (see the [crate documentation](crate#bad-samples)).
    /// Real-time safe.
    #[inline]
    pub(crate) fn for_each<S: Sample>(self, buffer: &AudioBuffer<S>, mut sink: impl FnMut(f64)) {
        let frames = buffer.num_frames();
        let channels = buffer.num_channels();
        match self {
            Self::Channel(index) if index < channels => {
                for &sample in buffer.channel(index) {
                    sink(screened(sample.to_f64()));
                }
            }
            Self::Mean if channels == 1 => {
                for &sample in buffer.channel(0) {
                    sink(screened(sample.to_f64()));
                }
            }
            Self::Mean if channels > 1 => {
                let count = channels as f64;
                for frame in 0..frames {
                    let sum: f64 = buffer
                        .iter_channels()
                        .map(|channel| {
                            channel
                                .get(frame)
                                .map_or(0.0, |&sample| screened(sample.to_f64()))
                        })
                        .sum();
                    sink(sum / count);
                }
            }
            _ => {
                for _ in 0..frames {
                    sink(0.0);
                }
            }
        }
    }
}

/// Calls `sink` with each sample of `samples`, screened.
#[inline]
pub(crate) fn for_each_sample<S: Sample>(samples: &[S], mut sink: impl FnMut(f64)) {
    for &sample in samples {
        sink(screened(sample.to_f64()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(source: Source, buffer: &AudioBuffer<f32>) -> Vec<f64> {
        let mut out = Vec::new();
        source.for_each(buffer, |sample| out.push(sample));
        out
    }

    #[test]
    fn a_source_takes_a_channel_or_the_mean() {
        let mut buffer = AudioBuffer::<f32>::new(2, 3);
        buffer.channel_mut(0).copy_from_slice(&[1.0, 0.5, f32::NAN]);
        buffer.channel_mut(1).copy_from_slice(&[0.0, 0.5, 0.25]);
        assert_eq!(collect(Source::Channel(0), &buffer), [1.0, 0.5, 0.0]);
        assert_eq!(collect(Source::Channel(1), &buffer), [0.0, 0.5, 0.25]);
        // A NaN in one channel does not wipe out the other.
        assert_eq!(collect(Source::Mean, &buffer), [0.5, 0.5, 0.125]);
        // A channel the block lacks is silence of the block's length.
        assert_eq!(collect(Source::Channel(2), &buffer), [0.0; 3]);
        assert_eq!(collect(Source::Mean, &AudioBuffer::new(0, 4)), [0.0; 4]);
        let mut mono = AudioBuffer::<f32>::new(1, 2);
        mono.channel_mut(0).copy_from_slice(&[0.75, f32::INFINITY]);
        assert_eq!(collect(Source::Mean, &mono), [0.75, 0.0]);
        assert_eq!(Source::default(), Source::Mean);
    }
}
