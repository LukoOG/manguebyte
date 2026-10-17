//! The framing of an Opus packet (RFC 6716, section 3): the table of
//! contents byte, and the frames a packet holds, with the self-delimiting
//! variant of Appendix B that every stream but the last of a multistream
//! packet uses (RFC 7845, section 5.1.1).

/// The coding mode of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Silk,
    Hybrid,
    Celt,
}

/// The audio bandwidth of a frame: narrow, medium, wide, super-wide and
/// full band (4, 6, 8, 12 and 20 kHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Bandwidth {
    Narrow,
    Medium,
    Wide,
    SuperWide,
    Full,
}

/// What the table of contents byte says (section 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Toc {
    pub(crate) mode: Mode,
    pub(crate) bandwidth: Bandwidth,
    /// Samples of each frame at 48 kHz.
    pub(crate) frame_size: usize,
    pub(crate) stereo: bool,
}

impl Toc {
    pub(crate) fn parse(toc: u8) -> Self {
        let config = toc >> 3;
        let (mode, bandwidth, frame_size) = match config {
            0..=11 => (
                Mode::Silk,
                [Bandwidth::Narrow, Bandwidth::Medium, Bandwidth::Wide][usize::from(config / 4)],
                [480, 960, 1_920, 2_880][usize::from(config & 3)],
            ),
            12..=15 => (
                Mode::Hybrid,
                if config < 14 {
                    Bandwidth::SuperWide
                } else {
                    Bandwidth::Full
                },
                [480, 960][usize::from(config & 1)],
            ),
            _ => (
                Mode::Celt,
                [
                    Bandwidth::Narrow,
                    Bandwidth::Wide,
                    Bandwidth::SuperWide,
                    Bandwidth::Full,
                ][usize::from((config - 16) / 4)],
                [120, 240, 480, 960][usize::from(config & 3)],
            ),
        };
        Self {
            mode,
            bandwidth,
            frame_size,
            stereo: toc & 4 != 0,
        }
    }
}

/// The most frames a packet holds: 120 ms of 2.5 ms frames.
pub(crate) const MAX_FRAMES: usize = 48;
/// The longest frame.
const MAX_FRAME_BYTES: usize = 1_275;

/// A packet taken apart.
#[derive(Debug, Clone)]
pub(crate) struct Packet {
    pub(crate) toc: Toc,
    /// Where each frame is in the packet, and its length.
    pub(crate) frames: [(usize, usize); MAX_FRAMES],
    pub(crate) count: usize,
    /// Bytes the packet takes, padding included: for a self-delimited
    /// packet, where the next one starts.
    pub(crate) len: usize,
}

impl Packet {
    /// Samples the packet decodes to, at 48 kHz.
    pub(crate) fn samples(&self) -> usize {
        self.count * self.toc.frame_size
    }
}

/// Reads a frame length of one or two bytes (section 3.2.1).
fn frame_len(data: &[u8], at: usize) -> Option<(usize, usize)> {
    let first = usize::from(*data.get(at)?);
    if first < 252 {
        Some((first, 1))
    } else {
        let second = usize::from(*data.get(at + 1)?);
        Some((first + 4 * second, 2))
    }
}

/// Takes `data` apart into its frames, or says why it is not a valid
/// packet (rules R1 to R7 of section 3.4).
pub(crate) fn parse(data: &[u8], self_delimited: bool) -> Result<Packet, &'static str> {
    let invalid = "invalid Opus packet";
    let &toc_byte = data.first().ok_or(invalid)?;
    let toc = Toc::parse(toc_byte);
    let mut at = 1usize;
    let mut left = data.len() - 1;
    let mut sizes = [0usize; MAX_FRAMES];
    let count;
    let mut cbr = false;
    let mut last_size = left;
    let mut padding = 0usize;
    match toc_byte & 3 {
        0 => count = 1,
        1 => {
            count = 2;
            cbr = true;
            if !self_delimited {
                if left % 2 != 0 {
                    return Err(invalid);
                }
                last_size = left / 2;
                sizes[0] = last_size;
            }
        }
        2 => {
            count = 2;
            let (size, bytes) = frame_len(data, at).ok_or(invalid)?;
            left -= bytes;
            if size > left {
                return Err(invalid);
            }
            at += bytes;
            sizes[0] = size;
            last_size = left - size;
        }
        _ => {
            if left < 1 {
                return Err(invalid);
            }
            let byte = data[at];
            at += 1;
            left -= 1;
            count = usize::from(byte & 0x3F);
            if count == 0 || toc.frame_size * count > 5_760 {
                return Err(invalid);
            }
            if byte & 0x40 != 0 {
                loop {
                    if left == 0 {
                        return Err(invalid);
                    }
                    let p = usize::from(data[at]);
                    at += 1;
                    left -= 1;
                    let taken = if p == 255 { 254 } else { p };
                    left = left.checked_sub(taken).ok_or(invalid)?;
                    padding += taken;
                    if p != 255 {
                        break;
                    }
                }
            }
            cbr = byte & 0x80 == 0;
            if !cbr {
                last_size = left;
                for size_slot in sizes.iter_mut().take(count - 1) {
                    let (size, bytes) = frame_len(&data[..at + left], at).ok_or(invalid)?;
                    left -= bytes;
                    if size > left {
                        return Err(invalid);
                    }
                    at += bytes;
                    *size_slot = size;
                    last_size = last_size.checked_sub(bytes + size).ok_or(invalid)?;
                }
            } else if !self_delimited {
                last_size = left / count;
                if last_size * count != left {
                    return Err(invalid);
                }
                for size_slot in sizes.iter_mut().take(count - 1) {
                    *size_slot = last_size;
                }
            }
        }
    }
    if self_delimited {
        let (size, bytes) = frame_len(&data[..at + left], at).ok_or(invalid)?;
        left -= bytes;
        if size > left {
            return Err(invalid);
        }
        at += bytes;
        sizes[count - 1] = size;
        if cbr {
            if size * count > left {
                return Err(invalid);
            }
            for size_slot in sizes.iter_mut().take(count - 1) {
                *size_slot = size;
            }
        } else if bytes + size > last_size {
            return Err(invalid);
        }
    } else {
        if last_size > MAX_FRAME_BYTES {
            return Err(invalid);
        }
        sizes[count - 1] = last_size;
    }
    let mut frames = [(0usize, 0usize); MAX_FRAMES];
    for (frame, &size) in frames.iter_mut().zip(&sizes).take(count) {
        if size > MAX_FRAME_BYTES {
            return Err(invalid);
        }
        *frame = (at, size);
        at += size;
    }
    if at > data.len() {
        return Err(invalid);
    }
    Ok(Packet {
        toc,
        frames,
        count,
        len: at + padding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_of_contents_names_mode_bandwidth_and_size() {
        let toc = Toc::parse(0b1111_1100);
        assert_eq!(toc.mode, Mode::Celt);
        assert_eq!(toc.bandwidth, Bandwidth::Full);
        assert_eq!(toc.frame_size, 960);
        assert!(toc.stereo);
        let toc = Toc::parse(3 << 3);
        assert_eq!(
            (toc.mode, toc.bandwidth, toc.frame_size),
            (Mode::Silk, Bandwidth::Narrow, 2_880)
        );
        let toc = Toc::parse(13 << 3);
        assert_eq!(
            (toc.mode, toc.bandwidth, toc.frame_size),
            (Mode::Hybrid, Bandwidth::SuperWide, 960)
        );
    }

    #[test]
    fn every_code_is_taken_apart() {
        // Code 0: one frame of the rest.
        let packet = parse(&[0xF8, 1, 2, 3], false).unwrap();
        assert_eq!((packet.count, packet.frames[0]), (1, (1, 3)));
        // Code 1: two of half each; an odd length is invalid.
        let packet = parse(&[0xF9, 1, 2, 3, 4], false).unwrap();
        assert_eq!(&packet.frames[..2], &[(1, 2), (3, 2)]);
        assert!(parse(&[0xF9, 1, 2, 3], false).is_err());
        // Code 2: the first length given.
        let packet = parse(&[0xFA, 1, 9, 8, 7], false).unwrap();
        assert_eq!(&packet.frames[..2], &[(2, 1), (3, 2)]);
        // Code 3, VBR with padding: three frames.
        let packet = parse(&[0xFB, 0xC3, 2, 1, 2, 9, 8, 7, 6, 5, 0, 0], false).unwrap();
        assert_eq!(packet.count, 3);
        assert_eq!(&packet.frames[..3], &[(5, 1), (6, 2), (8, 2)]);
        // More than 120 ms is invalid.
        assert!(parse(&[0x1B, 0x03, 0, 0, 0], false).is_err());
        assert!(parse(&[0x1B, 0x02, 0, 0], false).is_ok());
        // A length of two bytes.
        let mut long = vec![0xFA, 252, 1];
        long.extend(std::iter::repeat_n(0, 300));
        let packet = parse(&long, false).unwrap();
        assert_eq!(packet.frames[0], (3, 256));
    }

    #[test]
    fn a_self_delimited_packet_says_where_the_next_starts() {
        let data = [0xF8, 2, 7, 7, 0xF8, 5];
        let packet = parse(&data, true).unwrap();
        assert_eq!(packet.frames[0], (2, 2));
        assert_eq!(packet.len, 4);
        assert!(parse(&[0xF8, 9, 1], true).is_err());
    }
}
