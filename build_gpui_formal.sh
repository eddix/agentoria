#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OPEN_APP=1
APP_NAME="Sessionary"
BIN_NAME="sessionary-gpui"
BUNDLE_ID="com.eddix.sessionary"
INSTALL_ROOT="${HOME}/Applications"
TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
STAGING_APP="${TARGET_DIR}/release/bundle/macos/${APP_NAME}.app"
INSTALLED_APP="${INSTALL_ROOT}/${APP_NAME}.app"
PREVIEW_DATA="${HOME}/Library/Application Support/Sessionary GPUI Preview"
FORMAL_DATA="${HOME}/Library/Application Support/Sessionary"

usage() {
  printf '%s\n' \
    "Usage: ./build_gpui_formal.sh [--no-open]" \
    "" \
    "Builds, signs, and installs the formal native Sessionary app."
}

for arg in "$@"; do
  case "$arg" in
    --no-open)
      OPEN_APP=0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$arg" >&2
      usage >&2
      exit 1
      ;;
  esac
done

build_binary() {
  cargo build \
    --manifest-path "${SCRIPT_DIR}/Cargo.toml" \
    -p sessionary-gpui \
    --bin "$BIN_NAME" \
    --release \
    --no-default-features \
    --features formal \
    --locked
}

assemble_bundle() {
  rm -rf "$STAGING_APP"
  mkdir -p \
    "${STAGING_APP}/Contents/MacOS" \
    "${STAGING_APP}/Contents/Resources"
  install -m 755 \
    "${TARGET_DIR}/release/${BIN_NAME}" \
    "${STAGING_APP}/Contents/MacOS/${BIN_NAME}"
  install -m 644 \
    "${SCRIPT_DIR}/crates/sessionary-gpui/Info.formal.plist" \
    "${STAGING_APP}/Contents/Info.plist"
  install -m 644 \
    "${SCRIPT_DIR}/assets/icon.icns" \
    "${STAGING_APP}/Contents/Resources/icon.icns"
  codesign --force --deep --sign - "$STAGING_APP"
  codesign --verify --deep --strict --verbose=2 "$STAGING_APP"
}

migrate_preview_index() {
  pkill -x sessionary-gpui-preview 2>/dev/null || true
  mkdir -p "$FORMAL_DATA"
  if [[ ! -f "${FORMAL_DATA}/index.sqlite" && -f "${PREVIEW_DATA}/index.sqlite" ]]; then
    ditto "${PREVIEW_DATA}/index.sqlite" "${FORMAL_DATA}/index.sqlite"
  fi
  if [[ ! -f "${FORMAL_DATA}/preferences.json" && -f "${PREVIEW_DATA}/preferences.json" ]]; then
    ditto "${PREVIEW_DATA}/preferences.json" "${FORMAL_DATA}/preferences.json"
  fi
}

install_bundle() {
  pkill -x sessionary 2>/dev/null || true
  pkill -x "$BIN_NAME" 2>/dev/null || true
  rm -rf "$INSTALLED_APP"
  ditto "$STAGING_APP" "$INSTALLED_APP"
  codesign --verify --deep --strict --verbose=2 "$INSTALLED_APP"
  rm -rf "$STAGING_APP"
  printf 'Installed: %s\nBundle ID: %s\n' "$INSTALLED_APP" "$BUNDLE_ID"
}

main() {
  build_binary
  assemble_bundle
  migrate_preview_index
  install_bundle
  if [[ "$OPEN_APP" -eq 1 ]]; then
    open -n "$INSTALLED_APP"
  fi
}

main
