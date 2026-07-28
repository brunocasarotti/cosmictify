#!/usr/bin/env bash
# Install / uninstall Cosmictify from GitHub Releases (no Rust / no compile).
#
# Install:
#   curl -fsSL https://raw.githubusercontent.com/brunocasarotti/cosmictify/main/install.sh | bash
#   PREFIX=$HOME/.local bash install.sh
#   VERSION=0.1.0 bash install.sh
#
# Uninstall (user install in ~/.local):
#   curl -fsSL https://raw.githubusercontent.com/brunocasarotti/cosmictify/main/install.sh | bash -s -- --uninstall
#   PREFIX=$HOME/.local bash install.sh --uninstall
#
# Note: packages installed via .deb should use: sudo apt remove cosmictify
set -euo pipefail

REPO="${REPO:-brunocasarotti/cosmictify}"
APP_NAME="cosmictify"
APP_ID="com.brunocasarotti.Cosmictify"
PREFIX="${PREFIX:-$HOME/.local}"

usage() {
  cat <<EOF
Usage: install.sh [options]

Options:
  --uninstall, -u   Remove a user install from PREFIX (default: ~/.local)
  --prefix DIR      Install/uninstall prefix (default: \$HOME/.local or \$PREFIX)
  --version VER     Install a specific release tag (e.g. 0.1.0 or v0.1.0)
  -h, --help        Show this help

Examples:
  bash install.sh
  bash install.sh --uninstall
  PREFIX=/usr/local bash install.sh --uninstall
  VERSION=0.1.0 bash install.sh
EOF
}

do_uninstall() {
  local removed=0
  local paths=(
    "$PREFIX/bin/${APP_NAME}"
    "$PREFIX/share/applications/${APP_ID}.desktop"
    "$PREFIX/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
    "$PREFIX/share/metainfo/${APP_ID}.metainfo.xml"
    "$PREFIX/share/appdata/${APP_ID}.metainfo.xml"
  )

  echo "Uninstalling Cosmictify from ${PREFIX}..."
  for f in "${paths[@]}"; do
    if [[ -e "$f" || -L "$f" ]]; then
      rm -f "$f"
      echo "  removed $f"
      removed=1
    fi
  done

  gtk-update-icon-cache -f "$PREFIX/share/icons/hicolor" 2>/dev/null || true

  # Stop a running applet instance (panel may restart until removed from config)
  if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    pkill -x "$APP_NAME" 2>/dev/null || true
    echo "  stopped running ${APP_NAME} process(es)"
  fi

  if [[ "$removed" -eq 0 ]]; then
    echo "Nothing to remove under ${PREFIX}."
    echo "If you installed the .deb: sudo apt remove ${APP_NAME}"
    return 0
  fi

  echo
  echo "Cosmictify removed from ${PREFIX}."
  echo "If the icon still appears in the panel, remove it in:"
  echo "  Settings → Desktop → Panel → Configure panel applets"
}

# --- args ---
ACTION="install"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --uninstall|-u)
      ACTION="uninstall"
      shift
      ;;
    --prefix)
      PREFIX="$2"
      shift 2
      ;;
    --prefix=*)
      PREFIX="${1#*=}"
      shift
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --version=*)
      VERSION="${1#*=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ "$ACTION" == "uninstall" ]]; then
  do_uninstall
  exit 0
fi

# --- install ---
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

ASSET_URL="$(printf '%s' "$JSON" | sed -n 's/.*"browser_download_url": "\([^"]*linux-x86_64\.tar\.gz\)".*/\1/p' | head -1)"
if [[ -z "$ASSET_URL" ]]; then
  echo "error: no linux-x86_64.tar.gz asset found on the release." >&2
  echo "Build from source: https://github.com/${REPO}#build-from-source" >&2
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
echo
echo "Uninstall later:"
echo "  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash -s -- --uninstall"
