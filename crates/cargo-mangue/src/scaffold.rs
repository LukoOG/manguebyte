//! Writing a new plugin crate.
//!
//! `cargo mangue new` produces a crate that builds, passes its own tests
//! and exports a CLAP and a VST 3 plugin without a line being changed. The
//! source comes from two templates embedded in the program, one for an
//! effect and one for an instrument, modelled on the example plugins
//! `mangue-duck` and `mangue-pluck` and cut down to the least that is still
//! a complete plugin.
//!
//! A template is itself valid Rust, written for a plugin called "Plugin
//! Template". That is deliberate: this crate's tests compile the templates
//! as they are and run the tests inside them, so a template cannot fall
//! behind the framework unnoticed. [`project`] then swaps the placeholder
//! names for the real ones, in a single pass that never looks again at text
//! it has already put in.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::error::Error;
use crate::layout::{check_identifier, check_plugin_name};

/// The effect template.
const EFFECT: &str = include_str!("../templates/effect.rs");
/// The instrument template.
const INSTRUMENT: &str = include_str!("../templates/instrument.rs");

/// The name the templates give their plugin type. `Params` and `Voice` are
/// appended to it for the other types.
const TYPE_PLACEHOLDER: &str = "PluginTemplate";
/// The plugin name in the templates, as it appears in documentation.
const NAME_PLACEHOLDER: &str = "Plugin Template";
/// The string literals of the templates that are replaced whole.
const NAME_LITERAL: &str = "\"Plugin Template\"";
const ID_LITERAL: &str = "\"com.example.plugin-template\"";
const VENDOR_LITERAL: &str = "\"Template Vendor\"";
const DESCRIPTION_LITERAL: &str = "\"Template description\"";

/// Where the Manguebyte source lives. A new plugin depends on it from here
/// unless it is pointed at a checkout.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The vendor a new plugin gets when none is given.
pub const DEFAULT_VENDOR: &str = "Example Audio";

/// Longest package name accepted, the limit crates.io sets.
const MAX_PACKAGE_NAME: usize = 64;

/// What sort of plugin to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Changes sound: audio in, audio out.
    Effect,
    /// Makes sound from notes.
    Instrument,
}

impl Kind {
    /// Both kinds.
    pub const ALL: [Self; 2] = [Self::Effect, Self::Instrument];

    /// Reads a kind as written on the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "effect" => Some(Self::Effect),
            "instrument" => Some(Self::Instrument),
            _ => None,
        }
    }

    /// The word for it: `effect` or `instrument`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Effect => "effect",
            Self::Instrument => "instrument",
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Effect => EFFECT,
            Self::Instrument => INSTRUMENT,
        }
    }

    /// The kind as it reads in a sentence: `an audio effect` or `an
    /// instrument`.
    pub fn described(self) -> &'static str {
        match self {
            Self::Effect => "an audio effect",
            Self::Instrument => "an instrument",
        }
    }
}

/// Where a new plugin gets Manguebyte from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// A git repository, by URL.
    Git(String),
    /// A folder holding the `manguebyte` crate, as written in `Cargo.toml`:
    /// absolute, or relative to the new crate.
    Path(String),
}

/// Everything that distinguishes one new plugin from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlugin {
    /// The package name, such as `my-gain`. It is also the folder.
    pub package: String,
    /// Effect or instrument.
    pub kind: Kind,
    /// The name shown to people, such as `My Gain`.
    pub name: String,
    /// Who makes it.
    pub vendor: String,
    /// The plugin's identifier in reverse-domain form, such as
    /// `com.example.my-gain`. Hosts store it in sessions.
    pub id: String,
    /// Where Manguebyte comes from.
    pub dependency: Dependency,
}

impl NewPlugin {
    /// A plugin with everything derived from its package name: `my-gain`
    /// becomes "My Gain" by "Example Audio" with the id
    /// `com.example.my-gain`, depending on Manguebyte's repository.
    ///
    /// # Errors
    ///
    /// Fails if the package name is not usable: see
    /// [`check_package_name`].
    pub fn new(package: &str, kind: Kind) -> Result<Self, Error> {
        check_package_name(package)?;
        Ok(Self {
            package: package.to_owned(),
            kind,
            name: display_name(package),
            vendor: DEFAULT_VENDOR.to_owned(),
            id: default_id(package),
            dependency: Dependency::Git(REPOSITORY.to_owned()),
        })
    }
}

/// One file of a new plugin crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    /// Its path inside the crate, with `/` between the parts.
    pub path: &'static str,
    /// Its text.
    pub contents: String,
}

/// Produces the files of a new plugin crate: `Cargo.toml`, `src/lib.rs`
/// and `.gitignore`.
///
/// ```
/// use cargo_mangue::scaffold::{Kind, NewPlugin, project};
///
/// let files = project(&NewPlugin::new("my-gain", Kind::Effect)?)?;
/// let paths: Vec<&str> = files.iter().map(|file| file.path).collect();
/// assert_eq!(paths, ["Cargo.toml", "src/lib.rs", ".gitignore"]);
///
/// let source = &files[1].contents;
/// assert!(source.contains("pub struct MyGain {"));
/// assert!(source.contains(r#"id: "com.example.my-gain","#));
/// assert!(source.contains("manguebyte::clap::export_clap!(MyGain);"));
/// # Ok::<(), cargo_mangue::Error>(())
/// ```
///
/// # Errors
///
/// Fails if the package name, the plugin name, the vendor or the id is not
/// usable. The message says what is.
pub fn project(plugin: &NewPlugin) -> Result<Vec<ProjectFile>, Error> {
    check_package_name(&plugin.package)?;
    check_plugin_name(&plugin.name)?;
    check_vendor(&plugin.vendor)?;
    check_id(&plugin.id)?;

    let description = format!(
        "{}: {} built with Manguebyte",
        plugin.name,
        plugin.kind.described()
    );
    let type_name = type_name(&plugin.package, plugin.kind);
    let name_literal = rust_string(&plugin.name);
    let id_literal = rust_string(&plugin.id);
    let vendor_literal = rust_string(&plugin.vendor);
    let description_literal = rust_string(&description);
    let source = replace_tokens(
        plugin.kind.template(),
        &[
            (NAME_LITERAL, &name_literal),
            (ID_LITERAL, &id_literal),
            (VENDOR_LITERAL, &vendor_literal),
            (DESCRIPTION_LITERAL, &description_literal),
            (TYPE_PLACEHOLDER, &type_name),
            (NAME_PLACEHOLDER, &plugin.name),
        ],
    );

    Ok(vec![
        ProjectFile {
            path: "Cargo.toml",
            contents: manifest(plugin, &description),
        },
        ProjectFile {
            path: "src/lib.rs",
            contents: source,
        },
        ProjectFile {
            path: ".gitignore",
            contents: "/target\n".to_owned(),
        },
    ])
}

/// The text of the new crate's `Cargo.toml`.
fn manifest(plugin: &NewPlugin, description: &str) -> String {
    let source = match &plugin.dependency {
        Dependency::Git(url) => format!("git = {}", toml_string(url)),
        Dependency::Path(path) => format!("path = {}", toml_string(path)),
    };
    format!(
        "[package]\n\
         name = {package}\n\
         version = \"0.1.0\"\n\
         edition = \"2024\"\n\
         rust-version = \"1.85\"\n\
         description = {description}\n\
         \n\
         [lib]\n\
         # A host loads the cdylib. The rlib lets tests, examples and other crates\n\
         # use the plugin as an ordinary library.\n\
         crate-type = [\"cdylib\", \"rlib\"]\n\
         \n\
         [dependencies]\n\
         manguebyte = {{ {source}, default-features = false, features = [\"clap\", \"vst3\", \"lv2\"] }}\n\
         \n\
         # What `cargo mangue bundle`, `install` and `validate` call the plugin.\n\
         [package.metadata.manguebyte]\n\
         name = {name}\n\
         id = {id}\n",
        package = toml_string(&plugin.package),
        description = toml_string(description),
        name = toml_string(&plugin.name),
        id = toml_string(&plugin.id),
    )
}

/// Replaces every occurrence of each token, reading the template once from
/// start to end. Text that has been put in is never examined again, so a
/// replacement that happens to contain a token stays as it is.
fn replace_tokens(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(first) = rest.chars().next() {
        let matched = replacements
            .iter()
            .find(|(token, _)| !token.is_empty() && rest.starts_with(token));
        match matched {
            Some((token, replacement)) => {
                output.push_str(replacement);
                rest = &rest[token.len()..];
            }
            None => {
                output.push(first);
                rest = &rest[first.len_utf8()..];
            }
        }
    }
    output
}

/// Writes `text` as a Rust string literal, quotes included.
fn rust_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            control if control.is_control() => {
                literal.push_str(&format!("\\u{{{:x}}}", u32::from(control)));
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

/// Writes `text` as a TOML basic string, quotes included (TOML 1.0,
/// "String").
fn toml_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            control if control.is_control() => {
                literal.push_str(&format!("\\u{:04X}", u32::from(control)));
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

/// The name people see for a package: `my-gain` becomes `My Gain`.
pub fn display_name(package: &str) -> String {
    let words: Vec<String> = words(package).map(capitalised).collect();
    words.join(" ")
}

/// The identifier a package gets when none is given: `com.example.`
/// followed by the package name, with underscores as hyphens.
pub fn default_id(package: &str) -> String {
    format!("com.example.{}", package.replace('_', "-"))
}

/// The name of the plugin's Rust type: `my-gain` becomes `MyGain`.
///
/// The templates bring the framework's names into scope, and a plugin
/// called `synth` or `harness` would get a type that hides the framework's
/// own `Synth` or `Harness` from the code that uses it. When the name, or
/// the name with `Params` or `Voice` after it, is one the template for
/// `kind` already uses, or one every Rust module starts with, `Plugin` is
/// appended: an instrument called `synth` gets the type `SynthPlugin`.
pub fn type_name(package: &str, kind: Kind) -> String {
    let mut name: String = words(package).map(capitalised).collect();
    let template = kind.template();
    let used: Vec<&str> = identifiers(template).collect();
    // Only the types the template really has can clash.
    let suffixes: Vec<&str> = ["", "Params", "Voice"]
        .into_iter()
        .filter(|suffix| template.contains(&format!("{TYPE_PLACEHOLDER}{suffix}")))
        .collect();
    let taken = |candidate: &str| {
        STD_PRELUDE.contains(&candidate)
            || suffixes
                .iter()
                .any(|suffix| used.contains(&format!("{candidate}{suffix}").as_str()))
    };
    // Appending once is enough for every name the framework has today;
    // the loop is for the ones it has not got yet.
    for _ in 0..4 {
        if !taken(&name) {
            break;
        }
        name.push_str("Plugin");
    }
    name
}

/// The parts of a package name between hyphens and underscores.
fn words(package: &str) -> impl Iterator<Item = &str> {
    package.split(['-', '_']).filter(|word| !word.is_empty())
}

/// A word with its first letter in upper case.
fn capitalised(word: &str) -> String {
    let mut letters = word.chars();
    match letters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + letters.as_str(),
        None => String::new(),
    }
}

/// The identifiers a template's code uses, without the placeholder types.
///
/// Comments and the insides of string literals are left out. The templates
/// keep to what makes that simple: no `//` and no escaped quote inside a
/// string, and no string that runs over a line.
fn identifiers(template: &str) -> impl Iterator<Item = &str> {
    template
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        // Between quotes is every second piece.
        .flat_map(|code| code.split('"').step_by(2))
        .flat_map(|code| code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')))
        .filter(|word| {
            !word.is_empty()
                && !word.starts_with(|c: char| c.is_ascii_digit())
                && !word.starts_with(TYPE_PLACEHOLDER)
        })
}

/// The names every Rust module starts with (the standard library's
/// `std::prelude`). The framework's macros may use any of them, so a
/// plugin type must not take one over.
const STD_PRELUDE: &[&str] = &[
    "AsMut",
    "AsRef",
    "Box",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "Err",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
];

/// The words Rust keeps for itself (The Rust Reference, "Keywords"), which
/// cargo refuses as package names.
const KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if",
    "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Names that would clash with a crate the new one is built against.
const TAKEN_CRATES: &[&str] = &[
    "alloc",
    "core",
    "manguebyte",
    "proc_macro",
    "proc-macro",
    "std",
    "test",
];

/// Checks that a name can be the name of a package and of its folder.
///
/// # Errors
///
/// Fails unless the name starts with a letter and continues with letters,
/// digits, hyphens and underscores, all ASCII, up to 64 characters. Fails
/// too for a Rust keyword, for the name of a crate every plugin is built
/// against (`std`, `core`, `alloc`, `test`, `proc_macro`, `manguebyte`),
/// and for a name Windows reserves for a device.
pub fn check_package_name(name: &str) -> Result<(), Error> {
    let starts_well = name.starts_with(|c: char| c.is_ascii_alphabetic());
    let continues_well = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let problem = if name.is_empty() {
        Some("it is empty")
    } else if !starts_well {
        Some("it does not start with a letter")
    } else if !continues_well {
        Some("it has a character other than a letter, a digit, a hyphen or an underscore")
    } else if name.len() > MAX_PACKAGE_NAME {
        Some("it is longer than 64 characters")
    } else if KEYWORDS.contains(&name) {
        Some("it is a Rust keyword")
    } else if TAKEN_CRATES.contains(&name) {
        Some("a crate of that name is already part of every plugin")
    } else if check_plugin_name(name).is_err() {
        Some("Windows reserves it for a device")
    } else {
        None
    };
    match problem {
        None => Ok(()),
        Some(problem) => Err(Error::usage(format!(
            "\"{}\" cannot be the name of a package: {problem}. Use a name such as my-gain; \
             the name people see is set separately, with --name.",
            name.escape_debug()
        ))),
    }
}

fn check_vendor(vendor: &str) -> Result<(), Error> {
    if vendor.trim().is_empty() || vendor.chars().any(char::is_control) || vendor != vendor.trim() {
        return Err(Error::usage(format!(
            "The vendor \"{}\" is not usable: give the name of who makes the plugin, as plain \
             text, with --vendor.",
            vendor.escape_debug()
        )));
    }
    Ok(())
}

fn check_id(id: &str) -> Result<(), Error> {
    check_identifier(id)?;
    if !id.contains('.') {
        return Err(Error::usage(format!(
            "The id \"{id}\" is not in reverse-domain form. Start it with a domain you \
             control, such as com.example.my-plugin, with --id."
        )));
    }
    Ok(())
}

/// Works out what to write in `Cargo.toml` for `--manguebyte-path`.
///
/// `given` is a checkout of the Manguebyte repository or the folder of its
/// `manguebyte` crate, and `current_dir` is where the new crate's folder is
/// being created. A relative path stays relative, adjusted to be read from
/// inside the new crate; an absolute one stays absolute. Either way it is
/// written with `/`, which cargo accepts on every system. The exception is
/// a Windows path in its `\\?\` form, in which `/` is not a separator: that
/// one keeps its backslashes.
///
/// # Errors
///
/// Fails if neither `given/crates/manguebyte/Cargo.toml` nor
/// `given/Cargo.toml` exists.
pub fn resolve_manguebyte_path(given: &Path, current_dir: &Path) -> Result<String, Error> {
    let absolute = current_dir.join(given);
    let inside = Path::new("crates").join("manguebyte");
    let crate_dir = if absolute.join(&inside).join("Cargo.toml").is_file() {
        given.join(&inside)
    } else if absolute.join("Cargo.toml").is_file() {
        given.to_path_buf()
    } else {
        return Err(Error::usage(format!(
            "There is no Manguebyte in {}: --manguebyte-path takes a checkout of {REPOSITORY} \
             or its crates/manguebyte folder.",
            absolute.display()
        )));
    };
    // The new crate is one folder below the current one.
    let from_crate = if crate_dir.is_absolute() {
        crate_dir
    } else {
        Path::new("..").join(crate_dir)
    };
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    let mut separator = "/";
    for component in from_crate.components() {
        match component {
            Component::Prefix(drive) => {
                if drive.kind().is_verbatim() {
                    separator = "\\";
                }
                prefix = drive.as_os_str().to_string_lossy().into_owned();
            }
            Component::RootDir => prefix.push_str(separator),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_owned()),
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
        }
    }
    Ok(format!("{prefix}{}", parts.join(separator)))
}

/// Writes the files of a new crate into `dir`.
///
/// # Errors
///
/// Fails if `dir` exists and is not an empty folder, so that nothing is
/// ever overwritten, and if a file cannot be written.
pub fn write_project(dir: &Path, files: &[ProjectFile]) -> Result<(), Error> {
    let cannot = |action: &str, path: &Path, error: &io::Error| {
        Error::failed(format!(
            "Could not {action} {}: {error}. Check that the folder can be written to.",
            path.display()
        ))
    };
    match fs::read_dir(dir) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(Error::failed(format!(
                    "The folder {} already exists and is not empty. Choose another name, or \
                     remove it first.",
                    dir.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Error::failed(format!(
                "{} is in the way and is not a folder that can be used ({error}). Choose \
                 another name, or remove it first.",
                dir.display()
            )));
        }
    }
    for file in files {
        let path = file
            .path
            .split('/')
            .fold(dir.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| cannot("create", parent, &error))?;
        }
        let mut created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| cannot("create", &path, &error))?;
        created
            .write_all(file.contents.as_bytes())
            .map_err(|error| cannot("write", &path, &error))?;
    }
    Ok(())
}

/// Most bytes of a `Cargo.toml` examined by [`enclosing_workspace`].
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// Looks for a Cargo workspace that `dir` would be inside of, and returns
/// its `Cargo.toml`.
///
/// Cargo refuses to build a package that sits inside a workspace without
/// being one of its members, so `cargo mangue new` tells the person when
/// that is about to happen. The test is textual: a `Cargo.toml` in a parent
/// folder with a line that starts a `[workspace` table.
pub fn enclosing_workspace(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().skip(1).find_map(|folder| {
        let manifest = folder.join("Cargo.toml");
        let mut text = String::new();
        fs::File::open(&manifest)
            .ok()?
            .take(MAX_MANIFEST_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        text.lines()
            .any(|line| line.trim_start().starts_with("[workspace"))
            .then_some(manifest)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_derived_from_the_package() {
        assert_eq!(display_name("my-gain"), "My Gain");
        assert_eq!(display_name("fm_synth2"), "Fm Synth2");
        assert_eq!(display_name("a--b__c"), "A B C");
        assert_eq!(display_name("reverb"), "Reverb");
        assert_eq!(default_id("my_gain"), "com.example.my-gain");
        assert_eq!(type_name("my-gain", Kind::Effect), "MyGain");
        assert_eq!(type_name("fm_synth2", Kind::Instrument), "FmSynth2");
        assert_eq!(type_name("xLFO", Kind::Effect), "XLFO");
    }

    #[test]
    fn a_type_that_would_hide_one_the_template_uses_is_renamed() {
        // The effect template uses neither `Gain` nor `Synth` nor `Adsr`.
        assert_eq!(type_name("gain", Kind::Effect), "Gain");
        assert_eq!(type_name("plugin", Kind::Effect), "PluginPlugin");
        assert_eq!(type_name("harness", Kind::Effect), "HarnessPlugin");
        assert_eq!(type_name("float", Kind::Effect), "Float");
        assert_eq!(type_name("float-param", Kind::Effect), "FloatParamPlugin");
        assert_eq!(type_name("r-a-t-e", Kind::Effect), "RATEPlugin");
        assert_eq!(type_name("arc", Kind::Effect), "ArcPlugin");
        // A name every module starts with is taken whatever the template.
        assert_eq!(type_name("option", Kind::Effect), "OptionPlugin");
        assert_eq!(type_name("some", Kind::Instrument), "SomePlugin");
        // The instrument template uses `Synth`, `Voice`, `Adsr` and
        // `AdsrParams`, the last of which `adsr` would produce.
        assert_eq!(type_name("synth", Kind::Instrument), "SynthPlugin");
        assert_eq!(type_name("voice", Kind::Instrument), "VoicePlugin");
        assert_eq!(type_name("adsr", Kind::Instrument), "AdsrPlugin");
        assert_eq!(
            type_name("oscillator", Kind::Instrument),
            "OscillatorPlugin"
        );
        assert_eq!(type_name("pluck", Kind::Instrument), "Pluck");
    }

    #[test]
    fn package_names_follow_cargo_and_avoid_what_cannot_build() {
        for good in ["a", "my-gain", "my_gain", "Gain2", "x-", "gain", "selfish"] {
            check_package_name(good).unwrap_or_else(|error| panic!("{good}: {error}"));
        }
        check_package_name(&"x".repeat(64)).unwrap();
        for bad in [
            "",
            "2gain",
            "-gain",
            "_gain",
            "my gain",
            "my.gain",
            "órgão",
            "a/b",
            "..",
            "self",
            "Self",
            "fn",
            "gen",
            "test",
            "std",
            "core",
            "manguebyte",
            "con",
            "nul",
            "com1",
        ] {
            let error = check_package_name(bad).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{bad:?}");
        }
        assert!(check_package_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn text_is_written_as_literals_that_read_back_the_same() {
        assert_eq!(rust_string("plain"), "\"plain\"");
        assert_eq!(rust_string("a \"b\" \\ c"), r#""a \"b\" \\ c""#);
        assert_eq!(rust_string("tab\there"), "\"tab\\u{9}here\"");
        assert_eq!(rust_string("Órgão"), "\"Órgão\"");
        assert_eq!(toml_string("a \"b\" \\ c"), r#""a \"b\" \\ c""#);
        assert_eq!(toml_string("tab\there"), "\"tab\\u0009here\"");
        assert_eq!(toml_string("C:/x y/z"), "\"C:/x y/z\"");
    }

    #[test]
    fn replaced_text_is_never_looked_at_again() {
        let replaced = replace_tokens(
            "<A> then <B>, <A> again; é stays",
            &[("<A>", "<B>"), ("<B>", "<A> and <A>")],
        );
        assert_eq!(replaced, "<B> then <A> and <A>, <B> again; é stays");
        assert_eq!(replace_tokens("", &[("x", "y")]), "");
        assert_eq!(replace_tokens("xyz", &[("", "!")]), "xyz");
    }

    #[test]
    fn every_placeholder_is_in_both_templates() {
        for kind in Kind::ALL {
            let template = kind.template();
            for token in [
                NAME_LITERAL,
                ID_LITERAL,
                VENDOR_LITERAL,
                DESCRIPTION_LITERAL,
                TYPE_PLACEHOLDER,
                "PluginTemplateParams",
                "//! Plugin Template:",
            ] {
                assert!(template.contains(token), "{} lacks {token}", kind.name());
            }
        }
        assert!(INSTRUMENT.contains("PluginTemplateVoice"));
        assert!(EFFECT.contains("PluginKind::Effect"));
        assert!(INSTRUMENT.contains("PluginKind::Instrument"));
    }

    #[test]
    fn a_manguebyte_path_is_rewritten_to_be_read_from_the_new_crate() {
        // This crate's own folder is two levels below a checkout.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let checkout = here.join("..").join("..");
        if !checkout
            .join("crates")
            .join("manguebyte")
            .join("Cargo.toml")
            .is_file()
        {
            // Built from a published package, outside the repository.
            return;
        }
        // A relative path to the checkout, and one straight to the crate.
        assert_eq!(
            resolve_manguebyte_path(Path::new("../.."), here).unwrap(),
            "../../../crates/manguebyte"
        );
        assert_eq!(
            resolve_manguebyte_path(Path::new("../manguebyte"), here).unwrap(),
            "../../manguebyte"
        );
        assert_eq!(
            resolve_manguebyte_path(Path::new("./../manguebyte/."), here).unwrap(),
            "../../manguebyte"
        );
        // An absolute path stays absolute, with forward slashes.
        let absolute = resolve_manguebyte_path(&checkout, Path::new("elsewhere")).unwrap();
        assert!(absolute.ends_with("/../../crates/manguebyte"), "{absolute}");
        assert!(!absolute.contains('\\'), "{absolute}");
        assert!(Path::new(&absolute).is_absolute(), "{absolute}");

        // The form Windows gives a path when asked for the real one,
        // \\?\C:\..., does not take forward slashes. Elsewhere this is an
        // ordinary absolute path and the same holds trivially.
        let real = fs::canonicalize(&checkout).unwrap();
        let written = resolve_manguebyte_path(&real, Path::new("elsewhere")).unwrap();
        assert!(
            Path::new(&written).join("Cargo.toml").is_file(),
            "{written}"
        );
        if written.starts_with(r"\\?\") {
            assert!(!written.contains('/'), "{written}");
        }

        let error = resolve_manguebyte_path(Path::new("src"), here).unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.message().contains("--manguebyte-path"));
    }
}
