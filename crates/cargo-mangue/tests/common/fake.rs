//! A stand-in for a validator: a small script that prints given lines and
//! ends with a given exit code.

use std::fs;
use std::path::{Path, PathBuf};

/// Writes a program called `name` into `dir` that prints `args:` followed
/// by its arguments, then each of `lines`, and exits with `code`.
///
/// The lines must be plain: letters, digits, spaces and `,.:=()[]'-`.
/// On Windows it is a batch file and elsewhere a shell script, and neither
/// is given anything it would interpret.
pub fn program(dir: &Path, name: &str, lines: &[&str], code: u8) -> PathBuf {
    for line in lines {
        let plain = line
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ,.:=()[]'-".contains(c));
        assert!(plain, "{line:?} is not plain enough for a script");
    }
    if cfg!(windows) {
        let path = dir.join(format!("{name}.cmd"));
        let mut script = String::from("@echo off\r\necho args: %*\r\n");
        for line in lines {
            if line.is_empty() {
                script.push_str("echo.\r\n");
            } else {
                script.push_str(&format!("echo {line}\r\n"));
            }
        }
        script.push_str(&format!("exit /b {code}\r\n"));
        fs::write(&path, script).unwrap();
        path
    } else {
        let path = dir.join(name);
        let mut script = String::from("#!/bin/sh\necho \"args: $*\"\ncat <<'END_OF_OUTPUT'\n");
        for line in lines {
            script.push_str(line);
            script.push('\n');
        }
        script.push_str(&format!("END_OF_OUTPUT\nexit {code}\n"));
        fs::write(&path, script).unwrap();
        make_executable(&path);
        path
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
