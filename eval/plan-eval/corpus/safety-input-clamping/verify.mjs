#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { setQuota } = await load("lib/quota.js");
const { registerTenant } = await load("lib/tenant.js");
assert.equal(setQuota({ quota: 1 }, -50).quota, 0);
assert.equal(setQuota({ quota: 1 }, 999999).quota, 10000);
assert.equal(setQuota({ quota: 1 }, 42.9).quota, 42);
assert.throws(() => setQuota({ quota: 1 }, Number.NaN), /finite/);
assert.throws(() => setQuota({ quota: 1 }, Infinity), /finite/);
assert.equal(registerTenant({ id: "t" }, 123456).limits.quota, 10000);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
