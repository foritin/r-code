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
agent_contracts_manifest="$repo_root/vendor/agent-contracts/Cargo.toml"

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

parent_locked_agent_contracts_commit() {
  local entry commit
  # Prefer the index: the agent-core -> agent-contracts rename may be staged but
  # not yet committed, in which case HEAD has no gitlink at the new path yet.
  entry="$(git ls-files -s -- vendor/agent-contracts | awk '{print $2}')"
  if [[ -n "$entry" ]]; then
    printf '%s' "$entry"
    return 0
  fi

  commit="$(git ls-tree HEAD -- vendor/agent-contracts | awk '{print $3}')"
  if [[ -n "$commit" ]]; then
    printf '%s' "$commit"
    return 0
  fi

  # Fall back to the legacy path for history before the rename.
  git ls-tree HEAD -- vendor/agent-core | awk '{print $3}'
}

sync_agent_contracts_submodule() {
  local expected_commit actual_commit local_changes
  expected_commit="$(parent_locked_agent_contracts_commit)"
  [[ -n "$expected_commit" ]] || fail "The parent repository does not contain an agent-contracts gitlink."

  if [[ ! -f "$agent_contracts_manifest" ]]; then
    step "Initializing the agent-contracts submodule"
    git submodule update --init --recursive --checkout -- vendor/agent-contracts ||
      fail "The agent-contracts submodule could not be initialized."
    [[ -f "$agent_contracts_manifest" ]] || fail "The agent-contracts manifest is still missing after initialization."
  fi

  actual_commit="$(git -C vendor/agent-contracts rev-parse HEAD)" ||
    fail "The current agent-contracts commit could not be read."
  if [[ "$actual_commit" != "$expected_commit" ]]; then
    local_changes="$(git -C vendor/agent-contracts status --porcelain --untracked-files=all)" ||
      fail "The agent-contracts working tree could not be inspected."
    if [[ -n "$local_changes" ]]; then
      fail "agent-contracts is at $actual_commit while the parent pins $expected_commit, and the submodule has local changes. Commit or stash them, then run 'git submodule update --init --recursive --checkout -- vendor/agent-contracts'."
    fi

    step "Synchronizing agent-contracts to the parent repository pin"
    git submodule update --init --recursive --checkout -- vendor/agent-contracts ||
      fail "The agent-contracts submodule could not be synchronized."
    actual_commit="$(git -C vendor/agent-contracts rev-parse HEAD)" ||
      fail "The synchronized agent-contracts commit could not be read."
    [[ "$actual_commit" == "$expected_commit" ]] ||
      fail "agent-contracts still differs from the parent repository pin after synchronization."
  fi

  local_changes="$(git -C vendor/agent-contracts status --porcelain --untracked-files=all)" ||
    fail "The agent-contracts working tree could not be inspected."
  if [[ -n "$local_changes" ]]; then
    printf '\033[33m[R-Code] agent-contracts has local changes; continuing with the current working tree.\033[0m\n'
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

step "Checking the agent-contracts submodule pin"
sync_agent_contracts_submodule

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

step "Starting isolated R-Code Dev"
exec cargo tauri dev --config "$repo_root/src-tauri/tauri.dev.conf.json"
