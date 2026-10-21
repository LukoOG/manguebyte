//! Times the operations of `manguebyte_core::vector` on every backend this
//! processor has, against the plain loops they replaced.
//!
//! ```text
//! cargo run --release -p manguebyte-core --example vector_speed
//! ```
//!
//! For each operation it prints nanoseconds per element (per sample, or
//! per complex number for the complex products) for blocks of 512 elements,
//! which stay in the first-level cache as an audio block does, and of
//! 65 536, which do not; and the speed-up of the fastest backend over the
//! loop it replaced. Each figure is the best of several runs of at least
//! 20 ms, so other work on the machine shows less. Build it optimised:
//! the debug figures mean nothing.
//!
//! "Before" is the loop as it was written before the vector module: the
//! `AudioBuffer` helpers' loops, the convolution's four-sum dot product and
//! complex product, and the WAV reader's and writer's sample-at-a-time
//! conversions.

use std::hint::black_box;
use std::time::{Duration, Instant};

use manguebyte_core::Sample;
use manguebyte_core::vector::Backend;

/// Seconds per call of `work`, the best of several timed runs.
fn time(mut work: impl FnMut()) -> f64 {
    work();
    let mut best = f64::INFINITY;
    for _ in 0..7 {
        let start = Instant::now();
        let mut calls = 0u64;
        while start.elapsed() < Duration::from_millis(20) {
            for _ in 0..16 {
                work();
            }
            calls += 16;
        }
        best = best.min(start.elapsed().as_secs_f64() / calls as f64);
    }
    best
}

/// The loops as they were before.
mod before {
    use manguebyte_core::Sample;

    pub fn scale<S: Sample>(dst: &mut [S], gain: S) {
        for sample in dst {
            *sample *= gain;
        }
    }

    pub fn ramp<S: Sample>(dst: &mut [S], start: S, step: S) {
        let mut gain = start;
        for sample in dst {
            *sample *= gain;
            gain += step;
        }
    }

    pub fn add_scaled<S: Sample>(dst: &mut [S], src: &[S], gain: S) {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d += s * gain;
        }
    }

    pub fn add<S: Sample>(dst: &mut [S], src: &[S]) {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d += s;
        }
    }

    pub fn peak<S: Sample>(src: &[S]) -> S {
        let mut peak = S::ZERO;
        for &sample in src {
            let magnitude = sample.abs();
            if magnitude.to_f64().is_nan() {
                return magnitude;
            }
            if magnitude > peak {
                peak = magnitude;
            }
        }
        peak
    }

    pub fn sum_of_squares<S: Sample>(src: &[S]) -> f64 {
        src.iter().map(|s| s.to_f64() * s.to_f64()).sum()
    }

    pub fn dot<S: Sample>(a: &[S], b: &[S]) -> S {
        let mut sums = [S::ZERO; 4];
        let (a_chunks, b_chunks) = (a.chunks_exact(4), b.chunks_exact(4));
        let (a_rest, b_rest) = (a_chunks.remainder(), b_chunks.remainder());
        for (x, y) in a_chunks.zip(b_chunks) {
            sums[0] += x[0] * y[0];
            sums[1] += x[1] * y[1];
            sums[2] += x[2] * y[2];
            sums[3] += x[3] * y[3];
        }
        let mut total = (sums[0] + sums[1]) + (sums[2] + sums[3]);
        for (&x, &y) in a_rest.iter().zip(b_rest) {
            total += x * y;
        }
        total
    }

    #[derive(Clone, Copy)]
    pub struct Complex<S> {
        pub re: S,
        pub im: S,
    }

    pub fn complex_multiply_add<S: Sample>(
        sum: &mut [Complex<S>],
        a: &[Complex<S>],
        b: &[Complex<S>],
    ) {
        for ((sum, &a), &b) in sum.iter_mut().zip(a).zip(b) {
            sum.re += a.re * b.re - a.im * b.im;
            sum.im += a.re * b.im + a.im * b.re;
        }
    }

    pub fn interleave<S: Sample>(dst: &mut [S], channels: [&[S]; 2]) {
        for (index, channel) in channels.iter().enumerate() {
            for (frame, &sample) in channel.iter().enumerate() {
                dst[frame * 2 + index] = sample;
            }
        }
    }

    pub fn deinterleave<S: Sample>(channels: [&mut [S]; 2], src: &[S]) {
        for (index, channel) in channels.into_iter().enumerate() {
            for (frame, sample) in channel.iter_mut().enumerate() {
                *sample = src[frame * 2 + index];
            }
        }
    }

    /// The WAV reader's decoder, called once a sample through a stride.
    fn decode_sample(bytes: &[u8]) -> f32 {
        match *bytes {
            [a, b] => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
            [a, b, c] => (i32::from_le_bytes([0, a, b, c]) >> 8) as f32 / 8_388_608.0,
            [a, b, c, d] => (f64::from(i32::from_le_bytes([a, b, c, d])) / 2_147_483_648.0) as f32,
            _ => unreachable!(),
        }
    }

    pub fn decode(dst: &mut [f32], src: &[u8], width: usize) {
        for (value, sample) in dst.iter_mut().zip(src.chunks(width)) {
            *value = decode_sample(&sample[..width]);
        }
    }

    pub fn encode(dst: &mut [u8], src: &[f32], width: usize) {
        for (&sample, bytes) in src.iter().zip(dst.chunks_mut(width)) {
            if width == 2 {
                let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                bytes[..2].copy_from_slice(&value.to_le_bytes());
            } else {
                let value = (f64::from(sample.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
                bytes[..3].copy_from_slice(&value.to_le_bytes()[..3]);
            }
        }
    }
}

/// One line of the table: the old loop and each backend, in seconds per
/// call.
struct Row {
    name: String,
    elements: usize,
    before: f64,
    backends: Vec<(Backend, f64)>,
}

impl Row {
    fn print(&self) {
        let per = |seconds: f64| seconds * 1e9 / self.elements as f64;
        let mut line = format!("| {:<28} | {:>7.3} |", self.name, per(self.before));
        for &(_, seconds) in &self.backends {
            line += &format!(" {:>7.3} |", per(seconds));
        }
        let best = self
            .backends
            .iter()
            .map(|&(_, seconds)| seconds)
            .fold(f64::INFINITY, f64::min);
        line += &format!(" {:>5.1}x |", self.before / best);
        println!("{line}");
    }
}

fn signal<S: Sample>(len: usize, seed: u64) -> Vec<S> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            S::from_f64((state >> 11) as f64 / (1u64 << 53) as f64 * 1.8 - 0.9)
        })
        .collect()
}

fn float_rows<S: Sample>(type_name: &str, len: usize, backends: &[Backend], rows: &mut Vec<Row>) {
    let a: Vec<S> = signal(len, 1);
    let b: Vec<S> = signal(len, 2);
    let mut out = a.clone();
    // Gains of one, or as near, so that a block scaled again and again
    // neither dies away into subnormal numbers nor grows without end.
    let gain = black_box(S::ONE);
    let step = black_box(S::from_f64(1e-12));
    let mut row =
        |name: &str, elements: usize, before: f64, each: &mut dyn FnMut(Backend) -> f64| {
            rows.push(Row {
                name: format!("{name} {type_name}"),
                elements,
                before,
                backends: backends
                    .iter()
                    .map(|&backend| (backend, each(backend)))
                    .collect(),
            });
        };

    let t = time(|| before::scale(black_box(&mut out), gain));
    row("scale", len, t, &mut |k| {
        time(|| k.scale(black_box(&mut out), gain))
    });
    let t = time(|| before::ramp(black_box(&mut out), gain, step));
    row("scale_ramp", len, t, &mut |k| {
        time(|| k.scale_ramp(black_box(&mut out), gain, step))
    });
    let t = time(|| before::add(black_box(&mut out), &b));
    row("add", len, t, &mut |k| {
        time(|| k.add(black_box(&mut out), &b))
    });
    let t = time(|| before::add_scaled(black_box(&mut out), &b, gain));
    row("add_scaled", len, t, &mut |k| {
        time(|| k.add_scaled(black_box(&mut out), &b, gain))
    });
    let t = time(|| {
        black_box(before::peak(black_box(&a)));
    });
    row("peak", len, t, &mut |k| {
        time(|| {
            black_box(k.peak(black_box(&a)));
        })
    });
    let t = time(|| {
        black_box(before::sum_of_squares(black_box(&a)));
    });
    row("sum_of_squares", len, t, &mut |k| {
        time(|| {
            black_box(k.sum_of_squares(black_box(&a)));
        })
    });
    let t = time(|| {
        black_box(before::dot(black_box(&a), &b));
    });
    row("dot", len, t, &mut |k| {
        time(|| {
            black_box(k.dot(black_box(&a), &b));
        })
    });

    let to_complex = |v: &[S]| -> Vec<before::Complex<S>> {
        v.chunks_exact(2)
            .map(|p| before::Complex { re: p[0], im: p[1] })
            .collect()
    };
    let (ca, cb) = (to_complex(&a), to_complex(&b));
    let mut csum = to_complex(&out);
    let t = time(|| before::complex_multiply_add(black_box(&mut csum), &ca, &cb));
    row("complex_multiply_add", len / 2, t, &mut |k| {
        time(|| k.complex_multiply_add(black_box(&mut out), &a, &b))
    });

    let mut frames = vec![S::ZERO; 2 * len];
    let t = time(|| before::interleave(black_box(&mut frames), [&a, &b]));
    row("interleave_stereo", len, t, &mut |k| {
        time(|| k.interleave_stereo(black_box(&mut frames), &a, &b))
    });
    let (mut left, mut right) = (vec![S::ZERO; len], vec![S::ZERO; len]);
    let t = time(|| before::deinterleave([black_box(&mut left), &mut right], &frames));
    row("deinterleave_stereo", len, t, &mut |k| {
        time(|| k.deinterleave_stereo(black_box(&mut left), &mut right, &frames))
    });
}

fn conversion_rows(len: usize, backends: &[Backend], rows: &mut Vec<Row>) {
    let samples: Vec<f32> = signal(len, 3);
    let mut bytes = vec![0u8; 4 * len];
    let mut out = vec![0.0f32; len];
    let mut row = |name: &str, before: f64, each: &mut dyn FnMut(Backend) -> f64| {
        rows.push(Row {
            name: name.to_string(),
            elements: len,
            before,
            backends: backends
                .iter()
                .map(|&backend| (backend, each(backend)))
                .collect(),
        });
    };
    for (width, name) in [(2usize, "16"), (3, "24"), (4, "32")] {
        let t = time(|| before::decode(black_box(&mut out), &bytes[..width * len], width));
        row(&format!("i{name}_le_to_f32"), t, &mut |k| {
            time(|| match width {
                2 => k.i16_le_to_f32(black_box(&mut out), &bytes[..2 * len]),
                3 => k.i24_le_to_f32(black_box(&mut out), &bytes[..3 * len]),
                _ => k.i32_le_to_f32(black_box(&mut out), &bytes[..4 * len]),
            })
        });
    }
    for (width, name) in [(2usize, "16"), (3, "24")] {
        let t = time(|| before::encode(black_box(&mut bytes[..width * len]), &samples, width));
        row(&format!("f32_to_i{name}_le"), t, &mut |k| {
            time(|| match width {
                2 => k.f32_to_i16_le(black_box(&mut bytes[..2 * len]), &samples),
                _ => k.f32_to_i24_le(black_box(&mut bytes[..3 * len]), &samples),
            })
        });
    }
}

/// Dot products as short as a filter's, where the cost of a call counts.
fn short_dot_rows<S: Sample>(type_name: &str, backends: &[Backend], rows: &mut Vec<Row>) {
    for len in [15, 63, 255] {
        let (a, b): (Vec<S>, Vec<S>) = (signal(len, 4), signal(len, 5));
        let before = time(|| {
            black_box(before::dot(black_box(&a), &b));
        });
        rows.push(Row {
            name: format!("dot of {len} {type_name}"),
            elements: len,
            before,
            backends: backends
                .iter()
                .map(|&k| {
                    (
                        k,
                        time(|| {
                            black_box(k.dot(black_box(&a), &b));
                        }),
                    )
                })
                .collect(),
        });
    }
}

fn header(backends: &[Backend]) {
    let mut header = format!("| {:<28} | {:>7} |", "operation", "before");
    let mut rule = format!("|{:-<30}|{:->9}|", "", "");
    for backend in backends {
        header += &format!(" {:>7} |", backend.name());
        rule += &format!("{:->9}|", "");
    }
    header += " speed-up |";
    rule += "-------:|";
    println!("{header}\n{rule}");
}

fn main() {
    // As on an audio thread.
    let _denormals = manguebyte_core::NoDenormals::new();
    let backends: Vec<Backend> = Backend::available().collect();
    println!(
        "Backends: {} (detected: {})",
        backends
            .iter()
            .map(|b| b.name())
            .collect::<Vec<_>>()
            .join(", "),
        Backend::detect().name()
    );
    if cfg!(debug_assertions) {
        println!("Warning: a debug build. Use --release for figures that mean anything.");
    }
    for len in [512, 65_536] {
        println!("\n{len} elements, nanoseconds per element\n");
        header(&backends);
        let mut rows = Vec::new();
        float_rows::<f32>("f32", len, &backends, &mut rows);
        float_rows::<f64>("f64", len, &backends, &mut rows);
        conversion_rows(len, &backends, &mut rows);
        for row in &rows {
            row.print();
        }
    }
    println!("\nShort dot products, nanoseconds per element\n");
    header(&backends);
    let mut rows = Vec::new();
    short_dot_rows::<f32>("f32", &backends, &mut rows);
    short_dot_rows::<f64>("f64", &backends, &mut rows);
    for row in &rows {
        row.print();
    }
}
