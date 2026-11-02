//! Measures what the space around the pipes costs.
//!
//! ```text
//! cargo run --release -p manguebyte-dsp --example space_speed
//! ```
//!
//! An organ of 2 000 placed pipes, 300 of them sounding, into stereo at
//! 48 kHz in blocks of 128 frames: the time a block takes in a
//! [`Windchest`] (every sounding pipe added, then the block read out), with
//! each kind of interpolation, and while every sounding pipe crossfades to
//! a new place. Then [`EarlyReflections`] of order 3 and 6 on one
//! division's bus, and a [`SwellBox`] on the stereo output. The pipes'
//! signals are made beforehand, so only the placing is timed. The cases
//! take turns, two seconds of audio each, ten rounds, and each figure is
//! its best round: on a busy machine the turns keep one case's bad moment
//! from landing on it alone.

use std::hint::black_box;
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::space::{
    Arrangement, Chest, EarlyReflections, Height, Interpolation, Listener, Placement, Position,
    ReflectionSettings, Reflections, Room, Speakers, SwellBox, Wall, Windchest, WindchestSettings,
};
use manguebyte_dsp::{ProcessSpec, Processor};

const RATE: f64 = 48_000.0;
const BLOCK: usize = 128;
const ROUND_SECONDS: f64 = 2.0;
const ROUNDS: usize = 10;
const PIPES: usize = 2_000;
const SOUNDING: usize = 300;

/// Where every pipe stands: ranks of 61 on chests at several depths and
/// heights, in three arrangements; and each one a metre to the right, for
/// moving them.
fn placements(listener: &Listener) -> (Vec<Placement>, Vec<Placement>) {
    let frequencies: Vec<f64> = (0..61).map(|k| 65.4 * 2f64.powf(k as f64 / 12.0)).collect();
    let arrangements = [
        Arrangement::Chromatic,
        Arrangement::CAndCSharp {
            largest_outside: true,
        },
        Arrangement::Mitre,
    ];
    let mut here = Vec::new();
    let mut there = Vec::new();
    for rank in 0..PIPES.div_ceil(61) {
        let centre = Position::new(6.0, 24.0 + (rank % 6) as f64, 4.0 + (rank % 3) as f64 * 3.0);
        let chest = Chest {
            height: Height::FollowLength { fraction: 0.5 },
            ..Chest::new(centre, 5.0, arrangements[rank % 3])
        };
        for position in chest.positions(&frequencies) {
            here.push(listener.place(position));
            there.push(listener.place(Position {
                x: position.x + 1.0,
                ..position
            }));
        }
    }
    here.truncate(PIPES);
    there.truncate(PIPES);
    (here, there)
}

type Case<'a> = (String, Box<dyn FnMut(usize) + 'a>);

fn main() {
    let listener = Listener::new(Position::new(6.0, 6.0, 1.6), Speakers::stereo());
    let (here, there) = placements(&listener);
    // The pipes' signals: 64 blocks of tones, every sixth pipe of the organ.
    let signals: Vec<Vec<f32>> = (0..SOUNDING)
        .map(|p| {
            let frequency = 65.4 * 2f64.powf((p % 61) as f64 / 12.0);
            (0..BLOCK * 64)
                .map(|n| {
                    let phase = std::f64::consts::TAU * frequency * n as f64 / RATE;
                    (0.01 * phase.sin()) as f32
                })
                .collect()
        })
        .collect();
    let bus: Vec<f32> = (0..BLOCK * 64)
        .map(|n| (0.1 * (n as f64 * 0.013).sin()) as f32)
        .collect();
    let (signals, bus) = (&signals, &bus);
    let (here, there) = (&here, &there);

    let mut cases: Vec<Case<'_>> = Vec::new();
    for interpolation in [
        Interpolation::Cubic,
        Interpolation::Linear,
        Interpolation::None,
    ] {
        let mut chest = Windchest::new(&WindchestSettings {
            interpolation,
            ..WindchestSettings::new(PIPES, 2, RATE, BLOCK)
        })
        .unwrap();
        for (p, placement) in here.iter().enumerate() {
            chest.set_placement(p, placement);
        }
        let mut out = AudioBuffer::<f32>::new(2, BLOCK);
        cases.push((
            format!("windchest, {interpolation:?}"),
            Box::new(move |n| {
                let at = (n % 64) * BLOCK;
                for (k, signal) in signals.iter().enumerate() {
                    chest.add(k * 6, &signal[at..at + BLOCK]);
                }
                chest.finish(black_box(&mut out));
            }),
        ));
    }
    {
        let mut chest = Windchest::new(&WindchestSettings::new(PIPES, 2, RATE, BLOCK)).unwrap();
        let mut out = AudioBuffer::<f32>::new(2, BLOCK);
        cases.push((
            "windchest, Cubic, every sounding pipe always crossfading".into(),
            Box::new(move |n| {
                let at = (n % 64) * BLOCK;
                // A move takes 20 ms, 7.5 blocks: a new one every 8.
                if n % 8 == 0 {
                    let to = if n % 16 == 0 { there } else { here };
                    for k in 0..SOUNDING {
                        chest.set_placement(k * 6, &to[k * 6]);
                    }
                }
                for (k, signal) in signals.iter().enumerate() {
                    chest.add(k * 6, &signal[at..at + BLOCK]);
                }
                chest.finish(black_box(&mut out));
            }),
        ));
    }

    let room = Room::new(12.0, 30.0, 14.0, Wall::new(0.1).with_high_cut(5_000.0));
    for order in [3, 6] {
        let settings = ReflectionSettings {
            max_order: order,
            ..ReflectionSettings::default()
        };
        let source = Position::new(6.0, 25.0, 6.0);
        let reflections = Reflections::compute(&room, source, &listener, &settings).unwrap();
        let mut early = EarlyReflections::new(&reflections, RATE, BLOCK).unwrap();
        let mut out = AudioBuffer::<f32>::new(2, BLOCK);
        cases.push((
            format!(
                "early reflections, order {order} ({} paths), mono in",
                reflections.taps().len()
            ),
            Box::new(move |n| {
                let at = (n % 64) * BLOCK;
                early.process(&bus[at..at + BLOCK], black_box(&mut out));
            }),
        ));
    }

    for moving in [false, true] {
        let mut swell = SwellBox::<f32>::default();
        swell.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: BLOCK,
            num_channels: 2,
        });
        let mut out = AudioBuffer::<f32>::new(2, BLOCK);
        for channel in out.iter_channels_mut() {
            channel.fill(0.1);
        }
        let name = if moving {
            "swell box, stereo, the pedal always moving"
        } else {
            "swell box, stereo, still"
        };
        cases.push((
            name.into(),
            Box::new(move |n| {
                if moving && n % 4 == 0 {
                    swell.set_opening((n / 4 % 128) as f64 / 127.0);
                }
                swell.process(black_box(&mut out));
            }),
        ));
    }

    let blocks = (ROUND_SECONDS * RATE) as usize / BLOCK;
    let mut best = vec![f64::INFINITY; cases.len()];
    for _ in 0..ROUNDS {
        for (case, best) in cases.iter_mut().zip(&mut best) {
            let start = Instant::now();
            for n in 0..blocks {
                (case.1)(n);
            }
            *best = best.min(start.elapsed().as_secs_f64() / blocks as f64);
        }
    }

    let deadline = BLOCK as f64 / RATE;
    println!(
        "| {PIPES} pipes placed, {SOUNDING} sounding, {BLOCK} frames at 48 kHz | per block | of the block's time |"
    );
    println!("|---|---:|---:|");
    for ((name, _), seconds) in cases.iter().zip(best) {
        println!(
            "| {name} | {:.1} µs | {:.1} % |",
            seconds * 1e6,
            100.0 * seconds / deadline
        );
    }
}
