#!/usr/bin/env bash

set -Eeuo pipefail

target="aarch64-apple-darwin"
signing_mode="adhoc"
bootstrap=true

usage() {
  printf '%s\n' \
    'Usage: bash ./scripts/manual/package-macos.sh [options]' \
    '' \
    'Options:' \
    '  --target <triple>   aarch64-apple-darwin (default) or x86_64-apple-darwin' \
    '  --signed            Require Developer ID signing and Apple notarization' \
    '  --adhoc             Build an ad-hoc signed local package (default)' \
    '  --skip-bootstrap    Skip dev.sh dependency/bootstrap checks' \
    '  -h, --help          Show this help' \
    '' \
    'Signed mode requires APPLE_SIGNING_IDENTITY plus either:' \
    '  APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID, or' \
    '  APPLE_API_KEY + APPLE_API_ISSUER + APPLE_API_KEY_PATH.'
}

fail() {
  printf '\033[31m[R-Code macOS] %s\033[0m\n' "$1" >&2
  exit 1
}

step() {
  printf '\033[36m[R-Code macOS] %s\033[0m\n' "$1"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || fail '--target requires a Rust target triple.'
      target="$2"
      shift 2
      ;;
    --signed)
      signing_mode="signed"
      shift
      ;;
    --adhoc)
      signing_mode="adhoc"
      shift
      ;;
    --skip-bootstrap)
      bootstrap=false
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "Unknown option: $1"
      ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || fail 'This script must run on macOS.'
case "$target" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) fail "Unsupported macOS target: $target" ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
bundle_root="$repo_root/target/$target/release/bundle"

command -v cargo >/dev/null 2>&1 || fail 'Rust/Cargo is missing. Install rustup first.'
command -v rustup >/dev/null 2>&1 || fail 'rustup is required to install the target toolchain.'
command -v xcode-select >/dev/null 2>&1 || fail 'Xcode Command Line Tools are missing.'
xcode-select -p >/dev/null 2>&1 || fail "Run 'xcode-select --install' and finish setup first."

cd "$repo_root"
if $bootstrap; then
  step 'Checking project dependencies'
  bash ./dev.sh --bootstrap-only
fi

step "Installing Rust target $target when needed"
rustup target add "$target"

if [[ "$signing_mode" == "signed" ]]; then
  [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] || fail 'APPLE_SIGNING_IDENTITY is required by --signed.'
  if ! /usr/bin/security find-identity -v -p codesigning | grep -F -- "$APPLE_SIGNING_IDENTITY" >/dev/null 2>&1; then
    fail 'APPLE_SIGNING_IDENTITY was not found in the current macOS keychain.'
  fi

  apple_id_ready=false
  api_key_ready=false
  if [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    apple_id_ready=true
  fi
  if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
    api_key_ready=true
  fi
  if ! $apple_id_ready && ! $api_key_ready; then
    fail 'Notarization credentials are incomplete. Configure the Apple ID trio or App Store Connect API key trio.'
  fi
  step "Building Developer ID signed and notarized $target app/dmg"
else
  # Ad-hoc signing avoids the misleading "app is damaged" result on Apple Silicon, but it is
  # only a local/test artifact and does not replace Developer ID signing or notarization.
  export APPLE_SIGNING_IDENTITY="-"
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH API_PRIVATE_KEYS_DIR
  step "Building ad-hoc signed $target app/dmg"
fi

(
  cd "$repo_root/src-tauri"
  cargo tauri build --bundles app,dmg --target "$target"
)

shopt -s nullglob
app_matches=("$bundle_root/macos/"*.app)
dmg_matches=("$bundle_root/dmg/"*.dmg)
(( ${#app_matches[@]} == 1 )) || fail "Expected one .app bundle under $bundle_root/macos; found ${#app_matches[@]}."
(( ${#dmg_matches[@]} == 1 )) || fail "Expected one .dmg bundle under $bundle_root/dmg; found ${#dmg_matches[@]}."
app_path="${app_matches[0]}"
dmg_path="${dmg_matches[0]}"

step 'Verifying application signature structure'
/usr/bin/codesign --verify --deep --strict --verbose=2 "$app_path"

if [[ "$signing_mode" == "signed" ]]; then
  step 'Verifying Gatekeeper assessment and notarization tickets'
  /usr/sbin/spctl --assess --type execute --verbose=4 "$app_path"
  /usr/bin/xcrun stapler validate "$app_path"
  /usr/bin/xcrun stapler validate "$dmg_path"
fi

step 'Build complete'
printf '  App: %s\n' "$app_path"
printf '  DMG: %s\n' "$dmg_path"
/usr/bin/shasum -a 256 "$dmg_path"
