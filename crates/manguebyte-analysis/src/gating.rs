//! Gated averages over a programme of any length, in a fixed amount of
//! memory.
//!
//! Integrated loudness and loudness range are both defined over every block
//! measured since the start, and both use a threshold that depends on all of
//! those blocks, so it moves as the programme goes on. The textbook
//! implementation keeps every block. Here the blocks are sorted into a
//! histogram of loudness instead: 10,000 bins of 0.01 LU from -70 LUFS to
//! +30 LUFS, the same whether the programme lasts a second or a week.

/// The constant of BS.1770-4 equation (2). It cancels the gain of the
/// K-weighting at 997 Hz, so a full-scale sine there reads -3.01 LKFS.
pub(crate) const LOUDNESS_OFFSET: f64 = -0.691;

/// The absolute gate of BS.1770-4 and EBU Tech 3342.
pub(crate) const ABSOLUTE_GATE_LUFS: f64 = -70.0;

/// How far below the ungated level the relative gate of integrated loudness
/// sits (BS.1770-4, equation 6).
const INTEGRATED_GATE_LU: f64 = -10.0;

/// The same for loudness range (EBU Tech 3342, section 3.1).
const RANGE_GATE_LU: f64 = -20.0;

/// The percentiles loudness range is measured between (EBU Tech 3342).
const RANGE_PERCENTILES: (f64, f64) = (10.0, 95.0);

/// Histogram bins per loudness unit.
const BINS_PER_LU: f64 = 100.0;

/// Bins from the absolute gate up to +30 LUFS.
const BIN_COUNT: usize = 10_000;

/// Loudness in LUFS of a channel-weighted mean square (BS.1770-4,
/// equation 2). Silence is minus infinity.
pub(crate) fn lufs(mean_square: f64) -> f64 {
    if mean_square > 0.0 {
        LOUDNESS_OFFSET + 10.0 * mean_square.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The bin a loudness falls into. Anything above the range goes into the
/// last bin.
fn bin_of(loudness: f64) -> usize {
    let position = (loudness - ABSOLUTE_GATE_LUFS) * BINS_PER_LU;
    // A float-to-integer cast saturates, so a negative position is bin 0.
    (position as usize).min(BIN_COUNT - 1)
}

/// The integrated loudness of BS.1770-4: the mean of all 400 ms gating
/// blocks above an absolute gate and above a relative gate 10 LU under the
/// mean of those.
///
/// Each bin keeps how many blocks fell into it and the sum of their mean
/// squares, so the average over any set of whole bins is exact, not rounded
/// to bin centres. The one approximation is at the relative gate, which
/// usually cuts through a bin: the blocks of that bin are all kept or all
/// dropped, according to which side of the gate their own mean lies. When
/// they are all on one side of the gate, which includes every case where
/// they are equal, that is exact. Otherwise it is what the exact method
/// would give with the gate moved by less than one bin, 0.01 LU.
///
/// Blocks louder than +30 LUFS share the last bin. Their mean is still
/// exact, but the gate cannot tell them apart.
#[derive(Debug, Clone)]
pub(crate) struct GatedLoudness {
    sums: Vec<f64>,
    counts: Vec<u64>,
    /// Sum and number of every block above the absolute gate.
    total_sum: f64,
    total_count: u64,
    /// The span of bins that hold anything.
    lowest: usize,
    highest: usize,
}

impl GatedLoudness {
    /// An empty measurement. This allocates.
    pub(crate) fn new() -> Self {
        Self {
            sums: vec![0.0; BIN_COUNT],
            counts: vec![0; BIN_COUNT],
            total_sum: 0.0,
            total_count: 0,
            lowest: 0,
            highest: 0,
        }
    }

    /// Forgets every block.
    pub(crate) fn clear(&mut self) {
        if self.total_count > 0 {
            self.sums[self.lowest..=self.highest].fill(0.0);
            self.counts[self.lowest..=self.highest].fill(0);
        }
        self.total_sum = 0.0;
        self.total_count = 0;
    }

    /// Adds one gating block, given its channel-weighted mean square.
    pub(crate) fn push(&mut self, mean_square: f64) {
        let loudness = lufs(mean_square);
        // Written this way round so that anything that is not a number
        // fails the test.
        let above_gate = loudness > ABSOLUTE_GATE_LUFS;
        if !above_gate {
            return;
        }
        let bin = bin_of(loudness);
        self.sums[bin] += mean_square;
        self.counts[bin] += 1;
        if self.total_count == 0 {
            self.lowest = bin;
            self.highest = bin;
        } else {
            self.lowest = self.lowest.min(bin);
            self.highest = self.highest.max(bin);
        }
        self.total_sum += mean_square;
        self.total_count += 1;
    }

    /// The relative gate in LUFS, or minus infinity before any block has
    /// passed the absolute gate.
    pub(crate) fn relative_gate(&self) -> f64 {
        if self.total_count == 0 {
            return f64::NEG_INFINITY;
        }
        lufs(self.total_sum / self.total_count as f64) + INTEGRATED_GATE_LU
    }

    /// The gated loudness in LUFS, or minus infinity before any block has
    /// passed the absolute gate.
    pub(crate) fn loudness(&self) -> f64 {
        if self.total_count == 0 {
            return f64::NEG_INFINITY;
        }
        let gate = self.relative_gate();
        let gate_bin = bin_of(gate);
        let (mut sum, mut count) = (0.0, 0u64);
        for bin in gate_bin.max(self.lowest)..=self.highest {
            let blocks = self.counts[bin];
            if blocks == 0 {
                continue;
            }
            if bin == gate_bin {
                let kept = lufs(self.sums[bin] / blocks as f64) > gate;
                if !kept {
                    continue;
                }
            }
            sum += self.sums[bin];
            count += blocks;
        }
        if count == 0 {
            f64::NEG_INFINITY
        } else {
            lufs(sum / count as f64)
        }
    }
}

/// The loudness range of EBU Tech 3342: the distance between the 10th and
/// the 95th percentile of the short-term loudness values that pass an
/// absolute gate and a relative gate 20 LU under the mean of those.
///
/// Each bin keeps how many values fell into it and the sum of their mean
/// squares. Two things are approximate, and they are of different kinds.
///
/// A percentile is reported as the centre of the bin it falls in, so each
/// is within 0.005 LU of the value it stands for, and the range within
/// 0.01 LU of the range of the same set of values.
///
/// Which values are in that set is decided by the relative gate, and the
/// gate usually cuts through a bin. As in [`GatedLoudness`], the values of
/// that bin are all kept or all dropped, according to which side of the
/// gate their own mean lies. When they are all on one side of the gate,
/// which includes every case where they are equal, that is exact.
/// Otherwise it is what the exact method would give with the gate moved by
/// less than one bin, 0.01 LU.
///
/// That last case is not a small error in the result. The exact method is
/// itself discontinuous in the gate: moving it past a few values changes
/// the ranks of the percentiles, and when a whole passage of the programme
/// sits on the gate, whether the 10th percentile falls in that passage or
/// above it. The range can then differ from the exact one by the distance
/// between the passages. `values_astride_the_gate_are_kept_or_dropped_together`
/// below measures such a case.
#[derive(Debug, Clone)]
pub(crate) struct RangeHistogram {
    counts: Vec<u64>,
    /// The sum of the mean squares in each bin. Only the gate's own bin is
    /// ever looked at, to see which side of the gate its values are on.
    sums: Vec<f64>,
    total_sum: f64,
    total_count: u64,
    lowest: usize,
    highest: usize,
}

impl RangeHistogram {
    /// An empty measurement. This allocates.
    pub(crate) fn new() -> Self {
        Self {
            counts: vec![0; BIN_COUNT],
            sums: vec![0.0; BIN_COUNT],
            total_sum: 0.0,
            total_count: 0,
            lowest: 0,
            highest: 0,
        }
    }

    /// Forgets every value.
    pub(crate) fn clear(&mut self) {
        if self.total_count > 0 {
            self.counts[self.lowest..=self.highest].fill(0);
            self.sums[self.lowest..=self.highest].fill(0.0);
        }
        self.total_sum = 0.0;
        self.total_count = 0;
    }

    /// Adds one short-term loudness value, given as a channel-weighted mean
    /// square over three seconds.
    pub(crate) fn push(&mut self, mean_square: f64) {
        let loudness = lufs(mean_square);
        // Tech 3342 keeps a value that is exactly on the absolute gate.
        let above_gate = loudness >= ABSOLUTE_GATE_LUFS;
        if !above_gate {
            return;
        }
        let bin = bin_of(loudness);
        self.counts[bin] += 1;
        self.sums[bin] += mean_square;
        if self.total_count == 0 {
            self.lowest = bin;
            self.highest = bin;
        } else {
            self.lowest = self.lowest.min(bin);
            self.highest = self.highest.max(bin);
        }
        self.total_sum += mean_square;
        self.total_count += 1;
    }

    /// The loudness range in LU, or zero while there is nothing to measure.
    pub(crate) fn range(&self) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        let gate = lufs(self.total_sum / self.total_count as f64) + RANGE_GATE_LU;
        // Every bin above the gate's own is kept whole and every bin below
        // it dropped whole, and both are exact. The gate's own bin goes by
        // the mean of its values. Tech 3342 keeps a value that is exactly
        // on the gate.
        let gate_bin = bin_of(gate);
        let values = self.counts[gate_bin];
        let keeps_gate_bin = values > 0 && lufs(self.sums[gate_bin] / values as f64) >= gate;
        let first = if keeps_gate_bin {
            gate_bin
        } else {
            gate_bin + 1
        };
        let first = first.max(self.lowest);
        if first > self.highest {
            return 0.0;
        }
        let kept: u64 = self.counts[first..=self.highest].iter().sum();
        if kept == 0 {
            return 0.0;
        }
        // Tech 3342 picks element `round((n - 1) * p / 100 + 1)` of the
        // sorted values, counting from one. Counting from zero, that is:
        let rank = |percentile: f64| ((kept - 1) as f64 * percentile / 100.0).round() as u64;
        let (low_rank, high_rank) = (rank(RANGE_PERCENTILES.0), rank(RANGE_PERCENTILES.1));

        let (mut low_bin, mut high_bin) = (None, first);
        let mut seen = 0u64;
        for bin in first..=self.highest {
            seen += self.counts[bin];
            if low_bin.is_none() && seen > low_rank {
                low_bin = Some(bin);
            }
            if seen > high_rank {
                high_bin = bin;
                break;
            }
        }
        (high_bin - low_bin.unwrap_or(first)) as f64 / BINS_PER_LU
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (SplitMix64), so the tests need no
    /// dependency.
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn mean_square_of(loudness: f64) -> f64 {
        10f64.powf((loudness - LOUDNESS_OFFSET) / 10.0)
    }

    /// BS.1770-4 equations (5) to (7), keeping every block, with the
    /// relative gate moved by `shift` LU.
    fn exact_integrated(blocks: &[f64], shift: f64) -> f64 {
        let above: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&block| lufs(block) > ABSOLUTE_GATE_LUFS)
            .collect();
        if above.is_empty() {
            return f64::NEG_INFINITY;
        }
        let gate = lufs(above.iter().sum::<f64>() / above.len() as f64) - 10.0 + shift;
        let kept: Vec<f64> = above
            .into_iter()
            .filter(|&block| lufs(block) > gate)
            .collect();
        lufs(kept.iter().sum::<f64>() / kept.len() as f64)
    }

    /// The MATLAB function printed in EBU Tech 3342, line for line. It is
    /// cut in three so that the tests can also ask what it would give with
    /// its relative gate somewhere else.
    fn exact_range(values: &[f64]) -> f64 {
        exact_range_with_the_gate_at(values, exact_range_gate(values))
    }

    /// The short-term values of that function that pass its absolute gate,
    /// in LUFS.
    fn above_the_absolute_gate(values: &[f64]) -> Vec<f64> {
        values
            .iter()
            .map(|&value| lufs(value))
            .filter(|&loudness| loudness >= -70.0)
            .collect()
    }

    /// The relative gate of that function, in LUFS.
    fn exact_range_gate(values: &[f64]) -> f64 {
        let above = above_the_absolute_gate(values);
        let power = above
            .iter()
            .map(|&loudness| 10f64.powf(loudness / 10.0))
            .sum::<f64>()
            / above.len() as f64;
        let integrated = 10.0 * power.log10();
        integrated - 20.0
    }

    /// The rest of that function, for a relative gate at `gate` LUFS.
    fn exact_range_with_the_gate_at(values: &[f64], gate: f64) -> f64 {
        let mut kept: Vec<f64> = above_the_absolute_gate(values)
            .into_iter()
            .filter(|&loudness| loudness >= gate)
            .collect();
        if kept.is_empty() {
            return 0.0;
        }
        kept.sort_by(f64::total_cmp);
        let n = kept.len() as f64;
        let low = kept[((n - 1.0) * 10.0 / 100.0 + 1.0).round() as usize - 1];
        let high = kept[((n - 1.0) * 95.0 / 100.0 + 1.0).round() as usize - 1];
        high - low
    }

    /// How far the histogram's range is from the exact one for the gate
    /// that suits it best among those less than one bin from the true
    /// gate. This is the accuracy the histogram promises: no more than the
    /// rounding of two percentiles, 0.01 LU.
    ///
    /// The exact result only changes when the gate passes a value, so it
    /// is enough to try the gate at both ends of that span and on every
    /// value inside it.
    fn distance_from_exact_for_a_gate_within_one_bin(values: &[f64]) -> f64 {
        let ours = histogram_range(values);
        let gate = exact_range_gate(values);
        let bin = BINS_PER_LU.recip();
        let mut gates = vec![gate - bin, gate, gate + bin];
        gates.extend(
            above_the_absolute_gate(values)
                .into_iter()
                .filter(|loudness| (loudness - gate).abs() < bin),
        );
        gates
            .into_iter()
            .map(|gate| (ours - exact_range_with_the_gate_at(values, gate)).abs())
            .fold(f64::INFINITY, f64::min)
    }

    fn histogram_integrated(blocks: &[f64]) -> f64 {
        let mut gated = GatedLoudness::new();
        for &block in blocks {
            gated.push(block);
        }
        gated.loudness()
    }

    fn histogram_range(values: &[f64]) -> f64 {
        let mut histogram = RangeHistogram::new();
        for &value in values {
            histogram.push(value);
        }
        histogram.range()
    }

    /// Programmes of very different character: loudness spread evenly over
    /// a wide span, clustered at a few levels, and drifting slowly.
    fn programmes() -> Vec<Vec<f64>> {
        let mut random = Random(7);
        let mut programmes = Vec::new();
        for (low, high) in [(-80.0, -5.0), (-40.0, -15.0), (-75.0, -60.0), (-30.0, 12.0)] {
            programmes.push(
                (0..20_000)
                    .map(|_| mean_square_of(low + (high - low) * random.next()))
                    .collect(),
            );
        }
        programmes.push(
            (0..20_000)
                .map(|index| {
                    let level = [-23.0, -33.5, -41.0, -18.0, -72.0][index % 5];
                    mean_square_of(level + 0.3 * random.next())
                })
                .collect(),
        );
        let mut level = -30.0;
        programmes.push(
            (0..50_000)
                .map(|_| {
                    level += 0.2 * (random.next() - 0.5);
                    mean_square_of(level)
                })
                .collect(),
        );
        programmes
    }

    #[test]
    fn loudness_is_the_formula_of_the_recommendation() {
        // A full-scale sine has a mean square of one half. With the gain of
        // the K-weighting at 997 Hz, +0.691 dB, it reads -3.01 LKFS.
        let weighted = 0.5 * 10f64.powf(0.691 / 10.0);
        assert!((lufs(weighted) + 3.01).abs() < 0.001);
        assert_eq!(lufs(0.0), f64::NEG_INFINITY);
        assert_eq!(lufs(-1.0), f64::NEG_INFINITY);
        assert_eq!(lufs(f64::NAN), f64::NEG_INFINITY);
    }

    #[test]
    fn the_histogram_agrees_with_keeping_every_block() {
        let mut worst = 0.0f64;
        for blocks in programmes() {
            let exact = exact_integrated(&blocks, 0.0);
            let ours = histogram_integrated(&blocks);
            println!("exact {exact:.6} LUFS, histogram {ours:.6} LUFS");
            worst = worst.max((ours - exact).abs());
            // The documented bound: the exact result for a gate within one
            // bin of the true one. A higher gate drops quiet blocks, so it
            // can only raise the result.
            let lower = exact_integrated(&blocks, -BINS_PER_LU.recip());
            let upper = exact_integrated(&blocks, BINS_PER_LU.recip());
            assert!(lower <= upper);
            assert!(
                ours >= lower - 1e-9 && ours <= upper + 1e-9,
                "{ours} is outside {lower}..{upper}"
            );
        }
        // In LU the difference is far smaller than the bin: what moves is
        // only the handful of blocks that share a bin with the gate, each
        // 10 LU under the mean.
        assert!(worst < 1e-3, "worst difference {worst} LU");
    }

    #[test]
    fn steady_levels_are_gated_exactly() {
        // 600 blocks at -23 LUFS and 200 quieter ones. With the quiet ones
        // at -34.139 the relative gate falls exactly on them, so these
        // levels put it a hair to either side, inside their own bin, where
        // a histogram that rounded to bin edges would decide wrongly.
        for quiet in [-33.0, -34.13, -34.138, -34.14, -34.15, -36.0] {
            let mut blocks = vec![mean_square_of(-23.0); 600];
            blocks.extend(vec![mean_square_of(quiet); 200]);
            let exact = exact_integrated(&blocks, 0.0);
            let ours = histogram_integrated(&blocks);
            assert!((ours - exact).abs() < 1e-9, "{quiet}: {ours} vs {exact}");
        }
    }

    #[test]
    fn the_absolute_gate_is_at_minus_seventy() {
        let mut gated = GatedLoudness::new();
        assert_eq!(gated.loudness(), f64::NEG_INFINITY);
        assert_eq!(gated.relative_gate(), f64::NEG_INFINITY);
        gated.push(mean_square_of(-70.001));
        gated.push(0.0);
        gated.push(f64::NAN);
        gated.push(-1.0);
        assert_eq!(gated.loudness(), f64::NEG_INFINITY);
        gated.push(mean_square_of(-69.999));
        assert!((gated.loudness() + 69.999).abs() < 1e-9);
        assert!((gated.relative_gate() + 79.999).abs() < 1e-9);
        gated.clear();
        assert_eq!(gated.loudness(), f64::NEG_INFINITY);
        gated.push(mean_square_of(-20.0));
        assert!((gated.loudness() + 20.0).abs() < 1e-9);
    }

    #[test]
    fn blocks_louder_than_the_histogram_reaches_are_still_averaged_exactly() {
        // All three land in the last bin, and the gate with them.
        let blocks = [
            mean_square_of(35.0),
            mean_square_of(40.0),
            mean_square_of(50.0),
        ];
        let ours = histogram_integrated(&blocks);
        // The quietest is more than 10 LU under the mean, so the exact
        // method drops it; sharing a bin with the others, it is kept. That
        // is the price of the histogram ending at +30 LUFS, a level no
        // signal within full scale can reach: a reading up there is the
        // plain mean of the blocks.
        let plain = lufs(blocks.iter().sum::<f64>() / 3.0);
        assert!((ours - plain).abs() < 1e-9, "{ours} vs {plain}");
        assert!(exact_integrated(&blocks, 0.0) > plain + 1.0);
    }

    #[test]
    fn the_range_agrees_with_the_reference_function() {
        for values in programmes() {
            let exact = exact_range(&values);
            let ours = histogram_range(&values);
            // Two percentiles, each within half a bin. In these programmes
            // the gate's bin holds a few values out of thousands, so which
            // way they go moves no percentile by more than that.
            assert!((ours - exact).abs() <= 0.0101, "{ours} vs {exact}");
            // What is promised whatever the gate's bin holds.
            let distance = distance_from_exact_for_a_gate_within_one_bin(&values);
            assert!(distance <= 0.0101, "{distance}");
        }
    }

    /// 300 short-term values at -20 LUFS, and `quiet` more whose levels
    /// `place` chooses once it is told where the relative gate will fall.
    /// The gate depends a little on those levels, so they are placed, the
    /// gate is found again, and so on until neither moves.
    fn loud_and_quiet(quiet: usize, place: impl Fn(f64, usize) -> f64) -> Vec<f64> {
        let build = |gate: f64| {
            let mut values = vec![mean_square_of(-20.0); 300];
            values.extend((0..quiet).map(|index| mean_square_of(place(gate, index))));
            values
        };
        let mut gate = -40.0;
        for _ in 0..50 {
            gate = exact_range_gate(&build(gate));
        }
        build(gate)
    }

    /// Where the bin that `loudness` falls into begins, in LUFS.
    fn bin_start(loudness: f64) -> f64 {
        ABSOLUTE_GATE_LUFS + bin_of(loudness) as f64 / BINS_PER_LU
    }

    #[test]
    fn a_steady_passage_at_the_gate_is_kept_or_dropped_exactly() {
        // A quiet passage some 21 LU under a loud one, at one level, a
        // hair above or below the relative gate. Kept, it holds the 10th
        // percentile and the range is the distance between the passages.
        // Dropped, the range is zero. Most of these levels share the
        // gate's bin. A histogram that went by the centre of that bin, as
        // this one used to, would keep them all and read 21 LU for the
        // ones under the gate.
        let (mut kept_in_the_gates_bin, mut dropped_in_the_gates_bin) = (0, 0);
        for offset in [
            -0.02, -0.0099, -0.006, -0.003, -0.0011, -0.0004, -0.0001, 0.0001, 0.0004, 0.0011,
            0.003, 0.006, 0.0099, 0.02,
        ] {
            let values = loud_and_quiet(100, |gate, _| gate + offset);
            let gate = exact_range_gate(&values);
            let quiet = lufs(values[300]);
            assert!((quiet - gate - offset).abs() < 1e-9, "{quiet} vs {gate}");
            let shares_the_bin = bin_of(quiet) == bin_of(gate);

            let exact = exact_range(&values);
            let ours = histogram_range(&values);
            println!(
                "quiet passage {offset:+.4} LU from the gate, which is {:.4} LU into its bin: \
                 {exact:.4} LU exact, {ours:.4} LU from the histogram",
                gate - bin_start(gate)
            );
            assert!(
                (ours - exact).abs() <= 0.0101,
                "{offset} LU from the gate: {ours} vs {exact}"
            );
            if offset > 0.0 {
                // Kept: the range is the distance between the passages.
                assert!((exact + 20.0 + quiet).abs() < 1e-9, "{exact}");
                kept_in_the_gates_bin += usize::from(shares_the_bin);
            } else {
                assert_eq!(exact, 0.0);
                dropped_in_the_gates_bin += usize::from(shares_the_bin);
            }
        }
        // Both outcomes occurred inside the gate's own bin, more than once.
        assert!(kept_in_the_gates_bin >= 2, "{kept_in_the_gates_bin}");
        assert!(dropped_in_the_gates_bin >= 2, "{dropped_in_the_gates_bin}");
    }

    #[test]
    fn values_astride_the_gate_are_kept_or_dropped_together() {
        // The limit of the histogram, in the open. A quiet passage of 36
        // values is spread evenly over the bin that the relative gate falls
        // into, so the gate has some of them on either side.
        let spread =
            |gate: f64, index: usize| bin_start(gate) + (index as f64 + 0.5) / 36.0 / BINS_PER_LU;
        let values = loud_and_quiet(36, spread);
        let gate = exact_range_gate(&values);
        let quiet: Vec<f64> = values[300..].iter().map(|&value| lufs(value)).collect();
        assert!(quiet.iter().all(|&level| bin_of(level) == bin_of(gate)));
        let above = quiet.iter().filter(|&&level| level >= gate).count();
        println!("gate at {gate:.5} LUFS; {above} of 36 quiet values are above it");
        assert!(above > 0 && above < 36);

        // The exact method keeps the ones above the gate. They are fewer
        // than a tenth of what is kept, so the 10th percentile is in the
        // loud passage and the range is zero.
        let exact = exact_range(&values);
        // The histogram goes by the mean of the bin, which is above the
        // gate, and keeps all 36. Now they are more than a tenth, the 10th
        // percentile is among them, and the range is the distance between
        // the passages.
        let ours = histogram_range(&values);
        println!("range: {exact:.4} LU exact, {ours:.4} LU from the histogram");
        assert_eq!(exact, 0.0);
        assert!((ours - 20.5).abs() < 0.1, "{ours}");

        // That is the exact result for a gate at the start of the bin, less
        // than a bin from where it is: the accuracy that is promised.
        let moved = exact_range_with_the_gate_at(&values, bin_start(gate));
        assert!(gate - bin_start(gate) < BINS_PER_LU.recip());
        assert!((ours - moved).abs() <= 0.0101, "{ours} vs {moved}");
        assert!(distance_from_exact_for_a_gate_within_one_bin(&values) <= 0.0101);

        // And the other way round: when the mean of the bin is under the
        // gate, all are dropped where the exact method keeps some. Here 60
        // quiet values lie just above the gate and 40 further below it,
        // halfway to the start of the bin. The exact method keeps the 60
        // and reads the distance between the passages.
        let lopsided = |gate: f64, index: usize| {
            let into_the_bin = gate - bin_start(gate);
            if index < 60 {
                gate + 0.01 * into_the_bin * (index as f64 + 1.0) / 60.0
            } else {
                gate - 0.5 * into_the_bin
            }
        };
        let values = loud_and_quiet(100, lopsided);
        let gate = exact_range_gate(&values);
        assert!(
            values[300..]
                .iter()
                .all(|&value| bin_of(lufs(value)) == bin_of(gate)),
            "the passage left the gate's bin at {gate}"
        );
        let (exact, ours) = (exact_range(&values), histogram_range(&values));
        println!("range: {exact:.4} LU exact, {ours:.4} LU from the histogram");
        assert!(exact > 20.0, "{exact}");
        assert_eq!(ours, 0.0);
        assert!(distance_from_exact_for_a_gate_within_one_bin(&values) <= 0.0101);
    }

    #[test]
    fn the_range_of_two_levels_is_their_distance() {
        // The first test signal of Tech 3342, reduced to its loudness
        // values: as many at -20 as at -30.
        let mut values = vec![mean_square_of(-20.0); 171];
        values.extend(vec![mean_square_of(-30.0); 171]);
        assert!((histogram_range(&values) - 10.0).abs() < 1e-9);
        // The fourth: the two quietest fifths are more than 20 LU under the
        // mean and are gated out, leaving 15 LU.
        let mut values = Vec::new();
        for level in [-50.0, -35.0, -20.0, -35.0, -50.0] {
            values.extend(vec![mean_square_of(level); 200]);
        }
        assert!((histogram_range(&values) - 15.0).abs() < 1e-9);
        assert!((exact_range(&values) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_range_is_zero() {
        let mut histogram = RangeHistogram::new();
        assert_eq!(histogram.range(), 0.0);
        histogram.push(mean_square_of(-80.0));
        histogram.push(f64::NAN);
        assert_eq!(histogram.range(), 0.0);
        histogram.push(mean_square_of(-23.0));
        assert_eq!(histogram.range(), 0.0);
        histogram.push(mean_square_of(-13.0));
        histogram.push(mean_square_of(-13.0));
        assert!(histogram.range() > 9.9);
        histogram.clear();
        assert_eq!(histogram.range(), 0.0);
    }
}
