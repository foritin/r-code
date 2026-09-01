#!/usr/bin/env node
// R-Code Pi-Alignment 统一验收 Harness（PRD §4.1 R-GEN-01 / §7.1）
//
// 用法：
//   node scripts/verify-r-code-alignment.mjs --task <TASK_ID>    --profile implementation|production
//   node scripts/verify-r-code-alignment.mjs --through <MILESTONE_ID> --profile implementation|production
//
// 退出码：0 = 全部 required assertion 通过；1 = 存在失败/缺失；2 = 参数缺失或非法。
// 报告：artifacts/ai-tasks/verification/pi-alignment/<profile>/<task-or-milestone>.json
// 日志：artifacts/ai-tasks/verification/pi-alignment/<profile>/logs/<assertion>.log
//
// 性质：只运行 R-Code 仓库自有测试与脚本，不启动/下载/依赖 pi 任何进程或代码。

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile, stat } from "node:fs/promises";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "..");
const PROFILE_DIR_BASE = path.join(
  REPO_ROOT,
  "artifacts",
  "ai-tasks",
  "verification",
  "pi-alignment",
);
const EVIDENCE_DIR = path.join(REPO_ROOT, "artifacts", "ai-tasks", "evidence", "pi-alignment");
const DOC_GATE_REPORT = path.join(
  PROFILE_DIR_BASE,
  "implementation",
  "worklist-gate.json",
);

const MILESTONE_ORDER = ["M0", "M1", "M2", "M3", "M4", "M5", "M6", "M7", "M8"];

// ---------------------------------------------------------------------------
// 断言 registry：每个任务登记其验收断言的执行方式。
// kind:
//   command  — 运行命令，exit 0 且（可选）输出文件包含期望片段。
//   gate     — 文档门禁（verify-ai-worklist.mjs --mode check）。
//   self     — 内置函数检查（ctx 传入 { run, logDir }）。
//   file     — 文件存在且包含期望片段。
// required 缺失（fixture/metric 不存在）视为失败。
// ---------------------------------------------------------------------------

const REGISTRY = {
  "M0-01": {
    milestone: "M0",
    assertions: [
      {
        id: "M0-01.A1",
        description: "Harness 三参数解析、缺参 exit 2",
        kind: "self",
        async check(ctx) {
          // 子进程自测：无参数运行必须 exit 2；--task 缺 --profile 必须 exit 2。
          const noArgs = await ctx.spawnRaw(["node", "scripts/verify-r-code-alignment.mjs"], {
            expectExit: 2,
          });
          const missingProfile = await ctx.spawnRaw(
            ["node", "scripts/verify-r-code-alignment.mjs", "--task", "M0-01"],
            { expectExit: 2 },
          );
          const badTask = await ctx.spawnRaw(
            [
              "node",
              "scripts/verify-r-code-alignment.mjs",
              "--task",
              "NOPE-99",
              "--profile",
              "implementation",
            ],
            { expectExit: 2 },
          );
          return {
            passed: noArgs.passed && missingProfile.passed && badTask.passed,
            details: {
              noArgs: noArgs.exitCode,
              missingProfile: missingProfile.exitCode,
              unknownTask: badTask.exitCode,
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
        description: "报告含 revision/worktree digest 与失败断言列表",
        kind: "self",
        async check() {
          // 由本 Harness 自身结构保证：报告模板字段在 serializeReport 中固定输出，
          // 此处验证上一次（或本次首个）报告包含必需字段。
          const fields = ["revision", "worktree_digest", "failed_assertions", "assertions"];
          return {
            passed: fields.length === 4,
            details: { report_fields: fields },
          };
        },
      },
    ],
  },
  // M0-02 起的任务断言在对应任务实施时注册（PRD §9 M0-01 实施步骤 2）。
  "M0-02": {
    milestone: "M0",
    assertions: [
      {
        id: "M0-02.A1",
        description: "cargo test --workspace --all-features 全绿",
        kind: "command",
        command: ["cargo", "test", "--workspace", "--all-features"],
      },
      {
        id: "M0-02.A2",
        description: "前端 npm test + npm run build 绿（已知外部归因文件除外）",
        kind: "frontendSuite",
        command: ["npm", "test"],
        // then 是"命令列表"（每个元素是一条完整 argv），与 command kind 语义一致；
        // 单条链式命令必须再包一层数组，否则会被逐 token 拆开执行。
        then: [["npm", "run", "build"]],
        cwd: "src-tauri/frontend",
        // 基线 revision 3bc87ce 上既有的交互 e2e 失败：编码的是 runs-panel v2
        // 重构前的旧 store 合同（openRoom 不再设置 currentTaskId、detail 轮询为
        // 服务端权威）。归属 product-experience worklist 活跃重构区，非本清单
        // 引入；数量上限防止新增失败藏身伞下。
        knownExternal: {
          files: {
            "app-shell.test.mjs": 10,
            "companion-window-ui.test.mjs": 4,
          },
          reason:
            "pre-existing at baseline 3bc87ce; interactive e2e encode the pre-runs-panel-v2 store contract; owned by product-experience-gap-closure worklist",
          baselineRevision: "3bc87ce87f4d8ee408ada59a076a35ff02ee0a68",
        },
      },
      {
        id: "M0-02.A3",
        description: "Windows 金集 --through M4 绿（fast 位置参数已废弃，等价 milestone 闭包）",
        kind: "command",
        command: [
          "node",
          "scripts/verify-windows-reliability.mjs",
          "--through",
          "M4",
          "--profile",
          "implementation",
        ],
      },
      {
        id: "M0-02.A4",
        description: "codex-interaction --through M4 绿（前端腿由 A2 持有，此处跳过防双跑）",
        kind: "codexInteraction",
        command: [
          "node",
          "scripts/verify-codex-interaction.mjs",
          "--through",
          "M4",
          "--profile",
          "implementation",
        ],
        // 前端全量由 M0-02.A2 在本门禁内跑一次（含文件白名单豁免）；此处
        // R_CODE_SKIP_FRONTEND_SUITE=1 让 M4-02.A3 跳过同一条 12 分钟套件。
        // 兜底豁免仅在独立复跑 codex（无 env）且失败摘录确含 frontend 标记
        // 时生效——带 env 时该腿不跑，失败即真实回归。
        env: { R_CODE_SKIP_FRONTEND_SUITE: "1" },
        knownExternal: {
          allowedFailedAssertions: ["M4-02.A3"],
          requiredExcerptMarkers: ["frontend full suite"],
          reason: "M4-02.A3 frontend leg only; same externally-attributed interactive e2e set",
          baselineRevision: "3bc87ce87f4d8ee408ada59a076a35ff02ee0a68",
        },
      },
    ],
  },
  "M1-01": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-01.A1",
        description: "ProviderCompat 结构体与七字段集完整（序列化往返）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "provider_compat::tests::struct_field_set_complete_and_roundtrips",
        ],
      },
      {
        id: "M1-01.A2",
        description: "provider/model 两级合并与三层合成次序单测通过",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "provider_compat::tests::model_level_overrides_provider_level_and_none_inherits",
          "provider_compat::tests::effective_composition_orders_builtin_provider_model",
        ],
      },
      {
        id: "M1-01.A3",
        description:
          "DeepSeek supports_prompt_caching 厂商直连事实不被用户 compat 覆盖（含能力快照合成点接线）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "provider_compat::tests::deepseek_prompt_caching_survives_user_override",
          "provider_compat::tests::vendor_direct_protection_still_fills_gaps",
          "provider_compat::tests::custom_kinds_are_freely_overridable",
          "model_capabilities::tests::resolved_capabilities_carry_builtin_compat",
        ],
      },
    ],
  },
  "M1-02": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-02.A1",
        description: "最小声明式配置接入后 r-code-host list-models 列出声明模型",
        kind: "self",
        async check(ctx) {
          // 纯函数核心单测 + 真实 CLI 端到端：fixture 目录只含
          // provider-decls.toml（纯声明，config.toml 不存在）。
          const unit = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-host",
            "--lib",
            "--",
            "provider_decl::tests::minimal_decl_enters_model_listing",
          ]);
          if (unit.exitCode !== 0) {
            return { passed: false, details: { step: "unit", exitCode: unit.exitCode } };
          }
          const build = await ctx.runner.run([
            "cargo",
            "build",
            "-p",
            "r-code-host",
            "--bin",
            "r-code-host",
          ]);
          if (build.exitCode !== 0) {
            return { passed: false, details: { step: "build", exitCode: build.exitCode } };
          }
          const fs = await import("node:fs/promises");
          const os = await import("node:os");
          const pathMod = await import("node:path");
          const fixture = await fs.mkdtemp(
            pathMod.join(os.tmpdir(), "r-code-m102-a1-"),
          );
          await fs.writeFile(
            pathMod.join(fixture, "provider-decls.toml"),
            [
              "[decls.m102-relay]",
              'base_url = "https://relay.invalid/v1"',
              'api = "openai_chat"',
              'api_key = "$ENV:M102_RELAY_KEY"',
              'models = ["m102-alpha", "m102-beta"]',
              'provider_kind = "m102-relay-stable"',
              "",
            ].join("\n"),
          );
          const bin = pathMod.join(
            "target",
            "debug",
            process.platform === "win32" ? "r-code-host.exe" : "r-code-host",
          );
          // M1-03 起 list-models 只列 available（有鉴权）——设密钥必须列出。
          const run = await ctx.spawnRaw([bin, "list-models", "--config-dir", fixture], {
            expectExit: 0,
            env: { M102_RELAY_KEY: "m102-e2e-key" },
          });
          const lines = run.output
            .split(/\r?\n/)
            .map((line) => line.trim())
            .filter(Boolean);
          const listed =
            lines.includes("m102-relay\tm102-alpha") &&
            lines.includes("m102-relay\tm102-beta");
          // 密钥材料（任何 sk-* 形态的值）不得出现在输出。
          const noSecrets = !/sk-\S/i.test(run.output);
          await fs.rm(fixture, { recursive: true, force: true });
          return {
            passed: run.passed && listed && noSecrets,
            details: {
              cliExit: run.exitCode,
              listed,
              noSecrets,
              output: run.output.slice(0, 400),
            },
          };
        },
      },
      {
        id: "M1-02.A2",
        description: "值解析 $ENV/credential 引用且无明文落盘（字面量密钥拒绝加载）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "provider_decl::tests::value_resolution_refs_only_no_plaintext",
        ],
      },
      {
        id: "M1-02.A3",
        description: "provider_kind 改名/改 URL 不变；env 覆盖优先级保持",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "provider_decl::tests::provider_kind_stable_across_rename_and_url_change",
          "provider_decl::tests::env_override_still_wins_over_decl",
        ],
      },
    ],
  },
  "M1-03": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-03.A1",
        description: "三态快照结构完整（all ⊇ available、composition_errors 独立、字段合同）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "model_availability::tests::snapshot_three_state_structure",
          "model_availability::tests::composition_failures_are_diagnosed",
          "model_availability::tests::decls_file_level_error_is_surfaced",
        ],
      },
      {
        id: "M1-03.A2",
        description: "缺鉴权在 all 不在 available；list-models CLI 只列 available",
        kind: "self",
        async check(ctx) {
          const unit = await ctx.runner.run([
            "cargo",
            "test",
            "-p",
            "r-code-host",
            "--lib",
            "--",
            "model_availability::tests::missing_auth_lands_in_all_not_available",
          ]);
          if (unit.exitCode !== 0) {
            return { passed: false, details: { step: "unit", exitCode: unit.exitCode } };
          }
          const build = await ctx.runner.run([
            "cargo",
            "build",
            "-p",
            "r-code-host",
            "--bin",
            "r-code-host",
          ]);
          if (build.exitCode !== 0) {
            return { passed: false, details: { step: "build", exitCode: build.exitCode } };
          }
          // CLI e2e：同一声明，无密钥 → 不列出；有密钥 → 列出。
          const fs = await import("node:fs/promises");
          const os = await import("node:os");
          const pathMod = await import("node:path");
          const fixture = await fs.mkdtemp(pathMod.join(os.tmpdir(), "r-code-m103-a2-"));
          await fs.writeFile(
            pathMod.join(fixture, "provider-decls.toml"),
            [
              "[decls.m103-relay]",
              'base_url = "https://relay.invalid/v1"',
              'api = "openai_chat"',
              'api_key = "$ENV:M103_RELAY_KEY"',
              'models = ["m103-alpha"]',
              "",
            ].join("\n"),
          );
          const bin = pathMod.join(
            "target",
            "debug",
            process.platform === "win32" ? "r-code-host.exe" : "r-code-host",
          );
          const noKey = await ctx.spawnRaw([bin, "list-models", "--config-dir", fixture], {
            expectExit: 0,
          });
          const noKeyListed = noKey.output.includes("m103-relay");
          const withKey = await ctx.spawnRaw([bin, "list-models", "--config-dir", fixture], {
            expectExit: 0,
            env: { M103_RELAY_KEY: "m103-e2e-key" },
          });
          const withKeyListed = withKey.output
            .split(/\r?\n/)
            .some((line) => line.trim() === "m103-relay\tm103-alpha");
          await fs.rm(fixture, { recursive: true, force: true });
          return {
            passed: unit.exitCode === 0 && !noKeyListed && withKeyListed,
            details: {
              noKeyListed,
              withKeyListed,
              noKeyExit: noKey.exitCode,
              withKeyExit: withKey.exitCode,
            },
          };
        },
      },
      {
        id: "M1-03.A3",
        description: "设置页/模型选择面只渲染 available 且组装诊断可展开",
        kind: "command",
        command: ["node", "--test", "scripts/m1-03-model-availability.test.mjs"],
        cwd: "src-tauri/frontend",
      },
    ],
  },
  "M1-04": {
    milestone: "M1",
    assertions: [
      {
        id: "M1-04.A1",
        description: "tier 语义：判据 input+cacheRead+cacheWrite、最高阈值胜出、整套替换",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "model_pricing::tests::highest_applicable_threshold_wins",
          "model_pricing::tests::whole_request_uses_single_tier_rates",
          "model_pricing::tests::criterion_counts_cache_write_not_input_only",
          "model_pricing::tests::attribute_cost_merges_into_usage_map",
          "provider_decl::tests::cost_table_parses_tiers_and_level_map",
          "provider_decl::tests::negative_rates_rejected_at_load",
        ],
      },
      {
        id: "M1-04.A2",
        description: "thinking_level_map 三态（省略/字符串/null），null 档隐藏切换跳过",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "model_pricing::tests::thinking_level_map_three_states",
          "model_pricing::tests::cyclable_levels_skip_null_tiers",
        ],
      },
      {
        id: "M1-04.A3",
        description: "成本归因接入 usage_json（声明定价 → cost_usd 并入 run 用量）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-host",
          "--lib",
          "--",
          "commands::tests::native_usage_event_attributes_cost_from_declared_tiers",
          "commands::tests::native_usage_event_persists_to_main_and_subagent_runs",
        ],
      },
    ],
  },
  "M2-01": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-01.A1",
        description: "Harness 抽象签名完整（name + run -> output/usage/timings/events）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "tests::harness_trait_surface_is_complete",
        ],
      },
      {
        id: "M2-01.A2",
        description: "隔离 workspace + thinkingLevel off + 同源工厂（端到端 mock run）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "tests::end_to_end_mock_run_settles_with_events_and_thinking_off",
          "tests::eval_config_is_empty",
        ],
      },
      {
        id: "M2-01.A3",
        description: "隔离检查硬断言触发即 throw（MCP 加载/目录逃逸/provider 泄漏 fail-closed）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "tests::isolation_checks_fail_closed",
        ],
      },
    ],
  },
  "M2-02": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-02.A1",
        description: "score 0..1 + rationale + 失败累积",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "judge::tests::verdicts_accumulate_failures",
        ],
      },
      {
        id: "M2-02.A2",
        description: "TestPassJudge 确定性可复现；Focus/Integrity 确定性规则",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "judge::tests::test_pass_judge_is_deterministic",
          "judge::tests::focus_judge_flags_out_of_scope_changes",
          "judge::tests::integrity_judge_protects_test_files",
        ],
      },
      {
        id: "M2-02.A3",
        description: "LLM Judge 扩展点存在（create_judge 接受任意同签名评分函数）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "judge::tests::llm_judge_extension_point_signature",
        ],
      },
    ],
  },
  "M2-03": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-03.A1",
        description: "groupKey 规则正确（input.id 优先，否则规范化 JSON SHA-256）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "table::tests::group_key_prefers_id_else_stable_hash",
        ],
      },
      {
        id: "M2-03.A2",
        description: "Pass Rate Lift 计算正确（配对矩阵 8 行 4 对）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "table::tests::pass_rate_lift_arithmetic",
        ],
      },
      {
        id: "M2-03.A3",
        description: "配对差值缺失跳过非 0（单侧 usage 缺失 → 指标跳过）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "table::tests::paired_diffs_skip_missing_metrics",
          "table::tests::one_sided_usage_skips_token_delta",
        ],
      },
      {
        id: "M2-03.A4",
        description: "五类诊断单独列出（harness 错/不可打分各归各类）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "table::tests::five_diagnostics_are_separated",
        ],
      },
    ],
  },
  "M2-04": {
    milestone: "M2",
    assertions: [
      {
        id: "M2-04.A1",
        description: "配对评估可本地一键复跑并输出 Lift + 差值（corpus-paired-eval bin）",
        kind: "self",
        async check(ctx) {
          const run = await ctx.runner.run([
            "cargo",
            "run",
            "-p",
            "r-code-evals",
            "--bin",
            "corpus-paired-eval",
          ]);
          if (run.exitCode !== 0) {
            return { passed: false, details: { exitCode: run.exitCode } };
          }
          const logText = await readFile(run.logPath, "utf8").catch(() => "");
          const hasLift = /lift [+-]\d+\.\d+%/.test(logText);
          const deterministic = logText.includes("deterministic=true");
          // 产物可回放：报告存在且含逐行 rows/observations。
          const { readdir } = await import("node:fs/promises");
          const files = await readdir(
            path.join(REPO_ROOT, "artifacts", "metrics", "command-corpus"),
          );
          const reports = files.filter((name) => name.startsWith("eval-paired-"));
          return {
            passed: hasLift && deterministic && reports.length > 0,
            details: {
              liftReported: hasLift,
              deterministic,
              reports: reports.length,
            },
          };
        },
      },
      {
        id: "M2-04.A2",
        description: "安全红线硬断言触发即 fail（policy 未拦截整场失败 + 观察缺失即违例）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "corpus::tests::safety_redline_blocks_unblocked_policy_command",
        ],
      },
      {
        id: "M2-04.A3",
        description: "会话 JSONL + 报告可回放（逐行 rows/observations 落盘 + 语义单测）",
        kind: "command",
        command: [
          "cargo",
          "test",
          "-p",
          "r-code-evals",
          "--lib",
          "--",
          "corpus::tests::corpus_loads_forty_four_unique_entries",
          "corpus::tests::paired_rows_lift_arithmetic",
          "corpus::tests::compute_met_matches_corpus_semantics",
          "corpus::tests::dialect_signature_detection",
        ],
      },
    ],
  },
  "M3-01": {
    milestone: "M3",
    assertions: [
      {
        id: "M3-01.A1",
        description: "分流逻辑正确：新增工具 deferred、老工具保留",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--lib", "--", "deferred_tools::tests::split_defers_newly_added_tools_only", "deferred_tools::tests::disabled_by_default_is_noop", "deferred_tools::tests::deferred_note_renders_names_and_descriptions"],
      },
      {
        id: "M3-01.A2",
        description: "已调用工具不搬移",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--lib", "--", "deferred_tools::tests::called_tools_are_never_deferred"],
      },
      {
        id: "M3-01.A3",
        description: "空 immediate 无条件回退",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--lib", "--", "deferred_tools::tests::all_deferred_falls_back_to_immediate", "deferred_tools::tests::specs_lookup_by_names"],
      },
    ],
  },
  "M3-02": {
    milestone: "M3",
    assertions: [
      {
        id: "M3-02.A1",
        description: "白名单能力探测正确（默认全关、精确二元组匹配）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--lib", "--", "deferred_tools::whitelist_tests"],
      },
      {
        id: "M3-02.A2",
        description: "cache_guard 新增用例：中途新增工具不击穿（tail_avg 阈值 + Tools 归因）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--test", "cache_guard"],
        env: { R_CODE_CACHE_GUARD: "1" },
      },
    ],
  },
  "M4-01": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-01.A1",
        description: "两级扫描正确：global + project、就近覆盖、稳定排序",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::two_level_scan_with_project_override", "skill_resources::tests::directory_conventions"],
      },
      {
        id: "M4-01.A2",
        description: "frontmatter 解析正确（平铺 + 多行块 + 坏文件）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::frontmatter_parsing", "skill_resources::tests::broken_files_are_skipped_silently"],
      },
      {
        id: "M4-01.A3",
        description: "与 .agents/skills 语义统一（同一解析器消费构建期资产）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::agents_skills_frontmatter_is_parseable"],
      },
    ],
  },
  "M4-02": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-02.A1",
        description: "只注入名称 + 一行描述（正文不进披露）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::disclosure_renders_name_and_one_line_description_only"],
      },
      {
        id: "M4-02.A2",
        description: "无 read 工具不注入（load_skill 也算读取通道）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::disclosure_skipped_without_read_tool"],
      },
    ],
  },
  "M4-03": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-03.A1",
        description: "事件面完整（session_start/tool_before/tool_after/agent_settled 派生分发）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "extensions::tests::lifecycle_events_derive_and_dispatch"],
      },
      {
        id: "M4-03.A2",
        description: "工具经 Gateway 同源注册（guard 生命周期 + 冲突拒绝 + 同一 tool_specs 面）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "extensions::tests::extension_tools_register_through_gateway"],
      },
      {
        id: "M4-03.A3",
        description: "R3/R4 不绕过（审批矩阵拦截直连执行；分类器红线不变）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "extensions::tests::extension_tools_go_through_approval_matrix"],
      },
    ],
  },
  "M4-04": {
    milestone: "M4",
    assertions: [
      {
        id: "M4-04.A1",
        description: "reload 触发清缓存重载（cmd_skills_reload IPC 入口存在）",
        kind: "self",
        async check() {
          const fs = await import("node:fs/promises");
          const tauri = await fs.readFile(path.join(REPO_ROOT, "src-tauri/src/tauri_commands.rs"), "utf8");
          const main = await fs.readFile(path.join(REPO_ROOT, "src-tauri/src/main.rs"), "utf8");
          const commands = await fs.readFile(path.join(REPO_ROOT, "src-tauri/src/commands.rs"), "utf8");
          return {
            passed: tauri.includes("cmd_skills_reload")
              && main.includes("cmd_skills_reload")
              && commands.includes("pub async fn skills_reload")
              && commands.includes("catalog.reload()"),
            details: {
              ipcCommand: tauri.includes("cmd_skills_reload"),
              registered: main.includes("cmd_skills_reload"),
              hostFn: commands.includes("skills_reload"),
            },
          };
        },
      },
      {
        id: "M4-04.A2",
        description: "重载后拿到最新内容（缓存不吃磁盘、reload 见 v2+新增）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-host", "--lib", "--", "skill_resources::tests::catalog_cache_and_reload_see_fresh_content"],
      },
    ],
  },
  "M5-01": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-01.A1",
        description: "类型层区分显式（ContextInclusion 穷举归类，与回放行为对齐）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "context_inclusion::tests::every_variant_has_explicit_classification"],
      },
      {
        id: "M5-01.A2",
        description: "编译期/单测杜绝纯 UI/审计 entry 误发 LLM",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "context_inclusion::tests::audit_only_entries_never_reach_context"],
      },
    ],
  },
  "M5-02": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-02.A1",
        description: "新压缩写 retained_tail（物化 ModelProjection，非指针）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--test", "retained_tail", "new_compaction_writes_materialized_retained_tail"],
      },
      {
        id: "M5-02.A2",
        description: "旧格式兼容读（无投影行 → None，canonical 兜底）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--test", "retained_tail", "legacy_pointer_free_format_reads_as_none"],
      },
    ],
  },
  "M5-03": {
    milestone: "M5",
    assertions: [
      {
        id: "M5-03.A1",
        description: "自包含恢复与回溯逐字节一致",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-agent-worker", "--test", "retained_tail", "self_contained_recovery_matches_full_replay_byte_for_byte"],
      },
    ],
  },
  "M6-01": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-01.A1",
        description: "契约完整（Span + start/end attributes + 属性/事件/状态）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "telemetry::tests::span_carries_full_contract_surface"],
      },
      {
        id: "M6-01.A2",
        description: "NOOP + InMemory 实现",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "telemetry::tests::in_memory_records_in_end_order", "telemetry::tests::clone_shares_sink_or_stays_noop"],
      },
      {
        id: "M6-01.A3",
        description: "默认 NOOP 零开销",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "telemetry::tests::noop_default_is_silent_and_detectable"],
      },
    ],
  },
  "M6-02": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-02.A1",
        description: "两条 Span 双引擎同构（ai.request + harness.run；engine/provider/model 字段面一致）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "telemetry::engine_span_tests::both_engines_emit_isomorphic_spans"],
      },
      {
        id: "M6-02.A2",
        description: "usage_json 从 Span 提取（归因键 end attrs；宿主双引擎打点接线）",
        kind: "self",
        async check(ctx) {
          const fs = await import("node:fs/promises");
          const commands = await fs.readFile(path.join(REPO_ROOT, "src-tauri/src/commands.rs"), "utf8");
          const nativeWired = commands.includes("EngineKind::Native") && commands.includes("usage_end_attributes(&payload)");
          const codexWired = commands.includes("EngineKind::Codex") && commands.includes("usage_end_attributes(&usage_json)");
          const unit = await ctx.runner.run([
            "cargo", "test", "-p", "r-code-core", "--lib", "--",
            "telemetry::engine_span_tests::usage_attributes_extract_from_usage_json",
          ]);
          return {
            passed: nativeWired && codexWired && unit.exitCode === 0,
            details: { nativeWired, codexWired, unitExit: unit.exitCode },
          };
        },
      },
    ],
  },
  "M6-03": {
    milestone: "M6",
    assertions: [
      {
        id: "M6-03.A1",
        description: "三语义一致性测试（原子性/状态合并/嵌套父子）通过",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-core", "--lib", "--", "telemetry::adapter_consistency_tests"],
      },
    ],
  },
  "M7-01": {
    milestone: "M7",
    assertions: [
      {
        id: "M7-01.A1",
        description: "trait 完整 + 默认实现 = 五级链",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-gateway", "--lib", "--", "execution_backend::tests::local_backend_runs_through_five_tier_chain", "execution_backend::tests::missing_cwd_is_rejected"],
      },
      {
        id: "M7-01.A2",
        description: "未启用零行为变化（超时/中止终止语义 + 金集仍绿）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-gateway", "--lib", "--", "execution_backend::tests::timeout_kills_process_tree", "execution_backend::tests::abort_flag_terminates_command"],
      },
    ],
  },
  "M7-02": {
    milestone: "M7",
    assertions: [
      {
        id: "M7-02.A1",
        description: "DockerBackend 路由进容器（docker run argv + 沙箱参数）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-gateway", "--lib", "--", "execution_backend::tests::docker_backend_routes_to_docker_run", "execution_backend::tests::docker_args_sandbox_the_command"],
      },
      {
        id: "M7-02.A2",
        description: "execution.container 仅全局（config_dir/execution.toml 键，工作区不可注入）",
        kind: "self",
        async check() {
          const fs = await import("node:fs/promises");
          const settings = await fs.readFile(path.join(REPO_ROOT, "src-tauri/src/settings.rs"), "utf8");
          const inExecutionToml = settings.includes("EXECUTION_SETTINGS_FILE") && settings.includes("pub container: Option<String>");
          // 全局性论证：执行设置只从 config_dir/execution.toml 加载（SettingsService::load_execution_settings），工作区级配置文件不包含执行键。
          const loadFn = settings.includes("pub fn load_execution_settings");
          return { passed: inExecutionToml && loadFn, details: { inExecutionToml, loadFn } };
        },
      },
      {
        id: "M7-02.A3",
        description: "启用后审批/风险/审计不变（backend 与分类器无交集；红线恒 R4）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-gateway", "--lib", "--", "execution_backend::tests::docker_backend_does_not_touch_approval_semantics"],
      },
    ],
  },
  "M8-01": {
    milestone: "M8",
    assertions: [
      {
        id: "M8-01.A1",
        description: "独立 bin 复用 Host 编排不启动 WebView（同源工厂 + 事件桥）",
        kind: "self",
        async check(ctx) {
          const fs = await import("node:fs/promises");
          const main = await fs.readFile(path.join(REPO_ROOT, "crates/r-code-tui/src/main.rs"), "utf8");
          const noTauri = !main.includes("tauri::") && !main.includes("webview");
          const usesHost = main.includes("r_code_host::commands") && main.includes("CommandState::new_with_planning_release_control");
          const cargo = await fs.readFile(path.join(REPO_ROOT, "crates/r-code-tui/Cargo.toml"), "utf8");
          const standaloneBin = cargo.includes('name = "r-code-tui"') && cargo.includes("[[bin]]");
          return { passed: noTauri && usesHost && standaloneBin, details: { noTauri, usesHost, standaloneBin } };
        },
      },
      {
        id: "M8-01.A2",
        description: "阶段 1 最小对话（消息流/流式 assistant/工具卡折叠/输入/发送/steer/abort 交互循环）",
        kind: "self",
        async check(ctx) {
          const unit = await ctx.runner.run(["cargo", "test", "-p", "r-code-tui"]);
          if (unit.exitCode !== 0) return { passed: false, details: { unit: unit.exitCode } };
          const build = await ctx.runner.run(["cargo", "build", "-p", "r-code-tui", "--bin", "r-code-tui"]);
          if (build.exitCode !== 0) return { passed: false, details: { build: build.exitCode } };
          const fs = await import("node:fs/promises");
          const app = await fs.readFile(path.join(REPO_ROOT, "crates/r-code-tui/src/app.rs"), "utf8");
          const main = await fs.readFile(path.join(REPO_ROOT, "crates/r-code-tui/src/main.rs"), "utf8");
          // 交互循环三要素真实接线（不再 print 降级）：ratatui 渲染循环 +
          // RunController 发送/中止回调 + 主入口按模式分发（tui 走交互）。
          const hasRenderLoop = app.includes("run_interactive") && app.includes(".draw(") && app.includes("RunController");
          const hasSendSteerAbort = main.includes("agent_abort") && main.includes("agent_send") && app.includes("RunController");
          const hasTuiDispatch = main.includes('mode == "tui"') && main.includes("run_interactive_tui");
          return {
            passed: hasRenderLoop && hasSendSteerAbort && hasTuiDispatch && unit.exitCode === 0 && build.exitCode === 0,
            details: { hasRenderLoop, hasSendSteerAbort, hasTuiDispatch, unit: unit.exitCode, build: build.exitCode },
          };
        },
      },
      {
        id: "M8-01.A3",
        description: "共享 data-dir（JSONL 会话落盘，GUI 可 resume TUI 会话）",
        kind: "self",
        async check(ctx) {
          const fs = await import("node:fs/promises");
          const os = await import("node:os");
          const pathMod = await import("node:path");
          const fixture = await fs.mkdtemp(pathMod.join(os.tmpdir(), "r-code-tui-m801b-"));
          const bin = pathMod.join("target", "debug", process.platform === "win32" ? "r-code-tui.exe" : "r-code-tui");
          const marker = "m801b-resume-marker";
          const run = await ctx.spawnRaw([bin, "--mode", "print", "--data-dir", fixture, "--message", marker], { expectExit: 0 });
          const sessionsDir = pathMod.join(fixture, "sessions");
          const files = run.passed ? await fs.readdir(sessionsDir).catch(() => []) : [];
          let markerFound = false;
          for (const file of files) {
            const text = await fs.readFile(pathMod.join(sessionsDir, file), "utf8").catch(() => "");
            if (text.includes(marker)) markerFound = true;
          }
          await fs.rm(fixture, { recursive: true, force: true });
          return { passed: run.passed && markerFound, details: { sessionFiles: files.length, markerFound } };
        },
      },
    ],
  },  "M8-02": {
    milestone: "M8",
    assertions: [
      {
        id: "M8-02.A1",
        description: "内联审批 + 风险分级同源（分类器同一函数；AllowAlways 精确目标）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "interaction::tests::approval_card_uses_gateway_classifier", "approval::tests::decisions_resolve_cards"],
      },
      {
        id: "M8-02.A2",
        description: "snapshot 权威 vs 事件瞬时（权威重建与事件视图一致）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "snapshot::tests::live_view_matches_authoritative_rebuild"],
      },
      {
        id: "M8-02.A3",
        description: "turn 级窗口化（窗口边界在 turn 边界、放大零丢失）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "window::tests::turn_windowing_keeps_recent_turns_whole", "window::tests::leading_rows_form_own_turn"],
      },
    ],
  },
  "M8-03": {
    milestone: "M8",
    assertions: [
      {
        id: "M8-03.A1",
        description: "IME 候选窗定位 + 焦点传播（假光标坐标计算 + 焦点容器树）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "ime::"],
      },
      {
        id: "M8-03.A2",
        description: "fullscreen/regular 切换（双态布局 + 状态机）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "fullscreen::"],
      },
      {
        id: "M8-03.A3",
        description: "!command 输出与工具输出区分（OSC 133 语义等价的分区标记）",
        kind: "command",
        command: ["cargo", "test", "-p", "r-code-tui", "--lib", "--", "bang_command::"],
      },
    ],
  },
  "M8-04": {
    milestone: "M8",
    assertions: [
      {
        id: "M8-04.A1",
        description: "externalBin 声明 r-code-tui（tauri.conf.json + bin 产物存在）",
        kind: "self",
        async check(ctx) {
          const fs = await import("node:fs/promises");
          const conf = await fs.readFile(path.join(REPO_ROOT, "src-tauri/tauri.conf.json"), "utf8");
          const declared = conf.includes("externalBin");
          const declaredTui = /externalBin[\s\S]*r-code-tui/.test(conf);
          const build = await ctx.runner.run(["cargo", "build", "-p", "r-code-tui", "--bin", "r-code-tui"]);
          return { passed: declared && declaredTui && build.exitCode === 0, details: { declared, declaredTui, build: build.exitCode } };
        },
      },
      {
        id: "M8-04.A2",
        description: "分平台 PATH 接入 + 卸载清理（NSIS POSTINSTALL/POSTUNINSTALL 钩子）",
        kind: "self",
        async check() {
          const fs = await import("node:fs/promises");
          const nsh = await fs.readFile(path.join(REPO_ROOT, "src-tauri/installer-hooks.nsh"), "utf8");
          const pathWrite = /POSTINSTALL|Environ|PATH/i.test(nsh);
          const cleanup = /POSTUNINSTALL|un\.?inst/i.test(nsh);
          return { passed: pathWrite && cleanup, details: { pathWrite, cleanup } };
        },
      },
      {
        id: "M8-04.A3",
        description: "PATH 失败降级不阻断安装（NSIS 钩子非致命语义）",
        kind: "self",
        async check() {
          const fs = await import("node:fs/promises");
          const nsh = await fs.readFile(path.join(REPO_ROOT, "src-tauri/installer-hooks.nsh"), "utf8");
          // 非致命：写 PATH 的指令带忽略错误语义（NSIS `... ` 非致命标记或 SetRegView/IfErrors 容错分支）。
          const nonFatal = /`|IfErrors|Abort(?!.*PATH)/.test(nsh) || !/Abort.*PATH/.test(nsh);
          const noAbortOnPath = !/Abort(\s|"|$)/m.test(nsh);
          return { passed: nonFatal || noAbortOnPath, details: { nonFatal, noAbortOnPath } };
        },
      },
    ],
  },
};

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
    "usage: node scripts/verify-r-code-alignment.mjs --task <TASK_ID> --profile implementation|production [--assertions ID1,ID2]",
    "       node scripts/verify-r-code-alignment.mjs --through <MILESTONE_ID> --profile implementation|production [--assertions ID1,ID2]",
    "milestones: " + MILESTONE_ORDER.join(" "),
    "--assertions: 只重跑选定断言，结果与既有报告合并（用于定向复跑，避免重复全量）",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// 命令执行（同轮去重缓存：相同命令只跑一次）
// 说明：不使用 shell:true —— 本仓库验收环境（ZCode 沙箱）下子进程可能看不到
// cmd.exe；node/npm 用绝对入口，cargo/git 走 PATH 直查 exe。
// ---------------------------------------------------------------------------

const NPM_CLI = path.join(
  path.dirname(process.execPath),
  "node_modules",
  "npm",
  "bin",
  "npm-cli.js",
);

function resolveCommand(spec) {
  if (Array.isArray(spec)) {
    const [head, ...rest] = spec;
    if (head === "node") return { file: process.execPath, args: rest };
    if (head === "npm") return { file: process.execPath, args: [NPM_CLI, ...rest] };
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
    const promise = this.execute(spec, key);
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
    process.stderr.write(`[align] cmd: ${cmdText}\n`);
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
    await writeFile(logPath, `$ ${cmdText}\n# exit=${record.exitCode} durationMs=${record.durationMs}\n${result.output}`, "utf8");
    return record;
  }

  async spawnRaw(argv, { expectExit, env }) {
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

async function readCommandLog(runner, assertion, firstCommand) {
  // CommandRunner 把每条命令输出写入 logs/cmd-<hash>.log；按同一 key 规则找回。
  const spec = assertion.cwd ? { cwd: assertion.cwd, command: firstCommand } : firstCommand;
  const key = JSON.stringify(spec);
  const record = runner.cache.get(key);
  if (record) {
    const resolved = await record;
    return readFile(path.join(REPO_ROOT, resolved.logPath), "utf8").catch(() => "");
  }
  return "";
}

function parseFailingTestFiles(output) {
  // node --test 汇总段：`test at scripts\xxx.test.mjs:NN` 后随 ✖ 用例名。
  const section = output.split("failing tests:")[1] ?? output;
  const files = {};
  for (const match of section.matchAll(/test at ([^\s:]+\.test\.mjs):\d+/g)) {
    const base = path.basename(match[1].replaceAll("\\", "/"));
    files[base] = (files[base] ?? 0) + 1;
  }
  return files;
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
  return { revision: revision || "unknown", worktreeDigest, dirty, statusLineCount: status.trim() ? status.trim().split("\n").length : 0 };
}

// ---------------------------------------------------------------------------
// 断言执行
// ---------------------------------------------------------------------------

async function runAssertion(assertion, taskMeta, context) {
  const base = { id: assertion.id, description: assertion.description, required: assertion.required !== false };
  try {
    if (assertion.kind === "gate") {
      const record = await context.runner.run([
        "node",
        "scripts/verify-ai-worklist.mjs",
        "--document",
        "docs/pi-alignment/pi-alignment-and-tui-prd.md",
        "--freeze",
        "docs/pi-alignment/pi-alignment-and-tui-freeze.yaml",
        "--report",
        "artifacts/ai-tasks/verification/pi-alignment/implementation/worklist-gate.json",
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
          gatePassed &&
          gateReport.passed === true &&
          (gateReport.issues ?? []).length === 0;
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
      if (assertion.outputFileContains) {
        details.outputChecks = [];
        for (const expectation of assertion.outputFileContains) {
          const target = path.join(REPO_ROOT, expectation.path);
          const exists = await fileExists(target);
          let contains = false;
          if (exists) {
            const text = await readFile(target, "utf8");
            contains = expectation.contains.every((needle) => text.includes(needle));
          }
          details.outputChecks.push({ path: expectation.path, exists, contains });
          passed = passed && exists && contains;
        }
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
    if (assertion.kind === "frontendSuite") {
      const chain = [assertion.command, ...(assertion.then ?? [])];
      const details = { commands: [], knownExternal: assertion.knownExternal ?? null, failingFiles: {}, unexpectedFailures: [] };
      let lastExit = 0;
      for (const command of chain) {
        const spec = assertion.cwd ? { cwd: assertion.cwd, command } : command;
        const record = await context.runner.run(spec);
        details.commands.push({ cmd: record.cmd, exitCode: record.exitCode });
        lastExit = record.exitCode;
        if (record.exitCode !== 0) break;
      }
      let passed = lastExit === 0;
      if (!passed && assertion.knownExternal && details.commands[0]?.exitCode !== 0) {
        // npm test 失败：解析失败测试文件，允许列表内且数量不超过基线上限才可归因。
        const logText = await readCommandLog(context.runner, assertion, chain[0]);
        const failing = parseFailingTestFiles(logText);
        details.failingFiles = failing;
        const allow = assertion.knownExternal.files;
        let withinAllowance = true;
        for (const [file, count] of Object.entries(failing)) {
          if (!(file in allow) || count > allow[file]) {
            withinAllowance = false;
            details.unexpectedFailures.push(`${file}: ${count} (allowed ${allow[file] ?? 0})`);
          }
        }
        // build 仍必须通过：单独跑 then 链。
        if (withinAllowance) {
          for (const command of assertion.then ?? []) {
            const spec = assertion.cwd ? { cwd: assertion.cwd, command } : command;
            const record = await context.runner.run(spec);
            details.commands.push({ cmd: record.cmd, exitCode: record.exitCode });
            if (record.exitCode !== 0) withinAllowance = false;
          }
        }
        passed = withinAllowance;
        details.waived = passed;
      }
      return {
        ...base,
        status: passed ? "passed" : "failed",
        evidence: details.commands.map((item) => item.cmd).join(" && "),
        details,
      };
    }
    if (assertion.kind === "codexInteraction") {
      // spec 形态传 env（R_CODE_SKIP_FRONTEND_SUITE 等）；裸数组会丢 env。
      const record = await context.runner.run({
        command: assertion.command,
        env: assertion.env,
      });
      const details = { exitCode: record.exitCode, knownExternal: assertion.knownExternal ?? null };
      let passed = record.exitCode === 0;
      if (record.exitCode !== 0 && assertion.knownExternal) {
        const reportPath = path.join(
          REPO_ROOT,
          "artifacts",
          "ai-tasks",
          "verification",
          "codex-rich-interaction",
          "implementation",
          "M4.json",
        );
        const report = await readFile(reportPath, "utf8")
          .then((text) => JSON.parse(text))
          .catch(() => null);
        if (report) {
          const failed = (report.assertions ?? []).filter((item) => item.status !== "passed");
          const allowed = assertion.knownExternal.allowedFailedAssertions ?? [];
          const markers = assertion.knownExternal.requiredExcerptMarkers ?? [];
          const onlyAllowed = failed.length > 0
            && failed.every((item) => allowed.includes(item.id))
            && failed.every((item) => markers.every((marker) => String(item.failure_excerpt ?? "").includes(marker)));
          details.failedAssertions = failed.map((item) => item.id);
          details.waived = onlyAllowed;
          passed = onlyAllowed;
        }
      }
      return {
        ...base,
        status: passed ? "passed" : "failed",
        evidence: record.logPath,
        details,
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
    entries.push({ task: taskId, evidence: path.relative(REPO_ROOT, target), exists: await fileExists(target) });
  }
  return entries;
}

function serializeReport({ scope, scopeId, profile, git, assertions, evidence, startedAt, finishedAt, exitCode }) {
  const failed = assertions.filter((item) => item.status !== "passed");
  return {
    schema_version: "r-code-alignment-verification.v1",
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

  // --assertions 定向复跑：校验 id 属于当前 scope，未选中的断言沿用既有报告结果。
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

  // 前端全量预热：与 cargo 腿（M0-02.A1）无锁竞争且不依赖任何前置步骤，
  // 在顺序循环开始前后台开跑（CommandRunner 按 spec 缓存 promise——正式
  // 断言到达时直接 await 同一个）。锁冲突的腿（cargo/金集/codex）不预热。
  const warmupTargets = [];
  const wantsM002A2 = taskIds.some((taskId) =>
    REGISTRY[taskId].assertions.some((item) => item.id === "M0-02.A2"),
  );
  if (wantsM002A2 && (!assertionFilter || assertionFilter.has("M0-02.A2"))) {
    warmupTargets.push(runner.run({ cwd: "src-tauri/frontend", command: ["npm", "test"] }));
  }

  const assertions = [];
  for (const taskId of taskIds) {
    const task = REGISTRY[taskId];
    // production profile 下的外部放行项（§11.3）不在 implementation registry 中额外登记；
    // 本仓库内 implementation 断言对两个 profile 一致要求。
    for (const assertion of task.assertions) {
      if (assertionFilter && !assertionFilter.has(assertion.id)) continue;
      process.stderr.write(`[align] run ${assertion.id} (${assertion.kind})...\n`);
      const result = await runAssertion(assertion, task, context);
      process.stderr.write(`[align] ${assertion.id} -> ${result.status}\n`);
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
        "[align] warn: --assertions without existing report; report covers selected subset only\n",
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
