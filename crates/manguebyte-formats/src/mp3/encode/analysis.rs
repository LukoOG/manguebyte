//! The analysis half of the hybrid filter bank of layer III (ISO/IEC
//! 11172-3, Annex C.1.3 and C.1.5.3): the polyphase filter bank that cuts
//! the signal into 32 subbands, the MDCT of 18 or of three times 6 lines of
//! each subband, and the butterflies that the decoder's alias reduction
//! undoes. Each step is the exact counterpart of the decoder's, with the
//! decoder's own tables.

use std::f64::consts::PI;
use std::sync::OnceLock;

use super::super::layer3::transform;
use super::super::tables::SYNTHESIS_WINDOW;

/// The analysis window `C[i]` of Table 3-C.1, which is the synthesis
/// window `D[i]` over 32, and the matrix `cos((2k + 1)(i - 16) pi / 64)`.
struct Polyphase {
    window: [f32; 512],
    matrix: [[f32; 64]; 32],
}

fn polyphase() -> &'static Polyphase {
    static POLYPHASE: OnceLock<Box<Polyphase>> = OnceLock::new();
    POLYPHASE.get_or_init(|| {
        let mut tables = Box::new(Polyphase {
            window: [0.0; 512],
            matrix: [[0.0; 64]; 32],
        });
        for (i, value) in tables.window.iter_mut().enumerate() {
            let units = if i <= 256 {
                SYNTHESIS_WINDOW[i]
            } else if i % 64 == 0 {
                SYNTHESIS_WINDOW[512 - i]
            } else {
                -SYNTHESIS_WINDOW[512 - i]
            };
            *value = (f64::from(units) / 65_536.0 / 32.0) as f32;
        }
        for (k, row) in tables.matrix.iter_mut().enumerate() {
            for (i, value) in row.iter_mut().enumerate() {
                *value = ((2 * k + 1) as f64 * (i as f64 - 16.0) * PI / 64.0).cos() as f32;
            }
        }
        tables
    })
}

/// What scales the decoder's transforms back to unity: its inverse MDCT
/// and synthesis filter bank give the input of this analysis times
/// `1 / SCALE`, with no scaling of their own.
const LONG_SCALE: f32 = 1.0 / 9.0;
const SHORT_SCALE: f32 = 1.0 / 3.0;

/// How many samples later than this analysis takes them the decoder gives
/// them back: 481 in the two polyphase filter banks, 576 in the
/// transforms, which take a granule after the one they code.
pub(super) const DELAY: usize = 1_057;

/// The kind of block a granule is coded with (2.4.2.7): 0 a long block,
/// 1 a long block that starts short ones, 2 three short blocks, 3 a long
/// block that ends them.
pub(super) type BlockType = u8;

/// The analysis filter bank of one channel.
pub(super) struct Analysis {
    /// The last 512 samples, newest first from `at`, twice over, so that
    /// they can be read without wrapping.
    history: Box<[f32; 1_024]>,
    at: usize,
    /// The subband samples of the last granule and of this one, by
    /// subband: what the MDCT of this granule takes.
    subbands: Box<[[f32; 36]; 32]>,
}

impl Analysis {
    pub(super) fn new() -> Self {
        Self {
            history: Box::new([0.0; 1_024]),
            at: 0,
            subbands: Box::new([[0.0; 36]; 32]),
        }
    }

    /// Runs the polyphase filter bank over the 576 samples of a granule:
    /// the 18 samples of each subband that follow those of the granule
    /// before.
    pub(super) fn filter(&mut self, samples: &[f32; 576]) {
        let tables = polyphase();
        for subband in self.subbands.iter_mut() {
            subband.copy_within(18..36, 0);
        }
        for (slot, block) in samples.chunks_exact(32).enumerate() {
            for &sample in block {
                self.at = (self.at + 511) % 512;
                self.history[self.at] = sample;
                self.history[self.at + 512] = sample;
            }
            let x = &self.history[self.at..self.at + 512];
            let mut y = [0.0f32; 64];
            for (i, value) in y.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for j in 0..8 {
                    sum += tables.window[i + 64 * j] * x[i + 64 * j];
                }
                *value = sum;
            }
            for (k, row) in tables.matrix.iter().enumerate() {
                let mut value: f32 = row.iter().zip(&y).map(|(&m, &v)| m * v).sum();
                // The decoder inverts every other sample of the odd
                // subbands after its inverse transform.
                if k % 2 == 1 && slot % 2 == 1 {
                    value = -value;
                }
                self.subbands[k][18 + slot] = value;
            }
        }
    }

    /// The MDCT of the granule last filtered, as a block of `block_type`,
    /// into `spectrum`, in the order the decoder reads it: for a long
    /// block, subband by subband with the alias butterflies applied; for
    /// short blocks, subband by subband, the six lines of each window
    /// together, which [`to_band_order`] puts in the coded order.
    pub(super) fn transform(&self, block_type: BlockType, spectrum: &mut [f32; 576]) {
        let t = transform();
        for (subband, input) in self.subbands.iter().enumerate() {
            let out = &mut spectrum[subband * 18..subband * 18 + 18];
            if block_type == 2 {
                for window in 0..3 {
                    let z = &input[6 + 6 * window..18 + 6 * window];
                    for k in 0..6 {
                        let mut sum = 0.0f32;
                        for (i, row) in t.short.iter().enumerate() {
                            sum += row[k] * t.short_window[i] * z[i];
                        }
                        out[window * 6 + k] = sum * SHORT_SCALE;
                    }
                }
            } else {
                let window = &t.windows[usize::from(block_type)];
                for (k, value) in out.iter_mut().enumerate() {
                    let mut sum = 0.0f32;
                    for (i, row) in t.long.iter().enumerate() {
                        sum += row[k] * window[i] * input[i];
                    }
                    *value = sum * LONG_SCALE;
                }
            }
        }
        if block_type != 2 {
            // The butterflies of 2.4.3.4.10.1, turned the other way.
            for subband in 1..32 {
                for i in 0..8 {
                    let below = 18 * subband - 1 - i;
                    let above = 18 * subband + i;
                    let (a, b) = (spectrum[below], spectrum[above]);
                    spectrum[below] = a * t.cs[i] + b * t.ca[i];
                    spectrum[above] = b * t.cs[i] - a * t.ca[i];
                }
            }
        }
    }
}

/// Puts the lines of short blocks from the order the transforms give them,
/// subband by subband with the six lines of each window together, into
/// the order they are coded in: band by band, and within each band window
/// by window (2.4.3.4.8 run backwards).
pub(super) fn to_band_order(spectrum: &mut [f32; 576], short_bands: &[u8; 14]) {
    let source = *spectrum;
    for band in 0..13 {
        let width = usize::from(short_bands[band + 1] - short_bands[band]);
        let start = 3 * usize::from(short_bands[band]);
        for window in 0..3 {
            for j in 0..width {
                let frequency = usize::from(short_bands[band]) + j;
                let from = (frequency / 6) * 18 + window * 6 + frequency % 6;
                spectrum[start + window * width + j] = source[from];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::layer3::{GranuleInfo, reduce_aliases, reorder, synthesize};
    use super::super::super::synth::Synthesis;
    use super::super::super::tables::SHORT_BANDS;
    use super::*;

    /// Runs `input` through this analysis and the decoder's synthesis,
    /// with the block types given granule by granule, and returns the
    /// output.
    fn round_trip(input: &[f32], types: &[BlockType]) -> Vec<f32> {
        let mut analysis = Analysis::new();
        let mut overlap = [[0.0f32; 18]; 32];
        let mut synthesis = Synthesis::new();
        let mut out = vec![0.0f32; input.len()];
        for (granule, &block_type) in types.iter().enumerate() {
            let mut samples = [0.0f32; 576];
            samples.copy_from_slice(&input[granule * 576..(granule + 1) * 576]);
            analysis.filter(&samples);
            let mut spectrum = [0.0f32; 576];
            analysis.transform(block_type, &mut spectrum);
            let gi = GranuleInfo::of_block_type(block_type);
            if block_type == 2 {
                to_band_order(&mut spectrum, &SHORT_BANDS[0]);
                reorder(&mut spectrum, 0, false);
            }
            reduce_aliases(&mut spectrum, &gi);
            synthesize(
                &spectrum,
                &gi,
                &mut overlap,
                &mut synthesis,
                &mut out[granule * 576..(granule + 1) * 576],
            );
        }
        out
    }

    #[test]
    fn the_decoders_filter_bank_gives_back_what_this_one_took() {
        let granules = 24;
        let input: Vec<f32> = (0..granules * 576)
            .map(|i| {
                let t = i as f64 / 44_100.0;
                (0.3 * (std::f64::consts::TAU * 440.0 * t).sin()
                    + 0.2 * (std::f64::consts::TAU * 5_123.0 * t).sin()
                    + 0.1 * (std::f64::consts::TAU * 15_000.0 * t).cos()) as f32
            })
            .collect();
        let sequences: [&[BlockType]; 2] = [
            &[0; 24],
            &[
                0, 0, 0, 0, 1, 2, 2, 3, 0, 0, 1, 2, 3, 0, 0, 0, 1, 2, 2, 2, 3, 0, 0, 0,
            ],
        ];
        for types in sequences {
            let out = round_trip(&input, types);
            // The polyphase filter banks delay the signal by 481 samples,
            // the transforms by a granule.
            let delay = DELAY;
            let (mut signal, mut noise) = (0.0f64, 0.0f64);
            for i in 2_000..input.len() - delay {
                let (a, b) = (f64::from(input[i]), f64::from(out[i + delay]));
                signal += a * a;
                noise += (a - b) * (a - b);
            }
            let snr = 10.0 * (signal / noise).log10();
            assert!(snr > 85.0, "{types:?}: {snr:.1} dB");
        }
    }
}
