//! The command line: turning arguments into a [`Command`], and the help
//! texts.
//!
//! Arguments are parsed by hand, which keeps the tool free of dependencies
//! beyond the JSON reader. The grammar is the usual one: `--option value`
//! or `--option=value`, `-p value` or `-pvalue`, and options that may be
//! given several times (`--format`, `--target`, `--features`) also take a
//! comma-separated list or several values in a row.

use std::path::PathBuf;

use crate::cargo::{BuildRequest, Profile};
use crate::error::Error;
use crate::platform::Format;
use crate::scaffold::Kind;

/// What the person asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Show a help text.
    Help(Topic),
    /// Show the version.
    Version,
    /// Build a plugin and write its bundles.
    Bundle(BundleArgs),
    /// Bundle, then copy into the folders hosts scan.
    Install(InstallArgs),
    /// Remove an installed plugin.
    Uninstall(UninstallArgs),
    /// Bundle, then run the validators.
    Validate(ValidateArgs),
    /// Create a plugin crate.
    New(NewArgs),
}

/// What a help text is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topic {
    /// The tool as a whole.
    General,
    /// `cargo mangue bundle`.
    Bundle,
    /// `cargo mangue install`.
    Install,
    /// `cargo mangue uninstall`.
    Uninstall,
    /// `cargo mangue validate`.
    Validate,
    /// `cargo mangue new`.
    New,
}

impl Topic {
    /// Every topic.
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Bundle,
        Self::Install,
        Self::Uninstall,
        Self::Validate,
        Self::New,
    ];

    fn of_command(name: &str) -> Option<Self> {
        match name {
            "bundle" => Some(Self::Bundle),
            "install" => Some(Self::Install),
            "uninstall" => Some(Self::Uninstall),
            "validate" => Some(Self::Validate),
            "new" => Some(Self::New),
            _ => None,
        }
    }
}

/// The options of `bundle`, shared by `install` and `validate`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleArgs {
    /// The plugin's package, if named.
    pub package: Option<String>,
    /// The plugin's name, if given here and not left to `Cargo.toml`.
    pub name: Option<String>,
    /// The formats to bundle. Empty means CLAP and VST 3.
    pub formats: Vec<Format>,
    /// How to build.
    pub request: BuildRequest,
    /// The target triples to build for. Empty means cargo's default.
    pub targets: Vec<String>,
    /// Where to write the bundles, if not `target/bundled`.
    pub out: Option<PathBuf>,
}

/// The options of `install`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallArgs {
    /// What to bundle.
    pub bundle: BundleArgs,
    /// Install for every user of the machine.
    pub system: bool,
}

/// The options of `uninstall`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallArgs {
    /// The plugin's package, if named.
    pub package: Option<String>,
    /// The plugin's name, if given here. With it, no `Cargo.toml` is
    /// needed.
    pub name: Option<String>,
    /// The formats to remove. Empty means CLAP and VST 3.
    pub formats: Vec<Format>,
    /// Remove from the folders shared by every user.
    pub system: bool,
    /// The `Cargo.toml` to read the name from.
    pub manifest_path: Option<PathBuf>,
}

/// The options of `validate`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidateArgs {
    /// What to bundle.
    pub bundle: BundleArgs,
    /// The `clap-validator` program, if named.
    pub clap_validator: Option<PathBuf>,
    /// The VST 3 `validator` program, if named.
    pub vst3_validator: Option<PathBuf>,
}

/// The options of `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArgs {
    /// The package name, which is also the folder created.
    pub package: String,
    /// Effect or instrument.
    pub kind: Kind,
    /// The name shown to people, if not derived from the package name.
    pub name: Option<String>,
    /// Who makes the plugin.
    pub vendor: Option<String>,
    /// The plugin's identifier.
    pub id: Option<String>,
    /// A checkout of Manguebyte to build against.
    pub manguebyte_path: Option<PathBuf>,
}

/// Reads the arguments that follow the program's name.
///
/// Cargo runs `cargo mangue bundle` as `cargo-mangue mangue bundle`, so a
/// leading `mangue` is skipped and both spellings mean the same.
///
/// ```
/// use cargo_mangue::cli::{Command, parse};
/// use cargo_mangue::platform::Format;
///
/// let args = ["mangue", "bundle", "-p", "my-gain", "--format", "clap", "--release"];
/// let Command::Bundle(bundle) = parse(args.map(String::from))? else {
///     unreachable!()
/// };
/// assert_eq!(bundle.package.as_deref(), Some("my-gain"));
/// assert_eq!(bundle.formats, [Format::Clap]);
/// # Ok::<(), cargo_mangue::Error>(())
/// ```
///
/// # Errors
///
/// Fails with [`Failure::Usage`](crate::Failure::Usage), exit code 2, for
/// anything it does not understand. The message names the argument and the
/// help to read.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, Error> {
    let mut args: Vec<String> = args.into_iter().collect();
    if args.first().is_some_and(|first| first == "mangue") {
        args.remove(0);
    }
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err(Error::usage(format!(
            "No command was given.\n\n{}",
            help(Topic::General)
        )));
    };
    match command.as_str() {
        "-h" | "--help" => Ok(Command::Help(Topic::General)),
        "-V" | "--version" => Ok(Command::Version),
        // `help` is a command like the others, so it answers `--help` and
        // `help help` too, with the text that describes it: the general one.
        "help" => match args.next().as_deref() {
            None | Some("help" | "-h" | "--help") => Ok(Command::Help(Topic::General)),
            Some(name) => Topic::of_command(name)
                .map(Command::Help)
                .ok_or_else(|| unknown_command(name)),
        },
        "bundle" => parse_build(Parser::new(args, "bundle"), Topic::Bundle),
        "install" => parse_build(Parser::new(args, "install"), Topic::Install),
        "validate" => parse_build(Parser::new(args, "validate"), Topic::Validate),
        "uninstall" => parse_uninstall(Parser::new(args, "uninstall")),
        "new" => parse_new(Parser::new(args, "new")),
        other => Err(unknown_command(other)),
    }
}

fn unknown_command(name: &str) -> Error {
    Error::usage(format!(
        "\"{name}\" is not a cargo mangue command. The commands are new, bundle, install, \
         uninstall and validate. Run cargo mangue --help to see what each does."
    ))
}

/// An option that collects several values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Formats,
    Targets,
    Features,
}

/// One argument, classified.
#[derive(Debug)]
enum Token {
    /// `--name`, `-p`: something that starts with a hyphen.
    Option(String),
    /// Anything else.
    Word(String),
}

/// Walks the arguments of one command.
struct Parser {
    args: std::vec::IntoIter<String>,
    command: &'static str,
    /// The value written after `=` or straight after a short option, not
    /// yet taken by the option it belongs to.
    attached: Option<String>,
    /// The list option whose values a following bare word continues.
    list: Option<List>,
}

impl Parser {
    fn new(args: std::vec::IntoIter<String>, command: &'static str) -> Self {
        Self {
            args,
            command,
            attached: None,
            list: None,
        }
    }

    fn see_help(&self) -> String {
        format!(
            "Run cargo mangue {} --help to see its options.",
            self.command
        )
    }

    /// The next argument, with `--option=value` and `-pvalue` split.
    fn next(&mut self) -> Result<Option<Token>, Error> {
        if let Some(value) = self.attached.take() {
            return Err(Error::usage(format!(
                "The value \"{value}\" was given to an option that takes none. {}",
                self.see_help()
            )));
        }
        let Some(arg) = self.args.next() else {
            return Ok(None);
        };
        if let Some(long) = arg.strip_prefix("--") {
            if let Some((name, value)) = long.split_once('=') {
                self.attached = Some(value.to_owned());
                return Ok(Some(Token::Option(format!("--{name}"))));
            }
            return Ok(Some(Token::Option(arg)));
        }
        if arg.len() > 1 && arg.starts_with('-') {
            // A short option, perhaps with its value attached: -pmy-gain.
            let mut characters = arg.chars();
            characters.next();
            let letter: String = characters.next().into_iter().collect();
            let rest = characters.as_str();
            if !rest.is_empty() {
                self.attached = Some(rest.strip_prefix('=').unwrap_or(rest).to_owned());
            }
            return Ok(Some(Token::Option(format!("-{letter}"))));
        }
        Ok(Some(Token::Word(arg)))
    }

    /// The value of the option just read.
    fn value(&mut self, option: &str) -> Result<String, Error> {
        if let Some(value) = self.attached.take() {
            return Ok(value);
        }
        match self.args.next() {
            Some(value) if !value.starts_with('-') || value == "-" => Ok(value),
            _ => Err(Error::usage(format!(
                "The option {option} needs a value. (Write {option}=VALUE if the value starts \
                 with a hyphen.) {}",
                self.see_help()
            ))),
        }
    }

    fn unknown(&self, option: &str) -> Error {
        Error::usage(format!(
            "cargo mangue {} has no option {option}. {}",
            self.command,
            self.see_help()
        ))
    }

    fn unexpected(&self, word: &str) -> Error {
        Error::usage(format!(
            "The argument \"{word}\" was not expected. {}",
            self.see_help()
        ))
    }
}

/// Splits a list value at its commas and gives each part to `add`.
fn each_item(
    option: &str,
    value: &str,
    mut add: impl FnMut(&str) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut any = false;
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        any = true;
        add(item)?;
    }
    if any {
        Ok(())
    } else {
        Err(Error::usage(format!("The option {option} needs a value.")))
    }
}

fn add_format(formats: &mut Vec<Format>, name: &str) -> Result<(), Error> {
    let Some(format) = Format::from_name(name) else {
        return Err(Error::usage(format!(
            "\"{name}\" is not a plugin format. The formats are clap, vst3, lv2 and au."
        )));
    };
    if !formats.contains(&format) {
        formats.push(format);
    }
    Ok(())
}

fn add_target(targets: &mut Vec<String>, triple: &str) -> Result<(), Error> {
    if !targets.iter().any(|target| target == triple) {
        targets.push(triple.to_owned());
    }
    Ok(())
}

/// Parses `bundle`, `install` and `validate`, which share most options.
fn parse_build(mut parser: Parser, topic: Topic) -> Result<Command, Error> {
    let mut bundle = BundleArgs::default();
    let mut system = false;
    let mut clap_validator = None;
    let mut vst3_validator = None;
    let mut release = false;
    let mut profile: Option<String> = None;

    while let Some(token) = parser.next()? {
        let option = match token {
            Token::Option(option) => option,
            Token::Word(word) => {
                // `--format clap vst3`: more values for the same list.
                match parser.list {
                    Some(List::Formats) => {
                        each_item("--format", &word, |item| {
                            add_format(&mut bundle.formats, item)
                        })?;
                    }
                    Some(List::Targets) => {
                        each_item("--target", &word, |item| {
                            add_target(&mut bundle.targets, item)
                        })?;
                    }
                    Some(List::Features) => bundle.request.features.push(word),
                    None => return Err(parser.unexpected(&word)),
                }
                continue;
            }
        };
        parser.list = None;
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help(topic)),
            "-p" | "--package" => bundle.package = Some(parser.value(&option)?),
            "--name" => bundle.name = Some(parser.value(&option)?),
            "--format" => {
                let value = parser.value(&option)?;
                each_item(&option, &value, |item| {
                    add_format(&mut bundle.formats, item)
                })?;
                parser.list = Some(List::Formats);
            }
            "--target" => {
                let value = parser.value(&option)?;
                each_item(&option, &value, |item| {
                    add_target(&mut bundle.targets, item)
                })?;
                parser.list = Some(List::Targets);
            }
            "--release" | "-r" => release = true,
            "--profile" => profile = Some(parser.value(&option)?),
            "--out" => bundle.out = Some(PathBuf::from(parser.value(&option)?)),
            "--manifest-path" => {
                bundle.request.manifest_path = Some(PathBuf::from(parser.value(&option)?));
            }
            "--features" | "-F" => {
                bundle.request.features.push(parser.value(&option)?);
                parser.list = Some(List::Features);
            }
            "--all-features" => bundle.request.all_features = true,
            "--no-default-features" => bundle.request.no_default_features = true,
            "--system" if topic == Topic::Install => system = true,
            "--clap-validator" if topic == Topic::Validate => {
                clap_validator = Some(PathBuf::from(parser.value(&option)?));
            }
            "--vst3-validator" if topic == Topic::Validate => {
                vst3_validator = Some(PathBuf::from(parser.value(&option)?));
            }
            _ => return Err(parser.unknown(&option)),
        }
    }

    bundle.request.profile = match (release, profile) {
        (true, Some(_)) => {
            return Err(Error::usage(
                "Both --release and --profile were given. Use one: --release is the same as \
                 --profile release.",
            ));
        }
        (true, None) => Profile::Release,
        (false, None) => Profile::Dev,
        (false, Some(name)) => match name.as_str() {
            "release" => Profile::Release,
            "dev" => Profile::Dev,
            _ => Profile::Named(name),
        },
    };
    Ok(match topic {
        Topic::Install => Command::Install(InstallArgs { bundle, system }),
        Topic::Validate => Command::Validate(ValidateArgs {
            bundle,
            clap_validator,
            vst3_validator,
        }),
        _ => Command::Bundle(bundle),
    })
}

fn parse_uninstall(mut parser: Parser) -> Result<Command, Error> {
    let mut uninstall = UninstallArgs::default();
    while let Some(token) = parser.next()? {
        let option = match token {
            Token::Option(option) => option,
            Token::Word(word) => {
                if parser.list != Some(List::Formats) {
                    return Err(parser.unexpected(&word));
                }
                each_item("--format", &word, |item| {
                    add_format(&mut uninstall.formats, item)
                })?;
                continue;
            }
        };
        parser.list = None;
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help(Topic::Uninstall)),
            "-p" | "--package" => uninstall.package = Some(parser.value(&option)?),
            "--name" => uninstall.name = Some(parser.value(&option)?),
            "--format" => {
                let value = parser.value(&option)?;
                each_item(&option, &value, |item| {
                    add_format(&mut uninstall.formats, item)
                })?;
                parser.list = Some(List::Formats);
            }
            "--system" => uninstall.system = true,
            "--manifest-path" => {
                uninstall.manifest_path = Some(PathBuf::from(parser.value(&option)?));
            }
            _ => return Err(parser.unknown(&option)),
        }
    }
    Ok(Command::Uninstall(uninstall))
}

fn parse_new(mut parser: Parser) -> Result<Command, Error> {
    let mut package = None;
    let mut kind = Kind::Effect;
    let mut name = None;
    let mut vendor = None;
    let mut id = None;
    let mut manguebyte_path = None;

    while let Some(token) = parser.next()? {
        let option = match token {
            Token::Option(option) => option,
            Token::Word(word) => {
                if package.is_some() {
                    return Err(parser.unexpected(&word));
                }
                package = Some(word);
                continue;
            }
        };
        match option.as_str() {
            "-h" | "--help" => return Ok(Command::Help(Topic::New)),
            "--kind" => {
                let value = parser.value(&option)?;
                kind = Kind::from_name(&value).ok_or_else(|| {
                    Error::usage(format!(
                        "\"{value}\" is not a kind of plugin. The kinds are effect and \
                         instrument."
                    ))
                })?;
            }
            "--name" => name = Some(parser.value(&option)?),
            "--vendor" => vendor = Some(parser.value(&option)?),
            "--id" => id = Some(parser.value(&option)?),
            "--manguebyte-path" => {
                manguebyte_path = Some(PathBuf::from(parser.value(&option)?));
            }
            _ => return Err(parser.unknown(&option)),
        }
    }
    let Some(package) = package else {
        return Err(Error::usage(format!(
            "cargo mangue new needs the name of the package to create, such as cargo mangue \
             new my-gain. {}",
            parser.see_help()
        )));
    };
    Ok(Command::New(NewArgs {
        package,
        kind,
        name,
        vendor,
        id,
        manguebyte_path,
    }))
}

/// The help text for a topic, ending with a line break.
pub fn help(topic: Topic) -> String {
    match topic {
        Topic::General => format!(
            "cargo-mangue {}\n{}",
            env!("CARGO_PKG_VERSION"),
            GENERAL_HELP
        ),
        Topic::Bundle => format!("{BUNDLE_HELP}{BUILD_OPTIONS}{COMMON_ENDING}"),
        Topic::Install => {
            format!("{INSTALL_HELP}{BUILD_OPTIONS}{INSTALL_OPTIONS}{COMMON_ENDING}")
        }
        Topic::Validate => {
            format!("{VALIDATE_HELP}{BUILD_OPTIONS}{VALIDATE_OPTIONS}{COMMON_ENDING}")
        }
        Topic::Uninstall => UNINSTALL_HELP.to_owned(),
        Topic::New => NEW_HELP.to_owned(),
    }
}

const GENERAL_HELP: &str = "\
The Manguebyte command-line tool: create, bundle, install and validate
audio plugins.

Usage: cargo mangue <command> [options]

Commands:
  new        Create a plugin crate that builds as it is
  bundle     Build a plugin and write its .clap and .vst3 (or .lv2, .component)
  install    Bundle, then copy into the folders hosts scan
  uninstall  Remove an installed plugin from those folders
  validate   Bundle, then run clap-validator and the VST 3 validator
  help       Show this text, or the help of a command

Options:
  -h, --help     Show this text
  -V, --version  Show the version

Exit codes:
  0  The command did its work
  1  It could not: a file could not be written, a tool is missing, ...
  2  The command line was wrong
  3  cargo build failed
  4  A validator reported a failed test, or gave no result
  5  No validator was found, so nothing was checked

Run cargo mangue <command> --help for the options of a command.
";

const BUNDLE_HELP: &str = "\
Build a plugin and write its .clap and its .vst3, or with --format lv2 its
.lv2, or on a Mac with --format au its .component.

Usage: cargo mangue bundle [options]

The plugin's library is built with cargo and found from what cargo reports,
then laid out the way hosts expect on the system it was built for:

  Windows  Name.clap                    Name.vst3/Contents/x86_64-win/Name.vst3
  Linux    Name.clap                    Name.vst3/Contents/x86_64-linux/Name.so
  macOS    Name.clap/Contents/MacOS/    Name.vst3/Contents/MacOS/
           with Info.plist              with Info.plist and PkgInfo

An LV2 bundle is a folder, Name.lv2, with the library (Name.dll, Name.so
or Name.dylib) and its Turtle files, manifest.ttl and one named after the
plugin's id. Those are written by a small program cargo mangue builds and
runs against the plugin's crate, which must call export_lv2! at its root
and have the rlib crate type as well as cdylib.

An Audio Unit is a macOS bundle, Name.component/Contents/MacOS/Name with
PkgInfo and an Info.plist that names the unit's codes, which a small
program writes in the same way: the crate must call export_au! at its root.
It is refused for Windows and Linux.

On macOS several --target options make one universal binary with lipo, and
the bundle (for LV2, its library) gets an ad-hoc signature with codesign.
If codesign is missing the bundle is written unsigned and a note says so.

The plugin's name comes from its Cargo.toml:

  [package.metadata.manguebyte]
  name = \"Mangue Pluck\"
  id = \"org.manguebyte.pluck\"    (the bundle identifier on macOS)

";

const INSTALL_HELP: &str = "\
Bundle a plugin, then copy it into the folders hosts scan.

Usage: cargo mangue install [options]

Without --system the plugin is installed for the current user:

  Windows  %LOCALAPPDATA%\\Programs\\Common\\CLAP and ...\\VST3
  Linux    ~/.clap and ~/.vst3
  macOS    ~/Library/Audio/Plug-Ins/CLAP and .../VST3

With --system it is installed for everybody, which needs an administrator:

  Windows  %COMMONPROGRAMFILES%\\CLAP and ...\\VST3
  Linux    /usr/lib/clap and /usr/lib/vst3
  macOS    /Library/Audio/Plug-Ins/CLAP and .../VST3

With --format lv2, an LV2 bundle goes to %APPDATA%\\LV2 on Windows (for
everybody, %COMMONPROGRAMFILES%\\LV2), ~/.lv2 on Linux (/usr/lib/lv2) and
~/Library/Audio/Plug-Ins/LV2 on macOS (/Library/Audio/Plug-Ins/LV2).
With --format au, an Audio Unit goes to ~/Library/Audio/Plug-Ins/Components
(/Library/Audio/Plug-Ins/Components), on macOS only.

An installed plugin of the same name is replaced. Close the host first:
Windows does not let go of a plugin that is loaded.

The bundles are written to the folder --out names, by default the one
cargo mangue bundle writes to, and copied from there. --out does not change
where the plugin is installed.

";

const VALIDATE_HELP: &str = "\
Bundle a plugin, then run each format's own conformance tool on it.

Usage: cargo mangue validate [options]

  CLAP   clap-validator, built from
         https://github.com/free-audio/clap-validator
  VST 3  validator, built from the VST 3 SDK,
         https://github.com/steinbergmedia/vst3sdk
  AU     auval, which comes with macOS, with --format au. It checks the
         unit installed with the bundle's codes: install it first

Each is taken from its option, else from its environment variable, else
from PATH. A format whose validator is not found is skipped with a note.
LV2 has no validator here, and --format lv2 is refused: check an LV2
bundle with lv2lint.
The command fails with exit code 4 if a validator reports a failed test or
ends without a result, and with exit code 5 if no validator was found at
all, so that passing never means that nothing was checked.

What the validators print is shown, except for three things the VST 3
validator prints by the thousand: the list of the plugin's parameters, the
[Succeeded] after each test, and a remark about one parameter that repeats
a remark already shown for another. A last line counts those repeats.

";

const BUILD_OPTIONS: &str = "\
Options:
  -p, --package <name>      The plugin's package. Not needed from inside the
                            plugin's folder, or when the workspace has one
      --name <name>         The plugin's name, which names the bundles.
                            Default: name under [package.metadata.manguebyte]
                            in its Cargo.toml, else the package name
      --format <format>     clap, vst3, lv2 or au. Repeat it, or write
                            clap,vst3, for several. Default: clap and vst3
  -r, --release             Build with the release profile
      --profile <name>      Build with this profile. Default: dev
      --target <triple>     Build for this target. Repeat it for several
                            architectures of one system: a universal binary
                            on macOS, a folder each in a .vst3 elsewhere.
                            Default: the target cargo is set up to build
                            for, which is this machine's unless
                            CARGO_BUILD_TARGET or build.target says otherwise
      --out <dir>           Where to write the bundles.
                            Default: bundled in cargo's target folder
      --manifest-path <path>  The Cargo.toml to use
  -F, --features <list>     Features to build with
      --all-features        Build with every feature
      --no-default-features  Build without the default features
";

// These three start with spaces, which a backslash at the end of the line
// before them would swallow, so each starts on the line of its quote.
const INSTALL_OPTIONS: &str =
    "      --system              Install for every user of the machine, which
                            needs an administrator
";

const VALIDATE_OPTIONS: &str =
    "      --clap-validator <path>  The clap-validator program. Default: the
                            CLAP_VALIDATOR variable, else PATH
      --vst3-validator <path>  The VST 3 validator program. Default: the
                            VST3_VALIDATOR variable, else PATH
";

const COMMON_ENDING: &str = "  -h, --help                Show this text
";

const UNINSTALL_HELP: &str = "\
Remove an installed plugin from the folders hosts scan.

Usage: cargo mangue uninstall [options]

Removes Name.clap and Name.vst3, and with the formats lv2 and au Name.lv2
and Name.component, from the
folders cargo mangue install copies into. A plugin that is not there is
reported and is not an error.

Options:
  -p, --package <name>      The plugin's package, to read its name from
      --name <name>         The plugin's name. With it, no Cargo.toml is
                            needed
      --format <format>     clap, vst3, lv2 or au. Default: clap and vst3
      --system              Remove from the folders shared by every user,
                            which needs an administrator
      --manifest-path <path>  The Cargo.toml to use
  -h, --help                Show this text
";

const NEW_HELP: &str = "\
Create a plugin crate that builds as it is.

Usage: cargo mangue new <name> [options]

Creates the folder <name> with a Cargo.toml and a src/lib.rs holding a
complete plugin: parameters, processing, tests that run it without a host,
and its export as CLAP and VST 3. Nothing existing is overwritten.

  effect      turns its input up or down, with automation on the exact frame
  instrument  plays a sawtooth for every note, sixteen at once

Options:
      --kind <kind>         effect or instrument. Default: effect
      --name <name>         The name people see. Default: from the package
                            name, my-gain becoming \"My Gain\"
      --vendor <vendor>     Who makes it. Default: \"Example Audio\"
      --id <id>             Its identifier in reverse-domain form, which
                            hosts store in sessions and which must never
                            change once the plugin has shipped.
                            Default: com.example.<name>
      --manguebyte-path <path>  Build against this checkout of Manguebyte
                            instead of its git repository
  -h, --help                Show this text
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command, Error> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn bundle(args: &[&str]) -> BundleArgs {
        match parsed(args) {
            Ok(Command::Bundle(bundle)) => bundle,
            other => panic!("{args:?} gave {other:?}"),
        }
    }

    fn usage_error(args: &[&str]) -> String {
        let error = parsed(args).expect_err(&format!("{args:?} was accepted"));
        assert_eq!(error.exit_code(), 2, "{args:?}: {error}");
        error.message().to_owned()
    }

    #[test]
    fn the_same_with_and_without_the_word_cargo_puts_in() {
        assert_eq!(parsed(&["mangue", "bundle"]), parsed(&["bundle"]));
        assert_eq!(
            parsed(&["mangue", "--help"]),
            Ok(Command::Help(Topic::General))
        );
        // Only the first one is cargo's: a plugin may be called mangue.
        let Ok(Command::New(new)) = parsed(&["mangue", "new", "mangue"]) else {
            panic!()
        };
        assert_eq!(new.package, "mangue");
    }

    #[test]
    fn bundle_with_nothing_else_is_all_defaults() {
        let args = bundle(&["bundle"]);
        assert_eq!(args, BundleArgs::default());
        assert_eq!(args.request.profile, Profile::Dev);
        assert!(args.formats.is_empty() && args.targets.is_empty());
    }

    #[test]
    fn every_bundle_option_is_read() {
        let args = bundle(&[
            "bundle",
            "-p",
            "mangue-pluck",
            "--name",
            "Mangue Pluck",
            "--format",
            "clap",
            "--format",
            "vst3",
            "--release",
            "--target",
            "x86_64-apple-darwin",
            "--target",
            "aarch64-apple-darwin",
            "--out",
            "dist",
            "--manifest-path",
            "plugins/pluck/Cargo.toml",
            "--features",
            "a,b",
            "-F",
            "c",
            "--all-features",
            "--no-default-features",
        ]);
        assert_eq!(args.package.as_deref(), Some("mangue-pluck"));
        assert_eq!(args.name.as_deref(), Some("Mangue Pluck"));
        assert_eq!(args.formats, [Format::Clap, Format::Vst3]);
        assert_eq!(args.request.profile, Profile::Release);
        assert_eq!(
            args.targets,
            ["x86_64-apple-darwin", "aarch64-apple-darwin"]
        );
        assert_eq!(args.out, Some(PathBuf::from("dist")));
        assert_eq!(
            args.request.manifest_path,
            Some(PathBuf::from("plugins/pluck/Cargo.toml"))
        );
        assert_eq!(args.request.features, ["a,b", "c"]);
        assert!(args.request.all_features && args.request.no_default_features);
    }

    #[test]
    fn values_can_be_attached_to_their_option() {
        let args = bundle(&[
            "bundle",
            "--package=my-gain",
            "--name=-12 dB",
            "--format=vst3",
            "--profile=fast",
        ]);
        assert_eq!(args.package.as_deref(), Some("my-gain"));
        assert_eq!(args.name.as_deref(), Some("-12 dB"));
        assert_eq!(args.formats, [Format::Vst3]);
        assert_eq!(args.request.profile, Profile::Named("fast".into()));

        assert_eq!(bundle(&["bundle", "-pmy-gain"]).package.unwrap(), "my-gain");
        assert_eq!(
            bundle(&["bundle", "-p=my-gain"]).package.unwrap(),
            "my-gain"
        );
        assert_eq!(bundle(&["bundle", "--name="]).name.unwrap(), "");
    }

    #[test]
    fn lists_take_repeats_commas_and_values_in_a_row() {
        let repeated = bundle(&["bundle", "--format", "clap", "--format", "vst3"]);
        let commas = bundle(&["bundle", "--format", "clap,vst3"]);
        let in_a_row = bundle(&["bundle", "--format", "clap", "vst3", "--release"]);
        assert_eq!(repeated.formats, [Format::Clap, Format::Vst3]);
        assert_eq!(commas.formats, repeated.formats);
        assert_eq!(in_a_row.formats, repeated.formats);
        assert_eq!(in_a_row.request.profile, Profile::Release);

        // The order given is kept, and a repeat counts once.
        let args = bundle(&["bundle", "--format", "VST3", "clap", "vst3"]);
        assert_eq!(args.formats, [Format::Vst3, Format::Clap]);

        let targets = bundle(&[
            "bundle",
            "--target",
            "a-b-c",
            "d-e-f",
            "--target=a-b-c,g-h-i",
        ]);
        assert_eq!(targets.targets, ["a-b-c", "d-e-f", "g-h-i"]);
    }

    #[test]
    fn profiles_by_name_and_by_flag_agree() {
        assert_eq!(
            bundle(&["bundle", "--profile", "release"]).request.profile,
            Profile::Release
        );
        assert_eq!(
            bundle(&["bundle", "--profile", "dev"]).request.profile,
            Profile::Dev
        );
        assert_eq!(bundle(&["bundle", "-r"]).request.profile, Profile::Release);
        assert!(usage_error(&["bundle", "--release", "--profile", "fast"]).contains("Use one"));
    }

    #[test]
    fn install_and_validate_add_their_own_options_to_those_of_bundle() {
        let Ok(Command::Install(install)) =
            parsed(&["install", "-p", "x", "--system", "--release"])
        else {
            panic!()
        };
        assert!(install.system);
        assert_eq!(install.bundle.package.as_deref(), Some("x"));
        assert_eq!(install.bundle.request.profile, Profile::Release);

        let Ok(Command::Validate(validate)) = parsed(&[
            "validate",
            "--clap-validator",
            "tools/clap-validator",
            "--vst3-validator=tools/validator",
            "--format",
            "clap",
        ]) else {
            panic!()
        };
        assert_eq!(
            validate.clap_validator,
            Some(PathBuf::from("tools/clap-validator"))
        );
        assert_eq!(
            validate.vst3_validator,
            Some(PathBuf::from("tools/validator"))
        );
        assert_eq!(validate.bundle.formats, [Format::Clap]);

        // Each command has only its own.
        assert!(usage_error(&["bundle", "--system"]).contains("no option --system"));
        assert!(usage_error(&["install", "--clap-validator", "x"]).contains("no option"));
        assert!(usage_error(&["validate", "--system"]).contains("no option --system"));
    }

    #[test]
    fn uninstall_needs_only_a_name() {
        let Ok(Command::Uninstall(uninstall)) = parsed(&[
            "uninstall",
            "--name",
            "Mangue Pluck",
            "--format",
            "clap",
            "--system",
        ]) else {
            panic!()
        };
        assert_eq!(uninstall.name.as_deref(), Some("Mangue Pluck"));
        assert_eq!(uninstall.formats, [Format::Clap]);
        assert!(uninstall.system);
        assert_eq!(
            parsed(&["uninstall"]),
            Ok(Command::Uninstall(UninstallArgs::default()))
        );
        assert!(usage_error(&["uninstall", "--release"]).contains("no option --release"));
    }

    #[test]
    fn new_reads_its_name_and_options_in_any_order() {
        let Ok(Command::New(new)) = parsed(&[
            "new",
            "--kind",
            "instrument",
            "my-synth",
            "--vendor",
            "Ana Lima",
            "--id=br.com.ana.my-synth",
            "--name",
            "My Synth",
            "--manguebyte-path",
            "../manguebyte",
        ]) else {
            panic!()
        };
        assert_eq!(new.package, "my-synth");
        assert_eq!(new.kind, Kind::Instrument);
        assert_eq!(new.vendor.as_deref(), Some("Ana Lima"));
        assert_eq!(new.id.as_deref(), Some("br.com.ana.my-synth"));
        assert_eq!(new.name.as_deref(), Some("My Synth"));
        assert_eq!(new.manguebyte_path, Some(PathBuf::from("../manguebyte")));

        let Ok(Command::New(plain)) = parsed(&["new", "my-gain"]) else {
            panic!()
        };
        assert_eq!(plain.kind, Kind::Effect);
        assert_eq!((plain.name, plain.vendor, plain.id), (None, None, None));

        assert!(usage_error(&["new"]).contains("needs the name"));
        assert!(usage_error(&["new", "a", "b"]).contains("\"b\""));
        assert!(
            usage_error(&["new", "a", "--kind", "generator"]).contains("effect and instrument")
        );
    }

    #[test]
    fn help_is_there_for_every_command_in_every_spelling() {
        for (name, topic) in [
            ("bundle", Topic::Bundle),
            ("install", Topic::Install),
            ("uninstall", Topic::Uninstall),
            ("validate", Topic::Validate),
            ("new", Topic::New),
        ] {
            assert_eq!(parsed(&[name, "--help"]), Ok(Command::Help(topic)));
            assert_eq!(parsed(&[name, "-h"]), Ok(Command::Help(topic)));
            assert_eq!(parsed(&["help", name]), Ok(Command::Help(topic)));
            // Asking for help wins over whatever else is on the line.
            assert_eq!(
                parsed(&[name, "--name", "x", "--help"]),
                Ok(Command::Help(topic))
            );
        }
        for general in [&["--help"][..], &["-h"], &["help"]] {
            assert_eq!(parsed(general), Ok(Command::Help(Topic::General)));
        }
        assert_eq!(parsed(&["--version"]), Ok(Command::Version));
        assert_eq!(parsed(&["-V"]), Ok(Command::Version));
    }

    /// Found in review: the general help lists `help` among the commands,
    /// and `cargo mangue help --help` answered that `--help` is not a
    /// command.
    #[test]
    fn the_help_command_has_help_like_every_other_command() {
        for args in [
            &["help", "--help"][..],
            &["help", "-h"],
            &["help", "help"],
            &["mangue", "help", "--help"],
        ] {
            assert_eq!(
                parsed(args),
                Ok(Command::Help(Topic::General)),
                "cargo mangue {}",
                args.join(" ")
            );
        }
        // The text it answers with says what `help` does.
        assert!(help(Topic::General).contains("\n  help "));
        // Something that is no command is still told so, by its own name.
        assert!(
            usage_error(&["help", "--hlep"]).contains("\"--hlep\" is not a cargo mangue command")
        );
    }

    #[test]
    fn help_texts_cover_their_options_and_fit_a_terminal() {
        let options: [(Topic, &[&str]); 5] = [
            (
                Topic::Bundle,
                &[
                    "--package",
                    "--name",
                    "--format",
                    "--release",
                    "--profile",
                    "--target",
                    "--out",
                    "--manifest-path",
                    "--features",
                    "--all-features",
                    "--no-default-features",
                    "--help",
                ],
            ),
            (Topic::Install, &["--system", "--package", "--help"]),
            (
                Topic::Validate,
                &[
                    "--clap-validator",
                    "--vst3-validator",
                    "CLAP_VALIDATOR",
                    "VST3_VALIDATOR",
                    "--package",
                    "--help",
                ],
            ),
            (
                Topic::Uninstall,
                &["--package", "--name", "--format", "--system", "--help"],
            ),
            (
                Topic::New,
                &[
                    "--kind",
                    "--name",
                    "--vendor",
                    "--id",
                    "--manguebyte-path",
                    "--help",
                ],
            ),
        ];
        for (topic, expected) in options {
            let text = help(topic);
            for option in expected {
                assert!(text.contains(option), "{topic:?} does not mention {option}");
            }
        }
        for topic in Topic::ALL {
            let text = help(topic);
            assert!(text.ends_with('\n'));
            assert!(text.contains("Usage: cargo mangue"), "{topic:?}");
            for line in text.lines() {
                assert!(
                    line.chars().count() <= 80,
                    "{topic:?} has a long line: {line}"
                );
                // Options line up: a short one two spaces in, a long one
                // six, and nothing flush against the margin.
                let aligned = !line.trim_start().starts_with('-')
                    || (line.starts_with("  -") && !line.starts_with("   "))
                    || (line.starts_with("      --") && !line.starts_with("       "));
                assert!(aligned, "{topic:?} has a misplaced option: {line:?}");
            }
            // Each option is listed once.
            for option in text.lines().filter_map(|line| {
                let line = line.trim_start();
                line.starts_with('-')
                    .then(|| line.split("  ").next().unwrap())
            }) {
                let listed = text
                    .lines()
                    .filter(|line| line.trim_start().split("  ").next() == Some(option))
                    .count();
                assert_eq!(listed, 1, "{topic:?} lists {option} {listed} times");
            }
        }
        let general = help(Topic::General);
        for command in ["new", "bundle", "install", "uninstall", "validate"] {
            assert!(general.contains(&format!("\n  {command} ")), "{command}");
        }
        for code in 0..=5 {
            assert!(
                general.contains(&format!("\n  {code}  ")),
                "exit code {code}"
            );
        }
    }

    #[test]
    fn mistakes_are_usage_errors_that_say_what_to_do() {
        assert!(usage_error(&[]).contains("No command was given"));
        assert!(usage_error(&["mangue"]).contains("Usage: cargo mangue"));
        assert!(usage_error(&["bundel"]).contains("\"bundel\" is not a cargo mangue command"));
        assert!(usage_error(&["help", "bundel"]).contains("bundel"));

        let unknown = usage_error(&["bundle", "--frmat", "clap"]);
        assert!(unknown.contains("no option --frmat"), "{unknown}");
        assert!(unknown.contains("cargo mangue bundle --help"), "{unknown}");

        assert!(usage_error(&["bundle", "--format", "vst2"]).contains("clap, vst3, lv2 and au"));
        assert!(usage_error(&["bundle", "--format"]).contains("needs a value"));
        assert!(usage_error(&["bundle", "--format", ","]).contains("needs a value"));
        assert!(usage_error(&["bundle", "--name", "--release"]).contains("--name=VALUE"));
        assert!(usage_error(&["bundle", "-p"]).contains("-p needs a value"));
        assert!(usage_error(&["bundle", "--release=yes"]).contains("takes none"));
        assert!(usage_error(&["bundle", "stray"]).contains("\"stray\" was not expected"));
        // A bare word continues a list only straight after it.
        assert!(usage_error(&["bundle", "--format", "clap", "--release", "vst3"]).contains("vst3"));
        assert!(usage_error(&["new", "x", "--kind"]).contains("needs a value"));
    }

    #[test]
    fn odd_arguments_never_panic() {
        for args in [
            &["-"][..],
            &["--"],
            &["bundle", "-"],
            &["bundle", "--"],
            &["bundle", "--="],
            &["bundle", "-é"],
            &["bundle", "-p", "-"],
            &["bundle", "--name", ""],
            &["new", ""],
            &["new", "é", "--kind", "é"],
            &["", "", ""],
            &["bundle", "--target", ",,,"],
        ] {
            let _ = parsed(args);
        }
        assert_eq!(bundle(&["bundle", "-p", "-"]).package.unwrap(), "-");
    }
}
