#!/usr/bin/env bash
# Builds a plugin crate in release mode and turns the result into a .clap
# file a host can load.
#
#   ./scripts/bundle-clap.sh <crate> "<Plugin Name>"
#   ./scripts/bundle-clap.sh mangue-pluck "Mangue Pluck"
#
# The plugin lands in target/bundled/. Copy it to a folder your host scans:
#   Windows  %COMMONPROGRAMFILES%\CLAP   or  %LOCALAPPDATA%\Programs\Common\CLAP
#   Linux    ~/.clap                     or  /usr/lib/clap
#   macOS    ~/Library/Audio/Plug-Ins/CLAP
set -euo pipefail
cd "$(dirname "$0")/.."

crate="${1:?usage: bundle-clap.sh <crate> \"<Plugin Name>\"}"
name="${2:-$crate}"
library="${crate//-/_}"

cargo build --release -p "$crate"

mkdir -p target/bundled
case "$(uname -s)" in
    Linux*)
        cp "target/release/lib${library}.so" "target/bundled/${name}.clap"
        ;;
    Darwin*)
        # On macOS a CLAP plugin is a bundle: a folder with a fixed layout.
        bundle="target/bundled/${name}.clap"
        rm -rf "$bundle"
        mkdir -p "$bundle/Contents/MacOS"
        cp "target/release/lib${library}.dylib" "$bundle/Contents/MacOS/${name}"
        cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>${name}</string>
    <key>CFBundleIdentifier</key><string>org.manguebyte.${crate}</string>
    <key>CFBundleName</key><string>${name}</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleVersion</key><string>1.0</string>
</dict>
</plist>
PLIST
        ;;
    *)
        # Windows, under Git Bash or MSYS.
        cp "target/release/${library}.dll" "target/bundled/${name}.clap"
        ;;
esac

echo "Built target/bundled/${name}.clap"
