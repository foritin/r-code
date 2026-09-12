/**
 * R26 — 原生设置/诊断屏 core 投影：能力只读（授予在桌面端）、清除本机
 * 令牌=回到未配对态、指纹核对（防中间人自查）、中继信息与连接质量、
 * 重连日志可复制、直连/仅中继切换。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deviceInfoUi, capabilitiesFromLabels } from "./capability-ui.ts";
import { stateBanner, transition, initialSnapshot } from "./connection-state.ts";
import { planConnection } from "./connection-strategy.ts";
import {
  actionKinds,
  appendLog,
  clearLocalCredentials,
  connectionQuality,
  fingerprintDisplay,
  MAX_LOG_ENTRIES,
  reconnectLog,
  sanitizeDetail,
  settingsModel,
} from "./settings-diagnostics.ts";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..", "..", "..");

const SECTION_OF = (model, id) => model.find((section) => section.id === id);

function baseInput(overrides = {}) {
  return {
    capabilities: capabilitiesFromLabels(["events-read", "tasks-write"]),
    snapshot: { state: "online", retryInSeconds: 0, attempt: 0 },
    plan: planConnection({ lanHost: "192.168.1.10", lanPort: 8787, relayUrl: null, forceRelay: false }),
    relayUrl: null,
    forceRelay: false,
    log: [],
    fingerprint: "a1b2c3d4".repeat(8),
    rttMs: 42,
    deviceName: "pixel",
    pairedAtMs: Date.UTC(2026, 8, 12, 9, 0, 0),
    version: "1.0.1",
    ...overrides,
  };
}

test("R26.A1: 能力只读展示（授予在桌面端，App 内不可自授）", () => {
  const info = deviceInfoUi(capabilitiesFromLabels(["events-read", "tasks-write"]));
  assert.match(info[0].value, /事件读取/);
  assert.match(info[0].value, /任务写入/);
  assert.ok(!info[0].value.includes("审批决策"), "未授予的能力不显示");

  // 屏④模型：能力段每一行都没有 action，且提示授予位置。
  const model = settingsModel(baseInput());
  const capabilitySection = SECTION_OF(model, "capabilities");
  assert.ok(capabilitySection.rows.length > 0);
  for (const row of capabilitySection.rows) {
    assert.equal(row.action, null, "能力行不可操作");
    assert.match(row.hint ?? "", /电脑端/);
  }
  // 全模型范围内不存在任何授予类动作。
  const kinds = actionKinds(model);
  assert.ok(!kinds.includes("grant"), "设置屏无授予入口");
  assert.ok(kinds.every((kind) => ["copy", "clear-token", "switch-strategy"].includes(kind)));
  // 设置屏永不展示令牌（模型序列化后不含 token 字段）。
  assert.doesNotMatch(JSON.stringify(model), /"token"/);
});

test("R26.A1: 清除令牌 → 回到未配对态（含端点缓存清空）", () => {
  const state = {
    hasCredentials: true,
    lanHost: "192.168.1.10",
    relayUrl: "relay.example.com:8787",
  };
  clearLocalCredentials(state);
  const snap = transition(initialSnapshot, {
    type: "credentialsChanged",
    hasCredentials: state.hasCredentials,
  });
  assert.equal(snap.state, "unpaired");
  assert.match(stateBanner(snap), /尚未配对/);
  assert.equal(state.lanHost, null);
  assert.equal(state.relayUrl, null);
  // 屏④提供清除入口，且只此一处涉及凭据。
  const model = settingsModel(baseInput());
  const clearRow = SECTION_OF(model, "session").rows.find((row) => row.id === "session-clear");
  assert.deepEqual(clearRow.action, { kind: "clear-token" });
  assert.match(clearRow.hint ?? "", /重新扫码配对/);
});

test("R26.A1: 指纹核对逐段显示（4 位一段，便于人工比对）", () => {
  const display = fingerprintDisplay("a1b2c3d4".repeat(8));
  const segments = display.split(" ");
  assert.equal(segments.length, 16);
  assert.ok(segments.every((seg) => seg.length === 4));
  assert.ok(display.length >= 63, "4 位分段 + 15 空格 = 63+ 字符");
  // 屏④关于段：显示分段指纹，复制的是原始指纹（便于与电脑端比对）。
  const fingerprint = "a1b2c3d4".repeat(8);
  const row = SECTION_OF(settingsModel(baseInput({ fingerprint })), "about").rows.find(
    (item) => item.id === "about-fingerprint",
  );
  assert.equal(row.value, display);
  assert.deepEqual(row.action, { kind: "copy", payload: fingerprint });
});

test("R26.A1: 中继信息与连接质量（未配置诚实显示，不臆造）", () => {
  const model = settingsModel(baseInput());
  const relay = SECTION_OF(model, "relay");
  assert.equal(relay.rows[0].value, "未配置（仅局域网直连）");
  assert.equal(relay.rows[0].action, null, "未配置时无复制内容");
  assert.match(relay.rows[1].value, /在线 · 42ms/);

  const configured = settingsModel(
    baseInput({
      relayUrl: "relay.example.com:8787",
      plan: planConnection({ lanHost: null, lanPort: null, relayUrl: "relay.example.com:8787", forceRelay: false }),
    }),
  );
  const relayConfigured = SECTION_OF(configured, "relay").rows[0];
  assert.equal(relayConfigured.value, "relay.example.com:8787");
  assert.deepEqual(relayConfigured.action, { kind: "copy", payload: "relay.example.com:8787" });

  // 离线/弱网分档。
  assert.equal(connectionQuality({ state: "offline" }, null).quality, "offline");
  assert.equal(connectionQuality({ state: "online", retryInSeconds: 0, attempt: 0 }, 900).quality, "poor");
  assert.equal(connectionQuality({ state: "online", retryInSeconds: 0, attempt: 0 }, null).quality, "fair");
});

test("R26.A1: 重连日志最近 N 条可复制（元数据，无命令内容）", () => {
  let log = [];
  for (let index = 0; index < MAX_LOG_ENTRIES + 12; index += 1) {
    log = appendLog(log, {
      atMs: Date.UTC(2026, 8, 12, 9, 0, index),
      event: index % 2 === 0 ? "disconnected" : "retry",
      detail: `attempt ${index}`,
    });
  }
  assert.equal(log.length, MAX_LOG_ENTRIES, "环形上限");
  const { entries, copyText } = reconnectLog(log, 5);
  assert.equal(entries.length, 5);
  assert.equal(entries[0].detail, `attempt ${MAX_LOG_ENTRIES + 11}`, "最新在前");
  assert.equal(copyText.split("\n").length, 5);
  // 详情长度上限：日志不承载载荷/路径。
  const long = sanitizeDetail("x".repeat(500));
  assert.ok(long.length <= 120);
  assert.ok(long.endsWith("…"));
});

test("R26.A1: 直连优先 / 仅中继切换（只改拨入次序，不动凭据与能力）", () => {
  const direct = settingsModel(baseInput());
  assert.equal(SECTION_OF(direct, "strategy").rows[0].value, "直连优先");
  assert.deepEqual(SECTION_OF(direct, "strategy").rows[1].action, {
    kind: "switch-strategy",
    forceRelay: true,
  });

  const forced = settingsModel(
    baseInput({
      forceRelay: true,
      relayUrl: "relay.example.com:8787",
      plan: planConnection({ lanHost: "192.168.1.10", lanPort: 8787, relayUrl: "relay.example.com:8787", forceRelay: true }),
    }),
  );
  assert.equal(SECTION_OF(forced, "strategy").rows[0].value, "仅中继");
  assert.deepEqual(SECTION_OF(forced, "strategy").rows[1].action, {
    kind: "switch-strategy",
    forceRelay: false,
  });
  // 切换不触碰能力段与令牌段。
  assert.deepEqual(SECTION_OF(forced, "capabilities"), SECTION_OF(direct, "capabilities"));
  assert.deepEqual(
    SECTION_OF(forced, "session").rows.map((row) => row.action),
    [{ kind: "clear-token" }],
  );
});

test("R26.A1: RN 屏④存在并消费共享 core（不复制投影、不引 tauri）", () => {
  const screenPath = join(root, "mobile", "src", "screens", "SettingsScreen.tsx");
  const appPath = join(root, "mobile", "App.tsx");
  assert.ok(existsSync(screenPath), "mobile/src/screens/SettingsScreen.tsx 存在");

  const screen = readFileSync(screenPath, "utf8");
  assert.match(screen, /from ["']react-native["']/, "RN 实现");
  assert.doesNotMatch(screen, /@tauri-apps/, "F13：RN 工程不引 tauri");
  assert.match(screen, /settings-diagnostics/, "消费共享 core 投影");
  assert.match(screen, /minHeight: 48/, "命中区 ≥48dp（Android）/≥44pt（iOS）");
  assert.match(screen, /#181818|colors\.background/, "obsidian 深色底");
  assert.match(screen, /accessibilityRole/, "无障碍标签");

  const app = readFileSync(appPath, "utf8");
  assert.match(app, /SettingsScreen/, "App 挂载设置/诊断屏");
  assert.match(app, /settingsModel/, "模型由 core 提供，RN 只渲染");
  assert.match(app, /secureStore\.remove/, "清除令牌走安全存储");
  assert.match(app, /createPlatformAdapter/, "装配 PlatformAdapter");
  // 屏④不渲染令牌：任何 device.token 都不进 Text。
  assert.doesNotMatch(app, /<Text[^>]*>[^<]*\btoken\b[^<]*<\/Text>/i);
});
