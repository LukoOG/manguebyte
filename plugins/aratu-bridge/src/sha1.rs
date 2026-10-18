//! SHA-1 as RFC 3174 describes it, for the one thing the WebSocket
//! handshake uses it for: `Sec-WebSocket-Accept` (RFC 6455 section 4.2.2).
//! It is not used for anything that needs it to resist collisions.

/// The 20-byte SHA-1 digest of `message`.
pub fn digest(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    // The message, a 1 bit, zeros, and its length in bits as a 64-bit
    // big-endian number, to a multiple of 64 bytes (section 4).
    let bits = (message.len() as u64).wrapping_mul(8);
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bits.to_be_bytes());

    let mut w = [0u32; 80];
    for block in padded.chunks_exact(64) {
        for (t, word) in block.chunks_exact(4).enumerate() {
            w[t] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for t in 16..80 {
            w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (t, &word) in w.iter().enumerate() {
            let (f, k) = match t {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(word)
                .wrapping_add(k);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *state = state.wrapping_add(value);
        }
    }
    let mut out = [0u8; 20];
    for (bytes, word) in out.chunks_exact_mut(4).zip(h) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02X}")).collect()
    }

    /// The four tests of RFC 3174 section 7.3.
    #[test]
    fn the_test_vectors_of_rfc_3174() {
        assert_eq!(
            hex(&digest(b"abc")),
            "A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        assert_eq!(
            hex(&digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
        let a = vec![b'a'; if cfg!(miri) { 1_000 } else { 1_000_000 }];
        if !cfg!(miri) {
            assert_eq!(hex(&digest(&a)), "34AA973CD4C4DAA4F61EEB2BDBAD27316534016F");
        }
        let repeated =
            b"0123456701234567012345670123456701234567012345670123456701234567".repeat(10);
        assert_eq!(
            hex(&digest(&repeated)),
            "DEA356A2CDDD90C7A7ECEDC5EBB563934F460452"
        );
    }

    #[test]
    fn the_empty_message_and_the_block_edges() {
        assert_eq!(
            hex(&digest(b"")),
            "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"
        );
        // 55, 56 and 64 bytes put the length in the same block, the next
        // one, and after a whole block.
        for length in [55, 56, 63, 64, 65] {
            let message = vec![0x5a; length];
            assert_ne!(digest(&message), digest(&message[..length - 1]));
        }
    }
}
