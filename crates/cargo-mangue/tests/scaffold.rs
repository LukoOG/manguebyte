//! The crates `cargo mangue new` writes.
//!
//! Three things together show that a generated crate builds and passes its
//! tests, without building one each time the tests run:
//!
//! 1. The templates are compiled as they are, and their tests run, in
//!    `template_effect.rs` and `template_instrument.rs`.
//! 2. A generated source file is the template with the names changed and
//!    nothing else: putting the placeholder names back gives the template,
//!    byte for byte.
//! 3. The names put in never clash with a name the template already uses,
//!    whatever the package is called.
//!
//! The last test does build one, with cargo, against this checkout. It is
//! slow and is run on request: `cargo test -p cargo-mangue -- --ignored`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_mangue::cargo::Cargo;
use cargo_mangue::layout::{BundleSpec, bundle_version, plan};
use cargo_mangue::platform::{Format, Target};
use cargo_mangue::scaffold::{
    DEFAULT_VENDOR, Dependency, Kind, NewPlugin, REPOSITORY, display_name, project, type_name,
};

const EFFECT: &str = include_str!("../templates/effect.rs");
const INSTRUMENT: &str = include_str!("../templates/instrument.rs");

fn template(kind: Kind) -> &'static str {
    match kind {
        Kind::Effect => EFFECT,
        Kind::Instrument => INSTRUMENT,
    }
}

/// How many tests a template carries, and so how many a crate generated
/// from it runs. Counted from the text, so that a test added to a template
/// is expected to pass in the generated crate too.
fn tests_in(kind: Kind) -> usize {
    template(kind)
        .lines()
        .filter(|line| line.trim() == "#[test]")
        .count()
}

/// The three files of a generated crate.
struct Generated {
    manifest: String,
    source: String,
    gitignore: String,
}

fn generate(plugin: &NewPlugin) -> Generated {
    let files = project(plugin).unwrap_or_else(|error| panic!("{}: {error}", plugin.package));
    let paths: Vec<&str> = files.iter().map(|file| file.path).collect();
    assert_eq!(paths, ["Cargo.toml", "src/lib.rs", ".gitignore"]);
    let mut contents = files.into_iter().map(|file| file.contents);
    Generated {
        manifest: contents.next().unwrap(),
        source: contents.next().unwrap(),
        gitignore: contents.next().unwrap(),
    }
}

/// Rust source with comments and the insides of string and character
/// literals replaced by spaces, so that what is left is code.
fn code_only(source: &str) -> String {
    let characters: Vec<char> = source.chars().collect();
    let mut code = String::with_capacity(source.len());
    let mut at = 0;
    while at < characters.len() {
        let character = characters[at];
        let next = characters.get(at + 1).copied();
        if character == '/' && next == Some('/') {
            while at < characters.len() && characters[at] != '\n' {
                code.push(' ');
                at += 1;
            }
        } else if character == '"' {
            code.push('"');
            at += 1;
            while at < characters.len() && characters[at] != '"' {
                if characters[at] == '\\' {
                    code.push(' ');
                    at += 1;
                }
                code.push(' ');
                at += 1;
            }
            code.push('"');
            at += 1;
        } else if character == '\'' && (next == Some('\\') || characters.get(at + 2) == Some(&'\''))
        {
            // A character literal; a lifetime has no closing quote. After
            // a backslash comes the character it escapes, which may be a
            // quote itself.
            let first = if next == Some('\\') { at + 3 } else { at + 2 };
            let end = (first..characters.len())
                .find(|&index| characters[index] == '\'')
                .unwrap();
            code.extend(std::iter::repeat_n(' ', end + 1 - at));
            at = end + 1;
        } else {
            code.push(character);
            at += 1;
        }
    }
    code
}

/// Whether every bracket in `code` is closed by its own kind, in order.
fn brackets_balance(code: &str) -> bool {
    let mut open = Vec::new();
    for character in code.chars() {
        let opener = match character {
            '(' | '[' | '{' => {
                open.push(character);
                continue;
            }
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => continue,
        };
        if open.pop() != Some(opener) {
            return false;
        }
    }
    open.is_empty()
}

/// How many times `word` stands in `code` as a whole identifier.
fn count_word(code: &str, word: &str) -> usize {
    code.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|found| *found == word)
        .count()
}

#[test]
fn the_checks_used_below_can_tell_good_source_from_bad() {
    assert!(brackets_balance(&code_only("fn f() { let x = [1, (2)]; }")));
    assert!(brackets_balance(&code_only("let s = \"}{)(\"; // }}}")));
    assert!(brackets_balance(&code_only(
        "let c = '{'; let d = '\\''; fn g<'a>(x: &'a str) {}"
    )));
    assert!(!brackets_balance(&code_only("fn f() { (}")));
    assert!(!brackets_balance(&code_only("fn f() {")));
    assert!(!brackets_balance(&code_only("fn f() }")));
    assert_eq!(code_only("a \"b\\\"c\" d // e"), "a \"    \" d     ");
    assert_eq!(
        count_word("Gain GainParams gain Gain<f32> (Gain)", "Gain"),
        3
    );
    // The templates themselves pass.
    for kind in Kind::ALL {
        assert!(brackets_balance(&code_only(template(kind))));
    }
}

#[test]
fn a_generated_crate_is_the_template_with_the_names_changed_and_nothing_else() {
    for kind in Kind::ALL {
        // Names that occur nowhere in the templates, so that putting the
        // placeholders back cannot touch anything else.
        let plugin = NewPlugin {
            package: "zq-xv".to_owned(),
            kind,
            name: "Zq Xv Nr".to_owned(),
            vendor: "Wj Kq Audio".to_owned(),
            id: "tld.wjkq.zq-xv".to_owned(),
            dependency: Dependency::Git(REPOSITORY.to_owned()),
        };
        for unique in ["ZqXv", "Zq Xv Nr", "Wj Kq", "tld.wjkq"] {
            assert!(!template(kind).contains(unique));
        }
        let generated = generate(&plugin);
        assert_ne!(generated.source, template(kind));

        let described = match kind {
            Kind::Effect => "\"Zq Xv Nr: an audio effect built with Manguebyte\"",
            Kind::Instrument => "\"Zq Xv Nr: an instrument built with Manguebyte\"",
        };
        assert_eq!(generated.source.matches(described).count(), 1);
        let restored = generated
            .source
            .replace(described, "\"Template description\"")
            .replace("\"tld.wjkq.zq-xv\"", "\"com.example.plugin-template\"")
            .replace("\"Wj Kq Audio\"", "\"Template Vendor\"")
            .replace("Zq Xv Nr", "Plugin Template")
            .replace("ZqXv", "PluginTemplate");
        assert!(
            restored == template(kind),
            "{} differs from its template",
            kind.name()
        );
    }
}

#[test]
fn nothing_of_the_template_is_left_in_a_generated_crate() {
    for kind in Kind::ALL {
        let generated = generate(&NewPlugin::new("my-gain", kind).unwrap());
        for leftover in [
            "PluginTemplate",
            "Plugin Template",
            "plugin-template",
            "Template Vendor",
            "Template description",
            "Template",
        ] {
            assert!(
                !generated.source.contains(leftover),
                "{leftover} in the source"
            );
            assert!(
                !generated.manifest.contains(leftover),
                "{leftover} in the manifest"
            );
        }
        assert_eq!(generated.gitignore, "/target\n");
        // What a plugin needs is all there.
        let kind_line = match kind {
            Kind::Effect => "kind: PluginKind::Effect,",
            Kind::Instrument => "kind: PluginKind::Instrument,",
        };
        for needed in [
            "use manguebyte::prelude::*;",
            "pub struct MyGainParams {",
            "param_set!(MyGainParams {",
            "pub struct MyGain {",
            "impl Plugin for MyGain {",
            "type Params = MyGainParams;",
            "id: \"com.example.my-gain\",",
            "name: \"My Gain\",",
            "vendor: \"Example Audio\",",
            "version: env!(\"CARGO_PKG_VERSION\"),",
            kind_line,
            "fn process(&mut self, buffer: &mut AudioBuffer<f32>, context: &ProcessContext<'_, f32>)",
            "manguebyte::clap::export_clap!(MyGain);",
            "manguebyte::vst3::export_vst3!(MyGain);",
            "manguebyte::lv2::export_lv2!(MyGain);",
            "#[cfg(test)]",
            "Harness::<MyGain>::new(RATE, 256)",
            "fn sounds_the_same_whatever_the_block_size()",
            "fn state_restores_every_parameter()",
        ] {
            assert!(
                generated.source.contains(needed),
                "no {needed:?} for {}",
                kind.name()
            );
        }
        assert!(generated.source.starts_with("//! My Gain: an "));
        assert!(generated.source.ends_with("}\n"));
        assert!(generated.source.matches("#[test]").count() >= 5);
    }
    let instrument = generate(&NewPlugin::new("my-gain", Kind::Instrument).unwrap());
    assert!(instrument.source.contains("impl Voice for MyGainVoice {"));
    assert!(instrument.source.contains("synth: Synth<MyGainVoice>,"));
}

#[test]
fn the_manifest_is_a_plugin_crate_that_depends_on_manguebyte() {
    let mut plugin = NewPlugin::new("my-gain", Kind::Effect).unwrap();
    assert_eq!(plugin.name, "My Gain");
    assert_eq!(plugin.vendor, DEFAULT_VENDOR);
    assert_eq!(plugin.id, "com.example.my-gain");
    assert_eq!(plugin.dependency, Dependency::Git(REPOSITORY.to_owned()));
    assert!(REPOSITORY.starts_with("https://"));

    let expected = format!(
        r#"[package]
name = "my-gain"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
description = "My Gain: an audio effect built with Manguebyte"

[lib]
# A host loads the cdylib. The rlib lets tests, examples and other crates
# use the plugin as an ordinary library.
crate-type = ["cdylib", "rlib"]

[dependencies]
manguebyte = {{ git = "{REPOSITORY}", default-features = false, features = ["clap", "vst3", "lv2"] }}

# What `cargo mangue bundle`, `install` and `validate` call the plugin.
[package.metadata.manguebyte]
name = "My Gain"
id = "com.example.my-gain"
"#
    );
    assert_eq!(generate(&plugin).manifest, expected);

    // Against a checkout, with everything else chosen too.
    plugin.kind = Kind::Instrument;
    plugin.name = "Órgão do Zé".to_owned();
    plugin.id = "br.com.ze.orgao".to_owned();
    plugin.dependency = Dependency::Path("../manguebyte/crates/manguebyte".to_owned());
    let manifest = generate(&plugin).manifest;
    assert!(manifest.contains(
        "manguebyte = { path = \"../manguebyte/crates/manguebyte\", default-features = false, \
         features = [\"clap\", \"vst3\", \"lv2\"] }\n"
    ));
    assert!(
        manifest.contains("description = \"Órgão do Zé: an instrument built with Manguebyte\"\n")
    );
    assert!(manifest.contains(
        "[package.metadata.manguebyte]\nname = \"Órgão do Zé\"\nid = \"br.com.ze.orgao\"\n"
    ));
    assert!(!manifest.contains("git = "));

    // A path with backslashes, as Windows writes them, stays valid TOML.
    plugin.dependency = Dependency::Path(r"C:\src\manguebyte".to_owned());
    assert!(
        generate(&plugin)
            .manifest
            .contains(r#"path = "C:\\src\\manguebyte","#)
    );
}

#[test]
fn whatever_the_package_is_called_the_source_stays_sound() {
    // Names chosen to collide with what the templates use, with the
    // standard library, and with each other's suffixes.
    let packages = [
        "my-gain",
        "a",
        "x_y-z",
        "FM8",
        "gain",
        "synth",
        "voice",
        "plugin",
        "harness",
        "adsr",
        "oscillator",
        "audio-buffer",
        "float-param",
        "param-smoother",
        "arc",
        "option",
        "some",
        "none",
        "vec",
        "string",
        "result",
        "event",
        "event-kind",
        "plugin-info",
        "plugin-kind",
        "audio-layout",
        "process-spec",
        "process-context",
        "waveform",
        "note-ref",
        "param-id",
        "smoothing",
        "r-a-t-e",
        "g-a-i-n",
        "voices",
        "glide-ms",
        "plugin-template",
        "template",
        "complex",
        "real-fft",
        "range",
        "default",
    ];
    let mut renamed = 0;
    for kind in Kind::ALL {
        let template_code = code_only(template(kind));
        for package in packages {
            let generated = generate(&NewPlugin::new(package, kind).unwrap());
            let code = code_only(&generated.source);
            assert!(brackets_balance(&code), "{package} as {}", kind.name());

            // Each type the plugin defines stands exactly where its
            // placeholder stood: it hides nothing the template uses, and
            // nothing the template uses is mistaken for it.
            let name = type_name(package, kind);
            assert!(name.starts_with(|c: char| c.is_ascii_uppercase()), "{name}");
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric()), "{name}");
            for suffix in ["", "Params", "Voice"] {
                assert_eq!(
                    count_word(&code, &format!("{name}{suffix}")),
                    count_word(&template_code, &format!("PluginTemplate{suffix}")),
                    "{name}{suffix} for {package} as {}",
                    kind.name()
                );
            }
            if name != "PluginTemplate" {
                assert_eq!(count_word(&code, "PluginTemplate"), 0, "{package}");
            }
            if name.ends_with("Plugin") && !package.ends_with("plugin") {
                renamed += 1;
            }
            // The name people see is not touched by any of that.
            assert!(
                generated
                    .source
                    .contains(&format!("name: \"{}\",", display_name(package)))
            );
        }
    }
    // The collisions were real: many of these names had to be changed.
    assert!(renamed >= 30, "only {renamed} names were changed");
}

#[test]
fn text_from_the_command_line_cannot_break_out_of_its_string() {
    let mut plugin = NewPlugin::new("my-gain", Kind::Effect).unwrap();
    plugin.vendor = "Ana \"Mangue\" \\ Lima; } fn main() {".to_owned();
    plugin.name = "Gain }} {0} 'n' Roll".to_owned();
    let generated = generate(&plugin);
    assert!(
        generated
            .source
            .contains(r#"vendor: "Ana \"Mangue\" \\ Lima; } fn main() {","#)
    );
    assert!(
        generated
            .source
            .contains(r#"name: "Gain }} {0} 'n' Roll","#)
    );
    // Still the same code around those strings.
    assert!(brackets_balance(&code_only(&generated.source)));
    assert_eq!(
        count_word(&code_only(&generated.source), "main"),
        0,
        "text from a string became code"
    );
    assert!(
        generated
            .manifest
            .contains("name = \"Gain }} {0} 'n' Roll\"\n")
    );

    // A name that is itself a placeholder is put in once and left alone.
    plugin.vendor = "Template Vendor \"Plugin Template\" PluginTemplate".to_owned();
    plugin.name = "Template description".to_owned();
    let generated = generate(&plugin);
    assert!(
        generated
            .source
            .contains(r#"vendor: "Template Vendor \"Plugin Template\" PluginTemplate","#)
    );
    assert!(generated.source.contains("name: \"Template description\","));
    assert!(generated.source.contains("pub struct MyGain {"));
}

#[test]
fn what_cannot_be_a_plugin_is_refused_with_a_usage_error() {
    let good = NewPlugin::new("my-gain", Kind::Effect).unwrap();
    assert!(project(&good).is_ok());

    for package in [
        "",
        "2fast",
        "my gain",
        "fn",
        "std",
        "manguebyte",
        "a/b",
        "nul",
    ] {
        let error = NewPlugin::new(package, Kind::Effect).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{package:?}");
        assert!(
            error.message().contains("cannot be the name of a package"),
            "{error}"
        );
        let mut plugin = good.clone();
        plugin.package = package.to_owned();
        assert_eq!(project(&plugin).unwrap_err().exit_code(), 2, "{package:?}");
    }
    for name in [
        "",
        "a/b",
        "a\\b",
        "what?",
        "\"quoted\"",
        " padded ",
        "line\nbreak",
    ] {
        let mut plugin = good.clone();
        plugin.name = name.to_owned();
        let error = project(&plugin).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{name:?}");
        assert!(error.message().contains("--name"), "{error}");
    }
    for vendor in ["", "   ", " padded", "line\nbreak", "tab\there"] {
        let mut plugin = good.clone();
        plugin.vendor = vendor.to_owned();
        let error = project(&plugin).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{vendor:?}");
        assert!(error.message().contains("--vendor"), "{error}");
    }
    for id in [
        "",
        "gain",
        "com..gain",
        ".com.gain",
        "com.my gain",
        "com.my_gain",
        "com/gain",
    ] {
        let mut plugin = good.clone();
        plugin.id = id.to_owned();
        assert_eq!(project(&plugin).unwrap_err().exit_code(), 2, "{id:?}");
    }
}

fn run(command: &mut Command, what: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{what} could not be started: {error}"));
    assert!(
        output.status.success(),
        "{what} failed with {}\n--- stdout\n{}\n--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Generates an effect and an instrument against this checkout with the
/// built tool, runs the tests each one carries, checks that each is
/// formatted as `cargo fmt` would leave it, and bundles each.
///
/// The crates are created outside the repository, in the system's
/// temporary folder, because cargo refuses a package inside a workspace
/// that does not list it. They share one target folder: the one in
/// `CARGO_TARGET_DIR` if that is set, which lets a run from this checkout
/// reuse what is already built, and otherwise one beside them.
#[test]
#[ignore = "builds two plugins with cargo; run with: cargo test -p cargo-mangue -- --ignored"]
fn a_generated_plugin_builds_passes_its_tests_and_bundles() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let tool = env!("CARGO_BIN_EXE_cargo-mangue");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let work = env::temp_dir().join(format!("cargo-mangue-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| work.join("target"));

    let triple = Cargo::from_env().default_target().unwrap();
    let host = Target::from_triple(&triple).unwrap();
    let has_rustfmt = Command::new(&cargo)
        .args(["fmt", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());

    // Found when this was run after the instrument gained a test of its own
    // (a change of level on its frame): the count of 5 written here no
    // longer matched the 6 the generated crate runs, so it is counted.
    for (package, kind, name) in [
        ("e2e-gain", Kind::Effect, "E2e Gain"),
        ("e2e-synth", Kind::Instrument, "E2e Synth"),
    ] {
        let tests = tests_in(kind);
        // As many as when this test was written, at least; and never none,
        // which the doc-tests' empty report would answer.
        assert!(tests >= 5, "the {} template has {tests} tests", kind.name());
        let kind = kind.name();
        run(
            Command::new(tool)
                .current_dir(&work)
                .args(["new", package, "--kind", kind, "--manguebyte-path"])
                .arg(&checkout),
            "cargo mangue new",
        );
        let project = work.join(package);
        // The versions of the dependencies this checkout is tested with.
        if let Ok(lock) = fs::read(checkout.join("Cargo.lock")) {
            fs::write(project.join("Cargo.lock"), lock).unwrap();
        }
        let in_project = |program: &std::ffi::OsStr| {
            let mut command = Command::new(program);
            command
                .current_dir(&project)
                .env("CARGO_TARGET_DIR", &target_dir);
            command
        };

        let tested = run(in_project(&cargo).arg("test"), "cargo test");
        let report = String::from_utf8_lossy(&tested.stdout).into_owned();
        assert!(
            report.contains(&format!("test result: ok. {tests} passed; 0 failed")),
            "{report}"
        );

        if has_rustfmt {
            run(
                in_project(&cargo).args(["fmt", "--check"]),
                "cargo fmt --check",
            );
        } else {
            eprintln!("rustfmt is not installed; the formatting of {package} was not checked");
        }

        let out = work.join("bundled");
        let bundled = run(
            in_project(tool.as_ref())
                .args(["bundle", "--out"])
                .arg(&out),
            "cargo mangue bundle",
        );
        let said = String::from_utf8_lossy(&bundled.stdout).into_owned();
        for format in Format::DEFAULT {
            let plan = plan(&BundleSpec {
                format,
                os: host.os,
                archs: vec![host.arch],
                name: name.to_owned(),
                identifier: format!("com.example.{package}"),
                version: bundle_version("0.1.0"),
            })
            .unwrap();
            for file in &plan.files {
                let path = out.join(&file.path);
                let size = fs::metadata(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
                    .len();
                assert!(size > 0, "{} is empty", path.display());
            }
            assert!(said.contains(&plan.root), "{said}");
        }

        // The LV2 bundle: the library, and the Turtle files that the plugin's
        // own code writes, naming it.
        let bundled = run(
            in_project(tool.as_ref())
                .args(["bundle", "--format", "lv2", "--out"])
                .arg(&out),
            "cargo mangue bundle --format lv2",
        );
        let said = String::from_utf8_lossy(&bundled.stdout).into_owned();
        let lv2 = plan(&BundleSpec {
            format: Format::Lv2,
            os: host.os,
            archs: vec![host.arch],
            name: name.to_owned(),
            identifier: format!("com.example.{package}"),
            version: bundle_version("0.1.0"),
        })
        .unwrap();
        assert!(said.contains(&lv2.root), "{said}");
        let folder = out.join(&lv2.root);
        let library = lv2.library_file_name().unwrap();
        assert!(fs::metadata(folder.join(library)).unwrap().len() > 0);
        let manifest = fs::read_to_string(folder.join("manifest.ttl")).unwrap();
        let encoded = library.replace(' ', "%20");
        assert!(
            manifest.contains(&format!("lv2:binary <{encoded}> ;")),
            "{manifest}"
        );
        assert!(
            manifest.contains(&format!("<urn:manguebyte:com.example.{package}>")),
            "{manifest}"
        );
        let described =
            fs::read_to_string(folder.join(format!("com.example.{package}.ttl"))).unwrap();
        assert!(described.contains("lv2:port"), "{described}");
    }
    fs::remove_dir_all(&work).unwrap();
}
