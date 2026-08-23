#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { runPipeline } = await load("lib/pipeline.js");
const { createWriter } = await load("lib/writer.js");
const batches = [];
const writer = runPipeline([1, 2, 3, 4, 5], (batch) => batches.push([...batch]));
assert.deepEqual(batches, [[1, 2, 3, 4, 5]], "single batch keeps order");
assert.equal(writer.flushCount(), 1);
runPipeline([], () => {});
const manual = createWriter(() => {});
manual.writeEach([1]);
manual.flushBuffer();
manual.flushBuffer();
assert.equal(manual.flushCount(), 1, "empty flush is a no-op");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
