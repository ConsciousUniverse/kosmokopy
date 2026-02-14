#!/usr/bin/env bash
#
# Build an AppImage for Kosmokopy.
#
# Usage:  ./appimage/build-appimage.sh
#
set -euo pipefail

APP="Kosmokopy"
PKG="kosmokopy"
VERSION="0.1.0"
ARCH="$(uname -m)"
APPDIR="target/appimage/${APP}.AppDir"

# Kill any running instances and unmount stale AppImages
echo "==> Stopping any running instances…"
pkill -f "${PKG}" 2>/dev/null || true
for mount in /tmp/.mount_Kosmo*; do
    [ -d "$mount" ] && fusermount -uz "$mount" 2>/dev/null || true
done
sleep 0.5

# Incremental release build (only recompiles changed source)
echo "==> Building release binary…"
cargo build --release

echo "==> Creating AppDir…"
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"

# Binary
cp "target/release/${PKG}" "${APPDIR}/usr/bin/${PKG}"
strip "${APPDIR}/usr/bin/${PKG}" 2>/dev/null || true

# Desktop file (must also be at AppDir root)
cp appimage/kosmokopy.desktop "${APPDIR}/usr/share/applications/${PKG}.desktop"
cp appimage/kosmokopy.desktop "${APPDIR}/${PKG}.desktop"

# Icon — use a placeholder if no icon file exists
if [ -f appimage/kosmokopy.png ]; then
    cp appimage/kosmokopy.png "${APPDIR}/usr/share/icons/hicolor/256x256/apps/${PKG}.png"
    cp appimage/kosmokopy.png "${APPDIR}/${PKG}.png"
else
    echo "    (no icon found — using placeholder)"
    python3 -c "
from PIL import Image, ImageDraw
img = Image.new('RGB', (256, 256), '#3584e4')
d = ImageDraw.Draw(img)
d.text((90, 60), 'K', fill='white')
img.save('${APPDIR}/${PKG}.png')
" 2>/dev/null || \
    printf '\x89PNG\r\n\x1a\n' > "${APPDIR}/${PKG}.png"
fi
ln -sf "${PKG}.png" "${APPDIR}/.DirIcon"

# AppRun
cat > "${APPDIR}/AppRun" <<'APPRUN'
#!/bin/bash
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/kosmokopy" "$@"
APPRUN
chmod +x "${APPDIR}/AppRun"

# Download appimagetool if not present
APPIMAGETOOL="target/appimage/appimagetool-${ARCH}.AppImage"
if [ ! -f "${APPIMAGETOOL}" ]; then
    echo "==> Downloading appimagetool…"
    wget -q -O "${APPIMAGETOOL}" \
        "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${ARCH}.AppImage"
    chmod +x "${APPIMAGETOOL}"
fi

# Build AppImage
echo "==> Packaging AppImage…"
OUTPUT="target/appimage/${APP}-${VERSION}-${ARCH}.AppImage"
ARCH="${ARCH}" "${APPIMAGETOOL}" "${APPDIR}" "${OUTPUT}"

echo ""
echo "========================================"
echo "  AppImage Created Successfully!"
echo "========================================"
echo ""
echo "Output: ${OUTPUT}"
echo "Size: $(du -h "${OUTPUT}" | cut -f1)"
