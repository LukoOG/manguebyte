//! Everything that reads text from outside, given text made to trip it.
//!
//! The tool reads arguments, target triples, names, paths cargo reports and
//! the lines of a validator. Each reader has its own tests for the inputs
//! its author thought of. This sweep, written in review, gives all of them
//! thousands of inputs put together from their own keywords, cut short and
//! mixed with characters of more than one byte, and asks only that none of
//! them panics.

use std::path::Path;

use cargo_mangue::cargo::built_for;
use cargo_mangue::cli;
use cargo_mangue::layout;
use cargo_mangue::lines::Lines;
use cargo_mangue::platform::{Format, Target};
use cargo_mangue::scaffold::{self, Kind};
use cargo_mangue::validate::{Judge, strip_ansi};

/// A small deterministic generator (xorshift64), so that the sweep tries
/// the same inputs on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }
}

/// Text put together from the pieces the readers look for, cut and mixed
/// with characters of more than one byte.
fn hostile(random: &mut Random) -> String {
    const PIECES: &[&str] = &[
        "",
        " ",
        "-",
        "--",
        "=",
        ",",
        ".",
        "..",
        "/",
        "\\",
        "\u{1b}",
        "\u{1b}[",
        "\u{1b}[31m",
        "é",
        "日本",
        "\u{0}",
        "\r",
        "\n",
        "0",
        "9",
        "18446744073709551616",
        " tests run, ",
        " passed, ",
        " failed",
        "Result: ",
        " tests passed, ",
        " tests failed",
        "Parameter ",
        " (id=",
        ")",
        ": [title=",
        "[Succeeded]",
        "host:",
        "x86_64",
        "aarch64",
        "windows",
        "linux",
        "darwin",
        "android",
        "CON",
        "COM1",
        "LPT",
        "bundle",
        "install",
        "uninstall",
        "validate",
        "new",
        "help",
        "mangue",
        "--format",
        "--target",
        "--features",
        "--name",
        "--out",
        "-p",
        "-F",
        "-h",
        "clap",
        "vst3",
        "Self",
        "plugin",
        "template",
        "target",
        "debug",
        "_",
    ];
    let mut text = String::new();
    for _ in 0..random.below(7) {
        text.push_str(PIECES[random.below(PIECES.len())]);
    }
    text
}

/// `type_name` reads a whole template each time it is called, so it gets
/// every fiftieth input.
#[test]
fn nothing_that_reads_text_from_outside_panics_on_hostile_input() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    // One judge of each format lives through the whole sweep, so that what
    // it remembers of earlier lines is in play too.
    let mut judges = Format::ALL.map(Judge::new);
    for round in 0..20_000 {
        let text = hostile(&mut random);
        let _ = Target::from_triple(&text);
        let _ = cargo_mangue::platform::host_triple(&text);
        let _ = layout::bundle_version(&text);
        let _ = layout::check_plugin_name(&text);
        let _ = layout::check_identifier(&text);
        let _ = layout::info_plist(Format::Vst3, &text, &text, &text);
        let _ = strip_ansi(&text);
        for format in Format::ALL {
            let mut judge = Judge::new(format);
            judge.feed(&text);
            let _ = judge.verdict();
        }
        for judge in &mut judges {
            judge.feed(&text);
            let _ = judge.verdict();
            let _ = judge.repeats_left_out();
        }
        let _ = scaffold::check_package_name(&text);
        let _ = scaffold::display_name(&text);
        let _ = scaffold::default_id(&text);
        if round % 50 == 0 {
            for kind in Kind::ALL {
                let _ = scaffold::type_name(&text, kind);
            }
        }
        let other = hostile(&mut random);
        let _ = built_for(Path::new(&text), Path::new(&other));
        for os in cargo_mangue::platform::Os::ALL {
            for format in Format::ALL {
                let _ = layout::plan(&layout::BundleSpec {
                    format,
                    os,
                    archs: vec![cargo_mangue::platform::Arch::X86_64],
                    name: other.clone(),
                    identifier: text.clone(),
                    version: text.clone(),
                });
            }
        }
        let arguments: Vec<String> = (0..random.below(6)).map(|_| hostile(&mut random)).collect();
        let _ = cli::parse(arguments);
        let mut lines = Lines::new(text.as_bytes(), random.below(9));
        while let Ok(Some(_)) = lines.next_line() {}
    }
}
