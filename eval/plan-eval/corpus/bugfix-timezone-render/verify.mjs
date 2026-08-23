#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { dayKey } = await load("lib/dates.js");
const { crossesMidnightUtc } = await load("lib/schedule.js");
const start = Date.parse("2026-08-21T23:30:00Z");
const end = Date.parse("2026-08-22T00:30:00Z");
assert.equal(dayKey(start), "2026-08-21");
assert.equal(dayKey(end), "2026-08-22");
assert.equal(crossesMidnightUtc(start, end), true);
assert.equal(crossesMidnightUtc(start, start + 60000), false);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
