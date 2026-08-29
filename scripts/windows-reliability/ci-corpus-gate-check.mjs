// M4-02.A2 + F-test-05：CI Windows job 必须含金集 fast 与 slow 两道门禁且失败会阻断。
// 检查 .github/workflows/ci.yml：存在 windows 条件化的 corpus-run 步骤（fast 与 slow
// 各一），且每道带 thresholds 检查（普通 step 失败天然阻断 job）。

import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");

async function main() {
  const ciPath = path.join(ROOT, ".github", "workflows", "ci.yml");
  const ci = await readFile(ciPath, "utf8");
  const failures = [];

  const gateSteps = [
    ...ci.matchAll(/- name: Command corpus golden gate[\s\S]*?(?=\n      - name:|\n  \w)/g),
  ].map((m) => m[0]);
  if (gateSteps.length < 2) {
    failures.push(`ci.yml 需含 fast 与 slow 两道金集门禁（当前刻到 ${gateSteps.length} 道）`);
  }
  for (const step of gateSteps) {
    if (!/matrix\.os == 'windows-latest'/.test(step)) {
      failures.push("金集门禁步骤未条件化到 windows-latest 腿");
    }
    if (!step.includes("corpus-run.mjs")) {
      failures.push("金集门禁步骤未调用 corpus-run.mjs");
    }
    if (!step.includes("--tier")) {
      failures.push("金集门禁步骤未启用 --tier");
    }
    if (!step.includes("thresholds")) {
      failures.push("金集门禁步骤缺 thresholds 检查（无阈值门禁不成其为门禁）");
    }
    if (/continue-on-error:\s*true/.test(step)) {
      failures.push("金集门禁步骤不得设置 continue-on-error（失败必须阻断）");
    }
  }

  // 门禁必须位于 test job（cargo test 所在 job）内——同 job 内失败才会阻断测试矩阵。
  const testJob = ci.slice(ci.indexOf("  test:"), ci.indexOf("  audit:"));
  if (!testJob.includes("Command corpus golden gate")) {
    failures.push("金集门禁步骤不在 test job 内");
  }

  if (failures.length > 0) {
    console.error(`ci-corpus-gate-check 失败：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log("ci-corpus-gate-check OK: test job windows 腿含 fast+slow 双金集门禁（thresholds + 失败阻断）");
}

await main();
