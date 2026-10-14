//! A folder for one test to write in, removed when the test ends.

use std::fs;
use std::path::{Path, PathBuf};

/// A folder of its own under the place cargo sets aside for test files, so
/// that nothing a test writes lands in the source tree.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates an empty folder for the test called `test`.
    pub fn new(test: &str) -> Self {
        let name = format!("cargo-mangue-{test}-{}", std::process::id());
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
