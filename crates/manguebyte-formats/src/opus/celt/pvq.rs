//! The pyramid vector quantiser of CELT: how many vectors of `n` integers
//! have `k` pulses, how one is read back from its index (RFC 6716, section
//! 4.3.4.2), and the table of what each number of pulses costs, which the
//! bit allocation works from (section 4.3.4.1).

use std::sync::OnceLock;

use super::tables::{BANDS, EBANDS, MAX_LM};
use crate::opus::range::{BITRES, RangeDecoder, ilog};

/// The widest band any frame splits into: 22 bins at 20 ms, eight short
/// blocks of them, with the band of a 40 ms frame the cache also covers.
const MAX_N: usize = 176;
/// The most pulses a band is ever given.
const MAX_K: usize = 128;
/// The pseudo-pulse counts the cache is indexed by go up to this.
pub(crate) const MAX_PSEUDO: usize = 40;
const LOG_MAX_PSEUDO: usize = 6;

/// `U(n, k)`, for `n` up to [`MAX_N`] and `k` up to `MAX_K + 1`, saturated
/// at `u32::MAX`: the number of vectors of `n` integers with `k - 1`
/// pulses whose first is positive, and more, as section 4.3.4.2 defines
/// it. `V(n, k) = U(n, k) + U(n, k + 1)` vectors have `k` pulses.
struct UTable {
    values: Vec<u32>,
}

impl UTable {
    const ROW: usize = MAX_K + 2;

    fn build() -> Self {
        let mut values = vec![0u64; (MAX_N + 1) * Self::ROW];
        values[0] = 1;
        for n in 1..=MAX_N {
            for k in 1..Self::ROW {
                let sum = values[(n - 1) * Self::ROW + k]
                    + values[n * Self::ROW + k - 1]
                    + values[(n - 1) * Self::ROW + k - 1];
                values[n * Self::ROW + k] = sum.min(u64::from(u32::MAX));
            }
        }
        Self {
            values: values.into_iter().map(|value| value as u32).collect(),
        }
    }

    #[inline]
    fn get(&self, n: usize, k: usize) -> u32 {
        self.values[n * Self::ROW + k]
    }
}

fn u_table() -> &'static UTable {
    static TABLE: OnceLock<UTable> = OnceLock::new();
    TABLE.get_or_init(UTable::build)
}

/// `V(n, k)`, the number of vectors of `n` integers whose magnitudes sum
/// to `k`, saturated at `u32::MAX`.
pub(crate) fn pvq_v(n: usize, k: usize) -> u32 {
    let table = u_table();
    table.get(n, k).saturating_add(table.get(n, k + 1))
}

/// Whether `V(n, k)` fits in 32 bits, so that the index of a vector can be
/// coded (`fits_in32` of the reference implementation).
fn fits_in32(n: usize, k: usize) -> bool {
    const MAX_N_FOR_K: [u16; 15] = [
        32_767, 32_767, 32_767, 1_476, 283, 109, 60, 40, 29, 24, 20, 18, 16, 14, 13,
    ];
    const MAX_K_FOR_N: [u16; 15] = [
        32_767, 32_767, 32_767, 32_767, 1_172, 238, 95, 53, 36, 27, 22, 18, 16, 15, 13,
    ];
    if n >= 14 {
        k < 14 && n <= usize::from(MAX_N_FOR_K[k])
    } else {
        k <= usize::from(MAX_K_FOR_N[n])
    }
}

/// The number of pulses a pseudo-pulse count stands for: up to eight one
/// for one, then eight steps for each doubling.
pub(crate) fn get_pulses(pseudo: usize) -> usize {
    if pseudo < 8 {
        pseudo
    } else {
        (8 + (pseudo & 7)) << ((pseudo >> 3) - 1)
    }
}

/// `log2(value)` with `frac` fractional bits, rounded up (`log2_frac`).
pub(crate) fn log2_frac(mut value: u32, mut frac: u32) -> i32 {
    let mut l = ilog(value) as i32;
    if value & value.wrapping_sub(1) != 0 {
        if l > 16 {
            value = ((value - 1) >> (l - 16)) + 1;
        } else {
            value <<= 16 - l;
        }
        l = (l - 1) << frac;
        loop {
            let b = (value >> 16) as i32;
            l += b << frac;
            value = (value + b as u32) >> b;
            value = (value * value + 0x7FFF) >> 15;
            if frac == 0 {
                break;
            }
            frac -= 1;
        }
        l + i32::from(value > 0x8000)
    } else {
        (l - 1) << frac
    }
}

/// What it costs to code each number of pseudo-pulses in each band size:
/// the `cache` of the reference implementation, built here as it builds
/// its own (`compute_pulse_cache`).
pub(crate) struct PulseCache {
    /// For each `LM + 1` from 0 to `MAX_LM + 1` and each band, where its
    /// entry starts in `bits`, or -1 for a band of no bins.
    index: [[i16; BANDS]; MAX_LM + 2],
    /// Each entry: the largest pseudo-pulse count, then the cost in
    /// eighths of a bit, less one, of each count from 1.
    bits: Vec<u8>,
}

impl PulseCache {
    #[allow(clippy::needless_range_loop)]
    fn build() -> Self {
        let mut index = [[-1i16; BANDS]; MAX_LM + 2];
        let mut entries: Vec<(usize, usize, usize)> = Vec::new();
        let mut size = 0usize;
        for lm in 0..=MAX_LM + 1 {
            for band in 0..BANDS {
                let n = ((EBANDS[band + 1] - EBANDS[band]) << lm) >> 1;
                // A band of a size seen before shares its entry.
                let mut found = None;
                'search: for earlier in 0..=lm {
                    for other in 0..BANDS {
                        if earlier == lm && other >= band {
                            break;
                        }
                        if n == ((EBANDS[other + 1] - EBANDS[other]) << earlier) >> 1 {
                            found = Some(index[earlier][other]);
                            break 'search;
                        }
                    }
                }
                if let Some(at) = found {
                    index[lm][band] = at;
                } else if n != 0 {
                    let mut k = 0;
                    while k < MAX_PSEUDO && fits_in32(n, get_pulses(k + 1)) {
                        k += 1;
                    }
                    index[lm][band] = size as i16;
                    entries.push((n, k, size));
                    size += k + 1;
                }
            }
        }
        let mut bits = vec![0u8; size];
        for &(n, k, at) in &entries {
            bits[at] = k as u8;
            for pseudo in 1..=k {
                let cost = log2_frac(pvq_v(n, get_pulses(pseudo)), BITRES);
                bits[at + pseudo] = (cost - 1) as u8;
            }
        }
        Self { index, bits }
    }

    /// The entry of `band` at `lm`, which runs from -1 for half a short
    /// block.
    #[inline]
    pub(crate) fn entry(&self, band: usize, lm: i32) -> &[u8] {
        let at = self.index[(lm + 1) as usize][band];
        &self.bits[at.max(0) as usize..]
    }
}

pub(crate) fn pulse_cache() -> &'static PulseCache {
    static CACHE: OnceLock<PulseCache> = OnceLock::new();
    CACHE.get_or_init(PulseCache::build)
}

/// The pseudo-pulse count whose cost is nearest to `bits` eighths of a bit
/// (`bits2pulses`).
pub(crate) fn bits_to_pulses(band: usize, lm: i32, bits: i32) -> usize {
    let cache = pulse_cache().entry(band, lm);
    let mut low = 0usize;
    let mut high = usize::from(cache[0]);
    let bits = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let middle = (low + high + 1) >> 1;
        if i32::from(cache[middle]) >= bits {
            high = middle;
        } else {
            low = middle;
        }
    }
    let below = if low == 0 { -1 } else { i32::from(cache[low]) };
    if bits - below <= i32::from(cache[high]) - bits {
        low
    } else {
        high
    }
}

/// What `pseudo` pulses cost in `band`, in eighths of a bit
/// (`pulses2bits`).
pub(crate) fn pulses_to_bits(band: usize, lm: i32, pseudo: usize) -> i32 {
    if pseudo == 0 {
        0
    } else {
        i32::from(pulse_cache().entry(band, lm)[pseudo]) + 1
    }
}

/// Reads the vector of `n` integers with `k` pulses whose index the frame
/// codes into `out`, and returns the sum of their squares (`decode_pulses`
/// and `cwrsi`).
pub(crate) fn decode_pulses(out: &mut [i32], n: usize, k: usize, ec: &mut RangeDecoder<'_>) -> f32 {
    let total = pvq_v(n, k);
    let index = ec.uint(total.max(2));
    unrank(out, n, k, index)
}

fn unrank(out: &mut [i32], mut n: usize, mut k: usize, mut i: u32) -> f32 {
    let u = u_table();
    let mut energy = 0i64;
    let mut at = 0;
    let mut push = |value: i32, at: &mut usize| {
        out[*at] = value;
        *at += 1;
        energy += i64::from(value) * i64::from(value);
    };
    while n > 2 {
        if k >= n {
            // Many pulses: the row of U for n.
            let mut p = u.get(n, k + 1);
            let negative = i >= p;
            if negative {
                i -= p;
            }
            let start = k;
            let q = u.get(n, n);
            if q > i {
                k = n;
                loop {
                    k -= 1;
                    p = u.get(n, k);
                    if p <= i {
                        break;
                    }
                }
            } else {
                p = u.get(n, k);
                while p > i {
                    k -= 1;
                    p = u.get(n, k);
                }
            }
            i -= p;
            let magnitude = (start - k) as i32;
            push(if negative { -magnitude } else { magnitude }, &mut at);
        } else {
            // Many dimensions.
            let p = u.get(n, k);
            let q = u.get(n, k + 1);
            if p <= i && i < q {
                i -= p;
                push(0, &mut at);
            } else {
                let negative = i >= q;
                if negative {
                    i -= q;
                }
                let start = k;
                let mut p;
                loop {
                    k -= 1;
                    p = u.get(n, k);
                    if p <= i {
                        break;
                    }
                }
                i -= p;
                let magnitude = (start - k) as i32;
                push(if negative { -magnitude } else { magnitude }, &mut at);
            }
        }
        n -= 1;
    }
    // n == 2
    let p = (2 * k + 1) as u32;
    let negative = i >= p;
    if negative {
        i -= p;
    }
    let start = k;
    k = ((i + 1) >> 1) as usize;
    if k > 0 {
        i -= (2 * k - 1) as u32;
    }
    let magnitude = (start - k) as i32;
    push(if negative { -magnitude } else { magnitude }, &mut at);
    // n == 1
    let magnitude = k as i32;
    push(if i != 0 { -magnitude } else { magnitude }, &mut at);
    energy as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counts_are_those_of_the_pyramid() {
        // |a| + |b| = 2: (+-2, 0), (0, +-2), (+-1, +-1).
        assert_eq!(pvq_v(2, 2), 8);
        assert_eq!(pvq_v(1, 5), 2);
        assert_eq!(pvq_v(3, 1), 6);
        // Beyond 32 bits, exactly where the reference says.
        assert!(pvq_v(4, 1_172) < u32::MAX);
        assert!(fits_in32(4, 1_172) && !fits_in32(4, 1_173));
        assert!(fits_in32(14, 13) && !fits_in32(14, 14));
    }

    #[test]
    fn every_index_gives_a_distinct_vector_of_k_pulses() {
        for (n, k) in [(2, 1), (2, 3), (3, 2), (4, 4), (5, 7), (8, 3), (3, 9)] {
            let total = pvq_v(n, k);
            let mut seen = std::collections::HashSet::new();
            let mut out = vec![0i32; n];
            for index in 0..total {
                let energy = unrank(&mut out, n, k, index);
                let pulses: i32 = out.iter().map(|value| value.abs()).sum();
                assert_eq!(pulses as usize, k, "n {n} k {k} index {index}: {out:?}");
                let squares: i32 = out.iter().map(|value| value * value).sum();
                assert_eq!(energy, squares as f32);
                assert!(seen.insert(out.clone()), "n {n} k {k}: {out:?} twice");
            }
        }
    }

    #[test]
    fn log2_frac_rounds_up() {
        assert_eq!(log2_frac(1, 3), 0);
        assert_eq!(log2_frac(2, 3), 8);
        assert_eq!(log2_frac(3, 3), 13);
        assert_eq!(log2_frac(6, 3), 21);
        assert_eq!(log2_frac(22, 3), 36);
        for (n, &expected) in crate::opus::celt::tables::LOG2_FRAC.iter().enumerate() {
            assert_eq!(log2_frac(n as u32 + 1, 3), i32::from(expected), "{n}");
        }
    }

    #[test]
    fn the_cache_has_the_shape_of_the_reference() {
        let cache = pulse_cache();
        // The reference's cache holds 392 entries for this mode.
        assert_eq!(cache.bits.len(), 392);
        // A band of one bin at half a short block has no entry; one of two
        // bins at 2.5 ms takes forty pseudo-pulses.
        assert_eq!(cache.entry(8, 0)[0], 40);
    }
}
