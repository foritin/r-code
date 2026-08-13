import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => fs.readFileSync(path.join(frontendDir, relative), "utf8");

test("companion is a separate native-window entry with a narrow capability", () => {
  const app = read("src/App.tsx");
  const main = read("src/main.tsx");
  const controller = read("src/components/companion/CompanionWindowController.tsx");
  const rustMain = read("../src/main.rs");
  const capability = JSON.parse(read("../capabilities/companion.json"));

  assert.doesNotMatch(app, /<CompanionHost\s*\/>/);
  assert.match(app, /<CompanionWindowController\s*\/>/);
  assert.match(main, /window\.location\.search/);
  assert.match(main, /<CompanionWindow\s*\/>/);
  assert.match(controller, /COMPANION_NAVIGATE_EVENT/);
  assert.match(controller, /openRoom\(taskId\)/);
  assert.match(controller, /mainWindow\.show\(\)/);
  assert.match(controller, /await sendCompanionPreferences\(companionPreferenceSnapshot\(\)\)/);
  assert.match(rustMain, /setup_companion_window/);
  assert.match(rustMain, /always_on_top\(true\)/);
  assert.match(rustMain, /visible_on_all_workspaces\(true\)/);
  assert.match(rustMain, /devtools\(false\)/);
  assert.match(rustMain, /RunEvent::Reopen/);
  assert.match(rustMain, /failed to close companion with the main window/);
  assert.deepEqual(capability.windows, ["companion"]);
  assert.ok(capability.permissions.includes("core:window:allow-start-dragging"));
  assert.ok(capability.permissions.includes("core:menu:allow-popup"));
  assert.ok(!capability.permissions.includes("opener:default"));
  assert.ok(!capability.permissions.includes("updater:default"));
});

test("session assistant exposes progress, unread, native drag and one-item close menu", () => {
  const component = read("src/components/companion/CompanionWindow.tsx");
  const settings = read("src/components/scenes/SettingsScene.tsx");
  const store = read("src/store/companion.ts");

  assert.match(component, /appWindow\.startDragging\(\)/);
  assert.match(component, /event\.button !== 0 \|\| !event\.isPrimary \|\| event\.ctrlKey/);
  assert.match(component, /event\.preventDefault\(\)/);
  assert.match(component, /text: "关闭小助手"/);
  assert.doesNotMatch(component, /重新载入|检查元素|Reload|Inspect/);
  assert.match(component, /COMPANION_NAVIGATE_EVENT/);
  assert.match(component, /attempt < 20/);
  assert.match(component, /companion-unread-badge/);
  assert.match(component, /pendingPermissionCount/);
  assert.match(component, /initializedTasks/);
  assert.match(component, /playCue/);
  assert.match(settings, /独立悬浮在其他应用之上/);
  assert.match(settings, /左键查看最近任务并跳转到对应会话/);
  assert.match(store, /r-code\.companion\.preferences\.v2/);
  assert.match(store, /revision/);
});

test("six progress moods plus singing and dancing have compositor animation and reduced-motion coverage", () => {
  const component = read("src/components/companion/CompanionWindow.tsx");
  const css = read("src/styles/companion.css");
  for (const mood of ["idle", "working", "attention", "success", "error", "review"]) {
    assert.match(component, new RegExp(`${mood}:`));
    assert.match(css, new RegExp(`sprite-state-${mood}`));
  }
  for (const animation of ["Idle", "Working", "Attention", "Success", "Error", "Review"]) {
    assert.match(css, new RegExp(`@keyframes sessionAssistant${animation}`));
  }
  for (const performance of ["sing", "dance"]) {
    assert.match(component, new RegExp(`"${performance}"`));
    assert.match(css, new RegExp(`sprite-state-${performance}`));
  }
  assert.match(css, /@keyframes sessionAssistantSing/);
  assert.match(css, /@keyframes sessionAssistantDance/);
  assert.match(css, /background-size: 400% 200%/);
  assert.doesNotMatch(component, /onPointerMove/);
  assert.doesNotMatch(component, /setInterval\(/);
  assert.match(component, /reconciliationInFlight/);
  assert.match(component, /active \? 5_000 : 20_000/);
  assert.match(component, /detail\.task\.updated_at !== task\.updated_at/);
  assert.match(component, /task\.state === "in_progress"/);
  assert.match(component, /if \(ids\.length\) await refreshDetails\(ids\)/);
  assert.match(component, /after movement settles/);
  assert.match(component, /setTimeout\(close, 1_000\)/);
  assert.match(component, /context\.state !== "running"/);
  assert.match(component, /Keep the candidate alive until/);
  assert.match(component, /performanceCooldownUntil/);
  assert.match(component, /next === "sing" \? 2_600 : 3_600/);
  assert.match(component, /companion-frame-layer is-leaving/);
  assert.match(css, /companion-frame-layer\.is-leaving/);
  assert.doesNotMatch(css, /companion-sprite-frame\.is-leaving/);
  assert.doesNotMatch(component, /requestAnimationFrame loop[^\n]*\n[^\n]*requestAnimationFrame\(/);
  assert.match(component, /document\.visibilityState === "hidden"/);
  assert.match(component, /shouldReduceMotion\(motion\)/);
  assert.match(css, /translateY\(-16px\)/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(css, /:not\(\.motion-full\)/);
  assert.match(css, /motion-reduced \.companion-sprite-frame/);
});

test("generated virtual-singer sprite is an RGBA 4 by 2 state and performance sheet", () => {
  const sprite = fs.readFileSync(path.join(
    frontendDir,
    "src/assets/companion/r-code-session-assistant-v2.png",
  ));
  assert.equal(sprite.subarray(1, 4).toString(), "PNG");
  assert.equal(sprite.readUInt32BE(16), 1536);
  assert.equal(sprite.readUInt32BE(20), 1024);
  assert.equal(sprite[25], 6);
});
