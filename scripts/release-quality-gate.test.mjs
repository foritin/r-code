import assert from "node:assert/strict";
import test from "node:test";

import {
  REQUIRED_CI_JOBS,
  ReleaseQualityGateError,
  findSuccessfulCiRun,
  validateRequiredCiJobs,
} from "./verify-release-quality-gate.mjs";

const TAG_COMMIT = "a".repeat(40);

function successfulRun(overrides = {}) {
  return {
    id: 42,
    name: "CI",
    event: "push",
    head_sha: TAG_COMMIT,
    head_branch: "main",
    status: "completed",
    conclusion: "success",
    updated_at: "2026-08-07T00:00:00Z",
    ...overrides,
  };
}

test("quality gate selects only a successful CI push for the tagged default-branch commit", () => {
  const selected = findSuccessfulCiRun({
    workflow_runs: [
      successfulRun({ id: 1, conclusion: "failure" }),
      successfulRun({ id: 2, head_branch: "dev" }),
      successfulRun({ id: 3, head_sha: "b".repeat(40) }),
      successfulRun({ id: 4, updated_at: "2026-08-06T00:00:00Z" }),
      successfulRun({ id: 5, updated_at: "2026-08-07T00:00:00Z" }),
    ],
  }, { tagCommit: TAG_COMMIT, defaultBranch: "main" });

  assert.equal(selected.id, 5);
});

test("quality gate rejects a tag when no successful matching CI run exists", () => {
  assert.throws(
    () => findSuccessfulCiRun({ workflow_runs: [successfulRun({ conclusion: "failure" })] }, {
      tagCommit: TAG_COMMIT,
      defaultBranch: "main",
    }),
    (error) => error instanceof ReleaseQualityGateError
      && /no successful CI push run exists/.test(error.message),
  );
});

test("quality gate requires every release-critical CI job to succeed", () => {
  const jobs = {
    jobs: REQUIRED_CI_JOBS.map((name) => ({
      name,
      status: "completed",
      conclusion: "success",
    })),
  };

  assert.doesNotThrow(() => validateRequiredCiJobs(jobs));
  jobs.jobs.find((job) => job.name === "Secret scanning").conclusion = "failure";
  assert.throws(
    () => validateRequiredCiJobs(jobs),
    /Secret scanning is not successful \(completed\/failure\)/,
  );
});

test("quality gate reports missing required jobs", () => {
  assert.throws(
    () => validateRequiredCiJobs({ jobs: [] }),
    /Frontend and Release Metadata is missing/,
  );
});
