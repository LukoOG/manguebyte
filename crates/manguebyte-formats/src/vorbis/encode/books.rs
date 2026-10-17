//! The codebooks of the encoder: Huffman codes designed from models of
//! what each book codes, with the codewords Vorbis assigns to their lengths
//! (section 3.2.1 of the Vorbis I specification), and the lattices of
//! values the vector books stand for.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::BitWriter;

/// The longest codeword the designs allow, well within the 32 bits of the
/// format.
const MAX_LENGTH: u8 = 24;

/// The values a vector book stands for: `values` integers per dimension,
/// from `-(values / 2) * step` in steps of `step` (lookup type 1).
#[derive(Debug, Clone, Copy)]
pub(super) struct Lattice {
    pub(super) values: u32,
    pub(super) step: i32,
}

impl Lattice {
    /// The largest magnitude the lattice holds.
    #[cfg(test)]
    pub(super) fn reach(&self) -> i32 {
        (self.values as i32 / 2) * self.step
    }
}

/// A codebook as the encoder uses it.
#[derive(Debug, Clone)]
pub(super) struct Book {
    pub(super) dimensions: usize,
    lengths: Vec<u8>,
    /// Each codeword, its first bit lowest, as it is written.
    codes: Vec<u32>,
    pub(super) lattice: Option<Lattice>,
}

impl Book {
    /// A scalar book of `weights.len()` entries, entry `i` coded with
    /// about `-log2(weights[i] / total)` bits.
    pub(super) fn scalar(weights: &[f64]) -> Self {
        let lengths = huffman_lengths(weights);
        Self {
            dimensions: 1,
            codes: codewords(&lengths),
            lengths,
            lattice: None,
        }
    }

    /// A vector book of `dimensions` values from `lattice`, each vector
    /// weighted as the product of `weight` of its values.
    pub(super) fn vector(dimensions: usize, lattice: Lattice, weight: impl Fn(i32) -> f64) -> Self {
        let values = lattice.values as usize;
        let entries = values.pow(dimensions as u32);
        let half = (values / 2) as i32;
        let weights: Vec<f64> = (0..entries)
            .map(|entry| {
                let mut rest = entry;
                let mut product = 1.0;
                for _ in 0..dimensions {
                    product *= weight(((rest % values) as i32 - half) * lattice.step);
                    rest /= values;
                }
                product
            })
            .collect();
        Self::lattice(dimensions, lattice, &weights)
    }

    /// A vector book of `dimensions` values from `lattice`, entry `i`
    /// weighted `weights[i]`, its first value in its lowest digit.
    pub(super) fn lattice(dimensions: usize, lattice: Lattice, weights: &[f64]) -> Self {
        debug_assert_eq!(
            weights.len(),
            (lattice.values as usize).pow(dimensions as u32)
        );
        let lengths = huffman_lengths(weights);
        Self {
            dimensions,
            codes: codewords(&lengths),
            lengths,
            lattice: Some(lattice),
        }
    }

    /// The same book, declared of `dimensions` dimensions: what a
    /// classbook of a residue states, the classes of that many partitions
    /// in each entry.
    pub(super) fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// Writes the codeword of `entry`.
    pub(super) fn write(&self, bits: &mut BitWriter, entry: usize) {
        bits.write(self.codes[entry], u32::from(self.lengths[entry]));
    }

    /// The entry of the vector `values`, which the lattice must hold.
    pub(super) fn entry_of(&self, values: &[i32]) -> usize {
        let lattice = self.lattice.expect("a vector book");
        let half = (lattice.values / 2) as i32;
        let mut entry = 0usize;
        let mut scale = 1usize;
        for &value in values {
            let digit = (value / lattice.step + half) as usize;
            entry += digit * scale;
            scale *= lattice.values as usize;
        }
        entry
    }

    /// Writes the book into the setup header (section 3.2.1).
    pub(super) fn write_header(&self, bits: &mut BitWriter) {
        bits.write(0x56_4342, 24);
        bits.write(self.dimensions as u32, 16);
        bits.write(self.lengths.len() as u32, 24);
        // Not ordered, not sparse: every entry has a codeword.
        bits.write(0, 1);
        bits.write(0, 1);
        for &length in &self.lengths {
            bits.write(u32::from(length) - 1, 5);
        }
        match self.lattice {
            None => bits.write(0, 4),
            Some(lattice) => {
                bits.write(1, 4);
                let half = (lattice.values / 2) as i32;
                bits.write(float32_pack(-half * lattice.step), 32);
                bits.write(float32_pack(lattice.step), 32);
                let value_bits = (32 - (lattice.values - 1).leading_zeros()).max(1);
                bits.write(value_bits - 1, 4);
                // Not a sequence: each value stands alone.
                bits.write(0, 1);
                for multiplicand in 0..lattice.values {
                    bits.write(multiplicand, value_bits);
                }
            }
        }
    }
}

/// Packs a whole number into the 32-bit float format of codebook headers
/// (section 9.2.2): a 21-bit mantissa, a 10-bit exponent biased by 788,
/// and a sign. Every whole number the books use is exact in it.
pub(super) fn float32_pack(value: i32) -> u32 {
    if value == 0 {
        return 0;
    }
    let sign = if value < 0 { 0x8000_0000 } else { 0 };
    let magnitude = value.unsigned_abs();
    let top = 31 - magnitude.leading_zeros();
    // A mantissa of 21 bits with its top bit set.
    let mantissa = if top <= 20 {
        magnitude << (20 - top)
    } else {
        magnitude >> (top - 20)
    };
    let exponent = 768 + top;
    sign | (exponent << 21) | mantissa
}

/// The lengths of a Huffman code (Huffman, 1952) for these weights, held to
/// [`MAX_LENGTH`] bits: weights too small to fit are raised, a little at a
/// time, until the code does.
fn huffman_lengths(weights: &[f64]) -> Vec<u8> {
    let count = weights.len();
    if count == 1 {
        return vec![1];
    }
    let total: f64 = weights.iter().sum();
    let mut floor = total * 1e-9;
    loop {
        // Weights as integers, for a merge order that is the same on every
        // machine.
        let scaled: Vec<u64> = weights
            .iter()
            .map(|&weight| ((weight.max(floor) / total) * 1e12) as u64 + 1)
            .collect();
        let lengths = huffman(&scaled);
        if lengths.iter().all(|&length| length <= MAX_LENGTH) {
            return lengths;
        }
        floor *= 4.0;
    }
}

/// The depth of each leaf of the Huffman tree of `weights`.
fn huffman(weights: &[u64]) -> Vec<u8> {
    let count = weights.len();
    // Nodes: the leaves, then the merges; each knows its parent.
    let mut parent = vec![usize::MAX; 2 * count - 1];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = weights
        .iter()
        .enumerate()
        .map(|(index, &weight)| Reverse((weight, index)))
        .collect();
    let mut next = count;
    while heap.len() > 1 {
        let Reverse((a, first)) = heap.pop().unwrap();
        let Reverse((b, second)) = heap.pop().unwrap();
        parent[first] = next;
        parent[second] = next;
        heap.push(Reverse((a + b, next)));
        next += 1;
    }
    (0..count)
        .map(|leaf| {
            let mut depth = 0u8;
            let mut node = leaf;
            while parent[node] != usize::MAX {
                node = parent[node];
                depth = depth.saturating_add(1);
            }
            depth
        })
        .collect()
}

/// The codeword of each entry, as the decoder assigns them (section 3.2.1):
/// in order of entry, the lowest codeword of its length that is neither
/// taken nor the start of one taken. Returned with the first bit lowest,
/// as packets carry them.
fn codewords(lengths: &[u8]) -> Vec<u32> {
    // The free subtrees of the code tree, as the start of their range of
    // 32-bit values and their depth, in rising order.
    let mut free: Vec<(u64, u8)> = vec![(0, 0)];
    let mut codes = Vec::with_capacity(lengths.len());
    for &length in lengths {
        let index = free
            .iter()
            .position(|&(_, depth)| depth <= length)
            .expect("a complete code fits");
        let (start, depth) = free.remove(index);
        let code = (start >> (32 - u32::from(length))) as u32;
        codes.push(code.reverse_bits() >> (32 - u32::from(length)));
        for (step, below) in (depth + 1..=length).rev().enumerate() {
            free.insert(
                index + step,
                (start + (1u64 << (32 - u32::from(below))), below),
            );
        }
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_codes_are_complete_and_follow_the_weights() {
        let weights = [40.0, 20.0, 20.0, 10.0, 5.0, 5.0];
        let lengths = huffman_lengths(&weights);
        let kraft: f64 = lengths.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
        assert!((kraft - 1.0).abs() < 1e-12);
        assert!(lengths[0] <= lengths[3] && lengths[3] <= lengths[5]);
        // Weights that would need more than 24 bits are held to it.
        let skewed: Vec<f64> = (0..60).map(|i| 0.5f64.powi(i)).collect();
        let lengths = huffman_lengths(&skewed);
        assert!(lengths.iter().all(|&l| l <= MAX_LENGTH));
        let kraft: f64 = lengths.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
        assert!((kraft - 1.0).abs() < 1e-12);
    }

    #[test]
    fn floats_pack_as_the_decoder_unpacks_them() {
        for value in [1, -1, 2, 3, -8, 16, 255, -256, 1_000_000] {
            let packed = float32_pack(value);
            assert_eq!(
                super::super::super::codebook::float32_unpack(packed),
                value as f32
            );
        }
        assert_eq!(float32_pack(0), 0);
    }

    #[test]
    fn codewords_are_those_of_the_specification() {
        // The example of section 3.2.1: 2, 4, 4, 4, 4, 2, 3, 3 give 00,
        // 0100, 0101, 0110, 0111, 10, 110, 111.
        let codes = codewords(&[2, 4, 4, 4, 4, 2, 3, 3]);
        let expected = [0b00, 0b0100, 0b0101, 0b0110, 0b0111, 0b10, 0b110, 0b111];
        let lengths = [2, 4, 4, 4, 4, 2, 3, 3];
        for ((&code, &want), &length) in codes.iter().zip(&expected).zip(&lengths) {
            assert_eq!(code, (want as u32).reverse_bits() >> (32 - length));
        }
    }
}
