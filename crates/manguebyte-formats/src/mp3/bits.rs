//! Reading bits as MPEG audio packs them: most significant bit first.

/// Reads bits from a byte slice, most significant first, up to a limit.
///
/// Reading past the limit fails rather than reading on: in layer III the
/// limit is the end of a granule's data, and data past it belongs to the
/// next granule.
#[derive(Debug, Clone)]
pub(super) struct BitReader<'a> {
    bytes: &'a [u8],
    /// Position in bits from the start of `bytes`.
    position: usize,
    /// The bit no read may reach.
    limit: usize,
}

impl<'a> BitReader<'a> {
    /// A reader from bit `position` up to bit `limit` of `bytes`, or up to
    /// their end if that comes first.
    pub(super) fn new(bytes: &'a [u8], position: usize, limit: usize) -> Self {
        Self {
            bytes,
            position,
            limit: limit.min(bytes.len() * 8),
        }
    }

    /// The bit the next read starts at.
    pub(super) fn position(&self) -> usize {
        self.position
    }

    /// Bits left before the limit.
    pub(super) fn left(&self) -> usize {
        self.limit.saturating_sub(self.position)
    }

    /// Reads one bit.
    #[inline]
    pub(super) fn bit(&mut self) -> Option<u32> {
        if self.position >= self.limit {
            return None;
        }
        let byte = self.bytes[self.position / 8];
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;
        Some(u32::from(bit))
    }

    /// Reads `count` bits, at most 32, as an unsigned number.
    pub(super) fn read(&mut self, count: u32) -> Option<u32> {
        if self.left() < count as usize {
            return None;
        }
        let mut value = 0u32;
        let mut left = count;
        while left > 0 {
            let byte = u32::from(self.bytes[self.position / 8]);
            let offset = (self.position % 8) as u32;
            let take = left.min(8 - offset);
            let chunk = (byte >> (8 - offset - take)) & ((1 << take) - 1);
            value = if take == 32 {
                chunk
            } else {
                (value << take) | chunk
            };
            self.position += take as usize;
            left -= take;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_are_read_from_the_top_and_stop_at_the_limit() {
        let bytes = [0b1011_0011, 0xF0, 0x0F];
        let mut bits = BitReader::new(&bytes, 0, 20);
        assert_eq!(bits.read(3), Some(0b101));
        assert_eq!(bits.bit(), Some(1));
        assert_eq!(bits.read(8), Some(0b0011_1111));
        assert_eq!(bits.left(), 8);
        assert_eq!(bits.read(9), None);
        assert_eq!(bits.read(8), Some(0));
        assert_eq!(bits.bit(), None);
        let mut bits = BitReader::new(&bytes, 4, 100);
        assert_eq!(bits.read(20), Some(0x3F00F));
        assert_eq!(bits.position(), 24);
    }
}
