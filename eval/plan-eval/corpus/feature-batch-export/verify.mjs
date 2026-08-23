#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { makeRepo } = await load("lib/repo.js");
const repo = makeRepo();
repo.add({ name: "a,b", score: 1 });
repo.add({ name: 'say "hi"', score: 2 });
const csv = repo.exportAs("csv");
assert.ok(csv.startsWith("name,score"));
assert.ok(csv.includes('"a,b"'));
const jsonl = repo.exportAs("jsonl");
assert.equal(jsonl.split("\n").length, 2);
assert.throws(() => repo.exportAs("xml"), /unknown format/);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
