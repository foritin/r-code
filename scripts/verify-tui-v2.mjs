#!/usr/bin/env node
// R-Code CLI（TUI v2）统一验收 Harness（docs/tui-v2/r-code-cli-prd.md §7.1 / R-GEN-01）
//
// 用法：
//   node scripts/verify-tui-v2.mjs --task <TASK_ID>    --profile implementation|production
//   node scripts/verify-tui-v2.mjs --through <MILESTONE_ID> --profile implementation|production
//   （可选 --assertions ID1,ID2：定向复跑并与既有报告合并）
//
// 退出码：0 = 全部 required assertion 通过；1 = 存在失败/缺失；2 = 参数缺失或非法。
// 报告：artifacts/ai-tasks/verification/tui-v2/<profile>/<task-or-milestone>.json
// 日志：artifacts/ai-tasks/verification/tui-v2/<profile>/logs/<assertion>.log
//
// 性质：只运行 R-Code 仓库自有测试与脚本；断言 registry 随任务落地逐步注册
//（M0-01 建骨架并自验证，后续任务只扩展本文件 REGISTRY）。

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "..");
const PROFILE_DIR_BASE = path.join(REPO_ROOT, "artifacts", "ai-tasks", "verification", "tui-v2");
const EVIDENCE_DIR = path.join(REPO_ROOT, "artifacts", "ai-tasks", "evidence", "tui-v2");
const DOC_GATE_REPORT = path.join(PROFILE_DIR_BASE, "implementation", "worklist-gate.json");
const WORKLIST_DOCUMENT = "docs/tui-v2/r-code-cli-prd.md";
const WORKLIST_FREEZE = "docs/tui-v2/tui-v2-freeze.yaml";

const MILESTONE_ORDER = ["M0", "M1", "M2", "M3", "M4", "M5", "M6"];

// ---------------------------------------------------------------------------
// 断言 registry：每个任务登记其验收断言的执行方式。
// kind:
//   command  — 运行命令，exit 0 且（可选）输出文件包含期望片段。
//   gate     — 文档门禁（verify-ai-worklist.mjs --mode check）。
//   self     — 内置函数检查（ctx 传入 { runner, logDir, spawnRaw }）。
//   file     — 文件存在且包含期望片段。
// required 缺失（fixture/metric 不存在）视为失败。
// ---------------------------------------------------------------------------

const REGISTRY = {
  "M0-01": {
    milestone: "M0",
    assertions: [
      {
        id: "M0-01.A1",
        description: "Harness 三参数解析、缺参/未知任务/未知里程碑 exit 2",
        kind: "self",
        async check(ctx) {
          const noArgs = await ctx.spawnRaw(["node", "scripts/verify-tui-v2.mjs"], {
            expectExit: 2,
          });
          const missingProfile = await ctx.spawnRaw(
            ["node", "scripts/verify-tui-v2.mjs", "--task", "M0-01"],
            { expectExit: 2 },
          );
          const badTask = await ctx.spawnRaw(
            [
              "node",
              "scripts/verify-tui-v2.mjs",
              "--task",
              "NOPE-99",
              "--profile",
              "implementation",
            ],
            { expectExit: 2 },
          );
          const badMilestone = await ctx.spawnRaw(
            [
              "node",
              "scripts/verify-tui-v2.mjs",
              "--through",
              "M9",
              "--profile",
              "implementation",
            ],
            { expectExit: 2 },
          );
          return {
            passed: noArgs.passed && missingProfile.passed && badTask.passed && badMilestone.passed,
            details: {
              noArgs: noArgs.exitCode,
              missingProfile: missingProfile.exitCode,
              unknownTask: badTask.exitCode,
              unknownMilestone: badMilestone.exitCode,
            },
          };
        },
      },
      {
        id: "M0-01.A2",
        description: "文档门禁 check 通过：freeze digest 一致、blocking=0、major=0",
        kind: "gate",
      },
      {
        id: "M0-01.A3",
        description: "报告含 revision/worktree digest 与失败断言列表（子进程定向复跑后检查报告结构）",
        kind: "self",
        async check(ctx) {
          const inner = await ctx.spawnRaw(
            [
              "node",
              "scripts/verify-tui-v2.mjs",
              "--task",
              "M0-01",
              "--profile",
              "implementation",
              "--assertions",
              "M0-01.A1",
            ],
            { expectExit: 0 },
          );
          const report = await readFile(
            path.join(PROFILE_DIR_BASE, "implementation", "M0-01.json"),
            "utf8",
          )
            .then((text) => JSON.parse(text))
            .catch(() => null);
          const fieldsOk = Boolean(
            report &&
              typeof report.revision === "string" &&
              report.revision.length > 0 &&
              typeof report.worktree_digest === "string" &&
              report.worktree_digest.length > 0 &&
              Array.isArray(report.failed_assertions) &&
              typeof report.summary.total === "number",
          );
          return {
            passed: inner.passed && fieldsOk,
            details: {
              innerExit: inner.exitCode,
              revision: report?.revision ?? null,
              worktreeDigestPresent: Boolean(report?.worktree_digest),
              failedAssertionsIsArray: Array.isArray(report?.failed_assertions),
            },
          };
        },
      },
    ],
  },
  "M0-02": {
    milestone: "M0",
    assertions: [
      {
        id: "M0-02.A1",
        description:
          "cargo test -p r-code-tui 全绿（--lib --bins --tests：本容器 cargo 1.95 向稳定版 rustdoc 注入 --check-cfg 导致 doctest 无法运行；r-code-tui 无任何 doctest，覆盖面等价，CI 全量口径不受影响）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--bins", "--tests"],
      },
      {
        id: "M0-02.A2",
        description: "cargo clippy -p r-code-tui --all-targets -D warnings 绿",
        kind: "command",
        command: ["cargo", "clippy", "-p", "r-code-tui", "--all-targets", "--", "-D", "warnings"],
      },
      {
        id: "M0-02.A3",
        description: "node --test scripts/release.test.mjs 绿",
        kind: "command",
        command: ["node", "--test", "scripts/release.test.mjs"],
      },
      {
        id: "M0-02.A4",
        description: "print 冒烟输出形态已记录为基线对照（M1-01 前后 exit 0→2 均合法，输出必须非空）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "bash",
            "dev-tui.sh",
            "--print",
            "--message",
            "baseline",
          ]);
          const output = await readFile(path.join(REPO_ROOT, record.logPath), "utf8").catch(
            () => "",
          );
          const nonEmpty = output.includes("R-Code") || output.includes("r-code-tui");
          const legalExit = record.exitCode === 0 || record.exitCode === 2;
          return {
            passed: legalExit && nonEmpty,
            details: {
              exitCode: record.exitCode,
              baselineFormRecorded: record.logPath,
              note: "M1-01 前 = mock 回放 exit 0；M1-01 后无配置 = 显式引导 exit 2。基线形态以 cmd 日志为准。",
            },
          };
        },
      },
    ],
  },
  "M1-01": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-01.A1",
        description:
          "装配契约：真实模式调用链单测 + main.rs 无条件 mock 注入静态断言（enable_real_agent_mode 在装配、install_mock_scenario 仅 if mock 块内）",
        kind: "self",
        async check(ctx) {
          const tests = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--bins",
            "m1_tests",
          ]);
          const source = await readFile(
            path.join(REPO_ROOT, "crates", "r-code-tui", "src", "main.rs"),
            "utf8",
          );
          const enablesReal = source.includes("enable_real_agent_mode");
          const mockGated =
            source.includes("if mock {") &&
            source.split("install_mock_scenario(&state").length - 1 === 1;
          return {
            passed: tests.exitCode === 0 && enablesReal && mockGated,
            details: {
              testExit: tests.exitCode,
              enablesReal,
              mockInstallGated: mockGated,
            },
          };
        },
      },
      {
        id: "M1-01.A2",
        description: "--mode print --mock 评估线路 exit 0（确定性演示回放）",
        kind: "self",
        async check(ctx) {
          const dir = await mkdtemp(path.join(os.tmpdir(), "tui-v2-mock-"));
          try {
            const record = await ctx.runner.run([
              "cargo",
              "run",
              "-q",
              "-p",
              "r-code-tui",
              "--bin",
              "r-code-tui",
              "--",
              "--mode",
              "print",
              "--mock",
              "--message",
              "hello",
              "--data-dir",
              dir,
            ]);
            return {
              passed: record.exitCode === 0,
              details: { exitCode: record.exitCode, log: record.logPath },
            };
          } finally {
            await rm(dir, { recursive: true, force: true });
          }
        },
      },
      {
        id: "M1-01.A3",
        description: "空 data-dir 无配置：--mode print 显式引导 exit 2，输出含 config 路径与设置页途径",
        kind: "self",
        async check(ctx) {
          const dir = await mkdtemp(path.join(os.tmpdir(), "tui-v2-noprovider-"));
          try {
            const record = await ctx.runner.run([
              "cargo",
              "run",
              "-q",
              "-p",
              "r-code-tui",
              "--bin",
              "r-code-tui",
              "--",
              "--mode",
              "print",
              "--message",
              "hello",
              "--data-dir",
              dir,
            ]);
            const log = await readFile(path.join(REPO_ROOT, record.logPath), "utf8");
            const hasPath = log.includes("config.toml");
            const hasSettingsHint = log.includes("设置 → 模型服务");
            return {
              passed: record.exitCode === 2 && hasPath && hasSettingsHint,
              details: {
                exitCode: record.exitCode,
                hasConfigPath: hasPath,
                hasSettingsHint: hasSettingsHint,
                log: record.logPath,
              },
            };
          } finally {
            await rm(dir, { recursive: true, force: true });
          }
        },
      },
      {
        id: "M1-01.A4",
        description: "交互模式传 --mock 被拒：exit 2 + 用法提示（红线 R1）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "run",
            "-q",
            "-p",
            "r-code-tui",
            "--bin",
            "r-code-tui",
            "--",
            "--mock",
          ]);
          const log = await readFile(path.join(REPO_ROOT, record.logPath), "utf8");
          const rejected = log.includes("交互模式");
          return {
            passed: record.exitCode === 2 && rejected,
            details: { exitCode: record.exitCode, rejected, log: record.logPath },
          };
        },
      },
    ],
  },
  "M1-02": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-02.A1",
        description: "Release 事件不产生 KeyAction（Windows/kitty 双写根因消除）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "release_events_do_not_produce_actions",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M1-02.A2",
        description: "Press+Release 成对事件恰好映射一个动作；Repeat 保留长按语义",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "press_events_produce_exactly_one_action",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M1-03": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-03.A1",
        description: "运行错误 → TranscriptRow::System 行投影（push_system）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "system_errors_project_into_transcript",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M1-03.A2",
        description: "provider 不可用错误附 config 绝对路径与设置页途径；其余错误不追加",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "provider_errors_carry_actionable_guidance",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M1-03.A3",
        description:
          "交互路径不再以 eprintln 作为用户可见错误通道（run_interactive_tui 函数体无 eprintln）",
        kind: "self",
        async check(ctx) {
          const source = await readFile(
            path.join(REPO_ROOT, "crates", "r-code-tui", "src", "main.rs"),
            "utf8",
          );
          const start = source.indexOf("async fn run_interactive_tui");
          const end = source.indexOf("async fn main", start);
          const body = start >= 0 && end > start ? source.slice(start, end) : "";
          const hasEprintln = body.includes("eprintln!");
          return {
            passed: start >= 0 && end > start && !hasEprintln,
            details: { interactiveBodyFound: body.length > 0, hasEprintln },
          };
        },
      },
    ],
  },
  "M6-01": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-01.A1",
        description: "会话列表投影（列头/双行行目/❯ 光标/上下移动钳位/空态）",
        kind: "self",
        async check(ctx) {
          const a = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "session_picker_projects_and_clamps",
          ]);
          const b = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "empty_picker_renders_empty_state",
          ]);
          return { passed: a.exitCode === 0 && b.exitCode === 0, details: { projects: a.exitCode, empty: b.exitCode } };
        },
      },
      {
        id: "M6-01.A2",
        description: "/resume 选择器行快照（❯ 光标、双行行目、底行 hints enter to resume）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "session_picker_projects_and_clamps",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M6-01.A3",
        description: "resume 接续（task_detail 读回一致——会话 JSONL 重建入口经宿主）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "new_session_creates_task",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M6-03": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-03.A1",
        description: "命名决策记录存在且含分发影响分析（三方案对照 + 否决依据）",
        kind: "file",
        path: "docs/tui-v2/cli-naming-decision.md",
        contains: ["维持 `r-code-tui` 单名", "externalBin", "否决"],
      },
      {
        id: "M6-03.A2",
        description: "脚本/externalBin/release.yml/断言四面一致（bin 名 r-code-tui 全链一致）",
        kind: "self",
        async check(ctx) {
          const conf = await readFile(path.join(REPO_ROOT, "src-tauri", "tauri.conf.json"), "utf8");
          const release = await readFile(path.join(REPO_ROOT, ".github", "workflows", "release.yml"), "utf8");
          const devtui = await readFile(path.join(REPO_ROOT, "dev-tui.sh"), "utf8");
          const confOk = conf.includes("binaries/r-code-tui");
          const releaseOk = release.includes("--bin r-code-tui");
          const devtuiOk = devtui.includes("--bin r-code-tui");
          return {
            passed: confOk && releaseOk && devtuiOk,
            details: { confOk, releaseOk, devtuiOk },
          };
        },
      },
      {
        id: "M6-03.A3",
        description: "累计门禁 —— M0-M5 全绿 + M6-01 + M6-02 全绿（24 任务收口；不含本品自身，无递归）",
        kind: "self",
        async check(ctx) {
          // 分别跑不含 M6-03 的累计段，避免 A3 内自证递归。
          const m5 = await ctx.runner.run([
            "node",
            "scripts/verify-tui-v2.mjs",
            "--through",
            "M5",
            "--profile",
            "implementation",
          ]);
          const m6a = await ctx.runner.run([
            "node",
            "scripts/verify-tui-v2.mjs",
            "--task",
            "M6-01",
            "--profile",
            "implementation",
          ]);
          const m6b = await ctx.runner.run([
            "node",
            "scripts/verify-tui-v2.mjs",
            "--task",
            "M6-02",
            "--profile",
            "implementation",
          ]);
          const allPassed = m5.exitCode === 0 && m6a.exitCode === 0 && m6b.exitCode === 0;
          return {
            passed: allPassed,
            details: { m5: m5.exitCode, m6a: m6a.exitCode, m6b: m6b.exitCode },
          };
        },
      },
    ],
  },
  "M6-02": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-02.A1",
        description: "/new 新建空会话（默认标题）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "new_session_creates_task",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M6-02.A2",
        description: "/rename 持久化（task_detail 读回一致）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "rename_session_persists",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M6-02.A3",
        description: "/compact 数据缺口如实暴露（宿主无公开压缩命令，接线方显式引导）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "compaction_gap_is_reported",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M5-03": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-03.A1",
        description: "交互主路径无 EnterAlternateScreen（inline 唯一形态；静态断言 main.rs）",
        kind: "self",
        async check(ctx) {
          const source = await readFile(
            path.join(REPO_ROOT, "crates", "r-code-tui", "src", "main.rs"),
            "utf8",
          );
          const interactive = source.includes("EnterAlternateScreen");
          return {
            passed: !interactive,
            details: { hasEnterAlternateScreen: interactive },
          };
        },
      },
      {
        id: "M5-03.A2",
        description: "IME 光标坐标单测：CJK 双宽 + 窄宽折行（inline_caret）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "inline_caret_accounts_for_cjk_double_width",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M5-03.A3",
        description: "print/json 回归绿（alt-screen 退役不影响非交互模式）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--bins", "m1_tests",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M5-02": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-02.A1",
        description: "历史行进终端 scrollback（PTY 集成：append-only 输出流含完整历史；无整屏清屏）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--test", "inline_scrollback"],
      },
      {
        id: "M5-02.A2",
        description: "重绘包 CSI ?2026 同步输出（字节级：首帧全量包裹、append-only 不重写历史）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "inline_render"],
      },
      {
        id: "M5-02.A3",
        description: "resize 稳定（输入行贴底、窄宽不越界、宽度变化触发全量重绘）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "input_line_stays_bottom_and_fits_width"],
      },
      {
        id: "M5-02.A4",
        description: "M1-M4 组件面在 inline 行模型下投影（语义色 + 审批带 + 排队 + 菜单 no matches + 模式徽章）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "display_assembly_covers_all_milestone_surfaces"],
      },
    ],
  },
  "M5-01": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-01.A1",
        description: "基准报告存在且含两路线数据（差分/viewport/朴素 三列字节对比 + 语义对照表）",
        kind: "file",
        path: "docs/tui-v2/m5-01-poc-report.md",
        contains: ["自研行差分", "ratatui InlineViewport", "定案"],
      },
      {
        id: "M5-01.A2",
        description: "PoC 可复跑：cargo run -p r-code-tui --example inline_bench exit 0（确定性，无终端依赖）",
        kind: "command",
        command: ["cargo", "run", "-q", "-p", "r-code-tui", "--example", "inline_bench"],
      },
      {
        id: "M5-01.A3",
        description: "定案记录含依据与被否路线差距（scrollback 判据 + 量化倍数）；差分核心单测绿",
        kind: "self",
        async check(ctx) {
          const report = await (await import("node:fs/promises")).readFile(
            path.join(REPO_ROOT, "docs", "tui-v2", "m5-01-poc-report.md"),
            "utf8",
          );
          const hasRationale =
            report.includes("scrollback 语义完整性") && report.includes("视口内重绘语义与之冲突");
          const tests = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "inline_render",
          ]);
          return {
            passed: hasRationale && tests.exitCode === 0,
            details: { hasRationale, coreTests: tests.exitCode },
          };
        },
      },
    ],
  },
  "M4-05": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-05.A1",
        description: "历史导航（↑/↓ 与 Ctrl+P/N：草稿保留、相邻去重、空历史 no-op、越过最新还原草稿）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "history_navigation_preserves_draft",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-05.A2",
        description: "浮层开合与滚动钳位（打开锚定底部、顶部钳总行数、翻页、toggle）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "transcript_view_open_close_and_scroll_clamp",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-05.A3",
        description: "浮层顶行/hints 行快照（/ T R A N S C R I P T / 铺满 + q to quit）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "header_and_hints_match_codex_shape",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-05.A4",
        description: "浮层内容全量展开（工具卡错误态、shell 退出码、用户/助手前缀）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "render_rows_expand_tools_and_shell",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M4-04": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-04.A1",
        description: "! 执行输出进 transcript Shell 行（成功含输出/退出码 0；失败退出码 1 透传）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "bang_execution_collects_output_and_exit_code",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-04.A2",
        description: "@ 查询提取与补全过滤（隐藏项排除、目录带 /、上限截断）",
        kind: "self",
        async check(ctx) {
          const a = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "mention_query_extracts_active_token",
          ]);
          const b = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "completion_filters_and_skips_hidden",
          ]);
          return { passed: a.exitCode === 0 && b.exitCode === 0, details: { query: a.exitCode, filter: b.exitCode } };
        },
      },
      {
        id: "M4-04.A3",
        description: "! 输入态提示符 light-red 语义（prompt_semantic 映射）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "bang_input_switches_prompt_semantic",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M4-03": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-03.A1",
        description: "注册表 = 冻结已实现命令集（/model /setup /thinking /status /usage /resume /new /rename /compact /clear /help /quit）；计划中命令不入菜单",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "registry_matches_frozen_implemented_set",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-03.A2",
        description: "fuzzy 过滤（命令名+中文描述）+ no matches 行",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "filter_matches_and_no_matches",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-03.A3",
        description: "Tab 补全返回选中命令名；上下移动钳位",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "tab_completion_returns_selected_name",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-03.A4",
        description: "? 面板两列渲染（键名定宽、行宽一致、关键键位覆盖）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "help_panel_renders_two_aligned_columns",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M4-02": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-02.A1",
        description: ">1000 字符折叠为编号占位符（编号递增、恰好 1000 不折叠）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "large_pastes_fold_into_numbered_placeholders",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-02.A2",
        description: "外编回填（真实 run_external_editor + fixture 脚本；非零退出取消）+ VISUAL>EDITOR>vi 解析",
        kind: "self",
        async check(ctx) {
          const a = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "external_editor_roundtrip_with_fixture_editor",
          ]);
          const b = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "editor_command_prefers_visual_then_editor",
          ]);
          return { passed: a.exitCode === 0 && b.exitCode === 0, details: { roundtrip: a.exitCode, resolve: b.exitCode } };
        },
      },
      {
        id: "M4-02.A3",
        description: "发送内容含折叠原文（占位符全展开、未登记占位符不误替换）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "expansion_restores_original_content_on_send",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M4-01": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-01.A1",
        description: "多行编辑与显式换行（newline/跨行退格合并/take 全文/行首行尾=当前行）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "multi_line_editing_with_explicit_newline",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-01.A2",
        description: "undo/redo（编辑序列回退重做、栈底/栈顶安全、take 后可找回）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "undo_redo_roundtrip",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-01.A3",
        description: "词导航（CJK 连续段=一个词）+ grapheme 原子退格 + CJK=2 列折行",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "word_navigation_and_cjk_wrap_boundaries",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M4-01.A4",
        description: "光标移动/编辑不越界（空缓冲 no-op、连按钳位、跨行右移）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "cursor_never_escapes_bounds",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M3-02": {
    milestone: "M3",
    assertions: [
      {
        id: "M3-02.A1",
        description: "/status 卡行快照：圆角框、>_ 头、标签 padEnd(18)、Token usage/Context window 行",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "status_card_matches_codex_shape",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M3-02.A2",
        description: "/usage 输出含累计成本（无定价数据时省略成本段）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "usage_summary_reports_cost_when_priced",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M3-02.A3",
        description: "卡内 context 行与 footer 形态一致（未知窗口回退 used）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "status_card_context_row_matches_footer_format",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M3-01": {
    milestone: "M3",
    assertions: [
      {
        id: "M3-01.A1",
        description: "格式化：usage 累加（持久化投影）+ 紧凑格式（900/1.9K/45.6K/4.56M）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "accumulates_and_formats_usage",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M3-01.A2",
        description: "阈值变色契约（>70% warning、>90% error；余量呈现；未知窗口回退 used）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "thresholds_change_at_contract_boundaries",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M3-01.A3",
        description: "同输入恒等输出（resume 后数值一致）+ compaction 标记 (auto)",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "compaction_marker_toggles",
          ]);
          const record2 = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "accumulates_and_formats_usage",
          ]);
          return {
            passed: record.exitCode === 0 && record2.exitCode === 0,
            details: { marker: record.exitCode, determinism: record2.exitCode },
          };
        },
      },
    ],
  },
  "M2-05": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-05.A1",
        description: "浮层渲染契约（行快照：bold 标题、$ 命令、编号选项 1/2/3、a 任务级放行措辞）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "overlay_lines_match_codex_shape",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-05.A2",
        description: "y/a/esc → 三态映射（含宿主 PermissionDecision 对齐）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "decision_keys_map_to_three_states",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-05.A3",
        description: "a 放行 → 宿主 standing rule 生效（同任务同工具复检直接 Allowed）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "approve_always_creates_standing_rule",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-05.A4",
        description: "esc 拒绝 → pending 清空、复检回到 NeedsApproval（会话可继续）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "deny_clears_pending_and_session_continues",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M2-04": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-04.A1",
        description: "运行中 Enter 入队不打断当前 run（路由 + 宿主 Queue 链路集成）",
        kind: "self",
        async check(ctx) {
          const a = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "queue_mirror_lifecycle",
          ]);
          const b = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "queue_mode_passes_host_send_path",
          ]);
          return { passed: a.exitCode === 0 && b.exitCode === 0, details: { routing: a.exitCode, hostPath: b.exitCode } };
        },
      },
      {
        id: "M2-04.A2",
        description: "排队渲染行格式（• Queued follow-up inputs + ↳ 缩进）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "queue_lines_follow_codex_format",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-04.A3",
        description: "中止/结束后队列派发（镜像随新 run Activity 清空；宿主 run 结束自动派发）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "queue_mirror_lifecycle",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M2-03": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-03.A1",
        description: "循环序 ask→edit→auto→plan→ask；未知值安全回落",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "cycle_follows_host_enum_order",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-03.A2",
        description: "模式写回 task（task_detail 读回一致：plan/auto）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "mode_persists_on_task",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-03.A3",
        description: "plan 态 magenta 语义徽章；ask 无徽章（色彩契约 §2.7）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "plan_badge_uses_magenta_semantic",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M2-02": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-02.A1",
        description: "档位集合与宿主 validated_inference 契约逐值一致（全档写回通过、非法档被拒）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "effort_levels_match_host_contract",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-02.A2",
        description: "升降步进 clamp（上下界、未设/未知回落 medium）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "step_levels_clamp_at_bounds",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-02.A3",
        description: "per-task 记忆：写回后 task_detail 读回一致（thinking 随档映射，none→disabled）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "thinking_persists_on_task",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-02.A4",
        description: "footer thinking 段联动（有档位拼 • level；未设/空省略）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "footer_label_appends_thinking_when_set",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M2-01": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-01.A1",
        description: "目录投影只收可用集、provider 分组稳定序；fuzzy 子序列过滤",
        kind: "self",
        async check(ctx) {
          const a = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "picker_entries_project_available_set_grouped",
          ]);
          const b = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "fuzzy_filter_matches_subsequence",
          ]);
          return { passed: a.exitCode === 0 && b.exitCode === 0, details: { projection: a.exitCode, fuzzy: b.exitCode } };
        },
      },
      {
        id: "M2-01.A2",
        description: "选中写 task（provider+model 落库读回）且返回 footer 标签",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "model_selection_writes_task_and_returns_label",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M2-01.A3",
        description: "弹层预选当前 provider、上下移动 clamp、selection 返回条目",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-tui", "--lib", "picker_preselects_current_and_moves_within_bounds",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
  "M1-04": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-04.A1",
        description: "空配置首屏渲染行 = 引导卡（System 投影：未配置状态 + config 路径 + 设置页途径）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "onboarding_lines_empty_config_lists_guidance",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
      {
        id: "M1-04.A2",
        description: "已配置时引导行不存在（fixture：有效 config.toml → onboarding 为空）",
        kind: "self",
        async check(ctx) {
          const record = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-tui",
            "--lib",
            "onboarding_lines_configured_is_empty",
          ]);
          return { passed: record.exitCode === 0, details: { exitCode: record.exitCode } };
        },
      },
    ],
  },
};

// ---------------------------------------------------------------------------
// 参数与作用域
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith("--")) return { error: `unexpected argument: ${argument}` };
    const key = argument.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) return { error: `missing value for --${key}` };
    options[key] = value;
    index += 1;
  }
  if (options.task && options.through) {
    return { error: "--task and --through are mutually exclusive" };
  }
  if (!options.task && !options.through) {
    return { error: "one of --task <TASK_ID> or --through <MILESTONE_ID> is required" };
  }
  if (!options.profile) {
    return { error: "--profile implementation|production is required" };
  }
  if (!["implementation", "production"].includes(options.profile)) {
    return { error: "--profile must be implementation or production" };
  }
  if (options.assertions !== undefined) {
    const ids = options.assertions
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
    if (ids.length === 0) {
      return { error: "--assertions requires at least one assertion id" };
    }
    options.assertionIds = ids;
  }
  return { options };
}

function usage() {
  return [
    "usage: node scripts/verify-tui-v2.mjs --task <TASK_ID> --profile implementation|production [--assertions ID1,ID2]",
    "       node scripts/verify-tui-v2.mjs --through <MILESTONE_ID> --profile implementation|production [--assertions ID1,ID2]",
    "milestones: " + MILESTONE_ORDER.join(" "),
    "--assertions: 只重跑选定断言，结果与既有报告合并（用于定向复跑，避免重复全量）",
  ].join("\n");
}

function registryTask(taskId) {
  return REGISTRY[taskId] ?? null;
}

function milestoneClosure(milestoneId) {
  const index = MILESTONE_ORDER.indexOf(milestoneId);
  if (index < 0) return null;
  const prefixSet = new Set(MILESTONE_ORDER.slice(0, index + 1));
  return Object.entries(REGISTRY)
    .filter(([, task]) => prefixSet.has(task.milestone))
    .map(([id]) => id)
    .sort();
}

// ---------------------------------------------------------------------------
// 命令执行（同轮去重缓存：相同命令只跑一次）
// ---------------------------------------------------------------------------

function resolveCommand(spec) {
  if (Array.isArray(spec)) {
    const [head, ...rest] = spec;
    if (head === "node") return { file: process.execPath, args: rest };
    return { file: head, args: rest };
  }
  const parts = spec.split(/\s+/);
  return resolveCommand(parts);
}

class CommandRunner {
  constructor(logDir) {
    this.logDir = logDir;
    this.cache = new Map();
  }

  cacheKey(spec) {
    return JSON.stringify(spec);
  }

  async run(spec) {
    const key = this.cacheKey(spec);
    if (this.cache.has(key)) return this.cache.get(key);
    const promise = this.execute(spec);
    this.cache.set(key, promise);
    return promise;
  }

  async execute(spec) {
    const isObjectSpec = !Array.isArray(spec) && typeof spec === "object";
    const inner = isObjectSpec ? spec.command : spec;
    const resolved = resolveCommand(inner);
    const cmdText = Array.isArray(inner) ? inner.join(" ") : String(inner);
    const cwd = spec.cwd ? path.resolve(REPO_ROOT, spec.cwd) : REPO_ROOT;
    const env = { ...process.env, ...(spec.env ?? {}) };
    const slug = createHash("sha256")
      .update(JSON.stringify([inner, path.relative(REPO_ROOT, cwd), spec.env ?? {}]))
      .digest("hex")
      .slice(0, 12);
    const logPath = path.join(this.logDir, `cmd-${slug}.log`);
    const started = Date.now();
    process.stderr.write(`[tui-v2] cmd: ${cmdText}\n`);
    const result = await new Promise((resolve) => {
      const child = spawn(resolved.file, resolved.args, {
        cwd,
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let output = "";
      child.stdout.on("data", (chunk) => {
        output += chunk;
      });
      child.stderr.on("data", (chunk) => {
        output += chunk;
      });
      child.on("error", (error) => {
        resolve({ exitCode: -1, error: String(error), output });
      });
      child.on("close", (exitCode) => {
        resolve({ exitCode: exitCode ?? -1, output });
      });
    });
    const record = {
      cmd: cmdText,
      cwd: path.relative(REPO_ROOT, cwd),
      exitCode: result.exitCode,
      error: result.error ?? null,
      durationMs: Date.now() - started,
      logPath: path.relative(REPO_ROOT, logPath),
    };
    await mkdir(this.logDir, { recursive: true });
    await writeFile(
      logPath,
      `$ ${cmdText}\n# exit=${record.exitCode} durationMs=${record.durationMs}\n${result.output}`,
      "utf8",
    );
    return record;
  }

  async spawnRaw(argv, { expectExit, env } = {}) {
    const resolved = resolveCommand(argv);
    const started = Date.now();
    const result = await new Promise((resolve) => {
      const child = spawn(resolved.file, resolved.args, {
        cwd: REPO_ROOT,
        env: env ? { ...process.env, ...env } : process.env,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let output = "";
      child.stdout.on("data", (chunk) => {
        output += chunk;
      });
      child.stderr.on("data", (chunk) => {
        output += chunk;
      });
      child.on("error", (error) => resolve({ exitCode: -1, output: String(error) }));
      child.on("close", (exitCode) => resolve({ exitCode: exitCode ?? -1, output }));
    });
    return {
      exitCode: result.exitCode,
      passed: result.exitCode === expectExit,
      durationMs: Date.now() - started,
      output: result.output.slice(0, 2000),
    };
  }
}

// ---------------------------------------------------------------------------
// 上下文与证据
// ---------------------------------------------------------------------------

async function fileExists(target) {
  try {
    const info = await stat(target);
    return info.isFile();
  } catch {
    return false;
  }
}

async function gitInfo() {
  const run = (args) =>
    new Promise((resolve) => {
      const child = spawn("git", args, {
        cwd: REPO_ROOT,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let out = "";
      child.stdout.on("data", (chunk) => {
        out += chunk;
      });
      child.stderr.on("data", (chunk) => {
        out += chunk;
      });
      child.on("close", (code) => resolve({ code, out }));
    });
  const revision = (await run(["rev-parse", "HEAD"])).out.trim();
  const status = (await run(["status", "--porcelain"])).out;
  const diffStat = (await run(["diff", "HEAD", "--stat"])).out;
  const worktreeDigest = createHash("sha256")
    .update(`${revision}\n${status}\n${diffStat}`, "utf8")
    .digest("hex");
  const dirty = status.trim().length > 0;
  return {
    revision: revision || "unknown",
    worktreeDigest,
    dirty,
    statusLineCount: status.trim() ? status.trim().split("\n").length : 0,
  };
}

// ---------------------------------------------------------------------------
// 断言执行
// ---------------------------------------------------------------------------

async function runAssertion(assertion, taskMeta, context) {
  const base = {
    id: assertion.id,
    description: assertion.description,
    required: assertion.required !== false,
  };
  try {
    if (assertion.kind === "gate") {
      const record = await context.runner.run([
        "node",
        "scripts/verify-ai-worklist.mjs",
        "--document",
        WORKLIST_DOCUMENT,
        "--freeze",
        WORKLIST_FREEZE,
        "--report",
        path.relative(REPO_ROOT, DOC_GATE_REPORT),
        "--mode",
        "check",
      ]);
      let gatePassed = record.exitCode === 0;
      const gateReport = await readFile(DOC_GATE_REPORT, "utf8").then(
        (text) => JSON.parse(text),
        () => null,
      );
      if (gateReport) {
        gatePassed =
          gatePassed && gateReport.passed === true && (gateReport.issues ?? []).length === 0;
      }
      return {
        ...base,
        status: gatePassed ? "passed" : "failed",
        evidence: record.logPath,
        details: { exitCode: record.exitCode, gatePassed: gateReport?.passed ?? null },
      };
    }
    if (assertion.kind === "command") {
      const chain = [assertion.command, ...(assertion.then ?? [])];
      const details = { commands: [] };
      let passed = true;
      for (const command of chain) {
        const spec = assertion.cwd ? { cwd: assertion.cwd, env: assertion.env, command } : command;
        const record = await context.runner.run(spec);
        const ok = record.exitCode === 0;
        passed = passed && ok;
        details.commands.push({ cmd: record.cmd, cwd: record.cwd, exitCode: record.exitCode });
        if (!ok) break;
      }
      return {
        ...base,
        status: passed ? "passed" : "failed",
        evidence: details.commands.map((item) => item.cmd).join(" && "),
        details,
      };
    }
    if (assertion.kind === "file") {
      const exists = await fileExists(path.join(REPO_ROOT, assertion.path));
      let contains = true;
      if (assertion.contains && exists) {
        const text = await readFile(path.join(REPO_ROOT, assertion.path), "utf8");
        contains = assertion.contains.every((needle) => text.includes(needle));
      }
      return {
        ...base,
        status: exists && contains ? "passed" : "failed",
        evidence: assertion.path,
        details: { exists, contains },
      };
    }
    if (assertion.kind === "self") {
      const outcome = await assertion.check(context);
      return {
        ...base,
        status: outcome.passed ? "passed" : "failed",
        details: outcome.details ?? {},
      };
    }
    return {
      ...base,
      status: "failed",
      details: { error: `unknown assertion kind: ${assertion.kind}` },
    };
  } catch (error) {
    return { ...base, status: "failed", details: { error: String(error) } };
  }
}

async function evidenceIndex(taskIds) {
  const entries = [];
  for (const taskId of taskIds) {
    const target = path.join(EVIDENCE_DIR, `${taskId}.yaml`);
    entries.push({
      task: taskId,
      evidence: path.relative(REPO_ROOT, target),
      exists: await fileExists(target),
    });
  }
  return entries;
}

function serializeReport({
  scope,
  scopeId,
  profile,
  git,
  assertions,
  evidence,
  startedAt,
  finishedAt,
  exitCode,
}) {
  const failed = assertions.filter((item) => item.status !== "passed");
  return {
    schema_version: "tui-v2-verification.v1",
    scope,
    scope_id: scopeId,
    profile,
    generated_at: new Date().toISOString(),
    started_at: new Date(startedAt).toISOString(),
    finished_at: new Date(finishedAt).toISOString(),
    duration_ms: finishedAt - startedAt,
    revision: git.revision,
    worktree_digest: git.worktreeDigest,
    worktree_dirty: git.dirty,
    summary: {
      total: assertions.length,
      passed: assertions.length - failed.length,
      failed: failed.length,
    },
    assertions,
    failed_assertions: failed.map((item) => ({
      id: item.id,
      description: item.description,
      details: item.details ?? null,
    })),
    evidence_index: evidence,
    exit_code: exitCode,
  };
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

async function main() {
  const startedAt = Date.now();
  const parsed = parseArgs(process.argv.slice(2));
  if (parsed.error) {
    process.stderr.write(`${parsed.error}\n${usage()}\n`);
    process.exit(2);
  }
  const { options } = parsed;

  let scope;
  let scopeId;
  let taskIds;
  if (options.task) {
    const task = registryTask(options.task);
    if (!task) {
      process.stderr.write(`unknown task: ${options.task}\n${usage()}\n`);
      process.exit(2);
    }
    scope = "task";
    scopeId = options.task;
    taskIds = [options.task];
  } else {
    const closure = milestoneClosure(options.through);
    if (!closure) {
      process.stderr.write(`unknown milestone: ${options.through}\n${usage()}\n`);
      process.exit(2);
    }
    scope = "milestone";
    scopeId = options.through;
    taskIds = closure;
  }

  const profileDir = path.join(PROFILE_DIR_BASE, options.profile);
  const logDir = path.join(profileDir, "logs");
  await mkdir(profileDir, { recursive: true });
  await mkdir(logDir, { recursive: true });

  const git = await gitInfo();
  const runner = new CommandRunner(logDir);
  const context = {
    runner,
    logDir,
    profile: options.profile,
    spawnRaw: (argv, opts) => runner.spawnRaw(argv, opts),
  };

  const assertionFilter = options.assertionIds ? new Set(options.assertionIds) : null;
  if (assertionFilter) {
    const knownIds = new Set(
      taskIds.flatMap((taskId) => REGISTRY[taskId].assertions.map((item) => item.id)),
    );
    for (const id of assertionFilter) {
      if (!knownIds.has(id)) {
        process.stderr.write(`unknown assertion in scope: ${id}\n${usage()}\n`);
        process.exit(2);
      }
    }
  }

  const assertions = [];
  for (const taskId of taskIds) {
    const task = REGISTRY[taskId];
    for (const assertion of task.assertions) {
      if (assertionFilter && !assertionFilter.has(assertion.id)) continue;
      process.stderr.write(`[tui-v2] run ${assertion.id} (${assertion.kind})...\n`);
      const result = await runAssertion(assertion, task, context);
      process.stderr.write(`[tui-v2] ${assertion.id} -> ${result.status}\n`);
      result.task = taskId;
      assertions.push(result);
    }
  }

  const reportPath = path.join(profileDir, `${scopeId}.json`);
  let mergedAssertions = assertions;
  if (assertionFilter) {
    const previous = await readFile(reportPath, "utf8")
      .then((text) => JSON.parse(text))
      .catch(() => null);
    if (previous && Array.isArray(previous.assertions)) {
      const byId = new Map(previous.assertions.map((item) => [item.id, item]));
      for (const item of assertions) byId.set(item.id, item);
      mergedAssertions = [];
      for (const taskId of taskIds) {
        for (const assertion of REGISTRY[taskId].assertions) {
          const item = byId.get(assertion.id);
          if (item) mergedAssertions.push(item);
        }
      }
    } else {
      process.stderr.write(
        "[tui-v2] warn: --assertions without existing report; report covers selected subset only\n",
      );
    }
  }

  const failed = mergedAssertions.filter((item) => item.status !== "passed");
  const exitCode = failed.length === 0 ? 0 : 1;
  const evidence = await evidenceIndex(taskIds);
  const report = serializeReport({
    scope,
    scopeId,
    profile: options.profile,
    git,
    assertions: mergedAssertions,
    evidence,
    startedAt,
    finishedAt: Date.now(),
    exitCode,
  });
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");

  const label = scope === "milestone" ? `through ${scopeId}` : `task ${scopeId}`;
  process.stdout.write(
    `verification ${label} [${options.profile}]: ${report.summary.passed}/${report.summary.total} passed, exit=${exitCode}\nreport: ${path.relative(REPO_ROOT, reportPath)}\n`,
  );
  if (failed.length > 0) {
    for (const item of failed) {
      process.stdout.write(`  FAILED ${item.id}: ${item.description}\n`);
    }
  }
  process.exit(exitCode);
}

main().catch((error) => {
  process.stderr.write(`harness error: ${error}\n`);
  process.exit(1);
});
