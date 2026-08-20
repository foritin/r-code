#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { buildFeed } = await load("lib/feed.js");
const { buildMatcher } = await load("lib/matcher.js");
const resources = Array.from({ length: 4000 }, (_, i) => ({
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
assert.ok(elapsedMs < 1000, "index lookup must stay well under budget: " + elapsedMs + "ms");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
