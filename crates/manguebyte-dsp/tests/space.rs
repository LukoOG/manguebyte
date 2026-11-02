//! The space around the pipes: panning that keeps the power, delays that
//! arrive when the distance says, the image sources of a known room, a
//! swell pedal that never zippers, and output that does not depend on how
//! the blocks are cut.

use std::f64::consts::{PI, TAU};

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::space::{
    EarlyReflections, Interpolation, Listener, Placement, Position, ReflectionSettings,
    Reflections, Room, Speakers, Surface, SwellBox, SwellLaw, Wall, Windchest, WindchestSettings,
};
use manguebyte_dsp::{ProcessSpec, Processor};

const RATE: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// A small, fixed random generator (xorshift64*), so failures repeat.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
}

fn sine(frequency: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| (TAU * frequency * n as f64 / RATE).sin() as f32)
        .collect()
}

/// The amplitude of the component at `frequency` in `samples`, which must
/// hold a whole number of its periods.
fn amplitude(samples: &[f32], frequency: f64) -> f64 {
    let (mut s, mut c) = (0.0, 0.0);
    for (n, &y) in samples.iter().enumerate() {
        let phase = TAU * frequency * n as f64 / RATE;
        s += f64::from(y) * phase.sin();
        c += f64::from(y) * phase.cos();
    }
    2.0 * s.hypot(c) / samples.len() as f64
}

fn largest_step(samples: &[f32]) -> f64 {
    samples
        .windows(2)
        .map(|pair| f64::from(pair[1] - pair[0]).abs())
        .fold(0.0, f64::max)
}

fn peak(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|s| f64::from(s.abs()))
        .fold(0.0, f64::max)
}

/// The place of the largest sample, refined by a parabola through it and
/// its neighbours.
fn interpolated_peak(samples: &[f32]) -> f64 {
    let m = (1..samples.len() - 1)
        .max_by(|&a, &b| samples[a].total_cmp(&samples[b]))
        .unwrap();
    let (a, b, c) = (
        f64::from(samples[m - 1]),
        f64::from(samples[m]),
        f64::from(samples[m + 1]),
    );
    m as f64 + 0.5 * (a - c) / (a - 2.0 * b + c)
}

/// Renders mono pipe signals through a windchest in blocks of `block`
/// frames: `pipes[p]` is pipe `p`'s signal and the frames it sounds in.
fn render_chest(
    chest: &mut Windchest,
    pipes: &[(Vec<f32>, std::ops::Range<usize>)],
    frames: usize,
    block: usize,
    mut before_block: impl FnMut(&mut Windchest, usize),
) -> Vec<Vec<f32>> {
    let channels = chest.channels();
    let mut out = vec![Vec::with_capacity(frames); channels];
    let mut buffer = AudioBuffer::<f32>::new(channels, MAX_BLOCK);
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        before_block(chest, start);
        for (p, (signal, sounding)) in pipes.iter().enumerate() {
            if sounding.contains(&start) {
                chest.add(p, &signal[start..end]);
            }
        }
        buffer.set_num_frames(end - start);
        chest.finish(&mut buffer);
        for (c, samples) in out.iter_mut().enumerate() {
            samples.extend_from_slice(buffer.channel(c));
        }
        start = end;
    }
    out
}

fn same_bits(a: &[Vec<f32>], b: &[Vec<f32>]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits())
        })
}

fn listener() -> Listener {
    Listener::new(Position::new(0.0, 0.0, 1.5), Speakers::stereo())
}

fn chest(pipes: usize, channels: usize, interpolation: Interpolation) -> Windchest {
    Windchest::new(&WindchestSettings {
        interpolation,
        ..WindchestSettings::new(pipes, channels, RATE, MAX_BLOCK)
    })
    .unwrap()
}

#[test]
fn panning_keeps_the_power_the_same_all_round() {
    let layouts = [
        Speakers::stereo(),
        Speakers::stereo_with_width(45.0).unwrap(),
        Speakers::new(&[-30.0, 30.0, 0.0, -110.0, 110.0]).unwrap(),
        Speakers::new(&[0.0, 45.0, 90.0, 135.0, 180.0, -135.0, -90.0, -45.0]).unwrap(),
    ];
    for speakers in layouts {
        let listener = Listener::new(Position::default(), speakers);
        let mut worst = 0.0f64;
        for k in 0..=1440 {
            let azimuth = (-180.0 + 0.25 * k as f64).to_radians();
            let source = Position::new(10.0 * azimuth.sin(), 10.0 * azimuth.cos(), 2.0);
            let placement = listener.place(source);
            let power: f64 = placement.pan.iter().map(|g| g * g).sum();
            worst = worst.max((power - 1.0).abs());
        }
        assert!(
            worst < 1e-14,
            "{} channels: power off by {worst}",
            speakers.channels()
        );
    }
}

#[test]
fn a_pipe_placed_left_is_louder_on_the_left_and_the_power_holds_across_the_arc() {
    // A 200 Hz tone from pipes on an arc 10 m round the listener: the sum
    // of the two channels' powers is the same wherever the pipe is.
    let listener = listener();
    let tone = sine(200.0, 9_600);
    let mut powers = Vec::new();
    for k in -6..=6 {
        let azimuth = (10.0 * k as f64).to_radians();
        let source = Position::new(10.0 * azimuth.sin(), 10.0 * azimuth.cos(), 1.5);
        let mut chest = chest(1, 2, Interpolation::Cubic);
        chest.set_placement(0, &listener.place(source));
        let out = render_chest(
            &mut chest,
            &[(tone.clone(), 0..9_600)],
            9_600,
            128,
            |_, _| {},
        );
        let left = amplitude(&out[0][4_800..], 200.0);
        let right = amplitude(&out[1][4_800..], 200.0);
        match k {
            k if k < 0 => assert!(left > right, "{k}: {left} {right}"),
            0 => assert!((left - right).abs() < 1e-6),
            _ => assert!(left < right, "{k}: {left} {right}"),
        }
        powers.push(left * left + right * right);
    }
    let (low, high) = powers
        .iter()
        .fold((f64::INFINITY, 0.0f64), |(l, h), &p| (l.min(p), h.max(p)));
    // 10 m: a distance gain of 0.1, so a power of 0.01.
    assert!((high - 0.01).abs() < 1e-5 && (high - low) / high < 1e-4);
}

#[test]
fn an_impulse_arrives_after_the_distance_over_the_speed_of_sound() {
    // A smooth pulse peaking at frame 64, from pipes straight ahead, with
    // no air absorption (which would add its own delay).
    let still = Listener {
        air_loss_db_per_metre: 0.0,
        ..listener()
    };
    let pulse: Vec<f32> = (0..2_048)
        .map(|n| {
            if n <= 128 {
                (PI * n as f64 / 128.0).sin().powi(2) as f32
            } else {
                0.0
            }
        })
        .collect();
    let mut worst = [0.0f64; 2];
    for (i, interpolation) in [Interpolation::Cubic, Interpolation::Linear]
        .into_iter()
        .enumerate()
    {
        for distance in [1.3, 3.7, 7.77, 10.0, 17.21, 26.0, 40.05] {
            let mut chest = chest(1, 2, interpolation);
            let placement = still.place(Position::new(0.0, distance, 1.5));
            chest.set_placement(0, &placement);
            let out = render_chest(
                &mut chest,
                &[(pulse.clone(), 0..2_048)],
                8_192,
                256,
                |_, _| {},
            );
            let arrived = interpolated_peak(&out[0]) - 64.0;
            let expected = distance / 343.0 * RATE;
            worst[i] = worst[i].max((arrived - expected).abs());
            assert!((placement.delay_seconds * 343.0 - distance).abs() < 1e-12);
        }
    }
    // Measured: 1.2e-4 of a sample (cubic) and 7.4e-5 (linear); most of
    // it is the parabola's error in finding the peak.
    assert!(worst[0] < 1e-3, "cubic: off by {} of a sample", worst[0]);
    assert!(worst[1] < 1e-3, "linear: off by {} of a sample", worst[1]);
}

#[test]
fn half_a_sample_of_delay_costs_what_the_documentation_says_at_high_frequencies() {
    // A pipe exactly 100.5 samples away, the worst fraction for both kinds
    // of interpolation, into one channel.
    let mut placement = Placement::centred(1);
    placement.delay_seconds = 100.5 / RATE;
    let expected = [
        (Interpolation::Linear, [-3.01, -6.02, -11.74]),
        (Interpolation::Cubic, [-1.07, -3.25, -8.42]),
    ];
    for (interpolation, losses) in expected {
        for (frequency, loss) in [12_000.0, 16_000.0, 20_000.0].into_iter().zip(losses) {
            let mut chest = chest(1, 1, interpolation);
            chest.set_placement(0, &placement);
            let tone = sine(frequency, 9_600);
            let out = render_chest(&mut chest, &[(tone, 0..9_600)], 9_600, 128, |_, _| {});
            let db = 20.0 * amplitude(&out[0][4_800..], frequency).log10();
            assert!(
                (db - loss).abs() < 0.01,
                "{interpolation:?} {frequency}: {db}"
            );
        }
    }
}

#[test]
fn farther_pipes_lose_more_high_frequencies_as_the_air_law_says() {
    let listener = Listener::new(Position::default(), Speakers::mono());
    let mut last = f64::INFINITY;
    for distance in [2.0, 5.0, 10.0, 20.0, 40.0, 80.0] {
        let placement = listener.place(Position::new(0.0, distance, 0.0));
        let mut gains = [0.0; 2];
        for (gain, frequency) in gains.iter_mut().zip([500.0, 10_000.0]) {
            // No interpolation, so that only the air shapes the response.
            let mut chest = chest(1, 1, Interpolation::None);
            chest.set_placement(0, &placement);
            let tone = sine(frequency, 24_000);
            let out = render_chest(&mut chest, &[(tone, 0..24_000)], 24_000, 128, |_, _| {});
            *gain = amplitude(&out[0][19_200..], frequency) / placement.distance_gain;
        }
        let ratio = gains[1] / gains[0];
        assert!(ratio <= last + 1e-6, "{distance} m");
        last = ratio;
        // What a one-pole at the law's cutoff takes at 10 kHz.
        let fc = placement.air_cutoff_hz;
        let expected = if fc < 0.49 * RATE {
            let x = (PI * 10_000.0 / RATE).tan() / (PI * fc / RATE).tan();
            1.0 / (1.0 + x * x).sqrt()
        } else {
            1.0
        };
        assert!(
            (gains[1] - expected).abs() < 1e-4,
            "{distance} m: {} against {expected}",
            gains[1]
        );
    }
    // 80 m of air take a few decibels at 10 kHz.
    assert!(20.0 * last.log10() < -2.5);
}

/// The scene the windchest's block tests render: forty pipes of noise,
/// half of them starting and stopping, ten of them moving.
fn scene(block: usize, events: bool, reverse: bool) -> Vec<Vec<f32>> {
    let listener = listener();
    let mut rng = Rng::new(7);
    let mut place = || {
        listener.place(Position::new(
            rng.between(-6.0, 6.0),
            rng.between(4.0, 30.0),
            rng.between(1.0, 9.0),
        ))
    };
    let frames = 6_144;
    let mut noise = Rng::new(11);
    // Each pipe: where it starts, where it moves to, its signal, and when
    // it sounds.
    let pipes: Vec<_> = (0..40)
        .map(|p| {
            let (from, to) = (place(), place());
            let signal: Vec<f32> = (0..frames)
                .map(|_| noise.between(-0.5, 0.5) as f32)
                .collect();
            let sounding = if events && p >= 20 {
                1_536..4_096
            } else {
                0..frames
            };
            (from, to, signal, sounding)
        })
        .collect();
    // Slot `i` of the windchest holds pipe `order[i]`, so reversing the
    // order adds the same pipes the other way round.
    let order: Vec<usize> = if reverse {
        (0..40).rev().collect()
    } else {
        (0..40).collect()
    };
    let mut chest = chest(40, 2, Interpolation::Cubic);
    for (slot, &p) in order.iter().enumerate() {
        chest.set_placement(slot, &pipes[p].0);
    }
    let signals: Vec<_> = order
        .iter()
        .map(|&p| (pipes[p].2.clone(), pipes[p].3.clone()))
        .collect();
    render_chest(&mut chest, &signals, frames, block, |chest, start| {
        if events && start == 2_048 {
            for (slot, &p) in order.iter().enumerate() {
                if p < 10 {
                    chest.set_placement(slot, &pipes[p].1);
                }
            }
        }
    })
}

#[test]
fn the_windchest_gives_the_same_bits_whatever_the_block_size() {
    let reference = scene(512, true, false);
    for block in [1, 2, 16, 64, 128, 256] {
        assert!(same_bits(&scene(block, true, false), &reference), "{block}");
    }
    let reference = scene(512, false, false);
    for block in [1, 7, 100, 333, 511] {
        assert!(
            same_bits(&scene(block, false, false), &reference),
            "{block}"
        );
    }
    assert!(peak(&reference[0]) > 0.05);
}

#[test]
fn the_order_the_pipes_are_added_in_changes_nothing() {
    assert!(same_bits(&scene(128, true, true), &scene(128, true, false)));
}

#[test]
fn moving_a_sounding_pipe_does_not_click() {
    let listener = listener();
    let near_left = listener.place(Position::new(-3.0, 4.0, 1.5));
    let far_right = listener.place(Position::new(8.0, 14.0, 6.0));
    let frequency = 300.0;
    let tone = sine(frequency, 14_400);
    let step = TAU * frequency / RATE;
    let pipe = [(tone.clone(), 0..14_400)];
    for interpolation in [
        Interpolation::Cubic,
        Interpolation::Linear,
        Interpolation::None,
    ] {
        let still = |placement: &Placement| {
            let mut chest = chest(1, 2, interpolation);
            chest.set_placement(0, placement);
            render_chest(&mut chest, &pipe, 14_400, 128, |_, _| {})
        };
        let (from, to) = (still(&near_left), still(&far_right));
        let mut chest = chest(1, 2, interpolation);
        chest.set_placement(0, &near_left);
        let out = render_chest(&mut chest, &pipe, 14_400, 128, |chest, start| {
            if start == 4_864 {
                chest.set_placement(0, &far_right);
            }
        });
        for (c, samples) in out.iter().enumerate() {
            // The steepest a steady tone at either place gets, and what a
            // 20 ms crossfade between the two can add.
            let (a, b) = (near_left.channel_gain(c), far_right.channel_gain(c));
            let bound = step * a.max(b) + (a + b) / 960.0;
            let worst = largest_step(&samples[1_000..]);
            assert!(
                worst <= 1.01 * bound,
                "{interpolation:?} channel {c}: {worst} > {bound}"
            );
            // Jumping from one placement to the other would have stepped
            // by many times as much.
            let jump = (4_864..5_824)
                .map(|t| f64::from(from[c][t] - to[c][t]).abs())
                .fold(0.0, f64::max);
            assert!(jump > 10.0 * worst, "{interpolation:?} channel {c}");
        }
        // It got there: the right channel holds the far placement's level.
        let right = amplitude(&out[1][9_600..14_400], frequency);
        assert!((right - far_right.channel_gain(1)).abs() < 0.01 * far_right.channel_gain(1));
    }
}

#[test]
fn first_order_reflections_are_the_image_sources() {
    let room = Room::new(10.0, 20.0, 8.0, Wall::new(0.1))
        .with_wall(Surface::Left, Wall::new(0.3))
        .with_wall(Surface::Right, Wall::new(0.05))
        .with_wall(Surface::Back, Wall::new(0.5))
        .with_wall(Surface::Front, Wall::new(0.2))
        .with_wall(Surface::Floor, Wall::new(0.6))
        .with_wall(Surface::Ceiling, Wall::new(0.15));
    let listener = Listener::new(Position::new(3.0, 4.0, 1.5), Speakers::stereo());
    let source = Position::new(6.0, 15.0, 3.0);
    let settings = ReflectionSettings {
        max_order: 1,
        ..ReflectionSettings::default()
    };
    let reflections = Reflections::compute(&room, source, &listener, &settings).unwrap();
    assert_eq!(reflections.taps().len(), 6);
    // Each wall mirrors the source once.
    let mirrors = [
        (Surface::Left, [-6.0, 15.0, 3.0], 0.3f64),
        (Surface::Right, [14.0, 15.0, 3.0], 0.05),
        (Surface::Back, [6.0, -15.0, 3.0], 0.5),
        (Surface::Front, [6.0, 25.0, 3.0], 0.2),
        (Surface::Floor, [6.0, 15.0, -3.0], 0.6),
        (Surface::Ceiling, [6.0, 15.0, 13.0], 0.15),
    ];
    for (surface, [x, y, z], absorption) in mirrors {
        let tap = reflections
            .taps()
            .iter()
            .find(|tap| tap.hits[surface.index()] == 1)
            .unwrap();
        assert_eq!(tap.order, 1);
        assert_eq!(tap.image, Position::new(x, y, z), "{surface:?}");
        let (dx, dy, dz) = (x - 3.0, y - 4.0, z - 1.5);
        let distance = (dx * dx + dy * dy + dz * dz).sqrt();
        let gain = (1.0 - absorption).sqrt() / distance;
        let azimuth = dx.atan2(dy).to_degrees();
        let p = (azimuth.clamp(-30.0, 30.0) + 30.0) / 60.0;
        let (left, right) = ((p * PI / 2.0).cos(), (p * PI / 2.0).sin());
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs().max(1e-300);
        assert!(close(tap.distance, distance), "{surface:?}");
        assert!(close(tap.delay_seconds, distance / 343.0), "{surface:?}");
        assert!(close(tap.gain, gain), "{surface:?}");
        assert!((tap.pan[0] - left).abs() < 1e-12 && (tap.pan[1] - right).abs() < 1e-12);
    }
    // Earliest first.
    assert!(
        reflections
            .taps()
            .windows(2)
            .all(|pair| pair[0].delay_seconds <= pair[1].delay_seconds)
    );
}

#[test]
fn the_reflections_play_their_taps() {
    let room = Room::new(9.0, 17.0, 7.0, Wall::new(0.25));
    let still = Listener {
        air_loss_db_per_metre: 0.0,
        ..Listener::new(Position::new(4.0, 3.0, 1.5), Speakers::stereo())
    };
    let settings = ReflectionSettings {
        max_order: 2,
        ..ReflectionSettings::default()
    };
    let reflections =
        Reflections::compute(&room, Position::new(5.0, 13.0, 4.0), &still, &settings).unwrap();
    let mut early = EarlyReflections::new(&reflections, RATE, 256).unwrap();
    let frames = 8_192;
    let mut impulse = vec![0.0f32; frames];
    impulse[0] = 1.0;
    let mut out = AudioBuffer::<f32>::new(2, frames);
    early.process(&impulse, &mut out);
    let first = reflections.taps()[0].delay_seconds * RATE;
    for channel in 0..2 {
        let samples = out.channel(channel);
        // Nothing before the first path (Lagrange reads up to a sample
        // early), and each channel's sum is the sum of its gains: the
        // interpolation passes a steady signal unchanged.
        assert!(samples[..first as usize - 1].iter().all(|&s| s == 0.0));
        let heard: f64 = samples.iter().map(|&s| f64::from(s)).sum();
        let expected: f64 = reflections
            .taps()
            .iter()
            .map(|tap| tap.channel_gain(channel))
            .sum();
        assert!(
            (heard - expected).abs() < 1e-5 * expected,
            "{heard} {expected}"
        );
    }
}

/// The reflections the block tests play, for a listener at `x`: a nave
/// with soft walls (`filtered`) or walls that take the same at every
/// frequency, with no air absorption.
fn nave(x: f64, filtered: bool) -> Reflections {
    let (wall, floor, air) = if filtered {
        (
            Wall::new(0.1).with_high_cut(5_000.0),
            Wall::new(0.3).with_high_cut(2_000.0),
            Listener::DEFAULT_AIR_LOSS,
        )
    } else {
        (Wall::new(0.1), Wall::new(0.3), 0.0)
    };
    let room = Room::new(12.0, 30.0, 14.0, wall).with_wall(Surface::Floor, floor);
    let listener = Listener {
        air_loss_db_per_metre: air,
        ..Listener::new(Position::new(x, 6.0, 1.6), Speakers::stereo())
    };
    Reflections::compute(
        &room,
        Position::new(6.0, 24.0, 6.0),
        &listener,
        &ReflectionSettings::default(),
    )
    .unwrap()
}

/// The frame at which [`render_early`] hands over a new set: after the
/// first one has been heard (its first path arrives after 2 600 frames),
/// and a multiple of every block size the tests try but the odd ones.
const CHANGE_AT: usize = 6_144;

/// Plays `input` through the reflections of [`nave`] at 4 m in blocks of
/// `block` frames, changing to those at 9 m at [`CHANGE_AT`] if `change`.
fn render_early(block: usize, change: bool, filtered: bool, input: &[f32]) -> Vec<Vec<f32>> {
    let first = nave(4.0, filtered);
    let later = nave(9.0, filtered);
    let mut early =
        EarlyReflections::with_capacity(&first, RATE, MAX_BLOCK, first.taps().len(), 0.5).unwrap();
    let mut out = vec![Vec::new(); 2];
    let mut buffer = AudioBuffer::<f32>::new(2, MAX_BLOCK);
    let mut start = 0;
    while start < input.len() {
        let end = (start + block).min(input.len());
        if change && start == CHANGE_AT {
            early.set_reflections(&later).unwrap();
        }
        buffer.set_num_frames(end - start);
        early.process(&input[start..end], &mut buffer);
        for (c, samples) in out.iter_mut().enumerate() {
            samples.extend_from_slice(buffer.channel(c));
        }
        start = end;
    }
    out
}

#[test]
fn the_reflections_give_the_same_bits_whatever_the_block_size() {
    let mut rng = Rng::new(3);
    let input: Vec<f32> = (0..12_288).map(|_| rng.between(-1.0, 1.0) as f32).collect();
    let reference = render_early(512, true, true, &input);
    for block in [1, 2, 16, 64, 128, 256] {
        let other = render_early(block, true, true, &input);
        assert!(same_bits(&other, &reference), "{block}");
    }
    let reference = render_early(512, false, true, &input);
    for block in [1, 7, 100, 333] {
        let other = render_early(block, false, true, &input);
        assert!(same_bits(&other, &reference), "{block}");
    }
    assert!(peak(&reference[0][CHANGE_AT..]) > 0.01);
}

#[test]
fn a_new_set_of_reflections_crossfades_in() {
    // With no filters, a change of set is exactly a linear crossfade from
    // the old set's output to the new one's, over 20 ms.
    let frequency = 250.0;
    let tone = sine(frequency, 16_384);
    let alone = |x: f64| {
        let reflections = nave(x, false);
        let mut early = EarlyReflections::new(&reflections, RATE, MAX_BLOCK).unwrap();
        let mut out = AudioBuffer::<f32>::new(2, tone.len());
        early.process(&tone, &mut out);
        out
    };
    let (from, to) = (alone(4.0), alone(9.0));
    let changed = render_early(128, true, false, &tone);
    for (c, y) in changed.iter().enumerate() {
        let (a, b) = (from.channel(c), to.channel(c));
        let mut worst_error = 0.0f64;
        for t in 0..tone.len() {
            let share = ((t + 1).saturating_sub(CHANGE_AT) as f64 / 960.0).min(1.0);
            let expected = (1.0 - share) * f64::from(a[t]) + share * f64::from(b[t]);
            worst_error = worst_error.max((f64::from(y[t]) - expected).abs());
        }
        assert!(worst_error < 1e-6, "channel {c}: {worst_error}");
        // So the largest step is no more than the steeper of the two sets
        // and what the fade adds; a jump from one to the other would have
        // been many times larger.
        let around = CHANGE_AT - 1_000..CHANGE_AT + 2_000;
        let worst = largest_step(&y[around.clone()]);
        let steepest = largest_step(&a[around.clone()]).max(largest_step(&b[around.clone()]));
        let apart = around
            .clone()
            .map(|t| f64::from(a[t] - b[t]).abs())
            .fold(0.0, f64::max);
        assert!(worst <= steepest + apart / 960.0 + 1e-6, "channel {c}");
        assert!(apart > 10.0 * worst, "channel {c}");
    }
}

/// The steady gain of a swell box at `opening` for a tone at `frequency`.
fn swell_gain(opening: f64, frequency: f64) -> f64 {
    let mut swell = SwellBox::<f32>::default();
    swell.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: 9_600,
        num_channels: 1,
    });
    swell.set_opening(opening);
    swell.reset();
    let tone = sine(frequency, 9_600);
    let mut block = AudioBuffer::<f32>::new(1, 9_600);
    block.channel_mut(0).copy_from_slice(&tone);
    swell.process(&mut block);
    amplitude(&block.channel(0)[4_800..], frequency)
}

#[test]
fn the_swell_box_gets_louder_and_brighter_as_it_opens() {
    let law = SwellLaw::default();
    let (mut low, mut high, mut tilt) = (0.0, 0.0, 0.0);
    for k in 0..=10 {
        let opening = k as f64 / 10.0;
        let (l, h) = (swell_gain(opening, 200.0), swell_gain(opening, 8_000.0));
        assert!(l > low && h > high && h / l > tilt, "{opening}");
        (low, high, tilt) = (l, h, h / l);
        // At 200 Hz the level is the law's, whatever the cutoff.
        let db = 20.0 * l.log10();
        assert!((db - law.level_db(opening)).abs() < 0.25, "{opening}: {db}");
    }
    // Closed takes 30 dB, and the highs more: 18 dB more at 8 kHz.
    assert!(20.0 * (swell_gain(0.0, 8_000.0) / swell_gain(0.0, 200.0)).log10() < -17.0);
    // Open, the box is all but transparent.
    assert!(swell_gain(1.0, 8_000.0) > 0.98);
}

/// A steady signal through a swell box whose pedal moves on block
/// boundaries.
fn swept(ramp: Option<f64>, block: usize) -> Vec<f32> {
    let mut swell = match ramp {
        Some(seconds) => SwellBox::<f32>::default().with_ramp(seconds),
        None => SwellBox::<f32>::default(),
    };
    swell.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: MAX_BLOCK,
        num_channels: 2,
    });
    let frames = 48_000;
    let mut out = Vec::with_capacity(frames);
    let mut buffer = AudioBuffer::<f32>::new(2, MAX_BLOCK);
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        // A slam shut, a slam open, then the pedal pushed along by MIDI
        // steps every 1344 frames, a multiple of every block size tried.
        if start % 1_344 == 0 {
            let opening = match start / 1_344 {
                0 => 1.0,
                1 => 0.0,
                2 => 1.0,
                n => (n * 7 % 128) as f64 / 127.0,
            };
            swell.set_opening(opening);
        }
        buffer.set_num_frames(end - start);
        for channel in buffer.iter_channels_mut() {
            channel.fill(1.0);
        }
        swell.process(&mut buffer);
        assert_eq!(buffer.channel(0), buffer.channel(1));
        out.extend_from_slice(buffer.channel(0));
        start = end;
    }
    out
}

#[test]
fn sweeping_the_swell_pedal_does_not_zipper() {
    // The gain moves 30 dB over the 50 ms glide at most: a step of at most
    // 1 · ln 10 / 20 · 30 / 2400 a sample.
    let smooth = swept(None, 64);
    let bound = 10f64.ln() / 20.0 * 30.0 / 2_400.0;
    assert!(largest_step(&smooth[100..]) <= 1.01 * bound);
    // Without the glide the same moves are jumps.
    let rough = swept(Some(0.0), 64);
    assert!(largest_step(&rough[100..]) > 0.9);
}

#[test]
fn the_swell_box_gives_the_same_bits_whatever_the_block_size() {
    let reference = swept(None, 64);
    for block in [1, 2, 3, 7, 448] {
        let other = swept(None, block);
        assert!(
            reference
                .iter()
                .zip(&other)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{block}"
        );
    }
}
