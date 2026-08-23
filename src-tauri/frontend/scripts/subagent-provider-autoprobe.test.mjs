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

test("auto-probe request list merges catalog entries with saved pool slots", () => {
  // 快照到达后按整个 snapshot 触发（请求列表在纯函数内合并目录 + 槽位）。
  assert.match(panel, /void runAutoProbe\(snapshot\);/);
  assert.match(panel, /export function buildAutoProbeRequests\(/);
  // 目录条目：配置就绪、有模型、未连通。selectable 本身由连通回执派生，
  // 不能反过来作为首次探测的前置条件，否则 untested 永远进不了请求列表。
  assert.match(
    panel,
    /entry\.ready && entry\.model && entry\.health\.state !== "connected"/,
  );
  assert.doesNotMatch(panel, /entry\.ready && entry\.selectable/);
  // 已保存槽位：按 (source, model) 键控的健康投影不是 connected 时补测。
  assert.match(panel, /slotHealthOf\(slot, snapshot\)\?\.health\.state \?\? "untested"/);
  // 槽位来源未就绪（无密钥/未安装）时跳过。
  assert.match(panel, /readySources\.has\(sourceKey\(slot\.source\)\)/);
  // 合并结果按 candidateKey 去重。
  assert.match(panel, /const seen = new Set<string>\(\);[\s\S]*?candidateKey\(source, model\)/);
  // 自动探测复用批量测试 IPC，且结果进入统一的 probeResults 通道。
  assert.match(panel, /const response = await subagentProviderTestBatch\(requests\);[\s\S]*?candidateKey\(entry\.source, entry\.model\)/);
});

test("auto-probe is throttled so repeated panel entry does not re-issue requests", () => {
  // 模块级节流窗口：短时间反复挂载/刷新不重复请求 provider。
  assert.match(panel, /const AUTO_PROBE_THROTTLE_MS = 60_000;/);
  assert.match(panel, /let lastAutoProbeAt = 0;/);
  assert.match(panel, /if \(now - lastAutoProbeAt < AUTO_PROBE_THROTTLE_MS\) \{[\s\S]*?return;/);
  // 并发守卫：上一次自动探测未完成时不叠加第二次。
  assert.match(panel, /if \(autoProbeBusy\.current\) return;/);
  // 手动"全部测试"走独立入口（testAll），不受节流影响。
  assert.match(panel, /const testAll = async \(\) => \{[\s\S]*?subagentProviderTestBatch\(requests\)[\s\S]*?\};[\s\S]*?const save =/);
});

test("auto-probe outcome is surfaced in a persistent status line", () => {
  assert.match(panel, /export function autoProbeStatusLabel\(/);
  assert.match(panel, /已自动测试 \$\{summary\.tested\} 项：\$\{summary\.connected\} 项连通/);
  assert.match(panel, /失败项可手动重测/);
  // 节流窗口内重复进入显示"沿用"而非清空结果。
  assert.match(panel, /一分钟内不重复测试：沿用最近的连通结果/);
  assert.match(panel, /data-testid="subagent-autoprobe-status"/);
});

test("auto-probe participates in the panel busy mutual exclusion", () => {
  assert.match(panel, /\["save", "batch", "reload", "auto-probe"\]\.some/);
  // 自动探测失败静默降级，不打断配置流程。
  assert.match(panel, /catch \{\s*\/\/ 自动探测失败不打断配置流程/);
});
