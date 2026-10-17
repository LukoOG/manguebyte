//! The `Info.plist` of an Audio Unit's `.component` bundle.
//!
//! macOS learns that a bundle holds Audio Units from the `AudioComponents`
//! entry of its `Info.plist`, before it loads the library: each unit's
//! type, subtype and manufacturer, its name and version, and the name of
//! the factory function the library exports. Written from Apple's
//! *Information Property List Key Reference* and the `AudioComponent.h`
//! header, which describes the entry.

use manguebyte_plugin::{PluginInfo, PluginKind};

use crate::codes::{code_text, codes, version_number};

/// The name of the factory function [`export_au!`](crate::export_au)
/// exports, which the `Info.plist` names.
pub const FACTORY_FUNCTION: &str = "ManguebyteAudioUnitFactory";

/// Replaces the characters that cannot stand for themselves in XML text.
fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            // XML 1.0 has no way to write most control characters.
            c if c.is_control() && !matches!(c, '\t' | '\n') => {}
            c => escaped.push(c),
        }
    }
    escaped
}

/// The name a host lists: `Vendor: Name`, as Apple's hosts expect, so that
/// they can file the unit under its maker.
pub fn display_name(info: &PluginInfo) -> String {
    if info.vendor.is_empty() {
        info.name.to_owned()
    } else {
        format!("{}: {}", info.vendor, info.name)
    }
}

/// The text of `Info.plist` for a component bundle whose executable, in
/// `Contents/MacOS`, is named `executable`, with the bundle identifier
/// `identifier` and the bundle version `version` (such as `1.2.3`).
///
/// The `AudioComponents` entry names [`FACTORY_FUNCTION`], the plugin's
/// codes (see [`codes`]), [`display_name`], the description, the version as
/// [`version_number`] makes it, `sandboxSafe`, and a tag a host files the
/// unit under: `Synthesizer` for an instrument, `Effects` for the others.
///
/// ```
/// # use manguebyte_plugin::{PluginInfo, PluginKind};
/// let info = PluginInfo {
///     id: "org.example.gain", name: "Gain", vendor: "Example", version: "1.2.3",
///     description: "Turns things up & down", kind: PluginKind::Effect,
/// };
/// let plist = manguebyte_au::info_plist(&info, "Gain", "org.example.gain", "1.2.3");
/// assert!(plist.contains("<key>type</key>\n            <string>aufx</string>"));
/// assert!(plist.contains("<string>Example: Gain</string>"));
/// assert!(plist.contains("<integer>66051</integer>"));
/// assert!(plist.contains("Turns things up &amp; down"));
/// ```
pub fn info_plist(info: &PluginInfo, executable: &str, identifier: &str, version: &str) -> String {
    let [kind, subtype, manufacturer] = codes(info).map(code_text);
    let tag = match info.kind {
        PluginKind::Instrument => "Synthesizer",
        _ => "Effects",
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20   <key>CFBundleDevelopmentRegion</key>\n\
         \x20   <string>English</string>\n\
         \x20   <key>CFBundleExecutable</key>\n\
         \x20   <string>{executable}</string>\n\
         \x20   <key>CFBundleIdentifier</key>\n\
         \x20   <string>{identifier}</string>\n\
         \x20   <key>CFBundleInfoDictionaryVersion</key>\n\
         \x20   <string>6.0</string>\n\
         \x20   <key>CFBundleName</key>\n\
         \x20   <string>{name}</string>\n\
         \x20   <key>CFBundlePackageType</key>\n\
         \x20   <string>BNDL</string>\n\
         \x20   <key>CFBundleShortVersionString</key>\n\
         \x20   <string>{version}</string>\n\
         \x20   <key>CFBundleSignature</key>\n\
         \x20   <string>????</string>\n\
         \x20   <key>CFBundleVersion</key>\n\
         \x20   <string>{version}</string>\n\
         \x20   <key>AudioComponents</key>\n\
         \x20   <array>\n\
         \x20       <dict>\n\
         \x20           <key>description</key>\n\
         \x20           <string>{description}</string>\n\
         \x20           <key>factoryFunction</key>\n\
         \x20           <string>{FACTORY_FUNCTION}</string>\n\
         \x20           <key>manufacturer</key>\n\
         \x20           <string>{manufacturer}</string>\n\
         \x20           <key>name</key>\n\
         \x20           <string>{display}</string>\n\
         \x20           <key>sandboxSafe</key>\n\
         \x20           <true/>\n\
         \x20           <key>subtype</key>\n\
         \x20           <string>{subtype}</string>\n\
         \x20           <key>tags</key>\n\
         \x20           <array>\n\
         \x20               <string>{tag}</string>\n\
         \x20           </array>\n\
         \x20           <key>type</key>\n\
         \x20           <string>{kind}</string>\n\
         \x20           <key>version</key>\n\
         \x20           <integer>{number}</integer>\n\
         \x20       </dict>\n\
         \x20   </array>\n\
         </dict>\n\
         </plist>\n",
        executable = xml(executable),
        identifier = xml(identifier),
        name = xml(info.name),
        version = xml(version),
        description = xml(info.description),
        display = xml(&display_name(info)),
        number = version_number(info.version),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: PluginInfo = PluginInfo {
        id: "org.manguebyte.pluck",
        name: "Mangue <Pluck>",
        vendor: "",
        version: "0.1.0",
        description: "Plucks\u{1}",
        kind: PluginKind::Instrument,
    };

    #[test]
    fn the_entry_names_the_unit_and_escapes_its_text() {
        let plist = info_plist(&INFO, "Mangue Pluck", "org.manguebyte.pluck", "0.1.0");
        assert!(plist.contains("<string>aumu</string>"));
        assert!(plist.contains(&format!(
            "<string>{}</string>",
            code_text(crate::subtype_code(INFO.id))
        )));
        assert!(plist.contains("<string>Mangue &lt;Pluck&gt;</string>"));
        assert!(plist.contains("<string>Plucks</string>"));
        assert!(plist.contains("<string>Synthesizer</string>"));
        assert!(plist.contains("<integer>256</integer>"));
        assert!(plist.contains(&format!("<string>{FACTORY_FUNCTION}</string>")));
        // Every element that opens closes.
        for tag in ["dict", "array", "plist"] {
            assert_eq!(
                plist.matches(&format!("<{tag}")).count(),
                plist.matches(&format!("</{tag}>")).count(),
                "{tag}"
            );
        }
        assert_eq!(display_name(&INFO), "Mangue <Pluck>");
    }
}
