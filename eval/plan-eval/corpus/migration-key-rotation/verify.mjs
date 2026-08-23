#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { verify } = await load("lib/auth.js");
const { rotate } = await load("lib/keystore.js");
const legacy = { t1: "secret-one" };
assert.equal(verify(legacy, "t1", "secret-one"), true);
const rotated = { ...rotate(legacy, "salt1"), __salt: "salt1" };
assert.ok(rotated.t1.startsWith("sha256:"));
assert.equal(verify(rotated, "t1", "secret-one"), true);
assert.equal(verify(rotated, "t1", "wrong"), false);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
