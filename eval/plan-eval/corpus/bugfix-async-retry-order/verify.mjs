#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { retry } = await load("lib/retry.js");
const { uploadAll } = await load("lib/uploader.js");
let calls = 0;
await retry(async () => { calls += 1; if (calls < 2) throw new Error("boom"); });
assert.equal(calls, 2);
assert.deepEqual(await uploadAll([1, 2], async () => {}), { uploaded: 2 });
await assert.rejects(() => retry(async () => { throw new Error("always"); }, 3), /always/);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
