// M0-02.A3：PRD §4.4 基线小节回填一致性检查。
// 断言：基线小节含 Windows 报告路径、四个数字字段（total/ok/dialect_failures/
// hint_hits）与 darwin 外部待执行标注，且与入库的基线报告 JSON 数字一致。

import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");
const PRD = path.join(ROOT, "docs", "support", "contracts", "windows-command-reliability-prd.md");
const BASELINE_REPORT = path.join(
  ROOT,
  "artifacts",
  "metrics",
  "command-corpus",
  "report-69ab1637c1ea346e0241a52ba4d939626dce9958-windows-baseline.json",
);

async function main() {
  const prd = await readFile(PRD, "utf8");
  const baseline = JSON.parse(await readFile(BASELINE_REPORT, "utf8"));

  const failures = [];
  const requiredTokens = [
    "report-69ab1637c1ea346e0241a52ba4d939626dce9958-windows-baseline.json",
    `total=${baseline.total}`,
    `ok=${baseline.ok}`,
    `dialect_failures=${baseline.dialect_failures}`,
    `hint_hits=${baseline.hint_hits}`,
    `dialect=\`${baseline.dialect}\``,
    "report-<sha>-darwin.json",
    "外部待执行",
  ];
  for (const token of requiredTokens) {
    if (!prd.includes(token)) {
      failures.push(`PRD §4.4 基线小节缺少标记：${token}`);
    }
  }
  const baselineSection = prd.slice(prd.indexOf("基线（M0-02 产出"), prd.indexOf("对照报告（M4-02"));
  if (baselineSection.length === 0) {
    failures.push("PRD §4.4 未找到基线小节锚点");
  }
  if (baseline.total < 40) {
    failures.push(`基线报告 total=${baseline.total} < 40`);
  }

  if (failures.length > 0) {
    console.error(`prd-baseline-check 失败：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log(
    `prd-baseline-check OK: windows 报告路径 + 四数字字段（total=${baseline.total} ok=${baseline.ok} dialect_failures=${baseline.dialect_failures} hint_hits=${baseline.hint_hits}）+ darwin 外部待执行标注`,
  );
}

await main();
