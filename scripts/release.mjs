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

function fail(message) {
  console.error(`release: ${message}`);
  process.exit(1);
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

function writeJson(path, value, newline) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}${newline}`);
}

function versionSnapshot() {
  const cargoToml = read(PATHS.cargoToml);
  const cargoLock = read(PATHS.cargoLock);
  const tauri = JSON.parse(read(PATHS.tauri));
  const installerTauri = JSON.parse(read(PATHS.installerTauri));
  const packageJson = JSON.parse(read(PATHS.packageJson));
  const packageLock = JSON.parse(read(PATHS.packageLock));
  const lockPackages = [...cargoLock.matchAll(/\[\[package\]\]\r?\nname = "(r-code-[^"]+)"\r?\nversion = "([^"]+)"/g)]
    .map((match) => ({ name: match[1], version: match[2] }));

  if (lockPackages.length === 0) fail("Cargo.lock contains no r-code-* workspace packages");

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

function checkVersions(tag) {
  const versions = versionSnapshot();
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
    if (!releaseHeading.test(read(PATHS.changelog))) {
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

  const definitionPattern = /^\[(Unreleased|[^\]]+)\]:\s+\S+\s*$/gm;
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

function prepare(version) {
  if (!version || version.startsWith("v") || !SEMVER.test(version)) {
    fail("prepare expects a SemVer without the v prefix, for example: 0.2.0");
  }

  const cargoToml = read(PATHS.cargoToml);
  const tauriText = read(PATHS.tauri);
  const installerTauriText = read(PATHS.installerTauri);
  const packageText = read(PATHS.packageJson);
  const packageLockText = read(PATHS.packageLock);
  const tauri = JSON.parse(tauriText);
  const installerTauri = JSON.parse(installerTauriText);
  const packageJson = JSON.parse(packageText);
  const packageLock = JSON.parse(packageLockText);

  const nextCargoToml = replaceWorkspaceVersion(cargoToml, version);
  const nextChangelog = stampChangelog(
    read(PATHS.changelog),
    version,
    repositoryUrl(cargoToml),
  );
  tauri.version = version;
  installerTauri.version = version;
  packageJson.version = version;
  packageLock.version = version;
  if (!packageLock.packages?.[""]) fail("package-lock.json is missing packages['']");
  packageLock.packages[""].version = version;

  writeFileSync(PATHS.cargoToml, nextCargoToml);
  writeJson(PATHS.tauri, tauri, newlineOf(tauriText));
  writeJson(PATHS.installerTauri, installerTauri, newlineOf(installerTauriText));
  writeJson(PATHS.packageJson, packageJson, newlineOf(packageText));
  writeJson(PATHS.packageLock, packageLock, newlineOf(packageLockText));
  writeFileSync(PATHS.changelog, nextChangelog);

  refreshCargoLock();
  checkVersions(`v${version}`);
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
  main(process.argv.slice(2));
}

export { parseReleaseTag, refreshCargoLock, replaceWorkspaceVersion, stampChangelog };
