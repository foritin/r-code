#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  throw new Error(`supply-chain: ${message}`);
}

function runCargoMetadata(root) {
  const result = spawnSync("cargo", ["metadata", "--locked", "--format-version", "1"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) fail(result.stderr?.trim() || "cargo metadata failed");
  return JSON.parse(result.stdout);
}

function packageNameFromLockPath(path, value) {
  if (value.name) return value.name;
  const marker = "node_modules/";
  const index = path.lastIndexOf(marker);
  return index >= 0 ? path.slice(index + marker.length) : path;
}

function sourceReference(pkg) {
  if (pkg.repository) return pkg.repository;
  if (pkg.resolved) return pkg.resolved;
  if (pkg.source?.startsWith("registry+")) return pkg.source.slice("registry+".length);
  return pkg.source ?? "";
}

function purl(ecosystem, name, version) {
  const encoded = name.split("/").map(encodeURIComponent).join("/");
  return `pkg:${ecosystem}/${encoded}@${encodeURIComponent(version)}`;
}

function normalizeComponent(component) {
  return {
    ecosystem: component.ecosystem,
    name: component.name,
    version: component.version,
    license: component.license?.trim() || "UNKNOWN",
    source: sourceReference(component),
    workspace: Boolean(component.workspace),
  };
}

export function collectComponents(cargoMetadata, packageLock) {
  const workspaceIds = new Set(cargoMetadata.workspace_members ?? []);
  const cargo = (cargoMetadata.packages ?? []).map((pkg) => normalizeComponent({
    ecosystem: "cargo",
    name: pkg.name,
    version: pkg.version,
    license: pkg.license || (pkg.license_file ? `SEE LICENSE FILE: ${pkg.license_file}` : ""),
    repository: pkg.repository,
    source: pkg.source,
    workspace: workspaceIds.has(pkg.id),
  }));

  const npm = Object.entries(packageLock.packages ?? {})
    .filter(([path, value]) => path && value && value.version && !value.link)
    .map(([path, value]) => normalizeComponent({
      ecosystem: "npm",
      name: packageNameFromLockPath(path, value),
      version: value.version,
      license: value.license,
      resolved: value.resolved,
      workspace: false,
    }));

  const unique = new Map();
  for (const component of [...cargo, ...npm]) {
    unique.set(`${component.ecosystem}:${component.name}@${component.version}`, component);
  }
  return [...unique.values()].sort((a, b) =>
    a.ecosystem.localeCompare(b.ecosystem) || a.name.localeCompare(b.name) || a.version.localeCompare(b.version)
  );
}

function deterministicSerial(components) {
  const hex = createHash("sha256").update(JSON.stringify(components)).digest("hex").slice(0, 32).split("");
  hex[12] = "5";
  hex[16] = ((Number.parseInt(hex[16], 16) & 0x3) | 0x8).toString(16);
  const value = hex.join("");
  return `urn:uuid:${value.slice(0, 8)}-${value.slice(8, 12)}-${value.slice(12, 16)}-${value.slice(16, 20)}-${value.slice(20)}`;
}

function workspaceVersion(root) {
  const cargoToml = readFileSync(join(root, "Cargo.toml"), "utf8");
  const match = cargoToml.match(/\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/);
  if (!match) fail("Cargo.toml is missing [workspace.package].version");
  return match[1];
}

export function createArtifacts({ components, version, timestamp = new Date().toISOString() }) {
  const thirdParty = components.filter((component) => !component.workspace);
  const bomComponents = components.map((component) => ({
    type: component.workspace ? "application" : "library",
    "bom-ref": purl(component.ecosystem, component.name, component.version),
    group: "",
    name: component.name,
    version: component.version,
    purl: purl(component.ecosystem, component.name, component.version),
    licenses: [{ license: { name: component.license } }],
    ...(component.source ? { externalReferences: [{ type: "distribution", url: component.source }] } : {}),
    properties: [{ name: "r-code:ecosystem", value: component.ecosystem }],
  }));
  const sbom = {
    bomFormat: "CycloneDX",
    specVersion: "1.5",
    serialNumber: deterministicSerial(components),
    version: 1,
    metadata: {
      timestamp,
      component: {
        type: "application",
        name: "R-Code",
        version,
        purl: `pkg:github/foritin/r-code@${encodeURIComponent(version)}`,
      },
      tools: { components: [{ type: "application", name: "R-Code supply-chain generator", version: "1" }] },
    },
    components: bomComponents,
  };

  const unknown = thirdParty.filter((component) => component.license === "UNKNOWN");
  const rows = thirdParty.map((component) =>
    `| ${component.ecosystem} | ${component.name.replaceAll("|", "\\|")} | ${component.version} | ${component.license.replaceAll("|", "\\|")} | ${component.source ? `[source](${component.source})` : "—"} |`
  );
  const licenses = [
    "# R-Code third-party dependency licenses",
    "",
    `Generated from \`Cargo.lock\`/Cargo metadata and \`src-tauri/frontend/package-lock.json\` for R-Code ${version}.`,
    "The package's own license files remain authoritative.",
    "",
    `Dependencies: ${thirdParty.length}; unresolved licenses: ${unknown.length}.`,
    "",
    "| Ecosystem | Package | Version | Declared license | Source |",
    "|---|---|---:|---|---|",
    ...rows,
    "",
  ].join("\n");

  return { sbom, licenses, unknown };
}

export function generate(root, outputDirectory, strict = false) {
  const cargoMetadata = runCargoMetadata(root);
  const packageLock = JSON.parse(readFileSync(join(root, "src-tauri", "frontend", "package-lock.json"), "utf8"));
  const components = collectComponents(cargoMetadata, packageLock);
  const artifacts = createArtifacts({ components, version: workspaceVersion(root) });
  if (strict && artifacts.unknown.length > 0) {
    fail(`missing declared licenses for ${artifacts.unknown.map((item) => `${item.ecosystem}:${item.name}@${item.version}`).join(", ")}`);
  }
  mkdirSync(outputDirectory, { recursive: true });
  writeFileSync(join(outputDirectory, "r-code-sbom.cdx.json"), `${JSON.stringify(artifacts.sbom, null, 2)}\n`);
  writeFileSync(join(outputDirectory, "THIRD_PARTY_LICENSES.md"), artifacts.licenses);
  console.log(`Generated ${components.length} SBOM components in ${relative(root, outputDirectory) || "."}`);
  return artifacts;
}

const invoked = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invoked) {
  const args = process.argv.slice(2);
  const strict = args.includes("--strict");
  const output = args.find((arg) => arg !== "--strict") ?? join(ROOT, "target", "supply-chain");
  try {
    generate(ROOT, resolve(ROOT, output), strict);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
