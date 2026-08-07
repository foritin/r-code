#!/usr/bin/env node

import { readFileSync } from "node:fs";

const REQUIRED_CI_JOBS = Object.freeze([
  "Frontend and Release Metadata",
  "Frontend dependency audit",
  "Secret scanning",
  "Format Check",
  "Clippy",
  "Test (ubuntu-latest)",
  "Test (macos-latest)",
  "Test (windows-latest)",
  "Security Audit",
  "Dependency Deny",
  "Submodule Pin Check",
]);

class ReleaseQualityGateError extends Error {}

function fail(message) {
  throw new ReleaseQualityGateError(message);
}

function runsFrom(payload) {
  if (Array.isArray(payload)) return payload;
  if (Array.isArray(payload?.workflow_runs)) return payload.workflow_runs;
  fail("CI runs payload has no workflow_runs array");
}

function jobsFrom(payload) {
  if (Array.isArray(payload)) return payload;
  if (Array.isArray(payload?.jobs)) return payload.jobs;
  fail("CI jobs payload has no jobs array");
}

function requireNonEmpty(value, name) {
  if (typeof value !== "string" || value.trim() === "") {
    fail(`${name} must be a non-empty string`);
  }
}

function newestFirst(left, right) {
  return String(right.updated_at ?? right.created_at ?? "")
    .localeCompare(String(left.updated_at ?? left.created_at ?? ""));
}

/**
 * Select a successful CI push run for the exact commit that a release tag peels
 * to. A tag-triggered release must never rely on a same-named CI run for a
 * different branch or commit.
 */
function findSuccessfulCiRun(payload, { tagCommit, defaultBranch }) {
  requireNonEmpty(tagCommit, "tagCommit");
  requireNonEmpty(defaultBranch, "defaultBranch");

  const candidates = runsFrom(payload)
    .filter((run) => run?.name === "CI"
      && run.event === "push"
      && run.head_sha === tagCommit
      && run.head_branch === defaultBranch
      && run.status === "completed"
      && run.conclusion === "success")
    .sort(newestFirst);
  const selected = candidates[0];
  if (!selected?.id) {
    fail(
      `no successful CI push run exists for ${defaultBranch} commit ${tagCommit}; `
      + "run the complete CI workflow for the tagged commit before releasing",
    );
  }
  return selected;
}

/**
 * A successful workflow conclusion alone is insufficient if a future workflow
 * edit accidentally skips a release-critical job. Require every expected job
 * from this repository's CI contract to have completed successfully.
 */
function validateRequiredCiJobs(payload, requiredJobs = REQUIRED_CI_JOBS) {
  const jobs = jobsFrom(payload);
  const failures = [];

  for (const requiredName of requiredJobs) {
    const matching = jobs.filter((job) => job?.name === requiredName);
    if (matching.length === 0) {
      failures.push(`${requiredName} is missing`);
      continue;
    }
    if (!matching.some((job) => job.status === "completed" && job.conclusion === "success")) {
      const states = matching
        .map((job) => `${job.status ?? "<missing>"}/${job.conclusion ?? "<missing>"}`)
        .join(", ");
      failures.push(`${requiredName} is not successful (${states})`);
    }
  }

  if (failures.length > 0) {
    fail(`release CI quality gate failed:\n- ${failures.join("\n- ")}`);
  }
}

function parseArguments(argv) {
  const [command, ...rest] = argv;
  const options = {};
  for (let index = 0; index < rest.length; index += 2) {
    const key = rest[index];
    const value = rest[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      fail("usage: verify-release-quality-gate.mjs <select|verify-jobs> --file value ...");
    }
    options[key.slice(2)] = value;
  }
  return { command, options };
}

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`cannot read ${label} JSON at ${path}: ${error.message}`);
  }
}

function main(argv) {
  const { command, options } = parseArguments(argv);
  if (command === "select") {
    const selected = findSuccessfulCiRun(readJson(options.runs, "CI runs"), {
      tagCommit: options["tag-commit"],
      defaultBranch: options["default-branch"],
    });
    process.stdout.write(`${selected.id}\n`);
    return;
  }
  if (command === "verify-jobs") {
    validateRequiredCiJobs(readJson(options.jobs, "CI jobs"));
    console.log("release quality gate: all required CI jobs succeeded");
    return;
  }
  fail("usage: verify-release-quality-gate.mjs <select|verify-jobs> --file value ...");
}

if (process.argv[1]?.endsWith("verify-release-quality-gate.mjs")) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`release quality gate: ${error.message}`);
    process.exitCode = 1;
  }
}

export {
  REQUIRED_CI_JOBS,
  ReleaseQualityGateError,
  findSuccessfulCiRun,
  validateRequiredCiJobs,
};
