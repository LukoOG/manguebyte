//! The `Info.plist` of an Audio Unit's `.component` bundle.
//!
//! macOS finds an Audio Unit by the `AudioComponents` entry of its bundle's
//! `Info.plist`: the unit's type, subtype and manufacturer codes, its name
//! and version, and the name of the factory function the library exports.
//! The codes are worked out from the plugin's id by the plugin's own code,
//! `manguebyte_au::info_plist`, and `export_au!` puts a function that calls
//! it, `__manguebyte_au_info_plist`, at the root of the plugin's crate.
//!
//! As for an LV2 bundle's Turtle files (see [`lv2`](crate::lv2)), this tool
//! writes a small program that depends on the plugin's crate, builds it
//! and runs it, in `<target>/manguebyte/au/<package>/`, and reads back what
//! it wrote. [`codes_from_plist`] reads the codes back out of an
//! `Info.plist`, for `auval`.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::cargo::{BuildRequest, Cargo, Metadata, Package};
use crate::error::Error;
use crate::lv2::{helper_manifest_for, plugin_features};

/// The name of the program's package.
pub const HELPER_PACKAGE: &str = "mangue-au-plist";

/// Largest `Info.plist` read back.
const MAX_PLIST_BYTES: u64 = 1 << 20;

/// The program's source: it writes the `Info.plist` `export_au!` gives into
/// the file named by its fourth argument, for the executable, bundle
/// identifier and version named by the first three.
pub const HELPER_SOURCE: &str = "\
//! Writes the Info.plist of a plugin's Audio Unit bundle. Made by cargo mangue.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [executable, identifier, version, file] = args.as_slice() else {
        eprintln!(\"usage: mangue-au-plist <executable> <identifier> <version> <file>\");
        std::process::exit(2);
    };
    let text = plugin::__manguebyte_au_info_plist(executable, identifier, version);
    if let Err(error) = std::fs::write(file, text) {
        eprintln!(\"{file} could not be written: {error}\");
        std::process::exit(1);
    }
}
";

/// The `--features` the program is run with. See
/// [`lv2::helper_features`](crate::lv2::helper_features), whose rules these
/// are.
///
/// # Errors
///
/// As for `lv2::helper_features`.
pub fn helper_features(request: &BuildRequest) -> Result<Vec<String>, Error> {
    plugin_features(request, "An Audio Unit")
}

/// Where the program's package is written for `package`.
pub fn helper_dir(target_directory: &Path, package: &str) -> PathBuf {
    target_directory.join("manguebyte").join("au").join(package)
}

/// Builds and runs the program for `package`, and returns the text of the
/// bundle's `Info.plist`, for an executable named `executable`, the bundle
/// identifier `identifier` and the bundle version `version`.
///
/// # Errors
///
/// Fails if the program's files cannot be written, if it does not build or
/// does not finish, and if what it wrote is not an `Info.plist` with an
/// `AudioComponents` entry or is larger than 1 MiB.
pub fn info_plist(
    cargo: &Cargo,
    metadata: &Metadata,
    package: &Package,
    request: &BuildRequest,
    executable: &str,
    identifier: &str,
    version: &str,
) -> Result<Vec<u8>, Error> {
    let features = helper_features(request)?;
    let dir = helper_dir(&metadata.target_directory, &package.name);
    let cannot = |what: &Path, error: &dyn std::fmt::Display| {
        Error::failed(format!(
            "The program that writes the Audio Unit's Info.plist could not be set up in {}: \
             {error}.",
            what.display()
        ))
    };
    fs::create_dir_all(dir.join("src")).map_err(|error| cannot(&dir, &error))?;
    let manifest = dir.join("Cargo.toml");
    let text = helper_manifest_for(
        package,
        request.no_default_features,
        HELPER_PACKAGE,
        "the Info.plist of the Audio Unit of",
    );
    fs::write(&manifest, text).map_err(|error| cannot(&manifest, &error))?;
    let main = dir.join("src").join("main.rs");
    fs::write(&main, HELPER_SOURCE).map_err(|error| cannot(&main, &error))?;
    if let Some(root) = &metadata.workspace_root {
        let lock = root.join("Cargo.lock");
        if lock.is_file() {
            fs::copy(&lock, dir.join("Cargo.lock")).map_err(|error| cannot(&lock, &error))?;
        }
    }
    let out = dir.join("Info.plist");
    let _ = fs::remove_file(&out);
    cargo.run_helper(
        &manifest,
        &metadata.target_directory,
        request,
        &features,
        &[
            OsString::from(executable),
            OsString::from(identifier),
            OsString::from(version),
            out.clone().into_os_string(),
        ],
        "The program that writes the Audio Unit's Info.plist did not build or did not finish; \
         its messages are above. The plugin must invoke export_au! (from manguebyte_au) at the \
         root of its crate, and the crate must have the rlib type as well as cdylib.",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(&out)
        .and_then(|file| file.take(MAX_PLIST_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| cannot(&out, &error))?;
    let looks_right = bytes.len() as u64 <= MAX_PLIST_BYTES
        && std::str::from_utf8(&bytes)
            .is_ok_and(|text| text.contains("<key>AudioComponents</key>"));
    if !looks_right {
        return Err(Error::failed(format!(
            "The program that writes the Audio Unit's Info.plist left {}, which is not one.",
            out.display()
        )));
    }
    Ok(bytes)
}

/// The type, subtype and manufacturer codes of the first unit an
/// `Info.plist` lists, as `auval -v` takes them. `None` if it lists none
/// with all three as four-character strings.
///
/// This looks for the three keys and the string after each, as the
/// `Info.plist` that `manguebyte_au` writes has them; it is not a parser
/// of property lists.
///
/// ```
/// use cargo_mangue::au::codes_from_plist;
///
/// let plist = "<key>AudioComponents</key><array><dict>\
///     <key>manufacturer</key><string>Ogvj</string>\
///     <key>subtype</key><string>h4yl</string>\
///     <key>type</key><string>aumu</string>\
///     </dict></array>";
/// assert_eq!(
///     codes_from_plist(plist),
///     Some(["aumu".to_owned(), "h4yl".to_owned(), "Ogvj".to_owned()])
/// );
/// assert_eq!(codes_from_plist("<key>type</key><string>aumu</string>"), None);
/// ```
pub fn codes_from_plist(text: &str) -> Option<[String; 3]> {
    let (_, components) = text.split_once("<key>AudioComponents</key>")?;
    let value = |key: &str| -> Option<String> {
        let marker = format!("<key>{key}</key>");
        let (_, after) = components.split_once(&marker)?;
        let after = after.trim_start().strip_prefix("<string>")?;
        let (code, _) = after.split_once("</string>")?;
        (code.chars().count() == 4 && code.is_ascii()).then(|| code.to_owned())
    };
    Some([value("type")?, value("subtype")?, value("manufacturer")?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_need_all_three_as_four_characters() {
        let entry = |kind: &str| {
            format!(
                "<key>AudioComponents</key><dict><key>type</key>\n  <string>{kind}</string>\
                 <key>subtype</key><string>abcd</string><key>manufacturer</key>\
                 <string>Wxyz</string></dict>"
            )
        };
        assert_eq!(
            codes_from_plist(&entry("aufx")),
            Some(["aufx".into(), "abcd".into(), "Wxyz".into()])
        );
        for broken in ["aufxx", "auf", "", "äufx"] {
            assert_eq!(codes_from_plist(&entry(broken)), None, "{broken}");
        }
        assert_eq!(codes_from_plist(""), None);
        assert_eq!(
            helper_dir(Path::new("/t"), "my-gain"),
            Path::new("/t/manguebyte/au/my-gain")
        );
    }
}
