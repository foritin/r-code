#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { migrate } = await load("lib/queue.js");
const result = migrate([
  { operation_id: "op1", payload: 1 },
  { operation_id: "op2", payload: 2 },
  { operation_id: "op1", payload: 3 },
]);
assert.equal(result.entries.length, 2);
assert.equal(result.entries[0].payload, 1);
assert.equal(result.dropped, 1);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
