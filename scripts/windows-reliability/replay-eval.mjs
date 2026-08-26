// M4-02.A3：Codex 链路重放评估（离线 fixture 演练）。
//   node scripts/windows-reliability/replay-eval.mjs --offline
//
// 编排 CORPUS_REPLAY=1 的 cargo 测试（同源 append_diagnosis 重放脱敏取证样本），
// 抽取结构化 JSON、校验零失配，并把报告写入 artifacts/metrics/command-corpus/。
//
// 真实账号复测（外部放行）：在装有已登录 codex CLI 的机器上运行真实委派负载，
// 用同 schema 收集 commandExecution 结果（≥92% 链路成功率门槛，PRD §11.3）。

import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");
const REPORT_DIR = path.join(ROOT, "artifacts", "metrics", "command-corpus");

function revSha() {
  const rev = spawnSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8", timeout: 15_000 });
  if (rev.status !== 0) {
    console.error("replay-eval: git rev-parse HEAD failed");
    process.exit(2);
  }
  return rev.stdout.trim();
}

async function main() {
  const mode = process.argv.includes("--offline") ? "offline" : "offline";
  if (process.argv.length > 2 && !process.argv.includes("--offline")) {
    console.error("replay-eval: 目前仅支持 --offline（真实账号复测见 PRD §11.3 外部放行说明）");
    process.exit(2);
  }

  const child = spawnSync(
    "cargo",
    ["test", "-p", "r-code-gateway", "--test", "replay_eval_runner", "--", "--nocapture"],
    {
      cwd: ROOT,
      encoding: "utf8",
      timeout: 5 * 60 * 1000,
      maxBuffer: 32 * 1024 * 1024,
      env: { ...process.env, CORPUS_REPLAY: "1" },
    },
  );
  if (child.status !== 0) {
    console.error(`replay-eval: runner 退出码 ${child.status}`);
    console.error(`${child.stdout ?? ""}\n${child.stderr ?? ""}`.trimEnd().slice(-3000));
    process.exit(1);
  }
  const stdout = child.stdout ?? "";
  const begin = stdout.indexOf("REPLAY_EVAL_JSON_BEGIN");
  const end = stdout.indexOf("REPLAY_EVAL_JSON_END");
  if (begin < 0 || end < 0) {
    console.error("replay-eval: runner 输出缺少 JSON 标记");
    process.exit(1);
  }
  const report = JSON.parse(stdout.slice(begin + "REPLAY_EVAL_JSON_BEGIN".length, end).trim());

  const failures = [];
  if (report.schema_version !== "codex-replay-eval.v1") failures.push("schema_version 不符");
  if (report.mode !== "offline-fixture") failures.push("mode 不符");
  if (typeof report.total !== "number" || report.total < 8) failures.push(`total=${report.total} < 8`);
  if (report.mismatch_count !== 0) failures.push(`mismatch_count=${report.mismatch_count}`);
  if (!Array.isArray(report.samples) || report.samples.length !== report.total) {
    failures.push("samples 数组不完整");
  }
  if (failures.length > 0) {
    console.error(`replay-eval 失败：`);
    for (const failure of failures) console.error(`  - ${failure}`);
    process.exit(1);
  }

  await mkdir(REPORT_DIR, { recursive: true });
  const reportPath = path.join(REPORT_DIR, `replay-eval-${revSha().slice(0, 10)}.json`);
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  console.log(
    `replay-eval OK: mode=${mode} total=${report.total} hint_hits=${report.hint_hits} mismatch=0 → ${path.relative(ROOT, reportPath)}`,
  );
}

await main();
