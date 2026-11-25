#!/bin/sh
# Build the release binary and wrap it in dist/Hangar.app.
set -eu
cd "$(dirname "$0")"
cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=dist/Hangar.app
if [ -e "$APP" ]; then trash "$APP"; fi
mkdir -p "$APP/Contents/MacOS"
cp target/release/hangar "$APP/Contents/MacOS/hangar"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Hangar</string>
  <key>CFBundleDisplayName</key><string>Hangar</string>
  <key>CFBundleIdentifier</key><string>dev.hangar.modmanager</string>
  <key>CFBundleExecutable</key><string>hangar</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSArchitecturePriority</key><array><string>arm64</string></array>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
codesign --force --sign - "$APP"
echo "Built $APP"
