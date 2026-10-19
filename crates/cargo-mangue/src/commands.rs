//! What each command does, built from the other modules.
//!
//! Results go to standard output, one line each (`Bundled <path>`,
//! `Installed <path>`), and everything else to standard error: cargo's own
//! progress, notes, and the error that ends a failed run.

use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::au;
use crate::bundle;
use crate::cargo::{BuiltFor, Cargo, built_for, select_package};
use crate::cli::{self, BundleArgs, Command, InstallArgs, NewArgs, UninstallArgs, ValidateArgs};
use crate::error::{Error, Failure};
use crate::install::{self, Scope};
use crate::layout::{
    BundleSpec, Contents, PlannedFile, bundle_version, check_plugin_name, macos_identifier, plan,
};
use crate::lv2;
use crate::platform::{Arch, Format, Os, Target};
use crate::scaffold::{self, Dependency, NewPlugin, REPOSITORY};
use crate::tools::SystemTools;
use crate::validate::{self, Verdict};

/// Carries out a command.
///
/// # Errors
///
/// Whatever stops the command, as a message for the person and an exit
/// code for the script: see [`Failure`].
pub fn execute(command: Command) -> Result<(), Error> {
    match command {
        Command::Help(topic) => {
            print!("{}", cli::help(topic));
            Ok(())
        }
        Command::Version => {
            println!("cargo-mangue {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Bundle(args) => bundle_plugin(&args, None).map(|_| ()),
        Command::Install(args) => install_plugin(&args),
        Command::Uninstall(args) => uninstall_plugin(&args),
        Command::Validate(args) => validate_plugin(&args),
        Command::New(args) => new_plugin(&args),
    }
}

fn env_var(name: &str) -> Option<OsString> {
    env::var_os(name)
}

/// The formats asked for, or CLAP and VST 3.
fn formats_or_all(formats: &[Format]) -> Vec<Format> {
    if formats.is_empty() {
        Format::DEFAULT.to_vec()
    } else {
        formats.to_vec()
    }
}

/// The folder cargo would look for a package from: the one holding the
/// named `Cargo.toml`, else the current one.
fn start_dir(manifest_path: Option<&Path>) -> Option<PathBuf> {
    match manifest_path {
        Some(manifest) => std::path::absolute(manifest)
            .ok()?
            .parent()
            .map(Path::to_path_buf),
        None => env::current_dir().ok(),
    }
}

/// This machine's system, for the commands that act on it.
fn host_os(command: &str) -> Result<Os, Error> {
    Os::host().ok_or_else(|| {
        Error::failed(format!(
            "cargo mangue {command} works on Windows, Linux and macOS, and this is none of \
             them. Use cargo mangue bundle and place the result by hand."
        ))
    })
}

/// The bundles of one plugin, as written.
struct Bundled {
    /// The plugin's name.
    name: String,
    /// The system they are for.
    os: Os,
    /// Each format's bundle.
    bundles: Vec<(Format, PathBuf)>,
}

/// Builds a plugin and writes its bundles.
///
/// `for_this_machine` names the command when the bundles are going to be
/// used here, installed or validated, and so must be for this system.
fn bundle_plugin(args: &BundleArgs, for_this_machine: Option<&str>) -> Result<Bundled, Error> {
    let cargo = Cargo::from_env();
    let manifest_path = args.request.manifest_path.as_deref();
    let metadata = cargo.metadata(manifest_path)?;
    let package = select_package(
        &metadata,
        args.package.as_deref(),
        start_dir(manifest_path).as_deref(),
    )?;
    let name = match &args.name {
        Some(name) => name.clone(),
        None => package.plugin_name().to_owned(),
    };

    // Which systems and architectures, before anything is built. For a
    // build with no target named this is what cargo is expected to build
    // for; what it did build for is read from its report afterwards.
    let triples: Vec<Option<&str>> = if args.targets.is_empty() {
        vec![None]
    } else {
        args.targets
            .iter()
            .map(|triple| Some(triple.as_str()))
            .collect()
    };
    let mut targets = Vec::with_capacity(triples.len());
    for triple in &triples {
        targets.push(match triple {
            Some(triple) => Target::from_triple(triple)?,
            None => Target::from_triple(&cargo.default_target()?)?,
        });
    }
    let os = targets[0].os;
    if let Some(other) = targets.iter().find(|target| target.os != os) {
        return Err(Error::usage(format!(
            "The targets are for {} and for {}. A bundle is for one system: run the command \
             once for each, with --out to keep the results apart.",
            os.name(),
            other.os.name()
        )));
    }
    if let Some(command) = for_this_machine {
        let host = host_os(command)?;
        if os != host {
            return Err(Error::usage(format!(
                "cargo mangue {command} works with plugins for this machine, which runs {}, \
                 and the target is for {}. Use cargo mangue bundle to build for another \
                 system.",
                host.name(),
                os.name()
            )));
        }
    }

    // The layouts next, so that a name or a combination that cannot work
    // is reported before a long build and not after it.
    let formats = formats_or_all(&args.formats);
    if formats.contains(&Format::Lv2) {
        lv2::helper_features(&args.request)?;
    }
    if formats.contains(&Format::Au) {
        au::helper_features(&args.request)?;
    }
    let plans_for = |archs: &[Arch]| {
        let planned = formats.iter().map(|&format| {
            plan(&BundleSpec {
                format,
                os,
                archs: archs.to_vec(),
                name: name.clone(),
                identifier: package.identifier(),
                version: bundle_version(&package.version),
            })
        });
        planned.collect::<Result<Vec<_>, Error>>()
    };
    let expected: Vec<Arch> = targets.iter().map(|target| target.arch).collect();
    let mut plans = plans_for(&expected)?;

    let mut libraries = Vec::with_capacity(targets.len());
    for (target, triple) in targets.iter().zip(&triples) {
        let library = cargo.build(package, &args.request, *triple, os)?;
        let built = built_target(
            &cargo,
            &library,
            &metadata.target_directory,
            *triple,
            *target,
        )?;
        libraries.push((built.arch, library));
    }
    // Cargo has the last word on what it built for. If that is another
    // architecture than expected, the layouts are worked out again, so that
    // no library is filed under an architecture it was not built for.
    let built: Vec<Arch> = libraries.iter().map(|(arch, _)| *arch).collect();
    if built != expected {
        plans = plans_for(&built)?;
    }
    // An LV2 bundle's Turtle files, which only the plugin's code can write.
    for (format, plan) in formats.iter().zip(&mut plans) {
        if *format != Format::Lv2 {
            continue;
        }
        let binary = plan
            .library_file_name()
            .ok_or_else(|| Error::failed("The LV2 bundle has no library in its plan."))?
            .to_owned();
        eprintln!("Writing the Turtle files of {}", plan.root);
        for (file, text) in lv2::turtle_files(&cargo, &metadata, package, &args.request, &binary)? {
            plan.files.push(PlannedFile {
                path: format!("{}/{file}", plan.root),
                contents: Contents::Bytes(text),
            });
        }
    }
    // An Audio Unit's Info.plist, whose codes only the plugin's code knows.
    for (format, plan) in formats.iter().zip(&mut plans) {
        if *format != Format::Au {
            continue;
        }
        eprintln!("Writing the Info.plist of {}", plan.root);
        let text = au::info_plist(
            &cargo,
            &metadata,
            package,
            &args.request,
            &name,
            &macos_identifier(&package.identifier())?,
            &bundle_version(&package.version),
        )?;
        plan.files.push(PlannedFile {
            path: format!("{}/Contents/Info.plist", plan.root),
            contents: Contents::Bytes(text),
        });
    }

    let out_dir = match &args.out {
        Some(out) => out.clone(),
        None => metadata.target_directory.join("bundled"),
    };
    let mut tools = SystemTools;
    let mut bundles = Vec::with_capacity(plans.len());
    for (format, plan) in formats.into_iter().zip(&plans) {
        let written = bundle::write(plan, &out_dir, &libraries, &mut tools)?;
        for note in &written.notes {
            eprintln!("note: {note}");
        }
        println!("Bundled {}", written.path.display());
        bundles.push((format, written.path));
    }
    Ok(Bundled { name, os, bundles })
}

/// The target a library was really built for.
///
/// `requested` is the target cargo was given with `--target`, if any, and
/// `expected` is what the build was taken to be for before it ran. When no
/// target was given, that was the `CARGO_BUILD_TARGET` variable or the
/// compiler's host, and cargo may have been set up to build for another
/// one with `build.target` in a configuration file, which the tool does not
/// read. Where cargo put the library says what it did ([`built_for`]), and
/// that decides: a library in the folder of the wrong architecture inside
/// a bundle is loaded by no host.
fn built_target(
    cargo: &Cargo,
    library: &Path,
    target_directory: &Path,
    requested: Option<&str>,
    expected: Target,
) -> Result<Target, Error> {
    let found = built_for(library, target_directory);
    let built = match (found, requested) {
        (BuiltFor::Target(folder), Some(requested)) => {
            // Cargo names the folder after the target it was given, so
            // these agree unless something other than cargo answered.
            match Target::from_triple(folder) {
                Ok(built) if built == expected => built,
                _ => {
                    return Err(Error::failed(format!(
                        "cargo was asked to build for {requested} and reports the library in \
                         the folder of {folder}: {}. Cargo itself does not do that; check which \
                         program the CARGO variable names.",
                        library.display()
                    )));
                }
            }
        }
        (BuiltFor::Target(folder), None) => Target::from_triple(folder).map_err(|error| {
            Error::failed(format!(
                "Cargo is set up to build for {folder}, with build.target in a configuration \
                 file or the CARGO_BUILD_TARGET variable. {} Say which with --target.",
                error.message()
            ))
        })?,
        // No target was named anywhere, so this is a build for the
        // compiler's host. That is what was expected unless the variable
        // named a target and cargo did not follow it.
        (BuiltFor::Host, None) => match cargo.target_variable() {
            None => expected,
            Some(_) => Target::from_triple(&cargo.host_target()?)?,
        },
        // Cargo was told what to build for, and where it put the result is
        // not its usual place: there is nothing to check the target against.
        (BuiltFor::Host | BuiltFor::Unknown, Some(_)) => expected,
        (BuiltFor::Unknown, None) => {
            eprintln!(
                "note: {} is not inside {}, where cargo puts what it builds, so what it was \
                 built for could not be checked. It is taken to be for {} on {}; use --target \
                 if it is not.",
                library.display(),
                target_directory.display(),
                expected.arch.name(),
                expected.os.name()
            );
            expected
        }
    };
    if built.os != expected.os {
        return Err(Error::failed(format!(
            "cargo built {}, a library for {}, and the bundle was to be for {}. If cargo is set \
             up to build for another system, say which with --target.",
            library.display(),
            built.os.name(),
            expected.os.name()
        )));
    }
    if built.arch != expected.arch {
        eprintln!(
            "note: cargo is set up to build for {} here and not for {}, so the bundles are for \
             {} on {}.",
            built.arch.name(),
            expected.arch.name(),
            built.arch.name(),
            built.os.name()
        );
    }
    Ok(built)
}

fn scope(system: bool) -> Scope {
    if system { Scope::System } else { Scope::User }
}

fn install_plugin(args: &InstallArgs) -> Result<(), Error> {
    let bundled = bundle_plugin(&args.bundle, Some("install"))?;
    for (format, bundle) in &bundled.bundles {
        let dir = install::plugin_dir(bundled.os, *format, scope(args.system), &env_var)?;
        let installed = install::install(bundle, &dir)?;
        println!("Installed {}", installed.display());
    }
    Ok(())
}

fn uninstall_plugin(args: &UninstallArgs) -> Result<(), Error> {
    let os = host_os("uninstall")?;
    let name = match &args.name {
        Some(name) => name.clone(),
        None => {
            let manifest_path = args.manifest_path.as_deref();
            let metadata = Cargo::from_env().metadata(manifest_path)?;
            let package = select_package(
                &metadata,
                args.package.as_deref(),
                start_dir(manifest_path).as_deref(),
            )?;
            package.plugin_name().to_owned()
        }
    };
    // The name becomes part of a path that is about to be removed.
    check_plugin_name(&name)?;
    for format in formats_or_all(&args.formats) {
        let dir = install::plugin_dir(os, format, scope(args.system), &env_var)?;
        let bundle_name = format!("{name}.{}", format.extension());
        match install::uninstall(&dir, &bundle_name)? {
            Some(removed) => println!("Removed {}", removed.display()),
            None => println!("{bundle_name} is not installed in {}", dir.display()),
        }
    }
    Ok(())
}

fn validate_plugin(args: &ValidateArgs) -> Result<(), Error> {
    host_os("validate")?;
    let formats = formats_or_all(&args.bundle.formats);
    if formats.contains(&Format::Lv2) {
        return Err(Error::usage(
            "cargo mangue validate runs the CLAP and VST 3 validators, and has none for LV2. \
             Bundle it with cargo mangue bundle --format lv2 and check it with lv2lint, from \
             the OpenMusicKontrollers project.",
        ));
    }

    // Find the validators first: a wrong path should not cost a build.
    let tools = SystemTools;
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for &format in &formats {
        let named = match format {
            Format::Clap => args.clap_validator.as_deref(),
            Format::Vst3 => args.vst3_validator.as_deref(),
            // Refused above.
            Format::Lv2 => None,
            // The system's own, from the AUVAL variable or PATH.
            Format::Au => None,
        };
        match validate::locate(format, named, &env_var, &tools)? {
            Some(validator) => found.push((format, validator)),
            None => missing.push(format),
        }
    }
    let how_to_get = |format: Format| {
        format!(
            "{}, then put it on PATH or name it with {} or the {} variable.",
            validate::where_to_get(format),
            validate::option(format),
            validate::variable(format)
        )
    };
    if found.is_empty() {
        let advice: Vec<String> = missing
            .iter()
            .map(|&format| {
                format!(
                    "For {}, {} was not found. {}",
                    format.name(),
                    validate::program(format),
                    how_to_get(format)
                )
            })
            .collect();
        return Err(Error::new(
            Failure::NoValidator,
            format!(
                "No validator was found, so nothing was checked. {}",
                advice.join(" ")
            ),
        ));
    }

    let bundled = bundle_plugin(&args.bundle, Some("validate"))?;
    let mut failures = Vec::new();
    for (format, validator) in &found {
        let Some((_, bundle)) = bundled.bundles.iter().find(|(built, _)| built == format) else {
            continue;
        };
        eprintln!(
            "Validating {} with {}",
            bundle.display(),
            validator.display()
        );
        let outcome = validate::run(*format, validator, bundle, &mut io::stdout())?;
        let label = format!("{} as {}", bundled.name, format.name());
        match outcome.verdict {
            Verdict::Passed { passed } => {
                println!("Validated {label}: {passed} tests passed, none failed");
                if let Some(ended) = &outcome.ended_badly {
                    eprintln!(
                        "note: {} reported no failed test and then ended with {ended}.",
                        validator.display()
                    );
                }
            }
            Verdict::Failed { passed, failed } => {
                failures.push(format!("{label}: {failed} tests failed, {passed} passed"));
            }
            Verdict::NoResult => failures.push(match &outcome.ended_badly {
                Some(ended) => format!("{label}: the validator ended with {ended} and no result"),
                None => format!("{label}: the validator gave no result"),
            }),
        }
    }
    for &format in &missing {
        eprintln!(
            "note: {} was not validated as {}: {} was not found. {}",
            bundled.name,
            format.name(),
            validate::program(format),
            how_to_get(format)
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::new(
            Failure::Validation,
            format!(
                "Validation failed. {}. The validator's output above names the tests.",
                failures.join("; ")
            ),
        ))
    }
}

fn new_plugin(args: &NewArgs) -> Result<(), Error> {
    let current_dir = env::current_dir().map_err(|error| {
        Error::failed(format!(
            "The current folder could not be read ({error}). Run the command from the folder \
             the new crate should be created in."
        ))
    })?;
    let mut plugin = NewPlugin::new(&args.package, args.kind)?;
    if let Some(name) = &args.name {
        plugin.name = name.clone();
    }
    if let Some(vendor) = &args.vendor {
        plugin.vendor = vendor.clone();
    }
    if let Some(id) = &args.id {
        plugin.id = id.clone();
    }
    if let Some(path) = &args.manguebyte_path {
        plugin.dependency =
            Dependency::Path(scaffold::resolve_manguebyte_path(path, &current_dir)?);
    } else if args.manguebyte_git {
        plugin.dependency = Dependency::Git(REPOSITORY.to_owned());
    }
    let files = scaffold::project(&plugin)?;
    let dir = current_dir.join(&plugin.package);
    scaffold::write_project(&dir, &files)?;

    println!(
        "Created {} \"{}\" in {}",
        plugin.kind.described(),
        plugin.name,
        dir.display()
    );
    eprintln!();
    eprintln!("  cd {}", plugin.package);
    eprintln!("  cargo test                      runs its tests, with no host");
    eprintln!("  cargo mangue bundle --release   builds its .clap and its .vst3");
    eprintln!("  cargo mangue bundle --release --format lv2   builds its .lv2");
    eprintln!("  cargo mangue bundle --release --format au    builds its .component, on a Mac");
    eprintln!("  cargo mangue install --release  and copies them to where hosts look");
    // Whatever was not chosen is a placeholder, and the id is the one that
    // cannot be changed later.
    if args.id.is_none() {
        eprintln!();
        eprintln!(
            "Before you give the plugin to anyone, set its id (now \"{}\") in src/lib.rs and \
             Cargo.toml. Hosts store the id in sessions, so it must never change after that.",
            plugin.id
        );
    }
    if args.vendor.is_none() {
        eprintln!();
        eprintln!(
            "Its vendor is \"{}\" until you set your own in src/lib.rs.",
            plugin.vendor
        );
    }
    if let Some(workspace) = scaffold::enclosing_workspace(&dir) {
        eprintln!();
        eprintln!(
            "note: the new crate is inside the workspace of {}. Unless that workspace's \
             members already cover it, add \"{}\" to them, or to exclude, before building.",
            workspace.display(),
            plugin.package
        );
    }
    Ok(())
}
