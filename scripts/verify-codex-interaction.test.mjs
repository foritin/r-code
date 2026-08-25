// M0-01.A1：统一验证 Harness 的 runner 自测（node:test，零依赖）。
// 覆盖任务卡要求的三类硬失败（未知 task / 缺失 required 断言 / 失败子命令
// 均非 0 退出且报告列出准确失败 ID），外加报告脱敏与 fixture checker。

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import assert from "node:assert/strict";

import { REGISTRY, MILESTONES, tasksForMilestone, validateRegistry } from "./codex-interaction/registry.mjs";
import { runVerification, redact } from "./codex-interaction/runner.mjs";
import { checkProtocolFixture } from "./codex-interaction/check-protocol-fixture.mjs";
import { validateAgainstSchema } from "./codex-interaction/schema-mini-validate.mjs";

const rootDir = path.resolve(import.meta.dirname, "..");

const EXPECTED_TASKS = {
  "M0-01": ["M0-01.A1", "M0-01.A2", "M0-01.A3"],
  "M0-02": ["M0-02.A1", "M0-02.A2", "M0-02.A3", "M0-02.A4"],
  "M1-01": ["M1-01.A1", "M1-01.A2", "M1-01.A3"],
  "M1-02": ["M1-02.A1", "M1-02.A2", "M1-02.A3"],
  "M1-03": ["M1-03.A1", "M1-03.A2", "M1-03.A3"],
  "M2-01": ["M2-01.A1", "M2-01.A2", "M2-01.A3"],
  "M2-02": ["M2-02.A1", "M2-02.A2", "M2-02.A3"],
  "M3-01": ["M3-01.A1", "M3-01.A2", "M3-01.A3"],
  "M3-02": ["M3-02.A1", "M3-02.A2", "M3-02.A3"],
  "M3-03": ["M3-03.A1", "M3-03.A2", "M3-03.A3"],
  "M4-01": ["M4-01.A1", "M4-01.A2", "M4-01.A3", "M4-01.A4"],
  "M4-02": ["M4-02.A1", "M4-02.A2", "M4-02.A3", "M4-02.A4"],
};

function stubRegistry(assertions) {
  return {
    "STUB-01": {
      milestone: "selftest",
      depends_on: [],
      assertions,
    },
  };
}

test("registry covers all 12 PRD tasks with exact assertion ids", () => {
  const issues = validateRegistry();
  assert.deepEqual(issues, []);
  assert.deepEqual(
    Object.keys(EXPECTED_TASKS).every((taskId) => REGISTRY[taskId]),
    true,
    "product tasks missing",
  );
  for (const [taskId, assertionIds] of Object.entries(EXPECTED_TASKS)) {
    assert.deepEqual(
      REGISTRY[taskId].assertions.map((a) => a.id),
      assertionIds,
      `${taskId} assertions mismatch`,
    );
  }
  assert.deepEqual(tasksForMilestone("M0"), ["M0-01", "M0-02"]);
  assert.deepEqual(tasksForMilestone("M4"), ["M4-01", "M4-02"]);
  assert.equal(new Set(MILESTONES).size, 5);
});

test("registry flags missing required assertions and bad dependencies", () => {
  const broken = {
    "STUB-01": { milestone: "selftest", depends_on: ["STUB-02"], assertions: [] },
  };
  const issues = validateRegistry(broken);
  assert.ok(issues.some((issue) => issue.includes("no assertions")));
  assert.ok(issues.some((issue) => issue.includes("unknown dependency target: STUB-02")));

  const cyclic = {
    "STUB-01": { milestone: "selftest", depends_on: ["STUB-02"], assertions: [{ id: "STUB-01.A1", profiles: ["implementation"] }] },
    "STUB-02": { milestone: "selftest", depends_on: ["STUB-01"], assertions: [{ id: "STUB-02.A1", profiles: ["implementation"] }] },
  };
  assert.ok(validateRegistry(cyclic).some((issue) => issue.includes("cycle")));
});

test("failing subcommand returns exit 1 and report lists the exact assertion id", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "codex-verify-fail-"));
  try {
    const { exitCode, failedIds, reportPath } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([
        {
          id: "STUB-01.A1",
          level: "contract",
          command: [process.execPath, "-e", "process.exit(3)"],
          timeout_ms: 30_000,
          profiles: ["implementation"],
        },
      ]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    assert.equal(exitCode, 1);
    assert.deepEqual(failedIds, ["STUB-01.A1"]);
    const report = JSON.parse(await readFile(reportPath, "utf8"));
    assert.equal(report.summary.failed, 1);
    assert.equal(report.assertions[0].exit_code, 3);
    assert.equal(report.assertions[0].status, "failed");
    assert.match(path.basename(reportPath), /^STUB-01\.json$/);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("not_implemented assertions fail loudly instead of being skipped", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "codex-verify-ni-"));
  try {
    const { exitCode, report } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([{ id: "STUB-01.A1", level: "contract", not_implemented: true, profiles: ["implementation"] }]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    assert.equal(exitCode, 1);
    assert.equal(report.summary.not_implemented, 1);
    assert.equal(report.assertions[0].status, "not_implemented");
    assert.match(report.assertions[0].reason, /尚未实施/);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("production profile skips implementation-only assertions and vice versa", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "codex-verify-prof-"));
  try {
    const { report } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([
        {
          id: "STUB-01.A1",
          level: "contract",
          command: [process.execPath, "-e", ""],
          timeout_ms: 30_000,
          profiles: ["implementation", "production"],
        },
        {
          id: "STUB-01.A2",
          level: "contract",
          not_implemented: true,
          profiles: ["production"],
          external: true,
        },
      ]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    assert.deepEqual(
      report.assertions.map((a) => a.id),
      ["STUB-01.A1"],
      "implementation profile must not run production-only assertions",
    );
    assert.equal(report.exit_code, 0);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("report digests stdout instead of storing it, and redacts failure excerpts", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "codex-verify-redact-"));
  try {
    const secret = "sk-abcdef123456";
    const { report } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([
        {
          id: "STUB-01.A1",
          level: "contract",
          command: [process.execPath, "-e", `console.error("${secret} leaked"); process.exit(1)`],
          timeout_ms: 30_000,
          profiles: ["implementation"],
        },
      ]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    const reportText = JSON.stringify(report);
    assert.ok(!reportText.includes(secret), "secret must not appear anywhere in the report");
    assert.match(report.assertions[0].failure_excerpt, /\[REDACTED:token\]/);
    assert.equal(redact(`token=${secret}`).includes(secret), false);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("report includes revision, fixture schema version and evidence index", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "codex-verify-meta-"));
  try {
    const { report } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([
        { id: "STUB-01.A1", level: "contract", command: [process.execPath, "-e", ""], timeout_ms: 30_000, profiles: ["implementation"] },
      ]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    assert.equal(report.schema_version, "codex-interaction-verification.v1");
    assert.match(report.fixture_schema_version, /^0\.145\.0$/);
    assert.ok(report.revision.git_revision, "git revision recorded");
    assert.ok(report.evidence_index.task_packet.endsWith("current.yaml"));
    assert.ok(report.platform.os.length > 0);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("cli rejects unknown task / bad profile / conflicting flags with exit 2", () => {
  const cli = path.join(rootDir, "scripts", "verify-codex-interaction.mjs");
  const unknown = spawnSync(process.execPath, [cli, "--task", "M9-99"], { encoding: "utf8", cwd: rootDir });
  assert.equal(unknown.status, 2);
  assert.match(unknown.stderr, /unknown task id: M9-99/);

  const badProfile = spawnSync(process.execPath, [cli, "--task", "M0-01", "--profile", "staging"], { encoding: "utf8", cwd: rootDir });
  assert.equal(badProfile.status, 2);
  assert.match(badProfile.stderr, /--profile must be implementation\|production/);

  const conflicting = spawnSync(process.execPath, [cli, "--task", "M0-01", "--through", "M0"], { encoding: "utf8", cwd: rootDir });
  assert.equal(conflicting.status, 2);
  assert.match(conflicting.stderr, /mutually exclusive/);

  const badMilestone = spawnSync(process.execPath, [cli, "--through", "M9"], { encoding: "utf8", cwd: rootDir });
  assert.equal(badMilestone.status, 2);
  assert.match(badMilestone.stderr, /unknown milestone: M9/);
});

test("cli --list dumps the registry without running anything", () => {
  const cli = path.join(rootDir, "scripts", "verify-codex-interaction.mjs");
  const listed = spawnSync(process.execPath, [cli, "--list"], { encoding: "utf8", cwd: rootDir });
  assert.equal(listed.status, 0);
  // 只断言与实现进度无关的结构：12 个任务 × 里程碑归属 × 断言数。
  const lines = listed.stdout.trim().split("\n");
  assert.equal(lines.length, 13, "12 product tasks + smoke task");
  const m002 = lines.find((line) => line.startsWith("M0-02\t"));
  assert.ok(m002.startsWith("M0-02\tM0\t"), "M0-02 belongs to milestone M0");
  assert.ok(/\t4 assertions$/.test(m002), "M0-02 has 4 assertions");
  const smoke = lines.find((line) => line.startsWith("M0-01-smoke\t"));
  assert.ok(smoke.startsWith("M0-01-smoke\tselftest\t"), "smoke task stays out of product milestones");
});

test("committed protocol fixture passes the offline checker", async () => {
  const fixturePath = path.join(rootDir, "fixtures", "codex-interaction", "protocol-0.145.0.json");
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
  assert.deepEqual(checkProtocolFixture(fixture, "protocol-0.145.0.json"), []);
});

test("fixture checker catches drift and credentials", async () => {
  const fixturePath = path.join(rootDir, "fixtures", "codex-interaction", "protocol-0.145.0.json");
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"));

  const drifted = structuredClone(fixture);
  const params = drifted.server_requests["item/tool/requestUserInput"].params_schema;
  params.required = ["threadId"];
  drifted.server_requests["item/tool/requestUserInput"].params_schema.properties.questions.items.required = ["id"];
  const issues = checkProtocolFixture(drifted, "protocol-0.145.0.json");
  assert.ok(issues.some((issue) => issue.includes("params required fields")), "required-field drift detected");
  assert.ok(issues.some((issue) => issue.includes("question required fields")));

  const badSample = structuredClone(fixture);
  badSample.sample_frames.agent_message_delta.frame.params.delta = 42;
  assert.ok(
    checkProtocolFixture(badSample, "protocol-0.145.0.json").some((issue) => issue.includes("agent_message_delta violates")),
  );

  const leaked = structuredClone(fixture);
  leaked.sample_frames.warning.frame.params.message = "token sk-abcdef123456 expired";
  assert.ok(checkProtocolFixture(leaked, "protocol-0.145.0.json").some((issue) => issue.includes("credential-like")));

  const renamed = structuredClone(fixture);
  assert.ok(checkProtocolFixture(renamed, "protocol-0.146.0.json").some((issue) => issue.includes("filename version")));
});

test("mini schema validator enforces required/type/enum without silently passing unknowns", () => {
  const schema = {
    type: "object",
    required: ["id"],
    properties: { id: { type: "string" }, n: { type: ["integer", "null"], minimum: 0 } },
    additionalProperties: false,
  };
  assert.deepEqual(validateAgainstSchema({ id: "a", n: null }, schema), []);
  assert.ok(validateAgainstSchema({ id: "a", extra: 1 }, schema).some((e) => e.includes("unexpected property")));
  assert.ok(validateAgainstSchema({ n: 1 }, schema).some((e) => e.includes('missing required property "id"')));
  assert.ok(validateAgainstSchema({ id: "a", n: -1 }, schema).some((e) => e.includes("< minimum")));
  assert.ok(validateAgainstSchema({ id: "a" }, { ...schema, properties: { id: { $ref: "#/definitions/X" } } }).some((e) => e.includes("unresolved $ref")));
  assert.deepEqual(
    validateAgainstSchema("x", { oneOf: [{ type: "string" }, { type: "string", enum: ["x"] }] }),
    ["$: matched 2 oneOf branches (expected exactly 1)"],
  );
});
