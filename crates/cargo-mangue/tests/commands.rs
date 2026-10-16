//! The whole path, on this machine: build, bundle, install, uninstall and
//! validate, with the real cargo.
//!
//! The plugin is an empty library (see `common/fixture.rs`), so this runs
//! in the ordinary test suite: nothing is downloaded and each build takes
//! a moment. It proves what the pure tests cannot: that the library is
//! found from what cargo reports, for every profile and for a named
//! target, and that what lands on disk is the layout planned for this
//! system. The plugin folders are a folder of the test's own; nothing is
//! written to the real ones. The validators are stand-ins that print what
//! the real ones print.
//!
//! One test, run in order, for the reason given in `validators.rs`.

mod common;
#[path = "common/fake.rs"]
mod fake;
#[path = "common/fixture.rs"]
mod fixture;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_mangue::cargo::Cargo;
use cargo_mangue::install::{Scope, plugin_dir};
use cargo_mangue::layout::{BundlePlan, BundleSpec, Contents, plan};
use cargo_mangue::platform::{Format, Os, Target};
use common::Scratch;

struct Flow {
    project: PathBuf,
    target_dir: PathBuf,
    home: Vec<(&'static str, OsString)>,
}

impl Flow {
    /// Runs the tool in the plugin's folder.
    fn tool(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-mangue"));
        command
            .current_dir(&self.project)
            .args(args)
            .env("CARGO_TARGET_DIR", &self.target_dir)
            .envs(self.home.iter().map(|(key, value)| (*key, value.clone())))
            .env_remove("CARGO_BUILD_TARGET")
            .env_remove("CLAP_VALIDATOR")
            .env_remove("VST3_VALIDATOR");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.tool(args).output().unwrap()
    }

    fn lookup(&self, name: &str) -> Option<OsString> {
        self.home
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

fn succeeded(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{}", stderr(output));
}

/// Whether the tool could tell from cargo's report what the library was
/// built for, and it was what the tool had expected: neither of its two
/// notes about that was printed.
fn target_was_as_expected(output: &Output) -> bool {
    let said = stderr(output);
    !said.contains("could not be checked") && !said.contains("is set up to build for")
}

/// The one dynamic library cargo left in a profile's folder.
fn built_library(profile_dir: &Path, os: Os) -> PathBuf {
    let mut found: Vec<PathBuf> = fs::read_dir(profile_dir)
        .unwrap_or_else(|error| panic!("{}: {error}", profile_dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(os.library_extension()))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

/// Checks that every file of `plan` is in `out`, and that the plugin's
/// code in it is `library`.
fn assert_bundled(plan: &BundlePlan, out: &Path, library: &Path, os: Os) {
    for file in &plan.files {
        let path = out.join(&file.path);
        assert!(path.is_file(), "{} is missing", path.display());
        match &file.contents {
            Contents::Bytes(bytes) => assert_eq!(&fs::read(&path).unwrap(), bytes),
            // Signing on macOS rewrites the library, so there it is only
            // checked to be there.
            _ if os == Os::Macos => assert!(fs::metadata(&path).unwrap().len() > 0),
            _ => assert_eq!(
                fs::read(&path).unwrap(),
                fs::read(library).unwrap(),
                "{} is not {}",
                path.display(),
                library.display()
            ),
        }
    }
    assert_eq!(out.join(&plan.root).is_file(), plan.single_file);
}

#[test]
fn a_plugin_goes_from_source_to_installed_and_validated() {
    let scratch = Scratch::new("commands");
    let mut home = vec![
        ("HOME", scratch.path().join("home").into_os_string()),
        (
            "LOCALAPPDATA",
            scratch.path().join("local").into_os_string(),
        ),
    ];
    // Cargo and rustup find themselves through HOME where it is the home
    // folder, so they are told where they are before it is pointed away.
    home.extend(toolchain_homes());
    let flow = Flow {
        project: fixture::plugin(scratch.path()),
        target_dir: scratch.path().join("target"),
        home,
    };

    let triple = Cargo::from_env().default_target().unwrap();
    let host = Target::from_triple(&triple).unwrap();
    let os = Os::host().unwrap();
    assert_eq!(host.os, os);
    let planned = |format: Format, name: &str| {
        plan(&BundleSpec {
            format,
            os,
            archs: vec![host.arch],
            name: name.to_owned(),
            identifier: fixture::IDENTIFIER.to_owned(),
            version: fixture::VERSION.to_owned(),
        })
        .unwrap()
    };
    // The tool says where each result is, one line per bundle.
    let reported = |output: &Output, verb: &str, root: &str| {
        stdout(output)
            .lines()
            .any(|line| line.starts_with(&format!("{verb} ")) && line.ends_with(root))
    };

    // --- what cannot work is refused before anything is built.
    let foreign = if os == Os::Linux {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    for (args, expected) in [
        (
            &["bundle", "--target", "wasm32-unknown-unknown"][..],
            "does not bundle plugins for",
        ),
        (
            &["bundle", "--target", "riscv64gc-unknown-linux-gnu"],
            "processor architecture",
        ),
        (
            &[
                "bundle",
                "--target",
                "x86_64-pc-windows-msvc",
                "x86_64-unknown-linux-gnu",
            ],
            "A bundle is for one system",
        ),
        (
            &[
                "bundle",
                "--target",
                "x86_64-pc-windows-msvc",
                "aarch64-pc-windows-msvc",
            ],
            "holds one architecture",
        ),
        (
            &[
                "bundle",
                "--target",
                "x86_64-pc-windows-msvc",
                "x86_64-pc-windows-gnu",
            ],
            "Give each architecture once",
        ),
        (
            &["bundle", "--name", "a/b"],
            "cannot be used as a file name",
        ),
        (
            &["bundle", "-p", "no-such-package"],
            "The packages that build a plugin are: fixture-plugin",
        ),
        (
            &["install", "--target", foreign],
            "works with plugins for this machine",
        ),
    ] {
        let refused = flow.run(args);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&refused)
        );
        assert!(stderr(&refused).contains(expected), "{}", stderr(&refused));
        assert_eq!(stdout(&refused), "");
    }
    assert!(!flow.target_dir.exists(), "something was built");

    // --- bundle: both formats, the name from Cargo.toml, into target/bundled.
    let bundled = flow.run(&["bundle"]);
    succeeded(&bundled);
    let out = flow.target_dir.join("bundled");
    let debug = built_library(&flow.target_dir.join("debug"), os);
    for format in Format::DEFAULT {
        let plan = planned(format, fixture::NAME);
        assert_bundled(&plan, &out, &debug, os);
        assert!(
            reported(&bundled, "Bundled", &plan.root),
            "{}",
            stdout(&bundled)
        );
    }
    assert_eq!(stdout(&bundled).lines().count(), 2);
    // Where cargo put the library told the tool what it was built for.
    assert!(target_was_as_expected(&bundled), "{}", stderr(&bundled));

    // --- as cargo runs it, with a name and a format and a folder chosen.
    let elsewhere = scratch.path().join("dist");
    let named = flow.run(&[
        "mangue",
        "bundle",
        "-p",
        fixture::PACKAGE,
        "--name",
        "Other Name",
        "--format",
        "clap",
        "--out",
        elsewhere.to_str().unwrap(),
    ]);
    succeeded(&named);
    assert_bundled(&planned(Format::Clap, "Other Name"), &elsewhere, &debug, os);
    assert!(!elsewhere.join("Other Name.vst3").exists());
    assert_eq!(stdout(&named).lines().count(), 1);

    // --- other profiles: the library is wherever cargo says it is.
    for (args, folder) in [
        (&["--release"][..], "release"),
        (&["--profile", "fast"], "fast"),
    ] {
        let dist = scratch.path().join(format!("dist-{folder}"));
        let mut all = vec!["bundle", "--out", dist.to_str().unwrap()];
        all.extend_from_slice(args);
        succeeded(&flow.run(&all));
        let library = built_library(&flow.target_dir.join(folder), os);
        assert_ne!(library, debug);
        for format in Format::DEFAULT {
            assert_bundled(&planned(format, fixture::NAME), &dist, &library, os);
        }
    }

    // --- a target named in cargo's configuration, which the tool does not
    // read. Cargo builds into that target's folder and says so, and the
    // tool goes by what cargo says. The target named is this machine's own,
    // the one that is installed; `cargo_target.rs` covers the others.
    let in_triple = flow.target_dir.join(&triple).join("debug");
    assert!(!in_triple.exists());
    let config = flow.project.join(".cargo");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!("[build]\ntarget = \"{triple}\"\n"),
    )
    .unwrap();
    let dist = scratch.path().join("dist-configured");
    let configured = flow.run(&["bundle", "--out", dist.to_str().unwrap()]);
    succeeded(&configured);
    let library = built_library(&in_triple, os);
    for format in Format::DEFAULT {
        assert_bundled(&planned(format, fixture::NAME), &dist, &library, os);
    }
    assert!(
        target_was_as_expected(&configured),
        "{}",
        stderr(&configured)
    );
    fs::remove_dir_all(&config).unwrap();

    // --- a named target, which cargo builds into the same folder.
    let dist = scratch.path().join("dist-target");
    let named_target = flow.run(&[
        "bundle",
        "--target",
        &triple,
        "--out",
        dist.to_str().unwrap(),
    ]);
    succeeded(&named_target);
    let library = built_library(&in_triple, os);
    for format in Format::DEFAULT {
        assert_bundled(&planned(format, fixture::NAME), &dist, &library, os);
    }
    assert!(
        target_was_as_expected(&named_target),
        "{}",
        stderr(&named_target)
    );

    // --- install, into the folders of the test's own home.
    let lookup = |name: &str| flow.lookup(name);
    let installed = flow.run(&["install"]);
    succeeded(&installed);
    let mut places = Vec::new();
    for format in Format::DEFAULT {
        let dir = plugin_dir(os, format, Scope::User, &lookup).unwrap();
        assert!(dir.starts_with(scratch.path()), "{}", dir.display());
        let plan = planned(format, fixture::NAME);
        assert_bundled(&plan, &dir, &debug, os);
        assert!(
            reported(&installed, "Installed", &plan.root),
            "{}",
            stdout(&installed)
        );
        places.push(dir.join(&plan.root));
    }
    // Installing again replaces what is there.
    succeeded(&flow.run(&["install", "--format", "vst3"]));
    assert!(places.iter().all(|place| place.exists()));

    // `--out` naming the plugin folder itself: the bundle is written
    // straight to where it is installed, and is left there. (Found in
    // review: it was removed as "the copy that is already installed", the
    // command failed to copy what it had just removed and ended with exit
    // code 1, and the plugin was gone.)
    for (format, place) in Format::DEFAULT.into_iter().zip(&places) {
        let dir = place.parent().unwrap();
        let in_place = flow.run(&[
            "install",
            "--format",
            format.extension(),
            "--out",
            dir.to_str().unwrap(),
        ]);
        succeeded(&in_place);
        let plan = planned(format, fixture::NAME);
        assert_bundled(&plan, dir, &debug, os);
        for verb in ["Bundled", "Installed"] {
            assert!(
                reported(&in_place, verb, &plan.root),
                "{}",
                stdout(&in_place)
            );
        }
        assert_eq!(stdout(&in_place).lines().count(), 2);
    }
    assert!(places.iter().all(|place| place.exists()));

    // --- uninstall, with the name read from Cargo.toml.
    let removed = flow.run(&["uninstall"]);
    succeeded(&removed);
    for (place, format) in places.iter().zip(Format::DEFAULT) {
        let root = planned(format, fixture::NAME).root;
        assert!(reported(&removed, "Removed", &root), "{}", stdout(&removed));
        assert!(!place.exists());
    }
    let nothing = flow.run(&["uninstall"]);
    succeeded(&nothing);
    assert_eq!(stdout(&nothing).matches(" is not installed in ").count(), 2);

    // --- validate, with stand-ins for the two validators.
    let clap_pass = fake::program(
        scratch.path(),
        "clap-pass",
        &["34 tests run, 34 passed, 0 failed, 10 skipped, 0 warnings"],
        0,
    );
    let clap_fail = fake::program(
        scratch.path(),
        "clap-fail",
        &["34 tests run, 33 passed, 1 failed, 10 skipped, 0 warnings"],
        1,
    );
    let vst3_pass = fake::program(
        scratch.path(),
        "vst3-pass",
        &["[Succeeded]", "Result: 47 tests passed, 0 tests failed"],
        0,
    );
    let silent = fake::program(scratch.path(), "silent", &[], 0);

    // One by option and one by variable; both pass.
    let passed = flow
        .tool(&["validate", "--clap-validator", clap_pass.to_str().unwrap()])
        .env("VST3_VALIDATOR", &vst3_pass)
        .output()
        .unwrap();
    succeeded(&passed);
    let said = stdout(&passed);
    assert!(said.contains("Validated Fixture Plugin as CLAP: 34 tests passed, none failed\n"));
    assert!(said.contains("Validated Fixture Plugin as VST 3: 47 tests passed, none failed\n"));
    // Each was given the bundle of its own format.
    assert!(said.contains("Fixture Plugin.clap"), "{said}");
    assert!(said.contains("Fixture Plugin.vst3"), "{said}");
    assert!(said.contains("args: validate "), "{said}");
    assert!(said.contains("args: -e "), "{said}");
    assert!(!said.contains("[Succeeded]"), "{said}");

    // A failed test in one format fails the command, with exit code 4,
    // after the other format has been validated too.
    let failed = flow
        .tool(&["validate", "--clap-validator", clap_fail.to_str().unwrap()])
        .env("VST3_VALIDATOR", &vst3_pass)
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(4), "{}", stderr(&failed));
    let complaint = stderr(&failed);
    assert!(
        complaint.contains("error: Validation failed."),
        "{complaint}"
    );
    assert!(complaint.contains("Fixture Plugin as CLAP: 1 tests failed, 33 passed"));
    assert!(stdout(&failed).contains("Validated Fixture Plugin as VST 3"));

    // A validator that says nothing has validated nothing.
    let unsaid = flow.run(&[
        "validate",
        "--format",
        "vst3",
        "--vst3-validator",
        silent.to_str().unwrap(),
    ]);
    assert_eq!(unsaid.status.code(), Some(4), "{}", stderr(&unsaid));
    assert!(stderr(&unsaid).contains("the validator gave no result"));

    // --- a build that fails is exit code 3, and nothing is bundled.
    fs::write(
        flow.project.join("src").join("lib.rs"),
        "this is not Rust\n",
    )
    .unwrap();
    let dist = scratch.path().join("dist-broken");
    let broken = flow.run(&["bundle", "--out", dist.to_str().unwrap()]);
    assert_eq!(broken.status.code(), Some(3), "{}", stderr(&broken));
    assert!(stderr(&broken).contains("error: cargo build failed for fixture-plugin"));
    assert_eq!(stdout(&broken), "");
    assert!(!dist.exists());
}

/// `CARGO_HOME` and `RUSTUP_HOME` as they are now, for a child whose
/// `HOME` is somewhere else. Only needed where they default to folders in
/// `HOME`, which is everywhere but Windows.
fn toolchain_homes() -> Vec<(&'static str, OsString)> {
    if cfg!(windows) {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let or_default = |name: &str, folder: &str| {
        std::env::var_os(name).unwrap_or_else(|| home.join(folder).into_os_string())
    };
    vec![
        ("CARGO_HOME", or_default("CARGO_HOME", ".cargo")),
        ("RUSTUP_HOME", or_default("RUSTUP_HOME", ".rustup")),
    ]
}
