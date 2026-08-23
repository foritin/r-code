import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => fs.readFileSync(path.join(frontendDir, relative), "utf8");

function losslessWebpDimensions(buffer) {
  let offset = 12;
  while (offset + 8 <= buffer.length) {
    const kind = buffer.subarray(offset, offset + 4).toString();
    const size = buffer.readUInt32LE(offset + 4);
    const data = offset + 8;
    if (kind === "VP8L") {
      assert.equal(buffer[data], 0x2f, "invalid VP8L signature");
      const bits = buffer.readUInt32LE(data + 1);
      return {
        width: (bits & 0x3fff) + 1,
        height: ((bits >>> 14) & 0x3fff) + 1,
      };
    }
    offset = data + size + (size & 1);
  }
  throw new Error("lossless WebP dimensions not found");
}

test("companion is a separate native-window entry with a narrow capability", () => {
  const app = read("src/App.tsx");
  const main = read("src/main.tsx");
  const controller = read("src/components/companion/CompanionWindowController.tsx");
  const bridge = read("src/components/companion/bridge.ts");
  const rustMain = read("../src/main.rs");
  const capability = JSON.parse(read("../capabilities/companion.json"));

  assert.doesNotMatch(app, /<CompanionHost\s*\/>/);
  assert.match(app, /<CompanionWindowController\s*\/>/);
  assert.match(main, /window\.location\.search/);
  assert.match(main, /<CompanionWindow\s*\/>/);
  assert.match(main, /prepareNativeCompanionWindow\(\)/);
  assert.match(main, /<NativeCompanionWindowController\s*\/>/);
  assert.match(controller, /COMPANION_NAVIGATE_EVENT/);
  assert.match(controller, /COMPANION_PREFERENCES_APPLIED_EVENT/);
  assert.match(controller, /attachMainCompanionHandshake/);
  assert.match(controller, /setIgnoreCursorEvents/);
  assert.match(controller, /pointHitsCompanionSurface/);
  assert.match(controller, /"\.companion-avatar",/);
  // 穿透降级必须自愈：失败时 fail-open（可点可拖）但持续探活，禁止永久性 give-up。
  assert.match(controller, /degraded[\s\S]*?cursorPolicy\.applied\(\) !== false/);
  assert.doesNotMatch(controller, /clickThroughSupported/);
  // 可见性以原生 isVisible 复核为准，防止 WebView2 visibilityState 滞留 hidden 导致整窗吞点击。
  assert.match(controller, /appWindow\.isVisible\(\)/);
  // 几何基底周期重取 + 拖拽移动停顿后释放按压态 + 隐藏元素不得认领交互。
  assert.match(controller, /GEOMETRY_REFRESH_TICKS/);
  assert.match(controller, /POINTER_IDLE_RELEASE_MS/);
  assert.match(controller, /checkVisibility/);
  assert.match(controller, /attachWithRetry/);
  assert.match(controller, /visibilitychange/);
  assert.match(controller, /\.companion-pulse-more/);
  assert.match(controller, /openRoom\(taskId\)/);
  assert.match(controller, /mainWindow\.show\(\)/);
  assert.match(controller, /restoreMainWindowBestEffort/);
  assert.match(controller, /sendSnapshot: sendCompanionPreferences/);
  assert.match(controller, /readSnapshot: companionPreferenceSnapshot/);
  assert.match(bridge, /COMPANION_STARTUP_LISTENERS/);
  assert.match(bridge, /pendingCompanionPreferences/);
  assert.match(rustMain, /setup_companion_window/);
  assert.match(rustMain, /always_on_top\(true\)/);
  assert.match(rustMain, /visible_on_all_workspaces\(true\)/);
  assert.match(rustMain, /background_color\(tauri::webview::Color\(0, 0, 0, 0\)\)/);
  assert.match(rustMain, /min_inner_size\(/);
  assert.match(rustMain, /max_inner_size\(/);
  assert.match(rustMain, /maximizable\(false\)/);
  assert.match(rustMain, /devtools\(false\)/);
  assert.match(rustMain, /closable\(false\)/);
  assert.match(rustMain, /cmd_companion_ensure/);
  assert.match(rustMain, /is_companion_window/);
  assert.match(rustMain, /RunEvent::Reopen/);
  assert.match(rustMain, /_window\.app_handle\(\)\.exit\(0\)/);
  assert.deepEqual(capability.windows, ["companion"]);
  assert.ok(capability.permissions.includes("core:window:allow-start-dragging"));
  assert.ok(capability.permissions.includes("core:menu:allow-popup"));
  assert.ok(capability.permissions.includes("core:window:allow-set-ignore-cursor-events"));
  assert.ok(capability.permissions.includes("core:window:allow-cursor-position"));
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
  assert.match(settings, /悬浮显示任务运行、等待与完成状态/);
  assert.match(settings, /左键查看任务/);
  assert.match(settings, /companionEnsure/);
  assert.match(store, /r-code\.companion\.preferences\.v2/);
  assert.match(store, /revision/);
});

test("companion recovery tolerates only an exact legacy-host command mismatch", () => {
  const ipc = read("src/lib/ipc.ts");
  const settings = read("src/components/scenes/SettingsScene.tsx");
  const commands = read("../src/tauri_commands.rs");

  assert.match(ipc, /LEGACY_COMPANION_ENSURE_MISSING_ERROR\s*=\s*\n\s*"Command cmd_companion_ensure not found"/);
  assert.match(ipc, /if \(!isLegacyCompanionEnsureMissing\(cause\)\) throw cause/);
  assert.match(ipc, /using its startup-created window/);
  assert.match(commands, /get_webview_window\(crate::COMPANION_WINDOW_LABEL\)[\s\S]*?\.is_none\(\)[\s\S]*?tracing::warn![\s\S]*?return Err\("native companion window is unavailable"\.to_string\(\)\)/);
  assert.match(settings, /catch \(cause\)[\s\S]*?console\.warn\("Companion window could not be enabled\.", cause\)/);
  assert.doesNotMatch(settings, /详细信息已写入诊断日志/);
});

test("six progress moods plus singing and dancing use bounded single-plane sprite sequences", () => {
  const component = read("src/components/companion/CompanionWindow.tsx");
  const sprite = read("src/components/companion/CompanionSprite.tsx");
  const css = read("src/styles/companion.css");
  const spriteCss = read("src/styles/companion-sprite.css");
  for (const mood of ["idle", "working", "attention", "success", "error", "review"]) {
    assert.match(component, new RegExp(`${mood}:`));
    assert.match(sprite, new RegExp(`${mood}:`));
  }
  for (const performance of ["sing", "dance"]) {
    assert.match(component, new RegExp(`"${performance}"`));
    assert.match(sprite, new RegExp(`${performance}:`));
  }
  assert.match(sprite, /columns: 8/);
  assert.match(sprite, /CODEX_IDLE_TIMING = \[280, 110, 110, 140, 140, 320\]/);
  assert.match(sprite, /duration \* 6/);
  assert.match(sprite, /backgroundPosition = framePosition/);
  assert.match(sprite, /activeSequence\.durations\[frame\]/);
  assert.match(sprite, /idle:[\s\S]*?loops: Number\.POSITIVE_INFINITY/);
  assert.match(sprite, /loops: 3/);
  assert.match(sprite, /working: sequence\(7, 6, 120, 220\)/);
  assert.match(sprite, /attention: sequence\(6, 6, 150, 260\)/);
  assert.match(sprite, /success: sequence\(3, 4, 140, 280\)/);
  assert.match(sprite, /error: sequence\(5, 8, 140, 240\)/);
  assert.match(sprite, /review: sequence\(8, 6, 150, 280\)/);
  for (const gesture of ["sing", "dance"]) {
    assert.match(sprite, new RegExp(`${gesture}:[\\s\\S]*?loops: 2`));
  }
  assert.match(sprite, /sing:[\s\S]*?frames: \[0, 1, 2, 3, 4, 5, 4, 2\][\s\S]*?row: 0/);
  assert.match(sprite, /dance:[\s\S]*?frames: \[0, 1, 2, 3, 4, 5, 4, 2\][\s\S]*?row: 0/);
  assert.doesNotMatch(sprite, /r-code-miku-performances-v4/);
  assert.match(sprite, /completedLoops >= activeSequence\.loops/);
  assert.match(sprite, /activeSequence = COMPANION_SPRITE_SEQUENCES\.idle/);
  assert.match(sprite, /showFrame\(requestedSequence, 0, state\)/);
  assert.doesNotMatch(sprite, /crossfadeTo|sequence-plane|isFront|opacity:/);
  assert.match(sprite, /backgroundSize: `\$\{requestedSequence\.columns \* 100\}% \$\{requestedSequence\.rows \* 100\}%`/);
  assert.match(spriteCss, /width: 168px;\s*\n\s*height: 182px/);
  assert.match(css, /is-expanded \.companion-avatar[\s\S]*?width: 168px;[\s\S]*?height: 196px/);
  assert.match(spriteCss, /is-expanded[\s\S]*?width: 168px;[\s\S]*?height: 182px/);
  assert.match(component, /nativeLayoutInFlight/);
  assert.match(component, /suppressNativeMovesUntil/);
  assert.match(component, /actual window position is authoritative/);
  assert.match(component, /placementAroundAvatar/);
  assert.match(component, /integerPhysicalPosition/);
  assert.match(component, /restoredAnchorForMonitor/);
  assert.match(component, /monitor\.scaleFactor/);
  assert.match(component, /Math\.round\(position\.x\)/);
  assert.doesNotMatch(component, /setPosition\(new PhysicalPosition/);
  assert.match(component, /Companion position could not be restored; showing at its current position/);
  assert.match(component, /await appWindow\.show\(\)/);
  assert.match(component, /roomLeft >= deltaX \|\| roomLeft >= roomRight/);
  assert.match(component, /roomAbove >= deltaY \|\| roomAbove >= roomBelow/);
  assert.match(component, /avatarAnchorFromWindow/);
  assert.match(css, /has-tracking:not\(\.is-expanded\)\.panel-right \.companion-avatar/);
  assert.doesNotMatch(css, /has-tracking:not\(\.is-expanded\)\.avatar-top \.companion-avatar/);
  assert.match(css, /\.companion-window-root\.is-mini \.companion-tracking-toggle\s*\{[\s\S]*?width: 28px/);
  assert.match(component, /companionFootprint/);
  assert.match(component, /PULSE_ROW_HEIGHT = 88/);
  assert.match(component, /PULSE_ROW_STRIDE = 96/);
  assert.match(component, /title=\{session\.task\.title\}/);
  assert.match(css, /-webkit-line-clamp: 2/);
  assert.match(component, /MAX_PULSE_SESSIONS = 2/);
  assert.match(component, /trackedSessions\.slice\(0, MAX_PULSE_SESSIONS\)/);
  assert.match(component, /hiddenPulseCount/);
  assert.match(component, /还有 \$\{hiddenPulseCount\} 个任务 · 查看全部/);
  assert.match(component, /trackingShowingAll/);
  assert.match(component, /全部 \$\{trackingCount\} 个任务 · 收起/);
  assert.match(component, /aria-expanded=\{showingAllPulseSessions\}/);
  assert.match(component, /badgeCount = unreadCount > 0 \? trackingCount : 0/);
  assert.match(component, /companion-tracking-toggle/);
  assert.match(component, /个任务正在运行，展开 Session 追踪/);
  assert.match(component, /useEffect\(\(\) => \{\s*panelOpenRef\.current = panelOpen;\s*\}, \[panelOpen\]\)/);
  assert.doesNotMatch(spriteCss, /transition:\s*opacity|drop-shadow/);
  assert.doesNotMatch(css, /companionFrameEnter|companionFrameLeave/);
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
  assert.match(component, /next === "sing"/);
  assert.match(component, /<CompanionSprite/);
  assert.doesNotMatch(component, /requestAnimationFrame loop[^\n]*\n[^\n]*requestAnimationFrame\(/);
  assert.match(component, /document\.visibilityState === "hidden"/);
  assert.match(component, /shouldReduceMotion\(motion\)/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(css, /:not\(\.motion-full\)/);
  assert.match(sprite, /motion === "reduced"/);
});

test("pet button suppresses the global rectangular focus card and only accents its aura", () => {
  const css = read("src/styles/companion.css");
  const spriteCss = read("src/styles/companion-sprite.css");

  assert.match(css, /\.companion-avatar:focus,\s*\n\.companion-avatar:focus-visible\s*{/);
  for (const reset of [
    /border: 0 !important/,
    /border-radius: 0 !important/,
    /outline: 0 !important/,
    /background: transparent !important/,
    /box-shadow: none !important/,
    /appearance: none/,
  ]) {
    assert.match(css, reset);
  }
  assert.match(css, /\.companion-avatar:focus-visible \.companion-aura\s*{/);
  assert.doesNotMatch(css, /\.companion-avatar:focus-visible::before/);
  assert.doesNotMatch(css, /drop-shadow/);
  assert.doesNotMatch(spriteCss, /drop-shadow/);
});

test("registered virtual-singer atlases use Codex cells and lossless transparency", () => {
  const assets = [
    ["r-code-miku-v4.webp", { width: 1536, height: 1872 }],
  ];
  for (const [name, expected] of assets) {
    const sprite = fs.readFileSync(path.join(frontendDir, "src/assets/companion", name));
    assert.equal(sprite.subarray(0, 4).toString(), "RIFF");
    assert.equal(sprite.subarray(8, 12).toString(), "WEBP");
    assert.ok(sprite.includes(Buffer.from("VP8L")), `${name} must be lossless RGBA WebP`);
    assert.deepEqual(losslessWebpDimensions(sprite), expected);
  }
});
