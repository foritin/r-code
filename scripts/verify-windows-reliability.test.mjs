// M0-01.A1：统一验证 Harness 的 runner 自测（node:test，零依赖）。
// 覆盖任务卡要求的硬失败语义（未知 task / 失败子命令 / registry 缺失 required
// 断言均非 0 退出且报告列出准确失败 ID），外加报告脱敏与金集 schema 校验器。

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import assert from "node:assert/strict";

import { REGISTRY, MILESTONES, tasksForMilestone, tasksThroughMilestone, validateRegistry } from "./windows-reliability/registry.mjs";
import { runVerification, redact } from "./windows-reliability/runner.mjs";
import { validateCorpusFile } from "./windows-reliability/corpus-schema.mjs";

const rootDir = path.resolve(import.meta.dirname, "..");

const EXPECTED_TASKS = {
  "M0-01": ["M0-01.A1", "M0-01.A2", "M0-01.A3"],
  "M0-02": ["M0-02.A1", "M0-02.A2", "M0-02.A3"],
  "M1-01": ["M1-01.A1", "M1-01.A2", "M1-01.A3"],
  "M1-02": ["M1-02.A1", "M1-02.A2", "M1-02.A3", "M1-02.A4"],
  "M2-01": ["M2-01.A1", "M2-01.A2", "M2-01.A3", "M2-01.A4"],
  "M2-02": ["M2-02.A1", "M2-02.A2", "M2-02.A3", "M2-02.A4"],
  "M3-01": ["M3-01.A1", "M3-01.A2", "M3-01.A3"],
  "M3-02": ["M3-02.A1", "M3-02.A2", "M3-02.A3"],
  "M4-01": ["M4-01.A1", "M4-01.A2", "M4-01.A3"],
  "M4-02": ["M4-02.A1", "M4-02.A2", "M4-02.A3", "M4-02.A4"],
  "M4-03": ["M4-03.A1", "M4-03.A2", "M4-03.A3"],
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

test("registry covers all 11 PRD tasks with exact assertion ids", () => {
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
  assert.deepEqual(tasksForMilestone("M4"), ["M4-01", "M4-02", "M4-03"]);
  assert.deepEqual(tasksThroughMilestone("M2"), [
    "M0-01",
    "M0-02",
    "M1-01",
    "M1-02",
    "M2-01",
    "M2-02",
  ]);
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
    "STUB-01": { milestone: "selftest", depends_on: ["STUB-02"], assertions: [{ id: "STUB-01.A1", command: ["node", "-e", ""], profiles: ["implementation"] }] },
    "STUB-02": { milestone: "selftest", depends_on: ["STUB-01"], assertions: [{ id: "STUB-02.A1", command: ["node", "-e", ""], profiles: ["implementation"] }] },
  };
  assert.ok(validateRegistry(cyclic).some((issue) => issue.includes("cycle")));
});

test("failing subcommand returns exit 1 and report lists the exact assertion id", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-fail-"));
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
        {
          id: "STUB-01.A2",
          level: "contract",
          command: [process.execPath, "-e", ""],
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
    assert.equal(report.assertions.find((a) => a.id === "STUB-01.A1").exit_code, 3);
    assert.equal(report.assertions.find((a) => a.id === "STUB-01.A1").status, "failed");
    assert.equal(report.assertions.find((a) => a.id === "STUB-01.A2").status, "passed");
    assert.match(path.basename(reportPath), /^STUB-01\.json$/);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("not_implemented assertions fail loudly instead of being skipped", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-ni-"));
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
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-prof-"));
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

test("assertion env vars are merged into the child environment", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-env-"));
  try {
    const { report } = await runVerification({
      mode: { kind: "task", id: "STUB-01" },
      registry: stubRegistry([
        {
          id: "STUB-01.A1",
          level: "contract",
          command: [process.execPath, "-e", "process.exit(process.env.WINREL_ENV_PROBE === '42' ? 0 : 9)"],
          timeout_ms: 30_000,
          profiles: ["implementation"],
          env: { WINREL_ENV_PROBE: "42" },
        },
      ]),
      rootDir,
      reportRoot,
      profile: "implementation",
      stdout: { write() {} },
    });
    assert.equal(report.assertions[0].status, "passed", JSON.stringify(report.assertions[0]));
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("report digests stdout instead of storing it, and redacts failure excerpts", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-redact-"));
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

test("report includes revision, corpus schema version and evidence index", async () => {
  const reportRoot = await mkdtemp(path.join(tmpdir(), "winrel-verify-meta-"));
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
    assert.equal(report.schema_version, "windows-reliability-verification.v1");
    assert.equal(report.corpus_schema_version, "command-corpus.v1");
    assert.ok(report.revision.git_revision, "git revision recorded");
    assert.ok(report.evidence_index.task_packet.endsWith("current.yaml"));
    assert.ok(report.evidence_index.evidence_dir.includes("windows-reliability"));
    assert.ok(report.platform.os.length > 0);
  } finally {
    await rm(reportRoot, { recursive: true, force: true });
  }
});

test("cli rejects unknown task / bad profile / conflicting flags with exit 2", () => {
  const cli = path.join(rootDir, "scripts", "verify-windows-reliability.mjs");
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
  const cli = path.join(rootDir, "scripts", "verify-windows-reliability.mjs");
  const listed = spawnSync(process.execPath, [cli, "--list"], { encoding: "utf8", cwd: rootDir });
  assert.equal(listed.status, 0);
  const lines = listed.stdout.trim().split("\n");
  assert.equal(lines.length, 11, "11 product tasks");
  const m002 = lines.find((line) => line.startsWith("M0-02\t"));
  assert.ok(m002.startsWith("M0-02\tM0\t"), "M0-02 belongs to milestone M0");
  assert.ok(/\t3 assertions$/.test(m002), "M0-02 has 3 assertions");
});

test("corpus schema validator accepts the committed corpus and rejects drift", async () => {
  const corpusPath = path.join(rootDir, "crates", "r-code-gateway", "tests", "command_corpus", "corpus.jsonl");
  const { issues, entryCount } = await validateCorpusFile(corpusPath);
  assert.deepEqual(issues, []);
  assert.ok(entryCount >= 40, `corpus has ${entryCount} entries`);

  const corrupt = path.join(await mkdtemp(path.join(tmpdir(), "winrel-corpus-")), "corpus.jsonl");
  const { writeFile } = await import("node:fs/promises");
  const original = await readFile(corpusPath, "utf8");
  const lines = original.split(/\r?\n/).filter((line) => line.trim().length > 0);

  const duplicated = [...lines, lines[0]].join("\n");
  await writeFile(corrupt, duplicated, "utf8");
  assert.ok((await validateCorpusFile(corrupt)).issues.some((issue) => issue.includes("重复 id")));

  const badEnum = lines[0].replace('"windows"', '"win95"');
  await writeFile(corrupt, badEnum, "utf8");
  assert.ok((await validateCorpusFile(corrupt)).issues.some((issue) => issue.includes("platform 必须是")));

  const extraField = lines[0].replace(/\}$/, ',"extra":1}');
  await writeFile(corrupt, extraField, "utf8");
  assert.ok((await validateCorpusFile(corrupt)).issues.some((issue) => issue.includes("多余字段")));

  // 抽掉某一类全部条目 → 触发类别下限。
  const policyEntries = lines.filter((line) => line.includes('"category":"policy"'));
  const withoutPolicy = lines.filter((line) => !line.includes('"category":"policy"')).join("\n");
  await writeFile(corrupt, withoutPolicy, "utf8");
  assert.ok(
    (await validateCorpusFile(corrupt)).issues.some(
      (issue) => issue.includes("policy") && issue.includes("低于下限"),
    ),
    `policy had ${policyEntries.length} entries; removing them must trip the minimum`,
  );

  await rm(path.dirname(corrupt), { recursive: true, force: true });
});
