import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { appendFile, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const DEFAULT_ATTEMPTS = 3;
const DEFAULT_TIMEOUT_MS = 30 * 60 * 1000;

export function slugify(value) {
  const slug = value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "suite";
}

function readPositiveInteger(value, option) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${option} must be a positive integer`);
  }
  return parsed;
}

export function parseArguments(argv) {
  const separator = argv.indexOf("--");
  if (separator === -1 || separator === argv.length - 1) {
    throw new Error(
      "usage: node scripts/flaky-test-report.mjs --name <suite> [--attempts <n>] [--cwd <path>] [--output-dir <path>] [--timeout-ms <n>] -- <command> [args...]",
    );
  }

  const options = {
    name: "",
    attempts: DEFAULT_ATTEMPTS,
    cwd: process.cwd(),
    outputDir: path.resolve(process.cwd(), "target", "flaky-report"),
    timeoutMs: DEFAULT_TIMEOUT_MS,
  };

  for (let index = 0; index < separator; index += 1) {
    const option = argv[index];
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`${option} requires a value`);
    }
    index += 1;

    switch (option) {
      case "--name":
        options.name = value.trim();
        break;
      case "--attempts":
        options.attempts = readPositiveInteger(value, option);
        break;
      case "--cwd":
        options.cwd = path.resolve(process.cwd(), value);
        break;
      case "--output-dir":
        options.outputDir = path.resolve(process.cwd(), value);
        break;
      case "--timeout-ms":
        options.timeoutMs = readPositiveInteger(value, option);
        break;
      default:
        throw new Error(`unknown option: ${option}`);
    }
  }

  if (!options.name) {
    throw new Error("--name is required");
  }

  const [command, ...commandArgs] = argv.slice(separator + 1);
  return { ...options, command, commandArgs };
}

export function classifyAttempts(attempts) {
  if (attempts.length === 0) {
    throw new Error("at least one attempt is required");
  }

  const passed = attempts.filter((attempt) => attempt.exitCode === 0).length;
  if (passed === attempts.length) {
    return "stable-pass";
  }
  if (passed > 0) {
    return "flaky-candidate";
  }
  return "persistent-failure";
}

function executableForPlatform(command) {
  if (process.platform !== "win32") {
    return command;
  }
  return ["npm", "npx", "pnpm", "yarn"].includes(command)
    ? `${command}.cmd`
    : command;
}

function displayCommand(command, args) {
  return [command, ...args]
    .map((part) => (/\s/.test(part) ? JSON.stringify(part) : part))
    .join(" ");
}

function terminateProcessTree(child, force) {
  if (!child.pid) {
    return;
  }

  if (process.platform === "win32") {
    // Windows has no reliable graceful signal for a cmd/npm process tree.
    const args = ["/pid", String(child.pid), "/T", "/F"];
    const killer = spawn("taskkill.exe", args, {
      stdio: "ignore",
      windowsHide: true,
    });
    killer.once("error", () => child.kill(force ? "SIGKILL" : "SIGTERM"));
    return;
  }

  try {
    process.kill(-child.pid, force ? "SIGKILL" : "SIGTERM");
  } catch {
    child.kill(force ? "SIGKILL" : "SIGTERM");
  }
}

async function runAttempt({
  command,
  commandArgs,
  cwd,
  timeoutMs,
  attemptNumber,
  logPath,
}) {
  const startedAt = new Date();
  const started = performance.now();
  const log = createWriteStream(logPath, { encoding: "utf8" });
  const heading = [
    `suite attempt: ${attemptNumber}`,
    `started: ${startedAt.toISOString()}`,
    `cwd: ${cwd}`,
    `command: ${displayCommand(command, commandArgs)}`,
    "",
  ].join("\n");
  log.write(heading);
  process.stdout.write(`\n${heading}`);

  return await new Promise((resolve) => {
    let timer;
    let forceKillTimer;
    const child = spawn(executableForPlatform(command), commandArgs, {
      cwd,
      env: {
        ...process.env,
        R_CODE_FLAKY_ATTEMPT: String(attemptNumber),
      },
      stdio: ["ignore", "pipe", "pipe"],
      detached: process.platform !== "win32",
      windowsHide: true,
    });

    let settled = false;
    let timedOut = false;
    const finish = (exitCode, signal, spawnError = null) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timer) {
        clearTimeout(timer);
      }
      if (forceKillTimer) {
        clearTimeout(forceKillTimer);
      }
      const durationMs = Math.round(performance.now() - started);
      const footer = [
        "",
        `finished: ${new Date().toISOString()}`,
        `duration_ms: ${durationMs}`,
        `exit_code: ${exitCode ?? "null"}`,
        `signal: ${signal ?? "none"}`,
        `timed_out: ${timedOut}`,
        spawnError ? `spawn_error: ${spawnError.message}` : "",
        "",
      ]
        .filter(Boolean)
        .join("\n");
      log.end(footer, () => {
        resolve({
          attempt: attemptNumber,
          exitCode: exitCode ?? 1,
          signal,
          timedOut,
          durationMs,
          logPath,
          error: spawnError?.message ?? null,
        });
      });
    };

    child.stdout.on("data", (chunk) => {
      process.stdout.write(chunk);
      log.write(chunk);
    });
    child.stderr.on("data", (chunk) => {
      process.stderr.write(chunk);
      log.write(chunk);
    });
    child.once("error", (error) => finish(1, null, error));
    child.once("close", (exitCode, signal) => finish(exitCode, signal));

    timer = setTimeout(() => {
      timedOut = true;
      terminateProcessTree(child, false);
      forceKillTimer = setTimeout(
        () => terminateProcessTree(child, true),
        10_000,
      );
    }, timeoutMs);
  });
}

function summaryMarkdown(report) {
  const heading = report.status === "stable-pass" ? "✅" : "❌";
  const rows = report.attempts
    .map(
      (attempt) =>
        `| ${attempt.attempt} | ${attempt.exitCode === 0 ? "pass" : "fail"} | ${(attempt.durationMs / 1000).toFixed(1)}s | ${attempt.timedOut ? "yes" : "no"} |`,
    )
    .join("\n");

  return [
    `## ${heading} Flaky test report: ${report.name}`,
    "",
    `**Classification:** \`${report.status}\``,
    "",
    "| Attempt | Result | Duration | Timed out |",
    "| ---: | --- | ---: | --- |",
    rows,
    "",
    report.status === "flaky-candidate"
      ? "> The same revision both passed and failed. Treat this as a flaky-test candidate."
      : report.status === "persistent-failure"
        ? "> Every attempt failed. This is likely a deterministic regression or environment failure."
        : "> Every attempt passed.",
    "",
  ].join("\n");
}

export async function main(argv = process.argv.slice(2)) {
  const options = parseArguments(argv);
  const slug = slugify(options.name);
  await mkdir(options.outputDir, { recursive: true });

  const attempts = [];
  for (let attemptNumber = 1; attemptNumber <= options.attempts; attemptNumber += 1) {
    attempts.push(
      await runAttempt({
        ...options,
        attemptNumber,
        logPath: path.join(
          options.outputDir,
          `${slug}.attempt-${attemptNumber}.log`,
        ),
      }),
    );
  }

  const status = classifyAttempts(attempts);
  const report = {
    schemaVersion: 1,
    name: options.name,
    status,
    command: [options.command, ...options.commandArgs],
    cwd: options.cwd,
    generatedAt: new Date().toISOString(),
    attempts,
  };
  const reportPath = path.join(options.outputDir, `${slug}.json`);
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");

  const markdown = summaryMarkdown(report);
  process.stdout.write(`\n${markdown}`);
  if (process.env.GITHUB_STEP_SUMMARY) {
    await appendFile(process.env.GITHUB_STEP_SUMMARY, markdown, "utf8");
  }

  if (status !== "stable-pass") {
    process.stderr.write(
      `::error title=Flaky test report (${options.name})::${status}; inspect the uploaded attempt logs\n`,
    );
    process.exitCode = 1;
  }

  return report;
}

const invokedUrl = process.argv[1]
  ? pathToFileURL(path.resolve(process.argv[1])).href
  : "";
if (import.meta.url === invokedUrl) {
  main().catch((error) => {
    process.stderr.write(`[flaky-test-report] ${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
