//! Finding the shared library inside a plugin bundle.
//!
//! On Windows and Linux a `.clap` plugin is a shared library with another
//! extension. On macOS it is a bundle: a folder laid out as
//!
//! ```text
//! Name.clap/
//!   Contents/
//!     Info.plist        names the executable, among other things
//!     MacOS/
//!       Name            the shared library
//! ```
//!
//! Nothing here calls into the operating system's bundle functions, so the
//! same code runs, and is tested, on every platform: a folder is treated as
//! a bundle wherever it is found. The reading of the bundle itself is shared
//! with the VST 3 host, in `crate::bundle`.

use std::path::{Path, PathBuf};

use super::ClapError;
use crate::bundle::mac_executable;

/// The shared library to load for `path`: the file itself, or the
/// executable of the bundle if `path` is a folder.
pub(crate) fn binary_of(path: &Path) -> Result<PathBuf, ClapError> {
    let metadata = std::fs::metadata(path).map_err(|_| ClapError::NotFound(path.to_owned()))?;
    if !metadata.is_dir() {
        return Ok(path.to_owned());
    }
    mac_executable(path).ok_or_else(|| ClapError::BundleWithoutExecutable(path.to_owned()))
}
