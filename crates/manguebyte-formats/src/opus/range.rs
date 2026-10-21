//! The range decoder of RFC 6716, section 4.1, which every symbol of an
//! Opus frame is read through, and the raw bits that CELT reads from the
//! end of the frame (section 4.1.4); the range encoder of section 5.1,
//! which writes them; and [`Coder`], which lets the parts of CELT that the
//! encoder and the decoder share run over either.
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
        tell_frac(self.total_bits, self.rng)
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

/// What the parts of CELT that the encoder and the decoder share code their
/// symbols through: the decoder reads each one and ignores the value it is
/// given, the encoder writes the value it is given. Either way the value
/// coded comes back, so that what follows is the same code for both.
pub(crate) trait Coder {
    /// Whether this writes: where the two differ, the encoder chooses.
    const ENCODING: bool;
    /// Bits used so far in eighths of a bit (`ec_tell_frac`).
    fn tell_frac(&self) -> i32;
    /// A bit that is set with probability `1 / 2^logp`.
    fn code_bit_logp(&mut self, bit: bool, logp: u32) -> bool;
    /// A whole number in `0..total`, spread evenly.
    fn code_uint(&mut self, value: u32, total: u32) -> u32;
    /// `count` raw bits, at most 25.
    fn code_bits(&mut self, value: u32, count: u32) -> u32;
    /// Where in `0..total` the code value falls: for the decoder, to find
    /// the symbol it then takes with [`update`](Self::update). The encoder
    /// gives zero.
    fn freq(&mut self, total: u32) -> u32;
    /// The symbol that covers `low..high` of `total`.
    fn update(&mut self, low: u32, high: u32, total: u32);
}

impl Coder for RangeDecoder<'_> {
    const ENCODING: bool = false;

    fn tell_frac(&self) -> i32 {
        RangeDecoder::tell_frac(self)
    }

    fn code_bit_logp(&mut self, _bit: bool, logp: u32) -> bool {
        self.bit_logp(logp)
    }

    fn code_uint(&mut self, _value: u32, total: u32) -> u32 {
        self.uint(total)
    }

    fn code_bits(&mut self, _value: u32, count: u32) -> u32 {
        self.bits(count)
    }

    fn freq(&mut self, total: u32) -> u32 {
        self.decode(total)
    }

    fn update(&mut self, low: u32, high: u32, total: u32) {
        RangeDecoder::update(self, low, high, total);
    }
}

/// Where the top byte of the code starts, with the bit of a carry above it.
const CODE_SHIFT: u32 = 32 - SYM_BITS - 1;

/// Encodes the symbols of one frame into a frame of a size fixed when it
/// starts (RFC 6716, section 5.1).
///
/// The state is that of the section: `low`, the bottom of the current
/// range, and `rng`, its size, renormalised a byte at a time so that `rng`
/// stays above 2^23. A byte that leaves the top of `low` may still take a
/// carry from below, so the last one is held back (`rem`), with a count of
/// the bytes of all ones after it, which a carry would turn to zeros
/// (`ext`). Raw bits go to the end of the frame, backwards, as the decoder
/// reads them; [`finish`](Self::finish) writes the shortest code that
/// stays inside the final range and puts the two together.
#[derive(Debug, Clone)]
pub(crate) struct RangeEncoder {
    buf: Vec<u8>,
    /// Bytes written from the front, and from the back.
    offset: usize,
    end_offset: usize,
    /// Raw bits not yet written, and how many.
    end_window: u32,
    end_bits: u32,
    /// Bits written so far, counted as the decoder counts them.
    total_bits: u32,
    low: u32,
    rng: u32,
    /// The byte held back for a carry, if any.
    rem: Option<u32>,
    ext: u32,
    /// Set if the symbols did not fit the frame.
    overflow: bool,
}

impl RangeEncoder {
    /// Starts a frame of `storage` bytes.
    pub(crate) fn new(storage: usize) -> Self {
        Self {
            buf: vec![0; storage],
            offset: 0,
            end_offset: 0,
            end_window: 0,
            end_bits: 0,
            total_bits: 32 + 1,
            low: 0,
            rng: CODE_TOP,
            rem: None,
            ext: 0,
            overflow: false,
        }
    }

    fn write_byte(&mut self, byte: u32) {
        if self.offset + self.end_offset >= self.buf.len() {
            self.overflow = true;
        } else {
            self.buf[self.offset] = byte as u8;
            self.offset += 1;
        }
    }

    fn write_byte_at_end(&mut self, byte: u32) {
        if self.offset + self.end_offset >= self.buf.len() {
            self.overflow = true;
        } else {
            self.end_offset += 1;
            let at = self.buf.len() - self.end_offset;
            self.buf[at] = byte as u8;
        }
    }

    /// Hands out the top byte of `low`, nine bits with the carry. A byte of
    /// all ones waits until it is known whether a carry reaches it.
    fn carry_out(&mut self, value: u32) {
        if value == 0xFF {
            self.ext += 1;
            return;
        }
        let carry = value >> SYM_BITS;
        if let Some(rem) = self.rem {
            self.write_byte(rem + carry);
        }
        for _ in 0..self.ext {
            self.write_byte((0xFF + carry) & 0xFF);
        }
        self.ext = 0;
        self.rem = Some(value & 0xFF);
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.carry_out(self.low >> CODE_SHIFT);
            self.low = (self.low << SYM_BITS) & (CODE_TOP - 1);
            self.rng <<= SYM_BITS;
            self.total_bits += SYM_BITS;
        }
    }

    /// Codes the symbol that covers `low..high` of `total` (`ec_encode`).
    pub(crate) fn encode(&mut self, low: u32, high: u32, total: u32) {
        let r = self.rng / total;
        if low > 0 {
            self.low += self.rng - r * (total - low);
            self.rng = r * (high - low);
        } else {
            self.rng -= r * (total - high);
        }
        self.normalize();
    }

    /// The same for a total of `2^bits` (`ec_encode_bin`).
    pub(crate) fn encode_bin(&mut self, low: u32, high: u32, bits: u32) {
        let r = self.rng >> bits;
        if low > 0 {
            self.low += self.rng - r * ((1 << bits) - low);
            self.rng = r * (high - low);
        } else {
            self.rng -= r * ((1 << bits) - high);
        }
        self.normalize();
    }

    /// Codes a bit that is set with probability `1 / 2^logp`; a set bit
    /// takes the top of the range.
    pub(crate) fn bit_logp(&mut self, bit: bool, logp: u32) {
        let s = self.rng >> logp;
        let r = self.rng - s;
        if bit {
            self.low += r;
            self.rng = s;
        } else {
            self.rng = r;
        }
        self.normalize();
    }

    /// Codes `symbol` of an inverse cumulative table in units of `2^bits`.
    pub(crate) fn icdf(&mut self, symbol: usize, icdf: &[u8], bits: u32) {
        let r = self.rng >> bits;
        let below = u32::from(icdf[symbol]);
        if symbol > 0 {
            let above = u32::from(icdf[symbol - 1]);
            self.low += self.rng - r * above;
            self.rng = r * (above - below);
        } else {
            self.rng -= r * below;
        }
        self.normalize();
    }

    /// Codes `value` in `0..total`: the top eight bits through the range
    /// coder, the rest as raw bits.
    pub(crate) fn uint(&mut self, value: u32, total: u32) {
        debug_assert!(total > 1 && value < total);
        let top = total - 1;
        let bits = ilog(top);
        if bits > 8 {
            let shift = bits - 8;
            let high_total = (top >> shift) + 1;
            let high = value >> shift;
            self.encode(high, high + 1, high_total);
            self.bits(value & ((1 << shift) - 1), shift);
        } else {
            self.encode(value, value + 1, total);
        }
    }

    /// Writes `count` raw bits, at most 25, to the end of the frame.
    pub(crate) fn bits(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 25 && value >> count == 0);
        let mut window = self.end_window;
        let mut used = self.end_bits;
        if used + count > 32 {
            while used >= SYM_BITS {
                self.write_byte_at_end(window & 0xFF);
                window >>= SYM_BITS;
                used -= SYM_BITS;
            }
        }
        window |= value << used;
        used += count;
        self.end_window = window;
        self.end_bits = used;
        self.total_bits += count;
    }

    /// Bits used so far, rounded up (`ec_tell`).
    pub(crate) fn tell(&self) -> i32 {
        self.total_bits as i32 - ilog(self.rng) as i32
    }

    /// Bits used so far in eighths of a bit.
    pub(crate) fn tell_frac(&self) -> i32 {
        tell_frac(self.total_bits, self.rng)
    }

    /// The bytes of the frame.
    pub(crate) fn storage(&self) -> usize {
        self.buf.len()
    }

    /// The state of the range, which a decoder that read every symbol ends
    /// the frame with too.
    pub(crate) fn range(&self) -> u32 {
        self.rng
    }

    /// Ends the frame (section 5.1.5): writes the fewest bits that keep
    /// the code inside the final range whatever follows them, flushes the
    /// byte held back, and puts the raw bits at the end, in the free low
    /// bits of the range coder's last byte when the two meet. Returns the
    /// frame, or `None` if the symbols did not fit it.
    pub(crate) fn finish(mut self) -> Option<Vec<u8>> {
        let mut bits = 32 - ilog(self.rng) as i32;
        let mut mask = (CODE_TOP - 1) >> bits;
        let mut end = (self.low + mask) & !mask;
        if (end | mask) >= self.low + self.rng {
            bits += 1;
            mask >>= 1;
            end = (self.low + mask) & !mask;
        }
        while bits > 0 {
            self.carry_out(end >> CODE_SHIFT);
            end = (end << SYM_BITS) & (CODE_TOP - 1);
            bits -= SYM_BITS as i32;
        }
        if self.rem.is_some() || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.end_bits;
        while used >= SYM_BITS {
            self.write_byte_at_end(window & 0xFF);
            window >>= SYM_BITS;
            used -= SYM_BITS;
        }
        if self.overflow {
            return None;
        }
        let storage = self.buf.len();
        self.buf[self.offset..storage - self.end_offset].fill(0);
        if used > 0 {
            if self.end_offset >= storage {
                return None;
            }
            // The low bits the last byte of the range coder leaves free.
            let free = (-bits) as u32;
            if self.offset + self.end_offset >= storage && free < used {
                return None;
            }
            self.buf[storage - self.end_offset - 1] |= window as u8;
        }
        Some(self.buf)
    }
}

impl Coder for RangeEncoder {
    const ENCODING: bool = true;

    fn tell_frac(&self) -> i32 {
        RangeEncoder::tell_frac(self)
    }

    fn code_bit_logp(&mut self, bit: bool, logp: u32) -> bool {
        self.bit_logp(bit, logp);
        bit
    }

    fn code_uint(&mut self, value: u32, total: u32) -> u32 {
        self.uint(value, total);
        value
    }

    fn code_bits(&mut self, value: u32, count: u32) -> u32 {
        self.bits(value, count);
        value
    }

    fn freq(&mut self, _total: u32) -> u32 {
        0
    }

    fn update(&mut self, low: u32, high: u32, total: u32) {
        self.encode(low, high, total);
    }
}

/// `ec_tell_frac` (section 4.1.6.2), for both coders: the bits used, in
/// eighths, from the whole bits counted and the size of the range.
fn tell_frac(total_bits: u32, rng: u32) -> i32 {
    let bits = total_bits << BITRES;
    let mut l = ilog(rng);
    let mut r = rng >> (l - 16);
    for _ in 0..BITRES {
        r = (r * r) >> 15;
        let b = r >> 16;
        l = (l << 1) | b;
        r >>= b;
    }
    bits as i32 - l as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Symbols of every kind, written and read back, with the raw bits
    /// meeting the range coder's in frames filled to the last bit.
    #[test]
    fn what_the_encoder_writes_the_decoder_reads() {
        let mut seed = 0x1234_5678u32;
        let mut next = |below: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % below
        };
        for round in 0..400 {
            let storage = 2 + next(60) as usize;
            let mut symbols = Vec::new();
            let mut encoder = RangeEncoder::new(storage);
            loop {
                let kind = next(5);
                let symbol = match kind {
                    0 => next(2),
                    1 => next(3),
                    2 => {
                        let below = 1 + next(3_000);
                        next(below)
                    }
                    3 => next(1 << 7),
                    _ => next(5),
                };
                let logp = 1 + next(15);
                let total = 2 + next(3_000);
                let count = 1 + next(12);
                let (value, cost) = match kind {
                    0 => (symbol, logp as i32),
                    1 => (symbol, 2),
                    2 => (symbol % total, 13),
                    3 => (symbol, 7),
                    _ => (symbol & ((1 << count) - 1), count as i32),
                };
                if encoder.tell() + cost + 2 > (storage * 8) as i32 {
                    break;
                }
                match kind {
                    0 => encoder.bit_logp(value != 0, logp),
                    1 => encoder.icdf(value as usize, &[200, 100, 0], 8),
                    2 => encoder.uint(value, total),
                    3 => encoder.encode_bin(value, value + 1, 7),
                    _ => encoder.bits(value, count),
                }
                symbols.push((kind, value, logp, total, count));
            }
            let range = encoder.range();
            let tell = encoder.tell();
            let frame = encoder.finish().expect("the symbols fit");
            assert_eq!(frame.len(), storage);
            let mut decoder = RangeDecoder::new(&frame);
            for &(kind, value, logp, total, count) in &symbols {
                let got = match kind {
                    0 => u32::from(decoder.bit_logp(logp)),
                    1 => decoder.icdf(&[200, 100, 0], 8) as u32,
                    2 => decoder.uint(total),
                    3 => {
                        let got = decoder.decode_bin(7);
                        decoder.update(got, got + 1, 128);
                        got
                    }
                    _ => decoder.bits(count),
                };
                assert_eq!(got, value, "round {round}: {symbols:?}");
            }
            assert_eq!(decoder.tell(), tell, "round {round}");
            assert_eq!(decoder.range(), range, "round {round}");
        }
    }

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
