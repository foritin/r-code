import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const stylesDir = path.join(frontendDir, "src", "styles");
const tokensPath = path.join(stylesDir, "tokens.css");
const roomComponents = [
  path.join(frontendDir, "src", "components", "room", "Canvas.tsx"),
  path.join(frontendDir, "src", "components", "room", "SubagentWorkbench.tsx"),
];

function filesBelow(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const entryPath = path.join(directory, entry.name);
    return entry.isDirectory() ? filesBelow(entryPath) : [entryPath];
  });
}

function withoutComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\r\n]/g, " "));
}

function explicitPixelSizes(value) {
  const sizes = [];
  for (const match of value.matchAll(/(-?(?:\d+\.?\d*|\.\d+))(px|rem|em)\b/gi)) {
    const amount = Number(match[1]);
    const unit = match[2].toLowerCase();
    sizes.push({ source: match[0], pixels: unit === "px" ? amount : amount * 16 });
  }
  if (/^\s*0\s*$/.test(value)) sizes.push({ source: "0", pixels: 0 });
  return sizes;
}

function cssBlock(source, selectorPattern) {
  const match = source.match(new RegExp(`${selectorPattern}\\s*\\{([^}]*)\\}`, "s"));
  assert.ok(match, `missing CSS block ${selectorPattern}`);
  return match[1];
}

function customProperty(block, name) {
  const match = block.match(new RegExp(`(?:^|\\n)\\s*${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*:\\s*([^;]+);`));
  assert.ok(match, `missing token ${name}`);
  return match[1].trim();
}

function pixelToken(block, name) {
  const value = customProperty(block, name);
  const match = value.match(/^(-?(?:\d+\.?\d*|\.\d+))px$/i);
  assert.ok(match, `${name} must be a stable px value, received ${value}`);
  return Number(match[1]);
}

function rgb(hex) {
  const normalized = hex.trim().replace(/^#/, "");
  assert.match(normalized, /^(?:[0-9a-f]{3}|[0-9a-f]{6})$/i, `expected a solid hex color, received ${hex}`);
  const expanded = normalized.length === 3
    ? [...normalized].map((digit) => `${digit}${digit}`).join("")
    : normalized;
  return [0, 2, 4].map((offset) => Number.parseInt(expanded.slice(offset, offset + 2), 16));
}

function relativeLuminance(hex) {
  const channels = rgb(hex).map((channel) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

function contrastRatio(foreground, background) {
  const foregroundLuminance = relativeLuminance(foreground);
  const backgroundLuminance = relativeLuminance(background);
  const lighter = Math.max(foregroundLuminance, backgroundLuminance);
  const darker = Math.min(foregroundLuminance, backgroundLuminance);
  return (lighter + 0.05) / (darker + 0.05);
}

test("explicit font-size declarations in UI styles never fall below 11px", () => {
  const cssFiles = filesBelow(stylesDir).filter((file) => file.endsWith(".css"));
  assert.ok(cssFiles.length > 0, "expected frontend CSS files to scan");
  const violations = [];

  for (const file of cssFiles) {
    const source = withoutComments(fs.readFileSync(file, "utf8"));
    for (const match of source.matchAll(/font-size\s*:\s*([^;}]+)/gi)) {
      const line = source.slice(0, match.index).split(/\r?\n/).length;
      for (const size of explicitPixelSizes(match[1])) {
        if (size.pixels < 11) {
          violations.push(`${path.relative(frontendDir, file)}:${line} (${size.source})`);
        }
      }
    }
  }

  assert.deepEqual(violations, [], `font-size below 11px:\n${violations.join("\n")}`);
});

test("typography tokens keep metadata and body copy above their readability floors", () => {
  const tokens = fs.readFileSync(tokensPath, "utf8");
  const root = cssBlock(tokens, ":root");

  assert.ok(pixelToken(root, "--text-meta") >= 11, "--text-meta must be at least 11px");
  assert.ok(pixelToken(root, "--text-base") >= 13, "--text-base must be at least 13px");
});

test("dark faint text reaches WCAG AA contrast against the app and panel backgrounds", () => {
  const tokens = fs.readFileSync(tokensPath, "utf8");
  const dark = cssBlock(tokens, ":root\\[data-theme=['\"]obsidian['\"]\\]");
  const foreground = customProperty(dark, "--fg-faint");
  for (const backgroundToken of ["--bg-app", "--bg-panel"]) {
    const background = customProperty(dark, backgroundToken);
    const ratio = contrastRatio(foreground, background);
    assert.ok(
      ratio >= 4.5,
      `--fg-faint ${foreground} on ${backgroundToken} ${background} has ${ratio.toFixed(3)}:1 contrast`,
    );
  }
});

test("task workbench launchers use the task-tool label instead of the extension label", () => {
  for (const file of roomComponents) {
    const source = fs.readFileSync(file, "utf8");
    const label = path.basename(file);
    assert.ok(!source.includes("新增扩展"), `${label} must not expose the old extension label`);
    assert.ok(source.includes('aria-label="打开任务工具"'), `${label} needs the task-tool aria label`);
    assert.ok(source.includes('title="打开任务工具"'), `${label} needs the task-tool title`);
  }
});
