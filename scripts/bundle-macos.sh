#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
profile=${DGG_BUILD_PROFILE:-release}
cargo build --locked --profile "$profile"
binary_dir=$profile
if [[ $profile == dev ]]; then binary_dir=debug; fi

bundle="${DGG_BUNDLE_DIR:-dist}/DGG Desktop.app"
mkdir -p "$bundle/Contents/MacOS"
cp "target/$binary_dir/dgg-desktop" "$bundle/Contents/MacOS/dgg-desktop"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>DGG Desktop</string>
    <key>CFBundleDisplayName</key><string>DGG Desktop</string>
    <key>CFBundleIdentifier</key><string>gg.destiny.desktop</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>dgg-desktop</string>
    <key>CFBundleVersion</key><string>4</string>
    <key>CFBundleShortVersionString</key><string>0.1.3</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$bundle"
printf '%s\n' "$PWD/$bundle"
