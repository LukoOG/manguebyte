//! Quantisation and Huffman coding of a granule: 2.4.3.4 of ISO/IEC
//! 11172-3 run backwards.
//!
//! The standard's encoder (Annex C.1.5.4) finds the gains with two nested
//! loops, one for the bits and one for the noise. This one goes straight
//! at the noise instead: for each scalefactor band it finds the coarsest
//! step whose noise stays within what the psychoacoustic model allows
//! there, by bisection, then takes the coarsest of those as the global
//! gain and makes up the difference in each band with its scalefactor.
//! What the granule then costs follows; a constant bitrate is held by
//! scaling what the model allows until the frame fits (see `encode.rs`).
//!
//! The Huffman tables, the region boundaries and the table of the
//! quadruples are chosen for the fewest bits, every choice tried.

use std::sync::OnceLock;

use super::super::layer3::power_table;
use super::super::tables::{
    LONG_BANDS, PairTable, QUAD_A, SCALEFACTOR_BITS, SHORT_BANDS, pair_table,
};
use super::BitWriter;
use super::analysis::BlockType;

/// The exponents of the requantisation a band can have: the global gain,
/// 0 to 255, less up to 15 times 4 of a scalefactor.
const E_MIN: i32 = -60;
const E_MAX: i32 = 255;

/// The largest magnitude a value can have: 15, and 13 more bits.
const LARGEST: i32 = 8_206;

/// The rounding of the quantiser, `nint(x - 0.0946)` (C.1.5.4.4.1).
const ROUNDING: f32 = 0.5 - 0.0946;

/// For each exponent `e` from [`E_MIN`], the step of the quantiser,
/// `2^(-3/16 (e - 210))`, and of the decoder, `2^((e - 210) / 4)`.
struct Steps {
    quantize: Vec<f32>,
    dequantize: Vec<f32>,
}

fn steps() -> &'static Steps {
    static STEPS: OnceLock<Steps> = OnceLock::new();
    STEPS.get_or_init(|| {
        let range = E_MIN..=E_MAX;
        Steps {
            quantize: range
                .clone()
                .map(|e| 2f64.powf(-0.1875 * f64::from(e - 210)) as f32)
                .collect(),
            dequantize: range
                .map(|e| 2f64.powf(0.25 * f64::from(e - 210)) as f32)
                .collect(),
        }
    })
}

/// A run of lines that share a scalefactor: a band of a long block, or a
/// band of one window of short blocks.
#[derive(Debug, Clone, Copy)]
pub(super) struct Partition {
    pub(super) start: usize,
    pub(super) end: usize,
    /// The largest scalefactor it can have: 15 or 7 by its group, 0 for
    /// the top band, which has none.
    largest_scalefactor: u8,
}

/// The partitions of a granule of each kind of block, at a sample rate of
/// MPEG-1, in the order the lines are coded.
pub(super) fn partitions(rate_index: usize, short: bool) -> Vec<Partition> {
    if short {
        let bands = &SHORT_BANDS[rate_index];
        let mut out = Vec::with_capacity(39);
        for band in 0..13 {
            let width = usize::from(bands[band + 1] - bands[band]);
            for window in 0..3 {
                let start = 3 * usize::from(bands[band]) + window * width;
                out.push(Partition {
                    start,
                    end: start + width,
                    largest_scalefactor: match band {
                        0..=5 => 15,
                        6..=11 => 7,
                        _ => 0,
                    },
                });
            }
        }
        out
    } else {
        let bands = &LONG_BANDS[rate_index];
        (0..22)
            .map(|band| Partition {
                start: usize::from(bands[band]),
                end: usize::from(bands[band + 1]),
                largest_scalefactor: match band {
                    0..=10 => 15,
                    11..=20 => 7,
                    _ => 0,
                },
            })
            .collect()
    }
}

/// A granule of one channel made ready to quantise: its lines, their
/// magnitudes to the power 3/4, and what each partition holds.
pub(super) struct Prepared {
    pub(super) lines: [f32; 576],
    powered: [f32; 576],
    energy: Vec<f32>,
    loudest: Vec<f32>,
    /// The noise the model allows in each partition, as power.
    pub(super) allowed: Vec<f32>,
    pub(super) block_type: BlockType,
}

impl Prepared {
    pub(super) fn new(
        lines: &[f32; 576],
        allowed: Vec<f32>,
        block_type: BlockType,
        partitions: &[Partition],
    ) -> Self {
        let mut powered = [0.0f32; 576];
        for (out, &line) in powered.iter_mut().zip(lines) {
            *out = line.abs().powf(0.75);
        }
        let energy = partitions
            .iter()
            .map(|p| lines[p.start..p.end].iter().map(|&x| x * x).sum())
            .collect();
        let loudest = partitions
            .iter()
            .map(|p| {
                powered[p.start..p.end]
                    .iter()
                    .fold(0.0f32, |most, &x| most.max(x))
            })
            .collect();
        Self {
            lines: *lines,
            powered,
            energy,
            loudest,
            allowed,
            block_type,
        }
    }
}

/// A granule of one channel, quantised, with everything its side
/// information says.
#[derive(Debug, Clone)]
pub(super) struct Coded {
    pub(super) values: [i32; 576],
    pub(super) global_gain: i32,
    pub(super) scalefac_scale: bool,
    /// By partition: a long block's 22 bands, or a short block's bands
    /// three windows at a time.
    pub(super) scalefactors: [u8; 39],
    pub(super) block_type: BlockType,
    pub(super) scalefac_compress: usize,
    pub(super) big_values: usize,
    pub(super) table_select: [u8; 3],
    pub(super) region0_count: u8,
    pub(super) region1_count: u8,
    pub(super) count1_table_b: bool,
    /// Where the quadruples end; the lines after are zero.
    pub(super) count1_end: usize,
    /// Bits of scalefactors and of Huffman codes.
    pub(super) part2_bits: u32,
    pub(super) part3_bits: u32,
}

impl Coded {
    pub(super) fn part2_3_length(&self) -> u32 {
        self.part2_bits + self.part3_bits
    }
}

/// The noise of quantising `lines` with exponent `e`, or `None` if a value
/// would be too large to code.
fn noise(lines: &[f32], powered: &[f32], e: i32) -> Option<f32> {
    let steps = steps();
    let at = (e - E_MIN) as usize;
    let (q, d) = (steps.quantize[at], steps.dequantize[at]);
    let powers = power_table();
    let mut sum = 0.0f32;
    for (&x, &p) in lines.iter().zip(powered) {
        let value = (p * q + ROUNDING) as i32;
        if value > LARGEST {
            return None;
        }
        let error = x.abs() - powers[value as usize] * d;
        sum += error * error;
    }
    Some(sum)
}

/// The exponent at which the loudest of the 3/4 powers `loudest` rounds to
/// `target`: below it, it rounds higher.
fn exponent_for(loudest: f32, target: f32) -> i32 {
    // loudest * 2^(-3/16 (e - 210)) = target
    let e = 210.0 - 16.0 / 3.0 * f64::from(target / loudest).log2();
    e.floor().clamp(f64::from(E_MIN), f64::from(E_MAX)) as i32
}

/// Quantises a granule with `allowance` times the noise the model allows.
pub(super) fn quantize(prepared: &Prepared, partitions: &[Partition], allowance: f32) -> Coded {
    // The coarsest exponent each partition can take, or `None` for one the
    // model lets go entirely.
    let mut coarsest: [Option<i32>; 39] = [None; 39];
    let mut finest = E_MIN;
    for (index, p) in partitions.iter().enumerate() {
        let lines = &prepared.lines[p.start..p.end];
        let powered = &prepared.powered[p.start..p.end];
        let loudest = prepared.loudest[index];
        if loudest == 0.0 {
            continue;
        }
        // Below `low` a value would be too large; from `high` on all are
        // zero.
        let low = (exponent_for(loudest, LARGEST as f32 + 1.0 - ROUNDING) + 1).min(E_MAX);
        finest = finest.max(low);
        let allowed = prepared.allowed[index] * allowance;
        if prepared.energy[index] <= allowed {
            continue;
        }
        let high = (exponent_for(loudest, 1.0 - ROUNDING) + 1).clamp(low, E_MAX);
        let fits = |e: i32| noise(lines, powered, e).is_some_and(|n| n <= allowed);
        let best = if !fits(low) {
            low
        } else {
            // The largest exponent that fits, between `low`, which does,
            // and `high`, which does not.
            let (mut good, mut bad) = (low, high);
            while bad - good > 1 {
                let middle = (good + bad) / 2;
                if fits(middle) {
                    good = middle;
                } else {
                    bad = middle;
                }
            }
            good
        };
        coarsest[index] = Some(best);
    }

    // The global gain and the scalefactors, with steps of 2 or of 4.
    let choose = |shift: i32| -> (i32, [u8; 39]) {
        let mut gain = coarsest.iter().flatten().copied().max().unwrap_or(E_MAX);
        for (index, p) in partitions.iter().enumerate() {
            if let Some(e) = coarsest[index] {
                gain = gain.min(e + shift * i32::from(p.largest_scalefactor));
            }
        }
        let gain = gain.clamp(finest.max(0), E_MAX);
        let mut scalefactors = [0u8; 39];
        for (index, p) in partitions.iter().enumerate() {
            if let Some(e) = coarsest[index] {
                let steps = (gain - e + shift - 1)
                    .div_euclid(shift)
                    .clamp(0, i32::from(p.largest_scalefactor));
                scalefactors[index] = steps as u8;
            }
        }
        (gain, scalefactors)
    };
    let (mut gain, mut scalefactors) = choose(2);
    let mut scalefac_scale = false;
    let (coarse_gain, coarse_scalefactors) = choose(4);
    if coarse_gain > gain {
        (gain, scalefactors, scalefac_scale) = (coarse_gain, coarse_scalefactors, true);
    }

    // An infinite allowance is silence, whatever the lines.
    if allowance.is_infinite() {
        gain = 0;
        scalefactors = [0; 39];
        scalefac_scale = false;
    }

    // The values.
    let shift = if scalefac_scale { 4 } else { 2 };
    let steps = steps();
    let mut values = [0i32; 576];
    for (index, p) in partitions.iter().enumerate() {
        let e = (gain - shift * i32::from(scalefactors[index])).max(E_MIN);
        let q = steps.quantize[(e - E_MIN) as usize];
        let range = p.start..p.end;
        for ((value, &powered), &line) in values[range.clone()]
            .iter_mut()
            .zip(&prepared.powered[range.clone()])
            .zip(&prepared.lines[range])
        {
            let magnitude = if allowance.is_infinite() {
                0
            } else {
                ((powered * q + ROUNDING) as i32).min(LARGEST)
            };
            *value = if line < 0.0 { -magnitude } else { magnitude };
        }
    }
    let mut coded = Coded {
        values,
        global_gain: gain,
        scalefac_scale,
        scalefactors,
        block_type: prepared.block_type,
        scalefac_compress: 0,
        big_values: 0,
        table_select: [0; 3],
        region0_count: 0,
        region1_count: 0,
        count1_table_b: false,
        count1_end: 0,
        part2_bits: 0,
        part3_bits: 0,
    };
    choose_scalefac_compress(&mut coded, [true; 4]);
    choose_tables(&mut coded, partitions);
    coded
}

/// Bits a value of `value` needs as a scalefactor.
fn width(value: u8) -> u8 {
    (8 - value.leading_zeros()) as u8
}

/// Picks the narrowest `scalefac_compress` for the scalefactors sent:
/// for a long block, those of the groups of bands `sent` says (the others
/// a second granule takes from the first, as `scfsi` says).
pub(super) fn choose_scalefac_compress(coded: &mut Coded, sent: [bool; 4]) {
    let (first, second, counts): (u8, u8, (u32, u32)) = if coded.block_type == 2 {
        let first = coded.scalefactors[..18].iter().copied().max().unwrap_or(0);
        let second = coded.scalefactors[18..36]
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        (first, second, (18, 18))
    } else {
        let groups = [0..6, 6..11, 11..16, 16..21];
        let mut first = 0u8;
        let mut second = 0u8;
        let mut counts = (0u32, 0u32);
        for (group, bands) in groups.into_iter().enumerate() {
            if !sent[group] {
                continue;
            }
            let count = bands.len() as u32;
            let largest = coded.scalefactors[bands].iter().copied().max().unwrap_or(0);
            if group < 2 {
                first = first.max(largest);
                counts.0 += count;
            } else {
                second = second.max(largest);
                counts.1 += count;
            }
        }
        (first, second, counts)
    };
    let (need1, need2) = (width(first), width(second));
    let (index, bits) = SCALEFACTOR_BITS
        .iter()
        .enumerate()
        .filter(|(_, (slen1, slen2))| *slen1 >= need1 && *slen2 >= need2)
        .map(|(index, &(slen1, slen2))| {
            (
                index,
                counts.0 * u32::from(slen1) + counts.1 * u32::from(slen2),
            )
        })
        .min_by_key(|&(_, bits)| bits)
        .expect("4 and 3 bits hold every scalefactor");
    coded.scalefac_compress = index;
    coded.part2_bits = bits;
}

/// The tables of pairs, with the most a value can be in each.
const SMALL_TABLES: [u8; 13] = [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15];

fn capacity(table: &PairTable) -> i32 {
    if table.linbits > 0 {
        15 + (1 << table.linbits) - 1
    } else {
        table.size as i32 - 1
    }
}

/// What coding `values` in pairs with a table costs: the bits of the
/// codes and the signs, and how many values escape to extra bits (which
/// the caller adds at the table's `linbits`). `None` if a value does not
/// fit a table without extra bits.
fn pair_bits(values: &[i32], table: &PairTable) -> Option<(u32, u32)> {
    let mut bits = 0u32;
    let mut escapes = 0u32;
    let escape = table.linbits > 0;
    for pair in values.chunks_exact(2) {
        let (mut x, mut y) = (
            pair[0].unsigned_abs() as usize,
            pair[1].unsigned_abs() as usize,
        );
        if escape {
            if x >= 15 {
                x = 15;
                escapes += 1;
            }
            if y >= 15 {
                y = 15;
                escapes += 1;
            }
        } else if x >= table.size || y >= table.size {
            return None;
        }
        bits += u32::from(table.lengths[x * table.size + y]);
        bits += u32::from(pair[0] != 0) + u32::from(pair[1] != 0);
    }
    Some((bits, escapes))
}

/// The best table for a run of pairs whose largest value is `largest`, and
/// its bits: `costs` gives each candidate's bits over the run.
fn best_table(largest: i32, costs: impl Fn(u8) -> Option<u32>) -> (u8, u32) {
    if largest == 0 {
        return (0, 0);
    }
    let holds =
        |table: u8| capacity(&pair_table(usize::from(table)).expect("a table of pairs")) >= largest;
    // The tables with extra bits: of each family, the one with the fewest
    // extra bits that still holds the largest value.
    let families = [16u8, 24].map(|family| (family..family + 8).find(|&table| holds(table)));
    let mut best = (0u8, u32::MAX);
    for table in SMALL_TABLES
        .into_iter()
        .filter(|&table| holds(table))
        .chain(families.into_iter().flatten())
    {
        match costs(table) {
            Some(bits) if bits < best.1 => best = (table, bits),
            _ => {}
        }
    }
    best
}

/// The bits of `values` in pairs with `table`, extra bits included.
fn run_bits(values: &[i32], table: u8) -> Option<u32> {
    if table == 0 {
        return values.iter().all(|&v| v == 0).then_some(0);
    }
    let info = pair_table(usize::from(table))?;
    let (bits, escapes) = pair_bits(values, &info)?;
    Some(bits + escapes * info.linbits)
}

/// Splits the values into big values and quadruples, and picks the
/// regions and tables of the big values and the table of the quadruples
/// for the fewest bits.
fn choose_tables(coded: &mut Coded, partitions: &[Partition]) {
    let values = &coded.values;
    // The last value that is not zero, rounded up to a pair; then back
    // over the quadruples of values no larger than 1.
    let mut end = values.iter().rposition(|&v| v != 0).map_or(0, |at| at + 1);
    end += end % 2;
    let mut big_end = end;
    while big_end >= 4 && values[big_end - 4..big_end].iter().all(|v| v.abs() <= 1) {
        big_end -= 4;
    }
    let count1 = &values[big_end..end];
    let quads = |table_b: bool| -> u32 {
        count1
            .chunks_exact(4)
            .map(|quad| {
                let index = quad
                    .iter()
                    .fold(0usize, |acc, &v| acc * 2 + usize::from(v != 0));
                let signs = quad.iter().filter(|&&v| v != 0).count() as u32;
                signs
                    + if table_b {
                        4
                    } else {
                        u32::from(QUAD_A.1[index])
                    }
            })
            .sum()
    };
    let (a, b) = (quads(false), quads(true));
    coded.count1_table_b = b < a;
    let count1_bits = a.min(b);
    coded.count1_end = end;
    coded.big_values = big_end / 2;

    let largest =
        |range: std::ops::Range<usize>| values[range].iter().map(|v| v.abs()).max().unwrap_or(0);
    let big = &values[..big_end];
    let bits = if coded.block_type != 0 {
        // Region 0 is the first 36 lines, region 1 the rest.
        let split = 36.min(big_end);
        let (t0, b0) = best_table(largest(0..split), |t| run_bits(&big[..split], t));
        let (t1, b1) = best_table(largest(split..big_end), |t| run_bits(&big[split..], t));
        coded.table_select = [t0, t1, 0];
        coded.region0_count = 0;
        coded.region1_count = 0;
        b0 + b1
    } else {
        // The bits of each band with each table, then every split into
        // three regions at band boundaries.
        let bands = partitions.len();
        let ranges: Vec<(usize, usize)> = partitions
            .iter()
            .map(|p| (p.start.min(big_end), p.end.min(big_end)))
            .collect();
        let maxima: Vec<i32> = ranges.iter().map(|&(s, e)| largest(s..e)).collect();
        let mut table_bits = [[0u32; 22]; 32];
        for table in SMALL_TABLES.iter().copied().chain(16..32) {
            let holds = capacity(&pair_table(usize::from(table)).expect("a table of pairs"));
            for (band, &(s, e)) in ranges.iter().enumerate() {
                table_bits[usize::from(table)][band] = if maxima[band] > holds {
                    u32::MAX / 64
                } else {
                    run_bits(&big[s..e], table).unwrap_or(u32::MAX / 64)
                };
            }
        }
        let region = |from: usize, to: usize| -> (u8, u32) {
            let to = to.min(bands);
            if from >= to {
                return (0, 0);
            }
            let most = maxima[from..to].iter().copied().max().unwrap_or(0);
            best_table(most, |t| {
                let sum: u32 = table_bits[usize::from(t)][from..to].iter().sum();
                (sum < u32::MAX / 64).then_some(sum)
            })
        };
        let mut best = (u32::MAX, 0u8, 0u8, [0u8; 3]);
        for r0 in 0..16usize {
            let first = r0 + 1;
            if first > bands {
                break;
            }
            let (t0, b0) = region(0, first);
            for r1 in 0..8usize {
                let second = r0 + r1 + 2;
                let (t1, b1) = region(first, second);
                let (t2, b2) = region(second, bands);
                let total = b0 + b1 + b2;
                if total < best.0 {
                    best = (total, r0 as u8, r1 as u8, [t0, t1, t2]);
                }
            }
        }
        coded.region0_count = best.1;
        coded.region1_count = best.2;
        coded.table_select = best.3;
        best.0
    };
    coded.part3_bits = bits + count1_bits;
}

/// Writes the scalefactors and the Huffman codes of a granule: its main
/// data. `sent` is what `scfsi` leaves to send of a long block's groups.
pub(super) fn write_main_data(
    bits: &mut BitWriter,
    coded: &Coded,
    sent: [bool; 4],
    rate_index: usize,
) {
    let (slen1, slen2) = SCALEFACTOR_BITS[coded.scalefac_compress];
    if coded.block_type == 2 {
        for (index, &sf) in coded.scalefactors[..36].iter().enumerate() {
            let width = if index < 18 { slen1 } else { slen2 };
            bits.write(u32::from(sf), u32::from(width));
        }
    } else {
        for (group, bands) in [0..6, 6..11, 11..16, 16..21].into_iter().enumerate() {
            if !sent[group] {
                continue;
            }
            let width = if group < 2 { slen1 } else { slen2 };
            for &sf in &coded.scalefactors[bands] {
                bits.write(u32::from(sf), u32::from(width));
            }
        }
    }

    // The regions of the big values, as the decoder finds them.
    let big_end = coded.big_values * 2;
    let (region1, region2) = if coded.block_type != 0 {
        (36, 576)
    } else {
        let long = &LONG_BANDS[rate_index];
        let band = |index: usize| usize::from(long[index.min(22)]);
        let first = usize::from(coded.region0_count) + 1;
        (
            band(first),
            band(first + usize::from(coded.region1_count) + 1),
        )
    };
    let values = &coded.values;
    let mut line = 0;
    while line < big_end {
        let region = if line < region1 {
            0
        } else if line < region2 {
            1
        } else {
            2
        };
        let number = usize::from(coded.table_select[region]);
        let Some(table) = pair_table(number) else {
            line += 2;
            continue;
        };
        let (x, y) = (values[line], values[line + 1]);
        let clip = |v: i32| -> usize {
            let magnitude = v.unsigned_abs() as usize;
            if table.linbits > 0 {
                magnitude.min(15)
            } else {
                magnitude
            }
        };
        let (cx, cy) = (clip(x), clip(y));
        let index = cx * table.size + cy;
        bits.write(
            u32::from(table.codes[index]),
            u32::from(table.lengths[index]),
        );
        for (value, clipped) in [(x, cx), (y, cy)] {
            if table.linbits > 0 && clipped == 15 {
                bits.write(value.unsigned_abs() - 15, table.linbits);
            }
            if value != 0 {
                bits.write(u32::from(value < 0), 1);
            }
        }
        line += 2;
    }
    for quad in values[big_end..coded.count1_end].chunks_exact(4) {
        let index = quad
            .iter()
            .fold(0usize, |acc, &v| acc * 2 + usize::from(v != 0));
        if coded.count1_table_b {
            bits.write(15 - index as u32, 4);
        } else {
            bits.write(u32::from(QUAD_A.0[index]), u32::from(QUAD_A.1[index]));
        }
        for &value in quad {
            if value != 0 {
                bits.write(u32::from(value < 0), 1);
            }
        }
    }
}
