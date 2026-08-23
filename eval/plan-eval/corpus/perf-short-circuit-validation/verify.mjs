#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { runBatch } = await load("lib/batch.js");
const { validateAll } = await load("lib/validator.js");
const rules = [
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
assert.equal(strict.evaluated, 1, "strict must stop at the first fatal error");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
