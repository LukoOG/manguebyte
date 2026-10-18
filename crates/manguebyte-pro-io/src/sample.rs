//! The sample types a driver's buffers can hold, and their conversion to
//! and from `f32`.

use std::fmt;

use crate::sys;

/// How a driver stores the samples of one channel, as `getChannelInfo`
/// reports it.
///
/// Integer types are full scale at ±1.0. The 32-bit containers with 16,
/// 18, 20 or 24 bits hold the sample in their low bits, right aligned and
/// sign extended. The 24-bit types are packed: three bytes a sample.
/// "MSB" types store the most significant byte first (big endian), "LSB"
/// types the least significant first (little endian). The DSD types are
/// one-bit streams, which this crate does not convert.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleType {
    /// 16-bit integer, big endian.
    Int16Msb,
    /// Packed 24-bit integer, big endian.
    Int24Msb,
    /// 32-bit integer, big endian.
    Int32Msb,
    /// IEEE 754 single, big endian.
    Float32Msb,
    /// IEEE 754 double, big endian.
    Float64Msb,
    /// 16 bits right aligned in a big-endian 32-bit word.
    Int32Msb16,
    /// 18 bits right aligned in a big-endian 32-bit word.
    Int32Msb18,
    /// 20 bits right aligned in a big-endian 32-bit word.
    Int32Msb20,
    /// 24 bits right aligned in a big-endian 32-bit word.
    Int32Msb24,
    /// 16-bit integer, little endian.
    Int16Lsb,
    /// Packed 24-bit integer, little endian.
    Int24Lsb,
    /// 32-bit integer, little endian.
    Int32Lsb,
    /// IEEE 754 single, little endian: what most drivers on x86 use.
    Float32Lsb,
    /// IEEE 754 double, little endian.
    Float64Lsb,
    /// 16 bits right aligned in a little-endian 32-bit word.
    Int32Lsb16,
    /// 18 bits right aligned in a little-endian 32-bit word.
    Int32Lsb18,
    /// 20 bits right aligned in a little-endian 32-bit word.
    Int32Lsb20,
    /// 24 bits right aligned in a little-endian 32-bit word.
    Int32Lsb24,
    /// DSD, eight one-bit samples a byte, the first in the least
    /// significant bit. Not converted.
    DsdInt8Lsb1,
    /// DSD, eight one-bit samples a byte, the first in the most
    /// significant bit. Not converted.
    DsdInt8Msb1,
    /// DSD, one 8-bit sample a byte. Not converted.
    DsdInt8Ner8,
    /// A code the SDK does not define.
    Unknown(i32),
}

/// A sample type this crate cannot convert: DSD, or a code the SDK does not
/// define.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedSampleType(pub SampleType);

impl fmt::Display for UnsupportedSampleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the sample type {} cannot be converted", self.0)
    }
}

impl std::error::Error for UnsupportedSampleType {}

/// How samples of an integer or float type sit in memory.
#[derive(Clone, Copy)]
enum Layout {
    /// An integer of `bits` bits, right aligned and sign extended in a
    /// container of `bytes` bytes; full scale is 2^(bits - 1).
    Int {
        bytes: usize,
        bits: u32,
        big: bool,
    },
    Float32 {
        big: bool,
    },
    Float64 {
        big: bool,
    },
}

impl SampleType {
    /// Every type the SDK defines.
    pub const ALL: [SampleType; 21] = [
        Self::Int16Msb,
        Self::Int24Msb,
        Self::Int32Msb,
        Self::Float32Msb,
        Self::Float64Msb,
        Self::Int32Msb16,
        Self::Int32Msb18,
        Self::Int32Msb20,
        Self::Int32Msb24,
        Self::Int16Lsb,
        Self::Int24Lsb,
        Self::Int32Lsb,
        Self::Float32Lsb,
        Self::Float64Lsb,
        Self::Int32Lsb16,
        Self::Int32Lsb18,
        Self::Int32Lsb20,
        Self::Int32Lsb24,
        Self::DsdInt8Lsb1,
        Self::DsdInt8Msb1,
        Self::DsdInt8Ner8,
    ];

    /// The type of an `ASIOSampleType` code.
    pub fn from_raw(code: i32) -> Self {
        match code {
            sys::ST_INT16_MSB => Self::Int16Msb,
            sys::ST_INT24_MSB => Self::Int24Msb,
            sys::ST_INT32_MSB => Self::Int32Msb,
            sys::ST_FLOAT32_MSB => Self::Float32Msb,
            sys::ST_FLOAT64_MSB => Self::Float64Msb,
            sys::ST_INT32_MSB16 => Self::Int32Msb16,
            sys::ST_INT32_MSB18 => Self::Int32Msb18,
            sys::ST_INT32_MSB20 => Self::Int32Msb20,
            sys::ST_INT32_MSB24 => Self::Int32Msb24,
            sys::ST_INT16_LSB => Self::Int16Lsb,
            sys::ST_INT24_LSB => Self::Int24Lsb,
            sys::ST_INT32_LSB => Self::Int32Lsb,
            sys::ST_FLOAT32_LSB => Self::Float32Lsb,
            sys::ST_FLOAT64_LSB => Self::Float64Lsb,
            sys::ST_INT32_LSB16 => Self::Int32Lsb16,
            sys::ST_INT32_LSB18 => Self::Int32Lsb18,
            sys::ST_INT32_LSB20 => Self::Int32Lsb20,
            sys::ST_INT32_LSB24 => Self::Int32Lsb24,
            sys::ST_DSD_INT8_LSB1 => Self::DsdInt8Lsb1,
            sys::ST_DSD_INT8_MSB1 => Self::DsdInt8Msb1,
            sys::ST_DSD_INT8_NER8 => Self::DsdInt8Ner8,
            other => Self::Unknown(other),
        }
    }

    /// The `ASIOSampleType` code.
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Int16Msb => sys::ST_INT16_MSB,
            Self::Int24Msb => sys::ST_INT24_MSB,
            Self::Int32Msb => sys::ST_INT32_MSB,
            Self::Float32Msb => sys::ST_FLOAT32_MSB,
            Self::Float64Msb => sys::ST_FLOAT64_MSB,
            Self::Int32Msb16 => sys::ST_INT32_MSB16,
            Self::Int32Msb18 => sys::ST_INT32_MSB18,
            Self::Int32Msb20 => sys::ST_INT32_MSB20,
            Self::Int32Msb24 => sys::ST_INT32_MSB24,
            Self::Int16Lsb => sys::ST_INT16_LSB,
            Self::Int24Lsb => sys::ST_INT24_LSB,
            Self::Int32Lsb => sys::ST_INT32_LSB,
            Self::Float32Lsb => sys::ST_FLOAT32_LSB,
            Self::Float64Lsb => sys::ST_FLOAT64_LSB,
            Self::Int32Lsb16 => sys::ST_INT32_LSB16,
            Self::Int32Lsb18 => sys::ST_INT32_LSB18,
            Self::Int32Lsb20 => sys::ST_INT32_LSB20,
            Self::Int32Lsb24 => sys::ST_INT32_LSB24,
            Self::DsdInt8Lsb1 => sys::ST_DSD_INT8_LSB1,
            Self::DsdInt8Msb1 => sys::ST_DSD_INT8_MSB1,
            Self::DsdInt8Ner8 => sys::ST_DSD_INT8_NER8,
            Self::Unknown(code) => code,
        }
    }

    fn layout(self) -> Option<Layout> {
        let int = |bytes, bits, big| Some(Layout::Int { bytes, bits, big });
        match self {
            Self::Int16Msb => int(2, 16, true),
            Self::Int24Msb => int(3, 24, true),
            Self::Int32Msb => int(4, 32, true),
            Self::Int32Msb16 => int(4, 16, true),
            Self::Int32Msb18 => int(4, 18, true),
            Self::Int32Msb20 => int(4, 20, true),
            Self::Int32Msb24 => int(4, 24, true),
            Self::Int16Lsb => int(2, 16, false),
            Self::Int24Lsb => int(3, 24, false),
            Self::Int32Lsb => int(4, 32, false),
            Self::Int32Lsb16 => int(4, 16, false),
            Self::Int32Lsb18 => int(4, 18, false),
            Self::Int32Lsb20 => int(4, 20, false),
            Self::Int32Lsb24 => int(4, 24, false),
            Self::Float32Msb => Some(Layout::Float32 { big: true }),
            Self::Float32Lsb => Some(Layout::Float32 { big: false }),
            Self::Float64Msb => Some(Layout::Float64 { big: true }),
            Self::Float64Lsb => Some(Layout::Float64 { big: false }),
            Self::DsdInt8Lsb1 | Self::DsdInt8Msb1 | Self::DsdInt8Ner8 | Self::Unknown(_) => None,
        }
    }

    /// Whether this crate converts the type: every PCM type, no DSD.
    pub fn is_supported(self) -> bool {
        self.layout().is_some()
    }

    /// Whether this is one of the DSD types.
    pub fn is_dsd(self) -> bool {
        matches!(
            self,
            Self::DsdInt8Lsb1 | Self::DsdInt8Msb1 | Self::DsdInt8Ner8
        )
    }

    /// Bytes one sample takes in the driver's buffer, for the types this
    /// crate converts.
    pub fn bytes_per_sample(self) -> Option<usize> {
        Some(match self.layout()? {
            Layout::Int { bytes, .. } => bytes,
            Layout::Float32 { .. } => 4,
            Layout::Float64 { .. } => 8,
        })
    }

    /// Bytes `frames` samples take in the driver's buffer, DSD included
    /// (eight samples a byte for the one-bit types). `None` for an unknown
    /// type.
    pub fn buffer_bytes(self, frames: usize) -> Option<usize> {
        match self {
            Self::DsdInt8Lsb1 | Self::DsdInt8Msb1 => Some(frames.div_ceil(8)),
            Self::DsdInt8Ner8 => Some(frames),
            other => Some(other.bytes_per_sample()? * frames),
        }
    }

    /// The resolution, in bits.
    pub fn bits(self) -> Option<u32> {
        Some(match self.layout()? {
            Layout::Int { bits, .. } => bits,
            Layout::Float32 { .. } => 32,
            Layout::Float64 { .. } => 64,
        })
    }

    /// Converts samples from the driver's buffer `bytes` into `out`, one
    /// sample per element of `out`. Real-time safe.
    ///
    /// # Panics
    ///
    /// Panics if `bytes` holds fewer than `out.len()` samples.
    pub fn read(self, bytes: &[u8], out: &mut [f32]) -> Result<(), UnsupportedSampleType> {
        let layout = self.layout().ok_or(UnsupportedSampleType(self))?;
        match layout {
            Layout::Int {
                bytes: 2,
                bits,
                big,
            } => {
                let scale = scale(bits) as f32;
                for (chunk, sample) in exact(bytes, 2, out.len()).zip(out) {
                    let pair = [chunk[0], chunk[1]];
                    let value = if big {
                        i16::from_be_bytes(pair)
                    } else {
                        i16::from_le_bytes(pair)
                    };
                    *sample = f32::from(value) / scale;
                }
            }
            Layout::Int {
                bytes: 3,
                bits,
                big,
            } => {
                let scale = scale(bits) as f32;
                for (chunk, sample) in exact(bytes, 3, out.len()).zip(out) {
                    let word = if big {
                        [chunk[0], chunk[1], chunk[2], 0]
                    } else {
                        [chunk[2], chunk[1], chunk[0], 0]
                    };
                    // The top three bytes, shifted down with their sign.
                    let value = i32::from_be_bytes(word) >> 8;
                    *sample = value as f32 / scale;
                }
            }
            Layout::Int { bits, big, .. } => {
                let scale = scale(bits);
                for (chunk, sample) in exact(bytes, 4, out.len()).zip(out) {
                    let value = word32(chunk, big) as i32;
                    // In a wider container, the sign is extended from the
                    // top bit of the sample, whatever the bits above it say.
                    let value = (value << (32 - bits)) >> (32 - bits);
                    *sample = (f64::from(value) / scale) as f32;
                }
            }
            Layout::Float32 { big } => {
                for (chunk, sample) in exact(bytes, 4, out.len()).zip(out) {
                    *sample = f32::from_bits(word32(chunk, big));
                }
            }
            Layout::Float64 { big } => {
                for (chunk, sample) in exact(bytes, 8, out.len()).zip(out) {
                    let mut word = [0u8; 8];
                    word.copy_from_slice(chunk);
                    let value = if big {
                        f64::from_be_bytes(word)
                    } else {
                        f64::from_le_bytes(word)
                    };
                    *sample = value as f32;
                }
            }
        }
        Ok(())
    }

    /// Converts `input` into the driver's buffer `bytes`, one sample per
    /// element of `input`. Integers are rounded to the nearest step and
    /// clipped to full scale; not-a-number becomes silence. Real-time
    /// safe.
    ///
    /// # Panics
    ///
    /// Panics if `bytes` has room for fewer than `input.len()` samples.
    pub fn write(self, input: &[f32], bytes: &mut [u8]) -> Result<(), UnsupportedSampleType> {
        let layout = self.layout().ok_or(UnsupportedSampleType(self))?;
        match layout {
            Layout::Int {
                bytes: size,
                bits,
                big,
            } => {
                let scale = scale(bits);
                let (low, high) = (-scale, scale - 1.0);
                for (chunk, &sample) in exact_mut(bytes, size, input.len()).zip(input) {
                    // `as` turns not-a-number into zero.
                    let value = (f64::from(sample) * scale).round().clamp(low, high) as i32;
                    match size {
                        2 => {
                            let value = value as i16;
                            chunk.copy_from_slice(&if big {
                                value.to_be_bytes()
                            } else {
                                value.to_le_bytes()
                            });
                        }
                        3 => {
                            let word = value.to_be_bytes();
                            let packed = if big {
                                [word[1], word[2], word[3]]
                            } else {
                                [word[3], word[2], word[1]]
                            };
                            chunk.copy_from_slice(&packed);
                        }
                        _ => {
                            chunk.copy_from_slice(&if big {
                                value.to_be_bytes()
                            } else {
                                value.to_le_bytes()
                            });
                        }
                    }
                }
            }
            Layout::Float32 { big } => {
                for (chunk, &sample) in exact_mut(bytes, 4, input.len()).zip(input) {
                    chunk.copy_from_slice(&if big {
                        sample.to_be_bytes()
                    } else {
                        sample.to_le_bytes()
                    });
                }
            }
            Layout::Float64 { big } => {
                for (chunk, &sample) in exact_mut(bytes, 8, input.len()).zip(input) {
                    let value = f64::from(sample);
                    chunk.copy_from_slice(&if big {
                        value.to_be_bytes()
                    } else {
                        value.to_le_bytes()
                    });
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for SampleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(code) => write!(f, "unknown ({code})"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// 2^(bits - 1): full scale of a signed integer of `bits` bits.
fn scale(bits: u32) -> f64 {
    f64::from(1u32 << (bits - 1))
}

fn word32(chunk: &[u8], big: bool) -> u32 {
    let word = [chunk[0], chunk[1], chunk[2], chunk[3]];
    if big {
        u32::from_be_bytes(word)
    } else {
        u32::from_le_bytes(word)
    }
}

fn exact(bytes: &[u8], size: usize, samples: usize) -> std::slice::ChunksExact<'_, u8> {
    bytes[..samples * size].chunks_exact(size)
}

fn exact_mut(bytes: &mut [u8], size: usize, samples: usize) -> std::slice::ChunksExactMut<'_, u8> {
    bytes[..samples * size].chunks_exact_mut(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_and_only_pcm_converts() {
        for ty in SampleType::ALL {
            assert_eq!(SampleType::from_raw(ty.to_raw()), ty);
            assert_eq!(ty.is_supported(), !ty.is_dsd(), "{ty}");
        }
        assert_eq!(SampleType::from_raw(99), SampleType::Unknown(99));
        assert!(!SampleType::Unknown(99).is_supported());
        assert_eq!(SampleType::DsdInt8Lsb1.buffer_bytes(64), Some(8));
        assert_eq!(SampleType::DsdInt8Ner8.buffer_bytes(64), Some(64));
        assert_eq!(SampleType::Int24Lsb.buffer_bytes(64), Some(192));
        assert_eq!(SampleType::Unknown(99).buffer_bytes(64), None);
        let mut out = [0.0f32; 4];
        assert_eq!(
            SampleType::DsdInt8Msb1.read(&[0; 4], &mut out),
            Err(UnsupportedSampleType(SampleType::DsdInt8Msb1))
        );
        assert!(SampleType::DsdInt8Ner8.write(&out, &mut [0; 4]).is_err());
    }

    #[test]
    fn scales_are_powers_of_two() {
        assert_eq!(scale(16), 32_768.0);
        assert_eq!(scale(18), 131_072.0);
        assert_eq!(scale(20), 524_288.0);
        assert_eq!(scale(24), 8_388_608.0);
        assert_eq!(scale(32), 2_147_483_648.0);
    }

    /// Known bit patterns, written out by hand from the SDK's definitions,
    /// for a half-scale positive and a full-scale negative sample.
    #[test]
    fn known_patterns_decode_and_encode() {
        let cases: [(SampleType, &[u8]); 18] = [
            (SampleType::Int16Lsb, &[0x00, 0x40, 0x00, 0x80]),
            (SampleType::Int16Msb, &[0x40, 0x00, 0x80, 0x00]),
            (SampleType::Int24Lsb, &[0x00, 0x00, 0x40, 0x00, 0x00, 0x80]),
            (SampleType::Int24Msb, &[0x40, 0x00, 0x00, 0x80, 0x00, 0x00]),
            (
                SampleType::Int32Lsb,
                &[0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x80],
            ),
            (
                SampleType::Int32Msb,
                &[0x40, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00],
            ),
            (
                SampleType::Int32Lsb16,
                &[0x00, 0x40, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff],
            ),
            (
                SampleType::Int32Msb16,
                &[0x00, 0x00, 0x40, 0x00, 0xff, 0xff, 0x80, 0x00],
            ),
            (
                SampleType::Int32Lsb18,
                &[0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xfe, 0xff],
            ),
            (
                SampleType::Int32Msb18,
                &[0x00, 0x01, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00],
            ),
            (
                SampleType::Int32Lsb20,
                &[0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0xf8, 0xff],
            ),
            (
                SampleType::Int32Msb20,
                &[0x00, 0x04, 0x00, 0x00, 0xff, 0xf8, 0x00, 0x00],
            ),
            (
                SampleType::Int32Lsb24,
                &[0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x80, 0xff],
            ),
            (
                SampleType::Int32Msb24,
                &[0x00, 0x40, 0x00, 0x00, 0xff, 0x80, 0x00, 0x00],
            ),
            (
                SampleType::Float32Lsb,
                &[0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x80, 0xbf],
            ),
            (
                SampleType::Float32Msb,
                &[0x3f, 0x00, 0x00, 0x00, 0xbf, 0x80, 0x00, 0x00],
            ),
            (
                SampleType::Float64Lsb,
                &[
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0xf0, 0xbf,
                ],
            ),
            (
                SampleType::Float64Msb,
                &[
                    0x3f, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xbf, 0xf0, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00,
                ],
            ),
        ];
        for (ty, bytes) in cases {
            let mut decoded = [9.0f32; 2];
            ty.read(bytes, &mut decoded).unwrap();
            assert_eq!(decoded, [0.5, -1.0], "{ty}");
            let mut encoded = vec![0x55u8; bytes.len()];
            ty.write(&decoded, &mut encoded).unwrap();
            assert_eq!(encoded, bytes, "{ty}");
        }
    }

    #[test]
    fn a_right_aligned_sample_ignores_the_bits_above_it() {
        // 0x7f in the top byte would be a large positive 32-bit number, but
        // only the low 24 bits are the sample: -1.0.
        let mut out = [0.0f32];
        SampleType::Int32Lsb24
            .read(&[0x00, 0x00, 0x80, 0x7f], &mut out)
            .unwrap();
        assert_eq!(out, [-1.0]);
    }

    #[test]
    fn writing_rounds_clips_and_silences_nan() {
        let mut bytes = [0u8; 8];
        SampleType::Int16Lsb
            .write(&[2.0, -3.0, f32::NAN, 1.0 / 65_536.0], &mut bytes)
            .unwrap();
        assert_eq!(bytes, [0xff, 0x7f, 0x00, 0x80, 0x00, 0x00, 0x01, 0x00]);
        let mut bytes = [0u8; 4];
        SampleType::Int32Lsb.write(&[1.0], &mut bytes).unwrap();
        assert_eq!(i32::from_le_bytes(bytes), i32::MAX);
    }

    /// Every integer step survives a trip through `f32` for the types of 24
    /// bits or fewer, and every `f32` in range survives a trip through each
    /// type to within its step.
    #[test]
    fn conversions_round_trip() {
        for ty in SampleType::ALL.into_iter().filter(|ty| ty.is_supported()) {
            let size = ty.bytes_per_sample().unwrap();
            let bits = ty.bits().unwrap();
            let values: Vec<f32> = (0..=2_000)
                .map(|step| step as f32 / 1_000.0 - 1.0)
                .chain([0.0, -0.0, 1.0, -1.0, 1e-7, -1e-7])
                .collect();
            let mut bytes = vec![0u8; values.len() * size];
            ty.write(&values, &mut bytes).unwrap();
            let mut back = vec![0.0f32; values.len()];
            ty.read(&bytes, &mut back).unwrap();
            let step = match ty {
                SampleType::Float32Lsb | SampleType::Float32Msb => 0.0,
                SampleType::Float64Lsb | SampleType::Float64Msb => 0.0,
                _ => 1.0 / scale(bits) as f32,
            };
            for (value, back) in values.iter().zip(&back) {
                // Full scale positive clips to the step below it.
                let allowed = if *value >= 1.0 { step } else { step * 0.5 };
                assert!(
                    (value - back).abs() <= allowed + f32::EPSILON * value.abs(),
                    "{ty}: {value} came back as {back}"
                );
            }
            // Integer steps are exact through `f32` up to 24 bits.
            if bits <= 24 && matches!(ty.layout(), Some(Layout::Int { .. })) {
                let mut again = vec![0u8; bytes.len()];
                ty.write(&back, &mut again).unwrap();
                assert_eq!(again, bytes, "{ty}");
            }
        }
    }
}
