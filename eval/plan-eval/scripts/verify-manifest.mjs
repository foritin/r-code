#!/usr/bin/env node
/**
 * verify-manifest.mjs —— 独立 claim verification。
 * 不导入 score.mjs；从 raw 文件、脱敏 artifact、preregistration 与 corpus lock
 * 重新计算身份、来源、完整性、成本和全部统计声明。
 */
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readFileSync, readdirSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

const rootFlag = process.argv.indexOf("--root");
const root = resolve(
  rootFlag >= 0 && process.argv[rootFlag + 1]
    ? process.argv[rootFlag + 1]
    : join(import.meta.dirname, ".."),
);
const artifacts = join(root, "artifacts");

function abort(message) {
  console.error(`verify-manifest: FAIL: ${message}`);
  process.exit(1);
}

function loadJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    abort(`${label} is unreadable or invalid JSON: ${error.message}`);
  }
}

function loadJsonl(path, label) {
  let source;
  try {
    source = readFileSync(path, "utf8");
  } catch (error) {
    abort(`${label} is unreadable: ${error.message}`);
  }
  return source
    .split(/\r?\n/)
    .filter((line) => line.trim())
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        abort(`${label} line ${index + 1} is invalid JSON: ${error.message}`);
      }
    });
}

const digest = (value) => createHash("sha256").update(value).digest("hex");
const digestFile = (path) => digest(readFileSync(path));
const shaPattern = /^[0-9a-f]{64}$/;
const commitPattern = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const sameArray = (left, right) =>
  Array.isArray(left) && JSON.stringify(left) === JSON.stringify(right);
const referencedArtifacts = new Set();
const canonical = (value) => {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
};

function finiteNumber(value, label, { integer = false, positive = false } = {}) {
  if (typeof value !== "number" || !Number.isFinite(value)) abort(`${label} must be finite`);
  if (integer && !Number.isInteger(value)) abort(`${label} must be an integer`);
  if (positive ? value <= 0 : value < 0) {
    abort(`${label} must be ${positive ? "positive" : "non-negative"}`);
  }
  return value;
}

function nonBlank(value, label) {
  if (typeof value !== "string" || !value.trim()) abort(`${label} must be non-blank`);
  return value;
}

function sha(value, label) {
  nonBlank(value, label);
  if (!shaPattern.test(value)) abort(`${label} must be a lowercase SHA-256 digest`);
  return value;
}

function uniqueStrings(value, label) {
  if (!Array.isArray(value) || value.length === 0) abort(`${label} must be non-empty`);
  if (value.some((item) => typeof item !== "string" || !item.trim())) {
    abort(`${label} contains a blank/non-string identifier`);
  }
  if (new Set(value).size !== value.length) abort(`${label} contains duplicates`);
  return value;
}

function scanNoSecrets(path) {
  if (!existsSync(path)) return;
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) abort(`artifact tree contains a symlink: ${path}`);
  if (!stat.isDirectory()) return;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isSymbolicLink()) abort(`artifact tree contains a symlink: ${child}`);
    if (entry.isDirectory()) scanNoSecrets(child);
    if (entry.isFile() && entry.name.toLowerCase() === "secrets.json") {
      abort(`artifact tree contains secret material: ${child}`);
    }
  }
}

function artifactPath(uri, label) {
  if (typeof uri !== "string" || !uri || isAbsolute(uri) || uri.includes("\\")) {
    abort(`${label} artifact URI is not a safe relative path`);
  }
  const target = resolve(artifacts, ...uri.split("/"));
  const rel = relative(artifacts, target);
  if (!rel || rel === ".." || rel.startsWith(`..${sep}`)) abort(`${label} artifact URI escapes root`);
  if (!existsSync(target)) abort(`${label} artifact is unavailable: ${uri}`);
  const stat = lstatSync(target);
  if (!stat.isFile() || stat.isSymbolicLink()) abort(`${label} artifact is not a regular file`);
  referencedArtifacts.add(uri);
  return target;
}

function enumerateRawArtifacts(path) {
  if (!existsSync(path)) return [];
  const found = [];
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isSymbolicLink()) abort(`raw artifact tree contains a symlink: ${child}`);
    if (entry.isDirectory()) found.push(...enumerateRawArtifacts(child));
    if (entry.isFile()) found.push(relative(artifacts, child).split(sep).join("/"));
  }
  return found;
}

for (const name of [
  "manifest.json",
  "raw-manifest.json",
  "raw-capability.jsonl",
  "raw-routing.jsonl",
]) {
  if (!existsSync(join(artifacts, name))) abort(`missing ${name}`);
}
scanNoSecrets(artifacts);

const preregPath = join(root, "schema", "preregistration.json");
const corpusPath = join(root, "corpus-lock.json");
const manifestPath = join(artifacts, "manifest.json");
const rawManifestPath = join(artifacts, "raw-manifest.json");
const capabilityPath = join(artifacts, "raw-capability.jsonl");
const routingPath = join(artifacts, "raw-routing.jsonl");
const prereg = loadJson(preregPath, "preregistration");
const manifest = loadJson(manifestPath, "evidence manifest");
const rawManifest = loadJson(rawManifestPath, "raw manifest");
const capability = loadJsonl(capabilityPath, "raw capability");
const routing = loadJsonl(routingPath, "raw routing");
const preregHash = digestFile(preregPath);
const corpusHash = digestFile(corpusPath);

if (manifest.schema !== "r-code-plan-evidence-manifest/v1") abort("evidence manifest schema mismatch");
if (rawManifest.schema !== "r-code-plan-raw-manifest/v1" || rawManifest.status !== "complete") {
  abort("raw manifest must use v1 schema and complete status");
}
if (manifest.provider_kind !== "deepseek") abort("manifest provider must be deepseek");
if (!commitPattern.test(rawManifest.commit ?? "") || manifest.commit !== rawManifest.commit) {
  abort("manifest/raw commit identity mismatch");
}
if (manifest.evidence_version !== rawManifest.evidence_version) abort("evidence version mismatch");
if (rawManifest.run_seed_sha256 !== digest(rawManifest.run_seed ?? "")) abort("run seed digest mismatch");
if (manifest.run_seed_sha256 !== rawManifest.run_seed_sha256) abort("manifest run seed mismatch");
if (manifest.preregistration_sha256 !== preregHash || rawManifest.preregistration_sha256 !== preregHash) {
  abort("preregistration digest mismatch");
}
if (manifest.corpus_lock_sha256 !== corpusHash || rawManifest.corpus_lock_sha256 !== corpusHash) {
  abort("corpus lock digest mismatch");
}

const expectedModels = ["deepseek-v4-flash", "deepseek-v4-pro"];
const expectedProtocols = ["openai_chat", "openai_responses", "anthropic_messages"];
if (!sameArray(rawManifest.allowed_models, expectedModels) || !sameArray(manifest.allowed_models, expectedModels)) {
  abort("model allowlist mismatch");
}
if (
  !sameArray(rawManifest.allowed_protocols, expectedProtocols) ||
  !sameArray(manifest.allowed_protocols, expectedProtocols)
) {
  abort("protocol allowlist mismatch");
}
if (
  !sameArray(rawManifest.allowed_endpoint_classes, ["official_api"]) ||
  !sameArray(manifest.allowed_endpoint_classes, ["official_api"])
) {
  abort("endpoint allowlist mismatch");
}
if (
  !expectedModels.includes(rawManifest.resolved_model) ||
  !expectedProtocols.includes(rawManifest.wire_protocol) ||
  rawManifest.endpoint_class !== "official_api" ||
  !shaPattern.test(rawManifest.base_url_sha256 ?? "")
) {
  abort("raw frozen route identity is invalid");
}

const pricing = rawManifest.pricing;
if (!pricing || pricing.currency !== "USD" || pricing.unit !== "per_million_tokens") {
  abort("raw pricing schedule is missing or malformed");
}
for (const field of [
  "input_usd_per_million",
  "cache_read_usd_per_million",
  "output_usd_per_million",
]) {
  finiteNumber(pricing[field], `pricing.${field}`);
}
if (pricing.input_usd_per_million === 0 || pricing.output_usd_per_million === 0) {
  abort("input/output pricing must be positive");
}
if (JSON.stringify(manifest.pricing) !== JSON.stringify(pricing)) abort("manifest pricing mismatch");
const reconstructedIdentity = {
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
if (rawManifest.identity_sha256 !== digest(canonical(reconstructedIdentity))) {
  abort("raw identity_sha256 does not cover the frozen identity");
}

function checkDescriptor(descriptor, name, expected, path) {
  if (!descriptor || descriptor.path !== name || descriptor.records !== expected) {
    abort(`${name} raw descriptor mismatch`);
  }
  if (descriptor.sha256 !== digestFile(path)) abort(`${name} raw descriptor digest mismatch`);
}
checkDescriptor(rawManifest.capability, "raw-capability.jsonl", 75, capabilityPath);
checkDescriptor(rawManifest.routing, "raw-routing.jsonl", 40, routingPath);
if (capability.length !== 75 || routing.length !== 40) abort("raw record count must be 75 + 40");

const identifierOwners = new Map();
const environmentOwners = new Map();
const models = new Set();
const protocols = new Set();
const capConfig = new Set();
const routeConfig = new Set();
let independentlySummedCost = 0;

function own(type, values, label) {
  for (const value of values) {
    const identity = `${type}:${value}`;
    if (identifierOwners.has(identity)) {
      abort(`${type} ${value} is shared by ${identifierOwners.get(identity)} and ${label}`);
    }
    identifierOwners.set(identity, label);
  }
}

function independentlyCheckRecord(record, label, orderGroup, orderIdentity, expectedIndex) {
  if (record.provider_kind !== "deepseek" || record.endpoint_class !== "official_api") {
    abort(`${label} provider provenance is not native official DeepSeek`);
  }
  const model = nonBlank(record.resolved_model, `${label}.resolved_model`);
  const protocol = nonBlank(record.wire_protocol, `${label}.wire_protocol`);
  if (!expectedModels.includes(model) || !expectedProtocols.includes(protocol)) {
    abort(`${label} model/protocol is outside the frozen allowlist`);
  }
  if (
    model !== rawManifest.resolved_model ||
    protocol !== rawManifest.wire_protocol ||
    record.endpoint_class !== rawManifest.endpoint_class
  ) {
    abort(`${label} route differs from the raw frozen identity`);
  }
  models.add(model);
  protocols.add(protocol);
  if (record.dry_run !== false) abort(`${label} is a dry-run record`);
  if (record.commit !== rawManifest.commit) abort(`${label} commit mismatch`);
  if (
    record.preregistration_sha256 !== preregHash ||
    record.corpus_lock_sha256 !== corpusHash ||
    record.run_seed_sha256 !== rawManifest.run_seed_sha256
  ) {
    abort(`${label} frozen hash identity mismatch`);
  }
  sha(record.config_sha256, `${label}.config_sha256`);
  sha(record.profile_sha256, `${label}.profile_sha256`);
  sha(record.request_audit_sha256, `${label}.request_audit_sha256`);
  sha(record.artifact_sha256, `${label}.artifact_sha256`);
  if (record.request_audit_mismatches !== 0) abort(`${label} has request-audit mismatches`);

  const requests = uniqueStrings(record.request_ids, `${label}.request_ids`);
  const operations = uniqueStrings(record.operation_ids, `${label}.operation_ids`);
  const runs = uniqueStrings(record.run_ids, `${label}.run_ids`);
  if (record.request_id !== requests[0] || record.operation_id !== operations[0]) {
    abort(`${label} primary request/operation identity mismatch`);
  }
  own("request", requests, label);
  own("operation", operations, label);
  own("run", runs, label);

  const input = finiteNumber(record.input_tokens, `${label}.input_tokens`, {
    integer: true,
    positive: true,
  });
  const output = finiteNumber(record.output_tokens, `${label}.output_tokens`, { integer: true });
  const cacheRead = finiteNumber(record.cache_read_tokens, `${label}.cache_read_tokens`, {
    integer: true,
  });
  finiteNumber(record.cache_write_tokens, `${label}.cache_write_tokens`, { integer: true });
  if (cacheRead > input || record.total_tokens !== input + output) {
    abort(`${label} token accounting is inconsistent`);
  }
  finiteNumber(record.total_tokens, `${label}.total_tokens`, { integer: true, positive: true });
  finiteNumber(record.rounds, `${label}.rounds`, { integer: true, positive: true });
  finiteNumber(record.wall_time_ms, `${label}.wall_time_ms`, { integer: true, positive: true });
  const retries = finiteNumber(record.retry_count, `${label}.retry_count`, { integer: true });
  if (
    retries > prereg.evidence.retry_count_max_per_record ||
    !Array.isArray(record.retry_reasons) ||
    record.retry_reasons.length !== retries ||
    record.retry_reasons.some((reason) => reason !== "stream_replay")
  ) {
    abort(`${label} retry evidence violates preregistration`);
  }
  const expectedCost =
    ((input - cacheRead) * pricing.input_usd_per_million +
      cacheRead * pricing.cache_read_usd_per_million +
      output * pricing.output_usd_per_million) /
    1_000_000;
  const actualCost = finiteNumber(record.cost_usd, `${label}.cost_usd`);
  if (Math.abs(actualCost - expectedCost) > 1e-12) abort(`${label} cost does not match pricing`);
  independentlySummedCost += actualCost;

  const expectedOrderKey = digest(`${rawManifest.run_seed}\0${orderGroup}\0${orderIdentity}`);
  if (record.order_key_sha256 !== expectedOrderKey || record.order_index !== expectedIndex) {
    abort(`${label} randomized order claim mismatch`);
  }
  const environment = nonBlank(record.environment_fingerprint, `${label}.environment_fingerprint`);
  if (environmentOwners.has(environment)) {
    abort(`${label} shares environment with ${environmentOwners.get(environment)}`);
  }
  environmentOwners.set(environment, label);

  const path = artifactPath(record.artifact_uri, label);
  if (digestFile(path) !== record.artifact_sha256) abort(`${label} artifact digest mismatch`);
  const artifact = loadJson(path, `${label} artifact`);
  const headers = artifact.request_headers;
  if (!Array.isArray(headers) || headers.length !== record.rounds) {
    abort(`${label} RequestHeader count mismatch`);
  }
  if (
    digest(JSON.stringify(headers)) !== record.request_audit_sha256 ||
    artifact.request_audit_sha256 !== record.request_audit_sha256 ||
    artifact.request_audit_mismatches !== 0
  ) {
    abort(`${label} RequestHeader audit digest mismatch`);
  }
  const artifactRequests = (artifact.origins ?? []).map((origin) => origin.request_key);
  const artifactOperations = (artifact.origins ?? []).map((origin) => origin.operation_id);
  if (!sameArray(artifactRequests, requests) || !sameArray(artifactOperations, operations)) {
    abort(`${label} artifact origin envelope mismatch`);
  }
  const perRun = artifact.run_usage;
  if (!Array.isArray(perRun) || !sameArray(perRun.map((usage) => usage.run_id), runs)) {
    abort(`${label} artifact run usage identity mismatch`);
  }
  const aggregate = perRun.reduce(
    (sum, usage) => {
      for (const field of [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "stream_retries",
      ]) {
        finiteNumber(usage[field], `${label}.run_usage.${field}`, { integer: true });
        sum[field] += usage[field];
      }
      return sum;
    },
    {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      stream_retries: 0,
    },
  );
  for (const [artifactField, recordField] of [
    ["input_tokens", "input_tokens"],
    ["output_tokens", "output_tokens"],
    ["cache_read_tokens", "cache_read_tokens"],
    ["cache_write_tokens", "cache_write_tokens"],
    ["stream_retries", "retry_count"],
  ]) {
    if (aggregate[artifactField] !== record[recordField]) {
      abort(`${label} per-run ${artifactField} does not sum to the raw record`);
    }
    if (artifact.usage?.[artifactField] !== record[recordField]) {
      abort(`${label} artifact aggregate ${artifactField} mismatch`);
    }
  }
  if (artifact.cost_usd !== actualCost) abort(`${label} artifact cost mismatch`);
  return artifact;
}

const capOrder = capability
  .map((record) => {
    const key = `${record.case_id}:${record.arm}`;
    return { key, order: digest(`${rawManifest.run_seed}\0capability\0${key}`) };
  })
  .sort((a, b) => a.order.localeCompare(b.order) || a.key.localeCompare(b.key));
const capIndex = new Map(capOrder.map((entry, index) => [entry.key, index]));
const capKeys = new Set();
const caseFixtureHashes = new Map();
const profileContract = {
  direct_agent: ["direct_agent", false, "off"],
  plan_baseline: ["baseline", false, "off"],
  plan_dual_track: ["plan_native_v1", true, "open"],
};
for (const record of capability) {
  const key = `${record.case_id}:${record.arm}`;
  if (capKeys.has(key)) abort(`duplicate capability record ${key}`);
  capKeys.add(key);
  const expectedProfile = profileContract[record.arm];
  if (
    !expectedProfile ||
    record.profile_kind !== expectedProfile[0] ||
    record.profile_enabled !== expectedProfile[1] ||
    record.release_state !== expectedProfile[2]
  ) {
    abort(`${key} profile isolation mismatch`);
  }
  if (typeof record.tests_passed !== "boolean" || typeof record.unapproved_side_effects !== "boolean") {
    abort(`${key} outcome booleans are missing`);
  }
  for (const field of [
    "fixture_sha256",
    "initial_workspace_sha256",
    "preapproval_workspace_sha256",
    "final_workspace_sha256",
    "diff_digest",
  ]) {
    sha(record[field], `${key}.${field}`);
  }
  if (record.fixture_sha256 !== record.initial_workspace_sha256) abort(`${key} fixture copy drift`);
  if (
    record.arm.startsWith("plan_") &&
    record.unapproved_side_effects !==
      (record.preapproval_workspace_sha256 !== record.initial_workspace_sha256)
  ) {
    abort(`${key} preapproval side-effect claim mismatch`);
  }
  if (record.artifact_uri !== `raw/capability/${record.case_id}/${record.arm}.json`) {
    abort(`${key} artifact URI identity mismatch`);
  }
  const artifact = independentlyCheckRecord(record, key, "capability", key, capIndex.get(key));
  if (
    artifact.schema !== "r-code-plan-capability-artifact/v1" ||
    artifact.case_id !== record.case_id ||
    artifact.arm !== record.arm
  ) {
    abort(`${key} artifact schema/identity mismatch`);
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
    if (artifact.hashes?.[field] !== record[field]) abort(`${key} artifact hash ${field} mismatch`);
  }
  capConfig.add(record.config_sha256);
  const fixtureSet = caseFixtureHashes.get(record.case_id) ?? new Set();
  fixtureSet.add(record.fixture_sha256);
  caseFixtureHashes.set(record.case_id, fixtureSet);
}
if (caseFixtureHashes.size !== 25) abort("capability must contain exactly 25 cases");
for (const [caseId, fixtures] of caseFixtureHashes) {
  if (fixtures.size !== 1) abort(`${caseId} arms use different fixtures`);
  for (const arm of Object.keys(profileContract)) {
    if (!capKeys.has(`${caseId}:${arm}`)) abort(`missing ${caseId}:${arm}`);
  }
}
if (capConfig.size !== 1) abort("capability config hash is not frozen");

const routingOrder = routing
  .map((record) => ({ id: record.id, order: digest(`${rawManifest.run_seed}\0routing\0${record.id}`) }))
  .sort((a, b) => a.order.localeCompare(b.order) || a.id.localeCompare(b.id));
const routingIndex = new Map(routingOrder.map((entry, index) => [entry.id, index]));
const routeIds = new Set();
for (const record of routing) {
  const label = `routing:${record.id}`;
  if (routeIds.has(record.id)) abort(`duplicate routing record ${record.id}`);
  routeIds.add(record.id);
  if (!new Set(["simple", "complex"]).has(record.label)) abort(`${label} invalid label`);
  if (
    record.profile_kind !== "routing_experiment" ||
    record.profile_enabled !== true ||
    record.release_state !== "open"
  ) {
    abort(`${label} release/profile mismatch`);
  }
  if (typeof record.suggested !== "boolean") abort(`${label}.suggested must be boolean`);
  finiteNumber(record.repeat_prompts, `${label}.repeat_prompts`, { integer: true });
  sha(record.initial_workspace_sha256, `${label}.initial_workspace_sha256`);
  sha(record.final_workspace_sha256, `${label}.final_workspace_sha256`);
  if (
    record.routing_side_effects !== false ||
    record.initial_workspace_sha256 !== record.final_workspace_sha256
  ) {
    abort(`${label} mutated the read-only routing workspace`);
  }
  if (record.artifact_uri !== `raw/routing/${record.id}.json`) abort(`${label} artifact URI mismatch`);
  const artifact = independentlyCheckRecord(
    record,
    label,
    "routing",
    record.id,
    routingIndex.get(record.id),
  );
  if (
    artifact.schema !== "r-code-plan-routing-artifact/v1" ||
    artifact.id !== record.id ||
    artifact.label !== record.label
  ) {
    abort(`${label} artifact schema/identity mismatch`);
  }
  for (const field of [
    "initial_workspace_sha256",
    "final_workspace_sha256",
    "config_sha256",
    "profile_sha256",
    "preregistration_sha256",
    "corpus_lock_sha256",
  ]) {
    if (artifact.hashes?.[field] !== record[field]) abort(`${label} artifact hash ${field} mismatch`);
  }
  routeConfig.add(record.config_sha256);
}
if (routeIds.size !== 40 || routeConfig.size !== 1) abort("routing identity/config is incomplete");
if (models.size !== 1 || protocols.size !== 1) abort("provider model/protocol changed across records");
const rawArtifactFiles = enumerateRawArtifacts(join(artifacts, "raw")).sort();
const referencedArtifactFiles = [...referencedArtifacts].sort();
if (!sameArray(rawArtifactFiles, referencedArtifactFiles) || referencedArtifactFiles.length !== 115) {
  abort("raw artifact tree is not exactly the 115 referenced redacted artifacts");
}
if (
  manifest.resolved_model !== [...models][0] ||
  manifest.wire_protocol !== [...protocols][0] ||
  manifest.config_sha256?.capability !== [...capConfig][0] ||
  manifest.config_sha256?.routing !== [...routeConfig][0]
) {
  abort("manifest provider/config claims do not match raw records");
}

const baseline = new Map(
  capability
    .filter((record) => record.arm === "plan_baseline")
    .map((record) => [record.case_id, record]),
);
const dual = new Map(
  capability
    .filter((record) => record.arm === "plan_dual_track")
    .map((record) => [record.case_id, record]),
);
let wins = 0;
let losses = 0;
for (const [caseId, base] of baseline) {
  const candidate = dual.get(caseId);
  if (!candidate) abort(`missing dual record for ${caseId}`);
  if (candidate.tests_passed && !base.tests_passed) wins += 1;
  if (!candidate.tests_passed && base.tests_passed) losses += 1;
}

function choose(n, k) {
  const smaller = Math.min(k, n - k);
  let result = 1;
  for (let index = 1; index <= smaller; index += 1) {
    result = (result * (n - smaller + index)) / index;
  }
  return result;
}
function binomialUpperTail(successes, failures) {
  const n = successes + failures;
  if (n === 0) return 1;
  let probability = 0;
  for (let count = successes; count <= n; count += 1) probability += choose(n, count) / 2 ** n;
  return Math.min(1, probability);
}
function independentMedian(values) {
  if (!values.length) abort("empty median sample");
  const ordered = values.toSorted((a, b) => a - b);
  const midpoint = Math.trunc(ordered.length / 2);
  return ordered.length % 2 ? ordered[midpoint] : (ordered[midpoint - 1] + ordered[midpoint]) / 2;
}
function independentP95(values) {
  if (!values.length) abort("empty p95 sample");
  const ordered = values.toSorted((a, b) => a - b);
  return ordered[Math.max(0, Math.ceil(0.95 * ordered.length) - 1)];
}
const pValue = binomialUpperTail(wins, losses);
const baselineValues = [...baseline.values()];
const dualValues = [...dual.values()];
const baseTokenMedian = independentMedian(baselineValues.map((record) => record.total_tokens));
const baseWallP95 = independentP95(baselineValues.map((record) => record.wall_time_ms));
if (baseTokenMedian <= 0 || baseWallP95 <= 0) abort("baseline metric denominator is zero");
const tokenRatio = independentMedian(dualValues.map((record) => record.total_tokens)) / baseTokenMedian;
const wallRatio = independentP95(dualValues.map((record) => record.wall_time_ms)) / baseWallP95;
if (!Number.isFinite(tokenRatio) || !Number.isFinite(wallRatio)) abort("non-finite efficiency ratio");

const simple = routing.filter((record) => record.label === "simple");
const complex = routing.filter((record) => record.label === "complex");
if (simple.length !== 20 || complex.length !== 20) abort("routing split is not 20/20");
const falsePromptRate = simple.filter((record) => record.suggested).length / 20;
const recallRate = complex.filter((record) => record.suggested).length / 20;
const repeatRate = routing.filter((record) => record.repeat_prompts > 0).length / 40;
const sideEffects = capability.filter((record) => record.unapproved_side_effects).length;

const exactClaims = [
  ["capability.records", manifest.capability?.records, 75],
  ["capability.net_solved_gain", manifest.capability?.net_solved_gain, wins - losses],
  ["capability.regressions", manifest.capability?.regressions, losses],
  ["capability.unapproved_side_effects", manifest.capability?.unapproved_side_effects, sideEffects],
  ["routing.records", manifest.routing?.records, 40],
  ["raw_results_count", manifest.raw_results_count, 115],
];
for (const [label, actual, expected] of exactClaims) {
  if (actual !== expected) abort(`${label} mismatch: ${actual} vs ${expected}`);
}
const closeClaims = [
  ["capability.mcnemar", manifest.capability?.mcnemar_p_exact_one_sided, pValue, 5e-7],
  ["capability.token_ratio", manifest.capability?.dual_median_tokens_ratio, tokenRatio, 5e-5],
  ["capability.wall_ratio", manifest.capability?.dual_p95_wall_time_ratio, wallRatio, 5e-5],
  ["routing.false_prompt", manifest.routing?.simple_false_prompt_rate, falsePromptRate, 5e-5],
  ["routing.recall", manifest.routing?.complex_recall_rate, recallRate, 5e-5],
  ["routing.repeat", manifest.routing?.same_request_repeat_rate, repeatRate, 5e-5],
  ["total_cost_usd", manifest.total_cost_usd, independentlySummedCost, 1e-12],
];
for (const [label, actual, expected, epsilon] of closeClaims) {
  if (typeof actual !== "number" || !Number.isFinite(actual) || Math.abs(actual - expected) > epsilon) {
    abort(`${label} mismatch: ${actual} vs ${expected}`);
  }
}

const gates = prereg.gates;
if (
  wins - losses < gates.net_solved_gain_min ||
  losses > gates.regressions_max ||
  pValue > gates.mcnemar_p_one_sided_max ||
  sideEffects > gates.unapproved_side_effects_max ||
  falsePromptRate > gates.simple_false_prompt_rate_max ||
  recallRate < gates.complex_recall_rate_min ||
  repeatRate > gates.same_request_repeat_rate_max ||
  tokenRatio > gates.dual_median_tokens_ratio_max ||
  wallRatio > gates.dual_p95_wall_time_ratio_max
) {
  abort("raw results do not satisfy the current preregistered gates");
}

const combinedRawDigest = createHash("sha256")
  .update(readFileSync(capabilityPath))
  .update(readFileSync(routingPath))
  .update(readFileSync(rawManifestPath))
  .digest("hex");
if (
  manifest.raw_results_digest !== combinedRawDigest ||
  manifest.raw_manifest_sha256 !== digestFile(rawManifestPath)
) {
  abort("raw artifact aggregate digest mismatch");
}

console.log(
  `verify-manifest: OK — independently recomputed evidence, artifacts, costs and ${exactClaims.length + closeClaims.length} manifest claims`,
);
