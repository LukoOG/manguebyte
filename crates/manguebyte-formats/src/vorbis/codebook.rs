//! Codebooks: the entropy codes of Vorbis, with the vectors some of them
//! stand for (section 3 of the Vorbis I specification).

use super::bits::{BitReader, ilog};
use super::{Limits, VorbisError};

/// Codewords of up to this many bits are found with one lookup.
const FAST_BITS: u32 = 10;

/// A codebook: a prefix code over its entries, and for a vector codebook
/// the vector each entry stands for.
#[derive(Debug)]
pub(super) struct Codebook {
    /// Values in each vector.
    pub(super) dimensions: usize,
    /// Entry number of each used entry. Decoding gives a position in this
    /// list, which is also where the entry's vector is in `vectors`.
    entries: Vec<u32>,
    /// For each run of `fast_bits` bits, read first bit lowest, the entry
    /// and length of the codeword it starts with, packed as
    /// `position << 6 | length`, or zero if the codeword is longer or there
    /// is none.
    fast: Vec<u32>,
    fast_bits: u32,
    /// Codewords longer than `fast_bits`, by their bits aligned to the top
    /// of a 32-bit number, in rising order.
    slow: Vec<SlowCode>,
    /// A codebook of one used entry needs no bits to tell entries apart.
    single: Option<u32>,
    /// Bits taken by the codeword of the single used entry.
    single_len: u32,
    /// The vectors, `dimensions` values each, in the order of `entries`.
    /// Empty for a codebook without them.
    vectors: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct SlowCode {
    start: u32,
    len: u8,
    position: u32,
}

impl Codebook {
    /// Reads a codebook from the setup header (section 3.2.1).
    pub(super) fn read(bits: &mut BitReader<'_>, limits: &mut Limits) -> Result<Self, VorbisError> {
        let truncated = || VorbisError::Malformed("setup header ends inside a codebook");
        if bits.read(24).ok_or_else(truncated)? != 0x56_4342 {
            return Err(VorbisError::Malformed(
                "codebook does not start with its sync pattern",
            ));
        }
        let dimensions = bits.read(16).ok_or_else(truncated)? as usize;
        let entries = bits.read(24).ok_or_else(truncated)?;
        // A few bits of an ordered codebook can declare millions of
        // entries, so their number is held to a budget for the whole setup.
        if entries > limits.entries_left {
            return Err(VorbisError::Unsupported(
                "codebooks with more entries than this decoder holds",
            ));
        }
        limits.entries_left -= entries;

        // The length of each entry's codeword; zero for an entry not used.
        let mut lengths = vec![0u8; entries as usize];
        let ordered = bits.flag().ok_or_else(truncated)?;
        if ordered {
            let mut length = bits.read(5).ok_or_else(truncated)? + 1;
            let mut entry = 0u32;
            while entry < entries {
                if length > 32 {
                    return Err(VorbisError::Malformed("codeword longer than 32 bits"));
                }
                let count = bits.read(ilog(entries - entry)).ok_or_else(truncated)?;
                if count > entries - entry {
                    return Err(VorbisError::Malformed("more codewords than entries"));
                }
                lengths[entry as usize..(entry + count) as usize].fill(length as u8);
                entry += count;
                length += 1;
            }
        } else {
            let sparse = bits.flag().ok_or_else(truncated)?;
            for length in lengths.iter_mut() {
                if !sparse || bits.flag().ok_or_else(truncated)? {
                    *length = bits.read(5).ok_or_else(truncated)? as u8 + 1;
                }
            }
        }
        let used = lengths.iter().filter(|&&length| length > 0).count() as u32;

        let lookup = bits.read(4).ok_or_else(truncated)?;
        let mut vectors = Vec::new();
        match lookup {
            0 => {}
            1 | 2 => {
                if dimensions == 0 {
                    return Err(VorbisError::Malformed("vector codebook of no dimensions"));
                }
                let minimum = float32_unpack(bits.read(32).ok_or_else(truncated)?);
                let delta = float32_unpack(bits.read(32).ok_or_else(truncated)?);
                let value_bits = bits.read(4).ok_or_else(truncated)? + 1;
                let sequence = bits.flag().ok_or_else(truncated)?;
                let values = if lookup == 1 {
                    lookup1_values(entries, dimensions as u32)
                } else {
                    u64::from(entries) * dimensions as u64
                };
                // Every multiplicand is in the packet, so this is no more
                // than the packet holds.
                if values * u64::from(value_bits) > bits.bits_left() as u64 {
                    return Err(truncated());
                }
                let mut multiplicands = Vec::with_capacity(values as usize);
                for _ in 0..values {
                    multiplicands.push(bits.read(value_bits).ok_or_else(truncated)?);
                }
                let total = u64::from(used) * dimensions as u64;
                if total > u64::from(limits.values_left) {
                    return Err(VorbisError::Unsupported(
                        "codebooks with more values than this decoder holds",
                    ));
                }
                limits.values_left -= total as u32;
                vectors.reserve_exact(total as usize);
                for (entry, _) in lengths
                    .iter()
                    .enumerate()
                    .filter(|(_, length)| **length > 0)
                {
                    let mut last = 0.0f32;
                    let mut divisor = 1u64;
                    for dimension in 0..dimensions {
                        let offset = if lookup == 1 {
                            // A lattice: the entry number, written in base
                            // `values`, picks one multiplicand per dimension.
                            let offset = (entry as u64 / divisor) % values;
                            divisor = divisor.saturating_mul(values);
                            offset as usize
                        } else {
                            entry * dimensions + dimension
                        };
                        let value = multiplicands[offset] as f32 * delta + minimum + last;
                        if sequence {
                            last = value;
                        }
                        vectors.push(value);
                    }
                }
            }
            _ => return Err(VorbisError::Malformed("reserved codebook lookup type")),
        }

        let mut book = Self {
            dimensions,
            entries: Vec::with_capacity(used as usize),
            fast: Vec::new(),
            fast_bits: 0,
            slow: Vec::new(),
            single: None,
            single_len: 0,
            vectors,
        };
        book.build_code(&lengths)?;
        Ok(book)
    }

    /// Gives every used entry its codeword (section 3.2.1): in order of
    /// entry number, each takes the lowest codeword of its length that is
    /// neither taken nor the start of one taken, and then builds the tables
    /// that decoding looks codewords up in.
    fn build_code(&mut self, lengths: &[u8]) -> Result<(), VorbisError> {
        let used: Vec<(u32, u8)> = lengths
            .iter()
            .enumerate()
            .filter(|(_, length)| **length > 0)
            .map(|(entry, &length)| (entry as u32, length))
            .collect();
        self.entries.extend(used.iter().map(|&(entry, _)| entry));
        match used.as_slice() {
            [] => return Ok(()),
            [(_, length)] => {
                self.single = Some(0);
                self.single_len = u32::from(*length);
                return Ok(());
            }
            _ => {}
        }

        // The codewords not taken yet, as whole subtrees of the code tree:
        // each is the start of its range of 32-bit values and its depth,
        // in rising order. The lowest free codeword of a length is at the
        // start of the first subtree at that depth or nearer the root.
        let mut free: Vec<(u64, u8)> = vec![(0, 0)];
        let mut codes = Vec::with_capacity(used.len());
        for &(_, length) in &used {
            let index = free.iter().position(|&(_, depth)| depth <= length).ok_or(
                VorbisError::Malformed("codebook has more codewords than fit"),
            )?;
            let (start, depth) = free.remove(index);
            codes.push((start >> (32 - length)) as u32);
            // What is left of the subtree after the codeword: one subtree
            // at each depth from the codeword's up to the one below.
            for (step, below) in (depth + 1..=length).rev().enumerate() {
                free.insert(index + step, (start + (1u64 << (32 - below)), below));
            }
            if free.len() > 4_096 {
                return Err(VorbisError::Unsupported("codebook too irregular"));
            }
        }

        let longest = u32::from(used.iter().map(|&(_, length)| length).max().unwrap_or(1));
        self.fast_bits = longest.min(FAST_BITS);
        self.fast = vec![0; 1 << self.fast_bits];
        for (position, (&(_, length), &code)) in used.iter().zip(&codes).enumerate() {
            let length = u32::from(length);
            if length <= self.fast_bits {
                // The codeword as it is read: its first bit lowest.
                let reversed = code.reverse_bits() >> (32 - length);
                let packed = (position as u32) << 6 | length;
                for fill in 0..1u32 << (self.fast_bits - length) {
                    self.fast[(reversed | fill << length) as usize] = packed;
                }
            } else {
                self.slow.push(SlowCode {
                    start: ((u64::from(code)) << (32 - length)) as u32,
                    len: length as u8,
                    position: position as u32,
                });
            }
        }
        self.slow.sort_unstable_by_key(|code| code.start);
        Ok(())
    }

    /// Whether the codebook has vectors, so that it can be used where the
    /// specification decodes vectors.
    pub(super) fn has_vectors(&self) -> bool {
        !self.vectors.is_empty() || self.entries.is_empty()
    }

    /// Reads one codeword and returns the position of its entry in the list
    /// of used entries. `None` at the end of the packet, or for bits that
    /// are no codeword, which the specification treats the same way.
    #[inline]
    fn decode(&self, bits: &mut BitReader<'_>) -> Option<u32> {
        if let Some(position) = self.single {
            return bits.consume(self.single_len).then_some(position);
        }
        if self.fast.is_empty() {
            return None;
        }
        let packed = self.fast[bits.peek(self.fast_bits) as usize];
        if packed != 0 {
            return bits.consume(packed & 63).then_some(packed >> 6);
        }
        let value = bits.peek(32).reverse_bits();
        let after = self.slow.partition_point(|code| code.start <= value);
        let code = self.slow.get(after.checked_sub(1)?)?;
        let span = 1u64 << (32 - u32::from(code.len));
        if u64::from(value - code.start) >= span {
            return None;
        }
        bits.consume(u32::from(code.len)).then_some(code.position)
    }

    /// Reads a codeword and returns its entry number: a scalar.
    #[inline]
    pub(super) fn decode_scalar(&self, bits: &mut BitReader<'_>) -> Option<u32> {
        self.decode(bits)
            .map(|position| self.entries[position as usize])
    }

    /// Reads a codeword and returns its entry's vector.
    #[inline]
    pub(super) fn decode_vector(&self, bits: &mut BitReader<'_>) -> Option<&[f32]> {
        let position = self.decode(bits)? as usize;
        let start = position * self.dimensions;
        self.vectors.get(start..start + self.dimensions)
    }
}

/// Unpacks the 32-bit float format of codebook headers (section 9.2.2): a
/// 21-bit mantissa, a 10-bit exponent biased by 788, and a sign.
pub(super) fn float32_unpack(value: u32) -> f32 {
    let mantissa = f64::from(value & 0x1F_FFFF);
    let exponent = ((value >> 21) & 0x3FF) as i32 - 788;
    let magnitude = mantissa * 2f64.powi(exponent);
    (if value & 0x8000_0000 != 0 {
        -magnitude
    } else {
        magnitude
    }) as f32
}

/// The largest number of values per dimension whose lattice has no more
/// points than `entries` (section 9.2.3).
pub(super) fn lookup1_values(entries: u32, dimensions: u32) -> u64 {
    if dimensions == 0 {
        return 0;
    }
    let fits = |values: u64| {
        let mut product = 1u64;
        for _ in 0..dimensions {
            product = match product.checked_mul(values) {
                Some(product) if product <= u64::from(entries) => product,
                _ => return false,
            };
        }
        true
    };
    // A first guess from floating point, then put right.
    let mut values = (f64::from(entries).powf(1.0 / f64::from(dimensions))) as u64;
    while values > 0 && !fits(values) {
        values -= 1;
    }
    while fits(values + 1) {
        values += 1;
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codewords_are_assigned_as_in_the_specification() {
        // The example of section 3.2.1.
        let lengths = [2, 4, 4, 4, 4, 2, 3, 3];
        let mut book = Codebook {
            dimensions: 1,
            entries: Vec::new(),
            fast: Vec::new(),
            fast_bits: 0,
            slow: Vec::new(),
            single: None,
            single_len: 0,
            vectors: Vec::new(),
        };
        book.build_code(&lengths).unwrap();
        let expected = ["00", "0100", "0101", "0110", "0111", "10", "110", "111"];
        for (entry, code) in expected.iter().enumerate() {
            // Write the codeword first bit first, which is lowest bit first
            // in the packet.
            let mut byte = 0u8;
            for (bit, digit) in code.chars().enumerate() {
                byte |= u8::from(digit == '1') << bit;
            }
            let bytes = [byte, 0xFF];
            let mut reader = BitReader::new(&bytes);
            assert_eq!(
                book.decode_scalar(&mut reader),
                Some(entry as u32),
                "{code}"
            );
        }
        // A tree with more codewords than fit is refused.
        assert!(book.build_code(&[1, 1, 1]).is_err());
    }

    #[test]
    fn long_codewords_are_found_past_the_table() {
        // Lengths 1, 2, ... 14, 14: a complete tree whose deepest codewords
        // are longer than the table.
        let mut lengths: Vec<u8> = (1..=14).collect();
        lengths.push(14);
        let mut book = Codebook {
            dimensions: 1,
            entries: Vec::new(),
            fast: Vec::new(),
            fast_bits: 0,
            slow: Vec::new(),
            single: None,
            single_len: 0,
            vectors: Vec::new(),
        };
        book.build_code(&lengths).unwrap();
        for entry in 0..15u32 {
            // Entry n is n ones and a zero; the last two are fourteen bits:
            // thirteen ones and a zero, and fourteen ones.
            let ones = entry.min(13);
            let mut value: u32 = (1 << ones) - 1;
            if entry == 14 {
                value |= 1 << 13;
            }
            let bytes = value.to_le_bytes();
            let mut reader = BitReader::new(&bytes);
            assert_eq!(
                book.decode_scalar(&mut reader),
                Some(entry),
                "entry {entry}"
            );
        }
    }

    #[test]
    fn floats_and_lattices_unpack_as_specified() {
        // 1.0: mantissa 2^20 and exponent 768 (2^20 * 2^(768 - 788)).
        assert_eq!(float32_unpack(768 << 21 | 1 << 20), 1.0);
        assert_eq!(float32_unpack(0x8000_0000 | 767 << 21 | 3 << 19), -0.75);
        assert_eq!(lookup1_values(625, 4), 5);
        assert_eq!(lookup1_values(624, 4), 4);
        assert_eq!(lookup1_values(81, 2), 9);
        assert_eq!(lookup1_values(1, 3), 1);
        assert_eq!(lookup1_values(0xFF_FFFF, 1), 0xFF_FFFF);
    }
}
