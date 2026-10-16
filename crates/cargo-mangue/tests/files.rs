//! What ends up on disk: bundles written from a plan, plugins installed
//! and removed, and new crates.
//!
//! `lipo` and `codesign` exist only on macOS, so the macOS path is driven
//! here with stand-ins that record how they were called. A stand-in for
//! `lipo` writes its inputs one after the other into its output, which is
//! enough to see that the right libraries went into the right file.

mod common;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use cargo_mangue::bundle::{self, codesign_command, lipo_command};
use cargo_mangue::install;
use cargo_mangue::layout::{BundlePlan, BundleSpec, plan};
use cargo_mangue::platform::{Arch, Format, Os};
use cargo_mangue::scaffold::{self, Kind, NewPlugin};
use cargo_mangue::tools::{Tools, find_on_path};
use common::Scratch;

/// Stand-ins for the programs of Apple's developer tools.
#[derive(Default)]
struct FakeTools {
    installed: Vec<&'static str>,
    /// Makes this program fail when run.
    broken: Option<&'static str>,
    /// Every run, as the program's name followed by its arguments.
    runs: Vec<Vec<String>>,
}

impl FakeTools {
    fn with(installed: &[&'static str]) -> Self {
        Self {
            installed: installed.to_vec(),
            ..Self::default()
        }
    }
}

impl Tools for FakeTools {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.installed
            .iter()
            .find(|name| **name == program)
            .map(|name| Path::new("/fake/bin").join(name))
    }

    fn run(&mut self, path: &Path, args: &[OsString]) -> Result<(), String> {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        self.runs
            .push(std::iter::once(name.clone()).chain(args.clone()).collect());
        if self.broken == Some(name.as_str()) {
            return Err(format!("{name} ended with exit status 1: it is broken"));
        }
        if name == "lipo" {
            // lipo -create -output <out> <in>...
            let mut merged = Vec::new();
            for input in &args[3..] {
                merged.extend(fs::read(input).map_err(|error| error.to_string())?);
            }
            fs::write(&args[2], merged).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn planned(format: Format, os: Os, archs: &[Arch]) -> BundlePlan {
    plan(&BundleSpec {
        format,
        os,
        archs: archs.to_vec(),
        name: "Mangue Pluck".to_owned(),
        identifier: "org.manguebyte.pluck".to_owned(),
        version: "0.1.0".to_owned(),
    })
    .unwrap()
}

/// Two small files standing for the library built for each architecture.
fn libraries(scratch: &Scratch) -> Vec<(Arch, PathBuf)> {
    let built = scratch.path().join("built");
    fs::create_dir_all(&built).unwrap();
    let intel = built.join("x86_64.lib");
    let arm = built.join("aarch64.lib");
    fs::write(&intel, b"code for x86_64;").unwrap();
    fs::write(&arm, b"code for aarch64;").unwrap();
    vec![(Arch::X86_64, intel), (Arch::Aarch64, arm)]
}

/// Every file under `root`, as paths with `/` relative to it, sorted.
fn tree(root: &Path) -> Vec<String> {
    fn walk(folder: &Path, prefix: &str, found: &mut Vec<String>) {
        for entry in fs::read_dir(folder).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_str().unwrap());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), found);
            } else {
                found.push(name);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, "", &mut found);
    found.sort();
    found
}

#[test]
fn a_plan_is_written_file_for_file_on_every_system() {
    let scratch = Scratch::new("write");
    let libraries = libraries(&scratch);
    let mut cases = 0;
    for os in Os::ALL {
        for format in Format::DEFAULT {
            let plan = planned(format, os, &[Arch::X86_64]);
            let out = scratch.path().join(format!("{os:?}-{format:?}"));
            // On macOS with neither lipo nor codesign: still written.
            let mut tools = FakeTools::default();
            let written = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();

            assert_eq!(written.path, out.join(&plan.root));
            assert_eq!(written.path.is_file(), plan.single_file);
            let mut expected: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();
            expected.sort();
            assert_eq!(tree(&out), expected);
            // The library is the library, byte for byte.
            let binary = out.join(&plan.files[0].path);
            assert_eq!(fs::read(binary).unwrap(), b"code for x86_64;");
            assert!(tools.runs.is_empty());
            cases += 1;
        }
    }
    assert_eq!(cases, 6);

    // The macOS extras hold what the plan says.
    let mac = scratch.path().join("Macos-Vst3").join("Mangue Pluck.vst3");
    assert_eq!(fs::read(mac.join("Contents/PkgInfo")).unwrap(), b"BNDL????");
    let plist = fs::read_to_string(mac.join("Contents/Info.plist")).unwrap();
    assert!(plist.contains("<key>CFBundleIdentifier</key><string>org.manguebyte.pluck</string>"));
    assert!(plist.contains("<key>CFBundleVersion</key><string>0.1.0</string>"));
}

#[test]
fn writing_replaces_the_bundle_that_was_there_whatever_it_was() {
    let scratch = Scratch::new("replace");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let mut tools = FakeTools::default();

    // A folder with a file from an older version of the plugin in it.
    let stale = out.join("Mangue Pluck.vst3/Contents/Resources/old.txt");
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, "old").unwrap();
    let vst3 = planned(Format::Vst3, Os::Windows, &[Arch::X86_64]);
    bundle::write(&vst3, &out, &libraries, &mut tools).unwrap();
    assert_eq!(
        tree(&out),
        ["Mangue Pluck.vst3/Contents/x86_64-win/Mangue Pluck.vst3"]
    );

    // A macOS bundle, a folder, where a Linux .clap, a file, is wanted,
    // and the other way round.
    let mac = planned(Format::Clap, Os::Macos, &[Arch::Aarch64]);
    let linux = planned(Format::Clap, Os::Linux, &[Arch::Aarch64]);
    bundle::write(&mac, &out, &libraries, &mut tools).unwrap();
    assert!(out.join("Mangue Pluck.clap").is_dir());
    bundle::write(&linux, &out, &libraries, &mut tools).unwrap();
    assert!(out.join("Mangue Pluck.clap").is_file());
    bundle::write(&mac, &out, &libraries, &mut tools).unwrap();
    assert!(out.join("Mangue Pluck.clap/Contents/Info.plist").is_file());

    // What is beside the bundle is left alone.
    assert!(out.join("Mangue Pluck.vst3").is_dir());
}

#[test]
fn on_macos_the_libraries_are_merged_with_lipo_and_the_bundle_is_signed() {
    let scratch = Scratch::new("universal");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let plan = planned(Format::Vst3, Os::Macos, &[Arch::X86_64, Arch::Aarch64]);
    let mut tools = FakeTools::with(&["lipo", "codesign"]);

    let written = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
    assert!(written.notes.is_empty());
    let bundle = out.join("Mangue Pluck.vst3");
    let binary = bundle.join("Contents").join("MacOS").join("Mangue Pluck");
    let text = |path: &Path| path.to_str().unwrap().to_owned();
    assert_eq!(
        tools.runs,
        [
            vec![
                "lipo".to_owned(),
                "-create".to_owned(),
                "-output".to_owned(),
                text(&binary),
                text(&libraries[0].1),
                text(&libraries[1].1),
            ],
            // Signed last, once everything is in place.
            vec![
                "codesign".to_owned(),
                "--force".to_owned(),
                "--sign".to_owned(),
                "-".to_owned(),
                text(&bundle),
            ],
        ]
    );
    // Both architectures, in the order the targets were given.
    assert_eq!(
        fs::read(&binary).unwrap(),
        b"code for x86_64;code for aarch64;"
    );
    assert_eq!(
        tree(&bundle),
        [
            "Contents/Info.plist",
            "Contents/MacOS/Mangue Pluck",
            "Contents/PkgInfo"
        ]
    );
}

#[test]
fn one_architecture_on_macos_needs_no_lipo_and_is_still_signed() {
    let scratch = Scratch::new("single-mac");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let plan = planned(Format::Clap, Os::Macos, &[Arch::Aarch64]);
    let mut tools = FakeTools::with(&["codesign"]);
    let written = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
    assert!(written.notes.is_empty());
    assert_eq!(tools.runs.len(), 1);
    assert_eq!(tools.runs[0][..4], ["codesign", "--force", "--sign", "-"]);
    let binary = out.join("Mangue Pluck.clap/Contents/MacOS/Mangue Pluck");
    assert_eq!(fs::read(binary).unwrap(), b"code for aarch64;");
}

#[test]
fn without_codesign_the_bundle_is_written_and_a_note_says_it_is_unsigned() {
    let scratch = Scratch::new("unsigned");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let plan = planned(Format::Clap, Os::Macos, &[Arch::Aarch64]);
    let mut tools = FakeTools::default();
    let written = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
    assert!(written.path.join("Contents/MacOS/Mangue Pluck").is_file());
    assert_eq!(written.notes.len(), 1);
    let note = &written.notes[0];
    assert!(note.contains("codesign was not found"), "{note}");
    assert!(note.contains("is not signed"), "{note}");
    assert!(note.contains("codesign --force --sign -"), "{note}");

    // Other systems have nothing to sign and nothing to say.
    let plan = planned(Format::Clap, Os::Linux, &[Arch::Aarch64]);
    let written = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
    assert!(written.notes.is_empty());
}

#[test]
fn without_lipo_a_universal_binary_is_an_error_that_says_what_to_do() {
    let scratch = Scratch::new("no-lipo");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let plan = planned(Format::Clap, Os::Macos, &[Arch::X86_64, Arch::Aarch64]);
    let mut tools = FakeTools::with(&["codesign"]);
    let error = bundle::write(&plan, &out, &libraries, &mut tools).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("lipo was not found"), "{error}");
    assert!(
        error.message().contains("xcode-select --install"),
        "{error}"
    );
    assert!(
        error.message().contains("one --target at a time"),
        "{error}"
    );
    // Nothing was signed as if it were complete.
    assert!(tools.runs.is_empty());
}

#[test]
fn a_tool_that_fails_is_an_error_with_what_it_said() {
    let scratch = Scratch::new("broken-tools");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let plan = planned(Format::Vst3, Os::Macos, &[Arch::X86_64, Arch::Aarch64]);

    for (broken, runs) in [("lipo", 1), ("codesign", 2)] {
        let mut tools = FakeTools::with(&["lipo", "codesign"]);
        tools.broken = Some(broken);
        let error = bundle::write(&plan, &out, &libraries, &mut tools).unwrap_err();
        assert_eq!(error.exit_code(), 1);
        assert!(error.message().contains(broken), "{error}");
        assert!(error.message().contains("it is broken"), "{error}");
        assert_eq!(tools.runs.len(), runs);
    }
}

#[test]
fn a_library_that_was_not_built_is_an_error_and_not_an_empty_bundle() {
    let scratch = Scratch::new("missing-library");
    let mut libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    let mut tools = FakeTools::default();

    // Built for one architecture, asked to bundle the other.
    libraries.truncate(1);
    let plan = planned(Format::Vst3, Os::Linux, &[Arch::Aarch64]);
    let error = bundle::write(&plan, &out, &libraries, &mut tools).unwrap_err();
    assert!(error.message().contains("aarch64"), "{error}");

    // Built, and then gone before it could be copied.
    fs::remove_file(&libraries[0].1).unwrap();
    let plan = planned(Format::Clap, Os::Linux, &[Arch::X86_64]);
    let error = bundle::write(&plan, &out, &libraries, &mut tools).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("x86_64.lib"), "{error}");
}

#[test]
fn the_commands_for_the_apple_tools_are_spelled_out() {
    let lipo = lipo_command(
        &[PathBuf::from("a/libx.dylib"), PathBuf::from("b/libx.dylib")],
        Path::new("X.clap/Contents/MacOS/X"),
    );
    assert_eq!(lipo.program, "lipo");
    assert_eq!(
        lipo.args,
        [
            "-create",
            "-output",
            "X.clap/Contents/MacOS/X",
            "a/libx.dylib",
            "b/libx.dylib"
        ]
        .map(OsString::from)
    );
    let codesign = codesign_command(Path::new("out/My Plugin.vst3"));
    assert_eq!(codesign.program, "codesign");
    assert_eq!(
        codesign.args,
        ["--force", "--sign", "-", "out/My Plugin.vst3"].map(OsString::from)
    );
}

#[test]
fn a_plugin_is_installed_replaced_and_removed() {
    let scratch = Scratch::new("install");
    let libraries = libraries(&scratch);
    let out = scratch.path().join("out");
    // A folder that does not exist yet, as on a machine with no plugins.
    let plugins = scratch.path().join("home").join(".vst3");
    let mut tools = FakeTools::default();

    for (format, os, name) in [
        (Format::Vst3, Os::Linux, "Mangue Pluck.vst3"),
        (Format::Clap, Os::Linux, "Mangue Pluck.clap"),
        (Format::Clap, Os::Macos, "Mangue Pluck.clap"),
    ] {
        let plan = planned(format, os, &[Arch::X86_64]);
        let built = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();

        let installed = install::install(&built.path, &plugins).unwrap();
        assert_eq!(installed, plugins.join(name));
        // The installed copy is the bundle, file for file.
        if plan.single_file {
            assert_eq!(
                fs::read(&installed).unwrap(),
                fs::read(&built.path).unwrap()
            );
        } else {
            assert_eq!(tree(&installed), tree(&built.path));
            for file in tree(&installed) {
                assert_eq!(
                    fs::read(installed.join(&file)).unwrap(),
                    fs::read(built.path.join(&file)).unwrap()
                );
            }
        }
        // The bundle it was copied from is still there.
        assert!(built.path.exists());

        // Installing again replaces: nothing of the old copy survives.
        if !plan.single_file {
            fs::write(installed.join("Contents").join("stale.txt"), "old").unwrap();
        }
        let again = install::install(&built.path, &plugins).unwrap();
        assert_eq!(again, installed);
        if !plan.single_file {
            assert_eq!(tree(&installed), tree(&built.path));
        }

        assert_eq!(
            install::uninstall(&plugins, name).unwrap(),
            Some(installed.clone())
        );
        assert!(!installed.exists());
        // Removing what is not there is not an error.
        assert_eq!(install::uninstall(&plugins, name).unwrap(), None);
    }
    assert_eq!(tree(&plugins), Vec::<String>::new());
    assert_eq!(
        install::uninstall(&scratch.path().join("no-such-folder"), "X.clap").unwrap(),
        None
    );
}

#[test]
fn installing_something_that_is_not_there_is_an_error() {
    let scratch = Scratch::new("install-missing");
    let plugins = scratch.path().join("plugins");
    let error = install::install(&scratch.path().join("Nothing.clap"), &plugins).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("Nothing.clap"), "{error}");
    assert_eq!(tree(&plugins), Vec::<String>::new());
}

/// Found in review: `cargo mangue install --out <the plugin folder>` wrote
/// the bundle over the installed one, removed it as "the copy that is
/// already installed", and then failed to copy what it had just removed.
/// The plugin was gone and the command ended with an error about a folder
/// that could not be written to.
#[test]
fn a_bundle_written_straight_into_the_plugin_folder_is_installed_where_it_is() {
    let scratch = Scratch::new("install-in-place");
    let libraries = libraries(&scratch);
    let home = scratch.path().join("home");
    let plugins = home.join("Plug-Ins");
    let mut tools = FakeTools::default();

    for (format, os) in [
        (Format::Vst3, Os::Linux),
        (Format::Clap, Os::Linux),
        (Format::Vst3, Os::Macos),
    ] {
        let plan = planned(format, os, &[Arch::X86_64]);
        // Installed the ordinary way first, from a folder of its own.
        let out = scratch.path().join("out");
        let elsewhere = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
        let installed = install::install(&elsewhere.path, &plugins).unwrap();
        let contents = |bundle: &Path| {
            if plan.single_file {
                vec![(String::new(), fs::read(bundle).unwrap())]
            } else {
                tree(bundle)
                    .into_iter()
                    .map(|file| {
                        let bytes = fs::read(bundle.join(&file)).unwrap();
                        (file, bytes)
                    })
                    .collect()
            }
        };
        let expected = contents(&elsewhere.path);
        assert_eq!(contents(&installed), expected);

        // Then written into the plugin folder itself, which is what
        // `--out <the plugin folder>` does, and installed from there.
        let in_place = bundle::write(&plan, &plugins, &libraries, &mut tools).unwrap();
        assert_eq!(in_place.path, installed);
        assert_eq!(
            install::install(&in_place.path, &plugins).unwrap(),
            installed
        );
        assert_eq!(contents(&installed), expected);

        // The same place written another way is still the same place:
        // through `..`, and in another case where the file system does not
        // tell cases apart.
        let roundabout = plugins.join("..").join("Plug-Ins").join(&plan.root);
        assert_eq!(install::install(&roundabout, &plugins).unwrap(), installed);
        assert_eq!(contents(&installed), expected);
        let shouted = home.join("PLUG-INS").join(&plan.root);
        if shouted.exists() {
            assert_eq!(install::install(&shouted, &plugins).unwrap(), installed);
            assert_eq!(contents(&installed), expected);
        }

        // A bundle of the same name from somewhere else still replaces it.
        if !plan.single_file {
            fs::write(installed.join("Contents").join("stale.txt"), "old").unwrap();
        }
        install::install(&elsewhere.path, &plugins).unwrap();
        assert_eq!(contents(&installed), expected);

        assert!(install::uninstall(&plugins, &plan.root).unwrap().is_some());
    }
    assert_eq!(tree(&plugins), Vec::<String>::new());
}

/// The same mistake one folder further in: `--out` naming the installed
/// bundle itself. Replacing the installed bundle would remove the new one
/// inside it, so nothing is removed and the error says where to write.
#[test]
fn a_bundle_inside_the_installed_one_it_would_replace_is_refused_and_nothing_is_removed() {
    let scratch = Scratch::new("install-nested");
    let libraries = libraries(&scratch);
    let plugins = scratch.path().join("home").join(".vst3");
    let mut tools = FakeTools::default();
    let plan = planned(Format::Vst3, Os::Linux, &[Arch::X86_64]);

    let out = scratch.path().join("out");
    let elsewhere = bundle::write(&plan, &out, &libraries, &mut tools).unwrap();
    let installed = install::install(&elsewhere.path, &plugins).unwrap();
    let before = tree(&installed);

    let nested = bundle::write(&plan, &installed, &libraries, &mut tools).unwrap();
    assert_eq!(nested.path, installed.join(&plan.root));
    let error = install::install(&nested.path, &plugins).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("one inside the other"), "{error}");
    assert!(error.message().contains("--out"), "{error}");
    // Both are still there, the installed plugin with every file it had.
    assert!(nested.path.exists());
    let after = tree(&installed);
    assert!(before.iter().all(|file| after.contains(file)), "{after:?}");
}

#[test]
fn programs_are_found_in_the_folders_of_path() {
    let scratch = Scratch::new("path");
    let first = scratch.path().join("first");
    let second = scratch.path().join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(second.join("clap-validator")).unwrap();
    for file in [second.join("validator"), second.join("lipo.exe")] {
        fs::write(&file, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path = std::env::join_paths([&first, &second, &scratch.path().join("missing")]).unwrap();
    let path = Some(path.as_os_str());

    assert_eq!(
        find_on_path("validator", path, false),
        Some(second.join("validator"))
    );
    // As Windows does, a name without an extension is tried with .exe.
    assert_eq!(
        find_on_path("lipo", path, true),
        Some(second.join("lipo.exe"))
    );
    assert_eq!(find_on_path("lipo", path, false), None);
    assert_eq!(
        find_on_path("lipo.exe", path, true),
        Some(second.join("lipo.exe"))
    );
    // A folder of that name is not a program.
    assert_eq!(find_on_path("clap-validator", path, false), None);
    assert_eq!(find_on_path("codesign", path, true), None);
    assert_eq!(find_on_path("validator", None, false), None);
    assert_eq!(
        find_on_path("validator", Some(OsString::new().as_os_str()), false),
        None
    );
}

#[test]
fn a_new_crate_is_written_and_nothing_is_ever_overwritten() {
    let scratch = Scratch::new("new");
    let files = scaffold::project(&NewPlugin::new("my-gain", Kind::Effect).unwrap()).unwrap();

    let dir = scratch.path().join("my-gain");
    scaffold::write_project(&dir, &files).unwrap();
    assert_eq!(tree(&dir), [".gitignore", "Cargo.toml", "src/lib.rs"]);
    for file in &files {
        assert_eq!(
            fs::read_to_string(dir.join(file.path)).unwrap(),
            file.contents
        );
    }

    // A second time into the same folder: refused, and nothing touched.
    fs::write(dir.join("src").join("lib.rs"), "// my work").unwrap();
    let error = scaffold::write_project(&dir, &files).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("already exists"), "{error}");
    assert_eq!(
        fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "// my work"
    );

    // An empty folder is as good as none.
    let empty = scratch.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    scaffold::write_project(&empty, &files).unwrap();
    assert_eq!(tree(&empty), [".gitignore", "Cargo.toml", "src/lib.rs"]);

    // A file in the way is not a folder to write into.
    let file = scratch.path().join("taken");
    fs::write(&file, "x").unwrap();
    let error = scaffold::write_project(&file, &files).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}

#[test]
fn a_workspace_around_the_new_crate_is_noticed() {
    let scratch = Scratch::new("workspace");
    let workspace = scratch.path().join("ws");
    let package = workspace.join("tools").join("plain");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "# a workspace\n[workspace]\nmembers = [\"plugins/*\"]\n",
    )
    .unwrap();
    // A package on the way up that is no workspace is passed by.
    fs::write(package.join("Cargo.toml"), "[package]\nname = \"plain\"\n").unwrap();

    assert_eq!(
        scaffold::enclosing_workspace(&package.join("my-gain")),
        Some(workspace.join("Cargo.toml"))
    );
    assert_eq!(
        scaffold::enclosing_workspace(&workspace.join("plugins").join("my-gain")),
        Some(workspace.join("Cargo.toml"))
    );
    // The crate's own manifest does not count: only what is above it.
    let own = scaffold::enclosing_workspace(&workspace);
    assert_ne!(own, Some(workspace.join("Cargo.toml")));
}
