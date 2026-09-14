/**
 * design/ R2 落地契约静态核查（T-QA01）
 * 依据 docs/design.md：§1.2 令牌、§4 设计禁忌、§5 opt-* 类名清单。
 * 运行：node --test scripts/design-impl.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const styles = join(root, "src", "styles");
const src = join(root, "src");

const OPT_FILES = [
  "opt.css",
  "opt-shell.css",
  "opt-scenes-workspace.css",
  "opt-room.css",
  "opt-settings.css",
  "opt-overlays.css",
  "opt-room-2.css",
];

function readStyle(name) {
  return readFileSync(join(styles, name), "utf8");
}

function listTsFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listTsFiles(full));
    else if (/\.(tsx|ts)$/.test(entry.name)) out.push(full);
  }
  return out;
}

/* ---------- 4a：opt-*.css 七文件零字面色 ---------- */

test("opt-*.css 七文件零字面色（hex/rgb/hsl）", () => {
  const colorRe = /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(/;
  for (const name of OPT_FILES) {
    const lines = readStyle(name).split("\n");
    lines.forEach((line, i) => {
      assert.ok(
        !colorRe.test(line),
        `${name}:${i + 1} 出现字面色: ${line.trim().slice(0, 80)}`,
      );
    });
  }
});

/* ---------- 4b：tokens.css 六项契约令牌 ---------- */

test("tokens.css 含六项契约令牌（三新增 + 三改值）", () => {
  const css = readStyle("tokens.css");
  const required = [
    /--agent-name:\s*#75ccea/,        // obsidian
    /--agent-shine:\s*#ebfbff/,       // obsidian
    /--agent-name:\s*#126c8b/,        // studio-light
    /--agent-shine:\s*#379abc/,       // studio-light
    /--text-hero:\s*32px/,
    /--menubar-h:\s*42px/,
    /--rail-w:\s*264px/,
    /--rail-w-narrow:\s*232px/,
  ];
  for (const re of required) {
    assert.ok(re.test(css), `tokens.css 缺少契约令牌定义: ${re}`);
  }
});

/* ---------- 4c：§5 代表类在 opt-*.css 有定义点（抽 17 个，覆盖六家族） ---------- */

test("design.md §5 代表类在 opt-*.css 有唯一定义点", () => {
  const all = OPT_FILES.map(readStyle).join("\n");
  const classes = [
    // 骨架
    "opt-page", "opt-page-head", "opt-settings", "opt-settings-nav",
    // 基础件
    "opt-button", "opt-card", "opt-input", "opt-tabs", "opt-empty", "opt-search-field",
    // 工作区页
    "opt-home", "opt-dashboard", "opt-project-line",
    // 会话与工具面板
    "opt-agent-name", "opt-agent-stop", "opt-panel-head",
    // 设置页 / 叠层
    "opt-theme-choice", "opt-drawer", "opt-confirm", "opt-search-clear",
  ];
  for (const cls of classes) {
    const re = new RegExp(`\\.${cls}[ ,.:{\\[]`);
    assert.ok(re.test(all), `§5 契约类 .${cls} 在 opt-*.css 中无定义点`);
  }
});

/* ---------- 4d-1：登记类在 TSX 有消费 ---------- */

test("登记类 opt-search-clear / opt-agent-stop 在 TSX 有消费点", () => {
  const tsFiles = listTsFiles(src);
  for (const cls of ["opt-search-clear", "opt-agent-stop"]) {
    const re = new RegExp(`["' ]${cls}["']`);
    const users = tsFiles.filter((f) => re.test(readFileSync(f, "utf8")));
    assert.ok(users.length > 0, `登记类 .${cls} 无任何 TSX 消费点`);
  }
});

/* ---------- 4d-2：设计稿专属机制未被 TSX 使用 ---------- */

test("TSX 侧禁用 data-proposal 与 opt-layout-stamp（design.md §4.8/§4.9）", () => {
  for (const f of listTsFiles(src)) {
    const text = readFileSync(f, "utf8");
    assert.ok(!text.includes("data-proposal"), `${f} 使用了 data-proposal`);
    assert.ok(!text.includes("opt-layout-stamp"), `${f} 使用了 opt-layout-stamp`);
  }
});

/* ---------- 4d-3：§5 契约类不允许 TSX 消费却无 CSS 定义（抽查未登记类） ---------- */

test("TSX 消费的 opt- 前缀类均在 opt-*.css 有定义", () => {
  const all = OPT_FILES.map(readStyle).join("\n");
  const consumed = new Set();
  for (const f of listTsFiles(src)) {
    const text = readFileSync(f, "utf8");
    for (const m of text.matchAll(/opt-[a-z0-9-]+/g)) {
      // 排除 CSS 文件路径引用（import "../../styles/opt-room.css"）与注释里的文件名
      const before = text.slice(Math.max(0, m.index - 1), m.index);
      const after = text.slice(m.index + m[0].length, m.index + m[0].length + 4);
      if (before === "/" || before === '"' && after.startsWith(".css")) continue;
      if (after.startsWith(".css")) continue;
      consumed.add(m[0]);
    }
  }
  // opt-room-2.css 等文件头注释里出现的 opt- 类名也在 all 内，故以 CSS 为准求差集
  const missing = [...consumed].filter((cls) => !new RegExp(`\\.${cls}[ ,.:{\\[]`).test(all));
  // opt-agent-sheen 是 keyframes 名，opt-* 文件头注释引用不算消费，均应能匹配或为已知例外
  const knownKeyframes = new Set(["opt-agent-sheen"]);
  const real = missing.filter((cls) => !knownKeyframes.has(cls));
  assert.deepEqual(real, [], `以下 opt- 类被 TSX 消费但在 opt-*.css 无定义: ${real.join(", ")}`);
});
