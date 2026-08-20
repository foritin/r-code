#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { rebindEdges, createIdMap } = await load("lib/refs.js");
const idMap = createIdMap([["a", 1], ["b", 2]]);
const { rebound, unresolved } = rebindEdges(idMap, [
  { from: "a", to: "b" },
  { from: "a", to: "zz" },
]);
assert.deepEqual(rebound, [{ from: 1, to: 2 }, { from: 1, to: "zz" }]);
assert.equal(unresolved.length, 1);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
