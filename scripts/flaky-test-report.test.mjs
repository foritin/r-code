import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  classifyAttempts,
  parseArguments,
  slugify,
} from "./flaky-test-report.mjs";

const scriptPath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "flaky-test-report.mjs",
);

test("slugify creates artifact-safe suite names", () => {
  assert.equal(slugify("Rust / Windows"), "rust-windows");
  assert.equal(slugify("***"), "suite");
});

test("parseArguments separates runner options from the test command", () => {
  const parsed = parseArguments([
    "--name",
    "frontend",
    "--attempts",
    "5",
    "--timeout-ms",
    "1000",
    "--",
    "npm",
    "test",
  ]);

  assert.equal(parsed.name, "frontend");
  assert.equal(parsed.attempts, 5);
  assert.equal(parsed.timeoutMs, 1000);
  assert.equal(parsed.command, "npm");
  assert.deepEqual(parsed.commandArgs, ["test"]);
});

test("parseArguments rejects missing commands and invalid attempt counts", () => {
  assert.throws(
    () => parseArguments(["--name", "frontend"]),
    /usage:/,
  );
  assert.throws(
    () =>
      parseArguments([
        "--name",
        "frontend",
        "--attempts",
        "0",
        "--",
        "npm",
        "test",
      ]),
    /positive integer/,
  );
});

test("classifyAttempts distinguishes stable, flaky and persistent failure", () => {
  assert.equal(
    classifyAttempts([{ exitCode: 0 }, { exitCode: 0 }]),
    "stable-pass",
  );
  assert.equal(
    classifyAttempts([{ exitCode: 1 }, { exitCode: 0 }]),
    "flaky-candidate",
  );
  assert.equal(
    classifyAttempts([{ exitCode: 1 }, { exitCode: 2 }]),
    "persistent-failure",
  );
});

test("runner writes a machine-readable stable-pass report", () => {
  const outputDirectory = mkdtempSync(
    path.join(os.tmpdir(), "r-code-flaky-report-"),
  );

  try {
    const result = spawnSync(
      process.execPath,
      [
        scriptPath,
        "--name",
        "Smoke test",
        "--attempts",
        "2",
        "--output-dir",
        outputDirectory,
        "--",
        process.execPath,
        "-e",
        "process.exit(0)",
      ],
      { encoding: "utf8" },
    );

    assert.equal(result.status, 0, result.stderr);
    const report = JSON.parse(
      readFileSync(path.join(outputDirectory, "smoke-test.json"), "utf8"),
    );
    assert.equal(report.status, "stable-pass");
    assert.equal(report.attempts.length, 2);
  } finally {
    rmSync(outputDirectory, { recursive: true, force: true });
  }
});

test("runner marks mixed outcomes as a flaky candidate", () => {
  const outputDirectory = mkdtempSync(
    path.join(os.tmpdir(), "r-code-flaky-report-"),
  );

  try {
    const result = spawnSync(
      process.execPath,
      [
        scriptPath,
        "--name",
        "Mixed smoke test",
        "--attempts",
        "2",
        "--output-dir",
        outputDirectory,
        "--",
        process.execPath,
        "-e",
        "process.exit(process.env.R_CODE_FLAKY_ATTEMPT === '1' ? 1 : 0)",
      ],
      { encoding: "utf8" },
    );

    assert.equal(result.status, 1);
    const report = JSON.parse(
      readFileSync(
        path.join(outputDirectory, "mixed-smoke-test.json"),
        "utf8",
      ),
    );
    assert.equal(report.status, "flaky-candidate");
    assert.deepEqual(
      report.attempts.map((attempt) => attempt.exitCode),
      [1, 0],
    );
  } finally {
    rmSync(outputDirectory, { recursive: true, force: true });
  }
});

test("runner records command launch failures as persistent failures", () => {
  const outputDirectory = mkdtempSync(
    path.join(os.tmpdir(), "r-code-flaky-report-"),
  );

  try {
    const result = spawnSync(
      process.execPath,
      [
        scriptPath,
        "--name",
        "Missing command",
        "--attempts",
        "1",
        "--output-dir",
        outputDirectory,
        "--",
        "r-code-command-that-does-not-exist",
      ],
      { encoding: "utf8" },
    );

    assert.equal(result.status, 1);
    const report = JSON.parse(
      readFileSync(
        path.join(outputDirectory, "missing-command.json"),
        "utf8",
      ),
    );
    assert.equal(report.status, "persistent-failure");
    assert.match(report.attempts[0].error, /ENOENT|not found/i);
  } finally {
    rmSync(outputDirectory, { recursive: true, force: true });
  }
});

test("runner terminates a timed-out process tree and records the timeout", () => {
  const outputDirectory = mkdtempSync(
    path.join(os.tmpdir(), "r-code-flaky-report-"),
  );

  try {
    const result = spawnSync(
      process.execPath,
      [
        scriptPath,
        "--name",
        "Timeout smoke test",
        "--attempts",
        "1",
        "--timeout-ms",
        "200",
        "--output-dir",
        outputDirectory,
        "--",
        process.execPath,
        "-e",
        "setInterval(() => {}, 1000)",
      ],
      { encoding: "utf8", timeout: 15_000 },
    );

    assert.equal(result.status, 1, result.stderr);
    assert.equal(result.error, undefined);
    const report = JSON.parse(
      readFileSync(
        path.join(outputDirectory, "timeout-smoke-test.json"),
        "utf8",
      ),
    );
    assert.equal(report.status, "persistent-failure");
    assert.equal(report.attempts[0].timedOut, true);
  } finally {
    rmSync(outputDirectory, { recursive: true, force: true });
  }
});
