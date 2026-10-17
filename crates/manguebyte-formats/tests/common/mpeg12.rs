//! Writing MPEG audio layer I and II streams for the tests, from quantised
//! subband samples, as ISO/IEC 11172-3 (2.4.1.5 and 2.4.1.6) lays frames
//! out. Nothing is encoded from audio: the tests choose the codes, so that
//! the same subband samples can be written as layer I and as layer II, in
//! stereo and in joint stereo, with and without a CRC, and the decodes
//! compared.
//!
//! The frames are MPEG-1 at 48 kHz: layer I at 448 kbit/s, layer II at
//! 384 kbit/s, which takes allocation table 3-B.2a (27 subbands).

// The loops go over subbands and channels in the order the standard
// writes them, and index several tables by both at once.
#![allow(clippy::needless_range_loop)]

use super::{BitWriter, Rng};

/// Quantised subband samples: for every time slot, a code for each
/// channel and subband, and for every twelve slots a scale factor index.
pub struct Subbands {
    pub channels: usize,
    /// The levels of each subband's quantiser: 15 below subband 23, 3
    /// from 23 to 26, and none from 27, which table 3-B.2a does not code.
    pub levels: [u32; 32],
    /// `codes[slot][channel][subband]`.
    pub codes: Vec<[[u32; 32]; 2]>,
    /// `scales[slot / 12][channel][subband]`.
    pub scales: Vec<[[u32; 32]; 2]>,
}

impl Subbands {
    /// `frames` layer II frames' worth of random samples, 36 slots each.
    /// Above subband 4 the two channels have the same codes, so that the
    /// stream can be written in joint stereo with any bound; the scale
    /// factors differ. Runs of equal scale factors make every selection
    /// of layer II occur.
    pub fn random(channels: usize, frames: usize, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut levels = [0u32; 32];
        for (sb, level) in levels.iter_mut().enumerate() {
            *level = match sb {
                0..=22 => 15,
                23..=26 => 3,
                _ => 0,
            };
        }
        let mut codes = Vec::new();
        for _ in 0..frames * 36 {
            let mut slot = [[0u32; 32]; 2];
            for sb in 0..32 {
                for channel in 0..channels {
                    slot[channel][sb] = if levels[sb] == 0 {
                        0
                    } else if channel == 1 && sb >= 4 {
                        slot[0][sb]
                    } else {
                        rng.below(u64::from(levels[sb])) as u32
                    };
                }
            }
            codes.push(slot);
        }
        let mut scales = Vec::new();
        for part in 0..frames * 3 {
            let mut scale = [[0u32; 32]; 2];
            for sb in 0..32 {
                for channel in 0..channels {
                    scale[channel][sb] = if part % 3 != 0 && rng.below(2) == 0 {
                        // The same as the part before.
                        let previous: &[[u32; 32]; 2] = scales.last().unwrap();
                        previous[channel][sb]
                    } else {
                        // Loud enough to matter, never so loud it clips.
                        rng.between(8, 40) as u32
                    };
                }
            }
            scales.push(scale);
        }
        Self {
            channels,
            levels,
            codes,
            scales,
        }
    }
}

/// How a stream's channels are coded.
#[derive(Clone, Copy)]
pub enum Mode {
    Mono,
    Stereo,
    /// Joint stereo with this mode extension: the subbands from
    /// `4 * (extension + 1)` on are coded once for both channels.
    Joint(u32),
}

fn header(layer: u32, bitrate_index: u32, mode: Mode, protected: bool) -> [u8; 4] {
    let (mode, extension) = match mode {
        Mode::Stereo => (0, 0),
        Mode::Joint(extension) => (1, extension),
        Mode::Mono => (3, 0),
    };
    // Sync, MPEG-1, the layer, the protection bit (zero when protected),
    // the bitrate, 48 kHz, no padding, then the mode.
    let layer_bits = 4 - layer;
    let word = (0x7FFu32 << 21)
        | (3 << 19)
        | (layer_bits << 17)
        | (u32::from(!protected) << 16)
        | (bitrate_index << 12)
        | (1 << 10)
        | (mode << 6)
        | (extension << 4);
    word.to_be_bytes()
}

fn bound(mode: Mode) -> usize {
    match mode {
        Mode::Joint(extension) => 4 * (extension as usize + 1),
        _ => 32,
    }
}

/// CRC-16 with the polynomial 0x8005 over `count` bits, from `crc`.
fn crc_bits(mut crc: u16, bytes: &[u8], count: usize) -> u16 {
    for bit in 0..count {
        let value = u16::from((bytes[bit / 8] >> (7 - bit % 8)) & 1);
        let top = crc >> 15;
        crc <<= 1;
        if top ^ value != 0 {
            crc ^= 0x8005;
        }
    }
    crc
}

/// Bits written, and how many.
#[derive(Default)]
struct Counted {
    writer: BitWriter,
    bits: usize,
}

impl Counted {
    fn put(&mut self, value: u64, count: u32) {
        self.writer.bits(value, count);
        self.bits += count as usize;
    }
}

/// Puts the header, the CRC of the header's last two bytes and of the
/// first `protected_bits` of `body`, and `body` together into a frame of
/// `len` bytes.
fn frame(
    header: [u8; 4],
    protected: bool,
    body: Counted,
    protected_bits: usize,
    len: usize,
) -> Vec<u8> {
    let mut out = header.to_vec();
    if protected {
        let crc = crc_bits(0xFFFF, &header[2..], 16);
        out.extend_from_slice(&crc_bits(crc, &body.writer.bytes, protected_bits).to_be_bytes());
    }
    assert!(
        out.len() * 8 + body.bits <= len * 8,
        "a frame holds too many bits"
    );
    out.extend_from_slice(&body.writer.bytes);
    out.resize(len, 0);
    out
}

/// The subband samples as layer I: a frame for every twelve slots.
pub fn layer1(subbands: &Subbands, mode: Mode, protected: bool) -> Vec<u8> {
    let channels = subbands.channels;
    let bound = bound(mode);
    // The allocation is the sample width less one: 4 bits for 15 levels,
    // 2 for 3.
    let allocation = |sb: usize| match subbands.levels[sb] {
        0 => 0u64,
        15 => 3,
        _ => 1,
    };
    let mut out = Vec::new();
    for (index, scales) in subbands.scales.iter().enumerate() {
        let mut body = Counted::default();
        for sb in 0..32 {
            for channel in 0..channels {
                if sb < bound || channel == 0 {
                    body.put(allocation(sb), 4);
                }
            }
        }
        let protected_bits = body.bits;
        for sb in 0..32 {
            for channel in 0..channels {
                if allocation(sb) != 0 {
                    body.put(u64::from(scales[channel][sb]), 6);
                }
            }
        }
        for slot in &subbands.codes[index * 12..index * 12 + 12] {
            for sb in 0..32 {
                for channel in 0..channels {
                    if allocation(sb) != 0 && (sb < bound || channel == 0) {
                        body.put(u64::from(slot[channel][sb]), allocation(sb) as u32 + 1);
                    }
                }
            }
        }
        // 448 kbit/s at 48 kHz: 112 slots of four bytes.
        out.extend(frame(
            header(1, 14, mode, protected),
            protected,
            body,
            protected_bits,
            448,
        ));
    }
    out
}

/// The subband samples as layer II: a frame for every 36 slots.
pub fn layer2(subbands: &Subbands, mode: Mode, protected: bool) -> Vec<u8> {
    let channels = subbands.channels;
    let bound = bound(mode).min(27);
    // Table 3-B.2a: the width of the allocation code of each subband and
    // the code for the levels the tests use.
    let allocation = |sb: usize| -> (u64, u32) {
        match (sb, subbands.levels[sb]) {
            (0..=2, 15) => (3, 4),
            (3..=10, 15) => (5, 4),
            (11..=22, 15) => (5, 3),
            (23..=26, 3) => (1, 2),
            _ => unreachable!("levels the table does not have here"),
        }
    };
    let mut out = Vec::new();
    for index in 0..subbands.codes.len() / 36 {
        let parts = &subbands.scales[index * 3..index * 3 + 3];
        let mut body = Counted::default();
        for sb in 0..27 {
            for channel in 0..channels {
                if sb < bound || channel == 0 {
                    let (code, width) = allocation(sb);
                    body.put(code, width);
                }
            }
        }
        // The selection of scale factors: which of the three are the same.
        let selection = |channel: usize, sb: usize| {
            let [a, b, c] = [0, 1, 2].map(|part| parts[part][channel][sb]);
            match (a == b, b == c) {
                (true, true) => 2,
                (true, false) => 1,
                (false, true) => 3,
                (false, false) => 0,
            }
        };
        for sb in 0..27 {
            for channel in 0..channels {
                body.put(selection(channel, sb), 2);
            }
        }
        let protected_bits = body.bits;
        for sb in 0..27 {
            for channel in 0..channels {
                let [a, b, c] = [0, 1, 2].map(|part| u64::from(parts[part][channel][sb]));
                let written: &[u64] = match selection(channel, sb) {
                    0 => &[a, b, c],
                    1 => &[a, c],
                    2 => &[a],
                    _ => &[a, b],
                };
                for &scale in written {
                    body.put(scale, 6);
                }
            }
        }
        let slots = &subbands.codes[index * 36..index * 36 + 36];
        for granule in 0..12 {
            for sb in 0..27 {
                for channel in 0..channels {
                    if sb >= bound && channel == 1 {
                        continue;
                    }
                    let samples = [0, 1, 2].map(|i| u64::from(slots[granule * 3 + i][channel][sb]));
                    if subbands.levels[sb] == 3 {
                        body.put(samples[0] + 3 * samples[1] + 9 * samples[2], 5);
                    } else {
                        for sample in samples {
                            body.put(sample, 4);
                        }
                    }
                }
            }
        }
        // 384 kbit/s at 48 kHz: 1152 bytes.
        out.extend(frame(
            header(2, 14, mode, protected),
            protected,
            body,
            protected_bits,
            1_152,
        ));
    }
    out
}
