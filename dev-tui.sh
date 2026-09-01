#!/usr/bin/env bash

set -Eeuo pipefail

# R-Code TUI（r-code-tui）开发启动脚本（Unix：macOS / Linux）。
#
# 与 dev.sh 的区别：TUI 是独立 Cargo 二进制（ratatui + crossterm），不启动
# WebView/Tauri，因此不需要前端 node_modules、Tauri CLI、本地回环检查。
# 只需 Rust 工具链 + agent-contracts 子模块就位。
#
# 默认启动交互终端（--mode tui）；--print / --json 走非交互单轮（脚本/管道用）。
# 默认 data-dir 指向 dev GUI 的同一 Dev 命名空间（Linux 的
# ~/.local/share/com.r-code.app.dev/r-code，macOS 的
# ~/Library/Application Support/com.rcode.desktop.dev/r-code），
# 让 TUI 复用 dev.sh 里配好的 provider 与密钥。

mode="tui"
message=""
skip_build=false

usage() {
  cat <<'EOF'
Usage: bash ./dev-tui.sh [--print|--json] --message <text> [--skip-build]

  (default)          Start the interactive TUI (--mode tui).
  --print            Single non-interactive turn, human-readable output.
  --json             Single non-interactive turn, event rows as JSONL.
  --message <text>   The user message for --print / --json (required there).
  --skip-build       Skip the explicit cargo build step (cargo run still builds
                     if the binary is stale).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --print)
      mode="print"
      ;;
    --json)
      mode="json"
      ;;
    --message)
      shift
      if [[ $# -eq 0 ]]; then
        printf '[R-Code TUI] --message requires a value.\n' >&2
        exit 2
      fi
      message="$1"
      ;;
    --skip-build)
      skip_build=true
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf '[R-Code TUI] Unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ "$mode" != "tui" && -z "$message" ]]; then
  printf '[R-Code TUI] --%s requires --message <text>.\n' "$mode" >&2
  exit 2
fi

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
agent_contracts_manifest="$repo_root/vendor/agent-contracts/Cargo.toml"

step() {
  printf '\033[36m[R-Code TUI] %s\033[0m\n' "$1"
}

fail() {
  printf '\033[31m[R-Code TUI] %s\033[0m\n' "$1" >&2
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
}

# Dev GUI 的同一命名空间：Linux 用 XDG data root，macOS 用 Application Support。
# 与 app_paths::AppFlavor::Development + dirs::data_dir() 的解析保持一致，
# 这样 TUI 才能读到 dev.sh 启动的 GUI 里配置好的 provider/密钥。
dev_data_dir() {
  local data_root bundle_id
  if [[ "$(uname -s)" == "Darwin" ]]; then
    data_root="${HOME}/Library/Application Support"
    bundle_id="com.rcode.desktop.dev"
  else
    data_root="${XDG_DATA_HOME:-${HOME}/.local/share}"
    bundle_id="com.r-code.app.dev"
  fi
  printf '%s' "${data_root}/${bundle_id}/r-code"
}

cd "$repo_root"

step "Checking base development tools"
require_command git "Install Git, then reopen the terminal."
require_command cargo "Install Rust with rustup, then reopen the terminal."
require_command rustc "Install Rust with rustup, then reopen the terminal."

step "Checking the agent-contracts submodule pin"
sync_agent_contracts_submodule

if ! $skip_build; then
  step "Building r-code-tui"
  cargo build -p r-code-tui --bin r-code-tui || fail "r-code-tui build failed."
fi

dev_data_dir="$(dev_data_dir)"
run_args=(--mode "$mode" --data-dir "$dev_data_dir")
if [[ "$mode" != "tui" ]]; then
  run_args+=(--message "$message")
fi

step "Starting r-code-tui"
if [[ "$mode" == "tui" ]]; then
  exec cargo run -q -p r-code-tui --bin r-code-tui -- "${run_args[@]}"
fi
cargo run -q -p r-code-tui --bin r-code-tui -- "${run_args[@]}"
