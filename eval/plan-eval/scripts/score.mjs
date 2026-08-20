#!/usr/bin/env node
/**
 * score.mjs —— 只消费 raw results 与 preregistration，自动生成证据 manifest
 *（docs/plan-mode-dual-track-gate.md §16.4/§16.5）。不做任何人工输入。
 *
 * 输入：
 *   artifacts/raw-capability.jsonl   75 条能力记录（25 case × 3 arm）
 *   artifacts/raw-routing.jsonl      40 条路由记录（20 simple + 20 complex）
 *   schema/preregistration.json      预注册阈值与失败规则
 *   artifacts/raw-manifest.json      评估器写入的原始产物清单（digest/count）
 *
 * 输出：
 *   artifacts/manifest.json          通过全部预注册门才会带 validated 字段；
 *                                    任一门失败时以非零退出码失败，不产出部分结论。
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");
const artifacts = join(root, "artifacts");

function fail(message) {
  console.error(`score: FAIL: ${message}`);
  process.exit(1);
}

const prereg = JSON.parse(readFileSync(join(root, "schema", "preregistration.json"), "utf8"));
const readLines = (name) =>
  readFileSync(join(artifacts, name), "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line));

if (!existsSync(join(artifacts, "raw-capability.jsonl"))) fail("missing raw-capability.jsonl");
if (!existsSync(join(artifacts, "raw-routing.jsonl"))) fail("missing raw-routing.jsonl");
if (!existsSync(join(artifacts, "raw-manifest.json"))) fail("missing raw-manifest.json");

const capability = readLines("raw-capability.jsonl");
const routing = readLines("raw-routing.jsonl");
const rawManifest = JSON.parse(readFileSync(join(artifacts, "raw-manifest.json"), "utf8"));

// ---- 记录完整性 / 唯一性 / Provider 来源（docs §16.4 验证器条款） ----
const expectedCases = 25;
const arms = ["direct_agent", "plan_baseline", "plan_dual_track"];
if (capability.length !== 75) fail(`capability records must be 75, got ${capability.length}`);
if (routing.length !== 40) fail(`routing records must be 40, got ${routing.length}`);

const capabilityKeys = new Set();
for (const record of capability) {
  const key = `${record.case_id}:${record.arm}`;
  if (capabilityKeys.has(key)) fail(`duplicate capability record: ${key}`);
  capabilityKeys.add(key);
  if (record.provider_kind !== "deepseek") {
    fail(`capability record ${key} provider_kind must be deepseek, got ${record.provider_kind}`);
  }
  if (record.dry_run) fail(`capability record ${key} must not be a dry-run record`);
  if (!Array.isArray(record.retry_reasons)) fail(`capability record ${key} missing retry_reasons`);
}
const caseIds = new Set(capability.map((record) => record.case_id));
if (caseIds.size !== expectedCases) fail(`expected ${expectedCases} distinct cases, got ${caseIds.size}`);
for (const caseId of caseIds) {
  for (const arm of arms) {
    if (!capabilityKeys.has(`${caseId}:${arm}`)) fail(`missing capability record ${caseId}:${arm}`);
  }
}
for (const record of routing) {
  if (record.provider_kind !== "deepseek") fail(`routing record ${record.id} must be deepseek`);
  if (record.dry_run) fail(`routing record ${record.id} must not be a dry-run record`);
}
const routingIds = new Set(routing.map((record) => record.id));
if (routingIds.size !== routing.length) fail("routing record ids must be unique");

// arm 隔离：同一 (case, arm) 的 workspace/db/session 指纹必须互不相同。
for (const caseId of caseIds) {
  const fingerprints = new Set(
    capability.filter((record) => record.case_id === caseId).map((record) => record.environment_fingerprint),
  );
  if (fingerprints.size !== 3) {
    fail(`case ${caseId} arms share state (fingerprints: ${[...fingerprints].join(", ")})`);
  }
}

// ---- 能力指标（预注册口径） ----
const solved = (record) => Boolean(record.tests_passed);
const byArm = (arm) => capability.filter((record) => record.arm === arm);
const direct = byArm("direct_agent");
const baseline = byArm("plan_baseline");
const dual = byArm("plan_dual_track");

let dualWins = 0;
let dualLosses = 0;
for (const baseRecord of baseline) {
  const dualRecord = dual.find((record) => record.case_id === baseRecord.case_id);
  if (solved(dualRecord) && !solved(baseRecord)) dualWins += 1;
  if (!solved(dualRecord) && solved(baseRecord)) dualLosses += 1;
}
const netSolvedGain = dualWins - dualLosses;

// 单侧 exact McNemar：P(X >= wins | X ~ Bin(discordant, 0.5))。
function logChoose(n, k) {
  let value = 0;
  for (let index = 0; index < k; index += 1) {
    value += Math.log(n - index) - Math.log(index + 1);
  }
  return value;
}
function exactMcNemarOneSided(wins, losses) {
  const discordant = wins + losses;
  if (discordant === 0) return 1;
  let tail = 0;
  for (let count = wins; count <= discordant; count += 1) {
    tail += Math.exp(logChoose(discordant, count) - discordant * Math.LN2);
  }
  return Math.min(1, tail);
}
const mcnemarP = exactMcNemarOneSided(dualWins, dualLosses);

const unapprovedSideEffects = capability.filter((record) => record.unapproved_side_effects).length;

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const p95 = (values) => {
  const sorted = [...values].sort((a, a2) => a - a2);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1)];
};
const safeRatio = (numerator, denominator) => (denominator > 0 ? numerator / denominator : Number.POSITIVE_INFINITY);
const dualMedianTokensRatio = safeRatio(
  median(dual.map((record) => record.total_tokens)),
  median(baseline.map((record) => record.total_tokens)),
);
const dualP95WallRatio = safeRatio(
  p95(dual.map((record) => record.wall_time_ms)),
  p95(baseline.map((record) => record.wall_time_ms)),
);

// ---- 路由指标 ----
const simple = routing.filter((record) => record.label === "simple");
const complex = routing.filter((record) => record.label === "complex");
if (simple.length !== 20 || complex.length !== 20) {
  fail(`routing split must be 20/20, got ${simple.length}/${complex.length}`);
}
const simpleFalsePromptRate = simple.filter((record) => record.suggested).length / simple.length;
const complexRecallRate = complex.filter((record) => record.suggested).length / complex.length;
const sameRequestRepeatRate =
  routing.filter((record) => record.repeat_prompts > 0).length / routing.length;

// ---- 预注册门 ----
const gates = prereg.gates;
const failures = [];
if (netSolvedGain < gates.net_solved_gain_min) failures.push(`net_solved_gain ${netSolvedGain} < ${gates.net_solved_gain_min}`);
if (dualLosses > gates.regressions_max) failures.push(`regressions ${dualLosses} > ${gates.regressions_max}`);
if (mcnemarP > gates.mcnemar_p_one_sided_max) failures.push(`mcnemar p ${mcnemarP.toFixed(4)} > ${gates.mcnemar_p_one_sided_max}`);
if (unapprovedSideEffects > gates.unapproved_side_effects_max) failures.push(`unapproved side effects ${unapprovedSideEffects} > 0`);
if (simpleFalsePromptRate > gates.simple_false_prompt_rate_max) failures.push(`simple false prompt rate ${simpleFalsePromptRate.toFixed(3)} > ${gates.simple_false_prompt_rate_max}`);
if (complexRecallRate < gates.complex_recall_rate_min) failures.push(`complex recall ${complexRecallRate.toFixed(3)} < ${gates.complex_recall_rate_min}`);
if (sameRequestRepeatRate > gates.same_request_repeat_rate_max) failures.push(`same request repeat rate ${sameRequestRepeatRate} > 0`);
if (dualMedianTokensRatio > gates.dual_median_tokens_ratio_max) failures.push(`dual median tokens ratio ${dualMedianTokensRatio.toFixed(3)} > ${gates.dual_median_tokens_ratio_max}`);
if (dualP95WallRatio > gates.dual_p95_wall_time_ratio_max) failures.push(`dual p95 wall ratio ${dualP95WallRatio.toFixed(3)} > ${gates.dual_p95_wall_time_ratio_max}`);

const rawDigest = createHash("sha256")
  .update(readFileSync(join(artifacts, "raw-capability.jsonl")))
  .update(readFileSync(join(artifacts, "raw-routing.jsonl")))
  .update(readFileSync(join(artifacts, "raw-manifest.json")))
  .digest("hex");

const manifest = {
  schema: "r-code-plan-evidence-manifest/v1",
  provider_kind: "deepseek",
  eligibility_profile_version: prereg.eligibility_profile_version,
  evidence_version: rawManifest.evidence_version,
  allowed_models: rawManifest.allowed_models,
  allowed_protocols: rawManifest.allowed_protocols,
  allowed_endpoint_classes: rawManifest.allowed_endpoint_classes,
  preregistration_sha256: createHash("sha256")
    .update(readFileSync(join(root, "schema", "preregistration.json")))
    .digest("hex"),
  corpus_lock_sha256: createHash("sha256")
    .update(readFileSync(join(root, "corpus-lock.json")))
    .digest("hex"),
  capability: {
    records: capability.length,
    net_solved_gain: netSolvedGain,
    regressions: dualLosses,
    mcnemar_p_exact_one_sided: Number(mcnemarP.toFixed(6)),
    unapproved_side_effects: unapprovedSideEffects,
    dual_median_tokens_ratio: Number(dualMedianTokensRatio.toFixed(4)),
    dual_p95_wall_time_ratio: Number(dualP95WallRatio.toFixed(4)),
  },
  routing: {
    records: routing.length,
    simple_false_prompt_rate: Number(simpleFalsePromptRate.toFixed(4)),
    complex_recall_rate: Number(complexRecallRate.toFixed(4)),
    same_request_repeat_rate: Number(sameRequestRepeatRate.toFixed(4)),
  },
  raw_results_count: capability.length + routing.length,
  raw_results_digest: rawDigest,
};

if (failures.length > 0) {
  console.error("score: preregistered gates FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

writeFileSync(join(artifacts, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log("score: all preregistered gates passed; manifest.json written");
console.log(JSON.stringify(manifest.capability, null, 2));
console.log(JSON.stringify(manifest.routing, null, 2));
