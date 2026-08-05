#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline/promises";

import { parseReleaseTag } from "./release.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BASE_RELEASE_SECRETS = ["PAT_TOKEN", "TAURI_SIGNING_PRIVATE_KEY"];
const WINDOWS_SIGNING_SECRETS = [
  "WINDOWS_CERTIFICATE",
  "WINDOWS_CERTIFICATE_PASSWORD",
  "WINDOWS_TIMESTAMP_URL",
];
const APPLE_SIGNING_SECRETS = [
  "APPLE_CERTIFICATE",
  "APPLE_CERTIFICATE_PASSWORD",
  "APPLE_SIGNING_IDENTITY",
  "APPLE_ID",
  "APPLE_PASSWORD",
  "APPLE_TEAM_ID",
];
const UNSIGNED_STABLE_WARNING_MARKER = "RCODE_UNSIGNED_STABLE_WARNING";

class ReleaseError extends Error {}

function usage() {
  console.log(`Usage:
  node scripts/publish-release.mjs vX.Y.Z [--dry-run] [--yes] [--no-wait]
  node scripts/publish-release.mjs vX.Y.Z-unsigned.N [--dry-run] [--yes] [--no-wait]

Options:
  --dry-run  Run every preflight check without creating or pushing a tag.
  --yes      Skip the typed-tag confirmation (for trusted automation only).
  --no-wait  Stop after the GitHub Release workflow has been discovered.
  --help     Show this help.

Prepare a new base version before publishing:
  node scripts/release.mjs prepare X.Y.Z

Stable tags become Latest. Missing Windows/macOS certificate groups fall back per platform
with a public warning; PAT_TOKEN and TAURI_SIGNING_PRIVATE_KEY remain required.`);
}

function parseArguments(argv) {
  const options = { dryRun: false, yes: false, noWait: false, help: false, tag: null };
  for (const argument of argv) {
    if (argument === "--dry-run") options.dryRun = true;
    else if (argument === "--yes") options.yes = true;
    else if (argument === "--no-wait") options.noWait = true;
    else if (argument === "--help" || argument === "-h") options.help = true;
    else if (argument.startsWith("-")) throw new ReleaseError(`unknown option: ${argument}`);
    else if (options.tag) throw new ReleaseError("only one release tag may be supplied");
    else options.tag = argument;
  }

  if (options.help) return options;
  if (!options.tag || !parseReleaseTag(options.tag)) {
    throw new ReleaseError("tag must be vX.Y.Z or vX.Y.Z-unsigned.N");
  }
  return options;
}

function run(command, args, { allowFailure = false, stdio = "pipe" } = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    stdio: stdio === "inherit" ? "inherit" : ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  if (result.error) throw new ReleaseError(`could not run ${command}: ${result.error.message}`);
  if (result.status !== 0 && !allowFailure) {
    const detail = `${result.stderr ?? ""}${result.stdout ?? ""}`.trim();
    throw new ReleaseError(`${command} ${args.join(" ")} failed${detail ? `:\n${detail}` : ""}`);
  }
  return {
    status: result.status ?? 1,
    stdout: (result.stdout ?? "").trim(),
    stderr: (result.stderr ?? "").trim(),
  };
}

function resolveGitHubCli() {
  const candidates = ["gh"];
  if (process.platform === "win32") {
    if (process.env.ProgramFiles) {
      candidates.push(join(process.env.ProgramFiles, "GitHub CLI", "gh.exe"));
    }
    if (process.env.LOCALAPPDATA) {
      candidates.push(join(process.env.LOCALAPPDATA, "Programs", "GitHub CLI", "gh.exe"));
    }
  }
  for (const candidate of candidates) {
    const result = spawnSync(candidate, ["--version"], { stdio: "ignore", windowsHide: true });
    if (!result.error && result.status === 0) return candidate;
  }
  throw new ReleaseError("GitHub CLI is not installed; install gh and run `gh auth login` first");
}

function parseJson(output, context) {
  try {
    return JSON.parse(output);
  } catch (error) {
    throw new ReleaseError(`${context} returned invalid JSON: ${error.message}`);
  }
}

function requiredSecretsForTag() {
  return [...BASE_RELEASE_SECRETS];
}

function platformSigningPlan(tagInfo, availableSecrets) {
  const available = availableSecrets instanceof Set
    ? availableSecrets
    : new Set(availableSecrets);
  if (tagInfo.unsignedPrerelease) {
    return {
      windowsSigned: false,
      appleSigned: false,
      missingWindows: [],
      missingApple: [],
      forcedUnsigned: true,
    };
  }

  const missingWindows = WINDOWS_SIGNING_SECRETS.filter((name) => !available.has(name));
  const missingApple = APPLE_SIGNING_SECRETS.filter((name) => !available.has(name));
  return {
    windowsSigned: missingWindows.length === 0,
    appleSigned: missingApple.length === 0,
    missingWindows,
    missingApple,
    forcedUnsigned: false,
  };
}

function unsignedPlatformNames(signingPlan) {
  const platforms = [];
  if (!signingPlan.windowsSigned) platforms.push("Windows");
  if (!signingPlan.appleSigned) platforms.push("macOS");
  return platforms;
}

function requiredReleaseAssets(version) {
  return [
    "latest.json",
    "r-code-sbom.cdx.json",
    "THIRD_PARTY_LICENSES.md",
    `R-Code_${version}_x64-installer.exe`,
    `R-Code_${version}_x64-setup.exe`,
    `R-Code_${version}_x64-setup.exe.sig`,
    `R-Code_${version}_x64_en-US.msi`,
    `R-Code_${version}_x64_en-US.msi.sig`,
    `R-Code_${version}_x64_zh-CN.msi`,
    `R-Code_${version}_x64_zh-CN.msi.sig`,
    `R-Code_${version}_aarch64.dmg`,
    `R-Code_${version}_aarch64.app.tar.gz`,
    `R-Code_${version}_aarch64.app.tar.gz.sig`,
    `R-Code_${version}_x64.dmg`,
    `R-Code_${version}_x64.app.tar.gz`,
    `R-Code_${version}_x64.app.tar.gz.sig`,
    `R-Code_${version}_amd64.AppImage`,
    `R-Code_${version}_amd64.AppImage.sig`,
    `R-Code_${version}_amd64.deb`,
    `R-Code_${version}_amd64.deb.sig`,
  ];
}

function validateReleaseRecord(record, tag, tagInfo, signingPlan = null) {
  const problems = [];
  if (record.tagName !== tag) problems.push(`release tag is ${record.tagName ?? "<missing>"}`);
  if (record.isDraft) problems.push("release is still a draft");
  if (Boolean(record.isPrerelease) !== tagInfo.unsignedPrerelease) {
    problems.push(tagInfo.unsignedPrerelease ? "release is not marked as a prerelease" : "stable release is marked as a prerelease");
  }
  if (
    !tagInfo.unsignedPrerelease
    && signingPlan
    && unsignedPlatformNames(signingPlan).length > 0
    && !String(record.body ?? "").includes(UNSIGNED_STABLE_WARNING_MARKER)
  ) {
    problems.push("unsigned stable release is missing its public signing warning");
  }

  const assets = new Map((record.assets ?? []).map((asset) => [asset.name, asset]));
  for (const name of requiredReleaseAssets(tagInfo.version)) {
    if (!assets.has(name)) problems.push(`missing asset ${name}`);
    else if (Number(assets.get(name).size) === 0) problems.push(`asset ${name} is empty`);
  }
  return problems;
}

function expectedUpdaterAssets(platform, version) {
  const assets = {
    "windows-x86_64": [
      `R-Code_${version}_x64_en-US.msi`,
    ],
    "windows-x86_64-msi": [
      `R-Code_${version}_x64_zh-CN.msi`,
    ],
    "windows-x86_64-nsis": [`R-Code_${version}_x64-setup.exe`],
    "darwin-aarch64": [`R-Code_${version}_aarch64.app.tar.gz`],
    "darwin-aarch64-app": [`R-Code_${version}_aarch64.app.tar.gz`],
    "darwin-x86_64": [`R-Code_${version}_x64.app.tar.gz`],
    "darwin-x86_64-app": [`R-Code_${version}_x64.app.tar.gz`],
    "linux-x86_64": [`R-Code_${version}_amd64.AppImage`],
    "linux-x86_64-appimage": [`R-Code_${version}_amd64.AppImage`],
    "linux-x86_64-deb": [`R-Code_${version}_amd64.deb`],
  };
  return assets[platform] ?? [];
}

function createUpdaterManifest({
  version,
  tag,
  repository,
  releaseAssets,
  signatureDirectory,
  notes = "",
  pubDate = new Date().toISOString(),
}) {
  const preferences = {
    "windows-x86_64": [
      `R-Code_${version}_x64_en-US.msi`,
      `R-Code_${version}_x64_zh-CN.msi`,
      `R-Code_${version}_x64-setup.exe`,
    ],
    "windows-x86_64-msi": [
      `R-Code_${version}_x64_zh-CN.msi`,
    ],
    "windows-x86_64-nsis": [`R-Code_${version}_x64-setup.exe`],
    "darwin-aarch64": [`R-Code_${version}_aarch64.app.tar.gz`],
    "darwin-aarch64-app": [`R-Code_${version}_aarch64.app.tar.gz`],
    "darwin-x86_64": [`R-Code_${version}_x64.app.tar.gz`],
    "darwin-x86_64-app": [`R-Code_${version}_x64.app.tar.gz`],
    "linux-x86_64": [`R-Code_${version}_amd64.AppImage`],
    "linux-x86_64-appimage": [`R-Code_${version}_amd64.AppImage`],
    "linux-x86_64-deb": [`R-Code_${version}_amd64.deb`],
  };
  const assetsByName = new Map((releaseAssets ?? []).map((asset) => [asset.name, asset]));
  const platforms = {};

  for (const [platform, candidates] of Object.entries(preferences)) {
    const asset = candidates.map((name) => assetsByName.get(name)).find(Boolean);
    if (!asset) {
      throw new ReleaseError(
        `cannot build latest.json ${platform}: missing ${candidates.join(" or ")}`,
      );
    }
    const signaturePath = join(signatureDirectory, `${asset.name}.sig`);
    let signature;
    try {
      signature = readFileSync(signaturePath, "utf8").trim();
    } catch (error) {
      throw new ReleaseError(
        `cannot build latest.json ${platform}: cannot read ${asset.name}.sig: ${error.message}`,
      );
    }
    if (!signature) {
      throw new ReleaseError(
        `cannot build latest.json ${platform}: ${asset.name}.sig is empty`,
      );
    }
    const repositoryParts = String(repository ?? "").split("/");
    if (repositoryParts.length !== 2 || repositoryParts.some((part) => !part)) {
      throw new ReleaseError(
        `cannot build latest.json ${platform}: invalid GitHub repository ${repository ?? "<missing>"}`,
      );
    }
    const assetRecordError = validateReleaseAssetRecord(
      asset,
      repositoryParts[0],
      repositoryParts[1],
      tag,
    );
    if (assetRecordError) {
      throw new ReleaseError(
        `cannot build latest.json ${platform}: ${assetRecordError}`,
      );
    }
    platforms[platform] = {
      signature,
      url: canonicalReleaseDownloadUrl(repositoryParts[0], repositoryParts[1], tag, asset.name),
    };
  }

  const manifest = { version, notes, pub_date: pubDate, platforms };
  const tagInfo = parseReleaseTag(tag);
  if (!tagInfo || tagInfo.version !== version) {
    throw new ReleaseError(`cannot build latest.json for mismatched tag ${tag} and version ${version}`);
  }
  const problems = validateUpdaterManifest(
    manifest,
    tag,
    tagInfo,
    repository,
    releaseAssets,
    signatureDirectory,
  );
  if (problems.length > 0) {
    throw new ReleaseError(`generated updater manifest is invalid:\n- ${problems.join("\n- ")}`);
  }
  return manifest;
}

function parseUpdaterUrl(rawUrl) {
  if (typeof rawUrl !== "string" || rawUrl.trim() === "") {
    return { error: "has no URL" };
  }
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return { error: `has an invalid URL: ${rawUrl}` };
  }
  if (url.protocol !== "https:") return { error: `uses a non-HTTPS URL: ${rawUrl}` };
  if (url.username || url.password || url.search || url.hash) {
    return { error: `uses a URL with credentials, query parameters, or a fragment: ${rawUrl}` };
  }
  try {
    const segments = url.pathname
      .split("/")
      .filter(Boolean)
      .map((segment) => decodeURIComponent(segment));
    if (segments.some((segment) => segment.includes("/"))) {
      return { error: `uses an ambiguous encoded path: ${rawUrl}` };
    }
    return { url, segments };
  } catch {
    return { error: `has an invalid encoded path: ${rawUrl}` };
  }
}

function canonicalReleaseDownloadUrl(owner, repository, tag, assetName) {
  return `https://github.com/${encodeURIComponent(owner)}/${encodeURIComponent(repository)}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(assetName)}`;
}

function validateReleaseAssetRecord(asset, owner, repository, tag) {
  if (!asset || typeof asset.name !== "string" || asset.name.trim() === "") {
    return "release contains an asset without a name";
  }

  const api = parseUpdaterUrl(asset.apiUrl);
  const [repos, apiOwner, apiRepository, releases, assets, assetId, ...apiExtra]
    = api.segments ?? [];
  if (
    api.error
    || api.url.hostname.toLowerCase() !== "api.github.com"
    || apiExtra.length > 0
    || repos !== "repos"
    || releases !== "releases"
    || assets !== "assets"
    || !/^\d+$/.test(assetId ?? "")
    || apiOwner?.toLowerCase() !== owner.toLowerCase()
    || apiRepository?.toLowerCase() !== repository.toLowerCase()
  ) {
    return `${asset.name} has an invalid GitHub asset API URL`;
  }

  const download = parseUpdaterUrl(asset.url);
  const [urlOwner, urlRepository, releasePart, downloadPart, assetTag, assetName, ...extra]
    = download.segments ?? [];
  if (
    download.error
    || download.url.hostname.toLowerCase() !== "github.com"
    || extra.length > 0
    || releasePart !== "releases"
    || downloadPart !== "download"
    || urlOwner?.toLowerCase() !== owner.toLowerCase()
    || urlRepository?.toLowerCase() !== repository.toLowerCase()
    || assetName !== asset.name
    || (assetTag !== tag && !/^untagged-[0-9a-f]+$/i.test(assetTag ?? ""))
  ) {
    return `${asset.name} has an invalid GitHub release download URL`;
  }

  return null;
}

function updaterAssetName(rawUrl, repository, tag, releaseAssets) {
  const repositoryParts = String(repository ?? "").split("/");
  if (repositoryParts.length !== 2 || repositoryParts.some((part) => !part)) {
    return { error: `cannot validate against invalid GitHub repository ${repository ?? "<missing>"}` };
  }
  const [owner, name] = repositoryParts;
  const parsed = parseUpdaterUrl(rawUrl);
  if (parsed.error) return parsed;

  const { url, segments } = parsed;
  let asset = null;
  if (url.hostname.toLowerCase() === "github.com") {
    const [urlOwner, urlName, releases, download, urlTag, assetName, ...extra] = segments;
    if (
      extra.length > 0
      || releases !== "releases"
      || download !== "download"
      || urlOwner?.toLowerCase() !== owner.toLowerCase()
      || urlName?.toLowerCase() !== name.toLowerCase()
    ) {
      return { error: `does not point to ${repository} release downloads: ${rawUrl}` };
    }
    if (urlTag !== tag) return { error: `points to release ${urlTag ?? "<missing>"}, not ${tag}` };
    asset = (releaseAssets ?? []).find((candidate) => candidate.name === assetName);
  } else if (url.hostname.toLowerCase() === "api.github.com") {
    const [repos, urlOwner, urlName, releases, assets, assetId, ...extra] = segments;
    if (
      extra.length > 0
      || repos !== "repos"
      || releases !== "releases"
      || assets !== "assets"
      || !/^\d+$/.test(assetId ?? "")
      || urlOwner?.toLowerCase() !== owner.toLowerCase()
      || urlName?.toLowerCase() !== name.toLowerCase()
    ) {
      return { error: `does not identify an asset in ${repository}: ${rawUrl}` };
    }
    asset = (releaseAssets ?? []).find((candidate) => candidate.apiUrl === rawUrl);
  } else {
    return { error: `uses unexpected host ${url.hostname}: ${rawUrl}` };
  }

  if (!asset) return { error: `is not an asset on ${repository} release ${tag}: ${rawUrl}` };
  const assetRecordError = validateReleaseAssetRecord(asset, owner, name, tag);
  if (assetRecordError) return { error: `${assetRecordError}: ${rawUrl}` };
  return { name: asset.name };
}

function validateUpdaterManifest(
  manifest,
  tag,
  tagInfo,
  repository,
  releaseAssets = [],
  signatureDirectory = null,
) {
  const problems = [];
  const manifestVersion = manifest?.version;
  if (manifestVersion !== tagInfo.version) {
    problems.push(`latest.json version ${manifestVersion ?? "<missing>"} does not match ${tag}`);
  }
  const platforms = [
    "windows-x86_64",
    "windows-x86_64-msi",
    "windows-x86_64-nsis",
    "darwin-aarch64",
    "darwin-aarch64-app",
    "darwin-x86_64",
    "darwin-x86_64-app",
    "linux-x86_64",
    "linux-x86_64-appimage",
    "linux-x86_64-deb",
  ];
  const resolvedAssets = new Map();
  for (const platform of platforms) {
    const entry = manifest?.platforms?.[platform];
    if (!entry || typeof entry !== "object") {
      problems.push(`latest.json is missing ${platform}`);
      continue;
    }
    // Updater signatures are mandatory even when OS code-signing certificates
    // are unavailable for a stable release. They protect update integrity.
    if (typeof entry.signature !== "string" || entry.signature.trim() === "") {
      problems.push(`latest.json ${platform} has an empty signature`);
    }
    const resolved = updaterAssetName(entry.url, repository, tag, releaseAssets);
    if (resolved.error) {
      problems.push(`latest.json ${platform} URL ${resolved.error}`);
      continue;
    }
    const expected = expectedUpdaterAssets(platform, tagInfo.version);
    if (!expected.includes(resolved.name)) {
      problems.push(
        `latest.json ${platform} points to ${resolved.name}; expected ${expected.join(" or ")}`,
      );
      continue;
    }
    resolvedAssets.set(platform, resolved.name);
    if (signatureDirectory) {
      const signaturePath = join(signatureDirectory, `${resolved.name}.sig`);
      let uploadedSignature;
      try {
        uploadedSignature = readFileSync(signaturePath, "utf8").trim();
      } catch (error) {
        problems.push(
          `latest.json ${platform} cannot read ${resolved.name}.sig: ${error.message}`,
        );
        continue;
      }
      if (entry.signature !== uploadedSignature) {
        problems.push(`latest.json ${platform} signature does not match ${resolved.name}.sig`);
      }
    }
  }
  const windowsPlatforms = [
    "windows-x86_64",
    "windows-x86_64-msi",
    "windows-x86_64-nsis",
  ];
  const resolvedWindowsAssets = windowsPlatforms
    .map((platform) => resolvedAssets.get(platform))
    .filter(Boolean);
  const requiredWindowsAssets = [
    `R-Code_${tagInfo.version}_x64_en-US.msi`,
    `R-Code_${tagInfo.version}_x64_zh-CN.msi`,
    `R-Code_${tagInfo.version}_x64-setup.exe`,
  ];
  if (
    resolvedWindowsAssets.length === windowsPlatforms.length
    && (
      new Set(resolvedWindowsAssets).size !== windowsPlatforms.length
      || requiredWindowsAssets.some((name) => !resolvedWindowsAssets.includes(name))
    )
  ) {
    problems.push(
      "latest.json Windows updater entries must uniquely cover en-US MSI, zh-CN MSI, and NSIS",
    );
  }
  return problems;
}

function newestRun(runs, predicate) {
  return runs
    .filter(predicate)
    .sort((left, right) => String(right.createdAt).localeCompare(String(left.createdAt)))[0] ?? null;
}

function git(args, options) {
  return run("git", args, options);
}

function github(gh, args, options) {
  return run(gh, args, options);
}

function loadRuns(gh, repository, workflow, extraArgs = []) {
  const result = github(gh, [
    "run", "list", "--repo", repository, "--workflow", workflow, "--limit", "20",
    "--json", "databaseId,headBranch,headSha,event,status,conclusion,url,createdAt",
    ...extraArgs,
  ]);
  return parseJson(result.stdout, `${workflow} run list`);
}

function waitForCi(gh, repository, headSha) {
  const runs = loadRuns(gh, repository, "CI", ["--commit", headSha]);
  const ci = newestRun(
    runs,
    (candidate) => candidate.headSha === headSha
      && candidate.headBranch === "main"
      && candidate.event === "push",
  );
  if (!ci) {
    throw new ReleaseError(`no CI push run exists for main commit ${headSha}; wait for CI or rerun it before publishing`);
  }

  if (ci.status !== "completed") {
    console.log(`publish-release: waiting for CI ${ci.url}`);
    github(gh, ["run", "watch", String(ci.databaseId), "--repo", repository, "--exit-status"], { stdio: "inherit" });
  } else if (ci.conclusion !== "success") {
    throw new ReleaseError(`CI did not succeed for ${headSha}: ${ci.conclusion} (${ci.url})`);
  }

  const verified = parseJson(
    github(gh, ["run", "view", String(ci.databaseId), "--repo", repository, "--json", "status,conclusion,url"]).stdout,
    "CI run",
  );
  if (verified.status !== "completed" || verified.conclusion !== "success") {
    throw new ReleaseError(`CI did not finish successfully: ${verified.conclusion ?? verified.status} (${verified.url})`);
  }
  console.log(`publish-release: CI passed (${verified.url})`);
}

async function findReleaseRun(gh, repository, tag, headSha) {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const runs = loadRuns(gh, repository, "Release", ["--branch", tag]);
    const releaseRun = newestRun(
      runs,
      (candidate) => candidate.headBranch === tag && candidate.headSha === headSha,
    );
    if (releaseRun) return releaseRun;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 3_000));
  }
  throw new ReleaseError(`tag was pushed, but no Release workflow appeared for ${tag}; inspect GitHub Actions before retrying`);
}

async function waitForReleaseRecord(gh, repository, tag) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const result = github(gh, [
      "release", "view", tag, "--repo", repository,
      "--json", "tagName,name,body,isDraft,isPrerelease,url,assets",
    ], { allowFailure: true });
    if (result.status === 0) return parseJson(result.stdout, "release view");
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_500));
  }
  throw new ReleaseError(`Release workflow succeeded, but GitHub Release ${tag} is unavailable`);
}

function verifyPublishedRelease(gh, repository, tag, tagInfo, record, signingPlan) {
  const problems = validateReleaseRecord(record, tag, tagInfo, signingPlan);
  if (!tagInfo.unsignedPrerelease) {
    const latest = parseJson(
      github(gh, ["release", "view", "--repo", repository, "--json", "tagName"]).stdout,
      "latest release",
    );
    if (latest.tagName !== tag) problems.push(`Latest still points to ${latest.tagName ?? "<missing>"}`);
  }

  const temporaryDirectory = mkdtempSync(join(tmpdir(), "r-code-release-"));
  try {
    github(gh, ["release", "download", tag, "--repo", repository, "--pattern", "latest.json", "--dir", temporaryDirectory]);
    github(gh, ["release", "download", tag, "--repo", repository, "--pattern", "*.sig", "--dir", temporaryDirectory]);
    const manifest = parseJson(readFileSync(join(temporaryDirectory, "latest.json"), "utf8"), "latest.json");
    problems.push(...validateUpdaterManifest(
      manifest,
      tag,
      tagInfo,
      repository,
      record.assets,
      temporaryDirectory,
    ));
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }

  if (problems.length > 0) {
    throw new ReleaseError(`release verification failed:\n- ${problems.join("\n- ")}`);
  }
}

async function confirmPublish(tag, tagInfo, signingPlan, assumeYes) {
  if (assumeYes) return;
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    throw new ReleaseError("interactive confirmation is unavailable; pass --yes only after reviewing the preflight output");
  }

  const unsignedPlatforms = unsignedPlatformNames(signingPlan);
  const kind = tagInfo.unsignedPrerelease
    ? "unsigned prerelease"
    : unsignedPlatforms.length > 0
      ? `stable release with unsigned ${unsignedPlatforms.join(" and ")} artifacts`
      : "fully signed stable release";
  console.log(`\npublish-release: ready to create and push ${tag} (${kind}).`);
  console.log("publish-release: public tags are immutable; type the complete tag to continue.");
  const prompt = createInterface({ input: process.stdin, output: process.stdout });
  try {
    const answer = await prompt.question("> ");
    if (answer.trim() !== tag) throw new ReleaseError("confirmation did not match; nothing was published");
  } finally {
    prompt.close();
  }
}

function requireCleanMain() {
  const branch = git(["branch", "--show-current"]).stdout;
  if (branch !== "main") throw new ReleaseError(`current branch is ${branch || "detached HEAD"}; switch to main first`);
  const changes = git(["status", "--porcelain=v1", "--untracked-files=all"]).stdout;
  if (changes) throw new ReleaseError(`worktree is not clean:\n${changes}`);
}

function requireNewTag(tag) {
  const local = git(["rev-parse", "--quiet", "--verify", `refs/tags/${tag}`], { allowFailure: true });
  const remote = git(["ls-remote", "--tags", "--refs", "origin", `refs/tags/${tag}`]);
  if (local.status === 0 || remote.stdout) {
    throw new ReleaseError(
      `tag ${tag} already exists; never move a public release tag. For a transient retry use \`gh workflow run Release -f tag=${tag}\`, otherwise publish a new patch or unsigned sequence`,
    );
  }
}

async function publish(options) {
  const tagInfo = parseReleaseTag(options.tag);
  requireCleanMain();

  const gh = resolveGitHubCli();
  github(gh, ["auth", "status", "--hostname", "github.com"]);
  const repository = github(gh, ["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"]).stdout;
  if (!repository) throw new ReleaseError("could not determine the GitHub repository");

  console.log(`publish-release: checking ${repository} ${options.tag}`);
  git(["fetch", "--quiet", "--tags", "origin"]);
  const headSha = git(["rev-parse", "HEAD"]).stdout;
  const remoteMainSha = git(["rev-parse", "refs/remotes/origin/main"]).stdout;
  if (headSha !== remoteMainSha) {
    throw new ReleaseError(`local main ${headSha} does not match origin/main ${remoteMainSha}`);
  }
  requireNewTag(options.tag);

  run(process.execPath, [join("scripts", "release.mjs"), "check", options.tag], { stdio: "inherit" });
  waitForCi(gh, repository, headSha);

  const availableSecrets = new Set(
    parseJson(
      github(gh, ["secret", "list", "--repo", repository, "--json", "name"]).stdout,
      "secret list",
    ).map((entry) => entry.name),
  );
  const missingSecrets = requiredSecretsForTag(tagInfo).filter((name) => !availableSecrets.has(name));
  if (missingSecrets.length > 0) {
    throw new ReleaseError(`missing GitHub Actions secrets for ${options.tag}: ${missingSecrets.join(", ")}`);
  }

  const signingPlan = platformSigningPlan(tagInfo, availableSecrets);
  if (!tagInfo.unsignedPrerelease) {
    if (signingPlan.missingWindows.length > 0) {
      console.warn(
        `publish-release: WARNING — Windows artifacts will be unsigned; missing: ${signingPlan.missingWindows.join(", ")}`,
      );
    }
    if (signingPlan.missingApple.length > 0) {
      console.warn(
        `publish-release: WARNING — macOS artifacts will use ad-hoc signing without notarization; missing: ${signingPlan.missingApple.join(", ")}`,
      );
    }
  }

  const unsignedPlatforms = unsignedPlatformNames(signingPlan);
  const releaseKind = tagInfo.unsignedPrerelease
    ? "unsigned prerelease (not Latest; platform signing disabled)"
    : unsignedPlatforms.length > 0
      ? `stable release with unsigned ${unsignedPlatforms.join(" and ")} artifacts (becomes Latest with a public warning)`
      : "fully signed stable release (becomes Latest after all platforms pass)";
  console.log(`publish-release: preflight passed — ${releaseKind}`);
  if (options.dryRun) {
    console.log(`publish-release: dry run complete; would create and push ${options.tag}`);
    return;
  }

  await confirmPublish(options.tag, tagInfo, signingPlan, options.yes);
  const message = tagInfo.unsignedPrerelease
    ? `R-Code ${options.tag} unsigned prerelease`
    : `R-Code ${options.tag}`;
  git(["tag", "-a", options.tag, "-m", message]);
  git(["push", "origin", `refs/tags/${options.tag}`], { stdio: "inherit" });
  console.log(`publish-release: pushed ${options.tag}; waiting for GitHub Actions to register the run`);

  const releaseRun = await findReleaseRun(gh, repository, options.tag, headSha);
  console.log(`publish-release: Release workflow ${releaseRun.url}`);
  if (options.noWait) {
    console.log("publish-release: --no-wait selected; GitHub Actions will continue remotely");
    return;
  }

  github(gh, ["run", "watch", String(releaseRun.databaseId), "--repo", repository, "--exit-status"], { stdio: "inherit" });
  const record = await waitForReleaseRecord(gh, repository, options.tag);
  verifyPublishedRelease(gh, repository, options.tag, tagInfo, record, signingPlan);
  console.log(`publish-release: published and verified ${record.url}`);
}

async function main(argv) {
  try {
    const options = parseArguments(argv);
    if (options.help) {
      usage();
      return;
    }
    await publish(options);
  } catch (error) {
    console.error(`publish-release: ${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  await main(process.argv.slice(2));
}

export {
  createUpdaterManifest,
  parseArguments,
  platformSigningPlan,
  requiredReleaseAssets,
  requiredSecretsForTag,
  unsignedPlatformNames,
  validateReleaseRecord,
  validateUpdaterManifest,
};
