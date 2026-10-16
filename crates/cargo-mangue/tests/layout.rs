//! The bundle layout of every system and format, compared with what
//! `scripts/bundle-clap.sh` and `scripts/bundle-vst3.sh` produce.
//!
//! Those two scripts are the layouts that the CLAP project's validator and
//! Steinberg's accept in CI on Windows, Linux and macOS. The tool has to
//! write the same files. Because a layout is a function of a description
//! and touches no disk, all three systems are checked here on whichever
//! one the tests run on.
//!
//! The comparison is made twice. The expected paths and property lists are
//! written out below, taken from the scripts by hand. And while the scripts
//! are still in the repository, the last tests read them and check that
//! every path the tool plans is a destination a script writes to, and that
//! the property lists are the scripts' own text, byte for byte.

use std::fs;
use std::path::Path;

use cargo_mangue::layout::{
    BundlePlan, BundleSpec, Contents, PKG_INFO, info_plist, plan, vst3_arch_folder,
};
use cargo_mangue::platform::{Arch, Format, Os};

const NAME: &str = "Mangue Pluck";
const CRATE: &str = "mangue-pluck";
/// What the scripts write as the identifier: `org.manguebyte.${crate}`.
const IDENTIFIER: &str = "org.manguebyte.mangue-pluck";
/// What the scripts write as the version.
const VERSION: &str = "1.0";

fn spec(format: Format, os: Os, archs: &[Arch]) -> BundleSpec {
    BundleSpec {
        format,
        os,
        archs: archs.to_vec(),
        name: NAME.to_owned(),
        identifier: IDENTIFIER.to_owned(),
        version: VERSION.to_owned(),
    }
}

fn planned(format: Format, os: Os, archs: &[Arch]) -> BundlePlan {
    plan(&spec(format, os, archs)).unwrap()
}

/// The planned paths with what goes in each, property lists as text.
fn files(plan: &BundlePlan) -> Vec<(&str, &Contents)> {
    plan.files
        .iter()
        .map(|file| (file.path.as_str(), &file.contents))
        .collect()
}

/// `Info.plist` as `bundle-clap.sh` writes it for "Mangue Pluck".
const CLAP_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Mangue Pluck</string>
    <key>CFBundleIdentifier</key><string>org.manguebyte.mangue-pluck</string>
    <key>CFBundleName</key><string>Mangue Pluck</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleVersion</key><string>1.0</string>
</dict>
</plist>
"#;

/// `Info.plist` as `bundle-vst3.sh` writes it for "Mangue Pluck".
const VST3_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Mangue Pluck</string>
    <key>CFBundleIdentifier</key><string>org.manguebyte.mangue-pluck</string>
    <key>CFBundleName</key><string>Mangue Pluck</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleSignature</key><string>????</string>
    <key>CFBundleVersion</key><string>1.0</string>
</dict>
</plist>
"#;

#[test]
fn a_clap_on_linux_and_windows_is_the_library_under_another_name() {
    // bundle-clap.sh: cp "target/release/lib${library}.so" "target/bundled/${name}.clap"
    // and:            cp "target/release/${library}.dll"   "target/bundled/${name}.clap"
    for os in [Os::Linux, Os::Windows] {
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::X86] {
            let plan = planned(Format::Clap, os, &[arch]);
            assert_eq!(plan.root, "Mangue Pluck.clap");
            assert!(plan.single_file);
            assert!(!plan.sign);
            assert_eq!(
                files(&plan),
                [("Mangue Pluck.clap", &Contents::Library(arch))]
            );
        }
    }
}

#[test]
fn a_vst3_on_linux_has_a_folder_named_after_the_machine() {
    // bundle-vst3.sh: cp ... "$bundle/Contents/${machine}-linux/${name}.so"
    // with machine as `uname -m` prints it: x86_64, aarch64.
    for (arch, folder) in [
        (Arch::X86_64, "x86_64-linux"),
        (Arch::Aarch64, "aarch64-linux"),
    ] {
        let plan = planned(Format::Vst3, Os::Linux, &[arch]);
        assert_eq!(plan.root, "Mangue Pluck.vst3");
        assert!(!plan.single_file && !plan.sign);
        let path = format!("Mangue Pluck.vst3/Contents/{folder}/Mangue Pluck.so");
        assert_eq!(files(&plan), [(path.as_str(), &Contents::Library(arch))]);
    }
}

#[test]
fn a_vst3_on_windows_has_a_folder_named_after_the_machine() {
    // bundle-vst3.sh: cp ... "$bundle/Contents/${machine}-win/${name}.vst3"
    // with machine x86_64, arm64 or x86.
    for (arch, folder) in [
        (Arch::X86_64, "x86_64-win"),
        (Arch::Aarch64, "arm64-win"),
        (Arch::X86, "x86-win"),
    ] {
        let plan = planned(Format::Vst3, Os::Windows, &[arch]);
        assert_eq!(plan.root, "Mangue Pluck.vst3");
        assert!(!plan.single_file && !plan.sign);
        let path = format!("Mangue Pluck.vst3/Contents/{folder}/Mangue Pluck.vst3");
        assert_eq!(files(&plan), [(path.as_str(), &Contents::Library(arch))]);
    }
}

#[test]
fn a_clap_on_macos_is_a_bundle_with_a_property_list() {
    // bundle-clap.sh: cp ... "$bundle/Contents/MacOS/${name}"
    //                 cat > "$bundle/Contents/Info.plist"
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let plan = planned(Format::Clap, Os::Macos, &[arch]);
        assert_eq!(plan.root, "Mangue Pluck.clap");
        assert!(!plan.single_file);
        assert!(plan.sign);
        assert_eq!(
            files(&plan),
            [
                (
                    "Mangue Pluck.clap/Contents/MacOS/Mangue Pluck",
                    &Contents::Library(arch)
                ),
                (
                    "Mangue Pluck.clap/Contents/Info.plist",
                    &Contents::Bytes(CLAP_PLIST.as_bytes().to_vec())
                ),
            ]
        );
    }
}

#[test]
fn a_vst3_on_macos_is_a_bundle_with_a_property_list_and_pkginfo() {
    // bundle-vst3.sh: cp ... "$bundle/Contents/MacOS/${name}"
    //                 printf 'BNDL????' > "$bundle/Contents/PkgInfo"
    //                 cat > "$bundle/Contents/Info.plist"
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let plan = planned(Format::Vst3, Os::Macos, &[arch]);
        assert_eq!(plan.root, "Mangue Pluck.vst3");
        assert!(!plan.single_file);
        assert!(plan.sign);
        assert_eq!(
            files(&plan),
            [
                (
                    "Mangue Pluck.vst3/Contents/MacOS/Mangue Pluck",
                    &Contents::Library(arch)
                ),
                (
                    "Mangue Pluck.vst3/Contents/PkgInfo",
                    &Contents::Bytes(b"BNDL????".to_vec())
                ),
                (
                    "Mangue Pluck.vst3/Contents/Info.plist",
                    &Contents::Bytes(VST3_PLIST.as_bytes().to_vec())
                ),
            ]
        );
    }
    assert_eq!(PKG_INFO.len(), 8);
}

#[test]
fn several_architectures_on_macos_become_one_universal_binary() {
    let both = [Arch::X86_64, Arch::Aarch64];
    for format in Format::DEFAULT {
        let single = planned(format, Os::Macos, &[Arch::Aarch64]);
        let universal = planned(format, Os::Macos, &both);
        // The same files in the same places: only the binary differs.
        assert_eq!(universal.files.len(), single.files.len());
        for (one, other) in universal.files.iter().zip(&single.files) {
            assert_eq!(one.path, other.path);
        }
        assert_eq!(
            universal.files[0].contents,
            Contents::Universal(both.to_vec())
        );
        assert_eq!(universal.files[1..], single.files[1..]);
    }
}

#[test]
fn several_architectures_elsewhere_share_a_vst3_and_cannot_share_a_clap() {
    let plan = planned(Format::Vst3, Os::Windows, &[Arch::X86_64, Arch::Aarch64]);
    assert_eq!(
        files(&plan),
        [
            (
                "Mangue Pluck.vst3/Contents/x86_64-win/Mangue Pluck.vst3",
                &Contents::Library(Arch::X86_64)
            ),
            (
                "Mangue Pluck.vst3/Contents/arm64-win/Mangue Pluck.vst3",
                &Contents::Library(Arch::Aarch64)
            ),
        ]
    );
    let plan = planned(Format::Vst3, Os::Linux, &[Arch::Aarch64, Arch::X86_64]);
    assert_eq!(
        files(&plan),
        [
            (
                "Mangue Pluck.vst3/Contents/aarch64-linux/Mangue Pluck.so",
                &Contents::Library(Arch::Aarch64)
            ),
            (
                "Mangue Pluck.vst3/Contents/x86_64-linux/Mangue Pluck.so",
                &Contents::Library(Arch::X86_64)
            ),
        ]
    );

    for os in [Os::Windows, Os::Linux] {
        let error = plan_error(Format::Clap, os, &[Arch::X86_64, Arch::Aarch64]);
        assert!(error.contains("one architecture"), "{error}");
        assert!(error.contains("--format vst3"), "{error}");
    }
}

fn plan_error(format: Format, os: Os, archs: &[Arch]) -> String {
    plan(&spec(format, os, archs)).unwrap_err().to_string()
}

#[test]
fn a_bundle_needs_an_architecture_and_each_only_once() {
    for os in Os::ALL {
        for format in Format::DEFAULT {
            assert!(plan(&spec(format, os, &[])).is_err());
            let error = plan_error(format, os, &[Arch::X86_64, Arch::X86_64]);
            assert!(error.contains("once"), "{error}");
        }
    }
}

#[test]
fn every_planned_path_stays_inside_its_bundle() {
    let archs = [Arch::X86_64, Arch::Aarch64, Arch::X86];
    for os in Os::ALL {
        for format in Format::DEFAULT {
            for arch in archs {
                let plan = planned(format, os, &[arch]);
                assert_eq!(plan.root, format!("{NAME}.{}", format.extension()));
                assert_eq!(plan.sign, os == Os::Macos);
                assert!(!plan.files.is_empty());
                for file in &plan.files {
                    let inside =
                        file.path == plan.root || file.path.starts_with(&format!("{}/", plan.root));
                    assert!(inside, "{} is outside {}", file.path, plan.root);
                    assert!(!file.path.contains('\\'), "{}", file.path);
                    assert!(
                        file.path
                            .split('/')
                            .all(|part| part != ".." && !part.is_empty())
                    );
                }
                // Exactly one file is the plugin's code.
                let binaries = plan
                    .files
                    .iter()
                    .filter(|file| !matches!(file.contents, Contents::Bytes(_)))
                    .count();
                assert_eq!(binaries, 1);
                assert_eq!(plan.single_file, plan.files[0].path == plan.root);
            }
        }
    }
}

#[test]
fn a_name_that_would_leave_the_output_folder_is_refused_for_every_layout() {
    for name in ["../Pluck", "a/b", "a\\b", "..", ""] {
        for os in Os::ALL {
            for format in Format::DEFAULT {
                let mut bad = spec(format, os, &[Arch::X86_64]);
                bad.name = name.to_owned();
                assert_eq!(plan(&bad).unwrap_err().exit_code(), 2, "{name:?}");
            }
        }
    }
    let mut bad = spec(Format::Vst3, Os::Macos, &[Arch::Aarch64]);
    bad.identifier = "not an identifier".to_owned();
    assert_eq!(plan(&bad).unwrap_err().exit_code(), 2);
}

#[test]
fn the_identifier_and_version_of_the_plugin_reach_the_property_list() {
    let mut spec = spec(Format::Vst3, Os::Macos, &[Arch::Aarch64]);
    spec.name = "Órgão & Côro".to_owned();
    spec.identifier = "br.com.exemplo.orgao".to_owned();
    spec.version = "2.3.4".to_owned();
    let plan = plan(&spec).unwrap();
    assert_eq!(
        plan.files[0].path,
        "Órgão & Côro.vst3/Contents/MacOS/Órgão & Côro"
    );
    let Contents::Bytes(plist) = &plan.files[2].contents else {
        panic!("the property list is not text");
    };
    let plist = String::from_utf8(plist.clone()).unwrap();
    assert!(plist.contains("<key>CFBundleExecutable</key><string>Órgão &amp; Côro</string>"));
    assert!(plist.contains("<key>CFBundleName</key><string>Órgão &amp; Côro</string>"));
    assert!(plist.contains("<key>CFBundleIdentifier</key><string>br.com.exemplo.orgao</string>"));
    assert!(plist.contains("<key>CFBundleVersion</key><string>2.3.4</string>"));
}

/// Found in review: `plan` checked the identifier for every system, and
/// only the macOS `Info.plist` holds one. A plugin whose `Cargo.toml` sets
/// `id = "com.example.my_gain"`, a legal CLAP id and the one `cargo mangue
/// new` tells its reader to keep in step with the id in the source, could
/// not be bundled for Windows or Linux either: `cargo mangue bundle` ended
/// with exit code 2 before building anything.
#[test]
fn an_identifier_only_macos_writes_does_not_stop_a_bundle_for_windows_or_linux() {
    // The last ones could not be written even on macOS.
    for identifier in ["com.example.my_gain", "", "com..gain", "no dots at all"] {
        for os in [Os::Windows, Os::Linux] {
            for format in Format::DEFAULT {
                let mut described = spec(format, os, &[Arch::X86_64]);
                described.identifier = identifier.to_owned();
                let bundle = plan(&described).unwrap_or_else(|error| {
                    panic!(
                        "a {} for {} holds no identifier, and was refused for {identifier:?}: \
                         {error}",
                        format.name(),
                        os.name()
                    )
                });
                // Nothing in it could have used the identifier, and it is
                // the bundle any other identifier gives.
                assert!(
                    bundle
                        .files
                        .iter()
                        .all(|file| matches!(file.contents, Contents::Library(_)))
                );
                assert_eq!(bundle, planned(format, os, &[Arch::X86_64]));
            }
        }
    }
}

#[test]
fn on_macos_an_underscore_in_the_identifier_is_written_as_a_hyphen() {
    for format in Format::DEFAULT {
        let mut described = spec(format, Os::Macos, &[Arch::Aarch64]);
        described.identifier = "com.example.my_gain".to_owned();
        let with_underscore = plan(&described).unwrap();
        described.identifier = "com.example.my-gain".to_owned();
        let with_hyphen = plan(&described).unwrap();
        assert_eq!(with_underscore, with_hyphen);

        let Contents::Bytes(plist) = &with_underscore.files.last().unwrap().contents else {
            panic!("the property list is not text");
        };
        let plist = String::from_utf8(plist.clone()).unwrap();
        assert!(
            plist.contains("<key>CFBundleIdentifier</key><string>com.example.my-gain</string>"),
            "{plist}"
        );

        // What cannot be a bundle identifier is still refused there, by the
        // identifier as it was given.
        for bad in ["", "com..gain", "com.my gain", "com/gain", "com._.é"] {
            described.identifier = bad.to_owned();
            let error = plan(&described).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{bad:?}");
            assert!(error.message().contains("macOS"), "{error}");
            assert!(error.message().contains(&format!("\"{bad}\"")), "{error}");
        }
    }
}

/// The one place the tool and `bundle-vst3.sh` differ, on purpose.
///
/// For 32-bit x86 the script maps `uname -m` to `x86` on every system, and
/// so would write `x86-linux`. The VST 3 documentation names that folder
/// `i386-linux`, and that is what a host looks for.
#[test]
fn thirty_two_bit_linux_follows_the_vst3_documentation_and_not_the_script() {
    let plan = planned(Format::Vst3, Os::Linux, &[Arch::X86]);
    assert_eq!(
        plan.files[0].path,
        "Mangue Pluck.vst3/Contents/i386-linux/Mangue Pluck.so"
    );
}

/// Reads one of the bundling scripts, or returns `None` once they have been
/// retired in favour of this tool. The expectations above do not depend on
/// them.
fn script(name: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scripts")
        .join(name);
    match fs::read_to_string(&path) {
        Ok(text) => Some(text.replace("\r\n", "\n")),
        Err(_) => {
            eprintln!(
                "{} is not there; comparing with it is skipped",
                path.display()
            );
            None
        }
    }
}

/// The property list a script writes: the text between `<<PLIST` and
/// `PLIST`, with the shell variables filled in for "Mangue Pluck".
fn script_plist(script: &str) -> String {
    let (_, after) = script.split_once("<<PLIST\n").expect("no property list");
    let (body, _) = after.split_once("\nPLIST\n").expect("no end of the list");
    format!("{body}\n")
        .replace("${name}", NAME)
        .replace("${crate}", CRATE)
}

/// Writes a planned path the way the scripts spell their destinations.
fn as_in_script(plan: &BundlePlan, path: &str, machine_folder: Option<&str>) -> String {
    let rest = path.strip_prefix(plan.root.as_str()).unwrap();
    let mut spelled = if plan.single_file {
        format!("target/bundled/${{name}}.{}", &plan.root[NAME.len() + 1..])
    } else {
        format!("$bundle{}", rest.replace(NAME, "${name}"))
    };
    if let Some(folder) = machine_folder {
        let system = folder.rsplit('-').next().unwrap();
        spelled = spelled.replace(folder, &format!("${{machine}}-{system}"));
    }
    format!("\"{spelled}\"")
}

#[test]
fn every_planned_path_is_one_the_shell_scripts_write() {
    let (Some(clap), Some(vst3)) = (script("bundle-clap.sh"), script("bundle-vst3.sh")) else {
        return;
    };
    // Where each script puts its bundle.
    assert!(clap.contains("bundle=\"target/bundled/${name}.clap\""));
    assert!(vst3.contains("bundle=\"target/bundled/${name}.vst3\""));
    // How bundle-vst3.sh names machines, which `vst3_arch_folder` repeats.
    assert!(vst3.contains("x86_64 | amd64) machine=\"x86_64\" ;;"));
    assert!(vst3.contains("aarch64 | arm64) machine=\"arm64\" ;;"));
    assert!(vst3.contains("[ \"$machine\" = \"arm64\" ] && machine=\"aarch64\""));
    assert_eq!(vst3_arch_folder(Os::Windows, Arch::X86_64), "x86_64-win");
    assert_eq!(vst3_arch_folder(Os::Windows, Arch::Aarch64), "arm64-win");
    assert_eq!(vst3_arch_folder(Os::Linux, Arch::X86_64), "x86_64-linux");
    assert_eq!(vst3_arch_folder(Os::Linux, Arch::Aarch64), "aarch64-linux");

    let mut checked = 0;
    for os in Os::ALL {
        for (format, script) in [(Format::Clap, &clap), (Format::Vst3, &vst3)] {
            for arch in [Arch::X86_64, Arch::Aarch64] {
                let plan = planned(format, os, &[arch]);
                let machine =
                    (format == Format::Vst3 && os != Os::Macos).then(|| vst3_arch_folder(os, arch));
                for file in &plan.files {
                    let spelled = as_in_script(&plan, &file.path, machine);
                    assert!(
                        script.contains(&spelled),
                        "{} for {os:?} is {spelled}, which the script never writes",
                        file.path
                    );
                    checked += 1;
                }
            }
        }
    }
    // 2 systems x 2 formats x 2 machines of one file each, and on macOS a
    // CLAP of two files and a VST 3 of three, for 2 machines.
    assert_eq!(checked, 8 + 2 * (2 + 3));
}

#[test]
fn the_property_lists_are_the_shell_scripts_own_text() {
    let (Some(clap), Some(vst3)) = (script("bundle-clap.sh"), script("bundle-vst3.sh")) else {
        return;
    };
    assert_eq!(script_plist(&clap), CLAP_PLIST);
    assert_eq!(script_plist(&vst3), VST3_PLIST);
    assert_eq!(
        info_plist(Format::Clap, NAME, IDENTIFIER, VERSION),
        script_plist(&clap)
    );
    assert_eq!(
        info_plist(Format::Vst3, NAME, IDENTIFIER, VERSION),
        script_plist(&vst3)
    );
    assert!(vst3.contains("printf 'BNDL????' > \"$bundle/Contents/PkgInfo\""));
    assert_eq!(PKG_INFO, b"BNDL????");
    // bundle-clap.sh writes no PkgInfo, and neither does the tool.
    assert!(!clap.contains("PkgInfo"));
}

/// An LV2 bundle is a plain folder holding the library, on every system;
/// the Turtle files beside it are added once the plugin is built. On macOS
/// the library, not the folder, is what is signed.
#[test]
fn an_lv2_bundle_is_a_folder_with_the_library_in_it() {
    for (os, library) in [
        (Os::Windows, "Mangue Pluck.lv2/Mangue Pluck.dll"),
        (Os::Linux, "Mangue Pluck.lv2/Mangue Pluck.so"),
        (Os::Macos, "Mangue Pluck.lv2/Mangue Pluck.dylib"),
    ] {
        let plan = planned(Format::Lv2, os, &[Arch::X86_64]);
        assert_eq!(plan.root, "Mangue Pluck.lv2");
        assert!(!plan.single_file);
        assert_eq!(files(&plan), [(library, &Contents::Library(Arch::X86_64))]);
        assert_eq!(plan.library_file_name(), library.rsplit('/').next());
        assert_eq!(plan.sign, os == Os::Macos);
        assert_eq!(plan.signed, library);
    }
    // Two architectures are one universal library on macOS, and refused
    // elsewhere.
    let both = [Arch::X86_64, Arch::Aarch64];
    let mac = planned(Format::Lv2, Os::Macos, &both);
    assert_eq!(mac.files[0].contents, Contents::Universal(both.to_vec()));
    for os in [Os::Windows, Os::Linux] {
        let error = plan(&spec(Format::Lv2, os, &both)).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{error}");
    }
    // The other formats sign the bundle itself.
    let vst3 = planned(Format::Vst3, Os::Macos, &[Arch::Aarch64]);
    assert_eq!(vst3.signed, vst3.root);
}
