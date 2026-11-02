//! Where a rank's pipes stand across a windchest.

use super::{Position, SPEED_OF_SOUND};

/// The order a rank's pipes stand in across a [`Chest`], from the lowest
/// (largest) pipe up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arrangement {
    /// Low to high, left to right.
    Chromatic,
    /// The C side and the C♯ side: the pipes of C, D, E, F♯, G♯, A♯ (every
    /// other semitone, counting the rank's first pipe as a C) on the left
    /// half of the chest, the others on the right half. Each half runs in
    /// whole tones.
    CAndCSharp {
        /// Whether the largest pipes stand at the outer ends of the chest
        /// (each half rising towards the middle) or in the middle (each
        /// half rising outwards).
        largest_outside: bool,
    },
    /// A mitre or pyramid: the largest pipe in the middle and the others
    /// alternating left and right of it, smaller and smaller outwards.
    Mitre,
}

/// How high a pipe's sound comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Height {
    /// Every pipe at the chest's height.
    Level,
    /// At the chest's height plus `fraction` of the pipe's speaking
    /// length, taken as that of an open pipe, half a wavelength: 2.6 m for
    /// a C of 65.4 Hz. A pipe speaks from its mouth, near its foot, and an
    /// open one from its top as well; a half puts the sound between the
    /// two. For a stopped rank, whose pipes are half as long, halve the
    /// fraction.
    FollowLength {
        /// Of the speaking length, from 0 (the foot) to 1 (the top).
        fraction: f64,
    },
}

/// A windchest that a rank stands on, to lay its pipes out by
/// [`positions`](Self::positions).
///
/// The pipes stand at even spacing in a row along `x`, centred on
/// [`centre`](Self::centre): with `n` pipes, slot `s` (0 the leftmost) is
/// at `centre.x - width/2 + (s + 1/2)·width/n`. Spacing does not follow the
/// pipes' sizes. A negative width mirrors the layout. Pipes at positions of
/// the caller's own choosing need no chest: give them to
/// [`Listener::place`](super::Listener::place) as they are.
///
/// ```
/// use manguebyte_dsp::space::{Arrangement, Chest, Height, Position};
///
/// let chest = Chest {
///     height: Height::FollowLength { fraction: 0.5 },
///     ..Chest::new(Position::new(0.0, 12.0, 4.0), 3.0, Arrangement::Mitre)
/// };
/// let frequencies: Vec<f64> = (0..5).map(|k| 65.4 * 2f64.powf(k as f64 / 12.0)).collect();
/// let pipes = chest.positions(&frequencies);
/// // The largest pipe stands in the middle and reaches highest.
/// assert_eq!(pipes[0].x, 0.0);
/// assert!(pipes[0].z > pipes[1].z);
/// assert!(pipes[1].x < 0.0 && pipes[2].x > 0.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chest {
    /// The middle of the row, at the height the pipes stand at.
    pub centre: Position,
    /// The length of the row along `x`, in metres.
    pub width: f64,
    /// The order of the pipes.
    pub arrangement: Arrangement,
    /// How high each pipe's sound comes from; [`Height::Level`] unless set.
    pub height: Height,
    /// For the pipes' lengths, in metres per second; [`SPEED_OF_SOUND`]
    /// unless set.
    pub speed_of_sound: f64,
}

impl Chest {
    /// A chest of `width` metres centred on `centre`.
    pub fn new(centre: Position, width: f64, arrangement: Arrangement) -> Self {
        Self {
            centre,
            width,
            arrangement,
            height: Height::Level,
            speed_of_sound: SPEED_OF_SOUND,
        }
    }

    /// The slot each of `pipes` pipes stands in, 0 the leftmost, pipe 0
    /// being the lowest.
    pub fn slots(&self, pipes: usize) -> Vec<usize> {
        let n = pipes;
        match self.arrangement {
            Arrangement::Chromatic => (0..n).collect(),
            Arrangement::CAndCSharp { largest_outside } => {
                // The C side has the first pipe, and one more if `n` is odd.
                let c_side = n.div_ceil(2);
                (0..n)
                    .map(|i| {
                        let j = i / 2;
                        match (i % 2 == 0, largest_outside) {
                            (true, true) => j,
                            (true, false) => c_side - 1 - j,
                            (false, true) => n - 1 - j,
                            (false, false) => c_side + j,
                        }
                    })
                    .collect()
            }
            Arrangement::Mitre => {
                // Slots by their distance from the middle, the left one of
                // each pair first.
                let mut slots: Vec<usize> = (0..n).collect();
                slots.sort_by_key(|&s| ((2 * s).abs_diff(n.saturating_sub(1)), s));
                slots
            }
        }
    }

    /// Where each pipe of a rank stands, given the pipes' frequencies in
    /// hertz from the lowest pipe up (the frequencies matter only for
    /// [`Height::FollowLength`]).
    pub fn positions(&self, frequencies: &[f64]) -> Vec<Position> {
        let n = frequencies.len();
        let spacing = self.width / n.max(1) as f64;
        let left = self.centre.x - self.width / 2.0;
        self.slots(n)
            .into_iter()
            .zip(frequencies)
            .map(|(slot, &frequency)| {
                let rise = match self.height {
                    Height::Level => 0.0,
                    Height::FollowLength { fraction } if frequency > 0.0 => {
                        fraction * self.speed_of_sound / (2.0 * frequency)
                    }
                    Height::FollowLength { .. } => 0.0,
                };
                Position {
                    x: left + (slot as f64 + 0.5) * spacing,
                    y: self.centre.y,
                    z: self.centre.z + rise,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(arrangement: Arrangement, n: usize) -> Vec<usize> {
        Chest::new(Position::default(), 1.0, arrangement).slots(n)
    }

    #[test]
    fn every_arrangement_fills_every_slot_once() {
        for n in [0, 1, 2, 5, 8, 61] {
            for arrangement in [
                Arrangement::Chromatic,
                Arrangement::CAndCSharp {
                    largest_outside: true,
                },
                Arrangement::CAndCSharp {
                    largest_outside: false,
                },
                Arrangement::Mitre,
            ] {
                let mut taken = slots(arrangement, n);
                taken.sort_unstable();
                assert_eq!(taken, (0..n).collect::<Vec<_>>(), "{arrangement:?}, {n}");
            }
        }
    }

    #[test]
    fn the_arrangements_put_the_pipes_in_their_order() {
        assert_eq!(slots(Arrangement::Chromatic, 4), [0, 1, 2, 3]);
        // C, C#, D, D#, E, F, F#: C D E F# on the left, C# D# F on the right.
        let outside = Arrangement::CAndCSharp {
            largest_outside: true,
        };
        assert_eq!(slots(outside, 7), [0, 6, 1, 5, 2, 4, 3]);
        let inside = Arrangement::CAndCSharp {
            largest_outside: false,
        };
        assert_eq!(slots(inside, 7), [3, 4, 2, 5, 1, 6, 0]);
        assert_eq!(slots(Arrangement::Mitre, 5), [2, 1, 3, 0, 4]);
        assert_eq!(slots(Arrangement::Mitre, 4), [1, 2, 0, 3]);
    }

    #[test]
    fn a_pipe_rises_with_its_length() {
        let chest = Chest {
            height: Height::FollowLength { fraction: 1.0 },
            ..Chest::new(Position::new(0.0, 0.0, 2.0), 2.0, Arrangement::Chromatic)
        };
        let pipes = chest.positions(&[65.4, 130.8]);
        assert!((pipes[0].z - (2.0 + 343.0 / 130.8)).abs() < 1e-12);
        assert!((pipes[1].z - (2.0 + 343.0 / 261.6)).abs() < 1e-12);
        assert_eq!((pipes[0].x, pipes[1].x), (-0.5, 0.5));
    }
}
