//! Finding and running the other programs the tool relies on.
//!
//! `lipo` and `codesign` on macOS and the two validators are separate
//! programs that may or may not be installed. Everything that needs one
//! goes through the [`Tools`] trait, so the tests can stand in for them and
//! exercise the macOS path on any machine.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One program to run, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCommand {
    /// The program's name, looked for on `PATH`.
    pub program: &'static str,
    /// Its arguments.
    pub args: Vec<OsString>,
}

/// Access to programs installed on the machine.
pub trait Tools {
    /// Where `program` is, or `None` if it is not installed.
    fn find(&self, program: &str) -> Option<PathBuf>;

    /// Runs the program at `path` and waits for it.
    ///
    /// # Errors
    ///
    /// Returns what went wrong, as text to show, if the program cannot be
    /// started or does not exit successfully.
    fn run(&mut self, path: &Path, args: &[OsString]) -> Result<(), String>;
}

/// The programs really installed: found on `PATH`, run as child processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTools;

impl Tools for SystemTools {
    fn find(&self, program: &str) -> Option<PathBuf> {
        find_on_path(program, env::var_os("PATH").as_deref(), cfg!(windows))
    }

    fn run(&mut self, path: &Path, args: &[OsString]) -> Result<(), String> {
        let output = Command::new(path)
            .args(args)
            .output()
            .map_err(|error| format!("{} could not be started: {error}", path.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        Err(if said.is_empty() {
            format!("{} ended with {}", path.display(), output.status)
        } else {
            format!("{} ended with {}: {said}", path.display(), output.status)
        })
    }
}

/// Looks for a program in the folders of a `PATH` value.
///
/// `path` is the value of the variable, with the separator of the system
/// the tool runs on. With `windows` set, a name without an extension is
/// also tried with `.exe`, which is how Windows itself finds programs.
///
/// Returns the first match that is a file (and, on Unix, executable).
pub fn find_on_path(program: &str, path: Option<&OsStr>, windows: bool) -> Option<PathBuf> {
    let path = path?;
    let with_exe =
        (windows && Path::new(program).extension().is_none()).then(|| format!("{program}.exe"));
    env::split_paths(path)
        .filter(|folder| !folder.as_os_str().is_empty())
        .flat_map(|folder| {
            let exe = with_exe.as_ref().map(|name| folder.join(name));
            exe.into_iter().chain([folder.join(program)])
        })
        .find(|candidate| is_program(candidate))
}

/// Whether `path` is a file this user could run.
fn is_program(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
