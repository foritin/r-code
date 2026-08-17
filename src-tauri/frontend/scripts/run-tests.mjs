import { readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const allTests = readdirSync(scriptsDir)
  .filter((name) => name.endsWith(".test.mjs"))
  .sort()
  .map((name) => join(scriptsDir, name));

// 可选的位置参数：只运行指定的测试文件（基名或 *.test.mjs 皆可）。
// 不传参数时保持全量串行，与既有本地/CI 行为一致：
//   npm test -- app-shell.test.mjs companion-window-ui.test.mjs
const requested = process.argv.slice(2).filter((argument) => !argument.startsWith("-"));
const tests = requested.length > 0
  ? requested.map((name) => join(scriptsDir, name.endsWith(".test.mjs") ? name : `${name}.test.mjs`))
  : allTests;
const missing = tests.filter((test) => !allTests.includes(test));
if (missing.length > 0) {
  console.error(`Unknown test file(s): ${missing.map((test) => basename(test)).join(", ")}`);  process.exit(1);
}

if (tests.length === 0) {
  console.error("No frontend regression tests were found.");
  process.exit(1);
}

console.log(`[run-tests] ${tests.length}/${allTests.length} file(s): ${tests.map((test) => basename(test)).join(", ")}`);

const result = spawnSync(
  process.execPath,
  ["--test", "--test-concurrency=1", ...tests],
  { cwd: dirname(scriptsDir), stdio: "inherit" },
);

if (result.error) throw result.error;
process.exit(result.status ?? 1);
