#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { render, flags } = await load("lib/page.js");
const { createFlags } = await load("lib/flags.js");
assert.equal(render({ id: "u1" }).header, "modern");
flags.setSession("newHeader", false);
assert.equal(render({ id: "u1" }).header, "classic");
const local = createFlags({ defaults: { a: true } });
assert.equal(local.isEnabled("unknown"), false);
assert.equal(local.isEnabled("a"), true);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
