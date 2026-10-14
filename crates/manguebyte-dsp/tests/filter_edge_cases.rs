//! The filter family (one-pole, ladder, IIR design, cascade, crossover) at
//! the edges of what it accepts, through its public interface: designs
//! whose poles sit very close to the unit circle or to 0 Hz, elliptic
//! specifications with almost no room between ripple and attenuation,
//! bands at the limits, coefficients far out of range, a crossover moved
//! across a tone, and a ladder changing mode. Each test states the claim
//! of the documentation it holds the code to.

use core::f64::consts::TAU;

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::iir::{Band, Design, Family, Sections};
use manguebyte_dsp::{
    BiquadCascade, BiquadCoefficients, Crossover, CrossoverSlope, LadderFilter, LadderMode,
    ProcessSpec, Processor,
};

fn db(gain: f64) -> f64 {
    20.0 * gain.log10()
}

/// Jury's conditions for `z² + a1·z + a2`: every pole strictly inside the
/// unit circle exactly when all three are above zero. The sums are taken in
/// the order that makes them exact where they are close to zero.
fn strictly_stable(section: &BiquadCoefficients) -> bool {
    let (a1, a2) = (section.a1, section.a2);
    (1.0 - a1) + a2 > 0.0 && 1.0 - a2 > 0.0 && (1.0 + a1) + a2 > 0.0
}

/// The radius of the further pole of one section, the textbook way.
fn textbook_radius(section: &BiquadCoefficients) -> f64 {
    let discriminant = section.a1 * section.a1 - 4.0 * section.a2;
    if discriminant < 0.0 {
        section.a2.sqrt()
    } else {
        (section.a1.abs() + discriminant.sqrt()) / 2.0
    }
}

/// Every pole of every section strictly inside the unit circle, by Jury's
/// conditions, by the radius `Sections` reports, and by the textbook radius
/// too.
fn assert_stable(sections: &Sections, what: &str) {
    for section in sections.iter() {
        assert!(strictly_stable(section), "{what}: {section:?}");
        assert!(textbook_radius(section) < 1.0, "{what}: {section:?}");
    }
    assert!(sections.largest_pole_radius() < 1.0, "{what}");
}

/// The iir module documentation: "every pole of the coefficients is strictly
/// inside the unit circle", for designs whose rounding to coefficients
/// would otherwise put a real pole at or beyond `z = 1`: a second-order
/// type II low-pass with 200 dB of attenuation has its poles at 0.00014 Hz
/// for a 10 Hz edge, and `1 + a1 + a2` of about 2e-17, below the last digit
/// of `a1`. (That the processor runs such a design from the design itself,
/// not from these coefficients, is tested next to the cascade.)
#[test]
fn a_chebyshev_ii_design_with_its_poles_near_0_hz_has_stable_coefficients() {
    for (cutoff_hz, rate) in [(10.0, 192_000.0), (1.0, 48_000.0), (10.0, 44_100.0)] {
        for attenuation in [110.0, 150.0, 175.0, 200.0] {
            for order in [1, 2, 3] {
                let design = Design::chebyshev_ii(Band::Lowpass { cutoff_hz }, order, attenuation);
                let sections = design.sections(rate);
                assert_stable(&sections, &format!("{design:?} at {rate} Hz"));
                let gain = sections.magnitude_at(0.0, rate);
                assert!(gain.is_finite() && gain > 0.0, "{design:?}: {gain}");
            }
        }
    }
}

/// The documentation: an elliptic design's attenuation is raised until no
/// pole of its prototype has a damping below 1e-8, and its coefficients are
/// strictly stable. 3 dB and 4 dB at order 11, and 1 dB and 2 dB at order
/// 13, used to round a section at 1 kHz onto the unit circle.
#[test]
fn an_elliptic_design_with_a_loose_stop_band_keeps_its_poles_inside_the_circle() {
    const RATE: f64 = 48_000.0;
    let band = Band::Lowpass { cutoff_hz: 1_000.0 };
    for (order, ripple, attenuation, raised) in [(11, 3.0, 4.0, 11.11), (13, 1.0, 2.0, 9.65)] {
        let design = Design::elliptic(band, order, ripple, attenuation);
        let Family::Elliptic { attenuation_db, .. } = design.family() else {
            unreachable!()
        };
        assert!((attenuation_db - raised).abs() < 0.01, "{attenuation_db}");
        let sections = design.sections(RATE);
        assert_stable(&sections, &format!("{design:?}"));
        // Not on the circle by a hair: the least damped pole is 6.5e-10
        // inside it, about the damping times the pre-warped cutoff.
        assert!(1.0 - sections.largest_pole_radius() > 5e-10);
    }
    // And every order with every ripple, at the least attenuation allowed.
    for order in 1..=16 {
        for ripple in [0.01, 0.5, 1.0, 3.0, 10.0, 20.0] {
            let design = Design::elliptic(band, order, ripple, 0.0);
            assert_stable(&design.sections(RATE), &format!("{design:?}"));
        }
    }
}

/// `Design::magnitude_at` is "the gain the formulas of the family give":
/// for an elliptic design it must stay inside the pass band that defines
/// it, and be a finite gain everywhere, however close the specification
/// comes to its limit.
#[test]
fn the_elliptic_formula_holds_for_loose_specifications() {
    const RATE: f64 = 48_000.0;
    let band = Band::Lowpass { cutoff_hz: 1_000.0 };
    let design = Design::elliptic(band, 11, 3.0, 4.0);
    let sections = design.sections(RATE);
    for hz in [0.0, 300.0, 600.0, 900.0, 990.0, 1_000.0] {
        let formula = db(design.magnitude_at(hz, RATE));
        assert!(
            (-3.0 - 1e-9..=1e-9).contains(&formula),
            "{design:?} at {hz} Hz: {formula} dB"
        );
    }
    // The sections agree with it, to what their coefficients allow.
    let (formula, realised) = (
        db(design.magnitude_at(900.0, RATE)),
        db(sections.magnitude_at(900.0, RATE)),
    );
    assert!(
        (formula - realised).abs() < 1e-4,
        "{formula} against {realised}"
    );
    for (order, ripple, attenuation) in [(15, 3.0, 4.0), (16, 1.0, 2.0), (12, 3.0, 4.0)] {
        let design = Design::elliptic(band, order, ripple, attenuation);
        for hz in [0.0, 500.0, 1_000.0, 5_000.0] {
            let gain = design.magnitude_at(hz, RATE);
            assert!(
                gain.is_finite() && gain > 0.0,
                "{design:?} at {hz} Hz: {gain}"
            );
        }
    }
}

/// The law the precision section of the iir module states for the
/// coefficients: at a frequency, an error of about 1e-16 times the sum over
/// the sections of `(1 + |a1| + |a2|) / |1 + a1·z⁻¹ + a2·z⁻²|` and `(|b0| +
/// |b1| + |b2|) / |b0 + b1·z⁻¹ + b2·z⁻²|`, at most 4 times it where it is
/// below 1 %. At 0 Hz, `z = 1`. For a pole pair close to 0 Hz the
/// denominator there is the product of their distances from `z = 1`, which
/// makes the error grow as `1/d²`; an earlier version of the documentation
/// said `1/d`, which these designs broke by 4 and 60 times.
#[test]
fn the_documented_precision_law_holds_for_poles_close_to_0_hz() {
    const RATE: f64 = 192_000.0;
    let error_and_law = |attenuation: f64| {
        let design = Design::chebyshev_ii(Band::Lowpass { cutoff_hz: 10.0 }, 2, attenuation);
        let sections = design.sections(RATE);
        let law: f64 = 1.1e-16
            * sections
                .iter()
                .map(|s| {
                    (1.0 + s.a1.abs() + s.a2.abs()) / ((1.0 + s.a1) + s.a2).abs()
                        + (s.b0.abs() + s.b1.abs() + s.b2.abs()) / ((s.b0 + s.b1) + s.b2).abs()
                })
                .sum::<f64>();
        let formula = design.magnitude_at(0.0, RATE);
        let error = (sections.magnitude_at(0.0, RATE) / formula - 1.0).abs();
        (error, law)
    };
    for attenuation in [80.0, 100.0, 120.0, 140.0, 160.0, 180.0] {
        let (error, law) = error_and_law(attenuation);
        if law < 0.01 {
            assert!(
                error <= 4.0 * law + 1e-11,
                "{attenuation} dB: {error:e}, law {law:e}"
            );
        } else {
            // Beyond 1 % the law no longer says how far off the
            // coefficients are, only that they are not to be trusted; and
            // they are not.
            assert!(error > 1e-4, "{attenuation} dB: {error:e}, law {law:e}");
        }
    }
    // Every 20 dB more moves the poles 3.16 times closer to 0 Hz and to the
    // circle: an error growing as 1/d would grow 3.16 times, as 1/d² it
    // grows 10 times.
    let growth = error_and_law(120.0).0 / error_and_law(100.0).0;
    assert!(growth > 6.0 && growth < 16.0, "{growth}");
}

/// The crossover documentation: moved from 1 kHz to 100 Hz, from 100 Hz to
/// 1 kHz or across the whole range, the sum's peak rises by at most about
/// 0.05 dB for LR2, 0.17 dB for LR4 and 0.66 dB for LR8. Here as the review
/// measured it (blocks of 64, the change on frame 9600, the peak from frame
/// 4800 on), with a tone at 156.92 Hz, which the review found worse than
/// the 300 Hz the documentation used to quote, and run until well after
/// the glide has arrived.
#[test]
fn the_sum_of_a_gliding_crossover_rises_no_more_than_documented() {
    const RATE: f64 = 48_000.0;
    const TONE_HZ: f64 = 156.92;
    for (slope, documented) in [
        (CrossoverSlope::Lr2, 0.05),
        (CrossoverSlope::Lr4, 0.17),
        (CrossoverSlope::Lr8, 0.66),
    ] {
        let mut crossover = Crossover::<f64>::two_way(slope, 1_000.0);
        crossover.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 64,
            num_channels: 1,
        });
        let mut piece = AudioBuffer::<f64>::new(1, 64);
        let mut sum = AudioBuffer::<f64>::new(1, 64);
        let mut peak = 0.0f64;
        for start in (0..28_800).step_by(64) {
            if start == 9_600 {
                crossover.set_frequency(0, 100.0);
            }
            for (i, sample) in piece.channel_mut(0).iter_mut().enumerate() {
                *sample = 0.5 * (TAU * TONE_HZ * (start + i) as f64 / RATE).sin();
            }
            crossover.split(&piece);
            crossover.sum_into(&mut sum);
            if start >= 4_800 {
                peak = sum.channel(0).iter().fold(peak, |p, s| p.max(s.abs()));
            }
        }
        let rise = db(peak / 0.5);
        assert!(
            rise <= documented,
            "{slope:?}: the sum rose by {rise} dB, documented as at most {documented} dB"
        );
    }
}

/// `LadderMode::output_bound` is the bound of a mode in force; while a
/// change of mode glides, the larger of the two modes' bounds holds. Right
/// after a change from `Hp12` to `Lp24` (which `mode()` then reports) the
/// output is above `Lp24`'s bound of 1, below `Hp12`'s 4, and back within 1
/// once the 20 ms glide has arrived.
#[test]
fn a_ladder_just_switched_to_a_low_pass_mode_keeps_to_the_larger_bound() {
    const RATE: f64 = 48_000.0;
    let mut filter = LadderFilter::<f64>::new(LadderMode::Hp12, 2_000.0)
        .with_resonance(0.95)
        .with_drive_db(24.0);
    filter.prepare(&ProcessSpec {
        sample_rate: RATE,
        max_block_size: 64,
        num_channels: 1,
    });
    let mut block = AudioBuffer::<f64>::new(1, 64);
    let (mut gliding, mut arrived) = (0.0f64, 0.0f64);
    for index in 0..250 {
        if index == 150 {
            filter.set_mode(LadderMode::Lp24);
        }
        // A square wave of 2 kHz, at the cutoff.
        for (i, sample) in block.channel_mut(0).iter_mut().enumerate() {
            *sample = if ((index * 64 + i) / 12) % 2 == 0 {
                1.0
            } else {
                -1.0
            };
        }
        filter.process(&mut block);
        let peak = block.channel(0).iter().fold(0.0f64, |p, s| p.max(s.abs()));
        assert!(peak.is_finite());
        if (150..165).contains(&index) {
            assert_eq!(filter.mode(), LadderMode::Lp24);
            gliding = gliding.max(peak);
        } else if index >= 165 {
            arrived = arrived.max(peak);
        }
    }
    let larger = LadderMode::Hp12
        .output_bound()
        .max(LadderMode::Lp24.output_bound());
    assert!(
        gliding > LadderMode::Lp24.output_bound() && gliding <= larger,
        "{gliding}"
    );
    assert!(arrived <= LadderMode::Lp24.output_bound(), "{arrived}");
}

/// The cascade passes a section through when its state-variable form is
/// not made of finite numbers: huge but finite coefficients give no
/// not-a-number, and leave nothing behind for the sections set after them.
#[test]
fn a_cascade_given_huge_finite_coefficients_stays_finite() {
    let spec = ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 64,
        num_channels: 1,
    };
    for a1 in [1e300, -1e300] {
        let huge = BiquadCoefficients {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1,
            a2: 0.0,
        };
        let mut cascade = BiquadCascade::<f64>::new(&[huge]).with_glide(0.0);
        cascade.prepare(&spec);
        let mut block = AudioBuffer::<f64>::new(1, 64);
        block.channel_mut(0).fill(0.5);
        cascade.process(&mut block);
        let with_huge = block.channel(0)[10];
        cascade.set_sections(&[BiquadCoefficients::IDENTITY]);
        block.channel_mut(0).fill(0.5);
        cascade.process(&mut block);
        let after = block.channel(0)[10];
        assert!(
            with_huge.is_finite() && after == 0.5,
            "{a1}: huge section: {with_huge}; then the identity section: {after}"
        );
    }
}

/// The iir documentation: band edges are limited to between `LOWEST_HZ`
/// and `HIGHEST_RATIO` of the sample rate, and a band too narrow is widened
/// from a limit inwards where widening it around its centre would cross
/// the limit. A band at the upper limit then ends there: the band-pass is
/// below -3.01 dB just beyond it.
#[test]
fn widening_a_narrow_band_keeps_its_edges_within_the_limits() {
    const RATE: f64 = 48_000.0;
    let top = 0.49 * RATE;
    let design = Design::butterworth(
        Band::Bandpass {
            low_hz: top,
            high_hz: top,
        },
        2,
    );
    let sections = design.sections(RATE);
    let beyond = db(sections.magnitude_at(0.4902 * RATE, RATE));
    assert!(
        beyond < -3.0103,
        "{beyond} dB at 0.4902 of the sample rate: the upper edge lies beyond 0.49"
    );
    let at_top = db(sections.magnitude_at(top, RATE));
    assert!(
        (at_top + 3.0103).abs() < 1e-4,
        "{at_top} dB at the upper edge"
    );
}
