use core::f64::consts::TAU;

use manguebyte_core::{AudioBuffer, Sample};

use super::*;
use crate::testing::{noise, run_in_blocks, sine};

const RATE: f64 = 48_000.0;

fn spec(channels: usize) -> ProcessSpec {
    ProcessSpec {
        sample_rate: RATE,
        max_block_size: 512,
        num_channels: channels,
    }
}

type Identity<S> = Stft<SpectralFn<for<'a> fn(SpectralFrame<'a, S>)>, S>;

/// Changes nothing.
fn identity<S: Sample>(settings: StftSettings) -> Identity<S> {
    fn keep<S: Sample>(_: SpectralFrame<'_, S>) {}
    Stft::new(settings, SpectralFn(keep::<S> as fn(SpectralFrame<'_, S>))).unwrap()
}

fn root_hann() -> StftWindow {
    StftWindow::SquareRoot(Window::Hann)
}

/// Transform size, frames per transform (`fft_size / hop`), and windows:
/// every pair the module documentation lists, at several sizes.
fn combinations() -> Vec<(usize, usize, StftWindow, StftWindow)> {
    let small = cfg!(miri);
    let pick = |normal: usize, miri: usize| if small { miri } else { normal };
    let hann = StftWindow::Shape(Window::Hann);
    let rect = StftWindow::Shape(Window::Rectangular);
    let mut list = vec![
        // The classic pairs: Hann at 50 % with plain overlap-add, and Hann
        // on both sides at 75 %.
        (pick(256, 32), 2, hann, rect),
        (pick(256, 32), 4, hann, hann),
        (pick(256, 32), 2, root_hann(), root_hann()),
        (pick(256, 32), 4, root_hann(), root_hann()),
        (pick(256, 32), 4, hann, rect),
        (pick(240, 48), 3, hann, hann),
        (pick(240, 48), 3, Window::Blackman.into(), rect),
        (pick(256, 32), 2, Window::Hamming.into(), rect),
        (pick(256, 32), 2, Window::Triangular.into(), rect),
        (pick(256, 32), 1, rect, rect),
        (pick(256, 32), 8, rect, rect),
        // Sizes that are not powers of two, one of them odd.
        (pick(300, 60), 4, hann, hann),
        (pick(255, 45), 3, hann, hann),
        // Tiny ones.
        (4, 4, hann, hann),
        (2, 2, hann, rect),
        (1, 1, rect, rect),
    ];
    if !small {
        list.push((2_048, 4, hann, hann));
        list.push((1_024, 2, hann, rect));
    }
    list
}

fn settings(fft_size: usize, overlap: usize, a: StftWindow, s: StftWindow) -> StftSettings {
    StftSettings::new(fft_size)
        .with_hop(fft_size / overlap)
        .with_windows(a, s)
}

fn assert_reconstructs<S: Sample>(tolerance: f64) {
    for (fft_size, overlap, analysis, synthesis) in combinations() {
        let what = format!("{fft_size} / {overlap}, {analysis:?} and {synthesis:?}");
        let mut stft = identity::<S>(settings(fft_size, overlap, analysis, synthesis));
        stft.prepare(&spec(2));
        let latency = stft.latency();
        let frames = latency + 3 * fft_size + 97;
        let input = noise::<S>(2, frames, 11);
        let mut output = input.clone();
        run_in_blocks(&mut stft, &mut output, 100);
        for channel in 0..2 {
            let (x, y) = (input.channel(channel), output.channel(channel));
            // Before the latency there is only the rounding of transforms
            // of silence.
            assert!(
                y[..latency].iter().all(|&s| s.to_f64().abs() <= tolerance),
                "{what}: output before the latency"
            );
            for (frame, (&got, &want)) in y[latency..].iter().zip(x).enumerate() {
                let error = (got.to_f64() - want.to_f64()).abs();
                assert!(
                    error <= tolerance,
                    "{what}: frame {frame} of channel {channel} is off by {error:e}"
                );
            }
        }
    }
}

#[test]
fn an_unchanged_spectrum_gives_back_the_input_after_the_latency() {
    assert_reconstructs::<f64>(1e-13);
}

#[test]
fn an_unchanged_spectrum_gives_back_the_input_in_single_precision() {
    assert_reconstructs::<f32>(5e-6);
}

#[test]
fn an_impulse_comes_out_exactly_the_reported_latency_later() {
    let rect = StftWindow::Shape(Window::Rectangular);
    let hann = StftWindow::Shape(Window::Hann);
    for (settings, expected) in [
        (settings(64, 2, hann, rect), 63),
        (settings(64, 4, hann, hann), 62),
        (settings(64, 2, root_hann(), root_hann()), 62),
        (settings(60, 3, Window::Blackman.into(), rect), 59),
        (settings(1, 1, rect, rect), 0),
    ] {
        let mut stft = identity::<f64>(settings);
        assert_eq!(stft.latency(), expected, "{settings:?}");
        stft.prepare(&spec(1));
        assert_eq!(stft.latency_samples(), expected);
        // Impulses at several positions within a hop.
        for at in [0, 1, 5, 17] {
            stft.reset();
            let mut buffer = AudioBuffer::<f64>::new(1, 400);
            buffer.channel_mut(0)[at] = 1.0;
            run_in_blocks(&mut stft, &mut buffer, 13);
            let out = buffer.channel(0);
            let peak = (0..out.len())
                .max_by(|&a, &b| out[a].abs().total_cmp(&out[b].abs()))
                .unwrap();
            assert_eq!(peak, at + expected, "{settings:?}, impulse at {at}");
            assert!((out[peak] - 1.0).abs() < 1e-12);
        }
    }
}

/// Turns every bin down by its own amount, and gates.
fn something_audible(stft_settings: StftSettings) -> Stft<impl SpectralProcessor<f32>, f32> {
    struct Tilt(SpectralGate);
    impl SpectralProcessor<f32> for Tilt {
        fn prepare(&mut self, layout: &StftLayout) {
            SpectralProcessor::<f32>::prepare(&mut self.0, layout);
        }
        fn process_frame(&mut self, frame: SpectralFrame<'_, f32>) {
            let len = frame.bins.len() as f32;
            for (index, bin) in frame.bins.iter_mut().enumerate() {
                *bin = bin.scale(1.0 - index as f32 / len) * Complex::new(0.6, 0.8);
            }
            self.0.process_frame(frame);
        }
    }
    Stft::new(stft_settings, Tilt(SpectralGate::new(-12.0))).unwrap()
}

#[test]
fn the_output_is_the_same_bit_for_bit_however_the_blocks_are_cut() {
    let fft_size = if cfg!(miri) { 64 } else { 512 };
    let frames = if cfg!(miri) { 700 } else { 6_000 };
    let input = noise::<f32>(2, frames, 3);
    let settings = StftSettings::new(fft_size).with_hop(fft_size / 4);

    let mut whole = input.clone();
    let mut stft = something_audible(settings);
    stft.prepare(&ProcessSpec {
        max_block_size: frames,
        ..spec(2)
    });
    stft.process(&mut whole);

    let patterns: [&[usize]; 5] = [&[1], &[7], &[64], &[511], &[3, 100, 1, 511, 64, 7, 250, 0]];
    for pattern in patterns {
        let mut stft = something_audible(settings);
        stft.prepare(&spec(2));
        let mut pieces = input.clone();
        let mut start = 0;
        for &size in pattern.iter().cycle() {
            if start == frames {
                break;
            }
            let end = (start + size).min(frames);
            pieces.windowed(start..end, |piece| stft.process(piece));
            start = end;
        }
        for channel in 0..2 {
            let same = whole
                .channel(channel)
                .iter()
                .zip(pieces.channel(channel))
                .all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "blocks of {pattern:?}, channel {channel}");
        }
    }
    // And the change did change something.
    let delayed = &whole.channel(0)[stft.latency()..];
    assert!(
        delayed
            .iter()
            .zip(input.channel(0))
            .any(|(a, b)| (a - b).abs() > 0.1)
    );
}

#[test]
fn windows_and_hops_that_do_not_overlap_add_are_refused() {
    let hann = StftWindow::Shape(Window::Hann);
    let rect = StftWindow::Shape(Window::Rectangular);
    let not_overlap_add = |settings: StftSettings| match Stft::<_, f32>::from_fn(
        settings,
        |_: SpectralFrame<'_, f32>| {},
    ) {
        Err(error @ StftError::NotOverlapAdd { ripple, .. }) => {
            assert!(ripple > OVERLAP_ADD_TOLERANCE, "{settings:?}");
            assert!(error.to_string().contains("overlap-add"));
        }
        other => panic!("{settings:?} gave {other:?}"),
    };
    // Hann on both sides at 50 % sums to a raised cosine, not a constant.
    not_overlap_add(settings(256, 2, hann, hann));
    // Hann at a hop of three quarters leaves gaps.
    not_overlap_add(
        StftSettings::new(256)
            .with_hop(192)
            .with_windows(hann, rect),
    );
    // A hop that does not divide the frame.
    not_overlap_add(
        StftSettings::new(256)
            .with_hop(100)
            .with_windows(rect, rect),
    );
    not_overlap_add(
        StftSettings::new(255)
            .with_hop(128)
            .with_windows(hann, rect),
    );
    // Kaiser windows never sum to a constant, nor does Hamming at 50 % with
    // Hann after it.
    not_overlap_add(settings(256, 4, Window::Kaiser(8.0).into(), rect));
    not_overlap_add(settings(256, 2, Window::Hamming.into(), hann));
    // A hop of the whole frame with any window that fades.
    not_overlap_add(settings(256, 1, hann, rect));

    let build = |settings: StftSettings| {
        Stft::<_, f64>::from_fn(settings, |_: SpectralFrame<'_, f64>| {}).map(|_| ())
    };
    assert_eq!(
        build(StftSettings::new(256).with_hop(0)),
        Err(StftError::Hop {
            hop: 0,
            fft_size: 256
        })
    );
    assert_eq!(
        build(StftSettings::new(256).with_hop(257)),
        Err(StftError::Hop {
            hop: 257,
            fft_size: 256
        })
    );
    assert_eq!(build(StftSettings::new(0)), Err(StftError::FftSize(0)));
    assert_eq!(
        build(StftSettings::new(MAX_FFT_SIZE + 1)),
        Err(StftError::FftSize(MAX_FFT_SIZE + 1))
    );
    // The flat-top window dips below zero, so it has no square root.
    assert_eq!(
        build(settings(
            256,
            4,
            StftWindow::SquareRoot(Window::FlatTop),
            hann
        )),
        Err(StftError::InvalidWindow)
    );
    assert_eq!(
        build(settings(256, 4, Window::Kaiser(f64::NAN).into(), hann)),
        Err(StftError::InvalidWindow)
    );
    for error in [
        StftError::FftSize(0),
        StftError::Hop {
            hop: 0,
            fft_size: 8,
        },
        StftError::InvalidWindow,
    ] {
        assert!(!error.to_string().is_empty());
    }
    // The defaults are valid.
    assert!(build(StftSettings::default()).is_ok());
    assert!(build(StftSettings::new(1_000)).is_ok());
}

#[test]
fn channels_are_processed_on_their_own_and_extra_ones_are_left_alone() {
    let mut stft = identity::<f64>(settings(64, 4, root_hann(), root_hann()));
    stft.prepare(&spec(3));
    let latency = stft.latency();
    let frames = 600;

    // Four channels into a processor prepared for three.
    let input = noise::<f64>(4, frames, 5);
    let mut output = input.clone();
    run_in_blocks(&mut stft, &mut output, 37);
    for channel in 0..3 {
        for (got, want) in output.channel(channel)[latency..]
            .iter()
            .zip(input.channel(channel))
        {
            assert!((got - want).abs() < 1e-13, "channel {channel}");
        }
    }
    assert_eq!(output.channel(3), input.channel(3));

    // One channel: the others run on silence and stay in step.
    stft.reset();
    let mut mono = noise::<f64>(1, frames, 6);
    let original = mono.clone();
    run_in_blocks(&mut stft, &mut mono, 29);
    for (got, want) in mono.channel(0)[latency..].iter().zip(original.channel(0)) {
        assert!((got - want).abs() < 1e-13);
    }
}

#[test]
fn frames_come_channel_by_channel_with_a_shared_index() {
    struct Record(Vec<(usize, u64, usize)>, Option<StftLayout>);
    impl SpectralProcessor<f64> for Record {
        fn prepare(&mut self, layout: &StftLayout) {
            self.1 = Some(*layout);
        }
        fn process_frame(&mut self, frame: SpectralFrame<'_, f64>) {
            self.0.push((frame.channel, frame.index, frame.bins.len()));
        }
        fn reset(&mut self) {
            self.0.clear();
        }
    }
    let mut stft = Stft::new(StftSettings::new(32).with_hop(8), Record(Vec::new(), None)).unwrap();
    stft.prepare(&spec(2));
    let layout = stft.processor().1.unwrap();
    assert_eq!((layout.fft_size, layout.hop, layout.bins), (32, 8, 17));
    assert_eq!(layout.channels, 2);
    assert!((layout.bin_frequency(4) - 6_000.0).abs() < 1e-9);
    // Σ of a periodic Hann window of 32 samples is 16.
    assert!((layout.amplitude_scale - 2.0 / 16.0).abs() < 1e-12);

    let mut buffer = AudioBuffer::<f64>::new(2, 23);
    stft.process(&mut buffer);
    // 23 samples hold two complete hops of 8.
    assert_eq!(
        stft.processor().0,
        [(0, 0, 17), (1, 0, 17), (0, 1, 17), (1, 1, 17)]
    );
    stft.process(&mut buffer);
    assert_eq!(stft.processor().0.len(), 10);
    stft.reset();
    assert!(stft.processor().0.is_empty());
    assert_eq!(stft.into_processor().0.len(), 0);
}

#[test]
fn reset_starts_again_from_silence() {
    let mut stft = something_audible(StftSettings::new(64));
    stft.prepare(&spec(2));
    let input = noise::<f32>(2, 500, 9);
    let mut first = input.clone();
    run_in_blocks(&mut stft, &mut first, 50);
    stft.reset();
    let mut second = input.clone();
    run_in_blocks(&mut stft, &mut second, 77);
    assert!(first == second);
}

/// The amplitude of the sine at `frequency` in `signal`, from a correlation
/// over the whole slice under a Hann window.
fn amplitude_at(signal: &[f64], frequency: f64) -> f64 {
    let len = signal.len();
    let (mut re, mut im, mut weight) = (0.0, 0.0, 0.0);
    for (n, &sample) in signal.iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * n as f64 / len as f64).cos();
        let (sin, cos) = (TAU * frequency * n as f64 / RATE).sin_cos();
        re += w * sample * cos;
        im += w * sample * sin;
        weight += w;
    }
    2.0 * re.hypot(im) / weight
}

#[test]
fn the_gate_removes_a_tone_under_its_threshold_and_keeps_one_over_it() {
    let fft_size = if cfg!(miri) { 128 } else { 1_024 };
    let bin_width = RATE / fft_size as f64;
    let (loud, quiet) = (10.0 * bin_width, 40.0 * bin_width);
    let frames = if cfg!(miri) { 1_500 } else { 12_000 };
    let mut signal = sine::<f64>(1, frames, loud, RATE, 0.5);
    let soft = sine::<f64>(1, frames, quiet, RATE, 0.01);
    signal.mix_from(&soft, 1.0);

    // -6 dBFS against -40 dBFS, with the threshold at -20 dBFS.
    let mut stft = Stft::new(StftSettings::new(fft_size), SpectralGate::new(-20.0)).unwrap();
    stft.prepare(&spec(1));
    run_in_blocks(&mut stft, &mut signal, 256);
    let steady = &signal.channel(0)[2 * fft_size..];
    let kept = amplitude_at(steady, loud);
    let removed = amplitude_at(steady, quiet);
    assert!(
        (kept - 0.5).abs() < 1e-3,
        "the loud tone came out at {kept}"
    );
    assert!(removed < 1e-5, "the quiet tone came out at {removed}");

    // With the threshold under both, both pass.
    stft.processor_mut().set_threshold_db(-60.0);
    stft.reset();
    let mut both = sine::<f64>(1, frames, loud, RATE, 0.5);
    both.mix_from(&soft, 1.0);
    run_in_blocks(&mut stft, &mut both, 256);
    let steady = &both.channel(0)[2 * fft_size..];
    assert!((amplitude_at(steady, quiet) - 0.01).abs() < 1e-4);

    // A floor turns gated bins down instead of off.
    stft.processor_mut().set_threshold_db(-20.0);
    stft.processor_mut().set_floor_db(-20.0);
    stft.reset();
    let mut floored = sine::<f64>(1, frames, quiet, RATE, 0.01);
    run_in_blocks(&mut stft, &mut floored, 256);
    let steady = &floored.channel(0)[2 * fft_size..];
    assert!((amplitude_at(steady, quiet) - 0.001).abs() < 1e-5);

    // A threshold of its own lets one bin gate harder.
    stft.processor_mut().set_floor_db(f64::NEG_INFINITY);
    stft.processor_mut().set_threshold_db(-60.0);
    for bin in 9..=11 {
        stft.processor_mut().set_bin_threshold_db(bin, 0.0);
    }
    stft.reset();
    let mut own = sine::<f64>(1, frames, loud, RATE, 0.5);
    run_in_blocks(&mut stft, &mut own, 256);
    assert!(amplitude_at(&own.channel(0)[2 * fft_size..], loud) < 1e-6);
    stft.processor_mut().clear_profile();
    assert!(stft.processor().profile().iter().all(|&t| t == 0.0));
}

#[test]
fn the_gate_learns_a_noise_profile_and_takes_the_noise_away() {
    let fft_size = if cfg!(miri) { 128 } else { 1_024 };
    let frames = if cfg!(miri) { 2_000 } else { 48_000 };
    let tone_frequency = 20.0 * RATE / fft_size as f64;
    let mut noise_floor = noise::<f64>(2, frames, 21);
    noise_floor.apply_gain(0.01);

    let mut stft = Stft::new(StftSettings::new(fft_size), SpectralGate::new(-120.0)).unwrap();
    stft.prepare(&spec(2));
    stft.processor_mut().set_learning(true);
    assert!(stft.processor().is_learning());
    let mut learning = noise_floor.clone();
    run_in_blocks(&mut stft, &mut learning, 512);
    stft.processor_mut().set_learning(false);
    // While it learnt, everything went through.
    let latency = stft.latency();
    assert!((learning.channel(1)[latency + 100] - noise_floor.channel(1)[100]).abs() < 1e-12);
    assert!(stft.processor().profile().iter().all(|&t| t > 0.0));

    // Then the same kind of noise under a tone.
    stft.reset();
    let mut noisy = noise::<f64>(2, frames, 22);
    noisy.apply_gain(0.01);
    let tone = sine::<f64>(2, frames, tone_frequency, RATE, 0.3);
    noisy.mix_from(&tone, 1.0);
    let before = noisy.clone();
    run_in_blocks(&mut stft, &mut noisy, 300);

    let start = 2 * fft_size;
    for channel in 0..2 {
        let out = &noisy.channel(channel)[start + latency..];
        let clean = &tone.channel(channel)[start..start + out.len()];
        let dirty = &before.channel(channel)[start..start + out.len()];
        let power = |a: &[f64], b: &[f64]| -> f64 {
            a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>() / a.len() as f64
        };
        let reduction_db = 10.0 * (power(out, clean) / power(dirty, clean)).log10();
        assert!(
            reduction_db < -18.0,
            "channel {channel}: the noise only fell {reduction_db:.1} dB"
        );
        let kept = amplitude_at(out, tone_frequency);
        assert!((kept - 0.3).abs() < 5e-4, "the tone came out at {kept}");
    }
}

#[test]
fn the_freeze_holds_the_spectrum_whatever_comes_in() {
    let fft_size = if cfg!(miri) { 128 } else { 1_024 };
    let hop = fft_size / 4;
    let (held, later) = (1_000.0, 3_000.0);
    let switch = 8 * fft_size;
    let frames = if cfg!(miri) {
        switch + 6 * fft_size
    } else {
        switch + 48_000
    };

    // One tone until the freeze, another after it.
    let mut signal = sine::<f64>(1, frames, held, RATE, 0.5);
    let other = sine::<f64>(1, frames, later, RATE, 0.5);
    signal.channel_mut(0)[switch..].copy_from_slice(&other.channel(0)[switch..]);

    let mut stft = Stft::new(
        StftSettings::new(fft_size).with_hop(hop),
        SpectralFreeze::new(),
    )
    .unwrap();
    stft.prepare(&spec(1));
    let latency = stft.latency();
    // Frozen a hop before the switch, so that the frame held is the last
    // one of the first tone alone.
    let freeze = switch - hop;
    signal.windowed(0..freeze, |piece| run_in_blocks(&mut stft, piece, 100));
    stft.processor_mut().set_frozen(true);
    signal.windowed(freeze..frames, |piece| run_in_blocks(&mut stft, piece, 100));

    // Give the overlap-add a frame to fade to the held one, and the latency.
    let settled = switch + latency + 2 * fft_size;
    let out = &signal.channel(0)[settled..];
    let span = out.len() / 2;
    for part in [&out[..span], &out[span..]] {
        let level = amplitude_at(part, held);
        assert!((level - 0.5).abs() < 1e-4, "the held tone is at {level}");
        let leak = amplitude_at(part, later);
        assert!(leak < 1e-6, "the new tone leaks in at {leak}");
    }

    // Letting go brings the input back.
    stft.processor_mut().set_frozen(false);
    let mut after = sine::<f64>(1, 6 * fft_size, later, RATE, 0.5);
    run_in_blocks(&mut stft, &mut after, 100);
    let tail = &after.channel(0)[latency + 2 * fft_size..];
    assert!((amplitude_at(tail, later) - 0.5).abs() < 1e-3);
    assert!(amplitude_at(tail, held) < 1e-3);
}

#[test]
fn the_pitch_shifter_moves_a_tone_and_leaves_it_alone_at_unity() {
    let fft_size = if cfg!(miri) { 256 } else { 2_048 };
    let frames = if cfg!(miri) { 4_000 } else { 48_000 };
    let frequency = 1_000.0;
    let input = sine::<f64>(1, frames, frequency, RATE, 0.5);

    let mut stft = Stft::new(StftSettings::new(fft_size), PitchShift::new(1.5)).unwrap();
    stft.prepare(&spec(1));
    let latency = stft.latency();
    let mut shifted = input.clone();
    run_in_blocks(&mut stft, &mut shifted, 128);
    let steady = &shifted.channel(0)[latency + 2 * fft_size..];
    let moved = amplitude_at(steady, 1.5 * frequency);
    let left = amplitude_at(steady, frequency);
    // Within a decibel of its level, and nothing left where it was.
    let within_a_db = |level: f64| (20.0 * (level / 0.5).log10()).abs() < 1.0;
    assert!(within_a_db(moved), "the shifted tone is at {moved}");
    assert!(left < 1e-3, "the original tone is still at {left}");

    // A fifth up by semitones lands close to the same place.
    stft.processor_mut().set_semitones(7.0);
    assert!((stft.processor().ratio() - 1.498_307).abs() < 1e-6);
    stft.processor_mut().set_ratio(100.0);
    assert_eq!(stft.processor().ratio(), MAX_PITCH_RATIO);
    stft.processor_mut().set_ratio(f64::NAN);
    assert_eq!(stft.processor().ratio(), 1.0);

    // At a ratio of 1 it changes nothing.
    stft.reset();
    let mut same = input.clone();
    run_in_blocks(&mut stft, &mut same, 128);
    for (got, want) in same.channel(0)[latency..].iter().zip(input.channel(0)) {
        assert!((got - want).abs() < 1e-12);
    }

    // And an octave down halves the frequency.
    stft.processor_mut().set_ratio(0.5);
    stft.reset();
    let mut down = input.clone();
    run_in_blocks(&mut stft, &mut down, 128);
    let steady = &down.channel(0)[latency + 2 * fft_size..];
    assert!(within_a_db(amplitude_at(steady, 0.5 * frequency)));
    assert!(amplitude_at(steady, frequency) < 1e-3);
}

#[test]
fn processing_before_prepare_passes_the_audio_through() {
    let mut stft = identity::<f32>(StftSettings::new(64));
    let mut buffer = noise::<f32>(1, 100, 1);
    let original = buffer.clone();
    stft.process(&mut buffer);
    assert!(buffer == original);
    assert_eq!((stft.fft_size(), stft.hop()), (64, 16));
    assert!(format!("{stft:?}").contains("latency: 62"));
}
