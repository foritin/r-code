import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const evalRoot = join(import.meta.dirname, "..");
const scoreScript = join(import.meta.dirname, "score.mjs");
const verifyScript = join(import.meta.dirname, "verify-manifest.mjs");
const sha = (value) => createHash("sha256").update(value).digest("hex");
const fileSha = (path) => sha(readFileSync(path));
const canonical = (value) => {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
};

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function writeJsonl(path, values) {
  writeFileSync(path, `${values.map((value) => JSON.stringify(value)).join("\n")}\n`);
}

function run(script, root) {
  return spawnSync(process.execPath, [script, "--root", root], {
    encoding: "utf8",
    windowsHide: true,
  });
}

function usageFor(kind) {
  const inputTokens = kind === "plan_dual_track" ? 90 : 80;
  return {
    input_tokens: inputTokens,
    output_tokens: 20,
    cache_read_tokens: 10,
    cache_write_tokens: inputTokens - 10,
    stream_retries: 0,
  };
}

function costFor(usage, pricing) {
  return (
    ((usage.input_tokens - usage.cache_read_tokens) * pricing.input_usd_per_million +
      usage.cache_read_tokens * pricing.cache_read_usd_per_million +
      usage.output_tokens * pricing.output_usd_per_million) /
    1_000_000
  );
}

function commonEvidence({
  id,
  artifactUri,
  usage,
  pricing,
  configHash,
  profileHash,
  preregHash,
  corpusHash,
  commit,
  seed,
  orderKind,
  orderIdentity,
  orderIndex,
  environment,
}) {
  const requestId = `request-${id}`;
  const operationId = `operation-${id}`;
  const runId = `run-${id}`;
  const requestHeaders = [
    {
      journal_id: runId,
      request_header: {
        system_sha256: sha(`system-${id}`),
        tools_sha256: sha(`tools-${id}`),
        messages_sha256: sha(`messages-${id}`),
        reason: "initial",
        excluded_tails: [],
        tool_names: ["read_file"],
        hosted_tool_names: [],
        max_tokens: 4096,
      },
    },
  ];
  const origins = [
    {
      request_key: requestId,
      operation_id: operationId,
      kind: "direct",
      parent_request_key: null,
      created_at: "2026-08-21T00:00:00Z",
    },
  ];
  const runUsage = [{ run_id: runId, ...usage }];
  const requestAuditSha = sha(JSON.stringify(requestHeaders));
  const costUsd = costFor(usage, pricing);
  return {
    requestId,
    operationId,
    runId,
    requestHeaders,
    origins,
    runUsage,
    requestAuditSha,
    costUsd,
    raw: {
      request_id: requestId,
      request_ids: [requestId],
      operation_id: operationId,
      operation_ids: [operationId],
      run_ids: [runId],
      provider_kind: "deepseek",
      resolved_model: "deepseek-v4-flash",
      wire_protocol: "openai_chat",
      endpoint_class: "official_api",
      config_sha256: configHash,
      profile_sha256: profileHash,
      preregistration_sha256: preregHash,
      corpus_lock_sha256: corpusHash,
      run_seed_sha256: sha(seed),
      order_index: orderIndex,
      order_key_sha256: sha(`${seed}\0${orderKind}\0${orderIdentity}`),
      dry_run: false,
      input_tokens: usage.input_tokens,
      output_tokens: usage.output_tokens,
      cache_read_tokens: usage.cache_read_tokens,
      cache_write_tokens: usage.cache_write_tokens,
      total_tokens: usage.input_tokens + usage.output_tokens,
      rounds: requestHeaders.length,
      wall_time_ms: orderKind === "capability" && orderIdentity.endsWith("plan_dual_track") ? 110 : 100,
      cost_usd: costUsd,
      retry_count: 0,
      retry_reasons: [],
      request_audit_sha256: requestAuditSha,
      request_audit_mismatches: 0,
      artifact_uri: artifactUri,
      environment_fingerprint: environment,
      commit,
      recorded_at: "2026-08-21T00:00:00Z",
    },
  };
}

function buildEvidenceRoot() {
  const root = mkdtempSync(join(tmpdir(), "r-code-plan-evidence-test-"));
  const schemaDir = join(root, "schema");
  const artifacts = join(root, "artifacts");
  mkdirSync(schemaDir, { recursive: true });
  mkdirSync(artifacts, { recursive: true });
  copyFileSync(join(evalRoot, "schema", "preregistration.json"), join(schemaDir, "preregistration.json"));
  copyFileSync(join(evalRoot, "corpus-lock.json"), join(root, "corpus-lock.json"));
  const preregHash = fileSha(join(schemaDir, "preregistration.json"));
  const corpusHash = fileSha(join(root, "corpus-lock.json"));
  const seed = "synthetic-seed-v1";
  const commit = "a".repeat(40);
  const pricing = {
    currency: "USD",
    unit: "per_million_tokens",
    input_usd_per_million: 1,
    cache_read_usd_per_million: 0.2,
    output_usd_per_million: 2,
  };
  const capabilityConfig = sha("capability-config");
  const routingConfig = sha("routing-config");
  const arms = ["direct_agent", "plan_baseline", "plan_dual_track"];
  const capabilityJobs = [];
  for (let caseIndex = 0; caseIndex < 25; caseIndex += 1) {
    const caseId = `case-${String(caseIndex + 1).padStart(2, "0")}`;
    for (const arm of arms) {
      const key = `${caseId}:${arm}`;
      capabilityJobs.push({ caseId, caseIndex, arm, key, order: sha(`${seed}\0capability\0${key}`) });
    }
  }
  capabilityJobs.sort((left, right) => left.order.localeCompare(right.order) || left.key.localeCompare(right.key));
  const capability = capabilityJobs.map((job, orderIndex) => {
    const profile = {
      direct_agent: ["direct_agent", false, "off"],
      plan_baseline: ["baseline", false, "off"],
      plan_dual_track: ["plan_native_v1", true, "open"],
    }[job.arm];
    const fixtureHash = sha(`fixture-${job.caseId}`);
    const finalHash = sha(`final-${job.caseId}-${job.arm}`);
    const profileHash = sha(`profile-${job.arm}`);
    const artifactUri = `raw/capability/${job.caseId}/${job.arm}.json`;
    const common = commonEvidence({
      id: `${job.caseId}-${job.arm}`,
      artifactUri,
      usage: usageFor(job.arm),
      pricing,
      configHash: capabilityConfig,
      profileHash,
      preregHash,
      corpusHash,
      commit,
      seed,
      orderKind: "capability",
      orderIdentity: job.key,
      orderIndex,
      environment: `cap-env-${job.caseId}-${job.arm}`,
    });
    const hashes = {
      fixture_sha256: fixtureHash,
      initial_workspace_sha256: fixtureHash,
      preapproval_workspace_sha256: fixtureHash,
      final_workspace_sha256: finalHash,
      diff_digest: sha(`diff-${fixtureHash}-${finalHash}`),
      config_sha256: capabilityConfig,
      profile_sha256: profileHash,
      preregistration_sha256: preregHash,
      corpus_lock_sha256: corpusHash,
    };
    const artifact = {
      schema: "r-code-plan-capability-artifact/v1",
      case_id: job.caseId,
      arm: job.arm,
      task_id: `task-${job.caseId}-${job.arm}`,
      origins: common.origins,
      run_usage: common.runUsage,
      request_headers: common.requestHeaders,
      request_audit_sha256: common.requestAuditSha,
      request_audit_mismatches: 0,
      hashes,
      usage: usageFor(job.arm),
      cost_usd: common.costUsd,
      recorded_at: "2026-08-21T00:00:00Z",
    };
    const artifactPath = join(artifacts, ...artifactUri.split("/"));
    mkdirSync(join(artifactPath, ".."), { recursive: true });
    writeJson(artifactPath, artifact);
    return {
      case_id: job.caseId,
      category: ["bugfix", "feature", "migration", "performance", "safety"][
        Math.floor(job.caseIndex / 5)
      ],
      arm: job.arm,
      release_state: profile[2],
      profile_kind: profile[0],
      profile_enabled: profile[1],
      fixture_sha256: fixtureHash,
      initial_workspace_sha256: fixtureHash,
      preapproval_workspace_sha256: fixtureHash,
      final_workspace_sha256: finalHash,
      diff_digest: hashes.diff_digest,
      tests_passed: job.arm === "plan_dual_track" && job.caseIndex < 6,
      unapproved_side_effects: false,
      artifact_sha256: fileSha(artifactPath),
      ...common.raw,
    };
  });

  const routingJobs = Array.from({ length: 40 }, (_, index) => {
    const id = `${index < 20 ? "simple" : "complex"}-${String((index % 20) + 1).padStart(2, "0")}`;
    return { id, label: index < 20 ? "simple" : "complex", ordinal: index % 20, order: sha(`${seed}\0routing\0${id}`) };
  }).sort((left, right) => left.order.localeCompare(right.order) || left.id.localeCompare(right.id));
  const routing = routingJobs.map((job, orderIndex) => {
    const profileHash = sha("routing-profile");
    const treeHash = sha("empty-routing-tree");
    const artifactUri = `raw/routing/${job.id}.json`;
    const usage = usageFor("routing");
    const common = commonEvidence({
      id: `routing-${job.id}`,
      artifactUri,
      usage,
      pricing,
      configHash: routingConfig,
      profileHash,
      preregHash,
      corpusHash,
      commit,
      seed,
      orderKind: "routing",
      orderIdentity: job.id,
      orderIndex,
      environment: `routing-env-${job.id}`,
    });
    const hashes = {
      initial_workspace_sha256: treeHash,
      final_workspace_sha256: treeHash,
      config_sha256: routingConfig,
      profile_sha256: profileHash,
      preregistration_sha256: preregHash,
      corpus_lock_sha256: corpusHash,
    };
    const artifact = {
      schema: "r-code-plan-routing-artifact/v1",
      id: job.id,
      label: job.label,
      task_id: `task-routing-${job.id}`,
      origins: common.origins,
      run_usage: common.runUsage,
      request_headers: common.requestHeaders,
      request_audit_sha256: common.requestAuditSha,
      request_audit_mismatches: 0,
      hashes,
      usage,
      cost_usd: common.costUsd,
      recorded_at: "2026-08-21T00:00:00Z",
    };
    const artifactPath = join(artifacts, ...artifactUri.split("/"));
    mkdirSync(join(artifactPath, ".."), { recursive: true });
    writeJson(artifactPath, artifact);
    return {
      id: job.id,
      label: job.label,
      release_state: "open",
      profile_kind: "routing_experiment",
      profile_enabled: true,
      initial_workspace_sha256: treeHash,
      final_workspace_sha256: treeHash,
      routing_side_effects: false,
      suggested: job.label === "simple" ? job.ordinal === 0 : job.ordinal < 18,
      repeat_prompts: 0,
      artifact_sha256: fileSha(artifactPath),
      ...common.raw,
    };
  });

  const capabilityPath = join(artifacts, "raw-capability.jsonl");
  const routingPath = join(artifacts, "raw-routing.jsonl");
  writeJsonl(capabilityPath, capability);
  writeJsonl(routingPath, routing);
  const identity = {
    evidence_version: "synthetic-evidence-v1",
    run_seed: seed,
    commit,
    preregistration_sha256: preregHash,
    corpus_lock_sha256: corpusHash,
    pricing,
    resolved_model: "deepseek-v4-flash",
    wire_protocol: "openai_chat",
    endpoint_class: "official_api",
    base_url_sha256: sha("https://api.deepseek.com"),
    allowed_models: ["deepseek-v4-flash", "deepseek-v4-pro"],
    allowed_protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    allowed_endpoint_classes: ["official_api"],
  };
  const rawManifest = {
    schema: "r-code-plan-raw-manifest/v1",
    status: "complete",
    identity_sha256: sha(canonical(identity)),
    evidence_version: identity.evidence_version,
    run_seed: seed,
    run_seed_sha256: sha(seed),
    commit,
    preregistration_sha256: preregHash,
    corpus_lock_sha256: corpusHash,
    pricing,
    resolved_model: identity.resolved_model,
    wire_protocol: identity.wire_protocol,
    endpoint_class: identity.endpoint_class,
    base_url_sha256: identity.base_url_sha256,
    allowed_models: identity.allowed_models,
    allowed_protocols: identity.allowed_protocols,
    allowed_endpoint_classes: identity.allowed_endpoint_classes,
    capability: { path: "raw-capability.jsonl", records: 75, sha256: fileSha(capabilityPath) },
    routing: { path: "raw-routing.jsonl", records: 40, sha256: fileSha(routingPath) },
    artifacts_root: "raw",
    updated_at: "2026-08-21T00:00:00Z",
  };
  writeJson(join(artifacts, "raw-manifest.json"), rawManifest);
  return root;
}

function rewriteCapability(root, mutate) {
  const artifacts = join(root, "artifacts");
  const path = join(artifacts, "raw-capability.jsonl");
  const records = readFileSync(path, "utf8")
    .trim()
    .split(/\r?\n/)
    .map(JSON.parse);
  mutate(records);
  writeJsonl(path, records);
  const rawManifestPath = join(artifacts, "raw-manifest.json");
  const rawManifest = JSON.parse(readFileSync(rawManifestPath, "utf8"));
  rawManifest.capability.sha256 = fileSha(path);
  writeJson(rawManifestPath, rawManifest);
}

test("score and independent verifier accept a complete internally consistent evidence tree", () => {
  const root = buildEvidenceRoot();
  try {
    const scored = run(scoreScript, root);
    assert.equal(scored.status, 0, scored.stderr);
    const verified = run(verifyScript, root);
    assert.equal(verified.status, 0, verified.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("score rejects zero-token evidence and removes a stale manifest", () => {
  const root = buildEvidenceRoot();
  try {
    assert.equal(run(scoreScript, root).status, 0);
    rewriteCapability(root, (records) => {
      records[0].input_tokens = 0;
      records[0].output_tokens = 0;
      records[0].total_tokens = 0;
    });
    const result = run(scoreScript, root);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /input_tokens|total_tokens/);
    assert.equal(existsSync(join(root, "artifacts", "manifest.json")), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("score rejects an unavailable or tampered raw artifact", () => {
  const root = buildEvidenceRoot();
  try {
    const record = JSON.parse(
      readFileSync(join(root, "artifacts", "raw-capability.jsonl"), "utf8").split(/\r?\n/)[0],
    );
    writeFileSync(join(root, "artifacts", ...record.artifact_uri.split("/")), "{}\n");
    const result = run(scoreScript, root);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /artifact digest mismatch/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("score rejects profile contamination even when raw file descriptors are updated", () => {
  const root = buildEvidenceRoot();
  try {
    rewriteCapability(root, (records) => {
      const baseline = records.find((record) => record.arm === "plan_baseline");
      baseline.profile_kind = "plan_native_v1";
      baseline.profile_enabled = true;
      baseline.release_state = "open";
    });
    const result = run(scoreScript, root);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /profile\/release isolation mismatch/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("independent verifier recomputes token ratio and frozen corpus hashes", () => {
  const root = buildEvidenceRoot();
  try {
    assert.equal(run(scoreScript, root).status, 0);
    const manifestPath = join(root, "artifacts", "manifest.json");
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    manifest.capability.dual_median_tokens_ratio = 1.0;
    writeJson(manifestPath, manifest);
    const ratioResult = run(verifyScript, root);
    assert.notEqual(ratioResult.status, 0);
    assert.match(ratioResult.stderr, /token_ratio mismatch/);

    assert.equal(run(scoreScript, root).status, 0);
    writeFileSync(join(root, "corpus-lock.json"), "{}\n");
    const hashResult = run(verifyScript, root);
    assert.notEqual(hashResult.status, 0);
    assert.match(hashResult.stderr, /corpus lock digest mismatch/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
