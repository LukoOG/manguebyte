//! Coding prediction residuals (RFC 9639, section 9.2.7).
//!
//! The residuals of a subframe are cut into 2^p partitions of equal length,
//! the first shortened by the predictor's order, and each partition is
//! coded on its own: with a Rice code, whose parameter suits the size of
//! its numbers, or "escaped", as plain numbers of a fixed width. A single
//! loud click therefore costs bits in its own partition only, and a
//! partition of zeros costs nothing beyond its header.
//!
//! Choosing all of this is a search. [`estimate`] does it quickly, from the
//! sum of each partition's numbers, to compare predictors. [`plan`] does it
//! exactly, by counting the bits each choice really takes, for the
//! predictor that is going to be written.

use super::bits::BitWriter;

/// The highest partition order the encoder uses: the limit of the
/// streamable subset, and 256 partitions.
pub(super) const MAX_PARTITION_ORDER: u32 = 8;

const MAX_PARTITIONS: usize = 1 << MAX_PARTITION_ORDER;

/// The magnitude every residual must stay below (RFC 9639, section 9.2.7),
/// so that decoders can hold residuals in 32-bit integers.
pub(super) const RESIDUAL_LIMIT: i64 = 1 << 31;

/// The largest Rice parameter each width of parameter field can state. The
/// value with every bit set is the escape code.
const MAX_PARAMETER_NARROW: u32 = 14;
const MAX_PARAMETER_WIDE: u32 = 30;

/// Maps a residual to an unsigned number, as Rice codes need: 0, -1, 1, -2,
/// 2, ... become 0, 1, 2, 3, 4, ...
#[inline]
pub(super) fn fold(residual: i64) -> u64 {
    ((residual << 1) ^ (residual >> 63)) as u64
}

/// How one partition is coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Code {
    /// A Rice code with this parameter.
    Rice(u8),
    /// Plain two's-complement numbers of this many bits, possibly zero.
    Escape(u8),
}

/// How the residuals of one subframe are coded.
#[derive(Debug, Clone)]
pub(super) struct ResidualPlan {
    pub(super) partition_order: u32,
    /// Whether Rice parameters are stored in five bits rather than four.
    pub(super) wide: bool,
    pub(super) codes: [Code; MAX_PARTITIONS],
}

impl ResidualPlan {
    pub(super) fn new() -> Self {
        Self {
            partition_order: 0,
            wide: false,
            codes: [Code::Rice(0); MAX_PARTITIONS],
        }
    }
}

/// The highest partition order allowed for `block_size` samples behind a
/// predictor of order `order`, up to `limit`.
///
/// The block must divide evenly into the partitions, and each partition
/// must be longer than the predictor order (RFC 9639, section 9.2.7). The
/// caller makes sure the block is longer than the order, so order 0 always
/// qualifies.
pub(super) fn max_partition_order(block_size: usize, order: usize, limit: u32) -> u32 {
    let mut partition_order = limit.min(MAX_PARTITION_ORDER);
    while partition_order > 0
        && (block_size % (1 << partition_order) != 0 || block_size >> partition_order <= order)
    {
        partition_order -= 1;
    }
    partition_order
}

/// The width that escaped numbers need to hold every residual whose folded
/// values combine, by OR, to `folded_or`, if the five-bit field can state
/// it.
fn escape_width(folded_or: u64) -> Option<u32> {
    // A folded value below 2^w is a residual in -2^(w-1)..2^(w-1).
    let width = 64 - folded_or.leading_zeros();
    (width <= 31).then_some(width)
}

/// The best Rice parameter up to `max` for `count` numbers adding up to
/// `sum`, by the estimate `count * (k + 1) + sum / 2^k`, and that estimate.
///
/// The estimate is never below the true size: the sum of the quotients is
/// at most the quotient of the sum.
fn estimate_rice(sum: u64, count: u64, max: u32) -> (u32, u64) {
    let mut parameter = 0;
    // Each step up halves what the quotients cost and adds a bit to every
    // number; it pays while the quotients save more than that.
    while parameter < max && sum >> (parameter + 1) > count {
        parameter += 1;
    }
    let cost = |k: u32| count * u64::from(k + 1) + (sum >> k);
    if parameter < max && cost(parameter + 1) < cost(parameter) {
        parameter += 1;
    }
    (parameter, cost(parameter))
}

/// The cost of a partition's numbers alone, without its parameter field,
/// for the cheaper of a Rice code up to `max` and an escape.
fn estimate_partition(sum: u64, folded_or: u64, count: u64, max: u32) -> u64 {
    let (_, rice) = estimate_rice(sum, count, max);
    match escape_width(folded_or) {
        Some(width) => rice.min(5 + count * u64::from(width)),
        None => rice,
    }
}

/// Estimates the bits that the residuals of a subframe take, including the
/// coding method and partition order fields, choosing the partition order
/// up to `limit` and the parameter width as [`plan`] would.
///
/// `residual` holds `block_size - order` values.
pub(super) fn estimate(residual: &[i64], order: usize, block_size: usize, limit: u32) -> u64 {
    let top = max_partition_order(block_size, order, limit);
    let mut sums = [0u64; MAX_PARTITIONS];
    let mut ors = [0u64; MAX_PARTITIONS];
    let partitions = 1usize << top;
    let length = block_size >> top;
    let mut start = 0;
    for (index, (sum, or)) in sums
        .iter_mut()
        .zip(ors.iter_mut())
        .take(partitions)
        .enumerate()
    {
        let count = if index == 0 { length - order } else { length };
        for &value in &residual[start..start + count] {
            let folded = fold(value);
            *sum += folded;
            *or |= folded;
        }
        start += count;
    }

    let mut best = u64::MAX;
    let mut partitions = partitions;
    let mut partition_order = top;
    loop {
        let length = (block_size >> partition_order) as u64;
        let (mut narrow, mut wide) = (0u64, 0u64);
        for index in 0..partitions {
            let count = if index == 0 {
                length - order as u64
            } else {
                length
            };
            narrow += 4 + estimate_partition(sums[index], ors[index], count, MAX_PARAMETER_NARROW);
            wide += 5 + estimate_partition(sums[index], ors[index], count, MAX_PARAMETER_WIDE);
        }
        best = best.min(narrow.min(wide));
        if partition_order == 0 {
            break;
        }
        // Each pair of partitions becomes one of the order below.
        partitions /= 2;
        partition_order -= 1;
        for index in 0..partitions {
            sums[index] = sums[2 * index] + sums[2 * index + 1];
            ors[index] = ors[2 * index] | ors[2 * index + 1];
        }
    }
    // The coding method and the partition order.
    2 + 4 + best
}

/// The exact cost of one partition's numbers with a Rice code no higher
/// than `max`, without the parameter field, and the parameter.
///
/// The estimate points at a parameter; the true sizes of it and of its two
/// neighbours are counted, and the smallest is taken.
fn exact_rice(values: &[i64], sum: u64, max: u32) -> (u32, u64) {
    let count = values.len() as u64;
    let (guess, _) = estimate_rice(sum, count, max);
    let low = guess.saturating_sub(1);
    let high = (guess + 1).min(max);
    let mut quotients = [0u64; 3];
    for &value in values {
        let folded = fold(value);
        for (slot, parameter) in (low..=high).enumerate() {
            quotients[slot] += folded >> parameter;
        }
    }
    let mut best = (low, u64::MAX);
    for (slot, parameter) in (low..=high).enumerate() {
        let cost = count * u64::from(parameter + 1) + quotients[slot];
        if cost < best.1 {
            best = (parameter, cost);
        }
    }
    best
}

/// Chooses exactly how to code the residuals of a subframe: the partition
/// order up to `limit`, the width of the parameter fields, and for each
/// partition a Rice parameter or an escape. Writes the choice into `plan`
/// and returns the bits it takes, fields included.
///
/// `residual` holds `block_size - order` values.
pub(super) fn plan(
    residual: &[i64],
    order: usize,
    block_size: usize,
    limit: u32,
    plan: &mut ResidualPlan,
) -> u64 {
    let top = max_partition_order(block_size, order, limit);
    let mut best = u64::MAX;
    let mut narrow_codes = [Code::Rice(0); MAX_PARTITIONS];
    let mut wide_codes = [Code::Rice(0); MAX_PARTITIONS];
    for partition_order in 0..=top {
        let partitions = 1usize << partition_order;
        let length = block_size >> partition_order;
        let (mut narrow, mut wide) = (0u64, 0u64);
        let mut start = 0;
        for index in 0..partitions {
            let count = if index == 0 { length - order } else { length };
            let values = &residual[start..start + count];
            start += count;
            let (sum, or) = values.iter().fold((0u64, 0u64), |(sum, or), &value| {
                let folded = fold(value);
                (sum + folded, or | folded)
            });
            let escape = escape_width(or).map(|width| (width, 5 + count as u64 * u64::from(width)));

            let (parameter, rice) = exact_rice(values, sum, MAX_PARAMETER_WIDE);
            let choose = |parameter: u32, rice: u64| match escape {
                Some((width, cost)) if cost < rice => (Code::Escape(width as u8), cost),
                _ => (Code::Rice(parameter as u8), rice),
            };
            let (code, cost) = choose(parameter, rice);
            wide_codes[index] = code;
            wide += 5 + cost;

            let (code, cost) = if parameter <= MAX_PARAMETER_NARROW {
                choose(parameter, rice)
            } else {
                let (parameter, rice) = exact_rice(values, sum, MAX_PARAMETER_NARROW);
                choose(parameter, rice)
            };
            narrow_codes[index] = code;
            narrow += 4 + cost;
        }
        let (cost, wide_wins) = if wide < narrow {
            (wide, true)
        } else {
            (narrow, false)
        };
        if cost < best {
            best = cost;
            plan.partition_order = partition_order;
            plan.wide = wide_wins;
            let codes = if wide_wins {
                &wide_codes
            } else {
                &narrow_codes
            };
            plan.codes[..partitions].copy_from_slice(&codes[..partitions]);
        }
    }
    2 + 4 + best
}

/// Writes the residuals of a subframe as `plan` says.
pub(super) fn write(
    writer: &mut BitWriter,
    residual: &[i64],
    order: usize,
    block_size: usize,
    plan: &ResidualPlan,
) {
    let width = if plan.wide { 5 } else { 4 };
    let escape = (1u64 << width) - 1;
    writer.write(u64::from(plan.wide), 2);
    writer.write(u64::from(plan.partition_order), 4);
    let partitions = 1usize << plan.partition_order;
    let length = block_size >> plan.partition_order;
    let mut start = 0;
    for (index, code) in plan.codes[..partitions].iter().enumerate() {
        let count = if index == 0 { length - order } else { length };
        let values = &residual[start..start + count];
        start += count;
        match *code {
            Code::Rice(parameter) => {
                writer.write(u64::from(parameter), width);
                for &value in values {
                    writer.write_rice(fold(value), u32::from(parameter));
                }
            }
            Code::Escape(bits) => {
                writer.write(escape, width);
                writer.write(u64::from(bits), 5);
                for &value in values {
                    writer.write_signed(value, u32::from(bits));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic source of numbers for the tests.
    fn noise(seed: u64, count: usize, scale: i64) -> Vec<i64> {
        let mut state = seed | 1;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % (2 * scale as u64 + 1)) as i64 - scale
            })
            .collect()
    }

    /// Writes `residual` as planned and checks that it takes the bits the
    /// plan said.
    fn written_bits(residual: &[i64], order: usize, block_size: usize, limit: u32) -> (u64, u64) {
        let mut chosen = ResidualPlan::new();
        let cost = plan(residual, order, block_size, limit, &mut chosen);
        let mut writer = BitWriter::with_capacity(1 << 20);
        write(&mut writer, residual, order, block_size, &chosen);
        (cost, writer.bit_len())
    }

    #[test]
    fn folding_interleaves_signs() {
        let folded: Vec<u64> = [0, -1, 1, -2, 2, i64::from(i32::MIN), i64::from(i32::MAX)]
            .into_iter()
            .map(fold)
            .collect();
        assert_eq!(folded, [0, 1, 2, 3, 4, 0xFFFF_FFFF, 0xFFFF_FFFE]);
    }

    #[test]
    fn partition_orders_respect_the_block_and_the_predictor() {
        assert_eq!(max_partition_order(4096, 0, 8), 8);
        assert_eq!(max_partition_order(4096, 12, 8), 8);
        // 4096 / 2^8 is 16, which must be longer than the order.
        assert_eq!(max_partition_order(4096, 16, 8), 7);
        assert_eq!(max_partition_order(4096, 32, 8), 6);
        // An odd block allows a single partition only.
        assert_eq!(max_partition_order(4095, 0, 8), 0);
        assert_eq!(max_partition_order(1152, 8, 8), 7);
        assert_eq!(max_partition_order(16, 4, 8), 1);
        assert_eq!(max_partition_order(4096, 4, 3), 3);
    }

    #[test]
    fn the_plan_counts_exactly_the_bits_that_are_written() {
        for (seed, scale) in [(1, 0), (2, 1), (3, 30), (4, 3_000), (5, 1 << 29)] {
            for (block_size, order) in [(4096, 0), (4096, 8), (1152, 2), (4095, 3), (16, 1)] {
                let mut residual = noise(seed, block_size - order, scale);
                if scale > 1 {
                    // Half of each kind of distribution.
                    residual.truncate(residual.len() / 2);
                    let rest = block_size - order - residual.len();
                    residual.extend(laplace(seed, rest, scale as f64 / 4.0));
                }
                let (cost, bits) = written_bits(&residual, order, block_size, 8);
                assert_eq!(cost, bits, "seed {seed}, block {block_size}, order {order}");
                // The estimate used to compare predictors is close to it.
                let estimated = estimate(&residual, order, block_size, 8);
                assert!(
                    estimated >= cost && estimated <= cost + cost / 8 + 64,
                    "estimate {estimated} for {cost}"
                );
            }
        }
    }

    #[test]
    fn silence_in_a_partition_costs_only_its_header() {
        // Loud numbers in the first half of the block, zeros in the second.
        let mut residual = noise(9, 4096, 20_000);
        residual[2048..].fill(0);
        let mut chosen = ResidualPlan::new();
        plan(&residual, 0, 4096, 8, &mut chosen);
        let partitions = 1 << chosen.partition_order;
        assert!(partitions >= 2);
        for code in &chosen.codes[partitions / 2..partitions] {
            assert_eq!(*code, Code::Escape(0));
        }
    }

    /// Numbers with a two-sided exponential distribution of mean magnitude
    /// `scale`, which is how prediction residuals are distributed and what
    /// Rice codes suit.
    fn laplace(seed: u64, count: usize, scale: f64) -> Vec<i64> {
        noise(seed, count, 1 << 30)
            .into_iter()
            .map(|value| {
                let uniform = (value.unsigned_abs() as f64 + 0.5) / f64::from(1u32 << 30);
                let magnitude = (-scale * uniform.ln()).round() as i64;
                if value < 0 { -magnitude } else { magnitude }
            })
            .collect()
    }

    #[test]
    fn large_numbers_take_the_wide_parameter_field() {
        // Residuals of about 2^18 need a parameter of about 18, more than
        // four bits can say. The wide field holds it; the narrow one,
        // limited to 14, would cost some 2^4 extra bits a number.
        let residual = laplace(11, 4096, f64::from(1 << 18));
        let mut chosen = ResidualPlan::new();
        let cost = plan(&residual, 0, 4096, 8, &mut chosen);
        assert!(chosen.wide);
        assert!(
            chosen.codes[..1 << chosen.partition_order]
                .iter()
                .all(|code| matches!(code, Code::Rice(16..=21))),
            "{:?}",
            &chosen.codes[..4]
        );
        assert_eq!(written_bits(&residual, 0, 4096, 8).1, cost);

        // Small ones keep the narrow field.
        let residual = laplace(12, 4096, 40.0);
        plan(&residual, 0, 4096, 8, &mut chosen);
        assert!(!chosen.wide);
    }

    #[test]
    fn a_partition_of_uniform_noise_may_be_escaped() {
        // Numbers spread evenly over the whole of 16 bits suit no Rice code
        // as well as they suit sixteen plain bits each.
        let residual = noise(13, 4096, 32_767);
        let mut chosen = ResidualPlan::new();
        let cost = plan(&residual, 0, 4096, 0, &mut chosen);
        assert_eq!(chosen.codes[0], Code::Escape(16));
        assert_eq!(cost, 2 + 4 + 4 + 5 + 4096 * 16);
    }
}
