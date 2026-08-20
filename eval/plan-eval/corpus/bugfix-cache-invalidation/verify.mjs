#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { loadSession, saveSession } = await load("lib/session.js");
const { cache } = await load("lib/cache.js");
saveSession("a", { user: 1 });
assert.deepEqual(loadSession("a"), { user: 1 });
saveSession("a", { user: 2 });
assert.deepEqual(loadSession("a"), { user: 2 });
assert.equal(cache.size(), 1);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
