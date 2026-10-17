//! The range decoder of RFC 6716, section 4.1, which every symbol of an
//! Opus frame is read through, and the raw bits that CELT reads from the
//! end of the frame (section 4.1.4).
//!
//! The decoder keeps the 32-bit state of the RFC: `rng`, the size of the
//! current range, and `val`, the distance from the top of the range to the
//! code value, both normalised so that `rng` stays above 2^23. Bytes past
//! the end of the frame read as zero, as the RFC has it, so that a damaged
//! or truncated frame decodes to something without reading out of bounds;
//! [`RangeDecoder::tell`] says how far a frame was read, which a caller
//! checks against its length.

/// Bits of a symbol: decoding takes bytes in.
const SYM_BITS: u32 = 8;
/// The top bit of the 32-bit code, less the bit kept for carries.
const CODE_TOP: u32 = 1 << 31;
/// The range is renormalised whenever it falls to this or below.
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
/// Bits of the first byte that go into the first `val`.
const CODE_EXTRA: u32 = 7;
/// The resolution in bits of the fractional bit counts `tell_frac` gives.
pub(crate) const BITRES: u32 = 3;

/// The number of bits needed to write `value`, zero for zero (`EC_ILOG`).
#[inline]
pub(crate) fn ilog(value: u32) -> u32 {
    32 - value.leading_zeros()
}

/// Decodes the symbols of one frame.
#[derive(Debug, Clone)]
pub(crate) struct RangeDecoder<'a> {
    data: &'a [u8],
    /// Bytes of `data` the range coder may read; the raw bits are read
    /// backwards from this end.
    storage: usize,
    /// The next byte the range coder reads.
    offset: usize,
    /// Bytes taken from the end for raw bits.
    end_offset: usize,
    /// Raw bits read from the end but not yet used, and how many.
    end_window: u32,
    end_bits: u32,
    /// Bits read so far, counted as the RFC counts them for `tell`.
    total_bits: u32,
    rng: u32,
    val: u32,
    /// The byte last read, whose low bit belongs to the next `val`.
    rem: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Starts decoding `data` (section 4.1.1).
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut decoder = Self {
            data,
            storage: data.len(),
            offset: 0,
            end_offset: 0,
            end_window: 0,
            end_bits: 0,
            total_bits: 32 + 1 - ((32 - CODE_EXTRA) / SYM_BITS) * SYM_BITS,
            rng: 1 << CODE_EXTRA,
            val: 0,
            rem: 0,
        };
        decoder.rem = decoder.read_byte();
        decoder.val = decoder.rng - 1 - (decoder.rem >> (SYM_BITS - CODE_EXTRA));
        decoder.normalize();
        decoder
    }

    fn read_byte(&mut self) -> u32 {
        if self.offset < self.storage {
            let byte = self.data[self.offset];
            self.offset += 1;
            u32::from(byte)
        } else {
            0
        }
    }

    fn read_byte_from_end(&mut self) -> u32 {
        if self.end_offset < self.storage {
            self.end_offset += 1;
            u32::from(self.data[self.storage - self.end_offset])
        } else {
            0
        }
    }

    /// Keeps `rng` above 2^23 by taking in a byte at a time (section
    /// 4.1.2.1).
    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.total_bits += SYM_BITS;
            self.rng <<= SYM_BITS;
            let symbol = self.rem;
            self.rem = self.read_byte();
            let symbol = ((symbol << SYM_BITS) | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS) + (0xFF & !symbol)) & (CODE_TOP - 1);
        }
    }

    /// The first step of decoding a symbol of a distribution summing to
    /// `total`: where the code value falls, in `0..total` (`ec_decode`).
    /// It must be followed by [`update`](Self::update).
    pub(crate) fn decode(&mut self, total: u32) -> u32 {
        let ext = self.rng / total;
        let value = self.val / ext;
        total - (value + 1).min(total)
    }

    /// The same for a total of `2^bits` (`ec_decode_bin`).
    pub(crate) fn decode_bin(&mut self, bits: u32) -> u32 {
        let ext = self.rng >> bits;
        let value = self.val / ext;
        let total = 1u32 << bits;
        total - (value + 1).min(total)
    }

    /// Takes the symbol that covers `low..high` of `total` (`ec_dec_update`).
    pub(crate) fn update(&mut self, low: u32, high: u32, total: u32) {
        let ext = self.rng / total;
        let step = ext * (total - high);
        self.val = self.val.wrapping_sub(step);
        self.rng = if low > 0 {
            ext * (high - low)
        } else {
            self.rng - step
        };
        self.normalize();
    }

    /// Decodes one bit that is set with probability `1 / 2^logp`
    /// (`ec_dec_bit_logp`).
    pub(crate) fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let set = d < s;
        if !set {
            self.val = d - s;
        }
        self.rng = if set { s } else { r - s };
        self.normalize();
        set
    }

    /// Decodes a symbol from an inverse cumulative table in units of
    /// `2^bits` (`ec_dec_icdf`). The table ends with zero.
    pub(crate) fn icdf(&mut self, icdf: &[u8], bits: u32) -> usize {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> bits;
        let mut symbol = 0;
        let mut t;
        loop {
            t = s;
            s = r * u32::from(icdf.get(symbol).copied().unwrap_or(0));
            if d >= s {
                break;
            }
            symbol += 1;
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        symbol
    }

    /// Decodes a whole number in `0..total`, spread evenly (`ec_dec_uint`):
    /// the top eight bits through the range coder and the rest as raw bits.
    pub(crate) fn uint(&mut self, total: u32) -> u32 {
        debug_assert!(total > 1);
        let top = total - 1;
        let bits = ilog(top);
        if bits > 8 {
            let shift = bits - 8;
            let high_total = (top >> shift) + 1;
            let high = self.decode(high_total);
            self.update(high, high + 1, high_total);
            let value = (high << shift) | self.bits(shift);
            // A value past the top is no valid symbol; the reference
            // gives the top, and so does this.
            value.min(top)
        } else {
            let value = self.decode(total);
            self.update(value, value + 1, total);
            value
        }
    }

    /// Reads `count` raw bits, at most 25, from the end of the frame
    /// (`ec_dec_bits`).
    pub(crate) fn bits(&mut self, count: u32) -> u32 {
        debug_assert!(count <= 25);
        let mut window = self.end_window;
        let mut available = self.end_bits;
        if available < count {
            loop {
                window |= self.read_byte_from_end() << available;
                available += SYM_BITS;
                if available > 32 - SYM_BITS {
                    break;
                }
            }
        }
        let value = window & ((1u32 << count) - 1);
        self.end_window = window >> count;
        self.end_bits = available - count;
        self.total_bits += count;
        value
    }

    /// Bits used so far, rounded up (`ec_tell`).
    pub(crate) fn tell(&self) -> i32 {
        self.total_bits as i32 - ilog(self.rng) as i32
    }

    /// Bits used so far in eighths of a bit (`ec_tell_frac`, section
    /// 4.1.6.2).
    pub(crate) fn tell_frac(&self) -> i32 {
        let bits = self.total_bits << BITRES;
        let mut l = ilog(self.rng);
        let mut r = self.rng >> (l - 16);
        for _ in 0..BITRES {
            r = (r * r) >> 15;
            let b = r >> 16;
            l = (l << 1) | b;
            r >>= b;
        }
        bits as i32 - l as i32
    }

    /// The bytes the frame holds for the range coder and the raw bits.
    pub(crate) fn storage(&self) -> usize {
        self.storage
    }

    /// Takes `bytes` off the end of the frame, which then belong to
    /// something else (the redundant CELT frame of a hybrid one).
    pub(crate) fn shrink(&mut self, bytes: usize) {
        self.storage = self.storage.saturating_sub(bytes);
    }

    /// Takes the range as if every bit of the frame had been read: what a
    /// silent CELT frame does.
    pub(crate) fn skip_to_end(&mut self) {
        let tell = self.tell();
        let all = (self.storage * 8) as i32;
        if all > tell {
            self.total_bits += (all - tell) as u32;
        }
    }

    /// The state of the range at the end, which an encoder and a decoder
    /// that agree on every symbol share (`OPUS_GET_FINAL_RANGE`).
    pub(crate) fn range(&self) -> u32 {
        self.rng
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_frame_reads_as_zeros_and_counts_its_bits() {
        let mut decoder = RangeDecoder::new(&[]);
        assert_eq!(decoder.tell(), 1);
        assert!(!decoder.bit_logp(15));
        assert_eq!(decoder.bits(8), 0);
        assert_eq!(decoder.uint(1_000), 0);
        assert!(decoder.tell() > 8);
    }

    #[test]
    fn raw_bits_come_from_the_end_lowest_first() {
        let data = [0u8, 0, 0b1010_0101, 0b1100_0011];
        let mut decoder = RangeDecoder::new(&data);
        assert_eq!(decoder.bits(4), 0b0011);
        assert_eq!(decoder.bits(4), 0b1100);
        assert_eq!(decoder.bits(8), 0b1010_0101);
    }

    #[test]
    fn the_fractional_count_agrees_with_the_whole_one() {
        let data: Vec<u8> = (0..64u32).map(|i| (i * 37 + 11) as u8).collect();
        let mut decoder = RangeDecoder::new(&data);
        for round in 0..200 {
            let whole = decoder.tell();
            let frac = decoder.tell_frac();
            assert!(frac <= whole << BITRES, "{frac} {whole}");
            assert!(frac > (whole - 1) << BITRES, "{frac} {whole}");
            match round % 3 {
                0 => {
                    decoder.bit_logp(2);
                }
                1 => {
                    decoder.icdf(&[200, 100, 0], 8);
                }
                _ => {
                    decoder.uint(37);
                }
            }
        }
    }
}
