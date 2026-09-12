/**
 * R19.A3 — 连接策略 node-test：直连优先、失败回落中继、强制中继、
 * 无端点诚实报错、吊销清缓存。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  planConnection,
  nextAfterFailure,
  invalidateEndpoints,
} from "./connection-strategy.ts";

test("R19.A3: 同网段直连优先，中继作为回落", () => {
  const plan = planConnection({
    lanHost: "192.168.1.10",
    lanPort: 8443,
    relayUrl: "relay.example.com:8787",
    forceRelay: false,
  });
  assert.deepEqual(plan.order, ["lan", "relay"]);
  assert.equal(plan.reason, "direct-preferred");
});

test("R19.A3: 直连失败回落中继；中继也失败才报错", () => {
  const plan = planConnection({
    lanHost: "192.168.1.10",
    lanPort: 8443,
    relayUrl: "relay.example.com:8787",
    forceRelay: false,
  });
  const afterLanFail = nextAfterFailure(plan, "lan");
  assert.deepEqual(afterLanFail.order, ["relay"]);
  assert.equal(afterLanFail.reason, "relay-fallback");
  const afterRelayFail = nextAfterFailure(afterLanFail, "relay");
  assert.deepEqual(afterRelayFail.order, []);
});

test("R19.A3: 强制中继跳过直连；无中继配置时诚实拒绝", () => {
  const forced = planConnection({
    lanHost: "192.168.1.10",
    lanPort: 8443,
    relayUrl: "relay.example.com:8787",
    forceRelay: true,
  });
  assert.deepEqual(forced.order, ["relay"]);
  assert.equal(forced.reason, "force-relay");
  const forcedNoRelay = planConnection({
    lanHost: "192.168.1.10",
    lanPort: 8443,
    relayUrl: null,
    forceRelay: true,
  });
  assert.deepEqual(forcedNoRelay.order, []);
  assert.equal(forcedNoRelay.reason, "no-endpoints");
});

test("R19.A3: 只配中继（外网/蜂窝场景）走 relay-fallback", () => {
  const plan = planConnection({
    lanHost: null,
    lanPort: null,
    relayUrl: "relay.example.com:8787",
    forceRelay: false,
  });
  assert.deepEqual(plan.order, ["relay"]);
  assert.equal(plan.reason, "relay-fallback");
});

test("R19.A3: 设备吊销清空端点缓存（纵深防御的客户端侧）", () => {
  const state = { lanHost: "192.168.1.10", relayUrl: "relay.example.com:8787" };
  invalidateEndpoints(state);
  assert.equal(state.lanHost, null);
  assert.equal(state.relayUrl, null);
});
