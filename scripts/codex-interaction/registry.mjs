// M0-01 建立的唯一断言注册表：PRD §9/§10 的 12 个任务 × 全部验收断言。
// 规则（PRD §8.1 / M0-01 任务卡第 4 步）：
//   - 未实现断言以 not_implemented 显式失败，不允许静默跳过；
//   - 每个任务至少 1 个 required 断言，缺失即 registry 校验失败；
//   - 随任务落地把 not_implemented 条目替换为真实命令，不得删除断言或降级 optional。

import process from "node:process";

const FRONTEND_DIR = "src-tauri/frontend";
const FIXTURE = "fixtures/codex-interaction/protocol-0.145.0.json";

// profile 语义：implementation = 离线 fixture 门禁；production = 在
// implementation 之上追加真实登录/安装包等外部放行（M4-02.A4）。
function notImplemented(taskId, assertionId, note) {
  return {
    id: assertionId,
    level: "contract",
    not_implemented: true,
    note: note ?? `任务 ${taskId} 尚未实施`,
    profiles: ["implementation", "production"],
  };
}

function cmd(id, level, command, options = {}) {
  return {
    id,
    level,
    command,
    cwd: options.cwd ?? ".",
    timeout_ms: options.timeout_ms ?? 10 * 60 * 1000,
    profiles: options.profiles ?? ["implementation", "production"],
    evidence_path: options.evidence_path,
    note: options.note,
    external: options.external ?? false,
  };
}

const npmLike = (args, options) => cmd(
  options.id,
  options.level,
  [process.execPath, "scripts/run-tests.mjs", ...args],
  { cwd: FRONTEND_DIR, ...options },
);

export const REGISTRY = {
  "M0-01": {
    milestone: "M0",
    depends_on: [],
    assertions: [
      cmd("M0-01.A1", "contract", [process.execPath, "--test", "scripts/verify-codex-interaction.test.mjs"], {
        note: "runner 自测：未知 task、缺失 required 断言、失败子命令、报告脱敏",
      }),
      cmd("M0-01.A2", "contract", [process.execPath, "scripts/codex-interaction/check-protocol-fixture.mjs", FIXTURE], {
        note: "0.145.0 requestUserInput fixture 离线一致性",
      }),
      cmd("M0-01.A3", "regression", [process.execPath, "scripts/codex-interaction/smoke-orchestration.mjs"], {
        timeout_ms: 45 * 60 * 1000,
        note: "Harness 编排 ≥1 Rust + ≥1 前端测试并产出无 secret JSON 索引",
      }),
    ],
  },
  "M0-02": {
    milestone: "M0",
    depends_on: ["M0-01"],
    assertions: [
      cmd("M0-02.A1", "contract", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "codex_interaction::tests::a1", "--", "--test-threads=1"], {
        note: "§4.1 已知帧→宿主事件转换 contract（含方法表对齐与 fixture 样例帧）",
      }),
      cmd("M0-02.A2", "compatibility", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "codex_interaction::tests::a2", "--", "--test-threads=1"], {
        note: "缺可选字段/未知字段/缺 scope fail-closed",
      }),
      cmd("M0-02.A3", "reliability", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "codex_interaction::tests::a3", "--", "--test-threads=1"], {
        note: "超限 payload 有界 + 重复完成幂等",
      }),
      cmd("M0-02.A4", "security", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "codex_interaction::tests::a4", "--", "--test-threads=1"], {
        note: "诊断脱敏 + 能力快照保守默认",
      }),
    ],
  },
  "M1-01": {
    milestone: "M1",
    depends_on: ["M0-02"],
    assertions: [
      cmd("M1-01.A1", "integration", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m1_01_a1", "--", "--test-threads=1"], {
        note: "commentary/final 字符按序各出现一次 + phase 正确（投影器单测 + e2e 流式）",
      }),
      cmd("M1-01.A2", "reliability", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m1_01_a2", "--", "--test-threads=1"], {
        note: "重复 delta/completed、迟到帧、交错 item/run 不重复不串线",
      }),
      cmd("M1-01.A3", "regression", [process.execPath, "scripts/codex-interaction/m1-01-regression.mjs"], {
        timeout_ms: 70 * 60 * 1000,
        note: "既有 Codex final / 原生 R-Code / 子代理消息测试回归",
      }),
    ],
  },
  "M1-02": {
    milestone: "M1",
    depends_on: ["M0-01"],
    assertions: [
      cmd("M1-02.A1", "contract", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m1_02_a1", "--", "--test-threads=1"], {
        note: "Codex prompt 含首次实质批次/阶段变化/新发现/低噪声/私有推理禁令",
      }),
      cmd("M1-02.A2", "regression", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m1_02_a2", "--", "--test-threads=1"], {
        note: "简单任务不播报；例行继续播报被禁止",
      }),
      cmd("M1-02.A3", "regression", [process.execPath, "scripts/codex-interaction/m1-02-regression.mjs"], {
        timeout_ms: 55 * 60 * 1000,
        note: "原生/Codex 共享语义（core+worker+host）且无重复合同",
      }),
    ],
  },
  "M1-03": {
    milestone: "M1",
    depends_on: ["M1-01"],
    assertions: [
      cmd("M1-03.A1", "unit", [process.execPath, "--test", "scripts/codex-message-stream.test.mjs", "--test-name-pattern", "m1_03_a1"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "10k delta 单节点、全文完整、可见刷新 ≤10Hz",
      }),
      cmd("M1-03.A2", "integration", [process.execPath, "--test", "scripts/codex-message-stream.test.mjs", "--test-name-pattern", "m1_03_a2"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "live 应用序列与历史重建结构/顺序/phase 一致",
      }),
      cmd("M1-03.A3", "visual", [process.execPath, "--test", "scripts/codex-message-stream.test.mjs", "--test-name-pattern", "m1_03_a3"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "真实 Timeline 分层渲染 + 1280×800 无横向溢出",
      }),
    ],
  },
  "M2-01": {
    milestone: "M2",
    depends_on: ["M0-02", "M1-03"],
    assertions: [
      cmd("M2-01.A1", "contract", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m2_01_a1", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "全部支持 kind 的 started/completed 映射 + 失败/退出码状态",
      }),
      cmd("M2-01.A2", "integration", [process.execPath, "scripts/codex-interaction/m2-01-integration.mjs"], {
        timeout_ms: 35 * 60 * 1000,
        note: "命令输出按序/可展开/截断可见 + 失败与 exit code 一致（rust e2e + 前端）",
      }),
      cmd("M2-01.A3", "reliability", [process.execPath, "scripts/codex-interaction/m2-01-reliability.mjs"], {
        timeout_ms: 35 * 60 * 1000,
        note: "超大/高频输出有界 + 迟到输出丢弃 + 终态不丢",
      }),
    ],
  },
  "M2-02": {
    milestone: "M2",
    depends_on: ["M0-02", "M1-03"],
    assertions: [
      cmd("M2-02.A1", "integration", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m2_02_a1", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "计划/diff/压缩/warning/usage 各自生成非聊天事件",
      }),
      cmd("M2-02.A2", "replay", [process.execPath, "--test", "scripts/codex-context-events.test.mjs", "--test-name-pattern", "m2_02_a2"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "重复 plan 幂等；live/历史重建结构一致",
      }),
      cmd("M2-02.A3", "visual", [process.execPath, "--test", "scripts/codex-context-events.test.mjs", "--test-name-pattern", "m2_02_a3"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "紧凑上下文行 + 长 diff 截断 + 无横向溢出",
      }),
    ],
  },
  "M3-01": {
    milestone: "M3",
    depends_on: ["M0-02", "M1-01"],
    assertions: [
      cmd("M3-01.A1", "integration", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m3_01_a1", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "单/多题答案按 question id 返回且 turn 继续",
      }),
      cmd("M3-01.A2", "security", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m3_01_a2", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "secret 只进单次 writer 响应，不进事件流/JSONL",
      }),
      cmd("M3-01.A3", "reliability", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m3_01_a3", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "重复提交/未知 key 原子 claim，唯一确定终态",
      }),
    ],
  },
  "M3-02": {
    milestone: "M3",
    depends_on: ["M3-01"],
    assertions: [
      cmd("M3-02.A1", "component", [process.execPath, "--test", "scripts/codex-question-card.test.mjs", "--test-name-pattern", "m3_02_a1"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "选项/其他/secret 编码正确；secret 不回显",
      }),
      cmd("M3-02.A2", "e2e", [process.execPath, "--test", "scripts/codex-question-card.test.mjs", "--test-name-pattern", "m3_02_a2"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "仅键盘完成提交；resolved 后同 turn 继续",
      }),
      cmd("M3-02.A3", "visual", [process.execPath, "--test", "scripts/codex-question-card.test.mjs", "--test-name-pattern", "m3_02_a3"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "1280×800/390×844 无溢出 + aria-live",
      }),
    ],
  },
  "M3-03": {
    milestone: "M3",
    depends_on: ["M3-01", "M3-02"],
    assertions: [
      cmd("M3-03.A1", "reliability", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m3_03_a1", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "timeout/resolved/迟到提交竞态：唯一 writer 结果 + 唯一 UI 终态",
      }),
      cmd("M3-03.A2", "integration", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m3_03_a2", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "pending 期间 steer 与问题回答双链路独立、顺序可追踪",
      }),
      cmd("M3-03.A3", "recovery", [process.execPath, "scripts/codex-interaction/m3-03-recovery.mjs"], {
        timeout_ms: 35 * 60 * 1000,
        note: "reload 保留 pending；restart 后 expired 只读、迟到提交拒绝",
      }),
    ],
  },
  "M4-01": {
    milestone: "M4",
    depends_on: ["M1-03", "M2-01", "M2-02", "M3-03"],
    assertions: [
      cmd("M4-01.A1", "security-negative", [process.execPath, "scripts/codex-interaction/m4-01-security.mjs"], {
        timeout_ms: 45 * 60 * 1000,
        note: "raw reasoning/secret/凭据在投影与持久化 oracle 中 0 命中",
      }),
      cmd("M4-01.A2", "security", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m4_01_a2", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "诊断仅元数据；unknown/overflow/timeout/duplicate 类别可定位",
      }),
      cmd("M4-01.A3", "replay", ["cargo", "test", "-p", "r-code-host", "--all-features", "--lib", "m4_01_a3", "--", "--test-threads=1"], {
        timeout_ms: 20 * 60 * 1000,
        note: "乱序/重复/断流/跨 run 最终状态确定",
      }),
      cmd("M4-01.A4", "performance", [process.execPath, "--test", "scripts/codex-performance.test.mjs"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 15 * 60 * 1000,
        evidence_path: "artifacts/ai-tasks/verification/codex-rich-interaction/implementation/performance.json",
        note: "§6 延迟/密度/有界指标 + 机器可读 performance.json",
      }),
    ],
  },
  "M4-02": {
    milestone: "M4",
    depends_on: ["M4-01"],
    assertions: [
      cmd("M4-02.A1", "cross-platform", [process.execPath, "scripts/codex-interaction/m4-02-cross-platform.mjs"], {
        timeout_ms: 55 * 60 * 1000,
        note: "共用断言本机全绿 + 无平台 cfg 分叉 + CI 三平台矩阵覆盖",
      }),
      cmd("M4-02.A2", "e2e", [process.execPath, "--test", "scripts/codex-full-flow-visual.test.mjs"], {
        cwd: FRONTEND_DIR,
        timeout_ms: 10 * 60 * 1000,
        note: "完整 commentary→tool→question→answer→final 亮/暗视觉证据（Rust 侧闭环见 m4_02_a2 e2e）",
      }),
      cmd("M4-02.A3", "regression", [process.execPath, "scripts/codex-interaction/m4-02-regression.mjs"], {
        timeout_ms: 90 * 60 * 1000,
        note: "workspace/frontend 回归 + 文档一致性门禁",
      }),
      {
        id: "M4-02.A4",
        level: "production",
        command: [process.execPath, "scripts/codex-interaction/m4-02-production-gate.mjs"],
        cwd: ".",
        timeout_ms: 5 * 60 * 1000,
        note: "真实登录/安装包冒烟；外部条件缺失时如实 external_pending（production profile 专属）",
        profiles: ["production"],
        external: true,
      },
    ],
  },
  // Harness 自编排冒烟（M0-01.A3）：证明 pipeline 能跑真实 Rust+前端测试。
  // 属于 selftest 里程碑，不进入 --through M0..M4 的累计门禁。
  "M0-01-smoke": {
    milestone: "selftest",
    depends_on: [],
    assertions: [
      cmd("M0-01-smoke.R1", "regression", ["cargo", "test", "-p", "r-code-host", "--all-features", "codex_app_server_input_keeps_text_and_local_images_in_order", "--", "--exact"], {
        timeout_ms: 30 * 60 * 1000,
        note: "Rust 编排冒烟：host 内嵌 fixture 单测",
      }),
      npmLike(["usage-label.test.mjs"], { id: "M0-01-smoke.R2", level: "regression", timeout_ms: 5 * 60 * 1000, note: "前端编排冒烟：node:test 子集" }),
    ],
  },
};

export const MILESTONES = ["M0", "M1", "M2", "M3", "M4"];

export function tasksForMilestone(milestone) {
  return Object.entries(REGISTRY)
    .filter(([, task]) => task.milestone === milestone)
    .map(([id]) => id)
    .sort();
}

/// through 语义：目标里程碑及之前所有里程碑的产品任务（不含 selftest）。
export function tasksThroughMilestone(milestone) {
  const index = MILESTONES.indexOf(milestone);
  if (index < 0) return [];
  const included = new Set(MILESTONES.slice(0, index + 1));
  return Object.entries(REGISTRY)
    .filter(([, task]) => included.has(task.milestone))
    .map(([id]) => id)
    .sort();
}

// registry 结构校验：任务数、断言非空、依赖存在且无环、profile 合法。
// 返回 issue 列表；空列表 = registry 可用。未知 task / 缺失 required 断言
// 的行为由 runner + CLI 在运行时基于这里的结果显式失败（M0-01.A1）。
export function validateRegistry(registry = REGISTRY) {
  const issues = [];
  const productTaskIds = Object.keys(registry).filter((id) => id !== "M0-01-smoke");
  if (productTaskIds.length !== 12) {
    issues.push(`expected 12 product tasks, found ${productTaskIds.length}: ${productTaskIds.join(", ")}`);
  }
  for (const milestone of MILESTONES) {
    if (tasksForMilestone(milestone).length === 0) {
      issues.push(`milestone ${milestone} has no tasks`);
    }
  }
  const seen = new Set();
  const visit = (taskId, stack) => {
    if (stack.includes(taskId)) {
      issues.push(`dependency cycle: ${[...stack, taskId].join(" -> ")}`);
      return;
    }
    if (seen.has(taskId)) {
      return;
    }
    seen.add(taskId);
    const task = registry[taskId];
    if (!task) {
      issues.push(`unknown dependency target: ${taskId}`);
      return;
    }
    if (!Array.isArray(task.assertions) || task.assertions.length === 0) {
      issues.push(`task ${taskId} has no assertions`);
    }
    for (const assertion of task.assertions ?? []) {
      if (!assertion.id?.startsWith(`${taskId}.`)) {
        issues.push(`assertion ${assertion.id} is not namespaced under task ${taskId}`);
      }
      for (const profile of assertion.profiles ?? []) {
        if (!["implementation", "production"].includes(profile)) {
          issues.push(`assertion ${assertion.id} has unknown profile ${profile}`);
        }
      }
    }
    for (const dep of task.depends_on ?? []) {
      visit(dep, [...stack, taskId]);
    }
  };
  for (const taskId of Object.keys(registry)) {
    visit(taskId, []);
  }
  return issues;
}

export function getTask(registry, taskId) {
  return registry[taskId] ?? null;
}
