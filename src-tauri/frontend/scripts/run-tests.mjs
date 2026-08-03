import { readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const tests = readdirSync(scriptsDir)
  .filter((name) => name.endsWith(".test.mjs"))
  .sort()
  .map((name) => join(scriptsDir, name));

if (tests.length === 0) {
  console.error("No frontend regression tests were found.");
  process.exit(1);
}

const result = spawnSync(
  process.execPath,
  ["--test", "--test-concurrency=1", ...tests],
  { cwd: dirname(scriptsDir), stdio: "inherit" },
);

if (result.error) throw result.error;
process.exit(result.status ?? 1);
