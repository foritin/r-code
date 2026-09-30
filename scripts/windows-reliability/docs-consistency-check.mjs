// M4-03.A3：文档一致性检查（可 grep 断言）。
// docs/support/archive/architecture-before-pluggable-harness.md 与 docs/support/operations/operations.md 必须含方言策略与设置键说明，
// 且与实现的关键标识一致（五级解析链 / Git Bash / WSL 排除 / 注册表实时 PATH /
// execution.bash_shell_path / MSYS_NO_PATHCONV）。

import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");

const ARCHITECTURE_MARKERS = [
  "五级",
  "Git Bash",
  "System32\\bash.exe",
  "execution.bash_shell_path",
  "MSYS_NO_PATHCONV=1",
  "注册表",
  "win_env",
  "pwsh.exe → powershell.exe → cmd.exe",
];

const OPERATIONS_MARKERS = [
  "Windows 命令执行排障",
  "Git Bash",
  "execution.bash_shell_path",
  "注册表实时合成",
  "blocked by policy",
  "full_access",
  "verify-windows-reliability",
];

// P32.3：安全边界 operator runbook 必须随安全发布策略存在——前置条件、
// SafeDisabled 姿态与 quarantine 恢复路径缺一不可（s32 套件也钉内容）。
const SAFETY_RUNBOOK_MARKERS = [
  "SafeDisabled",
  "quarantine",
  "Activated",
  "helper",
  "downgrade",
];

async function check(file, markers) {
  const content = await readFile(path.join(ROOT, file), "utf8");
  return markers
    .filter((marker) => !content.includes(marker))
    .map((marker) => `${file} 缺少标记：${marker}`);
}

async function main() {
  const failures = [
    ...(await check("docs/support/archive/architecture-before-pluggable-harness.md", ARCHITECTURE_MARKERS)),
    ...(await check("docs/support/operations/operations.md", OPERATIONS_MARKERS)),
    ...(await check("docs/support/operations/safety-boundary.md", SAFETY_RUNBOOK_MARKERS)),
  ];
  if (failures.length > 0) {
    console.error(`docs-consistency-check 失败：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log("docs-consistency-check OK: architecture/operations/safety-boundary 均含所需标记");
}

await main();
