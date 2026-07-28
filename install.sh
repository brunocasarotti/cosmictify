#!/usr/bin/env bash
# Install Cosmictify from the latest GitHub Release (no Rust / no compile).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/brunocasarotti/cosmictify/main/install.sh | bash
#   PREFIX=$HOME/.local bash install.sh
#   VERSION=0.1.0 bash install.sh
set -euo pipefail

REPO="${REPO:-brunocasarotti/cosmictify}"
APP_NAME="cosmictify"
APP_ID="com.brunocasarotti.Cosmictify"
PREFIX="${PREFIX:-$HOME/.local}"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ASSET_ARCH="x86_64" ;;
  *)
    echo "error: unsupported architecture: $ARCH (need x86_64 for now)" >&2
    exit 1
    ;;
esac

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

API="https://api.github.com/repos/${REPO}/releases/latest"
if [[ -n "${VERSION:-}" ]]; then
  TAG="v${VERSION#v}"
  API="https://api.github.com/repos/${REPO}/releases/tags/${TAG}"
fi

echo "Fetching release metadata from ${REPO}..."
JSON="$(curl -fsSL "$API")"

# Prefer tarball asset
ASSET_URL="$(printf '%s' "$JSON" | sed -n 's/.*"browser_download_url": "\([^"]*linux-x86_64\.tar\.gz\)".*/\1/p' | head -1)"
if [[ -z "$ASSET_URL" ]]; then
  echo "error: no linux-x86_64.tar.gz asset found on the release." >&2
  echo "Build from source: https://github.com/${REPO}#build--install-user-local" >&2
  exit 1
fi

TAG_NAME="$(printf '%s' "$JSON" | sed -n 's/.*"tag_name": "\([^"]*\)".*/\1/p' | head -1)"
echo "Installing ${APP_NAME} ${TAG_NAME} → ${PREFIX}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

TARBALL="$TMP/cosmictify.tar.gz"
curl -fsSL "$ASSET_URL" -o "$TARBALL"
tar -xzf "$TARBALL" -C "$TMP"

STAGE="$(find "$TMP" -maxdepth 1 -type d -name 'cosmictify-*' | head -1)"
if [[ -z "$STAGE" || ! -x "$STAGE/install.sh" ]]; then
  echo "error: unexpected archive layout" >&2
  exit 1
fi

PREFIX="$PREFIX" bash "$STAGE/install.sh"

echo
echo "Done. If the applet does not appear yet:"
echo "  1) Ensure ${PREFIX}/bin is on your PATH"
echo "  2) Settings → Desktop → Panel → Configure panel applets → Cosmictify"
echo "  3) Or: pkill -x cosmictify  (panel will relaunch it)"
