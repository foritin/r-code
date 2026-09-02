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
