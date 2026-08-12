#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const PATHS = {
  cargoToml: join(ROOT, "Cargo.toml"),
  cargoLock: join(ROOT, "Cargo.lock"),
  tauri: join(ROOT, "src-tauri", "tauri.conf.json"),
  installerTauri: join(ROOT, "installer", "tauri.conf.json"),
  packageJson: join(ROOT, "src-tauri", "frontend", "package.json"),
  packageLock: join(ROOT, "src-tauri", "frontend", "package-lock.json"),
  changelog: join(ROOT, "CHANGELOG.md"),
};

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const RELEASE_TAG = /^v((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))(?:-unsigned\.([1-9]\d*))?$/;
const PRERELEASE_MARKER = "预上线版本（Pre-release）";

class ReleaseError extends Error {}

function fail(message) {
  throw new ReleaseError(message);
}

function read(path) {
  return readFileSync(path, "utf8");
}

function newlineOf(text) {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

function workspaceVersion(cargoToml) {
  const match = cargoToml.match(/\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/);
  if (!match) fail("Cargo.toml is missing [workspace.package].version");
  return match[1];
}

function repositoryUrl(cargoToml) {
  const match = cargoToml.match(/\[workspace\.package\][\s\S]*?\nrepository\s*=\s*"([^"]+)"/);
  if (!match) fail("Cargo.toml is missing [workspace.package].repository");
  return match[1].replace(/\/$/, "");
}

function replaceWorkspaceVersion(cargoToml, version) {
  let replaced = false;
  const next = cargoToml.replace(
    /(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")([^"]+)(")/,
    (_match, prefix, _oldVersion, suffix) => {
      replaced = true;
      return `${prefix}${version}${suffix}`;
    },
  );
  if (!replaced) fail("could not update [workspace.package].version");
  return next;
}

function parseReleaseTag(tag) {
  const match = tag?.match(RELEASE_TAG);
  if (!match) return null;
  return {
    version: match[1],
    unsignedPrerelease: match[2] !== undefined,
    sequence: match[2] ? Number(match[2]) : null,
  };
}

function changelogSection(changelog, version) {
  const lines = changelog.split(/\r?\n/);
  const heading = `## [${version}]`;
  const start = lines.findIndex((line) => line.startsWith(heading));
  if (start < 0) return "";
  let end = lines.findIndex((line, index) => index > start && line.startsWith("## ["));
  if (end < 0) end = lines.length;
  return lines.slice(start + 1, end).join("\n").trim();
}

function isPreReleaseVersion(changelog, version) {
  return changelogSection(changelog, version).includes(PRERELEASE_MARKER);
}

function jsonText(value, newline) {
  return `${JSON.stringify(value, null, 2)}${newline}`;
}

function versionSnapshot(paths = PATHS) {
  const cargoToml = read(paths.cargoToml);
  const cargoLock = read(paths.cargoLock);
  const tauri = JSON.parse(read(paths.tauri));
  const installerTauri = JSON.parse(read(paths.installerTauri));
  const packageJson = JSON.parse(read(paths.packageJson));
  const packageLock = JSON.parse(read(paths.packageLock));
  const lockPackages = [...cargoLock.matchAll(/\[\[package\]\]\r?\nname = "((?:r-code|hermes)-[^"]+)"\r?\nversion = "([^"]+)"/g)]
    .map((match) => ({ name: match[1], version: match[2] }));

  if (lockPackages.length === 0) fail("Cargo.lock contains no R-Code workspace packages");

  return {
    workspace: workspaceVersion(cargoToml),
    tauri: tauri.version,
    installerTauri: installerTauri.version,
    frontend: packageJson.version,
    frontendLock: packageLock.version,
    frontendLockRoot: packageLock.packages?.[""]?.version,
    lockPackages,
  };
}

function checkVersions(tag, paths = PATHS) {
  const versions = versionSnapshot(paths);
  const expected = versions.workspace;
  if (!SEMVER.test(expected)) fail(`workspace version is not SemVer: ${expected}`);

  const named = [
    ["src-tauri/tauri.conf.json", versions.tauri],
    ["installer/tauri.conf.json", versions.installerTauri],
    ["src-tauri/frontend/package.json", versions.frontend],
    ["src-tauri/frontend/package-lock.json", versions.frontendLock],
    ["src-tauri/frontend/package-lock.json packages['']", versions.frontendLockRoot],
  ];
  for (const [name, actual] of named) {
    if (actual !== expected) fail(`${name} has version ${actual ?? "<missing>"}; expected ${expected}`);
  }
  for (const entry of versions.lockPackages) {
    if (entry.version !== expected) {
      fail(`Cargo.lock package ${entry.name} has version ${entry.version}; expected ${expected}`);
    }
  }

  if (tag) {
    const tagInfo = parseReleaseTag(tag);
    if (!tagInfo || tagInfo.version !== expected) {
      fail(`tag ${tag} must be v${expected} or v${expected}-unsigned.N`);
    }
    const escaped = expected.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const releaseHeading = new RegExp(`^## \\[${escaped}\\] - \\d{4}-\\d{2}-\\d{2}$`, "m");
    if (!releaseHeading.test(read(paths.changelog))) {
      fail(`CHANGELOG.md has no dated section for ${expected}; run \"node scripts/release.mjs prepare ${expected}\" first`);
    }
  }

  console.log(`release: versions are consistent at ${expected}${tag ? ` (${tag})` : ""}`);
  return expected;
}

function stampChangelog(changelog, version, repository) {
  const newline = newlineOf(changelog);
  const releasePattern = new RegExp(`^## \\[${version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\] - \\d{4}-\\d{2}-\\d{2}$`, "m");
  if (releasePattern.test(changelog)) return changelog;

  const marker = "## [Unreleased]";
  const markerIndex = changelog.indexOf(marker);
  if (markerIndex < 0) fail("CHANGELOG.md is missing an ## [Unreleased] section");
  const contentStart = markerIndex + marker.length;
  const nextHeading = changelog.indexOf(`${newline}## [`, contentStart);
  const contentEnd = nextHeading < 0 ? changelog.length : nextHeading;
  const unreleasedBody = changelog.slice(contentStart, contentEnd).trim();
  if (!unreleasedBody) fail("CHANGELOG.md [Unreleased] section is empty");

  const date = new Date().toISOString().slice(0, 10);
  const before = changelog.slice(0, contentStart).trimEnd();
  const after = nextHeading < 0 ? "" : changelog.slice(nextHeading).trimStart();
  let next = `${before}${newline}${newline}## [${version}] - ${date}${newline}${newline}${unreleasedBody}`;
  if (after) next += `${newline}${newline}${after}`;

  const definitionPattern = /^\[(Unreleased|[^\]]+)\]:\s+\S+[^\S\r\n]*$/gm;
  const definitions = [...next.matchAll(definitionPattern)];
  const keep = definitions
    .filter((match) => match[1] !== "Unreleased" && match[1] !== version)
    .map((match) => match[0]);
  next = next.replace(definitionPattern, "").trimEnd();
  const links = [
    ...keep,
    `[Unreleased]: ${repository}/compare/v${version}...HEAD`,
    `[${version}]: ${repository}/releases/tag/v${version}`,
  ];
  return `${next}${newline}${newline}${links.join(newline)}${newline}`;
}

function refreshCargoLock(spawn = spawnSync, cwd = ROOT) {
  // Resolving the full graph is intentional. `cargo metadata --no-deps` does not
  // refresh workspace package versions in Cargo.lock after a release bump.
  const result = spawn("cargo", ["metadata", "--format-version", "1"], {
    cwd,
    stdio: ["ignore", "ignore", "inherit"],
  });
  if (result.error) fail(`could not run cargo metadata: ${result.error.message}`);
  if (result.status !== 0) fail("cargo metadata failed while refreshing Cargo.lock");
}

function applyFileTransaction(
  { writes, backupPaths, afterWrite },
  {
    readFile = readFileSync,
    writeFile = (path, content) => writeFileSync(path, content),
  } = {},
) {
  const backups = new Map(backupPaths.map((path) => [path, readFile(path)]));
  let completed = false;
  let operationError = null;
  const rollbackErrors = [];

  try {
    for (const { path, content } of writes) writeFile(path, content, "prepare");
    afterWrite();
    completed = true;
  } catch (error) {
    operationError = error;
  } finally {
    if (!completed) {
      for (const [path, content] of backups) {
        try {
          writeFile(path, content, "rollback");
        } catch (error) {
          rollbackErrors.push(`${path}: ${error.message}`);
        }
      }
    }
  }

  if (operationError) {
    if (rollbackErrors.length > 0) {
      throw new ReleaseError(
        `${operationError.message}; rollback was incomplete:\n- ${rollbackErrors.join("\n- ")}`,
        { cause: operationError },
      );
    }
    throw operationError;
  }
}

function validatePreparedOutputs(version, outputs) {
  if (workspaceVersion(outputs.cargoToml) !== version) {
    fail(`prepared Cargo.toml does not contain version ${version}`);
  }
  for (const [name, text] of [
    ["src-tauri/tauri.conf.json", outputs.tauri],
    ["installer/tauri.conf.json", outputs.installerTauri],
    ["src-tauri/frontend/package.json", outputs.packageJson],
  ]) {
    if (JSON.parse(text).version !== version) {
      fail(`prepared ${name} does not contain version ${version}`);
    }
  }
  const packageLock = JSON.parse(outputs.packageLock);
  if (packageLock.version !== version || packageLock.packages?.[""]?.version !== version) {
    fail(`prepared src-tauri/frontend/package-lock.json does not contain version ${version}`);
  }
  const escaped = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  if (!new RegExp(`^## \\[${escaped}\\] - \\d{4}-\\d{2}-\\d{2}$`, "m").test(outputs.changelog)) {
    fail(`prepared CHANGELOG.md has no dated section for ${version}`);
  }
}

function prepare(
  version,
  {
    paths = PATHS,
    root = ROOT,
    spawn = spawnSync,
    readFile = readFileSync,
    writeFile = (path, content) => writeFileSync(path, content),
  } = {},
) {
  if (!version || version.startsWith("v") || !SEMVER.test(version)) {
    fail("prepare expects a SemVer without the v prefix, for example: 0.2.0");
  }

  const cargoToml = read(paths.cargoToml);
  const tauriText = read(paths.tauri);
  const installerTauriText = read(paths.installerTauri);
  const packageText = read(paths.packageJson);
  const packageLockText = read(paths.packageLock);
  const changelog = read(paths.changelog);
  const tauri = JSON.parse(tauriText);
  const installerTauri = JSON.parse(installerTauriText);
  const packageJson = JSON.parse(packageText);
  const packageLock = JSON.parse(packageLockText);

  const nextCargoToml = replaceWorkspaceVersion(cargoToml, version);
  const nextChangelog = stampChangelog(
    changelog,
    version,
    repositoryUrl(cargoToml),
  );
  tauri.version = version;
  installerTauri.version = version;
  packageJson.version = version;
  packageLock.version = version;
  if (!packageLock.packages?.[""]) fail("package-lock.json is missing packages['']");
  packageLock.packages[""].version = version;

  const outputs = {
    cargoToml: nextCargoToml,
    tauri: jsonText(tauri, newlineOf(tauriText)),
    installerTauri: jsonText(installerTauri, newlineOf(installerTauriText)),
    packageJson: jsonText(packageJson, newlineOf(packageText)),
    packageLock: jsonText(packageLock, newlineOf(packageLockText)),
    changelog: nextChangelog,
  };
  validatePreparedOutputs(version, outputs);

  const writes = [
    { path: paths.cargoToml, content: outputs.cargoToml },
    { path: paths.tauri, content: outputs.tauri },
    { path: paths.installerTauri, content: outputs.installerTauri },
    { path: paths.packageJson, content: outputs.packageJson },
    { path: paths.packageLock, content: outputs.packageLock },
    { path: paths.changelog, content: outputs.changelog },
  ];
  applyFileTransaction(
    {
      writes,
      backupPaths: [...writes.map(({ path }) => path), paths.cargoLock],
      afterWrite: () => {
        refreshCargoLock(spawn, root);
        checkVersions(`v${version}`, paths);
      },
    },
    { readFile, writeFile },
  );
  console.log(`release: prepared v${version}`);
  console.log("release: review the diff, run the verification suite, commit, then create and push the tag");
}

function usage() {
  console.log("Usage:");
  console.log("  node scripts/release.mjs check [vX.Y.Z|vX.Y.Z-unsigned.N]");
  console.log("  node scripts/release.mjs prepare X.Y.Z");
}

function main(argv) {
  const [command, argument, ...extra] = argv;
  if (extra.length > 0) {
    usage();
    process.exit(1);
  }

  if (command === "check") {
    checkVersions(argument);
  } else if (command === "prepare") {
    prepare(argument);
  } else {
    usage();
    process.exit(command ? 1 : 0);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`release: ${error.message}`);
    process.exitCode = 1;
  }
}

export {
  applyFileTransaction,
  changelogSection,
  isPreReleaseVersion,
  parseReleaseTag,
  prepare,
  refreshCargoLock,
  replaceWorkspaceVersion,
  stampChangelog,
};
