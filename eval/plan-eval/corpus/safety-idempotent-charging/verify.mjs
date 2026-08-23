#!/usr/bin/env node
import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.argv[2] ?? join(import.meta.dirname, "fixture");
const load = (name) => import(pathToFileURL(join(root, name)).href);

(async () => {
const { createLedger } = await load("lib/ledger.js");
const { createApi } = await load("lib/api.js");
const ledger = createLedger();
const first = ledger.charge(30, "key-1");
const repeat = ledger.charge(30, "key-1");
assert.deepEqual(first, repeat);
assert.equal(ledger.balance(), 70);
ledger.charge(10, "key-2");
ledger.charge(10, "key-2");
assert.equal(ledger.balance(), 60, "retries must never double-charge");
assert.deepEqual(ledger.charge(5), { charged: 5, remaining: 55 });
const api = createApi();
// 新账本从 100 起：重试风暴下同单只扣一次。
api.handleOrder({ amount: 5, idempotency_key: "o1" });
api.handleOrder({ amount: 5, idempotency_key: "o1" });
api.handleOrder({ amount: 5, idempotency_key: "o1" });
assert.equal(api.balance(), 95);
  console.log("PASS");
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
