//! Ogg Vorbis streams written here, from the specification and apart from
//! the decoder: its own bit packing, its own assignment of codewords, and
//! the layout of each residue type as section 8.6 describes it.

use super::{Rng, ogg_crc};

/// Packs bits least significant first, as Vorbis does.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    used: u32,
}

impl Bits {
    fn put(&mut self, value: u64, count: u32) {
        for bit in 0..count {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let last = self.bytes.last_mut().unwrap();
            *last |= (((value >> bit) & 1) as u8) << self.used;
            self.used = (self.used + 1) % 8;
        }
    }

    fn flag(&mut self, value: bool) {
        self.put(u64::from(value), 1);
    }
}

/// The codeword of each entry of a code with these lengths: in order of
/// entry, the lowest codeword of its length that neither is taken nor
/// starts or is started by one taken (section 3.2.1). Worked out here by
/// trying every codeword in turn, as the specification states it.
fn codewords(lengths: &[u32]) -> Vec<u32> {
    let mut taken: Vec<(u32, u32)> = Vec::new();
    for &length in lengths {
        let code = (0..1u32 << length)
            .find(|&code| {
                taken.iter().all(|&(other, other_length)| {
                    let shorter = length.min(other_length);
                    code >> (length - shorter) != other >> (other_length - shorter)
                })
            })
            .expect("the lengths fit");
        taken.push((code, length));
    }
    taken.into_iter().map(|(code, _)| code).collect()
}

/// A codebook, as the setup header states it and as packets use it.
struct Book {
    dimensions: u32,
    lengths: Vec<u32>,
    /// Lookup type, and for types 1 and 2 the multiplicands.
    lookup: u32,
    multiplicands: Vec<u32>,
    codes: Vec<u32>,
}

impl Book {
    fn new(dimensions: u32, lengths: Vec<u32>, lookup: u32, multiplicands: Vec<u32>) -> Self {
        let codes = codewords(&lengths);
        Self {
            dimensions,
            lengths,
            lookup,
            multiplicands,
            codes,
        }
    }

    /// Writes the codebook into a setup header (section 3.2.1): not ordered,
    /// not sparse; vectors of the minimum -1 plus a delta of 1 times each
    /// multiplicand of two bits.
    fn write(&self, bits: &mut Bits) {
        bits.put(0x56_4342, 24);
        bits.put(u64::from(self.dimensions), 16);
        bits.put(self.lengths.len() as u64, 24);
        bits.flag(false);
        bits.flag(false);
        for &length in &self.lengths {
            bits.put(u64::from(length - 1), 5);
        }
        bits.put(u64::from(self.lookup), 4);
        if self.lookup > 0 {
            bits.put(float(-1.0), 32);
            bits.put(float(1.0), 32);
            bits.put(1, 4);
            bits.flag(false);
            for &value in &self.multiplicands {
                bits.put(u64::from(value), 2);
            }
        }
    }

    /// Writes the codeword of `entry`, its first bit first.
    fn code(&self, bits: &mut Bits, entry: usize) {
        let (code, length) = (self.codes[entry], self.lengths[entry]);
        for bit in (0..length).rev() {
            bits.put(u64::from((code >> bit) & 1), 1);
        }
    }
}

/// A small whole number in the float format of section 9.2.2.
fn float(value: f64) -> u64 {
    let sign = if value < 0.0 { 1u64 << 31 } else { 0 };
    let mantissa = (value.abs() * f64::from(1u32 << 15)) as u64;
    sign | (788 - 15) << 21 | mantissa
}

/// The entry of a vector book whose two values are `pair`, each -1, 0 or 1:
/// the lattice reads the entry number in base 3, lowest digit first.
fn entry_of(pair: [i32; 2]) -> usize {
    (pair[0] + 1) as usize + 3 * (pair[1] + 1) as usize
}

/// How a stream codes its residue.
#[derive(Clone, Copy, PartialEq)]
pub enum Coding {
    /// Residue type 0, 1 or 2, with vectors from a lattice (lookup type 1)
    /// or listed (lookup type 2).
    Residue { kind: u32, listed: bool },
    /// A floor of type 0, which the decoder refuses.
    Floor0,
}

/// Values in each partition of a residue.
const PARTITION: usize = 16;

/// An Ogg Vorbis stream of `channels` channels whose packets hold, for each
/// channel, the residue `values[packet][channel]`: half a block of -1, 0 or
/// 1, over a floor that is flat and loud. Both block sizes are the same, a
/// power of two from 64 to 8192, so that every packet finishes half a
/// block.
pub fn stream(channels: usize, values: &[Vec<Vec<i32>>], coding: Coding) -> Vec<u8> {
    let half = values[0][0].len();
    let block = 2 * half;
    let exponent = block.trailing_zeros() as u8;
    let y_book = Book::new(1, vec![7; 128], 0, Vec::new());
    let class_book = Book::new(1, vec![1, 1], 0, Vec::new());
    let lengths = vec![3, 3, 3, 3, 3, 3, 3, 4, 4];
    let lattice = Book::new(2, lengths.clone(), 1, vec![0, 1, 2]);
    let listed_values = (0..9).flat_map(|entry| [entry % 3, entry / 3]).collect();
    let listed = Book::new(2, lengths, 2, listed_values);
    let books = [&y_book, &class_book, &lattice, &listed];

    // Identification header.
    let mut ident = b"\x01vorbis".to_vec();
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(channels as u8);
    ident.extend_from_slice(&44_100u32.to_le_bytes());
    ident.extend_from_slice(&[0; 12]);
    ident.push(exponent << 4 | exponent);
    ident.push(1);

    let mut comment = b"\x03vorbis".to_vec();
    comment.extend_from_slice(&4u32.to_le_bytes());
    comment.extend_from_slice(b"test");
    comment.extend_from_slice(&0u32.to_le_bytes());
    comment.push(1);

    // Setup header.
    let mut setup = Bits::default();
    for &byte in b"\x05vorbis" {
        setup.put(u64::from(byte), 8);
    }
    setup.put(books.len() as u64 - 1, 8);
    for book in books {
        book.write(&mut setup);
    }
    setup.put(0, 6);
    setup.put(0, 16);
    if coding == Coding::Floor0 {
        // Two floors: one of type 0, which the mapping uses, and one of
        // type 1.
        setup.put(1, 6);
        setup.put(0, 16);
        for (value, width) in [(8, 8), (44_100, 16), (128, 16), (6, 6), (100, 8)] {
            setup.put(value, width);
        }
        setup.put(0, 4);
        setup.put(0, 8);
    } else {
        setup.put(0, 6);
    }
    // A floor of type 1: one partition of class 0, a class of one value
    // read with book 0, multiplier 2, and three points: 0, 128 and 64.
    setup.put(1, 16);
    setup.put(1, 5);
    setup.put(0, 4);
    setup.put(0, 3);
    setup.put(0, 2);
    setup.put(1, 8);
    setup.put(1, 2);
    setup.put(7, 4);
    setup.put(64, 7);
    // One residue.
    let (kind, listed) = match coding {
        Coding::Residue { kind, listed } => (kind, listed),
        Coding::Floor0 => (1, false),
    };
    setup.put(0, 6);
    setup.put(u64::from(kind), 16);
    let size = if kind == 2 { half * channels } else { half };
    setup.put(0, 24);
    setup.put(size as u64, 24);
    setup.put(PARTITION as u64 - 1, 24);
    setup.put(1, 6);
    setup.put(1, 8);
    // Class 0 has no book in any pass; class 1 has the vector book in the
    // first.
    setup.put(0, 3);
    setup.flag(false);
    setup.put(1, 3);
    setup.flag(false);
    setup.put(if listed { 3 } else { 2 }, 8);
    // One mapping: one submap, no coupling.
    setup.put(0, 6);
    setup.put(0, 16);
    setup.flag(false);
    setup.flag(false);
    setup.put(0, 2);
    setup.put(0, 8);
    setup.put(0, 8);
    setup.put(0, 8);
    // One mode.
    setup.put(0, 6);
    setup.flag(false);
    setup.put(0, 16);
    setup.put(0, 16);
    setup.put(0, 8);
    setup.flag(true);

    let packets: Vec<Vec<u8>> = values
        .iter()
        .map(|channels_values| audio_packet(channels_values, kind, &books))
        .collect();

    let mut out = Vec::new();
    page(&mut out, &[&ident], 0x02, 0, 0);
    page(&mut out, &[&comment, &setup.bytes], 0, 0, 1);
    let refs: Vec<&[u8]> = packets.iter().map(Vec::as_slice).collect();
    let granule = (packets.len() as u64 - 1) * half as u64;
    page(&mut out, &refs, 0x04, granule, 2);
    out
}

/// An audio packet: the flat floor of every channel, then the residue in
/// the layout of `kind`.
fn audio_packet(values: &[Vec<i32>], kind: u32, books: &[&Book; 4]) -> Vec<u8> {
    let [y_book, class_book, lattice, _] = *books;
    let mut bits = Bits::default();
    bits.flag(false);
    for _ in values {
        // Used, both ends at 127, and the point between them on the line:
        // a curve of 254 everywhere.
        bits.flag(true);
        bits.put(127, 7);
        bits.put(127, 7);
        y_book.code(&mut bits, 0);
    }
    // The vectors to write, one list per vector the residue decodes into.
    let vectors: Vec<Vec<i32>> = if kind == 2 {
        let channels = values.len();
        let half = values[0].len();
        vec![
            (0..half * channels)
                .map(|i| values[i % channels][i / channels])
                .collect(),
        ]
    } else {
        values.to_vec()
    };
    let partitions = vectors[0].len() / PARTITION;
    // Pass 0 is the only one with a book: the classification of each
    // partition of each vector, then its values (section 8.6.2).
    for partition in 0..partitions {
        let range = partition * PARTITION..(partition + 1) * PARTITION;
        for vector in &vectors {
            let class = usize::from(vector[range.clone()].iter().any(|&value| value != 0));
            class_book.code(&mut bits, class);
        }
        for vector in &vectors {
            let part = &vector[range.clone()];
            if part.iter().all(|&value| value == 0) {
                continue;
            }
            if kind == 0 {
                // Each vector's two values are half a partition apart.
                let step = PARTITION / 2;
                for i in 0..step {
                    lattice.code(&mut bits, entry_of([part[i], part[i + step]]));
                }
            } else {
                for pair in part.chunks(2) {
                    lattice.code(&mut bits, entry_of([pair[0], pair[1]]));
                }
            }
        }
    }
    bits.bytes
}

/// Appends an Ogg page holding whole `packets`.
fn page(out: &mut Vec<u8>, packets: &[&[u8]], flags: u8, granule: u64, sequence: u32) {
    let mut lacing = Vec::new();
    for packet in packets {
        lacing.extend(std::iter::repeat_n(255u8, packet.len() / 255));
        lacing.push((packet.len() % 255) as u8);
    }
    assert!(lacing.len() <= 255);
    let start = out.len();
    out.extend_from_slice(b"OggS\0");
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&0x5EED_u32.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    for packet in packets {
        out.extend_from_slice(packet);
    }
    let crc = ogg_crc(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
}

/// Residue values for `packets` packets of `channels` channels and blocks of
/// `block` samples: -1, 0 or 1 at random, with some partitions left empty.
pub fn random_values(
    packets: usize,
    channels: usize,
    block: usize,
    seed: u64,
) -> Vec<Vec<Vec<i32>>> {
    let mut rng = Rng::new(seed);
    (0..packets)
        .map(|_| {
            (0..channels)
                .map(|_| {
                    let mut values: Vec<i32> =
                        (0..block / 2).map(|_| rng.below(3) as i32 - 1).collect();
                    for partition in values.chunks_mut(PARTITION) {
                        if rng.below(3) == 0 {
                            partition.fill(0);
                        }
                    }
                    values
                })
                .collect()
        })
        .collect()
}

/// The headers of a stream of one channel whose setup header is `setup`,
/// and no audio: for setups that are not what they should be.
pub fn headers_with_setup(setup: &[u8]) -> Vec<u8> {
    let mut ident = b"\x01vorbis".to_vec();
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(1);
    ident.extend_from_slice(&44_100u32.to_le_bytes());
    ident.extend_from_slice(&[0; 12]);
    ident.push(0x88);
    ident.push(1);
    let mut comment = b"\x03vorbis".to_vec();
    comment.extend_from_slice(&[0; 8]);
    comment.push(1);
    let mut out = Vec::new();
    page(&mut out, &[&ident], 0x02, 0, 0);
    page(&mut out, &[&comment, setup], 0x04, 0, 1);
    out
}

/// A setup header whose first codebook declares `entries` entries, all of
/// one length, in the few bits of the ordered form, and ends there.
pub fn setup_with_large_codebook(entries: u32, length: u32) -> Vec<u8> {
    let mut bits = Bits::default();
    for &byte in b"\x05vorbis" {
        bits.put(u64::from(byte), 8);
    }
    bits.put(0, 8);
    bits.put(0x56_4342, 24);
    bits.put(1, 16);
    bits.put(u64::from(entries), 24);
    bits.flag(true);
    bits.put(u64::from(length - 1), 5);
    // One run of every entry, its count in as many bits as the number of
    // entries left needs.
    bits.put(u64::from(entries), 32 - entries.leading_zeros());
    bits.put(0, 4);
    bits.bytes
}
