#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { parseLine } = await load("lib/parser.js");
const { parseAll } = await load("lib/stats.js");
const lines = ["a => 1", "b => 2", "a => 1"];
const parsed = parseAll(lines);
assert.equal(parsed[0].value, "1");
assert.equal(parsed[2].value, "1");
// 缓存契约：同输入命中同一结果对象（未缓存实现每次都是新对象）。
assert.ok(parseLine("a => 1") === parseLine("a => 1"), "identical input must hit the cache");
assert.deepEqual(parseLine("a => 1"), { key: "a", value: "1" });
assert.deepEqual(parseLine("only-key"), { key: "only-key", value: null });
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
