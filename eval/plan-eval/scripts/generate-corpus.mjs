#!/usr/bin/env node
/**
 * 生成冻结 corpus 的 25 个自包含 case（docs/support/archive/implementation/plan-mode-dual-track-gate.md §16.1）。
 *
 * 每个 case 目录包含：
 * - case.json     元数据（任务指令 + 期望复杂度信号）
 * - fixture/      只读起始工作区
 * - oracle.patch  参考修复（由 git 程序化生成，绝无手写上下文漂移）
 * - verify.mjs    确定性验收脚本：fixture 上必红、oracle patch 后必绿
 *
 * verify.mjs 以 argv[2] 接收目标目录（评估器传 arm 工作区；本地默认相邻
 * fixture），按 file URL 动态导入——脚本本身不进入被测工作区。
 *
 * 只在创建/修订 corpus 时运行；产物入库后以 corpus-lock.json 冻结
 *（scripts/validate-corpus.mjs --freeze）。
 */
import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");

const CASES = [
  c("bugfix", "bugfix-offbyone-pagination",
    "修复分页偏移缺陷：`lib/pager.js` 的 pageToOffset 对第 1 页多减了一页，导致列表页首条丢失；`lib/report.js` 依赖该函数输出报表区间。补齐两处行为并让验收断言通过。",
    ["multi_subsystem"],
    { "lib/pager.js": `export const PAGE_SIZE = 20;
export function pageToOffset(page) {
  // page is 1-based
  return (page - 2) * PAGE_SIZE;
}
export function clampPage(page, totalItems) {
  const maxPage = Math.max(1, Math.ceil(totalItems / PAGE_SIZE));
  return Math.min(Math.max(1, page), maxPage);
}
`, "lib/report.js": `import { pageToOffset, clampPage, PAGE_SIZE } from "./pager.js";

export function reportWindow(page, totalItems) {
  const safe = clampPage(page, totalItems);
  const offset = pageToOffset(safe);
  const end = Math.min(totalItems, offset + PAGE_SIZE);
  const start = Math.max(0, offset);
  return { start, end };
}
` },
    { "lib/pager.js": `export const PAGE_SIZE = 20;
export function pageToOffset(page) {
  // page is 1-based: the first page starts at zero.
  return (page - 1) * PAGE_SIZE;
}
export function clampPage(page, totalItems) {
  const maxPage = Math.max(1, Math.ceil(totalItems / PAGE_SIZE));
  return Math.min(Math.max(1, page), maxPage);
}
` },
    [`const { pageToOffset, clampPage } = await load("lib/pager.js");
const { reportWindow } = await load("lib/report.js");`],
    `assert.equal(pageToOffset(1), 0);
assert.equal(pageToOffset(2), 20);
assert.deepEqual(reportWindow(1, 45), { start: 0, end: 20 });
assert.deepEqual(reportWindow(3, 45), { start: 40, end: 45 });
assert.equal(clampPage(9, 45), 3);`),

  c("bugfix", "bugfix-null-coalesce-config",
    "修复配置合并缺陷：`lib/config.js` 在用户配置缺省时把嵌套默认值整段覆盖为 undefined，`lib/service.js` 读取 server.port 时因此抛错。要求缺省字段逐级回退默认值，并保持显式 null 被保留的既有语义。",
    ["multi_subsystem"],
    { "lib/config.js": `export const DEFAULTS = {
  server: { host: "0.0.0.0", port: 8080 },
  log: { level: "info" },
};

export function mergeConfig(user) {
  if (!user) return structuredClone(DEFAULTS);
  return { ...DEFAULTS, ...user };
}
`, "lib/service.js": `import { mergeConfig } from "./config.js";

export function servicePort(user) {
  const config = mergeConfig(user);
  return config.server.port;
}
` },
    { "lib/config.js": `export const DEFAULTS = {
  server: { host: "0.0.0.0", port: 8080 },
  log: { level: "info" },
};

export function mergeConfig(user) {
  if (!user) return structuredClone(DEFAULTS);
  const merged = structuredClone(DEFAULTS);
  for (const [key, value] of Object.entries(user)) {
    if (value === null || Array.isArray(value) || typeof value !== "object") {
      merged[key] = value;
      continue;
    }
    merged[key] = { ...(merged[key] ?? {}), ...value };
  }
  return merged;
}
` },
    [`const { mergeConfig } = await load("lib/config.js");
const { servicePort } = await load("lib/service.js");`],
    `assert.equal(servicePort(undefined), 8080);
assert.equal(servicePort({}), 8080);
assert.equal(servicePort({ log: { level: "debug" } }), 8080);
assert.equal(servicePort({ server: { host: "127.0.0.1" } }), 8080);
assert.equal(servicePort({ server: { port: 9000 } }), 9000);
// 显式 null 保留：server 被整体置空，读取方按缺数据处理。
assert.equal(mergeConfig({ server: null }).server, null);
assert.throws(() => servicePort({ server: null }));`),

  c("bugfix", "bugfix-async-retry-order",
    "修复重试器缺陷：`lib/retry.js` 在异步回调尚未完成时就判定成功，`lib/uploader.js` 因此在部分失败时仍报告完成。要求按回调结果判定重试并透出最终错误；保持最大 3 次尝试。",
    ["multi_stage_verification"],
    { "lib/retry.js": `export async function retry(fn, attempts = 3) {
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      fn();
      return true;
    } catch (error) {
      if (attempt === attempts) throw error;
    }
  }
}
`, "lib/uploader.js": `import { retry } from "./retry.js";

export async function uploadAll(items, send) {
  for (const item of items) {
    await retry(() => send(item));
  }
  return { uploaded: items.length };
}
` },
    { "lib/retry.js": `export async function retry(fn, attempts = 3) {
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      await fn();
      return true;
    } catch (error) {
      if (attempt === attempts) throw error;
    }
  }
}
` },
    [`const { retry } = await load("lib/retry.js");
const { uploadAll } = await load("lib/uploader.js");`],
    `let calls = 0;
await retry(async () => { calls += 1; if (calls < 2) throw new Error("boom"); });
assert.equal(calls, 2);
assert.deepEqual(await uploadAll([1, 2], async () => {}), { uploaded: 2 });
await assert.rejects(() => retry(async () => { throw new Error("always"); }, 3), /always/);`),

  c("bugfix", "bugfix-timezone-render",
    "修复时间渲染缺陷：`lib/dates.js` 使用本地时区解析 UTC 时间戳，`lib/schedule.js` 的跨日边界判断随之漂移。统一以 UTC 计算并保持输出格式不变。",
    ["multi_subsystem"],
    { "lib/dates.js": `export function dayKey(timestampMs) {
  const date = new Date(timestampMs);
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return year + "-" + month + "-" + day;
}
`, "lib/schedule.js": `import { dayKey } from "./dates.js";

export function crossesMidnightUtc(startMs, endMs) {
  return dayKey(startMs) !== dayKey(endMs);
}
` },
    { "lib/dates.js": `export function dayKey(timestampMs) {
  const date = new Date(timestampMs);
  const year = date.getUTCFullYear();
  const month = String(date.getUTCMonth() + 1).padStart(2, "0");
  const day = String(date.getUTCDate()).padStart(2, "0");
  return year + "-" + month + "-" + day;
}
` },
    [`const { dayKey } = await load("lib/dates.js");
const { crossesMidnightUtc } = await load("lib/schedule.js");`],
    `const start = Date.parse("2026-08-21T23:30:00Z");
const end = Date.parse("2026-08-22T00:30:00Z");
assert.equal(dayKey(start), "2026-08-21");
assert.equal(dayKey(end), "2026-08-22");
assert.equal(crossesMidnightUtc(start, end), true);
assert.equal(crossesMidnightUtc(start, start + 60000), false);`),

  c("bugfix", "bugfix-cache-invalidation",
    "修复缓存失效缺陷：`lib/cache.js` 在 key 更新后未写入正确条目，`lib/session.js` 读取到空会话。要求写入同 key 时替换并让新值立即可见、旧值不可达。",
    ["multi_subsystem"],
    { "lib/cache.js": `const store = new Map();

export const cache = {
  get(key) { return store.get(key); },
  set(key, value) { store.set(key + "#", value); },
  size() { return store.size; },
};
`, "lib/session.js": `import { cache } from "./cache.js";

export function loadSession(id) {
  return cache.get("session:" + id) ?? null;
}

export function saveSession(id, data) {
  cache.set("session:" + id, data);
}
` },
    { "lib/cache.js": `const store = new Map();

export const cache = {
  get(key) { return store.get(key); },
  set(key, value) { store.set(key, value); },
  size() { return store.size; },
};
` },
    [`const { loadSession, saveSession } = await load("lib/session.js");
const { cache } = await load("lib/cache.js");`],
    `saveSession("a", { user: 1 });
assert.deepEqual(loadSession("a"), { user: 1 });
saveSession("a", { user: 2 });
assert.deepEqual(loadSession("a"), { user: 2 });
assert.equal(cache.size(), 1);`),

  c("feature", "feature-rate-limiter",
    "为服务添加可配置限流：新增 `lib/limiter.js`（滑动窗口计数），并在 `lib/server.js` 的 handleRequest 入口按 per-key 限流（超出返回 429），`lib/config.js` 暴露 { windowMs, max } 默认 60000/100。",
    ["multi_subsystem", "design_decision"],
    { "lib/config.js": `export const CONFIG = {
  bodyLimit: 1024,
};
`, "lib/server.js": `export function makeServer() {
  const routes = new Map();
  return {
    on(path, handler) { routes.set(path, handler); },
    async handleRequest(path, key, payload) {
      const handler = routes.get(path);
      if (!handler) return { status: 404 };
      return handler(payload, key);
    },
  };
}
` },
    { "lib/config.js": `export const CONFIG = {
  bodyLimit: 1024,
  rateLimit: {
    windowMs: 60000,
    max: 100,
  },
};
`, "lib/limiter.js": `export function createRateLimiter({ windowMs, max }) {
  const hits = new Map();
  return {
    allow(key, now = Date.now()) {
      const windowStart = now - windowMs;
      const timestamps = (hits.get(key) ?? []).filter((time) => time > windowStart);
      if (timestamps.length >= max) {
        hits.set(key, timestamps);
        return false;
      }
      timestamps.push(now);
      hits.set(key, timestamps);
      return true;
    },
  };
}
`, "lib/server.js": `import { createRateLimiter } from "./limiter.js";
import { CONFIG } from "./config.js";

export function makeServer(options = {}) {
  const routes = new Map();
  const limiter = createRateLimiter(options.rateLimit ?? CONFIG.rateLimit);
  return {
    on(path, handler) { routes.set(path, handler); },
    async handleRequest(path, key, payload) {
      if (!limiter.allow(key)) {
        return { status: 429 };
      }
      const handler = routes.get(path);
      if (!handler) return { status: 404 };
      return handler(payload, key);
    },
  };
}
` },
    [`const { makeServer } = await load("lib/server.js");
const { CONFIG } = await load("lib/config.js");`],
    `assert.deepEqual(CONFIG.rateLimit, { windowMs: 60000, max: 100 });
const server = makeServer({ rateLimit: { windowMs: 1000, max: 2 } });
server.on("/ping", () => ({ status: 200 }));
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 200);
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 200);
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 429);
assert.equal((await server.handleRequest("/ping", "u2", null)).status, 200);`),

  c("feature", "feature-search-highlight",
    "实现搜索结果高亮：新增 `lib/highlight.js`（把命中片段截断为上下文窗口），并接入 `lib/search.js` 的 search 返回值（每条命中附 snippet 字段，命中词用《》包裹）。",
    ["multi_subsystem"],
    { "lib/search.js": `export function search(docs, query) {
  const lowered = query.toLowerCase();
  return docs
    .filter((doc) => doc.text.toLowerCase().includes(lowered))
    .map((doc) => ({ id: doc.id }));
}
` },
    { "lib/highlight.js": `export function snippetFor(text, query, radius = 12) {
  const index = text.toLowerCase().indexOf(query.toLowerCase());
  if (index < 0) return text.slice(0, radius * 2);
  const start = Math.max(0, index - radius);
  const end = Math.min(text.length, index + query.length + radius);
  const before = text.slice(start, index);
  const hit = text.slice(index, index + query.length);
  const after = text.slice(index + query.length, end);
  return before + "《" + hit + "》" + after;
}
`, "lib/search.js": `import { snippetFor } from "./highlight.js";

export function search(docs, query) {
  const lowered = query.toLowerCase();
  return docs
    .filter((doc) => doc.text.toLowerCase().includes(lowered))
    .map((doc) => ({ id: doc.id, snippet: snippetFor(doc.text, query) }));
}
` },
    [`const { search } = await load("lib/search.js");
const { snippetFor } = await load("lib/highlight.js");`],
    `const docs = [
  { id: 1, text: "the quick brown fox jumps over the lazy dog" },
  { id: 2, text: "nothing relevant here" },
];
const hits = search(docs, "fox");
assert.equal(hits.length, 1);
assert.ok(hits[0].snippet.includes("《fox》"), hits[0].snippet);
assert.equal(search(docs, "cat").length, 0);
assert.ok(snippetFor("abc", "z").length <= 24);`),

  c("feature", "feature-audit-log",
    "添加审计日志：新增 `lib/audit.js`（append-only 记录 + 按操作类型查询，外部拿到的是拷贝），并在 `lib/account.js` 的 transfer 里记录 from/to/amount 与时间戳。",
    ["multi_subsystem", "expensive_rollback"],
    { "lib/account.js": `export function makeAccount(balance) {
  return { balance };
}

export function transfer(from, to, amount) {
  if (from.balance < amount) throw new Error("insufficient");
  from.balance -= amount;
  to.balance += amount;
  return true;
}
` },
    { "lib/audit.js": `const entries = [];

export const audit = {
  record(entry) {
    entries.push({ ...entry, at: entry.at ?? new Date().toISOString() });
  },
  byAction(action) {
    return entries.filter((entry) => entry.action === action).map((entry) => ({ ...entry }));
  },
  size() { return entries.length; },
};
`, "lib/account.js": `import { audit } from "./audit.js";

export function makeAccount(balance) {
  return { balance };
}

export function transfer(from, to, amount, at) {
  if (from.balance < amount) throw new Error("insufficient");
  from.balance -= amount;
  to.balance += amount;
  audit.record({ action: "transfer", from: from.id ?? "?", to: to.id ?? "?", amount, at });
  return true;
}
` },
    [`const { makeAccount, transfer } = await load("lib/account.js");
const { audit } = await load("lib/audit.js");`],
    `const alice = { id: "alice", ...makeAccount(100) };
const bob = { id: "bob", ...makeAccount(0) };
transfer(alice, bob, 40, "2026-08-21T00:00:00Z");
transfer(alice, bob, 10, "2026-08-21T00:01:00Z");
assert.equal(alice.balance, 50);
assert.equal(audit.byAction("transfer").length, 2);
assert.equal(audit.byAction("transfer")[0].amount, 40);
audit.byAction("transfer")[0].amount = 9999;
assert.equal(audit.byAction("transfer")[0].amount, 40, "entries must be copies");`),

  c("feature", "feature-feature-flags",
    "实现功能开关：新增 `lib/flags.js`（分层默认 → 用户覆盖 → 会话覆盖），并在 `lib/page.js` 的 render 中根据开关启用新版页头；未知开关回落 false。",
    ["design_decision", "multi_subsystem"],
    { "lib/page.js": `export function render(user) {
  return { header: "classic", user: user.id };
}
` },
    { "lib/flags.js": `export function createFlags({ defaults = {}, userOverrides = {} } = {}) {
  const sessionOverrides = new Map();
  return {
    isEnabled(name, context = {}) {
      const session = sessionOverrides.get(name);
      if (session !== undefined) return session;
      const user = userOverrides[context.user ?? ""]?.[name];
      if (user !== undefined) return user;
      return Boolean(defaults[name]);
    },
    setSession(name, enabled) { sessionOverrides.set(name, enabled); },
  };
}
`, "lib/page.js": `import { createFlags } from "./flags.js";

export const flags = createFlags({ defaults: { newHeader: true } });

export function render(user) {
  const header = flags.isEnabled("newHeader", { user: user.id }) ? "modern" : "classic";
  return { header, user: user.id };
}
` },
    [`const { render, flags } = await load("lib/page.js");
const { createFlags } = await load("lib/flags.js");`],
    `assert.equal(render({ id: "u1" }).header, "modern");
flags.setSession("newHeader", false);
assert.equal(render({ id: "u1" }).header, "classic");
const local = createFlags({ defaults: { a: true } });
assert.equal(local.isEnabled("unknown"), false);
assert.equal(local.isEnabled("a"), true);`),

  c("feature", "feature-batch-export",
    "实现批量导出：新增 `lib/exporter.js`（把记录数组导出为 CSV 与 JSONL 两种格式，处理转义），并在 `lib/repo.js` 增加 exportAs(records, format) 委托；非法 format 抛错。",
    ["multi_subsystem"],
    { "lib/repo.js": `export function makeRepo() {
  const items = [];
  return {
    add(item) { items.push(item); return items.length; },
    all() { return [...items]; },
  };
}
` },
    { "lib/exporter.js": `function csvCell(value) {
  const text = String(value ?? "");
  return /[",\\n]/.test(text) ? '"' + text.replace(/"/g, '""') + '"' : text;
}

export function toCsv(records) {
  if (records.length === 0) return "";
  const headers = Object.keys(records[0]);
  const lines = records.map((record) => headers.map((key) => csvCell(record[key])).join(","));
  return [headers.join(","), ...lines].join("\\n");
}

export function toJsonl(records) {
  return records.map((record) => JSON.stringify(record)).join("\\n");
}
`, "lib/repo.js": `import { toCsv, toJsonl } from "./exporter.js";

export function makeRepo() {
  const items = [];
  return {
    add(item) { items.push(item); return items.length; },
    all() { return [...items]; },
    exportAs(format) {
      if (format === "csv") return toCsv(items);
      if (format === "jsonl") return toJsonl(items);
      throw new Error("unknown format: " + format);
    },
  };
}
` },
    [`const { makeRepo } = await load("lib/repo.js");`],
    `const repo = makeRepo();
repo.add({ name: "a,b", score: 1 });
repo.add({ name: 'say "hi"', score: 2 });
const csv = repo.exportAs("csv");
assert.ok(csv.startsWith("name,score"));
assert.ok(csv.includes('"a,b"'));
const jsonl = repo.exportAs("jsonl");
assert.equal(jsonl.split("\\n").length, 2);
assert.throws(() => repo.exportAs("xml"), /unknown format/);`),

  c("migration", "migration-v1-v2-settings",
    "把用户设置从 v1 平面结构迁移到 v2 嵌套结构：新增 `lib/migrate.js` 的 migrateSettings(v1)，v1 的 notify_email/notify_push 映射到 v2 的 notify.email/push；`lib/store.js` 的 load 读取旧数据时透明迁移。旧字段必须不再出现在输出。",
    ["migration_or_data"],
    { "lib/store.js": `export function loadSettings(raw) {
  return typeof raw === "string" ? JSON.parse(raw) : raw;
}
` },
    { "lib/migrate.js": `export function migrateSettings(v1) {
  if (!v1 || v1.version === 2) return v1;
  const { notify_email, notify_push, ...rest } = v1;
  return {
    ...rest,
    version: 2,
    notify: {
      email: Boolean(notify_email),
      push: Boolean(notify_push),
    },
  };
}
`, "lib/store.js": `import { migrateSettings } from "./migrate.js";

export function loadSettings(raw) {
  const parsed = typeof raw === "string" ? JSON.parse(raw) : raw;
  return migrateSettings(parsed);
}
` },
    [`const { loadSettings } = await load("lib/store.js");
const { migrateSettings } = await load("lib/migrate.js");`],
    `const migrated = loadSettings('{"version":1,"theme":"dark","notify_email":true,"notify_push":false}');
assert.equal(migrated.version, 2);
assert.deepEqual(migrated.notify, { email: true, push: false });
assert.equal(migrated.theme, "dark");
assert.ok(!("notify_email" in migrated));
const already = migrateSettings({ version: 2, notify: { email: true, push: true } });
assert.deepEqual(already.notify, { email: true, push: true });`),

  c("migration", "migration-legacy-ids",
    "把字符串 ID 迁移为数字 ID：新增 `lib/idmap.js`（字符串→数字稳定映射），`lib/refs.js` 的 rebindEdges 把引用旧 ID 的边列表改写为新 ID；未登记的旧 ID 保留原样并收集到 unresolved。",
    ["migration_or_data", "expensive_rollback"],
    { "lib/refs.js": `export function rebind(edges) {
  return edges.map((edge) => ({ ...edge }));
}
` },
    { "lib/idmap.js": `export function createIdMap(pairs = []) {
  const map = new Map(pairs);
  return {
    toNumeric(legacyId) {
      return map.get(legacyId) ?? null;
    },
    register(legacyId, numericId) { map.set(legacyId, numericId); },
  };
}
`, "lib/refs.js": `import { createIdMap } from "./idmap.js";

export { createIdMap };

export function rebindEdges(idMap, edges) {
  const unresolved = [];
  const rebound = edges.map((edge) => {
    const from = idMap.toNumeric(edge.from);
    const to = idMap.toNumeric(edge.to);
    if (from == null || to == null) unresolved.push(edge);
    return { from: from ?? edge.from, to: to ?? edge.to };
  });
  return { rebound, unresolved };
}
` },
    [`const { rebindEdges, createIdMap } = await load("lib/refs.js");`],
    `const idMap = createIdMap([["a", 1], ["b", 2]]);
const { rebound, unresolved } = rebindEdges(idMap, [
  { from: "a", to: "b" },
  { from: "a", to: "zz" },
]);
assert.deepEqual(rebound, [{ from: 1, to: 2 }, { from: 1, to: "zz" }]);
assert.equal(unresolved.length, 1);`),

  c("migration", "migration-key-rotation",
    "实现密钥轮换迁移：新增 `lib/keystore.js`（v1 明文密钥迁移为 v2 salted 摘要存储，保留 legacy 校验路径直到全部轮换），`lib/auth.js` 的 verify 同时接受旧明文等值与新摘要。",
    ["migration_or_data", "expensive_rollback"],
    { "lib/auth.js": `export function verify(store, token, candidate) {
  return store[token] === candidate;
}
` },
    { "lib/keystore.js": `import { createHash } from "node:crypto";

export function digest(token, salt) {
  return createHash("sha256").update(salt + ":" + token).digest("hex");
}

export function rotate(store, salt) {
  const rotated = {};
  for (const [token, value] of Object.entries(store)) {
    rotated[token] = typeof value === "string" && value.startsWith("sha256:")
      ? value
      : "sha256:" + digest(value, salt);
  }
  return rotated;
}
`, "lib/auth.js": `import { digest } from "./keystore.js";

export function verify(store, token, candidate) {
  const stored = store[token];
  if (stored == null) return false;
  if (typeof stored === "string" && stored.startsWith("sha256:")) {
    return stored === "sha256:" + digest(candidate, store.__salt ?? "");
  }
  return stored === candidate;
}
` },
    [`const { verify } = await load("lib/auth.js");
const { rotate } = await load("lib/keystore.js");`],
    `const legacy = { t1: "secret-one" };
assert.equal(verify(legacy, "t1", "secret-one"), true);
const rotated = { ...rotate(legacy, "salt1"), __salt: "salt1" };
assert.ok(rotated.t1.startsWith("sha256:"));
assert.equal(verify(rotated, "t1", "secret-one"), true);
assert.equal(verify(rotated, "t1", "wrong"), false);`),

  c("migration", "migration-schema-backfill",
    "实现缺失字段回填迁移：`lib/backfill.js` 的 backfill(posts) 给所有缺 slug 的文章生成稳定 slug（标题小写、非字母数字转 -、去重加序号），`lib/index.js` 的 buildIndex 要求全部条目都有 slug。",
    ["migration_or_data"],
    { "lib/index.js": `export function buildIndex(posts) {
  const index = new Map();
  for (const post of posts) index.set(post.slug, post.id);
  return index;
}
` },
    { "lib/backfill.js": `function slugify(title) {
  return String(title)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

export function backfill(posts) {
  const seen = new Set(posts.map((post) => post.slug).filter(Boolean));
  return posts.map((post) => {
    if (post.slug) return post;
    const base = slugify(post.title) || "post";
    let slug = base;
    let serial = 2;
    while (seen.has(slug)) slug = base + "-" + serial++;
    seen.add(slug);
    return { ...post, slug };
  });
}
`, "lib/index.js": `import { backfill } from "./backfill.js";

export function buildIndex(posts) {
  const complete = backfill(posts);
  const index = new Map();
  for (const post of complete) index.set(post.slug, post.id);
  return index;
}
` },
    [`const { buildIndex } = await load("lib/index.js");
const { backfill } = await load("lib/backfill.js");`],
    `const posts = [
  { id: 1, title: "Hello World!" },
  { id: 2, title: "Hello World!" },
  { id: 3, title: "Already", slug: "kept" },
];
const index = buildIndex(posts);
assert.equal(index.size, 3);
assert.ok(index.has("hello-world"));
assert.ok(index.has("hello-world-2"));
assert.ok(index.has("kept"));
assert.equal(backfill([{ id: 4, title: "固定" }])[0].slug, "post");`),

  c("migration", "migration-queue-dedupe",
    "把可重复投递的旧队列数据迁移为幂等队列：新增 `lib/dedupe.js`（按 operation_id 去重，保留首次出现），`lib/queue.js` 的 migrate 去掉重复项并输出 dropped 计数。",
    ["migration_or_data"],
    { "lib/queue.js": `export function migrate(entries) {
  return { entries: [...entries], dropped: 0 };
}
` },
    { "lib/dedupe.js": `export function dedupeByOperationId(entries) {
  const seen = new Set();
  return entries.filter((entry) => {
    if (seen.has(entry.operation_id)) return false;
    seen.add(entry.operation_id);
    return true;
  });
}
`, "lib/queue.js": `import { dedupeByOperationId } from "./dedupe.js";

export function migrate(entries) {
  const unique = dedupeByOperationId(entries);
  return { entries: unique, dropped: entries.length - unique.length };
}
` },
    [`const { migrate } = await load("lib/queue.js");`],
    `const result = migrate([
  { operation_id: "op1", payload: 1 },
  { operation_id: "op2", payload: 2 },
  { operation_id: "op1", payload: 3 },
]);
assert.equal(result.entries.length, 2);
assert.equal(result.entries[0].payload, 1);
assert.equal(result.dropped, 1);`),

  c("performance", "perf-nested-loop-lookup",
    "优化 `lib/matcher.js` 的 matchTags：当前对每个查询标签线性扫描全部资源（O(n*m)）。改为预建倒排索引，语义不变；`lib/feed.js` 组装推荐时复用同一匹配器。",
    ["multi_stage_verification"],
    { "lib/matcher.js": `export function buildMatcher(resources) {
  return {
    matchTags(tag) {
      return resources.filter((resource) =>
        resource.tags.some((candidate) => candidate.toLowerCase() === tag.toLowerCase())
      ).map((resource) => resource.id);
    },
  };
}
`, "lib/feed.js": `import { buildMatcher } from "./matcher.js";

export function buildFeed(resources, requested) {
  const matcher = buildMatcher(resources);
  return requested.flatMap((tag) => matcher.matchTags(tag));
}
` },
    { "lib/matcher.js": `export function buildMatcher(resources) {
  const index = new Map();
  for (const resource of resources) {
    for (const tag of resource.tags) {
      const key = tag.toLowerCase();
      const bucket = index.get(key) ?? [];
      bucket.push(resource.id);
      index.set(key, bucket);
    }
  }
  return {
    matchTags(tag) {
      return index.get(tag.toLowerCase()) ?? [];
    },
    indexed() { return true; },
  };
}
` },
    [`const { buildFeed } = await load("lib/feed.js");
const { buildMatcher } = await load("lib/matcher.js");`],
    `const resources = Array.from({ length: 4000 }, (_, i) => ({
  id: i,
      tags: ["tag" + (i % 50), "common"],
    }));
    const matcher = buildMatcher(resources);
    // 倒排索引能力契约：旧线性实现没有 indexed()。
assert.equal(typeof matcher.indexed, "function", "matcher must expose the index probe");
assert.equal(matcher.indexed(), true);
assert.deepEqual(matcher.matchTags("tag7"), resources.filter((r) => r.tags.includes("tag7")).map((r) => r.id));
assert.equal(buildFeed(resources, ["tag7"]).length, 80);
// 性能烟测（宽松上限）：200 次查询不得退化回逐资源线性扫描。
const started = process.hrtime.bigint();
for (let round = 0; round < 200; round += 1) matcher.matchTags("tag7");
const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
assert.ok(elapsedMs < 1000, "index lookup must stay well under budget: " + elapsedMs + "ms");`),

  c("performance", "perf-memoize-parse",
    "消除 `lib/parser.js` 的重复解析：相同输入反复全量解析。加入按原文缓存的结果复用，并在 `lib/stats.js` 统计多轮解析时生效；缓存不得改变解析语义。",
    ["multi_stage_verification"],
    { "lib/parser.js": `export function parseLine(line) {
  const parts = line.split("=>").map((part) => part.trim());
  return { key: parts[0], value: parts[1] ?? null };
}
`, "lib/stats.js": `import { parseLine } from "./parser.js";

export function parseAll(lines) {
  return lines.map((line) => parseLine(line));
}
` },
    { "lib/parser.js": `const cache = new Map();

export function parseLine(line) {
  const cached = cache.get(line);
  if (cached !== undefined) return cached;
  const parts = line.split("=>").map((part) => part.trim());
  const parsed = { key: parts[0], value: parts[1] ?? null };
  if (cache.size < 10000) cache.set(line, parsed);
  return parsed;
}
` },
    [`const { parseLine } = await load("lib/parser.js");
const { parseAll } = await load("lib/stats.js");`],
    `const lines = ["a => 1", "b => 2", "a => 1"];
const parsed = parseAll(lines);
assert.equal(parsed[0].value, "1");
assert.equal(parsed[2].value, "1");
// 缓存契约：同输入命中同一结果对象（未缓存实现每次都是新对象）。
assert.ok(parseLine("a => 1") === parseLine("a => 1"), "identical input must hit the cache");
assert.deepEqual(parseLine("a => 1"), { key: "a", value: "1" });
assert.deepEqual(parseLine("only-key"), { key: "only-key", value: null });`),

  c("performance", "perf-batched-writes",
    "优化 `lib/writer.js` 的逐条写入：writeEach 每条一次 sink。改为缓冲批量提交（保留顺序），`lib/pipeline.js` 的 runPipeline 在结尾必须 flushBuffer 保证全部落盘。",
    ["multi_stage_verification"],
    { "lib/writer.js": `export function createWriter(sink) {
  return {
    writeEach(records) {
      for (const record of records) sink([record]);
    },
  };
}
`, "lib/pipeline.js": `import { createWriter } from "./writer.js";

export function runPipeline(records, sink) {
  const writer = createWriter(sink);
  writer.writeEach(records);
}
` },
    { "lib/writer.js": `export function createWriter(sink) {
  let buffer = [];
  let flushes = 0;
  return {
    writeEach(records) {
      buffer.push(...records);
    },
    flushBuffer() {
      if (buffer.length > 0) {
        sink(buffer);
        flushes += 1;
        buffer = [];
      }
    },
    flushCount() { return flushes; },
  };
}
`, "lib/pipeline.js": `import { createWriter } from "./writer.js";

export function runPipeline(records, sink) {
  const writer = createWriter(sink);
  writer.writeEach(records);
  writer.flushBuffer();
  return writer;
}
` },
    [`const { runPipeline } = await load("lib/pipeline.js");
const { createWriter } = await load("lib/writer.js");`],
    `const batches = [];
const writer = runPipeline([1, 2, 3, 4, 5], (batch) => batches.push([...batch]));
assert.deepEqual(batches, [[1, 2, 3, 4, 5]], "single batch keeps order");
assert.equal(writer.flushCount(), 1);
runPipeline([], () => {});
const manual = createWriter(() => {});
manual.writeEach([1]);
manual.flushBuffer();
manual.flushBuffer();
assert.equal(manual.flushCount(), 1, "empty flush is a no-op");`),

  c("performance", "perf-lazy-index",
    "把 `lib/catalog.js` 的即时全量索引改为惰性构建：makeCatalog 构造时不建索引，首次 findByTag 才构建一次并复用；`lib/shelf.js` 创建大量 catalog 后只查询少数的路径不再为每个 catalog 付费。",
    ["multi_stage_verification"],
    { "lib/catalog.js": `export function makeCatalog(items) {
  const byTag = new Map();
  for (const item of items) {
    for (const tag of item.tags) {
      const bucket = byTag.get(tag) ?? [];
      bucket.push(item.id);
      byTag.set(tag, bucket);
    }
  }
  return {
    findByTag(tag) { return byTag.get(tag) ?? []; },
    indexBuiltAt: Date.now(),
  };
}
`, "lib/shelf.js": `import { makeCatalog } from "./catalog.js";

export function buildShelf(collections) {
  return collections.map((items) => makeCatalog(items));
}
` },
    { "lib/catalog.js": `export function makeCatalog(items) {
  let byTag = null;
  return {
    findByTag(tag) {
      if (byTag === null) {
        byTag = new Map();
        for (const item of items) {
          for (const tag of item.tags) {
            const bucket = byTag.get(tag) ?? [];
            bucket.push(item.id);
            byTag.set(tag, bucket);
          }
        }
      }
      return byTag.get(tag) ?? [];
    },
    indexBuilt() { return byTag !== null; },
  };
}
` },
    [`const { makeCatalog } = await load("lib/catalog.js");
const { buildShelf } = await load("lib/shelf.js");`],
    `const catalog = makeCatalog([{ id: 1, tags: ["a"] }, { id: 2, tags: ["a", "b"] }]);
assert.equal(catalog.indexBuilt(), false, "index must be lazy");
assert.deepEqual(catalog.findByTag("a"), [1, 2]);
assert.equal(catalog.indexBuilt(), true);
const shelf = buildShelf([
  [{ id: 1, tags: ["x"] }],
  [{ id: 2, tags: ["y"] }],
]);
assert.deepEqual(shelf[0].findByTag("x"), [1]);
assert.equal(shelf[1].indexBuilt(), false, "unqueried catalogs stay unbuilt");`),

  c("performance", "perf-short-circuit-validation",
    "优化 `lib/validator.js` 的 validateAll：当前对每条记录跑全部规则并聚合所有错误。改为 strict 模式下致命错误短路，`lib/batch.js` 大批量校验在 strict 模式下提前终止；默认模式行为不变。",
    ["multi_stage_verification"],
    { "lib/validator.js": `export function validateAll(records, rules) {
  const errors = [];
  for (const record of records) {
    for (const rule of rules) {
      const error = rule(record);
      if (error) errors.push(error);
    }
  }
  return { errors };
}
`, "lib/batch.js": `import { validateAll } from "./validator.js";

export function runBatch(records, rules, options = {}) {
  return validateAll(records, rules);
}
` },
    { "lib/validator.js": `export function validateAll(records, rules, options = {}) {
  const errors = [];
  let evaluated = 0;
  for (const record of records) {
    for (const rule of rules) {
      evaluated += 1;
      const error = rule(record);
      if (error) errors.push(error);
      if (options.strict && error) {
        return { errors, evaluated, stoppedEarly: true };
      }
    }
  }
  return { errors, evaluated, stoppedEarly: false };
}
`, "lib/batch.js": `import { validateAll } from "./validator.js";

export function runBatch(records, rules, options = {}) {
  return validateAll(records, rules, options);
}
` },
    [`const { runBatch } = await load("lib/batch.js");
const { validateAll } = await load("lib/validator.js");`],
    `const rules = [
  (record) => record.id ? null : "missing id",
  (record) => record.value >= 0 ? null : "negative value",
];
const lenient = runBatch([
  { id: null, value: -1 },
  { id: "ok", value: 1 },
], rules);
assert.equal(lenient.errors.length, 2);
assert.equal(lenient.stoppedEarly, false);
const strict = runBatch([{ id: null, value: -1 }], rules, { strict: true });
assert.equal(strict.stoppedEarly, true);
assert.equal(strict.evaluated, 1, "strict must stop at the first fatal error");`),

  c("safety", "safety-path-traversal",
    "修复路径穿越漏洞：`lib/paths.js` 的 resolveInside 对 ../ 序列未规范化，`lib/static.js` 用它提供静态文件。要求拒绝任何逃出根目录的输入（返回 null），保留合法子路径。",
    ["expensive_rollback"],
    { "lib/paths.js": `import { resolve } from "node:path";

export function resolveInside(root, relative) {
  return resolve(root, relative);
}
`, "lib/static.js": `import { readFileSync } from "node:fs";
import { resolveInside } from "./paths.js";

export function readAsset(root, relative) {
  const path = resolveInside(root, relative);
  return path ? readFileSync(path, "utf8") : null;
}
` },
    { "lib/paths.js": `import { resolve, relative } from "node:path";

export function resolveInside(root, relativeInput) {
  const target = resolve(root, relativeInput);
  const rel = relative(root, target);
  if (rel === "") return target;
  if (rel.startsWith("..")) return null;
  return target;
}
` },
    [`const { resolveInside } = await load("lib/paths.js");`],
    `const root = process.cwd();
assert.ok(resolveInside(root, "a/b.txt").startsWith(root));
assert.equal(resolveInside(root, "../secret.txt"), null);
assert.equal(resolveInside(root, "a/../../secret.txt"), null);
assert.ok(resolveInside(root, ".").startsWith(root));`),

  c("safety", "safety-html-escape",
    "修复 XSS 注入：`lib/render.js` 的 renderComment 直接拼接用户文本。要求对所有动态内容做 HTML 转义后再进入模板；`lib/feed.js` 的 renderFeed 复用同一转义路径。",
    ["expensive_rollback"],
    { "lib/render.js": `export function renderComment(comment) {
  return '<div class="comment">' + comment.body + '</div>';
}
`, "lib/feed.js": `import { renderComment } from "./render.js";

export function renderFeed(comments) {
  return comments.map(renderComment).join("\\n");
}
` },
    { "lib/render.js": `export function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function renderComment(comment) {
  return '<div class="comment">' + escapeHtml(comment.body) + '</div>';
}
` },
    [`const { renderComment, escapeHtml } = await load("lib/render.js");
const { renderFeed } = await load("lib/feed.js");`],
    `const hostile = '<script>alert("x")</script>';
const rendered = renderComment({ body: hostile });
assert.ok(!rendered.includes("<script>"), rendered);
assert.ok(rendered.includes("&lt;script&gt;"));
assert.ok(!renderFeed([{ body: '<img src=x onerror="1">' }]).includes("<img"));
assert.equal(escapeHtml("a&b"), "a&amp;b");`),

  c("safety", "safety-secret-redaction",
    "实现日志脱敏：新增 `lib/redact.js`（按模式遮蔽 Bearer token、api key 与长十六进制串），并在 `lib/logger.js` 的 info/warn 输出前统一脱敏；原始值不得出现在任何输出行。",
    ["expensive_rollback"],
    { "lib/logger.js": `const lines = [];

export const logger = {
  info(message) { lines.push(message); },
  warn(message) { lines.push(message); },
  all() { return [...lines]; },
};
` },
    { "lib/redact.js": `export function redact(text) {
  return String(text)
    .replace(/Bearer\\s+[A-Za-z0-9._-]+/gi, "Bearer [REDACTED]")
    .replace(/(api[_-]?key["'=:\\s]+)[A-Za-z0-9._-]+/gi, "$1[REDACTED]")
    .replace(/\\b[0-9a-f]{32,}\\b/gi, "[REDACTED]");
}
`, "lib/logger.js": `import { redact } from "./redact.js";

const lines = [];

export const logger = {
  info(message) { lines.push(redact(message)); },
  warn(message) { lines.push(redact(message)); },
  all() { return [...lines]; },
};
` },
    [`const { logger } = await load("lib/logger.js");
const { redact } = await load("lib/redact.js");`],
    `const token = "sk-abcd1234efgh5678";
logger.info("connect with Bearer " + token);
logger.warn("api_key=" + token);
logger.info("digest deadbeefdeadbeefdeadbeefdeadbeef");
const all = logger.all().join("\\n");
assert.ok(!all.includes(token));
assert.ok(!all.includes("deadbeefdeadbeefdeadbeefdeadbeef"));
assert.ok(all.includes("Bearer [REDACTED]"));
assert.equal(redact("clean message"), "clean message");`),

  c("safety", "safety-idempotent-charging",
    "实现幂等扣费：`lib/ledger.js` 的 charge 对同一 idempotency_key 不得重复扣款，重复请求返回首次结果；`lib/api.js` 的 handleOrder 在重试风暴下只生效一次。",
    ["expensive_rollback"],
    { "lib/ledger.js": `export function createLedger() {
  const balance = { amount: 100 };
  return {
    charge(amount) {
      balance.amount -= amount;
      return { charged: amount, remaining: balance.amount };
    },
    balance() { return balance.amount; },
  };
}
`, "lib/api.js": `import { createLedger } from "./ledger.js";

export function createApi() {
  const ledger = createLedger();
  return {
    handleOrder(order) {
      return ledger.charge(order.amount);
    },
  };
}
` },
    { "lib/ledger.js": `export function createLedger() {
  const balance = { amount: 100 };
  const seen = new Map();
  return {
    charge(amount, idempotencyKey) {
      if (idempotencyKey != null && seen.has(idempotencyKey)) {
        return seen.get(idempotencyKey);
      }
      balance.amount -= amount;
      const result = { charged: amount, remaining: balance.amount };
      if (idempotencyKey != null) seen.set(idempotencyKey, result);
      return result;
    },
    balance() { return balance.amount; },
  };
}
`, "lib/api.js": `import { createLedger } from "./ledger.js";

export function createApi() {
  const ledger = createLedger();
  return {
    handleOrder(order) {
      return ledger.charge(order.amount, order.idempotency_key);
    },
    balance() { return ledger.balance(); },
  };
}
` },
    [`const { createLedger } = await load("lib/ledger.js");
const { createApi } = await load("lib/api.js");`],
    `const ledger = createLedger();
const first = ledger.charge(30, "key-1");
const repeat = ledger.charge(30, "key-1");
assert.deepEqual(first, repeat);
assert.equal(ledger.balance(), 70);
ledger.charge(10, "key-2");
ledger.charge(10, "key-2");
assert.equal(ledger.balance(), 60, "retries must never double-charge");
assert.deepEqual(ledger.charge(5), { charged: 5, remaining: 55 });
const api = createApi();
// 新账本从 100 起：重试风暴下同单只扣一次。
api.handleOrder({ amount: 5, idempotency_key: "o1" });
api.handleOrder({ amount: 5, idempotency_key: "o1" });
api.handleOrder({ amount: 5, idempotency_key: "o1" });
assert.equal(api.balance(), 95);`),

  c("safety", "safety-input-clamping",
    "实现输入钳制：`lib/quota.js` 的 setQuota 接受任意数字导致负配额与超大配额破坏系统。要求钳制到 [0, 10000] 并拒绝 NaN/Infinity；`lib/tenant.js` 注册租户时走同一钳制。",
    ["expensive_rollback"],
    { "lib/quota.js": `export function setQuota(current, next) {
  return { ...current, quota: next };
}
`, "lib/tenant.js": `import { setQuota } from "./quota.js";

export function registerTenant(tenant, quota) {
  return { ...tenant, limits: setQuota(tenant.limits ?? { quota: 0 }, quota) };
}
` },
    { "lib/quota.js": `export const QUOTA_MIN = 0;
export const QUOTA_MAX = 10000;

export function setQuota(current, next) {
  if (typeof next !== "number" || !Number.isFinite(next)) {
    throw new Error("quota must be a finite number");
  }
  const clamped = Math.min(QUOTA_MAX, Math.max(QUOTA_MIN, Math.trunc(next)));
  return { ...current, quota: clamped };
}
` },
    [`const { setQuota } = await load("lib/quota.js");
const { registerTenant } = await load("lib/tenant.js");`],
    `assert.equal(setQuota({ quota: 1 }, -50).quota, 0);
assert.equal(setQuota({ quota: 1 }, 999999).quota, 10000);
assert.equal(setQuota({ quota: 1 }, 42.9).quota, 42);
assert.throws(() => setQuota({ quota: 1 }, Number.NaN), /finite/);
assert.throws(() => setQuota({ quota: 1 }, Infinity), /finite/);
assert.equal(registerTenant({ id: "t" }, 123456).limits.quota, 10000);`),
];

function c(category, id, task, signals, files, oracle, verifyImports, verifyBody) {
  return { category, id, task, signals, files, oracle, verifyImports, verifyBody };
}

function renderVerify(imports, body) {
  return `#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
${imports}
${body}
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
`;
}

function sh(command, args, options = {}) {
  return execFileSync(command, args, {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 120_000,
    ...options,
  }).toString();
}

const stagingRoot = mkdtempSync(join(tmpdir(), "plan-eval-gen-"));
let written = 0;
try {
  for (const testCase of CASES) {
    const dir = join(root, "corpus", testCase.id);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(join(dir, "fixture"), { recursive: true });
    writeFileSync(join(dir, "case.json"), JSON.stringify({
      id: testCase.id,
      category: testCase.category,
      task: testCase.task,
      expected_signals: testCase.signals,
    }, null, 2) + "\n");
    for (const [name, content] of Object.entries(testCase.files)) {
      const path = join(dir, "fixture", name);
      mkdirSync(join(path, ".."), { recursive: true });
      writeFileSync(path, content);
    }
    // oracle 树 = fixture + oracle 覆盖；在临时 git 仓库（工作区即 fixture 内容）
    // 内程序化生成 patch，路径与评估工作区一致，可直接 `git apply`。
    const repo = join(stagingRoot, testCase.id);
    mkdirSync(repo, { recursive: true });
    for (const entry of readdirSync(join(dir, "fixture"))) {
      cpSync(join(dir, "fixture", entry), join(repo, entry), { recursive: true });
    }
    sh("git", ["init", "-q"], { cwd: repo });
    sh("git", ["add", "."], { cwd: repo });
    sh("git", ["-c", "user.email=eval@r-code.local", "-c", "user.name=plan-eval", "commit", "-qm", "fixture"], { cwd: repo });
    for (const [name, content] of Object.entries(testCase.oracle)) {
      const path = join(repo, name);
      mkdirSync(join(path, ".."), { recursive: true });
      writeFileSync(path, content);
    }
    sh("git", ["add", "."], { cwd: repo });
    const patch = sh("git", ["-c", "user.email=eval@r-code.local", "-c", "user.name=plan-eval", "diff", "--cached", "--src-prefix=a/", "--dst-prefix=b/"], { cwd: repo });
    if (patch.trim().length === 0) {
      throw new Error(`oracle for ${testCase.id} is identical to fixture`);
    }
    writeFileSync(join(dir, "oracle.patch"), patch);
    writeFileSync(join(dir, "verify.mjs"), renderVerify(testCase.verifyImports, testCase.verifyBody));
    written += 1;
  }
} finally {
  rmSync(stagingRoot, { recursive: true, force: true });
}

// 路由 probe 集：20 simple + 20 complex（docs §16.3，与能力实验完全分离）。
const SIMPLE_PROMPTS = [
  "把这个函数改名为 buildUrl，并更新它的两处调用点。",
  "帮我把 README 里的安装命令改成 pnpm。",
  "解释这段递归函数在做什么。",
  "把按钮文案从「提交」改成「保存」。",
  "修复这个拼写错误：recieve → receive。",
  "给这个接口加一个可选的 timeout 参数，默认不传行为不变。",
  "这个报错是什么意思？TypeError: cannot read property of undefined",
  "把这两行日志删掉。",
  "给 User 模型加一个 nickname 字段的读取方法。",
  "把这个常量从 10 改成 25。",
  "查看这个文件的前 50 行并总结结构。",
  "把这个测试里的断言从 equal 改成 deepEqual。",
  "帮我把这个函数的参数名从 data 改成 payload。",
  "解释这个正则表达式的含义。",
  "把这个 CSS 的颜色从 #fff 改成 #fafafa。",
  "给这个模块补一行文件头注释。",
  "这个配置文件里 server.port 是多少？",
  "把这个函数的返回值改成数组形式。",
  "把这个目录下的图片文件列出来。",
  "把这段代码的缩进统一为两个空格。",
];
const COMPLEX_PROMPTS = [
  "把认证系统从 session 迁移到 JWT：涉及网关、用户服务、前端鉴权拦截与存量会话回收。",
  "重构订单与库存两个服务共享的扣减逻辑，改成事件驱动的最终一致，保留回滚路径。",
  "把单机任务队列迁移到分布式队列，处理重复投递、失败重试与死信，并兼容旧队列数据。",
  "为整个应用引入多租户隔离：数据库 schema、缓存键、审计日志都要按租户划分。",
  "把构建产物从 CommonJS 迁移到 ESM，处理循环依赖、动态 require 与第三方兼容。",
  "设计并实现统一的通知中心：邮件、短信、站内信三通道，失败降级与幂等去重。",
  "实现文件存储从本地磁盘到对象存储的迁移，包含增量同步、校验与回切方案。",
  "把同步导出改成异步任务：队列、进度查询、取消与部分失败恢复，跨三个模块。",
  "重构权限模型从角色制到 RBAC+ABAC 混合，迁移存量数据并保持 API 兼容。",
  "为支付链路添加对账系统：三方流水拉取、差异检测、自动申诉与人工兜底。",
  "把配置中心从环境变量迁移到远程配置：热更新、灰度、回滚与启动兜底。",
  "实现全链路追踪接入：SDK 封装、采样策略、跨服务上下文透传与存储成本控制。",
  "把单体里的搜索模块抽成独立服务：索引同步、查询代理、降级与数据回填。",
  "重构缓存层：多级缓存一致性、防击穿/穿透/雪崩，并下线旧的旁路缓存。",
  "把定时任务调度迁移到分布式调度器：分片、失败转移、幂等与监控告警。",
  "实现灰度发布系统：流量分桶、指标观测、自动回滚，涉及网关与部署链路。",
  "把用户反馈系统与工单系统合并：数据迁移、去重合并规则与双写过渡期。",
  "为 API 网关实现限流与配额：多维度计数、平滑放量、超卖保护与审计。",
  "把日志系统从文件迁移到流式采集：采集器、传输保障、脱敏与存储分层。",
  "重构前端状态管理：从混用多方案收敛到单一方案，保持组件 API 不变。",
];
const probes = [
  ...SIMPLE_PROMPTS.map((prompt, index) => ({ id: `simple-${String(index + 1).padStart(2, "0")}`, label: "simple", prompt })),
  ...COMPLEX_PROMPTS.map((prompt, index) => ({ id: `complex-${String(index + 1).padStart(2, "0")}`, label: "complex", prompt })),
];
writeFileSync(join(root, "routing", "probes.json"), JSON.stringify(probes, null, 2) + "\n");

console.log(`generated ${written} cases and ${probes.length} routing probes`);
