import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repositoryDir = path.resolve(frontendDir, "..", "..");
const fixturePath = path.join(
  repositoryDir,
  "fixtures",
  "automation",
  "public-contract-v1.json",
);
const typesPath = path.join(frontendDir, "src", "lib", "automation-types.ts");
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8"));
const typeSource = fs.readFileSync(typesPath, "utf8");

function sorted(values) {
  return [...values].sort();
}

function exactKeys(value, expected, label) {
  assert.deepEqual(sorted(Object.keys(value)), sorted(expected), `${label} field set changed`);
}

function typeAliasBody(name) {
  const match = typeSource.match(new RegExp(`export\\s+type\\s+${name}\\s*=([\\s\\S]*?);`));
  assert.ok(match, `missing TypeScript type alias ${name}`);
  return match[1];
}

function stringLiterals(source) {
  return [...source.matchAll(/"([a-z][a-z0-9_]*)"/g)].map((match) => match[1]);
}

function interfaceFields(name) {
  const match = typeSource.match(new RegExp(`export\\s+interface\\s+${name}\\s*{([\\s\\S]*?)\\n}`));
  assert.ok(match, `missing TypeScript interface ${name}`);
  return [...match[1].matchAll(/^\s*([a-z][a-z0-9_]*)\??\s*:/gm)].map((entry) => entry[1]);
}

function assertUtc(value, label) {
  assert.equal(typeof value, "string", `${label} must be a string`);
  assert.match(value, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/, `${label} must be UTC`);
  assert.ok(Number.isFinite(Date.parse(value)), `${label} must be a valid timestamp`);
}

function assertLocalTime(value, label) {
  assert.equal(typeof value, "string", `${label} must be a string`);
  assert.match(value, /^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?$/, `${label} must be a wall time`);
}

test("the JSON fixture uses exactly the TypeScript public field sets", () => {
  exactKeys(fixture.definition, interfaceFields("AutomationDefinition"), "definition");
  exactKeys(fixture.run, interfaceFields("AutomationRun"), "run");
  exactKeys(
    fixture.definition.execution_profile,
    interfaceFields("ExecutionProfile"),
    "execution profile",
  );
  exactKeys(
    fixture.run.definition_snapshot,
    interfaceFields("AutomationDefinitionSnapshot"),
    "definition snapshot",
  );

  assert.equal(fixture.run.automation_id, fixture.definition.id);
  assert.equal(fixture.run.definition_snapshot.definition_id, fixture.definition.id);
  assert.deepEqual(
    fixture.run.definition_snapshot,
    {
      definition_id: fixture.definition.id,
      name: fixture.definition.name,
      workspace_path: fixture.definition.workspace_path,
      prompt: fixture.definition.prompt,
      execution_profile: fixture.definition.execution_profile,
      schedule: fixture.definition.schedule,
      timezone: fixture.definition.timezone,
      permission: fixture.definition.permission,
      base_ref: fixture.definition.base_ref,
      definition_updated_at: fixture.definition.updated_at,
    },
  );
});

test("TypeScript literal unions exactly match every stable serialized Rust name", () => {
  const mappings = [
    ["AutomationPermission", fixture.stable_names.permissions],
    ["AutomationDefinitionState", fixture.stable_names.definition_states],
    ["RunTrigger", fixture.stable_names.run_triggers],
    ["RunStatus", fixture.stable_names.run_statuses],
    ["AutomationWeekday", fixture.stable_names.weekdays],
  ];
  for (const [typeName, expected] of mappings) {
    assert.deepEqual(
      stringLiterals(typeAliasBody(typeName)),
      expected,
      `${typeName} must match the cross-layer fixture`,
    );
  }
});

test("all five schedule variants have the frozen TypeScript and JSON shapes", () => {
  assert.match(typeSource, /HOURLY_INTERVAL_MINUTES\s*=\s*60\s+as\s+const/);
  const scheduleMatch = typeSource.match(
    /export\s+type\s+ScheduleSpec\s*=([\s\S]*?)\n\nexport\s+interface\s+AutomationDefinition/,
  );
  assert.ok(scheduleMatch, "missing TypeScript ScheduleSpec union");
  const scheduleBody = scheduleMatch[1];
  assert.deepEqual(
    stringLiterals(scheduleBody),
    ["once", "hourly", "daily", "weekdays", "weekly"],
  );
  assert.match(scheduleBody, /interval_minutes:\s*typeof\s+HOURLY_INTERVAL_MINUTES/);

  assert.equal(fixture.schedule_specs.length, 5);
  const [once, hourly, daily, weekdays, weekly] = fixture.schedule_specs;
  exactKeys(once, ["kind", "run_at_utc"], "once schedule");
  exactKeys(hourly, ["kind", "anchor_at_utc", "interval_minutes"], "hourly schedule");
  exactKeys(daily, ["kind", "local_time"], "daily schedule");
  exactKeys(weekdays, ["kind", "local_time"], "weekdays schedule");
  exactKeys(weekly, ["kind", "weekday", "local_time"], "weekly schedule");
  assert.deepEqual(
    fixture.schedule_specs.map((schedule) => schedule.kind),
    ["once", "hourly", "daily", "weekdays", "weekly"],
  );
  assertUtc(once.run_at_utc, "once.run_at_utc");
  assertUtc(hourly.anchor_at_utc, "hourly.anchor_at_utc");
  assert.equal(hourly.interval_minutes, 60);
  assertLocalTime(daily.local_time, "daily.local_time");
  assertLocalTime(weekdays.local_time, "weekdays.local_time");
  assertLocalTime(weekly.local_time, "weekly.local_time");
  assert.ok(fixture.stable_names.weekdays.includes(weekly.weekday));
});

test("definition, profile, permission, run, and nullable fields use valid values", () => {
  const { definition, run, stable_names: stableNames } = fixture;
  assert.equal(typeof definition.id, "string");
  assert.equal(typeof definition.name, "string");
  assert.equal(typeof definition.workspace_path, "string");
  assert.equal(typeof definition.prompt, "string");
  assert.ok(["r_code", "codex"].includes(definition.execution_profile.agent_engine));
  assert.equal(typeof definition.execution_profile.provider_name, "string");
  assert.equal(typeof definition.execution_profile.model, "string");
  assert.ok(
    definition.execution_profile.reasoning_effort === null
      || typeof definition.execution_profile.reasoning_effort === "string",
  );
  assert.ok(stableNames.permissions.includes(definition.permission));
  assert.ok(stableNames.definition_states.includes(definition.state));
  assert.ok(definition.base_ref === null || typeof definition.base_ref === "string");
  assert.ok(definition.next_run_at_utc === null || typeof definition.next_run_at_utc === "string");
  assertUtc(definition.created_at, "definition.created_at");
  assertUtc(definition.updated_at, "definition.updated_at");
  assert.ok(stableNames.run_triggers.includes(run.trigger));
  assert.ok(stableNames.run_statuses.includes(run.status));
  assert.ok(run.task_id === null || typeof run.task_id === "string");
  assertUtc(run.scheduled_for, "run.scheduled_for");
  assert.equal(typeof run.idempotency_key, "string");
  assert.ok(run.lease_owner === null || typeof run.lease_owner === "string");
  assert.ok(run.lease_expires_at === null || typeof run.lease_expires_at === "string");
  assert.ok(Number.isInteger(run.missed_count) && run.missed_count >= 0);
  assert.ok(run.started_at === null || typeof run.started_at === "string");
  assert.ok(run.finished_at === null || typeof run.finished_at === "string");
  assert.ok(run.error_code === null || typeof run.error_code === "string");
});
