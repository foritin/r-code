import assert from "node:assert/strict";
import { access, mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";

import {
  parseTimeoutMs,
  runBoundedProcess,
} from "./verify-tui-v2.mjs";

test("TUI verifier validates configured command timeouts", () => {
  assert.equal(parseTimeoutMs(undefined, "TEST_TIMEOUT", 500), 500);
  assert.equal(parseTimeoutMs("250", "TEST_TIMEOUT", 500), 250);
  assert.throws(() => parseTimeoutMs("0", "TEST_TIMEOUT", 500), /positive integer/);
  assert.throws(() => parseTimeoutMs("oops", "TEST_TIMEOUT", 500), /positive integer/);
});

test("TUI verifier timeout kills the complete process tree", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "r-code-tui-verifier-"));
  const marker = path.join(dir, "descendant-survived.txt");
  const descendant = `setTimeout(() => require("node:fs").writeFileSync(${JSON.stringify(marker)}, "alive"), 800)`;
  const parent = `require("node:child_process").spawn(process.execPath, ["-e", ${JSON.stringify(descendant)}], { stdio: "ignore" }); setInterval(() => {}, 1000)`;

  try {
    const result = await runBoundedProcess(
      { file: process.execPath, args: ["-e", parent] },
      { cwd: dir, env: process.env, timeoutMs: 100 },
    );
    assert.equal(result.timedOut, true);
    assert.equal(result.exitCode === 0, false);
    await delay(1_000);
    await assert.rejects(access(marker));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
