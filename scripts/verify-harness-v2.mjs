#!/usr/bin/env node
// verify-harness-v2.mjs — 统一验收入口（T41）。
// 用法：node scripts/verify-harness-v2.mjs --profile quick|full
// quick = 架构守卫 + 一致性套件；full = quick + 定向任务测试 + SDK 示例。

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const profile = args.includes("--profile") ? args[args.indexOf("--profile") + 1] : "quick";

const TIMEOUT_MS = 10 * 60 * 1000;
const steps = [];

function run(name, command, options = {}) {
  const started = Date.now();
  try {
    const output = execFileSync(command[0], command.slice(1), {
      cwd: root,
      timeout: TIMEOUT_MS,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      ...options,
    });
    steps.push({ name, ok: true, ms: Date.now() - started });
    return output;
  } catch (error) {
    steps.push({ name, ok: false, ms: Date.now() - started, detail: String(error.stderr || error.message).slice(0, 2000) });
    throw error;
  }
}

function guard(name, condition, detail) {
  steps.push({ name, ok: Boolean(condition), detail });
  if (!condition) {
    throw new Error(`guard failed: ${name} ${detail ?? ""}`);
  }
}

// 1. Architecture guards: crates exist and forbidden dependencies stay out.
guard("protocol crate exists", existsSync(join(root, "crates/r-code-harness-protocol/Cargo.toml")));
guard("kernel crate exists", existsSync(join(root, "crates/r-code-kernel/Cargo.toml")));
guard("runtime crate exists", existsSync(join(root, "crates/r-code-runtime/Cargo.toml")));
guard("client crate exists", existsSync(join(root, "crates/r-code-client/Cargo.toml")));
guard("sdk crate exists", existsSync(join(root, "crates/r-code-harness-sdk/Cargo.toml")));
guard("native plugin exists", existsSync(join(root, "plugins/native/Cargo.toml")));
guard("codex plugin exists", existsSync(join(root, "plugins/codex/Cargo.toml")));

const kernelManifest = readFileSync(join(root, "crates/r-code-kernel/Cargo.toml"), "utf8");
const kernelDeps = kernelManifest.split("[dependencies]")[1]?.split("[")[0] ?? "";
guard("kernel has no tauri/sqlite/gateway", !/tauri|rusqlite|r-code-gateway|r-code-runtime/.test(kernelDeps));

const protocolManifest = readFileSync(join(root, "crates/r-code-harness-protocol/Cargo.toml"), "utf8");
const protocolDeps = protocolManifest.split("[dependencies]")[1]?.split("[")[0] ?? "";
guard("protocol is neutral", !/tauri|rusqlite|r-code-gateway|r-code-runtime|r-code-store/.test(protocolDeps));

const nativeManifest = readFileSync(join(root, "plugins/native/Cargo.toml"), "utf8");
const nativeDeps = nativeManifest.split("[dependencies]")[1]?.split("[")[0] ?? "";
guard("native plugin is host-free", !/r-code-runtime|r-code-store|r-code-gateway|tauri/.test(nativeDeps));

// 1b. T42 retirement guards: the old orchestration path stays retired.
guard("agent-worker crate retired", !existsSync(join(root, "crates/r-code-agent-worker")));
guard("extensions.rs retired", !existsSync(join(root, "src-tauri/src/extensions.rs")));
guard("work_card.rs retired", !existsSync(join(root, "src-tauri/src/work_card.rs")));
const rootManifest = readFileSync(join(root, "Cargo.toml"), "utf8");
guard("workspace has no agent-worker member", !rootManifest.includes("r-code-agent-worker"));
const hostManifest = readFileSync(join(root, "src-tauri/Cargo.toml"), "utf8");
guard("host has no agent-worker dep", !hostManifest.includes("r-code-agent-worker"));
const evalsManifest = readFileSync(join(root, "crates/r-code-evals/Cargo.toml"), "utf8");
guard("evals have no host dep", !evalsManifest.includes("r-code-host"));
const tuiManifest = readFileSync(join(root, "crates/r-code-tui/Cargo.toml"), "utf8");
guard("tui has no host dep", !tuiManifest.includes("r-code-host"));
for (const file of [
  "src-tauri/src/commands.rs",
  "src-tauri/src/tauri_commands.rs",
  "src-tauri/src/settings.rs",
  "src-tauri/src/mcp_server.rs",
  "src-tauri/src/plan_entry_commands.rs",
]) {
  const text = readFileSync(join(root, file), "utf8");
  guard(`${file} has no agent-worker import`, !/r_code_agent_worker|r-code-agent-worker/.test(text));
}

// 2. Protocol documents ship beside the PRD.
guard("protocol doc exists", existsSync(join(root, "docs/prd/pluggable-harness/protocol-v1.md")));
guard("plugin author guide exists", existsSync(join(root, "docs/prd/pluggable-harness/plugin-author-guide.md")));
guard("architecture doc exists", existsSync(join(root, "docs/prd/pluggable-harness/architecture.md")));

// 3. Deterministic conformance as a hard gate.
run("conformance suite", ["cargo", "run", "-p", "r-code-evals", "--bin", "harness-conformance"]);

if (profile === "full") {
  // 4. Targeted task-level gates (bounded, deterministic).
  run("sdk example fixture", ["cargo", "test", "-p", "r-code-harness-sdk", "--test", "t11_deliver_rust_sdk_and_independent_harness_fixture"]);
  run("native loop fixture", ["cargo", "test", "-p", "r-code-harness-native", "--test", "t26_port_the_native_model_tool_loop_to_a_harness_bin"]);
  run("application lifecycle", ["cargo", "test", "-p", "r-code-runtime", "--test", "t32_compose_the_headless_applicationservice"]);
  run("conversation engine", ["cargo", "test", "-p", "r-code-runtime", "--test", "conversation_engine"]);
  run("paired eval", ["cargo", "test", "-p", "r-code-evals", "--test", "t40_add_coding_task_paired_evaluations"]);
  run("desktop bridge", ["cargo", "test", "-p", "r-code-host", "--test", "t33_adapt_desktop_task_flow_to_applicationservice"]);
  run("tui plugin commands", ["cargo", "test", "-p", "r-code-tui", "--test", "t36_add_tui_plugin_catalog_and_selection_commands"]);
  run("legacy reader", ["cargo", "test", "-p", "r-code-runtime", "--test", "t37_implement_read_only_legacy_history_access"]);
  run("packaging inspection", ["node", "--test", "scripts/harness-packaging.test.mjs"]);
  // 5. T35/T42 switch gates: TUI detached, chat flows through the daemon.
  run("tui detached from host", ["cargo", "test", "-p", "r-code-tui", "--test", "t35_detach_tui_from_the_tauri_host"]);
  run("desktop chat v2 bridge", ["cargo", "test", "-p", "r-code-host", "--test", "harness_v2_chat"]);
}

// Report.
console.log(`verify-harness-v2 profile=${profile}`);
for (const step of steps) {
  console.log(`${step.ok ? "PASS" : "FAIL"} ${step.name} (${step.ms}ms)${step.detail ? ` — ${step.detail}` : ""}`);
}
const failed = steps.filter((step) => !step.ok).length;
if (failed > 0) {
  process.exit(1);
}
console.log(`all ${steps.length} checks passed`);
