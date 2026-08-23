#!/usr/bin/env node
/**
 * score.mjs —— 只消费冻结的 raw results、raw manifest 与 preregistration，
 * fail closed 生成发布证据。任何失败都会先删除旧 manifest，避免陈旧绿灯残留。
 */
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

const rootFlag = process.argv.indexOf("--root");
const root = resolve(
  rootFlag >= 0 && process.argv[rootFlag + 1]
    ? process.argv[rootFlag + 1]
    : join(import.meta.dirname, ".."),
);
const artifacts = join(root, "artifacts");
const outputManifest = join(artifacts, "manifest.json");

if (existsSync(outputManifest)) unlinkSync(outputManifest);

function fail(message) {
  console.error(`score: FAIL: ${message}`);
  process.exit(1);
}

function parseJsonFile(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${label} is not valid JSON: ${error.message}`);
  }
}

function parseJsonl(path, label) {
  let text;
  try {
    text = readFileSync(path, "utf8");
  } catch (error) {
    fail(`cannot read ${label}: ${error.message}`);
  }
  return text
    .split(/\r?\n/)
    .filter((line) => line.trim().length > 0)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        fail(`${label} line ${index + 1} is not valid JSON: ${error.message}`);
      }
    });
}

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const fileSha256 = (path) => sha256(readFileSync(path));
const isSha256 = (value) => typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
const isCommit = (value) =>
  typeof value === "string" && /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(value);
const exactArray = (left, right) =>
  Array.isArray(left) && JSON.stringify(left) === JSON.stringify(right);
const referencedArtifactUris = new Set();
const canonicalJson = (value) => {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
};

function requireString(record, field, key) {
  const value = record[field];
  if (typeof value !== "string" || value.trim().length === 0) {
    fail(`${key} missing non-blank ${field}`);
  }
  return value;
}

function requireSha(record, field, key) {
  const value = requireString(record, field, key);
  if (!isSha256(value)) fail(`${key} ${field} must be a lowercase SHA-256 digest`);
  return value;
}

function requireNumber(record, field, key, { integer = false, positive = false } = {}) {
  const value = record[field];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    fail(`${key} ${field} must be finite`);
  }
  if (integer && !Number.isInteger(value)) fail(`${key} ${field} must be an integer`);
  if (positive && value <= 0) fail(`${key} ${field} must be positive`);
  if (!positive && value < 0) fail(`${key} ${field} must be non-negative`);
  return value;
}

function requireUniqueStrings(record, field, key) {
  const values = record[field];
  if (!Array.isArray(values) || values.length === 0) fail(`${key} ${field} must be non-empty`);
  if (values.some((value) => typeof value !== "string" || value.trim().length === 0)) {
    fail(`${key} ${field} must contain only non-blank strings`);
  }
  if (new Set(values).size !== values.length) fail(`${key} ${field} must be unique`);
  return values;
}

function rejectSecrets(path) {
  if (!existsSync(path)) return;
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) fail(`artifact tree must not contain symlinks: ${path}`);
  if (!stat.isDirectory()) return;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isSymbolicLink()) fail(`artifact tree must not contain symlinks: ${child}`);
    if (entry.isDirectory()) rejectSecrets(child);
    if (entry.isFile() && entry.name.toLowerCase() === "secrets.json") {
      fail(`secret material found under artifacts: ${child}`);
    }
  }
}

function resolveArtifact(uri, key) {
  if (typeof uri !== "string" || uri.length === 0 || isAbsolute(uri) || uri.includes("\\")) {
    fail(`${key} artifact_uri must be a relative forward-slash path`);
  }
  const target = resolve(artifacts, ...uri.split("/"));
  const rel = relative(artifacts, target);
  if (rel === "" || rel === ".." || rel.startsWith(`..${sep}`)) {
    fail(`${key} artifact_uri escapes artifacts root`);
  }
  if (!existsSync(target)) fail(`${key} raw artifact is unavailable: ${uri}`);
  const stat = lstatSync(target);
  if (!stat.isFile() || stat.isSymbolicLink()) fail(`${key} raw artifact is not a regular file`);
  referencedArtifactUris.add(uri);
  return target;
}

function listRawArtifactUris(path) {
  if (!existsSync(path)) return [];
  const values = [];
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isSymbolicLink()) fail(`raw artifact tree contains a symlink: ${child}`);
    if (entry.isDirectory()) values.push(...listRawArtifactUris(child));
    if (entry.isFile()) values.push(relative(artifacts, child).split(sep).join("/"));
  }
  return values;
}

for (const required of ["raw-capability.jsonl", "raw-routing.jsonl", "raw-manifest.json"]) {
  if (!existsSync(join(artifacts, required))) fail(`missing ${required}`);
}
rejectSecrets(artifacts);

const preregPath = join(root, "schema", "preregistration.json");
const corpusLockPath = join(root, "corpus-lock.json");
const prereg = parseJsonFile(preregPath, "preregistration.json");
const rawManifestPath = join(artifacts, "raw-manifest.json");
const rawManifest = parseJsonFile(rawManifestPath, "raw-manifest.json");
const capabilityPath = join(artifacts, "raw-capability.jsonl");
const routingPath = join(artifacts, "raw-routing.jsonl");
const capability = parseJsonl(capabilityPath, "raw-capability.jsonl");
const routing = parseJsonl(routingPath, "raw-routing.jsonl");

if (prereg.schema !== "r-code-plan-preregistration/v1") fail("unexpected preregistration schema");
if (rawManifest.schema !== "r-code-plan-raw-manifest/v1") fail("unexpected raw manifest schema");
if (rawManifest.status !== "complete") {
  fail(`raw manifest status must be complete, got ${rawManifest.status}`);
}
if (!isSha256(rawManifest.identity_sha256)) fail("raw manifest identity_sha256 is invalid");
if (!isCommit(rawManifest.commit)) fail("raw manifest commit is invalid");
if (typeof rawManifest.evidence_version !== "string" || rawManifest.evidence_version.length === 0) {
  fail("raw manifest evidence_version is required");
}
if (typeof rawManifest.run_seed !== "string" || rawManifest.run_seed.length === 0) {
  fail("raw manifest run_seed is required");
}
if (rawManifest.run_seed_sha256 !== sha256(rawManifest.run_seed)) {
  fail("raw manifest run_seed_sha256 mismatch");
}
if (!exactArray(rawManifest.allowed_models, ["deepseek-v4-flash", "deepseek-v4-pro"])) {
  fail("raw manifest model allowlist mismatch");
}
if (
  !exactArray(rawManifest.allowed_protocols, [
    "openai_chat",
    "openai_responses",
    "anthropic_messages",
  ])
) {
  fail("raw manifest protocol allowlist mismatch");
}
if (!exactArray(rawManifest.allowed_endpoint_classes, ["official_api"])) {
  fail("raw manifest endpoint allowlist mismatch");
}
if (
  !rawManifest.allowed_models.includes(rawManifest.resolved_model) ||
  !rawManifest.allowed_protocols.includes(rawManifest.wire_protocol) ||
  rawManifest.endpoint_class !== "official_api" ||
  !isSha256(rawManifest.base_url_sha256)
) {
  fail("raw manifest frozen route identity is invalid");
}

const preregistrationSha256 = fileSha256(preregPath);
const corpusLockSha256 = fileSha256(corpusLockPath);
if (rawManifest.preregistration_sha256 !== preregistrationSha256) {
  fail("raw manifest preregistration hash does not match the frozen file");
}
if (rawManifest.corpus_lock_sha256 !== corpusLockSha256) {
  fail("raw manifest corpus lock hash does not match the frozen file");
}

function validateRawDescriptor(descriptor, expectedPath, expectedRecords, actualPath) {
  if (!descriptor || descriptor.path !== expectedPath) {
    fail(`raw descriptor path mismatch for ${expectedPath}`);
  }
  if (descriptor.records !== expectedRecords) {
    fail(`${expectedPath} descriptor must report ${expectedRecords} records`);
  }
  if (descriptor.sha256 !== fileSha256(actualPath)) {
    fail(`${expectedPath} descriptor digest mismatch`);
  }
}
validateRawDescriptor(
  rawManifest.capability,
  "raw-capability.jsonl",
  prereg.capability.records_expected,
  capabilityPath,
);
validateRawDescriptor(
  rawManifest.routing,
  "raw-routing.jsonl",
  prereg.routing.records_expected,
  routingPath,
);

const pricing = rawManifest.pricing;
if (!pricing || pricing.currency !== "USD" || pricing.unit !== "per_million_tokens") {
  fail("raw manifest must freeze a USD per-million-token pricing schedule");
}
for (const field of [
  "input_usd_per_million",
  "cache_read_usd_per_million",
  "output_usd_per_million",
]) {
  if (
    typeof pricing[field] !== "number" ||
    !Number.isFinite(pricing[field]) ||
    pricing[field] < 0
  ) {
    fail(`raw manifest pricing.${field} must be finite and non-negative`);
  }
}
if (pricing.input_usd_per_million === 0 || pricing.output_usd_per_million === 0) {
  fail("raw manifest input/output price must be positive");
}
const rawIdentity = {
  evidence_version: rawManifest.evidence_version,
  run_seed: rawManifest.run_seed,
  commit: rawManifest.commit,
  preregistration_sha256: rawManifest.preregistration_sha256,
  corpus_lock_sha256: rawManifest.corpus_lock_sha256,
  pricing,
  resolved_model: rawManifest.resolved_model,
  wire_protocol: rawManifest.wire_protocol,
  endpoint_class: rawManifest.endpoint_class,
  base_url_sha256: rawManifest.base_url_sha256,
  allowed_models: rawManifest.allowed_models,
  allowed_protocols: rawManifest.allowed_protocols,
  allowed_endpoint_classes: rawManifest.allowed_endpoint_classes,
};
if (rawManifest.identity_sha256 !== sha256(canonicalJson(rawIdentity))) {
  fail("raw manifest identity digest mismatch");
}

if (capability.length !== prereg.capability.records_expected) {
  fail(`capability records must be ${prereg.capability.records_expected}, got ${capability.length}`);
}
if (routing.length !== prereg.routing.records_expected) {
  fail(`routing records must be ${prereg.routing.records_expected}, got ${routing.length}`);
}

const arms = prereg.capability.arms;
const expectedProfiles = {
  direct_agent: ["direct_agent", false, "off"],
  plan_baseline: ["baseline", false, "off"],
  plan_dual_track: ["plan_native_v1", true, "open"],
};
const ownedIds = new Map();
const environments = new Map();
const capabilityKeys = new Set();
const routingIds = new Set();
const capabilityConfigHashes = new Set();
const routingConfigHashes = new Set();
const observedModels = new Set();
const observedProtocols = new Set();

function claimOwnership(kind, values, key) {
  for (const value of values) {
    const ownerKey = `${kind}:${value}`;
    if (ownedIds.has(ownerKey)) {
      fail(`${kind} ${value} is shared by ${ownedIds.get(ownerKey)} and ${key}`);
    }
    ownedIds.set(ownerKey, key);
  }
}

function validateProviderAndEvidence(record, key, expectedOrder, orderKind, orderIdentity) {
  if (record.provider_kind !== "deepseek") fail(`${key} provider_kind must be deepseek`);
  const model = requireString(record, "resolved_model", key);
  const protocol = requireString(record, "wire_protocol", key);
  if (!rawManifest.allowed_models.includes(model)) fail(`${key} model is outside the allowlist`);
  if (!rawManifest.allowed_protocols.includes(protocol)) {
    fail(`${key} protocol is outside the allowlist`);
  }
  if (record.endpoint_class !== "official_api") fail(`${key} endpoint_class must be official_api`);
  if (
    model !== rawManifest.resolved_model ||
    protocol !== rawManifest.wire_protocol ||
    record.endpoint_class !== rawManifest.endpoint_class
  ) {
    fail(`${key} route differs from the raw manifest frozen identity`);
  }
  observedModels.add(model);
  observedProtocols.add(protocol);
  if (record.dry_run !== false) fail(`${key} must not be a dry-run record`);
  if (record.commit !== rawManifest.commit) fail(`${key} commit mismatch`);
  if (record.preregistration_sha256 !== preregistrationSha256) {
    fail(`${key} preregistration hash mismatch`);
  }
  if (record.corpus_lock_sha256 !== corpusLockSha256) fail(`${key} corpus lock hash mismatch`);
  if (record.run_seed_sha256 !== rawManifest.run_seed_sha256) fail(`${key} run seed hash mismatch`);
  requireSha(record, "config_sha256", key);
  requireSha(record, "profile_sha256", key);
  requireSha(record, "request_audit_sha256", key);
  requireSha(record, "artifact_sha256", key);
  if (record.request_audit_mismatches !== 0) {
    fail(`${key} request audit self-check mismatches must be zero`);
  }

  const requestIds = requireUniqueStrings(record, "request_ids", key);
  const operationIds = requireUniqueStrings(record, "operation_ids", key);
  const runIds = requireUniqueStrings(record, "run_ids", key);
  if (record.request_id !== requestIds[0]) fail(`${key} request_id must equal request_ids[0]`);
  if (record.operation_id !== operationIds[0]) {
    fail(`${key} operation_id must equal operation_ids[0]`);
  }
  claimOwnership("request", requestIds, key);
  claimOwnership("operation", operationIds, key);
  claimOwnership("run", runIds, key);

  const totalTokens = requireNumber(record, "total_tokens", key, {
    integer: true,
    positive: true,
  });
  const inputTokens = requireNumber(record, "input_tokens", key, {
    integer: true,
    positive: true,
  });
  const outputTokens = requireNumber(record, "output_tokens", key, { integer: true });
  const cacheReadTokens = requireNumber(record, "cache_read_tokens", key, { integer: true });
  requireNumber(record, "cache_write_tokens", key, { integer: true });
  if (cacheReadTokens > inputTokens) fail(`${key} cache_read_tokens exceeds input_tokens`);
  if (inputTokens + outputTokens !== totalTokens) {
    fail(`${key} total_tokens does not equal input + output`);
  }
  requireNumber(record, "rounds", key, { integer: true, positive: true });
  requireNumber(record, "wall_time_ms", key, { integer: true, positive: true });
  const costUsd = requireNumber(record, "cost_usd", key);
  const expectedCostUsd =
    ((inputTokens - cacheReadTokens) * pricing.input_usd_per_million +
      cacheReadTokens * pricing.cache_read_usd_per_million +
      outputTokens * pricing.output_usd_per_million) /
    1_000_000;
  if (Math.abs(costUsd - expectedCostUsd) > 1e-12) fail(`${key} cost_usd does not match frozen pricing`);
  const retryCount = requireNumber(record, "retry_count", key, { integer: true });
  if (!Array.isArray(record.retry_reasons) || record.retry_reasons.length !== retryCount) {
    fail(`${key} retry_reasons length must equal retry_count`);
  }
  if (record.retry_reasons.some((reason) => reason !== "stream_replay")) {
    fail(`${key} contains an unrecognized retry reason`);
  }
  if (retryCount > prereg.evidence.retry_count_max_per_record) {
    fail(`${key} has ${retryCount} retries, exceeding the preregistered maximum`);
  }

  const expectedOrderKey = sha256(`${rawManifest.run_seed}\0${orderKind}\0${orderIdentity}`);
  if (record.order_index !== expectedOrder || record.order_key_sha256 !== expectedOrderKey) {
    fail(`${key} does not match the reproducibly randomized run order`);
  }
  const environment = requireString(record, "environment_fingerprint", key);
  if (environments.has(environment)) {
    fail(`${key} shares environment state with ${environments.get(environment)}`);
  }
  environments.set(environment, key);

  const artifactPath = resolveArtifact(record.artifact_uri, key);
  if (fileSha256(artifactPath) !== record.artifact_sha256) fail(`${key} artifact digest mismatch`);
  const artifact = parseJsonFile(artifactPath, `${key} artifact`);
  if (!Array.isArray(artifact.request_headers) || artifact.request_headers.length !== record.rounds) {
    fail(`${key} artifact RequestHeader count mismatch`);
  }
  if (sha256(JSON.stringify(artifact.request_headers)) !== record.request_audit_sha256) {
    fail(`${key} sanitized RequestHeader digest mismatch`);
  }
  if (
    artifact.request_audit_sha256 !== record.request_audit_sha256 ||
    artifact.request_audit_mismatches !== 0
  ) {
    fail(`${key} artifact request audit claim mismatch`);
  }
  const artifactRequestIds = (artifact.origins ?? []).map((origin) => origin.request_key);
  const artifactOperationIds = (artifact.origins ?? []).map((origin) => origin.operation_id);
  const artifactRunIds = (artifact.run_usage ?? []).map((usage) => usage.run_id);
  if (!exactArray(artifactRequestIds, requestIds) || !exactArray(artifactOperationIds, operationIds)) {
    fail(`${key} artifact origin IDs mismatch`);
  }
  if (!exactArray(artifactRunIds, runIds)) fail(`${key} artifact run usage IDs mismatch`);
  for (const field of [
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "stream_retries",
  ]) {
    const recordField = field === "stream_retries" ? "retry_count" : field;
    if (artifact.usage?.[field] !== record[recordField]) {
      fail(`${key} artifact usage.${field} mismatch`);
    }
  }
  if (artifact.cost_usd !== record.cost_usd) fail(`${key} artifact cost mismatch`);
  return artifact;
}

const expectedCapabilityOrder = capability
  .map((record) => ({
    key: `${record.case_id}:${record.arm}`,
    order: sha256(`${rawManifest.run_seed}\0capability\0${record.case_id}:${record.arm}`),
  }))
  .sort((left, right) => left.order.localeCompare(right.order) || left.key.localeCompare(right.key));
const capabilityOrderByKey = new Map(
  expectedCapabilityOrder.map((item, index) => [item.key, index]),
);

for (const record of capability) {
  const key = `${record.case_id}:${record.arm}`;
  if (capabilityKeys.has(key)) fail(`duplicate capability record: ${key}`);
  capabilityKeys.add(key);
  if (!arms.includes(record.arm) || !expectedProfiles[record.arm]) fail(`${key} has unknown arm`);
  const [profileKind, profileEnabled, releaseState] = expectedProfiles[record.arm];
  if (
    record.profile_kind !== profileKind ||
    record.profile_enabled !== profileEnabled ||
    record.release_state !== releaseState
  ) {
    fail(`${key} profile/release isolation mismatch`);
  }
  if (
    typeof record.tests_passed !== "boolean" ||
    typeof record.unapproved_side_effects !== "boolean"
  ) {
    fail(`${key} missing boolean test/side-effect outcomes`);
  }
  for (const digest of [
    "fixture_sha256",
    "initial_workspace_sha256",
    "preapproval_workspace_sha256",
    "final_workspace_sha256",
    "diff_digest",
  ]) {
    requireSha(record, digest, key);
  }
  if (record.fixture_sha256 !== record.initial_workspace_sha256) {
    fail(`${key} initial workspace differs from fixture`);
  }
  if (
    record.arm.startsWith("plan_") &&
    record.unapproved_side_effects !==
      (record.preapproval_workspace_sha256 !== record.initial_workspace_sha256)
  ) {
    fail(`${key} unapproved side-effect flag does not match the preapproval tree digest`);
  }
  if (record.artifact_uri !== `raw/capability/${record.case_id}/${record.arm}.json`) {
    fail(`${key} artifact_uri mismatch`);
  }
  const artifact = validateProviderAndEvidence(
    record,
    key,
    capabilityOrderByKey.get(key),
    "capability",
    key,
  );
  if (
    artifact.schema !== "r-code-plan-capability-artifact/v1" ||
    artifact.case_id !== record.case_id ||
    artifact.arm !== record.arm
  ) {
    fail(`${key} artifact identity mismatch`);
  }
  for (const field of [
    "fixture_sha256",
    "initial_workspace_sha256",
    "preapproval_workspace_sha256",
    "final_workspace_sha256",
    "diff_digest",
    "config_sha256",
    "profile_sha256",
    "preregistration_sha256",
    "corpus_lock_sha256",
  ]) {
    if (artifact.hashes?.[field] !== record[field]) {
      fail(`${key} artifact hashes.${field} mismatch`);
    }
  }
  capabilityConfigHashes.add(record.config_sha256);
}

const caseIds = new Set(capability.map((record) => record.case_id));
if (caseIds.size !== 25) fail(`expected 25 distinct cases, got ${caseIds.size}`);
for (const caseId of caseIds) {
  for (const arm of arms) {
    if (!capabilityKeys.has(`${caseId}:${arm}`)) fail(`missing capability record ${caseId}:${arm}`);
  }
  const records = capability.filter((record) => record.case_id === caseId);
  if (new Set(records.map((record) => record.fixture_sha256)).size !== 1) {
    fail(`case ${caseId} arms do not share the same frozen fixture hash`);
  }
}
if (capabilityConfigHashes.size !== 1) {
  fail("capability records do not share one frozen config hash");
}

const expectedRoutingOrder = routing
  .map((record) => ({
    id: record.id,
    order: sha256(`${rawManifest.run_seed}\0routing\0${record.id}`),
  }))
  .sort((left, right) => left.order.localeCompare(right.order) || left.id.localeCompare(right.id));
const routingOrderById = new Map(expectedRoutingOrder.map((item, index) => [item.id, index]));
for (const record of routing) {
  const key = `routing:${record.id}`;
  if (routingIds.has(record.id)) fail(`duplicate routing record: ${record.id}`);
  routingIds.add(record.id);
  if (!new Set(["simple", "complex"]).has(record.label)) fail(`${key} has invalid label`);
  if (
    record.profile_kind !== "routing_experiment" ||
    record.profile_enabled !== true ||
    record.release_state !== "open"
  ) {
    fail(`${key} routing profile isolation mismatch`);
  }
  if (typeof record.suggested !== "boolean") fail(`${key} suggested must be boolean`);
  requireNumber(record, "repeat_prompts", key, { integer: true });
  requireSha(record, "initial_workspace_sha256", key);
  requireSha(record, "final_workspace_sha256", key);
  if (
    record.routing_side_effects !== false ||
    record.initial_workspace_sha256 !== record.final_workspace_sha256
  ) {
    fail(`${key} routing probe mutated its read-only workspace`);
  }
  if (record.artifact_uri !== `raw/routing/${record.id}.json`) fail(`${key} artifact_uri mismatch`);
  const artifact = validateProviderAndEvidence(
    record,
    key,
    routingOrderById.get(record.id),
    "routing",
    record.id,
  );
  if (
    artifact.schema !== "r-code-plan-routing-artifact/v1" ||
    artifact.id !== record.id ||
    artifact.label !== record.label
  ) {
    fail(`${key} artifact identity mismatch`);
  }
  for (const field of [
    "initial_workspace_sha256",
    "final_workspace_sha256",
    "config_sha256",
    "profile_sha256",
    "preregistration_sha256",
    "corpus_lock_sha256",
  ]) {
    if (artifact.hashes?.[field] !== record[field]) {
      fail(`${key} artifact hashes.${field} mismatch`);
    }
  }
  routingConfigHashes.add(record.config_sha256);
}
if (routingConfigHashes.size !== 1) fail("routing records do not share one frozen config hash");
if (observedModels.size !== 1 || observedProtocols.size !== 1) {
  fail("all capability and routing records must use one frozen model and wire protocol");
}
const publishedArtifactUris = listRawArtifactUris(join(artifacts, "raw")).sort();
const referencedUris = [...referencedArtifactUris].sort();
if (!exactArray(publishedArtifactUris, referencedUris) || referencedUris.length !== 115) {
  fail("raw artifact tree must contain exactly the 115 referenced redacted artifacts");
}

const solved = (record) => record.tests_passed === true;
const byArm = (arm) => capability.filter((record) => record.arm === arm);
const baseline = byArm("plan_baseline");
const dual = byArm("plan_dual_track");
let dualWins = 0;
let dualLosses = 0;
for (const baseRecord of baseline) {
  const dualRecord = dual.find((record) => record.case_id === baseRecord.case_id);
  if (!dualRecord) fail(`missing dual record for ${baseRecord.case_id}`);
  if (solved(dualRecord) && !solved(baseRecord)) dualWins += 1;
  if (!solved(dualRecord) && solved(baseRecord)) dualLosses += 1;
}
const netSolvedGain = dualWins - dualLosses;

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
  if (values.length === 0) fail("cannot compute median of an empty sample");
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const p95 = (values) => {
  if (values.length === 0) fail("cannot compute p95 of an empty sample");
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1)];
};
const baselineMedianTokens = median(baseline.map((record) => record.total_tokens));
const baselineP95Wall = p95(baseline.map((record) => record.wall_time_ms));
if (baselineMedianTokens <= 0 || baselineP95Wall <= 0) {
  fail("baseline token/wall denominators must be positive");
}
const dualMedianTokensRatio =
  median(dual.map((record) => record.total_tokens)) / baselineMedianTokens;
const dualP95WallRatio = p95(dual.map((record) => record.wall_time_ms)) / baselineP95Wall;
if (!Number.isFinite(dualMedianTokensRatio) || !Number.isFinite(dualP95WallRatio)) {
  fail("token/wall ratios must be finite");
}

const simple = routing.filter((record) => record.label === "simple");
const complex = routing.filter((record) => record.label === "complex");
if (
  simple.length !== prereg.routing.probes_simple ||
  complex.length !== prereg.routing.probes_complex
) {
  fail(`routing split must be ${prereg.routing.probes_simple}/${prereg.routing.probes_complex}`);
}
const simpleFalsePromptRate = simple.filter((record) => record.suggested).length / simple.length;
const complexRecallRate = complex.filter((record) => record.suggested).length / complex.length;
const sameRequestRepeatRate =
  routing.filter((record) => record.repeat_prompts > 0).length / routing.length;

const gates = prereg.gates;
const failures = [];
if (netSolvedGain < gates.net_solved_gain_min) {
  failures.push(`net_solved_gain ${netSolvedGain} < ${gates.net_solved_gain_min}`);
}
if (dualLosses > gates.regressions_max) {
  failures.push(`regressions ${dualLosses} > ${gates.regressions_max}`);
}
if (mcnemarP > gates.mcnemar_p_one_sided_max) {
  failures.push(`mcnemar p ${mcnemarP.toFixed(4)} > ${gates.mcnemar_p_one_sided_max}`);
}
if (unapprovedSideEffects > gates.unapproved_side_effects_max) {
  failures.push(
    `unapproved side effects ${unapprovedSideEffects} > ${gates.unapproved_side_effects_max}`,
  );
}
if (simpleFalsePromptRate > gates.simple_false_prompt_rate_max) {
  failures.push(
    `simple false prompt rate ${simpleFalsePromptRate.toFixed(3)} > ${gates.simple_false_prompt_rate_max}`,
  );
}
if (complexRecallRate < gates.complex_recall_rate_min) {
  failures.push(
    `complex recall ${complexRecallRate.toFixed(3)} < ${gates.complex_recall_rate_min}`,
  );
}
if (sameRequestRepeatRate > gates.same_request_repeat_rate_max) {
  failures.push(
    `same request repeat rate ${sameRequestRepeatRate} > ${gates.same_request_repeat_rate_max}`,
  );
}
if (dualMedianTokensRatio > gates.dual_median_tokens_ratio_max) {
  failures.push(
    `dual median tokens ratio ${dualMedianTokensRatio.toFixed(3)} > ${gates.dual_median_tokens_ratio_max}`,
  );
}
if (dualP95WallRatio > gates.dual_p95_wall_time_ratio_max) {
  failures.push(
    `dual p95 wall ratio ${dualP95WallRatio.toFixed(3)} > ${gates.dual_p95_wall_time_ratio_max}`,
  );
}
if (failures.length > 0) fail(`preregistered gates failed:\n  - ${failures.join("\n  - ")}`);

const rawDigest = createHash("sha256")
  .update(readFileSync(capabilityPath))
  .update(readFileSync(routingPath))
  .update(readFileSync(rawManifestPath))
  .digest("hex");
const totalCostUsd = [...capability, ...routing].reduce(
  (sum, record) => sum + record.cost_usd,
  0,
);
if (!Number.isFinite(totalCostUsd)) fail("aggregate cost is not finite");

const manifest = {
  schema: "r-code-plan-evidence-manifest/v1",
  provider_kind: "deepseek",
  eligibility_profile_version: prereg.eligibility_profile_version,
  evidence_version: rawManifest.evidence_version,
  allowed_models: rawManifest.allowed_models,
  allowed_protocols: rawManifest.allowed_protocols,
  allowed_endpoint_classes: rawManifest.allowed_endpoint_classes,
  preregistration_sha256: preregistrationSha256,
  corpus_lock_sha256: corpusLockSha256,
  commit: rawManifest.commit,
  run_seed_sha256: rawManifest.run_seed_sha256,
  resolved_model: [...observedModels][0],
  wire_protocol: [...observedProtocols][0],
  config_sha256: {
    capability: [...capabilityConfigHashes][0],
    routing: [...routingConfigHashes][0],
  },
  pricing,
  total_cost_usd: totalCostUsd,
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
  raw_manifest_sha256: fileSha256(rawManifestPath),
};

writeFileSync(outputManifest, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(
  "score: all preregistered gates and raw-evidence checks passed; manifest.json written",
);
