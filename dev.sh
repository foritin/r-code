#!/usr/bin/env bash

set -Eeuo pipefail

bootstrap_only=false

for argument in "$@"; do
  case "$argument" in
    --bootstrap-only)
      bootstrap_only=true
      ;;
    -h|--help)
      cat <<'EOF'
Usage: bash ./dev.sh [--bootstrap-only]

  --bootstrap-only  Install and verify dependencies without starting Tauri.
EOF
      exit 0
      ;;
    *)
      printf '[R-Code] Unknown option: %s\n' "$argument" >&2
      exit 2
      ;;
  esac
done

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
frontend_dir="$repo_root/src-tauri/frontend"
agent_core_manifest="$repo_root/vendor/agent-core/Cargo.toml"

step() {
  printf '\033[36m[R-Code] %s\033[0m\n' "$1"
}

fail() {
  printf '\033[31m[R-Code] %s\033[0m\n' "$1" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  local install_hint="$2"

  if ! command -v "$command_name" >/dev/null 2>&1; then
    fail "Command '$command_name' was not found. $install_hint"
  fi
}

cd "$repo_root"

step "Checking base development tools"
require_command git "Install Git, then reopen the terminal."
require_command cargo "Install Rust with rustup, then reopen the terminal."
require_command rustc "Install Rust with rustup, then reopen the terminal."
require_command node "Install Node.js, then reopen the terminal."
require_command npm "Install a Node.js distribution that includes npm."

if [[ "$(uname -s)" == "Darwin" ]]; then
  require_command xcode-select "Run 'xcode-select --install' on macOS."
  if ! xcode-select -p >/dev/null 2>&1; then
    fail "Xcode Command Line Tools are missing. Run 'xcode-select --install', finish the installer, then retry."
  fi
fi

tauri_version=""
if command -v cargo-tauri >/dev/null 2>&1; then
  tauri_version="$(cargo tauri --version 2>/dev/null || true)"
fi

if [[ ! "$tauri_version" =~ ^tauri-cli\ 2\. ]]; then
  step "Installing Tauri 2 CLI (the first install may take a few minutes)"
  install_args=(install tauri-cli --version '^2.0.0' --locked)
  if [[ -n "$tauri_version" ]]; then
    install_args+=(--force)
  fi
  cargo "${install_args[@]}" || fail "Tauri 2 CLI installation failed."

  tauri_version="$(cargo tauri --version 2>/dev/null || true)"
  if [[ ! "$tauri_version" =~ ^tauri-cli\ 2\. ]]; then
    fail "Tauri CLI was installed, but this terminal cannot find version 2.x. Reopen the terminal and retry."
  fi
fi
printf '[R-Code] %s\n' "$tauri_version"

if [[ ! -f "$agent_core_manifest" ]]; then
  step "Initializing the agent-core submodule"
  git submodule update --init --recursive -- vendor/agent-core ||
    fail "The agent-core submodule could not be initialized."
  [[ -f "$agent_core_manifest" ]] || fail "The agent-core manifest is still missing after initialization."
fi

step "Checking frontend dependencies"
vite_command="$frontend_dir/node_modules/.bin/vite"
root_lock="$frontend_dir/package-lock.json"
installed_lock="$frontend_dir/node_modules/.package-lock.json"
npm_ready=false

if [[ -x "$vite_command" ]]; then
  npm_ready=true
fi

if $npm_ready && [[ -f "$root_lock" ]]; then
  if [[ ! -f "$installed_lock" || "$root_lock" -nt "$installed_lock" ]]; then
    npm_ready=false
  fi
fi

if $npm_ready && ! (cd "$frontend_dir" && npm ls --depth=0 --silent >/dev/null 2>&1); then
  npm_ready=false
fi

if ! $npm_ready; then
  step "Installing frontend dependencies"
  if [[ -f "$root_lock" ]]; then
    (cd "$frontend_dir" && npm ci) || fail "Frontend dependency installation failed."
  else
    (cd "$frontend_dir" && npm install) || fail "Frontend dependency installation failed."
  fi
fi

step "Validating the Cargo workspace"
cargo metadata --no-deps --format-version 1 >/dev/null 2>&1 ||
  fail "Cargo workspace validation failed."

step "Checking local TCP loopback"
(cd "$frontend_dir" && node scripts/check-loopback.mjs) ||
  fail "Local TCP loopback validation failed. Follow the guidance above."

if $bootstrap_only; then
  printf '\033[32m[R-Code] Dependencies are ready.\033[0m\n'
  exit 0
fi

step "Starting Tauri development mode"
exec cargo tauri dev
