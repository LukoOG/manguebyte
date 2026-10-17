//! The noise-to-mask ratio of a decode against its source (Brandenburg,
//! "Evaluation of quality for audio encoding at low bit rates", AES 82nd
//! convention, 1987), with a masking model of its own, apart from the
//! encoders': the source in frames of 2048 samples under a Hann window,
//! its power summed in bands one Bark wide, spread with the function of
//! Schroeder, Atal and Hall (1979), lowered by Johnston's offsets for tones
//! and noise from the spectral flatness of each band (1988), and never
//! below the threshold in quiet with full scale at 96 dB SPL. The noise is
//! the decode less the source, in the same bands.

use manguebyte_core::AudioBuffer;

const FRAME: usize = 2_048;

fn bark(hz: f64) -> f64 {
    13.0 * (0.000_76 * hz).atan() + 3.5 * (hz / 7_500.0).powi(2).atan()
}

fn quiet_db(hz: f64) -> f64 {
    let khz = (hz / 1_000.0).clamp(0.02, 20.0);
    3.64 * khz.powf(-0.8) - 6.5 * (-0.6 * (khz - 3.3).powi(2)).exp() + 1e-3 * khz.powi(4)
}

/// A radix-2 FFT, in place, of a power-of-two length, with the twiddles
/// `cos` and `sin` of `-2 pi k / n` for `k` up to `n / 2`.
fn fft(re: &mut [f64], im: &mut [f64], cos: &[f64], sin: &[f64]) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let stride = n / size;
        for start in (0..n).step_by(size) {
            for k in 0..size / 2 {
                let (wr, wi) = (cos[k * stride], sin[k * stride]);
                let (a, b) = (start + k, start + k + size / 2);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        size *= 2;
    }
}

/// What the measurement found: the mean noise-to-mask ratio over the bands
/// of every frame, in decibels, and the share of bands whose noise is
/// above the mask.
#[derive(Debug, Clone, Copy)]
pub struct Nmr {
    pub mean_db: f64,
    pub audible: f64,
}

/// The noise-to-mask ratio of `decoded` against `source`, both at `rate`,
/// over the frames that lie wholly in `range`.
pub fn nmr(
    decoded: &AudioBuffer<f32>,
    source: &AudioBuffer<f32>,
    rate: u32,
    range: std::ops::Range<usize>,
) -> Nmr {
    let rate = f64::from(rate);
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();
    let hz = |bin: usize| bin as f64 * rate / FRAME as f64;
    let band_of: Vec<usize> = (0..FRAME / 2).map(|bin| bark(hz(bin)) as usize).collect();
    let bands = band_of[FRAME / 2 - 1] + 1;
    // The power of a full-scale sine in one bin, under the window.
    let full_scale = (FRAME as f64 / 4.0).powi(2);
    let spread = |dz: f64| {
        let db = 15.81 + 7.5 * (dz + 0.474) - 17.5 * (1.0 + (dz + 0.474).powi(2)).sqrt();
        10f64.powf(db / 10.0)
    };
    let mut ratios = 0.0f64;
    let mut count = 0usize;
    let mut audible = 0usize;
    let mut re = vec![0.0f64; FRAME];
    let mut im = vec![0.0f64; FRAME];
    let angle = |k: usize| -std::f64::consts::TAU * k as f64 / FRAME as f64;
    let cos: Vec<f64> = (0..FRAME / 2).map(|k| angle(k).cos()).collect();
    let sin: Vec<f64> = (0..FRAME / 2).map(|k| angle(k).sin()).collect();
    // The spreading from every band to every other, and the threshold in
    // quiet of each band, which are the same for every frame.
    let spreading: Vec<Vec<f64>> = (0..bands)
        .map(|band| {
            (0..bands)
                .map(|other| spread(band as f64 - other as f64))
                .collect()
        })
        .collect();
    let mut quiet = vec![0.0f64; bands];
    for bin in 1..FRAME / 2 {
        quiet[band_of[bin]] += full_scale * 10f64.powf((quiet_db(hz(bin)) - 96.0) / 10.0);
    }
    for (got, want) in decoded.iter_channels().zip(source.iter_channels()) {
        let mut start = range.start;
        while start + FRAME <= range.end {
            let spectrum = |samples: &[f32], re: &mut Vec<f64>, im: &mut Vec<f64>| -> Vec<f64> {
                for i in 0..FRAME {
                    re[i] = f64::from(samples[start + i]) * window[i];
                    im[i] = 0.0;
                }
                fft(re, im, &cos, &sin);
                (0..FRAME / 2)
                    .map(|bin| re[bin] * re[bin] + im[bin] * im[bin])
                    .collect()
            };
            let signal = spectrum(want, &mut re, &mut im);
            let difference: Vec<f32> = got.iter().zip(want).map(|(&a, &b)| a - b).collect();
            let noise = spectrum(&difference, &mut re, &mut im);
            let mut energy = vec![0.0f64; bands];
            let mut noise_energy = vec![0.0f64; bands];
            let mut log_sum = vec![0.0f64; bands];
            let mut bins = vec![0usize; bands];
            for bin in 1..FRAME / 2 {
                let band = band_of[bin];
                energy[band] += signal[bin];
                noise_energy[band] += noise[bin];
                log_sum[band] += (signal[bin] + 1e-12).ln();
                bins[band] += 1;
            }
            for band in 0..bands {
                if bins[band] == 0 {
                    continue;
                }
                let spread_energy: f64 = energy
                    .iter()
                    .zip(&spreading[band])
                    .map(|(e, s)| e * s)
                    .sum();
                let mean = energy[band] / bins[band] as f64 + 1e-12;
                let geometric = (log_sum[band] / bins[band] as f64).exp();
                let flatness_db = 10.0 * (geometric / mean).log10();
                let tonality = (flatness_db / -60.0).clamp(0.0, 1.0);
                let offset = tonality * (14.5 + band as f64) + (1.0 - tonality) * 5.5;
                let mask = (spread_energy * 10f64.powf(-offset / 10.0)).max(quiet[band]);
                // Only bands with something to mask count.
                if energy[band] > quiet[band] {
                    let ratio = noise_energy[band] / mask;
                    ratios += ratio;
                    count += 1;
                    if ratio > 1.0 {
                        audible += 1;
                    }
                }
            }
            start += FRAME / 2;
        }
    }
    Nmr {
        mean_db: 10.0 * (ratios / count.max(1) as f64).log10(),
        audible: audible as f64 / count.max(1) as f64,
    }
}
