#!/usr/bin/env bash
# Build a standalone Boothcraft.app for macOS. Only builds boothcraft-app (the GUI) and its own
# dependencies via `cargo build -p`, so this never needs boothcraft-cli to build successfully.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE="release"
BIN_NAME="boothcraft-app"
APP_NAME="Boothcraft"
BUNDLE_ID="com.schroeds.boothcraft"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

cargo build --release -p "$BIN_NAME"

OUT_DIR="target/$PROFILE/bundle/osx"
APP_DIR="$OUT_DIR/$APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cp "target/$PROFILE/$BIN_NAME" "$APP_DIR/Contents/MacOS/$APP_NAME"
chmod +x "$APP_DIR/Contents/MacOS/$APP_NAME"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.music</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Boothcraft needs microphone access to record audio.</string>
</dict>
</plist>
PLIST

echo "Built $APP_DIR"
