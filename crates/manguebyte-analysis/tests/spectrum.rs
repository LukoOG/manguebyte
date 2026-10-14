//! The spectrum analyser measured: the level of a sine in its bin for
//! every window, and near either end of the spectrum where its mirror image
//! adds to it; the scalloping loss between bins; the level of a sine in its
//! band; the bands of white and pink noise against the power that the
//! noise was built with; and how fast a reading falls when its sound stops.
//!
//! Run with `--nocapture` to see the measured figures.

mod common;

use core::f64::consts::{PI, TAU};

use common::Noise;
use manguebyte_analysis::{
    OctaveFraction, SPECTRUM_FLOOR_DB, SpectrumAnalyzer, SpectrumReader, SpectrumSettings,
    Weighting,
};
use manguebyte_dsp::ProcessSpec;
use manguebyte_dsp::fft::{Complex, RealFft};
use manguebyte_dsp::window::{Symmetry, Window};

const RATE: f64 = 48_000.0;

const WINDOWS: [Window; 9] = [
    Window::Rectangular,
    Window::Triangular,
    Window::Hann,
    Window::Hamming,
    Window::Blackman,
    Window::BlackmanHarris,
    Window::Nuttall,
    Window::FlatTop,
    Window::Kaiser(8.0),
];

fn prepared(settings: SpectrumSettings) -> (SpectrumAnalyzer, SpectrumReader) {
    let (mut analyzer, reader) = SpectrumAnalyzer::new(settings);
    analyzer.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: 4_096,
        num_channels: 1,
    });
    (analyzer, reader)
}

/// A full-scale sine whose frequency is `cycles` per `period` samples, in
/// single precision. The phase is reduced exactly before it is turned into
/// an angle, so the sine stays on its bin however long it runs.
fn sine_on_grid(cycles: f64, period: usize, frames: usize, phase: f64) -> Vec<f32> {
    let twice = (2.0 * cycles).round() as usize;
    (0..frames)
        .map(|n| {
            // `cycles` is a whole or a half number: work in half cycles.
            let turns = (twice * n) % (2 * period);
            (PI * turns as f64 / period as f64 + phase).sin() as f32
        })
        .collect()
}

#[test]
fn a_full_scale_sine_reads_0_dbfs_in_its_bin_with_every_window() {
    let mut worst = 0.0f64;
    for window in WINDOWS {
        // A power of two, a size the FFT does by Bluestein's method, and
        // an odd one.
        for size in [4_096usize, 3_000, 1_001] {
            for bin in [100, size / 4, size / 2 - 100] {
                let settings = SpectrumSettings::new(size).with_window(window);
                let (mut analyzer, mut reader) = prepared(settings);
                analyzer.measure_samples(&sine_on_grid(bin as f64, size, 2 * size, 0.3));
                let read = analyzer.magnitudes_db()[bin];
                worst = worst.max(read.abs());
                assert!(
                    read.abs() < 1e-3,
                    "{window:?}, size {size}, bin {bin}: {read} dB"
                );
                // The reader gets the same, in single precision.
                let published = f64::from(reader.latest().magnitudes_db()[bin]);
                assert!((published - read).abs() < 1e-5);
            }
        }
    }
    println!("largest distance from 0 dBFS of a full-scale sine on its bin: {worst:.2e} dB");
}

/// The transform of `window` (periodic, `size` samples) at a whole number
/// of bins `bins` from its centre, relative to its value at the centre,
/// from the definition.
fn window_transform(window: Window, size: usize, bins: usize) -> Complex<f64> {
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for n in 0..size {
        let value = window.value(n, size, Symmetry::Periodic);
        // The angle reduced exactly before it is formed.
        let angle = TAU * ((bins * n) % size) as f64 / size as f64;
        re += value * angle.cos();
        im -= value * angle.sin();
        sum += value;
    }
    Complex::new(re / sum, im / sum)
}

#[test]
fn near_either_end_a_sine_on_a_bin_reads_its_mirror_image_as_the_window_predicts() {
    // A sine on bin k is two phasors, at k and at -k. The one at -k lies
    // 2k bins away (fft_size - 2k near the top), and the bin reads
    // |1 - e^(-2i phase) W(2k) / W(0)| times full scale, where W is the
    // window's transform: 0 dBFS wherever W vanishes there. Checked on the
    // lowest and highest eight bins, at an even and an odd size, for two
    // phases a quarter of a turn apart.
    let mut table = Vec::new();
    for window in WINDOWS {
        let mut bin_one = (f64::INFINITY, f64::NEG_INFINITY);
        let mut beyond_one = 0.0f64;
        // How many of the lowest eight bins at the even size, and of the
        // highest eight at the odd size, the window leaks the image into.
        let mut leaking = [0usize; 2];
        for size in [4_096usize, 1_001] {
            let top = (size - 1) / 2;
            let bins: Vec<usize> = (1..=8).chain(top - 7..=top).collect();
            let settings = SpectrumSettings::new(size).with_window(window);
            let (mut analyzer, _) = prepared(settings);
            for bin in bins {
                let image = window_transform(window, size, (2 * bin) % size);
                if image.norm() > 1e-12 {
                    match (size, bin <= 8) {
                        (4_096, true) => leaking[0] += 1,
                        (1_001, false) => leaking[1] += 1,
                        _ => {}
                    }
                }
                for phase in [0.3, 0.3 + PI / 2.0] {
                    analyzer.reset();
                    analyzer.measure_samples(&sine_on_grid(bin as f64, size, size, phase));
                    let read = analyzer.magnitudes_db()[bin];
                    let rotation = Complex::from_polar(1.0, -2.0 * phase);
                    let predicted =
                        20.0 * (Complex::new(1.0, 0.0) - rotation * image).norm().log10();
                    assert!(
                        (read - predicted).abs() < 1e-5,
                        "{window:?}, size {size}, bin {bin}, phase {phase}: {read} dB, predicted {predicted}"
                    );
                    // Where the window has no leakage, exactly full scale.
                    if image.norm() < 1e-12 {
                        assert!(read.abs() < 1e-6, "{window:?} {size} {bin}: {read}");
                    }
                    if size == 4_096 && bin == 1 {
                        // The extremes over all phases: |1 -+ |W(2)/W(0)||.
                        let ratio = image.norm();
                        bin_one = (20.0 * (1.0 - ratio).log10(), 20.0 * (1.0 + ratio).log10());
                    } else if size == 4_096 && bin > 1 && bin < top - 7 {
                        // The larger of the two extremes.
                        beyond_one = beyond_one.max(-20.0 * (1.0 - image.norm()).log10());
                    }
                }
            }
        }
        println!(
            "{window:?}: bin 1 reads {:+.3} to {:+.3} dB depending on phase; bins 2 to 8 within {:.4} dB; the image reaches {} of the lowest bins (even size) and {} of the highest (odd size)",
            bin_one.0, bin_one.1, beyond_one, leaking[0], leaking[1]
        );
        table.push((window, bin_one, beyond_one, leaking));
    }
    // The figures the documentation gives.
    for (window, low, high, beyond, bins) in [
        (Window::Rectangular, 0.0, 0.0, 0.0, [0, 0]),
        (Window::Triangular, 0.0, 0.0, 0.0, [0, 8]),
        (Window::Hann, 0.0, 0.0, 0.0, [0, 1]),
        (Window::Hamming, 0.0, 0.0, 0.0, [0, 1]),
        (Window::Blackman, -0.87, 0.79, 0.0, [1, 1]),
        (Window::BlackmanHarris, -1.90, 1.56, 0.0, [1, 2]),
        (Window::Nuttall, -1.97, 1.60, 0.0, [1, 2]),
        (Window::FlatTop, -8.95, 4.31, 0.14, [2, 2]),
    ] {
        let &(_, (read_low, read_high), read_beyond, leaking) =
            table.iter().find(|(known, ..)| *known == window).unwrap();
        assert!((read_low - low).abs() < 0.01, "{window:?}: {read_low}");
        assert!((read_high - high).abs() < 0.01, "{window:?}: {read_high}");
        assert!(
            (read_beyond - beyond).abs() < 0.01,
            "{window:?}: {read_beyond}"
        );
        assert_eq!(leaking, bins, "{window:?}");
    }
    let &(_, (low, high), beyond, _) = table
        .iter()
        .find(|(window, ..)| *window == Window::Kaiser(8.0))
        .unwrap();
    assert!(
        (low + 0.69).abs() < 0.01 && (high - 0.64).abs() < 0.01,
        "{low} {high}"
    );
    assert!(beyond < 0.003, "{beyond}");
}

#[test]
fn a_reading_whose_sound_stops_falls_at_a_rate_set_by_the_release_time_alone() {
    // Release 0.5 s, frames of 480 samples (10 ms) that do not overlap, a
    // full-scale tone on bin 10 that, at a frame boundary, either stops or
    // drops to -60 dBFS. Smoothing power, the reading falls by
    // 10 log10(e) / 0.5 = 8.69 dB/s in both cases until it nears where it
    // is going: 4.34 dB in the first half second, whatever the floor.
    // (Smoothing decibels towards the floor of -200 dB, it used to fall
    // 124 dB in that half second when the tone stopped, and 37 dB when it
    // dropped to -60 dBFS.)
    let release: f64 = 0.5;
    let per_frame = 10.0 * core::f64::consts::E.log10() * 0.01 / release;
    for after in [0.0, 1e-3] {
        let settings = SpectrumSettings::new(480)
            .with_hop(480)
            .with_window(Window::Rectangular)
            .with_bands(OctaveFraction::Whole)
            .with_ballistics(0.0, release);
        let (mut analyzer, _) = prepared(settings);
        let band = analyzer
            .bands()
            .iter()
            .position(|band| band.nominal() == 1_000.0)
            .unwrap();
        // One second of the tone; with no attack time it reads 0 dB.
        analyzer.measure_samples(&sine_on_grid(10.0, 480, 48_000, 0.0));
        assert!(analyzer.magnitudes_db()[10].abs() < 1e-6);
        let quieter: Vec<f32> = sine_on_grid(10.0, 480, 480, 0.0)
            .iter()
            .map(|&sample| after as f32 * sample)
            .collect();
        let target = 20.0 * f64::from(after as f32).log10();
        let target_power = 10f64.powf(target / 10.0);
        for frame in 1..=1_000 {
            analyzer.measure_samples(&quieter);
            let read = analyzer.magnitudes_db()[10];
            let expected = if after == 0.0 {
                -per_frame * frame as f64
            } else {
                let left = (-(frame as f64) * 0.01 / release).exp();
                10.0 * (target_power + (1.0 - target_power) * left).log10()
            };
            let expected = expected.max(SPECTRUM_FLOOR_DB);
            assert!(
                (read - expected).abs() < 1e-5,
                "to {target} dB, frame {frame}: {read} against {expected}"
            );
            // The band holds the same power and moves the same way.
            assert!((analyzer.bands_db()[band] - read).abs() < 1e-6);
            if frame == 50 {
                println!("{read:.4} dB half a second after the tone went to {after} of full scale");
                assert!((read + 4.343).abs() < 0.001, "{read}");
            }
        }
        if after > 0.0 {
            // Ten seconds, twenty time constants, on it has all but
            // arrived: the e^-20 of the step in power still to go is
            // 0.009 dB at -60 dB.
            let read = analyzer.magnitudes_db()[10];
            assert!(read > target && read - target < 0.01, "{read}");
        }
    }
}

/// The magnitude of the transform of `window` at `bins` bins from its
/// centre, relative to its value at the centre, from the definition.
fn window_gain(window: Window, size: usize, bins: f64) -> f64 {
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for n in 0..size {
        let value = window.value(n, size, Symmetry::Periodic);
        let angle = TAU * bins * n as f64 / size as f64;
        re += value * angle.cos();
        im -= value * angle.sin();
        sum += value;
    }
    (re * re + im * im).sqrt() / sum
}

/// The scalloping loss of `window`: its gain half a bin from its centre,
/// in decibels.
fn scalloping_loss_db(window: Window, size: usize) -> f64 {
    20.0 * window_gain(window, size, 0.5).log10()
}

#[test]
fn a_sine_half_way_between_two_bins_reads_the_scalloping_loss_of_the_window() {
    // Harris (1978), table 1, for the windows it lists.
    let published = [
        (Window::Rectangular, -3.92),
        (Window::Triangular, -1.82),
        (Window::Hann, -1.42),
        (Window::Hamming, -1.75),
        (Window::Blackman, -1.10),
        (Window::BlackmanHarris, -0.83),
    ];
    let size = 4_096;
    for window in WINDOWS {
        let (mut analyzer, _) = prepared(SpectrumSettings::new(size).with_window(window));
        analyzer.measure_samples(&sine_on_grid(500.5, size, size, 0.0));
        let magnitudes = analyzer.magnitudes_db();
        let read = magnitudes[500].max(magnitudes[501]);
        let expected = scalloping_loss_db(window, size);
        // The sine's negative frequency, at -500.5 bins, reaches bins 500
        // and 501 as well, through the window's sidelobes 1000.5 and
        // 1001.5 bins away. Windows that do not fall to zero at their ends
        // (rectangular, Hamming) have sidelobes that decay slowly, and
        // there it is measurable: the reading may differ from the
        // scalloping loss by as much as those sidelobes allow.
        let image = window_gain(window, size, 1_000.5).max(window_gain(window, size, 1_001.5))
            / window_gain(window, size, 0.5);
        let allowed = -20.0 * (1.0 - image).log10() + 1e-6;
        println!(
            "{window:?}: reads {read:.4} dB half way between bins, the window gives {expected:.4}; difference {:.1e}, image allows {allowed:.1e}",
            read - expected
        );
        assert!(
            (read - expected).abs() < allowed,
            "{window:?}: {read} against {expected}"
        );
        if let Some(&(_, harris)) = published.iter().find(|(known, _)| *known == window) {
            assert!(
                (read - harris).abs() < 0.01,
                "{window:?}: {read} against {harris}"
            );
        }
    }
    // The flat top exists for this: within a hundredth of a decibel.
    assert!(scalloping_loss_db(Window::FlatTop, size).abs() < 0.01);
}

#[test]
fn a_full_scale_sine_reads_0_db_in_its_band() {
    // Third-octave and octave bands with a transform of 8192 samples:
    // bins 5.9 Hz apart, so the 1 kHz third (891 to 1122 Hz) is 39 bins
    // wide. Wherever the sine falls in the band, as long as the window's
    // main lobe (up to five bins either side for the flat top) does too,
    // and whatever its phase, the band holds all of its power.
    let mut worst = 0.0f64;
    for (fraction, index, frequencies) in [
        (OctaveFraction::Third, 0, [930.0, 1_000.0, 1_017.3, 1_080.0]),
        (OctaveFraction::Whole, 0, [750.0, 1_000.0, 1_234.5, 1_360.0]),
    ] {
        for window in [Window::Hann, Window::BlackmanHarris, Window::FlatTop] {
            for frequency in frequencies {
                for phase in [0.0, 1.0] {
                    let settings = SpectrumSettings::new(8_192)
                        .with_window(window)
                        .with_bands(fraction);
                    let (mut analyzer, _) = prepared(settings);
                    let signal: Vec<f64> = (0..8_192)
                        .map(|n| (TAU * frequency * n as f64 / RATE + phase).sin())
                        .collect();
                    analyzer.measure_samples(&signal);
                    let band = analyzer
                        .bands()
                        .iter()
                        .position(|band| band.index() == index)
                        .unwrap();
                    let read = analyzer.bands_db()[band];
                    worst = worst.max(read.abs());
                    assert!(
                        read.abs() < 0.01,
                        "{fraction:?} {window:?} {frequency} Hz: {read} dB"
                    );
                }
            }
        }
    }
    println!("largest distance from 0 dB of a full-scale sine in its band: {worst:.1e} dB");
}

#[test]
fn an_octave_band_is_the_sum_of_its_three_thirds() {
    // The bins are shared out by frequency, and an octave band has the
    // edges of its outer thirds, so the powers add up exactly, frame by
    // frame, for any signal.
    let mut noise = Noise(7);
    let signal: Vec<f64> = (0..20_000).map(|_| 0.3 * noise.next()).collect();
    let analyse = |fraction| {
        let settings = SpectrumSettings::new(2_048).with_bands(fraction);
        let (mut analyzer, _) = prepared(settings);
        analyzer.measure_samples(&signal);
        let powers: Vec<f64> = analyzer
            .bands_db()
            .iter()
            .map(|db| 10f64.powf(db / 10.0))
            .collect();
        (analyzer.bands().to_vec(), powers)
    };
    let (octaves, octave_powers) = analyse(OctaveFraction::Whole);
    let (thirds, third_powers) = analyse(OctaveFraction::Third);
    let mut checked = 0;
    for (octave, &power) in octaves.iter().zip(&octave_powers) {
        let parts: Vec<f64> = thirds
            .iter()
            .zip(&third_powers)
            .filter(|(third, _)| {
                (3 * octave.index() - 1..=3 * octave.index() + 1).contains(&third.index())
            })
            .map(|(_, &power)| power)
            .collect();
        if parts.len() == 3 {
            let sum: f64 = parts.iter().sum();
            assert!(
                (sum / power - 1.0).abs() < 1e-9,
                "{octave:?}: {sum} against {power}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 9, "{checked}");
}

/// Noise that repeats every `len` samples, made in the frequency domain:
/// every bin from 1 to `len / 2 - 1` has a random phase and a magnitude of
/// 1 (white) or `1 / sqrt(bin)` (pink). Returns the signal, scaled to a
/// mean square of `mean_square`, and the power each bin contributes,
/// relative to a full-scale sine.
fn periodic_noise(len: usize, pink: bool, seed: u64, mean_square: f64) -> (Vec<f64>, Vec<f64>) {
    let mut spectrum = vec![Complex::<f64>::ZERO; len / 2 + 1];
    let mut noise = Noise(seed);
    for (bin, value) in spectrum.iter_mut().enumerate().take(len / 2).skip(1) {
        let magnitude = if pink { 1.0 / (bin as f64).sqrt() } else { 1.0 };
        *value = Complex::from_polar(magnitude, PI * noise.next());
    }
    let mut signal = vec![0.0; len];
    RealFft::new(len).inverse(&spectrum, &mut signal);
    // A bin k of magnitude |X| holds a cosine of amplitude 2|X|/len, whose
    // power relative to a full-scale sine is (2|X|/len)^2.
    let mut powers: Vec<f64> = spectrum
        .iter()
        .map(|value| (2.0 * value.norm() / len as f64).powi(2))
        .collect();
    let actual: f64 = signal.iter().map(|x| x * x).sum::<f64>() / len as f64;
    let scale = (mean_square / actual).sqrt();
    for sample in &mut signal {
        *sample *= scale;
    }
    for power in &mut powers {
        *power *= scale * scale;
    }
    (signal, powers)
}

#[test]
fn bands_of_white_and_pink_noise_read_the_power_that_lies_in_them() {
    // A second and a half of each kind of noise at -20 dBFS RMS, which
    // repeats after 2^20 samples. The analyser takes transforms of 8192
    // samples every 4096 and the test averages the power of every band
    // over all of them; the expected power of a band is the sum of what
    // the noise's own bins put into it, 0.046 Hz apart.
    let len = 1 << 20;
    let size = 8_192;
    let hop = 4_096;
    let spacing = RATE / len as f64;
    for (pink, name) in [(false, "white"), (true, "pink")] {
        let (signal, powers) = periodic_noise(len, pink, 3, 0.01);
        let settings = SpectrumSettings::new(size)
            .with_hop(hop)
            .with_bands(OctaveFraction::Third);
        let (mut analyzer, _) = prepared(settings);
        let bands = analyzer.bands().to_vec();
        let mut sums = vec![0.0; bands.len()];
        let mut frames = 0;
        analyzer.measure_samples(&signal[..size - hop]);
        for chunk in signal[size - hop..].chunks_exact(hop) {
            analyzer.measure_samples(chunk);
            for (sum, db) in sums.iter_mut().zip(analyzer.bands_db()) {
                *sum += 10f64.powf(db / 10.0);
            }
            frames += 1;
        }
        assert_eq!(frames, (len - size) / hop + 1);

        let expected: Vec<f64> = bands
            .iter()
            .map(|band| {
                powers
                    .iter()
                    .enumerate()
                    .map(|(bin, &power)| {
                        let low = (bin as f64 - 0.5) * spacing;
                        let high = (bin as f64 + 0.5) * spacing;
                        let overlap = high.min(band.upper()) - low.max(band.lower());
                        if overlap > 0.0 {
                            power * overlap / spacing
                        } else {
                            0.0
                        }
                    })
                    .sum()
            })
            .collect();

        let bin_width = RATE / size as f64;
        let mut worst = 0.0f64;
        let (mut total_read, mut total_expected) = (0.0, 0.0);
        for ((band, sum), expected) in bands.iter().zip(&sums).zip(&expected) {
            let read = sum / frames as f64;
            total_read += read;
            total_expected += expected;
            let difference = 10.0 * (read / expected).log10();
            let bins = band.width() / bin_width;
            println!(
                "{name} {:>7} Hz ({bins:5.1} bins): {:7.2} dB, expected {:7.2} dB, {difference:+.3}",
                band.nominal(),
                10.0 * read.log10(),
                10.0 * expected.log10()
            );
            if bins >= 4.0 {
                worst = worst.max(difference.abs());
                assert!(
                    difference.abs() < 0.2,
                    "{name}, {} Hz: {difference} dB",
                    band.nominal()
                );
            }
        }
        let total = 10.0 * (total_read / total_expected).log10();
        println!(
            "{name}: all bands {:.3} dB against {:.3} dB, {total:+.4}; worst band of 4 bins or more {worst:.3} dB",
            10.0 * total_read.log10(),
            10.0 * total_expected.log10()
        );
        assert!(
            total.abs() < 0.05,
            "{name}: the bands add up to {total} dB off"
        );
        // The noise is what it says: white rises 1 dB per third, pink is
        // flat, which the expected powers of the upper bands show.
        let step = 10.0 * (expected[25] / expected[24]).log10();
        let wanted = if pink { 0.0 } else { 1.0 };
        assert!((step - wanted).abs() < 0.01, "{name}: {step} dB per third");
    }
}

#[test]
fn weighting_applies_to_bands_as_well() {
    for weighting in [Weighting::A, Weighting::C] {
        let settings = SpectrumSettings::new(8_192)
            .with_bands(OctaveFraction::Third)
            .with_weighting(weighting);
        let (mut analyzer, _) = prepared(settings);
        let signal: Vec<f64> = (0..8_192)
            .map(|n| (TAU * 100.0 * n as f64 / RATE).sin())
            .collect();
        analyzer.measure_samples(&signal);
        let band = analyzer
            .bands()
            .iter()
            .position(|band| band.nominal() == 100.0)
            .unwrap();
        // The band holds the sine's power, weighted by the curve across
        // the few bins the window spreads it over.
        let read = analyzer.bands_db()[band];
        let expected = weighting.gain_db(100.0);
        assert!(
            (read - expected).abs() < 0.05,
            "{weighting:?}: {read} against {expected}"
        );
    }
}

#[test]
fn the_reader_never_sees_a_frame_half_written() {
    // Each transform of 1024 samples, one after the other with a
    // rectangular window, holds a sine on bin 64 at a level that steps
    // down 1 dB per frame and wraps every 60 frames. A frame's own count
    // therefore says what its bin must read; a frame mixed from two would
    // not match. The analyser runs on its own thread.
    let size = 1_024;
    let frames = 3_000;
    let settings = SpectrumSettings::new(size)
        .with_hop(size)
        .with_window(Window::Rectangular)
        .with_bands(OctaveFraction::Whole);
    let (mut analyzer, mut reader) = prepared(settings);
    let audio = std::thread::spawn(move || {
        let tone = sine_on_grid(64.0, size, size, 0.0);
        let mut block = vec![0.0f32; size];
        for frame in 0..frames {
            let gain = 10f32.powf(-((frame % 60) as f32) / 20.0);
            for (sample, &value) in block.iter_mut().zip(&tone) {
                *sample = gain * value;
            }
            // Fed in two pieces, so the publishing happens at every
            // second call.
            analyzer.measure_samples(&block[..300]);
            analyzer.measure_samples(&block[300..]);
        }
        analyzer
    });

    let mut seen = 0u64;
    let mut checked = 0u64;
    let mut check = |reader: &mut SpectrumReader| {
        if !reader.update() {
            return;
        }
        let frame = reader.frame();
        let count = frame.frame_count();
        assert!(count >= seen, "frame {count} after frame {seen}");
        seen = count;
        if count > 0 {
            let expected = -(((count - 1) % 60) as f64);
            let read = f64::from(frame.magnitudes_db()[64]);
            assert!((read - expected).abs() < 1e-3, "frame {count}: {read} dB");
            let band = frame.bands_db().iter().copied().fold(f32::MIN, f32::max);
            assert!(
                (f64::from(band) - expected).abs() < 1e-3,
                "frame {count}: band {band}"
            );
            checked += 1;
        }
    };
    // Read until the audio thread has ended, whether it finished or
    // panicked; a reader that waited for the last frame instead would wait
    // for ever after a panic.
    while !audio.is_finished() {
        check(&mut reader);
        std::thread::yield_now();
    }
    // The join passes on a panic of the audio thread, and after it the
    // last frame published is there to read.
    let analyzer = audio.join().unwrap();
    check(&mut reader);
    assert_eq!(analyzer.frame_count(), frames);
    assert_eq!(seen, frames, "the reader never got the last frame");
    assert!(checked > 0);
    println!("the reader checked {checked} of {frames} frames");
    // Before the first frame the reader saw the floor.
    let (_, fresh) = prepared(SpectrumSettings::new(64));
    assert!(
        fresh
            .frame()
            .magnitudes_db()
            .iter()
            .all(|&db| db == SPECTRUM_FLOOR_DB as f32)
    );
}
