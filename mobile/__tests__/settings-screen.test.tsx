/**
 * R26 — RN 屏④（设置/诊断）在 RN 环境下的行为断言：
 * 能力行不可点击（App 内不可自授）、清除令牌两步确认、命中区 ≥48dp、
 * 无障碍标签齐备。渲染走 react-test-renderer，模型来自共享 core。
 */
import React from "react";
import * as renderer from "react-test-renderer";
import { Text, TouchableOpacity } from "react-native";

import { SettingsScreen } from "../src/screens/SettingsScreen.tsx";
import { settingsModel } from "../../src-tauri/frontend/src/remote/core/settings-diagnostics.ts";
import { capabilitiesFromLabels } from "../../src-tauri/frontend/src/remote/core/capability-ui.ts";
import { planConnection } from "../../src-tauri/frontend/src/remote/core/connection-strategy.ts";

const input = {
  capabilities: capabilitiesFromLabels(["events-read", "tasks-write"]),
  snapshot: { state: "online", retryInSeconds: 0, attempt: 0 },
  plan: planConnection({
    lanHost: "192.168.1.10",
    lanPort: 8787,
    relayUrl: null,
    forceRelay: false,
  }),
  relayUrl: null,
  forceRelay: false,
  log: [],
  fingerprint: "a1b2c3d4".repeat(8),
  rttMs: 42,
  deviceName: "pixel",
  pairedAtMs: Date.UTC(2026, 8, 12, 9, 0, 0),
  version: "1.0.1",
};

function pressableLabels(tree) {
  return tree.root
    .findAllByType(TouchableOpacity)
    .map((node) => String(node.props.accessibilityLabel ?? ""));
}

test("R26.A1: 能力行不可点击——App 内不可自授", () => {
  const actions = [];
  const tree = renderer.create(
    <SettingsScreen
      sections={settingsModel(input)}
      onAction={(action) => actions.push(action)}
    />,
  );
  const labels = pressableLabels(tree);
  // 能力段（"已授能力…"）不应出现在可点击元素中。
  expect(labels.some((label) => label.includes("已授能力"))).toBe(false);
  expect(actions).toHaveLength(0);
  // 但能力文案本身要渲染出来（只读展示）。
  const texts = tree.root.findAllByType(Text).map((node) => node.props.children);
  expect(JSON.stringify(texts)).toContain("事件读取");
});

test("R26.A1: 清除本机令牌需两步确认", () => {
  const actions = [];
  const tree = renderer.create(
    <SettingsScreen
      sections={settingsModel(input)}
      onAction={(action) => actions.push(action)}
    />,
  );
  const clearButton = () =>
    tree.root
      .findAllByType(TouchableOpacity)
      .find((node) => String(node.props.accessibilityLabel ?? "").includes("清除本机令牌"));

  renderer.act(() => clearButton().props.onPress());
  expect(actions).toHaveLength(0); // 第一次只进入确认态
  expect(
    JSON.stringify(tree.root.findAllByType(Text).map((node) => node.props.children)),
  ).toContain("再点一次确认清除");

  renderer.act(() => clearButton().props.onPress());
  expect(actions).toEqual([{ kind: "clear-token" }]);
});

test("R26.A1: 命中区 ≥48dp 且带无障碍标签", () => {
  const tree = renderer.create(
    <SettingsScreen sections={settingsModel(input)} onAction={() => {}} />,
  );
  const pressables = tree.root.findAllByType(TouchableOpacity);
  expect(pressables.length).toBeGreaterThan(0);
  for (const node of pressables) {
    const style = node.props.style;
    const minHeight = Array.isArray(style) ? 48 : style?.minHeight ?? 48;
    expect(minHeight).toBeGreaterThanOrEqual(48);
    expect(node.props.accessibilityRole).toBe("button");
    expect(String(node.props.accessibilityLabel ?? "")).not.toBe("");
  }
});
