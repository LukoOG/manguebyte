//! Reading a packet bit by bit, as Vorbis packs it.

/// Reads bits from a packet, least significant bit of each byte first, as
/// section 2 of the Vorbis I specification packs them.
///
/// Running out of bits is not an error at this level: a read past the end
/// of the packet returns `None`, and what that means is up to the caller.
/// In a header it is an error; in an audio packet the specification makes
/// it a normal way for a packet to end early.
pub(super) struct BitReader<'a> {
    bytes: &'a [u8],
    /// Position in bits from the start of `bytes`.
    position: usize,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// The next `count` bits, at most 32, without taking them. Bits past
    /// the end of the packet read as zero.
    pub(super) fn peek(&self, count: u32) -> u32 {
        debug_assert!(count <= 32);
        let byte = self.position / 8;
        let mut word = [0u8; 8];
        if let Some(rest) = self.bytes.get(byte..) {
            let have = rest.len().min(8);
            word[..have].copy_from_slice(&rest[..have]);
        }
        // At least 57 of these bits are the packet's.
        let value = u64::from_le_bytes(word) >> (self.position % 8);
        (value & ((1u64 << count) - 1)) as u32
    }

    /// Takes `count` bits. Returns `false`, and moves to the end, if fewer
    /// are left.
    pub(super) fn consume(&mut self, count: u32) -> bool {
        let end = self.position + count as usize;
        if end > self.bytes.len() * 8 {
            self.position = self.bytes.len() * 8;
            return false;
        }
        self.position = end;
        true
    }

    /// Reads `count` bits, at most 32, as an unsigned number.
    pub(super) fn read(&mut self, count: u32) -> Option<u32> {
        let value = self.peek(count);
        self.consume(count).then_some(value)
    }

    /// Reads one bit.
    pub(super) fn flag(&mut self) -> Option<bool> {
        self.read(1).map(|bit| bit == 1)
    }

    /// How many bits are left.
    pub(super) fn bits_left(&self) -> usize {
        (self.bytes.len() * 8).saturating_sub(self.position)
    }

    /// Whether every bit has been read.
    #[cfg(test)]
    pub(super) fn at_end(&self) -> bool {
        self.position >= self.bytes.len() * 8
    }
}

/// The number of bits needed to write `value`: the position of its highest
/// set bit, counting from one, and zero for zero (`ilog` in section 9.2.1).
pub(super) fn ilog(value: u32) -> u32 {
    32 - value.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_are_read_from_the_bottom_of_each_byte() {
        // The example of section 2.1.4 of the specification: the values
        // 12 in 4 bits, -1 in 3, 17 in 7 and 6969 in 13 make these bytes.
        let bytes = [0b1111_1100, 0b0100_1000, 0b1100_1110, 0b0000_0110];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read(4), Some(12));
        assert_eq!(reader.read(3), Some(0b111));
        assert_eq!(reader.read(7), Some(17));
        assert_eq!(reader.read(13), Some(6969));
        assert!(!reader.at_end());
        assert_eq!(reader.read(5), Some(0));
        assert!(reader.at_end());
        assert_eq!(reader.read(1), None);
        assert_eq!(reader.peek(32), 0);
    }

    #[test]
    fn a_read_past_the_end_fails_and_leaves_nothing_to_read() {
        let mut reader = BitReader::new(&[0xFF, 0x01]);
        assert_eq!(reader.read(9), Some(0x1FF));
        assert_eq!(reader.read(8), None);
        assert!(reader.at_end());
        let mut reader = BitReader::new(&[0xA5; 8]);
        assert_eq!(reader.read(32), Some(0xA5A5_A5A5));
        assert_eq!(reader.read(32), Some(0xA5A5_A5A5));
    }

    #[test]
    fn ilog_counts_bits() {
        for (value, bits) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 3),
            (7, 3),
            (u32::MAX, 32),
        ] {
            assert_eq!(ilog(value), bits, "{value}");
        }
    }
}
