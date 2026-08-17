import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const panel = fs.readFileSync(
  path.join(frontendDir, "src/components/scenes/SubagentProvidersPanel.tsx"),
  "utf8",
);

test("entering the subagent provider panel auto-probes unverified ready sources", () => {
  // 快照到达后必须自动发起批量探测，不再依赖用户先点"全部测试"。
  assert.match(panel, /void autoProbe\(snapshot\.catalog\?\.entries \?\? \[\]\)/);
  // 只补测就绪且未连通的来源；已连通的沿用持久化 receipt。
  assert.match(
    panel,
    /entry\.ready && entry\.selectable && entry\.model[\s\S]*?entry\.health\.state !== "connected"/,
  );
  // 自动探测复用批量测试 IPC，且结果进入统一的 probeResults 通道。
  assert.match(panel, /const response = await subagentProviderTestBatch\(requests\);[\s\S]*?candidateKey\(entry\.source, entry\.model\)/);
});

test("auto-probe is throttled so repeated panel entry does not re-issue requests", () => {
  // 模块级节流窗口：短时间反复挂载/刷新不重复请求 provider。
  assert.match(panel, /const AUTO_PROBE_THROTTLE_MS = 60_000;/);
  assert.match(panel, /let lastAutoProbeAt = 0;/);
  assert.match(panel, /if \(now - lastAutoProbeAt < AUTO_PROBE_THROTTLE_MS\) return;/);
  // 并发守卫：上一次自动探测未完成时不叠加第二次。
  assert.match(panel, /if \(autoProbeBusy\.current\) return;/);
  // 手动"全部测试"走独立入口（testAll），不受节流影响。
  assert.match(panel, /const testAll = async \(\) => \{[\s\S]*?subagentProviderTestBatch\(requests\)[\s\S]*?\};[\s\S]*?const save =/);
});

test("auto-probe participates in the panel busy mutual exclusion", () => {
  assert.match(panel, /\["save", "batch", "reload", "auto-probe"\]\.some/);
  // 自动探测失败静默降级，不打断配置流程。
  assert.match(panel, /catch \{\s*\/\/ 自动探测失败不打断配置流程/);
});
