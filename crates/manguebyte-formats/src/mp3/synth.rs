//! The polyphase synthesis filter bank that turns 32 subbands back into
//! samples (ISO/IEC 11172-3, 2.4.3.2.2 and Figure A.2).

use std::f64::consts::PI;
use std::sync::OnceLock;

use super::tables::SYNTHESIS_WINDOW;

/// The matrix of the filter bank, `cos((16 + i)(2k + 1) pi / 64)` for the 64
/// values `i` of a vector and the 32 subbands `k`, and the whole window.
struct Coefficients {
    matrix: [[f32; 32]; 64],
    window: [f32; 512],
}

fn coefficients() -> &'static Coefficients {
    static COEFFICIENTS: OnceLock<Box<Coefficients>> = OnceLock::new();
    COEFFICIENTS.get_or_init(|| {
        let mut coefficients = Box::new(Coefficients {
            matrix: [[0.0; 32]; 64],
            window: [0.0; 512],
        });
        for (i, row) in coefficients.matrix.iter_mut().enumerate() {
            for (k, value) in row.iter_mut().enumerate() {
                *value = ((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos() as f32;
            }
        }
        for (i, value) in coefficients.window.iter_mut().enumerate() {
            let units = if i <= 256 {
                SYNTHESIS_WINDOW[i]
            } else if i % 64 == 0 {
                SYNTHESIS_WINDOW[512 - i]
            } else {
                -SYNTHESIS_WINDOW[512 - i]
            };
            *value = units as f32 / 65_536.0;
        }
        coefficients
    })
}

/// The state of the filter bank for one channel: the last sixteen vectors
/// of 64 values, in a ring.
#[derive(Debug, Clone)]
pub(super) struct Synthesis {
    vectors: [f32; 1_024],
    /// Where the newest vector starts.
    offset: usize,
}

impl Synthesis {
    pub(super) fn new() -> Self {
        Self {
            vectors: [0.0; 1_024],
            offset: 0,
        }
    }

    /// Forgets everything, as at the start of a stream.
    pub(super) fn reset(&mut self) {
        self.vectors = [0.0; 1_024];
        self.offset = 0;
    }

    /// Turns one sample of each of the 32 subbands into 32 samples.
    pub(super) fn run(&mut self, subbands: &[f32; 32], out: &mut [f32]) {
        let coefficients = coefficients();
        self.offset = (self.offset + 1_024 - 64) & 1_023;
        // Half the values of a vector are worked out; the others follow, as
        // the rows of the matrix are the negatives or the copies of others:
        // cos(pi - x) = -cos(x) makes V[32 - i] = -V[i], and
        // cos(2 pi - x) = cos(x) makes V[96 - i] = V[i].
        for i in (0..=16).chain(33..=48) {
            let row = &coefficients.matrix[i];
            let value: f32 = row.iter().zip(subbands).map(|(&c, &s)| c * s).sum();
            self.vectors[(self.offset + i) & 1_023] = value;
            if i < 16 {
                self.vectors[(self.offset + 32 - i) & 1_023] = -value;
            } else if (33..48).contains(&i) {
                self.vectors[(self.offset + 96 - i) & 1_023] = value;
            }
        }
        // The vector U of the standard takes, from each of the last sixteen
        // vectors in turn, its first or its last 32 values; each of the 32
        // samples is a sum along U through the window.
        for (j, sample) in out.iter_mut().enumerate().take(32) {
            let mut sum = 0.0f32;
            for i in 0..8 {
                let first = self.vectors[(self.offset + i * 128 + j) & 1_023];
                let last = self.vectors[(self.offset + i * 128 + 96 + j) & 1_023];
                sum += first * coefficients.window[i * 64 + j];
                sum += last * coefficients.window[i * 64 + 32 + j];
            }
            *sample = sum;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_left_out_are_the_negatives_or_copies_of_others() {
        let matrix = &coefficients().matrix;
        let close = |a: &[f32; 32], b: &[f32; 32], sign: f32| {
            a.iter().zip(b).all(|(&x, &y)| (x - sign * y).abs() < 1e-6)
        };
        for i in 0..16 {
            assert!(close(&matrix[32 - i], &matrix[i], -1.0), "row {i}");
        }
        for i in 33..48 {
            assert!(close(&matrix[96 - i], &matrix[i], 1.0), "row {i}");
        }
        assert!(close(&matrix[16], &[0.0; 32], 1.0));
    }
}
