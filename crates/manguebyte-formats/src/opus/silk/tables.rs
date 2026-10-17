//! The tables of the SILK decoder, as section 4.2 of RFC 6716 prints them.
//!
//! Probability tables are kept as the RFC gives them, as probability
//! distributions in 256ths; [`icdf`] turns each into the inverse cumulative
//! form the range decoder reads, and a test checks that each sums to 256.

/// Turns a distribution in 256ths into its inverse cumulative form: entry
/// `k` is 256 less the probability of the symbols up to and including `k`.
pub(crate) const fn icdf<const N: usize>(pdf: [u8; N]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut total = 0u32;
    let mut index = 0;
    while index < N {
        total += pdf[index] as u32;
        out[index] = (256 - total) as u8;
        index += 1;
    }
    assert!(total == 256, "a distribution must sum to 256");
    out
}

/// Table 8: whether a stereo frame codes only its mid channel.
pub(crate) const MID_ONLY: [u8; 2] = icdf([192, 64]);

/// Table 9: the type of a frame and its quantisation offset, after an
/// inactive and after an active VAD flag. The first two symbols are
/// inactive, low and high offset; then unvoiced and voiced, each low and
/// high.
pub(crate) const FRAME_TYPE_INACTIVE: [u8; 2] = icdf([26, 230]);
pub(crate) const FRAME_TYPE_ACTIVE: [u8; 4] = icdf([24, 74, 148, 10]);

/// Table 11: the three high bits of a gain coded on its own, by signal
/// type: inactive, unvoiced, voiced.
pub(crate) const GAIN_MSB: [[u8; 8]; 3] = [
    icdf([32, 112, 68, 29, 12, 1, 1, 1]),
    icdf([2, 17, 45, 60, 62, 47, 19, 4]),
    icdf([1, 3, 26, 71, 94, 50, 9, 2]),
];

/// Table 12: its three low bits.
pub(crate) const GAIN_LSB: [u8; 8] = icdf([32; 8]);

/// Table 13: a gain coded against the one before it.
pub(crate) const GAIN_DELTA: [u8; 41] = icdf([
    6, 5, 11, 31, 132, 21, 8, 4, 3, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
]);

/// Table 14: the first stage of the normalized LSFs, by bandwidth
/// (narrow or medium band, then wideband) and by whether the frame is
/// voiced.
pub(crate) const LSF_STAGE1: [[[u8; 32]; 2]; 2] = [
    [
        icdf([
            44, 34, 30, 19, 21, 12, 11, 3, 3, 2, 16, 2, 2, 1, 5, 2, 1, 3, 3, 1, 1, 2, 2, 2, 3, 1,
            9, 9, 2, 7, 2, 1,
        ]),
        icdf([
            1, 10, 1, 8, 3, 8, 8, 14, 13, 14, 1, 14, 12, 13, 11, 11, 12, 11, 10, 10, 11, 8, 9, 8,
            7, 8, 1, 1, 6, 1, 6, 5,
        ]),
    ],
    [
        icdf([
            31, 21, 3, 17, 1, 8, 17, 4, 1, 18, 16, 4, 2, 3, 1, 10, 1, 3, 16, 11, 16, 2, 2, 3, 2,
            11, 1, 4, 9, 8, 7, 3,
        ]),
        icdf([
            1, 4, 16, 5, 18, 11, 5, 14, 15, 1, 3, 12, 13, 14, 14, 6, 14, 12, 2, 6, 1, 12, 12, 11,
            10, 3, 10, 5, 1, 1, 1, 3,
        ]),
    ],
];

/// Tables 15 and 16: the second stage, distributions `a` to `h` for
/// narrow and medium band and `i` to `p` for wideband, in that order.
pub(crate) const LSF_STAGE2: [[u8; 9]; 16] = [
    icdf([1, 1, 1, 15, 224, 11, 1, 1, 1]),
    icdf([1, 1, 2, 34, 183, 32, 1, 1, 1]),
    icdf([1, 1, 4, 42, 149, 55, 2, 1, 1]),
    icdf([1, 1, 8, 52, 123, 61, 8, 1, 1]),
    icdf([1, 3, 16, 53, 101, 74, 6, 1, 1]),
    icdf([1, 3, 17, 55, 90, 73, 15, 1, 1]),
    icdf([1, 7, 24, 53, 74, 67, 26, 3, 1]),
    icdf([1, 1, 18, 63, 78, 58, 30, 6, 1]),
    icdf([1, 1, 1, 9, 232, 9, 1, 1, 1]),
    icdf([1, 1, 2, 28, 186, 35, 1, 1, 1]),
    icdf([1, 1, 3, 42, 152, 53, 2, 1, 1]),
    icdf([1, 1, 10, 49, 126, 65, 2, 1, 1]),
    icdf([1, 4, 19, 48, 100, 77, 5, 1, 1]),
    icdf([1, 1, 14, 54, 100, 72, 12, 1, 1]),
    icdf([1, 1, 15, 61, 87, 61, 25, 4, 1]),
    icdf([1, 7, 21, 50, 77, 81, 17, 1, 1]),
];

/// Table 17: which second-stage distribution each coefficient of a narrow
/// or medium band frame takes, by its first-stage index, as the letters the
/// RFC names them by.
pub(crate) const LSF_SELECT_NB: [&[u8; 10]; 32] = [
    b"aaaaaaaaaa",
    b"bdbccbcbbb",
    b"cbbbbbbbbb",
    b"bccccbcbbb",
    b"cddddccccc",
    b"afddccccbb",
    b"accccccccb",
    b"cdgeeefeff",
    b"ceffefegee",
    b"ceehefeffe",
    b"edddcdcccc",
    b"bffgefefff",
    b"chegffffff",
    b"chfffffgfe",
    b"ddfeefefee",
    b"cddffeeeee",
    b"ceegefefff",
    b"cfegfffefe",
    b"chefefefff",
    b"cfeghgfgfe",
    b"dghegffgef",
    b"chgeeefeff",
    b"effeggfgfe",
    b"cffgfgegee",
    b"efffdheffe",
    b"cdeffgeffe",
    b"cdcddecddd",
    b"bbcccccdcc",
    b"effgggfgef",
    b"dffeeeeddc",
    b"cfdhffeefe",
    b"eefefgfgfe",
];

/// Table 18: the same for wideband, letters `i` to `p`.
pub(crate) const LSF_SELECT_WB: [&[u8; 16]; 32] = [
    b"iiiiiiiiiiiiiiii",
    b"klllllkkkkkjjjil",
    b"knnlpmmnknmnnmll",
    b"ikjkkjjjjjiiiiij",
    b"ionmompnmmmnnmml",
    b"ilnnmllnllllllkm",
    b"iiiiiiiiiiiiiiii",
    b"ikolpknlmnnmllkl",
    b"iokoomnmonmmnlll",
    b"kjiiiiiiiiiiiiii",
    b"ijiiiiiiiiiiiiij",
    b"kklmnlllllllkkjl",
    b"kkllmllllllllkjl",
    b"lmmmommnlnmmnmlm",
    b"iomnmpnkonpmmlnl",
    b"ijijjjjjjjiiiiji",
    b"jonpnmnlmnmmmllm",
    b"jllmmllnkllnnnlm",
    b"kllkkklkjkjkjjjm",
    b"iklnllkkkjjiiiii",
    b"lmlnllkkjjjjjkkm",
    b"kolppmnmnlnllkll",
    b"klnoolnlmmllllkm",
    b"jllmmmmlnnnljjjj",
    b"knloompmmnlmmlll",
    b"iojjiiiiiiiiiiii",
    b"ioolnknnlmmppmmm",
    b"llplnmlllkklllkl",
    b"iijiiikjkjjkkkjj",
    b"ilknllklkjiijiij",
    b"lnnmpnllklkkjiji",
    b"klnlmlllkjkomiii",
];

/// Table 19: the extension of a second-stage index at either end of its
/// range.
pub(crate) const LSF_EXTENSION: [u8; 7] = icdf([156, 60, 24, 9, 4, 2, 1]);

/// Table 20: the weights, in Q8, with which each second-stage residual
/// predicts the one below it: `A` and `B` for narrow and medium band, `C`
/// and `D` for wideband.
pub(crate) const LSF_PRED_NB: [[u8; 9]; 2] = [
    [179, 138, 140, 148, 151, 149, 153, 151, 163],
    [116, 67, 82, 59, 92, 72, 100, 89, 92],
];
pub(crate) const LSF_PRED_WB: [[u8; 15]; 2] = [
    [
        175, 148, 160, 176, 178, 173, 174, 164, 177, 174, 196, 182, 198, 192, 182,
    ],
    [
        68, 62, 66, 60, 72, 117, 85, 90, 118, 136, 151, 142, 160, 142, 155,
    ],
];

/// Table 21: which weight each coefficient of a narrow or medium band frame
/// takes, by its first-stage index.
pub(crate) const LSF_PRED_SELECT_NB: [&[u8; 9]; 32] = [
    b"ABAAAAAAA",
    b"BAAAAAAAA",
    b"AAAAAAAAA",
    b"BBBAAAABA",
    b"ABAAAAAAA",
    b"ABAAAAAAA",
    b"BABBAAABA",
    b"ABBAABBAA",
    b"AABBABABB",
    b"AABBAABBB",
    b"AAAAAAAAA",
    b"ABABBBBBA",
    b"ABABBBBBA",
    b"ABBBBBBBA",
    b"BABBABBBB",
    b"ABBBBBABA",
    b"AABBABABA",
    b"AABBBABBB",
    b"ABBAABBBA",
    b"AAABBBABA",
    b"ABBAABABA",
    b"ABBAAABBA",
    b"AAAAABBBB",
    b"AABBAAABB",
    b"AAABABBBB",
    b"ABBBBBBBA",
    b"AAAAAAAAA",
    b"AAAAAAAAA",
    b"AABABBABA",
    b"BAABAAAAA",
    b"AAABBABAB",
    b"BABBABBBB",
];

/// Table 22: the same for wideband.
pub(crate) const LSF_PRED_SELECT_WB: [&[u8; 15]; 32] = [
    b"CCCCCCCCCCCCCCD",
    b"CCCCCCCCCCCCCCC",
    b"CCDCCDDDCDDDDCC",
    b"CCCCCCCCCCCCDCC",
    b"CDDCDCDDCDDDDDC",
    b"CCDCCCCCCCCCCCC",
    b"DCCCCCCCCCCDCDC",
    b"CDDCCCDCDDDCDCD",
    b"CDCDDCDCDCDDDDD",
    b"CCCCCCCCCCCCCCD",
    b"CDCCCCCCCCCCCCC",
    b"CCDCDDDDDDDCDCC",
    b"CCDCCDCDCDCCDCC",
    b"CCCCDDCDCDDDDCC",
    b"CDCCCDDCDDDCDDD",
    b"CCDDCCCCCCCCDDC",
    b"CDDCDCDDDDDCDCC",
    b"CCDCCCCDCCDDDCC",
    b"CCCCCCCCCCCCCCD",
    b"CCCCCCCCCCCCDCC",
    b"CCCCCCCCCCCCCCC",
    b"CDCDCDDCDCDCDDC",
    b"CCDDDDCDDCCDDCC",
    b"CDDCDCDCDCCCCDC",
    b"CCCDDCDCDDDDDDD",
    b"CCCCCCCCCCCCCCD",
    b"CDDCCCDDCCDDDDD",
    b"CCCCCDCDDDDCDDD",
    b"CCCCCCCCCCCCCCD",
    b"CCCCCCCCCCCCCCD",
    b"DCCCCCCCCCCDCCC",
    b"CCDCCDDDCCDCCDC",
];

/// Table 23: the first-stage codebook of narrow and medium band, in Q8.
pub(crate) const LSF_CODEBOOK_NB: [[u8; 10]; 32] = [
    [12, 35, 60, 83, 108, 132, 157, 180, 206, 228],
    [15, 32, 55, 77, 101, 125, 151, 175, 201, 225],
    [19, 42, 66, 89, 114, 137, 162, 184, 209, 230],
    [12, 25, 50, 72, 97, 120, 147, 172, 200, 223],
    [26, 44, 69, 90, 114, 135, 159, 180, 205, 225],
    [13, 22, 53, 80, 106, 130, 156, 180, 205, 228],
    [15, 25, 44, 64, 90, 115, 142, 168, 196, 222],
    [19, 24, 62, 82, 100, 120, 145, 168, 190, 214],
    [22, 31, 50, 79, 103, 120, 151, 170, 203, 227],
    [21, 29, 45, 65, 106, 124, 150, 171, 196, 224],
    [30, 49, 75, 97, 121, 142, 165, 186, 209, 229],
    [19, 25, 52, 70, 93, 116, 143, 166, 192, 219],
    [26, 34, 62, 75, 97, 118, 145, 167, 194, 217],
    [25, 33, 56, 70, 91, 113, 143, 165, 196, 223],
    [21, 34, 51, 72, 97, 117, 145, 171, 196, 222],
    [20, 29, 50, 67, 90, 117, 144, 168, 197, 221],
    [22, 31, 48, 66, 95, 117, 146, 168, 196, 222],
    [24, 33, 51, 77, 116, 134, 158, 180, 200, 224],
    [21, 28, 70, 87, 106, 124, 149, 170, 194, 217],
    [26, 33, 53, 64, 83, 117, 152, 173, 204, 225],
    [27, 34, 65, 95, 108, 129, 155, 174, 210, 225],
    [20, 26, 72, 99, 113, 131, 154, 176, 200, 219],
    [34, 43, 61, 78, 93, 114, 155, 177, 205, 229],
    [23, 29, 54, 97, 124, 138, 163, 179, 209, 229],
    [30, 38, 56, 89, 118, 129, 158, 178, 200, 231],
    [21, 29, 49, 63, 85, 111, 142, 163, 193, 222],
    [27, 48, 77, 103, 133, 158, 179, 196, 215, 232],
    [29, 47, 74, 99, 124, 151, 176, 198, 220, 237],
    [33, 42, 61, 76, 93, 121, 155, 174, 207, 225],
    [29, 53, 87, 112, 136, 154, 170, 188, 208, 227],
    [24, 30, 52, 84, 131, 150, 166, 186, 203, 229],
    [37, 48, 64, 84, 104, 118, 156, 177, 201, 230],
];

/// Table 24: the first-stage codebook of wideband, in Q8.
pub(crate) const LSF_CODEBOOK_WB: [[u8; 16]; 32] = [
    [
        7, 23, 38, 54, 69, 85, 100, 116, 131, 147, 162, 178, 193, 208, 223, 239,
    ],
    [
        13, 25, 41, 55, 69, 83, 98, 112, 127, 142, 157, 171, 187, 203, 220, 236,
    ],
    [
        15, 21, 34, 51, 61, 78, 92, 106, 126, 136, 152, 167, 185, 205, 225, 240,
    ],
    [
        10, 21, 36, 50, 63, 79, 95, 110, 126, 141, 157, 173, 189, 205, 221, 237,
    ],
    [
        17, 20, 37, 51, 59, 78, 89, 107, 123, 134, 150, 164, 184, 205, 224, 240,
    ],
    [
        10, 15, 32, 51, 67, 81, 96, 112, 129, 142, 158, 173, 189, 204, 220, 236,
    ],
    [
        8, 21, 37, 51, 65, 79, 98, 113, 126, 138, 155, 168, 179, 192, 209, 218,
    ],
    [
        12, 15, 34, 55, 63, 78, 87, 108, 118, 131, 148, 167, 185, 203, 219, 236,
    ],
    [
        16, 19, 32, 36, 56, 79, 91, 108, 118, 136, 154, 171, 186, 204, 220, 237,
    ],
    [
        11, 28, 43, 58, 74, 89, 105, 120, 135, 150, 165, 180, 196, 211, 226, 241,
    ],
    [
        6, 16, 33, 46, 60, 75, 92, 107, 123, 137, 156, 169, 185, 199, 214, 225,
    ],
    [
        11, 19, 30, 44, 57, 74, 89, 105, 121, 135, 152, 169, 186, 202, 218, 234,
    ],
    [
        12, 19, 29, 46, 57, 71, 88, 100, 120, 132, 148, 165, 182, 199, 216, 233,
    ],
    [
        17, 23, 35, 46, 56, 77, 92, 106, 123, 134, 152, 167, 185, 204, 222, 237,
    ],
    [
        14, 17, 45, 53, 63, 75, 89, 107, 115, 132, 151, 171, 188, 206, 221, 240,
    ],
    [
        9, 16, 29, 40, 56, 71, 88, 103, 119, 137, 154, 171, 189, 205, 222, 237,
    ],
    [
        16, 19, 36, 48, 57, 76, 87, 105, 118, 132, 150, 167, 185, 202, 218, 236,
    ],
    [
        12, 17, 29, 54, 71, 81, 94, 104, 126, 136, 149, 164, 182, 201, 221, 237,
    ],
    [
        15, 28, 47, 62, 79, 97, 115, 129, 142, 155, 168, 180, 194, 208, 223, 238,
    ],
    [
        8, 14, 30, 45, 62, 78, 94, 111, 127, 143, 159, 175, 192, 207, 223, 239,
    ],
    [
        17, 30, 49, 62, 79, 92, 107, 119, 132, 145, 160, 174, 190, 204, 220, 235,
    ],
    [
        14, 19, 36, 45, 61, 76, 91, 108, 121, 138, 154, 172, 189, 205, 222, 238,
    ],
    [
        12, 18, 31, 45, 60, 76, 91, 107, 123, 138, 154, 171, 187, 204, 221, 236,
    ],
    [
        13, 17, 31, 43, 53, 70, 83, 103, 114, 131, 149, 167, 185, 203, 220, 237,
    ],
    [
        17, 22, 35, 42, 58, 78, 93, 110, 125, 139, 155, 170, 188, 206, 224, 240,
    ],
    [
        8, 15, 34, 50, 67, 83, 99, 115, 131, 146, 162, 178, 193, 209, 224, 239,
    ],
    [
        13, 16, 41, 66, 73, 86, 95, 111, 128, 137, 150, 163, 183, 206, 225, 241,
    ],
    [
        17, 25, 37, 52, 63, 75, 92, 102, 119, 132, 144, 160, 175, 191, 212, 231,
    ],
    [
        19, 31, 49, 65, 83, 100, 117, 133, 147, 161, 174, 187, 200, 213, 227, 242,
    ],
    [
        18, 31, 52, 68, 88, 103, 117, 126, 138, 149, 163, 177, 192, 207, 223, 239,
    ],
    [
        16, 29, 47, 61, 76, 90, 106, 119, 133, 147, 161, 176, 193, 209, 224, 240,
    ],
    [
        15, 21, 35, 50, 61, 73, 86, 97, 110, 119, 129, 141, 175, 198, 218, 237,
    ],
];

/// Table 25: the least distance, in Q15, each coefficient keeps from the
/// one below it, and the last from the top.
pub(crate) const LSF_MIN_SPACING_NB: [i32; 11] = [250, 3, 6, 3, 3, 3, 4, 3, 3, 3, 461];
pub(crate) const LSF_MIN_SPACING_WB: [i32; 17] =
    [100, 3, 40, 3, 3, 3, 5, 14, 14, 10, 11, 3, 8, 9, 7, 3, 347];

/// Table 26: how far a 20 ms frame interpolates from the last frame's
/// coefficients in its first half.
pub(crate) const LSF_INTERPOLATION: [u8; 5] = icdf([13, 22, 29, 11, 181]);

/// Table 27: the order in which the coefficients' cosines go into the
/// polynomials.
pub(crate) const LSF_ORDERING_NB: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
pub(crate) const LSF_ORDERING_WB: [usize; 16] =
    [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];

/// Table 28: the cosine at 129 points of the half turn, in Q12.
pub(crate) const COSINE: [i32; 129] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3997, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3613, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2676, 2599, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1568, 1474, 1380, 1285, 1189, 1093, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0, -101,
    -201, -301, -401, -501, -601, -700, -799, -897, -995, -1093, -1189, -1285, -1380, -1474, -1568,
    -1660, -1751, -1842, -1931, -2019, -2106, -2191, -2276, -2359, -2440, -2520, -2599, -2676,
    -2751, -2824, -2896, -2967, -3035, -3102, -3166, -3229, -3290, -3349, -3406, -3461, -3513,
    -3564, -3613, -3659, -3703, -3745, -3784, -3822, -3857, -3889, -3920, -3948, -3973, -3997,
    -4017, -4036, -4052, -4065, -4076, -4085, -4091, -4095, -4096,
];

/// Table 29: the high part of an absolutely coded pitch lag.
pub(crate) const PITCH_HIGH: [u8; 32] = icdf([
    3, 3, 6, 11, 21, 30, 32, 19, 11, 10, 12, 13, 13, 12, 11, 9, 8, 7, 6, 4, 2, 2, 2, 1, 1, 1, 1, 1,
    1, 1, 1, 1,
]);

/// Table 30: its low part, by bandwidth.
pub(crate) const PITCH_LOW_NB: [u8; 4] = icdf([64; 4]);
pub(crate) const PITCH_LOW_MB: [u8; 6] = icdf([43, 42, 43, 43, 42, 43]);
pub(crate) const PITCH_LOW_WB: [u8; 8] = icdf([32; 8]);

/// Table 31: a pitch lag coded against the last one.
pub(crate) const PITCH_DELTA: [u8; 21] = icdf([
    46, 2, 2, 3, 4, 6, 10, 15, 26, 38, 30, 22, 15, 10, 7, 6, 4, 4, 2, 2, 2,
]);

/// Table 32: the pitch contour, by bandwidth and frame size.
pub(crate) const CONTOUR_NB_10MS: [u8; 3] = icdf([143, 50, 63]);
pub(crate) const CONTOUR_NB_20MS: [u8; 11] = icdf([68, 12, 21, 17, 19, 22, 30, 24, 17, 16, 10]);
pub(crate) const CONTOUR_WB_10MS: [u8; 12] = icdf([91, 46, 39, 19, 14, 12, 8, 7, 6, 5, 5, 4]);
pub(crate) const CONTOUR_WB_20MS: [u8; 34] = icdf([
    33, 22, 18, 16, 15, 14, 14, 13, 13, 10, 9, 9, 8, 6, 6, 6, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2, 2,
    2, 2, 1, 1, 1, 1,
]);

/// Tables 33 to 36: the offset of each subframe's lag, by contour.
pub(crate) const LAGS_NB_10MS: [[i8; 2]; 3] = [[0, 0], [1, 0], [0, 1]];
pub(crate) const LAGS_NB_20MS: [[i8; 4]; 11] = [
    [0, 0, 0, 0],
    [2, 1, 0, -1],
    [-1, 0, 1, 2],
    [-1, 0, 0, 1],
    [-1, 0, 0, 0],
    [0, 0, 0, 1],
    [0, 0, 1, 1],
    [1, 1, 0, 0],
    [1, 0, 0, 0],
    [0, 0, 0, -1],
    [1, 0, 0, -1],
];
pub(crate) const LAGS_WB_10MS: [[i8; 2]; 12] = [
    [0, 0],
    [0, 1],
    [1, 0],
    [-1, 1],
    [1, -1],
    [-1, 2],
    [2, -1],
    [-2, 2],
    [2, -2],
    [-2, 3],
    [3, -2],
    [-3, 3],
];
pub(crate) const LAGS_WB_20MS: [[i8; 4]; 34] = [
    [0, 0, 0, 0],
    [0, 0, 1, 1],
    [1, 1, 0, 0],
    [-1, 0, 0, 0],
    [0, 0, 0, 1],
    [1, 0, 0, 0],
    [-1, 0, 0, 1],
    [0, 0, 0, -1],
    [-1, 0, 1, 2],
    [1, 0, 0, -1],
    [-2, -1, 1, 2],
    [2, 1, 0, -1],
    [-2, 0, 0, 2],
    [-2, 0, 1, 3],
    [2, 1, -1, -2],
    [-3, -1, 1, 3],
    [2, 0, 0, -2],
    [3, 1, 0, -2],
    [-3, -1, 2, 4],
    [-4, -1, 1, 4],
    [3, 1, -1, -3],
    [-4, -1, 2, 5],
    [4, 2, -1, -3],
    [4, 1, -1, -4],
    [-5, -1, 2, 6],
    [5, 2, -1, -4],
    [-6, -2, 2, 6],
    [-5, -2, 2, 5],
    [6, 2, -1, -5],
    [-7, -2, 3, 8],
    [6, 2, -2, -6],
    [5, 2, -2, -5],
    [8, 3, -2, -7],
    [-9, -3, 3, 9],
];

/// Table 37: which codebook of LTP filters the frame uses.
pub(crate) const LTP_PERIODICITY: [u8; 3] = icdf([77, 80, 99]);

/// Table 38: the filter of each subframe, by codebook.
pub(crate) const LTP_FILTER_0: [u8; 8] = icdf([185, 15, 13, 13, 9, 9, 6, 6]);
pub(crate) const LTP_FILTER_1: [u8; 16] =
    icdf([57, 34, 21, 20, 15, 13, 12, 13, 10, 10, 9, 10, 9, 8, 7, 8]);
pub(crate) const LTP_FILTER_2: [u8; 32] = icdf([
    15, 16, 14, 12, 12, 12, 11, 11, 11, 10, 9, 9, 9, 9, 8, 8, 8, 8, 7, 7, 6, 6, 5, 4, 5, 4, 4, 4,
    3, 4, 3, 2,
]);

/// Tables 39 to 41: the five taps of each filter, in Q7.
pub(crate) const LTP_TAPS_0: [[i8; 5]; 8] = [
    [4, 6, 24, 7, 5],
    [0, 0, 2, 0, 0],
    [12, 28, 41, 13, -4],
    [-9, 15, 42, 25, 14],
    [1, -2, 62, 41, -9],
    [-10, 37, 65, -4, 3],
    [-6, 4, 66, 7, -8],
    [16, 14, 38, -3, 33],
];
pub(crate) const LTP_TAPS_1: [[i8; 5]; 16] = [
    [13, 22, 39, 23, 12],
    [-1, 36, 64, 27, -6],
    [-7, 10, 55, 43, 17],
    [1, 1, 8, 1, 1],
    [6, -11, 74, 53, -9],
    [-12, 55, 76, -12, 8],
    [-3, 3, 93, 27, -4],
    [26, 39, 59, 3, -8],
    [2, 0, 77, 11, 9],
    [-8, 22, 44, -6, 7],
    [40, 9, 26, 3, 9],
    [-7, 20, 101, -7, 4],
    [3, -8, 42, 26, 0],
    [-15, 33, 68, 2, 23],
    [-2, 55, 46, -2, 15],
    [3, -1, 21, 16, 41],
];
pub(crate) const LTP_TAPS_2: [[i8; 5]; 32] = [
    [-6, 27, 61, 39, 5],
    [-11, 42, 88, 4, 1],
    [-2, 60, 65, 6, -4],
    [-1, -5, 73, 56, 1],
    [-9, 19, 94, 29, -9],
    [0, 12, 99, 6, 4],
    [8, -19, 102, 46, -13],
    [3, 2, 13, 3, 2],
    [9, -21, 84, 72, -18],
    [-11, 46, 104, -22, 8],
    [18, 38, 48, 23, 0],
    [-16, 70, 83, -21, 11],
    [5, -11, 117, 22, -8],
    [-6, 23, 117, -12, 3],
    [3, -8, 95, 28, 4],
    [-10, 15, 77, 60, -15],
    [-1, 4, 124, 2, -4],
    [3, 38, 84, 24, -25],
    [2, 13, 42, 13, 31],
    [21, -4, 56, 46, -1],
    [-1, 35, 79, -13, 19],
    [-7, 65, 88, -9, -14],
    [20, 4, 81, 49, -29],
    [20, 0, 75, 3, -17],
    [5, -9, 44, 92, -8],
    [1, -3, 22, 69, 31],
    [-6, 95, 41, -12, 5],
    [39, 67, 16, -4, 1],
    [0, -6, 120, 55, -36],
    [-13, 44, 122, 4, -24],
    [81, 5, 11, 3, 7],
    [2, 0, 9, 10, 88],
];

/// Table 42: the scaling of the LTP state at the start of a packet, and
/// its values in Q14.
pub(crate) const LTP_SCALING: [u8; 3] = icdf([128, 64, 64]);
pub(crate) const LTP_SCALES_Q14: [i32; 3] = [15_565, 12_288, 8_192];

/// Table 43, and the other uniform distributions.
pub(crate) const UNIFORM3: [u8; 3] = icdf([85, 86, 85]);
pub(crate) const UNIFORM4: [u8; 4] = icdf([64; 4]);
pub(crate) const UNIFORM5: [u8; 5] = icdf([51, 51, 52, 51, 51]);

/// Table 45: the rate level of the excitation, unvoiced or voiced.
pub(crate) const RATE_LEVEL: [[u8; 9]; 2] = [
    icdf([15, 51, 12, 46, 45, 13, 33, 27, 14]),
    icdf([33, 30, 36, 17, 34, 49, 18, 21, 18]),
];

/// Table 46: the pulses of a shell block, by rate level; level 9 follows
/// an escape, and level 10, which cannot escape again, the tenth.
pub(crate) const PULSE_COUNT: [[u8; 18]; 11] = [
    icdf([131, 74, 25, 8, 3, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    icdf([58, 93, 60, 23, 7, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    icdf([43, 51, 46, 33, 24, 16, 11, 8, 6, 3, 3, 3, 2, 1, 1, 2, 1, 2]),
    icdf([17, 52, 71, 57, 31, 12, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    icdf([6, 21, 41, 53, 49, 35, 21, 11, 6, 3, 2, 2, 1, 1, 1, 1, 1, 1]),
    icdf([
        7, 14, 22, 28, 29, 28, 25, 20, 17, 13, 11, 9, 7, 5, 4, 4, 3, 10,
    ]),
    icdf([2, 5, 14, 29, 42, 46, 41, 31, 19, 11, 6, 3, 2, 1, 1, 1, 1, 1]),
    icdf([
        1, 2, 4, 10, 19, 29, 35, 37, 34, 28, 20, 14, 8, 5, 4, 2, 2, 2,
    ]),
    icdf([
        1, 2, 2, 5, 9, 14, 20, 24, 27, 28, 26, 23, 20, 15, 11, 8, 6, 15,
    ]),
    icdf([1, 1, 1, 6, 27, 58, 56, 39, 25, 14, 10, 6, 3, 3, 2, 1, 1, 2]),
    icdf([2, 1, 6, 27, 58, 56, 39, 25, 14, 10, 6, 3, 3, 2, 1, 1, 2, 0]),
];

/// Tables 47 to 50: how the pulses of a partition of 16, 8, 4 and 2
/// samples split between its halves, by the pulses it holds, from 1 to
/// 16; each table has `pulses + 1` entries, padded here to 17.
pub(crate) const SPLIT: [[[u8; 17]; 16]; 4] = [
    split_rows([
        &[126, 130],
        &[56, 142, 58],
        &[25, 101, 104, 26],
        &[12, 60, 108, 64, 12],
        &[7, 35, 84, 87, 37, 6],
        &[4, 20, 59, 86, 63, 21, 3],
        &[3, 12, 38, 72, 75, 42, 12, 2],
        &[2, 8, 25, 54, 73, 59, 27, 7, 1],
        &[2, 5, 17, 39, 63, 65, 42, 18, 4, 1],
        &[1, 4, 12, 28, 49, 63, 54, 30, 11, 3, 1],
        &[1, 4, 8, 20, 37, 55, 57, 41, 22, 8, 2, 1],
        &[1, 3, 7, 15, 28, 44, 53, 48, 33, 16, 6, 1, 1],
        &[1, 2, 6, 12, 21, 35, 47, 48, 40, 25, 12, 5, 1, 1],
        &[1, 1, 4, 10, 17, 27, 37, 47, 43, 33, 21, 9, 4, 1, 1],
        &[1, 1, 1, 8, 14, 22, 33, 40, 43, 38, 28, 16, 8, 1, 1, 1],
        &[1, 1, 1, 1, 13, 18, 27, 36, 41, 41, 34, 24, 14, 1, 1, 1, 1],
    ]),
    split_rows([
        &[127, 129],
        &[53, 149, 54],
        &[22, 105, 106, 23],
        &[11, 61, 111, 63, 10],
        &[6, 35, 86, 88, 36, 5],
        &[4, 20, 59, 87, 62, 21, 3],
        &[3, 13, 40, 71, 73, 41, 13, 2],
        &[3, 9, 27, 53, 70, 56, 28, 9, 1],
        &[3, 8, 19, 37, 57, 61, 44, 20, 6, 1],
        &[3, 7, 15, 28, 44, 54, 49, 33, 17, 5, 1],
        &[1, 7, 13, 22, 34, 46, 48, 38, 28, 14, 4, 1],
        &[1, 1, 11, 22, 27, 35, 42, 47, 33, 25, 10, 1, 1],
        &[1, 1, 6, 14, 26, 37, 43, 43, 37, 26, 14, 6, 1, 1],
        &[1, 1, 4, 10, 20, 31, 40, 42, 40, 31, 20, 10, 4, 1, 1],
        &[1, 1, 3, 8, 16, 26, 35, 38, 38, 35, 26, 16, 8, 3, 1, 1],
        &[1, 1, 2, 6, 12, 21, 30, 36, 38, 36, 30, 21, 12, 6, 2, 1, 1],
    ]),
    split_rows([
        &[127, 129],
        &[49, 157, 50],
        &[20, 107, 109, 20],
        &[11, 60, 113, 62, 10],
        &[7, 36, 84, 87, 36, 6],
        &[6, 24, 57, 82, 60, 23, 4],
        &[5, 18, 39, 64, 68, 42, 16, 4],
        &[6, 14, 29, 47, 61, 52, 30, 14, 3],
        &[1, 15, 23, 35, 51, 50, 40, 30, 10, 1],
        &[1, 1, 21, 32, 42, 52, 46, 41, 18, 1, 1],
        &[1, 6, 16, 27, 36, 42, 42, 36, 27, 16, 6, 1],
        &[1, 5, 12, 21, 31, 38, 40, 38, 31, 21, 12, 5, 1],
        &[1, 3, 9, 17, 26, 34, 38, 38, 34, 26, 17, 9, 3, 1],
        &[1, 3, 7, 14, 22, 29, 34, 36, 34, 29, 22, 14, 7, 3, 1],
        &[1, 2, 5, 11, 18, 25, 31, 35, 35, 31, 25, 18, 11, 5, 2, 1],
        &[1, 1, 4, 9, 15, 21, 28, 32, 34, 32, 28, 21, 15, 9, 4, 1, 1],
    ]),
    split_rows([
        &[128, 128],
        &[42, 172, 42],
        &[21, 107, 107, 21],
        &[12, 60, 112, 61, 11],
        &[8, 34, 86, 86, 35, 7],
        &[8, 23, 55, 90, 55, 20, 5],
        &[5, 15, 38, 72, 72, 36, 15, 3],
        &[6, 12, 27, 52, 77, 47, 20, 10, 5],
        &[6, 19, 28, 35, 40, 40, 35, 28, 19, 6],
        &[4, 14, 22, 31, 37, 40, 37, 31, 22, 14, 4],
        &[3, 10, 18, 26, 33, 38, 38, 33, 26, 18, 10, 3],
        &[2, 8, 13, 21, 29, 36, 38, 36, 29, 21, 13, 8, 2],
        &[1, 5, 10, 17, 25, 32, 38, 38, 32, 25, 17, 10, 5, 1],
        &[1, 4, 7, 13, 21, 29, 35, 36, 35, 29, 21, 13, 7, 4, 1],
        &[1, 2, 5, 10, 17, 25, 32, 36, 36, 32, 25, 17, 10, 5, 2, 1],
        &[1, 2, 4, 7, 13, 21, 28, 34, 36, 34, 28, 21, 13, 7, 4, 2, 1],
    ]),
];

/// Turns the rows of a split table, of 2 to 17 entries, into inverse
/// cumulative tables of 17, each checked to sum to 256.
const fn split_rows(rows: [&[u8]; 16]) -> [[u8; 17]; 16] {
    let mut out = [[0u8; 17]; 16];
    let mut row = 0;
    while row < 16 {
        let pdf = rows[row];
        assert!(
            pdf.len() == row + 2,
            "a split row has one entry more than its pulses"
        );
        let mut total = 0u32;
        let mut index = 0;
        while index < pdf.len() {
            total += pdf[index] as u32;
            out[row][index] = (256 - total) as u8;
            index += 1;
        }
        assert!(total == 256, "a distribution must sum to 256");
        row += 1;
    }
    out
}

/// Table 51: each bit below the pulses.
pub(crate) const EXCITATION_LSB: [u8; 2] = icdf([136, 120]);

/// Table 52: the sign of each pulse, by signal type and quantisation
/// offset, inactive low, inactive high, unvoiced low and so on, and by the
/// pulses of the block, from 0 to 6 or more: the probability of a
/// negative sign, in 256ths.
pub(crate) const SIGN_NEGATIVE: [[u8; 7]; 6] = [
    [2, 207, 189, 179, 174, 163, 157],
    [58, 245, 238, 232, 225, 220, 211],
    [1, 210, 190, 178, 169, 162, 152],
    [48, 242, 235, 224, 214, 205, 190],
    [1, 162, 152, 147, 144, 141, 138],
    [8, 203, 187, 176, 168, 161, 154],
];

/// Table 53: the offset of the excitation, in Q10, by voicing and
/// quantisation offset type.
pub(crate) const QUANT_OFFSETS_Q10: [[i32; 2]; 2] = [[100, 240], [32, 100]];

/// Table 4: which frames of a 40 or 60 ms packet carry LBRR data; the
/// symbol is one less than the flags.
pub(crate) const LBRR_FLAGS_40MS: [u8; 3] = icdf([53, 53, 150]);
pub(crate) const LBRR_FLAGS_60MS: [u8; 7] = icdf([41, 20, 29, 41, 15, 28, 82]);

/// Table 6: the stereo prediction weights, and Table 7, their values in
/// Q13.
pub(crate) const STEREO_STAGE1: [u8; 25] = icdf([
    7, 2, 1, 1, 1, 10, 24, 8, 1, 1, 3, 23, 92, 23, 3, 1, 1, 8, 24, 10, 1, 1, 1, 2, 7,
]);
pub(crate) const STEREO_WEIGHTS_Q13: [i32; 16] = [
    -13_732, -10_050, -8_266, -7_526, -6_500, -5_000, -2_950, -820, 820, 2_950, 5_000, 6_500,
    7_526, 8_266, 10_050, 13_732,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cosine_table_is_odd_about_its_middle() {
        for i in 0..=64 {
            assert_eq!(COSINE[i], -COSINE[128 - i], "{i}");
        }
    }

    #[test]
    fn the_lettered_tables_use_their_letters() {
        for row in LSF_SELECT_NB {
            assert!(row.iter().all(|&letter| (b'a'..=b'h').contains(&letter)));
        }
        for row in LSF_SELECT_WB {
            assert!(row.iter().all(|&letter| (b'i'..=b'p').contains(&letter)));
        }
        for row in LSF_PRED_SELECT_NB {
            assert!(row.iter().all(|&letter| letter == b'A' || letter == b'B'));
        }
        for row in LSF_PRED_SELECT_WB {
            assert!(row.iter().all(|&letter| letter == b'C' || letter == b'D'));
        }
    }
}
