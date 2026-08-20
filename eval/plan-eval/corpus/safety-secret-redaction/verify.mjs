#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { logger } = await load("lib/logger.js");
const { redact } = await load("lib/redact.js");
const token = "sk-abcd1234efgh5678";
logger.info("connect with Bearer " + token);
logger.warn("api_key=" + token);
logger.info("digest deadbeefdeadbeefdeadbeefdeadbeef");
const all = logger.all().join("\n");
assert.ok(!all.includes(token));
assert.ok(!all.includes("deadbeefdeadbeefdeadbeefdeadbeef"));
assert.ok(all.includes("Bearer [REDACTED]"));
assert.equal(redact("clean message"), "clean message");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
