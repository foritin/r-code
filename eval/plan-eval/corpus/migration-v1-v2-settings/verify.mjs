#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { loadSettings } = await load("lib/store.js");
const { migrateSettings } = await load("lib/migrate.js");
const migrated = loadSettings('{"version":1,"theme":"dark","notify_email":true,"notify_push":false}');
assert.equal(migrated.version, 2);
assert.deepEqual(migrated.notify, { email: true, push: false });
assert.equal(migrated.theme, "dark");
assert.ok(!("notify_email" in migrated));
const already = migrateSettings({ version: 2, notify: { email: true, push: true } });
assert.deepEqual(already.notify, { email: true, push: true });
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
