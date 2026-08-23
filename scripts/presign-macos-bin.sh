#!/usr/bin/env bash
# tauri beforeBundleCommand 钩子（src-tauri/tauri.presign-macos.conf.json）：
# plan_eval 是 r-code-host 的 cargo [[bin]]，会被 tauri-bundler 原样拷进 .app 的
# Contents/MacOS；bundler 只签主二进制，嵌套未签名二进制让 codesign 在 x86_64 上
# 确定性失败。本钩子在「编译完成后、打包拷贝前」签名，不会被后续编译冲掉。
# 身份取 tauri-action 注入的 APPLE_SIGNING_IDENTITY（正式证书或 ad-hoc "-"）。
set -euo pipefail

# 用脚本自身位置定位仓库根，不依赖调用方 cwd（钩子 cwd 语义未在文档中固化）。
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "presign: 非 macOS 环境，跳过" >&2
  exit 0
fi

identity="${APPLE_SIGNING_IDENTITY:--}"
triple="${TAURI_ENV_TARGET_TRIPLE:-}"
if [[ -n "$triple" && -x "$repo_root/target/$triple/release/plan_eval" ]]; then
  binary="$repo_root/target/$triple/release/plan_eval"
else
  binary="$(ls -t "$repo_root"/target/*/release/plan_eval 2>/dev/null | head -1 || true)"
fi
if [[ -z "${binary:-}" ]]; then
  echo "presign: 未找到 plan_eval 二进制" >&2
  exit 1
fi

# ad-hoc 签名不接受时间戳；Developer ID 必须带时间戳才能过公证。
if [[ "$identity" == "-" ]]; then
  timestamp_flag=(--timestamp=none)
else
  timestamp_flag=(--timestamp)
fi
codesign --force --sign "$identity" "${timestamp_flag[@]}" "$binary"
codesign --verify --strict "$binary"
echo "presign: $binary 已用身份 '$identity' 签名"
