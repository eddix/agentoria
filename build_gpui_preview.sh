#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="release"
OPEN_APP=1
APP_NAME="Sessionary GPUI Preview"
BIN_NAME="sessionary-gpui-preview"
BUNDLE_ID="com.eddix.sessionary.gpui-preview"
INSTALL_ROOT="${HOME}/Applications"

usage() {
  printf '%s\n' \
    "Usage: ./build_gpui_preview.sh [debug|release] [--no-open]" \
    "" \
    "Builds, signs, and installs the isolated GPUI Preview app."
}

for arg in "$@"; do
  case "$arg" in
    debug|--debug)
      PROFILE="debug"
      ;;
    release|--release)
      PROFILE="release"
      ;;
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

TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
STAGING_APP="${TARGET_DIR}/${PROFILE}/bundle/macos/${APP_NAME}.app"
INSTALLED_APP="${INSTALL_ROOT}/${APP_NAME}.app"
BINARY="${TARGET_DIR}/${PROFILE}/${BIN_NAME}"

build_binary() {
  local profile_flag=()
  if [[ "$PROFILE" == "release" ]]; then
    profile_flag=(--release)
  fi
  cargo build \
    --manifest-path "${SCRIPT_DIR}/Cargo.toml" \
    -p sessionary-gpui \
    --bin "$BIN_NAME" \
    --locked \
    "${profile_flag[@]}"
}

assemble_bundle() {
  rm -rf "$STAGING_APP"
  mkdir -p \
    "${STAGING_APP}/Contents/MacOS" \
    "${STAGING_APP}/Contents/Resources"
  install -m 755 "$BINARY" "${STAGING_APP}/Contents/MacOS/${BIN_NAME}"
  install -m 644 \
    "${SCRIPT_DIR}/crates/sessionary-gpui/Info.plist" \
    "${STAGING_APP}/Contents/Info.plist"
  install -m 644 \
    "${SCRIPT_DIR}/src-tauri/icons/icon.icns" \
    "${STAGING_APP}/Contents/Resources/icon.icns"
  codesign --force --deep --sign - "$STAGING_APP"
  codesign --verify --deep --strict --verbose=2 "$STAGING_APP"
}

install_bundle() {
  mkdir -p "$INSTALL_ROOT"
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
  install_bundle
  if [[ "$OPEN_APP" -eq 1 ]]; then
    open -n "$INSTALLED_APP"
  fi
}

main
