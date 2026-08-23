#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { search } = await load("lib/search.js");
const { snippetFor } = await load("lib/highlight.js");
const docs = [
  { id: 1, text: "the quick brown fox jumps over the lazy dog" },
  { id: 2, text: "nothing relevant here" },
];
const hits = search(docs, "fox");
assert.equal(hits.length, 1);
assert.ok(hits[0].snippet.includes("《fox》"), hits[0].snippet);
assert.equal(search(docs, "cat").length, 0);
assert.ok(snippetFor("abc", "z").length <= 24);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
