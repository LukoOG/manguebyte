//! What cargo built for decides the bundle, and not what the tool expected
//! it to build for.
//!
//! Before a build the tool can only know the targets it passes to cargo
//! itself and the `CARGO_BUILD_TARGET` variable. Cargo may be set up to
//! build for another one, with `build.target` in a configuration file, and
//! says so by the folder it reports the library in. These tests give the
//! tool a cargo that reports a library in one folder or another and look at
//! what it does with it.
//!
//! No second target is installed on a test machine, so cargo here is a
//! stand-in: a script that prints the two reports the tool reads, in the
//! shape cargo gives them. `commands.rs` does the same with the real cargo
//! for the one target that is installed everywhere, the machine's own.
//!
//! Found in review: with `build.target` naming another architecture, the
//! library was bundled under this machine's, silently. On x86_64 Windows a
//! library reported in `target\aarch64-pc-windows-msvc\debug` was copied to
//! `foreign.vst3/Contents/x86_64-win/foreign.vst3`, where no host of either
//! architecture loads it, and the tool printed `Bundled` and exited with 0.
//!
//! Every test takes a lock, for the reason given in `validators.rs`: a
//! script one thread has just written cannot be started on Linux while
//! another thread is starting a program of its own.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard};

use cargo_mangue::cargo::Cargo;
use cargo_mangue::layout::vst3_arch_folder;
use cargo_mangue::platform::{Arch, Os, Target};
use common::Scratch;

/// Held by every test, since each starts other programs.
static STARTING_PROGRAMS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    STARTING_PROGRAMS
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// A stand-in for cargo: prints `metadata.json` when asked for metadata and
/// `build.json` when asked to build, both from its own folder.
fn stand_in_cargo(dir: &Path, metadata: &str, build: &str) -> PathBuf {
    fs::write(dir.join("metadata.json"), metadata).unwrap();
    fs::write(dir.join("build.json"), build).unwrap();
    if cfg!(windows) {
        let path = dir.join("cargo.cmd");
        let script = "@echo off\r\n\
                      if \"%1\"==\"metadata\" (type \"%~dp0metadata.json\") else (type \"%~dp0build.json\")\r\n";
        fs::write(&path, script).unwrap();
        path
    } else {
        let path = dir.join("cargo");
        let script = "#!/bin/sh\n\
                      here=$(dirname \"$0\")\n\
                      if [ \"$1\" = metadata ]; then cat \"$here/metadata.json\"; \
                      else cat \"$here/build.json\"; fi\n";
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

/// A plugin called `foreign` whose library cargo reports wherever a test
/// says.
struct Reported {
    /// The folder everything is in, which lasts as long as this does.
    _scratch: Scratch,
    project: PathBuf,
    cargo: PathBuf,
    /// The library, with a few bytes standing for its code.
    library: PathBuf,
    /// Where the tool is asked to write.
    out: PathBuf,
}

const CODE: &[u8] = b"a library for some processor";

impl Reported {
    /// `place` is the folder the library is in, inside the target
    /// directory: `debug`, or `<triple>/debug`.
    fn new(test: &str, os: Os, place: &Path) -> Self {
        let scratch = Scratch::new(test);
        let project = scratch.path().join("plugin");
        let target_dir = scratch.path().join("target");
        let built_in = target_dir.join(place);
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&built_in).unwrap();
        let manifest = project.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let prefix = if os == Os::Windows { "" } else { "lib" };
        let library = built_in.join(format!("{prefix}foreign.{}", os.library_extension()));
        fs::write(&library, CODE).unwrap();

        let id = "path+file:///work/plugin#foreign@0.1.0";
        let lib_target = serde_json::json!({
            "kind": ["cdylib"], "crate_types": ["cdylib"], "name": "foreign",
        });
        let metadata = serde_json::json!({
            "packages": [{
                "name": "foreign", "version": "0.1.0", "id": id,
                "manifest_path": manifest.to_str().unwrap(),
                "targets": [lib_target.clone()], "metadata": null,
            }],
            "target_directory": target_dir.to_str().unwrap(),
            "version": 1,
        });
        let built = serde_json::json!({
            "reason": "compiler-artifact", "package_id": id,
            "manifest_path": manifest.to_str().unwrap(),
            "target": lib_target, "features": [],
            "filenames": [library.to_str().unwrap()],
            "executable": null, "fresh": false,
        });
        let finished = serde_json::json!({ "reason": "build-finished", "success": true });
        let cargo = stand_in_cargo(
            scratch.path(),
            &metadata.to_string(),
            &format!("{built}\n{finished}\n"),
        );
        let out = scratch.path().join("dist");
        Self {
            _scratch: scratch,
            project,
            cargo,
            library,
            out,
        }
    }

    /// Runs `cargo mangue bundle --format vst3` with more arguments.
    fn bundle(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cargo-mangue"))
            .current_dir(&self.project)
            .args(["bundle", "--format", "vst3", "--out"])
            .arg(&self.out)
            .args(extra)
            .env("CARGO", &self.cargo)
            .env_remove("CARGO_BUILD_TARGET")
            .output()
            .unwrap()
    }

    /// The folder of one architecture inside the bundle.
    fn arch_folder(&self, os: Os, arch: Arch) -> PathBuf {
        self.out
            .join("foreign.vst3")
            .join("Contents")
            .join(vst3_arch_folder(os, arch))
    }

    /// Every file under the bundle, to see where the library went.
    fn bundled_files(&self) -> Vec<PathBuf> {
        fn walk(folder: &Path, found: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(folder) else {
                return;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.out, &mut found);
        found
    }
}

/// This machine as the Rust compiler names it, and a target of the same
/// system for another processor.
fn host_and_other(os: Os) -> (String, Target, &'static str, Arch) {
    let triple = Cargo::from_env().host_target().unwrap();
    let host = Target::from_triple(&triple).unwrap();
    assert_eq!(host.os, os);
    let (other, other_arch) = match (os, host.arch) {
        (Os::Windows, Arch::Aarch64) => ("x86_64-pc-windows-msvc", Arch::X86_64),
        (Os::Windows, _) => ("aarch64-pc-windows-msvc", Arch::Aarch64),
        (Os::Linux, Arch::Aarch64) => ("x86_64-unknown-linux-gnu", Arch::X86_64),
        (Os::Linux, _) => ("aarch64-unknown-linux-gnu", Arch::Aarch64),
        (Os::Macos, Arch::Aarch64) => ("x86_64-apple-darwin", Arch::X86_64),
        (Os::Macos, _) => ("aarch64-apple-darwin", Arch::Aarch64),
    };
    (triple, host, other, other_arch)
}

#[test]
fn a_library_cargo_built_for_another_architecture_is_filed_under_that_architecture() {
    let Some(os) = Os::host() else {
        return;
    };
    if os == Os::Macos {
        // One folder holds every architecture there, and the bundle would
        // be signed, which a few bytes of text do not survive.
        return;
    }
    let _one_at_a_time = one_at_a_time();
    let (_, host, other, other_arch) = host_and_other(os);
    let reported = Reported::new("target-other-arch", os, &Path::new(other).join("debug"));

    let bundled = reported.bundle(&[]);
    assert_eq!(bundled.status.code(), Some(0), "{}", stderr(&bundled));

    // Not under this machine's architecture, which is where it went before.
    let wrong = reported.arch_folder(os, host.arch);
    assert!(
        !wrong.exists(),
        "cargo reported the library in target/{other}/debug, and it was bundled as a library for \
         this machine in {} (standard output: {})",
        wrong.display(),
        stdout(&bundled).trim()
    );
    // Under the one cargo built it for, and nowhere else.
    let extension = if os == Os::Windows { "vst3" } else { "so" };
    let right = reported
        .arch_folder(os, other_arch)
        .join(format!("foreign.{extension}"));
    assert_eq!(reported.bundled_files(), std::slice::from_ref(&right));
    assert_eq!(fs::read(&right).unwrap(), CODE);
    assert_eq!(fs::read(&reported.library).unwrap(), CODE);
    // And the person is told, since it is not what a bundle here usually is.
    let said = stderr(&bundled);
    assert!(
        said.contains(&format!(
            "note: cargo is set up to build for {} here and not for {}",
            other_arch.name(),
            host.arch.name()
        )),
        "{said}"
    );
    assert!(stdout(&bundled).starts_with("Bundled "));
}

#[test]
fn a_library_in_this_machines_folder_is_bundled_for_this_machine_with_nothing_said() {
    let Some(os) = Os::host() else {
        return;
    };
    if os == Os::Macos {
        return;
    }
    let _one_at_a_time = one_at_a_time();
    let (triple, host, _, _) = host_and_other(os);
    let extension = if os == Os::Windows { "vst3" } else { "so" };
    // Straight in the profile's folder, as with no target named anywhere,
    // and in the folder of this machine's own target, as with a
    // configuration file that names it.
    for (test, place) in [
        ("target-host-plain", PathBuf::from("release")),
        ("target-host-named", Path::new(&triple).join("release")),
    ] {
        let reported = Reported::new(test, os, &place);
        let bundled = reported.bundle(&[]);
        assert_eq!(bundled.status.code(), Some(0), "{}", stderr(&bundled));
        let right = reported
            .arch_folder(os, host.arch)
            .join(format!("foreign.{extension}"));
        assert_eq!(reported.bundled_files(), [right]);
        assert!(!stderr(&bundled).contains("note:"), "{}", stderr(&bundled));
    }
}

#[test]
fn a_library_somewhere_cargo_does_not_build_to_is_bundled_as_expected_and_a_note_says_so() {
    let Some(os) = Os::host() else {
        return;
    };
    if os == Os::Macos {
        return;
    }
    let _one_at_a_time = one_at_a_time();
    let (_, host, other, _) = host_and_other(os);
    // Deeper than cargo's layout goes: neither a profile's folder nor a
    // target's.
    let place = Path::new("somewhere").join(other).join("debug");
    let reported = Reported::new("target-unknown", os, &place);
    let bundled = reported.bundle(&[]);
    assert_eq!(bundled.status.code(), Some(0), "{}", stderr(&bundled));
    assert!(reported.arch_folder(os, host.arch).is_dir());
    let said = stderr(&bundled);
    assert!(said.contains("could not be checked"), "{said}");
    assert!(said.contains("--target"), "{said}");
}

#[test]
fn a_target_cargo_is_set_up_for_that_cannot_be_bundled_is_refused_by_name() {
    let Some(os) = Os::host() else {
        return;
    };
    let _one_at_a_time = one_at_a_time();
    // Targets whose libraries have this system's extension and that the
    // tool does not bundle for.
    let unsupported = match os {
        Os::Windows => "arm64ec-pc-windows-msvc",
        Os::Linux => "riscv64gc-unknown-linux-gnu",
        Os::Macos => "aarch64-apple-ios",
    };
    let place = Path::new(unsupported).join("debug");
    let reported = Reported::new("target-unsupported", os, &place);
    let refused = reported.bundle(&[]);
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    let said = stderr(&refused);
    assert!(
        said.contains(&format!("Cargo is set up to build for {unsupported}")),
        "{said}"
    );
    assert!(said.contains("build.target"), "{said}");
    assert!(said.contains("--target"), "{said}");
    assert_eq!(stdout(&refused), "");
    assert_eq!(reported.bundled_files(), Vec::<PathBuf>::new());
}

#[test]
fn a_library_reported_for_another_target_than_the_one_asked_for_is_refused() {
    let Some(os) = Os::host() else {
        return;
    };
    let _one_at_a_time = one_at_a_time();
    let (triple, _, other, _) = host_and_other(os);
    let reported = Reported::new("target-not-as-asked", os, &Path::new(other).join("debug"));
    let refused = reported.bundle(&["--target", &triple]);
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    let said = stderr(&refused);
    assert!(
        said.contains(&format!("cargo was asked to build for {triple}")),
        "{said}"
    );
    assert!(said.contains(other), "{said}");
    assert_eq!(stdout(&refused), "");
    assert_eq!(reported.bundled_files(), Vec::<PathBuf>::new());

    // Asked for the target it is reported for, it is bundled. (Not on
    // macOS, where the bundle would then be signed.)
    if os != Os::Macos {
        let bundled = reported.bundle(&["--target", other]);
        assert_eq!(bundled.status.code(), Some(0), "{}", stderr(&bundled));
        assert!(!stderr(&bundled).contains("note:"), "{}", stderr(&bundled));
        assert_eq!(reported.bundled_files().len(), 1);
    }
}
