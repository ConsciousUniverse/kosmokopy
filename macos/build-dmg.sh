#!/usr/bin/env bash
#
# Build a macOS .dmg containing Kosmokopy.app
#
# Usage:  ./macos/build-dmg.sh
#
# Prerequisites:
#   - Rust toolchain (cargo)
#   - macOS with sips + iconutil (built-in)
#   - GTK4 installed via Homebrew (brew install gtk4)
#
set -euo pipefail

APP="Kosmokopy"
PKG="kosmokopy"
VERSION="0.1.0"
ARCH="$(uname -m)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

DIST_DIR="${PROJECT_DIR}/target/macos"
APP_BUNDLE="${DIST_DIR}/${APP}.app"
DMG_OUTPUT="${DIST_DIR}/${APP}-${VERSION}-${ARCH}.dmg"

# ── Build release binary ────────────────────────────────────────────────
echo "==> Building release binary…"
cd "$PROJECT_DIR"
cargo build --release

# ── Create .app bundle structure ────────────────────────────────────────
echo "==> Creating ${APP}.app bundle…"
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"

# Binary
cp "target/release/${PKG}" "${APP_BUNDLE}/Contents/MacOS/${APP}"

# ── Generate .icns icon ─────────────────────────────────────────────────
echo "==> Generating app icon…"
ICONSET_DIR="${DIST_DIR}/${PKG}.iconset"
rm -rf "${ICONSET_DIR}"
mkdir -p "${ICONSET_DIR}"

SRC_ICON="${SCRIPT_DIR}/../appimage/kosmokopy.png"
if [ ! -f "$SRC_ICON" ]; then
    echo "Error: Icon not found at ${SRC_ICON}"
    exit 1
fi

# Generate required icon sizes from 256x256 source
# Sizes we can produce from a 256px source (downscaling only)
for SIZE in 16 32 64 128 256; do
    sips -z ${SIZE} ${SIZE} "$SRC_ICON" --out "${ICONSET_DIR}/icon_${SIZE}x${SIZE}.png" >/dev/null
done
# Retina variants: 16@2x=32, 32@2x=64, 128@2x=256
cp "${ICONSET_DIR}/icon_32x32.png"   "${ICONSET_DIR}/icon_16x16@2x.png"
cp "${ICONSET_DIR}/icon_64x64.png"   "${ICONSET_DIR}/icon_32x32@2x.png"
cp "${ICONSET_DIR}/icon_256x256.png" "${ICONSET_DIR}/icon_128x128@2x.png"
# Remove the 64x64 (not a standard iconset size)
rm -f "${ICONSET_DIR}/icon_64x64.png"

iconutil -c icns -o "${APP_BUNDLE}/Contents/Resources/${PKG}.icns" "${ICONSET_DIR}"
rm -rf "${ICONSET_DIR}"

# ── Info.plist ──────────────────────────────────────────────────────────
cat > "${APP_BUNDLE}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP}</string>
    <key>CFBundleIdentifier</key>
    <string>dev.kosmokopy.app</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>${APP}</string>
    <key>CFBundleIconFile</key>
    <string>${PKG}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
</dict>
</plist>
PLIST

# ── Bundle GTK4 dylibs (use DYLD_FALLBACK_LIBRARY_PATH) ────────────────
# For a portable .app we create a wrapper that sets up the GTK environment.
# The user must have GTK4 installed via Homebrew.
mv "${APP_BUNDLE}/Contents/MacOS/${APP}" "${APP_BUNDLE}/Contents/MacOS/${APP}-bin"
cat > "${APP_BUNDLE}/Contents/MacOS/${APP}" <<'LAUNCHER'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"

# Find Homebrew prefix
if [ -d "/opt/homebrew" ]; then
    BREW="/opt/homebrew"
elif [ -d "/usr/local" ]; then
    BREW="/usr/local"
else
    echo "Homebrew not found. Please install GTK4: brew install gtk4"
    exit 1
fi

export DYLD_FALLBACK_LIBRARY_PATH="${BREW}/lib:${DYLD_FALLBACK_LIBRARY_PATH:-}"
export GDK_PIXBUF_MODULE_FILE="${BREW}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
export XDG_DATA_DIRS="${BREW}/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export GSETTINGS_SCHEMA_DIR="${BREW}/share/glib-2.0/schemas"

exec "${DIR}/${0##*/}-bin" "$@"
LAUNCHER
chmod +x "${APP_BUNDLE}/Contents/MacOS/${APP}"

# ── Create .dmg ─────────────────────────────────────────────────────────
echo "==> Creating DMG…"
rm -f "${DMG_OUTPUT}"

# Create a temporary DMG with the app and an Applications symlink
STAGING="${DIST_DIR}/dmg-staging"
rm -rf "${STAGING}"
mkdir -p "${STAGING}"
cp -a "${APP_BUNDLE}" "${STAGING}/"
ln -s /Applications "${STAGING}/Applications"

hdiutil create -volname "${APP}" \
    -srcfolder "${STAGING}" \
    -ov -format UDZO \
    "${DMG_OUTPUT}"

rm -rf "${STAGING}"

echo ""
echo "========================================"
echo "  DMG Created Successfully!"
echo "========================================"
echo ""
echo "Output: ${DMG_OUTPUT}"
echo "Size:   $(du -h "${DMG_OUTPUT}" | cut -f1)"
echo ""
echo "Note: GTK4 must be installed on the target Mac (brew install gtk4)"
