import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { replaceWorkspaceVersion, stampChangelog } from "./release.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("replaceWorkspaceVersion only changes the workspace package version", () => {
  const input = `[workspace]\nresolver = "2"\n\n[workspace.package]\nversion = "0.1.0"\n\n[dependencies]\nexample = "0.1.0"\n`;
  const actual = replaceWorkspaceVersion(input, "0.2.0");
  assert.match(actual, /\[workspace\.package\]\nversion = "0\.2\.0"/);
  assert.match(actual, /example = "0\.1\.0"/);
});

test("stampChangelog moves Unreleased notes into a dated release", () => {
  const input = `# Changelog\n\n## [Unreleased]\n\n### Added\n\n- First public build.\n`;
  const actual = stampChangelog(input, "0.1.0", "https://github.com/foritin/r-code");
  assert.match(actual, /^## \[Unreleased\]\n\n## \[0\.1\.0\] - \d{4}-\d{2}-\d{2}$/m);
  assert.match(actual, /- First public build\./);
  assert.match(actual, /^\[Unreleased\]: .*\/compare\/v0\.1\.0\.\.\.HEAD$/m);
  assert.match(actual, /^\[0\.1\.0\]: .*\/releases\/tag\/v0\.1\.0$/m);
});

test("stampChangelog keeps older release links and advances the comparison base", () => {
  const input = `# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- A regression.\n\n## [0.1.0] - 2026-07-01\n\n### Added\n\n- Initial release.\n\n[Unreleased]: https://github.com/foritin/r-code/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/foritin/r-code/releases/tag/v0.1.0\n`;
  const actual = stampChangelog(input, "0.1.1", "https://github.com/foritin/r-code");
  assert.match(actual, /^## \[0\.1\.1\] - \d{4}-\d{2}-\d{2}$/m);
  assert.match(actual, /^\[0\.1\.0\]: .*\/releases\/tag\/v0\.1\.0$/m);
  assert.match(actual, /^\[Unreleased\]: .*\/compare\/v0\.1\.1\.\.\.HEAD$/m);
  assert.match(actual, /^\[0\.1\.1\]: .*\/releases\/tag\/v0\.1\.1$/m);
  assert.equal((actual.match(/^\[Unreleased\]:/gm) ?? []).length, 1);
});

test("macOS packaging keeps native window chrome and app/dmg targets", () => {
  const config = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "src-tauri", "tauri.macos.conf.json"), "utf8"),
  );
  const window = config.app.windows[0];

  assert.equal(config.identifier, "com.rcode.desktop");
  assert.equal(window.decorations, true);
  assert.equal(window.titleBarStyle, "Overlay");
  assert.equal(window.hiddenTitle, true);
  assert.deepEqual(config.bundle.targets, ["app", "dmg"]);
  assert.equal(config.bundle.macOS.minimumSystemVersion, "11.0");
});

test("macOS local builder supports explicit ad-hoc and notarized modes", () => {
  const script = fs.readFileSync(path.join(repoRoot, "scripts", "build-macos.sh"), "utf8");

  assert.match(script, /aarch64-apple-darwin/);
  assert.match(script, /APPLE_SIGNING_IDENTITY/);
  assert.match(script, /APPLE_ID/);
  assert.match(script, /APPLE_API_KEY/);
  assert.match(script, /cargo tauri build --bundles app,dmg --target/);
  assert.match(script, /codesign --verify --deep --strict/);
  assert.match(script, /xcrun stapler validate/);
  assert.doesNotMatch(script, /find .*maxdepth/, "macOS BSD find does not support -maxdepth");
});

test("release workflow refuses unsigned macOS releases and verifies notarization", () => {
  const workflow = fs.readFileSync(
    path.join(repoRoot, ".github", "workflows", "release.yml"),
    "utf8",
  );

  assert.match(workflow, /platform: macos-latest[\s\S]*args: "--bundles app,dmg"/);
  for (const secret of [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_SIGNING_IDENTITY",
    "APPLE_ID",
    "APPLE_PASSWORD",
    "APPLE_TEAM_ID",
  ]) {
    assert.match(workflow, new RegExp(`secrets\\.${secret}`));
  }
  assert.match(workflow, /Import Apple Developer ID certificate/);
  assert.match(workflow, /Verify signed and notarized macOS bundles/);
  assert.match(workflow, /spctl --assess --type execute/);
  assert.match(workflow, /xcrun stapler validate/);
  assert.doesNotMatch(workflow, /find .*maxdepth/, "macOS BSD find does not support -maxdepth");
});
