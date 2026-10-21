//! Every backend against the scalar definitions, bit for bit, over lengths
//! from 0 up, odd ones, and slices that start at every alignment.

use super::*;

/// A small, fixed random generator (xorshift64*), so failures repeat.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

trait TestFloat: Sample {
    fn from_random_bits(bits: u64) -> Self;
    fn bits(self) -> u64;
    fn is_nan(self) -> bool;
    const SPECIALS: [Self; 10];
}

impl TestFloat for f32 {
    fn from_random_bits(bits: u64) -> Self {
        f32::from_bits(bits as u32)
    }
    fn bits(self) -> u64 {
        u64::from(self.to_bits())
    }
    fn is_nan(self) -> bool {
        f32::is_nan(self)
    }
    const SPECIALS: [f32; 10] = [
        0.0,
        -0.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
        1.0e-40,
        -1.0e-42,
        1.0,
    ];
}

impl TestFloat for f64 {
    fn from_random_bits(bits: u64) -> Self {
        f64::from_bits(bits)
    }
    fn bits(self) -> u64 {
        self.to_bits()
    }
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }
    const SPECIALS: [f64; 10] = [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        1.0e-310,
        -1.0e-320,
        1.0,
    ];
}

/// Mostly ordinary audio, with some special values and some arbitrary bit
/// patterns. NaN only if `nan` is set.
fn value<S: TestFloat>(rng: &mut Rng, nan: bool) -> S {
    loop {
        let x = match rng.below(10) {
            0 => S::SPECIALS[rng.below(S::SPECIALS.len())],
            1 | 2 => S::from_random_bits(rng.next()),
            _ => S::from_f64(rng.unit() * 4.0 - 2.0),
        };
        if nan || !x.is_nan() {
            return x;
        }
    }
}

fn values<S: TestFloat>(rng: &mut Rng, len: usize, nan: bool) -> Vec<S> {
    (0..len).map(|_| value(rng, nan)).collect()
}

/// The same value, or both NaN: a NaN's sign and payload are not
/// specified.
fn same<S: TestFloat>(a: S, b: S) -> bool {
    a.bits() == b.bits() || (a.is_nan() && b.is_nan())
}

#[track_caller]
fn assert_same<S: TestFloat>(got: &[S], want: &[S], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: lengths");
    for (index, (&g, &w)) in got.iter().zip(want).enumerate() {
        assert!(same(g, w), "{what}: element {index}: {g:?} is not {w:?}");
    }
}

/// Lengths to try: every one up to a few registers' worth, then some long
/// odd ones. Shorter under Miri, which only runs the scalar code.
fn lengths() -> Vec<usize> {
    let mut lengths: Vec<usize> = (0..=if cfg!(miri) { 9 } else { 70 }).collect();
    if !cfg!(miri) {
        lengths.extend([127, 128, 129, 255, 1_000, 1_023, 4_099]);
    }
    lengths
}

/// Where in a slice to start, so that every alignment is tried.
const OFFSETS: [usize; 4] = [0, 1, 2, 3];

fn backends() -> Vec<Backend> {
    let backends: Vec<Backend> = Backend::available().collect();
    assert!(backends.contains(&Backend::Scalar));
    backends
}

/// Runs `check` for every backend, length and offset, with a fresh
/// generator for each.
fn sweep(mut check: impl FnMut(Backend, &mut Rng, usize, usize, &str)) {
    for backend in backends() {
        for len in lengths() {
            for offset in OFFSETS {
                let mut rng = Rng::new((len * 7 + offset) as u64);
                let what = format!("{} len {len} offset {offset}", backend.name());
                check(backend, &mut rng, len, offset, &what);
            }
        }
    }
}

/// An operation on a slice, run on a backend.
type Op<'a, S> = dyn Fn(Backend, &mut [S]) + 'a;

fn element_wise<S: TestFloat>() {
    sweep(|backend, rng, len, offset, what| {
        let dst: Vec<S> = values(rng, len + offset, false);
        let src: Vec<S> = values(rng, len + offset, false);
        let gain: S = value(rng, false);
        let (dst, src) = (&dst[offset..], &src[offset..]);

        let run = |backend: Backend, op: &Op<'_, S>| {
            let mut out = dst.to_vec();
            op(backend, &mut out);
            out
        };
        let ops: [(&str, &Op<'_, S>); 6] = [
            ("scale", &|b, d| b.scale(d, gain)),
            ("copy_scaled", &|b, d| b.copy_scaled(d, src, gain)),
            ("add", &|b, d| b.add(d, src)),
            ("add_scaled", &|b, d| b.add_scaled(d, src, gain)),
            ("multiply", &|b, d| b.multiply(d, src)),
            ("scale_ramp", &|b, d| {
                b.scale_ramp(d, gain, src.first().copied().unwrap_or(S::ONE))
            }),
        ];
        for (name, op) in ops {
            let want = run(Backend::Scalar, op);
            assert_same(&run(backend, op), &want, &format!("{name} {what}"));
        }

        // The definitions themselves, written out once more.
        let mut by_hand = dst.to_vec();
        for (d, &s) in by_hand.iter_mut().zip(src) {
            *d += s * gain;
        }
        assert_same(
            &run(backend, ops[3].1),
            &by_hand,
            &format!("add_scaled by hand {what}"),
        );
    });
}

#[test]
fn element_wise_operations_match_the_definition_bit_for_bit_f32() {
    element_wise::<f32>();
}

#[test]
fn element_wise_operations_match_the_definition_bit_for_bit_f64() {
    element_wise::<f64>();
}

fn ramp_is_closed_form<S: TestFloat>() {
    for backend in backends() {
        let mut ones = vec![S::ONE; 37];
        backend.scale_ramp(&mut ones, S::from_f64(0.5), S::from_f64(-0.125));
        for (index, &gain) in ones.iter().enumerate() {
            let want = S::from_f64(0.5) + S::from_f64(-0.125) * S::from_f64(index as f64);
            assert!(same(gain, want), "{} index {index}", backend.name());
        }
    }
}

#[test]
fn a_ramp_is_worked_out_from_each_index() {
    ramp_is_closed_form::<f32>();
    ramp_is_closed_form::<f64>();
}

fn stereo<S: TestFloat>() {
    sweep(|backend, rng, len, offset, what| {
        let left: Vec<S> = values(rng, len + offset, true);
        let right: Vec<S> = values(rng, len + offset, true);
        let (left, right) = (&left[offset..], &right[offset..]);
        let mut interleaved = vec![S::ZERO; 2 * len + offset];
        backend.interleave_stereo(&mut interleaved[offset..], left, right);
        let interleaved = &interleaved[offset..];
        for frame in 0..len {
            // Moves only: NaNs keep their bits too.
            assert_eq!(interleaved[2 * frame].bits(), left[frame].bits(), "{what}");
            assert_eq!(
                interleaved[2 * frame + 1].bits(),
                right[frame].bits(),
                "{what}"
            );
        }
        let (mut l, mut r) = (vec![S::ZERO; len + offset], vec![S::ZERO; len + 1]);
        backend.deinterleave_stereo(&mut l[offset..], &mut r[1..], interleaved);
        assert!(
            l[offset..]
                .iter()
                .zip(left)
                .all(|(a, b)| a.bits() == b.bits()),
            "{what}"
        );
        assert!(
            r[1..].iter().zip(right).all(|(a, b)| a.bits() == b.bits()),
            "{what}"
        );
    });
}

#[test]
fn stereo_interleaving_moves_every_bit() {
    stereo::<f32>();
    stereo::<f64>();
}

fn complex<S: TestFloat>() {
    sweep(|backend, rng, len, offset, what| {
        let len = 2 * len;
        let sum: Vec<S> = values(rng, len + offset, false);
        let a: Vec<S> = values(rng, len + offset, false);
        let b: Vec<S> = values(rng, len + offset, false);
        let (a, b) = (&a[offset..], &b[offset..]);
        let mut want = sum[offset..].to_vec();
        Backend::Scalar.complex_multiply_add(&mut want, a, b);
        let mut got = sum[offset..].to_vec();
        backend.complex_multiply_add(&mut got, a, b);
        assert_same(&got, &want, &format!("complex_multiply_add {what}"));
    });
}

#[test]
fn complex_products_match_the_definition_bit_for_bit() {
    complex::<f32>();
    complex::<f64>();
}

fn peaks<S: TestFloat>() {
    sweep(|backend, rng, len, offset, what| {
        let with_nan = rng.below(4) == 0;
        let src: Vec<S> = values(rng, len + offset, with_nan);
        let src = &src[offset..];
        let want = src.iter().fold(S::ZERO, |peak, &x| {
            let x = x.abs();
            if peak.is_nan() || x.is_nan() {
                S::from_f64(f64::NAN)
            } else if x > peak {
                x
            } else {
                peak
            }
        });
        let got = backend.peak(src);
        assert!(same(got, want), "peak {what}: {got:?} is not {want:?}");
        assert!(same(got, Backend::Scalar.peak(src)), "peak {what}");
    });
}

#[test]
fn peaks_are_the_same_on_every_backend_and_see_every_nan() {
    peaks::<f32>();
    peaks::<f64>();
    for backend in backends() {
        let mut quiet = vec![0.25f32; 100];
        quiet[77] = -0.5;
        assert_eq!(backend.peak(&quiet), 0.5);
        quiet[99] = f32::NAN;
        assert!(backend.peak(&quiet).is_nan(), "{}", backend.name());
        assert_eq!(backend.peak::<f32>(&[]), 0.0);
        assert_eq!(backend.peak(&[-0.0f64]).to_bits(), 0.0f64.to_bits());
    }
}

/// Sums: the same bits on every backend, because the order is fixed, and
/// close to the exact sum.
fn sums<S: TestFloat>(tolerance: f64) {
    sweep(|backend, rng, len, offset, what| {
        // Ordinary values, so that the sum can be compared with an exact
        // one; the bit-for-bit comparison also gets special values.
        let ordinary = |rng: &mut Rng| -> Vec<S> {
            (0..len + offset)
                .map(|_| S::from_f64(rng.unit() * 2.0 - 1.0))
                .collect()
        };
        let (a, b) = (ordinary(rng), ordinary(rng));
        let (a, b) = (&a[offset..], &b[offset..]);

        let dot = backend.dot(a, b);
        assert!(same(dot, Backend::Scalar.dot(a, b)), "dot {what}");
        let squares = backend.sum_of_squares(a);
        assert_eq!(
            squares.to_bits(),
            Backend::Scalar.sum_of_squares(a).to_bits(),
            "{what}"
        );

        let exact_dot: f64 = a.iter().zip(b).map(|(x, y)| x.to_f64() * y.to_f64()).sum();
        let scale: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (x.to_f64() * y.to_f64()).abs())
            .sum();
        assert!(
            (dot.to_f64() - exact_dot).abs() <= tolerance * scale.max(f64::MIN_POSITIVE),
            "dot {what}: {dot:?} against {exact_dot}"
        );
        let exact_squares: f64 = a.iter().map(|x| x.to_f64() * x.to_f64()).sum();
        assert!(
            (squares - exact_squares).abs() <= 1e-12 * exact_squares.max(f64::MIN_POSITIVE),
            "sum_of_squares {what}: {squares} against {exact_squares}"
        );

        let (x, y): (Vec<S>, Vec<S>) = (values(rng, len, true), values(rng, len, true));
        assert!(
            same(backend.dot(&x, &y), Backend::Scalar.dot(&x, &y)),
            "dot specials {what}"
        );
        let (p, q) = (
            backend.sum_of_squares(&x),
            Backend::Scalar.sum_of_squares(&x),
        );
        assert!(
            p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan()),
            "{what}"
        );
    });
}

#[test]
fn sums_are_the_same_on_every_backend_and_close_to_exact() {
    // Sixteen running sums, then four rounds of pairs: each product's error
    // grows with the number of additions it goes through, at most
    // `len / 16 + 4` for the longest slices tried.
    sums::<f32>(300.0 * f64::from(f32::EPSILON));
    sums::<f64>(600.0 * f64::EPSILON);
}

#[test]
fn the_order_of_a_sum_is_sixteen_lanes_then_pairs_then_the_rest() {
    // 1 + 2^-24 is not an `f32`: it rounds to 1, so the order shows.
    // Elements 0, 1 and 2 go to running sums 0, 1 and 2: 1, 2^-24 and -1.
    // In pairs, 0 + 2 gives 0 and 1 + 3 gives 2^-24, and then 0 + 2^-24.
    // Elements 16 to 19 are left over and added one after another; element
    // 17 makes 2^-23, which is exact. One after another from the first,
    // 1 + 2^-24 is 1 and the second 2^-24 comes after the -1: 2^-24.
    let tiny = f32::EPSILON / 2.0;
    let mut a = [0.0f32; 20];
    a[0] = 1.0;
    a[1] = tiny;
    a[2] = -1.0;
    a[17] = tiny;
    let ones = [1.0f32; 20];
    for backend in backends() {
        assert_eq!(backend.dot(&a, &ones), f32::EPSILON, "{}", backend.name());
    }
    let one_after_another = a.iter().fold(0.0f32, |sum, &x| sum + x);
    assert_eq!(one_after_another, tiny);
}

#[test]
fn integers_of_16_bits_decode_exactly_every_value() {
    let bytes: Vec<u8> = (0..=u16::MAX).flat_map(|v| v.to_le_bytes()).collect();
    let mut want = vec![0.0f32; 65_536];
    Backend::Scalar.i16_le_to_f32(&mut want, &bytes);
    for (v, &x) in (0..=u16::MAX).zip(&want) {
        assert_eq!(x, f32::from(v as i16) / 32_768.0);
    }
    for backend in backends() {
        for offset in OFFSETS {
            let len = 65_536 - offset;
            let mut got = vec![0.0f32; len];
            backend.i16_le_to_f32(&mut got, &bytes[2 * offset..]);
            assert_same(&got, &want[offset..], backend.name());
        }
    }
}

#[test]
fn integers_of_24_bits_decode_exactly_every_value() {
    // The 2^24 values in 256 pieces, every piece at another alignment.
    // One piece in seven, and the last, which holds the negative extremes,
    // in an ordinary run; all of them with `--ignored`.
    every_24_bit_value(|p| p % 7 == 0 || p == 255);
}

#[test]
#[ignore = "16 million values on each backend, twice: run by hand"]
fn integers_of_24_bits_decode_exactly_every_value_all_of_them() {
    every_24_bit_value(|_| true);
}

fn every_24_bit_value(wanted: impl Fn(usize) -> bool) {
    let piece = if cfg!(miri) { 64 } else { 1 << 16 };
    let pieces = if cfg!(miri) { 2 } else { 256 };
    let mut bytes = vec![0u8; 3 * piece + 3];
    let (mut want, mut got) = (vec![0.0f32; piece], vec![0.0f32; piece]);
    for p in (0..pieces).filter(|&p| wanted(p)) {
        let offset = p % 4;
        let codes = &mut bytes[offset..offset + 3 * piece];
        for (k, code) in codes.chunks_exact_mut(3).enumerate() {
            code.copy_from_slice(&((p * piece + k) as u32).to_le_bytes()[..3]);
        }
        let codes = &bytes[offset..offset + 3 * piece];
        Backend::Scalar.i24_le_to_f32(&mut want, codes);
        for backend in backends() {
            backend.i24_le_to_f32(&mut got, codes);
            assert_same(&got, &want, backend.name());
            // A shorter slice ends at every point of a group.
            let short = piece - 1 - p % 7;
            backend.i24_le_to_f32(&mut got[..short], &codes[..3 * short]);
            assert_same(&got[..short], &want[..short], backend.name());
        }
    }
}

#[test]
fn integers_of_24_bits_decode_at_their_extremes() {
    let mut extremes = [0.0f32; 3];
    Backend::Scalar.i24_le_to_f32(
        &mut extremes,
        &[0xFF, 0xFF, 0x7F, 0, 0, 0x80, 0xFF, 0xFF, 0xFF],
    );
    assert_eq!(
        extremes,
        [8_388_607.0 / 8_388_608.0, -1.0, -1.0 / 8_388_608.0]
    );
}

#[test]
fn integers_of_32_bits_decode_as_one_rounding() {
    let mut rng = Rng::new(32);
    let mut words: Vec<i32> = vec![0, 1, -1, i32::MAX, i32::MIN, 1 << 24, (1 << 24) + 1];
    // Halfway cases between two `f32`, which round to even.
    for shift in 0..8 {
        words.push((1 << 24 | 1) << shift);
        words.push(-((1 << 24 | 3) << shift));
    }
    let count = if cfg!(miri) { 40 } else { 100_000 };
    words.extend((0..count).map(|_| rng.next() as i32));
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut want = vec![0.0f32; words.len()];
    Backend::Scalar.i32_le_to_f32(&mut want, &bytes);
    for (&w, &x) in words.iter().zip(&want) {
        assert_eq!(x, (f64::from(w) / 2_147_483_648.0) as f32);
    }
    for backend in backends() {
        for offset in OFFSETS {
            let mut got = vec![0.0f32; words.len() - offset];
            backend.i32_le_to_f32(&mut got, &bytes[4 * offset..]);
            assert_same(&got, &want[offset..], backend.name());
        }
    }
}

/// Samples that test rounding to integers: around every halfway point
/// near full scale and near zero, specials, and arbitrary bit patterns.
fn quantisation_inputs(full_scale: f64) -> Vec<f32> {
    let mut rng = Rng::new(16);
    let mut samples = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        1.5,
        -7.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        -f32::NAN,
        f32::MIN_POSITIVE,
        1.0e-40,
    ];
    let near = |k: f64| {
        let x = (k / full_scale) as f32;
        [
            x,
            f32::from_bits(x.to_bits().wrapping_add(1)),
            f32::from_bits(x.to_bits().wrapping_sub(1)),
        ]
    };
    let count = if cfg!(miri) { 4 } else { 2_000 };
    for k in 0..count {
        let k = f64::from(k);
        samples.extend(near(k + 0.5));
        samples.extend(near(-k - 0.5));
        samples.extend(near(full_scale - k - 0.5));
        samples.extend(near(-full_scale + k + 0.5));
    }
    let count = if cfg!(miri) { 40 } else { 200_000 };
    for _ in 0..count {
        samples.push(f32::from_bits(rng.next() as u32));
        samples.push((rng.unit() * 2.2 - 1.1) as f32);
    }
    samples
}

#[test]
fn samples_encode_as_16_bit_integers_bit_for_bit() {
    let samples = quantisation_inputs(32_767.0);
    let mut want = vec![0u8; 2 * samples.len()];
    Backend::Scalar.f32_to_i16_le(&mut want, &samples);
    for (bytes, &x) in want.chunks_exact(2).zip(&samples) {
        let value = (x.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        assert_eq!(bytes, value.to_le_bytes(), "{x}");
    }
    for backend in backends() {
        for offset in OFFSETS {
            let mut got = vec![0u8; 2 * (samples.len() - offset)];
            backend.f32_to_i16_le(&mut got, &samples[offset..]);
            assert!(got == want[2 * offset..], "{}", backend.name());
        }
    }
}

#[test]
fn samples_encode_as_24_bit_integers_bit_for_bit() {
    let samples = quantisation_inputs(8_388_607.0);
    let mut want = vec![0u8; 3 * samples.len()];
    Backend::Scalar.f32_to_i24_le(&mut want, &samples);
    for (bytes, &x) in want.chunks_exact(3).zip(&samples) {
        let value = (f64::from(x.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
        assert_eq!(bytes, &value.to_le_bytes()[..3], "{x}");
    }
    for backend in backends() {
        for offset in OFFSETS {
            let mut got = vec![0u8; 3 * (samples.len() - offset)];
            backend.f32_to_i24_le(&mut got, &samples[offset..]);
            assert!(got == want[3 * offset..], "{}", backend.name());
        }
    }
}

/// Every `f32` there is, through both encoders on every backend. Too slow
/// for every run; run it with `--ignored` in an optimised build.
#[test]
#[ignore = "4 billion samples: run by hand in an optimised build"]
fn every_f32_encodes_bit_for_bit() {
    const PIECE: usize = 1 << 16;
    let mut samples = vec![0.0f32; PIECE];
    let (mut want16, mut got16) = (vec![0u8; 2 * PIECE], vec![0u8; 2 * PIECE]);
    let (mut want24, mut got24) = (vec![0u8; 3 * PIECE], vec![0u8; 3 * PIECE]);
    for piece in 0..(1u64 << 32) / PIECE as u64 {
        for (k, sample) in samples.iter_mut().enumerate() {
            *sample = f32::from_bits((piece * PIECE as u64 + k as u64) as u32);
        }
        Backend::Scalar.f32_to_i16_le(&mut want16, &samples);
        Backend::Scalar.f32_to_i24_le(&mut want24, &samples);
        for backend in backends().into_iter().skip(1) {
            backend.f32_to_i16_le(&mut got16, &samples);
            assert!(got16 == want16, "{} piece {piece}", backend.name());
            backend.f32_to_i24_le(&mut got24, &samples);
            assert!(got24 == want24, "{} piece {piece}", backend.name());
        }
    }
}

/// A ramp longer than 2^24 frames, where `f32` indices stop being exact
/// and the vector code hands over to the definition.
#[test]
#[ignore = "64 MB: run by hand"]
fn a_ramp_past_two_to_the_twenty_four_frames_matches() {
    let len = (1 << 24) + 1_000;
    let mut want = vec![1.0f32; len];
    Backend::Scalar.scale_ramp(&mut want, 0.0, 1.0);
    for backend in backends() {
        let mut got = vec![1.0f32; len];
        backend.scale_ramp(&mut got, 0.0, 1.0);
        assert_same(&got, &want, backend.name());
    }
}

#[test]
fn detection_picks_an_available_backend_and_keeps_it() {
    let best = Backend::detect();
    assert!(best.is_available());
    assert_eq!(Backend::detect(), best);
    assert!(Backend::Scalar.is_available());
    if cfg!(miri) {
        assert_eq!(best, Backend::Scalar);
    }
    #[cfg(all(not(miri), target_arch = "x86_64"))]
    assert!(Backend::Sse2.is_available(), "SSE2 is part of x86-64");
    #[cfg(all(not(miri), target_arch = "aarch64"))]
    assert!(Backend::Neon.is_available(), "NEON is part of AArch64");
    // A backend this processor lacks runs the definition instead.
    for backend in Backend::ALL {
        let mut x = [1.0f32, 2.0, 3.0];
        backend.scale(&mut x, 2.0);
        assert_eq!(x, [2.0, 4.0, 6.0], "{}", backend.name());
    }
}

/// A sample type of its own runs the definitions.
#[test]
fn other_sample_types_use_the_definitions() {
    #[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
    struct Wrapped(f64);
    macro_rules! op {
        ($tr:ident, $f:ident, $op:tt) => {
            impl core::ops::$tr for Wrapped {
                type Output = Self;
                fn $f(self, other: Self) -> Self {
                    Wrapped(self.0 $op other.0)
                }
            }
        };
    }
    op!(Add, add, +);
    op!(Sub, sub, -);
    op!(Mul, mul, *);
    op!(Div, div, /);
    impl core::ops::Neg for Wrapped {
        type Output = Self;
        fn neg(self) -> Self {
            Wrapped(-self.0)
        }
    }
    impl core::ops::AddAssign for Wrapped {
        fn add_assign(&mut self, other: Self) {
            self.0 += other.0;
        }
    }
    impl core::ops::SubAssign for Wrapped {
        fn sub_assign(&mut self, other: Self) {
            self.0 -= other.0;
        }
    }
    impl core::ops::MulAssign for Wrapped {
        fn mul_assign(&mut self, other: Self) {
            self.0 *= other.0;
        }
    }
    impl Sample for Wrapped {
        const ZERO: Self = Wrapped(0.0);
        const ONE: Self = Wrapped(1.0);
        fn from_f64(value: f64) -> Self {
            Wrapped(value)
        }
        fn to_f64(self) -> f64 {
            self.0
        }
        fn from_f32(value: f32) -> Self {
            Wrapped(f64::from(value))
        }
        fn to_f32(self) -> f32 {
            self.0 as f32
        }
        fn abs(self) -> Self {
            Wrapped(self.0.abs())
        }
        fn sqrt(self) -> Self {
            Wrapped(self.0.sqrt())
        }
    }

    let mut x = [Wrapped(1.0), Wrapped(-3.0), Wrapped(2.0)];
    add_scaled(&mut x, &[Wrapped(1.0); 3], Wrapped(0.5));
    assert_eq!(x, [Wrapped(1.5), Wrapped(-2.5), Wrapped(2.5)]);
    assert_eq!(peak(&x), Wrapped(2.5));
    assert_eq!(dot(&x, &x), Wrapped(1.5 * 1.5 + 2.5 * 2.5 + 2.5 * 2.5));
    assert_eq!(sum_of_squares(&x), 1.5 * 1.5 + 2.5 * 2.5 + 2.5 * 2.5);
}

#[test]
#[should_panic(expected = "slice lengths differ")]
fn slices_of_different_lengths_are_refused() {
    add(&mut [0.0f32; 3], &[1.0; 4]);
}
