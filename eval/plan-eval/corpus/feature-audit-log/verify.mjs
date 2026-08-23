#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { makeAccount, transfer } = await load("lib/account.js");
const { audit } = await load("lib/audit.js");
const alice = { id: "alice", ...makeAccount(100) };
const bob = { id: "bob", ...makeAccount(0) };
transfer(alice, bob, 40, "2026-08-21T00:00:00Z");
transfer(alice, bob, 10, "2026-08-21T00:01:00Z");
assert.equal(alice.balance, 50);
assert.equal(audit.byAction("transfer").length, 2);
assert.equal(audit.byAction("transfer")[0].amount, 40);
audit.byAction("transfer")[0].amount = 9999;
assert.equal(audit.byAction("transfer")[0].amount, 40, "entries must be copies");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
