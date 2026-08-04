import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  parseReleaseTag,
  refreshCargoLock,
  replaceWorkspaceVersion,
  stampChangelog,
} from "./release.mjs";
import {
  parseArguments,
  platformSigningPlan,
  requiredReleaseAssets,
  requiredSecretsForTag,
  unsignedPlatformNames,
  validateReleaseRecord,
  validateUpdaterManifest,
} from "./publish-release.mjs";
import { collectComponents, createArtifacts } from "./generate-supply-chain.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("replaceWorkspaceVersion only changes the workspace package version", () => {
  const input = `[workspace]\nresolver = "2"\n\n[workspace.package]\nversion = "0.1.0"\n\n[dependencies]\nexample = "0.1.0"\n`;
  const actual = replaceWorkspaceVersion(input, "0.2.0");
  assert.match(actual, /\[workspace\.package\]\nversion = "0\.2\.0"/);
  assert.match(actual, /example = "0\.1\.0"/);
});

test("refreshCargoLock resolves dependencies so workspace versions are rewritten", () => {
  let invocation;
  refreshCargoLock((command, args, options) => {
    invocation = { command, args, options };
    return { status: 0 };
  }, "D:/example/r-code");

  assert.equal(invocation.command, "cargo");
  assert.deepEqual(invocation.args, ["metadata", "--format-version", "1"]);
  assert.equal(invocation.options.cwd, "D:/example/r-code");
  assert.deepEqual(invocation.options.stdio, ["ignore", "ignore", "inherit"]);
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
  assert.doesNotMatch(actual, /\n{3,}/, "release links must not retain surrounding blank lines");
});

test("stampChangelog preserves CRLF without creating mixed line endings", () => {
  const input = "# Changelog\r\n\r\n## [Unreleased]\r\n\r\n### Fixed\r\n\r\n- A regression.\r\n\r\n## [0.1.0] - 2026-07-01\r\n\r\n- Initial release.\r\n\r\n[Unreleased]: https://github.com/foritin/r-code/compare/v0.1.0...HEAD\r\n[0.1.0]: https://github.com/foritin/r-code/releases/tag/v0.1.0\r\n";
  const actual = stampChangelog(input, "0.1.1", "https://github.com/foritin/r-code");

  assert.equal(actual.replaceAll("\r\n", "").includes("\n"), false);
  assert.doesNotMatch(actual, /\r\r\n/);
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

test("release workflow falls back per platform while preserving explicit unsigned prereleases", () => {
  const workflow = fs.readFileSync(
    path.join(repoRoot, ".github", "workflows", "release.yml"),
    "utf8",
  );

  assert.match(workflow, /runner: macos-latest[\s\S]*rust-target: aarch64-apple-darwin/);
  assert.match(workflow, /runner: macos-15-intel[\s\S]*rust-target: x86_64-apple-darwin/);
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
  assert.match(workflow, /WINDOWS_CERTIFICATE/);
  assert.match(workflow, /Import-PfxCertificate/);
  assert.match(workflow, /signtool[\s\S]*verify/);
  assert.match(workflow, /required=\(PAT_TOKEN TAURI_SIGNING_PRIVATE_KEY\)/);
  assert.match(workflow, /windows_signed: \$\{\{ steps\.release-mode\.outputs\.windows_signed \}\}/);
  assert.match(workflow, /apple_signed: \$\{\{ steps\.release-mode\.outputs\.apple_signed \}\}/);
  assert.match(workflow, /Unsigned Windows artifacts/);
  assert.match(workflow, /Ad-hoc macOS artifacts/);
  const fallbackStep = workflow.match(
    /- name: Build and publish unsigned or Linux artifacts([\s\S]*?)(?=\n      - name:)/,
  )?.[1];
  assert.ok(fallbackStep, "unsigned fallback build step must exist");
  assert.match(fallbackStep, /needs\.validate\.outputs\.windows_signed != 'true'/);
  assert.match(fallbackStep, /needs\.validate\.outputs\.apple_signed != 'true'/);
  assert.match(fallbackStep, /APPLE_SIGNING_IDENTITY: .*&& '-' \|\| ''/);
  assert.match(fallbackStep, /args: \$\{\{ matrix\.args \}\} --target \$\{\{ matrix\.rust-target \}\}/);
  assert.match(fallbackStep, /prerelease: \$\{\{ contains\(env\.RELEASE_TAG, '-unsigned\.'\) \}\}/);
  assert.doesNotMatch(fallbackStep, /APPLE_ID|APPLE_PASSWORD|APPLE_TEAM_ID/);
  assert.doesNotMatch(fallbackStep, /tauri\.release-windows\.conf\.json|matrix\.signed_config/);
  assert.match(workflow, /signed_config: "--config tauri\.release-windows\.conf\.json"/);
  assert.match(
    workflow,
    /platform: windows-x64[\s\S]*?upload_workflow_artifacts: false[\s\S]*?rust-target: x86_64-pc-windows-msvc/,
  );
  assert.equal(
    (workflow.match(/uploadWorkflowArtifacts: \$\{\{ matrix\.upload_workflow_artifacts \}\}/g) ?? [])
      .length,
    2,
    "signed and fallback builds must use the per-platform workflow-artifact policy",
  );
  assert.match(
    workflow,
    /Build and publish platform-signed artifacts[\s\S]*?args: \$\{\{ matrix\.args \}\} \$\{\{ matrix\.signed_config \}\}/,
  );
  assert.match(workflow, /这是未签名预发布版本，仅用于测试/);
  assert.match(workflow, /RCODE_UNSIGNED_STABLE_WARNING/);
  assert.match(workflow, /此 Latest 版本包含未完成平台代码签名的安装包/);
  assert.match(workflow, /--prerelease/);
  assert.match(workflow, /--latest/);
  assert.match(workflow, /GH_REPO: \$\{\{ github\.repository \}\}/);
  assert.match(workflow, /r-code-sbom\.cdx\.json/);
  assert.match(workflow, /THIRD_PARTY_LICENSES\.md/);
  assert.match(workflow, /Verify release credentials and select signing mode/);
  assert.match(workflow, /Missing required release secrets/);
  assert.equal(
    (workflow.match(/token: \$\{\{ secrets\.PAT_TOKEN \}\}/g) ?? []).length,
    2,
    "both release jobs that clone agent-core must use the private-submodule token",
  );
  assert.equal(
    (workflow.match(/submodules: recursive/g) ?? []).length,
    2,
    "authenticated checkout must own release submodule initialization",
  );
  assert.doesNotMatch(workflow, /find .*maxdepth/, "macOS BSD find does not support -maxdepth");
});

test("release tags distinguish signed releases from numbered unsigned prereleases", () => {
  assert.deepEqual(parseReleaseTag("v0.1.0"), {
    version: "0.1.0",
    unsignedPrerelease: false,
    sequence: null,
  });
  assert.deepEqual(parseReleaseTag("v0.1.0-unsigned.1"), {
    version: "0.1.0",
    unsignedPrerelease: true,
    sequence: 1,
  });
  assert.equal(parseReleaseTag("v0.1.0-unsigned.0"), null);
  assert.equal(parseReleaseTag("v0.1.0-beta.1"), null);
  assert.equal(parseReleaseTag("0.1.0-unsigned.1"), null);
});

test("publish-release parses safe release modes and flags", () => {
  assert.deepEqual(parseArguments(["v0.2.1-unsigned.2", "--dry-run", "--no-wait"]), {
    dryRun: true,
    yes: false,
    noWait: true,
    help: false,
    tag: "v0.2.1-unsigned.2",
  });
  assert.throws(() => parseArguments(["v0.2.1-beta.1"]), /tag must be/);
  assert.throws(() => parseArguments(["v0.2.1", "--force"]), /unknown option/);
});

test("publish-release requires updater integrity secrets but treats platform certificates as optional", () => {
  const stable = requiredSecretsForTag(parseReleaseTag("v0.2.1"));
  const unsigned = requiredSecretsForTag(parseReleaseTag("v0.2.1-unsigned.1"));

  assert.deepEqual(unsigned, ["PAT_TOKEN", "TAURI_SIGNING_PRIVATE_KEY"]);
  assert.deepEqual(stable, unsigned);
});

test("publish-release selects signing independently for Windows and macOS", () => {
  const stable = parseReleaseTag("v0.2.1");
  const windowsSecrets = [
    "WINDOWS_CERTIFICATE",
    "WINDOWS_CERTIFICATE_PASSWORD",
    "WINDOWS_TIMESTAMP_URL",
  ];
  const appleSecrets = [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_SIGNING_IDENTITY",
    "APPLE_ID",
    "APPLE_PASSWORD",
    "APPLE_TEAM_ID",
  ];
  const windowsOnly = platformSigningPlan(stable, windowsSecrets);
  assert.equal(windowsOnly.windowsSigned, true);
  assert.equal(windowsOnly.appleSigned, false);
  assert.deepEqual(unsignedPlatformNames(windowsOnly), ["macOS"]);
  assert.ok(windowsOnly.missingApple.includes("APPLE_SIGNING_IDENTITY"));

  const none = platformSigningPlan(stable, []);
  assert.deepEqual(unsignedPlatformNames(none), ["Windows", "macOS"]);
  assert.ok(none.missingWindows.includes("WINDOWS_CERTIFICATE"));

  const fullySigned = platformSigningPlan(stable, [...windowsSecrets, ...appleSecrets]);
  assert.deepEqual(unsignedPlatformNames(fullySigned), []);
  assert.equal(fullySigned.windowsSigned, true);
  assert.equal(fullySigned.appleSigned, true);

  const explicitUnsigned = platformSigningPlan(
    parseReleaseTag("v0.2.1-unsigned.1"),
    [...windowsSecrets, "APPLE_SIGNING_IDENTITY"],
  );
  assert.equal(explicitUnsigned.forcedUnsigned, true);
  assert.deepEqual(unsignedPlatformNames(explicitUnsigned), ["Windows", "macOS"]);
});

test("publish-release verifies all four platform asset families", () => {
  const required = requiredReleaseAssets("0.2.1");
  assert.equal(required.length, 20);
  assert.ok(required.includes("R-Code_0.2.1_x64-installer.exe"));
  assert.ok(required.includes("R-Code_0.2.1_aarch64.dmg"));
  assert.ok(required.includes("R-Code_0.2.1_x64.dmg"));
  assert.ok(required.includes("R-Code_0.2.1_amd64.AppImage"));

  const record = {
    tagName: "v0.2.1-unsigned.1",
    isDraft: false,
    isPrerelease: true,
    assets: required.map((name) => ({ name, size: 1 })),
  };
  assert.deepEqual(
    validateReleaseRecord(record, record.tagName, parseReleaseTag(record.tagName)),
    [],
  );
  record.assets = record.assets.filter((asset) => asset.name !== "THIRD_PARTY_LICENSES.md");
  assert.match(
    validateReleaseRecord(record, record.tagName, parseReleaseTag(record.tagName)).join("\n"),
    /missing asset THIRD_PARTY_LICENSES\.md/,
  );
});

test("publish-release requires a public warning for unsigned stable releases", () => {
  const tag = "v0.2.1";
  const tagInfo = parseReleaseTag(tag);
  const signingPlan = platformSigningPlan(tagInfo, []);
  const record = {
    tagName: tag,
    isDraft: false,
    isPrerelease: false,
    body: "Generated notes",
    assets: requiredReleaseAssets(tagInfo.version).map((name) => ({ name, size: 1 })),
  };

  assert.match(
    validateReleaseRecord(record, tag, tagInfo, signingPlan).join("\n"),
    /missing its public signing warning/,
  );
  record.body = "<!-- RCODE_UNSIGNED_STABLE_WARNING -->\n> [!WARNING]";
  assert.deepEqual(validateReleaseRecord(record, tag, tagInfo, signingPlan), []);
});

test("publish-release verifies the updater manifest contract", () => {
  const tag = "v0.2.1-unsigned.1";
  const tagInfo = parseReleaseTag(tag);
  const manifest = {
    version: "0.2.1",
    platforms: Object.fromEntries(
      ["windows-x86_64", "darwin-aarch64", "darwin-x86_64", "linux-x86_64"]
        .map((platform) => [platform, {}]),
    ),
  };
  assert.deepEqual(validateUpdaterManifest(manifest, tag, tagInfo), []);
  delete manifest.platforms["darwin-x86_64"];
  assert.match(
    validateUpdaterManifest(manifest, tag, tagInfo).join("\n"),
    /latest\.json is missing darwin-x86_64/,
  );
});

test("CI authenticates every private agent-core checkout", () => {
  const workflow = fs.readFileSync(
    path.join(repoRoot, ".github", "workflows", "ci.yml"),
    "utf8",
  );

  const submoduleInitializers = workflow.match(/submodules: recursive/g) ?? [];
  const privateTokens = workflow.match(/token: \$\{\{ secrets\.PAT_TOKEN \}\}/g) ?? [];
  assert.equal(submoduleInitializers.length, 6);
  assert.equal(privateTokens.length, submoduleInitializers.length);
  assert.match(
    workflow,
    /name: Clippy[\s\S]*name: Install Linux dependencies[\s\S]*libwebkit2gtk-4\.1-dev/,
    "Linux clippy must install the native libraries required by the Tauri host crate",
  );
  assert.match(
    workflow,
    /name: Test[\s\S]*fail-fast:\s*false[\s\S]*macos-latest, windows-latest/,
    "one platform failure must not cancel the other platform test result",
  );

  const installerConfig = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "installer", "tauri.conf.json"), "utf8"),
  );
  assert.ok(
    installerConfig.bundle.icon.includes("../icons/icon.png"),
    "the installer needs a portable PNG icon so generate_context works on macOS/Linux",
  );
});

test("supply-chain generator emits CycloneDX and separates workspace packages", () => {
  const components = collectComponents({
    workspace_members: ["local 1.0.0"],
    packages: [
      { id: "local 1.0.0", name: "local", version: "1.0.0", license: "MIT" },
      { id: "dep 2.0.0", name: "dep", version: "2.0.0", license: "Apache-2.0", source: "registry+https://example.test/index" },
    ],
  }, {
    packages: {
      "": { name: "frontend", version: "1.0.0" },
      "node_modules/react": { version: "18.3.1", license: "MIT", resolved: "https://example.test/react.tgz" },
    },
  });
  const result = createArtifacts({ components, version: "1.0.0", timestamp: "2026-08-03T00:00:00.000Z" });

  assert.equal(result.sbom.bomFormat, "CycloneDX");
  assert.equal(result.sbom.specVersion, "1.5");
  assert.equal(result.sbom.components.length, 3);
  assert.equal(result.unknown.length, 0);
  assert.match(result.licenses, /\| cargo \| dep \| 2\.0\.0 \| Apache-2\.0 \|/);
  assert.doesNotMatch(result.licenses, /\| cargo \| local \|/);
});
