#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { buildIndex } = await load("lib/index.js");
const { backfill } = await load("lib/backfill.js");
const posts = [
  { id: 1, title: "Hello World!" },
  { id: 2, title: "Hello World!" },
  { id: 3, title: "Already", slug: "kept" },
];
const index = buildIndex(posts);
assert.equal(index.size, 3);
assert.ok(index.has("hello-world"));
assert.ok(index.has("hello-world-2"));
assert.ok(index.has("kept"));
assert.equal(backfill([{ id: 4, title: "固定" }])[0].slug, "post");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
