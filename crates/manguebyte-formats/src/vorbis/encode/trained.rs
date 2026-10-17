//! What the codebooks are designed from: how often each value came up when
//! an earlier design of this encoder coded the test signals of this crate
//! (tones, sweeps, silence, noise and clicks, one to six channels, 8 to
//! 96 kHz) at qualities 0 to 10. Huffman codes built on these counts take
//! some 8 % fewer bits than on the smooth models they replaced, the most
//! of it in the classes, which are zero far more often than not. Every
//! entry has a count of at least 1, so that every value can be coded.

/// The classes of two neighbouring partitions, the first in the high digit,
/// per 100 000: 90 % as counted and 10 % as if the two were independent.
pub(super) const CLASS_PAIRS: [u32; 64] = [
    70_217, 448, 253, 331, 389, 410, 629, 10, 802, 1547, 273, 137, 111, 53, 57, 2, 321, 370, 1108,
    411, 120, 52, 64, 1, 390, 146, 454, 1868, 686, 119, 90, 3, 462, 105, 103, 677, 2447, 790, 175,
    6, 492, 57, 49, 108, 750, 2791, 941, 13, 828, 67, 48, 91, 155, 871, 5894, 64, 49, 1, 1, 2, 4,
    12, 53, 26,
];

/// The floor codes of section 7.2.4, per 100 000, each averaged with the
/// codes two above and two below it: even codes are posts above their
/// prediction, odd ones below it.
pub(super) const FLOOR_CODES: [u16; 128] = [
    11_233, 10_319, 9050, 8174, 5280, 4819, 3127, 3270, 1923, 2818, 1393, 2669, 1041, 2231, 954,
    1883, 795, 1520, 667, 1452, 699, 1450, 596, 1367, 687, 1302, 573, 1187, 608, 1136, 492, 1075,
    396, 982, 320, 874, 310, 803, 326, 697, 307, 618, 295, 507, 237, 459, 220, 390, 163, 329, 170,
    274, 172, 259, 158, 247, 140, 249, 117, 228, 111, 212, 110, 192, 108, 174, 103, 182, 91, 178,
    82, 170, 92, 139, 89, 129, 106, 112, 104, 116, 94, 126, 69, 132, 49, 141, 55, 96, 64, 65, 81,
    23, 85, 17, 63, 10, 41, 20, 23, 19, 18, 16, 8, 1, 9, 1, 10, 1, 10, 1, 9, 1, 7, 1, 7, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

/// The four values of a vector of the book of class 1, each -1, 0 or 1,
/// the first in the lowest digit, per 10 000. Neighbouring values go
/// together: the vector of four zeros and those of four equal values are
/// the likeliest.
pub(super) const CLASS_1: [u16; 81] = [
    294, 20, 52, 16, 188, 41, 18, 22, 96, 7, 21, 6, 27, 126, 24, 25, 39, 23, 51, 12, 105, 45, 135,
    14, 35, 7, 22, 22, 28, 18, 194, 301, 369, 20, 31, 44, 259, 109, 197, 450, 3161, 442, 146, 114,
    216, 44, 31, 19, 291, 304, 221, 19, 30, 22, 19, 7, 38, 15, 134, 40, 98, 13, 43, 23, 43, 21, 28,
    127, 22, 4, 20, 7, 72, 22, 14, 27, 181, 16, 40, 18, 336,
];

/// For the other books, how often each magnitude came up, per 10 000, a
/// value and its negative alike.
pub(super) const CLASS_2: [u16; 3] = [3887, 3790, 2323];
pub(super) const CLASS_4: [u16; 5] = [2682, 2831, 2194, 1608, 686];
pub(super) const CLASS_8: [u16; 9] = [1555, 1430, 1318, 1218, 1125, 1089, 959, 864, 441];
pub(super) const CLASS_16: [u16; 17] = [
    1285, 1040, 966, 928, 866, 807, 721, 630, 559, 580, 447, 346, 266, 206, 162, 115, 77,
];
/// In steps of 16 and of 256.
pub(super) const COARSE_16: [u16; 9] = [3766, 4072, 1345, 467, 194, 82, 39, 24, 11];
pub(super) const COARSE_256: [u16; 9] = [8559, 1356, 73, 11, 1, 1, 1, 1, 1];
