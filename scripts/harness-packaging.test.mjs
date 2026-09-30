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

  const codexSource = JSON.parse(
    readFileSync(join(repoRoot, "plugins/codex/harness.json"), "utf8"),
  );
  const codexStaged = JSON.parse(
    readFileSync(join(repoRoot, "src-tauri/plugins/codex/harness.json"), "utf8"),
  );
  assert.deepEqual(codexStaged, codexSource, "staged Codex manifest must match its source");
  assert.equal(codexSource.apiMajor, 1);
  assert.equal(codexSource.apiMinor, 1);
  assert.deepEqual(
    codexSource.requestedHostServices
      .filter((service) => service.startsWith("host.process."))
      .sort(),
    ["host.process.close", "host.process.open", "host.process.read", "host.process.write"],
  );

  // P19A advanced the native package to API v1.2 for WorkUnit effect/network
  // fields; P23.4 advances it to the final Wave 3 minor 3, where the package
  // also declares it runs as one process and therefore requires the host's
  // single-process guarantee. A package carrying the flag can never declare an
  // older minor, exactly like the effect-fields rule one minor below it.
  const nativeSource = JSON.parse(
    readFileSync(join(repoRoot, "plugins/native/harness.json"), "utf8"),
  );
  // tauri.conf.json bundles src-tauri/plugins/* and the installer scripts
  // stage the sidecar binary beside that manifest, so the staged copy is the
  // one that ships: it can never drift from its source.
  const nativeStaged = JSON.parse(
    readFileSync(join(repoRoot, "src-tauri/plugins/native/harness.json"), "utf8"),
  );
  assert.deepEqual(nativeStaged, nativeSource, "staged native manifest must match its source");
  assert.equal(nativeSource.apiMajor, 1);
  assert.equal(nativeSource.apiMinor, 3, "native package advanced to the final Wave 3 API v1.3");
  assert.equal(nativeSource.requiresEffectFields, true);
  assert.equal(
    nativeSource.requiresSingleProcess,
    true,
    "the native harness declares one process, so the host must enforce the guarantee",
  );
  assert.ok(
    !("requiresEffectFields" in codexSource) || codexSource.apiMinor >= 2,
    "a package requiring effect fields cannot declare an older minor",
  );
  assert.ok(
    !("requiresSingleProcess" in codexSource) || codexSource.apiMinor >= 3,
    "a package requiring the single-process guarantee cannot declare an older minor",
  );

  const publicSchema = JSON.parse(
    readFileSync(join(repoRoot, "crates/r-code-harness-protocol/schema/harness-v1.schema.json"), "utf8"),
  );
  assert.ok(
    publicSchema.properties.requestedHostServices.items.enum.includes("host.process.read"),
    "manifest schema exposes ProcessRead",
  );
  assert.ok(publicSchema.definitions.ProcessReadRequest, "ProcessRead request schema frozen");
  assert.ok(publicSchema.definitions.ProcessReadReply, "ProcessRead reply schema frozen");
  // P19A: the effect/network contract is frozen in the public schema.
  assert.equal(publicSchema.properties.requiresEffectFields.type, "boolean");
  // P23.1: so is the child-process declaration, additive and defaulted off.
  assert.equal(publicSchema.properties.requiresSingleProcess.type, "boolean");
  assert.equal(publicSchema.properties.requiresSingleProcess.default, false);
  assert.deepEqual(publicSchema.definitions.WorkUnitEffectClass.enum, [
    "read-only",
    "workspace-mutation",
    "dependency-preparation",
  ]);
  assert.deepEqual(publicSchema.definitions.NetworkCeiling.enum, [
    "offline",
    "public-internet-client",
    "host-network",
  ]);

  const appServer = readFileSync(join(repoRoot, "plugins/codex/src/app_server.rs"), "utf8");
  assert.ok(!appServer.includes("codex.event.next"), "private Codex polling method removed");
  assert.ok(appServer.includes("process_read"), "Codex consumes the public ProcessRead SDK");

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

test("guardian and safety-probe helpers ship through every packaging flow (P24H)", () => {
  const helpers = ["r-code-process-guardian", "r-code-safety-probe"];

  // 1) Both real packaging scripts stage the helpers with target-triple
  //    names, exactly like every other sidecar (substitution, not a
  //    separate convention).
  const ps1 = readFileSync(join(repoRoot, "scripts/build-branded-installer.ps1"), "utf8");
  const sh = readFileSync(join(repoRoot, "scripts/manual/package-macos.sh"), "utf8");
  for (const helper of helpers) {
    assert.ok(
      ps1.includes(`Bin = "${helper}"`),
      `installer does not build the ${helper} sidecar`,
    );
    assert.ok(ps1.includes(`"$($sidecar.Bin)-$architectureTarget.exe"`), "installer triple staging");
    assert.ok(sh.includes(helper), `macOS script does not build the ${helper} sidecar`);
  }

  // 2) The Tauri bundle declares the helpers, and the local-package overlay
  //    mirrors the same externalBin set (no divergence between flows).
  const conf = JSON.parse(readFileSync(join(repoRoot, "src-tauri/tauri.conf.json"), "utf8"));
  const localConf = JSON.parse(
    readFileSync(join(repoRoot, "src-tauri/tauri.local-package.conf.json"), "utf8"),
  );
  for (const helper of helpers) {
    assert.ok(
      conf.bundle.externalBin.includes(`binaries/${helper}`),
      `tauri.conf.json does not bundle ${helper}`,
    );
  }
  assert.deepEqual(
    localConf.bundle.externalBin,
    conf.bundle.externalBin,
    "local-package overlay must mirror the bundled sidecar set",
  );

  // 3) build.rs stays validation-only and knows the helpers: no nested
  //    cargo launch, just the placeholder/magic refusal in packaging mode.
  const buildRs = readFileSync(join(repoRoot, "src-tauri/build.rs"), "utf8");
  for (const helper of helpers) {
    assert.ok(buildRs.includes(`"${helper}"`), `build.rs sidecar list missing ${helper}`);
  }
  assert.ok(
    !buildRs.includes("Command::new") && !buildRs.includes("std::process"),
    "build.rs must never launch a nested build (comments may mention cargo)",
  );

  // 4) Omission is a refusal, not a silent shrink: the runtime resolver and
  //    the profile plumbing exist for the helpers the scripts stage.
  const resolver = readFileSync(
    join(repoRoot, "crates/r-code-runtime/src/services/helper_binaries.rs"),
    "utf8",
  );
  assert.ok(resolver.includes(`pub const GUARDIAN_HELPER`), "resolver names the guardian");
  assert.ok(resolver.includes(`pub const SAFETY_PROBE_HELPER`), "resolver names the probe");
  const client = readFileSync(join(repoRoot, "crates/r-code-client/src/lib.rs"), "utf8");
  assert.ok(
    client.includes("--helper-dir"),
    "the daemon auto-start must bind the helper directory",
  );
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
      const ownerPath = join(dataRoot, "harness-v1", "owner.json");
      const pluginsDir = join(dataRoot, "harness-v1", "plugins", "native.r-code");
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
