//! The resampler that takes SILK's 8, 12 or 16 kHz up to 48 kHz.
//!
//! Section 4.2.9 of RFC 6716 leaves the resampler to the decoder and fixes
//! only how much delay it may add (Table 54). This one is built as the
//! reference implementation builds its own, so that SILK frames decode to
//! the same samples it gives: a 2x upsampler of two allpass branches
//! (three first-order sections each), then a fractional FIR of eight taps
//! in twelve phases that interpolates the rest, in 16-bit fixed point. Its
//! coefficients are those of the reference implementation (BSD licence).

/// The two branches of the 2x allpass upsampler, in Q16.
const UP2_HQ_0: [i32; 3] = [1_746, 14_986, 39_083 - 65_536];
const UP2_HQ_1: [i32; 3] = [6_854, 25_769, 55_542 - 65_536];

/// Half of each phase of the interpolating filter: the other half is the
/// mirror image of phase `11 - phase`.
const FRAC_FIR_12: [[i32; 4]; 12] = [
    [189, -600, 617, 30_567],
    [117, -159, -1_070, 29_704],
    [52, 221, -2_392, 28_276],
    [-4, 529, -3_350, 26_341],
    [-48, 758, -3_956, 23_973],
    [-80, 905, -4_235, 21_254],
    [-99, 972, -4_222, 18_278],
    [-107, 967, -3_957, 15_143],
    [-103, 896, -3_487, 11_950],
    [-91, 773, -2_865, 8_798],
    [-71, 611, -2_143, 5_784],
    [-46, 425, -1_375, 2_996],
];

/// Taps of the interpolating filter.
const FIR_ORDER: usize = 8;
/// Input samples resampled at a time, at most: 10 ms at 16 kHz. The
/// reference resamples 10 ms at a time, and where a batch ends changes
/// the phase of the interpolator, so the batches are kept.
const MAX_BATCH: usize = 160;

#[inline]
fn smulwb(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b as i16)) >> 16) as i32
}

#[inline]
fn sat16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// `(value + 2^(shift - 1)) >> shift`, as SILK rounds.
#[inline]
fn rshift_round(value: i32, shift: u32) -> i32 {
    ((value >> (shift - 1)) + 1) >> 1
}

/// The state of the resampler for one channel.
#[derive(Debug, Clone)]
pub(crate) struct Resampler {
    iir: [i32; 6],
    fir: [i16; FIR_ORDER],
    /// The last input samples, held back by the delay of Table 54.
    delay_buf: [i16; 16],
    input_delay: usize,
    rate_in_khz: usize,
    inv_ratio_q16: i32,
}

impl Resampler {
    /// A resampler from `rate_in` hertz, 8000, 12000 or 16000, to 48 kHz,
    /// with nothing in it.
    pub(crate) fn new(rate_in: u32) -> Self {
        let rate_out = 48_000i64;
        // The delay of each input rate to 48 kHz, in input samples.
        let input_delay = match rate_in {
            8_000 => 0,
            12_000 => 4,
            _ => 7,
        };
        let mut inv_ratio_q16 = (((i64::from(rate_in) << 15) / rate_out) << 2) as i32;
        while ((i64::from(inv_ratio_q16) * rate_out) >> 16) < i64::from(rate_in) << 1 {
            inv_ratio_q16 += 1;
        }
        Self {
            iir: [0; 6],
            fir: [0; FIR_ORDER],
            delay_buf: [0; 16],
            input_delay,
            rate_in_khz: (rate_in / 1_000) as usize,
            inv_ratio_q16,
        }
    }

    /// Resamples `input`, a whole number of milliseconds, into `out`, which
    /// takes six times, four times or three times as many samples.
    pub(crate) fn run(&mut self, out: &mut [i16], input: &[i16]) {
        let khz = self.rate_in_khz;
        let n = khz - self.input_delay;
        // The first millisecond goes through the delay buffer.
        self.delay_buf[self.input_delay..khz].copy_from_slice(&input[..n]);
        let first = self.delay_buf;
        let written = self.iir_fir(out, &first[..khz]);
        self.iir_fir(
            &mut out[written..],
            &input[n..input.len() - self.input_delay],
        );
        let len = input.len();
        self.delay_buf[..self.input_delay].copy_from_slice(&input[len - self.input_delay..]);
    }

    /// The 2x upsampler and the interpolator over `input`; returns how many
    /// samples were written (`silk_resampler_private_IIR_FIR`).
    fn iir_fir(&mut self, out: &mut [i16], mut input: &[i16]) -> usize {
        let mut written = 0;
        let mut buf = [0i16; 2 * MAX_BATCH + FIR_ORDER];
        buf[..FIR_ORDER].copy_from_slice(&self.fir);
        let step = self.inv_ratio_q16;
        let mut last = 0;
        while !input.is_empty() {
            let count = input.len().min(10 * self.rate_in_khz);
            self.up2_hq(&mut buf[FIR_ORDER..FIR_ORDER + 2 * count], &input[..count]);
            let max_index = (count as i32) << 17;
            let mut index = 0i32;
            while index < max_index {
                let phase = smulwb(index & 0xFFFF, 12) as usize;
                let at = (index >> 16) as usize;
                let taps = &buf[at..at + FIR_ORDER];
                let low = &FRAC_FIR_12[phase];
                let high = &FRAC_FIR_12[11 - phase];
                let mut sum = i32::from(taps[0]).wrapping_mul(low[0]);
                sum = sum.wrapping_add(i32::from(taps[1]) * low[1]);
                sum = sum.wrapping_add(i32::from(taps[2]) * low[2]);
                sum = sum.wrapping_add(i32::from(taps[3]) * low[3]);
                sum = sum.wrapping_add(i32::from(taps[4]) * high[3]);
                sum = sum.wrapping_add(i32::from(taps[5]) * high[2]);
                sum = sum.wrapping_add(i32::from(taps[6]) * high[1]);
                sum = sum.wrapping_add(i32::from(taps[7]) * high[0]);
                out[written] = sat16(rshift_round(sum, 15));
                written += 1;
                index += step;
            }
            input = &input[count..];
            last = count;
            if !input.is_empty() {
                buf.copy_within(2 * count..2 * count + FIR_ORDER, 0);
            }
        }
        if last > 0 {
            self.fir
                .copy_from_slice(&buf[2 * last..2 * last + FIR_ORDER]);
        }
        written
    }

    /// The 2x upsampler of two allpass branches
    /// (`silk_resampler_private_up2_HQ`).
    fn up2_hq(&mut self, out: &mut [i16], input: &[i16]) {
        let s = &mut self.iir;
        for (k, &sample) in input.iter().enumerate() {
            let in32 = i32::from(sample) << 10;
            // The even output: three allpass sections.
            let y = in32 - s[0];
            let x = smulwb(y, UP2_HQ_0[0]);
            let out1 = s[0] + x;
            s[0] = in32 + x;
            let y = out1 - s[1];
            let x = smulwb(y, UP2_HQ_0[1]);
            let out2 = s[1] + x;
            s[1] = out1 + x;
            let y = out2 - s[2];
            let x = y + smulwb(y, UP2_HQ_0[2]);
            let out1 = s[2] + x;
            s[2] = out2 + x;
            out[2 * k] = sat16(rshift_round(out1, 10));
            // The odd output.
            let y = in32 - s[3];
            let x = smulwb(y, UP2_HQ_1[0]);
            let out1 = s[3] + x;
            s[3] = in32 + x;
            let y = out1 - s[4];
            let x = smulwb(y, UP2_HQ_1[1]);
            let out2 = s[4] + x;
            s[4] = out1 + x;
            let y = out2 - s[5];
            let x = y + smulwb(y, UP2_HQ_1[2]);
            let out1 = s[5] + x;
            s[5] = out2 + x;
            out[2 * k + 1] = sat16(rshift_round(out1, 10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steady_level_comes_out_at_the_same_level() {
        for rate in [8_000, 12_000, 16_000] {
            let mut resampler = Resampler::new(rate);
            let khz = (rate / 1_000) as usize;
            let input = vec![10_000i16; 20 * khz];
            let mut out = vec![0i16; 20 * 48];
            for _ in 0..5 {
                resampler.run(&mut out, &input);
            }
            for &sample in &out[out.len() / 2..] {
                assert!((i32::from(sample) - 10_000).abs() < 40, "{rate}: {sample}");
            }
        }
    }
}
