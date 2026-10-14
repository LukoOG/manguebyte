//! Writing bits most significant first, as every FLAC field is stored.

/// Appends bits to a byte vector, most significant bit first.
///
/// The vector is given its capacity when the writer is made. The encoder
/// sizes it for the largest frame it can produce, so writing a frame never
/// allocates.
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits not yet moved to `bytes`, in the low `held` bits.
    pending: u64,
    /// Fewer than eight between calls.
    held: u32,
}

impl BitWriter {
    pub(super) fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
            pending: 0,
            held: 0,
        }
    }

    /// Empties the writer, keeping its storage.
    pub(super) fn clear(&mut self) {
        self.bytes.clear();
        self.pending = 0;
        self.held = 0;
    }

    /// Bits written so far.
    #[cfg(test)]
    pub(super) fn bit_len(&self) -> u64 {
        self.bytes.len() as u64 * 8 + u64::from(self.held)
    }

    /// Whether the bits written so far fill a whole number of bytes.
    pub(super) fn is_aligned(&self) -> bool {
        self.held == 0
    }

    /// The bytes written so far. Bits short of a whole byte are not
    /// included: call [`align`](Self::align) first.
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Writes the low `count` bits of `value`, up to 32 at a time.
    fn write_short(&mut self, value: u64, count: u32) {
        debug_assert!(count <= 32);
        if count == 0 {
            return;
        }
        // At most 7 bits are held and at most 32 added, so nothing is lost
        // off the top of the 64.
        self.pending = (self.pending << count) | (value & ((1u64 << count) - 1));
        self.held += count;
        while self.held >= 8 {
            self.held -= 8;
            self.bytes.push((self.pending >> self.held) as u8);
        }
        self.pending &= (1u64 << self.held) - 1;
    }

    /// Writes the low `count` bits of `value`, `count` up to 64.
    pub(super) fn write(&mut self, value: u64, count: u32) {
        debug_assert!(count <= 64);
        if count > 32 {
            self.write_short(value >> 32, count - 32);
            self.write_short(value, 32);
        } else {
            self.write_short(value, count);
        }
    }

    /// Writes `value` as a two's-complement number of `count` bits. The
    /// caller makes sure it fits.
    pub(super) fn write_signed(&mut self, value: i64, count: u32) {
        self.write(value as u64, count);
    }

    /// Writes `count` zero bits.
    pub(super) fn write_zeros(&mut self, mut count: u64) {
        while count > 0 {
            let step = count.min(32) as u32;
            self.write_short(0, step);
            count -= u64::from(step);
        }
    }

    /// Writes `zeros` zero bits and a one: the unary code FLAC uses.
    pub(super) fn write_unary(&mut self, zeros: u64) {
        self.write_zeros(zeros);
        self.write_short(1, 1);
    }

    /// Writes `folded` as a Rice code with parameter `parameter`: the bits
    /// above the parameter in unary, then the parameter's low bits.
    pub(super) fn write_rice(&mut self, folded: u64, parameter: u32) {
        let quotient = folded >> parameter;
        if quotient < 24 && parameter <= 7 {
            // The common case in one step: the quotient's zeros, its end
            // bit and the low bits together take at most 32 bits.
            let low = folded & ((1u64 << parameter) - 1);
            self.write_short((1u64 << parameter) | low, quotient as u32 + 1 + parameter);
        } else {
            self.write_unary(quotient);
            self.write(folded, parameter);
        }
    }

    /// Fills the rest of the current byte with zero bits.
    pub(super) fn align(&mut self) {
        if self.held > 0 {
            self.write_short(0, 8 - self.held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_packed_most_significant_bit_first() {
        let mut writer = BitWriter::with_capacity(16);
        writer.write(0b101, 3);
        writer.write_unary(8);
        assert!(!writer.is_aligned());
        writer.write_signed(-1, 4);
        assert!(writer.is_aligned());
        writer.write_signed(-1, 8);
        assert_eq!(writer.bytes(), &[0b1010_0000, 0b0001_1111, 0xFF]);
        assert_eq!(writer.bit_len(), 24);

        writer.clear();
        writer.write(0x1_2345_6789, 33);
        writer.align();
        assert_eq!(writer.bit_len(), 40);
        assert_eq!(writer.bytes(), &[0x91, 0xA2, 0xB3, 0xC4, 0x80]);
    }

    #[test]
    fn rice_codes_take_the_same_bits_whichever_way_they_are_written() {
        for parameter in 0..=30 {
            for folded in [0u64, 1, 2, 5, 23, 24, 25, 100, 1000, 65_535, 1 << 31] {
                if folded >> parameter > 5_000 {
                    continue;
                }
                let mut fast = BitWriter::with_capacity(1024);
                fast.write_rice(folded, parameter);
                let mut slow = BitWriter::with_capacity(1024);
                slow.write_unary(folded >> parameter);
                slow.write(folded, parameter);
                assert_eq!(fast.bit_len(), slow.bit_len());
                fast.align();
                slow.align();
                assert_eq!(fast.bytes(), slow.bytes(), "{folded} with {parameter}");
            }
        }
    }
}
