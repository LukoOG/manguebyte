//! The program itself: what it prints and the code it exits with.
//!
//! Each test runs the built `cargo-mangue` the way a person or a script
//! would. Nothing here builds a plugin; the test that does is in
//! `scaffold.rs` and is run on request.

mod common;

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use cargo_mangue::install::{Scope, plugin_dir};
use cargo_mangue::platform::{Format, Os};
use cargo_mangue::scaffold;
use common::Scratch;

fn tool() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-mangue"));
    // Whatever the machine running the tests has set must not decide them.
    command
        .env_remove("CLAP_VALIDATOR")
        .env_remove("VST3_VALIDATOR")
        .env_remove("CARGO_BUILD_TARGET");
    command
}

fn run(args: &[&str]) -> Output {
    tool().args(args).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

#[test]
fn help_for_the_tool_and_for_every_command_goes_to_standard_output() {
    let general = run(&["--help"]);
    assert_eq!(general.status.code(), Some(0));
    assert!(stdout(&general).starts_with("cargo-mangue "));
    assert!(stdout(&general).contains("Usage: cargo mangue <command> [options]"));
    assert_eq!(stderr(&general), "");

    for command in ["new", "bundle", "install", "uninstall", "validate"] {
        let help = run(&[command, "--help"]);
        assert_eq!(help.status.code(), Some(0), "{command}");
        let text = stdout(&help);
        assert!(
            text.contains(&format!("Usage: cargo mangue {command}")),
            "{text}"
        );
        assert!(text.contains("Options:"), "{text}");
        assert_eq!(stderr(&help), "");
        // The other spellings give the same text.
        assert_eq!(stdout(&run(&[command, "-h"])), text);
        assert_eq!(stdout(&run(&["help", command])), text);
    }

    // `help` is listed as a command, and answers like one. (Found in
    // review: `help --help` was refused as "not a cargo mangue command".)
    for args in [
        &["help"][..],
        &["help", "--help"],
        &["help", "-h"],
        &["help", "help"],
    ] {
        let help = run(args);
        assert_eq!(help.status.code(), Some(0), "{args:?}: {}", stderr(&help));
        assert_eq!(stdout(&help), stdout(&general), "{args:?}");
        assert_eq!(stderr(&help), "");
    }
}

#[test]
fn it_answers_to_cargo_mangue_and_to_cargo_dash_mangue() {
    // `cargo mangue x` reaches the program as `cargo-mangue mangue x`.
    for args in [&["mangue", "--version"][..], &["--version"], &["-V"]] {
        let version = run(args);
        assert_eq!(version.status.code(), Some(0));
        assert_eq!(
            stdout(&version),
            format!("cargo-mangue {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
    assert_eq!(
        stdout(&run(&["mangue", "bundle", "--help"])),
        stdout(&run(&["bundle", "--help"]))
    );
}

#[test]
fn a_wrong_command_line_exits_with_2_and_says_what_to_do() {
    let nothing = run(&[]);
    assert_eq!(nothing.status.code(), Some(2));
    assert_eq!(stdout(&nothing), "");
    assert!(stderr(&nothing).starts_with("error: No command was given."));
    assert!(stderr(&nothing).contains("Usage: cargo mangue <command> [options]"));

    for (args, expected) in [
        (&["bundel"][..], "\"bundel\" is not a cargo mangue command"),
        (
            &["bundle", "--frmat", "clap"],
            "cargo mangue bundle has no option --frmat",
        ),
        (
            &["bundle", "--format", "vst2"],
            "The formats are clap and vst3",
        ),
        (&["bundle", "--target"], "The option --target needs a value"),
        (&["install", "--release", "--profile", "fast"], "Use one"),
        (&["new"], "needs the name of the package"),
        (&["new", "2fast"], "cannot be the name of a package"),
        (
            &["new", "my-gain", "--kind", "generator"],
            "effect and instrument",
        ),
        (&["new", "my-gain", "--id", "no dots"], "is not usable"),
        (
            &["uninstall", "--name", "../../etc"],
            "cannot be used as a file name",
        ),
    ] {
        let output = run(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(stdout(&output), "", "{args:?}");
        let message = stderr(&output);
        assert!(message.starts_with("error: "), "{message}");
        assert!(message.contains(expected), "{args:?}: {message}");
    }
}

#[test]
fn new_writes_a_crate_and_refuses_to_write_over_one() {
    let scratch = Scratch::new("cli-new");
    let new = |args: &[&str]| {
        tool()
            .current_dir(scratch.path())
            .args(args)
            .output()
            .unwrap()
    };

    let created = new(&["new", "my-gain"]);
    assert_eq!(created.status.code(), Some(0), "{}", stderr(&created));
    let dir = scratch.path().join("my-gain");
    assert!(stdout(&created).starts_with("Created an audio effect \"My Gain\" in "));
    assert!(stdout(&created).contains("my-gain"));
    // What to do next, and what must be decided before shipping.
    let advice = stderr(&created);
    assert!(advice.contains("cargo test"), "{advice}");
    assert!(advice.contains("cargo mangue bundle"), "{advice}");
    assert!(
        advice.contains("set its id (now \"com.example.my-gain\")"),
        "{advice}"
    );
    assert!(
        advice.contains("Its vendor is \"Example Audio\" until"),
        "{advice}"
    );
    // Cargo refuses a package inside a workspace that does not list it, so
    // the tool says so when that is where the crate landed.
    assert_eq!(
        advice.contains("is inside the workspace"),
        scaffold::enclosing_workspace(&dir).is_some(),
        "{advice}"
    );

    let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("name = \"my-gain\""));
    assert!(manifest.contains("[package.metadata.manguebyte]\nname = \"My Gain\"\n"));
    let source = fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
    assert!(source.contains("pub struct MyGain {"));
    assert!(source.contains("kind: PluginKind::Effect,"));
    assert_eq!(
        fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "/target\n"
    );

    // Again: an error, and the work in the folder is untouched.
    fs::write(dir.join("src").join("lib.rs"), "// my work").unwrap();
    let again = new(&["new", "my-gain", "--kind", "instrument"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        stderr(&again).contains("already exists"),
        "{}",
        stderr(&again)
    );
    assert_eq!(
        fs::read_to_string(dir.join("src").join("lib.rs")).unwrap(),
        "// my work"
    );

    // Every option, for an instrument.
    let synth = new(&[
        "new",
        "my-synth",
        "--kind",
        "instrument",
        "--name",
        "Sintetizador da Ana",
        "--vendor",
        "Ana Lima",
        "--id",
        "br.com.ana.synth",
    ]);
    assert_eq!(synth.status.code(), Some(0), "{}", stderr(&synth));
    assert!(stdout(&synth).starts_with("Created an instrument \"Sintetizador da Ana\" in "));
    // Nothing left to remind about once the id and vendor are chosen.
    let advice = stderr(&synth);
    assert!(!advice.contains("Before you give"), "{advice}");
    assert!(!advice.contains("Its vendor is"), "{advice}");
    let source = fs::read_to_string(scratch.path().join("my-synth/src/lib.rs")).unwrap();
    assert!(source.contains("pub struct MySynth {"));
    assert!(source.contains("id: \"br.com.ana.synth\","));
    assert!(source.contains("name: \"Sintetizador da Ana\","));
    assert!(source.contains("vendor: \"Ana Lima\","));
    assert!(source.contains("kind: PluginKind::Instrument,"));

    // A path that holds no Manguebyte is refused before anything is made.
    let lost = new(&["new", "other", "--manguebyte-path", "my-gain/src"]);
    assert_eq!(lost.status.code(), Some(2));
    assert!(
        stderr(&lost).contains("--manguebyte-path"),
        "{}",
        stderr(&lost)
    );
    assert!(!scratch.path().join("other").exists());
}

/// The variables that decide where plugins are installed, pointed into a
/// folder of the test's own.
fn home_in(folder: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("HOME", folder.join("home").into_os_string()),
        ("LOCALAPPDATA", folder.join("local").into_os_string()),
        ("CommonProgramFiles", folder.join("common").into_os_string()),
        ("CommonProgramW6432", folder.join("common").into_os_string()),
    ]
}

#[test]
fn uninstall_removes_what_is_installed_and_reports_what_is_not() {
    let Some(os) = Os::host() else {
        return;
    };
    let scratch = Scratch::new("cli-uninstall");
    let home = home_in(scratch.path());
    let lookup = |name: &str| {
        home.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    };
    let uninstall = |args: &[&str]| {
        tool()
            .envs(home.iter().map(|(key, value)| (*key, value.clone())))
            .arg("uninstall")
            .args(args)
            .output()
            .unwrap()
    };

    // Nothing installed: said, and not an error.
    let nothing = uninstall(&["--name", "Mangue Pluck"]);
    assert_eq!(nothing.status.code(), Some(0), "{}", stderr(&nothing));
    let said = stdout(&nothing);
    assert!(
        said.contains("Mangue Pluck.clap is not installed in "),
        "{said}"
    );
    assert!(
        said.contains("Mangue Pluck.vst3 is not installed in "),
        "{said}"
    );

    // Put a plugin where `install` would, in both formats, with a
    // neighbour that must survive.
    let clap_dir = plugin_dir(os, Format::Clap, Scope::User, &lookup).unwrap();
    let vst3_dir = plugin_dir(os, Format::Vst3, Scope::User, &lookup).unwrap();
    assert!(clap_dir.starts_with(scratch.path()) && vst3_dir.starts_with(scratch.path()));
    let clap = clap_dir.join("Mangue Pluck.clap");
    let vst3 = vst3_dir.join("Mangue Pluck.vst3");
    let neighbour = vst3_dir.join("Mangue Duck.vst3");
    fs::create_dir_all(&clap_dir).unwrap();
    fs::write(&clap, "a plugin").unwrap();
    for bundle in [&vst3, &neighbour] {
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(bundle.join("Contents").join("library"), "a plugin").unwrap();
    }

    // One format only.
    let one = uninstall(&["--name", "Mangue Pluck", "--format", "vst3"]);
    assert_eq!(one.status.code(), Some(0), "{}", stderr(&one));
    assert_eq!(stdout(&one), format!("Removed {}\n", vst3.display()));
    assert!(!vst3.exists() && clap.exists() && neighbour.exists());

    // The rest.
    let rest = uninstall(&["--name", "Mangue Pluck"]);
    assert_eq!(rest.status.code(), Some(0), "{}", stderr(&rest));
    let said = stdout(&rest);
    assert!(
        said.contains(&format!("Removed {}\n", clap.display())),
        "{said}"
    );
    assert!(
        said.contains("Mangue Pluck.vst3 is not installed in "),
        "{said}"
    );
    assert!(!clap.exists());
    assert!(neighbour.join("Contents").join("library").is_file());

    // A name that would reach outside the plugin folder removes nothing.
    let escape = uninstall(&["--name", "../VST3/Mangue Duck"]);
    assert_eq!(escape.status.code(), Some(2));
    assert!(neighbour.exists());
}

#[test]
fn validate_says_so_when_there_is_nothing_to_validate_with() {
    if Os::host().is_none() {
        return;
    }
    let scratch = Scratch::new("cli-validate");
    // A PATH with nothing on it: no validator can be found, and the tool
    // has to notice before it needs cargo for anything.
    let empty = scratch.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let nothing = tool()
        .env("PATH", &empty)
        .args(["validate"])
        .output()
        .unwrap();
    assert_eq!(nothing.status.code(), Some(5), "{}", stderr(&nothing));
    let message = stderr(&nothing);
    assert!(message.starts_with("error: No validator was found, so nothing was checked."));
    for mention in [
        "clap-validator",
        "--clap-validator",
        "CLAP_VALIDATOR",
        "--vst3-validator",
        "VST3_VALIDATOR",
        "https://github.com/free-audio/clap-validator",
    ] {
        assert!(message.contains(mention), "no {mention} in: {message}");
    }
    // Asked for one format, told about that one.
    let clap_only = tool()
        .env("PATH", &empty)
        .args(["validate", "--format", "clap"])
        .output()
        .unwrap();
    assert_eq!(clap_only.status.code(), Some(5));
    assert!(!stderr(&clap_only).contains("VST3_VALIDATOR"));

    // A validator named and not there is a mistake to correct, by option
    // or by variable, and not a reason to go looking for another.
    let missing = scratch.path().join("no-such-validator");
    let by_option = tool()
        .env("PATH", &empty)
        .args(["validate", "--clap-validator"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(by_option.status.code(), Some(1), "{}", stderr(&by_option));
    assert!(stderr(&by_option).contains("--clap-validator names"));
    assert!(stderr(&by_option).contains("no-such-validator"));

    let by_variable = tool()
        .env("PATH", &empty)
        .env("VST3_VALIDATOR", &missing)
        .args(["validate"])
        .output()
        .unwrap();
    assert_eq!(
        by_variable.status.code(),
        Some(1),
        "{}",
        stderr(&by_variable)
    );
    assert!(stderr(&by_variable).contains("The VST3_VALIDATOR variable names"));
}

#[test]
fn a_folder_that_is_no_cargo_project_exits_with_1() {
    let scratch = Scratch::new("cli-no-project");
    let manifest = scratch.path().join("nowhere").join("Cargo.toml");
    for command in ["bundle", "install"] {
        let output = tool()
            .args([command, "--manifest-path"])
            .arg(&manifest)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert_eq!(stdout(&output), "");
        let message = stderr(&output);
        assert!(
            message.contains("error: cargo could not describe the workspace"),
            "{message}"
        );
        assert!(message.contains("--manifest-path"), "{message}");
    }
    // Without a name, uninstall needs the project too.
    let output = tool()
        .args(["uninstall", "--manifest-path"])
        .arg(&manifest)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}
