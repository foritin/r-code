#!/usr/bin/env bash
# Tauri's normal macOS dev path lets the linker apply an ad-hoc signature whose designated
# requirement is the binary CDHash. R-Code no longer uses Keychain for macOS model/MCP credentials,
# but a stable signing identity is still useful when testing other OS-integrated capabilities.
#
# This cargo-compatible runner replaces only `cargo run` on macOS: it builds first and, when
# R_CODE_MACOS_DEV_SIGNING_IDENTITY is explicitly configured, signs the host with that identity.
# It intentionally never manufactures an identifier-only designated requirement: such an ad-hoc
# requirement is forgeable by another local binary and is not a safe Keychain principal. Developers
# Release builds still pass through to Cargo/Tauri's normal signing path.

set -euo pipefail

if [[ $# -eq 0 ]]; then
  exec cargo
fi

R_CODE_CARGO_ACTION="$1"
shift

if [[ "$(uname -s)" != "Darwin" || "$R_CODE_CARGO_ACTION" != "run" ]]; then
  exec cargo "$R_CODE_CARGO_ACTION" "$@"
fi

R_CODE_BUILD_ARGS=()
R_CODE_APP_ARGS=()
R_CODE_PROFILE="debug"
R_CODE_TARGET=""
R_CODE_TARGET_DIR_OVERRIDE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --)
      shift
      R_CODE_APP_ARGS=("$@")
      break
      ;;
    --release)
      R_CODE_PROFILE="release"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --profile)
      R_CODE_BUILD_ARGS+=("$1")
      shift
      [[ $# -gt 0 ]] || { echo "missing value for --profile" >&2; exit 2; }
      R_CODE_PROFILE="$1"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --profile=*)
      R_CODE_PROFILE="${1#--profile=}"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --target)
      R_CODE_BUILD_ARGS+=("$1")
      shift
      [[ $# -gt 0 ]] || { echo "missing value for --target" >&2; exit 2; }
      R_CODE_TARGET="$1"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --target=*)
      R_CODE_TARGET="${1#--target=}"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --target-dir)
      R_CODE_BUILD_ARGS+=("$1")
      shift
      [[ $# -gt 0 ]] || { echo "missing value for --target-dir" >&2; exit 2; }
      R_CODE_TARGET_DIR_OVERRIDE="$1"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    --target-dir=*)
      R_CODE_TARGET_DIR_OVERRIDE="${1#--target-dir=}"
      R_CODE_BUILD_ARGS+=("$1")
      ;;
    *)
      R_CODE_BUILD_ARGS+=("$1")
      ;;
  esac
  shift
done

cargo build "${R_CODE_BUILD_ARGS[@]}"

if [[ -n "$R_CODE_TARGET_DIR_OVERRIDE" ]]; then
  if [[ "$R_CODE_TARGET_DIR_OVERRIDE" = /* ]]; then
    R_CODE_TARGET_DIR="$R_CODE_TARGET_DIR_OVERRIDE"
  else
    R_CODE_TARGET_DIR="$(pwd)/$R_CODE_TARGET_DIR_OVERRIDE"
  fi
else
  R_CODE_TARGET_DIR="$({ cargo metadata --format-version 1 --no-deps; } | node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", chunk => input += chunk);
    process.stdin.on("end", () => process.stdout.write(JSON.parse(input).target_directory));
  ')"
fi

if [[ "$R_CODE_PROFILE" == "dev" ]]; then
  R_CODE_PROFILE="debug"
fi

R_CODE_BINARY_DIR="$R_CODE_TARGET_DIR"
if [[ -n "$R_CODE_TARGET" ]]; then
  R_CODE_BINARY_DIR="$R_CODE_BINARY_DIR/$R_CODE_TARGET"
fi
R_CODE_BINARY="$R_CODE_BINARY_DIR/$R_CODE_PROFILE/r-code-host"

if [[ ! -x "$R_CODE_BINARY" ]]; then
  echo "R-Code macOS dev runner could not find executable: $R_CODE_BINARY" >&2
  exit 1
fi

if [[ -n "${R_CODE_MACOS_DEV_SIGNING_IDENTITY:-}" ]]; then
  /usr/bin/codesign \
    --force \
    --sign "$R_CODE_MACOS_DEV_SIGNING_IDENTITY" \
    --identifier com.rcode.desktop.dev \
    "$R_CODE_BINARY"
  /usr/bin/codesign --verify --strict "$R_CODE_BINARY"
  echo "Running signed macOS development host: $R_CODE_BINARY" >&2
else
  echo "Running ad-hoc macOS development host: $R_CODE_BINARY" >&2
  echo "Hint: set R_CODE_MACOS_DEV_SIGNING_IDENTITY to a trusted code-signing identity when testing signed-app behavior." >&2
fi
if [[ "${R_CODE_DEV_RUNNER_SIGN_ONLY:-0}" == "1" ]]; then
  exit 0
fi

exec "$R_CODE_BINARY" "${R_CODE_APP_ARGS[@]}"
