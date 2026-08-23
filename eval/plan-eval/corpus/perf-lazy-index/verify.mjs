#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { makeCatalog } = await load("lib/catalog.js");
const { buildShelf } = await load("lib/shelf.js");
const catalog = makeCatalog([{ id: 1, tags: ["a"] }, { id: 2, tags: ["a", "b"] }]);
assert.equal(catalog.indexBuilt(), false, "index must be lazy");
assert.deepEqual(catalog.findByTag("a"), [1, 2]);
assert.equal(catalog.indexBuilt(), true);
const shelf = buildShelf([
  [{ id: 1, tags: ["x"] }],
  [{ id: 2, tags: ["y"] }],
]);
assert.deepEqual(shelf[0].findByTag("x"), [1]);
assert.equal(shelf[1].indexBuilt(), false, "unqueried catalogs stay unbuilt");
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
