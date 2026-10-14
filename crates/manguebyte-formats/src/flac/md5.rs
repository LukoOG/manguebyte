//! MD5, as RFC 1321 defines it.
//!
//! FLAC's stream information carries the MD5 digest of the audio before it
//! was encoded, so that a decoder can tell that what it gives back is what
//! went in. This is the one place the crate needs a hash, and MD5 is short
//! enough to write out here rather than to take a dependency for it.
//!
//! MD5 is long broken as a defence against someone who wants a collision.
//! Here it only detects accidental damage and coding errors, which it still
//! does well, and the format leaves no choice.

/// The constant added in each of the 64 steps: the integer part of
/// `2^32 * |sin(i)|` for `i` from 1 to 64, in radians (RFC 1321, section
/// 3.4). A test checks the table against that definition.
const SINES: [u32; 64] = [
    0xD76A_A478,
    0xE8C7_B756,
    0x2420_70DB,
    0xC1BD_CEEE,
    0xF57C_0FAF,
    0x4787_C62A,
    0xA830_4613,
    0xFD46_9501,
    0x6980_98D8,
    0x8B44_F7AF,
    0xFFFF_5BB1,
    0x895C_D7BE,
    0x6B90_1122,
    0xFD98_7193,
    0xA679_438E,
    0x49B4_0821,
    0xF61E_2562,
    0xC040_B340,
    0x265E_5A51,
    0xE9B6_C7AA,
    0xD62F_105D,
    0x0244_1453,
    0xD8A1_E681,
    0xE7D3_FBC8,
    0x21E1_CDE6,
    0xC337_07D6,
    0xF4D5_0D87,
    0x455A_14ED,
    0xA9E3_E905,
    0xFCEF_A3F8,
    0x676F_02D9,
    0x8D2A_4C8A,
    0xFFFA_3942,
    0x8771_F681,
    0x6D9D_6122,
    0xFDE5_380C,
    0xA4BE_EA44,
    0x4BDE_CFA9,
    0xF6BB_4B60,
    0xBEBF_BC70,
    0x289B_7EC6,
    0xEAA1_27FA,
    0xD4EF_3085,
    0x0488_1D05,
    0xD9D4_D039,
    0xE6DB_99E5,
    0x1FA2_7CF8,
    0xC4AC_5665,
    0xF429_2244,
    0x432A_FF97,
    0xAB94_23A7,
    0xFC93_A039,
    0x655B_59C3,
    0x8F0C_CC92,
    0xFFEF_F47D,
    0x8584_5DD1,
    0x6FA8_7E4F,
    0xFE2C_E6E0,
    0xA301_4314,
    0x4E08_11A1,
    0xF753_7E82,
    0xBD3A_F235,
    0x2AD7_D2BB,
    0xEB86_D391,
];

/// How far each step rotates, four values per round (RFC 1321, section
/// 3.4).
const ROTATIONS: [[u32; 4]; 4] = [
    [7, 12, 17, 22],
    [5, 9, 14, 20],
    [4, 11, 16, 23],
    [6, 10, 15, 21],
];

/// An MD5 digest being computed over data given in pieces of any size.
///
/// It holds a fixed amount of state and never allocates.
#[derive(Clone)]
pub(crate) struct Md5 {
    state: [u32; 4],
    /// The bytes of a block of 64 that has not been completed yet.
    pending: [u8; 64],
    pending_len: usize,
    /// Bytes taken in so far, of which the message length is made.
    length: u64,
}

impl Md5 {
    /// A digest of nothing yet, with the starting values of RFC 1321,
    /// section 3.3.
    pub(crate) fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476],
            pending: [0; 64],
            pending_len: 0,
            length: 0,
        }
    }

    /// Takes in more of the message.
    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.pending_len > 0 {
            let take = (64 - self.pending_len).min(bytes.len());
            self.pending[self.pending_len..self.pending_len + take].copy_from_slice(&bytes[..take]);
            self.pending_len += take;
            bytes = &bytes[take..];
            if self.pending_len < 64 {
                return;
            }
            let block = self.pending;
            self.compress(&block);
            self.pending_len = 0;
        }
        let mut blocks = bytes.chunks_exact(64);
        for block in &mut blocks {
            // `chunks_exact` gives 64 bytes, so the conversion cannot fail.
            if let Ok(block) = <&[u8; 64]>::try_from(block) {
                self.compress(block);
            }
        }
        let rest = blocks.remainder();
        self.pending[..rest.len()].copy_from_slice(rest);
        self.pending_len = rest.len();
    }

    /// Completes the message and returns its digest.
    ///
    /// The message is padded with a one bit, zeros up to 8 bytes short of a
    /// whole block, and its length in bits, least significant byte first
    /// (RFC 1321, sections 3.1 and 3.2).
    pub(crate) fn finish(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let mut tail = [0u8; 72];
        tail[0] = 0x80;
        let zeros = (55usize.wrapping_sub(self.pending_len)) % 64;
        let tail_len = 1 + zeros + 8;
        tail[1 + zeros..tail_len].copy_from_slice(&bits.to_le_bytes());
        // The padding is not part of the length that was just written.
        let length = self.length;
        self.update(&tail[..tail_len]);
        self.length = length;
        debug_assert_eq!(self.pending_len, 0);

        let mut digest = [0u8; 16];
        for (bytes, word) in digest.chunks_exact_mut(4).zip(self.state) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        digest
    }

    /// Runs the 64 steps of RFC 1321, section 3.4, on one block.
    fn compress(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for step in 0..64 {
            let round = step / 16;
            // The round's function of b, c and d, and which word it takes.
            let (mixed, word) = match round {
                0 => ((b & c) | (!b & d), step),
                1 => ((b & d) | (c & !d), (5 * step + 1) % 16),
                2 => (b ^ c ^ d, (3 * step + 5) % 16),
                _ => (c ^ (b | !d), (7 * step) % 16),
            };
            let sum = a
                .wrapping_add(mixed)
                .wrapping_add(SINES[step])
                .wrapping_add(words[word]);
            let rotated = b.wrapping_add(sum.rotate_left(ROTATIONS[round][step % 4]));
            // The four registers take turns: what was d becomes a.
            a = d;
            d = c;
            c = b;
            b = rotated;
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d]) {
            *state = state.wrapping_add(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(digest: [u8; 16]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn md5(bytes: &[u8]) -> String {
        let mut hash = Md5::new();
        hash.update(bytes);
        hex(hash.finish())
    }

    #[test]
    fn the_test_suite_of_rfc_1321_passes() {
        // Appendix A.5 of the RFC.
        for (message, digest) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(md5(message.as_bytes()), digest, "{message:?}");
        }
    }

    #[test]
    fn the_constants_are_the_sines_the_rfc_defines() {
        for (index, &constant) in SINES.iter().enumerate() {
            let exact = ((index + 1) as f64).sin().abs() * 4_294_967_296.0;
            assert_eq!(constant, exact as u32, "constant {index}");
        }
    }

    #[test]
    fn the_digest_does_not_depend_on_how_the_message_is_cut() {
        // Lengths around the block size and the point where the length no
        // longer fits in the last block, cut at every place and in pieces.
        let message: Vec<u8> = (0..300u32).map(|index| (index * 131 % 251) as u8).collect();
        for len in [0, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128, 300] {
            let whole = md5(&message[..len]);
            for cut in 0..=len {
                let mut hash = Md5::new();
                hash.update(&message[..cut]);
                hash.update(&message[cut..len]);
                assert_eq!(hex(hash.finish()), whole, "length {len} cut at {cut}");
            }
            let mut hash = Md5::new();
            for byte in &message[..len] {
                hash.update(std::slice::from_ref(byte));
            }
            assert_eq!(hex(hash.finish()), whole, "length {len} byte by byte");
        }
    }

    #[test]
    fn a_long_message_matches_a_known_digest() {
        // A million times the letter a, a vector given with other
        // implementations of MD5 and easy to check with any tool.
        let block = [b'a'; 1000];
        let mut hash = Md5::new();
        for _ in 0..1000 {
            hash.update(&block);
        }
        assert_eq!(hex(hash.finish()), "7707d6ae4e027c70eea2a935c2296f21");
    }
}
