#!/usr/bin/env node
// verify-remote.mjs — 远程控制（含 R0 审批通道）统一验收入口。
//
// 用法：
//   node scripts/verify-remote.mjs --task <ID> [--profile implementation]
//   node scripts/verify-remote.mjs --through <MILESTONE>
//   node scripts/verify-remote.mjs --list
//
// 契约（prd/remote-control/worklist.md §8）：
//   - 非交互；exit 0 仅当全部 required assertion 通过
//   - 输出机器可读 JSON 报告到 artifacts/ai-tasks/verification/<profile>/<id>.json
//   - 不删除/跳过失败断言；缺断言=失败
//
// 当前状态（计划 draft）：注册表已建立，任务实现后在此文件追加 assertion 映射。
// 每个 assertion 映射到一个非交互命令（cargo test 过滤名 / node --test 文件）。

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// ── 里程碑出口（累计门禁，按依赖顺序执行组）─────────────────────────────────
export const MILESTONES = {
  R0: ["RA1", "RA2", "RA3"],
  R1: ["R00", "R01", "R02", "R03", "R04", "R05", "R06", "R07", "R08"],
  R2: ["R09", "R10", "R11"],
  R3: ["R12", "R07b", "R07c"],
  R4: ["R13", "R14"],
  R5: ["R15", "R16", "R17", "R18", "R19", "R20"],
  R6: ["R21", "R22", "R23", "R24", "R25", "R26", "R27", "R28"],
};

// ── Assertion registry ──────────────────────────────────────────────────────
// 实现任务时把每个 A 断言填成可二值判定的非交互命令。
// kind: cargo-test | node-test | static-guard | script
// 未实现任务的断言状态为 "pending"：--task 遇到 pending 断言以 exit 2 报告
// （“断言尚未实现”），绝不静默通过。
export const ASSERTIONS = {
  RA1: { milestone: "R0", required: ["RA1.A1", "RA1.A2", "RA1.A3"] },
  RA2: { milestone: "R0", required: ["RA2.A1", "RA2.A2"] },
  RA3: { milestone: "R0", required: ["RA3.A1", "RA3.A2"] },
  R00: { milestone: "R1", required: ["R00.A1"] },
  R01: { milestone: "R1", required: ["R01.A1", "R01.A2", "R01.A3"] },
  R02: { milestone: "R1", required: ["R02.A1", "R02.A2", "R02.A3"] },
  R03: { milestone: "R1", required: ["R03.A1", "R03.A2", "R03.A3"] },
  R04: { milestone: "R1", required: ["R04.A1", "R04.A2", "R04.A3", "R04.A4"] },
  R05: { milestone: "R1", required: ["R05.A1", "R05.A2", "R05.A3"] },
  R06: { milestone: "R1", required: ["R06.A1", "R06.A2"] },
  R07: { milestone: "R1", required: ["R07.A1", "R07.A2"] },
  R08: { milestone: "R1", required: ["R08.A1", "R08.A2", "R08.A3"] },
  R09: { milestone: "R2", required: ["R09.A1", "R09.A2"] },
  R10: { milestone: "R2", required: ["R10.A1", "R10.A2", "R10.A3"] },
  R11: { milestone: "R2", required: ["R11.A1", "R11.A2", "R11.A3"] },
  R12: { milestone: "R3", required: ["R12.A1", "R12.A2"] },
  "R07b": { milestone: "R3", required: ["R07b.A1", "R07b.A2"] },
  "R07c": { milestone: "R3", required: ["R07c.A1"] },
  R13: { milestone: "R4", required: ["R13.A1"] },
  R14: { milestone: "R4", required: ["R14.A1", "R14.A2"] },
  R15: { milestone: "R5", required: ["R15.A1"] },
  R16: { milestone: "R5", required: ["R16.A1", "R16.A2"] },
  R17: { milestone: "R5", required: ["R17.A1", "R17.A2"] },
  R18: { milestone: "R5", required: ["R18.A1"] },
  R19: { milestone: "R5", required: ["R19.A1", "R19.A2", "R19.A3"] },
  R20: { milestone: "R5", required: ["R20.A1", "R20.A2"] },
  R21: { milestone: "R6", required: ["R21.A1", "R21.A2"] },
  R22: { milestone: "R6", required: ["R22.A1", "R22.A2"] },
  R23: { milestone: "R6", required: ["R23.A1", "R23.A2"] },
  R24: { milestone: "R6", required: ["R24.A1", "R24.A2"] },
  R25: { milestone: "R6", required: ["R25.A1", "R25.A2"] },
  R26: { milestone: "R6", required: ["R26.A1"] },
  R27: { milestone: "R6", required: ["R27.A1", "R27.A2"] },
  R28: { milestone: "R6", required: ["R28.A1", "R28.A2"] },
};

// 断言→命令映射在实现任务时填充。键为 assertion id。
// 例：
// "RA1.A1": { kind: "cargo-test", pkg: "r-code-runtime", filter: "approval_request_is_persisted" }
export const COMMANDS = {
  "RA1.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra1_persistent_approvals",
    filter: "ra1_a1_request_persists_blocks_and_unblocks_on_grant",
  },
  "RA1.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra1_persistent_approvals",
    filter: "ra1_a2_pending_ops_rebuild_from_journal_after_restart",
  },
  "RA1.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra1_persistent_approvals",
    filter: "ra1_a3_forged_reference_is_refused_without_events",
  },
  "RA2.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra2_approval_methods",
    filter: "ra2_a1_list_then_decide_with_connection_auditing",
  },
  "RA2.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra2_approval_methods",
    filter: "ra2_a2_command_dedup_replays_and_conflicts_are_refused",
  },
  "RA3.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra3_approval_e2e",
    filter: "ra3_a1_grant_unblocks_the_plugin_and_completes_the_run",
  },
  "RA3.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "ra3_approval_e2e",
    filter: "ra3_a2_deny_settles_and_timeout_denies_without_hanging",
  },
  "R00.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r00_transport_seam",
    filter: "r00_a1",
  },
  "R01.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "register_roundtrip_token_valid_and_not_on_disk",
  },
  "R01.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "revoke_blocks_authentication_immediately",
  },
  "R01.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "fresh_devices_are_read_only_and_versioned",
  },
  "R02.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "r02_a1_expired_replayed_and_wrong_codes_are_distinct_errors",
  },
  "R02.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "r02_a2_successful_pairing_yields_a_device_and_one_time_token",
  },
  "R02.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "r02_a3_remote_source_cannot_start_pairing",
  },
  "R03.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "r03_a1_identity_is_stable_across_restarts",
  },
  "R03.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r03_tls_pinning",
    filter: "r03_a2_wrong_fingerprint_fails_in_the_tls_handshake",
  },
  "R03.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r03_tls_pinning",
    filter: "r03_a3_plaintext_client_cannot_get_frames_through",
  },
  "R04.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r04_remote_listener",
    filter: "r04_a1_a4_listener_lifecycle_follows_devices_and_switch",
  },
  "R04.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r04_remote_listener",
    filter: "r04_a2_authenticated_commands_replay_and_bad_tokens_drop",
  },
  "R04.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r04_remote_listener",
    filter: "r04_a3_capabilities_and_forbidden_methods_are_enforced",
  },
  "R04.A4": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r04_remote_listener",
    filter: "r04_a1_a4_listener_lifecycle_follows_devices_and_switch",
  },
  "R05.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r05_fanout_stream",
    filter: "r05_a1_live_subscription_streams_new_events_by_seq",
  },
  "R05.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r05_fanout_stream",
    filter: "r05_a2_reconnect_with_cursor_resumes_without_loss_or_duplication",
  },
  "R05.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r05_fanout_stream",
    filter: "r05_a3_two_subscribers_isolation_on_the_wire",
  },
  "R06.A1": {
    kind: "cargo-test",
    pkg: "r-code-client",
    test: "r06_ws_transport",
    filter: "r06_a1_wss_roundtrip_task_list_and_events",
  },
  "R06.A2": {
    kind: "cargo-test",
    pkg: "r-code-client",
    test: "r06_ws_transport",
    filter: "r06_a2_reconnect_replays_the_same_command_id",
  },
  "R07.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/projection.test.mjs",
  },
  "R07.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r07_app_hosting",
    filter: "r07_a2_app_served_when_built_and_honest_404_when_not",
  },
  "R08.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r08_manual_pairing_e2e",
    filter: "r08_a1_manual_pairing_read_only_loop",
  },
  "R08.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r08_manual_pairing_e2e",
    filter: "r08_a2_unauthenticated_connections_get_nothing",
  },
  "R08.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "public_and_wildcard_binds_are_refused",
  },
  "R09.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    lib: true,
    filter: "r09_a1",
  },
  "R09.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r09_mdns_discovery",
    filter: "r09_a2_mdns_advertises_port_and_fingerprint_until_stopped",
  },
  "R10.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r10_remote_write",
    filter: "r10_a1_a3_remote_send_is_audited_and_queue_semantics_match_local",
  },
  "R10.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r10_remote_write",
    filter: "r10_a2_read_only_device_writes_are_all_refused",
  },
  "R10.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r10_remote_write",
    filter: "r10_a1_a3_remote_send_is_audited_and_queue_semantics_match_local",
  },
  "R11.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r11_device_management",
    filter: "r11_a1_list_carries_the_device_fields",
  },
  "R11.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r11_device_management",
    filter: "r11_a2_revocation_drops_the_live_connection_and_refuses_reconnect",
  },
  "R11.A3": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r11_device_management",
    filter: "r11_a3_listener_switch_drops_connections_but_keeps_devices",
  },
  "R12.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r12_remote_approvals",
    filter: "r12_a1_read_only_decide_is_refused_and_op_stays_pending",
  },
  "R12.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r12_remote_approvals",
    filter: "r12_a2_remote_decision_flows_to_the_plugin_and_audits_the_device",
  },
  "R07b.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r07b.test.mjs",
  },
  "R07b.A2": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r07b-layout.test.mjs",
  },
  "R07c.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r07c.test.mjs",
  },
  "R13.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r13.test.mjs",
  },
  "R14.A1": {
    kind: "script",
    args: ["scripts/verify-remote-guards.mjs"],
  },
  "R14.A2": {
    kind: "script",
    args: ["scripts/verify-remote-guards.mjs"],
  },
  "R15.A1": {
    kind: "node-test",
    file: "scripts/remote/r15-relay-interface.test.mjs",
  },
  "R16.A1": {
    kind: "cargo-test",
    pkg: "r-code-relay",
    test: "r16_relay_integration",
    filter: "r16_a1_register_bind_reject_forward_paths",
  },
  "R16.A2": {
    kind: "cargo-test",
    pkg: "r-code-relay",
    test: "r16_relay_integration",
    filter: "r16_a2_audit_is_content_free_and_rate_limit_enforced",
  },
  "R17.A1": {
    kind: "cargo-test",
    pkg: "r-code-relay",
    test: "r17_e2ee",
    filter: "r17_a1_e2ee_roundtrip_and_stream_semantics_match_a_pipe",
  },
  "R17.A2": {
    kind: "cargo-test",
    pkg: "r-code-relay",
    test: "r17_e2ee",
    filter: "r17_a2_malicious_relay_cannot_read_tamper_or_inject",
  },
  "R18.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r18_relay_config",
    filter: "r18_a1",
  },
  "R19.A1": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r19_relay_path",
    filter: "r19_a1_e2ee_bridge_roundtrip_and_wrong_secret_refused",
  },
  "R19.A2": {
    kind: "cargo-test",
    pkg: "r-code-runtime",
    test: "r19_relay_path",
    filter: "r19_a2_revocation_enforced_by_daemon_through_a_live_bridge",
  },
  "R19.A3": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r19-connection-strategy.test.mjs",
  },
  "R20.A1": {
    kind: "cargo-test",
    pkg: "r-code-relay",
    test: "r16_relay_integration",
    filter: "r16_a1",
  },
  "R20.A2": {
    kind: "node-test",
    file: "scripts/remote/r20-deployment.test.mjs",
  },
  "R21.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r21-purity.test.mjs",
  },
  "R21.A2": {
    kind: "node-test",
    file: "scripts/remote/r21-ci.test.mjs",
  },
  "R22.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r22-pairing.test.mjs",
  },
  "R22.A2": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r21-purity.test.mjs",
  },
  "R23.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r23-native-screens.test.mjs",
  },
  "R23.A2": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r23-native-screens.test.mjs",
  },
  "R24.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r24-approval-experience.test.mjs",
  },
  "R24.A2": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r24-approval-experience.test.mjs",
  },
  "R25.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r25-push.test.mjs",
  },
  "R25.A2": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r25-push.test.mjs",
  },
  "R26.A1": {
    kind: "node-test",
    file: "src-tauri/frontend/src/remote/core/r26-settings.test.mjs",
  },
  "R27.A1": {
    kind: "node-test",
    file: "scripts/remote/r27-compliance.test.mjs",
  },
  "R27.A2": {
    kind: "node-test",
    file: "scripts/remote/r27-compliance.test.mjs",
  },
};

function runCommand(spec) {
  const started = Date.now();
  try {
    let args;
    if (spec.kind === "cargo-test") {
      args = ["test", "-p", spec.pkg];
      if (spec.test) args.push("--test", spec.test);
      if (spec.lib) args.push("--lib");
      args.push(spec.filter, "--", "--exact", "--nocapture");
    } else if (spec.kind === "node-test") {
      args = ["--test", spec.file];
    } else if (spec.kind === "script") {
      args = spec.args;
    } else {
      throw new Error(`unknown spec kind: ${spec.kind}`);
    }
    const program =
      spec.kind === "node-test"
        ? process.execPath
        : spec.kind === "script"
          ? process.execPath
          : "cargo";
    execFileSync(program, args, {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
      timeout: 10 * 60 * 1000,
      encoding: "utf8",
    });
    return { passed: true, ms: Date.now() - started };
  } catch (error) {
    return {
      passed: false,
      ms: Date.now() - started,
      detail: String(error.stderr || error.message).slice(0, 4000),
    };
  }
}

function evaluate(ids, profile) {
  const results = [];
  for (const id of ids) {
    const entry = ASSERTIONS[id];
    if (!entry) {
      results.push({ task: id, status: "unknown-task" });
      continue;
    }
    for (const assertion of entry.required) {
      const spec = COMMANDS[assertion];
      if (!spec) {
        results.push({ task: id, assertion, status: "pending-implementation" });
        continue;
      }
      const outcome = runCommand(spec);
      results.push({ task: id, assertion, ...outcome });
    }
  }
  const failed = results.filter((r) => r.passed === false);
  const pending = results.filter((r) => r.status === "pending-implementation");
  const report = {
    profile,
    generated_at: new Date().toISOString(),
    tasks: ids,
    totals: {
      evaluated: results.length,
      passed: results.filter((r) => r.passed === true).length,
      failed: failed.length,
      pending: pending.length,
    },
    results,
  };
  return report;
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes("--list")) {
    for (const [id, entry] of Object.entries(ASSERTIONS)) {
      console.log(`${id}\t${entry.milestone}\t${entry.required.length} assertions`);
    }
    return;
  }
  const profile = args.includes("--profile")
    ? args[args.indexOf("--profile") + 1]
    : "implementation";
  let ids;
  const taskIdx = args.indexOf("--task");
  const throughIdx = args.indexOf("--through");
  if (taskIdx >= 0) {
    ids = [args[taskIdx + 1]];
  } else if (throughIdx >= 0) {
    const milestone = args[throughIdx + 1];
    const chain = ["R0", "R1", "R2", "R3", "R4", "R5"];
    const upto = chain.indexOf(milestone);
    if (upto < 0) {
      console.error(`unknown milestone: ${milestone}`);
      process.exit(64);
    }
    ids = chain.slice(0, upto + 1).flatMap((m) => MILESTONES[m]);
  } else {
    console.error("usage: verify-remote.mjs --task <ID> | --through <R0..R5> [--profile ...]");
    process.exit(64);
  }

  const report = evaluate(ids, profile);
  const outDir = join(root, "artifacts/ai-tasks/verification", profile);
  mkdirSync(outDir, { recursive: true });
  const name = taskIdx >= 0 ? ids[0] : `through-${args[throughIdx + 1]}`;
  const outPath = join(outDir, `${name}.json`);
  writeFileSync(outPath, JSON.stringify(report, null, 2));

  const ok = report.totals.failed === 0 && report.totals.pending === 0;
  console.log(
    `verify-remote ${name}: ${report.totals.passed}/${report.totals.evaluated} passed` +
      (report.totals.pending ? `, ${report.totals.pending} pending implementation` : "") +
      (report.totals.failed ? `, ${report.totals.failed} FAILED` : "")
  );
  console.log(`report: ${outPath}`);
  // exit 2 while assertions are unimplemented (plan draft); flips to 0/1 only.
  process.exit(ok ? 0 : report.totals.failed ? 1 : 2);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}