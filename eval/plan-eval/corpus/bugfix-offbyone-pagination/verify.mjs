#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { pageToOffset, clampPage } = await load("lib/pager.js");
const { reportWindow } = await load("lib/report.js");
assert.equal(pageToOffset(1), 0);
assert.equal(pageToOffset(2), 20);
assert.deepEqual(reportWindow(1, 45), { start: 0, end: 20 });
assert.deepEqual(reportWindow(3, 45), { start: 40, end: 45 });
assert.equal(clampPage(9, 45), 3);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
