#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OPEN_APP=1
APP_NAME="Agentoria"
BIN_NAME="agentoria"
BUNDLE_ID="com.eddix.agentoria"
INSTALL_ROOT="${HOME}/Applications"
TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
STAGING_APP="${TARGET_DIR}/release/bundle/macos/${APP_NAME}.app"
INSTALLED_APP="${INSTALL_ROOT}/${APP_NAME}.app"
LEGACY_APP="${INSTALL_ROOT}/Sessionary.app"

usage() {
  printf '%s\n' \
    "Usage: ./build_gpui_formal.sh [--no-open]" \
    "" \
    "Builds, signs, and installs the formal native Agentoria app."
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
    -p agentoria-gpui \
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
    "${SCRIPT_DIR}/crates/agentoria-gpui/Info.formal.plist" \
    "${STAGING_APP}/Contents/Info.plist"
  install -m 644 \
    "${SCRIPT_DIR}/assets/icon.icns" \
    "${STAGING_APP}/Contents/Resources/icon.icns"
  codesign --force --deep --sign - "$STAGING_APP"
  codesign --verify --deep --strict --verbose=2 "$STAGING_APP"
}

install_bundle() {
  pkill -x sessionary 2>/dev/null || true
  pkill -x sessionary-gpui 2>/dev/null || true
  pkill -x "$BIN_NAME" 2>/dev/null || true
  rm -rf "$INSTALLED_APP"
  ditto "$STAGING_APP" "$INSTALLED_APP"
  codesign --verify --deep --strict --verbose=2 "$INSTALLED_APP"
  if [[ -f "${LEGACY_APP}/Contents/Info.plist" ]] &&
    [[ "$(plutil -extract CFBundleIdentifier raw "${LEGACY_APP}/Contents/Info.plist")" == "com.eddix.sessionary" ]]; then
    rm -rf "$LEGACY_APP"
  fi
  rm -rf "$STAGING_APP"
  printf 'Installed: %s\nBundle ID: %s\n' "$INSTALLED_APP" "$BUNDLE_ID"
}

main() {
  build_binary
  assemble_bundle
  install_bundle
  if [[ "$OPEN_APP" -eq 1 ]]; then
    open -n "$INSTALLED_APP"
  fi
}

main
