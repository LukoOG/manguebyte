//! Running each format's own conformance tool on a bundle, and judging
//! what it prints.
//!
//! - CLAP: [`clap-validator`](https://github.com/free-audio/clap-validator),
//!   from the CLAP project, run as `clap-validator validate <plugin>`. Its
//!   last line is a summary such as
//!   `34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings`.
//! - VST 3: `validator`, from Steinberg's VST 3 SDK, run as
//!   `validator -e <bundle>`. It ends with
//!   `Result: 47 tests passed, 0 tests failed`.
//! - LV2: nothing is run. `cargo mangue validate --format lv2` is refused;
//!   an LV2 bundle is checked with lv2lint, from the OpenMusicKontrollers
//!   project, as this project's CI does. For LV2 the functions below name
//!   lv2lint where they name a program, and have no variable or option to
//!   give, and a [`Judge`] finds no summary in any output.
//!
//! The summary line decides, exactly as in this project's CI: a plugin
//! passes when every summary reports no failed test and at least one that
//! passed. Output with no summary is a failure, because a validator that
//! crashed half-way has not validated anything.
//!
//! A validator's output is somebody else's text. It is read one line at a
//! time in bounded memory, and nothing in it can make the judge panic.
//!
//! Steinberg's validator prints thousands of lines for a plugin with many
//! parameters, so not all of them are shown: see [`Judge::feed`] for which
//! are left out. No kind of remark is left out altogether, so what the
//! validator says about a failed test is there to read.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::Error;
use crate::lines::Lines;
use crate::platform::Format;
use crate::tools::Tools;

/// Most bytes kept of one line of a validator's output.
const MAX_LINE_BYTES: usize = 64 << 10;

/// Most remarks about single parameters whose wording is remembered, so
/// that a repeat for another parameter can be told from a new remark. A
/// real run makes one or two; past this number every remark is shown.
const MAX_REMARKS: usize = 64;

/// Longest wording of a remark that is remembered, in bytes. A longer one
/// is shown every time.
const MAX_REMARK_BYTES: usize = 512;

/// The name of the program that validates a format, as looked for on
/// `PATH`.
pub fn program(format: Format) -> &'static str {
    match format {
        Format::Clap => "clap-validator",
        Format::Vst3 => "validator",
        Format::Lv2 => "lv2lint",
    }
}

/// The environment variable that names the validator of a format.
pub fn variable(format: Format) -> &'static str {
    match format {
        Format::Clap => "CLAP_VALIDATOR",
        Format::Vst3 => "VST3_VALIDATOR",
        // None: no LV2 validator is run.
        Format::Lv2 => "",
    }
}

/// The command-line option that names the validator of a format.
pub fn option(format: Format) -> &'static str {
    match format {
        Format::Clap => "--clap-validator",
        Format::Vst3 => "--vst3-validator",
        // None: no LV2 validator is run.
        Format::Lv2 => "",
    }
}

/// Where to get the validator of a format, for messages.
pub fn where_to_get(format: Format) -> &'static str {
    match format {
        Format::Clap => {
            "Build it from https://github.com/free-audio/clap-validator (cargo build --release)"
        }
        Format::Vst3 => {
            "Build the validator target of the VST 3 SDK from \
             https://github.com/steinbergmedia/vst3sdk"
        }
        Format::Lv2 => {
            "cargo mangue does not run an LV2 validator; check the bundle with lv2lint, from \
             the OpenMusicKontrollers project"
        }
    }
}

/// Finds the validator of a format.
///
/// In order: the path given on the command line, the path in the format's
/// environment variable, and the program on `PATH`. Returns `None` if it is
/// not named and not installed.
///
/// # Errors
///
/// Fails if a validator is named, by option or by variable, and no file is
/// there. Falling back to another one would hide the mistake.
pub fn locate(
    format: Format,
    named: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
    tools: &dyn Tools,
) -> Result<Option<PathBuf>, Error> {
    let check = |path: PathBuf, source: &str| {
        if path.is_file() {
            Ok(Some(path))
        } else {
            Err(Error::failed(format!(
                "{source} names {} as the {} validator, and there is no such file. Correct \
                 the path, or remove it to look for {} on PATH.",
                path.display(),
                format.name(),
                program(format)
            )))
        }
    };
    if let Some(named) = named {
        return check(named.to_path_buf(), option(format));
    }
    if let Some(value) = env(variable(format)).filter(|value| !value.is_empty()) {
        return check(
            PathBuf::from(value),
            &format!("The {} variable", variable(format)),
        );
    }
    Ok(tools.find(program(format)))
}

/// The arguments a format's validator is run with on `bundle`.
pub fn arguments(format: Format, bundle: &Path) -> Vec<OsString> {
    let first = match format {
        Format::Clap => "validate",
        // Run the extensive tests, as CI does.
        Format::Vst3 => "-e",
        // Not run: lv2lint takes the plugin's URI as well as the bundle.
        Format::Lv2 => return vec![bundle.as_os_str().to_owned()],
    };
    vec![OsString::from(first), bundle.as_os_str().to_owned()]
}

/// Removes terminal colour and cursor sequences from a line.
///
/// These are the control sequences of ECMA-48: the escape character, `[`,
/// parameters, and a final byte from `@` to `~`.
pub fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\x1b' && characters.peek() == Some(&'[') {
            characters.next();
            for inner in characters.by_ref() {
                if ('@'..='~').contains(&inner) {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    Cow::Owned(plain)
}

/// What a validator concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every summary reported no failure, and this many tests passed.
    Passed {
        /// Tests that passed.
        passed: u64,
    },
    /// At least one test failed.
    Failed {
        /// Tests that passed.
        passed: u64,
        /// Tests that failed.
        failed: u64,
    },
    /// The output had no summary, or the summary counted no test at all:
    /// nothing was validated.
    NoResult,
}

impl Verdict {
    /// Whether the plugin passed.
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Passed { .. })
    }
}

/// Reads a validator's output line by line and reaches a [`Verdict`].
///
/// ```
/// use cargo_mangue::platform::Format;
/// use cargo_mangue::validate::{Judge, Verdict};
///
/// let mut judge = Judge::new(Format::Clap);
/// judge.feed("   - state-reproducibility-basic: PASSED");
/// judge.feed("34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings");
/// assert_eq!(judge.verdict(), Verdict::Passed { passed: 34 });
///
/// let mut judge = Judge::new(Format::Vst3);
/// judge.feed("Result: 46 tests passed, 1 tests failed");
/// assert_eq!(judge.verdict(), Verdict::Failed { passed: 46, failed: 1 });
/// ```
#[derive(Debug, Clone)]
pub struct Judge {
    format: Format,
    summaries: u32,
    passed: u64,
    failed: u64,
    /// The wordings of the remarks about single parameters shown so far:
    /// what each line had before and after the parameter's number and id.
    remarks: Vec<(String, String)>,
    /// How many lines repeated one of them for another parameter.
    repeats: u64,
}

impl Judge {
    /// A judge for the validator of `format`.
    pub fn new(format: Format) -> Self {
        Self {
            format,
            summaries: 0,
            passed: 0,
            failed: 0,
            remarks: Vec::new(),
            repeats: 0,
        }
    }

    /// Takes one line of output. Returns whether the line is worth showing.
    ///
    /// Every line of `clap-validator` is. Of what Steinberg's validator
    /// prints, three kinds of line are not:
    ///
    /// - the listing of the plugin's parameters, one line each of the form
    ///   `Parameter 12 (id=34): [title="Cutoff"] ...`, up to about two
    ///   thousand lines for a plugin that maps every MIDI controller on
    ///   every channel;
    /// - `[Succeeded]` after every test, and empty lines;
    /// - a remark about one parameter, such as `Parameter 12 (id=34) is
    ///   kIsHidden and NOT kIsReadOnly!`, when the same remark has already
    ///   been shown for another parameter. The first one in each wording is
    ///   shown, so no kind of remark goes unseen, and
    ///   [`repeats_left_out`](Self::repeats_left_out) counts the rest.
    ///
    /// CI leaves out every line that names a parameter by number, remarks
    /// included; this shows more than that and never less.
    pub fn feed(&mut self, line: &str) -> bool {
        let line = strip_ansi(line);
        let summary = match self.format {
            Format::Clap => clap_summary(&line),
            Format::Vst3 => vst3_summary(&line),
            Format::Lv2 => None,
        };
        if let Some((passed, failed)) = summary {
            self.summaries = self.summaries.saturating_add(1);
            self.passed = self.passed.saturating_add(passed);
            self.failed = self.failed.saturating_add(failed);
        }
        match self.format {
            Format::Clap | Format::Lv2 => true,
            Format::Vst3 => self.shows_vst3(&line),
        }
    }

    /// How many lines were not shown because each repeated, for another
    /// parameter, a remark that had been shown.
    pub fn repeats_left_out(&self) -> u64 {
        self.repeats
    }

    /// Whether a line of Steinberg's validator is worth showing.
    fn shows_vst3(&mut self, line: &str) -> bool {
        let line = line.trim_end();
        if line.is_empty() || line == "[Succeeded]" {
            return false;
        }
        let Some((before, after)) = parameter_mention(line) else {
            return true;
        };
        if after.starts_with(": [title=") {
            // The listing.
            return false;
        }
        // A remark. Its wording is the line without the parameter's number
        // and id.
        if before.len() + after.len() > MAX_REMARK_BYTES {
            return true;
        }
        let seen = self
            .remarks
            .iter()
            .any(|(start, end)| start == before && end == after);
        if seen {
            self.repeats = self.repeats.saturating_add(1);
            return false;
        }
        if self.remarks.len() < MAX_REMARKS {
            self.remarks.push((before.to_owned(), after.to_owned()));
        }
        true
    }

    /// The conclusion from the lines fed so far.
    pub fn verdict(&self) -> Verdict {
        if self.failed > 0 {
            Verdict::Failed {
                passed: self.passed,
                failed: self.failed,
            }
        } else if self.summaries == 0 || self.passed == 0 {
            Verdict::NoResult
        } else {
            Verdict::Passed {
                passed: self.passed,
            }
        }
    }
}

/// Splits a number off the start of `text`.
fn leading_number(text: &str) -> Option<(u64, &str)> {
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let number = text[..digits].parse().ok()?;
    Some((number, &text[digits..]))
}

/// Reads `<n> tests run, <n> passed, <n> failed`, as (passed, failed).
fn clap_summary(line: &str) -> Option<(u64, u64)> {
    let (before, after) = line.split_once(" tests run, ")?;
    // The count of tests run ends the text before the marker.
    let run = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    before[before.len() - run..].parse::<u64>().ok()?;
    let (passed, rest) = leading_number(after)?;
    let (failed, rest) = leading_number(rest.strip_prefix(" passed, ")?)?;
    rest.strip_prefix(" failed")?;
    Some((passed, failed))
}

/// Reads `Result: <n> tests passed, <n> tests failed`, as (passed, failed).
fn vst3_summary(line: &str) -> Option<(u64, u64)> {
    let (_, after) = line.split_once("Result: ")?;
    let (passed, rest) = leading_number(after)?;
    let (failed, rest) = leading_number(rest.strip_prefix(" tests passed, ")?)?;
    rest.strip_prefix(" tests failed")?;
    Some((passed, failed))
}

/// Finds where a line of Steinberg's validator names a parameter by its
/// number and id, as in `Parameter 12 (id=34)`, and returns what the line
/// has before the number and what it has after the id.
///
/// The form is the one CI's filter looks for, `Parameter [0-9]* (id=`. Of
/// several mentions the first counts, and the others are part of what
/// comes after it.
fn parameter_mention(line: &str) -> Option<(&str, &str)> {
    const MARKER: &str = "Parameter ";
    const ID: &str = " (id=";
    let digits =
        |text: &str| text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    line.match_indices(MARKER).find_map(|(at, _)| {
        let number = at + MARKER.len();
        let rest = &line[number..];
        let rest = rest[digits(rest)..].strip_prefix(ID)?;
        // The id, signed or not, and the parenthesis that closes it if it
        // is there.
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        let rest = &rest[digits(rest)..];
        Some((&line[..number], rest.strip_prefix(')').unwrap_or(rest)))
    })
}

/// Judges everything a validator prints, copying the lines worth showing
/// to `shown`.
///
/// Which lines those are is said at [`Judge::feed`]. If remarks were left
/// out because they repeated one already shown for another parameter, a
/// last line in parentheses says how many, so that what is shown never
/// passes for the whole output.
///
/// Memory use does not grow with the amount of output or the length of its
/// lines.
///
/// # Errors
///
/// Fails if reading the output or writing to `shown` fails.
pub fn judge_stream(
    format: Format,
    output: impl BufRead,
    shown: &mut dyn Write,
) -> io::Result<Verdict> {
    let mut judge = Judge::new(format);
    let mut lines = Lines::new(output, MAX_LINE_BYTES);
    while let Some(line) = lines.next_line()? {
        let text = String::from_utf8_lossy(line.bytes);
        if judge.feed(&text) {
            writeln!(shown, "{text}")?;
        }
    }
    let repeats = judge.repeats_left_out();
    if repeats > 0 {
        writeln!(
            shown,
            "({repeats} more lines are not shown: each repeats, for another parameter, a remark \
             shown above.)"
        )?;
    }
    Ok(judge.verdict())
}

/// What happened when a validator was run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// What its output says.
    pub verdict: Verdict,
    /// How the program ended, as text, if it did not end successfully. A
    /// validator that reports no failed test and still ends this way is
    /// worth a look, and CI judges by the summary alone, so this is
    /// reported and does not change the verdict.
    pub ended_badly: Option<String>,
}

/// Runs a validator on a bundle, showing its output on `shown`.
///
/// # Errors
///
/// Fails if the validator cannot be started or its output cannot be read.
/// A failed test is not an error here: it is in the [`Outcome`].
pub fn run(
    format: Format,
    validator: &Path,
    bundle: &Path,
    shown: &mut dyn Write,
) -> Result<Outcome, Error> {
    let cannot = |what: &str, error: &io::Error| {
        Error::failed(format!(
            "The {} validator {} {what}: {error}.",
            format.name(),
            validator.display()
        ))
    };
    let mut child = Command::new(validator)
        .args(arguments(format, bundle))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| cannot("could not be started", &error))?;
    let judged = match child.stdout.take() {
        Some(stdout) => judge_stream(format, BufReader::new(stdout), shown),
        None => Ok(Verdict::NoResult),
    };
    let status = child
        .wait()
        .map_err(|error| cannot("could not be waited for", &error))?;
    let verdict =
        judged.map_err(|error| cannot("printed something that could not be read", &error))?;
    Ok(Outcome {
        verdict,
        ended_badly: (!status.success()).then(|| status.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judged(format: Format, output: &str) -> Verdict {
        let mut judge = Judge::new(format);
        for line in output.lines() {
            judge.feed(line);
        }
        judge.verdict()
    }

    #[test]
    fn a_clap_summary_with_no_failure_passes() {
        let output = "Plugin 'Mangue Pluck'\n   - param-conversions: PASSED\n\n\
                      34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings\n";
        assert_eq!(judged(Format::Clap, output), Verdict::Passed { passed: 34 });
    }

    #[test]
    fn a_clap_summary_in_colour_is_read_the_same() {
        let output = "\x1b[1m34 tests run\x1b[0m, \x1b[32m33 passed\x1b[0m, \
                      \x1b[31m1 failed\x1b[0m, 10 skipped, 0 warnings";
        assert_eq!(
            judged(Format::Clap, output),
            Verdict::Failed {
                passed: 33,
                failed: 1
            }
        );
    }

    #[test]
    fn a_vst3_result_line_decides() {
        let passed = "* Scanning classes...\n[Scan Parameters]\n[Succeeded]\n\
                      -------------------------------------------------------------\n\
                      Result: 47 tests passed, 0 tests failed\n\
                      -------------------------------------------------------------\n";
        assert_eq!(judged(Format::Vst3, passed), Verdict::Passed { passed: 47 });
        let failed = "Result: 40 tests passed, 7 tests failed\n";
        assert_eq!(
            judged(Format::Vst3, failed),
            Verdict::Failed {
                passed: 40,
                failed: 7
            }
        );
    }

    #[test]
    fn several_summaries_add_up_and_one_failure_is_enough() {
        let output = "Result: 47 tests passed, 0 tests failed\n\
                      Result: 46 tests passed, 1 tests failed\n";
        assert_eq!(
            judged(Format::Vst3, output),
            Verdict::Failed {
                passed: 93,
                failed: 1
            }
        );
    }

    #[test]
    fn output_with_no_summary_or_no_test_is_not_a_pass() {
        for (format, output) in [
            (Format::Clap, ""),
            (Format::Clap, "error: could not load the plugin"),
            (Format::Clap, "0 tests run, 0 passed, 0 failed, 0 skipped"),
            (Format::Vst3, "Result: 0 tests passed, 0 tests failed"),
            (Format::Vst3, "Segmentation fault"),
            // Each format reads only its own validator's summary.
            (Format::Vst3, "34 tests run, 34 passed, 0 failed"),
            (Format::Clap, "Result: 47 tests passed, 0 tests failed"),
        ] {
            assert_eq!(judged(format, output), Verdict::NoResult, "{output:?}");
        }
    }

    #[test]
    fn a_summary_cut_short_or_damaged_is_not_a_summary() {
        let clap = "34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings";
        let vst3 = "Result: 47 tests passed, 0 tests failed";
        for (format, line, shortest) in [
            (Format::Clap, clap, "34 tests run, 34 passed, 0 failed"),
            (Format::Vst3, vst3, vst3),
        ] {
            // Cut at every byte: a pass only once the whole summary is in.
            for cut in 0..=line.len() {
                let verdict = judged(format, &line[..cut]);
                assert_eq!(
                    verdict.is_pass(),
                    cut >= shortest.len(),
                    "{:?}",
                    &line[..cut]
                );
            }
            // One byte changed, at every position: never a panic.
            for at in 0..line.len() {
                for replacement in ["x", "9", " ", "\u{1b}", "é", ""] {
                    let mut damaged = line.to_owned();
                    damaged.replace_range(at..=at, replacement);
                    let _ = judged(format, &damaged);
                }
            }
        }
        for line in [
            "tests run, 34 passed, 0 failed",
            "x tests run, 34 passed, 0 failed",
            "34 tests run, passed, 0 failed",
            "34 tests run, 34 passed, failed",
            "34 tests run, 34 passed, -1 failed",
            "34 tests run, 99999999999999999999999 passed, 0 failed",
            "99999999999999999999999 tests run, 34 passed, 0 failed",
        ] {
            assert_eq!(judged(Format::Clap, line), Verdict::NoResult, "{line}");
        }
        for line in [
            "Result: tests passed, 0 tests failed",
            "Result: 47 tests passed, tests failed",
            "Result: 47 tests passed",
            "Result: 99999999999999999999999 tests passed, 0 tests failed",
        ] {
            assert_eq!(judged(Format::Vst3, line), Verdict::NoResult, "{line}");
        }
    }

    #[test]
    fn counts_too_large_to_add_do_not_overflow() {
        let mut judge = Judge::new(Format::Vst3);
        for _ in 0..3 {
            judge.feed("Result: 18446744073709551615 tests passed, 0 tests failed");
        }
        assert_eq!(judge.verdict(), Verdict::Passed { passed: u64::MAX });
    }

    #[test]
    fn colour_sequences_are_removed_and_nothing_else() {
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[1;32mgreen\x1b[0m text"), "green text");
        assert_eq!(strip_ansi("\x1b[2Kcleared"), "cleared");
        // An escape that starts no sequence is kept; a sequence that never
        // ends takes the rest of the line with it.
        assert_eq!(strip_ansi("a\x1bb"), "a\x1bb");
        assert_eq!(strip_ansi("a\x1b[31"), "a");
        assert_eq!(strip_ansi("\x1b"), "\x1b");
    }

    #[test]
    fn the_long_lists_of_the_vst3_validator_are_not_shown() {
        let mut judge = Judge::new(Format::Vst3);
        assert!(!judge.feed("   Parameter 1742 (id=1742): [title=\"MIDI CC 3|5\"]"));
        assert!(!judge.feed("[Succeeded]"));
        assert!(!judge.feed(""));
        assert!(!judge.feed("   "));
        assert!(judge.feed("[Scan Parameters]"));
        assert!(judge.feed("[Failed]"));
        assert!(judge.feed("Info:  ===== Parameter count: 2054 ====="));
        assert!(judge.feed("Result: 47 tests passed, 0 tests failed"));

        let mut judge = Judge::new(Format::Clap);
        assert!(judge.feed(""));
        assert!(judge.feed("[Succeeded]"));
    }

    #[test]
    fn a_stream_is_judged_and_the_lines_worth_showing_are_copied() {
        // The parameter line is in the form of the validator's listing. It
        // used to be `Parameter 0 (id=7): x`, which no validator prints and
        // which is now taken for what it would be, a remark.
        let output = b"[Scan Parameters]\n   Parameter 0 (id=7): [title=\"x\"]\n[Succeeded]\n\r\n\
                       Result: 2 tests passed, 0 tests failed\n";
        let mut shown = Vec::new();
        let verdict = judge_stream(Format::Vst3, &output[..], &mut shown).unwrap();
        assert_eq!(verdict, Verdict::Passed { passed: 2 });
        assert_eq!(
            String::from_utf8(shown).unwrap(),
            "[Scan Parameters]\nResult: 2 tests passed, 0 tests failed\n"
        );
    }

    /// Found in review: every line naming a parameter by number was left
    /// out, and Steinberg's validator prints remarks in that form too, such
    /// as `=>Parameter 006 (id=2130706432) is kIsHidden and NOT
    /// kIsReadOnly!`. Whatever it had to say about one parameter was gone,
    /// while a failed run sent the reader to "the validator's output above".
    #[test]
    fn a_remark_about_a_parameter_is_shown_once_in_each_wording() {
        let mut judge = Judge::new(Format::Vst3);
        // The lines of a real run, for a wrapper with MIDI controllers.
        let listing = |number: u32, id: u32| {
            format!(
                "Info:     Parameter {number:03} (id={id}): [title=\"MIDI 1 CC {number}\"] \
                 [unit=\"\"] [type = Float, default = 0.000000, unit = 0]"
            )
        };
        let hidden = |number: u32, id: u32| {
            format!("Info:  =>Parameter {number:03} (id={id}) is kIsHidden and NOT kIsReadOnly!")
        };
        assert!(!judge.feed(&listing(6, 2_130_706_432)));
        assert!(judge.feed(&hidden(6, 2_130_706_432)));
        for number in 7..2_086 {
            let id = 2_130_706_426 + number;
            assert!(!judge.feed(&listing(number, id)), "{number}");
            assert!(!judge.feed(&hidden(number, id)), "{number}");
        }
        assert_eq!(judge.repeats_left_out(), 2_079);

        // Another remark is another wording, however little it differs, and
        // is shown; so is the same one after another prefix.
        assert!(judge.feed("Info:  =>Parameter 006 (id=2130706432) is kIsHidden and kIsReadOnly!"));
        assert!(
            judge.feed("ERROR: =>Parameter 006 (id=2130706432) is kIsHidden and NOT kIsReadOnly!")
        );
        assert!(judge.feed("Parameter 5 (id=9) has no title"));
        assert!(!judge.feed("Parameter 6 (id=10) has no title"));
        assert!(judge.feed("Parameter 6 (id=10): has no title"));
        // A remark that names two parameters is told apart by the second.
        assert!(judge.feed("Parameter 1 (id=1) has the id of Parameter 2 (id=1)"));
        assert!(judge.feed("Parameter 3 (id=4) has the id of Parameter 5 (id=4)"));
        assert!(!judge.feed("Parameter 9 (id=4) has the id of Parameter 5 (id=4)"));
        assert_eq!(judge.repeats_left_out(), 2_081);
        // None of that is a result.
        assert_eq!(judge.verdict(), Verdict::NoResult);

        // The CLAP validator's lines are all shown, whatever they look like.
        let mut clap = Judge::new(Format::Clap);
        assert!(clap.feed(&hidden(1, 1)));
        assert!(clap.feed(&hidden(2, 2)));
        assert!(clap.feed(&listing(2, 2)));
        assert_eq!(clap.repeats_left_out(), 0);
    }

    #[test]
    fn what_is_remembered_of_remarks_is_bounded_and_past_it_everything_is_shown() {
        let mut judge = Judge::new(Format::Vst3);
        // More wordings than are remembered: each is shown, twice over,
        // since the later ones cannot be recognised.
        for round in 0..2 {
            for wording in 0..MAX_REMARKS + 10 {
                let shown = judge.feed(&format!("Parameter 1 (id=1) differs in way {wording}"));
                let remembered = wording < MAX_REMARKS;
                assert_eq!(shown, round == 0 || !remembered, "{round} {wording}");
            }
        }
        assert_eq!(judge.remarks.len(), MAX_REMARKS);
        // A wording too long to remember is shown every time.
        let long = format!("Parameter 1 (id=1) {}", "x".repeat(MAX_REMARK_BYTES));
        assert!(judge.feed(&long));
        assert!(judge.feed(&long));
        assert_eq!(judge.remarks.len(), MAX_REMARKS);
        let kept: usize = judge
            .remarks
            .iter()
            .map(|(start, end)| start.len() + end.len())
            .sum();
        assert!(kept <= MAX_REMARKS * MAX_REMARK_BYTES);
    }

    #[test]
    fn a_stream_says_how_many_repeated_remarks_it_left_out() {
        let output = "[Scan Parameters]\n\
                      Info:     Parameter 000 (id=5): [title=\"Level\"]\n\
                      Info:     Parameter 001 (id=6): [title=\"MIDI 1 CC 0\"]\n\
                      Info:  =>Parameter 001 (id=6) is kIsHidden and NOT kIsReadOnly!\n\
                      Info:     Parameter 002 (id=7): [title=\"MIDI 1 CC 1\"]\n\
                      Info:  =>Parameter 002 (id=7) is kIsHidden and NOT kIsReadOnly!\n\
                      Info:     Parameter 003 (id=8): [title=\"MIDI 1 CC 2\"]\n\
                      Info:  =>Parameter 003 (id=8) is kIsHidden and NOT kIsReadOnly!\n\
                      [Succeeded]\n\
                      Result: 1 tests passed, 0 tests failed\n";
        let mut shown = Vec::new();
        let verdict = judge_stream(Format::Vst3, output.as_bytes(), &mut shown).unwrap();
        assert_eq!(verdict, Verdict::Passed { passed: 1 });
        assert_eq!(
            String::from_utf8(shown).unwrap(),
            "[Scan Parameters]\n\
             Info:  =>Parameter 001 (id=6) is kIsHidden and NOT kIsReadOnly!\n\
             Result: 1 tests passed, 0 tests failed\n\
             (2 more lines are not shown: each repeats, for another parameter, a remark shown \
             above.)\n"
        );
    }

    #[test]
    fn mentions_of_a_parameter_are_found_whatever_surrounds_them() {
        assert_eq!(
            parameter_mention("Info:  =>Parameter 006 (id=2130706432) is hidden"),
            Some(("Info:  =>Parameter ", " is hidden"))
        );
        assert_eq!(
            parameter_mention("Parameter 0 (id=7): [title=\"x\"]"),
            Some(("Parameter ", ": [title=\"x\"]"))
        );
        // As in CI's filter, the number may be missing; the id may be
        // signed, and the line may stop anywhere.
        assert_eq!(
            parameter_mention("Parameter  (id=3) x"),
            Some(("Parameter ", " x"))
        );
        assert_eq!(
            parameter_mention("Parameter 1 (id=-3) x"),
            Some(("Parameter ", " x"))
        );
        assert_eq!(
            parameter_mention("Parameter 1 (id="),
            Some(("Parameter ", ""))
        );
        assert_eq!(
            parameter_mention("Parameter 1 (id=3"),
            Some(("Parameter ", ""))
        );
        // The first mention that is one counts.
        assert_eq!(
            parameter_mention("Parameter count, then Parameter 2 (id=3)!"),
            Some(("Parameter count, then Parameter ", "!"))
        );
        for no_mention in [
            "",
            "Parameter",
            "Parameter ",
            "Parameter 12",
            "Parameter 12 (id",
            "Parameter 12(id=3)",
            "Parameter é (id=3)",
            "Info:  ===== Parameter count: 2054 =====",
            "parameter 1 (id=3)",
        ] {
            assert_eq!(parameter_mention(no_mention), None, "{no_mention:?}");
        }
    }

    #[test]
    fn output_that_is_not_text_is_judged_without_a_panic() {
        let mut output = vec![0xff, 0xfe, 0x00, b'\n', 0x80];
        output.extend_from_slice(b"\n34 tests run, 34 passed, 0 failed\n");
        output.extend_from_slice(&[0xc3]);
        let verdict = judge_stream(Format::Clap, &output[..], &mut io::sink()).unwrap();
        assert_eq!(verdict, Verdict::Passed { passed: 34 });
    }

    #[test]
    fn the_validators_are_run_the_way_ci_runs_them() {
        let bundle = Path::new("target/bundled/Mangue Pluck.clap");
        assert_eq!(
            arguments(Format::Clap, bundle),
            [OsString::from("validate"), bundle.into()]
        );
        let bundle = Path::new("target/bundled/Mangue Pluck.vst3");
        assert_eq!(
            arguments(Format::Vst3, bundle),
            [OsString::from("-e"), bundle.into()]
        );
        assert_eq!(program(Format::Clap), "clap-validator");
        assert_eq!(program(Format::Vst3), "validator");
        assert_eq!(variable(Format::Clap), "CLAP_VALIDATOR");
        assert_eq!(variable(Format::Vst3), "VST3_VALIDATOR");
    }
}
