//! Running a validator as a separate program and judging what it prints.
//!
//! The validators themselves are not on a test machine, so small scripts
//! stand in for them, printing what `clap-validator` and Steinberg's
//! `validator` print. What is checked is everything around the real tools:
//! that the program is started with the right arguments, that its output is
//! read to the end and shown, and that the verdict follows the summary.
//!
//! This file has a single test on purpose. On Linux a script that one
//! thread has just written cannot be started while another thread is in
//! the middle of starting a program of its own, and one test at a time is
//! the simple way never to meet that.

mod common;
#[path = "common/fake.rs"]
mod fake;

use std::path::Path;

use cargo_mangue::platform::Format;
use cargo_mangue::validate::{self, Verdict};
use common::Scratch;

fn run(format: Format, validator: &Path, bundle: &Path) -> (validate::Outcome, String) {
    let mut shown = Vec::new();
    let outcome = validate::run(format, validator, bundle, &mut shown).unwrap();
    (outcome, String::from_utf8(shown).unwrap())
}

#[test]
fn a_validator_is_started_read_to_the_end_and_judged_by_its_summary() {
    let scratch = Scratch::new("validators");
    let dir = scratch.path();
    // The stand-ins never open it, so it need not exist.
    let clap = dir.join("Mangue Pluck.clap");
    let vst3 = dir.join("Mangue Pluck.vst3");

    // Every test passed.
    let passing = fake::program(
        dir,
        "clap-pass",
        &[
            "Plugin 'Mangue Pluck' (org.manguebyte.pluck)",
            "   - param-conversions: PASSED",
            "",
            "34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings",
        ],
        0,
    );
    let (outcome, shown) = run(Format::Clap, &passing, &clap);
    assert_eq!(outcome.verdict, Verdict::Passed { passed: 34 });
    assert_eq!(outcome.ended_badly, None);
    // It was run as `clap-validator validate <plugin>`.
    let arguments = shown.lines().next().unwrap();
    assert!(arguments.starts_with("args: validate "), "{arguments}");
    assert!(arguments.contains("Mangue Pluck.clap"), "{arguments}");
    // Everything it printed was shown, in order.
    assert!(
        shown.contains("   - param-conversions: PASSED\n\n34 tests run"),
        "{shown}"
    );

    // A test failed, and the program said so with its exit code too.
    let failing = fake::program(
        dir,
        "clap-fail",
        &[
            "   - state-reproducibility-basic: FAILED",
            "34 tests run, 33 passed, 1 failed, 10 skipped, 0 warnings",
        ],
        1,
    );
    let (outcome, shown) = run(Format::Clap, &failing, &clap);
    assert_eq!(
        outcome.verdict,
        Verdict::Failed {
            passed: 33,
            failed: 1
        }
    );
    assert!(outcome.ended_badly.is_some());
    assert!(
        shown.contains("state-reproducibility-basic: FAILED"),
        "{shown}"
    );

    // It stopped before any summary: nothing was validated.
    let crashing = fake::program(dir, "clap-crash", &["Plugin 'Mangue Pluck'"], 3);
    let (outcome, _) = run(Format::Clap, &crashing, &clap);
    assert_eq!(outcome.verdict, Verdict::NoResult);
    let ended = outcome.ended_badly.unwrap();
    assert!(ended.contains('3'), "{ended}");

    // No failed test, and then a bad ending: the summary decides, as in
    // CI, and the ending is reported.
    let untidy = fake::program(
        dir,
        "vst3-untidy",
        &["Result: 47 tests passed, 0 tests failed"],
        1,
    );
    let (outcome, _) = run(Format::Vst3, &untidy, &vst3);
    assert_eq!(outcome.verdict, Verdict::Passed { passed: 47 });
    assert!(outcome.ended_badly.is_some());

    // Steinberg's validator, with the lines nobody reads left out.
    let steinberg = fake::program(
        dir,
        "vst3-pass",
        &[
            "[Scan Parameters]",
            "Info:  ===== Parameter count: 2 =====",
            "   Parameter 0 (id=1164): [title=Cutoff]",
            "   Parameter 1 (id=77): [title=Level]",
            "[Succeeded]",
            "",
            "[Valid State Transition]",
            "[Succeeded]",
            "Result: 2 tests passed, 0 tests failed",
        ],
        0,
    );
    let (outcome, shown) = run(Format::Vst3, &steinberg, &vst3);
    assert_eq!(outcome.verdict, Verdict::Passed { passed: 2 });
    assert_eq!(outcome.ended_badly, None);
    let shown: Vec<&str> = shown.lines().collect();
    // It was run as `validator -e <bundle>`.
    assert!(shown[0].starts_with("args: -e "), "{}", shown[0]);
    assert!(shown[0].contains("Mangue Pluck.vst3"), "{}", shown[0]);
    assert_eq!(
        shown[1..],
        [
            "[Scan Parameters]",
            "Info:  ===== Parameter count: 2 =====",
            "[Valid State Transition]",
            "Result: 2 tests passed, 0 tests failed",
        ]
    );

    // The same output is not a CLAP summary.
    let (outcome, _) = run(Format::Clap, &steinberg, &clap);
    assert_eq!(outcome.verdict, Verdict::NoResult);

    // A validator that is not there cannot be started, which is an error
    // and not a verdict.
    let mut shown = Vec::new();
    let missing = dir.join("no-such-validator");
    let error = validate::run(Format::Clap, &missing, &clap, &mut shown).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("no-such-validator"), "{error}");
    assert!(error.message().contains("could not be started"), "{error}");
}
