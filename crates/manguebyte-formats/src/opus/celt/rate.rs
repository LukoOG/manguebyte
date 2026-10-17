//! The bit allocation of CELT (RFC 6716, section 4.3.3): how the bits of a
//! frame are shared out between the bands, for their fine energy and their
//! shape, from the static allocation table, the trim and the boosts the
//! frame codes. Encoder and decoder must agree on every bit of it, so it is
//! integer arithmetic throughout, in eighths of a bit, as the reference
//! implementation does it.

use super::pvq::pulse_cache;
use super::tables::{ALLOCATION, BANDS, CAPS, EBANDS, LOG2_FRAC};
use crate::opus::range::{BITRES, RangeDecoder};

const ALLOC_STEPS: u32 = 6;
const FINE_OFFSET: i32 = 21;
pub(crate) const MAX_FINE_BITS: i32 = 8;

/// `log2` of each band's width at 2.5 ms, in eighths of a bit.
pub(crate) fn log_n(band: usize) -> i32 {
    super::pvq::log2_frac((EBANDS[band + 1] - EBANDS[band]) as u32, BITRES)
}

/// The most each band can use, in eighths of a bit (`init_caps`).
pub(crate) fn caps(lm: usize, channels: usize, caps: &mut [i32; BANDS]) {
    for (band, cap) in caps.iter_mut().enumerate() {
        let n = (EBANDS[band + 1] - EBANDS[band]) << lm;
        let stored = i32::from(CAPS[2 * lm + channels - 1][band]);
        *cap = ((stored + 64) * channels as i32 * n as i32) >> 2;
    }
}

/// What the allocation decided.
pub(crate) struct Allocation {
    /// Bands from the first coded one up to this one get bits for their
    /// shape.
    pub(crate) coded_bands: usize,
    /// Bits over the caps, to be given to the bands that follow.
    pub(crate) balance: i32,
    /// The first band coded as intensity stereo.
    pub(crate) intensity: usize,
    pub(crate) dual_stereo: bool,
    /// For each band, the bits of its shape, in eighths of a bit.
    pub(crate) pulses: [i32; BANDS],
    /// Bits of fine energy, per channel.
    pub(crate) fine_quant: [i32; BANDS],
    /// Which bands get the bits left at the end first.
    pub(crate) fine_priority: [i32; BANDS],
}

/// Shares out `total` eighths of a bit between bands `start..end`
/// (`clt_compute_allocation`), reading the skip, intensity and dual stereo
/// decisions from the frame as it goes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_allocation(
    start: usize,
    end: usize,
    offsets: &[i32; BANDS],
    cap: &[i32; BANDS],
    alloc_trim: i32,
    total: i32,
    channels: usize,
    lm: usize,
    ec: &mut RangeDecoder<'_>,
) -> Allocation {
    let c = channels as i32;
    let mut total = total.max(0);
    let mut skip_start = start;
    // A bit to say where skipping bands stops.
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }

    let mut thresh = [0i32; BANDS];
    let mut trim_offset = [0i32; BANDS];
    for band in start..end {
        let width = (EBANDS[band + 1] - EBANDS[band]) as i32;
        // Below this, a band gets no bits for its shape.
        thresh[band] = (c << BITRES).max(((3 * width) << lm << BITRES) >> 4);
        // The tilt the trim gives the allocation.
        trim_offset[band] = (c
            * width
            * (alloc_trim - 5 - lm as i32)
            * (end - band - 1) as i32
            * (1 << (lm as u32 + BITRES)))
            >> 6;
        // A band of one bin gets less, as one coarse value per bin does
        // more for it.
        if (width << lm) == 1 {
            trim_offset[band] -= c << BITRES;
        }
    }

    let band_bits = |quality: usize, band: usize| -> i32 {
        let width = (EBANDS[band + 1] - EBANDS[band]) as i32;
        ((c * width * i32::from(ALLOCATION[quality][band])) << lm) >> 2
    };

    // The highest quality of the table whose allocation fits.
    let mut low = 1usize;
    let mut high = ALLOCATION.len() - 1;
    loop {
        let mut done = false;
        let mut sum = 0;
        let middle = (low + high) >> 1;
        for band in (start..end).rev() {
            let mut bits = band_bits(middle, band);
            if bits > 0 {
                bits = (bits + trim_offset[band]).max(0);
            }
            bits += offsets[band];
            if bits >= thresh[band] || done {
                done = true;
                sum += bits.min(cap[band]);
            } else if bits >= c << BITRES {
                sum += c << BITRES;
            }
        }
        if sum > total {
            if middle == 0 {
                break;
            }
            high = middle - 1;
        } else {
            low = middle + 1;
        }
        if low > high {
            break;
        }
    }
    let high = low;
    let low = low - 1;

    let mut bits1 = [0i32; BANDS];
    let mut bits2 = [0i32; BANDS];
    for band in start..end {
        let mut first = band_bits(low, band);
        let mut second = if high >= ALLOCATION.len() {
            cap[band]
        } else {
            band_bits(high, band)
        };
        if first > 0 {
            first = (first + trim_offset[band]).max(0);
        }
        if second > 0 {
            second = (second + trim_offset[band]).max(0);
        }
        if low > 0 {
            first += offsets[band];
        }
        second += offsets[band];
        if offsets[band] > 0 {
            skip_start = band;
        }
        bits1[band] = first;
        bits2[band] = (second - first).max(0);
    }

    interpolate(
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        skip_rsv,
        intensity_rsv,
        dual_stereo_rsv,
        channels,
        lm,
        ec,
    )
}

/// The second half of the allocation (`interp_bits2pulses`): the step
/// between two rows of the table, the bands skipped, the stereo
/// parameters, and the split of each band's bits between fine energy and
/// shape.
#[allow(clippy::too_many_arguments)]
fn interpolate(
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; BANDS],
    bits2: &[i32; BANDS],
    thresh: &[i32; BANDS],
    cap: &[i32; BANDS],
    mut total: i32,
    skip_rsv: i32,
    mut intensity_rsv: i32,
    mut dual_stereo_rsv: i32,
    channels: usize,
    lm: usize,
    ec: &mut RangeDecoder<'_>,
) -> Allocation {
    let c = channels as i32;
    let alloc_floor = c << BITRES;
    let stereo = i32::from(channels > 1);
    let log_m = (lm as i32) << BITRES;

    let mut low = 0i32;
    let mut high = 1i32 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let middle = (low + high) >> 1;
        let mut sum = 0;
        let mut done = false;
        for band in (start..end).rev() {
            let bits = bits1[band] + ((middle * bits2[band]) >> ALLOC_STEPS);
            if bits >= thresh[band] || done {
                done = true;
                sum += bits.min(cap[band]);
            } else if bits >= alloc_floor {
                sum += alloc_floor;
            }
        }
        if sum > total {
            high = middle;
        } else {
            low = middle;
        }
    }

    let mut bits = [0i32; BANDS];
    let mut sum = 0;
    let mut done = false;
    for band in (start..end).rev() {
        let mut value = bits1[band] + ((low * bits2[band]) >> ALLOC_STEPS);
        if value < thresh[band] && !done {
            value = if value >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        value = value.min(cap[band]);
        bits[band] = value;
        sum += value;
    }

    // Which bands to skip, from the top down.
    let width_to = |band: usize| (EBANDS[band] - EBANDS[start]) as i32;
    let mut coded_bands = end;
    loop {
        let band = coded_bands - 1;
        // Never the first band, nor one dynalloc boosted.
        if band <= skip_start {
            total += skip_rsv;
            break;
        }
        // The bits this band would get from what is left, the bits of the
        // bands above it skipped included.
        let mut left = total - sum;
        let per_coefficient = left / width_to(coded_bands);
        left -= width_to(coded_bands) * per_coefficient;
        let rem = (left - width_to(band)).max(0);
        let band_width = (EBANDS[coded_bands] - EBANDS[band]) as i32;
        let mut band_bits = bits[band] + per_coefficient * band_width + rem;
        // A skip decision is coded only for a band above its threshold;
        // one below it is skipped without a word.
        if band_bits >= thresh[band].max(alloc_floor + (1 << BITRES)) {
            if ec.bit_logp(1) {
                break;
            }
            sum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        // Take back the band's bits.
        sum -= bits[band] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC[band - start]);
        }
        sum += intensity_rsv;
        if band_bits >= alloc_floor {
            // A fine energy bit for each channel.
            sum += alloc_floor;
            bits[band] = alloc_floor;
        } else {
            bits[band] = 0;
        }
        coded_bands -= 1;
    }

    let mut intensity = 0;
    if intensity_rsv > 0 {
        intensity = start + ec.uint((coded_bands + 1 - start) as u32) as usize;
    }
    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = dual_stereo_rsv > 0 && ec.bit_logp(1);

    // What is left goes to the coded bands, evenly per bin, and the rest
    // from the bottom up.
    let mut left = total - sum;
    let per_coefficient = left / width_to(coded_bands);
    left -= width_to(coded_bands) * per_coefficient;
    for band in start..coded_bands {
        bits[band] += per_coefficient * (EBANDS[band + 1] - EBANDS[band]) as i32;
    }
    for band in start..coded_bands {
        let extra = left.min((EBANDS[band + 1] - EBANDS[band]) as i32);
        bits[band] += extra;
        left -= extra;
    }

    let mut fine_quant = [0i32; BANDS];
    let mut fine_priority = [0i32; BANDS];
    let mut balance = 0;
    let mut band = start;
    while band < coded_bands {
        let n0 = (EBANDS[band + 1] - EBANDS[band]) as i32;
        let n = n0 << lm;
        let bit = bits[band] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[band]).max(0);
            bits[band] = bit - excess;
            // The extra degree of freedom of stereo.
            let den = c * n + i32::from(channels == 2 && n > 2 && !dual_stereo && band < intensity);
            let nc_log_n = den * (log_n(band) + log_m);
            // Fine bits are offset by log2(N)/2 + FINE_OFFSET from their
            // fair share of the total.
            let mut offset = (nc_log_n >> 1) - den * FINE_OFFSET;
            // N = 2 is the one size off the curve.
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            // The second and third fine bit cost differently.
            if bits[band] + offset < (den * 2) << BITRES {
                offset += nc_log_n >> 2;
            } else if bits[band] + offset < (den * 3) << BITRES {
                offset += nc_log_n >> 3;
            }
            let rounded = (bits[band] + offset + (den << (BITRES - 1))).max(0);
            let mut fine = (rounded / den) >> BITRES;
            // Never more than the band has.
            if c * fine > (bits[band] >> BITRES) {
                fine = (bits[band] >> stereo) >> BITRES;
            }
            fine = fine.min(MAX_FINE_BITS);
            fine_priority[band] = i32::from(fine * (den << BITRES) >= bits[band] + offset);
            bits[band] -= (c * fine) << BITRES;
            fine_quant[band] = fine;
        } else {
            // A band of one bin: a sign bit, and the rest is fine energy.
            excess = (bit - (c << BITRES)).max(0);
            bits[band] = bit - excess;
            fine_quant[band] = 0;
            fine_priority[band] = 1;
        }
        // Fine energy cannot take part in the rebalancing of the shapes,
        // so it is rebalanced here.
        if excess > 0 {
            let extra_fine =
                (excess >> (stereo + BITRES as i32)).min(MAX_FINE_BITS - fine_quant[band]);
            fine_quant[band] += extra_fine;
            let extra_bits = (extra_fine * c) << BITRES;
            fine_priority[band] = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        balance = excess;
        band += 1;
    }
    // Skipped bands put all their bits into fine energy.
    while band < end {
        fine_quant[band] = (bits[band] >> stereo) >> BITRES;
        bits[band] = 0;
        fine_priority[band] = i32::from(fine_quant[band] < 1);
        band += 1;
    }

    // The reference keeps its cache warm here; nothing else needs it.
    let _ = pulse_cache();
    Allocation {
        coded_bands,
        balance,
        intensity,
        dual_stereo,
        pulses: bits,
        fine_quant,
        fine_priority,
    }
}
