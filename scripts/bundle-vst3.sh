#!/usr/bin/env bash
# Builds a plugin crate in release mode and turns the result into a .vst3
# bundle a host can load.
#
#   ./scripts/bundle-vst3.sh <crate> "<Plugin Name>"
#   ./scripts/bundle-vst3.sh mangue-pluck "Mangue Pluck"
#
# The bundle lands in target/bundled/. Copy it to a folder your host scans:
#   Windows  %COMMONPROGRAMFILES%\VST3   or  %LOCALAPPDATA%\Programs\Common\VST3
#   Linux    ~/.vst3                     or  /usr/lib/vst3
#   macOS    ~/Library/Audio/Plug-Ins/VST3
#
# A VST 3 plugin is a folder with a fixed layout on every platform, with the
# library in a subfolder named after the machine it was built for.
set -euo pipefail
cd "$(dirname "$0")/.."

crate="${1:?usage: bundle-vst3.sh <crate> \"<Plugin Name>\"}"
name="${2:-$crate}"
library="${crate//-/_}"

cargo build --release -p "$crate"

# The names VST 3 gives processor architectures.
case "$(uname -m)" in
    x86_64 | amd64) machine="x86_64" ;;
    aarch64 | arm64) machine="arm64" ;;
    i?86) machine="x86" ;;
    *) machine="$(uname -m)" ;;
esac

bundle="target/bundled/${name}.vst3"
rm -rf "$bundle"
case "$(uname -s)" in
    Linux*)
        # Linux keeps the kernel's own name for 64-bit ARM, and calls
        # 32-bit Intel i386.
        [ "$machine" = "arm64" ] && machine="aarch64"
        [ "$machine" = "x86" ] && machine="i386"
        mkdir -p "$bundle/Contents/${machine}-linux"
        cp "target/release/lib${library}.so" "$bundle/Contents/${machine}-linux/${name}.so"
        ;;
    Darwin*)
        mkdir -p "$bundle/Contents/MacOS"
        cp "target/release/lib${library}.dylib" "$bundle/Contents/MacOS/${name}"
        printf 'BNDL????' > "$bundle/Contents/PkgInfo"
        cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>${name}</string>
    <key>CFBundleIdentifier</key><string>org.manguebyte.${crate}</string>
    <key>CFBundleName</key><string>${name}</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleSignature</key><string>????</string>
    <key>CFBundleVersion</key><string>1.0</string>
</dict>
</plist>
PLIST
        ;;
    *)
        # Windows, under Git Bash or MSYS.
        mkdir -p "$bundle/Contents/${machine}-win"
        cp "target/release/${library}.dll" "$bundle/Contents/${machine}-win/${name}.vst3"
        ;;
esac

echo "Built $bundle"
