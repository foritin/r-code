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
const SIGNED_RELEASE_SECRETS = [
  "WINDOWS_CERTIFICATE",
  "WINDOWS_CERTIFICATE_PASSWORD",
  "WINDOWS_TIMESTAMP_URL",
  "APPLE_CERTIFICATE",
  "APPLE_CERTIFICATE_PASSWORD",
  "APPLE_SIGNING_IDENTITY",
  "APPLE_ID",
  "APPLE_PASSWORD",
  "APPLE_TEAM_ID",
];

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
  node scripts/release.mjs prepare X.Y.Z`);
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

function requiredSecretsForTag(tagInfo) {
  return tagInfo.unsignedPrerelease
    ? [...BASE_RELEASE_SECRETS]
    : [...BASE_RELEASE_SECRETS, ...SIGNED_RELEASE_SECRETS];
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

function validateReleaseRecord(record, tag, tagInfo) {
  const problems = [];
  if (record.tagName !== tag) problems.push(`release tag is ${record.tagName ?? "<missing>"}`);
  if (record.isDraft) problems.push("release is still a draft");
  if (Boolean(record.isPrerelease) !== tagInfo.unsignedPrerelease) {
    problems.push(tagInfo.unsignedPrerelease ? "release is not marked as a prerelease" : "stable release is marked as a prerelease");
  }

  const assets = new Map((record.assets ?? []).map((asset) => [asset.name, asset]));
  for (const name of requiredReleaseAssets(tagInfo.version)) {
    if (!assets.has(name)) problems.push(`missing asset ${name}`);
    else if (Number(assets.get(name).size) === 0) problems.push(`asset ${name} is empty`);
  }
  return problems;
}

function validateUpdaterManifest(manifest, tag, tagInfo) {
  const problems = [];
  if (manifest.version !== tagInfo.version && manifest.version !== tag.replace(/^v/, "")) {
    problems.push(`latest.json version ${manifest.version ?? "<missing>"} does not match ${tag}`);
  }
  for (const platform of ["windows-x86_64", "darwin-aarch64", "darwin-x86_64", "linux-x86_64"]) {
    if (!manifest.platforms?.[platform]) problems.push(`latest.json is missing ${platform}`);
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
      "--json", "tagName,isDraft,isPrerelease,url,assets",
    ], { allowFailure: true });
    if (result.status === 0) return parseJson(result.stdout, "release view");
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_500));
  }
  throw new ReleaseError(`Release workflow succeeded, but GitHub Release ${tag} is unavailable`);
}

function verifyPublishedRelease(gh, repository, tag, tagInfo, record) {
  const problems = validateReleaseRecord(record, tag, tagInfo);
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
    const manifest = parseJson(readFileSync(join(temporaryDirectory, "latest.json"), "utf8"), "latest.json");
    problems.push(...validateUpdaterManifest(manifest, tag, tagInfo));
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }

  if (problems.length > 0) {
    throw new ReleaseError(`release verification failed:\n- ${problems.join("\n- ")}`);
  }
}

async function confirmPublish(tag, tagInfo, assumeYes) {
  if (assumeYes) return;
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    throw new ReleaseError("interactive confirmation is unavailable; pass --yes only after reviewing the preflight output");
  }

  const kind = tagInfo.unsignedPrerelease ? "unsigned prerelease" : "signed stable release";
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

  const releaseKind = tagInfo.unsignedPrerelease
    ? "unsigned prerelease (not Latest; platform signing disabled)"
    : "signed stable release (becomes Latest after all platforms pass)";
  console.log(`publish-release: preflight passed — ${releaseKind}`);
  if (options.dryRun) {
    console.log(`publish-release: dry run complete; would create and push ${options.tag}`);
    return;
  }

  await confirmPublish(options.tag, tagInfo, options.yes);
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
  verifyPublishedRelease(gh, repository, options.tag, tagInfo, record);
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
  parseArguments,
  requiredReleaseAssets,
  requiredSecretsForTag,
  validateReleaseRecord,
  validateUpdaterManifest,
};
