#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { resolveInside } = await load("lib/paths.js");
const root = process.cwd();
assert.ok(resolveInside(root, "a/b.txt").startsWith(root));
assert.equal(resolveInside(root, "../secret.txt"), null);
assert.equal(resolveInside(root, "a/../../secret.txt"), null);
assert.ok(resolveInside(root, ".").startsWith(root));
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
