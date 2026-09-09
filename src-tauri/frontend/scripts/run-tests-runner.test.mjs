import assert from "node:assert/strict";
import { access, mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";

import { parseTimeoutMs, runWithTimeout } from "./run-tests.mjs";

test("frontend runner validates its overall timeout", () => {
  assert.equal(parseTimeoutMs(undefined), 35 * 60 * 1000);
  assert.equal(parseTimeoutMs("250"), 250);
  assert.throws(() => parseTimeoutMs("0"), /positive integer/);
  assert.throws(() => parseTimeoutMs("invalid"), /positive integer/);
});

test("frontend runner timeout terminates descendant processes", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "r-code-run-tests-"));
  const marker = path.join(dir, "descendant-survived.txt");
  const descendant = `setTimeout(() => require("node:fs").writeFileSync(${JSON.stringify(marker)}, "alive"), 800)`;
  const parent = `require("node:child_process").spawn(process.execPath, ["-e", ${JSON.stringify(descendant)}], { stdio: "ignore" }); setInterval(() => {}, 1000)`;

  try {
    const result = await runWithTimeout(process.execPath, ["-e", parent], {
      cwd: dir,
      timeoutMs: 100,
      stdio: "ignore",
    });
    assert.equal(result.timedOut, true);
    await delay(1_000);
    await assert.rejects(access(marker));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
