#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { renderComment, escapeHtml } = await load("lib/render.js");
const { renderFeed } = await load("lib/feed.js");
const hostile = '<script>alert("x")</script>';
const rendered = renderComment({ body: hostile });
assert.ok(!rendered.includes("<script>"), rendered);
assert.ok(rendered.includes("&lt;script&gt;"));
assert.ok(!renderFeed([{ body: '<img src=x onerror="1">' }]).includes("<img"));
assert.equal(escapeHtml("a&b"), "a&amp;b");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
