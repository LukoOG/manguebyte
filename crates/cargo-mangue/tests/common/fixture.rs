//! The smallest crate the tool can work on: an empty library built as a
//! `cdylib`, with nothing to download and a second or two to build.
//!
//! It is not a plugin a host could load. The tool does not look inside a
//! library, so it is enough to exercise everything from `cargo build` to
//! the installed bundle.

use std::fs;
use std::path::{Path, PathBuf};

/// The package name of the fixture.
pub const PACKAGE: &str = "fixture-plugin";
/// The plugin name its `Cargo.toml` sets.
pub const NAME: &str = "Fixture Plugin";
/// The identifier its `Cargo.toml` sets.
pub const IDENTIFIER: &str = "org.example.fixture";
/// Its version.
pub const VERSION: &str = "1.2.3";

/// Writes the fixture crate into `dir/fixture-plugin` and returns that
/// folder.
pub fn plugin(dir: &Path) -> PathBuf {
    let project = dir.join(PACKAGE);
    fs::create_dir_all(project.join("src")).unwrap();
    let manifest = format!(
        "[package]\n\
         name = \"{PACKAGE}\"\n\
         version = \"{VERSION}\"\n\
         edition = \"2021\"\n\
         \n\
         [lib]\n\
         crate-type = [\"cdylib\"]\n\
         \n\
         [package.metadata.manguebyte]\n\
         name = \"{NAME}\"\n\
         id = \"{IDENTIFIER}\"\n\
         \n\
         # A profile whose folder has a name of its own.\n\
         [profile.fast]\n\
         inherits = \"dev\"\n\
         \n\
         # Stands alone, wherever the test's folder happens to be.\n\
         [workspace]\n"
    );
    fs::write(project.join("Cargo.toml"), manifest).unwrap();
    fs::write(
        project.join("src").join("lib.rs"),
        "//! An empty library.\n",
    )
    .unwrap();
    project
}
