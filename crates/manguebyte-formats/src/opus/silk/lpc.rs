//! From the normalized line spectral frequencies a frame codes to the
//! coefficients of its LPC filter (RFC 6716, sections 4.2.7.5.3 to
//! 4.2.7.5.8), in the fixed-point arithmetic the RFC prescribes, so that
//! every decoder gets the same filter bit for bit.

use super::tables::{
    COSINE, LSF_CODEBOOK_NB, LSF_CODEBOOK_WB, LSF_MIN_SPACING_NB, LSF_MIN_SPACING_WB,
    LSF_ORDERING_NB, LSF_ORDERING_WB, LSF_PRED_NB, LSF_PRED_SELECT_NB, LSF_PRED_SELECT_WB,
    LSF_PRED_WB,
};

/// The order of the filter of a wideband frame; narrow and medium band
/// frames use ten.
pub(crate) const MAX_ORDER: usize = 16;

#[inline]
fn ilog(value: u32) -> i32 {
    (32 - value.leading_zeros()) as i32
}

/// The coefficients from the two stages of the codebook (sections
/// 4.2.7.5.2 to 4.2.7.5.4): `stage1` is the first-stage index, `stage2`
/// the residual of each coefficient.
pub(crate) fn decode_nlsf(wideband: bool, stage1: usize, stage2: &[i32], nlsf: &mut [i32]) {
    let order = if wideband { 16 } else { 10 };
    let codebook: &[u8] = if wideband {
        &LSF_CODEBOOK_WB[stage1]
    } else {
        &LSF_CODEBOOK_NB[stage1]
    };
    let qstep: i32 = if wideband { 9_830 } else { 11_796 };

    // The residuals, each predicted from the one above it.
    let mut res_q10 = [0i32; MAX_ORDER];
    for k in (0..order).rev() {
        let prediction = if k + 1 < order {
            let weight = if wideband {
                let row = LSF_PRED_SELECT_WB[stage1][k];
                i32::from(LSF_PRED_WB[usize::from(row - b'C')][k])
            } else {
                let row = LSF_PRED_SELECT_NB[stage1][k];
                i32::from(LSF_PRED_NB[usize::from(row - b'A')][k])
            };
            (res_q10[k + 1] * weight) >> 8
        } else {
            0
        };
        let i2 = stage2[k];
        let level = ((i2 << 10) - i2.signum() * 102) * qstep;
        res_q10[k] = prediction + (level >> 16);
    }

    // The weights of Laroia, Phamdo and Farvardin (1991) from the
    // first-stage vector, an approximation of the inverse harmonic mean of
    // the distances to its neighbours.
    for k in 0..order {
        let below = if k == 0 {
            0
        } else {
            i32::from(codebook[k - 1])
        };
        let above = if k + 1 == order {
            256
        } else {
            i32::from(codebook[k + 1])
        };
        let here = i32::from(codebook[k]);
        let w2_q18 = ((1_024 / (here - below)) + (1_024 / (above - here))) << 16;
        let i = ilog(w2_q18 as u32);
        let f = (w2_q18 >> (i - 8)) & 127;
        let y = (if i & 1 != 0 { 32_768 } else { 46_214 }) >> ((32 - i) >> 1);
        let w_q9 = y + ((213 * f * y) >> 16);
        let value = (here << 7) + (res_q10[k] << 14) / w_q9;
        nlsf[k] = value.clamp(0, 32_767);
    }
    stabilize(
        nlsf,
        if wideband {
            &LSF_MIN_SPACING_WB
        } else {
            &LSF_MIN_SPACING_NB
        },
    );
}

/// Moves the coefficients apart until each keeps its distance from its
/// neighbours (`silk_NLSF_stabilize`, section 4.2.7.5.4).
fn stabilize(nlsf: &mut [i32], spacing: &[i32]) {
    let order = nlsf.len().min(spacing.len() - 1);
    let nlsf = &mut nlsf[..order];
    for _ in 0..20 {
        // The coefficient closest to breaking its constraint.
        let mut worst = i32::MAX;
        let mut at = 0;
        for i in 0..=order {
            let below = if i == 0 { 0 } else { nlsf[i - 1] };
            let here = if i == order { 32_768 } else { nlsf[i] };
            let distance = here - below - spacing[i];
            if distance < worst {
                worst = distance;
                at = i;
            }
        }
        if worst >= 0 {
            return;
        }
        if at == 0 {
            nlsf[0] = spacing[0];
        } else if at == order {
            nlsf[order - 1] = 32_768 - spacing[order];
        } else {
            let mut min_center = spacing[at] >> 1;
            for &step in &spacing[..at] {
                min_center += step;
            }
            let mut max_center = 32_768 - (spacing[at] >> 1);
            for &step in &spacing[at + 1..=order] {
                max_center -= step;
            }
            let center = ((nlsf[at - 1] + nlsf[at] + 1) >> 1).clamp(min_center, max_center);
            nlsf[at - 1] = center - (spacing[at] >> 1);
            nlsf[at] = nlsf[at - 1] + spacing[at];
        }
    }
    // Twenty rounds were not enough: the fallback.
    nlsf.sort_unstable();
    let mut below = 0;
    for (value, &step) in nlsf.iter_mut().zip(spacing) {
        *value = (*value).max(below + step);
        below = *value;
    }
    let mut above = 32_768;
    for (value, &step) in nlsf.iter_mut().zip(&spacing[1..]).rev() {
        *value = (*value).min(above - step);
        above = *value;
    }
}

/// Bandwidth expansion: each coefficient scaled by a power of
/// `chirp_q16` (`silk_bwexpander_32`).
fn bandwidth_expand(a: &mut [i32], chirp_q16: i32) {
    let mut chirp = i64::from(chirp_q16);
    let step = i64::from(chirp_q16);
    for value in a.iter_mut() {
        *value = ((i64::from(*value) * chirp) >> 16) as i32;
        chirp = (step * chirp + 32_768) >> 16;
    }
}

/// `(1 << q) / b`, in the fixed point SILK works it out in
/// (`silk_INVERSE32_varQ`).
pub(crate) fn inverse32_var_q(b: i32, q: i32) -> i32 {
    if b == 0 || b == i32::MIN {
        return i32::MAX;
    }
    let headroom = b.unsigned_abs().leading_zeros() as i32 - 1;
    let normalized = b << headroom;
    let inverse = (i32::MAX >> 2) / (normalized >> 16);
    let mut result = inverse << 16;
    let err_q32 = ((1i32 << 29) - smulwb(normalized, inverse)) << 3;
    result = smlaww(result, err_q32, inverse);
    let shift = 61 - headroom - q;
    if shift <= 0 {
        lshift_sat32(result, -shift)
    } else if shift < 32 {
        result >> shift
    } else {
        0
    }
}

/// `a * (b as i16) >> 16`, rounded down (`silk_SMULWB`).
#[inline]
pub(crate) fn smulwb(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b as i16)) >> 16) as i32
}

/// `a * b >> 16`, rounded down (`silk_SMULWW`).
#[inline]
pub(crate) fn smulww(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 16) as i32
}

/// `a + (b * c >> 16)` (`silk_SMLAWW`).
#[inline]
pub(crate) fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulww(b, c))
}

/// `a << shift`, saturated to 32 bits (`silk_LSHIFT_SAT32`).
#[inline]
pub(crate) fn lshift_sat32(a: i32, shift: i32) -> i32 {
    let limit_low = i32::MIN >> shift;
    let limit_high = i32::MAX >> shift;
    a.clamp(limit_low, limit_high) << shift
}

/// Whether the filter of `a_q12` is stable with an inverse prediction gain
/// of at least 1/10000 (`silk_LPC_inverse_pred_gain`, section 4.2.7.5.8).
/// It steps down through the reflection coefficients as the reference
/// implementation does, with its rounding, and also refuses a filter whose
/// coefficients leave 32 bits on the way.
fn is_stable(a_q12: &[i32]) -> bool {
    const A_LIMIT: i32 = 16_773_022;
    let order = a_q12.len();
    let dc: i32 = a_q12.iter().sum();
    if dc >= 4_096 {
        return false;
    }
    let mut a = [0i32; MAX_ORDER];
    for (slot, &value) in a.iter_mut().zip(a_q12) {
        *slot = value << 12;
    }
    let mut inv_gain_q30: i32 = 1 << 30;
    let round_shift = |value: i64, shift: i32| -> i64 { ((value >> (shift - 1)) + 1) >> 1 };
    for k in (1..order).rev() {
        if a[k] > A_LIMIT || a[k] < -A_LIMIT {
            return false;
        }
        let rc_q31 = -(a[k] << 7);
        let rc_mult1_q30 = (1i32 << 30) - ((i64::from(rc_q31) * i64::from(rc_q31)) >> 32) as i32;
        inv_gain_q30 = (((i64::from(inv_gain_q30) * i64::from(rc_mult1_q30)) >> 32) as i32) << 2;
        if inv_gain_q30 < 107_374 {
            return false;
        }
        let mult2_q = 32 - rc_mult1_q30.unsigned_abs().leading_zeros() as i32;
        let rc_mult2 = inverse32_var_q(rc_mult1_q30, mult2_q + 30);
        for n in 0..(k + 1) >> 1 {
            let tmp1 = a[n];
            let tmp2 = a[k - n - 1];
            let frac = |x: i32| round_shift(i64::from(x) * i64::from(rc_q31), 31) as i32;
            let first = round_shift(
                i64::from(tmp1.saturating_sub(frac(tmp2))) * i64::from(rc_mult2),
                mult2_q,
            );
            if first > i64::from(i32::MAX) || first < i64::from(i32::MIN) {
                return false;
            }
            let second = round_shift(
                i64::from(tmp2.saturating_sub(frac(tmp1))) * i64::from(rc_mult2),
                mult2_q,
            );
            if second > i64::from(i32::MAX) || second < i64::from(i32::MIN) {
                return false;
            }
            a[n] = first as i32;
            a[k - n - 1] = second as i32;
        }
    }
    if a[0] > A_LIMIT || a[0] < -A_LIMIT {
        return false;
    }
    let rc_q31 = -(a[0] << 7);
    let rc_mult1_q30 = (1i32 << 30) - ((i64::from(rc_q31) * i64::from(rc_q31)) >> 32) as i32;
    inv_gain_q30 = (((i64::from(inv_gain_q30) * i64::from(rc_mult1_q30)) >> 32) as i32) << 2;
    inv_gain_q30 >= 107_374
}

/// The coefficients of the LPC filter, in Q12, from the frequencies
/// (`silk_NLSF2A`, sections 4.2.7.5.6 to 4.2.7.5.8).
pub(crate) fn nlsf_to_lpc(nlsf: &[i32], out: &mut [i16]) {
    let order = nlsf.len();
    let ordering: &[usize] = if order == 16 {
        &LSF_ORDERING_WB
    } else {
        &LSF_ORDERING_NB
    };
    let mut c_q17 = [0i64; MAX_ORDER];
    for (k, &value) in nlsf.iter().enumerate() {
        let i = (value >> 8) as usize;
        let f = i64::from(value & 255);
        let cos = i64::from(COSINE[i]);
        let next = i64::from(COSINE[i + 1]);
        c_q17[ordering[k]] = (cos * 256 + (next - cos) * f + 4) >> 3;
    }

    // The symmetric and antisymmetric polynomials, from their roots.
    let half = order / 2;
    let mut p = [0i64; MAX_ORDER / 2 + 2];
    let mut q = [0i64; MAX_ORDER / 2 + 2];
    for (poly, offset) in [(&mut p, 0usize), (&mut q, 1usize)] {
        poly[0] = 1 << 16;
        poly[1] = -c_q17[offset];
        for k in 1..half {
            let c = c_q17[2 * k + offset];
            poly[k + 1] = 2 * poly[k - 1] - ((c * poly[k] + 32_768) >> 16);
            for j in (2..=k).rev() {
                poly[j] += poly[j - 2] - ((c * poly[j - 1] + 32_768) >> 16);
            }
            poly[1] -= c;
        }
    }
    let mut a32 = [0i32; MAX_ORDER];
    for k in 0..half {
        let sum_p = p[k + 1] + p[k];
        let diff_q = q[k + 1] - q[k];
        a32[k] = (-diff_q - sum_p) as i32;
        a32[order - k - 1] = (diff_q - sum_p) as i32;
    }
    let a32 = &mut a32[..order];

    // Keep the coefficients within 16 bits (section 4.2.7.5.7).
    let mut saturate = true;
    for _ in 0..10 {
        let mut max = 0i64;
        let mut at = 0;
        for (k, &value) in a32.iter().enumerate() {
            let magnitude = i64::from(value).abs();
            if magnitude > max {
                max = magnitude;
                at = k;
            }
        }
        let max_q12 = ((max + 16) >> 5).min(163_838);
        if max_q12 <= 32_767 {
            saturate = false;
            break;
        }
        let chirp = 65_470 - (((max_q12 - 32_767) << 14) / ((max_q12 * (at as i64 + 1)) >> 2));
        bandwidth_expand(a32, chirp as i32);
    }
    if saturate {
        for value in a32.iter_mut() {
            let q12 = ((*value + 16) >> 5).clamp(-32_768, 32_767);
            *value = q12 << 5;
        }
    }

    // And keep the filter stable (section 4.2.7.5.8).
    let mut a_q12 = [0i32; MAX_ORDER];
    for round in 0..16 {
        for (slot, &value) in a_q12.iter_mut().zip(a32.iter()) {
            *slot = (value + 16) >> 5;
        }
        if is_stable(&a_q12[..order]) {
            break;
        }
        bandwidth_expand(a32, 65_536 - (2 << round));
        if round == 15 {
            for (slot, &value) in a_q12.iter_mut().zip(a32.iter()) {
                *slot = (value + 16) >> 5;
            }
        }
    }
    for (slot, &value) in out.iter_mut().zip(&a_q12[..order]) {
        *slot = value as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evenly_spread_frequencies_give_a_flat_stable_filter() {
        // Frequencies evenly spread over the band are the roots of a
        // filter of no prediction at all.
        for order in [10, 16] {
            let nlsf: Vec<i32> = (1..=order)
                .map(|k| (k * 32_768 / (order + 1)) as i32)
                .collect();
            let mut a = [0i16; 16];
            nlsf_to_lpc(&nlsf, &mut a[..order]);
            for &value in &a[..order] {
                assert!(value.abs() < 64, "{order}: {a:?}");
            }
        }
    }

    #[test]
    fn crowded_frequencies_are_spread_apart() {
        let mut nlsf = [100, 100, 100, 100, 100, 100, 100, 100, 100, 32_767];
        stabilize(&mut nlsf, &LSF_MIN_SPACING_NB);
        let mut below = 0;
        for (k, &value) in nlsf.iter().enumerate() {
            assert!(value - below >= LSF_MIN_SPACING_NB[k], "{nlsf:?}");
            below = value;
        }
        assert!(32_768 - below >= LSF_MIN_SPACING_NB[10]);
    }
}
