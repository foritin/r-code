#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { makeServer } = await load("lib/server.js");
const { CONFIG } = await load("lib/config.js");
assert.deepEqual(CONFIG.rateLimit, { windowMs: 60000, max: 100 });
const server = makeServer({ rateLimit: { windowMs: 1000, max: 2 } });
server.on("/ping", () => ({ status: 200 }));
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 200);
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 200);
assert.equal((await server.handleRequest("/ping", "u1", null)).status, 429);
assert.equal((await server.handleRequest("/ping", "u2", null)).status, 200);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
