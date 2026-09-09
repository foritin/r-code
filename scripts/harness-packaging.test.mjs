// T38 — cross-platform harness packaging inspection (node --test).
//
// Verifies the packaging surface: per-platform executable/header staging
// declarations, plugin resource layout (manifest + bin), sidecar config
// wiring, and an installed startup check through the normal immutable
// registry with a path containing spaces.

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  mkdtempSync,
  copyFileSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const isWindows = process.platform === "win32";
const exeSuffix = isWindows ? ".exe" : "";

function cargo(...args) {
  execFileSync("cargo", args, { cwd: repoRoot, timeout: 10 * 60 * 1000, stdio: ["ignore", "pipe", "pipe"] });
}

test("packaging declarations cover service, guardian mode and built-in plugins", () => {
  // 1) tauri.conf.json: sidecars + plugin resources declared.
  const conf = JSON.parse(readFileSync(join(repoRoot, "src-tauri/tauri.conf.json"), "utf8"));
  for (const sidecar of [
    "binaries/r-code-tui",
    "binaries/r-code-service",
    "binaries/r-code-harness-native",
    "binaries/r-code-harness-codex",
  ]) {
    assert.ok(conf.bundle.externalBin.includes(sidecar), `externalBin missing ${sidecar}`);
  }
  const resources = conf.bundle.resources ?? [];
  assert.ok(resources.some((entry) => entry.startsWith("plugins/")), "plugin resources declared");

  // 2) build.rs validates every sidecar in packaging mode (placeholders in dev).
  const buildRs = readFileSync(join(repoRoot, "src-tauri/build.rs"), "utf8");
  for (const name of ["r-code-service", "r-code-harness-native", "r-code-harness-codex"]) {
    assert.ok(buildRs.includes(`"${name}"`), `build.rs sidecar set missing ${name}`);
  }

  // 3) Built-in manifests: per-platform executables for all four platforms.
  for (const builtin of ["native", "codex"]) {
    const manifest = JSON.parse(
      readFileSync(join(repoRoot, `src-tauri/plugins/${builtin}/harness.json`), "utf8"),
    );
    assert.equal(manifest.schema_version, "1");
    const platforms = manifest.supportedPlatforms.map((entry) => entry.platform).sort();
    assert.deepEqual(platforms, ["linux-x64", "macos-arm64", "macos-x64", "windows-x64"]);
    for (const entry of manifest.supportedPlatforms) {
      assert.ok(entry.executable.startsWith("bin/"), `package-relative entry: ${entry.executable}`);
      assert.ok(!entry.executable.includes(".."), "no traversal in entrypoint");
    }
    assert.ok(manifest.id.length > 0);
    assert.ok(manifest.requestedHostServices.length > 0);
  }

  // 4) Codex ships its declarative process profile as package data.
  const profile = JSON.parse(
    readFileSync(join(repoRoot, "src-tauri/plugins/codex/process-profile.json"), "utf8"),
  );
  assert.equal(profile.framing, "ndjson-rpc");
  assert.ok(profile.methods.length > 0);

  // 5) Packaging scripts build and stage every sidecar + plugin package.
  const ps1 = readFileSync(join(repoRoot, "scripts/build-branded-installer.ps1"), "utf8");
  for (const name of ["r-code-service", "r-code-harness-native", "r-code-harness-codex"]) {
    assert.ok(ps1.includes(`"${name}"`), `installer missing ${name}`);
  }
  const sh = readFileSync(join(repoRoot, "scripts/manual/package-macos.sh"), "utf8");
  assert.ok(sh.includes("r-code-service"), "macOS script builds the service");
});

test("installed startup works through the immutable registry, spaces in path", () => {
  cargo("build", "-p", "r-code-runtime", "--bin", "r-code-service");
  cargo("build", "-p", "r-code-harness-native");

  // A layout with spaces in the path, mimicking install dirs like
  // "C:/Program Files/R-Code".
  const work = mkdtempSync(join(tmpdir(), "t38 space test-"));
  try {
    const resources = join(work, "resources");
    mkdirSync(join(resources, "plugins", "native", "bin"), { recursive: true });
    copyFileSync(
      join(repoRoot, "plugins/native/harness.json"),
      join(resources, "plugins/native/harness.json"),
    );
    copyFileSync(
      join(repoRoot, `target/debug/r-code-harness-native${exeSuffix}`),
      join(resources, "plugins/native/bin", `r-code-harness-native${exeSuffix}`),
    );

    const dataRoot = join(work, "data");
    mkdirSync(dataRoot, { recursive: true });
    const service = join(repoRoot, `target/debug/r-code-service${exeSuffix}`);

    // Detached start (the daemon outlives frontends by design); poll for
    // the built-in registration, then stop it explicitly.
    const daemon = spawn(service, [
      "--profile", "development",
      "--data-root", dataRoot,
      "--ipc-name", "t38 spaced",
    ], {
      stdio: ["ignore", "ignore", "ignore"],
      detached: true,
      // Custom layouts point the daemon at their staged plugin resources
      // explicitly (packaged layouts discover them beside the binary).
      env: { ...process.env, R_CODE_BUILTIN_PLUGINS_DIR: resources },
    });
    try {
      const ownerPath = join(dataRoot, "harness-v2", "owner.json");
      const pluginsDir = join(dataRoot, "harness-v2", "plugins", "native.r-code");
      const deadline = Date.now() + 30_000;
      while (Date.now() < deadline) {
        if (existsSync(ownerPath) && existsSync(pluginsDir)) break;
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 250);
      }
      assert.ok(existsSync(ownerPath), "daemon became the owner (spaced path)");

      const owner = JSON.parse(readFileSync(ownerPath, "utf8"));
      assert.ok(owner.token.length >= 32, "owner token written");

      // The built-in registered through the normal registry: its immutable
      // package directory exists under plugins/<id>/<version>/<digest>.
      assert.ok(existsSync(pluginsDir), "built-in installed into the registry");
      const versionDir = readdirSync(pluginsDir)[0];
      assert.ok(versionDir, "version dir present");
      const digestDir = readdirSync(join(pluginsDir, versionDir))[0];
      assert.match(digestDir, /^[0-9a-f]{16}$/, "digest-named install dir");
      const installedManifest = JSON.parse(
        readFileSync(join(pluginsDir, versionDir, digestDir, "harness.json"), "utf8"),
      );
      const shipped = JSON.parse(
        readFileSync(join(repoRoot, "plugins/native/harness.json"), "utf8"),
      );
      assert.equal(installedManifest.id, shipped.id);
      assert.equal(installedManifest.version, shipped.version);
      // The entry binary was copied package-relative.
      assert.ok(
        existsSync(join(pluginsDir, versionDir, digestDir, "bin", `r-code-harness-native${exeSuffix}`)),
        "entry binary staged inside the immutable package",
      );
    } finally {
      // Explicit stop cancels/drains the daemon (tests own their daemon).
      if (isWindows) {
        try {
          execFileSync("taskkill", ["/PID", String(daemon.pid), "/T", "/F"], { stdio: "ignore" });
        } catch {
          /* already gone */
        }
      } else {
        try {
          process.kill(-daemon.pid, "SIGTERM");
        } catch {
          /* already gone */
        }
      }
    }
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
});
