//! The shape of each band (RFC 6716, section 4.3.4): the split of a band
//! into halves and of a stereo band into mid and side, each with the angle
//! between them, the pulses of the pyramid vector quantiser, the spreading
//! rotation, folding from lower bands where no pulse was coded, and the
//! changes of time and frequency resolution. This follows the reference
//! implementation, with the updates of RFC 8251, in floating point.

use super::pvq::{bits_to_pulses, decode_pulses, get_pulses, pulse_cache, pulses_to_bits};
use super::rate::log_n;
use super::tables::{BANDS, EBANDS};
use crate::opus::range::{BITRES, RangeDecoder, ilog};

/// How the spreading rotation is set (Table 56: the `spread` symbol).
pub(crate) const SPREAD_NONE: usize = 0;
pub(crate) const SPREAD_AGGRESSIVE: usize = 3;

const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;

/// The generator of the noise folded into bands that coded nothing
/// (`celt_lcg_rand`).
#[inline]
pub(crate) fn lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

/// `(a * b + 2^14) >> 15` on 16-bit values, as the angle code computes.
#[inline]
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16_384 + i32::from(a as i16) * i32::from(b as i16)) >> 15
}

/// The cosine of `x * pi / 32768`, in Q15, exactly as the encoder works it
/// out (`bitexact_cos`).
fn bitexact_cos(x: i32) -> i32 {
    let tmp = (4_096 + x * x) >> 13;
    let x2 = tmp;
    let x2 = (32_767 - x2) + frac_mul16(x2, -7_651 + frac_mul16(x2, 8_277 + frac_mul16(-626, x2)));
    1 + x2
}

/// `log2(sin / cos)` in Q11, exactly (`bitexact_log2tan`).
fn bitexact_log2tan(mut sin: i32, mut cos: i32) -> i32 {
    let lc = ilog(cos as u32) as i32;
    let ls = ilog(sin as u32) as i32;
    cos <<= 15 - lc;
    sin <<= 15 - ls;
    (ls - lc) * (1 << 11) + frac_mul16(sin, frac_mul16(sin, -2_597) + 7_932)
        - frac_mul16(cos, frac_mul16(cos, -2_597) + 7_932)
}

/// The resolution of the angle a split codes (`compute_qn`).
fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [
        16_384, 17_866, 19_483, 21_247, 23_170, 25_267, 27_554, 30_048,
    ];
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    // Enough is left for a pulse in the side at itheta = 16384.
    let mut qb = (b + n2 * offset) / n2;
    qb = qb.min(b - pulse_cap - (4 << BITRES));
    qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let qn = EXP2_TABLE8[(qb & 7) as usize] >> (14 - (qb >> BITRES));
        ((qn + 1) >> 1) << 1
    }
}

/// `x[i] *= gain / |x|`, so that `x` has the length `gain`.
fn renormalise(x: &mut [f32], gain: f32) {
    let energy = 1e-15 + x.iter().map(|&v| v * v).sum::<f32>();
    let g = (1.0 / energy.sqrt()) * gain;
    for value in x {
        *value *= g;
    }
}

/// One step of the spreading rotation, both ways along `x`
/// (`exp_rotation1`).
fn exp_rotation1(x: &mut [f32], stride: usize, c: f32, s: f32) {
    let len = x.len();
    let ms = -s;
    if len <= stride {
        return;
    }
    for i in 0..len - stride {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 + ms * x2;
    }
    if len < 2 * stride + 1 {
        return;
    }
    for i in (0..len - 2 * stride).rev() {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 + ms * x2;
    }
}

/// Undoes the rotation that spreads the energy of few pulses over more
/// bins (`exp_rotation` with `dir = -1`, section 4.3.4.3).
fn exp_rotation(x: &mut [f32], len: usize, stride: usize, k: usize, spread: usize) {
    const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[spread - 1];
    let gain = len as f32 / (len + factor * k) as f32;
    let theta = 0.5 * (gain * gain);
    let c = (0.5 * std::f32::consts::PI * theta).cos();
    let s = (0.5 * std::f32::consts::PI * (1.0 - theta)).cos();
    let mut stride2 = 0;
    if len >= 8 * stride {
        stride2 = 1;
        // The rounded square root of len / stride.
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let part = len / stride;
    for i in 0..stride {
        let block = &mut x[i * part..(i + 1) * part];
        if stride2 > 0 {
            exp_rotation1(block, stride2, s, c);
        }
        exp_rotation1(block, 1, c, s);
    }
}

/// Which of the `blocks` short blocks a vector of pulses touches
/// (`extract_collapse_mask`).
fn collapse_mask(pulses: &[i32], n: usize, blocks: usize) -> u32 {
    if blocks <= 1 {
        return 1;
    }
    let n0 = n / blocks;
    let mut mask = 0;
    for block in 0..blocks {
        if pulses[block * n0..(block + 1) * n0].iter().any(|&p| p != 0) {
            mask |= 1 << block;
        }
    }
    mask
}

/// The Haar transform of pairs `stride` apart (`haar1`).
fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let half = n0 >> 1;
    let scale = std::f32::consts::FRAC_1_SQRT_2;
    for i in 0..stride {
        for j in 0..half {
            let a = scale * x[stride * 2 * j + i];
            let b = scale * x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = a + b;
            x[stride * (2 * j + 1) + i] = a - b;
        }
    }
}

/// The order of the short blocks for the Hadamard interleaving.
const ORDERY: [usize; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

fn deinterleave_hadamard(x: &mut [f32], tmp: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    if hadamard {
        let order = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[order[i] * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

fn interleave_hadamard(x: &mut [f32], tmp: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    if hadamard {
        let order = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[order[i] * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

/// Mixes mid and side back into left and right (`stereo_merge`).
fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32) {
    let mut xp = 0.0f32;
    let mut side = 0.0f32;
    for (&a, &b) in y.iter().zip(x.iter()) {
        xp += a * b;
        side += a * a;
    }
    let xp = mid * xp;
    let mid2 = mid;
    let el = mid2 * mid2 + side - 2.0 * xp;
    let er = mid2 * mid2 + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        y.copy_from_slice(x);
        return;
    }
    let lgain = 1.0 / el.sqrt();
    let rgain = 1.0 / er.sqrt();
    for (a, b) in x.iter_mut().zip(y.iter_mut()) {
        let l = mid * *a;
        let r = *b;
        *a = lgain * (l - r);
        *b = rgain * (l + r);
    }
}

/// What the angle of a split decided (`split_ctx`).
struct Split {
    inv: bool,
    imid: i32,
    iside: i32,
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

/// The state that runs through the bands of a frame (`band_ctx`).
pub(crate) struct Bands<'e, 'd> {
    pub(crate) ec: &'e mut RangeDecoder<'d>,
    pub(crate) band: usize,
    pub(crate) intensity: usize,
    pub(crate) spread: usize,
    pub(crate) tf_change: i32,
    pub(crate) remaining_bits: i32,
    pub(crate) seed: u32,
    /// Scratch for the reorderings and the pulses.
    pub(crate) tmp: Vec<f32>,
    pub(crate) pulses: Vec<i32>,
}

impl Bands<'_, '_> {
    /// Reads the angle of a split (`compute_theta`).
    #[allow(clippy::too_many_arguments)]
    fn compute_theta(
        &mut self,
        n: usize,
        b: &mut i32,
        blocks: usize,
        blocks0: usize,
        lm: i32,
        stereo: bool,
        fill: &mut u32,
    ) -> Split {
        let ni = n as i32;
        let pulse_cap = log_n(self.band) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if stereo && n == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = compute_qn(ni, *b, offset, pulse_cap, stereo);
        if stereo && self.band >= self.intensity {
            qn = 1;
        }
        let tell = self.ec.tell_frac();
        let mut itheta = 0;
        let mut inv = false;
        if qn != 1 {
            if stereo && n > 2 {
                // A step: probability 3 up to the middle, 1 after.
                let p0 = 3;
                let x0 = qn / 2;
                let ft = (p0 * (x0 + 1) + x0) as u32;
                let fs = self.ec.decode(ft) as i32;
                let x = if fs < (x0 + 1) * p0 {
                    fs / p0
                } else {
                    x0 + 1 + (fs - (x0 + 1) * p0)
                };
                let (low, high) = if x <= x0 {
                    (p0 * x, p0 * (x + 1))
                } else {
                    ((x - 1 - x0) + (x0 + 1) * p0, (x - x0) + (x0 + 1) * p0)
                };
                self.ec.update(low as u32, high as u32, ft);
                itheta = x;
            } else if blocks0 > 1 || stereo {
                itheta = self.ec.uint((qn + 1) as u32) as i32;
            } else {
                // A triangle.
                let half = qn >> 1;
                let ft = ((half + 1) * (half + 1)) as u32;
                let fm = self.ec.decode(ft);
                let (fl, fs);
                if fm < ((half * (half + 1)) >> 1) as u32 {
                    itheta = ((isqrt(8 * fm + 1) - 1) >> 1) as i32;
                    fs = itheta + 1;
                    fl = (itheta * (itheta + 1)) >> 1;
                } else {
                    itheta = ((2 * (qn as u32 + 1) - isqrt(8 * (ft - fm - 1) + 1)) >> 1) as i32;
                    fs = qn + 1 - itheta;
                    fl = ft as i32 - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1);
                }
                self.ec.update(fl as u32, (fl + fs) as u32, ft);
            }
            itheta = ((itheta as u32 * 16_384) / qn as u32) as i32;
        } else if stereo {
            if *b > 2 << BITRES && self.remaining_bits > 2 << BITRES {
                inv = self.ec.bit_logp(2);
            }
            itheta = 0;
        }
        let qalloc = self.ec.tell_frac() - tell;
        *b -= qalloc;

        let (imid, iside, delta);
        if itheta == 0 {
            imid = 32_767;
            iside = 0;
            *fill &= (1 << blocks) - 1;
            delta = -16_384;
        } else if itheta == 16_384 {
            imid = 0;
            iside = 32_767;
            *fill &= ((1 << blocks) - 1) << blocks;
            delta = 16_384;
        } else {
            imid = bitexact_cos(itheta);
            iside = bitexact_cos(16_384 - itheta);
            // The split of bits between mid and side that minimises the
            // squared error.
            delta = frac_mul16((ni - 1) << 7, bitexact_log2tan(iside, imid));
        }
        Split {
            inv,
            imid,
            iside,
            delta,
            itheta,
            qalloc,
        }
    }

    /// A band of one bin: its sign (`quant_band_n1`).
    fn band_n1(
        &mut self,
        x: &mut [f32],
        y: Option<&mut [f32]>,
        lowband_out: Option<&mut [f32]>,
    ) -> u32 {
        let read_sign = |this: &mut Self| -> f32 {
            let mut sign = false;
            if this.remaining_bits >= 1 << BITRES {
                sign = this.ec.bits(1) != 0;
                this.remaining_bits -= 1 << BITRES;
            }
            if sign { -1.0 } else { 1.0 }
        };
        x[0] = read_sign(self);
        if let Some(y) = y {
            y[0] = read_sign(self);
        }
        if let Some(out) = lowband_out {
            out[0] = x[0];
        }
        1
    }

    /// The shape of one partition of a band, split in halves as long as it
    /// is given more bits than one codebook takes (`quant_partition`).
    #[allow(clippy::too_many_arguments)]
    fn partition(
        &mut self,
        x: &mut [f32],
        n: usize,
        b: i32,
        blocks: usize,
        lowband: Option<&[f32]>,
        lm: i32,
        gain: f32,
        fill: u32,
    ) -> u32 {
        let cache = pulse_cache().entry(self.band, lm);
        let mut fill = fill;
        let mut b = b;
        let blocks0 = blocks;
        if lm != -1 && b > i32::from(cache[usize::from(cache[0])]) + 12 && n > 2 {
            let half = n >> 1;
            let lm = lm - 1;
            if blocks == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            let blocks = (blocks + 1) >> 1;
            let split = self.compute_theta(half, &mut b, blocks, blocks0, lm, false, &mut fill);
            let mut delta = split.delta;
            let mid = (1.0 / 32_768.0) * split.imid as f32;
            let side = (1.0 / 32_768.0) * split.iside as f32;
            // More bits to the quieter short blocks than they would
            // deserve.
            if blocks0 > 1 && split.itheta & 0x3FFF != 0 {
                if split.itheta > 8_192 {
                    // Pre-echo masking, roughly.
                    delta -= delta >> (4 - lm);
                } else {
                    // Forward masking of 1.5 dB per 10 ms.
                    delta = (delta + ((half as i32) << BITRES >> (5 - lm))).min(0);
                }
            }
            let mut mbits = b.min((b - delta) / 2).max(0);
            let mut sbits = b - mbits;
            self.remaining_bits -= split.qalloc;
            let (first, second) = x.split_at_mut(half);
            let next_lowband2 = lowband.map(|low| &low[half..]);
            let mut rebalance = self.remaining_bits;
            let mask = if mbits >= sbits {
                let mut cm =
                    self.partition(first, half, mbits, blocks, lowband, lm, gain * mid, fill);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && split.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                cm |= self.partition(
                    second,
                    half,
                    sbits,
                    blocks,
                    next_lowband2,
                    lm,
                    gain * side,
                    fill >> blocks,
                ) << (blocks0 >> 1);
                cm
            } else {
                let mut cm = self.partition(
                    second,
                    half,
                    sbits,
                    blocks,
                    next_lowband2,
                    lm,
                    gain * side,
                    fill >> blocks,
                ) << (blocks0 >> 1);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && split.itheta != 16_384 {
                    mbits += rebalance - (3 << BITRES);
                }
                cm |= self.partition(first, half, mbits, blocks, lowband, lm, gain * mid, fill);
                cm
            };
            return mask;
        }

        // No split: a codebook of pulses.
        let mut q = bits_to_pulses(self.band, lm, b);
        let mut curr_bits = pulses_to_bits(self.band, lm, q);
        self.remaining_bits -= curr_bits;
        // Never past the budget.
        while self.remaining_bits < 0 && q > 0 {
            self.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses_to_bits(self.band, lm, q);
            self.remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = get_pulses(q);
            let mut pulses = std::mem::take(&mut self.pulses);
            let energy = decode_pulses(&mut pulses[..n], n, k, self.ec);
            let g = (1.0 / energy.sqrt()) * gain;
            for (value, &pulse) in x[..n].iter_mut().zip(&pulses[..n]) {
                *value = g * pulse as f32;
            }
            exp_rotation(&mut x[..n], n, blocks, k, self.spread);
            let mask = collapse_mask(&pulses[..n], n, blocks);
            self.pulses = pulses;
            return mask;
        }
        // No pulse: fill the band all the same.
        let cm_mask = ((1u64 << blocks) - 1) as u32;
        fill &= cm_mask;
        if fill == 0 {
            x[..n].fill(0.0);
            return 0;
        }
        let mask = match lowband {
            None => {
                // Noise.
                for value in &mut x[..n] {
                    self.seed = lcg_rand(self.seed);
                    *value = ((self.seed as i32) >> 20) as f32;
                }
                cm_mask
            }
            Some(lowband) => {
                // The spectrum below, folded, with a little noise about
                // 48 dB down.
                for (value, &low) in x[..n].iter_mut().zip(lowband) {
                    self.seed = lcg_rand(self.seed);
                    let tmp = 1.0 / 256.0;
                    *value = low + if self.seed & 0x8000 != 0 { tmp } else { -tmp };
                }
                fill
            }
        };
        renormalise(&mut x[..n], gain);
        mask
    }

    /// The shape of a band of one channel, with its changes of resolution
    /// (`quant_band`). `lowband` is a copy of the spectrum folded from,
    /// which this may change.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn band(
        &mut self,
        x: &mut [f32],
        n: usize,
        b: i32,
        blocks: usize,
        mut lowband: Option<&mut [f32]>,
        lm: i32,
        lowband_out: Option<&mut [f32]>,
        gain: f32,
        fill: u32,
    ) -> u32 {
        let n0 = n;
        let mut n_b = n;
        let mut blocks = blocks;
        let long_blocks = blocks == 1;
        let mut time_divide = 0;
        let mut fill = fill;
        let mut tf_change = self.tf_change;

        n_b /= blocks;
        if n == 1 {
            return self.band_n1(x, None, lowband_out);
        }
        let recombine = tf_change.max(0) as usize;

        // A finer frequency resolution: the short blocks recombined.
        for k in 0..recombine {
            const BIT_INTERLEAVE: [u32; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
            if let Some(low) = lowband.as_deref_mut() {
                haar1(low, n >> k, 1 << k);
            }
            fill = BIT_INTERLEAVE[(fill & 0xF) as usize]
                | BIT_INTERLEAVE[((fill >> 4) & 0xF) as usize] << 2;
        }
        blocks >>= recombine;
        n_b <<= recombine;

        // A finer time resolution.
        while (n_b & 1) == 0 && tf_change < 0 {
            if let Some(low) = lowband.as_deref_mut() {
                haar1(low, n_b, blocks);
            }
            fill |= fill << blocks;
            blocks <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf_change += 1;
        }
        let blocks0 = blocks;
        let n_b0 = n_b;

        // Samples in time order rather than frequency order.
        let mut tmp = std::mem::take(&mut self.tmp);
        if blocks0 > 1 {
            if let Some(low) = lowband.as_deref_mut() {
                deinterleave_hadamard(
                    low,
                    &mut tmp,
                    n_b >> recombine,
                    blocks0 << recombine,
                    long_blocks,
                );
            }
        }

        let mut cm = self.partition(x, n, b, blocks, lowband.as_deref(), lm, gain, fill);

        // Back to frequency order, and the changes of resolution undone.
        if blocks0 > 1 {
            interleave_hadamard(
                x,
                &mut tmp,
                n_b >> recombine,
                blocks0 << recombine,
                long_blocks,
            );
        }
        self.tmp = tmp;
        n_b = n_b0;
        blocks = blocks0;
        for _ in 0..time_divide {
            blocks >>= 1;
            n_b <<= 1;
            cm |= cm >> blocks;
            haar1(x, n_b, blocks);
        }
        for k in 0..recombine {
            const BIT_DEINTERLEAVE: [u32; 16] = [
                0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3,
                0xFC, 0xFF,
            ];
            cm = BIT_DEINTERLEAVE[(cm & 0xF) as usize];
            haar1(x, n0 >> k, 1 << k);
        }
        blocks <<= recombine;

        // Scaled for the bands that will fold from this one.
        if let Some(out) = lowband_out {
            let scale = (n0 as f32).sqrt();
            for (out, &value) in out[..n0].iter_mut().zip(&x[..n0]) {
                *out = scale * value;
            }
        }
        cm & ((1u64 << blocks) - 1) as u32
    }

    /// The shape of a stereo band, as mid and side and the angle between
    /// them (`quant_band_stereo`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn band_stereo(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        n: usize,
        b: i32,
        blocks: usize,
        lowband: Option<&mut [f32]>,
        lm: i32,
        lowband_out: Option<&mut [f32]>,
        fill: u32,
    ) -> u32 {
        if n == 1 {
            return self.band_n1(x, Some(y), lowband_out);
        }
        let orig_fill = fill;
        let mut fill = fill;
        let mut b = b;
        let split = self.compute_theta(n, &mut b, blocks, blocks, lm, true, &mut fill);
        let mid = (1.0 / 32_768.0) * split.imid as f32;
        let side = (1.0 / 32_768.0) * split.iside as f32;
        let cm;
        if n == 2 {
            // Mid and side are at right angles: the side takes one bit.
            let mut mbits = b;
            let mut sbits = 0;
            if split.itheta != 0 && split.itheta != 16_384 {
                sbits = 1 << BITRES;
            }
            mbits -= sbits;
            let swap = split.itheta > 8_192;
            self.remaining_bits -= split.qalloc + sbits;
            let mut sign = 0;
            if sbits != 0 {
                sign = self.ec.bits(1) as i32;
            }
            let sign = (1 - 2 * sign) as f32;
            {
                let (x2, y2) = if swap {
                    (&mut *y, &mut *x)
                } else {
                    (&mut *x, &mut *y)
                };
                cm = self.band(
                    x2,
                    n,
                    mbits,
                    blocks,
                    lowband,
                    lm,
                    lowband_out,
                    1.0,
                    orig_fill,
                );
                y2[0] = -sign * x2[1];
                y2[1] = sign * x2[0];
            }
            x[0] *= mid;
            x[1] *= mid;
            y[0] *= side;
            y[1] *= side;
            let tmp = x[0];
            x[0] = tmp - y[0];
            y[0] += tmp;
            let tmp = x[1];
            x[1] = tmp - y[1];
            y[1] += tmp;
        } else {
            let mut mbits = b.min((b - split.delta) / 2).max(0);
            let mut sbits = b - mbits;
            self.remaining_bits -= split.qalloc;
            let mut rebalance = self.remaining_bits;
            if mbits >= sbits {
                // The mid is left unscaled: later bands fold from it.
                let mut mask = self.band(x, n, mbits, blocks, lowband, lm, lowband_out, 1.0, fill);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && split.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                // The side never folds: the high bits of fill are clear.
                mask |= self.band(y, n, sbits, blocks, None, lm, None, side, fill >> blocks);
                cm = mask;
            } else {
                let mut mask = self.band(y, n, sbits, blocks, None, lm, None, side, fill >> blocks);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && split.itheta != 16_384 {
                    mbits += rebalance - (3 << BITRES);
                }
                mask |= self.band(x, n, mbits, blocks, lowband, lm, lowband_out, 1.0, fill);
                cm = mask;
            }
            stereo_merge(&mut x[..n], &mut y[..n], mid);
        }
        if split.inv {
            for value in &mut y[..n] {
                *value = -*value;
            }
        }
        cm
    }
}

/// The parameters of the bands of a frame that [`quant_all_bands`] needs.
pub(crate) struct FrameBands<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) channels: usize,
    pub(crate) lm: usize,
    pub(crate) short_blocks: bool,
    pub(crate) spread: usize,
    pub(crate) dual_stereo: bool,
    pub(crate) intensity: usize,
    pub(crate) tf_res: &'a [i32; BANDS],
    pub(crate) pulses: &'a [i32; BANDS],
    pub(crate) total_bits: i32,
    pub(crate) balance: i32,
    pub(crate) coded_bands: usize,
}

/// Scratch for [`quant_all_bands`], sized once for the largest frame.
pub(crate) struct BandScratch {
    /// The normalised spectrum of the bands done, which later ones fold
    /// from, one half per channel.
    norm: Vec<f32>,
    /// A copy of the part folded from, which the band may change.
    lowband: Vec<f32>,
    tmp: Vec<f32>,
    pulses: Vec<i32>,
}

impl BandScratch {
    pub(crate) fn new() -> Self {
        let n = EBANDS[BANDS] << super::tables::MAX_LM;
        Self {
            norm: vec![0.0; 2 * n],
            lowband: vec![0.0; n],
            tmp: vec![0.0; n],
            pulses: vec![0; n],
        }
    }
}

/// Decodes the shape of every band (`quant_all_bands`) into `x`, which
/// holds the bins of one channel and then of the other, and the collapse
/// masks of the bands, a byte for each band and channel.
pub(crate) fn quant_all_bands(
    frame: &FrameBands<'_>,
    x: &mut [f32],
    collapse_masks: &mut [u8; 2 * BANDS],
    ec: &mut RangeDecoder<'_>,
    seed: &mut u32,
    scratch: &mut BandScratch,
) {
    let m = 1usize << frame.lm;
    let n_total = m * super::tables::SHORT_MDCT;
    let blocks = if frame.short_blocks { m } else { 1 };
    let norm_offset = m * EBANDS[frame.start];
    let channels = frame.channels;
    let mut dual_stereo = frame.dual_stereo;
    let mut balance = frame.balance;
    let mut lowband_offset = 0usize;
    let mut update_lowband = true;

    let BandScratch {
        norm,
        lowband: lowband_buf,
        tmp,
        pulses,
    } = scratch;
    let (norm, norm2) = norm.split_at_mut(n_total);
    let mut ctx = Bands {
        ec,
        band: 0,
        intensity: frame.intensity,
        spread: frame.spread,
        tf_change: 0,
        remaining_bits: 0,
        seed: *seed,
        tmp: std::mem::take(tmp),
        pulses: std::mem::take(pulses),
    };
    let (xs, ys) = x.split_at_mut(n_total);

    for band in frame.start..frame.end {
        ctx.band = band;
        let last = band == frame.end - 1;
        let lo = m * EBANDS[band];
        let n = m * EBANDS[band + 1] - lo;
        let tell = ctx.ec.tell_frac();
        // The bits this band gets.
        if band != frame.start {
            balance -= tell;
        }
        let remaining_bits = frame.total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        let b = if band < frame.coded_bands {
            let curr_balance = balance / (frame.coded_bands - band).min(3) as i32;
            (remaining_bits + 1)
                .min(frame.pulses[band] + curr_balance)
                .clamp(0, 16_383)
        } else {
            0
        };

        if (lo >= n + m * EBANDS[frame.start] || band == frame.start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = band;
        }
        if band == frame.start + 1 {
            // Enough of the first band to fold the second from (RFC 8251).
            let n1 = m * (EBANDS[frame.start + 1] - EBANDS[frame.start]);
            let n2 = m * (EBANDS[frame.start + 2] - EBANDS[frame.start + 1]);
            if n2 > n1 {
                norm.copy_within(2 * n1 - n2..n1, n1);
                if dual_stereo {
                    norm2.copy_within(2 * n1 - n2..n1, n1);
                }
            }
        }
        ctx.tf_change = frame.tf_res[band];

        // Which short blocks the bands folded from had energy in.
        let mut effective_lowband = None;
        let (x_cm, y_cm);
        if lowband_offset != 0
            && (frame.spread != SPREAD_AGGRESSIVE || blocks > 1 || ctx.tf_change < 0)
        {
            // Never the same content twice in one band.
            let effective = (m * EBANDS[lowband_offset]).saturating_sub(norm_offset + n);
            effective_lowband = Some(effective);
            let mut fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if m * EBANDS[fold_start] <= effective + norm_offset {
                    break;
                }
            }
            let mut fold_end = lowband_offset - 1;
            loop {
                fold_end += 1;
                if !(fold_end < band && m * EBANDS[fold_end] < effective + norm_offset + n) {
                    break;
                }
            }
            let (mut xm, mut ym) = (0u32, 0u32);
            for fold in fold_start..fold_end {
                xm |= u32::from(collapse_masks[fold * channels]);
                ym |= u32::from(collapse_masks[fold * channels + channels - 1]);
            }
            x_cm = xm;
            y_cm = ym;
        } else {
            // Folding from the generator touches every block.
            x_cm = ((1u64 << blocks) - 1) as u32;
            y_cm = x_cm;
        }

        if dual_stereo && band == frame.intensity {
            // Dual stereo gives way to intensity stereo from here.
            dual_stereo = false;
            for j in 0..lo - norm_offset {
                norm[j] = 0.5 * (norm[j] + norm2[j]);
            }
        }

        let out_at = lo - norm_offset;
        let lm = frame.lm as i32;
        let x_band = &mut xs[lo..lo + n];
        let (new_x_cm, new_y_cm);
        if dual_stereo {
            let low = effective_lowband.map(|at| {
                lowband_buf[..n].copy_from_slice(&norm[at..at + n]);
                &mut lowband_buf[..n]
            });
            let out = if last {
                None
            } else {
                Some(&mut norm[out_at..out_at + n])
            };
            new_x_cm = ctx.band(x_band, n, b / 2, blocks, low, lm, out, 1.0, x_cm);
            let low = effective_lowband.map(|at| {
                lowband_buf[..n].copy_from_slice(&norm2[at..at + n]);
                &mut lowband_buf[..n]
            });
            let out = if last {
                None
            } else {
                Some(&mut norm2[out_at..out_at + n])
            };
            let y_band = &mut ys[lo..lo + n];
            new_y_cm = ctx.band(y_band, n, b / 2, blocks, low, lm, out, 1.0, y_cm);
        } else {
            let low = effective_lowband.map(|at| {
                lowband_buf[..n].copy_from_slice(&norm[at..at + n]);
                &mut lowband_buf[..n]
            });
            let out = if last {
                None
            } else {
                Some(&mut norm[out_at..out_at + n])
            };
            let cm = if channels == 2 {
                let y_band = &mut ys[lo..lo + n];
                ctx.band_stereo(x_band, y_band, n, b, blocks, low, lm, out, x_cm | y_cm)
            } else {
                ctx.band(x_band, n, b, blocks, low, lm, out, 1.0, x_cm | y_cm)
            };
            new_x_cm = cm;
            new_y_cm = cm;
        }
        collapse_masks[band * channels] = new_x_cm as u8;
        collapse_masks[band * channels + channels - 1] = new_y_cm as u8;
        balance += frame.pulses[band] + tell;
        // Folding moves up only while there is a bit a sample.
        update_lowband = b > (n as i32) << BITRES;
    }
    *seed = ctx.seed;
    *tmp = ctx.tmp;
    *pulses = ctx.pulses;
}

/// Fills the short blocks of transient frames that came out empty with
/// noise at about the level of the frames before (`anti_collapse`, section
/// 4.3.5).
#[allow(clippy::too_many_arguments)]
pub(crate) fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8; 2 * BANDS],
    lm: usize,
    channels: usize,
    size: usize,
    start: usize,
    end: usize,
    log_e: &[f32],
    prev1: &[f32],
    prev2: &[f32],
    pulses: &[i32; BANDS],
    mut seed: u32,
) {
    for band in start..end {
        let n0 = EBANDS[band + 1] - EBANDS[band];
        // The depth of the band in eighths of a bit per bin.
        let depth = ((1 + pulses[band]) as u32 / n0 as u32) >> lm;
        let thresh = 0.5 * exp2(-0.125 * depth as f32);
        let sqrt_1 = 1.0 / ((n0 << lm) as f32).sqrt();
        for c in 0..channels {
            let mut p1 = prev1[c * BANDS + band];
            let mut p2 = prev2[c * BANDS + band];
            if channels == 1 {
                p1 = p1.max(prev1[BANDS + band]);
                p2 = p2.max(prev2[BANDS + band]);
            }
            let ediff = (log_e[c * BANDS + band] - p1.min(p2)).max(0.0);
            // Short blocks hold less energy than long ones.
            let mut r = 2.0 * exp2(-ediff);
            if lm == 3 {
                r *= std::f32::consts::SQRT_2;
            }
            let r = r.min(thresh) * sqrt_1;
            let base = c * size + (EBANDS[band] << lm);
            let band_x = &mut x[base..base + (n0 << lm)];
            let mut renormalize = false;
            for k in 0..1usize << lm {
                if u32::from(collapse_masks[band * channels + c]) & (1 << k) == 0 {
                    for j in 0..n0 {
                        seed = lcg_rand(seed);
                        band_x[(j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                    }
                    renormalize = true;
                }
            }
            if renormalize {
                renormalise(band_x, 1.0);
            }
        }
    }
}

/// `2^x`, as the reference implementation's floating point build works it
/// out.
#[inline]
pub(crate) fn exp2(x: f32) -> f32 {
    #[cfg(test)]
    if APPROXIMATE_EXP2.get() {
        return exp2_approximate(x);
    }
    (std::f64::consts::LN_2 * f64::from(x)).exp() as f32
}

#[cfg(test)]
thread_local! {
    /// Set by the tests that compare with a reference built with
    /// `FLOAT_APPROX`, which works `2^x` out with the cubic of
    /// [`exp2_approximate`] instead of the C library's `exp`.
    pub(crate) static APPROXIMATE_EXP2: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `2^x` as the reference implementation approximates it when it is built
/// with `FLOAT_APPROX`: a cubic in the fraction, put into the exponent.
#[cfg(test)]
fn exp2_approximate(x: f32) -> f32 {
    let integer = x.floor() as i32;
    if integer < -50 {
        return 0.0;
    }
    let frac = x - integer as f32;
    let value =
        0.999_925_2f32 + frac * (0.695_833_56 + frac * (0.226_067_16 + 0.078_024_52 * frac));
    let bits = value.to_bits().wrapping_add((integer as u32) << 23) & 0x7FFF_FFFF;
    f32::from_bits(bits)
}

/// Integer square root, rounded down (`isqrt32`).
fn isqrt(value: u32) -> u32 {
    let mut root = f64::from(value).sqrt() as u32;
    while root.saturating_mul(root) > value {
        root -= 1;
    }
    while (root + 1).saturating_mul(root + 1) <= value {
        root += 1;
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exact_cosine_and_log_tangent_hold_their_ends() {
        assert_eq!(bitexact_cos(0), 32_768);
        assert!(bitexact_cos(16_320) < 300);
        assert!((bitexact_cos(8_192) - 23_170).abs() < 8);
        assert_eq!(bitexact_log2tan(23_170, 23_170), 0);
        assert!(bitexact_log2tan(32_767, 16_384) > 1_900);
    }

    #[test]
    fn haar_and_hadamard_orders_undo_themselves() {
        let mut x: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let original = x.clone();
        let mut tmp = vec![0.0; 16];
        deinterleave_hadamard(&mut x, &mut tmp, 4, 4, true);
        interleave_hadamard(&mut x, &mut tmp, 4, 4, true);
        assert_eq!(x, original);
        haar1(&mut x, 16, 1);
        haar1(&mut x, 16, 1);
        for (a, b) in x.iter().zip(&original) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
