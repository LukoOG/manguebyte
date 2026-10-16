//! The three header packets of a Vorbis stream (section 4.2 of the
//! Vorbis I specification) and the configuration the setup header holds.

use super::VorbisError;
use super::bits::{BitReader, ilog};
use super::codebook::Codebook;

/// What a codebook budget allows a whole setup header: no stream an encoder
/// writes comes near, and a few bytes of header cannot ask for more memory
/// than this.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Entries left to declare, across all codebooks.
    pub(crate) entries_left: u32,
    /// Vector values left to hold, across all codebooks.
    pub(crate) values_left: u32,
}

impl Limits {
    pub(crate) fn new() -> Self {
        Self {
            entries_left: 1 << 18,
            values_left: 1 << 20,
        }
    }
}

/// The identification header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Identification {
    pub(crate) channels: usize,
    pub(crate) sample_rate: u32,
    /// The short and the long block size, in samples.
    pub(crate) block_sizes: [usize; 2],
}

/// Checks the type and the name every header packet starts with, and
/// returns what follows them.
fn header_body(packet: &[u8], kind: u8) -> Result<&[u8], VorbisError> {
    match packet {
        [first, b'v', b'o', b'r', b'b', b'i', b's', rest @ ..] if *first == kind => Ok(rest),
        _ => Err(VorbisError::NotVorbis),
    }
}

/// Reads the identification header (section 4.2.2).
pub(crate) fn identification(packet: &[u8]) -> Result<Identification, VorbisError> {
    let body = header_body(packet, 1)?;
    if body.len() < 23 {
        return Err(VorbisError::Malformed("identification header too short"));
    }
    let version = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    if version != 0 {
        return Err(VorbisError::Unsupported("a Vorbis version other than 1"));
    }
    let channels = usize::from(body[4]);
    let sample_rate = u32::from_le_bytes([body[5], body[6], body[7], body[8]]);
    let short = 1usize << (body[21] & 0x0F);
    let long = 1usize << (body[21] >> 4);
    if channels == 0 {
        return Err(VorbisError::Malformed("no channels"));
    }
    if sample_rate == 0 {
        return Err(VorbisError::Malformed("sample rate of zero"));
    }
    if !(64..=8_192).contains(&short) || !(64..=8_192).contains(&long) || short > long {
        return Err(VorbisError::Malformed("invalid block sizes"));
    }
    if body[22] & 1 == 0 {
        return Err(VorbisError::Malformed("identification header not framed"));
    }
    Ok(Identification {
        channels,
        sample_rate,
        block_sizes: [short, long],
    })
}

/// What the comment header says (section 5).
#[derive(Debug, Clone, Default)]
pub(crate) struct Comments {
    pub(crate) vendor: String,
    pub(crate) comments: Vec<(String, String)>,
}

/// Reads the comment header. Its lengths are checked against the packet, so
/// nothing is allocated for what is not there.
pub(crate) fn comments(packet: &[u8]) -> Result<Comments, VorbisError> {
    let body = header_body(packet, 3)?;
    let mut at = 0usize;
    let mut take = |len: usize| -> Result<&[u8], VorbisError> {
        let end = at
            .checked_add(len)
            .filter(|&end| end <= body.len())
            .ok_or(VorbisError::Malformed("comment header too short"))?;
        let bytes = &body[at..end];
        at = end;
        Ok(bytes)
    };
    let length =
        |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let vendor_len = length(take(4)?);
    let vendor = String::from_utf8_lossy(take(vendor_len)?).into_owned();
    let count = length(take(4)?);
    let mut comments = Vec::new();
    for _ in 0..count {
        let len = length(take(4)?);
        let text = take(len)?;
        // A comment is a name, an equals sign and a value. One without the
        // sign is not a comment, and is left out.
        if let Some(split) = text.iter().position(|&byte| byte == b'=') {
            let name = String::from_utf8_lossy(&text[..split]).into_owned();
            let value = String::from_utf8_lossy(&text[split + 1..]).into_owned();
            comments.push((name, value));
        }
    }
    Ok(Comments { vendor, comments })
}

/// A floor of type 1 (section 7.2.2): a curve of straight lines in decibels.
#[derive(Debug)]
pub(crate) struct Floor1 {
    pub(crate) partition_classes: Vec<u8>,
    pub(crate) class_dimensions: [u8; 16],
    pub(crate) class_subclasses: [u8; 16],
    pub(crate) class_masterbooks: [u8; 16],
    /// The codebook of each subclass of each class, or `None`.
    pub(crate) subclass_books: [[Option<u8>; 8]; 16],
    pub(crate) multiplier: u8,
    /// The X of each point, in the order the packet gives their Y.
    pub(crate) x: Vec<u16>,
    /// The points in order of X.
    pub(crate) sorted: Vec<u8>,
    /// For each point from the third, its neighbours below and above among
    /// the points before it.
    pub(crate) low_neighbour: Vec<u8>,
    pub(crate) high_neighbour: Vec<u8>,
}

/// A residue (section 8.6.1).
#[derive(Debug)]
pub(crate) struct Residue {
    /// 0, 1 or 2.
    pub(crate) kind: u16,
    pub(crate) begin: usize,
    pub(crate) end: usize,
    pub(crate) partition_size: usize,
    pub(crate) classifications: usize,
    pub(crate) classbook: u8,
    /// For each classification, the codebook of each of the eight passes.
    pub(crate) books: Vec<[Option<u8>; 8]>,
}

/// A mapping (section 4.2.4, type 0).
#[derive(Debug)]
pub(crate) struct Mapping {
    /// Magnitude and angle channel of each coupling step.
    pub(crate) coupling: Vec<(u8, u8)>,
    /// The submap of each channel.
    pub(crate) mux: Vec<u8>,
    /// The floor and the residue of each submap.
    pub(crate) submaps: Vec<(u8, u8)>,
}

/// A mode: which block size, and which mapping.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mode {
    pub(crate) long: bool,
    pub(crate) mapping: u8,
}

/// Everything the setup header configures.
#[derive(Debug)]
pub(crate) struct Setup {
    pub(crate) codebooks: Vec<Codebook>,
    /// `None` for a floor of type 0, which this decoder does not decode.
    pub(crate) floors: Vec<Option<Floor1>>,
    pub(crate) residues: Vec<Residue>,
    pub(crate) mappings: Vec<Mapping>,
    pub(crate) modes: Vec<Mode>,
}

/// Reads the setup header (section 4.2.4).
pub(crate) fn setup(packet: &[u8], ident: &Identification) -> Result<Setup, VorbisError> {
    let body = header_body(packet, 5)?;
    let mut bits = BitReader::new(body);
    let bits = &mut bits;
    let short = || VorbisError::Malformed("setup header too short");

    let mut limits = Limits::new();
    let count = bits.read(8).ok_or_else(short)? as usize + 1;
    let mut codebooks = Vec::with_capacity(count);
    for _ in 0..count {
        codebooks.push(Codebook::read(bits, &mut limits)?);
    }
    let book = |index: u32| -> Result<u8, VorbisError> {
        if (index as usize) < codebooks.len() {
            Ok(index as u8)
        } else {
            Err(VorbisError::Malformed("no such codebook"))
        }
    };

    // Time domain transforms: placeholders, all of type zero.
    let count = bits.read(6).ok_or_else(short)? + 1;
    for _ in 0..count {
        if bits.read(16).ok_or_else(short)? != 0 {
            return Err(VorbisError::Malformed(
                "time domain transform of a reserved type",
            ));
        }
    }

    let count = bits.read(6).ok_or_else(short)? + 1;
    let mut floors = Vec::with_capacity(count as usize);
    for _ in 0..count {
        floors.push(match bits.read(16).ok_or_else(short)? {
            0 => {
                skip_floor0(bits, &book)?;
                None
            }
            1 => Some(floor1(bits, &book)?),
            _ => return Err(VorbisError::Malformed("floor of a reserved type")),
        });
    }

    let count = bits.read(6).ok_or_else(short)? + 1;
    let mut residues = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let kind = bits.read(16).ok_or_else(short)? as u16;
        if kind > 2 {
            return Err(VorbisError::Malformed("residue of a reserved type"));
        }
        let begin = bits.read(24).ok_or_else(short)? as usize;
        let end = bits.read(24).ok_or_else(short)? as usize;
        let partition_size = bits.read(24).ok_or_else(short)? as usize + 1;
        let classifications = bits.read(6).ok_or_else(short)? as usize + 1;
        let classbook = book(bits.read(8).ok_or_else(short)?)?;
        let mut cascade = Vec::with_capacity(classifications);
        for _ in 0..classifications {
            let low = bits.read(3).ok_or_else(short)?;
            let high = if bits.flag().ok_or_else(short)? {
                bits.read(5).ok_or_else(short)?
            } else {
                0
            };
            cascade.push(high << 3 | low);
        }
        let mut books = Vec::with_capacity(classifications);
        for passes in cascade {
            let mut row = [None; 8];
            for (pass, slot) in row.iter_mut().enumerate() {
                if passes & (1 << pass) != 0 {
                    let index = book(bits.read(8).ok_or_else(short)?)?;
                    if !codebooks[usize::from(index)].has_vectors() {
                        return Err(VorbisError::Malformed("residue codebook without vectors"));
                    }
                    *slot = Some(index);
                }
            }
            books.push(row);
        }
        // Each classification word must have room for a digit per
        // dimension of the classbook.
        if codebooks[usize::from(classbook)].dimensions == 0 {
            return Err(VorbisError::Malformed("residue classbook of no dimensions"));
        }
        residues.push(Residue {
            kind,
            begin,
            end,
            partition_size,
            classifications,
            classbook,
            books,
        });
    }

    let count = bits.read(6).ok_or_else(short)? + 1;
    let mut mappings = Vec::with_capacity(count as usize);
    let channel_bits = ilog(ident.channels as u32 - 1);
    for _ in 0..count {
        if bits.read(16).ok_or_else(short)? != 0 {
            return Err(VorbisError::Malformed("mapping of a reserved type"));
        }
        let submap_count = if bits.flag().ok_or_else(short)? {
            bits.read(4).ok_or_else(short)? as usize + 1
        } else {
            1
        };
        let mut coupling = Vec::new();
        if bits.flag().ok_or_else(short)? {
            let steps = bits.read(8).ok_or_else(short)? + 1;
            for _ in 0..steps {
                let magnitude = bits.read(channel_bits).ok_or_else(short)? as usize;
                let angle = bits.read(channel_bits).ok_or_else(short)? as usize;
                if magnitude == angle || magnitude >= ident.channels || angle >= ident.channels {
                    return Err(VorbisError::Malformed("invalid channel coupling"));
                }
                coupling.push((magnitude as u8, angle as u8));
            }
        }
        if bits.read(2).ok_or_else(short)? != 0 {
            return Err(VorbisError::Malformed("reserved mapping bits set"));
        }
        let mut mux = vec![0u8; ident.channels];
        if submap_count > 1 {
            for slot in mux.iter_mut() {
                let submap = bits.read(4).ok_or_else(short)? as usize;
                if submap >= submap_count {
                    return Err(VorbisError::Malformed("channel mapped to no submap"));
                }
                *slot = submap as u8;
            }
        }
        let mut submaps = Vec::with_capacity(submap_count);
        for _ in 0..submap_count {
            bits.read(8).ok_or_else(short)?;
            let floor = bits.read(8).ok_or_else(short)? as usize;
            let residue = bits.read(8).ok_or_else(short)? as usize;
            if floor >= floors.len() || residue >= residues.len() {
                return Err(VorbisError::Malformed(
                    "submap names no such floor or residue",
                ));
            }
            submaps.push((floor as u8, residue as u8));
        }
        mappings.push(Mapping {
            coupling,
            mux,
            submaps,
        });
    }

    let count = bits.read(6).ok_or_else(short)? + 1;
    let mut modes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let long = bits.flag().ok_or_else(short)?;
        let window = bits.read(16).ok_or_else(short)?;
        let transform = bits.read(16).ok_or_else(short)?;
        let mapping = bits.read(8).ok_or_else(short)? as usize;
        if window != 0 || transform != 0 {
            return Err(VorbisError::Malformed(
                "mode of a reserved window or transform",
            ));
        }
        if mapping >= mappings.len() {
            return Err(VorbisError::Malformed("mode names no such mapping"));
        }
        modes.push(Mode {
            long,
            mapping: mapping as u8,
        });
    }
    if !bits.flag().ok_or_else(short)? {
        return Err(VorbisError::Malformed("setup header not framed"));
    }
    Ok(Setup {
        codebooks,
        floors,
        residues,
        mappings,
        modes,
    })
}

/// Reads the configuration of a floor of type 0 (section 6.2.1), only to
/// step over it: such a floor is refused when a packet uses it.
fn skip_floor0(
    bits: &mut BitReader<'_>,
    book: &dyn Fn(u32) -> Result<u8, VorbisError>,
) -> Result<(), VorbisError> {
    let short = || VorbisError::Malformed("setup header too short");
    // Order, rate, bark map size, amplitude bits and offset.
    for width in [8, 16, 16, 6, 8] {
        bits.read(width).ok_or_else(short)?;
    }
    let books = bits.read(4).ok_or_else(short)? + 1;
    for _ in 0..books {
        book(bits.read(8).ok_or_else(short)?)?;
    }
    Ok(())
}

/// Reads the configuration of a floor of type 1 (section 7.2.2).
fn floor1(
    bits: &mut BitReader<'_>,
    book: &dyn Fn(u32) -> Result<u8, VorbisError>,
) -> Result<Floor1, VorbisError> {
    let short = || VorbisError::Malformed("setup header too short");
    let partitions = bits.read(5).ok_or_else(short)? as usize;
    let mut partition_classes = Vec::with_capacity(partitions);
    for _ in 0..partitions {
        partition_classes.push(bits.read(4).ok_or_else(short)? as u8);
    }
    let classes = partition_classes
        .iter()
        .max()
        .map_or(0, |&max| usize::from(max) + 1);
    let mut class_dimensions = [0u8; 16];
    let mut class_subclasses = [0u8; 16];
    let mut class_masterbooks = [0u8; 16];
    let mut subclass_books = [[None; 8]; 16];
    for class in 0..classes {
        class_dimensions[class] = bits.read(3).ok_or_else(short)? as u8 + 1;
        class_subclasses[class] = bits.read(2).ok_or_else(short)? as u8;
        if class_subclasses[class] != 0 {
            class_masterbooks[class] = book(bits.read(8).ok_or_else(short)?)?;
        }
        for slot in subclass_books[class]
            .iter_mut()
            .take(1 << class_subclasses[class])
        {
            let index = bits.read(8).ok_or_else(short)?;
            *slot = match index {
                0 => None,
                index => Some(book(index - 1)?),
            };
        }
    }
    let multiplier = bits.read(2).ok_or_else(short)? as u8 + 1;
    let range_bits = bits.read(4).ok_or_else(short)?;
    let mut x = vec![0u16, 1 << range_bits];
    for &class in &partition_classes {
        for _ in 0..class_dimensions[usize::from(class)] {
            x.push(bits.read(range_bits).ok_or_else(short)? as u16);
        }
    }
    // The points must be told apart by their X: two at the same place give
    // the line between them no length.
    let mut sorted: Vec<u8> = (0..x.len() as u8).collect();
    sorted.sort_by_key(|&index| x[usize::from(index)]);
    if sorted
        .windows(2)
        .any(|pair| x[usize::from(pair[0])] == x[usize::from(pair[1])])
    {
        return Err(VorbisError::Malformed("floor points share an X"));
    }
    // The neighbours of section 9.2.4 and 9.2.5: among the points before a
    // point, the one with the greatest X below its X, and the one with the
    // least X above it. The first two points are 0 and the top, so every
    // later point has both.
    let mut low_neighbour = vec![0u8; x.len()];
    let mut high_neighbour = vec![1u8; x.len()];
    for point in 2..x.len() {
        let here = x[point];
        let mut low = 0;
        let mut high = 1;
        for before in 0..point {
            let value = x[before];
            if value < here && value > x[low] {
                low = before;
            }
            if value > here && value < x[high] {
                high = before;
            }
        }
        low_neighbour[point] = low as u8;
        high_neighbour[point] = high as u8;
    }
    Ok(Floor1 {
        partition_classes,
        class_dimensions,
        class_subclasses,
        class_masterbooks,
        subclass_books,
        multiplier,
        x,
        sorted,
        low_neighbour,
        high_neighbour,
    })
}
