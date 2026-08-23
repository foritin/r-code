#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { mergeConfig } = await load("lib/config.js");
const { servicePort } = await load("lib/service.js");
assert.equal(servicePort(undefined), 8080);
assert.equal(servicePort({}), 8080);
assert.equal(servicePort({ log: { level: "debug" } }), 8080);
assert.equal(servicePort({ server: { host: "127.0.0.1" } }), 8080);
assert.equal(servicePort({ server: { port: 9000 } }), 9000);
// 显式 null 保留：server 被整体置空，读取方按缺数据处理。
assert.equal(mergeConfig({ server: null }).server, null);
assert.throws(() => servicePort({ server: null }));
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
