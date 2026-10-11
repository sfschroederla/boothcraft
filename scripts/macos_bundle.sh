#!/usr/bin/env bash
# Build a standalone Boothcraft.app for macOS. Only builds boothcraft-app (the GUI) and its own
# dependencies via `cargo build -p`, so this never needs boothcraft-cli to build successfully.
#
# Environment:
#   UNIVERSAL=1   build aarch64 + x86_64 and merge them with lipo (needs both rustup targets)
#   VERSION=x.y.z override the version in Info.plist (default: workspace Cargo.toml)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PROFILE="release"
BIN_NAME="boothcraft-app"
APP_NAME="Boothcraft"
BUNDLE_ID="com.schroeds.boothcraft"
VERSION="${VERSION:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')}"

if [[ "${UNIVERSAL:-0}" == "1" ]]; then
    cargo build --release -p "$BIN_NAME" --target aarch64-apple-darwin
    cargo build --release -p "$BIN_NAME" --target x86_64-apple-darwin
    mkdir -p "target/$PROFILE"
    lipo -create -output "target/$PROFILE/$BIN_NAME" \
        "target/aarch64-apple-darwin/$PROFILE/$BIN_NAME" \
        "target/x86_64-apple-darwin/$PROFILE/$BIN_NAME"
else
    cargo build --release -p "$BIN_NAME"
fi

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

# Apple Silicon refuses unsigned code, and lipo drops the linker's ad-hoc signature.
codesign --force --deep --sign - "$APP_DIR"

echo "Built $APP_DIR"
