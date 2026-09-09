import { readdirSync } from "node:fs";
import { spawn } from "node:child_process";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const scriptsDir = dirname(scriptPath);
const DEFAULT_TIMEOUT_MS = 35 * 60 * 1000;
const KILL_GRACE_MS = 5_000;

export function parseTimeoutMs(value) {
  if (value === undefined || value === "") return DEFAULT_TIMEOUT_MS;
  const timeoutMs = Number(value);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
    throw new Error("R_CODE_FRONTEND_TEST_TIMEOUT_MS must be a positive integer");
  }
  return timeoutMs;
}

function terminateProcessTree(child) {
  if (!child.pid) return;
  if (process.platform === "win32") {
    const killer = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
    killer.once("error", () => child.kill("SIGKILL"));
    killer.once("close", (code) => {
      if (code !== 0) child.kill("SIGKILL");
    });
    return;
  }
  try {
    process.kill(-child.pid, "SIGKILL");
  } catch {
    child.kill("SIGKILL");
  }
}

export function runWithTimeout(file, args, { cwd, timeoutMs, stdio = "inherit" }) {
  return new Promise((resolveResult) => {
    const child = spawn(file, args, {
      cwd,
      stdio,
      detached: process.platform !== "win32",
      windowsHide: true,
    });
    let settled = false;
    let timedOut = false;
    let killFallback;
    const finish = (status, signal, error = null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (killFallback) clearTimeout(killFallback);
      resolveResult({ status, signal, error, timedOut });
    };
    const timer = setTimeout(() => {
      timedOut = true;
      terminateProcessTree(child);
      // A broken taskkill/signal implementation must not leave the runner waiting forever.
      killFallback = setTimeout(() => finish(null, "SIGKILL"), KILL_GRACE_MS);
    }, timeoutMs);

    child.once("error", (error) => finish(null, null, error));
    child.once("close", (status, signal) => finish(status, signal));
  });
}

export async function main(argv = process.argv.slice(2)) {
  const allTests = readdirSync(scriptsDir)
    .filter((name) => name.endsWith(".test.mjs"))
    .sort()
    .map((name) => join(scriptsDir, name));

  // 可选的位置参数：只运行指定的测试文件（基名或 *.test.mjs 皆可）。
  const requested = argv.filter((argument) => !argument.startsWith("-"));
  const tests = requested.length > 0
    ? requested.map((name) => join(scriptsDir, name.endsWith(".test.mjs") ? name : `${name}.test.mjs`))
    : allTests;
  const missing = tests.filter((test) => !allTests.includes(test));
  if (missing.length > 0) {
    console.error(`Unknown test file(s): ${missing.map((test) => basename(test)).join(", ")}`);
    return 1;
  }
  if (tests.length === 0) {
    console.error("No frontend regression tests were found.");
    return 1;
  }

  const timeoutMs = parseTimeoutMs(process.env.R_CODE_FRONTEND_TEST_TIMEOUT_MS);
  console.log(`[run-tests] ${tests.length}/${allTests.length} file(s): ${tests.map((test) => basename(test)).join(", ")}`);
  console.log(`[run-tests] overall timeout: ${timeoutMs}ms`);

  const result = await runWithTimeout(
    process.execPath,
    ["--test", "--test-concurrency=1", ...tests],
    { cwd: dirname(scriptsDir), timeoutMs },
  );
  if (result.error) throw result.error;
  if (result.timedOut) {
    console.error(`[run-tests] timed out after ${timeoutMs}ms; terminated the test process tree`);
    return 124;
  }
  if (result.signal) {
    console.error(`[run-tests] test process exited from signal ${result.signal}`);
  }
  return result.status ?? 1;
}

if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  main()
    .then((exitCode) => {
      process.exitCode = exitCode;
    })
    .catch((error) => {
      console.error(`[run-tests] ${error instanceof Error ? error.message : String(error)}`);
      process.exitCode = 1;
    });
}
