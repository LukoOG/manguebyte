//! Layers I and II (ISO/IEC 11172-3, 2.4.1.5 to 2.4.1.6 and 2.4.3.2 to
//! 2.4.3.3, with the bit allocation of the lower sample frequencies from
//! ISO/IEC 13818-3, 2.4.3.3 and Annex B).
//!
//! Both code 32 subbands directly: each frame states how many bits each
//! subband's samples take, a scale factor for each, and the samples, which
//! are scaled back and handed to the same synthesis filter bank layer III
//! ends with. Nothing carries over from one frame to the next but the
//! filter bank's memory of its last sixteen time slots.

use super::Mp3Error;
use super::bits::BitReader;
use super::header::{FrameHeader, Mode, Version};
use super::synth::Synthesis;

/// The levels a subband can be quantised to, by allocation code, for each
/// row of the allocation tables of layer II (Table 3-B.2 of 11172-3, and
/// Table B.1 of 13818-3). Zero is "not coded"; three, five and nine levels
/// are coded in groups of three samples.
const ROW_A: [u32; 16] = [
    0, 3, 7, 15, 31, 63, 127, 255, 511, 1_023, 2_047, 4_095, 8_191, 16_383, 32_767, 65_535,
];
const ROW_B: [u32; 16] = [
    0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1_023, 2_047, 4_095, 8_191, 65_535,
];
const ROW_C: [u32; 16] = [0, 3, 5, 7, 9, 15, 31, 65_535, 0, 0, 0, 0, 0, 0, 0, 0];
const ROW_D: [u32; 16] = [0, 3, 5, 65_535, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const ROW_E: [u32; 16] = [
    0, 3, 5, 9, 15, 31, 63, 127, 255, 511, 1_023, 2_047, 4_095, 8_191, 16_383, 32_767,
];
const ROW_F: [u32; 16] = [
    0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1_023, 2_047, 4_095, 8_191, 16_383,
];

/// One subband of an allocation table: the bits of its allocation code and
/// the levels each code stands for.
type Band = (u32, &'static [u32; 16]);

/// Builds a table from runs of subbands that share a row.
const fn table<const N: usize>(runs: &[(usize, Band)]) -> [Band; N] {
    let mut out: [Band; N] = [(0, &ROW_A); N];
    let mut at = 0;
    let mut run = 0;
    while run < runs.len() {
        let (count, band) = runs[run];
        let mut index = 0;
        while index < count {
            out[at] = band;
            at += 1;
            index += 1;
        }
        run += 1;
    }
    assert!(at == N, "the runs must fill the table");
    out
}

/// Table 3-B.2a: 27 subbands, for 48 kHz, and 32 and 44.1 kHz at 56 to 80
/// kbit/s a channel.
const TABLE_A: [Band; 27] = table(&[
    (3, (4, &ROW_A)),
    (8, (4, &ROW_B)),
    (12, (3, &ROW_C)),
    (4, (2, &ROW_D)),
]);
/// Table 3-B.2b: 30 subbands, for 32 and 44.1 kHz from 96 kbit/s a channel.
const TABLE_B: [Band; 30] = table(&[
    (3, (4, &ROW_A)),
    (8, (4, &ROW_B)),
    (12, (3, &ROW_C)),
    (7, (2, &ROW_D)),
]);
/// Table 3-B.2c: 8 subbands, for 44.1 and 48 kHz at 32 to 48 kbit/s a
/// channel.
const TABLE_C: [Band; 8] = table(&[(2, (4, &ROW_E)), (6, (3, &ROW_E))]);
/// Table 3-B.2d: 12 subbands, for 32 kHz at 32 to 48 kbit/s a channel.
const TABLE_D: [Band; 12] = table(&[(2, (4, &ROW_E)), (10, (3, &ROW_E))]);
/// Table B.1 of 13818-3: 30 subbands, for every rate of the lower sample
/// frequencies.
const TABLE_LSF: [Band; 30] = table(&[(4, (4, &ROW_F)), (7, (3, &ROW_E)), (19, (2, &ROW_E))]);

/// The allocation table a layer II frame uses (2.4.3.3.1 of 11172-3, and
/// 13818-3 for the lower sample frequencies). A bitrate a channel the
/// standard does not pair with the mode, such as 32 kbit/s in stereo, gets
/// the table of the bitrates next to it.
fn allocation_table(header: &FrameHeader) -> &'static [Band] {
    if header.version != Version::Mpeg1 {
        return &TABLE_LSF;
    }
    let per_channel = header.bitrate / 1_000 / header.channels() as u32;
    match (per_channel, header.sample_rate) {
        (..=48, 32_000) => &TABLE_D,
        (..=48, _) => &TABLE_C,
        (..=80, _) | (_, 48_000) => &TABLE_A,
        _ => &TABLE_B,
    }
}

/// The scale factors, by index: 2^(1 - i/3), from 2.0 down (Table 3-B.1).
/// Index 63 is not used by the standard; it gives the next value down.
fn scale_factor(index: u32) -> f32 {
    (2f64.powf(1.0 - f64::from(index) / 3.0)) as f32
}

/// The value of quantised sample `code` of a quantiser of `levels` levels,
/// between -1 and 1 (2.4.3.2.3 and 2.4.3.3.4): the code with its first bit
/// inverted, read as a fraction in two's complement, plus a half step and
/// scaled so that the steps fill the range, which comes to
/// (2 code + 1 - levels) / levels.
fn dequantise(code: u32, levels: u32) -> f32 {
    ((2.0 * f64::from(code) + 1.0 - f64::from(levels)) / f64::from(levels)) as f32
}

/// The bits a codeword of a quantiser of `levels` levels takes, and how many
/// samples it holds: three in a group for 3, 5 and 9 levels.
fn codeword(levels: u32) -> (u32, bool) {
    match levels {
        3 => (5, true),
        5 => (7, true),
        9 => (10, true),
        _ => (32 - levels.leading_zeros(), false),
    }
}

/// CRC-16 with the polynomial 0x8005 (2.4.3.1), continued from `crc` over
/// `count` bits of `bytes` from bit `start`.
fn crc16_bits(mut crc: u16, bytes: &[u8], start: usize, count: usize) -> u16 {
    for bit in start..start + count {
        let value = (bytes[bit / 8] >> (7 - bit % 8)) & 1;
        let top = (crc >> 15) as u8 & 1;
        crc <<= 1;
        if top ^ value != 0 {
            crc ^= 0x8005;
        }
    }
    crc
}

/// The state of a decoder of layers I and II between frames: the filter
/// banks, and the samples of the last frame.
pub(super) struct Layer12 {
    synthesis: [Synthesis; 2],
    /// The samples of the frame decoded last.
    pub(super) pcm: Box<[[f32; 1_152]; 2]>,
    /// The subband samples of a frame: 36 time slots of 32 subbands.
    subbands: Box<[[[f32; 32]; 36]; 2]>,
}

impl Layer12 {
    pub(super) fn new() -> Self {
        Self {
            synthesis: [Synthesis::new(), Synthesis::new()],
            pcm: Box::new([[0.0; 1_152]; 2]),
            subbands: Box::new([[[0.0; 32]; 36]; 2]),
        }
    }

    /// Forgets the state left by earlier frames, as at the start of a
    /// stream.
    pub(super) fn reset(&mut self) {
        for synthesis in &mut self.synthesis {
            synthesis.reset();
        }
    }

    /// Decodes the whole frame in `frame`, header included, into
    /// [`pcm`](Self::pcm): 384 samples a channel in layer I, 1152 in layer
    /// II. A frame with a CRC that does not match is
    /// [`Corrupt`](Mp3Error::Corrupt), and one too short for what it says
    /// it holds is malformed; neither leaves the filter banks changed.
    pub(super) fn decode(&mut self, header: &FrameHeader, frame: &[u8]) -> Result<(), Mp3Error> {
        let slots = if header.layer == 1 {
            self.layer1(header, frame)?
        } else {
            self.layer2(header, frame)?
        };
        for channel in 0..header.channels() {
            for slot in 0..slots {
                let out = &mut self.pcm[channel][slot * 32..slot * 32 + 32];
                self.synthesis[channel].run(&self.subbands[channel][slot], out);
            }
        }
        Ok(())
    }

    /// The first subband coded once for both channels in joint stereo, or
    /// 32 when every subband is coded for each channel.
    fn bound(header: &FrameHeader) -> usize {
        if header.mode == Mode::JointStereo {
            4 * (usize::from(header.mode_extension) + 1)
        } else {
            32
        }
    }

    /// Checks the CRC, which covers the last two bytes of the header and
    /// the `bits` bits after the CRC.
    fn check(header: &FrameHeader, frame: &[u8], bits: usize) -> Result<(), Mp3Error> {
        if !header.protected {
            return Ok(());
        }
        if frame.len() * 8 < 48 + bits {
            return Err(Mp3Error::Malformed("frame too short"));
        }
        let stored = u16::from_be_bytes([frame[4], frame[5]]);
        let crc = crc16_bits(0xFFFF, frame, 16, 16);
        if crc16_bits(crc, frame, 48, bits) != stored {
            return Err(Mp3Error::Corrupt);
        }
        Ok(())
    }

    /// Reads the subband samples of a layer I frame; returns the time slots
    /// it holds, 12.
    // The loops go over subbands and channels in the order the standard
    // writes them, and index several tables by both at once.
    #[allow(clippy::needless_range_loop)]
    fn layer1(&mut self, header: &FrameHeader, frame: &[u8]) -> Result<usize, Mp3Error> {
        const SHORT: Mp3Error = Mp3Error::Malformed("frame too short");
        let channels = header.channels();
        let bound = Self::bound(header);
        let start = header.side_info_start() * 8;
        let mut bits = BitReader::new(frame, start, frame.len() * 8);

        let mut allocation = [[0u32; 32]; 2];
        for sb in 0..32 {
            for channel in 0..channels {
                allocation[channel][sb] = if sb < bound || channel == 0 {
                    bits.read(4).ok_or(SHORT)?
                } else {
                    allocation[0][sb]
                };
            }
        }
        Self::check(header, frame, bits.position() - start)?;
        if allocation.iter().flatten().any(|&code| code == 15) {
            return Err(Mp3Error::Malformed("a forbidden bit allocation"));
        }

        let mut scales = [[0f32; 32]; 2];
        for sb in 0..32 {
            for channel in 0..channels {
                if allocation[channel][sb] != 0 {
                    scales[channel][sb] = scale_factor(bits.read(6).ok_or(SHORT)?);
                }
            }
        }

        for slot in 0..12 {
            for sb in 0..32 {
                let mut shared = 0.0;
                for channel in 0..channels {
                    let code = allocation[channel][sb];
                    let value = if code == 0 {
                        0.0
                    } else if sb < bound || channel == 0 {
                        let width = code + 1;
                        let levels = (1 << width) - 1;
                        let value = dequantise(bits.read(width).ok_or(SHORT)?, levels);
                        shared = value;
                        value
                    } else {
                        shared
                    };
                    self.subbands[channel][slot][sb] = value * scales[channel][sb];
                }
            }
        }
        Ok(12)
    }

    /// Reads the subband samples of a layer II frame; returns the time
    /// slots it holds, 36.
    #[allow(clippy::needless_range_loop)]
    fn layer2(&mut self, header: &FrameHeader, frame: &[u8]) -> Result<usize, Mp3Error> {
        const SHORT: Mp3Error = Mp3Error::Malformed("frame too short");
        let channels = header.channels();
        let table = allocation_table(header);
        let limit = table.len();
        let bound = Self::bound(header).min(limit);
        let start = header.side_info_start() * 8;
        let mut bits = BitReader::new(frame, start, frame.len() * 8);

        // The number of levels of each subband of each channel.
        let mut levels = [[0u32; 32]; 2];
        for (sb, &(width, row)) in table.iter().enumerate() {
            for channel in 0..channels {
                levels[channel][sb] = if sb < bound || channel == 0 {
                    row[bits.read(width).ok_or(SHORT)? as usize]
                } else {
                    levels[0][sb]
                };
            }
        }
        let mut selection = [[0u32; 32]; 2];
        for sb in 0..limit {
            for channel in 0..channels {
                if levels[channel][sb] != 0 {
                    selection[channel][sb] = bits.read(2).ok_or(SHORT)?;
                }
            }
        }
        Self::check(header, frame, bits.position() - start)?;

        // Three scale factors for each subband, one for each third of the
        // frame; the selection says which of them are the same.
        let mut scales = [[[0f32; 3]; 32]; 2];
        for sb in 0..limit {
            for channel in 0..channels {
                if levels[channel][sb] == 0 {
                    continue;
                }
                let mut read = || bits.read(6).map(scale_factor).ok_or(SHORT);
                scales[channel][sb] = match selection[channel][sb] {
                    0 => [read()?, read()?, read()?],
                    1 => {
                        let (first, last) = (read()?, read()?);
                        [first, first, last]
                    }
                    2 => {
                        let all = read()?;
                        [all; 3]
                    }
                    _ => {
                        let (first, last) = (read()?, read()?);
                        [first, last, last]
                    }
                };
            }
        }

        // Twelve granules of three samples a subband.
        let subbands = &mut self.subbands;
        for granule in 0..12 {
            let part = granule / 4;
            for sb in 0..32 {
                let mut shared = [0.0f32; 3];
                for channel in 0..channels {
                    let level = if sb < limit { levels[channel][sb] } else { 0 };
                    let values = if level == 0 {
                        [0.0; 3]
                    } else if sb < bound || channel == 0 {
                        let (width, grouped) = codeword(level);
                        let mut values = [0.0; 3];
                        if grouped {
                            let mut code = bits.read(width).ok_or(SHORT)?;
                            for value in &mut values {
                                *value = dequantise(code % level, level);
                                code /= level;
                            }
                        } else {
                            for value in &mut values {
                                *value = dequantise(bits.read(width).ok_or(SHORT)?, level);
                            }
                        }
                        shared = values;
                        values
                    } else {
                        shared
                    };
                    let scale = scales[channel][sb][part];
                    for (index, value) in values.into_iter().enumerate() {
                        subbands[channel][granule * 3 + index][sb] = value * scale;
                    }
                }
            }
        }
        Ok(36)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantisers_are_symmetric_and_fill_the_range() {
        for levels in [3, 5, 7, 9, 15, 31, 63, 127, 255, 65_535] {
            let lowest = dequantise(0, levels);
            let highest = dequantise(levels - 1, levels);
            assert_eq!(lowest, -highest, "{levels}");
            assert!((highest - (levels - 1) as f32 / levels as f32).abs() < 1e-7);
            // An odd number of levels has a step at zero.
            assert_eq!(dequantise(levels / 2, levels), 0.0);
        }
        // The examples the standard's formula gives for three bits: the
        // code with its first bit inverted as a fraction, plus a quarter,
        // times 8/7.
        for code in 0..7u32 {
            let fraction = (code as f64 - 4.0) / 4.0;
            let expected = (8.0 / 7.0 * (fraction + 0.25)) as f32;
            assert!((dequantise(code, 7) - expected).abs() < 1e-7);
        }
    }

    #[test]
    fn codewords_and_scale_factors_follow_the_tables() {
        assert_eq!(codeword(3), (5, true));
        assert_eq!(codeword(5), (7, true));
        assert_eq!(codeword(9), (10, true));
        assert_eq!(codeword(7), (3, false));
        assert_eq!(codeword(65_535), (16, false));
        assert_eq!(scale_factor(0), 2.0);
        assert_eq!(scale_factor(3), 1.0);
        assert!((scale_factor(1) - 1.587_401).abs() < 1e-6);
        assert_eq!(scale_factor(62), 2f64.powf(1.0 - 62.0 / 3.0) as f32);
        // Every table holds as many codes as its widths can say.
        for table in [&TABLE_A[..], &TABLE_B, &TABLE_C, &TABLE_D, &TABLE_LSF] {
            for &(width, row) in table {
                assert!(row[..1 << width].iter().skip(1).all(|&levels| levels > 0));
            }
        }
        assert_eq!(
            (TABLE_A.len(), TABLE_B.len(), TABLE_C.len(), TABLE_D.len()),
            (27, 30, 8, 12)
        );
    }

    #[test]
    fn the_bit_crc_matches_the_byte_crc() {
        let bytes = b"123456789";
        let whole = crc16_bits(0xFFFF, bytes, 0, 72);
        assert_eq!(whole, 0xAEE7);
        let split = crc16_bits(crc16_bits(0xFFFF, bytes, 0, 13), bytes, 13, 59);
        assert_eq!(split, whole);
    }
}
