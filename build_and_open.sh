#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="release"
OPEN_APP=1
APP_NAME="Sessionary"
BIN_NAME="sessionary"
LOG_FILE="${TMPDIR:-/tmp}/sessionary.log"

usage() {
  cat <<'EOF'
Usage: ./build_and_open.sh [debug|release] [--no-open]

Examples:
  ./build_and_open.sh
  ./build_and_open.sh debug
  ./build_and_open.sh release --no-open
EOF
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
      echo "Unknown argument: $arg" >&2
      usage >&2
      exit 1
      ;;
  esac
done

require_tool() {
  local path="$1"
  local name="$2"

  if [[ ! -x "$path" ]]; then
    echo "Missing $name: $path" >&2
    echo "Run dependency install first." >&2
    exit 1
  fi
}

build_frontend() {
  echo "==> Building frontend"

  if command -v pnpm >/dev/null 2>&1; then
    (
      cd "$SCRIPT_DIR"
      pnpm build
    )
    return
  fi

  if command -v npm >/dev/null 2>&1; then
    (
      cd "$SCRIPT_DIR"
      npm run build
    )
    return
  fi

  require_tool "$SCRIPT_DIR/node_modules/.bin/tsc" "TypeScript compiler"
  require_tool "$SCRIPT_DIR/node_modules/.bin/vite" "Vite"
  "$SCRIPT_DIR/node_modules/.bin/tsc"
  "$SCRIPT_DIR/node_modules/.bin/vite" build
}

build_backend() {
  echo "==> Building tauri binary ($PROFILE)"
  if [[ "$PROFILE" == "release" ]]; then
    # `tauri build` handles cargo build (with custom-protocol auto-injected)
    # and produces the .app bundle so macOS picks up the icon and Info.plist.
    (cd "$SCRIPT_DIR" && pnpm tauri build)
  else
    cargo build --manifest-path "$SCRIPT_DIR/src-tauri/Cargo.toml"
  fi
}

open_app() {
  local target_dir="$SCRIPT_DIR/src-tauri/target/$PROFILE"
  local app_bundle="$target_dir/bundle/macos/${APP_NAME}.app"
  local binary_path="$target_dir/$BIN_NAME"

  if [[ "$OPEN_APP" -eq 0 ]]; then
    echo "==> Build finished, skip opening"
    echo "Binary: $binary_path"
    return
  fi

  echo "==> Launching app"

  if [[ -d "$app_bundle" ]] && command -v open >/dev/null 2>&1; then
    open -n "$app_bundle"
    echo "Opened: $app_bundle"
    return
  fi

  if [[ ! -x "$binary_path" ]]; then
    echo "Built binary not found: $binary_path" >&2
    exit 1
  fi

  nohup "$binary_path" >"$LOG_FILE" 2>&1 &
  echo "Started: $binary_path"
  echo "PID: $!"
  echo "Logs: $LOG_FILE"
}

main() {
  # `pnpm tauri build` invokes `beforeBuildCommand` (which is `pnpm build`),
  # so skip the manual frontend build to avoid running tsc + vite twice.
  if [[ "$PROFILE" != "release" ]]; then
    build_frontend
  fi
  build_backend
  open_app
}

main
