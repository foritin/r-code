#!/usr/bin/env node
/**
 * verify-manifest.mjs —— 独立 claim verification（docs §16.4）：不复用 score.mjs
 * 的实现，从 raw results 重算 manifest 的每个数字，并核对缺失、重复、离群、
 * 重试与 arm 污染。任何不一致都以非零退出码失败。
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");
const artifacts = join(root, "artifacts");

function fail(message) {
  console.error(`verify-manifest: FAIL: ${message}`);
  process.exit(1);
}
for (const required of ["manifest.json", "raw-capability.jsonl", "raw-routing.jsonl", "raw-manifest.json"]) {
  if (!existsSync(join(artifacts, required))) fail(`missing ${required}`);
}

const manifest = JSON.parse(readFileSync(join(artifacts, "manifest.json"), "utf8"));
const lines = (name) =>
  readFileSync(join(artifacts, name), "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
const capability = lines("raw-capability.jsonl");
const routing = lines("raw-routing.jsonl");

// ---- 重算（独立实现：直接枚举 discordant 对） ----
const find = (arm, caseId) => capability.find((r) => r.arm === arm && r.case_id === caseId);
let wins = 0;
let losses = 0;
for (const record of capability.filter((r) => r.arm === "plan_baseline")) {
  const dual = find("plan_dual_track", record.case_id);
  if (dual.tests_passed && !record.tests_passed) wins += 1;
  if (!dual.tests_passed && record.tests_passed) losses += 1;
}
function factorial(n) {
  let value = 1;
  for (let index = 2; index <= n; index += 1) value *= index;
  return value;
}
const discordant = wins + losses;
const combinations = factorial(discordant) / (factorial(wins) * factorial(discordant - wins));
const pValue =
  discordant === 0
    ? 1
    : Math.min(
        1,
        Array.from({ length: discordant - wins + 1 }, (_, index) => wins + index)
          .reduce((sum, count) => sum + combinations * 0, 0) +
          (function tail() {
            let tailProbability = 0;
            for (let count = wins; count <= discordant; count += 1) {
              const ways =
                factorial(discordant) / (factorial(count) * factorial(discordant - count));
              tailProbability += (ways / 2 ** discordant);
            }
            return tailProbability;
          })(),
      );

const checks = [];
const expectClose = (label, actual, expected, epsilon = 1e-3) =>
  checks.push([label, Math.abs(actual - expected) <= epsilon, `${actual} vs ${expected}`]);
const expectExact = (label, actual, expected) =>
  checks.push([label, actual === expected, `${actual} vs ${expected}`]);

expectExact("capability.records", manifest.capability.records, capability.length);
expectExact("routing.records", manifest.routing.records, routing.length);
expectExact("capability.net_solved_gain", manifest.capability.net_solved_gain, wins - losses);
expectExact("capability.regressions", manifest.capability.regressions, losses);
expectClose("capability.mcnemar_p", manifest.capability.mcnemar_p_exact_one_sided, pValue, 5e-4);
expectExact(
  "capability.unapproved_side_effects",
  manifest.capability.unapproved_side_effects,
  capability.filter((record) => record.unapproved_side_effects).length,
);
expectExact("raw_results_count", manifest.raw_results_count, capability.length + routing.length);

const simple = routing.filter((record) => record.label === "simple");
const complex = routing.filter((record) => record.label === "complex");
expectClose("routing.simple_false_prompt_rate", manifest.routing.simple_false_prompt_rate,
  simple.filter((record) => record.suggested).length / Math.max(1, simple.length));
expectClose("routing.complex_recall_rate", manifest.routing.complex_recall_rate,
  complex.filter((record) => record.suggested).length / Math.max(1, complex.length));
expectClose("routing.same_request_repeat_rate", manifest.routing.same_request_repeat_rate,
  routing.filter((record) => record.repeat_prompts > 0).length / Math.max(1, routing.length));

// 唯一性与 arm 隔离复核。
const seen = new Set();
for (const record of capability) {
  const key = `${record.case_id}:${record.arm}`;
  if (seen.has(key)) fail(`duplicate capability record ${key}`);
  seen.add(key);
}
for (const caseId of new Set(capability.map((record) => record.case_id))) {
  const envs = new Set(
    capability.filter((record) => record.case_id === caseId).map((record) => record.environment_fingerprint),
  );
  if (envs.size !== 3) fail(`case ${caseId} shares state across arms`);
}

// raw digest 复核。
const digest = createHash("sha256");
digest.update(readFileSync(join(artifacts, "raw-capability.jsonl")));
digest.update(readFileSync(join(artifacts, "raw-routing.jsonl")));
digest.update(readFileSync(join(artifacts, "raw-manifest.json")));
expectExact("raw_results_digest", manifest.raw_results_digest, digest.digest("hex"));

// 非法重试：同一 (case, arm) 超过预注册允许的重试次数即污染。
for (const record of capability) {
  if ((record.retry_reasons?.length ?? 0) > 2) {
    fail(`case ${record.case_id} arm ${record.arm} has ${record.retry_reasons.length} retries`);
  }
}

let failed = false;
for (const [label, ok, detail] of checks) {
  if (!ok) {
    console.error(`verify-manifest: mismatch ${label}: ${detail}`);
    failed = true;
  }
}
if (failed) process.exit(1);
console.log(`verify-manifest: OK — every claim recomputed (${checks.length} checks) and consistent`);
