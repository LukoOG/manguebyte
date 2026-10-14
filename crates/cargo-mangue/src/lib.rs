//! The Manguebyte command-line tool.
//!
//! `cargo mangue` takes a plugin from nothing to something a host loads:
//!
//! | Command | What it does |
//! |---|---|
//! | `cargo mangue new <name>` | Writes a plugin crate that builds as it is |
//! | `cargo mangue bundle` | Builds a plugin and writes its `.clap` and `.vst3` |
//! | `cargo mangue install` | Bundles, then copies into the folders hosts scan |
//! | `cargo mangue uninstall` | Removes an installed plugin |
//! | `cargo mangue validate` | Bundles, then runs the CLAP and VST 3 validators |
//!
//! It does for Manguebyte what Projucer and the CMake functions do for
//! JUCE, as a cargo subcommand. From a checkout, `cargo install --path
//! crates/cargo-mangue` puts it on `PATH`, where it answers to both
//! `cargo mangue` and `cargo-mangue`. Run `cargo mangue --help` for the
//! options.
//!
//! # How it is put together
//!
//! This library holds everything; the program is a call to [`main`]. The
//! parts that decide things are plain functions from values to values, so
//! each can be used, and is tested, without building a plugin:
//!
//! | Module | What it decides |
//! |---|---|
//! | [`cli`] | What the arguments mean |
//! | [`platform`] | Which system and architecture a target triple names |
//! | [`layout`] | Which files a bundle consists of, for every system and format |
//! | [`cargo`] | Which library cargo built, from cargo's own report |
//! | [`install`] | Which folder a plugin is installed into |
//! | [`validate`] | Whether a validator's output is a pass |
//! | [`scaffold`] | The files of a new plugin crate |
//!
//! [`bundle`], [`tools`] and [`commands`] do the work those decide on:
//! writing files and running cargo, `lipo`, `codesign` and the validators.
//!
//! # Example
//!
//! The layout of a macOS bundle, worked out on any machine:
//!
//! ```
//! use cargo_mangue::layout::{BundleSpec, Contents, plan};
//! use cargo_mangue::platform::{Arch, Format, Os};
//!
//! let plan = plan(&BundleSpec {
//!     format: Format::Clap,
//!     os: Os::Macos,
//!     archs: vec![Arch::X86_64, Arch::Aarch64],
//!     name: "Mangue Pluck".into(),
//!     identifier: "org.manguebyte.pluck".into(),
//!     version: "0.1.0".into(),
//! })?;
//! let paths: Vec<&str> = plan.files.iter().map(|file| file.path.as_str()).collect();
//! assert_eq!(paths, [
//!     "Mangue Pluck.clap/Contents/MacOS/Mangue Pluck",
//!     "Mangue Pluck.clap/Contents/Info.plist",
//! ]);
//! // Two architectures on macOS are one universal binary.
//! assert_eq!(
//!     plan.files[0].contents,
//!     Contents::Universal(vec![Arch::X86_64, Arch::Aarch64])
//! );
//! assert!(plan.sign);
//! # Ok::<(), cargo_mangue::Error>(())
//! ```
//!
//! # Limits
//!
//! - It bundles for Windows, Linux and macOS, on x86_64, aarch64 and 32-bit
//!   x86. Other targets are refused, not guessed at.
//! - On macOS it signs ad hoc, which lets a plugin load on the machine that
//!   built it. Signing with a Developer ID and notarising are not done.
//! - A CLAP on Windows and Linux is one library, so it holds one
//!   architecture; several `--target`s there are possible for VST 3 only.
//! - Without `--target`, a bundle is for whatever cargo is set up to build
//!   for. The tool does not read cargo's configuration files; it goes by
//!   the folder cargo reports the library in, which names the target.
//! - `install` copies to the 64-bit plugin folders on Windows.
//! - It reads the output of cargo and of the validators as text from
//!   another program: anything it does not understand is an error with a
//!   message, never a panic, and long output is read in bounded memory.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod cargo;
pub mod cli;
pub mod commands;
mod error;
pub mod install;
pub mod layout;
pub mod lines;
pub mod platform;
pub mod scaffold;
pub mod tools;
pub mod validate;

use std::ffi::OsString;
use std::process::ExitCode;

pub use error::{Error, Failure};

/// Runs the tool with the arguments of a process, program name first, and
/// returns the code to exit with.
///
/// An error is printed on standard error as `error: ` and its message. The
/// codes are those of [`Failure`], and zero for success.
///
/// ```
/// use std::process::ExitCode;
///
/// let code = cargo_mangue::main(["cargo-mangue", "mangue", "--version"].map(Into::into));
/// assert_eq!(code, ExitCode::SUCCESS);
/// ```
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

/// Runs the tool with the arguments of a process, program name first.
///
/// # Errors
///
/// A usage error if an argument is not valid Unicode or the command line
/// is not understood, and otherwise whatever stops the command.
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Error> {
    let mut texts = Vec::new();
    for arg in args.into_iter().skip(1) {
        texts.push(arg.into_string().map_err(|arg| {
            Error::usage(format!(
                "The argument {} is not valid Unicode, and cargo mangue only takes arguments \
                 that are.",
                arg.to_string_lossy()
            ))
        })?);
    }
    commands::execute(cli::parse(texts)?)
}
