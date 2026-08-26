// windows-reliability 断言注册表（PRD §6 需求追踪表 + §9 任务卡 → 36 断言）。
// 断言 ID 命名空间必须 `<TASK_ID>.A<n>`；全部断言均为 required
// （not_implemented / external_pending 在对应 profile 下计为显式失败/外部放行）。
//
// cargo test 过滤器同时是**测试命名合同**：实现侧的单测/集成测名称必须包含
// 这些过滤子串，改名等同于破坏验收合同。

import process from "node:process";

function cmd(id, level, command, options = {}) {
  return {
    id,
    level,
    command,
    cwd: options.cwd ?? ".",
    timeout_ms: options.timeout_ms ?? 10 * 60 * 1000,
    profiles: options.profiles ?? ["implementation", "production"],
    evidence_path: options.evidence_path ?? null,
    note: options.note ?? null,
    external: options.external ?? false,
    env: options.env ?? null,
    not_implemented: options.not_implemented ?? false,
  };
}

const nodeBin = process.execPath;

export const REGISTRY = {
  "M0-01": {
    milestone: "M0",
    depends_on: [],
    assertions: [
      cmd("M0-01.A1", "contract", [nodeBin, "--test", "scripts/verify-windows-reliability.test.mjs"], {
        note: "Harness 自测：未知 task / 失败子命令 / registry 缺失 required 断言均非 0 退出且报告列出准确失败 ID",
      }),
      cmd("M0-01.A2", "contract", [nodeBin, "scripts/windows-reliability/corpus-schema.mjs"], {
        note: "corpus.jsonl 通过 schema 校验（枚举合法、八类数量达下限、无重复 id）",
      }),
      cmd("M0-01.A3", "integration", [nodeBin, "scripts/windows-reliability/corpus-run.mjs", "--tier", "fast", "--tag", "m0-01", "--check", "dialect-field", "--check", "both-executed=1"], {
        timeout_ms: 15 * 60 * 1000,
        note: "runner 真实执行至少一条 both/fast 命令并产出含 dialect 字段的报告",
      }),
    ],
  },
  "M0-02": {
    milestone: "M0",
    depends_on: ["M0-01"],
    assertions: [
      cmd("M0-02.A1", "regression", [nodeBin, "scripts/windows-reliability/corpus-report-check.mjs", "--platform", "windows", "--suffix", "baseline", "--min-total", "40"], {
        evidence_path: "artifacts/metrics/command-corpus",
        note: "Windows 基线报告存在、schema 完整且 total ≥ 40",
      }),
      {
        id: "M0-02.A2",
        level: "regression",
        profiles: ["production"],
        external: true,
        note: "darwin 基线报告需 macOS 主机执行：在该平台 M0 提交上运行 `node scripts/verify-windows-reliability.mjs --task M0-01` 的 corpus 步骤或 `CORPUS_RUN=all cargo test -p r-code-gateway --test command_corpus_runner -- --nocapture`，产出 report-<sha>-darwin.json 后本断言转 implementation 复检",
      },
      cmd("M0-02.A3", "contract", [nodeBin, "scripts/windows-reliability/prd-baseline-check.mjs"], {
        note: "PRD §4.4 基线小节含报告路径与四个数字字段（windows 基线回填 + darwin 外部待执行标注）",
      }),
    ],
  },
  "M1-01": {
    milestone: "M1",
    depends_on: ["M0-02"],
    assertions: [
      cmd("M1-01.A1", "unit", ["cargo", "test", "-p", "r-code-gateway", "resolve_windows_shell"], {
        note: "五级解析链单测：设置覆盖（含失败报错不静默回落）、已知位置、git.exe 反推、PATH 探测、PowerShell 回落",
      }),
      cmd("M1-01.A2", "security-negative", ["cargo", "test", "-p", "r-code-gateway", "wsl_bash"], {
        note: "PATH 首位为 C:\\Windows\\System32\\bash.exe 时跳过并继续下一级",
      }),
      cmd("M1-01.A3", "integration", [nodeBin, "scripts/windows-reliability/corpus-run.mjs", "--tier", "fast", "--tag", "m1-01", "--check", "dialect=git-bash", "--check", "categories=dialect-chain,env-prefix,quoting"], {
        timeout_ms: 15 * 60 * 1000,
        note: "Git Bash 档金集 dialect-chain/env-prefix/quoting 全绿（回落档由 M1-01.A1 的 fallback 单测覆盖）",
      }),
    ],
  },
  "M1-02": {
    milestone: "M1",
    depends_on: ["M1-01"],
    assertions: [
      cmd("M1-02.A1", "unit", ["cargo", "test", "-p", "r-code-gateway", "bash_tier_env"], {
        note: "bash 档子进程 env 含 MSYS_NO_PATHCONV=1 与 LANG=C.UTF-8",
      }),
      cmd("M1-02.A2", "unit", ["cargo", "test", "-p", "r-code-gateway", "unix_only_rejection"], {
        note: "grep 在 PowerShell 档被 hint 拦截、在 bash 档放行",
      }),
      cmd("M1-02.A3", "contract", ["cargo", "test", "-p", "r-code-gateway", "windows_tool_description"], {
        note: "Windows bash 档描述含 Git Bash、回落档描述含 PowerShell 语义字符串",
      }),
      cmd("M1-02.A4", "regression", [nodeBin, "scripts/windows-reliability/corpus-run.mjs", "--tier", "fast", "--tag", "m1-02", "--check", "categories=encoding,path,pipe", "--check", "no-mojibake"], {
        timeout_ms: 15 * 60 * 1000,
        note: "金集 encoding/path/pipe 类通过，中文无 U+FFFD 替换符",
      }),
    ],
  },
  "M2-01": {
    milestone: "M2",
    depends_on: ["M0-02"],
    assertions: [
      cmd("M2-01.A1", "unit", ["cargo", "test", "-p", "r-code-core", "win_env"], {
        note: "合成顺序 HKLM→HKCU→进程差集、REG_EXPAND_SZ 展开、大小写不敏感去重",
      }),
      cmd("M2-01.A2", "integration", ["cargo", "test", "-p", "r-code-gateway", "-p", "r-code-host", "synthesized_path_child"], {
        timeout_ms: 20 * 60 * 1000,
        note: "bash 与两条 Codex 拉起路径的子进程 PATH 为合成值，RTK 前缀在最前且无覆盖丢失",
      }),
      cmd("M2-01.A3", "failure-path", ["cargo", "test", "-p", "r-code-core", "win_env_fallthrough"], {
        note: "注册表读取失败时 fallthrough 进程 PATH 且有日志",
      }),
      cmd("M2-01.A4", "regression", [nodeBin, "scripts/windows-reliability/cfg-isolation-check.mjs"], {
        note: "win_env 全部经 #[cfg(windows)] 隔离、无非 cfg 引用（macOS 实机构建由 CI darwin job 把关）",
      }),
    ],
  },
  "M2-02": {
    milestone: "M2",
    depends_on: ["M1-02"],
    assertions: [
      cmd("M2-02.A1", "unit", ["cargo", "test", "-p", "r-code-gateway", "append_diagnosis_samples"], {
        note: "四类取证样本（ParserError/相对路径 exe/not recognized/blocked by policy）各产出正确提示要点",
      }),
      cmd("M2-02.A2", "boundary", ["cargo", "test", "-p", "r-code-gateway", "diagnosis_boundary"], {
        note: "正常输出零污染、提示长度 ≤400 字符",
      }),
      cmd("M2-02.A3", "integration", ["cargo", "test", "-p", "r-code-host", "codex_diagnosis_projection"], {
        timeout_ms: 20 * 60 * 1000,
        note: "codex commandExecution 错误投影含同源提示",
      }),
      cmd("M2-02.A4", "contract", ["cargo", "test", "-p", "r-code-gateway", "diagnosis_counters"], {
        note: "诊断命中计数可读取且只含类别与次数",
      }),
    ],
  },
  "M3-01": {
    milestone: "M3",
    depends_on: ["M0-02"],
    assertions: [
      cmd("M3-01.A1", "unit", ["cargo", "test", "-p", "r-code-host", "codex_exec_reasoning_effort"], {
        timeout_ms: 20 * 60 * 1000,
        note: "无设置时 codex exec 参数不含 reasoning 覆盖；有设置时按值传递",
      }),
      cmd("M3-01.A2", "unit", ["cargo", "test", "-p", "r-code-host", "delegation_prompt_convention"], {
        timeout_ms: 20 * 60 * 1000,
        note: "Windows 委派提示含规约常量（五要素语义），Unix 不含；常量 ≤300 字符",
      }),
      cmd("M3-01.A3", "regression", ["cargo", "test", "-p", "r-code-host", "codex_exec_web_search"], {
        timeout_ms: 20 * 60 * 1000,
        note: "web_search=disabled 与既有 codex 拉起参数不回归",
      }),
    ],
  },
  "M3-02": {
    milestone: "M3",
    depends_on: ["M3-01"],
    assertions: [
      cmd("M3-02.A1", "contract", ["cargo", "test", "-p", "r-code-agent-worker", "delegate_task_description"], {
        note: "delegate_task 描述含 full_access 参数语义字符串",
      }),
      cmd("M3-02.A2", "integration", ["cargo", "test", "-p", "r-code-host", "policy_rejection_system_hint"], {
        timeout_ms: 20 * 60 * 1000,
        note: "mock 连续 2 次 blocked by policy 后事件流出现只读档位提示",
      }),
      cmd("M3-02.A3", "integration", ["cargo", "test", "-p", "r-code-host", "policy_rejection_hint_threshold"], {
        timeout_ms: 20 * 60 * 1000,
        note: "该提示来自 System 通道且 1 次拒绝不触发",
      }),
    ],
  },
  "M4-01": {
    milestone: "M4",
    depends_on: ["M1-02"],
    assertions: [
      cmd("M4-01.A1", "security-negative", ["cargo", "test", "-p", "r-code-gateway", "classifier_bash_dialect"], {
        note: "专项清单（sudo/rm -rf/curl|sh/管道位置）全部按预期定级，无漏判为 R0/R1",
      }),
      cmd("M4-01.A2", "regression", ["cargo", "test", "-p", "r-code-gateway", "classifier_not_looser"], {
        note: "同一命令集分级不低于 Unix 现状基线",
      }),
      cmd("M4-01.A3", "unit", ["cargo", "test", "-p", "r-code-gateway", "classifier_shell_wrap"], {
        note: "powershell -Command 包壳命令按内层命令定级",
      }),
    ],
  },
  "M4-02": {
    milestone: "M4",
    depends_on: ["M1-02", "M2-01", "M2-02", "M3-02", "M4-01"],
    assertions: [
      cmd("M4-02.A1", "performance", [nodeBin, "scripts/windows-reliability/corpus-run.mjs", "--tier", "all", "--tag", "final", "--check", "thresholds"], {
        timeout_ms: 20 * 60 * 1000,
        note: "改造后全量金集：符合率 ≥96% 且方言类失败占比 <2%",
      }),
      cmd("M4-02.A2", "ci-contract", [nodeBin, "scripts/windows-reliability/ci-corpus-gate-check.mjs"], {
        note: "CI Windows job 含金集 fast 档步骤且失败会阻断",
      }),
      cmd("M4-02.A3", "contract", [nodeBin, "scripts/windows-reliability/replay-eval.mjs", "--offline"], {
        note: "Codex 链路重放评估脚本可离线运行并输出结构化结果",
      }),
      {
        id: "M4-02.A4",
        level: "performance",
        profiles: ["production"],
        external: true,
        note: "真实 Codex 账号 ≥92% 链路复测（外部放行；离线重放评估由 M4-02.A3 覆盖）",
      },
    ],
  },
  "M4-03": {
    milestone: "M4",
    depends_on: ["M2-01"],
    assertions: [
      cmd("M4-03.A1", "unit", ["cargo", "test", "-p", "r-code-host", "execution_settings"], {
        timeout_ms: 20 * 60 * 1000,
        note: "execution.bash_shell_path / codex.subagent_reasoning_effort 读写经 SettingsService，空串=强制回落",
      }),
      cmd("M4-03.A2", "component", [nodeBin, "scripts/run-tests.mjs", "execution-env-card.test.mjs"], {
        cwd: "src-tauri/frontend",
        timeout_ms: 15 * 60 * 1000,
        note: "设置卡：未检出 Git Bash 警示可见；检出时展示路径",
      }),
      cmd("M4-03.A3", "docs-contract", [nodeBin, "scripts/windows-reliability/docs-consistency-check.mjs"], {
        note: "architecture.md/operations.md 含方言策略与设置键说明",
      }),
    ],
  },
};

export const MILESTONES = ["M0", "M1", "M2", "M3", "M4"];

export function tasksForMilestone(milestone) {
  return Object.entries(REGISTRY)
    .filter(([, task]) => task.milestone === milestone)
    .map(([taskId]) => taskId);
}

export function tasksThroughMilestone(milestone) {
  const index = MILESTONES.indexOf(milestone);
  if (index < 0) {
    return [];
  }
  const included = new Set(MILESTONES.slice(0, index + 1));
  return Object.entries(REGISTRY)
    .filter(([, task]) => included.has(task.milestone))
    .map(([taskId]) => taskId);
}

export function validateRegistry(registry = REGISTRY) {
  const issues = [];
  const productTasks = Object.keys(registry);
  if (productTasks.length !== 11) {
    issues.push(`registry must declare exactly 11 product tasks, found ${productTasks.length}`);
  }
  for (const milestone of MILESTONES) {
    if (tasksForMilestone(milestone).length === 0) {
      issues.push(`milestone ${milestone} has no tasks`);
    }
  }
  const seenAssertionIds = new Set();
  for (const [taskId, task] of Object.entries(registry)) {
    for (const dependency of task.depends_on ?? []) {
      if (!registry[dependency]) {
        issues.push(`${taskId}: unknown dependency target: ${dependency}`);
      }
    }
    if (!task.assertions || task.assertions.length === 0) {
      issues.push(`${taskId}: no assertions (required assertion missing is a hard failure)`);
      continue;
    }
    for (const assertion of task.assertions) {
      if (!assertion.id.startsWith(`${taskId}.`)) {
        issues.push(`${assertion.id}: assertion id must be namespaced as ${taskId}.A<n>`);
      }
      if (seenAssertionIds.has(assertion.id)) {
        issues.push(`${assertion.id}: duplicate assertion id`);
      }
      seenAssertionIds.add(assertion.id);
      if (!assertion.not_implemented && !assertion.external && !Array.isArray(assertion.command)) {
        issues.push(`${assertion.id}: runnable assertion requires a command array`);
      }
      const profiles = assertion.profiles ?? [];
      if (profiles.length === 0 || profiles.some((p) => !["implementation", "production"].includes(p))) {
        issues.push(`${assertion.id}: profiles must be a non-empty subset of implementation|production`);
      }
    }
  }
  // 依赖图必须无环（DFS）。
  const visiting = new Set();
  const visited = new Set();
  const visit = (taskId) => {
    if (visited.has(taskId)) {
      return;
    }
    if (visiting.has(taskId)) {
      issues.push(`dependency cycle detected at ${taskId}`);
      return;
    }
    visiting.add(taskId);
    for (const dependency of registry[taskId]?.depends_on ?? []) {
      if (registry[dependency]) {
        visit(dependency);
      }
    }
    visiting.delete(taskId);
    visited.add(taskId);
  };
  for (const taskId of productTasks) {
    visit(taskId);
  }
  return issues;
}

export function getTask(registry, taskId) {
  return registry[taskId];
}
