#!/usr/bin/env bash
# Package cosmictify for end-user install (tarball + optional .deb).
# Usage: scripts/package-release.sh [version]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="cosmictify"
APP_ID="com.brunocasarotti.Cosmictify"
VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
fi

BIN="${CARGO_TARGET_DIR:-target}/release/${APP_NAME}"
if [[ ! -x "$BIN" ]]; then
  echo "error: missing release binary at $BIN — run: cargo build --release" >&2
  exit 1
fi

DIST="$ROOT/dist"
STAGE="$DIST/stage/${APP_NAME}-${VERSION}-linux-x86_64"
rm -rf "$DIST/stage"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/scalable/apps" "$STAGE/share/metainfo"

install -Dm0755 "$BIN" "$STAGE/bin/${APP_NAME}"
install -Dm0644 resources/app.desktop "$STAGE/share/applications/${APP_ID}.desktop"
install -Dm0644 resources/icon.svg "$STAGE/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
if [[ -f resources/app.metainfo.xml ]]; then
  install -Dm0644 resources/app.metainfo.xml "$STAGE/share/metainfo/${APP_ID}.metainfo.xml"
fi

# Bundled installer for the tarball (installs into ~/.local)
cat > "$STAGE/install.sh" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID="com.brunocasarotti.Cosmictify"
APP_NAME="cosmictify"

install -Dm0755 "$ROOT/bin/${APP_NAME}" "$PREFIX/bin/${APP_NAME}"
install -Dm0644 "$ROOT/share/applications/${APP_ID}.desktop" \
  "$PREFIX/share/applications/${APP_ID}.desktop"
install -Dm0644 "$ROOT/share/icons/hicolor/scalable/apps/${APP_ID}.svg" \
  "$PREFIX/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
if [[ -f "$ROOT/share/metainfo/${APP_ID}.metainfo.xml" ]]; then
  install -Dm0644 "$ROOT/share/metainfo/${APP_ID}.metainfo.xml" \
    "$PREFIX/share/metainfo/${APP_ID}.metainfo.xml"
fi
gtk-update-icon-cache -f "$PREFIX/share/icons/hicolor" 2>/dev/null || true

# Ensure ~/.local/bin is on PATH hint
if ! command -v cosmictify >/dev/null 2>&1; then
  echo "Note: add \$HOME/.local/bin to your PATH if cosmictify is not found."
fi

echo "Installed Cosmictify to $PREFIX"
echo "Add it: Settings → Desktop → Panel → Configure panel applets → Cosmictify"
EOF
chmod +x "$STAGE/install.sh"

cat > "$STAGE/README.txt" << EOF
Cosmictify ${VERSION}
=====================

Spotify panel applet for COSMIC Desktop / Pop!_OS.

Install (no compiler needed):
  ./install.sh

Or manually copy bin/ and share/ into ~/.local/

Then add the applet in COSMIC Settings → Desktop → Panel → Applets.
EOF

mkdir -p "$DIST"
TARBALL="$DIST/${APP_NAME}-${VERSION}-linux-x86_64.tar.gz"
tar -C "$DIST/stage" -czf "$TARBALL" "$(basename "$STAGE")"
echo "Wrote $TARBALL"

# --- .deb (amd64) ---
DEB_ROOT="$DIST/deb"
DEB_NAME="${APP_NAME}_${VERSION}_amd64"
rm -rf "$DEB_ROOT"
mkdir -p "$DEB_ROOT/${DEB_NAME}/DEBIAN"
mkdir -p "$DEB_ROOT/${DEB_NAME}/usr/bin"
mkdir -p "$DEB_ROOT/${DEB_NAME}/usr/share/applications"
mkdir -p "$DEB_ROOT/${DEB_NAME}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "$DEB_ROOT/${DEB_NAME}/usr/share/metainfo"

install -Dm0755 "$BIN" "$DEB_ROOT/${DEB_NAME}/usr/bin/${APP_NAME}"
install -Dm0644 resources/app.desktop \
  "$DEB_ROOT/${DEB_NAME}/usr/share/applications/${APP_ID}.desktop"
install -Dm0644 resources/icon.svg \
  "$DEB_ROOT/${DEB_NAME}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
if [[ -f resources/app.metainfo.xml ]]; then
  install -Dm0644 resources/app.metainfo.xml \
    "$DEB_ROOT/${DEB_NAME}/usr/share/metainfo/${APP_ID}.metainfo.xml"
fi

SIZE_KB="$(du -sk "$DEB_ROOT/${DEB_NAME}" | cut -f1)"
cat > "$DEB_ROOT/${DEB_NAME}/DEBIAN/control" << EOF
Package: ${APP_NAME}
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Bruno Casarotti <20110633+brunocasarotti@users.noreply.github.com>
Installed-Size: ${SIZE_KB}
Depends: libdbus-1-3, libc6
Homepage: https://github.com/brunocasarotti/cosmictify
Description: Spotify panel applet for COSMIC Desktop
 Cosmictify shows what's playing on Spotify in the COSMIC panel
 (cover, marquee title/artist, progress) with MPRIS transport controls.
EOF

# strip is optional; keep symbols for now for easier crash reports
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --build --root-owner-group "$DEB_ROOT/${DEB_NAME}" "$DIST/${DEB_NAME}.deb"
  echo "Wrote $DIST/${DEB_NAME}.deb"
else
  echo "warning: dpkg-deb not found; skipped .deb" >&2
fi

(
  cd "$DIST"
  sha256sum ./*.tar.gz ./*.deb 2>/dev/null > SHA256SUMS || true
  echo "Wrote $DIST/SHA256SUMS"
)

echo "Done. Artifacts in $DIST/"
