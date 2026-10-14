//! Finding the shared library inside a VST 3 bundle.
//!
//! A VST 3 plugin is a folder named `Name.vst3` on every system, with one
//! shared library per kind of machine it was built for:
//!
//! ```text
//! Name.vst3/
//!   Contents/
//!     x86_64-win/Name.vst3      Windows: also x86-win, arm64-win, arm64ec-win
//!     x86_64-linux/Name.so      Linux: the machine as `uname -m` prints it
//!     MacOS/Name                macOS, named by Contents/Info.plist
//!     Resources/                pictures, presets, the module description
//! ```
//!
//! A single file is also accepted and loaded as it is: the form VST 3 had on
//! Windows before bundles, and what a build leaves before it is packaged.
//!
//! Which folder to look in is decided by a [`Platform`] value and not by
//! conditional compilation, so every layout is resolved, and tested, on
//! every system.

use std::path::{Path, PathBuf};

use super::Vst3Error;
use crate::bundle::{mac_executable, only_file};

/// The system and machine a library has to have been built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Platform<'a> {
    /// The system, as `std::env::consts::OS` names it.
    pub(crate) os: &'a str,
    /// The machine, as `std::env::consts::ARCH` names it.
    pub(crate) arch: &'a str,
}

impl Platform<'static> {
    /// The platform this program was built for.
    pub(crate) const CURRENT: Self = Self {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    };
}

impl Platform<'_> {
    /// The folders under `Contents` that may hold this platform's library,
    /// most likely first.
    fn folders(&self) -> Vec<String> {
        if self.os == "windows" {
            let machines: &[&str] = match self.arch {
                "x86" => &["x86"],
                // A library for ARM64EC runs in an ARM64 process too.
                "aarch64" => &["arm64", "arm64ec"],
                "arm" => &["arm"],
                other => &[other],
            };
            return machines.iter().map(|name| format!("{name}-win")).collect();
        }
        // Elsewhere the folder is named after what `uname -m` prints, which
        // is not always what Rust calls the machine.
        let machines: &[&str] = match self.arch {
            "x86" => &["i386", "i686", "i586"],
            "arm" => &["armv7l", "armv8l", "armv6l", "arm"],
            other => &[other],
        };
        machines
            .iter()
            .map(|name| format!("{name}-{}", self.os))
            .collect()
    }

    /// What this platform's library is called, extension aside.
    fn extension(&self) -> &'static str {
        if self.os == "windows" { "vst3" } else { "so" }
    }
}

/// The shared library to load for `path` on this system: the file itself,
/// or the library inside the bundle if `path` is a folder.
pub(crate) fn binary_of(path: &Path) -> Result<PathBuf, Vst3Error> {
    binary_for(path, Platform::CURRENT)
}

/// As [`binary_of`], for any platform.
pub(crate) fn binary_for(path: &Path, platform: Platform<'_>) -> Result<PathBuf, Vst3Error> {
    let metadata = std::fs::metadata(path).map_err(|_| Vst3Error::NotFound(path.to_owned()))?;
    if !metadata.is_dir() {
        return Ok(path.to_owned());
    }
    let missing = || Vst3Error::BundleWithoutBinary(path.to_owned());
    if platform.os == "macos" {
        return mac_executable(path).ok_or_else(missing);
    }

    let contents = path.join("Contents");
    let extension = platform.extension();
    let folders = platform.folders();
    // The library is named after the bundle.
    if let Some(stem) = path.file_stem() {
        let mut name = stem.to_owned();
        name.push(".");
        name.push(extension);
        for folder in &folders {
            let candidate = contents.join(folder).join(&name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // Failing that, a folder that holds exactly one library leaves no doubt.
    folders
        .iter()
        .find_map(|folder| only_file(&contents.join(folder), Some(extension)))
        .ok_or_else(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for the files of one test, made empty.
    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join("manguebyte-host-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not a library").unwrap();
    }

    const WINDOWS: Platform<'static> = Platform {
        os: "windows",
        arch: "x86_64",
    };
    const LINUX: Platform<'static> = Platform {
        os: "linux",
        arch: "x86_64",
    };
    const MAC: Platform<'static> = Platform {
        os: "macos",
        arch: "aarch64",
    };

    #[test]
    fn each_system_finds_its_own_library_in_one_bundle() {
        let root = scratch("layouts");
        let bundle = root.join("Mangue Pluck.vst3");
        let contents = bundle.join("Contents");
        touch(&contents.join("x86_64-win").join("Mangue Pluck.vst3"));
        touch(&contents.join("arm64ec-win").join("Mangue Pluck.vst3"));
        touch(&contents.join("x86_64-linux").join("Mangue Pluck.so"));
        touch(&contents.join("aarch64-linux").join("Mangue Pluck.so"));
        touch(&contents.join("i386-linux").join("Mangue Pluck.so"));
        touch(&contents.join("MacOS").join("Mangue Pluck"));
        touch(&contents.join("Resources").join("moduleinfo.json"));

        let found = |platform| binary_for(&bundle, platform).unwrap();
        let inside = |folder: &str, file: &str| contents.join(folder).join(file);
        assert_eq!(found(WINDOWS), inside("x86_64-win", "Mangue Pluck.vst3"));
        assert_eq!(found(LINUX), inside("x86_64-linux", "Mangue Pluck.so"));
        assert_eq!(found(MAC), inside("MacOS", "Mangue Pluck"));
        // A machine whose folder goes by another name than Rust's.
        let arm_windows = Platform {
            os: "windows",
            arch: "aarch64",
        };
        assert_eq!(
            found(arm_windows),
            inside("arm64ec-win", "Mangue Pluck.vst3")
        );
        let arm_linux = Platform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(found(arm_linux), inside("aarch64-linux", "Mangue Pluck.so"));
        let old_linux = Platform {
            os: "linux",
            arch: "x86",
        };
        assert_eq!(found(old_linux), inside("i386-linux", "Mangue Pluck.so"));

        // A machine the bundle was not built for.
        let other = Platform {
            os: "linux",
            arch: "riscv64",
        };
        assert_eq!(
            binary_for(&bundle, other),
            Err(Vst3Error::BundleWithoutBinary(bundle.clone()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_is_taken_as_it_is_and_nothing_is_an_error() {
        let root = scratch("single");
        let file = root.join("Old Style.vst3");
        touch(&file);
        for platform in [WINDOWS, LINUX, MAC] {
            assert_eq!(binary_for(&file, platform), Ok(file.clone()));
        }
        let nothing = root.join("Not There.vst3");
        assert_eq!(
            binary_for(&nothing, WINDOWS),
            Err(Vst3Error::NotFound(nothing.clone()))
        );
        assert_eq!(binary_of(&nothing), Err(Vst3Error::NotFound(nothing)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_library_under_another_name_is_found_only_when_it_is_the_only_one() {
        let root = scratch("renamed");
        let bundle = root.join("Renamed.vst3");
        let folder = bundle.join("Contents").join("x86_64-win");
        touch(&folder.join("Original.vst3"));
        // Something that is not a library does not count.
        touch(&folder.join("notes.txt"));
        assert_eq!(
            binary_for(&bundle, WINDOWS),
            Ok(folder.join("Original.vst3"))
        );
        // Two candidates: which one is meant cannot be known.
        touch(&folder.join("Another.vst3"));
        assert_eq!(
            binary_for(&bundle, WINDOWS),
            Err(Vst3Error::BundleWithoutBinary(bundle.clone()))
        );
        // An empty bundle, and a folder that is no bundle at all.
        let empty = root.join("Empty.vst3");
        std::fs::create_dir_all(empty.join("Contents")).unwrap();
        for platform in [WINDOWS, LINUX, MAC] {
            assert_eq!(
                binary_for(&empty, platform),
                Err(Vst3Error::BundleWithoutBinary(empty.clone()))
            );
            assert_eq!(
                binary_for(&root, platform),
                Err(Vst3Error::BundleWithoutBinary(root.clone()))
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
