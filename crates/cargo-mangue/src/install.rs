//! The folders hosts scan for plugins, and copying bundles into them.
//!
//! Each format fixes where a plugin goes on each system, once for the
//! current user and once for everybody:
//!
//! | | For the user | For everybody |
//! |---|---|---|
//! | Windows | `%LOCALAPPDATA%\Programs\Common\CLAP`, `...\VST3` | `%COMMONPROGRAMFILES%\CLAP`, `...\VST3` |
//! | Linux | `~/.clap`, `~/.vst3` | `/usr/lib/clap`, `/usr/lib/vst3` |
//! | macOS | `~/Library/Audio/Plug-Ins/CLAP`, `.../VST3` | `/Library/Audio/Plug-Ins/CLAP`, `.../VST3` |
//!
//! An LV2 bundle goes to `~/.lv2` and `/usr/lib/lv2` on Linux,
//! `~/Library/Audio/Plug-Ins/LV2` and `/Library/Audio/Plug-Ins/LV2` on
//! macOS, and `%APPDATA%\LV2` and `%COMMONPROGRAMFILES%\LV2` on Windows.
//! An Audio Unit goes to `~/Library/Audio/Plug-Ins/Components` and
//! `/Library/Audio/Plug-Ins/Components`, on macOS only.
//!
//! # Sources
//!
//! - The CLAP header `clap/entry.h` (free-audio), which lists the folders a
//!   CLAP host searches.
//! - *VST 3 Developer Portal*, "Plug-in Locations" (Steinberg).
//! - The default `LV2_PATH` of lilv, the LV2 project's host library.
//! - *Audio Unit Programming Guide* (Apple): where components are
//!   installed.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::bundle;
use crate::error::Error;
use crate::platform::{Format, Os};

/// Who an installed plugin is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The current user. Needs no special rights.
    User,
    /// Every user of the machine. Needs an administrator.
    System,
}

/// The folder a plugin of this format is installed into.
///
/// `env` looks up an environment variable; passing it in is what lets the
/// folders of every system be checked on any one of them.
///
/// ```
/// use std::ffi::OsString;
/// use std::path::Path;
/// use cargo_mangue::install::{Scope, plugin_dir};
/// use cargo_mangue::platform::{Format, Os};
///
/// let env = |name: &str| (name == "HOME").then(|| OsString::from("/home/ana"));
/// let dir = plugin_dir(Os::Linux, Format::Clap, Scope::User, &env)?;
/// assert_eq!(dir, Path::new("/home/ana").join(".clap"));
/// # Ok::<(), cargo_mangue::Error>(())
/// ```
///
/// # Errors
///
/// Fails if the variable the folder is derived from is not set: `HOME` on
/// Linux and macOS, `LOCALAPPDATA` (`APPDATA` for LV2) or
/// `CommonProgramFiles` on Windows.
pub fn plugin_dir(
    os: Os,
    format: Format,
    scope: Scope,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, Error> {
    let variable = |name: &str| {
        env(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let needed = |name: &str| {
        variable(name).ok_or_else(|| {
            Error::failed(format!(
                "The {name} variable is not set, so the folder plugins are installed into is \
                 not known. Set it, or write the bundle somewhere with cargo mangue bundle \
                 --out <dir> and copy it yourself."
            ))
        })
    };
    if format == Format::Au && os != Os::Macos {
        return Err(Error::usage(format!(
            "An Audio Unit is installed on macOS only, and this is {}.",
            os.name()
        )));
    }
    let upper = match format {
        Format::Clap => "CLAP",
        Format::Vst3 => "VST3",
        Format::Lv2 => "LV2",
        Format::Au => "Components",
    };
    Ok(match (os, scope) {
        // LV2's folder for the user on Windows is in the roaming profile.
        (Os::Windows, Scope::User) if format == Format::Lv2 => needed("APPDATA")?.join(upper),
        (Os::Windows, Scope::User) => needed("LOCALAPPDATA")?
            .join("Programs")
            .join("Common")
            .join(upper),
        // A 32-bit program on 64-bit Windows is shown the 32-bit folder
        // under the usual name, and the real one under this name.
        (Os::Windows, Scope::System) => match variable("CommonProgramW6432") {
            Some(common) => common.join(upper),
            None => needed("CommonProgramFiles")?.join(upper),
        },
        (Os::Linux, Scope::User) => needed("HOME")?.join(format!(".{}", format.extension())),
        (Os::Linux, Scope::System) => Path::new("/usr/lib").join(format.extension()),
        (Os::Macos, Scope::User) => needed("HOME")?
            .join("Library")
            .join("Audio")
            .join("Plug-Ins")
            .join(upper),
        (Os::Macos, Scope::System) => Path::new("/Library/Audio/Plug-Ins").join(upper),
    })
}

/// Copies a bundle into a plugin folder, replacing one of the same name,
/// and returns where it now is.
///
/// A bundle that already is in the plugin folder, because it was written
/// straight into it, is left where it is: it is installed. Replacing means
/// removing what is there first, and that would remove the bundle itself.
///
/// # Errors
///
/// Fails if the folder cannot be created, an existing copy cannot be
/// removed, or the copy fails. The message says what usually helps: closing
/// the host that has the plugin loaded, or running as an administrator.
/// Fails too, before anything is removed, if the bundle is inside the
/// installed plugin it would replace or the other way round.
pub fn install(bundle: &Path, dir: &Path) -> Result<PathBuf, Error> {
    let Some(name) = bundle.file_name() else {
        return Err(Error::failed(format!(
            "{} is not a bundle that can be installed.",
            bundle.display()
        )));
    };
    fs::create_dir_all(dir).map_err(|error| {
        Error::failed(format!(
            "Could not create the plugin folder {}: {error}. For a folder shared by all users, \
             run the command as an administrator (with sudo on Linux and macOS).",
            dir.display()
        ))
    })?;
    let destination = dir.join(name);
    match overlap(bundle, &destination) {
        Overlap::None => {}
        Overlap::Same => return Ok(destination),
        Overlap::Nested => {
            return Err(Error::failed(format!(
                "The bundle {} and the place it is installed to, {}, are one inside the other, \
                 so installing would remove the bundle itself. Write the bundle somewhere else: \
                 leave --out off, or give it a folder outside the plugin folders.",
                bundle.display(),
                destination.display()
            )));
        }
    }
    bundle::remove(&destination)?;
    bundle::copy(bundle, &destination)?;
    Ok(destination)
}

/// How two paths that exist relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlap {
    /// Neither is the other or inside it, or one of them is not there.
    None,
    /// They are the same file or folder.
    Same,
    /// One is inside the other.
    Nested,
}

/// Compares where two paths really lead, whatever way each was written:
/// relative or absolute, through a link, or in another case on a system
/// that does not tell cases apart.
fn overlap(one: &Path, other: &Path) -> Overlap {
    // The real path of something that is not there cannot be asked for, and
    // what is not there cannot be in the way either.
    let (Ok(real_one), Ok(real_other)) = (fs::canonicalize(one), fs::canonicalize(other)) else {
        return Overlap::None;
    };
    if real_one == real_other || same_file(one, other) {
        Overlap::Same
    } else if real_one.starts_with(&real_other) || real_other.starts_with(&real_one) {
        Overlap::Nested
    } else {
        Overlap::None
    }
}

/// Whether two paths name one file by the file system's own account, which
/// also holds where two spellings of a name differ only in case.
#[cfg(unix)]
fn same_file(one: &Path, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(one), fs::metadata(other)) {
        (Ok(one), Ok(other)) => one.dev() == other.dev() && one.ino() == other.ino(),
        _ => false,
    }
}

/// Elsewhere the real paths are compared and nothing more: on Windows the
/// real path of a file is spelled the way the file system stores it.
#[cfg(not(unix))]
fn same_file(_one: &Path, _other: &Path) -> bool {
    false
}

/// Removes the bundle called `bundle_name` from a plugin folder. Returns
/// where it was, or `None` if it was not there.
///
/// # Errors
///
/// Fails if it is there and cannot be removed.
pub fn uninstall(dir: &Path, bundle_name: &str) -> Result<Option<PathBuf>, Error> {
    let installed = dir.join(bundle_name);
    Ok(bundle::remove(&installed)?.then_some(installed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(os: Os, format: Format, scope: Scope, vars: &[(&str, &str)]) -> Result<PathBuf, Error> {
        let env = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        };
        plugin_dir(os, format, scope, &env)
    }

    #[test]
    fn the_folders_of_every_system_are_the_documented_ones() {
        use Format::{Clap, Vst3};
        use Scope::{System, User};

        let windows = [
            ("LOCALAPPDATA", r"C:\Users\ana\AppData\Local"),
            ("CommonProgramFiles", r"C:\Program Files\Common Files"),
        ];
        let local = Path::new(r"C:\Users\ana\AppData\Local")
            .join("Programs")
            .join("Common");
        let common = Path::new(r"C:\Program Files\Common Files");
        assert_eq!(
            dir(Os::Windows, Clap, User, &windows).unwrap(),
            local.join("CLAP")
        );
        assert_eq!(
            dir(Os::Windows, Vst3, User, &windows).unwrap(),
            local.join("VST3")
        );
        assert_eq!(
            dir(Os::Windows, Clap, System, &windows).unwrap(),
            common.join("CLAP")
        );
        assert_eq!(
            dir(Os::Windows, Vst3, System, &windows).unwrap(),
            common.join("VST3")
        );

        let home = [("HOME", "/home/ana")];
        let ana = Path::new("/home/ana");
        assert_eq!(
            dir(Os::Linux, Clap, User, &home).unwrap(),
            ana.join(".clap")
        );
        assert_eq!(
            dir(Os::Linux, Vst3, User, &home).unwrap(),
            ana.join(".vst3")
        );
        assert_eq!(
            dir(Os::Linux, Clap, System, &[]).unwrap(),
            Path::new("/usr/lib").join("clap")
        );
        assert_eq!(
            dir(Os::Linux, Vst3, System, &[]).unwrap(),
            Path::new("/usr/lib").join("vst3")
        );

        let plug_ins = ana.join("Library").join("Audio").join("Plug-Ins");
        assert_eq!(
            dir(Os::Macos, Clap, User, &home).unwrap(),
            plug_ins.join("CLAP")
        );
        assert_eq!(
            dir(Os::Macos, Vst3, User, &home).unwrap(),
            plug_ins.join("VST3")
        );
        assert_eq!(
            dir(Os::Macos, Clap, System, &[]).unwrap(),
            Path::new("/Library/Audio/Plug-Ins").join("CLAP")
        );
        assert_eq!(
            dir(Os::Macos, Vst3, System, &[]).unwrap(),
            Path::new("/Library/Audio/Plug-Ins").join("VST3")
        );
    }

    #[test]
    fn a_32_bit_tool_on_64_bit_windows_still_installs_for_64_bit_hosts() {
        let vars = [
            ("CommonProgramFiles", r"C:\Program Files (x86)\Common Files"),
            ("CommonProgramW6432", r"C:\Program Files\Common Files"),
        ];
        assert_eq!(
            dir(Os::Windows, Format::Vst3, Scope::System, &vars).unwrap(),
            Path::new(r"C:\Program Files\Common Files").join("VST3")
        );
    }

    #[test]
    fn a_missing_variable_is_an_error_that_names_it() {
        for (os, scope, variable) in [
            (Os::Windows, Scope::User, "LOCALAPPDATA"),
            (Os::Windows, Scope::System, "CommonProgramFiles"),
            (Os::Linux, Scope::User, "HOME"),
            (Os::Macos, Scope::User, "HOME"),
        ] {
            let error = dir(os, Format::Clap, scope, &[]).unwrap_err();
            assert_eq!(error.exit_code(), 1);
            assert!(error.message().contains(variable), "{error}");
            // Set but empty is the same as not set.
            assert!(dir(os, Format::Clap, scope, &[(variable, "")]).is_err());
        }
    }

    #[test]
    fn an_audio_unit_goes_to_the_components_folders_of_macos_only() {
        let home = [("HOME", "/Users/ana")];
        assert_eq!(
            dir(Os::Macos, Format::Au, Scope::User, &home).unwrap(),
            Path::new("/Users/ana/Library/Audio/Plug-Ins/Components")
        );
        assert_eq!(
            dir(Os::Macos, Format::Au, Scope::System, &home).unwrap(),
            Path::new("/Library/Audio/Plug-Ins/Components")
        );
        for os in [Os::Windows, Os::Linux] {
            let error = dir(os, Format::Au, Scope::User, &home).unwrap_err();
            assert_eq!(error.exit_code(), 2);
        }
    }
}
