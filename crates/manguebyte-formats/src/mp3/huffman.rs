//! Decoding the Huffman codes of layer III.

use std::sync::OnceLock;

use super::bits::BitReader;
use super::tables::{self, QUAD_A};

/// A prefix code as a binary tree, walked a bit at a time: each node holds
/// the two nodes or leaves its bits lead to. A leaf is `!value`, so that it
/// is negative, and an empty branch, which a complete code has none of, is
/// `0`, the root, which no branch leads back to.
#[derive(Debug, Default)]
pub(super) struct Tree {
    nodes: Vec<[i16; 2]>,
}

impl Tree {
    /// Builds the tree of `codes` and `lengths`, the value of each codeword
    /// being its index.
    fn new(codes: &[u16], lengths: &[u8]) -> Self {
        let mut tree = Self {
            nodes: vec![[0, 0]],
        };
        for (value, (&code, &length)) in codes.iter().zip(lengths).enumerate() {
            let mut node = 0usize;
            for bit in (0..u32::from(length)).rev() {
                let branch = usize::from((u32::from(code) >> bit) & 1 == 1);
                if bit == 0 {
                    tree.nodes[node][branch] = !(value as i16);
                } else {
                    let next = tree.nodes[node][branch];
                    node = if next > 0 {
                        next as usize
                    } else {
                        tree.nodes.push([0, 0]);
                        let new = tree.nodes.len() - 1;
                        tree.nodes[node][branch] = new as i16;
                        new
                    };
                }
            }
        }
        tree
    }

    /// Reads one codeword. `None` at the end of the data, or for bits that
    /// lead nowhere.
    #[inline]
    pub(super) fn decode(&self, bits: &mut BitReader<'_>) -> Option<usize> {
        let mut node = 0usize;
        loop {
            let next = self.nodes[node][bits.bit()? as usize];
            if next < 0 {
                return Some(!next as usize);
            }
            if next == 0 {
                return None;
            }
            node = next as usize;
        }
    }
}

/// The trees of every table of pairs, by table number, and of quadruple
/// table A.
pub(super) struct Trees {
    pub(super) pairs: Vec<Option<Tree>>,
    pub(super) quad_a: Tree,
}

/// The trees, built the first time they are needed.
pub(super) fn trees() -> &'static Trees {
    static TREES: OnceLock<Trees> = OnceLock::new();
    TREES.get_or_init(|| Trees {
        pairs: (0..32)
            .map(|number| {
                tables::pair_table(number).map(|table| Tree::new(table.codes, table.lengths))
            })
            .collect(),
        quad_a: {
            let codes: Vec<u16> = QUAD_A.0.iter().map(|&code| u16::from(code)).collect();
            Tree::new(&codes, QUAD_A.1)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `codes` and `lengths` make a complete prefix code: no
    /// codeword starts another, and together they cover every string of
    /// bits, so that the lengths meet Kraft's inequality with equality.
    fn complete_prefix_code(codes: &[u16], lengths: &[u8]) -> Result<(), String> {
        let mut kraft = 0u64;
        for (index, (&code, &length)) in codes.iter().zip(lengths).enumerate() {
            if length == 0 || length > 19 || u32::from(code) >> length != 0 {
                return Err(format!("entry {index}: code {code:#x} of {length} bits"));
            }
            kraft += 1 << (19 - length);
            for (other, (&other_code, &other_length)) in codes.iter().zip(lengths).enumerate() {
                if other != index
                    && other_length >= length
                    && u32::from(other_code) >> (other_length - length) == u32::from(code)
                {
                    return Err(format!("entry {index} starts entry {other}"));
                }
            }
        }
        if kraft != 1 << 19 {
            return Err(format!("the code covers {kraft} of {} strings", 1u64 << 19));
        }
        Ok(())
    }

    #[test]
    fn every_table_is_a_complete_prefix_code() {
        for number in 0..32 {
            if let Some(table) = tables::pair_table(number) {
                assert_eq!(table.codes.len(), table.size * table.size);
                assert_eq!(table.lengths.len(), table.size * table.size);
                complete_prefix_code(table.codes, table.lengths)
                    .unwrap_or_else(|problem| panic!("table {number}: {problem}"));
            }
        }
        let codes: Vec<u16> = QUAD_A.0.iter().map(|&code| u16::from(code)).collect();
        complete_prefix_code(&codes, QUAD_A.1).unwrap();
    }

    #[test]
    fn a_tree_decodes_each_codeword_to_its_value() {
        let trees = trees();
        for number in [1, 7, 13, 15, 16, 24] {
            let table = tables::pair_table(number).unwrap();
            let tree = trees.pairs[number].as_ref().unwrap();
            for (value, (&code, &length)) in table.codes.iter().zip(table.lengths).enumerate() {
                // The codeword at the top of three bytes, then ones.
                let word = (u32::from(code) << (24 - length)) | ((1 << (24 - length)) - 1);
                let bytes = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
                let mut bits = BitReader::new(&bytes, 0, 24);
                assert_eq!(tree.decode(&mut bits), Some(value), "table {number}");
                assert_eq!(bits.position(), u32::from(length) as usize);
            }
        }
    }
}
