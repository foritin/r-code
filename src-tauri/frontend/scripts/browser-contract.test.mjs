import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repositoryDir = path.resolve(frontendDir, "..", "..");
const fixturePath = path.join(repositoryDir, "fixtures", "browser", "contract-v1.json");
const typesPath = path.join(frontendDir, "src", "lib", "browser-contract.ts");
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8"));
const typeSource = fs.readFileSync(typesPath, "utf8");

const expectedToolNames = [
  "open",
  "navigate",
  "snapshot",
  "screenshot",
  "click",
  "type",
  "select",
  "press",
  "scroll",
  "wait",
  "tabs",
  "console",
  "network-errors",
  "close",
];

function sorted(values) {
  return [...values].sort();
}

function exactKeys(value, expected, label) {
  assert.deepEqual(sorted(Object.keys(value)), sorted(expected), `${label} field set changed`);
}

function interfaceFields(name) {
  const match = typeSource.match(
    new RegExp(`export\\s+interface\\s+${name}\\s*{([\\s\\S]*?)\\n}`),
  );
  assert.ok(match, `missing TypeScript interface ${name}`);
  return [...match[1].matchAll(/^\s*([a-z][a-z0-9_]*)\??\s*:/gm)].map(
    (entry) => entry[1],
  );
}

function typeAliasBody(name) {
  const match = typeSource.match(new RegExp(`export\\s+type\\s+${name}\\s*=([\\s\\S]*?);`));
  assert.ok(match, `missing TypeScript type alias ${name}`);
  return match[1];
}

function stringLiterals(source) {
  return [...source.matchAll(/"([a-z][a-z0-9_-]*)"/g)].map((match) => match[1]);
}

function constArray(name) {
  const match = typeSource.match(
    new RegExp(`export\\s+const\\s+${name}\\s*=\\s*\\[([\\s\\S]*?)\\]\\s+as\\s+const`),
  );
  assert.ok(match, `missing TypeScript const array ${name}`);
  return stringLiterals(match[1]);
}

function assertTimestamp(value, label) {
  assert.equal(typeof value, "string", `${label} must be a string`);
  assert.match(
    value,
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/,
    `${label} must be UTC`,
  );
  assert.ok(Number.isFinite(Date.parse(value)), `${label} must be a valid timestamp`);
}

test("TypeScript and JSON freeze the exact fourteen Browser tool names", () => {
  assert.equal(new Set(expectedToolNames).size, 14);
  assert.deepEqual(constArray("BROWSER_TOOL_NAMES"), expectedToolNames);
  assert.deepEqual(fixture.tool_names, expectedToolNames);
  assert.equal(new Set(fixture.tool_names).size, fixture.tool_names.length);

  const requestMatch = typeSource.match(
    /export\s+type\s+BrowserToolRequest\s*=([\s\S]*?)\n\nexport\s+interface\s+BrowserSessionInput/,
  );
  assert.ok(requestMatch, "missing TypeScript BrowserToolRequest union");
  assert.deepEqual(stringLiterals(requestMatch[1]), expectedToolNames);
  assert.deepEqual(
    fixture.tool_requests.map((request) => request.tool),
    expectedToolNames,
  );
});

test("fixture DTOs use exactly the frozen TypeScript public fields", () => {
  exactKeys(
    fixture.runtime_manifest,
    interfaceFields("BrowserRuntimeManifest"),
    "runtime manifest",
  );
  exactKeys(fixture.session, interfaceFields("BrowserSession"), "session");
  exactKeys(fixture.tab, interfaceFields("BrowserTab"), "tab");
  for (const [index, grant] of fixture.permission_grants.entries()) {
    exactKeys(grant, interfaceFields("BrowserPermissionGrant"), `permission grant ${index}`);
    exactKeys(grant.origin, interfaceFields("BrowserOrigin"), `permission origin ${index}`);
  }

  assert.equal(fixture.schema_version, 1);
  assert.equal(fixture.runtime_manifest.schema_version, fixture.schema_version);
  assert.equal(fixture.session.session_id, fixture.tab.session_id);
  assert.equal(fixture.session.task_id, fixture.permission_grants[0].task_id);
  assertTimestamp(fixture.tab.created_at, "tab.created_at");
  assertTimestamp(fixture.tab.updated_at, "tab.updated_at");
  for (const [index, grant] of fixture.permission_grants.entries()) {
    assertTimestamp(grant.granted_at, `permission_grants[${index}].granted_at`);
    assert.ok(grant.revoked_at === null || typeof grant.revoked_at === "string");
  }
});

test("permission names and timeout constants stay aligned with the Rust wire contract", () => {
  assert.deepEqual(
    stringLiterals(typeAliasBody("BrowserPermissionCapability")),
    ["browse", "interact"],
  );
  assert.deepEqual(stringLiterals(typeAliasBody("BrowserPermissionScope")), ["once", "task"]);
  assert.match(typeSource, /BROWSER_CONTRACT_SCHEMA_VERSION\s*=\s*1\s+as\s+const/);
  assert.match(typeSource, /MAX_BROWSER_TIMEOUT_MS\s*=\s*30_000\s+as\s+const/);

  const combinations = new Set();
  for (const capability of ["browse", "interact"]) {
    for (const scope of ["once", "task"]) combinations.add(`${capability}:${scope}`);
  }
  assert.deepEqual(sorted(combinations), [
    "browse:once",
    "browse:task",
    "interact:once",
    "interact:task",
  ]);
  for (const grant of fixture.permission_grants) {
    assert.ok(["browse", "interact"].includes(grant.capability));
    assert.ok(["once", "task"].includes(grant.scope));
    assert.ok(combinations.has(`${grant.capability}:${grant.scope}`));
  }

  const wait = fixture.tool_requests.find((request) => request.tool === "wait");
  assert.ok(wait, "fixture must include the wait request");
  assert.equal(wait.input.timeout_ms, 30_000);
});

test("every fixture result carries the stable action metadata shape", () => {
  assert.ok(fixture.tool_results.length > 0, "fixture must contain Browser tool results");
  for (const result of fixture.tool_results) {
    for (const field of interfaceFields("BrowserActionMetadata")) {
      assert.ok(
        Object.hasOwn(result.output, field),
        `${result.tool} result is missing action metadata field ${field}`,
      );
    }
    assert.equal(typeof result.output.session_id, "string");
    assert.ok(result.output.session_id.length > 0);
    assert.ok(result.output.tab_id === null || typeof result.output.tab_id === "string");
    assert.ok(result.output.url === null || typeof result.output.url === "string");
    assert.equal(typeof result.output.action_id, "string");
    assert.ok(result.output.action_id.length > 0);
    assertTimestamp(result.output.timestamp, `${result.tool}.output.timestamp`);
  }
});

test("snapshot values and console/network records prove sensitive data redaction", () => {
  const snapshot = fixture.tool_results.find((result) => result.tool === "snapshot");
  assert.ok(snapshot, "fixture must include a snapshot result");
  const password = snapshot.output.snapshot.elements.find(
    (element) => element.name.toLowerCase() === "password",
  );
  assert.ok(password, "snapshot fixture must identify the password element");
  assert.deepEqual(password.value, { state: "redacted" });

  const consoleResult = fixture.tool_results.find((result) => result.tool === "console");
  assert.ok(consoleResult, "fixture must include a console result proving secret redaction");
  assert.ok(consoleResult.output.entries.length > 0, "console fixture must include an entry");
  exactKeys(consoleResult.output.entries[0], interfaceFields("BrowserConsoleEntry"), "console entry");
  assert.ok(
    consoleResult.output.entries.every((entry) => entry.redacted === true),
    "every console fixture entry must be explicitly redacted",
  );

  const networkResult = fixture.tool_results.find(
    (result) => result.tool === "network-errors",
  );
  assert.ok(networkResult, "fixture must include a network-errors result proving secret redaction");
  assert.ok(networkResult.output.errors.length > 0, "network fixture must include an error");
  for (const [index, error] of networkResult.output.errors.entries()) {
    exactKeys(error, interfaceFields("BrowserNetworkError"), `network error ${index}`);
    assert.equal(error.redacted, true);
    assertTimestamp(error.timestamp, `network error ${index}.timestamp`);
  }
});
